use std::{
    fs::File,
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rustix::fs::{self, Mode, OFlags, RenameFlags};

use super::{Config, MAX_CONFIG_BYTES, parse, revision};

pub(super) const CURRENT: &str = "workloads.yaml";
const HISTORY: &str = "history";
pub const HISTORY_LIMIT: usize = 8;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub revision: String,
    pub modified_at: chrono::DateTime<chrono::Utc>,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'static str>,
}
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
    pub(super) filename: String,
    pub(super) history_name: String,
}

impl Store {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            filename: CURRENT.into(),
            history_name: HISTORY.into(),
        }
    }

    /// Select the same private YAML path used by Linux startup.
    pub fn at_path(path: &Path) -> Result<Self> {
        ensure!(path.is_absolute(), "configuration path must be absolute");
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("configuration filename must be UTF-8"))?;
        ensure!(
            (filename.ends_with(".yaml") || filename.ends_with(".yml"))
                && filename != "apply-candidate.yaml"
                && !filename.starts_with('.'),
            "invalid configuration filename"
        );
        Ok(Self {
            directory: path.parent().unwrap().to_path_buf(),
            filename: filename.into(),
            history_name: format!("{filename}.history"),
        })
    }

    pub fn path(&self) -> PathBuf {
        self.directory.join(&self.filename)
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
            Ok(directory) => current(&directory, &self.filename),
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
        let history = open_child_directory(&directory, &self.history_name)?;
        let snapshot = read_snapshot(&history, &format!("{id}.yaml"))?;
        ensure!(
            snapshot.revision == id,
            "historical revision does not match its contents"
        );
        Ok(snapshot)
    }

    pub fn list_history(&self) -> Result<Vec<String>> {
        Ok(self
            .history_entries()?
            .into_iter()
            .map(|entry| entry.revision)
            .collect())
    }

    pub fn history_entries(&self) -> Result<Vec<HistoryEntry>> {
        let directory = match self.directory() {
            Ok(directory) => directory,
            Err(error) if missing(&error) => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let history = match open_child_directory(&directory, &self.history_name) {
            Ok(directory) => directory,
            Err(error) if missing(&error) => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut entries = Vec::new();
        for name in history_names(&history, HISTORY_LIMIT, false)? {
            ensure!(
                entries.len() < HISTORY_LIMIT,
                "configuration history entry limit exceeded"
            );
            let revision = name
                .strip_suffix(".yaml")
                .expect("validated history name")
                .to_owned();
            let modified = fs::statat(&history, &name, fs::AtFlags::SYMLINK_NOFOLLOW)
                .ok()
                .and_then(|info| {
                    chrono::DateTime::from_timestamp(info.st_mtime, info.st_mtime_nsec as u32)
                })
                .unwrap_or_else(|| {
                    chrono::DateTime::from_timestamp(-62_135_596_800, 0).expect("Go zero time")
                });
            let (valid, error) = match read_snapshot(&history, &name) {
                Ok(snapshot) if snapshot.revision == revision => match snapshot.config() {
                    Ok(_) => (true, None),
                    Err(_) => (
                        false,
                        Some(
                            "This revision contains invalid configuration; inspect privately for recovery.",
                        ),
                    ),
                },
                _ => (
                    false,
                    Some("History entry is damaged, inaccessible or unsafe."),
                ),
            };
            entries.push(HistoryEntry {
                revision,
                modified_at: modified,
                valid,
                error,
            });
        }
        entries.sort_by(|a, b| {
            b.modified_at
                .cmp(&a.modified_at)
                .then_with(|| a.revision.cmp(&b.revision))
        });
        Ok(entries)
    }

    /// Publish valid YAML if the current exact-byte revision matches the review.
    pub fn save(&self, bytes: &[u8], expected: &str) -> Result<Snapshot> {
        let candidate = parse(bytes)?;
        let directory = self.lock_directory()?;
        super::journal::ensure_idle(&directory)?;
        self.save_locked(&directory, bytes, expected, &candidate)
    }

    pub(super) fn lock_directory(&self) -> Result<LockedDirectory> {
        self.ensure_directory()?;
        let directory = open_directory(&self.directory)?;
        LockedDirectory::acquire(directory)
    }

    pub(super) fn save_locked(
        &self,
        directory: &File,
        bytes: &[u8],
        expected: &str,
        candidate: &Config,
    ) -> Result<Snapshot> {
        let previous = current(directory, &self.filename)?;
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
            remember(directory, &self.history_name, &previous)?;
            let latest = current(directory, &self.filename)?;
            if latest.as_ref().map(|value| value.revision.as_str()) != Some(expected) {
                return Err(RevisionConflict.into());
            }
        }

        let first = expected == MISSING_REVISION;
        match publish(directory, &self.filename, bytes, first) {
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

/// Explicit unlock prevents an inherited/duplicated open-file description from
/// retaining the publication lock after the transaction's owner has finished.
pub(super) struct LockedDirectory(File);
impl LockedDirectory {
    fn acquire(directory: File) -> Result<Self> {
        lock(&directory)?;
        Ok(Self(directory))
    }
    pub(super) fn try_acquire(directory: File) -> Result<Self> {
        fs::flock(&directory, fs::FlockOperation::NonBlockingLockExclusive)?;
        Ok(Self(directory))
    }
}
impl std::ops::Deref for LockedDirectory {
    type Target = File;
    fn deref(&self) -> &File {
        &self.0
    }
}
impl std::os::fd::AsFd for LockedDirectory {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.0)
    }
}
impl Drop for LockedDirectory {
    fn drop(&mut self) {
        let _ = fs::flock(&self.0, fs::FlockOperation::Unlock);
    }
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

pub(super) fn current(directory: &File, filename: &str) -> Result<Option<Snapshot>> {
    match read_snapshot(directory, filename) {
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

fn missing(error: &anyhow::Error) -> bool {
    error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT)
}

fn history_names(directory: &File, limit: usize, allow_unexpected: bool) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut scanned = 0;
    for entry in fs::Dir::read_from(directory)? {
        let entry = entry?;
        let raw = entry.file_name().to_bytes();
        if raw == b"." || raw == b".." {
            continue;
        }
        scanned += 1;
        ensure!(
            scanned <= limit,
            "configuration history scan limit exceeded"
        );
        let name = std::str::from_utf8(raw)
            .ok()
            .filter(|name| name.strip_suffix(".yaml").is_some_and(valid_revision));
        let Some(name) = name else {
            if allow_unexpected {
                continue;
            }
            anyhow::bail!("unexpected configuration history entry");
        };
        names.push(name.to_owned());
    }
    Ok(names)
}

pub(super) fn remember(directory: &File, history_name: &str, previous: &Snapshot) -> Result<()> {
    match fs::mkdirat(directory, history_name, Mode::from_bits_truncate(0o700)) {
        Ok(()) => directory.sync_all()?,
        Err(rustix::io::Errno::EXIST) => {}
        Err(error) => return Err(error.into()),
    }
    let history = open_child_directory(directory, history_name)?;
    let name = format!("{}.yaml", previous.revision);
    match publish(&history, &name, &previous.bytes, true) {
        Err(error)
            if error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::EXIST) =>
        {
            ensure!(
                read_snapshot(&history, &name)?.bytes == previous.bytes,
                "historical revision differs from its original bytes"
            );
            let file = fs::openat(
                &history,
                &name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )?;
            let file = File::from(file);
            let info = file.metadata()?;
            ensure!(
                info.is_file()
                    && info.nlink() == 1
                    && info.uid() == unsafe { libc::geteuid() }
                    && info.mode() & 0o077 == 0,
                "historical revision is not a private regular file"
            );
            let now = fs::Timespec {
                tv_sec: 0,
                tv_nsec: fs::UTIME_NOW,
            };
            fs::futimens(
                &file,
                &fs::Timestamps {
                    last_access: now,
                    last_modification: now,
                },
            )?;
        }
        result => result?,
    }
    let mut previous_entries = Vec::new();
    for entry in history_names(&history, 4096, true)? {
        if entry == name {
            continue;
        }
        let info = fs::statat(&history, &entry, fs::AtFlags::SYMLINK_NOFOLLOW)?;
        previous_entries.push((entry, info.st_mtime, info.st_mtime_nsec));
    }
    previous_entries.sort_by(|a, b| (b.1, b.2).cmp(&(a.1, a.2)).then_with(|| a.0.cmp(&b.0)));
    for (entry, _, _) in previous_entries.into_iter().skip(HISTORY_LIMIT - 1) {
        fs::unlinkat(&history, &entry, fs::AtFlags::empty())?;
    }
    history.sync_all()?;
    Ok(())
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
    fn publication_lock_is_released_even_while_a_duplicate_handle_survives() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let duplicate;
        {
            let locked =
                LockedDirectory::try_acquire(open_directory(root.path()).unwrap()).unwrap();
            duplicate = locked.try_clone().unwrap();
            assert!(LockedDirectory::try_acquire(open_directory(root.path()).unwrap()).is_err());
        }
        let acquired = LockedDirectory::try_acquire(open_directory(root.path()).unwrap()).unwrap();
        assert!(duplicate.metadata().unwrap().is_dir());
        drop(acquired);
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
    fn history_is_bounded_by_recency_and_reused_bases_are_refreshed() {
        let (_directory, store) = fixture();
        let mut revisions = Vec::new();
        let mut current = store.save(b"version: 1\n", MISSING_REVISION).unwrap();
        for index in 0..12 {
            revisions.push(current.revision.clone());
            current = store
                .save(
                    format!("version: 1\n# edit {index}\n").as_bytes(),
                    &current.revision,
                )
                .unwrap();
        }
        let entries = store.history_entries().unwrap();
        assert_eq!(entries.len(), HISTORY_LIMIT);
        assert_eq!(entries[0].revision, revisions[11]);
        assert!(
            entries
                .iter()
                .all(|entry| entry.valid && entry.error.is_none())
        );
        assert!(store.read_history(&revisions[0]).is_err());
        let reused = store.read_history(&revisions[9]).unwrap();
        current = store.save(&reused.bytes, &current.revision).unwrap();
        store
            .save(b"version: 1\n# after reuse\n", &current.revision)
            .unwrap();
        let entries = store.history_entries().unwrap();
        assert_eq!(entries[0].revision, reused.revision);
        assert_eq!(entries.len(), HISTORY_LIMIT);
    }

    #[test]
    fn history_surfaces_damaged_invalid_and_unsafe_entries_without_contents() {
        let (directory, store) = fixture();
        let initial = store.save(b"version: 1\n", MISSING_REVISION).unwrap();
        store
            .save(b"version: 1\n# next\n", &initial.revision)
            .unwrap();
        let path = directory
            .path()
            .join(HISTORY)
            .join(format!("{}.yaml", initial.revision));
        std::fs::write(&path, "secret tampering").unwrap();
        let entries = store.history_entries().unwrap();
        assert!(!entries[0].valid);
        assert!(entries[0].error.unwrap().contains("unsafe"));
        assert!(
            !serde_json::to_string(&entries)
                .unwrap()
                .contains("secret tampering")
        );
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/nonexistent", &path).unwrap();
        assert!(!store.history_entries().unwrap()[0].valid);
        std::fs::remove_file(&path).unwrap();
        let invalid = b"version: 1\nsecret: hidden-value\n";
        let invalid_id = revision(invalid);
        let invalid_path = directory
            .path()
            .join(HISTORY)
            .join(format!("{invalid_id}.yaml"));
        std::fs::write(&invalid_path, invalid).unwrap();
        std::fs::set_permissions(&invalid_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let entries = store.history_entries().unwrap();
        assert!(!entries[0].valid);
        assert!(entries[0].error.unwrap().contains("invalid configuration"));
        assert!(
            !serde_json::to_string(&entries)
                .unwrap()
                .contains("hidden-value")
        );
        std::fs::write(directory.path().join(HISTORY).join("foreign"), "retain").unwrap();
        assert!(store.history_entries().is_err());
        let current = store.read().unwrap().unwrap();
        store
            .save(b"version: 1\n# safe update\n", &current.revision)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.path().join(HISTORY).join("foreign")).unwrap(),
            "retain"
        );
    }

    #[test]
    fn selected_authorities_do_not_share_history() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let first = Store::at_path(&directory.path().join("first.yaml")).unwrap();
        let second = Store::at_path(&directory.path().join("second.yaml")).unwrap();
        let original = first.save(b"version: 1\n", MISSING_REVISION).unwrap();
        first
            .save(b"version: 1\n# first edit\n", &original.revision)
            .unwrap();
        assert!(second.history_entries().unwrap().is_empty());
        let other = second
            .save(b"version: 1\n# second base\n", MISSING_REVISION)
            .unwrap();
        second
            .save(b"version: 1\n# second edit\n", &other.revision)
            .unwrap();
        assert_eq!(first.list_history().unwrap(), [original.revision]);
        assert_eq!(second.list_history().unwrap(), [other.revision]);
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

    #[test]
    fn selected_authority_keeps_atomic_revisions_and_history() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("maestro.yaml");
        let store = Store::at_path(&path).unwrap();
        let first = store.save(b"version: 1\n", MISSING_REVISION).unwrap();
        let second = store
            .save(b"version: 1\n# edited\n", &first.revision)
            .unwrap();
        assert_eq!(store.path(), path);
        assert_eq!(std::fs::read(&path).unwrap(), second.bytes);
        assert!(!directory.path().join(CURRENT).exists());
        assert_eq!(
            store.read_history(&first.revision).unwrap().bytes,
            first.bytes
        );
        assert!(
            store
                .save(b"version: 1\n# stale\n", &first.revision)
                .is_err()
        );
        assert!(Store::at_path(Path::new("relative.yaml")).is_err());
        assert!(Store::at_path(&directory.path().join("apply-candidate.yaml")).is_err());
    }

    #[test]
    fn recovery_is_bound_to_the_selected_authority() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::at_path(&directory.path().join("maestro.yaml")).unwrap();
        let first = store.save(b"version: 1\n", MISSING_REVISION).unwrap();
        let candidate = b"version: 1\n# reviewed\n";
        let plan = store.preview(candidate, &first.revision).unwrap();
        let fingerprint = plan.fingerprint.clone();
        {
            let directory = store.lock_directory().unwrap();
            super::super::journal::ApplyJournal::begin(
                &directory,
                &store.filename,
                plan,
                candidate,
            )
            .unwrap();
        }
        assert_eq!(store.pending_candidate(&fingerprint).unwrap(), candidate);
        let other = Store::at_path(&directory.path().join("other.yaml")).unwrap();
        assert!(other.pending_apply().is_err());
        assert!(other.pending_candidate(&fingerprint).is_err());
        assert!(other.save(b"version: 1\n", MISSING_REVISION).is_err());
        assert!(!other.path().exists());
    }
}
