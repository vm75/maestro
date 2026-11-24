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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<super::model::DiskSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_virtual_bytes: Option<u64>,
}

struct PreparedProvision {
    plan: ProvisionPlan,
    parent: File,
    name: String,
    marker: Option<StorageMeta>,
    source: Option<SourceImage>,
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
        let kind = if storage.kind.is_empty() { "directory" } else { storage.kind.as_str() };
        ensure!(
            (kind == "directory" && size_gib == 0)
                || (kind == "file" && (1..=4096).contains(&size_gib)
                    && matches!(storage.format.as_str(), "raw" | "qcow2")),
            "directory size must be zero; disks require raw/qcow2 and 1–4096 GiB"
        );
        ensure!(storage.source.is_none() || kind == "file", "only disks may have an initial image");
        if storage.format == "qcow2" || storage.source.is_some() {
            ensure!(!state.runner.is_test_mode() && qemu_img().is_some(),
                "native qemu-img is required for qcow2 or source image provisioning");
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
        let source = match &storage.source {
            Some(source) => Some(SourceImage::inspect(state, authority_root, source, size_gib << 30).await?),
            None => None,
        };
        let token = super::revision(&serde_json::to_vec(&serde_json::json!([
            revision,id,destination,kind,storage.format,size_gib,info.dev(),info.ino(),
            storage.mount,uid,gid,consumers,storage.source,source.as_ref().map(|image| &image.identity),
        ]))?);
        Ok(PreparedProvision {
            plan: ProvisionPlan {
                storage: id.into(), revision: revision.into(), path: destination,
                kind: kind.into(), format: storage.format.clone(), size_gib, available_bytes,
                uid, gid, mode: if kind == "directory" { "0700" } else { "0600" },
                consumers, review_token: token,
                source: storage.source.clone(), source_virtual_bytes: source.as_ref().map(|image| image.virtual_bytes),
            },
            parent, name, marker, source,
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
                if let Some(source) = &prepared.source {
                    let definition = prepared.plan.source.as_ref().ok_or_else(|| anyhow!("source definition missing"))?;
                    source.verify_checksum(&definition.sha256, cancellation).await?;
                    ensure!(!*cancellation.borrow(), "provisioning cancelled before conversion");
                    let program = qemu_img().ok_or_else(|| anyhow!("native qemu-img disappeared"))?;
                    let input = descriptor_path(&source.file);
                    let output = descriptor_path(&staged);
                    state.runner.capture_bounded(&program, &["convert", "-n", "-f", &definition.format, "-O", &prepared.plan.format, &input, &output],
                        std::time::Duration::from_secs(1200), 64 * 1024).await?;
                    source.verify_checksum(&definition.sha256, cancellation).await?;
                    ensure!(inspect_image(state, &staged, &prepared.plan.format).await? == bytes,
                        "converted disk size verification failed");
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

const MAX_IMAGE_BYTES: u64 = 4096 << 30;

struct SourceImage {
    file: File,
    virtual_bytes: u64,
    identity: serde_json::Value,
}

fn descriptor_path(file: &File) -> String {
    use std::os::fd::AsRawFd;
    format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd())
}

fn check_image_header(file: &File, format: &str) -> Result<()> {
    use std::os::unix::fs::FileExt;
    if format == "raw" { return Ok(()); }
    ensure!(format == "qcow2", "unsupported disk image format");
    let mut header = [0u8; 104];
    file.read_exact_at(&mut header[..72], 0)?;
    let version = u32::from_be_bytes(header[4..8].try_into()?);
    ensure!(header[..4] == [0x51, 0x46, 0x49, 0xfb] && matches!(version, 2 | 3),
        "unsupported qcow2 header");
    ensure!(header[8..20].iter().all(|byte| *byte == 0)
        && header[32..36].iter().all(|byte| *byte == 0),
        "backed or encrypted qcow2 images are not supported");
    if version == 3 {
        file.read_exact_at(&mut header[72..], 72)?;
        ensure!(header[72..80].iter().all(|byte| *byte == 0),
            "qcow2 incompatible features, including external data files, are not supported");
    }
    Ok(())
}

async fn inspect_image(state: &crate::state::AppState, file: &File, format: &str) -> Result<u64> {
    check_image_header(file, format)?;
    let program = qemu_img().ok_or_else(|| anyhow!("native qemu-img is unavailable"))?;
    let path = descriptor_path(file);
    let output = state.runner.capture_bounded(&program, &["info", "--output=json", "-f", format, &path],
        std::time::Duration::from_secs(30), 64 * 1024).await?;
    let info: serde_json::Value = serde_json::from_slice(&output)?;
    ensure!(info["format"] == format && info.get("backing-filename").is_none()
        && info.get("full-backing-filename").is_none(),
        "source image format or backing file verification failed");
    let size = info["virtual-size"].as_u64().ok_or_else(|| anyhow!("image virtual size unavailable"))?;
    ensure!((1..=MAX_IMAGE_BYTES).contains(&size), "image virtual size out of range");
    Ok(size)
}

impl SourceImage {
    async fn inspect(state: &crate::state::AppState, root: &std::path::Path,
        source: &super::model::DiskSource, capacity: u64) -> Result<Self> {
        use std::{os::unix::fs::MetadataExt, path::Path};
        ensure!(super::path_valid(&source.path, false) && source.path != ".",
            "source image must be a relative asset");
        ensure!(matches!(source.format.as_str(), "raw" | "qcow2")
            && store::valid_revision(&source.sha256), "invalid image format or checksum");
        let path = Path::new(&source.path);
        let mut parent = store::open_directory(root)?;
        let uid = unsafe { libc::geteuid() };
        for part in path.parent().ok_or_else(|| anyhow!("invalid source parent"))?.components() {
            let name = part.as_os_str();
            let fd = fs::openat(&parent, name,
                fs::OFlags::RDONLY | fs::OFlags::DIRECTORY | fs::OFlags::NOFOLLOW | fs::OFlags::CLOEXEC,
                fs::Mode::empty())?;
            parent = File::from(fd);
            let info = parent.metadata()?;
            ensure!((info.uid() == uid || info.uid() == 0) && info.mode() & 0o022 == 0,
                "source directory is not owned and protected");
        }
        let fd = fs::openat(&parent, path.file_name().ok_or_else(|| anyhow!("invalid source name"))?,
            fs::OFlags::RDONLY | fs::OFlags::NOFOLLOW | fs::OFlags::NONBLOCK | fs::OFlags::CLOEXEC,
            fs::Mode::empty())?;
        let file = File::from(fd);
        let info = file.metadata()?;
        ensure!(info.is_file() && (1..=MAX_IMAGE_BYTES).contains(&info.len())
            && info.nlink() == 1 && (info.uid() == uid || info.uid() == 0)
            && info.mode() & 0o022 == 0, "source must be a protected regular image file");
        let virtual_bytes = inspect_image(state, &file, &source.format).await?;
        ensure!(virtual_bytes <= capacity, "source image exceeds requested disk capacity");
        let identity = serde_json::json!([info.dev(),info.ino(),info.len(),info.mtime(),
            info.mtime_nsec(),info.ctime(),info.ctime_nsec(),virtual_bytes]);
        Ok(Self { file, virtual_bytes, identity })
    }

    async fn verify_checksum(&self, expected: &str, cancellation: &tokio::sync::watch::Receiver<bool>) -> Result<()> {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::FileExt;
        let file = self.file.try_clone()?;
        let expected = expected.to_owned();
        let cancellation = cancellation.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut hash = Sha256::new();
            let mut offset = 0u64;
            let mut buffer = vec![0u8; 1024 * 1024];
            loop {
                ensure!(!*cancellation.borrow(), "provisioning cancelled while verifying source");
                let read = file.read_at(&mut buffer, offset)?;
                if read == 0 { break; }
                offset += read as u64;
                ensure!(offset <= MAX_IMAGE_BYTES, "source image exceeds checksum limit");
                hash.update(&buffer[..read]);
            }
            ensure!(format!("{:x}", hash.finalize()) == expected, "source image checksum mismatch");
            Ok(())
        }).await?
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

    #[test]
    fn qcow2_header_rejects_backing_and_external_data() {
        use std::io::Write;
        let mut file = tempfile::tempfile().unwrap();
        let mut header = [0u8; 104];
        header[..4].copy_from_slice(&[0x51, 0x46, 0x49, 0xfb]);
        header[4..8].copy_from_slice(&3u32.to_be_bytes());
        file.write_all(&header).unwrap();
        assert!(check_image_header(&file, "qcow2").is_ok());
        use std::os::unix::fs::FileExt;
        file.write_all_at(&[1], 79).unwrap();
        assert!(check_image_header(&file, "qcow2").is_err());
        file.write_all_at(&[0], 79).unwrap();
        file.write_all_at(&[1], 15).unwrap();
        assert!(check_image_header(&file, "qcow2").is_err());
    }

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
