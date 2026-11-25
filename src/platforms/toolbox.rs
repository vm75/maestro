//! Router-only installation of complete Toolbox Entware release bundles.
//! No HTTP route exposes package installation.

use std::{
    cmp::Ordering,
    collections::HashMap,
    ffi::CString,
    fs,
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, symlink},
        },
    },
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Output},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, process::Command as AsyncCommand, time::timeout};
use uuid::Uuid;

use crate::{
    backup,
    config::{AppConfig, Platform},
};

const RELEASES_API: &str = "https://api.github.com/repos/vm75/toolbox/releases?per_page=100";
const TAG_API_BASE: &str = "https://api.github.com/repos/vm75/toolbox/releases/tags";
const RELEASE_BASE: &str = "https://github.com/vm75/toolbox/releases/download";
const ARCH: &str = "aarch64-3.10";
const OPKG: &str = "/opt/bin/opkg";
const MAX_RELEASE_PAGES: usize = 10;
const MAX_IPK_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CHECKSUM_BYTES: u64 = 64 * 1024;
const MAX_RELEASE_BYTES: u64 = 8 * 1024 * 1024;
const OPKG_TIMEOUT: Duration = Duration::from_secs(300);
const RELOAD_TIMEOUT: Duration = Duration::from_secs(20);
const MIN_FREE_HEADROOM_BYTES: u64 = 16 * 1024 * 1024;
const MODULES_DIR: &str = "/opt/lib/nginx/modules";
const MODULE_CONFIG_DIR: &str = "/opt/etc/nginx/module.d";
const NGINX_PACKAGES: [&str; 4] = [
    "nginx-ssl",
    "nginx-mod-brotli",
    "nginx-mod-stream",
    "nginx-mod-headers-more",
];

#[derive(Debug, clap::Subcommand)]
pub enum ToolboxCommand {
    /// List the latest version of each compatible tool.
    List {
        /// Show every available version.
        #[arg(long)]
        all: bool,
    },
    /// Install a tool and all of its packages.
    Install {
        /// Tool name or exact name-version (see toolbox list --all).
        #[arg(value_name = "TOOL")]
        selector: String,
        /// Stage the verified bundle without installing.
        #[arg(long)]
        download_only: bool,
        /// Skip the pre-install Maestro backup.
        #[arg(long)]
        no_backup: bool,
        /// Permit replacing a newer installed package with an older version.
        #[arg(long)]
        allow_downgrade: bool,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
struct Asset {
    name: String,
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

pub fn require_router(platform: Platform, test_mode: bool) -> Result<()> {
    ensure!(
        platform == Platform::Asuswrt && !test_mode,
        "toolbox is available only on a live Asuswrt-Merlin router"
    );
    Ok(())
}

fn entware_tag(tag: &str) -> bool {
    tag.starts_with("toolbox-") && tag.contains("-entware-") && validate_asset_name(tag).is_ok()
}

fn package_parts(filename: &str) -> Result<(&str, &str)> {
    validate_asset_name(filename)?;
    let suffix = format!("_{ARCH}.ipk");
    let stem = filename
        .strip_suffix(suffix.as_str())
        .context("incompatible Entware IPK architecture")?;
    let (name, version) = stem.split_once('_').context("invalid IPK filename")?;
    ensure!(
        !name.is_empty() && !version.is_empty() && !version.contains('_'),
        "invalid IPK filename: {filename}"
    );
    Ok((name, version))
}

fn bundle(release: &Release) -> Result<Vec<&Asset>> {
    ensure!(
        !release.draft && !release.prerelease && entware_tag(&release.tag_name),
        "not a published Toolbox Entware release"
    );
    ensure!(
        release
            .assets
            .iter()
            .filter(|a| a.name == "SHA256SUMS")
            .count()
            == 1,
        "release must contain one SHA256SUMS manifest"
    );
    let mut packages = Vec::new();
    let mut names = HashMap::new();
    for asset in &release.assets {
        if !asset.name.ends_with(".ipk") {
            continue;
        }
        let (name, version) = package_parts(&asset.name)?;
        ensure!(
            names.insert(name, version).is_none(),
            "duplicate package in release: {name}"
        );
        ensure!(
            asset.size > 0 && asset.size <= MAX_IPK_BYTES,
            "invalid IPK size: {}",
            asset.name
        );
        packages.push(asset);
    }
    ensure!(!packages.is_empty(), "release contains no compatible IPKs");
    if release.tag_name.starts_with("toolbox-nginx-entware-")
        || NGINX_PACKAGES.iter().any(|name| names.contains_key(*name))
    {
        let version = names.get("nginx-ssl").context("missing nginx-ssl")?;
        for name in NGINX_PACKAGES {
            ensure!(
                names.get(name) == Some(version),
                "missing or mismatched NGINX package: {name}"
            );
        }
    }
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(packages)
}

fn verify_manifest(packages: &[&Asset], checksums: &HashMap<String, String>) -> Result<()> {
    ensure!(
        checksums
            .keys()
            .filter(|name| name.ends_with(".ipk"))
            .count()
            == packages.len(),
        "IPK assets and checksum manifest disagree"
    );
    for asset in packages {
        let checksum = checksums
            .get(&asset.name)
            .with_context(|| format!("missing checksum for {}", asset.name))?;
        if let Some(digest) = &asset.digest {
            ensure!(
                digest.strip_prefix("sha256:") == Some(checksum.as_str()),
                "GitHub digest and manifest disagree for {}",
                asset.name
            );
        }
    }
    Ok(())
}

fn parse_checksums(content: &str) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let digest = parts.next().context("missing digest in checksum line")?;
        let file = parts.next().context("missing filename in checksum line")?;
        ensure!(
            parts.next().is_none(),
            "unexpected extra fields in checksum line: {line}"
        );
        ensure!(
            digest.len() == 64 && digest.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid SHA-256 digest: {digest}"
        );
        let filename = file.trim_start_matches('*');
        validate_asset_name(filename)?;
        ensure!(
            map.insert(filename.to_string(), digest.to_ascii_lowercase())
                .is_none(),
            "duplicate checksum for {filename}"
        );
    }
    Ok(map)
}

fn check_opkg_architecture() -> Result<bool> {
    if !Path::new(OPKG).is_file() {
        return Ok(false);
    }
    let output = ProcessCommand::new(OPKG)
        .arg("print-architecture")
        .output()
        .context("unable to inspect Entware architectures")?;
    ensure!(output.status.success(), "opkg print-architecture failed");
    let printed = String::from_utf8(output.stdout)?;
    ensure!(
        printed.lines().any(|line| {
            let mut fields = line.split_whitespace();
            fields.next() == Some("arch") && fields.next() == Some(ARCH)
        }),
        "this Entware installation does not support {ARCH}; refusing to install an incompatible IPK"
    );
    Ok(true)
}

fn test_nginx_config(
    nginx_bin: &Path,
    config_path: &Path,
    prefix_path: Option<&Path>,
) -> Result<()> {
    if !nginx_bin.is_file() {
        return Ok(());
    }
    let mut cmd = ProcessCommand::new(nginx_bin);
    if let Some(prefix) = prefix_path {
        cmd.arg("-p").arg(prefix);
    }
    cmd.arg("-t").arg("-c").arg(config_path);
    let output = cmd
        .output()
        .context("failed to invoke nginx configuration test")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!("nginx configuration test failed:\n{}{}", stdout, stderr);
    }
    Ok(())
}

