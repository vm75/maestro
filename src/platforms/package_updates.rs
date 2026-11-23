use std::{
    fs::{self, File},
    io::Read,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    sync::LazyLock,
    time::Duration,
};

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::Serialize;

use crate::{core::jobs::RunningJob, state::AppState};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Family {
    Apt,
    Dnf,
    Pacman,
    Zypper,
    Unsupported,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Manager {
    id: Family,
    title: &'static str,
    available: bool,
    reason: Option<&'static str>,
    write_reason: Option<&'static str>,
    can_refresh: bool,
    can_install: bool,
}

#[derive(Serialize)]
struct Package {
    name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    architecture: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    installed: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    candidate: String,
    kind: &'static str,
}

#[derive(Serialize)]
struct Reboot {
    required: Option<bool>,
    reason: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    manager: Manager,
    distribution: String,
    packages: Vec<Package>,
    valid: bool,
    notice: &'static str,
    cache_source: &'static str,
    reboot: Reboot,
    read_at: DateTime<Utc>,
}

fn detect(
    state: &AppState,
) -> (
    Release,
    Family,
    &'static str,
    PathBuf,
    &'static [&'static str],
) {
    let paths = &state.config.paths;
    let release = read_release(&paths.os_release);
    let family = if paths.ostree_booted.exists()
        || matches!(
            release.variant.as_str(),
            "silverblue" | "kinoite" | "sericea" | "onyx"
        ) {
        Family::Unsupported
    } else {
        release.family()
    };
    let (title, program, args) = match family {
        Family::Apt => ("APT", "apt-get", APT_ARGS),
        Family::Dnf => {
            let program = if is_executable(&paths.package_bin_dir.join("dnf")) {
                "dnf"
            } else {
                "dnf5"
            };
            ("DNF", program, DNF_ARGS)
        }
        Family::Pacman => ("Pacman", "pacman", PACMAN_ARGS),
        Family::Zypper => ("Zypper", "zypper", ZYPPER_ARGS),
        Family::Unsupported => ("Unsupported", "", &[][..]),
    };
    (
        release,
        family,
        title,
        paths.package_bin_dir.join(program),
        args,
    )
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.mode() & 0o111 != 0)
}

fn availability_reason(state: &AppState, family: Family, program: &Path) -> Option<&'static str> {
    if state.config.test_mode {
        Some("Package status is unavailable in simulation.")
    } else if family == Family::Unsupported {
        Some("No supported native package manager identified for this operating system.")
    } else if !is_executable(program) {
        Some("The native package-manager executable is unavailable.")
    } else {
        None
    }
}

pub fn available(state: &AppState) -> bool {
    if state.config.test_mode {
        return false;
    }
    let (_, family, _, program, _) = detect(state);
    availability_reason(state, family, &program).is_none()
}

pub fn writes_enabled(_state: &AppState) -> bool {
    // Fail closed until each native package mutation has been moved into the
    // root-owned helper. Running the workload server as root is not a fallback.
    false
}

