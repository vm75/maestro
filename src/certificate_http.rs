//! HTTP-01 windows. Persistent intent is separate from transient session state.
//! The optional IPv4 WAN adapter uses a kernel ipset timeout: its accept gate
//! expires even if Maestro dies. nginx/local listener cleanup still needs
//! startup recovery after an abrupt process death.
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf, time::Duration};

use crate::{
    config::AppConfig,
    models::CertificateSpec,
    nginx_ingress::{self, HttpMode, WanAccess},
    nginx_render::{self, GlobalSettings},
    state::AppState,
};

const INPUT_CHAIN: &str = "RH_ACME_INPUT";
const NAT_CHAIN: &str = "RH_ACME_NAT";
const FORWARD_CHAIN: &str = "RH_ACME_FORWARD";
const WINDOW_SET: &str = "rh_acme_window";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub names: BTreeSet<String>,
    pub expires_at: DateTime<Utc>,
    pub listener_profile: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub state: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub names: BTreeSet<String>,
    pub wan_control: String,
    pub error: Option<String>,
}

fn path(config: &AppConfig, name: &str) -> Result<PathBuf> {
    let path = config.http_state_dir().join(name);
    if config.platform == crate::config::Platform::Asuswrt {
        crate::nginx::ensure_no_symlink_ancestors(&config.paths.data_dir, &path)?;
    } else {
        nginx_render::ensure_managed_path(config, &path)?;
    }
    Ok(path)
}

pub fn active(config: &AppConfig) -> Result<Option<Session>> {
    let path = path(config, "http01-session.json")?;
    let session: Session = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("invalid HTTP validation session")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if session.names.is_empty()
        || session.names.len() > 256
        || !nginx_ingress::name_valid(&session.listener_profile)
        || session.expires_at > Utc::now() + chrono::Duration::days(1)
        || session.names.iter().any(|name| {
            name.is_empty()
                || name.len() > 253
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c))
        })
    {
        bail!("invalid HTTP session hostname");
    }
    Ok((session.expires_at > Utc::now()).then_some(session))
}

