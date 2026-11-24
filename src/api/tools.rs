use std::{net::IpAddr, sync::Arc};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Extension, Path, Request, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header, uri::Authority},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

use crate::{
    core::{
        declarative::{CompositeProvider, MISSING_REVISION, Store},
        jobs::{Jobs, Outcome},
        monitor::{Monitor, ProcessSort},
    },
    platforms::{admin, package_updates, systemd},
    state::AppState,
};
use uuid::Uuid;

use super::sessions::{self, Sessions};

enum Tool {
    Status,
    Capabilities,
    JobsList,
    JobsGet,
    JobsCancel,
    ServicesList,
    ServicesDetails,
    ServicesLogs,
    MonitorSnapshot,
    MonitorProcesses,
    UpdatesStatus,
    PackagesInstall,
    DependenciesInstall,
    BackupInventory,
    ManagedConfigRead,
    ManagedConfigValidate,
    ManagedConfigHealth,
    ManagedConfigReadiness,
    ManagedConfigCheck,
    ManagedServices,
    ManagedConfigModel,
    ManagedConfigHistory,
    ManagedConfigHistoryRead,
    ManagedConfigPlan,
    ManagedConfigSave,
    ManagedApplyStatus,
    ManagedApplyRetry,
    ManagedConfigRevertPlan,
    ManagedConfigRevert,
    ManagedDirectoryBrowse,
    ManagedResourcePlan,
    ManagedResourceSave,
    WorkloadsGet,
    WorkloadsPlan,
    WorkloadsApply,
    WorkloadsHistory,
    WorkloadsPending,
    WorkloadsResume,
    WorkloadsRevert,
    ApplicationsList,
    ApplicationsAction,
    ApplicationsLogs,
    SharesList,
    ManagedShares,
    ManagedSharePublish,
    SharesSambaPassword,
    SharesSetup,
    SharesStatus,
}

impl Tool {
    const ALL: [Self; 48] = [
        Self::Status,
        Self::Capabilities,
        Self::JobsList,
        Self::JobsGet,
        Self::JobsCancel,
        Self::ServicesList,
        Self::ServicesDetails,
        Self::ServicesLogs,
        Self::MonitorSnapshot,
        Self::MonitorProcesses,
        Self::UpdatesStatus,
        Self::PackagesInstall,
        Self::DependenciesInstall,
        Self::BackupInventory,
        Self::ManagedConfigRead,
        Self::ManagedConfigValidate,
        Self::ManagedConfigHealth,
        Self::ManagedConfigReadiness,
        Self::ManagedConfigCheck,
        Self::ManagedServices,
        Self::ManagedConfigModel,
        Self::ManagedConfigHistory,
        Self::ManagedConfigHistoryRead,
        Self::ManagedConfigPlan,
        Self::ManagedConfigSave,
        Self::ManagedApplyStatus,
        Self::ManagedApplyRetry,
        Self::ManagedConfigRevertPlan,
        Self::ManagedConfigRevert,
        Self::ManagedDirectoryBrowse,
        Self::ManagedResourcePlan,
        Self::ManagedResourceSave,
        Self::WorkloadsGet,
        Self::WorkloadsPlan,
        Self::WorkloadsApply,
        Self::WorkloadsHistory,
        Self::WorkloadsPending,
        Self::WorkloadsResume,
        Self::WorkloadsRevert,
        Self::ApplicationsList,
        Self::ApplicationsAction,
        Self::ApplicationsLogs,
        Self::SharesList,
        Self::ManagedShares,
        Self::ManagedSharePublish,
        Self::SharesSambaPassword,
        Self::SharesSetup,
        Self::SharesStatus,
    ];

    fn metadata(&self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Status => (
                "system_status",
                "Server status",
                "Read the connected server identity and management write policy.",
            ),
            Self::Capabilities => (
                "system_capabilities",
                "Backend capabilities",
                "Read backend availability and management write policy.",
            ),
            Self::JobsList => ("jobs_list", "Recent jobs", "List recent management jobs."),
            Self::JobsGet => (
                "jobs_get",
                "Job details",
                "Read a job and its bounded output.",
            ),
            Self::JobsCancel => (
                "jobs_cancel",
                "Cancel job",
                "Request cancellation of a running job.",
            ),
            Self::ServicesList => (
                "services_list",
                "Service inventory",
                "List installed and loaded systemd units.",
            ),
            Self::ServicesDetails => (
                "services_details",
                "Service details",
                "Read selected systemd properties without environment or command lines.",
            ),
            Self::ServicesLogs => (
                "services_logs",
                "Service journal",
                "Read recent journal entries for an exact service unit.",
            ),
            Self::MonitorSnapshot => (
                "monitor_snapshot",
                "Live resources",
                "Read cached Linux resource metrics and recent history.",
            ),
            Self::MonitorProcesses => (
                "monitor_processes",
                "Process activity",
                "Read cached process activity with sorting and filtering.",
            ),
            Self::UpdatesStatus => (
                "updates_status",
                "Pending system updates",
                "Read the native package manager's cached pending updates.",
            ),
            Self::PackagesInstall => (
                "packages_install",
                "Install named native packages",
                "Install up to 32 validated repository package names via an authorized helper.",
            ),
            Self::DependenciesInstall => (
                "dependencies_install",
                "Install backend dependencies",
                "Install a fixed set of supported native packages for a selected backend.",
            ),
            Self::BackupInventory => (
                "managed_backup_inventory",
                "Review backup inputs",
                "Read bounded private source metadata and native prerequisites; not a restorable backup.",
            ),
            Self::ManagedConfigRead => (
                "managed_config_read",
                "Inspect authoritative configuration",
                "Read private raw YAML and validity without exposing secret-file contents.",
            ),
            Self::ManagedConfigValidate => (
                "managed_config_validate",
                "Validate authoritative YAML",
                "Strictly parse a candidate YAML and report errors without saving or changing the host.",
            ),
            Self::ManagedConfigHealth => (
                "managed_config_health",
                "Configuration validity",
                "Report the current authored YAML validity without native mutations.",
            ),
            Self::ManagedConfigReadiness => (
                "managed_config_readiness",
                "Configuration readiness",
                "Read cached backend prerequisites and fresh file/mount checks without inspecting running workloads or changing host state. Missing or invalid configurations remain inspectable.",
            ),
            Self::ManagedConfigCheck => (
                "managed_config_check",
                "Recheck configuration readiness",
                "Refresh native prerequisites and file/mount checks. Never installs packages or creates missing data.",
            ),
            Self::ManagedServices => (
                "managed_services",
                "Managed applications",
                "Read configuration health and selected runtime status without secret values or backend mutations.",
            ),
            Self::ManagedConfigModel => (
                "managed_config_model",
                "Typed configuration definitions",
                "Read typed private definitions for an editor; no file secret contents.",
            ),
            Self::ManagedConfigHistory => (
                "managed_config_history",
                "Private configuration history",
                "Read verified historical revision identifiers.",
            ),
            Self::ManagedConfigHistoryRead => (
                "managed_config_history_read",
                "Read a historical revision",
                "Inspect exact historical YAML bytes, including invalid recovery revisions.",
            ),
            Self::ManagedConfigPlan => (
                "managed_config_plan",
                "Preview configuration changes",
                "Validate and preview configuration changes without publication.",
            ),
            Self::ManagedConfigSave => (
                "managed_config_save",
                "Save and apply configuration",
                "Persist authoritative configuration and apply affected definitions as a tracked job.",
            ),
            Self::ManagedApplyStatus => (
                "managed_apply_status",
                "Inspect apply progress",
                "Read durable per-resource application status without retrying or starting resources.",
            ),
            Self::ManagedApplyRetry => (
                "managed_apply_retry",
                "Retry configuration apply",
                "Inspect incomplete application and retry safe publication as a job. Interrupted removals require explicit inspection.",
            ),
            Self::ManagedConfigRevertPlan => (
                "managed_config_revert_plan",
                "Review configuration revert",
                "Validate a historical definition and preview affected resources. Reverting configuration does not restore data or reverse client effects.",
            ),
            Self::ManagedConfigRevert => (
                "managed_config_revert",
                "Revert configuration",
                "Restore a verified historical definition and reconcile owned resources while preserving workload data.",
            ),
            Self::ManagedDirectoryBrowse => (
                "managed_directory_browse",
                "Browse directories",
                "List accessible directory paths without showing file contents.",
            ),
            Self::ManagedResourcePlan => (
                "managed_resource_plan",
                "Preview typed resource edit",
                "Strictly review a typed definition edit through the shared authoritative YAML plan.",
            ),
            Self::ManagedResourceSave => (
                "managed_resource_save",
                "Apply reviewed typed resource edit",
                "Recompute and apply a revision-checked reviewed edit through the shared journal.",
            ),
            Self::WorkloadsGet => (
                "workloads_get",
                "Workloads configuration",
                "Read the current declarative workloads YAML and revision.",
            ),
            Self::WorkloadsPlan => (
                "workloads_plan",
                "Plan workloads changes",
                "Review proposed workload YAML changes and generate an execution plan with fingerprint.",
            ),
            Self::WorkloadsApply => (
                "workloads_apply",
                "Apply workloads changes",
                "Apply reviewed workload changes as a tracked management job.",
            ),
            Self::WorkloadsHistory => (
                "workloads_history",
                "Workloads revision history",
                "List historical declarative workload revisions.",
            ),
            Self::WorkloadsPending => (
                "workloads_pending",
                "Pending workload apply",
                "Read the pending recovery journal.",
            ),
            Self::WorkloadsResume => (
                "workloads_resume",
                "Resume workload apply",
                "Resume the reviewed pending apply after verifying native state.",
            ),
            Self::WorkloadsRevert => (
                "workloads_revert",
                "Revert workloads revision",
                "Plan or apply a revert to a historical workload revision.",
            ),
            Self::ApplicationsList => (
                "applications_list",
                "Applications inventory",
                "List configured applications, their runtime engine/backend and state.",
            ),
            Self::ApplicationsAction => (
                "applications_action",
                "Application lifecycle action",
                "Perform a start, stop, or restart action on an application.",
            ),
            Self::ApplicationsLogs => (
                "applications_logs",
                "Application logs",
                "Read recent logs for an application.",
            ),
            Self::SharesList => (
                "shares_list",
                "Network shares inventory",
                "Read native Samba and NFS share metadata, connection and export state.",
            ),
            Self::ManagedShares => (
                "managed_shares",
                "Configured file shares",
                "Read authoritative share definitions without changing native daemons.",
            ),
            Self::ManagedSharePublish => (
                "managed_share_publish",
                "Publish or unpublish share",
                "Persist the share's publication toggle and apply the authoritative protocol set. Does not change shared data, accounts or host daemon enablement.",
            ),
            Self::SharesSambaPassword => (
                "shares_samba_password",
                "Set Samba password",
                "Set the trusted workload account's Samba password through the restricted optional helper.",
            ),
            Self::SharesSetup => (
                "shares_setup",
                "Set up share server",
                "Install native Samba/NFS server packages, prepare managed configuration and enable/start its system service.",
            ),
            Self::SharesStatus => (
                "shares_status",
                "Shares service status",
                "Inspect the status and active exports of Samba and NFS.",
            ),
        }
    }
}

pub(super) fn router(state: AppState) -> anyhow::Result<Router<AppState>> {
    let sessions = Sessions::new(state.config.clone())?;
    let store = crate::core::linux_config::store(&state.config)?;
    let provider = Arc::new(CompositeProvider::new(&state.config, state.runner.clone()));
    Ok(Router::new()
        .route("/status", get(status))
        .route("/tools", get(discover))
        .route("/tools/{name}", post(invoke))
        .route("/mcp", post(super::mcp::handle_mcp))
        .route("/session", axum::routing::delete(sessions::logout))
        .route_layer(middleware::from_fn_with_state(
            sessions.clone(),
            sessions::authorize,
        ))
        .route("/session", post(sessions::login))
        .route("/auth/mode", get(sessions::mode))
        .route("/auth/setup", post(sessions::setup))
        .layer(Extension(sessions))
        .layer(middleware::from_fn_with_state(
            state.config.clone(),
            require_origin,
        ))
        .layer(Extension(Monitor::new(state.config.clone())))
        .layer(Extension(Jobs::new(state.config.paths.data_dir.clone())))
        .layer(Extension(store))
        .layer(Extension(provider))
        .layer(Extension(Utc::now()))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(middleware::map_response(|mut response: Response| async {
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        })))
}

