//! Explicit shared module operations for the origin-checked Linux session UI.
//! Keep the existing bearer-authenticated router routes unchanged.
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::Method,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    models::{ApiMessage, CertificateMethod, CertificateSpec, FirewallPolicy},
    platforms::admin,
    state::AppState,
};

use super::{ApiError, adguard, firewall, nginx, wol};

pub(super) fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/shared/modules", get(modules))
        .route("/shared/nginx/test", post(nginx::test))
        .route("/shared/nginx/reload", post(nginx::reload))
        .route("/shared/nginx/start", post(nginx::start))
        .route("/shared/nginx/stop", post(nginx::stop))
        .route("/shared/nginx/objects/{kind}", post(nginx::create_object))
        .route(
            "/shared/nginx/objects/{kind}/{domain}/{name}",
            get(nginx::get_object)
                .put(nginx::update_object)
                .delete(nginx::delete_object),
        )
        .route(
            "/shared/nginx/objects/{kind}/{domain}/{name}/{action}",
            post(nginx::object_action),
        )
        .route("/shared/nginx/status", get(super::nginx::status))
        .route("/shared/nginx/sites", get(super::nginx::list_objects))
        .route("/shared/nginx/settings", get(nginx::get_settings).put(nginx::put_settings))
        .route("/shared/nginx/access-preview", post(nginx::preview_access))
        .route("/shared/nginx/ingress-usage", get(nginx::ingress_usage))
        .route("/shared/nginx/logs", get(super::nginx::list_logs))
        .route("/shared/nginx/logs/{*path}", get(super::nginx::get_log))
        .route("/shared/certificates", get(certificates).post(create_certificate))
        .route(
            "/shared/certificates/{id}",
            delete(super::certificates::delete).put(update_certificate),
        )
        .route("/shared/certificates/{id}/issue", post(issue_certificate))
        .route("/shared/certificates/{id}/renew", post(renew_certificate))
        .route(
            "/shared/certificates/http-access",
            get(super::certificates::http_access_status)
                .delete(super::certificates::close_http_access),
        )
        .route("/shared/certificates/lock", get(certificate_lock))
        .route("/shared/certificates/lock/clear", post(clear_certificate_lock))
        .route("/shared/certificates/dehydrated/update", post(update_dehydrated))
        .route("/shared/firewall", get(firewall::status).put(update_firewall))
        .route("/shared/firewall/capability", get(firewall_capability))
        .route("/shared/firewall/allowlist", post(firewall::add_allowlist))
        .route("/shared/firewall/allowlist/{network}", delete(firewall::delete_allowlist))
        .route("/shared/firewall/bans", post(firewall::manual_ban))
        .route("/shared/firewall/bans/{network}", delete(firewall::unban))
        .route("/shared/firewall/reset-counts/{network}", post(firewall::reset_counts))
        .route("/shared/wol", get(wol::list).post(wol::create))
        .route("/shared/wol/status", get(wol::status))
        .route("/shared/wol/{id}", delete(wol::delete).put(wol::update))
        .route("/shared/wol/{id}/wake", post(wol::wake))
        .route("/shared/adguard/config", get(adguard::get_config).put(adguard::update_config))
        .route("/shared/adguard/status", get(adguard::get_adguard_status))
        .route("/shared/adguard/protection", post(adguard::set_protection))
        .route("/shared/adguard/filtering", post(adguard::set_filtering))
        .route("/shared/adguard/rewrites", get(adguard::get_rewrites).put(adguard::update_rewrites))
        .route("/shared/wireguard/status", get(wireguard_status))
        .route("/shared/wireguard/toggle", post(wireguard_toggle))
        .route("/shared/wireguard/peers", get(wireguard_peers).put(write_wireguard_peers))
        .route_layer(middleware::from_fn_with_state(state, authorize_mutation))
}