pub fn status(config: &AppConfig) -> Result<Status> {
    let global = nginx_render::load_global(config)?;
    let access = global.http_access.as_ref();
    let mut result = match std::fs::read(path(config, "http01-status.json")?) {
        Ok(bytes) => serde_json::from_slice::<Status>(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Status {
            state: if nginx_ingress::mode(&global) == HttpMode::Always {
                "always_on"
            } else {
                "unknown"
            }
            .into(),
            expires_at: None,
            names: BTreeSet::new(),
            wan_control: "unconfigured".into(),
            error: None,
        },
        Err(error) => return Err(error.into()),
    };
    result.wan_control = if access.is_some_and(|a| a.wan.is_some()) {
        "managed_ipv4_kernel_expiry_ipv6_blocked"
    } else if access.is_some_and(|a| a.externally_managed_wan) {
        "external"
    } else {
        "unconfigured"
    }
    .into();
    if nginx_ingress::mode(&global) == HttpMode::Always && result.state != "cleanup_failed" {
        result.state = "always_on".into();
        result.expires_at = None;
    }
    if result.state == "active"
        && result
            .expires_at
            .is_some_and(|deadline| deadline <= Utc::now())
    {
        result.state = "expired_cleanup_pending".into();
    }
    Ok(result)
}

fn record(state: &AppState, status: Status) -> Result<()> {
    crate::nginx::atomic_write(
        &path(&state.config, "http01-status.json")?,
        &serde_json::to_vec_pretty(&status)?,
    )
}

pub fn configured(state: &AppState, global: &GlobalSettings) -> Result<()> {
    if active(&state.config)?.is_some() {
        return Ok(());
    }
    record(
        state,
        Status {
            state: if nginx_ingress::mode(global) == HttpMode::Always {
                "always_on"
            } else {
                "closed"
            }
            .into(),
            expires_at: None,
            names: BTreeSet::new(),
            wan_control: String::new(),
            error: None,
        },
    )
}

async fn regenerate(state: &AppState) -> Result<()> {
    let _lock = state.nginx_lock.lock().await;
    // Do not overwrite an unrelated settings edit using a session's old snapshot.
    let current = nginx_render::load_global(&state.config)?;
    let files = crate::nginx::http_forwarder_files(&state.config)?;
    let mut transaction = nginx_render::FileTransaction::apply(&state.config, files)?;
    let test = crate::api::nginx::nginx_syntax_test(state).await?;
    if !test.success {
        bail!("nginx rejected HTTP validation transition: {}", test.stderr);
    }
    if crate::api::nginx::nginx_running(state).await {
        let reload = crate::api::nginx::nginx_reload(state).await?;
        if !reload.success {
            transaction.rollback()?;
            let _ = crate::api::nginx::nginx_reload(state).await;
            bail!(
                "nginx reload failed during HTTP validation transition: {}",
                reload.stderr
            );
        }
    }
    transaction.commit();
    // Report authored conflicts after applying our cleanup: another listener
    // must not prevent removal of the managed validation server.
    verify_http_ownership(state, &current).await?;
    Ok(())
}

/// Inspect expanded includes, not only managed object files. Authored servers
/// must not share challenge HTTP ports in validation-only mode.
pub(crate) async fn verify_http_ownership(state: &AppState, global: &GlobalSettings) -> Result<()> {
    if state.config.test_mode || nginx_ingress::mode(global) != HttpMode::RenewalOnly {
        return Ok(());
    }
    let dump = crate::api::nginx::run_nginx(
        state,
        vec![
            "-T".into(),
            "-c".into(),
            state
                .config
                .nginx
                .config_path
                .to_string_lossy()
                .into_owned(),
        ],
    )
    .await?;
    if !dump.success {
        bail!(
            "cannot inventory HTTP listeners with nginx -T: {}",
            dump.stderr
        );
    }
    if dump.stdout.len() > 8 * 1024 * 1024 {
        bail!("expanded nginx configuration exceeds the 8 MiB HTTP inventory limit");
    }
    let ports = nginx_ingress::profiles(global)
        .values()
        .flat_map(|p| p.http.iter().map(|e| e.port))
        .chain(std::iter::once(80))
        .collect::<BTreeSet<_>>();
    let owned = [
        state.config.nginx.http_forwarder_path.clone(),
        state
            .config
            .nginx
            .root_dir
            .join("ban-shield/default-drop.conf"),
    ];
    let conflicts = unmanaged_http_listeners(&dump.stdout, &owned, &ports)?;
    if !conflicts.is_empty() {
        bail!(
            "unmanaged HTTP listeners prevent validation-only isolation: {}. Remove these listeners or retain Always on; authored configuration was not changed",
            conflicts.join(", ")
        );
    }
    Ok(())
}

fn unmanaged_http_listeners(
    dump: &str,
    owned: &[PathBuf],
    ports: &BTreeSet<u16>,
) -> Result<Vec<String>> {
    let normalize =
        |path: &std::path::Path| path.canonicalize().unwrap_or_else(|_| path.to_owned());
    let owned = owned.iter().map(|p| normalize(p)).collect::<Vec<_>>();
    let mut origin = String::from("expanded nginx configuration");
    let mut sections = Vec::new();
    let mut text = String::new();
    for line in dump.lines() {
        if let Some(file) = line
            .strip_prefix("# configuration file ")
            .and_then(|s| s.strip_suffix(':'))
        {
            sections.push((origin, std::mem::take(&mut text)));
            origin = file.into();
        } else {
            text.push_str(line);
            text.push('\n');
        }
    }
    sections.push((origin, text));
    let mut conflicts = BTreeSet::new();
    for (origin, text) in sections {
        if owned.contains(&normalize(std::path::Path::new(&origin))) {
            continue;
        }
        let mut words: Vec<String> = Vec::new();
        let mut word = String::new();
        let mut quote = None;
        let mut escaped = false;
        let mut comment = false;
        for ch in text.chars() {
            if comment {
                if ch == '\n' {
                    comment = false;
                } else {
                    continue;
                }
            }
            if escaped {
                word.push(ch);
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if let Some(delimiter) = quote {
                if ch == delimiter {
                    quote = None;
                } else {
                    word.push(ch);
                }
                continue;
            }
            if ch == '\'' || ch == '"' {
                quote = Some(ch);
                continue;
            }
            if ch == '#' {
                comment = true;
                continue;
            }
            if ch.is_whitespace() || matches!(ch, ';' | '{' | '}') {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                if ch == ';' {
                    if words.first().is_some_and(|w| w == "listen")
                        && !words.iter().any(|w| w == "ssl" || w == "quic")
                    {
                        let address = words
                            .get(1)
                            .context("HTTP listen directive has no address")?;
                        if !address.starts_with("unix:") {
                            let port = address
                                .parse::<u16>()
                                .ok()
                                .or_else(|| {
                                    address.rsplit_once(':').and_then(|(_, p)| p.parse().ok())
                                })
                                .or_else(|| {
                                    (address.parse::<std::net::IpAddr>().is_ok()
                                        || address.ends_with(']'))
                                    .then_some(80)
                                })
                                .context("cannot verify an authored HTTP listener port")?;
                            if ports.contains(&port) {
                                conflicts.insert(format!("{origin}: {address}"));
                            }
                        }
                    }
                    words.clear();
                } else if matches!(ch, '{' | '}') {
                    words.clear();
                }
            } else {
                word.push(ch);
            }
        }
    }
    Ok(conflicts.into_iter().collect())
}

pub async fn begin(state: &AppState, certificates: &[CertificateSpec]) -> Result<Option<Session>> {
    let transition = state.http_access_lock.lock().await;
    if state.certificates_stopping() {
        bail!("certificate service is shutting down");
    }
    let names: BTreeSet<String> = certificates
        .iter()
        .filter(|spec| matches!(spec.method, crate::models::CertificateMethod::Http))
        .flat_map(|spec| spec.domains.iter().cloned())
        .collect();
    if names.is_empty() {
        return Ok(None);
    }
    let lock = state.dehydrated_lock_status().await?;
    if lock.locked || lock.status == crate::models::DehydratedActivity::Running {
        bail!("dehydrated is running or locked; cannot acquire HTTP validation endpoint");
    }
    let global = {
        let _nginx = state.nginx_lock.lock().await;
        nginx_render::load_global(&state.config)?
    };
    match nginx_ingress::mode(&global) {
        HttpMode::Always => return Ok(None),
        HttpMode::Off => {
            bail!("HTTP-01 is disabled; use DNS-01 or enable certificate validation only")
        }
        HttpMode::RenewalOnly => {}
    }
    let access = global.http_access.as_ref().expect("explicit HTTP mode");
    if access.wan.is_none() && !access.externally_managed_wan {
        bail!(
            "configure HTTP WAN access management or explicitly select external WAN management before HTTP-01 issuance"
        );
    }
    if global
        .shared
        .get("acme_challenge_dir")
        .and_then(serde_json::Value::as_str)
        != Some(
            state
                .config
                .certificates
                .certs_dir
                .join("acme-challenge")
                .to_string_lossy()
                .as_ref(),
        )
    {
        bail!("nginx ACME challenge directory must match dehydrated WELLKNOWN");
    }
    if !crate::api::nginx::nginx_running(state).await {
        bail!("start nginx before HTTP-01 issuance");
    }
    let session = Session {
        names,
        expires_at: Utc::now() + chrono::Duration::seconds(access.window_seconds as i64),
        listener_profile: access.listener_profile.clone(),
    };
    crate::nginx::atomic_write(
        &path(&state.config, "http01-session.json")?,
        &serde_json::to_vec(&session)?,
    )?;
    let result: Result<()> = async {
        // WAN stays closed until the challenge-only handler is installed.
        if access.wan.is_some() {
            wan_gate(state, &global, false).await?;
        }
        regenerate(state).await?;
        verify_listener(state, &global).await?;
        if access.wan.is_some() {
            wan_gate(state, &global, true).await?;
        }
        record(
            state,
            Status {
                state: "active".into(),
                expires_at: Some(session.expires_at),
                names: session.names.clone(),
                wan_control: String::new(),
                error: None,
            },
        )?;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        drop(transition);
        let cleanup = close(state).await.err();
        bail!(
            "HTTP-01 setup failed: {error:#}{}",
            cleanup
                .map(|e| format!("; cleanup also failed: {e:#}"))
                .unwrap_or_default()
        );
    }
    Ok(Some(session))
}

struct ProbeFile(PathBuf);
impl Drop for ProbeFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn verify_listener(state: &AppState, global: &GlobalSettings) -> Result<()> {
    if state.config.test_mode {
        return Ok(());
    }
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let session =
        active(&state.config)?.context("HTTP validation window expired before readiness check")?;
    let token = format!("maestro-probe-{}", uuid::Uuid::new_v4().simple());
    let probe = state
        .config
        .certificates
        .certs_dir
        .join("acme-challenge")
        .join(&token);
    crate::nginx::ensure_no_symlink_ancestors(&state.config.certificates.certs_dir, &probe)?;
    std::fs::create_dir_all(probe.parent().unwrap())?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&probe)?;
    let _probe = ProbeFile(probe);
    file.write_all(token.as_bytes())?;
    let profiles = nginx_ingress::profiles(global);
    let host = session
        .names
        .first()
        .context("HTTP challenge has no hostname")?;
    for endpoint in &profiles[&session.listener_profile].http {
        let address = if endpoint.address.is_unspecified() {
            if endpoint.address.is_ipv4() {
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
            } else {
                std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
            }
        } else {
            endpoint.address
        };
        let mut ready = false;
        for _ in 0..10 {
            let remaining = (session.expires_at - Utc::now())
                .to_std()
                .context("HTTP validation window expired during readiness check")?;
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(remaining.min(Duration::from_secs(3)))
                .build()?;
            let socket = std::net::SocketAddr::new(address, endpoint.port);
            if let Ok(mut response) = client
                .get(format!(
                    "http://{socket}/.well-known/acme-challenge/{token}"
                ))
                .header("Host", host)
                .send()
                .await
            {
                if response.status().is_success() {
                    let mut body = Vec::new();
                    let mut valid = true;
                    while let Some(chunk) = response.chunk().await? {
                        if body.len() + chunk.len() > token.len() {
                            valid = false;
                            break;
                        }
                        body.extend_from_slice(&chunk);
                    }
                    if valid && body == token.as_bytes() {
                        ready = true;
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !ready {
            bail!(
                "HTTP challenge listener {} did not serve its readiness token; WAN access was not opened",
                endpoint.socket()
            );
        }
    }
    Ok(())
}

/// Both steps are attempted: a failed firewall command must not skip nginx closure.
pub async fn close(state: &AppState) -> Result<()> {
    close_impl(state, false).await
}

pub async fn recover_startup(state: &AppState) -> Result<()> {
    close(state).await?;
    let global = nginx_render::load_global(&state.config)?;
    if nginx_ingress::mode(&global) == HttpMode::Always {
        regenerate(state).await?;
    }
    Ok(())
}

async fn close_impl(state: &AppState, allow_running_owned: bool) -> Result<()> {
    let _transition = state.http_access_lock.lock().await;
    let global = {
        let _nginx = state.nginx_lock.lock().await;
        nginx_render::load_global(&state.config)?
    };
    let always = nginx_ingress::mode(&global) == HttpMode::Always;
    let previous_owner = read_owner(&state.config)?;
    let desired_owner = owner_for(&global);
    let release_stale = previous_owner.as_ref().is_some_and(|owner| {
        !owner.released
            && desired_owner
                .as_ref()
                .is_none_or(|desired| owner.wan != desired.wan || owner.port != desired.port)
    });
    if always && !release_stale {
        match std::fs::remove_file(path(&state.config, "http01-session.json")?) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        return Ok(());
    }
    let lock = state.dehydrated_lock_status().await?;
    if lock.status == crate::models::DehydratedActivity::Running
        && !(allow_running_owned && state.certificate_busy() && active(&state.config)?.is_some())
    {
        let error = "dehydrated is running; verify endpoint ownership before HTTP cleanup";
        record(
            state,
            Status {
                state: "cleanup_failed".into(),
                expires_at: None,
                names: BTreeSet::new(),
                wan_control: String::new(),
                error: Some(error.into()),
            },
        )?;
        bail!(error);
    }
    let mut errors = Vec::new();
    if release_stale {
        if let Err(error) = release_owner(state).await {
            errors.push(format!("stale WAN ownership recovery: {error:#}"));
        }
    }
    if global.http_access.as_ref().is_some_and(|a| a.wan.is_some()) {
        if let Err(error) = wan_gate(state, &global, false).await {
            errors.push(format!("WAN closure: {error:#}"));
        }
    }
    match std::fs::remove_file(path(&state.config, "http01-session.json")?) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => errors.push(format!("session cleanup: {error}")),
    }
    if !always {
        if let Err(error) = regenerate(state).await {
            errors.push(format!("nginx closure: {error:#}"));
        }
    }
    let error = (!errors.is_empty()).then(|| errors.join("; "));
    record(
        state,
        Status {
            state: if error.is_some() {
                "cleanup_failed"
            } else if always {
                "always_on"
            } else {
                "closed"
            }
            .into(),
            expires_at: None,
            names: BTreeSet::new(),
            wan_control: String::new(),
            error: error.clone(),
        },
    )?;
    if let Some(error) = error {
        bail!(error);
    }
    Ok(())
}

async fn command(
    state: &AppState,
    program: &std::path::Path,
    args: &[String],
) -> Result<crate::models::CommandResult> {
    state
        .runner
        .run(
            program,
            args.iter().cloned(),
            Duration::from_secs(state.config.nginx.reload_timeout_seconds),
        )
        .await
}

async fn checked(state: &AppState, program: &std::path::Path, args: &[String]) -> Result<()> {
    let result = command(state, program, args).await?;
    if !result.success {
        bail!(
            "{} {}: {}",
            program.display(),
            args.join(" "),
            result.stderr
        );
    }
    Ok(())
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).into()).collect()
}

fn window_accept_args(port: u16) -> Vec<String> {
    let mut args = strings(&[
        "-m",
        "conntrack",
        "--ctproto",
        "tcp",
        "--ctdir",
        "ORIGINAL",
        "--ctorigdstport",
        "80",
    ]);
    if port != 80 {
        args.extend(strings(&["--ctstate", "DNAT"]));
    }
    args.extend(strings(&[
        "-m",
        "set",
        "--match-set",
        WINDOW_SET,
        "dst",
        "-j",
        "ACCEPT",
    ]));
    args
}

async fn ensure_chain(
    state: &AppState,
    program: &std::path::Path,
    table: &str,
    chain: &str,
) -> Result<()> {
    if !command(state, program, &strings(&["-t", table, "-S", chain]))
        .await?
        .success
    {
        checked(state, program, &strings(&["-t", table, "-N", chain])).await?;
    }
    Ok(())
}

async fn ensure_hook(
    state: &AppState,
    program: &std::path::Path,
    table: &str,
    parent: &str,
    args: Vec<String>,
) -> Result<()> {
    let rules = command(state, program, &strings(&["-t", table, "-S", parent])).await?;
    if !rules.success {
        bail!("unable to inspect {table}/{parent} HTTP hook ordering");
    }
    let rows = rules
        .stdout
        .lines()
        .filter(|line| line.starts_with("-A "))
        .collect::<Vec<_>>();
    if rows.len() > 4096 {
        bail!("too many firewall rules to reconcile HTTP hooks");
    }
    let first = rows.iter().position(|line| hook_matches(line, &args));
    let owned_jump = format!("-j {}", args.last().unwrap());
    if first.is_some_and(|index| rows[..=index].iter().all(|line| line.contains(&owned_jump))) {
        return Ok(());
    }
    // Insert the guard first, then remove only exact old copies from the bottom.
    // Existing broad ACCEPT rules never get a gap while moving our hook.
    let mut insert = strings(&["-t", table, "-I", parent, "1"]);
    insert.extend(args.clone());
    checked(state, program, &insert).await?;
    for (index, line) in rows.iter().enumerate().rev() {
        if hook_matches(line, &args) {
            checked(
                state,
                program,
                &strings(&["-t", table, "-D", parent, &(index + 2).to_string()]),
            )
            .await?;
        }
    }
    Ok(())
}

fn hook_matches(line: &str, args: &[String]) -> bool {
    fn pairs(tokens: &[&str]) -> std::collections::BTreeMap<String, String> {
        tokens
            .chunks(2)
            .filter_map(|pair| {
                if pair.len() != 2 || pair == ["-m", "tcp"] {
                    return None;
                }
                Some((
                    pair[0].to_string(),
                    pair[1].trim_end_matches("/32").to_string(),
                ))
            })
            .collect()
    }
    let tokens = line.split_whitespace().skip(2).collect::<Vec<_>>();
    pairs(&tokens) == pairs(&args.iter().map(String::as_str).collect::<Vec<_>>())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WanOwner {
    wan: WanAccess,
    port: u16,
    released: bool,
}

fn owner_for(global: &GlobalSettings) -> Option<WanOwner> {
    let access = global.http_access.as_ref()?;
    Some(WanOwner {
        wan: access.wan.clone()?,
        port: nginx_ingress::profiles(global)
            .get(&access.listener_profile)?
            .http
            .first()?
            .port,
        released: false,
    })
}

fn read_owner(config: &AppConfig) -> Result<Option<WanOwner>> {
    let owner_path = path(config, "http01-wan-owner.json")?;
    let owner: WanOwner = match std::fs::read(owner_path) {
        Ok(bytes) if bytes.len() <= 4096 => serde_json::from_slice(&bytes)?,
        Ok(_) => bail!("HTTP WAN ownership record is too large"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if owner.port == 0
        || owner.wan.interface.is_empty()
        || owner.wan.interface.len() > 15
        || !owner
            .wan
            .interface
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
        || owner.wan.target.is_loopback()
        || owner.wan.target.is_unspecified()
        || owner.wan.target.is_multicast()
        || owner.wan.target.is_broadcast()
    {
        bail!("invalid HTTP WAN ownership record");
    }
    Ok(Some(owner))
}

fn write_owner(config: &AppConfig, owner: &WanOwner) -> Result<()> {
    crate::nginx::atomic_write(
        &path(config, "http01-wan-owner.json")?,
        &serde_json::to_vec(owner)?,
    )
}

async fn claim_owner(state: &AppState, owner: &WanOwner) -> Result<()> {
    match read_owner(&state.config)? {
        Some(previous)
            if !previous.released && (previous.wan != owner.wan || previous.port != owner.port) =>
        {
            bail!("stale HTTP WAN ownership must be recovered before changing bindings")
        }
        None if !state.config.test_mode => {
            for (program, table, chain) in [
                (&state.config.commands.iptables, "filter", INPUT_CHAIN),
                (&state.config.commands.ip6tables, "filter", INPUT_CHAIN),
                (&state.config.commands.iptables, "filter", FORWARD_CHAIN),
                (&state.config.commands.iptables, "nat", NAT_CHAIN),
            ] {
                if command(state, program, &strings(&["-t", table, "-S", chain]))
                    .await?
                    .success
                {
                    bail!(
                        "HTTP reserved chain {chain} exists without this installation's ownership record; inspect it before enabling automatic WAN control"
                    );
                }
            }
            if command(
                state,
                &state.config.commands.ipset,
                &strings(&["list", WINDOW_SET]),
            )
            .await?
            .success
            {
                bail!("HTTP reserved ipset exists without this installation's ownership record");
            }
        }
        _ => {}
    }
    // Write ahead of the first rule change, so a crash during configuration
    // replacement can still remove old interface/address hooks at startup.
    write_owner(&state.config, owner)
}

/// A rebuilt/reordered firewall closes the window without extending its lease.
pub async fn reconcile(state: &AppState) -> Result<()> {
    let Ok(transition) = state.http_access_lock.try_lock() else {
        return Ok(());
    };
    let global = {
        let _nginx = state.nginx_lock.lock().await;
        nginx_render::load_global(&state.config)?
    };
    let Some(owner) = read_owner(&state.config)? else {
        return Ok(());
    };
    if owner.released {
        return Ok(());
    }
    let desired = owner_for(&global);
    let window_open = active(&state.config)?.is_some();
    let mut healthy = desired
        .as_ref()
        .is_some_and(|desired| desired.wan == owner.wan && desired.port == owner.port);
    let interface = &owner.wan.interface;
    let target = owner.wan.target.to_string();
    let port = owner.port.to_string();
    for (program, args) in [
        (
            &state.config.commands.iptables,
            strings(&[
                "-i",
                interface,
                "-d",
                &target,
                "-p",
                "tcp",
                "--dport",
                &port,
                "-j",
                INPUT_CHAIN,
            ]),
        ),
        (
            &state.config.commands.iptables,
            strings(&[
                "-i",
                interface,
                "-p",
                "tcp",
                "--dport",
                "80",
                "-j",
                INPUT_CHAIN,
            ]),
        ),
        (
            &state.config.commands.ip6tables,
            strings(&[
                "-i",
                interface,
                "-p",
                "tcp",
                "--dport",
                "80",
                "-j",
                INPUT_CHAIN,
            ]),
        ),
    ] {
        let rules = command(state, program, &strings(&["-t", "filter", "-S", "INPUT"])).await?;
        let rows = rules
            .stdout
            .lines()
            .filter(|line| line.starts_with("-A "))
            .collect::<Vec<_>>();
        healthy &= rules.success
            && rows
                .iter()
                .position(|line| hook_matches(line, &args))
                .is_some_and(|index| {
                    rows[..=index]
                        .iter()
                        .all(|line| line.ends_with(&format!("-j {INPUT_CHAIN}")))
                });
        let body = command(
            state,
            program,
            &strings(&["-t", "filter", "-S", INPUT_CHAIN]),
        )
        .await?;
        let rows = body
            .stdout
            .lines()
            .filter(|line| line.starts_with("-A "))
            .collect::<Vec<_>>();
        let expect_accept = window_open && program == &state.config.commands.iptables;
        healthy &= body.success
            && rows.len() == if expect_accept { 2 } else { 1 }
            && rows
                .last()
                .is_some_and(|line| *line == format!("-A {INPUT_CHAIN} -j DROP"));
        if expect_accept {
            let mut rule = strings(&["-t", "filter", "-C", INPUT_CHAIN]);
            rule.extend(window_accept_args(owner.port));
            healthy &= command(state, program, &rule).await?.success;
        }
    }
    let forward_args = strings(&[
        "-i",
        interface,
        "-d",
        &target,
        "-p",
        "tcp",
        "--dport",
        &port,
        "-j",
        FORWARD_CHAIN,
    ]);
    let forward = command(
        state,
        &state.config.commands.iptables,
        &strings(&["-t", "filter", "-S", "FORWARD"]),
    )
    .await?;
    let rows = forward
        .stdout
        .lines()
        .filter(|line| line.starts_with("-A "))
        .collect::<Vec<_>>();
    healthy &= forward.success
        && rows
            .iter()
            .position(|line| hook_matches(line, &forward_args))
            .is_some_and(|index| {
                rows[..=index]
                    .iter()
                    .all(|line| line.ends_with(&format!("-j {FORWARD_CHAIN}")))
            });
    let body = command(
        state,
        &state.config.commands.iptables,
        &strings(&["-t", "filter", "-S", FORWARD_CHAIN]),
    )
    .await?;
    healthy &= body.success
        && body
            .stdout
            .lines()
            .filter(|line| line.starts_with("-A "))
            .collect::<Vec<_>>()
            == vec![format!("-A {FORWARD_CHAIN} -j DROP").as_str()];
    let rules = command(
        state,
        &state.config.commands.iptables,
        &strings(&["-t", "nat", "-S", "PREROUTING"]),
    )
    .await?;
    let rows = rules
        .stdout
        .lines()
        .filter(|line| line.starts_with("-A "))
        .collect::<Vec<_>>();
    healthy &= rules.success
        && if window_open {
            rows.first()
                .is_some_and(|line| line.ends_with(&format!("-j {NAT_CHAIN}")))
        } else {
            !rows
                .iter()
                .any(|line| line.ends_with(&format!("-j {NAT_CHAIN}")))
        };
    healthy &= check_forwarding_conflicts(state).await.is_ok();
    drop(transition);
    if !healthy {
        close_impl(state, true).await?;
        bail!("HTTP firewall guard was rebuilt or reordered; validation access was closed");
    }
    Ok(())
}

pub fn start_reconciliation(state: AppState) {
    if state.config.test_mode {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        loop {
            interval.tick().await;
            if let Err(error) = reconcile(&state).await {
                tracing::warn!(%error,"HTTP WAN reconciliation failed");
            }
        }
    });
}

async fn wan_gate(state: &AppState, global: &GlobalSettings, open: bool) -> Result<()> {
    let owner = owner_for(global).context("WAN HTTP adapter is not configured")?;
    claim_owner(state, &owner).await?;
    if open {
        if !state.config.test_mode {
            let addresses = command(
                state,
                &state.config.commands.ip,
                &strings(&["-o", "-4", "addr", "show"]),
            )
            .await?;
            if !addresses.success
                || !addresses
                    .stdout
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .windows(2)
                    .any(|pair| {
                        pair[0] == "inet"
                            && pair[1].split('/').next()
                                == Some(owner.wan.target.to_string().as_str())
                    })
            {
                bail!(
                    "managed HTTP WAN target is not a verified local IPv4 address; no forwarding was opened"
                );
            }
        }
        check_forwarding_conflicts(state).await?;
    }
    wan_gate_owned(state, &owner, open).await?;
    if !open {
        check_forwarding_conflicts(state).await?;
    }
    Ok(())
}

async fn check_forwarding_conflicts(state: &AppState) -> Result<()> {
    let rules = command(
        state,
        &state.config.commands.iptables,
        &strings(&["-t", "nat", "-S"]),
    )
    .await?;
    if !rules.success {
        bail!("cannot verify existing WAN HTTP forwarding rules");
    }
    for line in rules.stdout.lines() {
        let tokens = line.split_whitespace().collect::<Vec<_>>();
        if tokens.get(1) == Some(&NAT_CHAIN) || tokens.windows(2).any(|pair| pair == ["-p", "udp"])
        {
            continue;
        }
        if !tokens
            .windows(2)
            .any(|pair| pair == ["-j", "DNAT"] || pair == ["-j", "REDIRECT"])
        {
            continue;
        }
        let ports = tokens
            .windows(2)
            .find(|pair| ["--dport", "--dports", "--destination-port"].contains(&pair[0]))
            .map(|pair| pair[1]);
        let matches = ports.is_none_or(|ports| {
            ports.split(',').any(|port| {
                if let Some((start, end)) = port.split_once(':') {
                    start.parse::<u16>().is_ok_and(|n| n <= 80)
                        && end.parse::<u16>().is_ok_and(|n| n >= 80)
                } else {
                    port == "80"
                }
            })
        });
        if matches {
            bail!(
                "an existing DNAT/REDIRECT rule can forward TCP 80; disable conflicting saved/router forwarding before using managed HTTP access"
            );
        }
    }
    Ok(())
}

async fn wan_gate_owned(state: &AppState, owner: &WanOwner, open: bool) -> Result<()> {
    let interface = &owner.wan.interface;
    let target = owner.wan.target.to_string();
    let port = owner.port.to_string();
    let iptables = &state.config.commands.iptables;
    let ip6tables = &state.config.commands.ip6tables;
    let ipset = &state.config.commands.ipset;
    // A private owned set is only a timed authorization gate, not a ban set.
    checked(
        state,
        ipset,
        &strings(&[
            "create", WINDOW_SET, "hash:ip", "family", "inet", "timeout", "86400", "-exist",
        ]),
    )
    .await?;
    checked(state, ipset, &strings(&["flush", WINDOW_SET])).await?;
    ensure_chain(state, iptables, "filter", INPUT_CHAIN).await?;
    ensure_chain(state, iptables, "filter", FORWARD_CHAIN).await?;
    ensure_chain(state, ip6tables, "filter", INPUT_CHAIN).await?;
    ensure_chain(state, iptables, "nat", NAT_CHAIN).await?;
    // Install a drop before rebuilding the conditional accept. An interrupted
    // update remains closed instead of falling through to firmware broad accepts.
    checked(
        state,
        iptables,
        &strings(&["-t", "filter", "-I", INPUT_CHAIN, "1", "-j", "DROP"]),
    )
    .await?;
    checked(
        state,
        ip6tables,
        &strings(&["-t", "filter", "-I", INPUT_CHAIN, "1", "-j", "DROP"]),
    )
    .await?;
    // If the router loses its target address, DNAT must never start forwarding
    // validation traffic to another LAN host that later takes that address.
    checked(
        state,
        iptables,
        &strings(&["-t", "filter", "-I", FORWARD_CHAIN, "1", "-j", "DROP"]),
    )
    .await?;
    let forward = command(
        state,
        iptables,
        &strings(&["-t", "filter", "-S", FORWARD_CHAIN]),
    )
    .await?;
    let count = forward
        .stdout
        .lines()
        .filter(|line| line.starts_with("-A "))
        .count();
    for index in (2..=count).rev() {
        checked(
            state,
            iptables,
            &strings(&["-t", "filter", "-D", FORWARD_CHAIN, &index.to_string()]),
        )
        .await?;
    }
    ensure_hook(
        state,
        iptables,
        "filter",
        "FORWARD",
        strings(&[
            "-i",
            interface,
            "-d",
            &target,
            "-p",
            "tcp",
            "--dport",
            &port,
            "-j",
            FORWARD_CHAIN,
        ]),
    )
    .await?;
    // Keep the first DROP while pruning old owned rules from the bottom.
    for program in [iptables, ip6tables] {
        let rules = command(
            state,
            program,
            &strings(&["-t", "filter", "-S", INPUT_CHAIN]),
        )
        .await?;
        if !rules.success {
            bail!("unable to inspect HTTP WAN guard");
        }
        let count = rules
            .stdout
            .lines()
            .filter(|line| line.starts_with("-A "))
            .count();
        for index in (2..=count).rev() {
            checked(
                state,
                program,
                &strings(&["-t", "filter", "-D", INPUT_CHAIN, &index.to_string()]),
            )
            .await?;
        }
    }
    ensure_hook(
        state,
        iptables,
        "filter",
        "INPUT",
        strings(&[
            "-i",
            interface,
            "-d",
            &target,
            "-p",
            "tcp",
            "--dport",
            &port,
            "-j",
            INPUT_CHAIN,
        ]),
    )
    .await?;
    ensure_hook(
        state,
        iptables,
        "filter",
        "INPUT",
        strings(&[
            "-i",
            interface,
            "-p",
            "tcp",
            "--dport",
            "80",
            "-j",
            INPUT_CHAIN,
        ]),
    )
    .await?;
    ensure_hook(
        state,
        ip6tables,
        "filter",
        "INPUT",
        strings(&[
            "-i",
            interface,
            "-p",
            "tcp",
            "--dport",
            "80",
            "-j",
            INPUT_CHAIN,
        ]),
    )
    .await?;
    if open {
        checked(state, iptables, &strings(&["-t", "nat", "-F", NAT_CHAIN])).await?;
        checked(
            state,
            iptables,
            &strings(&[
                "-t",
                "nat",
                "-A",
                NAT_CHAIN,
                "-p",
                "tcp",
                "--dport",
                "80",
                "-j",
                "DNAT",
                "--to-destination",
                &format!("{target}:{port}"),
            ]),
        )
        .await?;
        ensure_hook(
            state,
            iptables,
            "nat",
            "PREROUTING",
            strings(&[
                "-i", interface, "-p", "tcp", "--dport", "80", "-j", NAT_CHAIN,
            ]),
        )
        .await?;
        let mut accept = strings(&["-t", "filter", "-I", INPUT_CHAIN, "1"]);
        accept.extend(window_accept_args(owner.port));
        checked(state, iptables, &accept).await?;
        // Authorize last. Expiry in the kernel leaves the following DROP active.
        let remaining = active(&state.config)?
            .context("HTTP validation window expired during setup")?
            .expires_at
            .signed_duration_since(Utc::now())
            .num_seconds();
        if remaining < 1 {
            bail!("HTTP validation window expired during setup");
        }
        checked(
            state,
            ipset,
            &strings(&[
                "add",
                WINDOW_SET,
                &target,
                "timeout",
                &remaining.to_string(),
                "-exist",
            ]),
        )
        .await?;
    } else {
        let hook = strings(&[
            "-t",
            "nat",
            "-C",
            "PREROUTING",
            "-i",
            interface,
            "-p",
            "tcp",
            "--dport",
            "80",
            "-j",
            NAT_CHAIN,
        ]);
        if command(state, iptables, &hook).await?.success {
            let mut remove = hook;
            remove[2] = "-D".into();
            checked(state, iptables, &remove).await?;
        }
        checked(state, iptables, &strings(&["-t", "nat", "-F", NAT_CHAIN])).await?;
    }
    Ok(())
}

/// Called under the nginx settings lock. Never touches administrator-owned rules.
pub async fn change_wan(
    state: &AppState,
    previous: &GlobalSettings,
    next: &GlobalSettings,
) -> Result<()> {
    if serde_json::to_value(&previous.http_access)? == serde_json::to_value(&next.http_access)?
        && previous.shared.get("router_ip") == next.shared.get("router_ip")
        && previous.shared.get("http_port") == next.shared.get("http_port")
        && serde_json::to_value(&previous.listener_profiles)?
            == serde_json::to_value(&next.listener_profiles)?
    {
        return Ok(());
    }
    // Settings already hold nginx_lock; fail instead of waiting in reverse
    // lock order while a session transition may be waiting for nginx_lock.
    let _transition = state
        .http_access_lock
        .try_lock()
        .context("HTTP access transition in progress; retry the settings save")?;
    release_owner(state).await?;
    if next
        .http_access
        .as_ref()
        .is_some_and(|access| access.wan.is_some())
    {
        wan_gate(state, next, false).await?;
    }
    Ok(())
}

async fn release_owner(state: &AppState) -> Result<()> {
    let Some(mut owner) = read_owner(&state.config)? else {
        return Ok(());
    };
    if owner.released {
        return Ok(());
    }
    wan_gate_owned(state, &owner, false).await?;
    let interface = &owner.wan.interface;
    let target = owner.wan.target.to_string();
    let port = owner.port.to_string();
    for (program, parent, hook) in [
        (
            &state.config.commands.iptables,
            "INPUT",
            strings(&[
                "-i",
                interface,
                "-d",
                &target,
                "-p",
                "tcp",
                "--dport",
                &port,
                "-j",
                INPUT_CHAIN,
            ]),
        ),
        (
            &state.config.commands.iptables,
            "INPUT",
            strings(&[
                "-i",
                interface,
                "-p",
                "tcp",
                "--dport",
                "80",
                "-j",
                INPUT_CHAIN,
            ]),
        ),
        (
            &state.config.commands.ip6tables,
            "INPUT",
            strings(&[
                "-i",
                interface,
                "-p",
                "tcp",
                "--dport",
                "80",
                "-j",
                INPUT_CHAIN,
            ]),
        ),
        (
            &state.config.commands.iptables,
            "FORWARD",
            strings(&[
                "-i",
                interface,
                "-d",
                &target,
                "-p",
                "tcp",
                "--dport",
                &port,
                "-j",
                FORWARD_CHAIN,
            ]),
        ),
    ] {
        for _ in 0..32 {
            let mut check = strings(&["-t", "filter", "-C", parent]);
            check.extend(hook.clone());
            if !command(state, program, &check).await?.success || state.config.test_mode {
                break;
            }
            let mut remove = strings(&["-t", "filter", "-D", parent]);
            remove.extend(hook.clone());
            checked(state, program, &remove).await?;
        }
    }
    owner.released = true;
    write_owner(&state.config, &owner)
}

/// Rendering reads the same bounded session record for every regeneration path.
pub fn challenge_output(
    config: &AppConfig,
    global: &GlobalSettings,
) -> Result<Option<(String, String)>> {
    if nginx_ingress::mode(global) != HttpMode::RenewalOnly {
        return Ok(None);
    }
    let Some(session) = active(config)? else {
        return Ok(None);
    };
    let access = global
        .http_access
        .as_ref()
        .context("HTTP settings missing")?;
    if session.listener_profile != access.listener_profile {
        bail!("cannot change challenge listener during an active HTTP session");
    }
    let profiles = nginx_ingress::profiles(global);
    let endpoints = &profiles[&session.listener_profile].http;
    let var_dir = global
        .shared
        .get("var_dir")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("/opt/var");
    let mut defaults = String::new();
    let mut server = String::from("server {\n");
    for endpoint in endpoints {
        defaults.push_str(&format!(
            "server {{ listen {} default_server; server_name _; access_log {var_dir}/log/nginx/unknown-host.log f2b_trap; return 444; }}\n",
            endpoint.socket()
        ));
        server.push_str(&format!(" listen {};\n", endpoint.socket()));
    }
    server.push_str(&format!(" server_name {};\n include snippets/acme-challenge.conf;\n location / {{ return 404; }}\n}}\n",session.names.into_iter().collect::<Vec<_>>().join(" ")));
    Ok(Some((defaults, server)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    async fn fixture() -> (tempfile::TempDir, AppState, GlobalSettings, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        config
            .apply_test_mode(&temp.path().join("test-fixtures/etc/maestro/maestro.yaml"))
            .unwrap();
        config.ensure_directories().unwrap();
        let commands = temp.path().join("commands");
        std::fs::create_dir_all(&commands).unwrap();
        for name in ["iptables", "ip6tables", "ipset", "ip", "nginx"] {
            let executable = commands.join(name);
            std::fs::write(
                &executable,
                include_str!("../test-fixtures/bin/netfilter-mock.py"),
            )
            .unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        config.test_mode = false;
        config.commands.iptables = commands.join("iptables");
        config.commands.ip6tables = commands.join("ip6tables");
        config.commands.ipset = commands.join("ipset");
        config.commands.ip = commands.join("ip");
        config.commands.nginx = commands.join("nginx");
        std::fs::write(&config.nginx.config_path, "events {} http {}\n").unwrap();
        std::fs::write(&config.nginx.pid_path, std::process::id().to_string()).unwrap();
        let stores = crate::storage::Stores::load(&config).await.unwrap();
        let firewall = crate::firewall::FirewallManager::new(config.clone(), stores.clone())
            .await
            .unwrap();
        let state = AppState::new(config, stores, firewall);
        let global:GlobalSettings=serde_json::from_value(json!({
            "shared":{"router_ip":"192.0.2.1","http_port":18080,"https_port":18443},
            "http_access":{"mode":"renewal_only","window_seconds":600,"wan":{"interface":"mock0","target":"192.0.2.1"}}
        })).unwrap();
        let global = nginx_render::with_path_defaults(global, &state.config);
        nginx_render::FileTransaction::apply(
            &state.config,
            nginx_render::shared_files(&state.config, &global).unwrap(),
        )
        .unwrap()
        .commit();
        (temp, state, global, commands)
    }

    fn read_state(commands: &std::path::Path) -> Value {
        serde_json::from_slice(&std::fs::read(commands.join("netfilter-state.json")).unwrap())
            .unwrap()
    }

    fn session(state: &AppState) {
        let session = Session {
            names: BTreeSet::from(["example.test".into()]),
            expires_at: Utc::now() + chrono::Duration::seconds(600),
            listener_profile: "legacy".into(),
        };
        crate::nginx::atomic_write(
            &path(&state.config, "http01-session.json").unwrap(),
            &serde_json::to_vec(&session).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn wan_rules_are_owned_ordered_and_time_limited() {
        let (_temp, state, global, commands) = fixture().await;
        wan_gate(&state, &global, false).await.unwrap();
        session(&state);
        wan_gate(&state, &global, true).await.unwrap();
        let rules = read_state(&commands);
        let lease = &rules["sets"][WINDOW_SET]["192.0.2.1"];
        assert!(
            lease["timeout"]
                .as_u64()
                .is_some_and(|ttl| ttl > 0 && ttl <= 600)
        );
        let guard = rules["iptables"][format!("filter:{INPUT_CHAIN}")]
            .as_array()
            .unwrap();
        assert!(guard[0].to_string().contains(WINDOW_SET));
        assert_eq!(
            guard[0],
            json!([
                "-m",
                "conntrack",
                "--ctproto",
                "tcp",
                "--ctdir",
                "ORIGINAL",
                "--ctorigdstport",
                "80",
                "--ctstate",
                "DNAT",
                "-m",
                "set",
                "--match-set",
                WINDOW_SET,
                "dst",
                "-j",
                "ACCEPT"
            ])
        );
        assert_eq!(guard[1], json!(["-j", "DROP"]));
        assert_eq!(
            rules["ip6tables"][format!("filter:{INPUT_CHAIN}")],
            json!([["-j", "DROP"]])
        );
        assert_eq!(
            rules["iptables"][format!("filter:{FORWARD_CHAIN}")],
            json!([["-j", "DROP"]])
        );
        assert_eq!(
            rules["iptables"]["filter:INPUT"]
                .as_array()
                .unwrap()
                .last()
                .unwrap(),
            &json!(["-j", "ACCEPT"]),
            "administrator rules remain intact behind the guard"
        );
        reconcile(&state).await.unwrap();
        std::fs::write(state.config.certificates.certs_dir.join("lock"), "stale").unwrap();
        close(&state).await.unwrap();
        let rules = read_state(&commands);
        assert_eq!(rules["sets"][WINDOW_SET], json!({}));
        assert_eq!(rules["iptables"]["nat:PREROUTING"], json!([]));
        assert_eq!(status(&state.config).unwrap().state, "closed");
    }

    #[tokio::test]
    async fn firewall_rebuild_closes_the_window_instead_of_reopening_it() {
        let (_temp, state, global, commands) = fixture().await;
        wan_gate(&state, &global, false).await.unwrap();
        session(&state);
        wan_gate(&state, &global, true).await.unwrap();
        let mut rules = read_state(&commands);
        rules["iptables"]["filter:INPUT"] = json!([["-j", "ACCEPT"]]);
        std::fs::write(commands.join("netfilter-state.json"), rules.to_string()).unwrap();
        assert!(reconcile(&state).await.is_err());
        assert!(active(&state.config).unwrap().is_none());
        let rules = read_state(&commands);
        assert_eq!(rules["sets"][WINDOW_SET], json!({}));
        assert_eq!(rules["iptables"]["nat:PREROUTING"], json!([]));
        assert!(
            rules["iptables"]["filter:INPUT"][0]
                .to_string()
                .contains(INPUT_CHAIN)
        );
    }

    #[tokio::test]
    async fn ownership_journal_recovers_old_bindings_after_configuration_changes() {
        let (_temp, state, global, commands) = fixture().await;
        wan_gate(&state, &global, false).await.unwrap();
        session(&state);
        wan_gate(&state, &global, true).await.unwrap();
        let mut changed = global.clone();
        changed.http_access = Some(nginx_ingress::HttpAccess {
            mode: HttpMode::Always,
            ..Default::default()
        });
        changed.shared.insert("http_port".into(), 19080.into());
        nginx_render::FileTransaction::apply(
            &state.config,
            nginx_render::shared_files(&state.config, &changed).unwrap(),
        )
        .unwrap()
        .commit();
        let restarted = AppState::new(
            (*state.config).clone(),
            state.stores.clone(),
            state.firewall.clone(),
        );
        close(&restarted).await.unwrap();
        let rules = read_state(&commands);
        assert_eq!(rules["iptables"]["filter:INPUT"], json!([["-j", "ACCEPT"]]));
        assert_eq!(
            rules["iptables"]["filter:FORWARD"],
            json!([["-j", "ACCEPT"]])
        );
        assert_eq!(
            rules["ip6tables"]["filter:INPUT"],
            json!([["-j", "ACCEPT"]])
        );
        assert!(read_owner(&state.config).unwrap().unwrap().released);
        assert!(active(&state.config).unwrap().is_none());
        assert_eq!(status(&state.config).unwrap().state, "always_on");
    }

    #[tokio::test]
    async fn cleanup_failure_is_reported_and_can_be_retried() {
        let (_temp, state, global, commands) = fixture().await;
        wan_gate(&state, &global, false).await.unwrap();
        session(&state);
        wan_gate(&state, &global, true).await.unwrap();
        std::fs::write(commands.join("fail-next"), "iptables -t nat -F").unwrap();
        assert!(close(&state).await.is_err());
        assert!(
            active(&state.config).unwrap().is_none(),
            "nginx cleanup is attempted even if WAN cleanup fails"
        );
        assert_eq!(status(&state.config).unwrap().state, "cleanup_failed");
        close(&state).await.unwrap();
        assert_eq!(status(&state.config).unwrap().state, "closed");
    }

    #[tokio::test]
    async fn refuses_to_adopt_foreign_reserved_chains() {
        let (_temp, state, global, commands) = fixture().await;
        let foreign = json!({"iptables":{format!("filter:{INPUT_CHAIN}"):[["-j","ACCEPT"]]},"ip6tables":{},"sets":{}});
        std::fs::write(commands.join("netfilter-state.json"), foreign.to_string()).unwrap();
        assert!(
            wan_gate(&state, &global, false)
                .await
                .unwrap_err()
                .to_string()
                .contains("ownership record")
        );
        assert_eq!(read_state(&commands), foreign);
        assert!(read_owner(&state.config).unwrap().is_none());
    }
}

#[cfg(test)]
mod http_inventory_tests {
    use super::*;

    #[test]
    fn expanded_inventory_distinguishes_owned_tls_comments_and_authored_http() {
        let dump = r#"# configuration file /lab/nginx.conf:
# listen 80;
events {} http { include conf.d/*.conf; }
# configuration file /lab/owned.conf:
server { listen 80; }
# configuration file /lab/custom.conf:
server { listen 443 ssl; return 200 "listen 80; # a quoted string"; }
server { listen [::]:18080; server_name private.example.test; }
server { listen 127.0.0.1:80; }
server { listen unix:/run/app.sock; }
"#;
        let conflicts = unmanaged_http_listeners(
            dump,
            &[PathBuf::from("/lab/owned.conf")],
            &BTreeSet::from([80, 18080]),
        )
        .unwrap();
        assert_eq!(
            conflicts,
            vec![
                "/lab/custom.conf: 127.0.0.1:80",
                "/lab/custom.conf: [::]:18080"
            ]
        );
        assert!(
            unmanaged_http_listeners("server { listen localhost; }", &[], &BTreeSet::from([80]))
                .is_err(),
            "unrecognized authored bindings require review, rather than a closed-state claim"
        );
    }
}
