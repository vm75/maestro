//! Static configuration health translated from Server Man preflight.go,
//! mounts.go and ports_preflight.go. No files are created or opened for writes.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CString,
    fs,
    io::Read,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::{Config, Store};

#[derive(Clone, Debug, Serialize)]
pub struct Issue {
    pub severity: &'static str,
    pub resource: String,
    pub field: String,
    pub code: &'static str,
    pub message: String,
    pub remedy: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub checked_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_checked_at: Option<DateTime<Utc>>,
    pub issues: Vec<Issue>,
}

fn issue(
    severity: &'static str,
    resource: &str,
    field: &str,
    code: &'static str,
    message: impl Into<String>,
    remedy: &'static str,
) -> Issue {
    Issue {
        severity,
        resource: resource.into(),
        field: field.into(),
        code,
        message: message.into(),
        remedy,
    }
}

const RESTORE: &str = "Restore or rebind the required data; do not replace it with empty data.";

#[derive(Debug)]
struct Mount {
    point: PathBuf,
    source: String,
    read_only: bool,
}

fn unescape(value: &str) -> String {
    value
        .replace(r"\040", " ")
        .replace(r"\011", "\t")
        .replace(r"\012", "\n")
        .replace(r"\134", "\\")
}

fn parse_mounts(text: &str) -> Option<Vec<Mount>> {
    let mut mounts = Vec::new();
    for line in text.lines() {
        if line.len() > 64 * 1024 || mounts.len() == 8192 {
            return None;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        let sep = fields.iter().position(|field| *field == "-")?;
        if fields.len() < 10 || sep < 6 || sep + 3 >= fields.len() {
            return None;
        }
        let ro = |options: &str| options.split(',').any(|option| option == "ro");
        let point = PathBuf::from(unescape(fields[4]));
        if !point.is_absolute() {
            return None;
        }
        mounts.push(Mount {
            point,
            source: unescape(fields[sep + 2]),
            read_only: ro(fields[5]) || ro(fields[sep + 3]),
        });
    }
    Some(mounts)
}

fn read_mounts(proc_root: &Path) -> Option<Vec<Mount>> {
    const LIMIT: u64 = 4 * 1024 * 1024;
    let mut text = String::new();
    fs::File::open(proc_root.join("self/mountinfo"))
        .ok()?
        .take(LIMIT + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > LIMIT {
        return None;
    }
    parse_mounts(&text)
}

fn mounted_filesystem<'a>(path: &Path, mounts: &'a [Mount]) -> Option<&'a Mount> {
    // max_by_key selects the last equally specific mount, the visible one.
    mounts
        .iter()
        .filter(|mount| path.starts_with(&mount.point))
        .max_by_key(|mount| mount.point.as_os_str().len())
}

pub(super) fn expected_mount(path: &Path, expected: &str, proc_root: Option<&Path>) -> bool {
    if !Path::new(expected).is_absolute() {
        return false;
    }
    let Some(mounts) = proc_root.and_then(read_mounts) else { return false; };
    let Ok(real) = fs::canonicalize(path) else { return false; };
    mounted_filesystem(&real, &mounts).is_some_and(|mount| mount.source == expected && !mount.read_only)
}

fn access(path: &Path, mode: libc::c_int) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // access(2) checks the workload account, and never changes the target.
    unsafe { libc::access(path.as_ptr(), mode) == 0 }
}

