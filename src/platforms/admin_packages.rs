//! Restricted native package primitives for the Linux root-owned helper.
//! The request selects *verbs* and validated native repository package names;
//! distribution, executable, arguments, environment and timeouts are fixed here.
use std::{fs, io, os::unix::fs::{MetadataExt, PermissionsExt}, path::{Path, PathBuf}, process::Stdio, time::Duration};

use anyhow::{Context, Result, ensure, bail};
use tokio::{io::AsyncReadExt, process::{ChildStdout, Command}, time::timeout};

const MAX_OUTPUT: usize = 8192;
const STEP_DEADLINE: Duration = Duration::from_secs(20 * 60);
const PACMAN_DB: &str = "/var/lib/maestro-admin/updates-pacman";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family { Apt, Dnf, Pacman, Zypper }

impl Family {
    fn id(self) -> &'static str {
        match self { Self::Apt => "apt", Self::Dnf => "dnf", Self::Pacman => "pacman", Self::Zypper => "zypper" }
    }
}

struct Manager {
    family: Family,
    distribution: String,
    program: PathBuf,
}

fn trusted_executable(path: &Path) -> Result<PathBuf> {
    // Native managers and their symlinks must ultimately resolve inside the
    // root-controlled system binary directories, never a caller's search path.
    let canonical = fs::canonicalize(path).context("native package executable unavailable")?;
    ensure!(
        canonical.starts_with("/usr/bin/") || canonical.starts_with("/usr/sbin/"),
        "native package executable is outside trusted system directories"
    );
    let parent = canonical.parent().context("missing binary parent")?;
    for ancestor in [Path::new("/"), Path::new("/usr"), parent] {
        let meta = fs::symlink_metadata(ancestor)?;
        ensure!(meta.is_dir() && meta.uid() == 0 && meta.permissions().mode() & 0o022 == 0,
            "native program parent is not root-owned and protected");
    }
    let meta = fs::symlink_metadata(&canonical)?;
    ensure!(
        meta.is_file() && meta.uid() == 0 && meta.permissions().mode() & 0o022 == 0
            && meta.permissions().mode() & 0o111 != 0 && meta.permissions().mode() & 0o6000 == 0,
        "native package binary must be a root-owned executable"
    );
    Ok(canonical)
}

fn os_release() -> Result<(Family, String)> {
    ensure!(!Path::new("/run/ostree-booted").exists(), "image-based OS packages are not supported");
    let content = fs::read("/etc/os-release")?;
    ensure!(content.len() <= 64 * 1024, "os-release exceeds limit");
    let text = std::str::from_utf8(&content)?;
    let mut id = "";
    let mut like = "";
    let mut variant = "";
    for line in text.lines() {
        let Some((key, raw)) = line.split_once('=') else { continue };
        let value = raw.trim().trim_matches(['"', '\'']);
        match key {
            "ID" => id = value,
            "ID_LIKE" => like = value,
            "VARIANT_ID" => variant = value,
            _ => {}
        }
    }
    ensure!(!matches!(variant, "silverblue" | "kinoite" | "sericea" | "onyx"),
        "image-based OS packages are not supported");
    let family = std::iter::once(id).chain(like.split_whitespace()).find_map(|name| {
        match name {
            "ubuntu" | "debian" | "linuxmint" | "pop" => Some(Family::Apt),
            "fedora" | "rhel" | "centos" | "rocky" | "almalinux" | "ol" => Some(Family::Dnf),
            "arch" | "manjaro" | "endeavouros" => Some(Family::Pacman),
            "suse" | "opensuse" | "opensuse-leap" | "opensuse-tumbleweed" | "sles" | "sled" => Some(Family::Zypper),
            _ => None,
        }
    }).context("unsupported native Linux package manager")?;
    Ok((family, id.to_owned()))
}

