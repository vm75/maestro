//! Init-script service adapter shared by ASUS Entware and Phase 1 Linux mode.
use std::path::{Path, PathBuf};

use crate::config::{AppConfig, Platform};

pub trait ServiceManager {
    fn init_directory<'a>(&self, config: &'a AppConfig) -> &'a Path;
    fn log_directories<'a>(&self, config: &'a AppConfig) -> &'a [PathBuf];
}

pub struct InitScriptServiceManager;

impl ServiceManager for InitScriptServiceManager {
    fn init_directory<'a>(&self, config: &'a AppConfig) -> &'a Path {
        &config.services.init_dir
    }

    fn log_directories<'a>(&self, config: &'a AppConfig) -> &'a [PathBuf] {
        &config.services.log_dirs
    }
}

/// Linux continues using existing compatible scripts; native systemd is Phase 2.
pub fn for_platform(_platform: Platform) -> InitScriptServiceManager {
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
            assert_eq!(manager.init_directory(&config), config.services.init_dir.as_path());
            assert_eq!(manager.log_directories(&config), config.services.log_dirs.as_slice());
        }
    }
}
