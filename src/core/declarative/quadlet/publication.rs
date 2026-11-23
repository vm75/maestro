use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};
use rustix::io::Errno;

use crate::core::declarative::{Change, Config, ObservedResource, store};

use super::render_quadlet;

/// Publication for an explicitly configured, owner-only rootless Quadlet directory.
pub struct QuadletArtifacts {
    pub(super) directory: PathBuf,
    pub(super) storage_base: PathBuf,
}

impl QuadletArtifacts {
    pub fn new(directory: PathBuf, storage_base: PathBuf) -> Result<Self> {
        ensure!(
            directory.is_absolute() && storage_base.is_absolute(),
            "Quadlet paths must be absolute"
        );
        Ok(Self {
            directory,
            storage_base,
        })
    }

    pub fn observe(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
    ) -> Result<ObservedResource> {
        let transition = Transition::new(change, previous, desired, &self.storage_base)?;
        transition.observe(&store::open_directory(&self.directory)?)
    }

    /// Publish only when every affected artifact matches the reviewed old bytes.
    /// A crash during a multi-file update can leave a mixed state. The caller
    /// retains its journal and must reconcile that state before retrying.
    pub fn publish(&self, change: &Change, previous: &Config, desired: &Config) -> Result<()> {
        let transition = Transition::new(change, previous, desired, &self.storage_base)?;
        let directory = store::open_directory(&self.directory)?;
        let directory = store::LockedDirectory::try_acquire(directory)?;
        ensure!(
            transition.observe(&directory)? == ObservedResource::Before,
            "Quadlet files differ from the reviewed prior state"
        );
        for (name, bytes) in &transition.after {
            let old = transition.before.get(name);
            if old == Some(bytes) {
                continue;
            }
            ensure!(
                read(&directory, name)?.as_deref() == old.map(String::as_bytes),
                "Quadlet file changed during publication"
            );
            store::publish(&directory, name, bytes.as_bytes(), old.is_none())?;
        }
        for (name, bytes) in &transition.before {
            if !transition.after.contains_key(name) {
                ensure!(
                    read(&directory, name)?.as_deref() == Some(bytes.as_bytes()),
                    "Quadlet file changed before removal"
                );
                rustix::fs::unlinkat(&directory, name, rustix::fs::AtFlags::empty())?;
                directory.sync_all()?;
            }
        }
        ensure!(
            transition.observe(&directory)? == ObservedResource::After,
            "Quadlet publication does not match reviewed contents"
        );
        Ok(())
    }
}

pub(super) struct Transition {
    pub(super) before: BTreeMap<String, String>,
    pub(super) after: BTreeMap<String, String>,
}

impl Transition {
    pub(super) fn new(
        change: &Change,
        previous: &Config,
        desired: &Config,
        root: &Path,
    ) -> Result<Self> {
        let id = change.resource.strip_prefix("service/").unwrap_or_default();
        ensure!(
            change.backend == "podman/quadlet" && crate::core::declarative::id_valid(id),
            "resource is not a Quadlet application"
        );
        let render = |config, fingerprint: Option<&str>| match fingerprint {
            Some(fingerprint) => render_quadlet(config, id, root, fingerprint),
            None => Ok(BTreeMap::new()),
        };
        let before = render(previous, change.before.as_deref())?;
        let after = render(desired, change.after.as_deref())?;
        ensure!(
            before != after,
            "Quadlet change does not modify generated files"
        );
        ensure!(
            after
                .values()
                .all(|file| file.len() <= crate::core::declarative::MAX_CONFIG_BYTES),
            "generated Quadlet artifact exceeds its size limit"
        );
        Ok(Self { before, after })
    }

    fn observe(&self, directory: &File) -> Result<ObservedResource> {
        let mut before = true;
        let mut after = true;
        for name in self.before.keys().chain(
            self.after
                .keys()
                .filter(|name| !self.before.contains_key(*name)),
        ) {
            let bytes = read(directory, name)?;
            before &= bytes.as_deref() == self.before.get(name).map(String::as_bytes);
            after &= bytes.as_deref() == self.after.get(name).map(String::as_bytes);
        }
        Ok(if before {
            ObservedResource::Before
        } else if after {
            ObservedResource::After
        } else {
            ObservedResource::Unknown
        })
    }
}