/// Resolve only metadata. Portable paths may not escape their configured root.
/// The source reference follows symlinks inside the root; native mutation
/// paths retain their independent, stricter ownership and no-follow guards.
fn check_path(
    issues: &mut Vec<Issue>,
    resource: &str,
    field: &str,
    root: Option<&Path>,
    path: &Path,
    directory: bool,
    private: bool,
) -> Option<(PathBuf, fs::Metadata)> {
    let mut add = |code, message, remedy| {
        issues.push(issue("error", resource, field, code, message, remedy));
    };
    if !path.is_absolute() {
        add(
            "invalid_root",
            "The configuration location must be absolute.",
            "Select an absolute manifest path.",
        );
        return None;
    }
    if let Some(root) = root {
        let Ok(real_root) = fs::canonicalize(root) else {
            add(
                "unavailable_root",
                "The configuration directory is inaccessible.",
                "Restore the configuration directory and its access permissions.",
            );
            return None;
        };
        if let Ok(real) = fs::canonicalize(path) {
            if !real.starts_with(real_root) {
                add(
                    "path_escape",
                    "A portable path resolves outside the configuration directory.",
                    "Replace the escaping link or declare external storage explicitly.",
                );
                return None;
            }
        }
    }
    let info = match fs::metadata(path) {
        Ok(info) => info,
        Err(error) => {
            if error.kind() == std::io::ErrorKind::NotFound {
                add(
                    "missing_path",
                    "A required file or folder is missing.",
                    RESTORE,
                );
            } else {
                add(
                    "inaccessible_path",
                    "A required path cannot be accessed.",
                    RESTORE,
                );
            }
            return None;
        }
    };
    if (directory && !info.is_dir()) || (!directory && !info.is_file()) {
        add(
            "wrong_type",
            "A required path has the wrong file type.",
            "Select the correct file or directory.",
        );
        return None;
    }
    let mode = libc::R_OK | if directory { libc::X_OK } else { 0 };
    if !access(path, mode) {
        add(
            "path_permissions",
            "A required path cannot be read or traversed by the service account.",
            "Restore access permissions or select the correct service account.",
        );
    }
    if private && info.permissions().mode() & 0o077 != 0 {
        add(
            "secret_permissions",
            "A referenced private file allows group or other access.",
            "Restrict file permissions to the service account.",
        );
    }
    Some((path.to_owned(), info))
}

fn writable_mount_issue(resource: &str, path: &Path, mounts: Option<&[Mount]>) -> Option<Issue> {
    let unknown = || {
        issue(
            "unknown",
            resource,
            "path",
            "mount_state_unavailable",
            "The filesystem write state could not be verified.",
            "Restore access to /proc/self/mountinfo and recheck before starting workloads that write to this storage.",
        )
    };
    let Some(mounts) = mounts else {
        return Some(unknown());
    };
    let resolved = fs::canonicalize(path).ok()?;
    let Some(mount) = mounted_filesystem(&resolved, mounts) else {
        return Some(unknown());
    };
    mount.read_only.then(|| issue(
        "error", resource, "path", "read_only_mount",
        "Required writable storage is on a read-only filesystem.",
        "Restore a writable filesystem mount or rebind this storage to a writable location, then check configuration.",
    ))
}

fn port_issues(config: &Config) -> Vec<Issue> {
    let mut declared = BTreeMap::<String, Vec<(String, String)>>::new();
    for (id, service) in &config.services {
        if service.kind != "container" {
            continue;
        }
        for (name, container) in &service.containers {
            for mapping in &container.ports {
                let (port, protocol) = mapping.split_once('/').unwrap_or((mapping, "tcp"));
                let parts: Vec<_> = port.split(':').collect();
                if parts.len() != 2 {
                    continue;
                }
                declared
                    .entry(format!("{protocol}/{}", parts[0]))
                    .or_default()
                    .push((format!("service/{id}"), format!("containers.{name}.ports")));
            }
        }
    }
    let mut issues = Vec::new();
    for (port, owners) in declared {
        if owners.len() < 2 {
            continue;
        }
        let mut seen = BTreeSet::new();
        for (resource, field) in owners {
            if seen.insert((resource.clone(), field.clone())) {
                issues.push(issue(
                    "warning", &resource, &field, "host_port_overlap",
                    format!("Managed host port {port} is declared by multiple containers; simultaneous starts may fail."),
                    "Choose distinct host ports for concurrent applications or stop the other application before starting this one.",
                ));
            }
        }
    }
    issues
}

