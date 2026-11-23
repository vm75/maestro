use std::{
    fs::File,
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rustix::fs::{self, Mode, OFlags, RenameFlags};

use super::{Config, MAX_CONFIG_BYTES, parse, revision};

const CURRENT: &str = "workloads.yaml";
const HISTORY: &str = "history";
pub const MISSING_REVISION: &str = "missing";

#[derive(Debug)]
pub struct RevisionConflict;

impl std::fmt::Display for RevisionConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Configuration changed. Reload before saving.")
    }
}

impl std::error::Error for RevisionConflict {}

pub struct Snapshot {
    pub bytes: Vec<u8>,
    pub revision: String,
}

impl Snapshot {
    pub fn config(&self) -> Result<Config> {
        parse(&self.bytes)
    }
}

/// Desired-state files in an existing owner-only directory.
///
/// All in-process and cross-process writers must use this store. Native
/// activation and recovery are separate responsibilities of the coordinator.
#[derive(Clone)]
pub struct Store {
    directory: PathBuf,
}

impl Store {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub fn ensure_directory(&self) -> Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        match std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&self.directory)
        {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        self.directory()?;
        Ok(())
    }

    pub(super) fn directory(&self) -> Result<File> {
        open_directory(&self.directory)
    }

    /// Read the exact authored bytes even when repairing invalid YAML.
    pub fn read(&self) -> Result<Option<Snapshot>> {
        match self.directory() {
            Ok(directory) => current(&directory),
            Err(error)
                if error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub fn read_history(&self, id: &str) -> Result<Snapshot> {
        ensure!(valid_revision(id), "invalid revision identifier");
        let directory = open_directory(&self.directory)?;
        let history = open_child_directory(&directory, HISTORY)?;
        let snapshot = read_snapshot(&history, &format!("{id}.yaml"))?;
        ensure!(
            snapshot.revision == id,
            "historical revision does not match its contents"
        );
        Ok(snapshot)
    }

    pub fn list_history(&self) -> Result<Vec<String>> {
        if !self.directory.exists() {
            return Ok(Vec::new());
        }
        let directory = open_directory(&self.directory)?;
        if open_child_directory(&directory, HISTORY).is_err() {
            return Ok(Vec::new());
        }
        let history_path = self.directory.join(HISTORY);
        let mut revisions = Vec::new();
        if let Ok(entries) = std::fs::read_dir(history_path) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if let Some(rev) = name.strip_suffix(".yaml") {
                        if valid_revision(rev) {
                            revisions.push(rev.to_string());
                        }
                    }
                }
            }
        }
        revisions.sort();
        revisions.reverse();
        Ok(revisions)
    }

    /// Publish valid YAML if the current exact-byte revision matches the review.
    pub fn save(&self, bytes: &[u8], expected: &str) -> Result<Snapshot> {
        let candidate = parse(bytes)?;
        let directory = self.lock_directory()?;
        super::journal::ensure_idle(&directory)?;
        Self::save_locked(&directory, bytes, expected, &candidate)
    }

    pub(super) fn lock_directory(&self) -> Result<File> {
        self.ensure_directory()?;
        let directory = open_directory(&self.directory)?;
        lock(&directory)?;
        Ok(directory)
    }

    pub(super) fn save_locked(
        directory: &File,
        bytes: &[u8],
        expected: &str,
        candidate: &Config,
    ) -> Result<Snapshot> {
        let previous = current(directory)?;
        let old_revision = previous
            .as_ref()
            .map_or(MISSING_REVISION, |value| &value.revision);
        if expected != old_revision {
            return Err(RevisionConflict.into());
        }
        let id = revision(bytes);
        if let Some(previous) = previous {
            if previous.bytes == bytes {
                return Ok(previous);
            }
            if let Ok(current) = previous.config() {
                validate_existing_guests(&current, candidate)?;
            }
            remember(directory, &previous)?;
            let latest = current(directory)?;
            if latest.as_ref().map(|value| value.revision.as_str()) != Some(expected) {
                return Err(RevisionConflict.into());
            }
        }

        let first = expected == MISSING_REVISION;
        match publish(directory, CURRENT, bytes, first) {
            Err(error)
                if first
                    && error.downcast_ref::<rustix::io::Errno>()
                        == Some(&rustix::io::Errno::EXIST) =>
            {
                return Err(RevisionConflict.into());
            }
            result => result?,
        }
        Ok(Snapshot {
            bytes: bytes.to_vec(),
            revision: id,
        })
    }
}