fn read(directory: &File, name: &str) -> Result<Option<Vec<u8>>> {
    match store::read_private(directory, name, crate::core::declarative::MAX_CONFIG_BYTES) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.downcast_ref::<Errno>() == Some(&Errno::NOENT) => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::declarative::{MISSING_REVISION, Store, parse};
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn fixture() -> (tempfile::TempDir, QuadletArtifacts, Store) {
        let temp = tempfile::tempdir().unwrap();
        let units = temp.path().join("units");
        let state = temp.path().join("state");
        for directory in [&units, &state] {
            std::fs::create_dir(directory).unwrap();
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let files = QuadletArtifacts::new(units, temp.path().to_path_buf()).unwrap();
        (temp, files, Store::new(state))
    }

    const APP: &[u8] =
        b"version: 1\nservices:\n  app: {kind: container, containers: {web: {image: alpine}}}\n";
    const EMPTY: &[u8] = b"version: 1\n";

    #[test]
    fn publishes_updates_and_removes_only_exact_generated_files() {
        let (temp, artifacts, store) = fixture();
        let unrelated = temp.path().join("units/unrelated.container");
        std::fs::write(&unrelated, "foreign").unwrap();

        let empty = parse(EMPTY).unwrap();
        let old = parse(APP).unwrap();
        let plan = store.preview(APP, MISSING_REVISION).unwrap();
        let change = &plan.changes[0];
        assert_eq!(
            artifacts.observe(change, &empty, &old).unwrap(),
            ObservedResource::Before
        );
        artifacts.publish(change, &empty, &old).unwrap();
        assert_eq!(
            artifacts.observe(change, &empty, &old).unwrap(),
            ObservedResource::After
        );
        let saved = store.save(APP, MISSING_REVISION).unwrap();

        let edited = String::from_utf8_lossy(APP).replace("alpine", "alpine:3");
        let new = parse(edited.as_bytes()).unwrap();
        let plan = store.preview(edited.as_bytes(), &saved.revision).unwrap();
        let change = &plan.changes[0];
        artifacts.publish(change, &old, &new).unwrap();
        assert_eq!(
            artifacts.observe(change, &old, &new).unwrap(),
            ObservedResource::After
        );
        let saved = store.save(edited.as_bytes(), &saved.revision).unwrap();

        let plan = store.preview(EMPTY, &saved.revision).unwrap();
        let change = &plan.changes[0];
        artifacts.publish(change, &new, &empty).unwrap();
        assert_eq!(
            artifacts.observe(change, &new, &empty).unwrap(),
            ObservedResource::After
        );
        assert_eq!(
            std::fs::read_dir(temp.path().join("units"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(std::fs::read_to_string(unrelated).unwrap(), "foreign");
    }

    #[test]
    fn refuses_partial_or_foreign_file_sets_and_symlinks() {
        let (temp, artifacts, store) = fixture();
        let plan = store.preview(APP, MISSING_REVISION).unwrap();
        let change = &plan.changes[0];
        let empty = parse(EMPTY).unwrap();
        let new = parse(APP).unwrap();
        let expected =
            render_quadlet(&new, "app", temp.path(), change.after.as_deref().unwrap()).unwrap();
        let (name, contents) = expected.first_key_value().unwrap();
        let file = temp.path().join("units").join(name);
        std::fs::write(&file, contents).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            artifacts.observe(change, &empty, &new).unwrap(),
            ObservedResource::Unknown
        );
        assert!(artifacts.publish(change, &empty, &new).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), *contents);
        std::fs::remove_file(&file).unwrap();
        symlink(temp.path().join("missing"), &file).unwrap();
        assert!(artifacts.publish(change, &empty, &new).is_err());
        assert!(std::fs::symlink_metadata(file).unwrap().is_symlink());
    }
}