pub fn check_files(
    store: &Store,
    config: &Config,
    storage_root: &Path,
    proc_root: Option<&Path>,
) -> Health {
    let checked_at = Utc::now();
    let authority = store.path();
    let private_root = authority.parent().unwrap_or(Path::new(""));
    let mounts = proc_root.and_then(read_mounts);
    let mut issues = Vec::new();
    let mut writable = BTreeSet::new();
    for service in config.services.values() {
        for container in service.containers.values() {
            for mount in &container.mounts {
                if !mount.read_only {
                    writable.insert(mount.storage.as_str());
                }
            }
        }
        if let Some(vm) = &service.vm {
            writable.extend(vm.disks.iter().map(String::as_str));
        }
    }
    for share in config.shares.values() {
        if !share.read_only {
            writable.insert(share.storage.as_str());
        }
    }
    for (id, storage) in &config.storage {
        let resource = format!("storage/{id}");
        let path = if storage.external {
            PathBuf::from(&storage.path)
        } else {
            storage_root.join(&storage.path)
        };
        let metadata = check_path(
            &mut issues,
            &resource,
            "path",
            (!storage.external).then_some(storage_root),
            &path,
            storage.kind != "file",
            false,
        );
        if let Some(source) = &storage.source {
            let first = issues.len();
            check_path(
                &mut issues,
                &resource,
                "source.path",
                Some(private_root),
                &private_root.join(&source.path),
                false,
                false,
            );
            if metadata.as_ref().is_some_and(|(_, info)| info.is_file()) {
                for issue in &mut issues[first..] {
                    issue.severity = "warning";
                    issue.remedy = "Restore the initial image asset before future provisioning or backup; the existing disk is never reinitialized.";
                }
            }
        }
        if writable.contains(id.as_str()) && metadata.is_some() {
            if !access(&path, libc::W_OK) {
                issues.push(issue(
                    "error",
                    &resource,
                    "path",
                    "storage_not_writable",
                    "The service account cannot write required persistent data.",
                    "Restore ownership, ACLs and mount write access before starting the workload.",
                ));
            }
            if let Some(issue) = writable_mount_issue(&resource, &path, mounts.as_deref()) {
                issues.push(issue);
            }
        }
        if !storage.mount.is_empty() {
            match mounts.as_deref() {
                None => issues.push(issue(
                    "unknown",
                    &resource,
                    "mount",
                    "mount_inventory_unavailable",
                    "Mounted filesystem identity could not be checked.",
                    "Restore access to the host mount inventory and recheck.",
                )),
                Some(_) if !Path::new(&storage.mount).is_absolute() => issues.push(issue(
                    "error",
                    &resource,
                    "mount",
                    "mount_check_pending",
                    "The expected mounted filesystem requires an absolute device source.",
                    "Set mount to the expected mount source, such as /dev/mapper/data.",
                )),
                Some(mounts) => {
                    let actual = fs::canonicalize(&path).ok().and_then(|resolved| {
                        mounted_filesystem(&resolved, mounts).map(|mount| mount.source.as_str())
                    });
                    if actual != Some(storage.mount.as_str()) {
                        issues.push(issue("error", &resource, "mount", "missing_mount",
                            "The required filesystem is not mounted at the selected data path.",
                            "Mount the expected source before starting services; an empty mountpoint is not restored data."));
                    }
                }
            }
        }
    }
    for (id, path) in &config.secrets {
        check_path(
            &mut issues,
            &format!("secret/{id}"),
            "path",
            Some(private_root),
            &private_root.join(path),
            false,
            true,
        );
    }

    let initial = issues.clone();
    for (id, service) in &config.services {
        let mut refs = BTreeSet::new();
        for container in service.containers.values() {
            refs.extend(
                container
                    .mounts
                    .iter()
                    .map(|mount| format!("storage/{}", mount.storage)),
            );
            refs.extend(
                container
                    .env_files
                    .iter()
                    .map(|secret| format!("secret/{secret}")),
            );
        }
        if let Some(vm) = &service.vm {
            refs.extend(vm.disks.iter().map(|disk| format!("storage/{disk}")));
        }
        for input in &initial {
            if refs.contains(&input.resource) {
                let (severity, code, message) = if input.severity == "warning" {
                    (
                        "warning",
                        "initial_asset_unavailable",
                        "An initial disk image asset needs attention; the existing disk remains authoritative data.",
                    )
                } else {
                    (
                        "error",
                        "blocked_reference",
                        "A required storage or private-file reference is blocked.",
                    )
                };
                issues.push(issue(
                    severity,
                    &format!("service/{id}"),
                    &input.resource,
                    code,
                    message,
                    input.remedy,
                ));
            }
        }
    }
    for (id, share) in &config.shares {
        for input in &initial {
            if input.resource == format!("storage/{}", share.storage) {
                issues.push(issue(
                    "error",
                    &format!("share/{id}"),
                    "storage",
                    "blocked_reference",
                    "The shared directory is unavailable or requires verification.",
                    input.remedy,
                ));
            }
        }
    }
    issues.extend(port_issues(config));
    issues.sort_by(|a, b| (&a.resource, &a.field).cmp(&(&b.resource, &b.field)));
    Health {
        checked_at,
        runtime_checked_at: None,
        issues,
    }
}

