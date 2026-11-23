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
    platforms::{package_updates, systemd},
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
    ServicesLogs,
    MonitorSnapshot,
    MonitorProcesses,
    UpdatesStatus,
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
    SharesStatus,
}

impl Tool {
    const ALL: [Self; 22] = [
        Self::Status,
        Self::Capabilities,
        Self::JobsList,
        Self::JobsGet,
        Self::JobsCancel,
        Self::ServicesList,
        Self::ServicesLogs,
        Self::MonitorSnapshot,
        Self::MonitorProcesses,
        Self::UpdatesStatus,
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
                "List configured Samba and NFS shares.",
            ),
            Self::SharesStatus => (
                "shares_status",
                "Shares service status",
                "Inspect the status and active exports of Samba and NFS.",
            ),
        }
    }
}

pub(super) fn router(state: AppState) -> Router<AppState> {
    let sessions = Sessions::new(state.config.clone());
    let store = Store::new(state.config.paths.data_dir.join("workloads"));
    let provider = Arc::new(CompositeProvider::new(&state.config, state.runner.clone()));
    Router::new()
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
        }))
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
                Tool::ServicesList | Tool::ServicesLogs => "systemd",
                Tool::MonitorSnapshot | Tool::MonitorProcesses => "monitor",
                Tool::UpdatesStatus => "updates",
                Tool::WorkloadsGet
                | Tool::WorkloadsPlan
                | Tool::WorkloadsApply
                | Tool::WorkloadsHistory
                | Tool::WorkloadsPending
                | Tool::WorkloadsResume
                | Tool::WorkloadsRevert
                | Tool::ApplicationsList
                | Tool::ApplicationsAction
                | Tool::ApplicationsLogs => "workloads",
                Tool::SharesList | Tool::SharesStatus => "shares",
                _ => "system",
            };
            let read_only = !matches!(
                tool,
                Tool::JobsCancel
                    | Tool::WorkloadsApply
                    | Tool::WorkloadsResume
                    | Tool::WorkloadsRevert
                    | Tool::ApplicationsAction
            );
            let input_schema = match tool {
                Tool::JobsGet | Tool::JobsCancel => json!({
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "format": "uuid"}
                    },
                    "required": ["id"],
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
                "destructive": false,
                "inputSchema": input_schema
            })
        })
        .collect();

    tools.extend(systemd::ACTIONS.map(|action| {
        json!({
            "name": format!("services_{action}"),
            "module": "systemd",
            "title": format!("Service {action}"),
            "description": "Run an opted-in systemd action as a tracked job.",
            "readOnly": false,
            "destructive": false,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scope": {"type": "string", "enum": ["system", "user"]},
                    "unit": {"type": "string", "maxLength": 255}
                },
                "required": ["unit"],
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

async fn discover(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "tools": discover_tools(&state), "modules": modules(&state) }))
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
    arguments: Map<String, Value>,
) -> (StatusCode, Value) {
    let state = ctx.state;
    let started_at = ctx.started_at;
    let jobs = ctx.jobs;
    let monitor = ctx.monitor;
    let store = ctx.store;
    let provider = ctx.provider;
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
        if !package_updates::permits(state, install) {
            return (
                StatusCode::FORBIDDEN,
                json_err(
                    "writes_disabled",
                    "Package operations require explicit opt-in, native tools, and root permissions.",
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
        if !systemd::permits(state, input.scope, &input.unit) {
            return (
                StatusCode::FORBIDDEN,
                json_err(
                    "writes_disabled",
                    "This service and scope are outside the write policy.",
                ),
            );
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
            | "workloads_history"
            | "applications_list"
            | "shares_list"
            | "shares_status"
    ) && !arguments.is_empty()
    {
        return (
            StatusCode::BAD_REQUEST,
            json_err("invalid_arguments", "This tool accepts no arguments."),
        );
    }

    match name {
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
                    "allowWrites": systemd::writes_enabled(state) || package_updates::writes_enabled(state) || writes_allowed(state)
                }),
            )
        }

        "system_capabilities" => (
            StatusCode::OK,
            json!({
                "modules": modules(state),
                "allowWrites": systemd::writes_enabled(state) || package_updates::writes_enabled(state) || writes_allowed(state)
            }),
        ),

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
                        || job.tool == "applications_action")
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
            match store.preview(candidate.as_bytes(), expected) {
                Ok(plan) => (
                    StatusCode::OK,
                    json!({
                        "plan": plan,
                        "blockedResources": provider.blocked_resources(&plan.changes),
                    }),
                ),
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
            start_job_value(jobs, "workloads_apply", move |running| async move {
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
                let outcome = match store.resume_apply(&fingerprint, &*provider).await {
                    Ok(snapshot) => Outcome::Succeeded(json!({"revision": snapshot.revision})),
                    Err(error) => Outcome::Failed(error.to_string()),
                };
                running.finish(outcome);
            })
            .await
        }

        "workloads_history" => match store.list_history() {
            Ok(history) => (StatusCode::OK, json!({ "history": history })),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json_err("history_failed", &e.to_string()),
            ),
        },

        "workloads_revert" => {
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
                        .apply_review(&historical.bytes, &expected, &fingerprint, &*provider_clone)
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
                match store.preview(&historical.bytes, expected) {
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

        "applications_action" => {
            if !writes_allowed(state) {
                return (
                    StatusCode::FORBIDDEN,
                    json_err(
                        "writes_disabled",
                        "Application actions require writes to be enabled.",
                    ),
                );
            }
            let Some(id) = arguments.get("id").and_then(Value::as_str) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err("invalid_arguments", "Expected 'id' string."),
                );
            };
            let Some(action) = arguments.get("action").and_then(Value::as_str) else {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "invalid_arguments",
                        "Expected 'action' string (start, stop, restart).",
                    ),
                );
            };
            if !matches!(action, "start" | "stop" | "restart") {
                return (
                    StatusCode::BAD_REQUEST,
                    json_err(
                        "invalid_arguments",
                        "Action must be start, stop, or restart.",
                    ),
                );
            }
            let store = store.clone();
            let id = id.to_string();
            let action = action.to_string();
            let provider_clone = provider.clone();
            start_job_value(jobs, "applications_action", move |running| async move {
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

        "applications_logs" => {
            let Some(id) = arguments.get("id").and_then(Value::as_str) else {
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
            match provider.logs(&config, id, tail).await {
                Ok(logs) => (StatusCode::OK, json!({ "id": id, "logs": logs })),
                Err(e) => (
                    StatusCode::BAD_REQUEST,
                    json_err("logs_error", &e.to_string()),
                ),
            }
        }

        "shares_list" => {
            let config = store
                .read()
                .ok()
                .flatten()
                .and_then(|s| s.config().ok())
                .unwrap_or_default();
            let shares = provider.list_shares(&config);
            (StatusCode::OK, json!({ "shares": shares }))
        }

        "shares_status" => (
            StatusCode::OK,
            json!({
                "samba": {
                    "configured": state.config.paths.samba_conf.exists(),
                    "available": true,
                    "path": state.config.paths.samba_conf.display().to_string(),
                },
                "nfs": {
                    "configured": state.config.paths.exports_file.exists(),
                    "available": true,
                    "path": state.config.paths.exports_file.display().to_string(),
                }
            }),
        ),

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
    Json(json!({
        "hostname": String::from_utf8_lossy(&hostname[..end]),
        "startedAt": started_at,
        "version": env!("CARGO_PKG_VERSION"),
        "allowWrites": systemd::writes_enabled(&state) || package_updates::writes_enabled(&state) || writes_allowed(&state)
    }))
    .into_response()
}

fn json_err(code: &str, message: &str) -> Value {
    json!({ "error": { "code": code, "message": message } })
}

pub(super) fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json_err(code, message))).into_response()
}
