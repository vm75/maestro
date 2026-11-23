use std::{collections::BTreeMap, time::Duration};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{platforms::admin, state::AppState};

const TIMEOUT: Duration = Duration::from_secs(12);
const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_UNITS: usize = 4096;

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    #[default]
    System,
    User,
}

impl Scope {
    fn argument(self) -> &'static str {
        match self {
            Self::System => "--system",
            Self::User => "--user",
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    scope: Scope,
    available: bool,
    journal_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    services: Vec<Service>,
    warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    permission_reason: Option<String>,
}

#[derive(Serialize)]
struct Service {
    name: String,
    description: String,
    load: String,
    active: String,
    sub: String,
    enabled: String,
    writable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    preset: Option<String>,
}

impl Service {
    fn unknown(name: &str) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            load: "unknown".into(),
            active: "unknown".into(),
            sub: "unknown".into(),
            enabled: "unknown".into(),
            writable: false,
            preset: None,
        }
    }
}

#[derive(Deserialize)]
struct UnitFile {
    #[serde(rename = "unit_file")]
    name: String,
    state: String,
    preset: Option<String>,
}

#[derive(Deserialize)]
struct LoadedUnit {
    #[serde(rename = "unit")]
    name: String,
    load: String,
    active: String,
    sub: String,
    description: String,
}

pub fn available(state: &AppState) -> bool {
    !state.config.test_mode && state.config.commands.systemctl.is_file()
}

pub async fn inventory(state: &AppState, scope: Scope) -> Result<Inventory> {
    let available = available(state);
    let mut result = Inventory {
        scope,
        available,
        journal_available: available && state.config.commands.journalctl.is_file(),
        reason: None,
        services: vec![],
        warnings: vec![],
        permission_reason: None,
    };
    if !available {
        result.reason = Some("systemctl is unavailable");
        return Ok(result);
    }
    let results = futures_util::future::join_all(["list-unit-files", "list-units"].map(
        |action| async move {
            let mut args = vec![
                scope.argument(),
                "--no-pager",
                "--no-ask-password",
                action,
                "--type=service",
                "--full",
                "--plain",
                "--no-legend",
                "--output=json",
            ];
            if action == "list-units" {
                args.push("--all");
            }
            let output = state
                .runner
                .capture_bounded(&state.config.commands.systemctl, &args, TIMEOUT, MAX_BYTES)
                .await;
            (action, output)
        },
    ))
    .await;
    let mut merged = BTreeMap::new();
    for (action, output) in results {
        match output {
            Ok(output) if merge_inventory(&mut merged, action, &output).is_ok() => {}
            Ok(_) => result
                .warnings
                .push(format!("{action} returned unsupported inventory")),
            Err(_) => result.warnings.push(format!(
                "{action} unavailable; check the selected bus and permissions"
            )),
        }
        ensure!(
            merged.len() <= MAX_UNITS,
            "service inventory exceeds {MAX_UNITS} units"
        );
    }
    let helper = if matches!(scope, Scope::System) {
        let probe = admin::probe().await;
        if probe.status != "ready" {
            result.permission_reason = Some(probe.reason.clone());
        } else if write_units(state, scope).iter().any(|unit| !probe.permits_unit(unit)) {
            result.permission_reason = Some("Some configured system units are not authorized by the root-owned helper policy.".into());
        }
        Some(probe)
    } else {
        None
    };
    result.services = merged
        .into_values()
        .map(|mut unit| {
            unit.writable = writable_unit(&unit.name)
                && write_units(state, scope).contains(&unit.name)
                && helper.as_ref().is_none_or(|probe| probe.permits_unit(&unit.name));
            unit
        })
        .collect();
    Ok(result)
}


const DETAIL_PROPERTIES: [&str; 16] = [
    "Id", "Description", "LoadState", "ActiveState", "SubState", "UnitFileState",
    "MainPID", "Result", "ExecMainCode", "ExecMainStatus", "ActiveEnterTimestamp",
    "InactiveEnterTimestamp", "FragmentPath", "CanStart", "CanStop", "CanReload",
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Details {
    #[serde(flatten)]
    service: Service,
    #[serde(rename = "mainPID", skip_serializing_if = "Option::is_none")]
    main_pid: Option<u64>,
    #[serde(skip_serializing_if = "String::is_empty")]
    result: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    exec_main_code: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    exec_main_status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    active_since: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    inactive_since: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    fragment_path: String,
    can_start: bool,
    can_stop: bool,
    can_reload: bool,
}

fn parse_details(bytes: &[u8]) -> Result<Details> {
    let text = std::str::from_utf8(bytes)?;
    let mut values = BTreeMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            if DETAIL_PROPERTIES.contains(&key) {
                values.insert(key, value);
            }
        }
    }
    ensure!(
        values.get("LoadState") != Some(&"not-found"),
        "service unit not found"
    );
    let id = values.get("Id").copied().unwrap_or_default();
    ensure!(
        valid_unit(id)
            && !values.get("LoadState").copied().unwrap_or_default().is_empty()
            && !values.get("ActiveState").copied().unwrap_or_default().is_empty(),
        "unsupported systemd detail response"
    );
    let field = |key| values.get(key).copied().unwrap_or_default().to_owned();
    let mut service = Service::unknown(id);
    service.description = field("Description");
    service.load = field("LoadState");
    service.active = field("ActiveState");
    service.sub = field("SubState");
    service.enabled = field("UnitFileState");
    Ok(Details {
        service,
        main_pid: field("MainPID").parse().ok(),
        result: field("Result"),
        exec_main_code: field("ExecMainCode"),
        exec_main_status: field("ExecMainStatus"),
        active_since: field("ActiveEnterTimestamp"),
        inactive_since: field("InactiveEnterTimestamp"),
        fragment_path: field("FragmentPath"),
        can_start: field("CanStart") == "yes",
        can_stop: field("CanStop") == "yes",
        can_reload: field("CanReload") == "yes",
    })
}