/// Native prerequisites are cached independently of fresh file and journal checks.
#[derive(Default)]
pub struct ReadinessCache {
    entry: tokio::sync::Mutex<Option<CachedPrerequisites>>,
}

struct CachedPrerequisites {
    revision: String,
    instant: std::time::Instant,
    checked_at: DateTime<Utc>,
    issues: Vec<Issue>,
}

pub fn service_backend(service: &super::Service) -> String {
    match service.backend.as_str() {
        "compose" | "quadlet" => format!("{}/{}", service.engine, service.backend),
        _ => service.backend.clone(),
    }
}

fn backend_issue(backend: &str, severity: &'static str, code: &'static str, message: &'static str) -> Issue {
    issue(
        severity,
        &format!("backend/{backend}"),
        "runtime",
        code,
        message,
        "Check the selected backend, service-account access and configuration, then recheck.",
    )
}

/// Bounded, fixed-argument probes. Never return native output to the caller.
async fn probe(
    state: &crate::state::AppState,
    program: &Path,
    args: &[&str],
) -> Option<Vec<u8>> {
    if !program.is_absolute() || !program.is_file() {
        return None;
    }
    let output = state.runner.capture_status(
        program, args, std::time::Duration::from_secs(4), 64 * 1024,
    ).await.ok()?;
    (output.status.success() && !output.truncated).then_some(output.stdout)
}

async fn backend_prerequisites(
    state: &crate::state::AppState,
    quadlet_available: bool,
    backend: &str,
) -> Vec<Issue> {
    let commands = &state.config.commands;
    let mut issues = Vec::new();
    if matches!(backend, "podman/quadlet" | "podman/compose") {
        let rootless = probe(
            state, &commands.podman,
            &["--remote=false", "info", "--format", "{{.Host.Security.Rootless}}"],
        ).await;
        if !rootless.as_deref().is_some_and(|bytes| {
            std::str::from_utf8(bytes).is_ok_and(|text| text.trim() == "true")
        }) {
            issues.push(backend_issue(backend, "error", "rootless_unavailable",
                "The local rootless Podman runtime could not be verified."));
        }
    }
    if matches!(backend, "podman/quadlet" | "podman/compose" | "docker/compose") {
        if probe(state, &commands.systemctl, &[
            "--user", "--no-pager", "--no-ask-password", "show", "--property=Version",
        ]).await.is_none() {
            issues.push(backend_issue(backend, "error", "user_manager_unavailable",
                "The workload account's systemd user manager is inaccessible."));
        }
    }
    match backend {
        "podman/quadlet" => {
            if !quadlet_available {
                issues.push(backend_issue(backend, "error", "missing_adapter",
                    "The rootless Quadlet provider is unavailable."));
            }
            // A reachable manager alone does not prove generator, boot or
            // resource-specific readiness.
            issues.push(backend_issue(backend, "unknown", "generator_check_pending",
                "Quadlet generator, boot and resource prerequisites are not fully verified."));
        }
        "podman/compose" | "docker/compose" => {
            let (program, args): (&Path, &[&str]) = if backend == "podman/compose" {
                (&commands.podman, &["--remote=false", "compose", "version"])
            } else {
                (&commands.docker, &["compose", "version"])
            };
            if probe(state, program, args).await.is_none() {
                issues.push(backend_issue(backend, "error", "compose_unavailable",
                    "A compatible Compose provider is unavailable."));
            }
            issues.push(backend_issue(backend, "unknown", "provider_checks_pending",
                "Native Compose ownership, boot policy and resource prerequisites are not yet verified."));
        }
        "libvirt" => {
            // Never select an implicit remote connection for readiness.
            let uri = state.config.linux_authority.as_ref()
                .map(|authority| authority.settings.libvirt_uri.as_str())
                .unwrap_or("qemu:///system");
            if !matches!(uri, "qemu:///system" | "qemu:///session") ||
                probe(state, &commands.virsh, &["--connect", uri, "uri"]).await.is_none() {
                issues.push(backend_issue(backend, "error", "libvirt_unavailable",
                    "The configured local libvirt connection is unavailable."));
            }
            issues.push(backend_issue(backend, "unknown", "provider_checks_pending",
                "KVM capabilities, guest identity and native provider readiness are not yet verified."));
        }
        "incus" => {
            if probe(state, &commands.incus, &["query", "local:/1.0"]).await.is_none() {
                issues.push(backend_issue(backend, "error", "incus_unavailable",
                    "The local Incus daemon is unavailable."));
            }
            issues.push(backend_issue(backend, "unknown", "provider_checks_pending",
                "QEMU, project, pool, network and native guest prerequisites are not yet verified."));
        }
        "samba" | "nfs" => issues.push(backend_issue(backend, "unknown",
            "provider_checks_pending", "Native share publication prerequisites are not yet verified.")),
        _ => issues.push(backend_issue(backend, "unknown", "check_unavailable",
            "Backend prerequisite checks are not implemented.")),
    }
    issues
}

