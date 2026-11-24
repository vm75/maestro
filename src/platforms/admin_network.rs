//! Narrow root-only network operations. No caller-provided executable, path,
//! raw nft syntax, WireGuard private key or shell hook crosses this boundary.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Mutex,
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Policy, network_operation, reference::{self, Request, Response}, trusted_component};
use crate::ban_attack::{BanBackend, BanTarget, BackendError};
use crate::ban_attack::ReconcileReport;

const NFT_TABLE: &str = "maestro_ban_shield";
const WG_PATH: &str = "/etc/wireguard/wg0.conf";
const MAX_NETWORKS: usize = 8192;
const MAX_PEERS: usize = 256;
const MAX_CONFIG: usize = 256 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FirewallInput {
    enabled: bool,
    active: Vec<IpNet>,
    exceptions: Vec<IpNet>,
}

fn nft_script(input: &FirewallInput) -> Result<String> {
    ensure!(input.active.len() <= MAX_NETWORKS && input.exceptions.len() <= MAX_NETWORKS, "too many networks");
    ensure!(input.enabled || (input.active.is_empty() && input.exceptions.is_empty()), "disabled firewall must be empty");
    // add is idempotent; delete then recreate the reserved table in a single
    // atomic nft transaction. No other table or chain is modified.
    let mut script = format!("add table inet {NFT_TABLE}\ndelete table inet {NFT_TABLE}\n");
    if !input.enabled {
        return Ok(script);
    }
    script.push_str(&format!("add table inet {NFT_TABLE}\n"));
    for (name, ipv4, nets) in [
        ("allow4", true, &input.exceptions),
        ("allow6", false, &input.exceptions),
        ("ban4", true, &input.active),
        ("ban6", false, &input.active),
    ] {
        let kind = if ipv4 { "ipv4_addr" } else { "ipv6_addr" };
        script.push_str(&format!("add set inet {NFT_TABLE} {name} {{ type {kind}; flags interval; auto-merge; }}\n"));
        let entries = nets.iter().filter(|net| net.addr().is_ipv4() == ipv4)
            .map(|net| net.trunc().to_string()).collect::<BTreeSet<_>>();
        if !entries.is_empty() {
            script.push_str(&format!("add element inet {NFT_TABLE} {name} {{ {} }}\n", entries.into_iter().collect::<Vec<_>>().join(", ")));
        }
    }
    script.push_str(&format!("add chain inet {NFT_TABLE} input {{ type filter hook input priority -10; policy accept; }}\n"));
    script.push_str(&format!("add rule inet {NFT_TABLE} input iifname \"lo\" return\n"));
    for (family, suffix) in [("ip", "4"), ("ip6", "6")] {
        script.push_str(&format!("add rule inet {NFT_TABLE} input {family} saddr @allow{suffix} return\n"));
        script.push_str(&format!("add rule inet {NFT_TABLE} input {family} saddr @ban{suffix} drop\n"));
    }
    Ok(script)
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let path = Path::new("/run/maestro-admin").join(format!("network-{}", uuid::Uuid::new_v4()));
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true)
        .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

async fn firewall(input: FirewallInput) -> Result<Value> {
    let script = nft_script(&input)?;
    let nft = reference::program("nft").context("nftables is unavailable")?;
    let scratch = Scratch::new()?;
    let path = scratch.0.join("rules.nft");
    private_write(&path, script.as_bytes())?;
    let output = reference::capture(&nft, &["-f", path.to_str().context("invalid scratch path")?], &[], 12).await?;
    ensure!(output.code == Some(0), "nftables transaction failed");
    Ok(json!({"enabled":input.enabled}))
}

// Engine callbacks are synchronous, including startup inside an async server.
// Execute each bounded helper call on a separate current-thread runtime.
fn apply_firewall(enabled: bool, active: &BTreeSet<IpNet>, exceptions: &BTreeSet<IpNet>) -> Result<(), BackendError> {
    let args = json!({"enabled":enabled,"active":active,"exceptions":exceptions});
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()
            .map_err(|e| BackendError(e.to_string()))?;
        rt.block_on(network_operation("ban_shield_apply", args))
            .map(|_| ()).map_err(|e| BackendError(e.to_string()))
    }).join().map_err(|_| BackendError("network helper worker failed".into()))?
}

#[derive(Default)]
pub struct HelperBanBackend {
    // Commit desired state only after the atomic host transaction succeeds.
    state: Mutex<(BTreeSet<IpNet>, BTreeSet<IpNet>)>,
}

fn network(target: &BanTarget) -> IpNet {
    match target {
        BanTarget::Ip(ip) => IpNet::from(*ip),
        BanTarget::Subnet(net) => *net,
    }
}

