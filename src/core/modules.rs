//! Static feature discovery. This is metadata, not an authorization or plugin mechanism.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::config::{AppConfig, Platform};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Unsupported,
    Disabled,
    Unavailable,
    Ready,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct UiContribution {
    pub tab: &'static str,
    pub tab_label: &'static str,
    pub tab_order: u16,
    pub section: &'static str,
    pub section_label: &'static str,
    pub section_order: u16,
    pub dashboard_widgets: &'static [&'static str],
}

#[derive(Clone, Copy, Debug)]
pub struct ModuleSpec {
    pub id: &'static str,
    pub targets: &'static [Platform],
    pub dependencies: &'static [&'static str],
    pub ui: &'static [UiContribution],
}

#[derive(Debug, Serialize)]
pub struct ModuleStatus {
    pub id: &'static str,
    pub availability: Availability,
    pub dependencies: &'static [&'static str],
    pub ui: &'static [UiContribution],
}

#[derive(Debug, Serialize)]
pub struct Discovery {
    pub platform: Platform,
    pub modules: Vec<ModuleStatus>,
}

const BOTH: &[Platform] = &[Platform::Asuswrt, Platform::Linux];
const ROUTER: &[Platform] = &[Platform::Asuswrt];

const fn contribution(
    tab: &'static str,
    tab_label: &'static str,
    tab_order: u16,
    section: &'static str,
    section_label: &'static str,
    section_order: u16,
    dashboard_widgets: &'static [&'static str],
) -> UiContribution {
    UiContribution {
        tab,
        tab_label,
        tab_order,
        section,
        section_label,
        section_order,
        dashboard_widgets,
    }
}

const fn module(
    id: &'static str,
    targets: &'static [Platform],
    dependencies: &'static [&'static str],
    ui: &'static [UiContribution],
) -> ModuleSpec {
    ModuleSpec {
        id,
        targets,
        dependencies,
        ui,
    }
}

const SERVICES_UI: &[UiContribution] = &[contribution(
    "services",
    "Services",
    10,
    "services",
    "Services",
    10,
    &["services"],
)];
const NGINX_UI: &[UiContribution] = &[contribution(
    "web",
    "Web",
    20,
    "nginx",
    "Nginx",
    10,
    &["nginx"],
)];
const CERTIFICATES_UI: &[UiContribution] = &[contribution(
    "web",
    "Web",
    20,
    "certificates",
    "Certificates",
    20,
    &["certificates"],
)];
const FIREWALL_UI: &[UiContribution] = &[contribution(
    "security",
    "Security",
    30,
    "firewall",
    "Ban Shield",
    10,
    &["active_bans"],
)];
const WOL_UI: &[UiContribution] = &[contribution(
    "network",
    "Network",
    40,
    "wol",
    "Wake-on-LAN",
    10,
    &[],
)];
const ADGUARD_UI: &[UiContribution] = &[contribution(
    "network",
    "Network",
    40,
    "adguard",
    "AdGuard Home",
    20,
    &[],
)];
const WIREGUARD_UI: &[UiContribution] = &[contribution(
    "network",
    "Network",
    40,
    "wireguard",
    "WireGuard",
    30,
    &[],
)];

const MODULES: &[ModuleSpec] = &[
    module("system", BOTH, &[], &[]),
    module("services", BOTH, &[], SERVICES_UI),
    module("nginx", BOTH, &[], NGINX_UI),
    module("certificates", BOTH, &[], CERTIFICATES_UI),
    module("ban_shield", BOTH, &[], FIREWALL_UI),
    module("wol", BOTH, &[], WOL_UI),
    module("adguard", BOTH, &[], ADGUARD_UI),
    module("wireguard", BOTH, &[], WIREGUARD_UI),
    module("asus_menu", ROUTER, &[], &[]),
    module("router_upgrade", ROUTER, &[], &[]),
    module("nginx_upgrade", ROUTER, &["nginx"], &[]),
];

pub fn module_specs() -> &'static [ModuleSpec] {
    MODULES
}

/// Check IDs, platform dependencies, and cycles before initializing the HTTP server.
pub fn validate_registry(specs: &[ModuleSpec]) -> Result<(), String> {
    let mut by_id = BTreeMap::new();
    let mut sections = BTreeSet::new();
    let mut widgets = BTreeSet::new();
    for spec in specs {
        if spec.id.is_empty() || by_id.insert(spec.id, spec).is_some() {
            return Err(format!("empty or duplicate module id: {}", spec.id));
        }
        if spec.targets.is_empty() {
            return Err(format!("module {} has no target platform", spec.id));
        }
        for ui in spec.ui {
            if !sections.insert((ui.tab, ui.section)) {
                return Err(format!("duplicate UI section {}:{}", ui.tab, ui.section));
            }
            for widget in ui.dashboard_widgets {
                if !widgets.insert(*widget) {
                    return Err(format!("duplicate dashboard widget: {widget}"));
                }
            }
        }
    }

    for spec in specs {
        for dependency in spec.dependencies {
            let required = by_id.get(dependency).ok_or_else(|| {
                format!(
                    "module {} requires missing dependency {dependency}",
                    spec.id
                )
            })?;
            for target in spec.targets {
                if !required.targets.contains(target) {
                    return Err(format!(
                        "module {} depends on unsupported {} for {target:?}",
                        spec.id, dependency
                    ));
                }
            }
        }
    }

    fn visit(
        id: &'static str,
        by_id: &BTreeMap<&str, &ModuleSpec>,
        visiting: &mut BTreeSet<&'static str>,
        complete: &mut BTreeSet<&'static str>,
    ) -> Result<(), String> {
        if complete.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id) {
            return Err(format!("cyclic module dependency at {id}"));
        }
        for dependency in by_id[id].dependencies {
            visit(dependency, by_id, visiting, complete)?;
        }
        visiting.remove(id);
        complete.insert(id);
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut complete = BTreeSet::new();
    for spec in specs {
        visit(spec.id, &by_id, &mut visiting, &mut complete)?;
    }
    Ok(())
}

