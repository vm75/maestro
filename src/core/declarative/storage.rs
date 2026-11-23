use std::{
    fs::{DirBuilder, File},
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
};

use anyhow::{Result, anyhow, ensure};
use rustix::{fs, io::Errno};
use serde::{Deserialize, Serialize};

use super::{ApplyProvider, Change, Config, ObservedResource, RecoveryProvider, Storage, store};

pub(super) const MARKER: &str = ".maestro-storage.json";

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StorageMeta {
    resource: String,
    fingerprint: String,
}

pub struct StorageProvider {
    pub base_dir: PathBuf,
}

impl StorageProvider {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    fn base(&self, create: bool) -> Result<Option<File>> {
        ensure!(self.base_dir.is_absolute(), "storage base must be absolute");
        if create {
            match DirBuilder::new().mode(0o700).create(&self.base_dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        optional(store::open_directory(&self.base_dir))
    }
}

pub(super) fn managed_directory(storage: &Storage) -> bool {
    !storage.external
        && matches!(storage.kind.as_str(), "" | "directory")
        && storage.source.is_none()
        && storage.mount.is_empty()
        && super::path_valid(&storage.path, false)
        && storage.path != "."
}

struct Transition<'a> {
    path: &'a str,
    before: Option<StorageMeta>,
    after: Option<StorageMeta>,
}

impl<'a> Transition<'a> {
    fn new(change: &Change, previous: &'a Config, desired: &'a Config) -> Result<Self> {
        let id = change.resource.strip_prefix("storage/").unwrap_or_default();
        ensure!(
            change.backend == "storage" && super::id_valid(id),
            "invalid storage resource identity"
        );
        let old = previous.storage.get(id);
        let new = desired.storage.get(id);
        let definition = |storage: Option<&'a Storage>| -> Result<Option<&'a str>> {
            storage
                .map(|storage| {
                    ensure!(
                        managed_directory(storage),
                        "storage provisioning requires a relative managed directory"
                    );
                    Ok(storage.path.as_str())
                })
                .transpose()
        };
        let before_path = definition(old)?;
        let after_path = definition(new)?;
        ensure!(
            before_path.is_none() || after_path.is_none() || before_path == after_path,
            "storage relocation requires a new definition"
        );
        ensure!(
            before_path.is_some() == change.before.is_some()
                && after_path.is_some() == change.after.is_some(),
            "storage revision does not match the transition"
        );
        let meta = |fingerprint: &String| -> Result<StorageMeta> {
            ensure!(
                store::valid_revision(fingerprint),
                "invalid storage fingerprint"
            );
            Ok(StorageMeta {
                resource: change.resource.clone(),
                fingerprint: fingerprint.clone(),
            })
        };
        Ok(Self {
            path: before_path
                .or(after_path)
                .ok_or_else(|| anyhow!("missing storage definition"))?,
            before: change.before.as_ref().map(meta).transpose()?,
            after: change.after.as_ref().map(meta).transpose()?,
        })
    }

    fn observe(&self, directory: Option<&File>) -> Result<ObservedResource> {
        let Some(directory) = directory else {
            return Ok(if self.before.is_none() {
                ObservedResource::Before
            } else {
                ObservedResource::Unknown
            });
        };
        let marker = optional(store::read_private(directory, MARKER, 4096))?
            .map(|data| serde_json::from_slice::<StorageMeta>(&data))
            .transpose()?;
        Ok(
            if self.after == marker && (marker.is_some() || self.before.is_some()) {
                ObservedResource::After
            } else if marker.is_some() && self.before == marker {
                ObservedResource::Before
            } else {
                ObservedResource::Unknown
            },
        )
    }
}

fn optional<T>(result: Result<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.downcast_ref::<Errno>() == Some(&Errno::NOENT) => Ok(None),
        Err(error) => Err(error),
    }
}

fn descend(mut dir: File, path: &str, create: bool) -> Result<Option<File>> {
    let mut components = path.split('/').peekable();
    while let Some(component) = components.next() {
        if create {
            match fs::mkdirat(&dir, component, fs::Mode::from_bits_truncate(0o700)) {
                Ok(()) => dir.sync_all()?,
                Err(Errno::EXIST) if components.peek().is_some() => {}
                Err(error) => return Err(error.into()),
            }
        }
        match optional(store::open_child_directory(&dir, component))? {
            Some(child) => dir = child,
            None => return Ok(None),
        }
    }
    Ok(Some(dir))
}

