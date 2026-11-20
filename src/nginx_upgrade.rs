//! Router-only, command-line Entware NGINX upgrades from GitHub Releases.
//! No HTTP route exposes this operation.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::{
    backup,
    config::{AppConfig, Platform},
};

pub const RELEASES_API: &str = "https://api.github.com/repos/vm75/router-hub/releases?per_page=30";
pub const TAG_API_BASE: &str = "https://api.github.com/repos/vm75/router-hub/releases/tags";
pub const RELEASE_BASE: &str = "https://github.com/vm75/router-hub/releases/download";
pub const ARCH: &str = "aarch64-3.10";
pub const OPKG: &str = "/opt/bin/opkg";
pub const MAX_IPK_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_CHECKSUM_BYTES: u64 = 64 * 1024;

pub const REQUIRED_PACKAGES: [&str; 4] = [
    "nginx-ssl",
    "nginx-mod-brotli",
    "nginx-mod-stream",
    "nginx-mod-headers-more",
];

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    clap::ValueEnum,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum NginxChannel {
    #[default]
    Stable,
    Mainline,
}

impl std::fmt::Display for NginxChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stable => write!(f, "stable"),
            Self::Mainline => write!(f, "mainline"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UpgradeNginxOptions {
    pub channel: NginxChannel,
    pub tag: Option<String>,
    pub version: Option<String>,
    pub download_only: bool,
    pub no_backup: bool,
    pub force: bool,
    pub check: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NginxTagInfo {
    pub tag: String,
    pub version: [u64; 3],
    pub version_str: String,
    pub run_id: Option<u64>,
    pub attempt: Option<u64>,
    pub channel: NginxChannel,
}

impl Ord for NginxTagInfo {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.version
            .cmp(&other.version)
            .then_with(|| self.run_id.cmp(&other.run_id))
            .then_with(|| self.attempt.cmp(&other.attempt))
    }
}

impl PartialOrd for NginxTagInfo {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

pub fn require_router(platform: Platform, test_mode: bool) -> Result<()> {
    ensure!(
        platform == Platform::Asuswrt && !test_mode,
        "upgrade-nginx is available only on a live Asuswrt-Merlin router, not Linux or test mode"
    );
    Ok(())
}

pub fn parse_version_numbers(version_str: &str) -> Option<[u64; 3]> {
    let mut parts = version_str.split('.');
    let mut numbers = [0u64; 3];
    for number in &mut numbers {
        let part = parts.next()?;
        if part.is_empty()
            || (part.len() > 1 && part.starts_with('0'))
            || !part.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        *number = part.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(numbers)
}

pub fn channel_for_version(version: [u64; 3]) -> NginxChannel {
    if version[1] % 2 == 0 {
        NginxChannel::Stable
    } else {
        NginxChannel::Mainline
    }
}

pub fn parse_nginx_tag(tag: &str) -> Option<NginxTagInfo> {
    let remainder = tag.strip_prefix("nginx-v")?;
    let mut parts = remainder.split('-');
    let version_part = parts.next()?;
    let version = parse_version_numbers(version_part)?;
    let channel = channel_for_version(version);

    let mut run_id = None;
    let mut attempt = None;
    for part in parts {
        if let Some(num) = part.strip_prefix("run") {
            if let Ok(val) = num.parse::<u64>() {
                run_id = Some(val);
            }
        } else if let Some(num) = part.strip_prefix('a') {
            if let Ok(val) = num.parse::<u64>() {
                attempt = Some(val);
            }
        }
    }

    Some(NginxTagInfo {
        tag: tag.to_string(),
        version,
        version_str: version_part.to_string(),
        run_id,
        attempt,
        channel,
    })
}

pub fn parse_installed_nginx_version(output: &str) -> Option<[u64; 3]> {
    for line in output.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(pos) = lower.find("nginx/") {
            let ver_part = &line[pos + "nginx/".len()..];
            let token = ver_part
                .split(|c: char| c.is_whitespace() || c == '(' || c == '/' || c == '-')
                .next()?;
            if let Some(numbers) = parse_version_numbers(token) {
                return Some(numbers);
            }
        }
    }
    None
}

pub fn parse_checksums(content: &str) -> Result<HashMap<String, String>> {
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
        map.insert(filename.to_string(), digest.to_ascii_lowercase());
    }
    Ok(map)
}

pub fn find_package_asset<'a>(
    assets: &'a [Asset],
    pkg_name: &str,
    version_str: &str,
    arch: &str,
) -> Result<&'a Asset> {
    let prefix = format!("{pkg_name}_{version_str}-");
    let suffix = format!("_{arch}.ipk");
    let matches: Vec<&'a Asset> = assets
        .iter()
        .filter(|a| a.name.starts_with(&prefix) && a.name.ends_with(&suffix))
        .collect();
    ensure!(
        !matches.is_empty(),
        "missing required asset for {pkg_name} (version {version_str}, arch {arch})"
    );
    ensure!(
        matches.len() == 1,
        "multiple matching assets found for {pkg_name}: {:?}",
        matches.iter().map(|a| &a.name).collect::<Vec<_>>()
    );
    Ok(matches[0])
}