pub(super) fn validate_existing_guests(previous: &Config, candidate: &Config) -> Result<()> {
    for (id, service) in &candidate.services {
        let Some(vm) = &service.vm else { continue };
        let Some(old_service) = previous.services.get(id) else {
            continue;
        };
        let Some(old_vm) = &old_service.vm else {
            continue;
        };
        ensure!(
            old_service.backend == service.backend,
            "Existing VM backend cannot be changed."
        );
        ensure!(
            (old_vm.uuid.is_empty() || old_vm.uuid == vm.uuid)
                && (old_vm.mac.is_empty() || old_vm.mac == vm.mac),
            "Existing VM identity must be preserved."
        );
    }
    Ok(())
}

fn lock(directory: &File) -> Result<()> {
    // Each save opens its own directory descriptor. Closing it releases flock.
    loop {
        if unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error).context("could not lock workload store");
        }
    }
}

pub(super) fn open_directory(path: &Path) -> Result<File> {
    let descriptor = fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    verify_directory(descriptor.into())
}

pub(super) fn open_child_directory(parent: &File, name: &str) -> Result<File> {
    let descriptor = fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    verify_directory(descriptor.into())
}

fn verify_directory(directory: File) -> Result<File> {
    let info = directory.metadata()?;
    // geteuid has no preconditions.
    ensure!(
        info.uid() == unsafe { libc::geteuid() } && info.mode() & 0o077 == 0,
        "workload store must be an owner-only directory"
    );
    Ok(directory)
}

