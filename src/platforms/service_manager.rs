//! Service-manager boundary. Both Phase 1 targets retain the existing
//! init-script implementation; the Linux systemd provider is deferred to Phase 2.

use std::path::{Path, PathBuf};

use anyhow::Result as AnyResult;
use axum::extract::{Path as AxumPath, State};

use crate::{
    api::ApiError,
    config::{AppConfig, Platform},
    models::{ApiMessage, CommandResult, ServiceInfo},
    state::AppState,
};

pub(crate) trait ServiceManager {
    fn init_directory<'a>(&self, config: &'a AppConfig) -> &'a Path;
    fn log_directories<'a>(&self, config: &'a AppConfig) -> &'a [PathBuf];

    async fn inventory(&self, state: &AppState) -> AnyResult<Vec<ServiceInfo>>;
    async fn act(
        &self,
        state: AppState,
        name: String,
        action: String,
    ) -> Result<CommandResult, ApiError>;
    async fn logs(&self, state: AppState, name: String) -> Result<ApiMessage, ApiError>;
}

pub(crate) struct InitScriptServiceManager;

impl ServiceManager for InitScriptServiceManager {
    fn init_directory<'a>(&self, config: &'a AppConfig) -> &'a Path {
        &config.services.init_dir
    }

    fn log_directories<'a>(&self, config: &'a AppConfig) -> &'a [PathBuf] {
        &config.services.log_dirs
    }

    async fn inventory(&self, state: &AppState) -> AnyResult<Vec<ServiceInfo>> {
        crate::platforms::init_scripts::collect_services(state).await
    }

    async fn act(
        &self,
        state: AppState,
        name: String,
        action: String,
    ) -> Result<CommandResult, ApiError> {
        Ok(crate::platforms::init_scripts::action(
            State(state),
            AxumPath((name, action)),
        )
        .await?
        .0)
    }

    async fn logs(&self, state: AppState, name: String) -> Result<ApiMessage, ApiError> {
        Ok(crate::platforms::init_scripts::logs(State(state), AxumPath(name))
            .await?
            .0)
    }
}

/// Linux continues using compatible init scripts; no systemctl calls in Phase 1.
pub(crate) fn for_platform(_platform: Platform) -> InitScriptServiceManager {
    InitScriptServiceManager
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_platforms_use_configured_script_directories() {
        let config = AppConfig::default();
        for platform in [Platform::Asuswrt, Platform::Linux] {
            let manager = for_platform(platform);
            assert_eq!(
                manager.init_directory(&config),
                config.services.init_dir.as_path()
            );
            assert_eq!(
                manager.log_directories(&config),
                config.services.log_dirs.as_slice()
            );
        }
    }
}
