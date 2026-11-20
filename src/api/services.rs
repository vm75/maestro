//! HTTP adapter for the platform-selected service manager.
//! Existing endpoints and response envelopes are preserved.

use axum::{
    Json,
    extract::{Path, State},
};

use crate::{
    api::ApiError,
    models::{ApiMessage, CommandResult, ServiceInfo},
    platforms::service_manager::{ServiceManager, for_platform},
    state::AppState,
};

pub use crate::platforms::init_scripts::collect_services;

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<ServiceInfo>>, ApiError> {
    let manager = for_platform(state.config.platform);
    Ok(Json(manager.inventory(&state).await?))
}

pub async fn action(
    State(state): State<AppState>,
    Path((name, action)): Path<(String, String)>,
) -> Result<Json<CommandResult>, ApiError> {
    let manager = for_platform(state.config.platform);
    Ok(Json(manager.act(state, name, action).await?))
}

pub async fn logs(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<ApiMessage>, ApiError> {
    let manager = for_platform(state.config.platform);
    Ok(Json(manager.logs(state, name).await?))
}