pub fn find_required_assets<'a>(
    assets: &'a [Asset],
    version_str: &str,
    arch: &str,
) -> Result<Vec<&'a Asset>> {
    let mut result = Vec::new();
    for pkg in REQUIRED_PACKAGES {
        result.push(find_package_asset(assets, pkg, version_str, arch)?);
    }
    Ok(result)
}

pub fn select_release_from_list<'a>(
    releases: &'a [Release],
    target_version: Option<&str>,
    channel: NginxChannel,
) -> Result<(&'a Release, NginxTagInfo)> {
    let mut candidates = Vec::new();
    for release in releases {
        if release.draft || release.prerelease {
            continue;
        }
        let Some(info) = parse_nginx_tag(&release.tag_name) else {
            continue;
        };
        if let Some(ver) = target_version {
            let req_ver =
                parse_version_numbers(ver).context("invalid --version argument: expected X.Y.Z")?;
            if info.version != req_ver {
                continue;
            }
        } else if info.channel != channel {
            continue;
        }
        candidates.push((release, info));
    }

    if candidates.is_empty() {
        if let Some(ver) = target_version {
            bail!("no NGINX release found matching version {ver}");
        } else {
            bail!("no {channel} NGINX releases found on GitHub");
        }
    }

    candidates.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(candidates.pop().unwrap())
}