async fn require_origin(
    axum::extract::State(config): axum::extract::State<std::sync::Arc<crate::config::AppConfig>>,
    request: Request,
    next: Next,
) -> Response {
    if valid_origin(&request, &config.server.allowed_origins) {
        return next.run(request).await;
    }
    error(
        StatusCode::FORBIDDEN,
        "forbidden_origin",
        "Request origin or host is not allowed.",
    )
}

fn valid_origin(request: &Request, allowed_origins: &[String]) -> bool {
    let headers = request.headers();
    let mut hosts = headers.get_all(header::HOST).iter();
    let authority = match hosts.next() {
        Some(host) => host
            .to_str()
            .ok()
            .and_then(|host| host.parse::<Authority>().ok()),
        None => request.uri().authority().cloned(),
    };
    let Some(authority) = authority.filter(|host| !host.as_str().contains('@')) else {
        return false;
    };
    if hosts.next().is_some()
        || request
            .uri()
            .authority()
            .is_some_and(|host| !host.as_str().eq_ignore_ascii_case(authority.as_str()))
    {
        return false;
    }
    let mut origins = headers.get_all(header::ORIGIN).iter();
    let origin = origins.next();
    if origins.next().is_some() {
        return false;
    }
    let host = authority.host().trim_matches(['[', ']']);
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if loopback && origin.is_none_or(|origin| origin == format!("http://{authority}").as_str()) {
        return true;
    }
    allowed_origins.iter().any(|allowed| {
        let Some((scheme, host)) = allowed.split_once("://") else {
            return false;
        };
        matches!(scheme, "http" | "https")
            && host.eq_ignore_ascii_case(authority.as_str())
            && origin.is_none_or(|origin| origin == allowed.as_str())
    })
}

fn modules(state: &AppState) -> Value {
    json!([
        {"id": "system", "title": "Server", "available": true},
        {"id": "systemd", "title": "systemd services", "available": systemd::available(state)},
        {"id": "monitor", "title": "Resource monitoring", "available": !state.config.test_mode},
        {"id": "updates", "title": "System package updates", "available": package_updates::available(state)},
        {"id": "workloads", "title": "Applications", "available": true, "writable": writes_allowed(state)},
        {"id": "shares", "title": "File sharing", "available": true}
    ])
}

fn writes_allowed(state: &AppState) -> bool {
    state.config.test_mode
        || (state.config.system.allow_workload_writes
            && matches!(state.config.platform, crate::config::Platform::Linux))
}

pub fn discover_tools(_state: &AppState) -> Vec<Value> {
    let mut tools: Vec<_> = Tool::ALL
        .iter()
        .map(|tool| {
            let (name, title, description) = tool.metadata();
            let module = match tool {
                Tool::ServicesList | Tool::ServicesDetails | Tool::ServicesLogs => "systemd",
                Tool::MonitorSnapshot | Tool::MonitorProcesses => "monitor",
                Tool::UpdatesStatus | Tool::PackagesInstall | Tool::DependenciesInstall => "updates",
                Tool::BackupInventory
                | Tool::ManagedConfigRead
                | Tool::ManagedConfigValidate
                | Tool::ManagedConfigHealth
                | Tool::ManagedConfigModel
                | Tool::ManagedConfigHistory
                | Tool::ManagedConfigHistoryRead
                | Tool::ManagedConfigPlan
                | Tool::ManagedConfigSave
                | Tool::ManagedApplyStatus
                | Tool::ManagedApplyRetry
                | Tool::ManagedConfigRevertPlan
                | Tool::ManagedConfigRevert
                | Tool::ManagedResourcePlan
                | Tool::ManagedResourceSave
                | Tool::WorkloadsGet
                | Tool::WorkloadsPlan
                | Tool::WorkloadsApply
                | Tool::WorkloadsHistory
                | Tool::WorkloadsPending
                | Tool::WorkloadsResume
                | Tool::WorkloadsRevert
                | Tool::ApplicationsList
                | Tool::ApplicationsAction
                | Tool::ApplicationsLogs => "workloads",
                Tool::ManagedConfigReadiness | Tool::ManagedConfigCheck | Tool::ManagedServices => "managed_status",
                Tool::SharesList | Tool::ManagedShares | Tool::SharesSambaPassword
                | Tool::SharesSetup | Tool::ManagedSharePublish | Tool::SharesStatus | Tool::ManagedDirectoryBrowse => "shares",
                _ => "system",
            };
            let read_only = !matches!(
                tool,
                Tool::JobsCancel
                    | Tool::WorkloadsApply
                    | Tool::WorkloadsResume
                    | Tool::WorkloadsRevert
                    | Tool::ApplicationsAction
                    | Tool::PackagesInstall
                    | Tool::DependenciesInstall
                    | Tool::ManagedConfigSave
                    | Tool::ManagedApplyRetry
                    | Tool::ManagedConfigRevert
                    | Tool::ManagedResourceSave
                    | Tool::SharesSambaPassword
                    | Tool::SharesSetup
                    | Tool::ManagedSharePublish
                    | Tool::BackupInventory
            );
            let input_schema = match tool {
                Tool::SharesList => json!({"additionalProperties":false,"properties":{"protocol":{"type":"string","description":"Optional protocol filter","enum":["samba","nfs"]}},"required":[],"type":"object"}),
                Tool::ManagedConfigReadiness | Tool::ManagedConfigCheck | Tool::ManagedServices => json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
                Tool::ManagedApplyStatus => json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
                Tool::ManagedApplyRetry => json!({"type":"object","properties":{
                    "confirm":{"type":"boolean","description":"Explicit acknowledgement of this destructive operation. Must be true."}
                },"required":["confirm"],"additionalProperties":false}),
                Tool::ManagedConfigRevertPlan | Tool::ManagedConfigRevert => {
                    let save = matches!(tool, Tool::ManagedConfigRevert);
                    let mut properties = json!({
                        "target":{"type":"string","description":"History revision to inspect or restore","minLength":1,"maxLength":64},
                        "revision":{"type":"string","description":"Current configuration revision","minLength":1,"maxLength":64}
                    });
                    let required = if save {
                        properties["confirm"] = json!({"type":"boolean","description":"Explicit acknowledgement of this destructive operation. Must be true."});
                        properties["mode"] = json!({"type":"string","description":"Apply now or publish for next start","enum":["now","next_start"]});
                        vec!["confirm","mode","revision","target"]
                    } else { vec!["revision","target"] };
                    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
                },
                Tool::ManagedConfigPlan | Tool::ManagedConfigSave => {
                    let save = matches!(tool, Tool::ManagedConfigSave);
                    let mut properties = json!({
                        "content": {"type":"string","description":"Workload YAML","minLength":1,"maxLength":1048576},
                        "revision": {"type":"string","description":"Current configuration revision, or missing","minLength":1,"maxLength":64}
                    });
                    let required = if save {
                        properties["confirm"] = json!({"type":"boolean","description":"Explicit acknowledgement of this destructive operation. Must be true."});
                        properties["mode"] = json!({"type":"string","description":"Apply now or publish changes for next start","enum":["now","next_start"]});
                        vec!["confirm","content","mode","revision"]
                    } else { vec!["content","revision"] };
                    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
                },
                Tool::ManagedDirectoryBrowse => json!({
                    "type": "object",
                    "properties": {"path": {"type": "string", "description": "Directory path to inspect", "maxLength": 4096}},
                    "required": [],
                    "additionalProperties": false
                }),
                Tool::JobsGet | Tool::JobsCancel => json!({
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "format": "uuid"}
                    },
                    "required": ["id"],
                    "additionalProperties": false
                }),
                Tool::ManagedResourcePlan | Tool::ManagedResourceSave => {
                    let save = matches!(tool, Tool::ManagedResourceSave);
                    let mut properties = json!({
                        "kind": {"type":"string","enum":["service","storage","share","binding","secret"]},
                        "id": {"type":"string","maxLength":63},
                        "operation": {"type":"string","enum":["save","remove"]},
                        "definition": {"type":"string","maxLength":262144},
                        "revision": {"type":"string","minLength":7,"maxLength":64}
                    });
                    let mut required = vec!["kind","id","operation","revision"];
                    if save {
                        properties["mode"] = json!({"type":"string","enum":["now","next_start"]});
                        properties["fingerprint"] = json!({"type":"string","minLength":64,"maxLength":64});
                        properties["confirm"] = json!({"const":true});
                        required.extend(["mode","fingerprint","confirm"]);
                    }
                    json!({
                        "type":"object",
                        "properties":properties,
                        "required":required,
                        "additionalProperties":false
                    })
                },
                Tool::ManagedSharePublish => json!({
                    "type":"object","properties":{
                        "share":{"type":"string","minLength":1,"maxLength":63,"description":"Share ID"},
                        "published":{"type":"boolean"},
                        "confirm":{"type":"boolean","description":"Explicit acknowledgement of this destructive operation. Must be true."}
                    },"required":["share","published","confirm"],"additionalProperties":false
                }),
                Tool::SharesSetup => json!({
                    "type":"object","properties":{
                        "protocol":{"type":"string","enum":["samba","nfs"],"description":"Server protocol"},
                        "confirm":{"type":"boolean","description":"Explicit acknowledgement of this destructive operation. Must be true."}
                    },"required":["protocol","confirm"],"additionalProperties":false
                }),
                Tool::SharesSambaPassword => json!({
                    "type": "object",
                    "properties": {
                        "password": {"type":"string", "minLength":1, "maxLength":256},
                        "confirm": {"const":true}
                    },
                    "required": ["password","confirm"],
                    "additionalProperties":false
                }),
                Tool::ManagedConfigValidate => json!({
                    "type": "object",
                    "properties": {
                        "content": {"type": "string", "maxLength": 1048576}
                    },
                    "required": ["content"],
                    "additionalProperties": false
                }),
                Tool::ManagedConfigHistory => json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
                Tool::ManagedConfigHistoryRead => json!({
                    "type": "object",
                    "properties": {
                        "target": {"type": "string", "description":"History revision to inspect or restore", "minLength": 1, "maxLength": 64}
                    },
                    "required": ["target"],
                    "additionalProperties": false
                }),
                Tool::PackagesInstall => json!({
                    "type": "object",
                    "properties": {
                        "packages": {"type": "string", "minLength": 1, "maxLength": 4096},
                        "confirm": {"const": true}
                    },
                    "required": ["packages", "confirm"],
                    "additionalProperties": false
                }),
                Tool::DependenciesInstall => json!({
                    "type": "object",
                    "properties": {
                        "target": {"type": "string", "enum": ["podman", "docker", "samba", "nfs", "virtualization", "incus"]},
                        "confirm": {"const": true}
                    },
                    "required": ["target", "confirm"],
                    "additionalProperties": false
                }),
                Tool::ServicesDetails => json!({
                    "type": "object",
                    "properties": {
                        "scope": {"type": "string", "enum": ["system", "user"]},
                        "unit": {"type": "string", "maxLength": 255}
                    },
                    "required": ["unit"],
                    "additionalProperties": false
                }),
                Tool::ServicesLogs => json!({
                    "type": "object",
                    "properties": {
                        "scope": {"type": "string", "enum": ["system", "user"]},
                        "unit": {"type": "string", "maxLength": 255},
                        "tail": {"type": "integer", "minimum": 1, "maximum": 2000}
                    },
                    "required": ["unit"],
                    "additionalProperties": false
                }),
                Tool::MonitorProcesses => json!({
                    "type": "object",
                    "properties": {
                        "sort": {"type": "string", "enum": ["cpu", "memory", "pid", "name"]},
                        "filter": {"type": "string", "maxLength": 128},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 500}
                    },
                    "additionalProperties": false
                }),
                Tool::ServicesList => json!({
                    "type": "object",
                    "properties": {
                        "scope": {"type": "string", "enum": ["system", "user"]}
                    },
                    "additionalProperties": false
                }),
                Tool::WorkloadsPlan => json!({
                    "type": "object",
                    "properties": {
                        "candidate": {"type": "string"},
                        "expected": {"type": "string"}
                    },
                    "required": ["candidate"],
                    "additionalProperties": false
                }),
                Tool::WorkloadsApply => json!({
                    "type": "object",
                    "properties": {
                        "candidate": {"type": "string"},
                        "expected": {"type": "string"},
                        "fingerprint": {"type": "string"}
                    },
                    "required": ["candidate", "fingerprint"],
                    "additionalProperties": false
                }),
                Tool::WorkloadsResume => json!({
                    "type": "object",
                    "properties": {"fingerprint": {"type": "string"}},
                    "required": ["fingerprint"],
                    "additionalProperties": false
                }),
                Tool::WorkloadsRevert => json!({
                    "type": "object",
                    "properties": {
                        "revision": {"type": "string"},
                        "expected": {"type": "string"},
                        "apply": {"type": "boolean"},
                        "fingerprint": {"type": "string"}
                    },
                    "required": ["revision"],
                    "additionalProperties": false
                }),
                Tool::ApplicationsAction => json!({
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "action": {"type": "string", "enum": ["start", "stop", "restart"]}
                    },
                    "required": ["id", "action"],
                    "additionalProperties": false
                }),
                Tool::ApplicationsLogs => json!({
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "tail": {"type": "integer", "minimum": 1, "maximum": 2000}
                    },
                    "required": ["id"],
                    "additionalProperties": false
                }),
                _ => json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            };
            json!({
                "name": name,
                "module": module,
                "title": title,
                "description": description,
                "readOnly": read_only,
                "destructive": matches!(
                    tool,
                    Tool::WorkloadsApply | Tool::WorkloadsResume | Tool::WorkloadsRevert
                        | Tool::ApplicationsAction | Tool::PackagesInstall | Tool::DependenciesInstall
                        | Tool::ManagedResourceSave | Tool::ManagedConfigSave | Tool::ManagedConfigRevert | Tool::ManagedApplyRetry | Tool::SharesSambaPassword | Tool::SharesSetup | Tool::ManagedSharePublish
                ),
                "inputSchema": input_schema
            })
        })
        .collect();

    tools.extend([
        json!({"name":"managed_storage_create","module":"managed_storage","title":"Provision new storage","description":"Create a reviewed empty directory or new sparse raw/qcow2 disk as a job. Supports cloning a declared SHA-256-pinned source image. Never overwrites data, restores backups, creates parents, changes ownership or starts workloads.","readOnly":false,"destructive":true,"inputSchema":{"additionalProperties":false,"properties":{"confirm":{"type":"boolean","description":"Explicit acknowledgement of this destructive operation. Must be true."},"purpose":{"type":"string","description":"Explicitly authorize new data, never replacement of missing restored data","enum":["new_data"]},"reviewToken":{"type":"string","description":"Token from the reviewed provisioning plan","minLength":1,"maxLength":64},"revision":{"type":"string","description":"Current configuration revision","minLength":1,"maxLength":64},"sizeGiB":{"type":"integer","description":"New disk capacity in GiB; omit or use zero for a directory","minimum":0,"maximum":4096},"storage":{"type":"string","description":"Existing storage reference ID","minLength":1,"maxLength":63}},"required":["confirm","purpose","reviewToken","revision","storage"],"type":"object"}}),
        json!({"name":"managed_storage_plan","module":"managed_storage","title":"Review new storage","description":"Inspect an absent owned path and stopped consumers without creating data. Existing parents must be owned real directories. Private paths require management writes.","readOnly":true,"destructive":false,"inputSchema":{"additionalProperties":false,"properties":{"revision":{"type":"string","description":"Current configuration revision","minLength":1,"maxLength":64},"sizeGiB":{"type":"integer","description":"New disk capacity in GiB; omit or use zero for a directory","minimum":0,"maximum":4096},"storage":{"type":"string","description":"Existing storage reference ID","minLength":1,"maxLength":63}},"required":["revision","storage"],"type":"object"}}),
    ]);

    // Keep the reference's individual lifecycle contracts on the existing provider path.
    tools.extend([
        json!({"name":"managed_service_disable","module":"managed_lifecycle","title":"Managed service disable","description":"Change the authoritative Quadlet, Compose, Libvirt or Incus boot policy as a tracked next-start job without starting, stopping or restarting workloads. Other native providers remain unavailable.","readOnly":false,"destructive":true,"inputSchema":{"additionalProperties":false,"properties":{"confirm":{"type":"boolean","description":"Explicit acknowledgement of this destructive operation. Must be true."},"service":{"type":"string","description":"Managed service ID","minLength":1,"maxLength":63}},"required":["confirm","service"],"type":"object"}}),
        json!({"name":"managed_service_enable","module":"managed_lifecycle","title":"Managed service enable","description":"Change the authoritative Quadlet, Compose, Libvirt or Incus boot policy as a tracked next-start job without starting, stopping or restarting workloads. Other native providers remain unavailable.","readOnly":false,"destructive":true,"inputSchema":{"additionalProperties":false,"properties":{"confirm":{"type":"boolean","description":"Explicit acknowledgement of this destructive operation. Must be true."},"service":{"type":"string","description":"Managed service ID","minLength":1,"maxLength":63}},"required":["confirm","service"],"type":"object"}}),
        json!({"name":"managed_service_logs","module":"managed_logs","title":"Read managed application logs","description":"Explicitly read bounded recent logs for one configured managed application. Runtime logs may contain private application data. This never starts a workload or reads another application.","readOnly":true,"destructive":false,"inputSchema":{"additionalProperties":false,"properties":{"service":{"type":"string","description":"Managed application ID","minLength":1,"maxLength":63},"tail":{"type":"integer","description":"Most recent entries per native log reader; default 200","minimum":1,"maximum":2000}},"required":["service"],"type":"object"}}),
        json!({"name":"managed_service_restart","module":"managed_lifecycle","title":"Managed service restart","description":"Manage the authoritative service as a tracked job. Start/stop/restart affect execution; enable/disable affect boot policy only.","readOnly":false,"destructive":true,"inputSchema":{"additionalProperties":false,"properties":{"confirm":{"type":"boolean","description":"Explicit acknowledgement of this destructive operation. Must be true."},"service":{"type":"string","description":"Managed service ID","minLength":1,"maxLength":63}},"required":["confirm","service"],"type":"object"}}),
        json!({"name":"managed_service_start","module":"managed_lifecycle","title":"Managed service start","description":"Manage the authoritative service as a tracked job. Start/stop/restart affect execution; enable/disable affect boot policy only.","readOnly":false,"destructive":false,"inputSchema":{"additionalProperties":false,"properties":{"service":{"type":"string","description":"Managed service ID","minLength":1,"maxLength":63}},"required":["service"],"type":"object"}}),
        json!({"name":"managed_service_stop","module":"managed_lifecycle","title":"Managed service stop","description":"Manage the authoritative service as a tracked job. Start/stop/restart affect execution; enable/disable affect boot policy only.","readOnly":false,"destructive":true,"inputSchema":{"additionalProperties":false,"properties":{"confirm":{"type":"boolean","description":"Explicit acknowledgement of this destructive operation. Must be true."},"service":{"type":"string","description":"Managed service ID","minLength":1,"maxLength":63}},"required":["confirm","service"],"type":"object"}}),
    ]);
    tools.extend(systemd::ACTIONS.map(|action| {
        let destructive = !matches!(action, "start" | "enable");
        json!({
            "name": format!("services_{action}"),
            "module": "systemd",
            "title": format!("Service {action}"),
            "description": "Run an opted-in systemd action as a tracked job.",
            "readOnly": false,
            "destructive": destructive,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scope": {"type": "string", "enum": ["system", "user"]},
                    "unit": {"type": "string", "maxLength": 255},
                    "confirm": {"const": true}
                },
                "required": if destructive { vec!["unit", "confirm"] } else { vec!["unit"] },
                "additionalProperties": false
            }
        })
    }));
    tools.extend(["updates_refresh", "updates_install"].map(|name| {
        let install = name == "updates_install";
        json!({
            "name": name, "module": "updates",
            "title": if install { "Install package updates" } else { "Refresh package metadata" },
            "description": "Run the native package manager as a tracked job.",
            "readOnly": false, "destructive": install,
            "inputSchema": {
                "type": "object",
                "properties": if install { json!({"confirm": {"const": true}}) } else { json!({}) },
                "required": if install { vec!["confirm"] } else { vec![] },
                "additionalProperties": false
            }
        })
    }));
    tools
}