pub async fn status(state: &AppState) -> Status {
    let paths = &state.config.paths;
    let (release, family, title, program, base_args) = detect(state);
    let mut args = base_args.to_vec();
    let pacman_db = pacman_cache(state);
    let mut cache_source = "Native cached repository metadata";
    if family == Family::Pacman && private_dir(&pacman_db) && pacman_db.join("sync").is_dir() {
        if let Some(path) = pacman_db.to_str() {
            args.extend(["--dbpath", path]);
            cache_source = "Maestro's isolated checkupdates database";
        }
    }
    let reason = availability_reason(state, family, &program);
    let mut snapshot = Status {
        manager: Manager {
            id: family,
            title,
            available: reason.is_none(),
            reason,
            write_reason: if !state.config.system.allow_package_writes {
                Some("Package writes are disabled by the administrator policy.")
            } else {
                Some("Protected package operations are gated until their restricted helper verbs and native acceptance are completed.")
            },
            can_install: reason.is_none() && writes_enabled(state),
            can_refresh: reason.is_none()
                && writes_enabled(state)
                && (family != Family::Pacman
                    || is_executable(&paths.package_bin_dir.join("checkupdates"))),
        },
        distribution: release.pretty_name,
        packages: vec![],
        valid: false,
        notice: reason.unwrap_or("Package listings use cached native metadata."),
        cache_source,
        reboot: reboot(family, &paths.reboot_required),
        read_at: Utc::now(),
    };
    if reason.is_some() {
        return snapshot;
    }
    let output = state
        .runner
        .capture_status(&program, &args, Duration::from_secs(12), 2 * 1024 * 1024)
        .await;
    let Ok(output) = output else {
        snapshot.notice =
            "Cached updates are unavailable. Check package-manager permissions and metadata.";
        return snapshot;
    };
    let accepted = output.status.success()
        || (family == Family::Zypper && matches!(output.status.code(), Some(100 | 101)))
        || (family == Family::Pacman
            && output.status.code() == Some(1)
            && !output.had_stderr
            && output.stdout.iter().all(u8::is_ascii_whitespace));
    if !accepted {
        snapshot.notice =
            "Cached updates are unavailable. Check package-manager permissions and metadata.";
        return snapshot;
    }
    match std::str::from_utf8(&output.stdout)
        .ok()
        .and_then(|text| parse(family, text))
    {
        Some(packages) => {
            snapshot.packages = packages;
            snapshot.valid = true;
        }
        None => snapshot.notice = "The package manager returned unsupported or oversized data.",
    }
    snapshot
}

fn pacman_cache(state: &AppState) -> PathBuf {
    state.config.paths.data_dir.join("updates-pacman")
}

fn private_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_dir()
            && metadata.mode() & 0o077 == 0
            && metadata.uid() == unsafe { libc::geteuid() }
    })
}

fn refresh_args(family: Family) -> &'static [&'static str] {
    match family {
        Family::Apt => &["-o", "APT::Update::Error-Mode=any", "update"],
        Family::Dnf => &["--assumeyes", "--refresh", "makecache"],
        Family::Zypper => &["--non-interactive", "refresh"],
        Family::Pacman | Family::Unsupported => &[],
    }
}

fn install_args(family: Family) -> &'static [&'static str] {
    match family {
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
        Family::Unsupported => &[],
    }
}

fn permits_operation(state: &AppState, family: Family, program: &Path, install: bool) -> bool {
    writes_enabled(state)
        && availability_reason(state, family, program).is_none()
        && (install
            || family != Family::Pacman
            || is_executable(&state.config.paths.package_bin_dir.join("checkupdates")))
}

pub fn permits(state: &AppState, install: bool) -> bool {
    if !writes_enabled(state) {
        return false;
    }
    let (_, family, _, program, _) = detect(state);
    permits_operation(state, family, &program, install)
}

pub async fn run_job(
    state: &AppState,
    install: bool,
    job: &RunningJob,
) -> anyhow::Result<serde_json::Value> {
    let (_, family, _, program, _) = detect(state);
    anyhow::ensure!(
        permits_operation(state, family, &program, install),
        "Package operation is unavailable or outside the write policy."
    );
    run_native_job(state, family, &program, install, job).await
}

