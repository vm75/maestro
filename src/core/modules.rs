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
const SERVICES_UI: &[UiContribution] = &[UiContribution {
    tab: "services", tab_label: "Services", tab_order: 10,
    section: "services", section_label: "Services", section_order: 10,
    dashboard_widgets: &["services"],
}];
const NGINX_UI: &[UiContribution] = &[UiContribution {
    tab: "web", tab_label: "Web", tab_order: 20,
    section: "nginx", section_label: "Nginx", section_order: 10,
    dashboard_widgets: &["nginx"],
}];
const CERTIFICATES_UI: &[UiContribution] = &[UiContribution {
    tab: "web", tab_label: "Web", tab_order: 20,
    section: "certificates", section_label: "Certificates", section_order: 20,
    dashboard_widgets: &["certificates"],
}];
const FIREWALL_UI: &[UiContribution] = &[UiContribution {
    tab: "security", tab_label: "Security", tab_order: 30,
    section: "firewall", section_label: "Ban Shield", section_order: 10,
    dashboard_widgets: &["active_bans"],
}];
const WOL_UI: &[UiContribution] = &[UiContribution {
    tab: "network", tab_label: "Network", tab_order: 40,
    section: "wol", section_label: "Wake-on-LAN", section_order: 10,
    dashboard_widgets: &[],
}];
const ADGUARD_UI: &[UiContribution] = &[UiContribution {
    tab: "network", tab_label: "Network", tab_order: 40,
    section: "adguard", section_label: "AdGuard Home", section_order: 20,
    dashboard_widgets: &[],
}];
const WIREGUARD_UI: &[UiContribution] = &[UiContribution {
    tab: "network", tab_label: "Network", tab_order: 40,
    section: "wireguard", section_label: "WireGuard", section_order: 30,
    dashboard_widgets: &[],
}];

const MODULES: &[ModuleSpec] = &[
    ModuleSpec { id: "system", targets: BOTH, dependencies: &[], ui: &[] },
    ModuleSpec { id: "services", targets: BOTH, dependencies: &[], ui: SERVICES_UI },
    ModuleSpec { id: "nginx", targets: BOTH, dependencies: &[], ui: NGINX_UI },
    ModuleSpec { id: "certificates", targets: BOTH, dependencies: &[], ui: CERTIFICATES_UI },
    ModuleSpec { id: "ban_shield", targets: BOTH, dependencies: &[], ui: FIREWALL_UI },
    ModuleSpec { id: "wol", targets: BOTH, dependencies: &[], ui: WOL_UI },
    ModuleSpec { id: "adguard", targets: BOTH, dependencies: &[], ui: ADGUARD_UI },
    ModuleSpec { id: "wireguard", targets: BOTH, dependencies: &[], ui: WIREGUARD_UI },
    ModuleSpec { id: "asus_menu", targets: ROUTER, dependencies: &[], ui: &[] },
    ModuleSpec { id: "router_upgrade", targets: ROUTER, dependencies: &[], ui: &[] },
    ModuleSpec { id: "nginx_upgrade", targets: ROUTER, dependencies: &["nginx"], ui: &[] },
];

pub fn module_specs() -> &'static [ModuleSpec] {
    MODULES
}

/// Check IDs, platform dependencies and cycles before initializing the HTTP server.
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
                format!("module {} requires missing dependency {dependency}", spec.id)
            })?;
            for target in spec.targets {
                if !required.targets.contains(target) {
                    return Err(format!(
                        "module {} depends on unsupported {} for {target:?}", spec.id, dependency
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
        if complete.contains(id) { return Ok(()); }
        if !visiting.insert(id) { return Err(format!("cyclic module dependency at {id}")); }
        for dependency in by_id[id].dependencies {
            visit(dependency, by_id, visiting, complete)?;
        }
        visiting.remove(id);
        complete.insert(id);
        Ok(())
    }
    let mut visiting = BTreeSet::new();
    let mut complete = BTreeSet::new();
    for spec in specs { visit(spec.id, &by_id, &mut visiting, &mut complete)?; }
    Ok(())
}

/// Discovery is read-only and never reveals configured credentials or paths.
/// A "ready" module means it is configured for this target, not that its daemon is running.
pub fn discover(config: &AppConfig) -> Discovery {
    Discovery {
        platform: config.platform,
        modules: module_specs().iter().map(|spec| {
            let availability = if !spec.targets.contains(&config.platform) {
                Availability::Unsupported
            } else {
                match spec.id {
                    "asus_menu" if !config.asus_ui.enabled || config.test_mode => Availability::Disabled,
                    "router_upgrade" | "nginx_upgrade" if config.test_mode => Availability::Disabled,
                    "adguard" if !config.adguard.enabled => Availability::Disabled,
                    "services" if !config.services.init_dir.is_dir() => Availability::Unavailable,
                    "nginx" if !config.nginx.config_path.exists() => Availability::Unavailable,
                    _ => Availability::Ready,
                }
            };
            ModuleStatus { id: spec.id, availability, dependencies: spec.dependencies, ui: spec.ui }
        }).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_valid_for_both_platforms() {
        validate_registry(module_specs()).unwrap();
        for platform in BOTH {
            let config = AppConfig { platform: *platform, ..AppConfig::default() };
            let discovery = discover(&config);
            assert_eq!(discovery.modules.len(), MODULES.len());
            assert_eq!(
                discovery.modules.iter().find(|m| m.id == "router_upgrade").unwrap().availability,
                if *platform == Platform::Linux { Availability::Unsupported } else { Availability::Ready }
            );
            assert_eq!(
                discovery.modules.iter().find(|m| m.id == "nginx").unwrap().ui[0].tab,
                "web"
            );
        }
    }

    #[test]
    fn disabled_is_not_unsupported() {
        let config = AppConfig {
            adguard: crate::config::AdGuardConfig {
                enabled: false, ..Default::default()
            },
            ..AppConfig::default()
        };
        assert_eq!(discover(&config).modules.iter().find(|m| m.id == "adguard").unwrap().availability, Availability::Disabled);
        let mut linux = config;
        linux.platform = Platform::Linux;
        assert_eq!(discover(&linux).modules.iter().find(|m| m.id == "asus_menu").unwrap().availability, Availability::Unsupported);
    }

    #[test]
    fn duplicate_missing_dependency_and_cycle_are_rejected() {
        let one = ModuleSpec { id: "one", targets: BOTH, dependencies: &[], ui: &[] };
        assert!(validate_registry(&[one, one]).is_err());
        let missing = ModuleSpec { dependencies: &["unknown"], ..one };
        assert!(validate_registry(&[missing]).is_err());
        let a = ModuleSpec { id: "a", dependencies: &["b"], ..one };
        let b = ModuleSpec { id: "b", dependencies: &["a"], ..one };
        assert!(validate_registry(&[a, b]).is_err());
        let router_only = ModuleSpec { id: "router", targets: ROUTER, ..one };
        let cross_platform = ModuleSpec { id: "cross", dependencies: &["router"], ..one };
        assert!(validate_registry(&[router_only, cross_platform]).is_err());
    }

    #[test]
    fn discovery_serializes_status_and_ui() {
        let value = serde_json::to_value(discover(&AppConfig::default())).unwrap();
        assert_eq!(value["modules"][1]["id"], "services");
        assert_eq!(value["modules"][1]["ui"][0]["tab"], "services");
        assert_eq!(value["modules"][1]["ui"][0]["dashboard_widgets"][0], "services");
    }
}