async fn permissions(state: &AppState) -> Value {
    let helper = admin::probe().await;
    let system_ready = systemd::available(state)
        && state.config.platform == crate::config::Platform::Linux
        && state
            .config
            .services
            .systemd_system_write_units
            .iter()
            .any(|unit| {
                systemd::permits(state, systemd::Scope::System, unit) && helper.permits_unit(unit)
            });
    let package_policy = state.config.system.allow_package_writes;
    let samba_password_policy = state.config.system.allow_share_writes;
    let samba_password_ready = samba_password_policy && helper.permits_samba_password();
    let share_ready = samba_password_policy && helper.permits_shares();
    let package_ready =
        package_policy && package_updates::available(state) && helper.permits_packages();
    let package_reason = if !package_policy {
        "Package writes are disabled in Maestro configuration."
    } else if !package_updates::available(state) {
        "No supported native package manager is available."
    } else if !helper.permits_packages() {
        "An authorized helper with root-owned package_writes policy is required."
    } else {
        "Package updates and installation are authorized."
    };
    json!({
        "helper": helper,
        "systemServices": {
            "status": if system_ready { "ready" } else { "unavailable" },
            "reason": if system_ready {
                "Configured system units are authorized through the helper."
            } else {
                "Protected system-unit actions require configured units and matching root-owned helper policy."
            }
        },
        "packageWrites": {
            "status": if package_ready { "ready" } else { "unavailable" },
            "code": if package_ready { "ready" } else if !package_policy { "writes_disabled" } else { "helper_unavailable" },
            "reason": package_reason
        },
        "shareWrites": {
            "status": if share_ready { "ready" } else { "unavailable" },
            "code": if share_ready { "ready" } else if !samba_password_policy { "writes_disabled" } else { "helper_unavailable" },
            "reason": if share_ready { "Protected share operations are authorized; native readiness is checked per protocol." } else { "Share publication requires enabled configuration and root-owned helper grants." }
        },
        "sambaPassword": {
            "status": if samba_password_ready { "ready" } else { "unavailable" },
            "code": if samba_password_ready { "ready" } else if !samba_password_policy { "writes_disabled" } else { "helper_unavailable" },
            "reason": if !samba_password_policy {
                "Samba password writes are disabled by Maestro policy."
            } else if !helper.permits_samba_password() {
                "An authorized helper with explicit root-owned samba_password policy is required."
            } else {
                "Samba password update for the trusted workload account is authorized."
            }
        }
    })
}

async fn discover(State(state): State<AppState>) -> Json<Value> {
    Json(
        json!({ "tools": discover_tools(&state), "modules": modules(&state), "permissions": permissions(&state).await }),
    )
}

pub struct ToolContext<'a> {
    pub state: &'a AppState,
    pub started_at: DateTime<Utc>,
    pub jobs: &'a Jobs,
    pub monitor: &'a Monitor,
    pub store: &'a Store,
    pub provider: &'a Arc<CompositeProvider>,
}

#[allow(clippy::too_many_arguments)]
async fn invoke(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Extension(started_at): Extension<DateTime<Utc>>,
    Extension(jobs): Extension<Jobs>,
    Extension(monitor): Extension<Monitor>,
    Extension(store): Extension<Store>,
    Extension(provider): Extension<Arc<CompositeProvider>>,
    arguments: Result<Json<Map<String, Value>>, JsonRejection>,
) -> Response {
    let arguments = match arguments {
        Ok(Json(arguments)) => arguments,
        Err(JsonRejection::MissingJsonContentType(_)) => {
            return error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
                "Content-Type must be application/json.",
            );
        }
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_arguments",
                "Expected a JSON object.",
            );
        }
    };
    let ctx = ToolContext {
        state: &state,
        started_at,
        jobs: &jobs,
        monitor: &monitor,
        store: &store,
        provider: &provider,
    };
    let (status, val) = execute_tool(&ctx, &name, arguments).await;
    (status, Json(val)).into_response()
}

