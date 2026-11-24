//! Direct port of the reference implementation shares/backend.go List and definitions.go InspectDefinitions.
//! Uses the shared typed definition and native parser modules.
use anyhow::{Result, ensure};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use super::{
    Failure, MAX_TAIL, OperationResult, PackageBackend, Policy, SYSTEMD_ENV, Tail, admin_packages,
    capture, capture_in, program, run, safe_system_unit, service, string,
};
use crate::core::declarative::{
    native_shares::{
        Definition, Export, connections, option_flag, option_value, parse_exports, parse_samba,
        share_key,
    },
    share_definitions::{self as defs, Snapshot},
};
use tokio::{
    sync::watch,
    time::{Duration, timeout},
};

const SAMBA_MAIN: &str = "/etc/samba/smb.conf";
const SAMBA_MANAGED: &str = "/etc/samba/maestro.conf";
const NFS_MAIN: &str = "/etc/exports";
const NFS_DIR: &str = "/etc/exports.d";
const NFS_MANAGED: &str = "/etc/exports.d/maestro.exports";

fn file(protocol: &str) -> &'static str {
    if protocol == "samba" {
        SAMBA_MANAGED
    } else {
        NFS_MANAGED
    }
}

fn shadow(data: &[u8], candidate: &str) -> Result<Vec<u8>> {
    let mut count = 0;
    let text = std::str::from_utf8(data)?;
    let result: Vec<_> = text
        .split('\n')
        .map(|line| {
            if !line.trim_start().starts_with(['#', ';']) {
                if let Some((key, value)) = line.split_once('=') {
                    if share_key(key) == "include" && value.trim() == SAMBA_MANAGED {
                        count += 1;
                        return format!("{key}= {candidate}");
                    }
                }
            }
            line.to_owned()
        })
        .collect();
    ensure!(
        count == 1,
        "Add exactly one literal include = {SAMBA_MANAGED} at the end of the main Samba config before managing shares"
    );
    Ok(result.join("\n").into_bytes())
}

async fn samba_effective(file: &str) -> Result<Vec<Definition>> {
    let native = program("testparm").ok_or_else(|| anyhow::anyhow!("testparm is unavailable"))?;
    let result = capture_in(
        &native,
        &["--suppress-prompt", file],
        &[],
        7,
        Path::new("/etc/samba"),
    )
    .await;
    let result = result
        .ok()
        .filter(|result| result.code == Some(0))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Samba validation failed; inspect the configuration with testparm on the host"
            )
        })?;
    parse_samba(&result.text)
}

async fn samba_connections() -> Result<BTreeMap<String, usize>> {
    let native = program("smbstatus").ok_or_else(|| anyhow::anyhow!("smbstatus is unavailable"))?;
    let result = capture(
        &native,
        &["--json", "--shares", "--configfile", SAMBA_MAIN],
        &[],
        3,
    )
    .await?;
    ensure!(
        result.code == Some(0),
        "Samba connection state is unavailable"
    );
    connections(&result.text)
}