pub(super) fn valid_revision(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn current(directory: &File) -> Result<Option<Snapshot>> {
    match read_snapshot(directory, CURRENT) {
        Ok(value) => Ok(Some(value)),
        Err(error)
            if error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn read_snapshot(directory: &File, name: &str) -> Result<Snapshot> {
    let bytes = read_private(directory, name, MAX_CONFIG_BYTES)?;

    Ok(Snapshot {
        revision: revision(&bytes),
        bytes,
    })
}

pub(super) fn read_private(directory: &File, name: &str, limit: usize) -> Result<Vec<u8>> {
    let descriptor = fs::openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let file = File::from(descriptor);
    let info = file.metadata()?;
    ensure!(
        info.is_file()
            && info.nlink() == 1
            && info.mode() & 0o077 == 0
            && info.uid() == unsafe { libc::geteuid() },
        "workload state must be an owner-only regular file"
    );
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "private file exceeds its size limit");
    Ok(bytes)
}

fn remember(directory: &File, previous: &Snapshot) -> Result<()> {
    match fs::mkdirat(directory, HISTORY, Mode::from_bits_truncate(0o700)) {
        Ok(()) => directory.sync_all()?,
        Err(rustix::io::Errno::EXIST) => {}
        Err(error) => return Err(error.into()),
    }
    let history = open_child_directory(directory, HISTORY)?;
    let name = format!("{}.yaml", previous.revision);
    match publish(&history, &name, &previous.bytes, true) {
        Err(error)
            if error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::EXIST) =>
        {
            ensure!(
                read_snapshot(&history, &name)?.bytes == previous.bytes,
                "historical revision differs from its original bytes"
            );
            Ok(())
        }
        result => result,
    }
}

pub(super) fn publish(directory: &File, name: &str, bytes: &[u8], exclusive: bool) -> Result<()> {
    let temp = format!(".workload-{}.tmp", uuid::Uuid::new_v4().simple());
    let descriptor = fs::openat(
        directory,
        &temp,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )?;
    let mut file = File::from(descriptor);
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        if exclusive {
            fs::renameat_with(directory, &temp, directory, name, RenameFlags::NOREPLACE)?;
        } else {
            fs::renameat(directory, &temp, directory, name)?;
        }
        directory.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::unlinkat(directory, temp, fs::AtFlags::empty());
    }
    result
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    fn fixture() -> (tempfile::TempDir, Store) {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::new(directory.path().to_path_buf());
        (directory, store)
    }

    #[test]
    fn save_preserves_exact_bytes_and_rejects_older_revisions() {
        let (directory, store) = fixture();
        let first = b"# original comment\r\nversion: 1\r\n";
        let updated = b"version: 1\n# edited\n";
        assert!(store.read().unwrap().is_none());
        let original = store.save(first, MISSING_REVISION).unwrap();
        assert_eq!(original.config().unwrap().version, 1);
        let latest = store.save(updated, &original.revision).unwrap();
        assert_eq!(store.read().unwrap().unwrap().bytes, updated);
        assert_eq!(store.read_history(&original.revision).unwrap().bytes, first);
        assert_eq!(
            std::fs::metadata(directory.path().join(CURRENT))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        assert!(
            store
                .save(first, &original.revision)
                .err()
                .unwrap()
                .is::<RevisionConflict>()
        );
        assert!(store.save(b"version: 99", &latest.revision).is_err());
        assert_eq!(store.read().unwrap().unwrap().revision, latest.revision);
        assert!(store.read_history("../workloads").is_err());
    }

    #[test]
    fn simultaneous_writers_cannot_both_publish_from_the_same_revision() {
        let (directory, store) = fixture();
        let original = store
            .save(b"version: 1\n", MISSING_REVISION)
            .unwrap()
            .revision;
        let results = std::thread::scope(|scope| {
            let first = scope.spawn(|| store.save(b"version: 1 # first\n", &original));
            let second = scope.spawn(|| {
                Store::new(directory.path().to_path_buf()).save(b"version: 1 # second\n", &original)
            });
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| result
                    .as_ref()
                    .is_err_and(|error| error.is::<RevisionConflict>()))
                .count(),
            1
        );
        assert_eq!(
            store.read_history(&original).unwrap().bytes,
            b"version: 1\n"
        );
    }

    #[test]
    fn invalid_current_yaml_remains_available_for_repair_and_history() {
        let (directory, store) = fixture();
        let path = directory.path().join(CURRENT);
        std::fs::write(&path, b"broken: [").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let prior = store.read().unwrap().unwrap();
        assert!(prior.config().is_err());
        store.save(b"version: 1\n", &prior.revision).unwrap();
        assert_eq!(
            store.read_history(&prior.revision).unwrap().bytes,
            b"broken: ["
        );
    }

    #[test]
    fn private_store_refuses_symlinks_and_public_files() {
        let (directory, store) = fixture();
        let path = directory.path().join(CURRENT);
        let target = directory.path().join("other");
        std::fs::write(&target, b"version: 1\n").unwrap();
        symlink(&target, &path).unwrap();
        assert!(store.read().is_err());
        assert!(store.save(b"version: 1\n", MISSING_REVISION).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&target, &path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.read().is_err());
    }

    #[test]
    fn existing_vm_identity_cannot_be_replaced_or_omitted() {
        let (_directory, store) = fixture();
        let yaml = r#"
version: 1
bindings:
  lan: {kind: bridge, name: br0}
services:
  example:
    kind: vm
    backend: incus
    vm: {image: images:debian/12, cpus: 2, memory_mib: 512, network: lan, mac: "02:01:02:03:04:05"}
"#;
        let original = store.save(yaml.as_bytes(), MISSING_REVISION).unwrap();
        for candidate in [
            yaml.replace("02:01:02:03:04:05", "02:01:02:03:04:06"),
            yaml.replace(r#", mac: "02:01:02:03:04:05""#, ""),
            yaml.replace("backend: incus", "backend: libvirt"),
        ] {
            assert!(
                store
                    .save(candidate.as_bytes(), &original.revision)
                    .is_err()
            );
        }
        assert_eq!(store.read().unwrap().unwrap().revision, original.revision);
    }

    #[test]
    fn test_list_history() {
        let (_directory, store) = fixture();
        assert!(store.list_history().unwrap().is_empty());
        let rev1 = store.save(b"version: 1\n", MISSING_REVISION).unwrap();
        assert!(store.list_history().unwrap().is_empty());
        let yaml2 = "version: 1\n# update 1\n";
        let _rev2 = store.save(yaml2.as_bytes(), &rev1.revision).unwrap();
        let history = store.list_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0], rev1.revision);
    }
}