impl HelperBanBackend {
    fn change(&self, change: impl FnOnce(&mut BTreeSet<IpNet>, &mut BTreeSet<IpNet>)) -> Result<(), BackendError> {
        let mut guard = self.state.lock().map_err(|_| BackendError("network backend lock failed".into()))?;
        let (mut active, mut exceptions) = guard.clone();
        change(&mut active, &mut exceptions);
        apply_firewall(true, &active, &exceptions)?;
        *guard = (active, exceptions);
        Ok(())
    }
}
impl BanBackend for HelperBanBackend {
    // The engine immediately reconciles persisted state after ensure. Avoid
    // replacing an existing table with an empty table during startup.
    fn ensure(&self) -> Result<(), BackendError> { Ok(()) }
    fn add(&self, target: &BanTarget) -> Result<(), BackendError> {
        self.change(|active, _| { active.insert(network(target)); })
    }
    fn remove(&self, target: &BanTarget) -> Result<(), BackendError> {
        self.change(|active, _| { active.remove(&network(target)); })
    }
    fn set_exceptions(&self, exceptions: &[IpNet]) -> Result<(), BackendError> {
        self.change(|_, current| { *current = exceptions.iter().copied().collect(); })
    }
    fn reconcile(&self, active: &[BanTarget], exceptions: &[IpNet]) -> Result<ReconcileReport, BackendError> {
        self.change(|current, allowed| {
            *current = active.iter().map(network).collect();
            *allowed = exceptions.iter().copied().collect();
        })?;
        Ok(ReconcileReport { restored_entries: active.len(), repaired: true })
    }
    fn disable(&self) -> Result<(), BackendError> {
        let mut guard = self.state.lock().map_err(|_| BackendError("network backend lock failed".into()))?;
        apply_firewall(false, &BTreeSet::new(), &BTreeSet::new())?;
        *guard = Default::default();
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Peer {
    public_key: String,
    allowed_ips: String,
    endpoint: Option<String>,
    persistent_keepalive: Option<u16>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerInput {
    revision: String,
    peers: Vec<Peer>,
}
struct Parsed {
    interface: String,
    peers: Vec<Peer>,
    preshared: BTreeMap<String, String>,
    address: String,
    listen_port: Option<u16>,
}
fn key_valid(key: &str) -> bool {
    STANDARD.decode(key).is_ok_and(|bytes| bytes.len() == 32 && STANDARD.encode(bytes) == key)
}
fn endpoint_valid(value: &str) -> bool {
    let Some((host, port)) = value.rsplit_once(':') else { return false; };
    if port.parse::<u16>().ok().filter(|port| *port > 0).is_none() { return false; }
    if let Some(ip) = host.strip_prefix('[').and_then(|host| host.strip_suffix(']')) {
        return ip.parse::<std::net::Ipv6Addr>().is_ok();
    }
    !host.is_empty() && host.len() <= 253 && host.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-')
}
fn normalize_peers(peers: &mut [Peer]) -> Result<()> {
    ensure!(peers.len() <= MAX_PEERS, "too many WireGuard peers");
    let mut keys = BTreeSet::new();
    for peer in peers {
        ensure!(key_valid(&peer.public_key) && keys.insert(peer.public_key.clone()), "invalid or duplicate WireGuard public key");
        let ips = peer.allowed_ips.split(',').map(|s| s.trim().parse::<IpNet>())
            .collect::<std::result::Result<Vec<_>, _>>().context("invalid allowed IP network")?;
        ensure!(!ips.is_empty() && ips.len() <= 128, "invalid allowed IP count");
        peer.allowed_ips = ips.iter().map(|net| net.trunc().to_string()).collect::<Vec<_>>().join(", ");
        ensure!(peer.endpoint.as_deref().is_none_or(endpoint_valid), "invalid WireGuard endpoint");
    }
    Ok(())
}

fn parse_config(text: &str) -> Result<Parsed> {
    let mut result = Parsed { interface: String::new(), peers: Vec::new(), preshared: BTreeMap::new(), address: String::new(), listen_port: None };
    let mut peer: Option<BTreeMap<String, String>> = None;
    let mut interface_seen = false;
    fn finish(fields: BTreeMap<String, String>, result: &mut Parsed) -> Result<()> {
        let public_key = fields.get("publickey").context("peer missing public key")?.clone();
        if let Some(key) = fields.get("presharedkey") {
            ensure!(key_valid(key), "invalid existing preshared key");
            result.preshared.insert(public_key.clone(), key.clone());
        }
        result.peers.push(Peer {
            public_key,
            allowed_ips: fields.get("allowedips").context("peer missing allowed IPs")?.clone(),
            endpoint: fields.get("endpoint").cloned(),
            persistent_keepalive: fields.get("persistentkeepalive").map(|v| {
                if v == "off" { Ok(0) } else { v.parse::<u16>() }
            }).transpose().context("invalid keepalive")?,
        });
        Ok(())
    }
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.eq_ignore_ascii_case("[Interface]") {
            ensure!(!interface_seen && peer.is_none(), "unsupported interface section ordering");
            interface_seen = true;
        } else if line.eq_ignore_ascii_case("[Peer]") {
            ensure!(interface_seen, "missing interface section");
            if let Some(fields) = peer.take() { finish(fields, &mut result)?; }
            peer = Some(BTreeMap::new());
            continue;
        } else if line.starts_with('[') {
            anyhow::bail!("unknown WireGuard section");
        }
        if let Some(ref mut fields) = peer {
            if line.is_empty() { continue; }
            let (key, value) = line.split_once('=').context("invalid peer directive")?;
            let key = key.trim().to_ascii_lowercase();
            ensure!(matches!(key.as_str(), "publickey" | "presharedkey" | "allowedips" | "endpoint" | "persistentkeepalive"), "unsupported peer directive");
            if key == "allowedips" {
                fields.entry(key).and_modify(|old| { old.push_str(", "); old.push_str(value.trim()); }).or_insert_with(|| value.trim().to_owned());
            } else {
                ensure!(fields.insert(key, value.trim().to_owned()).is_none(), "duplicate peer directive");
            }
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            match key.trim().to_ascii_lowercase().as_str() {
                "saveconfig" => ensure!(!value.trim().eq_ignore_ascii_case("true"), "disable SaveConfig before managing peers"),
                "address" => { if !result.address.is_empty() { result.address.push_str(", "); } result.address.push_str(value.trim()); }
                "listenport" => result.listen_port = Some(value.trim().parse().context("invalid listen port")?),
                _ => {}
            }
        }
        result.interface.push_str(raw);
        result.interface.push('\n');
    }
    if let Some(fields) = peer { finish(fields, &mut result)?; }
    ensure!(interface_seen, "missing WireGuard interface section");
    normalize_peers(&mut result.peers)?;
    Ok(result)
}

fn read_config() -> Result<String> {
    for path in ["/", "/etc", "/etc/wireguard"] { trusted_component(Path::new(path), true)?; }
    trusted_component(Path::new(WG_PATH), false)?;
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(WG_PATH)?;
    let meta = file.metadata()?;
    ensure!(meta.is_file() && meta.uid() == 0 && meta.mode() & 0o077 == 0 && meta.len() <= MAX_CONFIG as u64, "WireGuard config must be a private root-owned regular file");
    let mut bytes = Vec::new();
    file.take((MAX_CONFIG + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_CONFIG, "WireGuard config exceeds limit");
    String::from_utf8(bytes).context("invalid WireGuard config encoding")
}
fn revision(text: &str) -> String { format!("{:x}", Sha256::digest(text.as_bytes())) }
fn render(parsed: &Parsed, peers: &[Peer]) -> String {
    let mut output = parsed.interface.clone();
    for peer in peers {
        output.push_str(&format!("\n[Peer]\nPublicKey = {}\nAllowedIPs = {}\n", peer.public_key, peer.allowed_ips));
        if let Some(key) = parsed.preshared.get(&peer.public_key) { output.push_str(&format!("PresharedKey = {key}\n")); }
        if let Some(endpoint) = &peer.endpoint { output.push_str(&format!("Endpoint = {endpoint}\n")); }
        if let Some(keepalive) = peer.persistent_keepalive { output.push_str(&format!("PersistentKeepalive = {keepalive}\n")); }
    }
    output
}
fn publish(text: &str) -> Result<()> {
    let temp = Path::new("/etc/wireguard").join(format!(".maestro-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        private_write(&temp, text.as_bytes())?;
        fs::rename(&temp, WG_PATH)?;
        fs::File::open("/etc/wireguard")?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(&temp);
    result
}
async fn wg_running() -> Result<bool> {
    let wg = reference::program("wg").context("WireGuard tools unavailable")?;
    let result = reference::capture(&wg, &["show", "interfaces"], &[], 4).await?;
    ensure!(result.code == Some(0), "cannot determine WireGuard state");
    Ok(result.text.split_whitespace().any(|name| name == "wg0"))
}
async fn peers_read() -> Result<Value> {
    let text = read_config()?;
    let parsed = parse_config(&text)?;
    Ok(json!({"revision":revision(&text),"peers":parsed.peers,"address":parsed.address,"listen_port":parsed.listen_port,"is_running":wg_running().await?}))
}
async fn peers_write(mut input: PeerInput) -> Result<Value> {
    normalize_peers(&mut input.peers)?;
    let previous = read_config()?;
    ensure!(revision(&previous) == input.revision, "WireGuard configuration changed; refresh before saving");
    let parsed = parse_config(&previous)?;
    let next = render(&parsed, &input.peers);
    ensure!(next.len() <= MAX_CONFIG, "WireGuard config exceeds limit");
    let running = wg_running().await?;
    // wg-quick route creation/removal is a separate lifecycle operation.
    // Do not make live peer edits that leave its routes inconsistent.
    ensure!(!running, "stop WireGuard before editing peers; start it again to apply routes");
    ensure!(read_config()? == previous, "WireGuard configuration changed; refresh before saving");
    publish(&next)?;
    Ok(json!({"revision":revision(&next),"peers":input.peers,"address":parsed.address,"listen_port":parsed.listen_port,"is_running":false}))
}

pub(super) async fn execute(request: Request, policy: &Policy) -> Response {
    let allowed = match request.operation.as_str() {
        "ban_shield_apply" => policy.ban_shield,
        "wireguard_peers_read" | "wireguard_peers_write" => policy.wireguard_peers,
        _ => false,
    };
    if !allowed {
        return Response { error: "Network operation not authorized by root policy".into(), code: "permission_denied".into(), status: 403, ..Default::default() };
    }
    let result: Result<Value> = async {
        match request.operation.as_str() {
            "ban_shield_apply" => firewall(serde_json::from_value(request.arguments).map_err(|_| anyhow::anyhow!("invalid firewall arguments"))?).await,
            "wireguard_peers_read" => {
                ensure!(request.arguments.as_object().is_some_and(|args| args.is_empty()), "unexpected WireGuard read arguments");
                peers_read().await
            }
            "wireguard_peers_write" => peers_write(serde_json::from_value(request.arguments).map_err(|_| anyhow::anyhow!("invalid WireGuard peer arguments"))?).await,
            _ => anyhow::bail!("invalid operation"),
        }
    }.await;
    match result {
        Ok(value) => Response { result: Some(value), ..Default::default() },
        Err(error) => Response { error: error.to_string(), code: "operation_failed".into(), status: 400, ..Default::default() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn firewall_only_owns_its_table_and_validates_networks() {
        let input: FirewallInput = serde_json::from_value(json!({"enabled":true,"active":["192.0.2.0/24","2001:db8::/32"],"exceptions":["192.0.2.1/32"]})).unwrap();
        let script = nft_script(&input).unwrap();
        assert!(script.contains("ip saddr @allow4 return"));
        assert!(script.contains("ip6 saddr @ban6 drop"));
        assert!(!script.contains("flush ruleset"));
        assert!(serde_json::from_value::<FirewallInput>(json!({"enabled":true,"active":["1.2.3.4; flush ruleset"],"exceptions":[]})).is_err());
    }
    #[tokio::test]
    async fn network_grants_default_to_denied() {
        let policy: Policy = serde_json::from_value(json!({
            "version":1,"workload_uid":1000,"system_units":[]
        })).unwrap();
        for operation in ["ban_shield_apply", "wireguard_peers_read", "wireguard_peers_write"] {
            let response = execute(Request { operation: operation.into(), arguments: json!({}) }, &policy).await;
            assert_eq!(response.status, 403);
            assert!(response.result.is_none());
        }
    }

    #[test]
    fn peer_input_cannot_supply_shell_hooks_or_private_keys() {
        let key = STANDARD.encode([1u8; 32]);
        let input = json!({"revision":"test","peers":[{"public_key":key,"allowed_ips":"10.0.0.2/32","post_up":"shell command"}]});
        assert!(serde_json::from_value::<PeerInput>(input).is_err());
        let input = json!({"revision":"test","peers":[],"private_key":"secret","path":"/tmp/test"});
        assert!(serde_json::from_value::<PeerInput>(input).is_err());
    }

    #[test]
    fn peer_update_preserves_interface_and_existing_psk_without_exposing_secrets() {
        let key = STANDARD.encode([1u8;32]);
        let psk = STANDARD.encode([2u8;32]);
        let text = format!("[Interface]\nPrivateKey = secret\nAddress = 10.0.0.1/24\nPostUp = existing-hook\n[Peer]\nPublicKey = {key}\nPresharedKey = {psk}\nAllowedIPs = 10.0.0.2/32\n");
        let parsed = parse_config(&text).unwrap();
        let public = serde_json::to_string(&parsed.peers).unwrap();
        assert!(!public.contains(&psk));
        assert!(!public.contains("secret"));
        let next = render(&parsed, &parsed.peers);
        assert!(next.contains("PostUp = existing-hook"));
        assert!(next.contains(&psk));
        assert!(parse_config("[Interface]\nSaveConfig=true\n").is_err());
        assert!(!endpoint_valid("host:123\nPostUp=bad"));
    }
}