async fn request_bytes(client: &reqwest::Client, url: &str, limit: u64) -> Result<Vec<u8>> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    if let Some(length) = response.content_length() {
        ensure!(
            length <= limit,
            "download is larger than the permitted limit"
        );
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() as u64 + chunk.len() as u64 <= limit,
            "download exceeded the permitted limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn download_and_verify_asset(
    client: &reqwest::Client,
    base_url: &str,
    asset_name: &str,
    expected_checksum: &str,
    dest_path: &Path,
    max_bytes: u64,
) -> Result<()> {
    let url = format!("{base_url}/{asset_name}");
    let mut response = client.get(&url).send().await?.error_for_status()?;
    if let Some(length) = response.content_length() {
        ensure!(
            length <= max_bytes,
            "asset {asset_name} size ({length} bytes) exceeds limit ({max_bytes} bytes)"
        );
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(dest_path)
        .await
        .context(format!("unable to create temporary file for {asset_name}"))?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0u64;
    while let Some(chunk) = response.chunk().await? {
        downloaded += chunk.len() as u64;
        ensure!(
            downloaded <= max_bytes,
            "asset {asset_name} exceeded limit ({max_bytes} bytes)"
        );
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    ensure!(downloaded > 0, "downloaded asset {asset_name} is empty");
    file.flush().await?;
    file.sync_all().await?;
    drop(file);
    let actual_checksum = format!("{:x}", hasher.finalize());
    ensure!(
        actual_checksum == expected_checksum,
        "checksum mismatch for {asset_name}: expected {expected_checksum}, got {actual_checksum}"
    );
    Ok(())
}

fn validate_asset_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name != "."
            && name
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'.' | b'-' | b'_' | b'+'))
            && !name.contains(".."),
        "unsafe release asset filename: {name}"
    );
    Ok(())
}

fn available_bytes(path: &Path) -> Result<u64> {
    let path_c = CString::new(path.as_os_str().as_bytes())?;
    let mut data = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let rc = unsafe { libc::statvfs(path_c.as_ptr(), data.as_mut_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot inspect free /tmp space");
    }
    let data = unsafe { data.assume_init() };
    Ok(data.f_bavail.saturating_mul(data.f_frsize))
}

fn ensure_free_space(path: &Path, required: u64) -> Result<()> {
    let free = available_bytes(path)?;
    ensure!(
        free >= required.saturating_add(MIN_FREE_HEADROOM_BYTES),
        "insufficient free space in {}: {} MiB available; {} MiB needed plus 16 MiB headroom",
        path.display(),
        free / 1_048_576,
        required / 1_048_576
    );
    Ok(())
}

/// flock prevents stale lockfiles from blocking retries after a crash.
struct UpgradeLock(fs::File);

impl Drop for UpgradeLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl UpgradeLock {
    fn acquire() -> Result<Self> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open("/tmp/.maestro-toolbox-install.lock")
            .context("unable to open Toolbox install lock")?;
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "another Toolbox installation is already running"
        );
        Ok(Self(file))
    }
}

fn create_private_directory(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .with_context(|| format!("unable to create {}", path.display()))
}

fn copy_node(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        symlink(fs::read_link(source)?, destination)?;
    } else if metadata.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_node(&entry.path(), &destination.join(entry.file_name()))?;
        }
        fs::set_permissions(destination, metadata.permissions())?;
    } else if metadata.is_file() {
        fs::copy(source, destination)?;
        fs::set_permissions(destination, metadata.permissions())?;
    } else {
        bail!("cannot snapshot special file: {}", source.display());
    }
    Ok(())
}