async fn discovery_available() -> bool {
    let Some(native) = program("systemctl") else {
        return false;
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        for unit in ["wsdd-server.service", "wsdd.service"] {
            if capture(
                &native,
                &[
                    "--system",
                    "--no-pager",
                    "--no-ask-password",
                    "show",
                    "--property=LoadState",
                    "--value",
                    "--",
                    unit,
                ],
                SYSTEMD_ENV,
                3,
            )
            .await
            .is_ok_and(|result| result.code == Some(0) && result.text.trim() == "loaded")
            {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

fn same_samba(a: &Definition, b: &Definition) -> bool {
    let mut users_a = a.users.clone();
    let mut users_b = b.users.clone();
    users_a.sort();
    users_b.sort();
    a.path == b.path
        && a.read_only == b.read_only
        && a.guest == b.guest
        && a.browseable == b.browseable
        && users_a == users_b
}

fn nfs_configured() -> Result<Vec<Export>> {
    let mut paths = vec![PathBuf::from(NFS_MAIN)];
    match fs::read_dir(NFS_DIR) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with('.') && name.ends_with(".exports") {
                    paths.push(entry.path());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    ensure!(paths.len() <= 65, "NFS configuration exceeds 64 fragments");
    paths[1..].sort();
    let mut result = Vec::new();
    let mut total = 0;
    for path in paths {
        if path == Path::new(NFS_MANAGED) {
            continue;
        }
        let data = defs::read_file(&path, false)?;
        total += data.data.len();
        ensure!(total <= defs::MAX_FILE, "NFS configuration exceeds 1 MiB");
        result.extend(parse_exports(std::str::from_utf8(&data.data)?)?);
        ensure!(
            result.len() <= 4096,
            "NFS configured inventory exceeds 4096 entries"
        );
    }
    Ok(result)
}

async fn nfs_active() -> Result<Vec<Export>> {
    let native = program("exportfs").ok_or_else(|| anyhow::anyhow!("exportfs is unavailable"))?;
    let result = capture(&native, &["-s"], &[], 5).await?;
    ensure!(result.code == Some(0), "NFS export table is unavailable");
    parse_exports(&result.text)
}

type Key = (String, String);
fn key(e: &Export) -> Key {
    (e.path.clone(), e.client.clone())
}
fn pairs(definitions: &[Definition]) -> BTreeMap<Key, Export> {
    let mut result = BTreeMap::new();
    for d in definitions {
        for selector in &d.clients {
            let e = Export {
                path: d.path.clone(),
                client: selector.clone(),
                options: defs::options(d).split(',').map(str::to_owned).collect(),
            };
            result.insert(key(&e), e);
        }
    }
    result
}
fn export_map(items: &[Export]) -> BTreeMap<Key, &Export> {
    items.iter().map(|e| (key(e), e)).collect()
}
fn same_export(a: &Export, b: &Export) -> bool {
    [
        ("ro", "rw", true),
        ("root_squash", "no_root_squash", true),
        ("sync", "async", true),
        ("all_squash", "no_all_squash", false),
        ("secure", "insecure", true),
        ("subtree_check", "no_subtree_check", true),
    ]
    .into_iter()
    .all(|(yes, no, default)| {
        option_flag(&a.options, yes, no, default) == option_flag(&b.options, yes, no, default)
    }) && option_value(&a.options, "sec", "sys") == option_value(&b.options, "sec", "sys")
}
fn row(
    d: &Definition,
    protocol: &str,
    managed: bool,
    configured: bool,
    runtime: &str,
    connections: Option<usize>,
) -> Value {
    let mut value = serde_json::to_value(d).expect("typed definition");
    value["protocol"] = json!(protocol);
    value["managed"] = json!(managed);
    value["configured"] = json!(configured);
    value["runtime"] = json!(runtime);
    if let Some(connections) = connections {
        value["connections"] = json!(connections);
    }
    value
}

fn samba_row(
    d: &Definition,
    managed: bool,
    configured: bool,
    connected: Option<&BTreeMap<String, usize>>,
) -> Value {
    let count = connected.map(|map| *map.get(&share_key(&d.name)).unwrap_or(&0));
    row(
        d,
        "samba",
        managed,
        configured,
        match count {
            None => "unknown",
            Some(0) => "not_observed",
            Some(_) => "connected",
        },
        count,
    )
}

async fn list(protocol: &str) -> OperationResult<Value> {
    if !matches!(protocol, "" | "samba" | "nfs") {
        return Err(Failure::invalid("Unsupported share protocol"));
    }
    let mut backends = Vec::new();
    let mut shares = Vec::new();
    let mut warnings = Vec::<String>::new();
    for p in ["samba", "nfs"] {
        if !protocol.is_empty() && protocol != p {
            continue;
        }
        let mut info = json!({"protocol":p,"managedFile":file(p),"revision":"","available":false,
            "discoveryAvailable":false,"canManage":false,"setupRequired":false});
        let mut reason = String::new();
        let owned = match defs::read_file(Path::new(file(p)), true) {
            Ok(snapshot) => {
                info["revision"] = json!(snapshot.revision());
                snapshot
            }
            Err(_) => {
                reason = "Managed fragment is inaccessible or is a symlink".into();
                Snapshot::default()
            }
        };
        let definitions = match defs::load(p, &owned) {
            Ok(definitions) => definitions,
            Err(error) => {
                reason = error.to_string();
                Vec::new()
            }
        };
        if p == "samba" {
            let discovery = discovery_available().await;
            let available = program("testparm").is_some();
            let control = program("smbcontrol").is_some();
            let mut can_manage = available && control && reason.is_empty();
            let main = defs::read_file(Path::new(SAMBA_MAIN), false);
            let setup = match &main {
                Ok(snapshot) if snapshot.exists => match shadow(&snapshot.data, SAMBA_MANAGED) {
                    Ok(_) => false,
                    Err(error) => {
                        warnings.push(error.to_string());
                        true
                    }
                },
                _ => {
                    warnings.push("Samba main configuration is inaccessible or missing".into());
                    true
                }
            };
            can_manage &= !setup;
            if !available {
                reason = "testparm is unavailable; install Samba tools and restart maestro".into();
            } else if !control {
                reason = "smbcontrol is unavailable; inventory can be read but changes cannot be applied".into();
            }
            if available && !discovery {
                warnings.push("WS-Discovery server service is unavailable; install or repair the wsdd server for Windows network discovery".into());
            }
            let effective = if available && main.is_ok_and(|snapshot| snapshot.exists) {
                match samba_effective(SAMBA_MAIN).await {
                    Ok(definitions) => definitions,
                    Err(error) => {
                        warnings.push(error.to_string());
                        can_manage = false;
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            let connected = samba_connections().await.ok();
            if connected.is_none() {
                warnings
                    .push("Samba connection state is unavailable; check smbstatus access".into());
            }
            let seen: BTreeMap<_, _> = definitions.iter().map(|d| (d.name.clone(), d)).collect();
            let mut reported = BTreeSet::new();
            for d in &definitions {
                reported.insert(d.name.clone());
                let configured = effective
                    .iter()
                    .find(|actual| share_key(&actual.name) == d.name)
                    .is_some_and(|actual| same_samba(actual, d));
                shares.push(samba_row(d, true, configured, connected.as_ref()));
            }
            for d in &effective {
                let key = share_key(&d.name);
                if seen.get(&key).is_some_and(|owned| same_samba(owned, d)) {
                    continue;
                }
                reported.insert(key);
                shares.push(samba_row(d, false, true, connected.as_ref()));
            }
            for (name, count) in connected.into_iter().flatten() {
                if count > 0 && !reported.contains(&name) {
                    shares.push(row(
                        &Definition {
                            name,
                            read_only: true,
                            ..Definition::default()
                        },
                        p,
                        false,
                        false,
                        "connected",
                        Some(count),
                    ));
                }
            }
            info["available"] = json!(available);
            info["discoveryAvailable"] = json!(discovery);
            info["canManage"] = json!(can_manage);
            info["setupRequired"] = json!(setup);
            info["notice"] = json!(
                "Connection counts show current use, not complete daemon configuration. Existing sessions can retain old settings after reload."
            );
        } else {
            let available = program("exportfs").is_some();
            let setup = !Path::new(NFS_DIR).is_dir();
            let mut can_manage = available && reason.is_empty() && !setup;
            if !available {
                reason =
                    "exportfs is unavailable; install the NFS server utilities and restart maestro"
                        .into();
            }
            let configured = nfs_configured().unwrap_or_else(|_| {
                warnings.push(
                    "Unmanaged NFS configuration could not be fully read; changes are blocked"
                        .into(),
                );
                can_manage = false;
                Vec::new()
            });
            let active = nfs_active().await;
            if active.is_err() {
                warnings.push("NFS export table is unavailable; check exportfs permissions".into());
                can_manage = false;
            }
            let known = active.is_ok();
            let active = active.unwrap_or_default();
            let registered = export_map(&active);
            let owned_pairs = pairs(&definitions);
            for d in &definitions {
                let mut runtime = if known { "exported" } else { "unknown" };
                if known {
                    for (key, want) in pairs(std::slice::from_ref(d)) {
                        let Some(actual) = registered.get(&key) else {
                            runtime = "not_exported";
                            break;
                        };
                        if !same_export(actual, &want) {
                            runtime = "mismatch";
                        }
                    }
                }
                shares.push(row(d, p, true, true, runtime, None));
            }
            let unmanaged = export_map(&configured);
            let mut displayed = BTreeSet::new();
            for e in configured.iter().chain(active.iter()) {
                let key = key(e);
                if (owned_pairs.contains_key(&key) && !unmanaged.contains_key(&key))
                    || !displayed.insert(key.clone())
                {
                    continue;
                }
                let d = Definition {
                    name: e.path.clone(),
                    path: e.path.clone(),
                    read_only: option_flag(&e.options, "ro", "rw", true),
                    clients: vec![e.client.clone()],
                    root_squash: option_flag(&e.options, "root_squash", "no_root_squash", true),
                    security: option_value(&e.options, "sec", "sys").into(),
                    ..Definition::default()
                };
                let runtime = if !known {
                    "unknown"
                } else if registered.contains_key(&key) {
                    "exported"
                } else {
                    "not_exported"
                };
                shares.push(row(
                    &d,
                    p,
                    false,
                    unmanaged.contains_key(&key),
                    runtime,
                    None,
                ));
            }
            info["available"] = json!(available);
            info["canManage"] = json!(can_manage);
            info["setupRequired"] = json!(setup);
            info["notice"] = json!(
                "Runtime state reflects the registered export table, not a verified client mount. Filesystem permissions and NFS server setup still apply."
            );
        }
        if !reason.is_empty() {
            info["reason"] = json!(reason);
        }
        backends.push(info);
    }
    shares.sort_by(|a, b| {
        a["protocol"]
            .as_str()
            .cmp(&b["protocol"].as_str())
            .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
    });
    Ok(json!({"backends":backends,"shares":shares,"warnings":warnings}))
}

async fn inspect(protocol: &str, definitions: &[Definition]) -> OperationResult<Value> {
    let snapshot = defs::read_file(Path::new(file(protocol)), true)
        .map_err(|e| Failure::failed(e.to_string()))?;
    defs::load(protocol, &snapshot).map_err(|e| Failure::failed(e.to_string()))?;
    let desired =
        defs::render(protocol, definitions).map_err(|e| Failure::invalid(e.to_string()))?;
    let inventory = list(protocol).await?;
    let backend = &inventory["backends"][0];
    let available = backend["canManage"] == true;
    let mut runtime_matches = available;
    for want in definitions {
        let actual = inventory["shares"]
            .as_array()
            .expect("share list")
            .iter()
            .find(|row| row["managed"] == true && row["name"] == want.name);
        if !actual.is_some_and(|row| {
            row["configured"] == true && (protocol != "nfs" || row["runtime"] == "exported")
        }) {
            runtime_matches = false;
        }
    }
    let mut result = json!({"revision":snapshot.revision(),"matches":snapshot.data==desired || (!snapshot.exists && definitions.is_empty()),
        "runtimeMatches":runtime_matches,"available":available,"discoveryAvailable":backend["discoveryAvailable"]});
    let reason = if backend["setupRequired"] == true {
        "Native share server setup is required"
    } else {
        backend["reason"].as_str().unwrap_or_default()
    };
    if !reason.is_empty() {
        result["reason"] = json!(reason);
    }
    Ok(result)
}

// Candidate checks mirror shares/backend.go definitionsTransaction and
// shares/samba.go validateSamba. Validation has no activation or publication.
async fn validate_samba(
    next: &[Definition],
    previous: &[Definition],
    candidate: &defs::Candidate,
) -> OperationResult<()> {
    let fail = |error: anyhow::Error| Failure::failed(error.to_string());
    let main = defs::read_file(Path::new(SAMBA_MAIN), false)
        .ok()
        .filter(|snapshot| snapshot.exists)
        .ok_or_else(|| Failure::failed("Samba main config is inaccessible"))?;
    let directory = Path::new("/etc/samba");
    let shadow_file = defs::Candidate::new(
        directory,
        &shadow(&main.data, candidate.text()).map_err(fail)?,
        0o600,
    )
    .map_err(fail)?;
    let empty = defs::Candidate::new(directory, &defs::render("samba", &[]).map_err(fail)?, 0o600)
        .map_err(fail)?;
    let unmanaged_file = defs::Candidate::new(
        directory,
        &shadow(&main.data, empty.text()).map_err(fail)?,
        0o600,
    )
    .map_err(fail)?;
    let unmanaged = samba_effective(unmanaged_file.text()).await.map_err(fail)?;
    for managed in next.iter().chain(previous) {
        if unmanaged
            .iter()
            .any(|other| managed.name == share_key(&other.name))
        {
            return Err(Failure::failed(
                "An unmanaged Samba section conflicts with a managed share name",
            ));
        }
    }
    let effective = samba_effective(shadow_file.text()).await.map_err(fail)?;
    for d in next {
        if !effective
            .iter()
            .find(|actual| share_key(&actual.name) == d.name)
            .is_some_and(|actual| same_samba(d, actual))
        {
            return Err(Failure::failed(
                "Samba effective configuration does not match the managed share; check includes and overrides",
            ));
        }
    }
    if !defs::read_file(Path::new(SAMBA_MAIN), false).is_ok_and(|latest| latest.data == main.data) {
        return Err(Failure::failed(
            "Samba main config changed during validation",
        ));
    }
    Ok(())
}

fn check_nfs_conflicts(
    previous: &[Definition],
    next: &[Definition],
    configured: &[Export],
    active: &[Export],
) -> OperationResult<()> {
    let old_pairs = pairs(previous);
    let mut touched = pairs(previous);
    touched.extend(pairs(next));
    let safe_options = regex::Regex::new(r"^[A-Za-z0-9_,=:.+/-]+$").unwrap();
    for e in active {
        if touched.contains_key(&key(e)) && !safe_options.is_match(&e.options.join(",")) {
            return Err(Failure::failed(
                "Existing runtime options cannot be safely restored; resolve them with native NFS tools first",
            ));
        }
    }
    for d in previous.iter().chain(next) {
        if configured.iter().any(|e| e.path == d.path) {
            return Err(Failure::invalid(
                "An unmanaged NFS export already uses this path",
            ));
        }
    }
    for d in next {
        if active
            .iter()
            .any(|e| e.path == d.path && !old_pairs.contains_key(&key(e)))
        {
            return Err(Failure::invalid(
                "An unmanaged runtime NFS export already uses this path",
            ));
        }
    }
    Ok(())
}

struct Prepared {
    candidate: defs::Candidate,
    data: Vec<u8>,
    active: Vec<Export>,
}

async fn validate_candidate(
    protocol: &str,
    old: &Snapshot,
    previous: &[Definition],
    next: &[Definition],
) -> OperationResult<Prepared> {
    let fail = |error: anyhow::Error| Failure::failed(error.to_string());
    let data = defs::render(protocol, next).map_err(|e| Failure::invalid(e.to_string()))?;
    let mut active = Vec::new();
    if protocol == "samba" {
        if program("testparm").is_none() || program("smbcontrol").is_none() {
            return Err(Failure::unavailable(
                "Samba testparm and smbcontrol are required",
            ));
        }
        let effective = samba_effective(SAMBA_MAIN).await.map_err(fail)?;
        let desired: BTreeSet<_> = next.iter().map(|d| d.name.as_str()).collect();
        let owned: BTreeSet<_> = previous.iter().map(|d| d.name.as_str()).collect();
        for d in effective {
            let name = share_key(&d.name);
            if desired.contains(name.as_str()) && !owned.contains(name.as_str()) {
                return Err(Failure::invalid(
                    "An unmanaged Samba share already uses this name",
                ));
            }
        }
    } else {
        if program("exportfs").is_none() {
            return Err(Failure::unavailable("exportfs is required"));
        }
        let configured = nfs_configured()
            .map_err(|_| Failure::failed("Cannot safely inspect unmanaged NFS exports"))?;
        active = nfs_active()
            .await
            .map_err(|_| Failure::failed("Cannot snapshot the current NFS export table"))?;
        check_nfs_conflicts(previous, next, &configured, &active)?;
    }
    let path = Path::new(file(protocol));
    let candidate = defs::Candidate::new(
        path.parent().expect("fixed managed path"),
        &data,
        if old.exists { old.mode } else { 0o644 },
    )
    .map_err(|e| Failure::failed(format!("Create managed fragment candidate: {e}")))?;
    if protocol == "samba" {
        validate_samba(next, previous, &candidate).await?;
    }
    if !defs::same_file(path, old) {
        return Err(Failure::failed(
            "Managed fragment changed during validation; refresh and retry",
        ));
    }
    Ok(Prepared {
        candidate,
        data,
        active,
    })
}

// Native activation and rollback from shares/nfs.go and shares/samba.go.
// Only the entries touched by the managed definitions may be changed.
fn target(e: &Export) -> String {
    if e.client.contains(':') {
        let selector = match e.client.split_once('/') {
            Some((address, bits)) => format!("[{address}]/{bits}"),
            None => format!("[{}]", e.client),
        };
        format!("{selector}:{}", e.path)
    } else {
        format!("{}:{}", e.client, e.path)
    }
}

async fn export(e: &Export, remove: bool, output: &mut Tail) -> Result<()> {
    let native = program("exportfs").ok_or_else(|| anyhow::anyhow!("exportfs is unavailable"))?;
    let target = target(e);
    let options = e.options.join(",");
    let args = if remove {
        vec!["-u", target.as_str()]
    } else {
        ensure!(
            regex::Regex::new(r"^[A-Za-z0-9_,=:.+/-]+$")
                .unwrap()
                .is_match(&options),
            "unsupported runtime options prevent safe restoration"
        );
        vec!["-i", "-o", options.as_str(), target.as_str()]
    };
    ensure!(
        run(&native, &args, &[], 10, false, output).await? == Some(0),
        "exportfs failed"
    );
    Ok(())
}

fn verify_nfs(
    old: &BTreeMap<Key, Export>,
    next: &BTreeMap<Key, Export>,
    actual: &[Export],
) -> Result<()> {
    let current = export_map(actual);
    for (key, want) in next {
        ensure!(
            current.get(key).is_some_and(|got| same_export(got, want)),
            "NFS runtime export table does not match the requested access"
        );
    }
    for key in old.keys() {
        ensure!(
            next.contains_key(key) || !current.contains_key(key),
            "Removed NFS client remains registered"
        );
    }
    Ok(())
}

async fn apply_nfs(
    before: &[Definition],
    after: &[Definition],
    active: &[Export],
    output: &mut Tail,
    cancel: &watch::Receiver<bool>,
) -> Result<()> {
    let old = pairs(before);
    let next = pairs(after);
    let current = export_map(active);
    for e in next.values() {
        ensure!(!*cancel.borrow(), "context canceled");
        export(e, false, output)
            .await
            .map_err(|_| anyhow::anyhow!("NFS export apply failed"))?;
    }
    for (key, e) in &old {
        if !next.contains_key(key) && current.contains_key(key) {
            ensure!(!*cancel.borrow(), "context canceled");
            export(e, true, output)
                .await
                .map_err(|_| anyhow::anyhow!("NFS export removal failed"))?;
        }
    }
    ensure!(!*cancel.borrow(), "context canceled");
    let actual = nfs_active()
        .await
        .map_err(|_| anyhow::anyhow!("NFS runtime verification failed"))?;
    verify_nfs(&old, &next, &actual)
}

async fn restore_nfs(
    before: &[Definition],
    after: &[Definition],
    active: &[Export],
    output: &mut Tail,
) -> Result<()> {
    let initial = export_map(active);
    let current_list = nfs_active().await?;
    let current = export_map(&current_list);
    let mut touched = pairs(before);
    touched.extend(pairs(after));
    let mut failures = 0;
    for key in touched.keys() {
        let result = if let Some(old) = initial.get(key) {
            export(old, false, output).await
        } else if let Some(now) = current.get(key) {
            export(now, true, output).await
        } else {
            Ok(())
        };
        if result.is_err() {
            failures += 1;
        }
    }
    ensure!(
        failures == 0,
        "NFS runtime rollback failed for {failures} entries"
    );
    let restored = nfs_active().await?;
    let restored = export_map(&restored);
    for key in touched.keys() {
        let old = initial.get(key);
        let now = restored.get(key);
        ensure!(
            match (old, now) {
                (None, None) => true,
                (Some(old), Some(now)) => old.options == now.options,
                _ => false,
            },
            "NFS runtime rollback verification failed"
        );
    }
    Ok(())
}

async fn reload_samba() -> Result<()> {
    let native =
        program("smbcontrol").ok_or_else(|| anyhow::anyhow!("smbcontrol is unavailable"))?;
    ensure!(
        run(
            &native,
            &[
                "--configfile",
                SAMBA_MAIN,
                "--timeout",
                "5",
                "smbd",
                "reload-config"
            ],
            &[],
            7,
            false,
            &mut Tail::default()
        )
        .await?
            == Some(0),
        "Samba reload failed"
    );
    Ok(())
}

async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    // A closed channel is also cancellation; the helper owns the sender.
    let _ = cancel.wait_for(|value| *value).await;
}

fn append(output: &mut Tail, message: &str) {
    output.append(message.as_bytes(), MAX_TAIL);
}

async fn replace_definitions(
    protocol: &str,
    old: &Snapshot,
    previous: &[Definition],
    next: &[Definition],
    output: &mut Tail,
    mut cancel: watch::Receiver<bool>,
) -> OperationResult<Value> {
    append(output, "Validating managed share configuration…\n");
    let prepared = tokio::select! {
        biased;
        _ = cancelled(&mut cancel) => return Err(Failure::failed("context canceled")),
        result = validate_candidate(protocol,old,previous,next) => result?,
    };
    let path = Path::new(file(protocol));
    if *cancel.borrow() {
        return Err(Failure::failed("context canceled"));
    }
    if !defs::same_file(path, old) {
        return Err(Failure::failed(
            "Managed fragment changed during validation; refresh and retry",
        ));
    }
    if let Err(error) = prepared.candidate.publish(path) {
        if defs::read_file(path, true)
            .is_ok_and(|current| current.exists && current.data == prepared.data)
        {
            if let Err(rollback) = defs::restore(path, old, &prepared.data) {
                return Err(Failure::failed(format!(
                    "Publish failed and file rollback failed: {rollback}"
                )));
            }
        }
        return Err(Failure::failed(format!(
            "Publish managed fragment: {error}"
        )));
    }
    // Once a rename succeeds, cancellation is an activation failure, not an
    // instruction to drop the transaction. Rollback gets its own deadline.
    // Let each short native command finish or hit its own deadline before
    // restoring the export table. Dropping an in-flight mutator on signal
    // could race its side effects against the rollback.
    let result = if *cancel.borrow() {
        Err(anyhow::anyhow!("context canceled"))
    } else if protocol == "samba" {
        reload_samba().await
    } else {
        apply_nfs(previous, next, &prepared.active, output, &cancel).await
    };
    if result.is_err() || *cancel.borrow() {
        append(
            output,
            "Apply failed; restoring previous configuration and runtime entries…\n",
        );
        let file_error = defs::restore(path, old, &prepared.data).err();
        let runtime_error = if file_error.is_none() {
            match timeout(Duration::from_secs(30), async {
                if protocol == "samba" {
                    reload_samba().await
                } else {
                    restore_nfs(previous, next, &prepared.active, output).await
                }
            })
            .await
            {
                Ok(result) => result.err(),
                Err(_) => Some(anyhow::anyhow!("context deadline exceeded")),
            }
        } else {
            None
        };
        if file_error.is_some() || runtime_error.is_some() {
            let describe = |error: Option<anyhow::Error>| {
                error.map_or_else(|| "<nil>".into(), |e| e.to_string())
            };
            return Err(Failure::failed(format!(
                "Share apply failed; rollback incomplete (file: {}, runtime: {}). Inspect host state before retrying",
                describe(file_error),
                describe(runtime_error)
            )));
        }
        return Err(Failure::failed(
            "Share apply failed; previous configuration and runtime entries were restored",
        ));
    }
    append(
        output,
        "Managed share configuration applied. Shared directory data was not changed.\n",
    );
    let revision = Snapshot {
        data: prepared.data,
        exists: true,
        mode: old.mode,
    }
    .revision();
    Ok(json!({"revision":revision}))
}

// shares/setup.go prepare and updates/share_server.go. Native setup
// is separate from definition publication; it never edits firewall rules or
// installs credentials and never rewrites administrator-owned share data.
fn setup_units(
    protocol: &str,
    family: admin_packages::Family,
) -> OperationResult<(&'static str, Option<&'static str>)> {
    match protocol {
        "samba" if family == admin_packages::Family::Apt => {
            Ok(("smbd.service", Some("wsdd-server.service")))
        }
        "samba" => Ok(("smb.service", Some("wsdd.service"))),
        "nfs" => Ok(("nfs-server.service", None)),
        _ => Err(Failure::invalid("unsupported share protocol")),
    }
}

fn ensure_native_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    for ancestor in path.ancestors().filter(|p| *p != path) {
        let metadata = fs::symlink_metadata(ancestor)?;
        ensure!(
            metadata.is_dir() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
            "native share configuration directory has an untrusted ancestor"
        );
    }
    match fs::DirBuilder::new().mode(0o755).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
        "native share configuration directory is not trusted"
    );
    Ok(())
}

fn samba_setup_data(original: &Snapshot, change_notify: bool) -> Result<Option<Vec<u8>>> {
    let text = std::str::from_utf8(&original.data)?;
    let mut included = false;
    let mut global = true;
    let mut smb2 = false;
    let mut smb3 = false;
    let mut notify = None;
    let mut kernel_notify = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            global = section.trim().eq_ignore_ascii_case("global");
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim().eq_ignore_ascii_case("include") && value.trim() == SAMBA_MANAGED {
                included = true;
            }
            if global {
                let disabled = matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "no" | "false" | "0"
                );
                match share_key(key).as_str() {
                    "changenotify" => notify = Some(!disabled),
                    "kernelchangenotify" => kernel_notify = Some(!disabled),
                    "smb2leases" => smb2 = disabled,
                    "smb3directoryleases" => smb3 = disabled,
                    _ => {}
                }
            }
        }
    }
    if included {
        shadow(&original.data, SAMBA_MANAGED)?;
    }
    let notifications_match = notify == Some(change_notify) && kernel_notify == Some(change_notify);
    if included && smb2 && smb3 && notifications_match {
        return Ok(None);
    }
    let mut data = if original.exists {
        original.data.clone()
    } else {
        b"[global]\n\tworkgroup = WORKGROUP\n\tserver string = %h server (Samba)\n\tserver role = standalone server\n\tmulticast dns register = yes\n".to_vec()
    };
    // A final global section overrides earlier values without rewriting custom
    // sections. Keep the managed include unique on repeated setup runs.
    if !smb2 || !smb3 {
        data.extend_from_slice(b"\n[global]\nsmb2 leases = no\nsmb3 directory leases = no\n");
    }
    if !notifications_match {
        let value = if change_notify { "yes" } else { "no" };
        data.extend_from_slice(
            format!("\n[global]\nchange notify = {value}\nkernel change notify = {value}\n")
                .as_bytes(),
        );
    }
    if !included {
        data.extend_from_slice(format!("\ninclude = {SAMBA_MANAGED}\n").as_bytes());
    }
    Ok(Some(data))
}

async fn prepare_server(protocol: &str, change_notify: bool) -> Result<()> {
    if protocol == "nfs" {
        return ensure_native_directory(Path::new(NFS_DIR));
    }
    ensure!(protocol == "samba", "unsupported share protocol");
    let managed = defs::read_file(Path::new(SAMBA_MANAGED), true)?;
    defs::load("samba", &managed).map_err(|_| {
        anyhow::anyhow!("Samba managed filename already contains unowned or invalid configuration")
    })?;
    let path = Path::new(SAMBA_MAIN);
    let original = defs::read_file(path, true)?;
    let directory = path.parent().expect("fixed Samba path");
    ensure_native_directory(directory)?;
    let Some(data) = samba_setup_data(&original, change_notify)? else {
        return Ok(());
    };
    let candidate = defs::Candidate::new(
        directory,
        &data,
        if original.exists {
            original.mode
        } else {
            0o644
        },
    )?;
    samba_effective(candidate.text()).await?;
    ensure!(
        defs::same_file(path, &original),
        "Samba configuration changed during setup; retry"
    );
    // A directory fsync failure can follow a successful rename; attempt
    // rollback only if our exact candidate remains at the destination.
    if let Err(error) = candidate.publish(path) {
        if defs::read_file(path, true).is_ok_and(|now| now.exists && now.data == data) {
            defs::restore(path, &original, &data)?;
        }
        return Err(error);
    }
    Ok(())
}

async fn setup_action(
    unit: &str,
    action: &str,
    policy: &Policy,
    output: &mut Tail,
) -> OperationResult<Value> {
    let args = json!({"scope":"system","unit":unit});
    service(
        &format!("services_{action}"),
        args.as_object().expect("fixed service input"),
        policy,
        output,
    )
    .await
}

async fn setup(
    protocol: &str,
    change_notify: bool,
    policy: &Policy,
    output: &mut Tail,
) -> OperationResult<Value> {
    if !policy.share_writes || !policy.package_writes {
        return Err(Failure::denied(
            "Share server setup is disabled by root-owned policy",
        ));
    }
    let mut backend = PackageBackend::new();
    let family = backend
        .family
        .ok_or_else(|| Failure::invalid("unsupported dependency target or package manager"))?;
    let (unit, discovery) = setup_units(protocol, family)?;
    // Retain Maestro's existing independent service authorization. Check
    // mandatory units before installing packages; optional NetBIOS units are
    // attempted only when specifically allowed.
    for required in std::iter::once(unit).chain(discovery) {
        if !safe_system_unit(required)
            || !policy
                .system_units
                .iter()
                .any(|allowed| allowed == required)
        {
            return Err(Failure::denied(format!(
                "Share server setup requires root policy authorization for {required}"
            )));
        }
    }
    let names = admin_packages::dependency_packages(family, &backend.distribution_id, protocol)
        .map_err(|_| Failure::invalid("unsupported dependency target or package manager"))?;
    backend.install(Some(names), output).await?;
    prepare_server(protocol, change_notify)
        .await
        .map_err(|e| Failure::failed(e.to_string()))?;
    for action in ["enable", "start"] {
        setup_action(unit, action, policy, output).await?;
    }
    if protocol == "samba" {
        reload_samba()
            .await
            .map_err(|e| Failure::failed(e.to_string()))?;
    }
    if let Some(discovery) = discovery {
        for action in ["enable", "start"] {
            setup_action(discovery, action, policy, output)
                .await
                .map_err(|e| {
                    Failure::failed(format!("WS-Discovery setup ({discovery}): {}", e.message))
                })?;
        }
        for optional in ["nmbd.service", "nmb.service"] {
            if policy
                .system_units
                .iter()
                .any(|allowed| allowed == optional)
            {
                for action in ["enable", "start"] {
                    let _ = setup_action(optional, action, policy, output).await;
                }
            }
        }
    }
    Ok(json!({"protocol":protocol,"unit":unit,"action":"server configured and started"}))
}

pub(super) async fn execute(
    operation: &str,
    input: &Map<String, Value>,
    policy: &Policy,
    output: &mut Tail,
    cancel: watch::Receiver<bool>,
) -> OperationResult<Value> {
    let protocol = string(input, "protocol");
    if operation == "shares_list" {
        return list(protocol).await;
    }
    if operation == "shares_setup" {
        let change_notify = match input.get("changeNotify") {
            None => true,
            Some(Value::Bool(value)) => *value,
            _ => return Err(Failure::invalid("changeNotify must be a boolean")),
        };
        return setup(protocol, change_notify, policy, output).await;
    }
    let definitions =
        defs::decode(string(input, "definitions")).map_err(|e| Failure::invalid(e.to_string()))?;
    let definitions = defs::normalize(
        protocol,
        definitions,
        operation != "shares_definitions_inspect",
    )
    .map_err(|e| Failure::invalid(e.to_string()))?;
    if operation == "shares_definitions_inspect" {
        return inspect(protocol, &definitions).await;
    }
    if operation == "shares_definitions_apply" && !policy.share_writes {
        return Err(Failure::denied(
            "Share publication is disabled by root-owned policy",
        ));
    }
    let snapshot = defs::read_file(Path::new(file(protocol)), true).map_err(|e| {
        Failure::failed(if operation == "shares_definitions_apply" {
            "managed share output is inaccessible".into()
        } else {
            e.to_string()
        })
    })?;
    if operation == "shares_definitions_apply" && snapshot.revision() != string(input, "revision") {
        return Err(Failure::new(
            "conflict",
            409,
            "Managed share output changed; recheck configuration",
        ));
    }
    let previous = defs::load(protocol, &snapshot).map_err(|e| Failure::failed(e.to_string()))?;
    if operation == "shares_definitions_validate" {
        validate_candidate(protocol, &snapshot, &previous, &definitions).await?;
        return Ok(json!({"valid":true}));
    }
    replace_definitions(protocol, &snapshot, &previous, &definitions, output, cancel).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_preserves_main_file_and_uses_fixed_units() {
        let original = Snapshot {
            data: b"[global]\nworkgroup = CUSTOM\n[existing]\npath = /srv/data\n".to_vec(),
            exists: true,
            mode: 0o640,
        };
        let added = samba_setup_data(&original, true).unwrap().unwrap();
        assert!(added.starts_with(&original.data));
        assert!(
            String::from_utf8_lossy(&added).ends_with(&format!("\ninclude = {SAMBA_MANAGED}\n"))
        );
        assert!(
            String::from_utf8_lossy(&added)
                .contains("[global]\nsmb2 leases = no\nsmb3 directory leases = no\n")
        );
        let configured = Snapshot {
            data: added.clone(),
            exists: true,
            mode: 0o640,
        };
        assert!(samba_setup_data(&configured, true).unwrap().is_none());
        let duplicate = Snapshot {
            data: [added, format!("include = {SAMBA_MANAGED}\n").into_bytes()].concat(),
            exists: true,
            mode: 0o640,
        };
        assert!(samba_setup_data(&duplicate, true).is_err());
        assert_eq!(
            setup_units("samba", admin_packages::Family::Apt).unwrap(),
            ("smbd.service", Some("wsdd-server.service"))
        );
        assert_eq!(
            setup_units("samba", admin_packages::Family::Dnf).unwrap(),
            ("smb.service", Some("wsdd.service"))
        );
        assert_eq!(
            setup_units("nfs", admin_packages::Family::Pacman).unwrap(),
            ("nfs-server.service", None)
        );
        assert!(setup_units("unknown", admin_packages::Family::Apt).is_err());
    }

    #[test]
    fn setup_disables_leases_in_existing_global_configuration() {
        for text in [
            format!(
                "[global]\nsmb2 leases = yes\nsmb3 directory leases = auto\ninclude = {SAMBA_MANAGED}\n[docs]\npath = /srv/docs\n"
            ),
            format!(
                "[docs]\npath = /srv/docs\nsmb2 leases = no\nsmb3 directory leases = no\ninclude = {SAMBA_MANAGED}\n"
            ),
            String::new(),
        ] {
            let original = Snapshot {
                data: text.into_bytes(),
                exists: true,
                mode: 0o644,
            };
            let data = samba_setup_data(&original, true).unwrap().unwrap();
            assert!(data.starts_with(&original.data));
            assert!(
                String::from_utf8_lossy(&data)
                    .contains("[global]\nsmb2 leases = no\nsmb3 directory leases = no\n")
            );
            let configured = Snapshot {
                data,
                exists: true,
                mode: 0o644,
            };
            assert!(samba_setup_data(&configured, true).unwrap().is_none());
        }
    }

    #[test]
    fn setup_toggles_both_notification_settings_together() {
        let mut snapshot = Snapshot::default();
        for enabled in [true, false, true] {
            let data = samba_setup_data(&snapshot, enabled).unwrap().unwrap();
            let value = if enabled { "yes" } else { "no" };
            assert!(
                String::from_utf8_lossy(&data).ends_with(&format!(
                    "change notify = {value}\nkernel change notify = {value}\n"
                )) || !snapshot.exists
            );
            snapshot = Snapshot {
                data,
                exists: true,
                mode: 0o644,
            };
            assert!(samba_setup_data(&snapshot, enabled).unwrap().is_none());
        }
    }

    #[test]
    fn nfs_verification_is_limited_to_touched_entries() {
        let old = Export {
            path: "/srv/docs".into(),
            client: "192.0.2.0/24".into(),
            options: vec!["ro".into()],
        };
        let mut next = old.clone();
        next.client = "2001:db8::/64".into();
        let unrelated = Export {
            path: "/srv/other".into(),
            client: "*".into(),
            options: vec!["rw".into()],
        };
        assert_eq!(target(&next), "[2001:db8::]/64:/srv/docs");
        let before = BTreeMap::from([(key(&old), old.clone())]);
        let after = BTreeMap::from([(key(&next), next.clone())]);
        assert!(verify_nfs(&before, &after, &[next.clone(), unrelated.clone()]).is_ok());
        assert!(verify_nfs(&before, &after, &[old, next.clone(), unrelated.clone()]).is_err());
        assert!(verify_nfs(&before, &after, &[unrelated]).is_err());
        let mut wrong = next;
        wrong.options = vec!["rw".into()];
        assert!(verify_nfs(&before, &after, &[wrong]).is_err());
    }

    #[test]
    fn nfs_conflicts_block_unmanaged_and_unrestorable_entries() {
        let next = defs::normalize("nfs", defs::decode(r#"[{"name":"docs","path":"/srv/docs","clients":["192.0.2.0/24"],"rootSquash":true,"security":"sys"}]"#).unwrap(), false).unwrap();
        let owned: Vec<_> = pairs(&next).into_values().collect();
        assert!(check_nfs_conflicts(&[], &next, &[], &owned).is_err());
        assert!(check_nfs_conflicts(&next, &next, &owned, &[]).is_err());
        assert!(check_nfs_conflicts(&next, &next, &[], &owned).is_ok());
        let mut unsafe_active = owned;
        unsafe_active[0].options.push("bad option".into());
        assert!(check_nfs_conflicts(&next, &next, &[], &unsafe_active).is_err());
    }

    #[test]
    fn reference_samba_include_and_nfs_flags() {
        let main = format!("[global]\n# include = {SAMBA_MANAGED}\ninclude = {SAMBA_MANAGED}\n");
        assert_eq!(
            String::from_utf8(shadow(main.as_bytes(), "/tmp/candidate").unwrap()).unwrap(),
            format!("[global]\n# include = {SAMBA_MANAGED}\ninclude = /tmp/candidate\n")
        );
        assert!(
            shadow(
                format!("{main}include = {SAMBA_MANAGED}\n").as_bytes(),
                "/tmp/candidate"
            )
            .is_err()
        );
        let a = Export {
            path: "/srv/test".into(),
            client: "*".into(),
            options: vec!["rw".into(), "sync".into()],
        };
        let mut b = a.clone();
        b.options.push("async".into());
        assert!(!same_export(&a, &b));
    }
}