fn propagate(config: &Config, health: &mut Health, native: Vec<Issue>) {
    health.issues.extend(native);
    let sources = health.issues.clone();
    for (id, service) in &config.services {
        let backend = format!("backend/{}", service_backend(service));
        for source in &sources {
            if source.resource == backend || source.resource == "configuration" {
                let mut copy = source.clone();
                copy.resource = format!("service/{id}");
                health.issues.push(copy);
            }
        }
    }
    for (id, share) in &config.shares {
        let backend = format!("backend/{}", share.protocol);
        for source in &sources {
            if source.resource == backend || source.resource == "configuration" {
                let mut copy = source.clone();
                copy.resource = format!("share/{id}");
                health.issues.push(copy);
            }
        }
    }
    let mut blocked: BTreeSet<String> = health.issues.iter()
        .filter(|issue| issue.severity != "warning" && issue.field != "autostart")
        .map(|issue| issue.resource.clone()).collect();
    if let Ok(order) = config.service_order() {
        for id in order {
            if config.services[id].depends_on.iter().any(|dep| blocked.contains(&format!("service/{dep}"))) {
                let key = format!("service/{id}");
                health.issues.push(issue("error", &key, "depends_on", "blocked_dependency",
                    "A required service has startup blockers.",
                    "Repair the dependency's configuration issues and recheck."));
                blocked.insert(key);
            }
        }
    }
}

