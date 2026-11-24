use std::{
    fs::{DirBuilder, File},
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
};

use anyhow::{Result, anyhow, ensure};
use rustix::{fs, io::Errno};
use serde::{Deserialize, Serialize};

use super::{
    ApplyMode, ApplyProvider, Change, Config, ObservedResource, RecoveryProvider, Storage, store,
};

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
    async fn preflight_mode(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        _: ApplyMode,
    ) -> Result<()> {
        self.preflight(change, previous, desired).await
    }
    async fn apply_mode(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        _: ApplyMode,
    ) -> Result<()> {
        self.apply(change, previous, desired).await
    }

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
        let base = store::LockedDirectory::try_acquire(base)?;
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
    async fn observe_mode(
        &self,
        change: &Change,
        previous: &Config,
        desired: &Config,
        _: ApplyMode,
    ) -> Result<ObservedResource> {
        self.observe(change, previous, desired).await
    }

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

/// Reviewed new-data provisioning. It is deliberately separate from ordinary
/// apply, and will never recreate a missing path without explicit authorization.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionPlan {
    pub storage: String,
    pub revision: String,
    pub path: PathBuf,
    pub kind: String,
    pub format: String,
    #[serde(rename = "sizeGiB")]
    pub size_gib: u64,
    pub available_bytes: u64,
    pub uid: u32,
    pub gid: u32,
    pub mode: &'static str,
    pub consumers: Vec<String>,
    pub review_token: String,
}

struct PreparedProvision {
    plan: ProvisionPlan,
    parent: File,
    name: String,
    marker: Option<StorageMeta>,
}

impl StorageProvider {
    async fn prepare_provision(
        &self,
        state: &crate::state::AppState,
        store: &super::Store,
        providers: &super::CompositeProvider,
        id: &str,
        revision: &str,
        size_gib: u64,
    ) -> Result<PreparedProvision> {
        use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
        use std::path::Path;
        ensure!(super::id_valid(id) && store::valid_revision(revision), "invalid storage or revision");
        ensure!(store.pending_apply()?.is_none(), "recover the pending apply before provisioning");
        let snapshot = store.read()?.ok_or_else(|| anyhow!("configuration is unavailable"))?;
        ensure!(snapshot.revision == revision, "configuration changed; review again");
        let config = snapshot.config()?;
        let storage = config.storage.get(id).ok_or_else(|| anyhow!("unknown storage reference"))?;
        ensure!(!storage.external && super::path_valid(&storage.path, false) && storage.path != ".",
            "new data requires an owned relative storage path");
        ensure!(storage.source.is_none(), "source image cloning is not yet supported; no destination was created");
        let kind = if storage.kind.is_empty() { "directory" } else { storage.kind.as_str() };
        ensure!(
            (kind == "directory" && size_gib == 0)
                || (kind == "file" && (1..=4096).contains(&size_gib)
                    && matches!(storage.format.as_str(), "raw" | "qcow2")),
            "directory size must be zero; disks require raw/qcow2 and 1–4096 GiB"
        );
        if storage.format == "qcow2" {
            ensure!(!state.runner.is_test_mode() && qemu_img().is_some(),
                "native qemu-img is required for qcow2 provisioning");
        }
        let relative = Path::new(&storage.path);
        let destination = self.base_dir.join(relative);
        let authority = store.path();
        let authority_root = authority.parent().ok_or_else(|| anyhow!("invalid authority path"))?;
        let overlaps = |left: &Path, right: &Path| left.starts_with(right) || right.starts_with(left);
        ensure!(!overlaps(&destination, &authority), "storage overlaps private configuration");
        ensure!(relative.components().all(|part| {
            let name = part.as_os_str().to_string_lossy();
            !name.starts_with(".maestro") && !name.starts_with(".workload-")
                && !name.starts_with(".server-man-")
        }), "storage overlaps reserved generated state");
        for secret in config.secrets.values() {
            ensure!(!overlaps(&destination, &authority_root.join(secret)),
                "storage overlaps a private-file reference");
        }
        for (other_id, other) in &config.storage {
            if other_id != id {
                let other_path = if other.external { PathBuf::from(&other.path) } else { self.base_dir.join(&other.path) };
                ensure!(!overlaps(&destination, &other_path), "storage overlaps another declared data path");
            }
            if let Some(source) = &other.source {
                ensure!(!overlaps(&destination, &authority_root.join(&source.path)),
                    "storage overlaps an initial image asset");
            }
        }

        let parent_path = relative.parent().ok_or_else(|| anyhow!("invalid storage parent"))?;
        let mut parent = self.base(false)?.ok_or_else(|| anyhow!("storage root must already exist"))?;
        for part in parent_path.components() {
            let name = part.as_os_str().to_str().ok_or_else(|| anyhow!("invalid parent name"))?;
            parent = store::open_child_directory(&parent, name)
                .map_err(|_| anyhow!("every parent must already be an owner-only real directory"))?;
        }
        let name = relative.file_name().and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("invalid storage filename"))?.to_owned();
        ensure!(
            matches!(fs::statat(&parent, &name, fs::AtFlags::SYMLINK_NOFOLLOW), Err(Errno::NOENT)),
            "storage path already exists or cannot be verified; existing data is never replaced"
        );
        let info = parent.metadata()?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: fstatvfs initializes the supplied structure on success.
        ensure!(unsafe { libc::fstatvfs(parent.as_raw_fd(), stat.as_mut_ptr()) } == 0,
            "storage parent filesystem is unavailable");
        let stat = unsafe { stat.assume_init() };
        ensure!(stat.f_flag & libc::ST_RDONLY == 0, "storage parent filesystem is read-only");
        let available_bytes = stat.f_bavail.saturating_mul(stat.f_frsize);
        ensure!(available_bytes >= 16 * 1024 * 1024, "storage parent requires at least 16 MiB free");
        if !storage.mount.is_empty() {
            ensure!(super::preflight::expected_mount(
                &self.base_dir.join(parent_path), &storage.mount, state.config.paths.proc_stat.parent(),
            ), "expected filesystem is not mounted; refusing replacement data");
        }