// Never send DNS hook environment variables (which may contain credentials)
// to the session UI.
async fn certificates(State(state): State<AppState>) -> Json<Vec<Value>> {
    let Json(statuses) = super::certificates::list(State(state)).await;
    Json(
        statuses
            .into_iter()
            .map(|status| {
                let spec = status.spec;
                json!({
                    "id": spec.id, "name": spec.name, "domains": spec.domains,
                    "method": spec.method, "staging": spec.staging,
                    "auto_renew": spec.auto_renew, "exists": status.exists,
                    "expires_at": status.expires_at, "days_remaining": status.days_remaining,
                    "renewal_due": status.renewal_due,
                })
            })
            .collect(),
    )
}

async fn certificate_lock(State(state): State<AppState>) -> Result<Json<Value>, super::ApiError> {
    let Json(lock) = super::certificates::dehydrated_lock(State(state)).await?;
    Ok(Json(
        json!({ "locked": lock.locked, "status": lock.status }),
    ))
}

async fn authorize_mutation(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        if state.config.platform != crate::config::Platform::Linux
            || state.config.linux_authority.is_none()
        {
            return ApiError::conflict("Native Linux configuration is required").into_response();
        }
        let ready = crate::core::linux_config::store(&state.config)
            .and_then(|store| crate::core::linux_config::mutation_ready(&state.config, &store));
        if let Err(error) = ready {
            return ApiError::conflict(error).into_response();
        }
    }
    next.run(request).await
}

async fn modules(State(state): State<AppState>) -> Json<crate::core::modules::Discovery> {
    use crate::core::modules::Availability;
    let mut discovery = crate::core::modules::discover(&state.config);
    let enabled = adguard::get_effective_config(&state).await.enabled;
    if let Some(module) = discovery.modules.iter_mut().find(|m| m.id == "adguard") {
        module.availability = if enabled {
            Availability::Ready
        } else {
            Availability::Disabled
        };
    }
    Json(discovery)
}

// Browser clients can edit public certificate metadata, not executable hooks or
// hook environment variables. Keep operator-provisioned DNS credentials intact.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CertificateInput {
    name: String,
    domains: Vec<String>,
    method: CertificateMethod,
    staging: bool,
    auto_renew: bool,
}

impl CertificateInput {
    fn apply(self, spec: &mut CertificateSpec) {
        spec.name = self.name;
        spec.domains = self.domains;
        spec.method = self.method;
        spec.staging = self.staging;
        spec.auto_renew = self.auto_renew;
    }
}

async fn create_certificate(
    State(state): State<AppState>,
    Json(input): Json<CertificateInput>,
) -> Result<Json<ApiMessage>, ApiError> {
    let mut spec = CertificateSpec {
        id: Uuid::new_v4(),
        name: String::new(),
        domains: Vec::new(),
        method: CertificateMethod::Http,
        hook: None,
        hook_env: Default::default(),
        staging: false,
        auto_renew: false,
        updated_at: chrono::Utc::now(),
    };
    input.apply(&mut spec);
    super::certificates::create(State(state), Json(spec)).await?;
    Ok(Json(ApiMessage::new("Certificate definition created")))
}

async fn update_certificate(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(input): Json<CertificateInput>,
) -> Result<Json<ApiMessage>, ApiError> {
    let mut spec = state
        .stores
        .certificates
        .read()
        .await
        .iter()
        .find(|spec| spec.id == id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("certificate not found"))?;
    input.apply(&mut spec);
    super::certificates::update(State(state), Path(id), Json(spec)).await?;
    Ok(Json(ApiMessage::new("Certificate definition updated")))
}