pub async fn details(state: &AppState, scope: Scope, unit: &str) -> Result<Details> {
    ensure!(valid_unit(unit), "invalid service unit");
    ensure!(available(state), "systemctl is unavailable");
    let property = format!("--property={}", DETAIL_PROPERTIES.join(","));
    let output = state
        .runner
        .capture_bounded(
            &state.config.commands.systemctl,
            &[scope.argument(), "--no-pager", "--no-ask-password", "show", &property, "--", unit],
            TIMEOUT,
            64 * 1024,
        )
        .await?;
    parse_details(&output)
}

#[derive(Serialize)]
pub struct Journal {
    text: String,
    truncated: bool,
}

pub async fn logs(state: &AppState, scope: Scope, unit: &str, tail: u16) -> Result<Journal> {
    ensure!(valid_unit(unit), "invalid service unit");
    ensure!((1..=2000).contains(&tail), "invalid journal line limit");
    let lines = tail.to_string();
    let (bytes, truncated) = state
        .runner
        .capture_tail(
            &state.config.commands.journalctl,
            &[
                scope.argument(),
                "--no-pager",
                "--quiet",
                "--output=short-iso",
                match scope {
                    Scope::System => "--unit",
                    Scope::User => "--user-unit",
                },
                unit,
                "--lines",
                &lines,
            ],
            Duration::from_secs(10),
            128 * 1024,
        )
        .await?;
    Ok(Journal {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        truncated,
    })
}

pub const ACTIONS: [&str; 6] = ["start", "stop", "restart", "reload", "enable", "disable"];

pub fn writes_enabled(state: &AppState) -> bool {
    available(state)
        && matches!(state.config.platform, crate::config::Platform::Linux)
        && state.config.services.systemd_user_write_units.iter().any(|unit| writable_unit(unit))
}

fn writable_unit(unit: &str) -> bool {
    valid_unit(unit)
        && !matches!(unit, "maestro.service" | "router-hub.service")
        && !crate::core::declarative::managed_unit(unit)
}

fn write_units(state: &AppState, scope: Scope) -> &[String] {
    let config = &state.config.services;
    match scope {
        Scope::System => &config.systemd_system_write_units,
        Scope::User => &config.systemd_user_write_units,
    }
}

pub fn permits(state: &AppState, scope: Scope, unit: &str) -> bool {
    available(state)
        && matches!(state.config.platform, crate::config::Platform::Linux)
        && writable_unit(unit)
        && write_units(state, scope)
            .iter()
            .any(|allowed| allowed == unit)
}

pub async fn act(state: &AppState, scope: Scope, unit: &str, action: &str) -> Result<String> {
    ensure!(
        permits(state, scope, unit),
        "service is outside the write policy"
    );
    ensure!(ACTIONS.contains(&action), "unsupported service action");
    if matches!(scope, Scope::System) {
        return admin::system_service(unit, action).await;
    }
    let identity = state
        .runner
        .capture_bounded(
            &state.config.commands.systemctl,
            &[
                scope.argument(),
                "--no-pager",
                "--no-ask-password",
                "show",
                "--property=Id",
                "--value",
                "--",
                unit,
            ],
            TIMEOUT,
            4096,
        )
        .await?;
    let canonical = std::str::from_utf8(&identity)?.trim();
    ensure!(
        permits(state, scope, canonical),
        "canonical service is outside the write policy"
    );
    let (bytes, _) = state
        .runner
        .capture_tail(
            &state.config.commands.systemctl,
            &[
                scope.argument(),
                "--no-pager",
                "--no-ask-password",
                action,
                "--",
                canonical,
            ],
            Duration::from_secs(20 * 60),
            128 * 1024,
        )
        .await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn merge_inventory(
    merged: &mut BTreeMap<String, Service>,
    action: &str,
    output: &[u8],
) -> Result<()> {
    let text = std::str::from_utf8(output)?.trim();
    let files = action == "list-unit-files";
    let rows = if text.starts_with('[') {
        if files {
            serde_json::from_str::<Vec<UnitFile>>(text)?
                .into_iter()
                .map(|file| {
                    let mut service = Service::unknown(&file.name);
                    service.enabled = file.state;
                    service.preset = file.preset;
                    service
                })
                .collect()
        } else {
            serde_json::from_str::<Vec<LoadedUnit>>(text)?
                .into_iter()
                .map(|unit| {
                    let mut service = Service::unknown(&unit.name);
                    service.load = unit.load;
                    service.active = unit.active;
                    service.sub = unit.sub;
                    service.description = unit.description;
                    service
                })
                .collect()
        }
    } else {
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| parse_text_row(line, files))
            .collect::<Result<Vec<_>>>()?
    };
    ensure!(
        rows.iter().all(|row| valid_unit(&row.name)
            && !row.enabled.is_empty()
            && !row.load.is_empty()
            && !row.active.is_empty()
            && !row.sub.is_empty()),
        "invalid service inventory row"
    );
    for row in rows {
        let service = merged
            .entry(row.name.clone())
            .or_insert_with(|| Service::unknown(&row.name));
        if files {
            service.enabled = row.enabled;
            service.preset = row.preset;
        } else {
            service.load = row.load;
            service.active = row.active;
            service.sub = row.sub;
            service.description = row.description;
        }
    }
    Ok(())
}