impl ApplyProvider for StorageProvider {
    async fn preflight(&self, change: &Change, previous: &Config, desired: &Config) -> Result<()> {
        ensure!(
            self.observe(change, previous, desired).await? == ObservedResource::Before,
            "storage differs from the reviewed prior state"
        );
        Ok(())
    }

    async fn apply(&self, change: &Change, previous: &Config, desired: &Config) -> Result<()> {
        let transition = Transition::new(change, previous, desired)?;
        let base = self
            .base(transition.before.is_none())?
            .ok_or_else(|| anyhow!("storage base is missing"))?;
        fs::flock(&base, fs::FlockOperation::NonBlockingLockExclusive)?;
        let existing = descend(base.try_clone()?, transition.path, false)?;
        ensure!(
            transition.observe(existing.as_ref())? == ObservedResource::Before,
            "storage changed before publication"
        );
        let directory = match existing {
            Some(dir) => dir,
            None => descend(base.try_clone()?, transition.path, true)?
                .ok_or_else(|| anyhow!("storage directory could not be created"))?,
        };
        if let Some(marker) = transition.after {
            let data = serde_json::to_vec(&marker)?;
            store::publish(&directory, MARKER, &data, transition.before.is_none())?;
        } else {
            fs::unlinkat(&directory, MARKER, fs::AtFlags::empty())?;
            directory.sync_all()?;
        }
        Ok(())
    }
}

impl RecoveryProvider for StorageProvider {
    async fn observe(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
    ) -> Result<ObservedResource> {
        let transition = Transition::new(change, previous, desired)?;
        let directory = match self.base(false)? {
            Some(dir) => descend(dir, transition.path, false)?,
            None => None,
        };
        transition.observe(directory.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::declarative::{Operation, Storage};
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn test_storage_provider_lifecycle() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let provider = StorageProvider::new(temp.path().to_path_buf());
        let change = Change {
            resource: "storage/data".to_string(),
            backend: "storage".to_string(),
            operation: Operation::Create,
            before: None,
            after: Some("a".repeat(64)),
        };
        let mut config = Config::default();
        config.storage.insert(
            "data".to_string(),
            Storage {
                path: "volumes/for-data".to_string(),
                ..Default::default()
            },
        );
        provider
            .preflight(&change, &Config::default(), &config)
            .await
            .unwrap();
        provider
            .apply(&change, &Config::default(), &config)
            .await
            .unwrap();
        assert!(temp.path().join("volumes/for-data").is_dir());
        assert!(
            temp.path()
                .join("volumes/for-data/.maestro-storage.json")
                .exists()
        );

        let obs = provider
            .observe(&change, &Config::default(), &config)
            .await
            .unwrap();
        assert_eq!(obs, ObservedResource::After);
        let location = temp.path().join("volumes/for-data");
        let payload = location.join("operator-file");
        std::fs::write(&payload, "keep").unwrap();
        let remove = Change {
            resource: change.resource.clone(),
            backend: "storage".into(),
            operation: Operation::Remove,
            before: change.after.clone(),
            after: None,
        };
        provider
            .apply(&remove, &config, &Config::default())
            .await
            .unwrap();
        assert!(
            provider
                .observe(&remove, &config, &Config::default())
                .await
                .unwrap()
                == ObservedResource::After
        );
        assert!(!location.join(MARKER).exists());
        assert_eq!(std::fs::read_to_string(&payload).unwrap(), "keep");
        assert!(
            provider
                .apply(&change, &Config::default(), &config)
                .await
                .is_err()
        );

        let redirect = temp.path().join("redirect");
        std::os::unix::fs::symlink(&location, &redirect).unwrap();
        config.storage.get_mut("data").unwrap().path = "redirect/subdir".into();
        assert!(
            provider
                .apply(&change, &Config::default(), &config)
                .await
                .is_err()
        );
        assert!(!location.join("subdir").exists());
    }
}