async fn run_native_job(
    state: &AppState,
    family: Family,
    program: &Path,
    install: bool,
    job: &RunningJob,
) -> anyhow::Result<serde_json::Value> {
    let mut environment = vec![
        ("DEBIAN_FRONTEND", "noninteractive"),
        ("NEEDRESTART_MODE", "l"),
        ("NO_COLOR", "1"),
    ];
    let cache = pacman_cache(state);
    if family == Family::Pacman && !install {
        anyhow::ensure!(
            cache.is_absolute(),
            "An absolute state directory is required."
        );
        match fs::DirBuilder::new().mode(0o700).create(&cache) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        anyhow::ensure!(
            private_dir(&cache),
            "The isolated Pacman cache must be an owner-only directory."
        );
        environment.push((
            "CHECKUPDATES_DB",
            cache
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("Invalid cache path."))?,
        ));
        let checkupdates = state.config.paths.package_bin_dir.join("checkupdates");
        let code = execute(state, job, &checkupdates, &[], &environment).await?;
        anyhow::ensure!(
            matches!(code, Some(0 | 2)),
            "Pacman metadata refresh failed."
        );
    } else if family != Family::Pacman {
        let code = execute(state, job, program, refresh_args(family), &environment).await?;
        anyhow::ensure!(code == Some(0), "Package metadata refresh failed.");
    }
    let mut reboot = reboot(family, &state.config.paths.reboot_required);
    let mut recheck = false;
    if install {
        let code = execute(state, job, program, install_args(family), &environment).await?;
        if family == Family::Zypper && matches!(code, Some(102 | 103)) {
            if code == Some(102) {
                reboot = Reboot {
                    required: Some(true),
                    reason: "Zypper reported that a reboot is required.",
                };
            } else {
                recheck = true;
            }
        } else {
            anyhow::ensure!(
                code == Some(0),
                "Package installation failed. Inspect the native package manager before retrying."
            );
            reboot = self::reboot(family, &state.config.paths.reboot_required);
        }
    }
    Ok(serde_json::json!({
        "manager": family,
        "action": if install { "updates installed" } else { "metadata refreshed" },
        "reboot": reboot,
        "recheckRequired": recheck,
    }))
}

async fn execute(
    state: &AppState,
    job: &RunningJob,
    program: &Path,
    args: &[&str],
    environment: &[(&str, &str)],
) -> anyhow::Result<Option<i32>> {
    let output = state
        .runner
        .capture_package_job(program, args, environment)
        .await?;
    job.append_output(&String::from_utf8_lossy(&output.stdout));
    if output.had_stderr {
        job.append_output("\nThe package manager wrote diagnostics to stderr.\n");
    }
    Ok(output.status.code())
}

const APT_ARGS: &[&str] = &[
    "--simulate",
    "--no-remove",
    "--with-new-pkgs",
    "-o",
    "Debug::NoLocking=1",
    "-o",
    "APT::Get::Show-User-Simulation-Note=false",
    "-o",
    "Dir::Cache::pkgcache=",
    "-o",
    "Dir::Cache::srcpkgcache=",
    "upgrade",
];

const DNF_ARGS: &[&str] = &[
    "--cacheonly",
    "--quiet",
    "repoquery",
    "--upgrades",
    "--latest-limit=1",
    "--queryformat",
    "%{name}\t%{arch}\t%{evr}\n",
];

const PACMAN_ARGS: &[&str] = &["--query", "--upgrades", "--color", "never"];
const ZYPPER_ARGS: &[&str] = &[
    "--non-interactive",
    "--no-refresh",
    "--xmlout",
    "list-updates",
    "--type",
    "package",
];

#[derive(Default)]
struct Release {
    id: String,
    like: String,
    pretty_name: String,
    variant: String,
}

impl Release {
    fn family(&self) -> Family {
        std::iter::once(self.id.as_str())
            .chain(self.like.split_whitespace())
            .find_map(|id| {
                Some(match id {
                    "debian" | "ubuntu" | "linuxmint" | "pop" => Family::Apt,
                    "fedora" | "rhel" | "centos" | "rocky" | "almalinux" | "ol" => Family::Dnf,
                    "arch" | "manjaro" | "endeavouros" => Family::Pacman,
                    "suse"
                    | "opensuse"
                    | "opensuse-leap"
                    | "opensuse-tumbleweed"
                    | "sles"
                    | "sled" => Family::Zypper,
                    _ => return None,
                })
            })
            .unwrap_or(Family::Unsupported)
    }
}