impl ReadinessCache {
    pub async fn check(
        &self,
        state: &crate::state::AppState,
        store: &Store,
        snapshot: &super::Snapshot,
        config: &Config,
        quadlet_available: bool,
        refresh: bool,
    ) -> Health {
        let mut health = check_files(store, config, &state.config.paths.storage_dir,
            state.config.paths.proc_stat.parent());
        match crate::core::linux_config::restart_required(&state.config, &snapshot.bytes) {
            Ok(false) => {}
            Ok(true) => health.issues.push(issue("error", "configuration", "server",
                "restart_required", "Server settings differ from the running process.",
                "Restart Maestro before managing resources.")),
            Err(_) => health.issues.push(issue("unknown", "configuration", "server",
                "restart_state_unknown", "Running server settings could not be compared.",
                "Inspect configuration and restart Maestro if needed.")),
        }
        match store.pending_apply() {
            Ok(None) => {}
            Ok(Some(_)) => health.issues.push(issue("error", "configuration", "apply",
                "apply_incomplete", "An earlier configuration apply has not completed.",
                "Inspect apply status and explicitly retry or revert before starting workloads.")),
            Err(_) => health.issues.push(issue("unknown", "configuration", "apply",
                "apply_unavailable", "Previous apply status cannot be verified.",
                "Inspect the private apply journal before managing workloads.")),
        }
        if state.runner.is_test_mode() {
            propagate(config, &mut health, Vec::new());
            return health;
        }
        let mut cache = self.entry.lock().await;
        if !refresh {
            if let Some(entry) = cache.as_ref().filter(|entry| {
                entry.revision == snapshot.revision && entry.instant.elapsed().as_secs() < 10
            }) {
                health.runtime_checked_at = Some(entry.checked_at);
                propagate(config, &mut health, entry.issues.clone());
                return health;
            }
        }
        let mut targets: BTreeSet<String> = config.services.values().map(service_backend).collect();
        targets.extend(config.shares.values().map(|share| share.protocol.clone()));
        use futures_util::{StreamExt, stream};
        let mut checks = stream::iter(targets)
            .map(|backend| async move {
                backend_prerequisites(state, quadlet_available, &backend).await
            }).buffer_unordered(4);
        let mut native = Vec::new();
        while let Some(mut issues) = checks.next().await {
            native.append(&mut issues);
        }
        native.sort_by(|a, b| (&a.resource, &a.field, &a.code).cmp(&(&b.resource, &b.field, &b.code)));
        let checked_at = Utc::now();
        health.runtime_checked_at = Some(checked_at);
        *cache = Some(CachedPrerequisites {
            revision: snapshot.revision.clone(), instant: std::time::Instant::now(),
            checked_at, issues: native.clone(),
        });
        propagate(config, &mut health, native);
        health
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn mount_flags_and_visible_mount_match_reference() {
        let mounts = parse_mounts(
            "1 0 8:1 / / rw - ext4 /dev/root rw\n\
             2 1 8:2 / /data\\040disk rw - ext4 /dev/data ro\n\
             3 1 8:3 / /data\\040disk rw - ext4 /dev/new rw",
        )
        .unwrap();
        assert!(mounts[1].read_only);
        assert_eq!(
            mounted_filesystem(Path::new("/data disk/child"), &mounts)
                .unwrap()
                .source,
            "/dev/new"
        );
        assert_eq!(
            mounted_filesystem(Path::new("/data disk2"), &mounts)
                .unwrap()
                .source,
            "/dev/root"
        );
        assert!(parse_mounts("malformed").is_none());
    }

    #[test]
    fn checks_required_data_without_creating_it_or_reading_secrets() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().to_owned());
        let storage = root.path().join("storage");
        fs::create_dir(&storage).unwrap();
        let secret = root.path().join("secret.env");
        fs::write(&secret, "DO_NOT_RETURN=secret").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).unwrap();
        let config = super::super::parse(b"version: 1\nstorage:\n  data: {kind: directory, path: data}\nsecrets:\n  key: secret.env\nservices:\n  app:\n    kind: container\n    engine: podman\n    backend: quadlet\n    containers:\n      web:\n        image: example/image:1\n        env_files: [key]\n        mounts: [{storage: data, target: /data}]\n").unwrap();
        let health = check_files(&store, &config, &storage, None);
        assert!(
            health
                .issues
                .iter()
                .any(|i| i.resource == "storage/data" && i.code == "missing_path")
        );
        assert!(
            health
                .issues
                .iter()
                .any(|i| i.resource == "secret/key" && i.code == "secret_permissions")
        );
        assert_eq!(
            health
                .issues
                .iter()
                .filter(|i| i.resource == "service/app" && i.code == "blocked_reference")
                .count(),
            2
        );
        assert!(!storage.join("data").exists());
        assert!(
            !serde_json::to_string(&health)
                .unwrap()
                .contains("DO_NOT_RETURN")
        );
        symlink(root.path(), storage.join("data")).unwrap();
        let health = check_files(&store, &config, &storage, None);
        assert!(
            health
                .issues
                .iter()
                .any(|i| i.resource == "storage/data" && i.code == "path_escape")
        );
    }

    #[test]
    fn overlapping_host_ports_are_warnings_and_protocol_specific() {
        let mut config = Config::default();
        for (id, protocol) in [("one", "tcp"), ("two", "tcp"), ("three", "udp")] {
            let container = super::super::Container {
                ports: vec![format!("8080:80/{protocol}"), "80".into()],
                ..Default::default()
            };
            let service = super::super::Service {
                kind: "container".into(),
                containers: BTreeMap::from([("web".into(), container)]),
                ..Default::default()
            };
            config.services.insert(id.into(), service);
        }
        let issues = port_issues(&config);
        assert_eq!(issues.len(), 2);
        assert!(
            issues
                .iter()
                .all(|i| i.severity == "warning" && i.code == "host_port_overlap")
        );
    }
}
