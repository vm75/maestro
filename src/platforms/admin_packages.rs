//! Restricted native package primitives for the Linux root-owned helper.
//! The request selects *verbs* and validated native repository package names;
//! distribution, executable, arguments, environment and timeouts are fixed here.
use std::{fs, os::unix::fs::{MetadataExt, PermissionsExt}, path::{Path, PathBuf}};
use anyhow::{Context, Result, bail, ensure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Family {
    Apt,
    Dnf,
    Pacman,
    Zypper,
}

impl Family {
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Apt => "apt",
            Self::Dnf => "dnf",
            Self::Pacman => "pacman",
            Self::Zypper => "zypper",
        }
    }
}

pub(super) fn trusted_executable(path: &Path) -> Result<PathBuf> {
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
        ensure!(
            meta.is_dir() && meta.uid() == 0 && meta.permissions().mode() & 0o022 == 0,
            "native program parent is not root-owned and protected"
        );
    }
    let meta = fs::symlink_metadata(&canonical)?;
    ensure!(
        meta.is_file()
            && meta.uid() == 0
            && meta.permissions().mode() & 0o022 == 0
            && meta.permissions().mode() & 0o111 != 0
            && meta.permissions().mode() & 0o6000 == 0,
        "native package binary must be a root-owned executable"
    );
    Ok(canonical)
}

/// Accepts only bare repository names: no flags, paths, URLs, versions, shell
/// syntax, repository changes or duplicate packages.
pub fn package_names(text: &str) -> Result<Vec<String>> {
    ensure!(text.len() <= 4096, "package list exceeds 4096 bytes");
    let names: Vec<String> = text.split_whitespace().map(str::to_owned).collect();
    ensure!(
        (1..=32).contains(&names.len()),
        "provide between 1 and 32 package names"
    );
    for (index, name) in names.iter().enumerate() {
        let bytes = name.as_bytes();
        ensure!(
            !bytes.is_empty()
                && bytes.len() <= 128
                && bytes[0].is_ascii_alphanumeric()
                && bytes
                    .iter()
                    .skip(1)
                    .all(|byte| byte.is_ascii_alphanumeric() || b"+_.-".contains(byte))
                && !names[..index].contains(name),
            "invalid or duplicate native repository package name"
        );
    }
    Ok(names)
}

pub(super) fn dependency_packages(family: Family, distribution: &str, target: &str) -> Result<Vec<String>> {
    // Pinned mappings from Server Man's dependencyPackages reference. Package
    // manager policy and native signature checks are never modified.
    let names = match (target, family) {
        ("podman", Family::Apt) => {
            "podman podman-compose uidmap slirp4netns passt netavark aardvark-dns nftables dbus-user-session"
        }
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
        ("virtualization", Family::Apt) => {
            "qemu-system-x86 qemu-utils libvirt-daemon-system libvirt-daemon-driver-qemu libvirt-clients"
        }
        ("virtualization", Family::Dnf) => "qemu-kvm qemu-img libvirt libvirt-client",
        ("virtualization", Family::Pacman) => "qemu-base libvirt",
        ("virtualization", Family::Zypper) => "qemu-x86 qemu-tools libvirt libvirt-client",
        _ => bail!("unsupported dependency group on this native package manager"),
    };
    package_names(names)
}

pub(super) fn refresh_args(family: Family) -> Vec<String> {
    let args: &[&str] = match family {
        Family::Apt => &["-o", "APT::Update::Error-Mode=any", "update"],
        Family::Dnf => &["--assumeyes", "--refresh", "makecache"],
        Family::Zypper => &["--non-interactive", "refresh"],
        Family::Pacman => &[],
    };
    args.iter().map(|s| (*s).to_owned()).collect()
}

pub(super) fn upgrade_args(family: Family) -> Vec<String> {
    let args: &[&str] = match family {
        Family::Apt => &[
            "--assume-yes",
            "--no-remove",
            "--with-new-pkgs",
            "-o",
            "Dpkg::Options::=--force-confdef",
            "-o",
            "Dpkg::Options::=--force-confold",
            "upgrade",
        ],
        Family::Dnf => &["--assumeyes", "upgrade"],
        Family::Pacman => &["--sync", "--refresh", "--sysupgrade", "--noconfirm"],
        Family::Zypper => &[
            "--non-interactive",
            "--no-refresh",
            "update",
            "--type",
            "package",
        ],
    };
    args.iter().map(|s| (*s).to_owned()).collect()
}

pub(super) fn install_args(family: Family, names: &[String]) -> Vec<String> {
    let args: &[&str] = match family {
        Family::Apt => &[
            "--assume-yes",
            "--no-remove",
            "--no-install-recommends",
            "-o",
            "Dpkg::Options::=--force-confdef",
            "-o",
            "Dpkg::Options::=--force-confold",
            "install",
            "--",
        ],
        Family::Dnf => &["--assumeyes", "install", "--"],
        Family::Pacman => &[
            "--sync",
            "--refresh",
            "--sysupgrade",
            "--needed",
            "--noconfirm",
            "--",
        ],
        Family::Zypper => &["--non-interactive", "--no-refresh", "install", "--"],
    };
    args.iter()
        .map(|s| (*s).to_owned())
        .chain(names.iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_names_reject_selectors_and_duplicates() {
        assert_eq!(
            package_names("nginx docker.io").unwrap(),
            ["nginx", "docker.io"]
        );
        for input in [
            "",
            "--assumeyes",
            "../../etc/passwd",
            "https://evil",
            "pkg=1",
            "foo foo",
            "pkg/repo",
            "pkg:arch",
            "a;touch",
            "pkg\n--help",
        ] {
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
        assert_eq!(
            dependency_packages(Family::Pacman, "arch", "virtualization").unwrap(),
            ["qemu-base", "libvirt"]
        );
    }

    #[test]
    fn fixed_package_commands_never_use_repo_override() {
        assert!(upgrade_args(Family::Apt).contains(&"--no-remove".into()));
        assert!(install_args(Family::Pacman, &["vim".into()]).contains(&"--sysupgrade".into()));
        assert_eq!(
            refresh_args(Family::Dnf),
            ["--assumeyes", "--refresh", "makecache"]
        );
        for family in [Family::Apt, Family::Dnf, Family::Pacman, Family::Zypper] {
            let args = install_args(family, &["vim".into()]);
            assert_eq!(args.last().map(String::as_str), Some("vim"));
            assert!(args.iter().any(|arg| arg == "--"));
        }
    }
}