pub fn check_opkg_architecture() -> Result<bool> {
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

pub fn get_installed_nginx_version(nginx_bin: &Path) -> Option<[u64; 3]> {
    if !nginx_bin.is_file() {
        return None;
    }
    let output = ProcessCommand::new(nginx_bin).arg("-v").output().ok()?;
    let text = if !output.stderr.is_empty() {
        String::from_utf8_lossy(&output.stderr)
    } else {
        String::from_utf8_lossy(&output.stdout)
    };
    parse_installed_nginx_version(&text)
}

pub fn test_nginx_config(
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

pub async fn run(
    config: &AppConfig,
    config_path: &Path,
    options: UpgradeNginxOptions,
) -> Result<()> {
    let opkg_available = check_opkg_architecture()?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("router-hub/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(120))
        .build()?;

    let (release, tag_info) = if let Some(tag) = &options.tag {
        let url = format!("{TAG_API_BASE}/{tag}");
        let rel: Release = client
            .get(&url)
            .send()
            .await?
            .error_for_status()
            .context(format!("unable to fetch release for tag {tag}"))?
            .json()
            .await
            .context("unable to deserialize GitHub release")?;
        let info = parse_nginx_tag(&rel.tag_name)
            .context("specified tag does not match NGINX release format (nginx-vX.Y.Z)")?;
        (rel, info)
    } else {
        let releases: Vec<Release> = client
            .get(RELEASES_API)
            .send()
            .await?
            .error_for_status()
            .context(
                "unable to query GitHub releases (check internet access and GitHub rate limits)",
            )?
            .json()
            .await
            .context("unable to deserialize GitHub releases list")?;
        let (selected, info) =
            select_release_from_list(&releases, options.version.as_deref(), options.channel)?;
        (selected.clone(), info)
    };

    let installed = get_installed_nginx_version(&config.commands.nginx);

    if options.check {
        match installed {
            Some(curr) => {
                let curr_str = format!("{}.{}.{}", curr[0], curr[1], curr[2]);
                println!("Current NGINX: {curr_str}");
                if tag_info.version > curr {
                    println!(
                        "Update available: {} ({} channel, tag: {})",
                        tag_info.version_str, tag_info.channel, tag_info.tag
                    );
                } else {
                    println!(
                        "NGINX is up to date (latest {} release: {}, tag: {}).",
                        tag_info.channel, tag_info.version_str, tag_info.tag
                    );
                }
            }
            None => {
                println!(
                    "NGINX is not detectable at {}.",
                    config.commands.nginx.display()
                );
                println!(
                    "Latest {} release: {} (tag: {})",
                    tag_info.channel, tag_info.version_str, tag_info.tag
                );
            }
        }
        return Ok(());
    }

    if let Some(curr) = installed {
        if tag_info.version <= curr && !options.force && options.tag.is_none() {
            println!(
                "NGINX {}.{}.{} is up to date (latest {}: {}). Use --force to reinstall.",
                curr[0], curr[1], curr[2], tag_info.channel, tag_info.tag
            );
            return Ok(());
        }
    }

    ensure!(
        release.assets.iter().any(|a| a.name == "SHA256SUMS"),
        "release {} does not contain SHA256SUMS; no upgrade was attempted",
        tag_info.tag
    );
    let matched_assets = find_required_assets(&release.assets, &tag_info.version_str, ARCH)?;

    let base_url = format!("{RELEASE_BASE}/{}", tag_info.tag);
    let checksum_url = format!("{base_url}/SHA256SUMS");
    let checksum_bytes = request_bytes(&client, &checksum_url, MAX_CHECKSUM_BYTES)
        .await
        .context("unable to download release checksums (SHA256SUMS)")?;
    let checksum_str = String::from_utf8(checksum_bytes).context("non-UTF-8 SHA256SUMS")?;
    let checksum_map = parse_checksums(&checksum_str)?;

    for asset in &matched_assets {
        ensure!(
            checksum_map.contains_key(&asset.name),
            "SHA256SUMS does not contain checksum for required asset {}",
            asset.name
        );
    }

    // Safety: Verify current configuration syntax before making any changes.
    test_nginx_config(
        &config.commands.nginx,
        &config.nginx.config_path,
        config.nginx.prefix_path.as_deref(),
    )
    .context("existing NGINX configuration test failed; aborting upgrade")?;

    // Pre-upgrade backup.
    if !options.no_backup {
        backup::run(config, config_path, None).await.context(
            "pre-upgrade backup failed; no installation was attempted (use --no-backup to skip)",
        )?;
    }

    // Stage downloads in a private temporary directory.
    let staging_dir = PathBuf::from("/tmp").join(format!(".nginx-upgrade-{}", Uuid::new_v4()));
    tokio::fs::create_dir_all(&staging_dir)
        .await
        .context("unable to create temporary staging directory")?;

    let download_result: Result<()> = async {
        for asset in &matched_assets {
            let expected_checksum = &checksum_map[&asset.name];
            let dest_path = staging_dir.join(&asset.name);
            download_and_verify_asset(
                &client,
                &base_url,
                &asset.name,
                expected_checksum,
                &dest_path,
                MAX_IPK_BYTES,
            )
            .await?;
        }
        Ok(())
    }
    .await;

    if let Err(e) = download_result {
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
        return Err(e);
    }

    let final_dir = PathBuf::from("/tmp").join(format!("nginx-{}", tag_info.tag));
    if final_dir.exists() {
        let _ = tokio::fs::remove_dir_all(&final_dir).await;
    }
    tokio::fs::rename(&staging_dir, &final_dir)
        .await
        .context("unable to finalize staged NGINX package directory")?;

    println!(
        "Verified NGINX {} ({}) packages saved to {}.",
        tag_info.version_str,
        tag_info.channel,
        final_dir.display()
    );

    let manual_cmd = format!("{OPKG} install {}/*.ipk", final_dir.display());
    if options.download_only || !opkg_available || unsafe { libc::geteuid() } != 0 {
        println!("To install, run as root: {manual_cmd}");
        return Ok(());
    }

    println!(
        "Installing NGINX {} ({}) through Entware...",
        tag_info.version_str, tag_info.channel
    );
    let mut opkg_cmd = ProcessCommand::new(OPKG);
    opkg_cmd.arg("install");
    if options.force {
        opkg_cmd.arg("--force-reinstall");
    }
    for asset in &matched_assets {
        opkg_cmd.arg(final_dir.join(&asset.name));
    }
    let output = opkg_cmd.output().context("unable to launch opkg install")?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "opkg install failed:\nstdout: {stdout}\nstderr: {stderr}\nRetry command: {manual_cmd}"
        );
    }

    // Verify configuration with the newly installed binary.
    if let Err(err) = test_nginx_config(
        &config.commands.nginx,
        &config.nginx.config_path,
        config.nginx.prefix_path.as_deref(),
    ) {
        eprintln!("\nWARNING: Configuration syntax test failed under the newly installed NGINX!");
        eprintln!("{err}");
        eprintln!("NGINX service was NOT restarted; existing worker processes remain active.");
        eprintln!(
            "Inspect configuration in {}.",
            config.nginx.root_dir.display()
        );
        eprintln!("A pre-upgrade backup is available in /tmp.");
        bail!("new NGINX failed configuration test; service was not restarted");
    }

    // Restart the service.
    let init_script = config.services.init_dir.join("S80nginx");
    if init_script.is_file() {
        println!("Restarting NGINX via {}...", init_script.display());
        let restart_out = ProcessCommand::new(&init_script).arg("restart").output();
        if let Ok(res) = restart_out {
            if !res.status.success() {
                let stderr = String::from_utf8_lossy(&res.stderr);
                eprintln!("Warning: NGINX init script restart returned non-zero: {stderr}");
            }
        }
    } else {
        println!("Reloading NGINX service...");
        let mut reload_cmd = ProcessCommand::new(&config.commands.nginx);
        if let Some(prefix) = &config.nginx.prefix_path {
            reload_cmd.arg("-p").arg(prefix);
        }
        reload_cmd
            .arg("-s")
            .arg("reload")
            .arg("-c")
            .arg(&config.nginx.config_path);
        let _ = reload_cmd.output();
    }

    println!(
        "Successfully updated NGINX to {} ({} channel).",
        tag_info.version_str, tag_info.channel
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_numbers_valid() {
        assert_eq!(parse_version_numbers("1.30.7"), Some([1, 30, 7]));
        assert_eq!(parse_version_numbers("1.28.0"), Some([1, 28, 0]));
        assert_eq!(parse_version_numbers("0.1.0"), Some([0, 1, 0]));
        assert_eq!(parse_version_numbers("2.0.0"), Some([2, 0, 0]));
    }

    #[test]
    fn parse_version_numbers_invalid() {
        for invalid in [
            "1.30", "1.30.7.1", "1.03.7", "1.30.07", "1.30.a", "", "v1.30.7",
        ] {
            assert_eq!(parse_version_numbers(invalid), None, "tested: {invalid}");
        }
    }

    #[test]
    fn channel_detection() {
        assert_eq!(channel_for_version([1, 30, 7]), NginxChannel::Stable);
        assert_eq!(channel_for_version([1, 28, 0]), NginxChannel::Stable);
        assert_eq!(channel_for_version([1, 26, 2]), NginxChannel::Stable);
        assert_eq!(channel_for_version([1, 29, 1]), NginxChannel::Mainline);
        assert_eq!(channel_for_version([1, 31, 0]), NginxChannel::Mainline);
        assert_eq!(channel_for_version([1, 27, 4]), NginxChannel::Mainline);
    }

    #[test]
    fn parse_nginx_tag_formats() {
        let tag1 = "nginx-v1.30.7-run44684227015-a1";
        let info1 = parse_nginx_tag(tag1).unwrap();
        assert_eq!(info1.tag, tag1);
        assert_eq!(info1.version, [1, 30, 7]);
        assert_eq!(info1.version_str, "1.30.7");
        assert_eq!(info1.run_id, Some(44684227015));
        assert_eq!(info1.attempt, Some(1));
        assert_eq!(info1.channel, NginxChannel::Stable);

        let tag2 = "nginx-v1.31.0";
        let info2 = parse_nginx_tag(tag2).unwrap();
        assert_eq!(info2.version, [1, 31, 0]);
        assert_eq!(info2.run_id, None);
        assert_eq!(info2.attempt, None);
        assert_eq!(info2.channel, NginxChannel::Mainline);

        assert!(parse_nginx_tag("v0.10.3").is_none());
        assert!(parse_nginx_tag("nginx-1.30.7").is_none());
        assert!(parse_nginx_tag("nginx-v1.30").is_none());
    }

    #[test]
    fn nginx_tag_ordering() {
        let tag_old = parse_nginx_tag("nginx-v1.30.6-run10-a1").unwrap();
        let tag_mid1 = parse_nginx_tag("nginx-v1.30.7-run10-a1").unwrap();
        let tag_mid2 = parse_nginx_tag("nginx-v1.30.7-run20-a1").unwrap();
        let tag_mid3 = parse_nginx_tag("nginx-v1.30.7-run20-a2").unwrap();
        let tag_new = parse_nginx_tag("nginx-v1.31.0-run5-a1").unwrap();

        assert!(tag_old < tag_mid1);
        assert!(tag_mid1 < tag_mid2);
        assert!(tag_mid2 < tag_mid3);
        assert!(tag_mid3 < tag_new);
    }

    #[test]
    fn parse_installed_nginx_version_formats() {
        let out1 = "nginx version: nginx/1.30.6\n";
        assert_eq!(parse_installed_nginx_version(out1), Some([1, 30, 6]));

        let out2 = "nginx version: nginx/1.28.0 (Ubuntu)\nbuilt with OpenSSL\n";
        assert_eq!(parse_installed_nginx_version(out2), Some([1, 28, 0]));

        let out3 = "NGINX version: nginx/1.31.0-entware\n";
        assert_eq!(parse_installed_nginx_version(out3), Some([1, 31, 0]));

        let out4 = "command not found: nginx\n";
        assert_eq!(parse_installed_nginx_version(out4), None);
    }

    #[test]
    fn parse_checksums_valid_and_invalid() {
        let text = r#"
84b939dcee71b639bca56219cd94fd6ac9b9669cb876eb5b3ff3aed9e26a53e8  nginx
53bbf44415b63873531cb5cef447573ca7adc4745b578838b7f312ce4b2e1bce *nginx-ssl_1.30.7-99_aarch64-3.10.ipk
# Comment
ad87cc6f9b5b693e0c4bc2e6feda0915dce461e4a780d0d2e207730df826c50f  nginx-mod-brotli_1.30.7-99_aarch64-3.10.ipk
"#;
        let map = parse_checksums(text).unwrap();
        assert_eq!(map.len(), 3);
        assert_eq!(
            map.get("nginx-ssl_1.30.7-99_aarch64-3.10.ipk"),
            Some(&"53bbf44415b63873531cb5cef447573ca7adc4745b578838b7f312ce4b2e1bce".to_string())
        );
        assert_eq!(
            map.get("nginx-mod-brotli_1.30.7-99_aarch64-3.10.ipk"),
            Some(&"ad87cc6f9b5b693e0c4bc2e6feda0915dce461e4a780d0d2e207730df826c50f".to_string())
        );

        assert!(parse_checksums("invalid_hash file.ipk").is_err());
        assert!(
            parse_checksums(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa file extra"
            )
            .is_err()
        );
    }

    #[test]
    fn asset_resolution_all_packages() {
        let assets = vec![
            Asset {
                name: "SHA256SUMS".into(),
            },
            Asset {
                name: "nginx-ssl_1.30.7-99_aarch64-3.10.ipk".into(),
            },
            Asset {
                name: "nginx-mod-brotli_1.30.7-99_aarch64-3.10.ipk".into(),
            },
            Asset {
                name: "nginx-mod-stream_1.30.7-99_aarch64-3.10.ipk".into(),
            },
            Asset {
                name: "nginx-mod-headers-more_1.30.7-99_aarch64-3.10.ipk".into(),
            },
        ];

        let matched = find_required_assets(&assets, "1.30.7", "aarch64-3.10").unwrap();
        assert_eq!(matched.len(), 4);

        // Missing package should fail
        let missing = vec![Asset {
            name: "nginx-ssl_1.30.7-99_aarch64-3.10.ipk".into(),
        }];
        assert!(find_required_assets(&missing, "1.30.7", "aarch64-3.10").is_err());
    }

    #[test]
    fn select_release_filters_by_channel_and_version() {
        let rel1 = Release {
            tag_name: "nginx-v1.30.6-run1-a1".into(),
            draft: false,
            prerelease: false,
            assets: vec![],
        };
        let rel2 = Release {
            tag_name: "nginx-v1.30.7-run2-a1".into(),
            draft: false,
            prerelease: false,
            assets: vec![],
        };
        let rel3 = Release {
            tag_name: "nginx-v1.31.0-run3-a1".into(),
            draft: false,
            prerelease: false,
            assets: vec![],
        };
        let rel_other = Release {
            tag_name: "v0.10.3".into(),
            draft: false,
            prerelease: false,
            assets: vec![],
        };

        let releases = vec![rel1, rel2, rel3, rel_other];

        // Stable selection picks 1.30.7
        let (sel_stable, info_stable) =
            select_release_from_list(&releases, None, NginxChannel::Stable).unwrap();
        assert_eq!(sel_stable.tag_name, "nginx-v1.30.7-run2-a1");
        assert_eq!(info_stable.version, [1, 30, 7]);

        // Mainline selection picks 1.31.0
        let (sel_mainline, info_mainline) =
            select_release_from_list(&releases, None, NginxChannel::Mainline).unwrap();
        assert_eq!(sel_mainline.tag_name, "nginx-v1.31.0-run3-a1");
        assert_eq!(info_mainline.version, [1, 31, 0]);

        // Specific version selection
        let (sel_ver, info_ver) =
            select_release_from_list(&releases, Some("1.30.6"), NginxChannel::Mainline).unwrap();
        assert_eq!(sel_ver.tag_name, "nginx-v1.30.6-run1-a1");
        assert_eq!(info_ver.version, [1, 30, 6]);

        // Non-existent version
        assert!(select_release_from_list(&releases, Some("1.32.0"), NginxChannel::Stable).is_err());
    }

    #[test]
    fn require_router_guard() {
        assert!(require_router(Platform::Asuswrt, false).is_ok());
        assert!(require_router(Platform::Asuswrt, true).is_err());
        assert!(require_router(Platform::Linux, false).is_err());
    }
}