fn detect() -> Result<Manager> {
    let (family, distribution) = os_release()?;
    let binary = match family {
        Family::Apt => "/usr/bin/apt-get",
        Family::Dnf if Path::new("/usr/bin/dnf").exists() => "/usr/bin/dnf",
        Family::Dnf => "/usr/bin/dnf5",
        Family::Pacman => "/usr/bin/pacman",
        Family::Zypper => "/usr/bin/zypper",
    };
    Ok(Manager { family, distribution, program: trusted_executable(Path::new(binary))? })
}

/// Accepts only bare repository names: no flags, paths, URLs, versions, shell
/// syntax, repository changes or duplicate packages.
pub fn package_names(text: &str) -> Result<Vec<String>> {
    ensure!(text.len() <= 4096, "package list exceeds 4096 bytes");
    let names: Vec<String> = text.split_whitespace().map(str::to_owned).collect();
    ensure!((1..=32).contains(&names.len()), "provide between 1 and 32 package names");
    for (index, name) in names.iter().enumerate() {
        let bytes = name.as_bytes();
        ensure!(
            !bytes.is_empty() && bytes.len() <= 128
                && bytes[0].is_ascii_alphanumeric()
                && bytes.iter().skip(1).all(|byte| byte.is_ascii_alphanumeric() || b"+_.-".contains(byte))
                && !names[..index].contains(name),
            "invalid or duplicate native repository package name"
        );
    }
    Ok(names)
}

fn dependency_packages(family: Family, distribution: &str, target: &str) -> Result<Vec<String>> {
    // Pinned mappings from Server Man's dependencyPackages reference. Package
    // manager policy and native signature checks are never modified.
    let names = match (target, family) {
        ("podman", Family::Apt) =>
            "podman podman-compose uidmap slirp4netns passt netavark aardvark-dns nftables dbus-user-session",
        ("podman", _) => "podman podman-compose passt netavark aardvark-dns nftables",
        ("docker", Family::Apt) if distribution == "ubuntu" => "docker.io docker-compose-v2",
        ("docker", Family::Apt) => "docker.io docker-cli docker-compose",
        ("docker", Family::Dnf) => "moby-engine docker-compose",
        ("docker", _) => "docker docker-compose",
        ("incus", Family::Apt) => "incus qemu-utils ovmf qemu-system-modules-spice dnsmasq-base",
        ("incus", Family::Pacman) => "incus cdrtools",
        ("samba", Family::Apt) => "samba wsdd-server",
        ("samba", _) => "samba wsdd",
        ("nfs", Family::Apt | Family::Zypper) => "nfs-kernel-server",
        ("nfs", _) => "nfs-utils",
        ("virtualization", Family::Apt) =>
            "qemu-system-x86 qemu-utils libvirt-daemon-system libvirt-daemon-driver-qemu libvirt-clients",
        ("virtualization", Family::Dnf) => "qemu-kvm qemu-img libvirt libvirt-client",
        ("virtualization", Family::Pacman) => "qemu-base libvirt",
        ("virtualization", Family::Zypper) => "qemu-x86 qemu-tools libvirt libvirt-client",
        _ => bail!("unsupported dependency group on this native package manager"),
    };
    package_names(names)
}

async fn drain(mut pipe: ChildStdout) -> io::Result<(String, bool)> {
    let mut buffer = [0_u8; 4096];
    let mut tail: Vec<u8> = Vec::with_capacity(MAX_OUTPUT);
    let mut truncated = false;
    loop {
        let n = pipe.read(&mut buffer).await?;
        if n == 0 { break; }
        if tail.len() + n > MAX_OUTPUT {
            let remove = tail.len() + n - MAX_OUTPUT;
            tail.drain(..remove.min(tail.len()));
            truncated = true;
        }
        tail.extend_from_slice(&buffer[..n]);
    }
    Ok((String::from_utf8_lossy(&tail).into_owned(), truncated))
}

