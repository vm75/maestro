//! Typed ingress inputs and the shared effective access resolver.
//! Source policies are checked in the rewrite phase, before proxying/redirects.
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
};

use anyhow::{Result, bail};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    models::NginxObjectKind,
    nginx_render::{GlobalSettings, Settings},
    nginx_sites::Sites,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub address: IpAddr,
    pub port: u16,
}

impl Endpoint {
    pub fn socket(&self) -> String {
        std::net::SocketAddr::new(self.address, self.port).to_string()
    }

    fn overlaps(&self, other: &Self) -> bool {
        self.port == other.port
            && self.address.is_ipv4() == other.address.is_ipv4()
            && (self.address == other.address
                || self.address.is_unspecified()
                || other.address.is_unspecified())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ListenerProfile {
    pub https: Vec<Endpoint>,
    pub http: Vec<Endpoint>,
    pub http_redirect: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccessPolicy {
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpMode {
    Off,
    #[default]
    RenewalOnly,
    Always,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpAccess {
    pub mode: HttpMode,
    pub window_seconds: u64,
    pub listener_profile: String,
    pub wan: Option<WanAccess>,
    pub externally_managed_wan: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WanAccess {
    pub interface: String,
    pub target: std::net::Ipv4Addr,
}

impl Default for HttpAccess {
    fn default() -> Self {
        Self {
            mode: HttpMode::RenewalOnly,
            window_seconds: 3600,
            listener_profile: "legacy".into(),
            wan: None,
            externally_managed_wan: false,
        }
    }
}

pub fn name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

pub fn network(value: &str) -> Result<IpNet> {
    Ok(value
        .parse::<IpNet>()
        .or_else(|_| value.parse::<IpAddr>().map(IpNet::from))?
        .trunc())
}

pub fn mode(global: &GlobalSettings) -> HttpMode {
    global
        .http_access
        .as_ref()
        .map_or(HttpMode::Always, |access| access.mode)
}

pub fn profiles(global: &GlobalSettings) -> BTreeMap<String, ListenerProfile> {
    let address = global
        .shared
        .get("router_ip")
        .and_then(Value::as_str)
        .unwrap_or("127.0.0.1")
        .parse()
        .expect("validated listener address");
    let endpoint = |key, fallback| Endpoint {
        address,
        port: global
            .shared
            .get(key)
            .and_then(Value::as_u64)
            .unwrap_or(fallback) as u16,
    };
    let mut result = global.listener_profiles.clone();
    result.insert(
        "legacy".into(),
        ListenerProfile {
            https: vec![endpoint("https_port", 443)],
            http: vec![endpoint("http_port", 80)],
            http_redirect: true,
        },
    );
    result
}

pub fn validate(global: &GlobalSettings) -> Result<()> {
    if global.listener_profiles.len() > 16 || global.access_policies.len() > 32 {
        bail!("at most 16 listener profiles and 32 access policies are allowed");
    }
    if global.listener_profiles.contains_key("legacy") {
        bail!("legacy listener is configured through shared listener settings");
    }
    if let Some(access) = &global.http_access {
        if !(60..=86400).contains(&access.window_seconds) {
            bail!("HTTP validation window must be 60..86400 seconds");
        }
        let profiles = profiles(global);
        let profile = profiles.get(&access.listener_profile).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown HTTP challenge listener: {}",
                access.listener_profile
            )
        })?;
        if access.mode == HttpMode::RenewalOnly
            && (profile.http.is_empty()
                || profile
                    .http
                    .iter()
                    .map(|e| e.port)
                    .collect::<BTreeSet<_>>()
                    .len()
                    != 1)
        {
            bail!("HTTP validation listener requires HTTP endpoints using one internal port");
        }
        if let Some(wan) = &access.wan {
            if access.mode == HttpMode::Always {
                bail!(
                    "always-on HTTP uses externally managed WAN forwarding; remove renewal-only WAN adapter settings"
                );
            }
            if wan.interface.is_empty()
                || wan.interface.len() > 15
                || !wan
                    .interface
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
                || wan.target.is_unspecified()
                || wan.target.is_loopback()
                || wan.target.is_multicast()
                || wan.target.is_broadcast()
            {
                bail!("WAN HTTP control requires a literal interface and non-loopback IPv4 target");
            }
            if !profile.http.iter().any(|endpoint| {
                endpoint.address == IpAddr::V4(wan.target)
                    || endpoint.address == IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
            }) {
                bail!("WAN HTTP target must match a configured challenge listener address");
            }
            if access.externally_managed_wan {
                bail!("choose managed or externally managed HTTP WAN access");
            }
            if profile
                .http
                .iter()
                .any(|endpoint| endpoint.address.is_ipv6())
            {
                bail!(
                    "managed WAN adapter supports IPv4 challenge listeners only; IPv6 WAN TCP 80 is blocked"
                );
            }
        }
    }
    let mut sockets: Vec<(String, Endpoint)> = Vec::new();
    for (name, profile) in profiles(global) {
        if !name_valid(&name)
            || profile.https.is_empty()
            || profile.https.len() > 8
            || profile.http.len() > 8
        {
            bail!("invalid listener profile: {name}");
        }
        if profile.http_redirect
            && (profile.http.is_empty()
                || profile
                    .https
                    .iter()
                    .map(|e| e.port)
                    .collect::<BTreeSet<_>>()
                    .len()
                    != 1)
        {
            bail!("listener {name}: redirects require HTTP endpoints and one HTTPS target port");
        }
        for endpoint in profile.https.iter().chain(&profile.http) {
            if endpoint.port == 0 {
                bail!("listener {name}: port must be 1..65535");
            }
            for (previous, socket) in &sockets {
                if endpoint.overlaps(socket) {
                    bail!(
                        "listener {name} endpoint {} conflicts with {previous} ({})",
                        endpoint.socket(),
                        socket.socket()
                    );
                }
            }
            sockets.push((name.clone(), endpoint.clone()));
        }
    }
    let mut policy_names = BTreeSet::new();
    for (name, policy) in &global.access_policies {
        if !name_valid(name)
            || ["public", "deny", "legacy"].contains(&name.to_ascii_lowercase().as_str())
            || policy.allow.len() > 256
            || !policy_names.insert(name.to_ascii_lowercase())
        {
            bail!("invalid access policy: {name}");
        }
        for address in &policy.allow {
            network(address)?;
        }
    }
    references(global, &global.site_defaults)?;
    Ok(())
}

pub fn references(global: &GlobalSettings, settings: &Settings) -> Result<()> {
    if settings.contains_key("access_policy") && settings.contains_key("enable_lanonly") {
        bail!("choose access_policy or legacy enable_lanonly at the same scope");
    }
    if let Some(name) = settings.get("listener_profile").and_then(Value::as_str) {
        if name != "legacy" && !global.listener_profiles.contains_key(name) {
            bail!("unknown listener_profile: {name}");
        }
    }
    for key in ["access_policy", "restricted_sessions_policy"] {
        if let Some(name) = settings.get(key).and_then(Value::as_str) {
            policy(global, name)?;
        }
    }
    for key in ["routes", "children"] {
        if let Some(routes) = settings.get(key).and_then(Value::as_array) {
            for route in routes {
                references(global, route.as_object().expect("validated route"))?;
            }
        }
    }
    Ok(())
}

pub fn merge(global: &GlobalSettings, settings: &Settings) -> Settings {
    let mut result = global.site_defaults.clone();
    if settings.contains_key("access_policy") {
        result.remove("enable_lanonly");
    }
    if settings.contains_key("enable_lanonly") {
        result.remove("access_policy");
    }
    result.extend(settings.clone());
    result
}

pub fn listener(settings: &Settings) -> &str {
    settings
        .get("listener_profile")
        .and_then(Value::as_str)
        .unwrap_or("legacy")
}

pub fn policy_name(settings: &Settings, inherited: &str) -> String {
    settings
        .get("access_policy")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            settings
                .get("enable_lanonly")
                .and_then(Value::as_bool)
                .map(|restricted| if restricted { "legacy" } else { "public" }.to_string())
        })
        .unwrap_or_else(|| inherited.to_owned())
}

/// None means no restriction; Some([]) means deny all.
pub fn policy(global: &GlobalSettings, name: &str) -> Result<Option<Vec<IpNet>>> {
    let allow = match name {
        "public" => return Ok(None),
        "deny" => return Ok(Some(Vec::new())),
        "legacy" => &global.allowed_ips,
        name => {
            &global
                .access_policies
                .get(name)
                .ok_or_else(|| anyhow::anyhow!("unknown access_policy: {name}"))?
                .allow
        }
    };
    Ok(Some(
        allow.iter().map(|ip| network(ip)).collect::<Result<_>>()?,
    ))
}

pub fn intersection(a: Option<Vec<IpNet>>, b: Option<Vec<IpNet>>) -> Option<Vec<IpNet>> {
    match (a, b) {
        (None, other) | (other, None) => other,
        (Some(a), Some(b)) => {
            let mut result = BTreeSet::new();
            for left in a {
                for right in &b {
                    if left.contains(right) {
                        result.insert(*right);
                    } else if right.contains(&left) {
                        result.insert(left);
                    }
                }
            }
            Some(result.into_iter().collect())
        }
    }
}

fn checks(names: &[String]) -> String {
    names
        .iter()
        .filter(|name| name.as_str() != "public")
        .map(|name| {
            if name == "deny" {
                "return 403;\n".into()
            } else {
                format!("if ($rh_policy_{name} = 0) {{ return 403; }}\n")
            }
        })
        .collect()
}

fn annotate_routes(
    global: &GlobalSettings,
    routes: &mut Value,
    ancestors: &[String],
) -> Result<()> {
    let Some(routes) = routes.as_array_mut() else {
        return Ok(());
    };
    for route in routes {
        let route = route.as_object_mut().expect("validated route");
        let mut names = ancestors.to_vec();
        let name = policy_name(route, ancestors.last().map_or("legacy", String::as_str));
        if !names.contains(&name) {
            names.push(name);
        }
        route.insert("_rh_access_checks".into(), checks(&names).into());
        let deny = names.iter().any(|name| {
            policy(global, name).is_ok_and(|ips| ips.is_some_and(|ips| ips.is_empty()))
        });
        route.insert("_rh_deny".into(), deny.into());
        if let Some(children) = route.get_mut("children") {
            annotate_routes(global, children, &names)?;
        }
    }
    Ok(())
}

fn uses_policies(settings: &Settings) -> bool {
    settings.contains_key("access_policy")
        || settings.contains_key("restricted_sessions_policy")
        || ["routes", "children"].iter().any(|key| {
            settings
                .get(*key)
                .and_then(Value::as_array)
                .is_some_and(|routes| {
                    routes
                        .iter()
                        .any(|r| uses_policies(r.as_object().expect("validated route")))
                })
        })
}

pub fn context(
    global: &GlobalSettings,
    settings: &Settings,
    kind: NginxObjectKind,
    names: &[String],
) -> Result<Settings> {
    references(global, settings)?;
    let mut context = merge(global, settings);
    let selected = listener(&context).to_owned();
    if kind != NginxObjectKind::Subfolder {
        context.insert("_rh_check_ingress".into(), true.into());
    }
    if kind == NginxObjectKind::Subfolder && settings.contains_key("listener_profile") {
        bail!("subfolders inherit their domain listener");
    }
    if selected != "legacy" && kind != NginxObjectKind::Subfolder {
        let profile = &global.listener_profiles[&selected];
        let mut directives = String::new();
        for endpoint in &profile.https {
            directives.push_str("listen ");
            directives.push_str(&endpoint.socket());
            directives.push_str(" ssl;\n");
        }
        context.insert("_rh_listens".into(), directives.into());
    }
    // Validate Host even when TLS selected this server using a different SNI.
    // Normalized hostnames contain only literals, wildcard labels or '_'.
    if selected != "legacy" || uses_policies(&context) {
        if names.iter().any(|name| {
            !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._*-".contains(&c))
        }) {
            bail!("managed ingress hostnames must be literal DNS names or wildcard labels");
        }
        let alternatives = names
            .iter()
            .map(|name| name.replace('.', "[.]").replace('*', "[^.]+"))
            .collect::<Vec<_>>()
            .join("|");
        if kind != NginxObjectKind::Subfolder {
            context.insert(
                "_rh_host_check".into(),
                format!("if ($host !~* \"^(?:{alternatives})$\") {{ return 444; }}").into(),
            );
        }
    }
    if uses_policies(&context) {
        let name = policy_name(&context, "legacy");
        context.insert(
            "_rh_access_checks".into(),
            checks(std::slice::from_ref(&name)).into(),
        );
        context.insert(
            "_rh_deny".into(),
            policy(global, &name)?
                .is_some_and(|ips| ips.is_empty())
                .into(),
        );
        let mut routes = expanded_routes(&context);
        context.insert("routes".into(), routes.clone());
        validate_internal_targets(global, &context, true)?;
        if context
            .get("enable_restricted_sessions")
            .and_then(Value::as_bool)
            == Some(true)
        {
            context.insert("enable_restricted_sessions".into(), false.into());
        }
        annotate_routes(global, &mut routes, &[name])?;
        context.insert("routes".into(), routes);
    } else if context
        .get("enable_restricted_sessions")
        .and_then(Value::as_bool)
        == Some(true)
    {
        // Keep legacy rendering, but validate against the same generated location.
        let mut expanded = context.clone();
        expanded.insert("routes".into(), expanded_routes(&context));
        validate_internal_targets(global, &expanded, false)?;
    }
    Ok(context)
}

fn expanded_routes(settings: &Settings) -> Value {
    let mut routes = settings.get("routes").cloned().unwrap_or(json!([]));
    if settings
        .get("enable_restricted_sessions")
        .and_then(Value::as_bool)
        == Some(true)
    {
        let policy = settings
            .get("restricted_sessions_policy")
            .and_then(Value::as_str)
            .unwrap_or("legacy");
        routes.as_array_mut().expect("validated routes").push(json!({
            "path": "/Sessions", "access_policy": policy, "access_log": false, "spa_fallback": ""
        }));
    }
    routes
}

fn source_subset(source: &Option<Vec<IpNet>>, target: &Option<Vec<IpNet>>) -> bool {
    match (source, target) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(source), Some(target)) => source
            .iter()
            .all(|source| target.iter().any(|target| target.contains(source))),
    }
}