pub async fn execute_tool(
    ctx: &ToolContext<'_>,
    name: &str,
    mut arguments: Map<String, Value>,
) -> (StatusCode, Value) {
    if name.starts_with("services_") && !arguments.contains_key("scope") {
        if let Some(authority) = &ctx.state.config.linux_authority {
            arguments.insert("scope".into(), json!(authority.settings.systemd_scope));
        }
    }
    let state = ctx.state;
    let started_at = ctx.started_at;
    let jobs = ctx.jobs;
    let monitor = ctx.monitor;
    let store = ctx.store;
    let provider = ctx.provider;
    if matches!(
        name,
        "managed_config_save"
            | "managed_apply_retry"
            | "managed_config_revert"
            | "managed_resource_save"
            | "workloads_apply"
            | "workloads_resume"
            | "workloads_revert"
            | "applications_action"
            | "managed_service_start"
            | "managed_service_stop"
            | "managed_service_restart"
            | "managed_storage_create"
            | "managed_service_enable"
            | "managed_service_disable"
    ) {
        if let Err(error) = crate::core::linux_config::mutation_ready(&state.config, store) {
            return (
                StatusCode::CONFLICT,
                json_err("restart_required", &error.to_string()),
            );
        }
    }

    if matches!(name, "updates_refresh" | "updates_install") {
        let install = name == "updates_install";
        if (install
            && (arguments.len() != 1 || arguments.get("confirm") != Some(&Value::Bool(true))))
            || (!install && !arguments.is_empty())
        {
            return (
                StatusCode::BAD_REQUEST,
                json_err(
                    "invalid_arguments",
                    "Installation requires confirm=true. Refresh accepts no arguments.",
                ),
            );
        }
        if !package_updates::permits(state, install).await {
            let policy_disabled = !state.config.system.allow_package_writes;
            return (
                if policy_disabled {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                json_err(
                    if policy_disabled {
                        "writes_disabled"
                    } else {
                        "helper_unavailable"
                    },
                    if policy_disabled {
                        "Package writes are disabled by policy."
                    } else {
                        "Protected package operations require an installed, authorized and compatible root-owned helper and supported native manager."
                    },
                ),
            );
        }
        let tool = if install {
            "updates_install"
        } else {
            "updates_refresh"
        };
        let state_clone = state.clone();
        return start_job_value(jobs, tool, move |running| async move {
            match package_updates::run_job(&state_clone, install, &running).await {
                Ok(result) => running.finish(Outcome::Succeeded(result)),
                Err(error) => running.finish(Outcome::Failed(error.to_string())),
            }
        })
        .await;
    }

    if matches!(name, "packages_install" | "dependencies_install") {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct PackageArgs {
            packages: String,
            confirm: bool,
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct DependencyArgs {
            target: String,
            confirm: bool,
        }
        let (named, argument) = if name == "packages_install" {
            let Ok(input) = serde_json::from_value::<PackageArgs>(Value::Object(arguments)) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "invalid_arguments",
                        "Provide package names and confirm=true.",
                    ),
                );
            };
            if !input.confirm
                || crate::platforms::admin_packages::package_names(&input.packages).is_err()
            {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "invalid_arguments",
                        "Provide 1–32 unique bare repository package names and confirm=true.",
                    ),
                );
            }
            (true, input.packages)
        } else {
            let Ok(input) = serde_json::from_value::<DependencyArgs>(Value::Object(arguments))
            else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "invalid_arguments",
                        "Provide a supported dependency target and confirm=true.",
                    ),
                );
            };
            if !input.confirm
                || !matches!(
                    input.target.as_str(),
                    "podman" | "docker" | "samba" | "nfs" | "virtualization" | "incus"
                )
            {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "invalid_arguments",
                        "Dependency target and confirmation are invalid.",
                    ),
                );
            }
            (false, input.target)
        };
        if !package_updates::permits(state, true).await {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                json_err(
                    "helper_unavailable",
                    "Named and dependency package installs require enabled package-write policy and an authorized privileged helper.",
                ),
            );
        }
        let state_clone = state.clone();
        if named {
            return start_job_value(jobs, "packages_install", move |running| async move {
                match package_updates::run_named_job(&state_clone, &argument, &running).await {
                    Ok(result) => running.finish(Outcome::Succeeded(result)),
                    Err(error) => running.finish(Outcome::Failed(error.to_string())),
                }
            })
            .await;
        }
        return start_job_value(jobs, "dependencies_install", move |running| async move {
            match package_updates::run_dependency_job(&state_clone, &argument, &running).await {
                Ok(result) => running.finish(Outcome::Succeeded(result)),
                Err(error) => running.finish(Outcome::Failed(error.to_string())),
            }
        })
        .await;
    }

    if let Some(action) = name.strip_prefix("services_").and_then(|action| {
        systemd::ACTIONS
            .into_iter()
            .find(|candidate| *candidate == action)
    }) {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ActionArgs {
            #[serde(default)]
            scope: systemd::Scope,
            unit: String,
            confirm: Option<bool>,
        }
        let Ok(input) = serde_json::from_value::<ActionArgs>(Value::Object(arguments)) else {
            return (
                StatusCode::BAD_REQUEST,
                json_err("invalid_arguments", "Expected a scope and service unit."),
            );
        };
        if !systemd::valid_unit(&input.unit) {
            return (
                StatusCode::BAD_REQUEST,
                json_err("invalid_arguments", "Expected an exact .service unit."),
            );
        }
        if !matches!(action, "start" | "enable") && input.confirm != Some(true) {
            return (
                StatusCode::BAD_REQUEST,
                json_err(
                    "confirmation_required",
                    "This destructive service action requires confirm=true.",
                ),
            );
        }
        if !systemd::permits(state, input.scope, &input.unit) {
            return (
                StatusCode::FORBIDDEN,
                json_err(
                    "writes_disabled",
                    "This service and scope are outside the write policy.",
                ),
            );
        }
        if matches!(input.scope, systemd::Scope::System) {
            let helper = admin::probe().await;
            if !helper.permits_unit(&input.unit) {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json_err(
                        "helper_unavailable",
                        if helper.status == "ready" {
                            "The selected service is not authorized by the root-owned helper policy."
                        } else {
                            &helper.reason
                        },
                    ),
                );
            }
        }

        let tool = match action {
            "start" => "services_start",
            "stop" => "services_stop",
            "restart" => "services_restart",
            "reload" => "services_reload",
            "enable" => "services_enable",
            "disable" => "services_disable",
            _ => unreachable!(),
        };
        let state_clone = state.clone();
        return start_job_value(jobs, tool, move |running| async move {
            let scope = input.scope;
            let unit = input.unit;
            let outcome = match systemd::act(&state_clone, scope, &unit, action).await {
                Ok(output) => {
                    running.append_output(&output);
                    Outcome::Succeeded(json!({"scope": scope, "unit": unit, "action": action}))
                }
                Err(error) => Outcome::Failed(error.to_string()),
            };
            running.finish(outcome);
        })
        .await;
    }

    if matches!(
        name,
        "system_status"
            | "system_capabilities"
            | "jobs_list"
            | "monitor_snapshot"
            | "updates_status"
            | "workloads_get"
            | "managed_backup_inventory"
            | "managed_config_read"
            | "managed_config_health"
            | "managed_config_readiness"
            | "managed_config_check"
            | "managed_services"
            | "managed_config_model"
            | "managed_config_history"
            | "workloads_history"
            | "applications_list"
            | "managed_shares"
            | "shares_status"
    ) && !arguments.is_empty()
    {
        return (
            StatusCode::BAD_REQUEST,
            json_err("invalid_arguments", "This tool accepts no arguments."),
        );
    }

    match name {
        "managed_storage_plan" | "managed_storage_create" => {
            if !writes_allowed(state) {
                return (StatusCode::FORBIDDEN, json_err("writes_disabled",
                    "Private storage provisioning requires management writes."));
            }
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase", deny_unknown_fields)]
            struct Input {
                storage: String,
                revision: String,
                #[serde(default, rename = "sizeGiB")]
                size_gib: u64,
                review_token: Option<String>,
                purpose: Option<String>,
                confirm: Option<bool>,
            }
            let create = name == "managed_storage_create";
            let keys_valid = if create {
                arguments.keys().all(|key| matches!(key.as_str(),
                    "storage" | "revision" | "sizeGiB" | "reviewToken" | "purpose" | "confirm"))
            } else {
                arguments.keys().all(|key| matches!(key.as_str(), "storage" | "revision" | "sizeGiB"))
            };
            let input = serde_json::from_value::<Input>(Value::Object(arguments));
            let Ok(input) = input else {
                return (StatusCode::BAD_REQUEST, json_err("invalid_arguments", "Invalid provisioning arguments."));
            };
            if !keys_valid || !crate::core::declarative::id_valid(&input.storage)
                || input.revision.len() != 64 || input.size_gib > 4096
                || (create && (input.confirm != Some(true) || input.purpose.as_deref() != Some("new_data")
                    || input.review_token.as_ref().is_none_or(|token| token.len() != 64))) {
                return (StatusCode::BAD_REQUEST, json_err("invalid_arguments", "Provide storage, revision, and an explicit reviewed new-data authorization for creation."));
            }
            if !create {
                return match provider.storage.provision_plan(state, store, provider,
                    &input.storage, &input.revision, input.size_gib).await {
                    Ok(plan) => (StatusCode::OK, json!(plan)),
                    Err(error) => (StatusCode::BAD_REQUEST, json_err("provision_unavailable", &error.to_string())),
                };
            }
            let state = state.clone();
            let store = store.clone();
            let provider = provider.clone();
            start_job_value(jobs, "managed_storage_create", move |running| async move {
                if *running.cancellation().borrow() {
                    running.finish(Outcome::Cancelled);
                    return;
                }
                let cancellation = running.cancellation();
                let result = provider.storage.provision_create(&state, &store, &provider,
                    &input.storage, &input.revision, input.size_gib, input.review_token.as_deref().unwrap_or_default(),
                    &cancellation).await;
                match result {
                    Ok(plan) => running.finish(Outcome::Succeeded(json!(plan))),
                    Err(error) => running.finish(Outcome::Failed(error.to_string())),
                }
            }).await
        }
        "managed_directory_browse" => {
            #[derive(serde::Deserialize)]
            #[serde(default, deny_unknown_fields)]
            #[derive(Default)]
            struct Input {
                path: String,
            }
            let Ok(input) = serde_json::from_value::<Input>(Value::Object(arguments)) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Expected an optional directory path."),
                );
            };
            if input.path.chars().count() > 4096 || input.path.contains('\0') {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Invalid directory path."),
                );
            }
            match crate::core::declarative::browse::browse(&input.path) {
                Ok(listing) => (StatusCode::OK, json!(listing)),
                Err(error) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json_err("unavailable", &error.to_string()),
                ),
            }
        }
        "system_status" => {
            let mut hostname = [0_u8; 256];
            if unsafe { libc::gethostname(hostname.as_mut_ptr().cast(), hostname.len()) } != 0 {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json_err("operation_failed", "Could not read the server hostname."),
                );
            }
            let end = hostname
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(hostname.len());
            (
                StatusCode::OK,
                json!({
                    "hostname": String::from_utf8_lossy(&hostname[..end]),
                    "startedAt": started_at,
                    "version": env!("CARGO_PKG_VERSION"),
                    "allowWrites": systemd::writes_enabled(state) || package_updates::writes_enabled(state).await || writes_allowed(state)
                }),
            )
        }

        "system_capabilities" => {
            let permission_states = permissions(state).await;
            let privileged_writes =
                permission_states["systemServices"]["status"].as_str() == Some("ready");
            (
                StatusCode::OK,
                json!({
                    "modules": modules(state),
                    "allowWrites": privileged_writes || systemd::writes_enabled(state) || package_updates::writes_enabled(state).await || writes_allowed(state),
                    "permissions": permission_states
                }),
            )
        }

        "jobs_list" => (StatusCode::OK, json!(jobs.list())),

        "jobs_get" | "jobs_cancel" => {
            let id = arguments
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok());
            let Some(id) = id.filter(|_| arguments.len() == 1) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Expected a job ID."),
                );
            };
            if name == "jobs_cancel" {
                if jobs.get(id).is_some_and(|job| {
                    (job.tool.starts_with("services_")
                        || job.tool.starts_with("updates_")
                        || job.tool.starts_with("workloads_")
                        || matches!(
                            job.tool,
                            "applications_action"
                                | "packages_install"
                                | "dependencies_install"
                                | "managed_resource_save"
                                | "managed_config_save"
                                | "managed_apply_retry"
                                | "managed_config_revert"
                        ))
                        && job.state == crate::core::jobs::JobState::Running
                }) {
                    return (
                        StatusCode::CONFLICT,
                        json_err(
                            "not_cancellable",
                            "The native operation may already be in progress.",
                        ),
                    );
                }
                return if jobs.cancel(id) {
                    (StatusCode::OK, json!({"status": "cancellation requested"}))
                } else {
                    (
                        StatusCode::NOT_FOUND,
                        json_err("not_found", "Job not found."),
                    )
                };
            }
            match jobs.get(id) {
                Some(job) => (StatusCode::OK, json!(job)),
                None => (
                    StatusCode::NOT_FOUND,
                    json_err("not_found", "Job not found."),
                ),
            }
        }

        "services_list" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct InventoryArgs {
                #[serde(default)]
                scope: systemd::Scope,
            }
            let Ok(input) = serde_json::from_value::<InventoryArgs>(Value::Object(arguments))
            else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Scope must be system or user."),
                );
            };
            match systemd::inventory(state, input.scope).await {
                Ok(inventory) => (StatusCode::OK, json!(inventory)),
                Err(_) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json_err("unavailable", "Systemd inventory exceeds its limits."),
                ),
            }
        }

        "services_details" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct DetailsArgs {
                #[serde(default)]
                scope: systemd::Scope,
                unit: String,
            }
            let Ok(input) = serde_json::from_value::<DetailsArgs>(Value::Object(arguments)) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "invalid_arguments",
                        "Expected a manager scope and exact service unit.",
                    ),
                );
            };
            if !systemd::valid_unit(&input.unit) {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Expected an exact .service unit."),
                );
            }
            match systemd::details(state, input.scope, &input.unit).await {
                Ok(details) => (StatusCode::OK, json!(details)),
                Err(error) if error.to_string() == "service unit not found" => (
                    StatusCode::NOT_FOUND,
                    json_err("not_found", "Service unit not found."),
                ),
                Err(_) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json_err(
                        "unavailable",
                        "Service details are unavailable for this systemd bus.",
                    ),
                ),
            }
        }

        "services_logs" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct JournalArgs {
                #[serde(default)]
                scope: systemd::Scope,
                unit: String,
                tail: Option<u16>,
            }
            let Ok(input) = serde_json::from_value::<JournalArgs>(Value::Object(arguments)) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "invalid_arguments",
                        "Expected a scope, service unit, and optional line limit.",
                    ),
                );
            };
            let tail = input.tail.unwrap_or(200);
            if !systemd::valid_unit(&input.unit) || !(1..=2000).contains(&tail) {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "invalid_arguments",
                        "Expected an exact .service unit and a limit between 1 and 2000.",
                    ),
                );
            }
            match systemd::logs(state, input.scope, &input.unit, tail).await {
                Ok(journal) => (StatusCode::OK, json!(journal)),
                Err(_) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json_err(
                        "unavailable",
                        "Journal unavailable. Check journal permissions and the selected scope.",
                    ),
                ),
            }
        }

        "monitor_snapshot" => (StatusCode::OK, json!(monitor.snapshot())),

        "monitor_processes" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct ProcessArgs {
                #[serde(default)]
                sort: ProcessSort,
                #[serde(default)]
                filter: String,
                #[serde(default = "default_limit")]
                limit: usize,
            }
            fn default_limit() -> usize {
                100
            }
            let Ok(input) = serde_json::from_value::<ProcessArgs>(Value::Object(arguments)) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "invalid_arguments",
                        "Expected a process sort, filter and row limit.",
                    ),
                );
            };
            if input.filter.chars().count() > 128 || !(1..=500).contains(&input.limit) {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "invalid_arguments",
                        "Filter and row limit exceed their allowed range.",
                    ),
                );
            }
            (
                StatusCode::OK,
                json!(monitor.processes(input.sort, &input.filter, input.limit)),
            )
        }

        "updates_status" => (StatusCode::OK, json!(package_updates::status(state).await)),

        "shares_setup" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct SetupArgs { protocol: String, confirm: bool }
            let Ok(input) = serde_json::from_value::<SetupArgs>(Value::Object(arguments)) else {
                return (StatusCode::BAD_REQUEST,json_err("invalid_arguments","Expected a protocol and confirm=true."));
            };
            if !input.confirm || !matches!(input.protocol.as_str(),"samba"|"nfs") {
                return (StatusCode::BAD_REQUEST,json_err("invalid_arguments","Expected samba or nfs and confirm=true."));
            }
            if !writes_allowed(state) || !state.config.system.allow_share_writes || !state.config.system.allow_package_writes {
                return (StatusCode::FORBIDDEN,json_err("writes_disabled","Share server setup requires enabled share and package writes."));
            }
            if state.config.test_mode || !provider.shares.native_enabled() {
                return (StatusCode::SERVICE_UNAVAILABLE,json_err("helper_unavailable","Native share setup requires the fixed reference configuration paths."));
            }
            let helper = admin::probe().await;
            if !helper.permits_shares() || !helper.permits_packages() {
                return (StatusCode::SERVICE_UNAVAILABLE,json_err("helper_unavailable","Share setup requires authorized share and package grants."));
            }
            start_job_value(jobs,"shares_setup",move |running| async move {
                match admin::share_operation("shares_setup",json!({"protocol":input.protocol,"confirm":true})).await {
                    Ok((result,output)) => {
                        running.append_output(&output);
                        running.finish(Outcome::Succeeded(result));
                    }
                    Err(error) => running.finish(Outcome::Failed(error.to_string())),
                }
            }).await
        }

        "shares_samba_password" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct PasswordArgs {
                password: String,
                confirm: bool,
            }
            let Ok(input) = serde_json::from_value::<PasswordArgs>(Value::Object(arguments)) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "invalid_arguments",
                        "Expected a Samba password and confirm=true; no username may be selected.",
                    ),
                );
            };
            if !input.confirm
                || input.password.len() > 256
                || input.password.trim().is_empty()
                || input.password.contains(['\0', '\r', '\n'])
            {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "invalid_arguments",
                        "Invalid Samba password or missing confirmation.",
                    ),
                );
            }
            if !state.config.system.allow_share_writes {
                return (
                    StatusCode::FORBIDDEN,
                    json_err(
                        "writes_disabled",
                        "Samba password changes are disabled by administrator policy.",
                    ),
                );
            }
            if !admin::probe().await.permits_samba_password() {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json_err(
                        "helper_unavailable",
                        "Samba password changes require an authorized optional helper and root-owned policy.",
                    ),
                );
            }
            // Never place secrets in durable Jobs, audit, tracing fields, logs or
            // tool result content. The helper uses its authenticated caller UID.
            match admin::samba_password(input.password).await {
                Ok(()) => (StatusCode::OK, json!({"status":"Samba password updated"})),
                Err(_) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json_err(
                        "operation_failed",
                        "Samba password update could not be completed. Check the helper and native Samba account.",
                    ),
                ),
            }
        }

        "managed_resource_plan" | "managed_resource_save" => {
            let apply = name == "managed_resource_save";
            let mut arguments = arguments;
            let requested_mode = if apply {
                arguments.remove("mode")
            } else {
                None
            };
            let provided_fingerprint = if apply {
                arguments.remove("fingerprint")
            } else {
                None
            };
            let confirmation = if apply {
                arguments.remove("confirm")
            } else {
                None
            };
            let input = match serde_json::from_value::<crate::core::declarative::edit::EditInput>(
                Value::Object(arguments),
            ) {
                Ok(input) => input,
                Err(_) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        json_err(
                            "invalid_arguments",
                            "Expected a typed resource kind, ID, operation, definition and current revision.",
                        ),
                    );
                }
            };
            if apply && confirmation != Some(Value::Bool(true)) {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "confirmation_required",
                        "Reviewed resource mutations require confirm=true.",
                    ),
                );
            }
            let mode = if apply {
                match requested_mode.as_ref().and_then(Value::as_str) {
                    Some("now") => crate::core::declarative::ApplyMode::Now,
                    Some("next_start") => crate::core::declarative::ApplyMode::NextStart,
                    _ => {
                        return (
                            StatusCode::BAD_REQUEST,
                            json_err("invalid_arguments", "Expected mode now or next_start."),
                        );
                    }
                }
            } else {
                crate::core::declarative::ApplyMode::Now
            };
            let candidate = match crate::core::declarative::edit::edit(store, &input) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return (
                        StatusCode::CONFLICT,
                        json_err("edit_invalid_or_conflicted", &error.to_string()),
                    );
                }
            };
            let preview = match crate::core::linux_config::preview(
                &state.config,
                store,
                &candidate,
                &input.revision,
            ) {
                Ok(plan) => plan,
                Err(error) => {
                    return (
                        StatusCode::CONFLICT,
                        json_err("plan_unavailable", &error.to_string()),
                    );
                }
            };
            let blocked: Vec<String> = provider
                .blocked_resources(&preview.changes)
                .into_iter()
                .map(str::to_owned)
                .collect();
            if !apply {
                if let Err(error)=provider.review_incus_changes(store,&candidate,&preview).await {
                    return (StatusCode::CONFLICT,json_err("incus_transition",&error.to_string()));
                }
                return (
                    StatusCode::OK,
                    json!({
                        "plan":preview,
                        "candidate":String::from_utf8_lossy(&candidate),
                        "blockedResources":blocked
                    }),
                );
            }
            if !writes_allowed(state) {
                return (
                    StatusCode::FORBIDDEN,
                    json_err(
                        "writes_disabled",
                        "Workload mutations are disabled by policy.",
                    ),
                );
            }
            if !blocked.is_empty() {
                return (
                    StatusCode::CONFLICT,
                    json_err(
                        "native_provider_blocked",
                        "At least one affected resource is not ready for native publication.",
                    ),
                );
            }
            let fingerprint =
                provided_fingerprint.and_then(|value| value.as_str().map(str::to_owned));
            let Some(fingerprint) = fingerprint.filter(|value| value == &preview.fingerprint)
            else {
                return (
                    StatusCode::CONFLICT,
                    json_err(
                        "review_mismatch",
                        "A fresh exact plan fingerprint must be reviewed before saving.",
                    ),
                );
            };
            let store_clone = store.clone();
            let provider_clone = provider.clone();
            let expected = input.revision;
            return start_job_value(jobs, "managed_resource_save", move |running| async move {
                let result = store_clone
                    .apply_review_mode(&candidate, &expected, &fingerprint, mode, &*provider_clone)
                    .await;
                match result {
                    Ok(snapshot) => running.finish(Outcome::Succeeded(
                        json!({"revision":snapshot.revision, "mode":mode}),
                    )),
                    Err(error) => running.finish(Outcome::Failed(error.to_string())),
                }
            })
            .await;
        }

        "managed_config_read" => {
            if !writes_allowed(state) {
                return (
                    StatusCode::FORBIDDEN,
                    json_err(
                        "writes_disabled",
                        "Private configuration editing is disabled by policy.",
                    ),
                );
            }
            match store.read() {
                Ok(Some(raw)) => {
                    let parsed = crate::core::declarative::parse(&raw.bytes);
                    (
                        StatusCode::OK,
                        json!({
                            "revision": raw.revision,
                            "content": String::from_utf8_lossy(&raw.bytes),
                            "valid": parsed.is_ok(),
                            "error": parsed.err().map(|error| error.to_string()).unwrap_or_default()
                        }),
                    )
                }
                Ok(None) => (
                    StatusCode::OK,
                    json!({
                        "revision": MISSING_REVISION,
                        "content": "version: 1\n",
                        "valid": true
                    }),
                ),
                Err(_) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json_err("unavailable", "Private YAML cannot be read safely."),
                ),
            }
        }
        "managed_config_validate" => {
            let Some(content) = arguments
                .get("content")
                .and_then(Value::as_str)
                .filter(|_| arguments.len() == 1)
            else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Expected only a YAML content string."),
                );
            };
            if content.len() > crate::core::declarative::MAX_CONFIG_BYTES {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "YAML exceeds 1 MiB."),
                );
            }
            match crate::core::declarative::parse(content.as_bytes()) {
                Ok(config) => (
                    StatusCode::OK,
                    json!({"valid":true, "runtimeChecked":false,
                    "files":crate::core::declarative::preflight::check_files(
                        store, &config, &state.config.paths.storage_dir,
                        state.config.paths.proc_stat.parent(),
                    )}),
                ),
                Err(error) => (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", &error.to_string()),
                ),
            }
        }
        "managed_config_readiness" | "managed_config_check" | "managed_services" => {
            let refresh = name == "managed_config_check";
            let services = name == "managed_services";
            let snapshot = match store.read() {
                Ok(Some(raw)) => raw,
                Ok(None) if !refresh => {
                    let result = if services {
                        json!({"configured":false,"services":[]})
                    } else {
                        json!({"configured":false,"valid":false})
                    };
                    return (StatusCode::OK, result);
                }
                _ if refresh => return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json_err("unavailable", "Configuration is unavailable or invalid."),
                ),
                _ => {
                    let mut result = json!({"configured":true,"valid":false,
                        "error":"Configuration is invalid or inaccessible; open Configuration to repair it."});
                    if services { result["services"] = json!([]); }
                    return (StatusCode::OK, result);
                }
            };
            let config = match snapshot.config() {
                Ok(config) => config,
                Err(_) if refresh => return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json_err("unavailable", "Configuration is unavailable or invalid."),
                ),
                Err(_) => {
                    let mut result = json!({"configured":true,"valid":false,
                        "error":"Configuration is invalid or inaccessible; open Configuration to repair it."});
                    if services { result["services"] = json!([]); }
                    return (StatusCode::OK, result);
                }
            };
            let health = provider.readiness.check(
                state, store, &snapshot, &config, provider.quadlet.is_some(), refresh,
            ).await;
            if refresh {
                return (StatusCode::OK, json!(health));
            }
            if services {
                let apps = provider.list_live_applications(&config).await;
                let items: Vec<_> = apps.iter().map(|app| {
                    let key = format!("service/{}", app.id);
                    let issues: Vec<_> = health.issues.iter().filter(|issue| issue.resource == key).collect();
                    let registered = app.runtime_matches.is_some();
                    let configuration = if app.restart_required {
                        "restart_required"
                    } else if registered {
                        "registered"
                    } else {
                        "unknown"
                    };
                    json!({"id":app.id,"name":app.name,"kind":app.kind,
                        "backend":crate::core::declarative::preflight::service_backend(&config.services[&app.id]),
                        "enabled":app.enabled,"state":app.state,"registered":registered,
                        "configuration":configuration,"issues":issues,"members":app.members})
                }).collect();
                return (StatusCode::OK, json!({"configured":true,"valid":true,
                    "revision":snapshot.revision,"health":health,"services":items}));
            }
            (StatusCode::OK, json!({"configured":true,"valid":true,
                "health":health,"runtimeChecked":false}))
        }
        "managed_config_health" => match store.read() {
            Ok(Some(raw)) => match crate::core::declarative::parse(&raw.bytes) {
                Ok(config) => (
                    StatusCode::OK,
                    json!({"configured":true, "valid":true, "runtimeChecked":false,
                    "files":crate::core::declarative::preflight::check_files(
                        store, &config, &state.config.paths.storage_dir,
                        state.config.paths.proc_stat.parent(),
                    )}),
                ),
                Err(error) => (
                    StatusCode::OK,
                    json!({"configured":true, "valid":false, "runtimeChecked":false,
                        "error":error.to_string()}),
                ),
            },
            Ok(None) => (
                StatusCode::OK,
                json!({"configured":false, "runtimeChecked":false}),
            ),
            Err(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                json_err(
                    "unavailable",
                    "Cannot read the private configuration safely.",
                ),
            ),
        },
        "managed_config_model" => {
            if !writes_allowed(state) {
                return (
                    StatusCode::FORBIDDEN,
                    json_err(
                        "writes_disabled",
                        "Private typed configuration editing is disabled by policy.",
                    ),
                );
            }
            match store.read() {
                Ok(Some(raw)) => match raw.config() {
                    Ok(config) => (
                        StatusCode::OK,
                        json!({
                            "config":config,
                            "revision":raw.revision,
                            "notice":"Private-file references are paths only; no file contents were read."
                        }),
                    ),
                    Err(_) => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        json_err("unavailable", "Invalid YAML; use the raw recovery editor."),
                    ),
                },
                Ok(None) => (
                    StatusCode::OK,
                    json!({
                        "config":{"version":1,"services":{},"storage":{},"shares":{},"bindings":{},"secrets":{}},
                        "revision":MISSING_REVISION
                    }),
                ),
                Err(_) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json_err("unavailable", "Typed configuration is not accessible."),
                ),
            }
        }
        "managed_config_history" => {
            if !writes_allowed(state) {
                return (
                    StatusCode::FORBIDDEN,
                    json_err(
                        "writes_disabled",
                        "Private configuration history is disabled by policy.",
                    ),
                );
            }
            match store.history_entries() {
                Ok(history) => (StatusCode::OK, json!({"entries":history})),
                Err(_) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json_err(
                        "unavailable",
                        "Private configuration history is not accessible.",
                    ),
                ),
            }
        }
        "managed_config_history_read" => {
            if !writes_allowed(state) {
                return (
                    StatusCode::FORBIDDEN,
                    json_err(
                        "writes_disabled",
                        "Private configuration history is disabled by policy.",
                    ),
                );
            }
            let Some(revision) = arguments
                .get("target")
                .and_then(Value::as_str)
                .filter(|_| arguments.len() == 1)
            else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Expected an exact target revision."),
                );
            };
            match store.read_history(revision) {
                Ok(raw) => (
                    StatusCode::OK,
                    json!({
                        "revision":raw.revision,
                        "content":String::from_utf8_lossy(&raw.bytes),
                        "valid":crate::core::declarative::parse(&raw.bytes).is_ok()
                    }),
                ),
                Err(_) => (
                    StatusCode::NOT_FOUND,
                    json_err(
                        "not_found",
                        "The requested private revision is not accessible.",
                    ),
                ),
            }
        }

        "managed_backup_inventory" => {
            // The frozen reference requires the private-configuration policy
            // even though inventory does not mutate native state.
            if !writes_allowed(state) {
                return (
                    StatusCode::FORBIDDEN,
                    json_err(
                        "writes_disabled",
                        "Private backup inventory is disabled by policy.",
                    ),
                );
            }
            // Never perform native mutation or claim the result can be restored.
            let authority_path = store.path();
            let root = authority_path.parent().expect("store path has a parent");
            match crate::core::declarative::backup_inventory::inventory(
                store,
                root,
                &state.config.paths.storage_dir,
            ) {
                Ok(snapshot) => (StatusCode::OK, json!(snapshot)),
                Err(_) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json_err(
                        "backup_inventory_unavailable",
                        "Backup inventory is unavailable; check authoritative YAML and account-readable input metadata.",
                    ),
                ),
            }
        }

        "managed_config_plan"
        | "managed_config_save"
        | "managed_config_revert_plan"
        | "managed_config_revert" => {
            let revert = matches!(name, "managed_config_revert_plan" | "managed_config_revert");
            let mut arguments = arguments;
            if revert {
                if !writes_allowed(state) {
                    return (
                        StatusCode::FORBIDDEN,
                        json_err(
                            "writes_disabled",
                            "Private configuration history is disabled by policy.",
                        ),
                    );
                }
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct RevertInput {
                    target: String,
                    revision: String,
                    mode: Option<String>,
                    confirm: Option<bool>,
                }
                let Ok(input) = serde_json::from_value::<RevertInput>(Value::Object(arguments))
                else {
                    return (
                        StatusCode::BAD_REQUEST,
                        json_err(
                            "invalid_arguments",
                            "Expected target and revision, and mode/confirm for revert.",
                        ),
                    );
                };
                let historical = match store.read_history(&input.target) {
                    Ok(snapshot) => snapshot,
                    Err(_) => {
                        return (
                            StatusCode::NOT_FOUND,
                            json_err(
                                "not_found",
                                "The requested private revision is not accessible.",
                            ),
                        );
                    }
                };
                let Ok(content) = String::from_utf8(historical.bytes) else {
                    return (
                        StatusCode::CONFLICT,
                        json_err(
                            "invalid_history",
                            "The selected revision is not valid configuration.",
                        ),
                    );
                };
                arguments = serde_json::from_value(json!({
                    "content":content,"revision":input.revision,"mode":input.mode,"confirm":input.confirm
                })).expect("configuration arguments are an object");
            }
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                content: String,
                revision: String,
                mode: Option<String>,
                confirm: Option<bool>,
            }
            let save = matches!(name, "managed_config_save" | "managed_config_revert");
            let Ok(input) = serde_json::from_value::<Input>(Value::Object(arguments)) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "invalid_arguments",
                        "Expected content and revision, and mode/confirm for saves.",
                    ),
                );
            };
            if input.content.is_empty()
                || input.content.len() > 1048576
                || input.revision.is_empty()
                || input.revision.len() > 64
                || (!save && (input.mode.is_some() || input.confirm.is_some()))
                || (save
                    && (input.confirm != Some(true)
                        || !matches!(input.mode.as_deref(), Some("now" | "next_start"))))
            {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "invalid_arguments",
                        "Invalid configuration arguments or missing destructive confirmation.",
                    ),
                );
            }
            if save && !writes_allowed(state) {
                return (
                    StatusCode::FORBIDDEN,
                    json_err("writes_disabled", "Workload writes are disabled."),
                );
            }
            if revert {
                if let Err(error) = store
                    .validate_revert(input.content.as_bytes(), &input.revision, &**provider)
                    .await
                {
                    return (
                        StatusCode::CONFLICT,
                        json_err("revert_unavailable", &error.to_string()),
                    );
                }
            }
            let plan = match crate::core::linux_config::preview(
                &state.config,
                store,
                input.content.as_bytes(),
                &input.revision,
            ) {
                Ok(plan) => plan,
                Err(error) => {
                    return (
                        StatusCode::CONFLICT,
                        json_err("plan_unavailable", &error.to_string()),
                    );
                }
            };
            if !save {
                if let Err(error)=provider.review_incus_changes(store,input.content.as_bytes(),&plan).await {
                    return (StatusCode::CONFLICT,json_err("incus_transition",&error.to_string()));
                }
                return (
                    StatusCode::OK,
                    json!({"unavailable":provider.blocked_resources(&plan.changes),"plan":plan}),
                );
            }
            let mode = if input.mode.as_deref() == Some("next_start") {
                crate::core::declarative::ApplyMode::NextStart
            } else {
                crate::core::declarative::ApplyMode::Now
            };
            let store = store.clone();
            let config = state.config.clone();
            let provider = provider.clone();
            let job_tool = if revert {
                "managed_config_revert"
            } else {
                "managed_config_save"
            };
            start_job_value(jobs, job_tool, move |running| async move {
                let result = async {
                    crate::core::linux_config::mutation_ready(&config, &store)?;
                    let plan = crate::core::linux_config::preview(&config, &store, input.content.as_bytes(), &input.revision)?;
                    let snapshot = if revert {
                        store.apply_revert_review_mode(input.content.as_bytes(), &input.revision, &plan.fingerprint, mode, &*provider).await?
                    } else {
                        store.apply_review_mode(input.content.as_bytes(), &input.revision, &plan.fingerprint, mode, &*provider).await?
                    };
                    Ok::<_, anyhow::Error>(json!({"revision":snapshot.revision,"restartRequired":plan.restart_required,"mode":input.mode}))
                }.await;
                match result {
                    Ok(result) => running.finish(Outcome::Succeeded(result)),
                    Err(error) => running.finish(Outcome::Failed(error.to_string())),
                }
            }).await
        }
        "workloads_get" => match store.read() {
            Ok(Some(snap)) => (
                StatusCode::OK,
                json!({
                    "yaml": String::from_utf8_lossy(&snap.bytes),
                    "revision": snap.revision,
                }),
            ),
            Ok(None) => (
                StatusCode::OK,
                json!({
                    "yaml": "",
                    "revision": null,
                }),
            ),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json_err("read_failed", &e.to_string()),
            ),
        },

        "workloads_plan" => {
            let Some(candidate) = arguments.get("candidate").and_then(Value::as_str) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Expected 'candidate' YAML string."),
                );
            };
            let current = store.read().ok().flatten();
            let current_rev = current.as_ref().map_or(MISSING_REVISION, |s| &s.revision);
            let expected = arguments
                .get("expected")
                .and_then(Value::as_str)
                .unwrap_or(current_rev);
            match crate::core::linux_config::preview(
                &state.config,
                store,
                candidate.as_bytes(),
                expected,
            ) {
                Ok(plan) => {
                    if let Err(error)=provider.review_incus_changes(store,candidate.as_bytes(),&plan).await {
                        return (StatusCode::CONFLICT,json_err("incus_transition",&error.to_string()));
                    }
                    (StatusCode::OK,json!({
                        "plan":plan,
                        "blockedResources":provider.blocked_resources(&plan.changes),
                    }))
                },
                Err(e) => (
                    StatusCode::BAD_REQUEST,
                    json_err("plan_error", &e.to_string()),
                ),
            }
        }

        "workloads_apply" => {
            if !writes_allowed(state) {
                return (
                    StatusCode::FORBIDDEN,
                    json_err(
                        "writes_disabled",
                        "Workload mutations require writes to be enabled.",
                    ),
                );
            }
            let Some(candidate) = arguments.get("candidate").and_then(Value::as_str) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Expected 'candidate' string."),
                );
            };
            let Some(fingerprint) = arguments.get("fingerprint").and_then(Value::as_str) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Expected 'fingerprint' string."),
                );
            };
            let current = store.read().ok().flatten();
            let current_rev = current
                .as_ref()
                .map_or(MISSING_REVISION, |s| s.revision.as_str());
            let expected = arguments
                .get("expected")
                .and_then(Value::as_str)
                .unwrap_or(current_rev)
                .to_string();
            let candidate = candidate.to_string();
            let fingerprint = fingerprint.to_string();
            let store_clone = store.clone();
            let provider_clone = provider.clone();
            let config = state.config.clone();
            start_job_value(jobs, "workloads_apply", move |running| async move {
                let review = crate::core::linux_config::mutation_ready(&config, &store_clone)
                    .and_then(|()| {
                        crate::core::linux_config::preview(
                            &config,
                            &store_clone,
                            candidate.as_bytes(),
                            &expected,
                        )
                    });
                if let Err(error) = review {
                    running.finish(Outcome::Failed(error.to_string()));
                    return;
                }
                match store_clone
                    .apply_review(
                        candidate.as_bytes(),
                        &expected,
                        &fingerprint,
                        &*provider_clone,
                    )
                    .await
                {
                    Ok(snapshot) => {
                        running.append_output("Workloads successfully applied.\n");
                        running
                            .finish(Outcome::Succeeded(json!({ "revision": snapshot.revision })));
                    }
                    Err(e) => {
                        running.finish(Outcome::Failed(e.to_string()));
                    }
                }
            })
            .await
        }

        "workloads_pending" if !arguments.is_empty() => (
            StatusCode::BAD_REQUEST,
            json_err("invalid_arguments", "Pending journal accepts no arguments."),
        ),
        "managed_apply_status" => {
            if !arguments.is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "No arguments are accepted."),
                );
            }
            match store.apply_status() {
                Ok(status) => (
                    StatusCode::OK,
                    status.unwrap_or_else(|| json!({"stage":"none"})),
                ),
                Err(_) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json_err(
                        "apply_status_unavailable",
                        "Apply journal is unreadable or invalid; inspect private state files.",
                    ),
                ),
            }
        }
        "managed_apply_retry" => {
            if !writes_allowed(state) {
                return (
                    StatusCode::FORBIDDEN,
                    json_err(
                        "writes_disabled",
                        "Workload mutations are disabled by policy.",
                    ),
                );
            }
            if arguments.len() != 1 || arguments.get("confirm") != Some(&Value::Bool(true)) {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("confirmation_required", "Retry requires only confirm=true."),
                );
            }
            let store = store.clone();
            let provider = provider.clone();
            start_job_value(jobs, "managed_apply_retry", move |running| async move {
                let outcome = match store.retry_apply(&*provider).await {
                    Ok(status) => {
                        if status["stage"] == "aborted" {
                            running.append_output("Prepared intent was not committed to YAML; discarded without publishing configuration or changing native resources.\n");
                        }
                        Outcome::Succeeded(status)
                    },
                    Err(error) => Outcome::Failed(error.to_string()),
                };
                running.finish(outcome);
            })
            .await
        }
        "workloads_pending" => match store.pending_apply() {
            Ok(journal) => (StatusCode::OK, json!({"pending": journal})),
            Err(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json_err("read_failed", "Could not read the workload journal."),
            ),
        },
        "workloads_resume" => {
            if !writes_allowed(state) {
                return (
                    StatusCode::FORBIDDEN,
                    json_err(
                        "writes_disabled",
                        "Workload mutations require writes to be enabled.",
                    ),
                );
            }
            let fingerprint = arguments
                .get("fingerprint")
                .and_then(Value::as_str)
                .filter(|_| arguments.len() == 1)
                .filter(|id| {
                    id.len() == 64
                        && id
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                });
            let Some(fingerprint) = fingerprint.map(str::to_owned) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Expected a pending plan fingerprint."),
                );
            };
            let store = store.clone();
            let provider = provider.clone();
            start_job_value(jobs, "workloads_resume", move |running| async move {
                let result = async {
                    let snapshot = store.resume_apply(&fingerprint, &*provider).await?;
                    let recovery_pending = store.pending_apply()?.is_some();
                    Ok::<_, anyhow::Error>((snapshot, recovery_pending))
                }.await;
                let outcome = match result {
                    Ok((snapshot, recovery_pending)) => {
                        if recovery_pending {
                            running.append_output("Replacement YAML was not committed; the prior interrupted transition has been restored and still requires recovery.\n");
                        }
                        Outcome::Succeeded(json!({"revision": snapshot.revision, "recoveryPending": recovery_pending}))
                    },
                    Err(error) => Outcome::Failed(error.to_string()),
                };
                running.finish(outcome);
            })
            .await
        }

        "workloads_history" if !writes_allowed(state) => (
            StatusCode::FORBIDDEN,
            json_err(
                "writes_disabled",
                "Private configuration history is disabled by policy.",
            ),
        ),
        "workloads_history" => match store.list_history() {
            Ok(history) => (StatusCode::OK, json!({ "history": history })),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json_err("history_failed", &e.to_string()),
            ),
        },

        "workloads_revert" => {
            if !writes_allowed(state) {
                return (
                    StatusCode::FORBIDDEN,
                    json_err(
                        "writes_disabled",
                        "Private configuration history is disabled by policy.",
                    ),
                );
            }

            let Some(revision) = arguments.get("revision").and_then(Value::as_str) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Expected 'revision' string."),
                );
            };
            let apply = arguments
                .get("apply")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let historical = match store.read_history(revision) {
                Ok(h) => h,
                Err(_) => {
                    return (
                        StatusCode::NOT_FOUND,
                        json_err("not_found", "Revision not found in history."),
                    );
                }
            };
            let current = store.read().ok().flatten();
            let current_rev = current.as_ref().map_or(MISSING_REVISION, |s| &s.revision);
            let expected = arguments
                .get("expected")
                .and_then(Value::as_str)
                .unwrap_or(current_rev);

            if let Err(error) = store
                .validate_revert(&historical.bytes, expected, &**provider)
                .await
            {
                return (
                    StatusCode::CONFLICT,
                    json_err("revert_unavailable", &error.to_string()),
                );
            }
            if apply {
                if !writes_allowed(state) {
                    return (
                        StatusCode::FORBIDDEN,
                        json_err(
                            "writes_disabled",
                            "Workload mutations require writes to be enabled.",
                        ),
                    );
                }
                let Some(fingerprint) = arguments.get("fingerprint").and_then(Value::as_str) else {
                    return (
                        StatusCode::BAD_REQUEST,
                        json_err(
                            "invalid_arguments",
                            "Expected 'fingerprint' string for revert apply.",
                        ),
                    );
                };
                let expected = expected.to_string();
                let fingerprint = fingerprint.to_string();
                let store_clone = store.clone();
                let provider_clone = provider.clone();
                start_job_value(jobs, "workloads_revert", move |running| async move {
                    match store_clone
                        .apply_revert_review(
                            &historical.bytes,
                            &expected,
                            &fingerprint,
                            &*provider_clone,
                        )
                        .await
                    {
                        Ok(snapshot) => {
                            running.append_output("Workloads successfully reverted.\n");
                            running.finish(Outcome::Succeeded(
                                json!({ "revision": snapshot.revision }),
                            ));
                        }
                        Err(e) => {
                            running.finish(Outcome::Failed(e.to_string()));
                        }
                    }
                })
                .await
            } else {
                match crate::core::linux_config::preview(
                    &state.config,
                    store,
                    &historical.bytes,
                    expected,
                ) {
                    Ok(plan) => (
                        StatusCode::OK,
                        json!({
                            "plan": plan,
                            "blockedResources": provider.blocked_resources(&plan.changes),
                            "candidate": String::from_utf8_lossy(&historical.bytes),
                        }),
                    ),
                    Err(e) => (
                        StatusCode::BAD_REQUEST,
                        json_err("plan_error", &e.to_string()),
                    ),
                }
            }
        }

        "applications_list" => {
            let config = store
                .read()
                .ok()
                .flatten()
                .and_then(|s| s.config().ok())
                .unwrap_or_default();
            let apps = provider.list_live_applications(&config).await;
            (StatusCode::OK, json!({ "applications": apps }))
        }

        "managed_service_enable" | "managed_service_disable" => {
            if !writes_allowed(state) {
                return (StatusCode::FORBIDDEN, json_err("writes_disabled", "Boot policy requires management writes."));
            }
            let Some(id) = arguments.get("service").and_then(Value::as_str)
                .filter(|id| crate::core::declarative::id_valid(id)) else {
                return (StatusCode::BAD_REQUEST, json_err("invalid_arguments", "Invalid managed service ID."));
            };
            if arguments.len() != 2 || arguments.get("confirm") != Some(&Value::Bool(true)) {
                return (StatusCode::BAD_REQUEST, json_err("invalid_arguments", "Changing boot policy requires confirm=true."));
            }
            let enable = name == "managed_service_enable";
            let tool = if enable { "managed_service_enable" } else { "managed_service_disable" };
            let id = id.to_owned();
            let state = state.clone();
            let store = store.clone();
            let provider = provider.clone();
            start_job_value(jobs, tool, move |running| async move {
                let result = async {
                    crate::core::linux_config::mutation_ready(&state.config, &store)?;
                    anyhow::ensure!(store.pending_apply()?.is_none(), "recover the pending apply before changing boot policy");
                    let snapshot = store.read()?.ok_or_else(|| anyhow::anyhow!("configuration is unavailable"))?;
                    let mut desired = snapshot.config()?;
                    let service = desired.services.get(&id).ok_or_else(|| anyhow::anyhow!("unknown managed service"))?;
                    anyhow::ensure!(matches!(service.backend.as_str(),"quadlet"|"compose"|"libvirt"|"incus"), "native boot policy is not verified for this backend");
                    let compose = if service.engine == "docker" {&provider.docker_compose} else {&provider.podman_compose};
                    if service.backend == "compose" {
                        compose.verify_boot_policy(&desired,&id).await?;
                    } else if service.backend == "libvirt" {
                        provider.libvirt.verify_boot_policy(&desired,&id).await?;
                    } else if service.backend == "incus" {
                        provider.incus.verify_boot_policy(&desired,&id).await?;
                    } else {
                        let native = provider.quadlet.as_ref().ok_or_else(|| anyhow::anyhow!("rootless Quadlet provider is unavailable"))?;
                        let status = native.status_details(&desired,&id).await?;
                        anyhow::ensure!(status.state != "unknown","managed runtime identity cannot be verified");
                    }
                    if enable {
                        crate::core::declarative::preflight::check_startup_files(&state, &store, &desired, &id)?;
                    }
                    if service.enabled == enable {
                        return Ok::<_, anyhow::Error>(json!({"service":id,"enabled":enable,"revision":snapshot.revision}));
                    }
                    desired.services.get_mut(&id).unwrap().enabled = enable;
                    let bytes = serde_yaml::to_string(&desired)?.into_bytes();
                    let plan = crate::core::linux_config::preview(&state.config, &store, &bytes, &snapshot.revision)?;
                    anyhow::ensure!(!plan.restart_required && plan.changes.len() == 1
                        && plan.changes[0].resource == format!("service/{id}"),
                        "boot-only update unexpectedly changes other definitions");
                    anyhow::ensure!(!*running.cancellation().borrow(), "boot policy job cancelled before publication");
                    let saved = store.apply_review_mode(&bytes, &snapshot.revision, &plan.fingerprint,
                        crate::core::declarative::ApplyMode::NextStart, &*provider).await?;
                    Ok(json!({"service":id,"enabled":enable,"revision":saved.revision}))
                }.await;
                match result {
                    Ok(value) => running.finish(Outcome::Succeeded(value)),
                    Err(error) => running.finish(Outcome::Failed(error.to_string())),
                }
            }).await
        }
        "applications_action"
        | "managed_service_start"
        | "managed_service_stop"
        | "managed_service_restart" => {
            if !writes_allowed(state) {
                return (
                    StatusCode::FORBIDDEN,
                    json_err(
                        "writes_disabled",
                        "Application actions require writes to be enabled.",
                    ),
                );
            }
            let (tool, id, action) = if name == "applications_action" {
                let id = arguments.get("id").and_then(Value::as_str);
                let action = arguments.get("action").and_then(Value::as_str);
                if arguments.len() != 2 || !matches!(action, Some("start" | "stop" | "restart")) {
                    return (
                        StatusCode::BAD_REQUEST,
                        json_err(
                            "invalid_arguments",
                            "Expected id and start, stop or restart action.",
                        ),
                    );
                }
                ("applications_action", id, action.unwrap())
            } else {
                let (tool, action) = match name {
                    "managed_service_start" => ("managed_service_start", "start"),
                    "managed_service_stop" => ("managed_service_stop", "stop"),
                    _ => ("managed_service_restart", "restart"),
                };
                let destructive = action != "start";
                if arguments.len() != if destructive { 2 } else { 1 }
                    || (destructive && arguments.get("confirm") != Some(&Value::Bool(true)))
                {
                    return (
                        StatusCode::BAD_REQUEST,
                        json_err(
                            "invalid_arguments",
                            "Provide a service ID and confirm=true for stop or restart.",
                        ),
                    );
                }
                (
                    tool,
                    arguments.get("service").and_then(Value::as_str),
                    action,
                )
            };
            let Some(id) = id.filter(|id| crate::core::declarative::id_valid(id)) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Invalid managed service ID."),
                );
            };
            let store = store.clone();
            let id = id.to_string();
            let action = action.to_string();
            let provider_clone = provider.clone();
            let state_clone = state.clone();
            start_job_value(jobs, tool, move |running| async move {
                let config = (|| -> anyhow::Result<_> {
                    anyhow::ensure!(
                        store.pending_apply()?.is_none(),
                        "Workload apply must be recovered before changing application state."
                    );
                    store
                        .read()?
                        .ok_or_else(|| anyhow::anyhow!("No workload configuration is installed."))?
                        .config()
                })();
                let config = match config {
                    Ok(config) => config,
                    Err(error) => {
                        running.finish(Outcome::Failed(error.to_string()));
                        return;
                    }
                };
                if action != "stop" && !state_clone.config.test_mode {
                    if let Err(error) = crate::core::declarative::preflight::check_startup_files(&state_clone, &store, &config, &id) {
                        running.finish(Outcome::Failed(error.to_string()));
                        return;
                    }
                }
                match provider_clone.action(&config, &id, &action).await {
                    Ok(msg) => {
                        running.append_output(&msg);
                        running.finish(Outcome::Succeeded(
                            json!({ "id": id, "action": action, "output": msg }),
                        ));
                    }
                    Err(e) => {
                        running.finish(Outcome::Failed(e.to_string()));
                    }
                }
            })
            .await
        }

        "applications_logs" | "managed_service_logs" => {
            let reference = name == "managed_service_logs";
            let key = if reference { "service" } else { "id" };
            if arguments
                .keys()
                .any(|argument| argument != key && argument != "tail")
            {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Unexpected managed logs argument."),
                );
            }
            let Some(id) = arguments
                .get(key)
                .and_then(Value::as_str)
                .filter(|id| crate::core::declarative::id_valid(id))
            else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Expected 'id' string."),
                );
            };
            let tail = match arguments.get("tail") {
                None => 200,
                Some(value) => match value.as_u64() {
                    Some(lines @ 1..=2000) => lines as usize,
                    _ => {
                        return (
                            StatusCode::BAD_REQUEST,
                            json_err(
                                "invalid_arguments",
                                "Journal line limit must be between 1 and 2000.",
                            ),
                        );
                    }
                },
            };
            let config = store
                .read()
                .ok()
                .flatten()
                .and_then(|s| s.config().ok())
                .unwrap_or_default();
            match provider.managed_logs(&config, id, tail).await {
                Ok(logs) if reference => (StatusCode::OK, json!(logs)),
                Ok(logs) => (
                    StatusCode::OK,
                    json!({ "id": id, "logs": logs.display_text() }),
                ),
                Err(e) => (
                    StatusCode::BAD_REQUEST,
                    json_err("logs_error", &e.to_string()),
                ),
            }
        }

        "managed_share_publish" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input { share: String, published: bool, confirm: bool }
            let Ok(input) = serde_json::from_value::<Input>(Value::Object(arguments)) else {
                return (StatusCode::BAD_REQUEST,json_err("invalid_arguments","Expected share, published and confirm=true."));
            };
            if !input.confirm || !crate::core::declarative::id_valid(&input.share) {
                return (StatusCode::BAD_REQUEST,json_err("invalid_arguments","Invalid share ID or missing confirmation."));
            }
            if !writes_allowed(state) {
                return (StatusCode::FORBIDDEN,json_err("writes_disabled","Share publication requires management writes."));
            }
            if !state.config.test_mode && (!provider.shares.native_enabled() || !admin::probe().await.permits_shares()) {
                return (StatusCode::SERVICE_UNAVAILABLE,json_err("helper_unavailable","Native share publication requires authorized helper grants and fixed configuration paths."));
            }
            let state = state.clone();
            let store = store.clone();
            let provider = provider.clone();
            start_job_value(jobs,"managed_share_publish",move |running| async move {
                let result = async {
                    crate::core::linux_config::mutation_ready(&state.config,&store)?;
                    anyhow::ensure!(store.pending_apply()?.is_none(),"recover the pending apply before changing share publication");
                    let snapshot = store.read()?.ok_or_else(|| anyhow::anyhow!("configuration is unavailable"))?;
                    let mut desired = snapshot.config()?;
                    let share = desired.shares.get_mut(&input.share).ok_or_else(|| anyhow::anyhow!("unknown managed share"))?;
                    if share.enabled == input.published {
                        return Ok::<_,anyhow::Error>(json!({"share":input.share,"published":input.published,"revision":snapshot.revision}));
                    }
                    share.enabled = input.published;
                    let edit = crate::core::declarative::edit::EditInput {
                        kind:"share".into(),id:input.share.clone(),operation:"save".into(),
                        definition:Some(serde_json::to_string(share)?),revision:snapshot.revision.clone(),
                    };
                    let candidate = crate::core::declarative::edit::edit(&store,&edit)?;
                    let plan = crate::core::linux_config::preview(&state.config,&store,&candidate,&snapshot.revision)?;
                    let saved = store.apply_review_mode(&candidate,&snapshot.revision,&plan.fingerprint,
                        crate::core::declarative::ApplyMode::Now,&*provider).await?;
                    Ok(json!({"share":input.share,"published":input.published,"revision":saved.revision}))
                }.await;
                match result {
                    Ok(value) => running.finish(Outcome::Succeeded(value)),
                    Err(error) => running.finish(Outcome::Failed(error.to_string())),
                }
            }).await
        }

        "managed_shares" => match store.read() {
            Ok(Some(snapshot)) => match snapshot.config() {
                Ok(config) => {
                    let mut shares = serde_json::to_value(provider.list_shares(&config))
                        .expect("share summaries serialize");
                    for item in shares.as_array_mut().expect("share array") {
                        if let Some(id) = item["id"].as_str().map(str::to_owned) {
                            item["definition"] = json!(config.shares[&id]);
                            item["configuration"] = json!("unknown");
                            item["issues"] = json!([]);
                        }
                    }
                    (
                        StatusCode::OK,
                        json!({"configured": true, "revision": snapshot.revision, "shares": shares}),
                    )
                }
                Err(_) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json_err(
                        "unavailable",
                        "Authoritative shares configuration is invalid.",
                    ),
                ),
            },
            Ok(None) => (StatusCode::OK, json!({"configured": false, "shares": []})),
            Err(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                json_err(
                    "unavailable",
                    "Authoritative shares configuration is unavailable.",
                ),
            ),
        },

        "shares_list" => {
            #[derive(serde::Deserialize, Default)]
            #[serde(default, deny_unknown_fields)]
            struct Input {
                protocol: Option<String>,
            }
            let Ok(input) = serde_json::from_value::<Input>(Value::Object(arguments)) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Expected an optional share protocol."),
                );
            };
            if input
                .protocol
                .as_deref()
                .is_some_and(|protocol| !matches!(protocol, "samba" | "nfs"))
            {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Unsupported share protocol."),
                );
            }
            // The authorized helper can inspect native ownership and runtime
            // tables that the workload account cannot read. Without it, keep
            // the unprivileged inventory available and fail closed on writes.
            if !state.config.test_mode && provider.shares.helper_paths() {
                let capability = admin::probe().await;
                if capability.status == "ready" {
                    let args = match input.protocol.as_deref() {
                        Some(protocol) => json!({"protocol":protocol}),
                        None => json!({}),
                    };
                    if let Ok((mut result,_)) = admin::share_operation("shares_list",args).await {
                        if !provider.shares.native_enabled() || !capability.permits_shares() {
                            if let Some(backends) = result["backends"].as_array_mut() {
                                for backend in backends {
                                    backend["canManage"] = json!(false);
                                    backend["reason"] = json!("Native share publication is disabled by root-owned policy.");
                                }
                            }
                        }
                        return (StatusCode::OK,result);
                    }
                }
            }
            let result = crate::core::declarative::native_shares::inventory(
                state,
                input.protocol.as_deref(),
            )
            .await;
            (StatusCode::OK, json!(result))
        }

        "shares_status" => {
            let result = crate::core::declarative::native_shares::inventory(state, None).await;
            let mut response = Map::new();
            for backend in result.backends {
                response.insert(
                    backend.protocol.into(),
                    json!({
                        "configured": !backend.setup_required && !state.config.test_mode,
                        "available": backend.available,
                        "path": backend.managed_file,
                        "reason": backend.reason,
                    }),
                );
            }
            (StatusCode::OK, Value::Object(response))
        }

        _ => (
            StatusCode::NOT_FOUND,
            json_err("not_found", "Unknown tool."),
        ),
    }
}