fn read_release(path: &Path) -> Release {
    let mut release = Release::default();
    let Ok(file) = File::open(path) else {
        return release;
    };
    let mut text = String::new();
    if file.take(65537).read_to_string(&mut text).is_err() || text.len() > 65536 {
        return release;
    }
    for (key, value) in text.lines().filter_map(|line| line.split_once('=')) {
        let target = match key {
            "ID" => &mut release.id,
            "ID_LIKE" => &mut release.like,
            "PRETTY_NAME" => &mut release.pretty_name,
            "VARIANT_ID" => &mut release.variant,
            _ => continue,
        };
        *target = value.trim().trim_matches(['"', '\'']).into();
    }
    release
}

fn reboot(family: Family, path: &Path) -> Reboot {
    if path.is_file() {
        Reboot {
            required: Some(true),
            reason: "The native reboot-required marker exists.",
        }
    } else if family == Family::Apt && matches!(path.try_exists(), Ok(false)) {
        Reboot {
            required: Some(false),
            reason: "No native reboot-required marker was found.",
        }
    } else {
        Reboot {
            required: None,
            reason: "Reboot requirement is unavailable.",
        }
    }
}

fn parse(family: Family, text: &str) -> Option<Vec<Package>> {
    let mut packages = match family {
        Family::Apt => parse_apt(text)?,
        Family::Dnf => text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let mut fields = line.trim_end().split('\t');
                let package = Package {
                    name: fields.next()?.into(),
                    architecture: fields.next()?.into(),
                    installed: String::new(),
                    candidate: fields.next()?.into(),
                    kind: "upgrade",
                };
                fields.next().is_none().then_some(package)
            })
            .collect::<Option<Vec<_>>>()?,
        Family::Pacman => parse_pacman(text)?,
        Family::Zypper => parse_zypper(text)?,
        Family::Unsupported => return None,
    };
    let name_valid = |name: &str| {
        !name.is_empty()
            && name.len() <= 255
            && name.as_bytes()[0].is_ascii_alphanumeric()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+_.:-".contains(&byte))
    };
    let version_valid = |version: &str| {
        version.len() <= 256 && !version.chars().any(|c| c.is_control() || c.is_whitespace())
    };
    if packages.len() > 2000
        || packages.iter().any(|p| {
            !name_valid(&p.name)
                || (!p.architecture.is_empty() && !name_valid(&p.architecture))
                || (!version_valid(&p.installed) || !version_valid(&p.candidate))
                || (p.candidate.is_empty() && p.kind != "held")
        })
    {
        return None;
    }
    packages.sort_unstable_by(|a, b| {
        (&a.name, &a.architecture, &a.candidate, a.kind).cmp(&(
            &b.name,
            &b.architecture,
            &b.candidate,
            b.kind,
        ))
    });
    packages.dedup_by(|a, b| {
        a.name == b.name
            && a.architecture == b.architecture
            && a.candidate == b.candidate
            && a.kind == b.kind
    });
    Some(packages)
}

fn parse_apt(text: &str) -> Option<Vec<Package>> {
    static INSTALL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^Inst ([^ ]+)(?: \[([^\]]+)\])? \(([^ )]+)(?: .*)?\)(?: \[[^\]]*\])?$")
            .unwrap()
    });
    static ARCH: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\)(?: \[[^\]]*\])?$").unwrap());
    let mut rows = Vec::new();
    let mut held = false;
    let mut recognized = false;
    for line in text.lines() {
        recognized |= line.contains(" upgraded,") || line.contains("Upgrading:");
        if line.contains("have been kept back:")
            || line.contains("have been deferred due to phasing:")
        {
            held = true;
            recognized = true;
            continue;
        }
        if held && line.starts_with("  ") {
            rows.extend(line.split_whitespace().map(|name| Package {
                name: name.into(),
                architecture: String::new(),
                installed: String::new(),
                candidate: String::new(),
                kind: "held",
            }));
            continue;
        }
        held = false;
        if !line.starts_with("Inst ") {
            continue;
        }
        let record = INSTALL.captures(line)?;
        let name = record.get(1)?.as_str();
        let arch = ARCH
            .captures(line)
            .and_then(|capture| capture.get(1))
            .map_or("", |value| value.as_str());
        let (name, arch) = name.split_once(':').unwrap_or((name, arch));
        let installed = record.get(2).map_or("", |value| value.as_str());
        rows.push(Package {
            name: name.into(),
            architecture: arch.into(),
            installed: installed.into(),
            candidate: record.get(3)?.as_str().into(),
            kind: if installed.is_empty() {
                "install"
            } else {
                "upgrade"
            },
        });
        recognized = true;
    }
    recognized.then_some(rows)
}