async fn step(program: &Path, args: &[String], extra_environment: Option<(&str, &str)>) -> Result<(String, bool, Option<i32>)> {
    // Neither program nor arguments are derived from an unvalidated request.
    let mut process = Command::new(program);
    process.args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", "/root")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("NEEDRESTART_MODE", "l")
        .env("NO_COLOR", "1")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some((name, value)) = extra_environment { process.env(name, value); }
    let mut child = process.spawn().context("could not start native package program")?;
    let stdout = child.stdout.take().context("native package program lacks output")?;
    timeout(STEP_DEADLINE, async {
        let (output, status) = tokio::join!(drain(stdout), child.wait());
        let (text, truncated) = output?;
        Ok::<_, anyhow::Error>((text, truncated, status?.code()))
    }).await.context("native package operation exceeded its deadline")?
}

async fn pacman_refresh() -> Result<(String, bool, Option<i32>)> {
    let checkupdates = trusted_executable(Path::new("/usr/bin/checkupdates"))?;
    let root = Path::new("/var/lib");
    let meta = fs::symlink_metadata(root)?;
    ensure!(meta.uid() == 0 && meta.is_dir() && meta.mode() & 0o022 == 0, "unsafe state directory parent");
    fs::create_dir_all("/var/lib/maestro-admin")?;
    for path in ["/var/lib/maestro-admin", PACMAN_DB] {
        if !Path::new(path).exists() { fs::create_dir(path)?; }
        let m = fs::symlink_metadata(path)?;
        ensure!(m.is_dir() && m.uid() == 0 && m.mode() & 0o077 == 0, "unsafe private Pacman cache");
    }
    step(&checkupdates, &[], Some(("CHECKUPDATES_DB", PACMAN_DB))).await
}

fn refresh_args(family: Family) -> Vec<String> {
    let args: &[&str] = match family {
        Family::Apt => &["-o", "APT::Update::Error-Mode=any", "update"],
        Family::Dnf => &["--assumeyes", "--refresh", "makecache"],
        Family::Zypper => &["--non-interactive", "refresh"],
        Family::Pacman => &[],
    };
    args.iter().map(|s| (*s).to_owned()).collect()
}

fn upgrade_args(family: Family) -> Vec<String> {
    let args: &[&str] = match family {
        Family::Apt => &["--assume-yes", "--no-remove", "--with-new-pkgs", "-o",
            "Dpkg::Options::=--force-confdef", "-o", "Dpkg::Options::=--force-confold", "upgrade"],
        Family::Dnf => &["--assumeyes", "upgrade"],
        Family::Pacman => &["--sync", "--refresh", "--sysupgrade", "--noconfirm"],
        Family::Zypper => &["--non-interactive", "--no-refresh", "update", "--type", "package"],
    };
    args.iter().map(|s| (*s).to_owned()).collect()
}

fn install_args(family: Family, names: &[String]) -> Vec<String> {
    let args: &[&str] = match family {
        Family::Apt => &["--assume-yes", "--no-remove", "--no-install-recommends", "-o",
            "Dpkg::Options::=--force-confdef", "-o", "Dpkg::Options::=--force-confold", "install", "--"],
        Family::Dnf => &["--assumeyes", "install", "--"],
        Family::Pacman => &["--sync", "--refresh", "--sysupgrade", "--needed", "--noconfirm", "--"],
        Family::Zypper => &["--non-interactive", "--no-refresh", "install", "--"],
    };
    args.iter().map(|s| (*s).to_owned()).chain(names.iter().cloned()).collect()
}

async fn run_step(manager: &Manager, args: &[String], refresh: bool, upgrade: bool) -> Result<(String, bool, Option<i32>)> {
    let (output, truncated, code) = if manager.family == Family::Pacman && refresh {
        pacman_refresh().await?
    } else {
        step(&manager.program, args, None).await?
    };
    ensure!(
        code == Some(0)
            || (manager.family == Family::Pacman && refresh && code == Some(2))
            || (manager.family == Family::Zypper && upgrade && matches!(code, Some(102 | 103))),
        "native package operation failed or was interrupted; inspect native package transaction state"
    );
    Ok((output, truncated, code))
}