async fn start_job_value<F, Fut>(jobs: &Jobs, tool: &'static str, run: F) -> (StatusCode, Value)
where
    F: FnOnce(crate::core::jobs::RunningJob) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let jobs_clone = jobs.clone();
    let (job, running) = match tokio::task::spawn_blocking(move || jobs_clone.start(tool)).await {
        Ok(Ok(Some(started))) => started,
        Ok(Ok(None)) => {
            return (
                StatusCode::CONFLICT,
                json_err("busy", "Another management job is running."),
            );
        }
        _ => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                json_err(
                    "operation_failed",
                    "Could not lock or audit the management job.",
                ),
            );
        }
    };
    tokio::spawn(run(running));
    (StatusCode::OK, json!({ "job": job }))
}

async fn status(
    State(state): State<AppState>,
    Extension(started_at): Extension<DateTime<Utc>>,
) -> Response {
    let mut hostname = [0_u8; 256];
    if unsafe { libc::gethostname(hostname.as_mut_ptr().cast(), hostname.len()) } != 0 {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "operation_failed",
            "Could not read the server hostname.",
        );
    }
    let end = hostname
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(hostname.len());
    let helper = admin::probe().await;
    let privileged_writes = systemd::available(&state)
        && state.config.platform == crate::config::Platform::Linux
        && state
            .config
            .services
            .systemd_system_write_units
            .iter()
            .any(|unit| {
                systemd::permits(&state, systemd::Scope::System, unit) && helper.permits_unit(unit)
            });
    Json(json!({
        "hostname": String::from_utf8_lossy(&hostname[..end]),
        "startedAt": started_at,
        "version": env!("CARGO_PKG_VERSION"),
        "allowWrites": privileged_writes || systemd::writes_enabled(&state) || package_updates::writes_enabled(&state).await || writes_allowed(&state)
    }))
    .into_response()
}

fn json_err(code: &str, message: &str) -> Value {
    json!({ "error": { "code": code, "message": message } })
}

pub(super) fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json_err(code, message))).into_response()
}