        let mut consumers = Vec::new();
        for (service_id, service) in &config.services {
            let uses_storage = service.containers.values().any(|container| {
                container.mounts.iter().any(|mount| mount.storage == id)
            }) || service.vm.as_ref().is_some_and(|vm| vm.disks.iter().any(|disk| disk == id));
            if !uses_storage { continue; }
            consumers.push(format!("service/{service_id}"));
            let stopped = if state.runner.is_test_mode() {
                !service.enabled
            } else if service.backend == "quadlet" {
                if let Some(provider) = &providers.quadlet {
                    provider.status_details(&config, service_id).await.is_ok_and(|status| status.state == "stopped")
                } else { false }
            } else { false };
            ensure!(stopped, "all referencing services must be verified stopped before provisioning");
        }
        for share in config.shares.values() {
            ensure!(share.storage != id, "referencing shares must be verified unpublished; native ownership verification is not available");
        }
        consumers.sort();
        let uid = unsafe { libc::geteuid() };
        let gid = if info.mode() & 0o2000 != 0 { info.gid() } else { unsafe { libc::getegid() } };
        let marker = if managed_directory(storage) {
            let bytes = serde_json::to_vec(&serde_json::json!({
                "key":format!("storage/{id}"),"definition":storage,
            }))?;
            Some(StorageMeta {
                resource: format!("storage/{id}"),
                fingerprint: super::revision(&bytes),
            })
        } else { None };
        let token = super::revision(&serde_json::to_vec(&serde_json::json!([
            revision,id,destination,kind,storage.format,size_gib,info.dev(),info.ino(),
            storage.mount,uid,gid,consumers,
        ]))?);
        Ok(PreparedProvision {
            plan: ProvisionPlan {
                storage: id.into(), revision: revision.into(), path: destination,
                kind: kind.into(), format: storage.format.clone(), size_gib, available_bytes,
                uid, gid, mode: if kind == "directory" { "0700" } else { "0600" },
                consumers, review_token: token,
            },
            parent, name, marker,
        })
    }

    pub async fn provision_plan(
        &self,
        state: &crate::state::AppState,
        store: &super::Store,
        providers: &super::CompositeProvider,
        id: &str,
        revision: &str,
        size_gib: u64,
    ) -> Result<ProvisionPlan> {
        Ok(self.prepare_provision(state, store, providers, id, revision, size_gib).await?.plan)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn provision_create(
        &self,
        state: &crate::state::AppState,
        store: &super::Store,
        providers: &super::CompositeProvider,
        id: &str,
        revision: &str,
        size_gib: u64,
        token: &str,
        cancellation: &tokio::sync::watch::Receiver<bool>,
    ) -> Result<ProvisionPlan> {
        use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
        let lock_store = store.clone();
        let authority = tokio::task::spawn_blocking(move || lock_store.lock_directory()).await??;
        super::journal::ensure_idle(&authority)?;
        crate::core::linux_config::mutation_ready(&state.config, store)?;
        let prepared = self.prepare_provision(state, store, providers, id, revision, size_gib).await?;
        ensure!(prepared.plan.review_token == token, "provisioning conditions changed; review again");
        let parent = &prepared.parent;
        let temp = format!(".maestro-new-storage-{}", uuid::Uuid::new_v4().simple());
        let directory = prepared.plan.kind == "directory";
        ensure!(!*cancellation.borrow(), "provisioning cancelled before staging");
        let staged = if directory {
            fs::mkdirat(parent, &temp, fs::Mode::from_bits_truncate(0o700))?;
            match store::open_child_directory(parent, &temp) {
                Ok(staged) => staged,
                Err(error) => {
                    // Do not remove a directory whose identity could not be verified.
                    return Err(error.context("staging directory could not be verified; inspect storage parent"));
                }
            }
        } else {
            let descriptor = fs::openat(parent, &temp,
                fs::OFlags::RDWR | fs::OFlags::CREATE | fs::OFlags::EXCL | fs::OFlags::NOFOLLOW | fs::OFlags::CLOEXEC,
                fs::Mode::from_bits_truncate(0o600))?;
            File::from(descriptor)
        };
        let identity = staged.metadata()?;
        let same_staging = || fs::statat(parent, &temp, fs::AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|info| info.st_dev == identity.dev() && info.st_ino == identity.ino());
        let result = async {
            if directory {
                if let Some(marker) = &prepared.marker {
                    store::publish(&staged, MARKER, &serde_json::to_vec(marker)?, true)?;
                }
                staged.sync_all()?;
            } else {
                let bytes = size_gib << 30;
                if prepared.plan.format == "raw" {
                    staged.set_len(bytes)?;
                } else {
                    let program = qemu_img().ok_or_else(|| anyhow!("native qemu-img disappeared"))?;
                    let path = format!("/proc/{}/fd/{}", std::process::id(), staged.as_raw_fd());
                    let size = format!("{size_gib}G");
                    state.runner.capture_bounded(&program, &["create", "-f", "qcow2", "-o", "preallocation=off", &path, &size],
                        std::time::Duration::from_secs(120), 64 * 1024).await?;
                    let metadata = state.runner.capture_bounded(&program, &["info", "--output=json", "-f", "qcow2", &path],
                        std::time::Duration::from_secs(30), 64 * 1024).await?;
                    let image: serde_json::Value = serde_json::from_slice(&metadata)?;
                    ensure!(image["format"] == "qcow2" && image["virtual-size"].as_u64() == Some(bytes)
                        && image.get("backing-filename").is_none(), "new qcow2 disk verification failed");
                }
                staged.sync_all()?;
            }
            let latest = self.prepare_provision(state, store, providers, id, revision, size_gib).await?;
            ensure!(latest.plan.review_token == token, "provisioning conditions changed; review again");
            ensure!(store::current(&authority, &store.filename)?.is_some_and(|snapshot| snapshot.revision == revision),
                "configuration changed; review again");
            ensure!(!*cancellation.borrow(), "provisioning cancelled before publication");
            ensure!(same_staging(), "staging identity changed; refusing publication");
            fs::renameat_with(parent, &temp, parent, &prepared.name, fs::RenameFlags::NOREPLACE)?;
            parent.sync_all().map_err(|_| anyhow!("data was published but parent sync failed; inspect destination before retrying"))?;
            Ok::<_, anyhow::Error>(())
        }.await;
        if result.is_err() && same_staging() {
            if directory {
                if prepared.marker.is_some() {
                    let _ = fs::unlinkat(&staged, MARKER, fs::AtFlags::empty());
                }
                let _ = fs::unlinkat(parent, &temp, fs::AtFlags::REMOVEDIR);
            } else {
                let _ = fs::unlinkat(parent, &temp, fs::AtFlags::empty());
            }
        }
        result?;
        Ok(prepared.plan)
    }
}

fn qemu_img() -> Option<PathBuf> {
    ["/usr/bin/qemu-img", "/usr/local/bin/qemu-img"]
        .into_iter().map(PathBuf::from).find(|path| path.is_file())
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