fn parse_pacman(text: &str) -> Option<Vec<Package>> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?.into();
            let installed = fields.next()?.into();
            if fields.next()? != "->" {
                return None;
            }
            let candidate = fields.next()?.into();
            let kind = match fields.next() {
                Some("[ignored]") => "held",
                None => "upgrade",
                _ => return None,
            };
            fields.next().is_none().then_some(Package {
                name,
                installed,
                candidate,
                architecture: String::new(),
                kind,
            })
        })
        .collect()
}

fn parse_zypper(text: &str) -> Option<Vec<Package>> {
    use quick_xml::{Reader, events::Event};

    let mut reader = Reader::from_str(text);
    let mut stack = Vec::<Vec<u8>>::new();
    let mut saw_stream = false;
    let mut saw_list = false;
    let mut rows = Vec::new();
    loop {
        let event = reader.read_event().ok()?;
        match &event {
            Event::Start(element) | Event::Empty(element) => {
                let tag = element.name();
                let tag = tag.as_ref();
                if stack.is_empty() {
                    if saw_stream || tag != b"stream" {
                        return None;
                    }
                    saw_stream = true;
                }
                if stack.len() >= 64 {
                    return None;
                }
                if tag == b"update-list"
                    && stack
                        .last()
                        .is_some_and(|parent| parent == b"update-status")
                {
                    saw_list = true;
                }
                if tag == b"message" || tag == b"update" {
                    let mut row = Package {
                        name: String::new(),
                        architecture: String::new(),
                        installed: String::new(),
                        candidate: String::new(),
                        kind: "upgrade",
                    };
                    let mut package = false;
                    for attribute in element.attributes() {
                        let attribute = attribute.ok()?;
                        let value = attribute.decode_and_unescape_value(reader.decoder()).ok()?;
                        match attribute.key.as_ref() {
                            b"type" if tag == b"message" && value == "error" => return None,
                            b"kind" => package = value == "package",
                            b"name" => row.name = value.into_owned(),
                            b"arch" => row.architecture = value.into_owned(),
                            b"edition" => row.candidate = value.into_owned(),
                            b"edition-old" => row.installed = value.into_owned(),
                            _ => {}
                        }
                    }
                    if tag == b"update"
                        && package
                        && stack.last().is_some_and(|parent| parent == b"update-list")
                    {
                        if rows.len() >= 2000 {
                            return None;
                        }
                        rows.push(row);
                    }
                }
                if matches!(&event, Event::Start(_)) {
                    stack.push(tag.to_vec());
                }
            }
            Event::End(_) => {
                stack.pop()?;
            }
            Event::DocType(_) => return None,
            Event::Text(text) if stack.is_empty() && !text.iter().all(u8::is_ascii_whitespace) => {
                return None;
            }
            Event::CData(_) | Event::GeneralRef(_) if stack.is_empty() => return None,
            Event::Eof => break,
            _ => {}
        }
    }
    (saw_stream && saw_list && stack.is_empty()).then_some(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_apt_simulation_classifies_candidates_and_deferred_packages() {
        let rows = parse(Family::Apt, "The following packages have been kept back:\n  delayed\nInst libc6:amd64 [1] (2 Debian [amd64])\nInst new (1 Debian [all])\n1 upgraded, 1 newly installed").unwrap();
        assert_eq!(rows[0].kind, "held");
        assert_eq!(rows[1].architecture, "amd64");
        assert_eq!(rows[1].candidate, "2");
        assert_eq!(rows[2].kind, "install");
        assert!(parse(Family::Apt, "unexpected").is_none());
        assert!(parse(Family::Dnf, "-x\tnoarch\t1").is_none());
    }

    #[test]
    fn parses_pacman_updates_and_ignored_rows() {
        let rows = parse(Family::Pacman, "linux 6.1 -> 6.2 [ignored]\nvim 1 -> 2").unwrap();
        assert_eq!(rows[0].kind, "held");
        assert_eq!(rows[1].candidate, "2");
        assert!(parse(Family::Pacman, "").unwrap().is_empty());
        assert!(parse(Family::Pacman, "unexpected").is_none());
    }

    #[test]
    fn parses_zypper_package_rows_and_rejects_incomplete_or_error_xml() {
        let xml = r#"<?xml version="1.0"?><stream><update-status><update-list>
            <update name="libfoo" arch="x86_64" edition="2" edition-old="1" kind="package"/>
            <update name="patch" kind="patch"/>
            </update-list></update-status></stream>"#;
        let rows = parse(Family::Zypper, xml).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].installed, "1");
        assert_eq!(rows[0].candidate, "2");
        assert!(
            parse(
                Family::Zypper,
                "<stream><update-status><update-list/></update-status></stream>"
            )
            .unwrap()
            .is_empty()
        );
        assert!(parse(Family::Zypper, "<stream><update-status><update-list/>").is_none());
        assert!(parse(Family::Zypper, "<stream><update-status><update-list/></update-status><message type=\"error\"/></stream>").is_none());
        assert!(parse(Family::Zypper, "<!DOCTYPE stream><stream/>").is_none());
    }

    #[test]
    fn package_write_commands_keep_native_upgrade_semantics() {
        assert!(refresh_args(Family::Apt).contains(&"APT::Update::Error-Mode=any"));
        assert!(install_args(Family::Apt).contains(&"--no-remove"));
        assert_eq!(install_args(Family::Dnf), ["--assumeyes", "upgrade"]);
        assert!(refresh_args(Family::Pacman).is_empty());
        assert_eq!(
            install_args(Family::Pacman),
            ["--sync", "--refresh", "--sysupgrade", "--noconfirm"]
        );
        assert!(install_args(Family::Zypper).contains(&"--no-refresh"));
    }

    #[tokio::test]
    async fn apt_job_refreshes_before_install_and_reports_reboot_marker() {
        use crate::{
            command::CommandRunner,
            config::AppConfig,
            core::jobs::{Jobs, Outcome},
            firewall::FirewallManager,
            storage::Stores,
        };
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig {
            test_mode: true,
            paths: crate::config::PathsConfig {
                data_dir: temp.path().to_path_buf(),
                reboot_required: temp.path().join("reboot-required"),
                ..Default::default()
            },
            ..Default::default()
        };
        let stores = Stores::load(&config).await.unwrap();
        let firewall = FirewallManager::new(config.clone(), stores.clone())
            .await
            .unwrap();
        let mut state = AppState::new(config, stores, firewall);
        state.runner = CommandRunner::new(false);

        let binary = temp.path().join("apt-get");
        fs::write(
            &binary,
            r#"#!/bin/sh
[ "$DEBIAN_FRONTEND" = noninteractive ] || exit 1
for arg do last="$arg"; done
case "$last" in
  update) printf '%s\n' refreshed > "$(dirname "$0")/refresh-evidence"; printf '%s\n' refreshed ;;
  upgrade) test -f "$(dirname "$0")/refresh-evidence" || exit 1
           printf '%s\n' reboot > "$(dirname "$0")/reboot-required"; printf '%s\n' installed ;;
  *) exit 1 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();

        let jobs = Jobs::new(temp.path().to_path_buf());
        let (job, running) = jobs.start("updates_install").unwrap().unwrap();
        let result = run_native_job(&state, Family::Apt, &binary, true, &running)
            .await
            .unwrap();
        assert_eq!(result["reboot"]["required"], true);
        running.finish(Outcome::Succeeded(result));
        assert_eq!(jobs.get(job.id).unwrap().output, "refreshed\ninstalled\n");
    }
}