// Do not return command output: DNS hook diagnostics can contain credentials.
async fn issue_certificate(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiMessage>, ApiError> {
    let result = super::certificates::issue(State(state), Path(id))
        .await
        .map_err(|_| ApiError::conflict("Certificate issuance failed; inspect server logs"))?
        .0;
    if !result.success {
        return Err(ApiError::conflict("Certificate issuance failed; inspect server logs"));
    }
    Ok(Json(ApiMessage::new("Certificate issued")))
}

async fn renew_certificate(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiMessage>, ApiError> {
    let result = super::certificates::renew(State(state), Path(id))
        .await
        .map_err(|_| ApiError::conflict("Certificate renewal failed; inspect server logs"))?
        .0;
    if !result.success {
        return Err(ApiError::conflict("Certificate renewal failed; inspect server logs"));
    }
    Ok(Json(ApiMessage::new("Certificate renewed")))
}

async fn clear_certificate_lock(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    super::certificates::clear_dehydrated_lock(State(state.clone())).await?;
    certificate_lock(State(state)).await
}

async fn update_dehydrated(State(state): State<AppState>) -> Result<Json<ApiMessage>, ApiError> {
    super::certificates::update_dehydrated(State(state)).await?;
    Ok(Json(ApiMessage::new("Dehydrated updated")))
}

// Enforcement is opt-in through the root-owned helper policy.
async fn firewall_capability() -> Json<Value> {
    let probe = admin::probe().await;
    Json(json!({"enforcement":probe.ban_shield,"reason":if probe.ban_shield {
        "Authorized nftables enforcement"
    } else {
        "Install nftables and authorize ban_shield in the root-owned helper policy"
    }}))
}
async fn update_firewall(
    State(state): State<AppState>,
    Json(policy): Json<FirewallPolicy>,
) -> Result<Json<FirewallPolicy>, ApiError> {
    if policy.enabled && !policy.observe_only && !admin::probe().await.ban_shield {
        return Err(ApiError::bad_request("Ban Shield enforcement requires the authorized nftables helper"));
    }
    firewall::update_policy(State(state), Json(policy)).await
}

const WG_UNIT: &str = "wg-quick@wg0.service";

async fn wireguard_status(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let probe = admin::probe().await;
    let result = state
        .runner
        .run(
            &state.config.commands.ip,
            ["link", "show", "dev", "wg0"],
            std::time::Duration::from_secs(5),
        )
        .await?;
    Ok(Json(json!({
        "interface": "wg0",
        "is_running": result.success,
        "writable": probe.permits_unit(WG_UNIT),
        "peers_writable": probe.wireguard_peers,
        "reason": if probe.permits_unit(WG_UNIT) { "Authorized system service" } else { "Authorize wg-quick@wg0.service in the root-owned helper policy" },
    })))
}

async fn wireguard_peers() -> Result<Json<Value>, ApiError> {
    if !admin::probe().await.wireguard_peers {
        return Err(ApiError::bad_request("WireGuard peer access requires the root-owned wireguard_peers grant"));
    }
    Ok(Json(admin::network_operation("wireguard_peers_read", json!({})).await.map_err(ApiError::conflict)?))
}

async fn write_wireguard_peers(Json(input): Json<Value>) -> Result<Json<Value>, ApiError> {
    if !admin::probe().await.wireguard_peers {
        return Err(ApiError::bad_request("WireGuard peer editing requires the root-owned wireguard_peers grant"));
    }
    Ok(Json(admin::network_operation("wireguard_peers_write", input).await.map_err(ApiError::conflict)?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireguardToggle {
    enable: bool,
}

async fn wireguard_toggle(
    Json(input): Json<WireguardToggle>,
) -> Result<Json<ApiMessage>, ApiError> {
    // Never pass browser-selected configuration paths or hooks to a root command.
    admin::system_service(WG_UNIT, if input.enable { "start" } else { "stop" })
        .await
        .map_err(ApiError::conflict)?;
    Ok(Json(ApiMessage::new("WireGuard service action completed")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_input_rejects_hook_fields() {
        let input = json!({
            "name": "example", "domains": ["example.test"], "method": "dns",
            "staging": true, "auto_renew": false, "hook_env": {"TOKEN": "secret"},
        });
        assert!(serde_json::from_value::<CertificateInput>(input).is_err());
    }

    #[test]
    fn wireguard_toggle_rejects_arbitrary_units_and_paths() {
        assert!(serde_json::from_value::<WireguardToggle>(
            json!({"enable": true, "unit": "other.service"})
        ).is_err());
        assert!(serde_json::from_value::<WireguardToggle>(
            json!({"enable": true, "config": "/tmp/wg.conf"})
        ).is_err());
        assert!(serde_json::from_value::<WireguardToggle>(json!({"enable": true})).is_ok());
    }
}