/// A rewrite with `break` or named SPA retry bypasses normal location selection.
/// Refuse static targets that would evade another route's stricter source gate.
fn validate_internal_targets(
    global: &GlobalSettings,
    settings: &Settings,
    cumulative: bool,
) -> Result<()> {
    fn collect(
        global: &GlobalSettings,
        settings: &Settings,
        inherited: &str,
        parent: Option<Vec<IpNet>>,
        cumulative: bool,
        nodes: &mut Vec<(Settings, Option<Vec<IpNet>>)>,
    ) -> Result<()> {
        let name = policy_name(settings, inherited);
        let sources = policy(global, &name)?;
        let sources = if cumulative {
            intersection(parent, sources)
        } else {
            sources
        };
        nodes.push((settings.clone(), sources.clone()));
        for key in ["routes", "children"] {
            if let Some(routes) = settings.get(key).and_then(Value::as_array) {
                for route in routes {
                    collect(
                        global,
                        route.as_object().unwrap(),
                        &name,
                        sources.clone(),
                        cumulative,
                        nodes,
                    )?;
                }
            }
        }
        Ok(())
    }
    let mut nodes = Vec::new();
    collect(global, settings, "legacy", None, cumulative, &mut nodes)?;
    for (source, sources) in &nodes {
        for key in ["rewrite_to", "spa_fallback"] {
            let Some(target) = source
                .get(key)
                .and_then(Value::as_str)
                .filter(|target| !target.is_empty())
            else {
                continue;
            };
            for (destination, allowed) in nodes.iter().skip(1) {
                if source_subset(sources, allowed) {
                    continue;
                }
                let path = destination["path"].as_str().unwrap();
                let matches = match destination
                    .get("match")
                    .and_then(Value::as_str)
                    .unwrap_or("prefix")
                {
                    "exact" => target == path,
                    "prefix" => target.starts_with(path),
                    kind => regex::RegexBuilder::new(&format!("^{path}"))
                        .case_insensitive(kind == "regex_i")
                        .build()
                        .map_err(|_| {
                            anyhow::anyhow!(
                                "cannot verify {key} against advanced restricted regex route {path}"
                            )
                        })?
                        .is_match(target),
                };
                if matches {
                    bail!("{key} target {target} bypasses stricter access on route {path}");
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectiveAccess {
    pub listener_profile: String,
    pub https: Vec<Endpoint>,
    pub http: Vec<Endpoint>,
    pub access_policy: String,
    pub allowed_sources: Option<Vec<IpNet>>,
    pub inherited_from: Option<String>,
    pub routes: Vec<RouteAccess>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteAccess {
    pub path: String,
    pub access_policy: String,
    pub allowed_sources: Option<Vec<IpNet>>,
}

fn route_summaries(
    global: &GlobalSettings,
    settings: &Settings,
    inherited: &str,
    sources: Option<Vec<IpNet>>,
    output: &mut Vec<RouteAccess>,
) -> Result<()> {
    for key in ["routes", "children"] {
        if let Some(routes) = settings.get(key).and_then(Value::as_array) {
            for route in routes {
                let route = route.as_object().expect("validated route");
                let name = policy_name(route, inherited);
                let effective = intersection(sources.clone(), policy(global, &name)?);
                output.push(RouteAccess {
                    path: route["path"].as_str().unwrap().into(),
                    access_policy: name.clone(),
                    allowed_sources: effective.clone(),
                });
                route_summaries(global, route, &name, effective, output)?;
            }
        }
    }
    Ok(())
}

pub fn effective(
    global: &GlobalSettings,
    settings: &Settings,
    parent: Option<(&str, &Settings)>,
) -> Result<EffectiveAccess> {
    references(global, settings)?;
    let merged = merge(global, settings);
    let name = policy_name(&merged, "legacy");
    let mut sources = policy(global, &name)?;
    let (profile, inherited_from) = if let Some((parent_name, parent)) = parent {
        let parent = merge(global, parent);
        sources = intersection(sources, policy(global, &policy_name(&parent, "legacy"))?);
        (listener(&parent).to_string(), Some(parent_name.into()))
    } else {
        (listener(&merged).to_string(), None)
    };
    let profiles = profiles(global);
    let selected = profiles
        .get(&profile)
        .ok_or_else(|| anyhow::anyhow!("unknown listener_profile: {profile}"))?;
    let mut routes = Vec::new();
    let mut expanded = merged;
    expanded.insert("routes".into(), expanded_routes(&expanded));
    route_summaries(global, &expanded, &name, sources.clone(), &mut routes)?;
    Ok(EffectiveAccess {
        listener_profile: profile,
        https: selected.https.clone(),
        http: if mode(global) == HttpMode::Always && selected.http_redirect {
            selected.http.clone()
        } else {
            vec![]
        },
        access_policy: name,
        allowed_sources: sources,
        inherited_from,
        routes,
    })
}

pub fn validate_tree(global: &GlobalSettings, sites: &Sites) -> Result<()> {
    let mut hosts = BTreeMap::new();
    let mut upstreams = BTreeMap::new();
    for (key, site) in &sites.sites {
        references(global, &site.settings)
            .map_err(|error| anyhow::anyhow!("site {key}: {error}"))?;
        if site.kind == NginxObjectKind::Subfolder {
            if site.settings.contains_key("listener_profile") {
                bail!("site {key}: subfolders inherit their listener");
            }
            continue;
        }
        let settings = merge(global, &site.settings);
        let profile = listener(&settings);
        let names = crate::nginx::normalize_server_names(
            site.kind,
            &site.domain,
            &site.object_name(key)?,
            &site
                .server_name
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        )?;
        for host in names {
            let host = host.to_ascii_lowercase();
            if let Some(previous) = hosts.insert((profile.to_owned(), host.clone()), key) {
                bail!("hostname {host} on listener {profile} belongs to both {previous} and {key}");
            }
            if let Some((previous_key, previous_upstream)) =
                upstreams.insert(host.clone(), (key, &site.upstream))
            {
                if previous_upstream != &site.upstream {
                    bail!(
                        "hostname {host} has different global upstreams in {previous_key} and {key}"
                    );
                }
            }
        }
    }
    Ok(())
}

pub fn geo_output(global: &GlobalSettings) -> Result<String> {
    let mut output = String::from("# Managed source policies. Never edit generated output.\n");
    for name in std::iter::once("legacy").chain(global.access_policies.keys().map(String::as_str)) {
        output.push_str(&format!("geo $rh_policy_{name} {{\n    default 0;\n"));
        for net in policy(global, name)?
            .unwrap_or_default()
            .into_iter()
            .collect::<BTreeSet<_>>()
        {
            output.push_str(&format!("    {net} 1;\n"));
        }
        output.push_str("}\n");
    }
    Ok(output)
}

fn exact_host_profiles(
    config: &crate::config::AppConfig,
    global: &GlobalSettings,
    sites: &Sites,
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut hosts: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (key, site) in &sites.sites {
        if site.kind == NginxObjectKind::Subfolder {
            continue;
        }
        let names = site
            .server_name
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let settings = merge(global, &site.settings);
        let name = site.object_name(key)?;
        let active = crate::nginx::symlink_exists(&crate::nginx::enabled_path(
            config,
            site.kind,
            &site.domain,
            &name,
        )?) && (site.kind != NginxObjectKind::Subdomain
            || crate::nginx::symlink_exists(&config.nginx.domains_enabled_dir.join(&site.domain)));
        for host in crate::nginx::normalize_server_names(site.kind, &site.domain, &name, &names)? {
            let host = host.trim_end_matches('.');
            if !host.is_empty()
                && !host.starts_with('.')
                && host
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            {
                let profiles = hosts.entry(host.to_ascii_lowercase()).or_default();
                if active {
                    profiles.insert(listener(&settings).to_owned());
                }
            }
        }
    }
    Ok(hosts)
}

fn host_scope_output(
    profiles: &BTreeMap<String, ListenerProfile>,
    hosts: &BTreeMap<String, BTreeSet<String>>,
) -> String {
    let mut output =
        String::from("map \"$server_addr/$server_port\" $rh_ingress_profile {\n default '';\n");
    let profile_ids = profiles
        .keys()
        .enumerate()
        .map(|(id, name)| (name, id))
        .collect::<BTreeMap<_, _>>();
    for (name, profile) in profiles {
        let id = profile_ids[name];
        for endpoint in &profile.https {
            let port = endpoint.port;
            let address = endpoint.address;
            if address.is_unspecified() {
                let pattern = if address.is_ipv4() {
                    "[0-9.]+"
                } else {
                    "[0-9a-f:]+"
                };
                output.push_str(&format!(" ~*^{pattern}/{port}$ {id};\n"));
            } else {
                output.push_str(&format!(" {address}/{port} {id};\n"));
            }
        }
    }
    output.push_str("}\nmap \"$host/\" $rh_managed_host {\n default 0;\n");
    for host in hosts.keys() {
        output.push_str(&format!(" {host}/ 1;\n"));
    }
    output.push_str("}\nmap \"$rh_ingress_profile/$host\" $rh_ingress_member {\n default 0;\n");
    for (host, profiles) in hosts {
        for profile in profiles {
            output.push_str(&format!(" {}/{host} 1;\n", profile_ids[profile]));
        }
    }
    output.push_str("}\nmap \"$rh_managed_host/$rh_ingress_member\" $rh_wrong_ingress {\n default 0;\n 1/0 1;\n}\n");
    output
}

pub(crate) fn scope_upstream_map(content: &str, kind: NginxObjectKind) -> String {
    let variable = match kind {
        NginxObjectKind::Domain => "domain_upstream",
        NginxObjectKind::Subdomain => "subdomain_upstream",
        NginxObjectKind::Subfolder => unreachable!("subfolder maps are scoped by their server"),
    };
    let raw = format!("rh_raw_{variable}");
    let content = content.replacen(
        &format!("map $host ${variable} {{"),
        &format!("map $host ${raw} {{"),
        1,
    );
    format!("{content}\nmap $rh_wrong_ingress ${variable} {{\n default ${raw};\n 1 '';\n}}\n")
}

/// Rebuild a fixed set of files, so removed profiles never leave stale includes.
pub fn runtime_files(
    config: &crate::config::AppConfig,
    global: &GlobalSettings,
) -> Result<BTreeMap<std::path::PathBuf, String>> {
    let profiles = profiles(global);
    let mode = mode(global);
    let var_dir = global
        .shared
        .get("var_dir")
        .and_then(Value::as_str)
        .unwrap_or("/opt/var");
    let mut defaults = String::from("# Managed ingress defaults.\n");
    for profile in profiles.values() {
        for endpoint in &profile.https {
            defaults.push_str(&format!("server {{\n listen {} ssl default_server;\n server_name _;\n ssl_reject_handshake on;\n access_log {var_dir}/log/nginx/unknown-host.log f2b_trap;\n return 444;\n}}\n", endpoint.socket()));
        }
        if mode == HttpMode::Always {
            for endpoint in &profile.http {
                defaults.push_str(&format!(
                    "server {{\n listen {} default_server;\n server_name _;\n access_log {var_dir}/log/nginx/unknown-host.log f2b_trap;\n return 444;\n}}\n",
                    endpoint.socket()
                ));
            }
        }
    }
    let sites = crate::nginx_sites::load(config)?;
    // Reconciliation must be able to close HTTP even when an existing tree
    // has hostname collisions. Site and settings writes validate the proposal.
    for (key, site) in &sites.sites {
        references(global, &site.settings)
            .map_err(|error| anyhow::anyhow!("site {key}: {error}"))?;
    }
    let host_profiles = exact_host_profiles(config, global, &sites)?;
    let mut redirects: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for object in crate::nginx::list_objects(config, false)? {
        if !object.enabled || object.kind == NginxObjectKind::Subfolder {
            continue;
        }
        if object.kind == NginxObjectKind::Subdomain
            && !crate::nginx::symlink_exists(&config.nginx.domains_enabled_dir.join(&object.domain))
        {
            continue;
        }
        let site = sites.sites.iter().find(|(key, site)| {
            site.kind == object.kind
                && site.domain == object.domain
                && site.object_name(key).is_ok_and(|name| name == object.name)
        });
        let profile = site
            .map(|(_, site)| listener(&merge(global, &site.settings)).to_string())
            .unwrap_or_else(|| "legacy".into());
        let content = crate::nginx::read_object(config, object.kind, &object.domain, &object.name)?;
        let names = crate::nginx::normalize_server_names(
            object.kind,
            &object.domain,
            &object.name,
            &crate::nginx::extract_server_names(&content),
        )?;
        redirects.entry(profile).or_default().extend(names);
    }
    let mut challenges = BTreeSet::new();
    let definitions = config.paths.data_dir.join("certificates.json");
    match std::fs::read_to_string(definitions) {
        Ok(content) => {
            let certificates: Vec<Value> = serde_json::from_str(&content)?;
            for certificate in certificates {
                if certificate.get("method").and_then(Value::as_str) != Some("http") {
                    continue;
                }
                if let Some(domains) = certificate.get("domains").and_then(Value::as_array) {
                    for domain in domains.iter().filter_map(Value::as_str) {
                        if !domain.starts_with("*.") {
                            if !domain
                                .bytes()
                                .all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c))
                            {
                                bail!("invalid HTTP challenge hostname");
                            }
                            challenges.insert(domain.to_ascii_lowercase());
                        }
                    }
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut http = String::from("# Managed HTTP forwarding and certificate validation.\n");
    if mode == HttpMode::Always {
        for (name, profile) in &profiles {
            if !profile.http_redirect || profile.http.is_empty() {
                continue;
            }
            let hosts = redirects.remove(name).unwrap_or_default();
            let mut challenge_only = BTreeSet::new();
            let mut blocked = BTreeSet::new();
            for (host, owners) in &host_profiles {
                if !owners.contains(name)
                    && !hosts.iter().any(|alias| alias.eq_ignore_ascii_case(host))
                {
                    blocked.insert(host.clone());
                }
            }
            if name == "legacy" {
                for host in &challenges {
                    if !hosts.iter().any(|alias| alias.eq_ignore_ascii_case(host)) {
                        blocked.remove(&host.to_ascii_lowercase());
                        challenge_only.insert(host.clone());
                    }
                }
            }
            http_server(&mut http, profile, blocked, false, "return 444;");
            http_server(&mut http, profile, challenge_only, true, "return 404;");
            let port = profile.https[0].port;
            let suffix = if port == 443 {
                String::new()
            } else {
                format!(":{port}")
            };
            http_server(
                &mut http,
                profile,
                hosts,
                true,
                &format!("return 301 https://$host{suffix}$request_uri;"),
            );
        }
    }
    if let Some((challenge_defaults, challenge_server)) =
        crate::certificate_http::challenge_output(config, global)?
    {
        defaults.push_str(&challenge_defaults);
        http.push_str(&challenge_server);
    }
    let data = BTreeMap::from([(
        "settings/free_subdomains.txt".to_owned(),
        global.free_subdomains.join("\n"),
    )]);
    let env = crate::nginx_render::environment(&data)?;
    let mut files = BTreeMap::from([
        (
            config.nginx.domain_upstream_map_path.clone(),
            scope_upstream_map(
                include_str!("../config/nginx/conf.d/02_domain_upstream_map.conf"),
                NginxObjectKind::Domain,
            ),
        ),
        (
            config.nginx.subdomain_upstream_map_path.clone(),
            scope_upstream_map(
                &env.get_template("conf.d/03_subdomain_upstream_map.conf.jinja")?
                    .render(())?,
                NginxObjectKind::Subdomain,
            ),
        ),
    ]);
    files.insert(
        config.nginx.root_dir.join("ban-shield/default-drop.conf"),
        defaults,
    );
    files.insert(config.nginx.http_forwarder_path.clone(), http);
    files.insert(
        config.nginx.root_dir.join("conf.d/00-ingress-access.conf"),
        geo_output(global)? + &host_scope_output(&profiles, &host_profiles),
    );
    files.insert(
        config.nginx.root_dir.join("snippets/tls-policy.conf"),
        include_str!("../config/nginx/snippets/ssl.conf.jinja")
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    Ok(files)
}

fn http_server(
    output: &mut String,
    profile: &ListenerProfile,
    hosts: BTreeSet<String>,
    challenge: bool,
    action: &str,
) {
    if hosts.is_empty() {
        return;
    }
    output.push_str("server {\n");
    for endpoint in &profile.http {
        output.push_str(&format!(" listen {};\n", endpoint.socket()));
    }
    output.push_str(&format!(
        " server_name {};\n include ban-shield/server-guard.conf;\n",
        hosts.into_iter().collect::<Vec<_>>().join(" ")
    ));
    if challenge {
        output.push_str(" include snippets/acme-challenge.conf;\n");
    }
    output.push_str(&format!(" location / {{ {action} }}\n}}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn intersections_never_broaden_either_family() {
        let a = Some(vec![
            network("192.0.2.0/24").unwrap(),
            network("2001:db8::/32").unwrap(),
        ]);
        let b = Some(vec![
            network("192.0.2.16/28").unwrap(),
            network("2001:db8:1::/48").unwrap(),
        ]);
        assert_eq!(intersection(a.clone(), b.clone()), b);
        assert_eq!(intersection(a.clone(), None), a);
        assert_eq!(
            intersection(a, Some(vec![network("198.51.100.0/24").unwrap()])),
            Some(vec![])
        );
    }

    #[test]
    fn wildcard_socket_conflicts_are_rejected() {
        let mut g = GlobalSettings::default();
        g.listener_profiles.insert(
            "other".into(),
            ListenerProfile {
                https: vec![Endpoint {
                    address: "0.0.0.0".parse().unwrap(),
                    port: 443,
                }],
                ..Default::default()
            },
        );
        assert!(g.validate().unwrap_err().to_string().contains("conflicts"));
    }

    #[test]
    fn ipv6_listeners_and_collection_limits_are_validated() {
        let mut global: GlobalSettings = serde_json::from_value(json!({
            "listener_profiles":{"v6":{"https":[{"address":"::1","port":19443}]}},
            "access_policies":{"empty":{"allow":[]}}
        }))
        .unwrap();
        global.validate().unwrap();
        let settings = json!({"listener_profile":"v6","access_policy":"empty"})
            .as_object()
            .unwrap()
            .clone();
        let output = crate::nginx_render::render_site(
            &global,
            &settings,
            NginxObjectKind::Subdomain,
            "example.test",
            "ipv6",
            &[],
        )
        .unwrap();
        assert!(output.contains("listen [::1]:19443 ssl"));
        assert!(output.contains("return 403"));
        let base = global.clone();
        global.access_policies.get_mut("empty").unwrap().allow = vec!["::1".into(); 257];
        assert!(global.validate().is_err());
        global = base.clone();
        global.listener_profiles.get_mut("v6").unwrap().https = (0..9)
            .map(|offset| Endpoint {
                address: "::1".parse().unwrap(),
                port: 19443 + offset,
            })
            .collect();
        assert!(global.validate().is_err());
        global = base.clone();
        for index in 0..16 {
            global.listener_profiles.insert(
                format!("profile_{index}"),
                ListenerProfile {
                    https: vec![Endpoint {
                        address: "127.0.0.1".parse().unwrap(),
                        port: 20000 + index,
                    }],
                    ..Default::default()
                },
            );
        }
        assert!(global.validate().is_err());
        global = base;
        for index in 0..32 {
            global
                .access_policies
                .insert(format!("rule_{index}"), AccessPolicy::default());
        }
        assert!(global.validate().is_err());
    }

    #[test]
    fn children_keep_ancestor_checks_even_with_public_override() {
        let mut g = GlobalSettings::default();
        g.access_policies.insert(
            "admins".into(),
            AccessPolicy {
                allow: vec!["192.0.2.4".into()],
            },
        );
        let settings = json!({"access_policy":"admins", "routes":[{"path":"/child", "access_policy":"public"}]}).as_object().unwrap().clone();
        let ctx = context(
            &g,
            &settings,
            NginxObjectKind::Subdomain,
            &["app.example.test".into()],
        )
        .unwrap();
        assert!(
            ctx["routes"][0]["_rh_access_checks"]
                .as_str()
                .unwrap()
                .contains("rh_policy_admins")
        );
    }
}