pub async fn execute(
    verb: &str,
    names: &[String],
    dependency_target: Option<&str>,
) -> Result<String> {
    let manager = detect()?;
    let (packages, action) = match verb {
        "package_refresh" if names.is_empty() && dependency_target.is_none() => (vec![], "refresh"),
        "package_upgrade" if names.is_empty() && dependency_target.is_none() => (vec![], "upgrade"),
        "package_install" if dependency_target.is_none() => (package_names(&names.join(" "))?, "install"),
        "dependency_install" if names.is_empty() => (
            dependency_packages(manager.family, &manager.distribution,
                dependency_target.context("dependency target missing")?)?, "install"),
        _ => bail!("invalid native package operation"),
    };
    let mut output = String::new();
    if action != "install" || manager.family != Family::Pacman {
        if manager.family != Family::Pacman {
            let (text, _, _) = run_step(&manager, &refresh_args(manager.family), true, false).await?;
            output = text;
        } else if action == "refresh" {
            let (text, _, _) = run_step(&manager, &[], true, false).await?;
            output = text;
        }
    }
    if action == "refresh" {
        return Ok(format!("{}: metadata refreshed\n{}", manager.family.id(), output));
    }
    let args = if action == "upgrade" {
        upgrade_args(manager.family)
    } else {
        install_args(manager.family, &packages)
    };
    let (text, truncated, code) = run_step(&manager, &args, false, action == "upgrade").await?;
    let tail = text.chars().rev().take(4096).collect::<String>().chars().rev().collect::<String>();
    let message = format!("{}: {} completed (exit={:?}; output_truncated={})\n{}",
        manager.family.id(), action, code, truncated, tail);
    // Bound the combined response; never include native environment/passwords.
    output.push_str(&message);
    if output.len() > 6000 {
        output = output.chars().rev().take(5800).collect::<String>().chars().rev().collect();
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_names_reject_selectors_and_duplicates() {
        assert_eq!(package_names("nginx docker.io").unwrap(), ["nginx", "docker.io"]);
        for input in ["", "--assumeyes", "../../etc/passwd", "https://evil", "pkg=1", "foo foo",
            "pkg/repo", "pkg:arch", "a;touch", "pkg\n--help"] {
            assert!(package_names(input).is_err(), "{input}");
        }
        assert!(package_names(&"x".repeat(129)).is_err());
        assert!(package_names(&vec!["valid"; 33].join(" ")).is_err());
    }

    #[test]
    fn dependency_mapping_matches_native_reference() {
        let ubuntu = dependency_packages(Family::Apt, "ubuntu", "docker").unwrap();
        let debian = dependency_packages(Family::Apt, "debian", "docker").unwrap();
        assert!(ubuntu.contains(&"docker-compose-v2".into()));
        assert!(debian.contains(&"docker-cli".into()));
        assert!(dependency_packages(Family::Zypper, "opensuse", "incus").is_err());
        assert_eq!(dependency_packages(Family::Pacman, "arch", "virtualization").unwrap(),
            ["qemu-base", "libvirt"]);
    }

    #[test]
    fn fixed_package_commands_never_use_repo_override() {
        assert!(upgrade_args(Family::Apt).contains(&"--no-remove".into()));
        assert!(install_args(Family::Pacman, &["vim".into()]).contains(&"--sysupgrade".into()));
        assert_eq!(refresh_args(Family::Dnf), ["--assumeyes", "--refresh", "makecache"]);
        for family in [Family::Apt, Family::Dnf, Family::Pacman, Family::Zypper] {
            let args = install_args(family, &["vim".into()]);
            assert_eq!(args.last().map(String::as_str), Some("vim"));
            assert!(args.iter().any(|arg| arg == "--"));
        }
    }
}