fn remove_node(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

struct RollbackSnapshot {
    directory: PathBuf,
    original_paths: Vec<(PathBuf, bool)>,
}

impl RollbackSnapshot {
    fn capture(paths: &[PathBuf], temp_root: &Path) -> Result<Self> {
        let directory = temp_root.join(format!(".nginx-rollback-{}", Uuid::new_v4()));
        create_private_directory(&directory)?;
        let mut original_paths = Vec::new();
        for original in paths {
            ensure!(original.is_absolute(), "rollback path must be absolute");
            let relative = original.strip_prefix("/")?;
            let saved_path = directory.join(relative);
            let exists = match fs::symlink_metadata(original) {
                Ok(_) => true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(error.into()),
            };
            if exists {
                fs::create_dir_all(saved_path.parent().context("missing snapshot parent")?)?;
                copy_node(original, &saved_path)
                    .with_context(|| format!("cannot snapshot {}", original.display()))?;
            }
            original_paths.push((original.clone(), exists));
        }
        Ok(Self {
            directory,
            original_paths,
        })
    }

    fn restore(&self) -> Result<()> {
        for (original, existed) in &self.original_paths {
            remove_node(original)?;
            if *existed {
                fs::create_dir_all(original.parent().context("missing restore parent")?)?;
                let saved_path = self.directory.join(original.strip_prefix("/")?);
                copy_node(&saved_path, original)
                    .with_context(|| format!("cannot restore {}", original.display()))?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NginxMasterIdentity {
    pid: i32,
    start_ticks: u64,
    exe_device: u64,
    exe_inode: u64,
}

/// Field 22 of /proc/<pid>/stat is the process start time in clock ticks.
/// Parse after the closing parenthesis: the comm field may contain spaces.
fn parse_proc_start_ticks(stat: &str) -> Option<u64> {
    let (_, fields) = stat.rsplit_once(") ")?;
    let mut fields = fields.split_whitespace();
    let state = fields.next()?;
    if matches!(state, "Z" | "X" | "x") {
        return None;
    }
    fields
        .nth(18)?
        .parse::<u64>()
        .ok()
        .filter(|ticks| *ticks > 0)
}

fn is_nginx_master_title(cmdline: &[u8]) -> bool {
    let title = cmdline.split(|byte| *byte == 0).next().unwrap_or(&[]);
    title == b"nginx: master process" || title.starts_with(b"nginx: master process ")
}

/// Read the identity from procfs, independently of the PID file. Comparing
/// start ticks and executable inode prevents a reused PID from passing a
/// second check, even if another NGINX master later receives the same PID.
fn nginx_master_identity_at(
    proc_root: &Path,
    pid: i32,
    nginx_binary: &Path,
) -> Result<NginxMasterIdentity> {
    ensure!(pid > 1, "invalid NGINX master PID {pid}");
    let process = proc_root.join(pid.to_string());
    let comm = fs::read_to_string(process.join("comm"))
        .with_context(|| format!("cannot inspect process {pid} name"))?;
    ensure!(comm.trim() == "nginx", "PID {pid} is not an NGINX process");

    let cmdline = fs::read(process.join("cmdline"))
        .with_context(|| format!("cannot inspect process {pid} command line"))?;
    ensure!(
        is_nginx_master_title(&cmdline),
        "PID {pid} is not an NGINX master process"
    );

    let stat = fs::read_to_string(process.join("stat"))
        .with_context(|| format!("cannot inspect process {pid} start time"))?;
    let start_ticks = parse_proc_start_ticks(&stat)
        .with_context(|| format!("PID {pid} has invalid or exited process state"))?;

    // The old executable may be unlinked by opkg and appear with a
    // " (deleted)" suffix. Its path must still match this configured NGINX.
    let running_exe = fs::read_link(process.join("exe"))
        .with_context(|| format!("cannot inspect process {pid} executable"))?;
    let running_exe = running_exe.to_string_lossy();
    let running_exe = running_exe
        .strip_suffix(" (deleted)")
        .unwrap_or(&running_exe);
    let expected_exe = fs::canonicalize(nginx_binary)
        .with_context(|| format!("cannot resolve {}", nginx_binary.display()))?;
    ensure!(
        Path::new(running_exe) == expected_exe.as_path(),
        "PID {pid} executable is not the configured NGINX binary ({})",
        nginx_binary.display()
    );
    let executable = fs::metadata(process.join("exe"))
        .with_context(|| format!("cannot stat process {pid} executable"))?;
    Ok(NginxMasterIdentity {
        pid,
        start_ticks,
        exe_device: executable.dev(),
        exe_inode: executable.ino(),
    })
}

fn nginx_master_identity(pid: i32, nginx_binary: &Path) -> Result<NginxMasterIdentity> {
    nginx_master_identity_at(Path::new("/proc"), pid, nginx_binary)
}

/// nginx -t creates an empty PID file when stopped. Accept that only when
/// procfs confirms no NGINX process exists; malformed nonempty files still fail.
/// Never use an unverified PID to signal a process.
fn nginx_master_from_pid_file_at(
    pid_path: &Path,
    nginx_binary: &Path,
    proc_root: &Path,
) -> Result<Option<NginxMasterIdentity>> {
    let contents = match fs::read_to_string(pid_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("cannot read NGINX PID file"),
    };
    if contents.trim().is_empty() {
        for entry in
            fs::read_dir(proc_root).context("cannot inspect processes for empty NGINX PID file")?
        {
            let entry = entry.context("cannot inspect process directory")?;
            if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
                continue;
            }
            let comm = match fs::read_to_string(entry.path().join("comm")) {
                Ok(comm) => comm,
                // Processes can exit during enumeration.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("cannot inspect process name"),
            };
            ensure!(
                comm.trim() != "nginx",
                "NGINX PID file is empty but an NGINX process exists; refusing to assume it is stopped"
            );
        }
        return Ok(None);
    }
    let pid = contents
        .trim()
        .parse::<i32>()
        .context("invalid NGINX PID file contents")?;
    Ok(Some(nginx_master_identity_at(
        proc_root,
        pid,
        nginx_binary,
    )?))
}

fn nginx_master_from_pid_file(
    pid_path: &Path,
    nginx_binary: &Path,
) -> Result<Option<NginxMasterIdentity>> {
    nginx_master_from_pid_file_at(pid_path, nginx_binary, Path::new("/proc"))
}

fn signal_nginx_master(
    expected: NginxMasterIdentity,
    nginx_binary: &Path,
    signal: i32,
) -> Result<()> {
    let current = nginx_master_identity(expected.pid, nginx_binary)
        .context("NGINX master identity could not be revalidated before signal")?;
    ensure!(
        current == expected,
        "NGINX master PID {} changed identity; refusing to signal a reused PID",
        expected.pid
    );
    // There is no pidfd_send_signal on older Asuswrt kernels. The procfs
    // identity check immediately precedes kill to minimize the remaining race.
    ensure!(
        unsafe { libc::kill(expected.pid, signal) } == 0,
        "cannot signal verified NGINX master {}: {}",
        expected.pid,
        std::io::Error::last_os_error()
    );
    Ok(())
}

fn process_uses_binary(pid: i32, binary: &Path) -> Result<bool> {
    let executable = fs::metadata(format!("/proc/{pid}/exe"))?;
    let installed = fs::metadata(binary)?;
    Ok(executable.dev() == installed.dev() && executable.ino() == installed.ino())
}

async fn wait_for_new_master(
    config: &AppConfig,
    old_master: NginxMasterIdentity,
) -> Result<NginxMasterIdentity> {
    for _ in 0..100 {
        // A partial PID file during master startup is transient; never signal
        // it. Only a fully verified new master can advance this operation.
        if let Ok(Some(candidate)) =
            nginx_master_from_pid_file(&config.nginx.pid_path, &config.commands.nginx)
        {
            if candidate.pid != old_master.pid
                && process_uses_binary(candidate.pid, &config.commands.nginx).unwrap_or(false)
            {
                tokio::time::sleep(Duration::from_secs(1)).await;
                ensure!(
                    nginx_master_identity(candidate.pid, &config.commands.nginx)? == candidate,
                    "new NGINX master identity changed during verification"
                );
                return Ok(candidate);
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    bail!("NGINX did not publish a verified new master PID after SIGUSR2 (20s timeout)")
}

async fn bounded_command(
    mut command: AsyncCommand,
    duration: Duration,
    description: &str,
) -> Result<Output> {
    command.kill_on_drop(true);
    timeout(duration, command.output())
        .await
        .with_context(|| {
            format!(
                "{description} timed out after {} seconds",
                duration.as_secs()
            )
        })?
        .with_context(|| format!("unable to execute {description}"))
}

async fn reload_nginx(config: &AppConfig) -> Result<()> {
    let expected = nginx_master_from_pid_file(&config.nginx.pid_path, &config.commands.nginx)?
        .context("no verified NGINX master is running for reload")?;
    let mut command = AsyncCommand::new(&config.commands.nginx);
    if let Some(prefix) = &config.nginx.prefix_path {
        command.arg("-p").arg(prefix);
    }
    command
        .arg("-s")
        .arg("reload")
        .arg("-c")
        .arg(&config.nginx.config_path);
    let output = bounded_command(command, RELOAD_TIMEOUT, "nginx reload").await?;
    ensure!(
        output.status.success(),
        "nginx reload failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        nginx_master_from_pid_file(&config.nginx.pid_path, &config.commands.nginx)?
            == Some(expected),
        "NGINX master identity changed after reload"
    );
    Ok(())
}

async fn recover_failure(
    config: &AppConfig,
    snapshot: &RollbackSnapshot,
    failure: anyhow::Error,
    was_running: bool,
) -> anyhow::Error {
    eprintln!("NGINX upgrade failed: {failure:#}");
    eprintln!("Restoring previous binary, module files and module load directives...");
    if let Err(restore_error) = snapshot.restore() {
        eprintln!("WARNING: automatic file rollback failed: {restore_error:#}");
        eprintln!("Manual recovery snapshot: {}", snapshot.directory.display());
        return failure.context(format!("rollback also failed: {restore_error:#}"));
    }
    if let Err(test_error) = test_nginx_config(
        &config.commands.nginx,
        &config.nginx.config_path,
        config.nginx.prefix_path.as_deref(),
    ) {
        eprintln!("WARNING: restored NGINX did not pass configuration test: {test_error:#}");
        return failure.context(format!(
            "restored NGINX failed syntax validation: {test_error:#}"
        ));
    }
    if was_running {
        // reload_nginx refuses to target a stale, reused or non-master PID.
        if let Err(reload_error) = reload_nginx(config).await {
            eprintln!("WARNING: restored NGINX could not be reloaded: {reload_error:#}");
            return failure.context(format!("restored NGINX reload failed: {reload_error:#}"));
        }
    }
    eprintln!(
        "Previous NGINX files restored; opkg package records may still describe the new version."
    );
    eprintln!(
        "Keep the recovery files at {} until the system is repaired.",
        snapshot.directory.display()
    );
    failure
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("maestro/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(120))
        .build()?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolRelease {
    name: String,
    version: String,
    tag: String,
}

impl ToolRelease {
    fn label(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }
}

/// Tags identify tools; the primary IPK supplies the actual package version.
/// A multi-package NGINX release is represented by its nginx-ssl version.
fn tool_release(release: &Release) -> Result<ToolRelease> {
    let packages = bundle(release)?;
    let (name, _) = release
        .tag_name
        .strip_prefix("toolbox-")
        .and_then(|tag| tag.split_once("-entware-"))
        .context("invalid Toolbox tool name")?;
    ensure!(!name.is_empty(), "empty Toolbox tool name");
    let primary = if name == "nginx" { "nginx-ssl" } else { name };
    let version = packages
        .iter()
        .filter_map(|asset| package_parts(&asset.name).ok())
        .find_map(|(package, version)| (package == primary).then_some(version))
        .context("release has no primary tool package")?;
    Ok(ToolRelease {
        name: name.into(),
        version: version.into(),
        tag: release.tag_name.clone(),
    })
}

/// Compare package version parts by alternating non-digit and numeric runs.
/// Numeric runs are compared without integer conversion, so 1.10 > 1.9
/// and even arbitrarily large build revisions compare correctly.
fn compare_version_part(mut left: &[u8], mut right: &[u8]) -> Ordering {
    fn order(c: Option<u8>) -> i32 {
        match c {
            Some(b'~') => -1,
            None => 0,
            Some(c) if c.is_ascii_digit() => 0,
            Some(c) if c.is_ascii_alphabetic() => i32::from(c),
            Some(c) => i32::from(c) + 256,
        }
    }

    while !left.is_empty() || !right.is_empty() {
        while left.first().is_some_and(|c| !c.is_ascii_digit())
            || right.first().is_some_and(|c| !c.is_ascii_digit())
        {
            let cmp = order(left.first().copied()).cmp(&order(right.first().copied()));
            if cmp != Ordering::Equal {
                return cmp;
            }
            if left.first().is_some_and(|c| !c.is_ascii_digit()) {
                left = &left[1..];
            }
            if right.first().is_some_and(|c| !c.is_ascii_digit()) {
                right = &right[1..];
            }
        }

        let ln = left.iter().take_while(|c| c.is_ascii_digit()).count();
        let rn = right.iter().take_while(|c| c.is_ascii_digit()).count();
        let l = &left[..ln];
        let r = &right[..rn];
        let l = &l[l.iter().take_while(|&&c| c == b'0').count()..];
        let r = &r[r.iter().take_while(|&&c| c == b'0').count()..];
        let cmp = l.len().cmp(&r.len()).then_with(|| l.cmp(r));
        if cmp != Ordering::Equal {
            return cmp;
        }
        left = &left[ln..];
        right = &right[rn..];
    }
    Ordering::Equal
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    // The supported IPK filename format excludes epoch separators.
    let (lu, lr) = left.rsplit_once('-').unwrap_or((left, "0"));
    let (ru, rr) = right.rsplit_once('-').unwrap_or((right, "0"));
    compare_version_part(lu.as_bytes(), ru.as_bytes())
        .then_with(|| compare_version_part(lr.as_bytes(), rr.as_bytes()))
}

type Catalog = Vec<Vec<ToolRelease>>;

fn add_to_catalog(catalog: &mut Catalog, release: &Release) {
    let Ok(tool) = tool_release(release) else {
        return;
    };
    if let Some(group) = catalog.iter_mut().find(|group| group[0].name == tool.name) {
        // GitHub lists newer releases first. Keep the first build of a
        // particular version so listing and installation resolve identically.
        if !group.iter().any(|entry| entry.version == tool.version) {
            group.push(tool);
        }
    } else {
        catalog.push(vec![tool]);
    }
}

fn sort_catalog(catalog: &mut Catalog) {
    for group in catalog {
        group.sort_by(|a, b| {
            compare_versions(&a.version, &b.version).then_with(|| a.version.cmp(&b.version))
        });
    }
}

fn catalog_labels(catalog: &Catalog, all: bool) -> Vec<String> {
    catalog
        .iter()
        .flat_map(|group| {
            let first = if all { 0 } else { group.len() - 1 };
            group[first..].iter().map(ToolRelease::label)
        })
        .collect()
}

fn select_tool<'a>(catalog: &'a Catalog, selector: &str) -> Result<&'a ToolRelease> {
    let mut matches = catalog.iter().flat_map(|group| {
        group.iter().filter(move |entry| {
            entry.label() == selector || (entry.name == selector && group.last() == Some(*entry))
        })
    });
    let selected = matches.next().with_context(|| {
        format!("Tool or version '{selector}' not found. Run maestro toolbox list --all.")
    })?;
    ensure!(
        matches.next().is_none(),
        "Ambiguous tool or version '{selector}'. Select an exact release tag instead."
    );
    Ok(selected)
}

async fn fetch_catalog(client: &reqwest::Client) -> Result<Catalog> {
    let mut catalog = Vec::new();
    for page in 1..=MAX_RELEASE_PAGES {
        let bytes = request_bytes(
            client,
            &format!("{RELEASES_API}&page={page}"),
            MAX_RELEASE_BYTES,
        )
        .await?;
        let releases: Vec<Release> = serde_json::from_slice(&bytes)?;
        let count = releases.len();
        for release in releases {
            add_to_catalog(&mut catalog, &release);
        }
        if count < 100 {
            sort_catalog(&mut catalog);
            return Ok(catalog);
        }
    }
    bail!(
        "Toolbox release catalog exceeds {MAX_RELEASE_PAGES} pages; cannot determine all versions. Select an exact release tag instead."
    )
}

async fn list(client: &reqwest::Client, all: bool) -> Result<()> {
    let catalog = fetch_catalog(client).await?;
    if catalog.is_empty() {
        println!("No compatible Toolbox tools found.");
    }
    for label in catalog_labels(&catalog, all) {
        println!("{label}");
    }
    Ok(())
}

async fn installed_packages() -> Result<HashMap<String, String>> {
    let mut command = AsyncCommand::new(OPKG);
    command.arg("list-installed");
    let output = bounded_command(command, OPKG_TIMEOUT, "opkg list-installed").await?;
    ensure!(output.status.success(), "opkg list-installed failed");
    let text = String::from_utf8(output.stdout)?;
    Ok(text
        .lines()
        .filter_map(|line| line.split_once(" - "))
        .map(|(name, version)| (name.to_string(), version.to_string()))
        .collect())
}

async fn check_downgrades(packages: &[&Asset], allow_downgrade: bool) -> Result<()> {
    if allow_downgrade {
        return Ok(());
    }
    let installed = installed_packages().await?;
    for asset in packages {
        let (name, version) = package_parts(&asset.name)?;
        if let Some(current) = installed.get(name) {
            if current == version {
                continue;
            }
            let mut command = AsyncCommand::new(OPKG);
            command.args(["compare-versions", current.as_str(), ">>", version]);
            let output = bounded_command(command, OPKG_TIMEOUT, "opkg compare-versions").await?;
            match output.status.code() {
                Some(0) => bail!(
                    "{name}: installed {current} is newer than {version}; use --allow-downgrade"
                ),
                Some(1) => {}
                _ => bail!("opkg could not compare versions for {name}"),
            }
        }
    }
    Ok(())
}

async fn verify_installed(packages: &[&Asset]) -> Result<()> {
    let installed = installed_packages().await?;
    for asset in packages {
        let (name, version) = package_parts(&asset.name)?;
        ensure!(
            installed.get(name).map(String::as_str) == Some(version),
            "opkg did not install the selected version of {name} ({version})"
        );
    }
    Ok(())
}

pub async fn run(config: &AppConfig, config_path: &Path, command: ToolboxCommand) -> Result<()> {
    require_router(config.platform, config.test_mode)?;
    let client = client()?;
    match command {
        ToolboxCommand::List { all } => list(&client, all).await,
        ToolboxCommand::Install {
            selector,
            download_only,
            no_backup,
            allow_downgrade,
        } => {
            install(
                config,
                config_path,
                &client,
                &selector,
                download_only,
                no_backup,
                allow_downgrade,
            )
            .await
        }
    }
}

async fn install(
    config: &AppConfig,
    config_path: &Path,
    client: &reqwest::Client,
    selector: &str,
    download_only: bool,
    no_backup: bool,
    allow_downgrade: bool,
) -> Result<()> {
    validate_asset_name(selector).context("invalid Toolbox tool or version")?;
    // Preserve exact tag selection for existing scripts and specific rebuilds.
    let selected = if entware_tag(selector) {
        None
    } else {
        let catalog = fetch_catalog(client).await?;
        Some(select_tool(&catalog, selector)?.clone())
    };
    let tag = selected
        .as_ref()
        .map_or(selector, |entry| entry.tag.as_str());
    let opkg_available = check_opkg_architecture()?;
    let automatic = !download_only && opkg_available && unsafe { libc::geteuid() } == 0;
    let _lock = if automatic {
        Some(UpgradeLock::acquire()?)
    } else {
        None
    };
    let bytes = request_bytes(client, &format!("{TAG_API_BASE}/{tag}"), MAX_RELEASE_BYTES).await?;
    let release: Release = serde_json::from_slice(&bytes)?;
    ensure!(release.tag_name == tag, "release tag mismatch");
    let packages = bundle(&release)?;
    let identity = tool_release(&release);
    if let Some(expected) = &selected {
        ensure!(
            identity.as_ref().is_ok_and(|actual| actual == expected),
            "Selected Toolbox release changed; run toolbox list again"
        );
    }
    let label = identity.map_or_else(|_| tag.to_string(), |entry| entry.label());
    let nginx = packages
        .iter()
        .any(|asset| package_parts(&asset.name).is_ok_and(|(name, _)| name == "nginx-ssl"));
    if automatic {
        check_downgrades(&packages, allow_downgrade).await?;
        if nginx {
            test_nginx_config(
                &config.commands.nginx,
                &config.nginx.config_path,
                config.nginx.prefix_path.as_deref(),
            )
            .context("existing NGINX configuration test failed")?;
        }
    }

    let expected_size: u64 = packages.iter().map(|asset| asset.size).sum();
    ensure_free_space(Path::new("/tmp"), expected_size)?;
    let base_url = format!("{RELEASE_BASE}/{tag}");
    let checksum_bytes = request_bytes(
        client,
        &format!("{base_url}/SHA256SUMS"),
        MAX_CHECKSUM_BYTES,
    )
    .await?;
    let checksums = parse_checksums(&String::from_utf8(checksum_bytes)?)?;
    verify_manifest(&packages, &checksums)?;

    let staging_dir = PathBuf::from("/tmp").join(format!(".toolbox-{}", Uuid::new_v4()));
    create_private_directory(&staging_dir)?;
    let download_result: Result<()> = async {
        for asset in &packages {
            let path = staging_dir.join(&asset.name);
            download_and_verify_asset(
                client,
                &base_url,
                &asset.name,
                &checksums[&asset.name],
                &path,
                asset.size,
            )
            .await?;
            ensure!(
                fs::metadata(&path)?.len() == asset.size,
                "IPK size mismatch"
            );
        }
        Ok(())
    }
    .await;
    if let Err(error) = download_result {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(error);
    }
    let final_dir = PathBuf::from("/tmp").join(format!("{label}-{}", Uuid::new_v4()));
    if let Err(error) = fs::rename(&staging_dir, &final_dir) {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(error).context("unable to publish verified IPKs");
    }
    let package_paths: Vec<PathBuf> = packages
        .iter()
        .map(|asset| final_dir.join(&asset.name))
        .collect();
    println!(
        "Verified {label}. Packages saved to {}",
        final_dir.display()
    );

    // Explicit release selection includes same-version rebuilds.
    let mut opkg_options = vec!["--force-reinstall"];
    if allow_downgrade {
        opkg_options.push("--force-downgrade");
    }
    let manual_cmd = format!(
        "{OPKG} {} install {}",
        opkg_options.join(" "),
        package_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
    if !no_backup {
        backup::run(config, config_path, None)
            .await
            .context("pre-install backup failed; no packages were installed")?;
    }
    if !automatic {
        println!("To install as root: {manual_cmd}");
        if nginx {
            println!(
                "Manual installation requires NGINX configuration testing and binary activation."
            );
        }
        return Ok(());
    }

    println!("Installing {label}...");
    println!("Manual retry command: {manual_cmd}");
    let mut opkg_cmd = AsyncCommand::new(OPKG);
    opkg_cmd
        .args(&opkg_options)
        .arg("install")
        .args(&package_paths);
    if nginx {
        install_nginx(config, opkg_cmd, &packages).await?;
    } else {
        let output = bounded_command(opkg_cmd, OPKG_TIMEOUT, "opkg install")
            .await
            .context(
                "installation state may be partial; inspect opkg and package hooks before retrying",
            )?;
        ensure!(
            output.status.success(),
            "opkg install failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        verify_installed(&packages).await?;
    }
    println!("Successfully installed {label}.");
    Ok(())
}

async fn install_nginx(
    config: &AppConfig,
    opkg_cmd: AsyncCommand,
    packages: &[&Asset],
) -> Result<()> {
    ensure_free_space(Path::new("/tmp"), 0)?;
    let snapshot_paths = vec![
        config.commands.nginx.clone(),
        PathBuf::from(MODULES_DIR),
        PathBuf::from(MODULE_CONFIG_DIR),
        config.services.init_dir.join("S80nginx"),
    ];
    let snapshot = RollbackSnapshot::capture(&snapshot_paths, Path::new("/tmp"))?;
    println!(
        "Previous NGINX executable/modules preserved at {}",
        snapshot.directory.display()
    );
    println!("If opkg fails, inspect the snapshot; package-manager metadata is not rolled back.");
    let old_master = nginx_master_from_pid_file(&config.nginx.pid_path, &config.commands.nginx)
        .context("NGINX PID file does not identify the configured master; refusing to install")?;
    let was_running = old_master.is_some();

    let opkg_out = match bounded_command(opkg_cmd, OPKG_TIMEOUT, "opkg install").await {
        Ok(result) => result,
        // A timed-out opkg process may leave package hooks running. Never
        // restore files underneath an installer whose state is uncertain.
        Err(error) => {
            return Err(error.context(format!(
                "opkg did not complete; do not overwrite modules until all package processes have exited; recovery snapshot: {}",
                snapshot.directory.display()
            )));
        }
    };
    if !opkg_out.status.success() {
        let error = anyhow!(
            "opkg install failed ({}):\nstdout: {}\nstderr: {}",
            opkg_out.status,
            String::from_utf8_lossy(&opkg_out.stdout),
            String::from_utf8_lossy(&opkg_out.stderr)
        );
        return Err(recover_failure(config, &snapshot, error, was_running).await);
    }
    if let Err(error) = test_nginx_config(
        &config.commands.nginx,
        &config.nginx.config_path,
        config.nginx.prefix_path.as_deref(),
    ) {
        return Err(recover_failure(config, &snapshot, error, was_running).await);
    }

    if !config.commands.nginx.is_file() {
        return Err(recover_failure(
            config,
            &snapshot,
            anyhow!("installed NGINX executable is missing"),
            was_running,
        )
        .await);
    }
    if let Err(error) = verify_installed(packages).await {
        return Err(recover_failure(config, &snapshot, error, was_running).await);
    }

    // HUP/reload only reconfigures the OLD master binary. NGINX's supported
    // live binary upgrade is SIGUSR2: the old master launches the replacement
    // master using inherited sockets; SIGQUIT retires old workers gracefully.
    if let Some(old_master) = old_master {
        // Validate the exact same master seen before opkg. If the PID was
        // reused, do not send a signal to its new owner.
        if let Err(error) = signal_nginx_master(old_master, &config.commands.nginx, libc::SIGUSR2) {
            return Err(recover_failure(config, &snapshot, error, was_running).await);
        }
        // Once USR2 succeeds, do not automatically restore modules: a new
        // master may have started, and changing on-disk modules underneath it
        // risks loading the wrong ABI. Preserve the snapshot for diagnosis.
        let new_master = wait_for_new_master(config, old_master).await.with_context(|| {
            format!(
                "live NGINX binary activation incomplete; inspect error logs and PID files;                  previous binary/modules are at {} (no automatic rollback after SIGUSR2)",
                snapshot.directory.display()
            )
        })?;
        // The old PID may now be in nginx.pid.oldbin, not nginx.pid.
        // Only retire it if its original process identity still matches.
        signal_nginx_master(old_master, &config.commands.nginx, libc::SIGQUIT).with_context(|| {
            format!(
                "new NGINX master PID {} is running, but previous PID {} could not be safely retired; inspect and retire manually",
                new_master.pid, old_master.pid
            )
        })?;
        ensure!(
            nginx_master_identity(new_master.pid, &config.commands.nginx)? == new_master,
            "new NGINX master identity changed after retiring the old master"
        );
        println!(
            "New NGINX master PID {} verified; old master PID {} retiring gracefully.",
            new_master.pid, old_master.pid
        );
    } else {
        println!(
            "NGINX was not running before the upgrade; packages installed, service left stopped."
        );
    }
    println!("NGINX recovery files: {}", snapshot.directory.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, names: &[&str]) -> Release {
        let mut assets: Vec<Asset> = names
            .iter()
            .map(|name| Asset {
                name: (*name).into(),
                size: 123,
                digest: None,
            })
            .collect();
        assets.push(Asset {
            name: "SHA256SUMS".into(),
            size: 100,
            digest: None,
        });
        Release {
            tag_name: tag.into(),
            draft: false,
            prerelease: false,
            assets,
        }
    }

    #[test]
    fn toolbox_release_selection_and_full_bundle() {
        let nginx = release(
            "toolbox-nginx-entware-1.30.8-99_aarch64",
            &[
                "nginx-ssl_1.30.8-99_aarch64-3.10.ipk",
                "nginx-mod-brotli_1.30.8-99_aarch64-3.10.ipk",
                "nginx-mod-stream_1.30.8-99_aarch64-3.10.ipk",
                "nginx-mod-headers-more_1.30.8-99_aarch64-3.10.ipk",
            ],
        );
        assert_eq!(bundle(&nginx).unwrap().len(), 4);
        let mut extended = nginx.clone();
        extended.assets.push(Asset {
            name: "nginx-extra_1.30.8-99_aarch64-3.10.ipk".into(),
            size: 123,
            digest: None,
        });
        assert_eq!(bundle(&extended).unwrap().len(), 5);
        let mut incomplete = nginx.clone();
        incomplete.assets.remove(0);
        assert!(bundle(&incomplete).is_err());
        let mut other = release(
            "toolbox-btop-entware-aarch64-r45389334971-a1",
            &["btop_1.4.8-1_aarch64-3.10.ipk"],
        );
        assert_eq!(bundle(&other).unwrap().len(), 1);
        let vaultwarden = release(
            "toolbox-vaultwarden-entware-aarch64-r45389574741-a1",
            &["vaultwarden_1.0.0-1_aarch64-3.10.ipk"],
        );
        assert_eq!(bundle(&vaultwarden).unwrap().len(), 1);
        let mut draft = vaultwarden.clone();
        draft.draft = true;
        assert!(bundle(&draft).is_err());
        draft.draft = false;
        draft.prerelease = true;
        assert!(bundle(&draft).is_err());
        other.assets[0].name = "btop_1.4.8-1_armv7-3.2.ipk".into();
        assert!(bundle(&other).is_err());
        other.tag_name = "toolbox-btop-android-arm64".into();
        assert!(bundle(&other).is_err());
        assert!(!entware_tag("toolbox-x-entware-../x"));
    }

    #[test]
    fn friendly_catalog_and_selection() {
        let mut catalog = Vec::new();
        for (tag, package) in [
            (
                "toolbox-vaultwarden-entware-aarch64-r100-a1",
                "vaultwarden_1.0.0-1_aarch64-3.10.ipk",
            ),
            (
                "toolbox-btop-entware-aarch64-r200-a1",
                "btop_1.4.10-1_aarch64-3.10.ipk",
            ),
            (
                "toolbox-btop-entware-aarch64-r199-a1",
                "btop_1.4.10-1_aarch64-3.10.ipk",
            ),
            (
                "toolbox-btop-entware-aarch64-r198-a1",
                "btop_1.4.9-10_aarch64-3.10.ipk",
            ),
            (
                "toolbox-vaultwarden-entware-aarch64-r99-a1",
                "vaultwarden_1.0.0-2_aarch64-3.10.ipk",
            ),
            (
                "toolbox-btop-entware-aarch64-r98-a1",
                "btop_1.5.0-1_armv7-3.2.ipk",
            ),
        ] {
            add_to_catalog(&mut catalog, &release(tag, &[package]));
        }
        let nginx = release(
            "toolbox-nginx-entware-1.30.8-99_aarch64",
            &[
                "nginx-ssl_1.30.8-99_aarch64-3.10.ipk",
                "nginx-mod-brotli_1.30.8-99_aarch64-3.10.ipk",
                "nginx-mod-stream_1.30.8-99_aarch64-3.10.ipk",
                "nginx-mod-headers-more_1.30.8-99_aarch64-3.10.ipk",
            ],
        );
        add_to_catalog(&mut catalog, &nginx);
        let mut draft = nginx.clone();
        draft.draft = true;
        assert!(tool_release(&draft).is_err());
        sort_catalog(&mut catalog);
        assert_eq!(
            catalog_labels(&catalog, false),
            ["vaultwarden-1.0.0-2", "btop-1.4.10-1", "nginx-1.30.8-99"]
        );
        assert_eq!(
            catalog_labels(&catalog, true),
            [
                "vaultwarden-1.0.0-1",
                "vaultwarden-1.0.0-2",
                "btop-1.4.9-10",
                "btop-1.4.10-1",
                "nginx-1.30.8-99",
            ]
        );
        assert_eq!(
            select_tool(&catalog, "btop").unwrap().tag,
            "toolbox-btop-entware-aarch64-r200-a1"
        );
        assert_eq!(
            select_tool(&catalog, "vaultwarden-1.0.0-1")
                .unwrap()
                .version,
            "1.0.0-1"
        );
        assert_eq!(select_tool(&catalog, "nginx").unwrap().tag, nginx.tag_name);
        assert!(select_tool(&catalog, "btop-1.5.0-1").is_err());
        assert!(select_tool(&catalog, "unknown").is_err());
    }

    #[test]
    fn package_versions_compare_numerically() {
        for (older, newer) in [
            ("1.4.9-99", "1.4.10-1"),
            ("1.0.0-2", "1.0.0-10"),
            ("1.0.0-9", "1.0.0-100000000000000000000000"),
            ("1.0.0-1", "1.0.1-1"),
            ("1.0.0~rc1-1", "1.0.0-1"),
        ] {
            assert_eq!(compare_versions(older, newer), Ordering::Less);
            assert_eq!(compare_versions(newer, older), Ordering::Greater);
        }
        assert_eq!(compare_versions("1.0-01", "1.0-1"), Ordering::Equal);
    }

    #[test]
    fn manifest_must_cover_the_exact_ipk_set() {
        let release = release(
            "toolbox-btop-entware-1.4.8_aarch64",
            &["btop_1.4.8-1_aarch64-3.10.ipk"],
        );
        let packages = bundle(&release).unwrap();
        let name = &packages[0].name;
        let hash = "a".repeat(64);
        let line = format!("{hash}  {name}\n");
        assert!(verify_manifest(&packages, &parse_checksums(&line).unwrap()).is_ok());
        assert!(parse_checksums(&format!("{line}{line}")).is_err());
        assert!(verify_manifest(&packages, &HashMap::new()).is_err());
        let extra = format!("{line}{hash}  other_1_aarch64-3.10.ipk\n");
        assert!(verify_manifest(&packages, &parse_checksums(&extra).unwrap()).is_err());
    }

    #[test]
    fn rollback_snapshot_restores_binary_modules_and_missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("nginx");
        let modules = dir.path().join("modules");
        let module_link = dir.path().join("module-link");
        let absent = dir.path().join("formerly-absent");
        fs::write(&executable, "old binary").unwrap();
        fs::create_dir(&modules).unwrap();
        fs::write(modules.join("module.so"), "old module").unwrap();
        symlink("modules/module.so", &module_link).unwrap();
        let snapshot = RollbackSnapshot::capture(
            &[
                executable.clone(),
                modules.clone(),
                module_link.clone(),
                absent.clone(),
            ],
            dir.path(),
        )
        .unwrap();

        fs::write(&executable, "new binary").unwrap();
        fs::write(modules.join("module.so"), "new module").unwrap();
        fs::write(modules.join("new.so"), "new extra module").unwrap();
        fs::remove_file(&module_link).unwrap();
        fs::write(&absent, "new file").unwrap();
        snapshot.restore().unwrap();

        assert_eq!(fs::read_to_string(&executable).unwrap(), "old binary");
        assert_eq!(
            fs::read_to_string(modules.join("module.so")).unwrap(),
            "old module"
        );
        assert!(!modules.join("new.so").exists());
        assert_eq!(
            fs::read_link(module_link).unwrap(),
            PathBuf::from("modules/module.so")
        );
        assert!(!absent.exists());
    }

    #[test]
    fn refuses_unsafe_release_filenames() {
        for valid in ["nginx-ssl_1.30.7-99_aarch64-3.10.ipk", "SHA256SUMS"] {
            assert!(validate_asset_name(valid).is_ok());
        }
        for invalid in ["../nginx", "foo/bar.ipk", "evil ipk", "foo\\bar"] {
            assert!(validate_asset_name(invalid).is_err(), "{invalid}");
        }
    }

    #[tokio::test]
    async fn command_execution_reports_nonzero_and_timeout() {
        let mut failed = AsyncCommand::new("sh");
        failed.arg("-c").arg("exit 17");
        let output = bounded_command(failed, Duration::from_secs(2), "test command")
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(17));

        let mut stuck = AsyncCommand::new("sh");
        stuck.arg("-c").arg("sleep 2");
        assert!(
            bounded_command(stuck, Duration::from_millis(50), "test timeout")
                .await
                .is_err()
        );
    }

    fn fake_stat(pid: i32, start: u64, state: &str) -> String {
        let mut fields = vec!["0".to_string(); 19];
        fields[18] = start.to_string();
        format!(
            "{pid} (nginx master with spaces) {state} {}\n",
            fields.join(" ")
        )
    }

    fn create_fake_master(proc_root: &Path, pid: i32, binary: &Path, start: u64) {
        let process = proc_root.join(pid.to_string());
        fs::create_dir_all(&process).unwrap();
        fs::write(process.join("comm"), "nginx\n").unwrap();
        fs::write(
            process.join("cmdline"),
            b"nginx: master process /opt/sbin/nginx\0",
        )
        .unwrap();
        fs::write(process.join("stat"), fake_stat(pid, start, "S")).unwrap();
        symlink(binary, process.join("exe")).unwrap();
    }

    #[test]
    fn empty_pid_file_requires_no_nginx_processes() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("nginx");
        let proc_root = dir.path().join("proc");
        let pid_file = dir.path().join("nginx.pid");
        fs::create_dir(&proc_root).unwrap();
        fs::write(&binary, "nginx executable").unwrap();
        for contents in ["", " \n"] {
            fs::write(&pid_file, contents).unwrap();
            assert!(
                nginx_master_from_pid_file_at(&pid_file, &binary, &proc_root)
                    .unwrap()
                    .is_none()
            );
        }
        create_fake_master(&proc_root, 43210, &binary, 12345);
        assert!(nginx_master_from_pid_file_at(&pid_file, &binary, &proc_root).is_err());
        fs::write(proc_root.join("43210/comm"), "httpd\n").unwrap();
        assert!(
            nginx_master_from_pid_file_at(&pid_file, &binary, &proc_root)
                .unwrap()
                .is_none()
        );
        fs::remove_dir_all(&proc_root).unwrap();
        assert!(nginx_master_from_pid_file_at(&pid_file, &binary, &proc_root).is_err());
    }

    #[test]
    fn parse_master_identity_rejects_stale_or_reused_pids() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("nginx");
        let other_binary = dir.path().join("unrelated");
        fs::write(&binary, "nginx executable").unwrap();
        fs::write(&other_binary, "unrelated executable").unwrap();
        let proc_root = dir.path().join("proc");
        let pid_file = dir.path().join("nginx.pid");
        let pid = 43210;

        assert!(
            nginx_master_from_pid_file_at(&pid_file, &binary, &proc_root)
                .unwrap()
                .is_none()
        );
        fs::write(&pid_file, "garbage").unwrap();
        assert!(nginx_master_from_pid_file_at(&pid_file, &binary, &proc_root).is_err());

        fs::write(&pid_file, pid.to_string()).unwrap();
        assert!(nginx_master_from_pid_file_at(&pid_file, &binary, &proc_root).is_err());

        create_fake_master(&proc_root, pid, &binary, 12345);
        let process = proc_root.join(pid.to_string());
        let original = nginx_master_from_pid_file_at(&pid_file, &binary, &proc_root)
            .unwrap()
            .unwrap();
        assert_eq!(original.pid, pid);
        assert_eq!(original.start_ticks, 12345);
        assert!(original.exe_inode > 0);

        // PID reuse is detected even if the new process runs the same binary.
        fs::write(process.join("stat"), fake_stat(pid, 12346, "S")).unwrap();
        let reused = nginx_master_identity_at(&proc_root, pid, &binary).unwrap();
        assert_ne!(reused, original);

        fs::write(process.join("stat"), fake_stat(pid, 12345, "S")).unwrap();
        fs::write(process.join("cmdline"), b"nginx: worker process\0").unwrap();
        assert!(nginx_master_identity_at(&proc_root, pid, &binary).is_err());

        fs::write(process.join("cmdline"), b"nginx: master process nginx\0").unwrap();
        fs::write(process.join("comm"), "bash\n").unwrap();
        assert!(nginx_master_identity_at(&proc_root, pid, &binary).is_err());

        fs::write(process.join("comm"), "nginx\n").unwrap();
        fs::remove_file(process.join("exe")).unwrap();
        symlink(&other_binary, process.join("exe")).unwrap();
        assert!(nginx_master_identity_at(&proc_root, pid, &binary).is_err());

        fs::remove_file(process.join("exe")).unwrap();
        symlink(&binary, process.join("exe")).unwrap();
        fs::write(process.join("stat"), fake_stat(pid, 12345, "Z")).unwrap();
        assert!(nginx_master_identity_at(&proc_root, pid, &binary).is_err());
    }

    #[test]
    fn parse_master_title_and_stat_require_valid_fields() {
        assert!(is_nginx_master_title(
            b"nginx: master process /opt/sbin/nginx\0"
        ));
        assert!(is_nginx_master_title(b"nginx: master process\0"));
        for invalid in [
            &b"nginx: worker process\0"[..],
            &b"/opt/sbin/nginx -s reload\0"[..],
            &b"nginx: master process-wrapper\0"[..],
            &b""[..],
        ] {
            assert!(!is_nginx_master_title(invalid));
        }
        assert_eq!(
            parse_proc_start_ticks(&fake_stat(123, 4567, "S")),
            Some(4567)
        );
        assert_eq!(parse_proc_start_ticks(&fake_stat(123, 4567, "Z")), None);
        assert_eq!(parse_proc_start_ticks("123 (nginx) S 0"), None);
        assert_eq!(parse_proc_start_ticks("not a proc stat"), None);
    }

    #[test]
    fn require_router_guard() {
        assert!(require_router(Platform::Asuswrt, false).is_ok());
        assert!(require_router(Platform::Asuswrt, true).is_err());
        assert!(require_router(Platform::Linux, false).is_err());
    }
}
