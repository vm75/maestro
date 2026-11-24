//! Read-only shared module inventory for the origin-checked Linux session UI.
 //! Keep the existing bearer-authenticated router routes unchanged.
use axum::{Json, Router, extract::State, routing::get};
use serde_json::{Value, json};

use crate::state::AppState;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/shared/modules", get(super::modules))
        .route("/shared/nginx/status", get(super::nginx::status))
        .route("/shared/nginx/sites", get(super::nginx::list_objects))
        .route("/shared/nginx/settings", get(nginx_settings))
        .route("/shared/nginx/logs", get(super::nginx::list_logs))
        .route("/shared/nginx/logs/{*path}", get(super::nginx::get_log))
        .route("/shared/certificates", get(certificates))
        .route("/shared/certificates/http-access", get(super::certificates::http_access_status))
        .route("/shared/certificates/lock", get(certificate_lock))
        .route("/shared/firewall", get(super::firewall::status))
}

// Only the counts needed by the workspace are returned; global settings
// may contain private configuration values.
async fn nginx_settings(
    State(state): State<AppState>,
) -> Result<Json<Value>, super::ApiError> {
    let Json(settings) = super::nginx::get_settings(State(state)).await?;
    Ok(Json(json!({
        "listener_profiles": settings.listener_profiles.len(),
        "access_policies": settings.access_policies.len(),
        "limit_policies": settings.limit_policies.len(),
        "allowed_ips": settings.allowed_ips.len(),
    })))
}

// Never send DNS hook environment variables (which may contain credentials)
// to the session UI.
async fn certificates(State(state): State<AppState>) -> Json<Vec<Value>> {
    let Json(statuses) = super::certificates::list(State(state)).await;
    Json(statuses.into_iter().map(|status| {
        let spec = status.spec;
        json!({
            "id": spec.id, "name": spec.name, "domains": spec.domains,
            "method": spec.method, "staging": spec.staging,
            "auto_renew": spec.auto_renew, "exists": status.exists,
            "expires_at": status.expires_at, "days_remaining": status.days_remaining,
            "renewal_due": status.renewal_due,
        })
    }).collect())
}

async fn certificate_lock(
    State(state): State<AppState>,
) -> Result<Json<Value>, super::ApiError> {
    let Json(lock) = super::certificates::dehydrated_lock(State(state)).await?;
    Ok(Json(json!({ "locked": lock.locked, "status": lock.status })))
}