/// Readiness here means configured for the target, not a guarantee a daemon is running.
/// The response is read-only and never contains credentials or configured paths.
pub fn discover(config: &AppConfig) -> Discovery {
    let specs = module_specs();
    let mut states = BTreeMap::new();
    for spec in specs {
        let availability = if !spec.targets.contains(&config.platform) {
            Availability::Unsupported
        } else {
            match spec.id {
                "asus_menu" if !config.asus_ui.enabled || config.test_mode => {
                    Availability::Disabled
                }
                "router_upgrade" | "nginx_upgrade" if config.test_mode => Availability::Disabled,
                "adguard" if !config.adguard.enabled => Availability::Disabled,
                "services" if !config.services.init_dir.is_dir() => Availability::Unavailable,
                "nginx" if !config.nginx.config_path.exists() => Availability::Unavailable,
                _ => Availability::Ready,
            }
        };
        states.insert(spec.id, availability);
    }

    // Resolve dependencies independent of declaration order. The registry is
    // validated for missing IDs and cycles before the HTTP server starts.
    loop {
        let mut changed = false;
        for spec in specs {
            if states[spec.id] == Availability::Ready
                && spec
                    .dependencies
                    .iter()
                    .any(|dependency| states.get(dependency) != Some(&Availability::Ready))
            {
                states.insert(spec.id, Availability::Unavailable);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    Discovery {
        platform: config.platform,
        modules: specs
            .iter()
            .map(|spec| ModuleStatus {
                id: spec.id,
                availability: states[spec.id],
                dependencies: spec.dependencies,
                ui: spec.ui,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_valid_for_both_platforms() {
        validate_registry(module_specs()).unwrap();
        for platform in BOTH {
            let config = AppConfig {
                platform: *platform,
                ..AppConfig::default()
            };
            let discovery = discover(&config);
            assert_eq!(discovery.modules.len(), MODULES.len());
            assert_eq!(
                discovery
                    .modules
                    .iter()
                    .find(|module| module.id == "router_upgrade")
                    .unwrap()
                    .availability,
                if *platform == Platform::Linux {
                    Availability::Unsupported
                } else {
                    Availability::Ready
                }
            );
            assert_eq!(
                discovery
                    .modules
                    .iter()
                    .find(|module| module.id == "nginx")
                    .unwrap()
                    .ui[0]
                    .tab,
                "web"
            );
        }
    }

    #[test]
    fn disabled_is_not_unsupported() {
        let config = AppConfig {
            adguard: crate::config::AdGuardConfig {
                enabled: false,
                ..Default::default()
            },
            ..AppConfig::default()
        };
        assert_eq!(
            discover(&config)
                .modules
                .iter()
                .find(|module| module.id == "adguard")
                .unwrap()
                .availability,
            Availability::Disabled
        );
        let mut linux = config;
        linux.platform = Platform::Linux;
        assert_eq!(
            discover(&linux)
                .modules
                .iter()
                .find(|module| module.id == "asus_menu")
                .unwrap()
                .availability,
            Availability::Unsupported
        );
    }

    #[test]
    fn duplicate_missing_dependency_and_cycle_are_rejected() {
        let one = ModuleSpec {
            id: "one",
            targets: BOTH,
            dependencies: &[],
            ui: &[],
        };
        assert!(validate_registry(&[one, one]).is_err());
        let missing = ModuleSpec {
            dependencies: &["unknown"],
            ..one
        };
        assert!(validate_registry(&[missing]).is_err());
        let a = ModuleSpec {
            id: "a",
            dependencies: &["b"],
            ..one
        };
        let b = ModuleSpec {
            id: "b",
            dependencies: &["a"],
            ..one
        };
        assert!(validate_registry(&[a, b]).is_err());
        let router_only = ModuleSpec {
            id: "router",
            targets: ROUTER,
            ..one
        };
        let cross_platform = ModuleSpec {
            id: "cross",
            dependencies: &["router"],
            ..one
        };
        assert!(validate_registry(&[router_only, cross_platform]).is_err());
    }

    #[test]
    fn dependent_module_readiness_follows_nginx() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        config.nginx.config_path = temp.path().join("nginx.conf");

        let modules = discover(&config);
        assert_eq!(
            modules
                .modules
                .iter()
                .find(|module| module.id == "nginx_upgrade")
                .unwrap()
                .availability,
            Availability::Unavailable
        );

        std::fs::write(&config.nginx.config_path, "events {}").unwrap();
        let modules = discover(&config);
        assert_eq!(
            modules
                .modules
                .iter()
                .find(|module| module.id == "nginx_upgrade")
                .unwrap()
                .availability,
            Availability::Ready
        );

        config.platform = Platform::Linux;
        let modules = discover(&config);
        assert_eq!(
            modules
                .modules
                .iter()
                .find(|module| module.id == "nginx_upgrade")
                .unwrap()
                .availability,
            Availability::Unsupported
        );
    }

    #[test]
    fn discovery_serializes_status_and_ui() {
        let value = serde_json::to_value(discover(&AppConfig::default())).unwrap();
        assert_eq!(value["modules"][1]["id"], "services");
        assert_eq!(value["modules"][1]["ui"][0]["tab"], "services");
        assert_eq!(
            value["modules"][1]["ui"][0]["dashboard_widgets"][0],
            "services"
        );
    }
}