fn parse_text_row(line: &str, files: bool) -> Result<Service> {
    let mut columns = line.split_whitespace();
    let mut service = Service::unknown(columns.next().unwrap_or_default());
    if files {
        service.enabled = columns.next().unwrap_or_default().into();
        service.preset = columns.next().map(str::to_owned);
        ensure!(columns.next().is_none(), "unexpected unit file column");
    } else {
        service.load = columns.next().unwrap_or_default().into();
        service.active = columns.next().unwrap_or_default().into();
        service.sub = columns.next().unwrap_or_default().into();
    }
    Ok(service)
}

pub fn valid_unit(name: &str) -> bool {
    static UNIT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(?:[A-Za-z0-9_@:.+-]|\\x[0-9A-Fa-f]{2})+\.service$").unwrap()
    });
    name.len() <= 255 && !name.starts_with('-') && UNIT.is_match(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_preserves_installed_only_and_loaded_only_units() {
        let mut result = BTreeMap::new();
        merge_inventory(
            &mut result, "list-unit-files",
            br#"[{"unit_file":"demo.service","state":"enabled","preset":"disabled"},{"unit_file":"offline.service","state":"disabled"}]"#,
        ).unwrap();
        merge_inventory(
            &mut result, "list-units",
            br#"[{"unit":"demo.service","load":"loaded","active":"active","sub":"running","description":"Demo"},{"unit":"transient.service","load":"loaded","active":"failed","sub":"failed","description":"Transient"}]"#,
        ).unwrap();
        assert_eq!(result["demo.service"].enabled, "enabled");
        assert_eq!(result["demo.service"].active, "active");
        assert_eq!(result["offline.service"].load, "unknown");
        assert_eq!(result["transient.service"].enabled, "unknown");
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn older_systemd_columns_are_supported_and_invalid_batches_are_discarded() {
        let mut result = BTreeMap::new();
        merge_inventory(
            &mut result,
            "list-unit-files",
            b"idle.service disabled enabled",
        )
        .unwrap();
        merge_inventory(
            &mut result,
            "list-units",
            b"idle.service loaded inactive dead Idle unit",
        )
        .unwrap();
        assert_eq!(result["idle.service"].active, "inactive");
        assert_eq!(result["idle.service"].preset.as_deref(), Some("enabled"));
        assert!(
            merge_inventory(
                &mut result,
                "list-unit-files",
                b"new.service enabled\n--bad.service enabled"
            )
            .is_err()
        );
        assert!(!result.contains_key("new.service"));
    }

    #[test]
    fn systemd_detail_parser_whitelists_properties_and_rejects_malformed_data() {
        let bytes = b"Id=demo.service\nDescription=Demo\nLoadState=loaded\nActiveState=active\nSubState=running\nUnitFileState=enabled\nMainPID=123\nCanStart=yes\nCanStop=no\nCanReload=yes\nEnvironment=PRIVATE\nExecStart=PRIVATE\n";
        let details = parse_details(bytes).unwrap();
        let data = serde_json::to_string(&details).unwrap();
        assert!(data.contains("\"mainPID\":123"));
        assert!(data.contains("\"canStart\":true"));
        assert!(!data.contains("PRIVATE"));
        assert!(!data.contains("Environment"));
        assert!(!data.contains("ExecStart"));
        assert!(parse_details(b"Id=demo.service\nLoadState=not-found\n").is_err());
        assert!(parse_details(b"Id=../bad.service\nLoadState=loaded\nActiveState=active\n").is_err());
        assert!(parse_details(b"Id=demo.socket\nLoadState=loaded\nActiveState=active\n").is_err());
        assert!(parse_details(b"garbage").is_err());
    }

    #[test]
    fn generated_maestro_units_are_reserved_for_the_coordinator() {
        for unit in [
            "maestro-example-app-0123456789abcdef.service",
            "maestro-network-0123456789abcdef01234567-network.service",
            "maestro.service",
            "router-hub.service",
        ] {
            assert!(!writable_unit(unit), "{unit}");
        }
        assert!(writable_unit("maestro-custom.service"));
        assert!(writable_unit("demo.service"));
    }
}
