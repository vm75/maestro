//! Read-only native shares inventory, translated from Server Man's Samba/NFS
//! parsers and inventory. This module never reloads daemons or publishes files.
//! Ownership remains explicitly unverified until the transactional helper is ported.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{ErrorKind, Read},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use crate::{command::CommandRunner, state::AppState};

const LIMIT: usize = 1024 * 1024;

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Definition {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) read_only: bool,
    pub(crate) guest: bool,
    pub(crate) browseable: bool,
    pub(crate) users: Vec<String>,
    pub(crate) clients: Vec<String>,
    pub(crate) root_squash: bool,
    pub(crate) security: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeShare {
    #[serde(flatten)]
    definition: Definition,
    protocol: &'static str,
    managed: bool,
    ownership: &'static str,
    configured: bool,
    runtime: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    connections: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Backend {
    pub protocol: &'static str,
    pub available: bool,
    pub discovery_available: bool,
    pub can_manage: bool,
    pub reason: &'static str,
    pub managed_file: String,
    pub revision: String,
    pub setup_required: bool,
    pub notice: &'static str,
}

#[derive(Default, Serialize)]
pub struct Inventory {
    pub backends: Vec<Backend>,
    pub shares: Vec<NativeShare>,
    pub warnings: Vec<&'static str>,
}

pub(crate) fn share_key(name: &str) -> String {
    name.split_whitespace().collect::<String>().to_lowercase()
}

pub(crate) fn parse_samba(text: &str) -> Result<Vec<Definition>> {
    let mut defaults = Definition {
        read_only: true,
        browseable: true,
        ..Definition::default()
    };
    let mut result: Vec<Definition> = Vec::new();
    let mut global = true;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            if share_key(name) == "global" {
                global = true;
            } else {
                ensure!(result.len() < 1024, "Samba inventory exceeds 1024 shares");
                let mut share = defaults.clone();
                share.name = name.trim().into();
                result.push(share);
                global = false;
            }
            continue;
        }
        let (key, value) = line.split_once('=').context("unsupported Samba output")?;
        let value = value.trim();
        let yes = matches!(value.to_ascii_lowercase().as_str(), "yes" | "true" | "1");
        let current = if global {
            &mut defaults
        } else {
            result.last_mut().unwrap()
        };
        match share_key(key).as_str() {
            "path" => current.path = value.into(),
            "readonly" => current.read_only = yes,
            "guestok" => current.guest = yes,
            "browseable" | "browsable" => current.browseable = yes,
            "validusers" => {
                current.users = value
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            _ => {} // No arbitrary native settings or credentials are exposed.
        }
    }
    Ok(result)
}

#[derive(Clone)]
pub(crate) struct Export {
    pub(crate) path: String,
    pub(crate) client: String,
    pub(crate) options: Vec<String>,
}

impl Export {
    fn key(&self) -> (String, String) {
        (self.path.clone(), self.client.clone())
    }
}

fn export_fields(line: &str) -> Result<Vec<String>> {
    let mut fields = Vec::new();
    let mut token = Vec::new();
    let mut quoted = false;
    let mut bytes = line.as_bytes().iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        match byte {
            b'"' => quoted = !quoted,
            b'#' if !quoted => break,
            b'\\' => {
                let mut next = bytes.clone();
                let digits: Vec<_> = next.by_ref().take(3).collect();
                if digits.len() == 3 && digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                    let decoded = u8::from_str_radix(std::str::from_utf8(&digits)?, 8)
                        .context("invalid NFS path escape")?;
                    token.push(decoded);
                    bytes = next;
                } else {
                    token.push(byte);
                }
            }
            b' ' | b'\t' | b'\r' if !quoted => {
                if !token.is_empty() {
                    fields.push(String::from_utf8(std::mem::take(&mut token))?);
                }
            }
            _ => token.push(byte),
        }
    }
    ensure!(!quoted, "unterminated export path quote");
    if !token.is_empty() {
        fields.push(String::from_utf8(token)?);
    }
    Ok(fields)
}

/// As in netip.Prefix.Masked, IP selectors use a canonical network prefix.
pub(crate) fn canonical_client(value: &str) -> String {
    let (address, bits) = value.split_once('/').unwrap_or((value, ""));
    let Ok(address) = address.parse::<IpAddr>() else {
        return value.into();
    };
    let max = if address.is_ipv4() { 32 } else { 128 };
    let bits = if bits.is_empty() {
        max
    } else {
        let Ok(bits) = bits.parse::<u32>() else {
            return value.into();
        };
        if bits > max {
            return value.into();
        }
        bits
    };
    let address = match address {
        IpAddr::V4(addr) => IpAddr::V4(Ipv4Addr::from(if bits == 0 {
            0
        } else {
            u32::from(addr) & (u32::MAX << (32 - bits))
        })),
        IpAddr::V6(addr) => IpAddr::V6(Ipv6Addr::from(if bits == 0 {
            0
        } else {
            u128::from(addr) & (u128::MAX << (128 - bits))
        })),
    };
    format!("{address}/{bits}")
}

pub(crate) fn parse_exports(text: &str) -> Result<Vec<Export>> {
    let mut result = Vec::new();
    for line in text.replace("\\\n", "").lines() {
        let fields = export_fields(line)?;
        if fields.is_empty() {
            continue;
        }
        ensure!(
            fields.len() >= 2 && Path::new(&fields[0]).is_absolute(),
            "unsupported NFS export line"
        );
        let mut defaults = Vec::new();
        for field in &fields[1..] {
            if let Some(options) = field.strip_prefix('-') {
                defaults = options.split(',').map(str::to_owned).collect();
                continue;
            }
            let (client, options) = if let Some((client, options)) = field.split_once('(') {
                let options = options
                    .strip_suffix(')')
                    .context("unsupported NFS export options")?;
                let mut merged = defaults.clone();
                merged.extend(options.split(',').map(str::to_owned));
                (client, merged)
            } else {
                (field.as_str(), defaults.clone())
            };
            ensure!(
                !client.is_empty() && result.len() < 4096,
                "invalid or oversized NFS inventory"
            );
            result.push(Export {
                path: fields[0].clone(),
                client: canonical_client(client),
                options,
            });
        }
    }
    Ok(result)
}

pub(crate) fn option_flag(options: &[String], yes: &str, no: &str, default: bool) -> bool {
    let mut value = default;
    for option in options {
        if option == yes {
            value = true;
        }
        if option == no {
            value = false;
        }
    }
    value
}

pub(crate) fn option_value<'a>(options: &'a [String], key: &str, default: &'a str) -> &'a str {
    options
        .iter()
        .rev()
        .find_map(|option| {
            let (name, value) = option.split_once('=')?;
            (name == key).then_some(value)
        })
        .unwrap_or(default)
}

fn read_file(path: &Path) -> Result<Option<String>> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.len() <= LIMIT as u64,
        "invalid native share file"
    );
    let mut bytes = Vec::new();
    file.take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= LIMIT, "native share file exceeds 1 MiB");
    Ok(Some(String::from_utf8(bytes)?))
}

fn fragment_revision(path: &Path) -> Result<String> {
    let text = read_file(path)?;
    let mut bytes = vec![u8::from(text.is_some())];
    if let Some(text) = text {
        bytes.extend(text.into_bytes());
    }
    Ok(super::revision(&bytes))
}

fn program(name: &str) -> Option<PathBuf> {
    ["/usr/bin", "/usr/sbin", "/bin", "/sbin"]
        .into_iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|path| path.is_file())
}

async fn capture(runner: &CommandRunner, name: &str, args: &[&str]) -> Result<String> {
    let path = program(name).context("native share utility is unavailable")?;
    let data = runner
        .capture_bounded(&path, args, Duration::from_secs(5), LIMIT)
        .await?;
    Ok(String::from_utf8(data)?)
}

fn literal_include(text: &str, fragment: &str) -> bool {
    text.lines()
        .filter(|line| {
            let line = line.trim();
            !line.starts_with(['#', ';'])
                && line.split_once('=').is_some_and(|(key, value)| {
                    share_key(key) == "include" && value.trim() == fragment
                })
        })
        .count()
        == 1
}

fn nfs_configured(main: &Path, directory: &Path) -> Result<Vec<Export>> {
    let mut paths = vec![main.to_owned()];
    match fs::read_dir(directory) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with('.') && name.ends_with(".exports") {
                    ensure!(paths.len() < 65, "NFS inventory exceeds 64 fragments");
                    paths.push(entry.path());
                }
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut result = Vec::new();
    let mut total = 0;
    for path in paths {
        if let Some(text) = read_file(&path)? {
            total += text.len();
            ensure!(total <= LIMIT, "NFS configuration exceeds 1 MiB");
            result.extend(parse_exports(&text)?);
            ensure!(
                result.len() <= 4096,
                "NFS configured inventory exceeds 4096 entries"
            );
        }
    }
    Ok(result)
}

pub(crate) fn connections(text: &str) -> Result<BTreeMap<String, usize>> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    let sessions = value
        .get("tcons")
        .and_then(serde_json::Value::as_object)
        .context("unsupported Samba status JSON")?;
    ensure!(
        sessions.len() <= 4096,
        "Samba connections exceed inventory limit"
    );
    let mut result = BTreeMap::new();
    for session in sessions.values() {
        if let Some(name) = session.get("service").and_then(serde_json::Value::as_str) {
            *result.entry(share_key(name)).or_insert(0) += 1;
        }
    }
    Ok(result)
}

fn native_row(
    definition: Definition,
    protocol: &'static str,
    configured: bool,
    runtime: &'static str,
    connections: Option<usize>,
) -> NativeShare {
    NativeShare {
        definition,
        protocol,
        managed: false,
        ownership: "unverified",
        configured,
        runtime,
        connections,
    }
}

pub async fn inventory(state: &AppState, filter: Option<&str>) -> Inventory {
    let mut result = Inventory::default();
    let simulated = state.config.test_mode;
    for protocol in ["samba", "nfs"] {
        if filter.is_some_and(|value| value != protocol) {
            continue;
        }
        let samba = protocol == "samba";
        let managed = if samba {
            &state.config.paths.samba_conf
        } else {
            &state.config.paths.exports_file
        };
        let available =
            !simulated && program(if samba { "testparm" } else { "exportfs" }).is_some();
        let mut backend = Backend {
            protocol,
            available,
            discovery_available: false,
            can_manage: false,
            reason: if simulated {
                "Native discovery is disabled in test mode."
            } else if !available {
                "Native share utility is unavailable."
            } else {
                "Native publication requires the verified optional helper; ownership is unverified."
            },
            managed_file: managed.to_string_lossy().into_owned(),
            revision: String::new(),
            setup_required: false,
            notice: if samba {
                "Connections show current use, not verified client access or ownership."
            } else {
                "Registered exports do not establish a verified client mount."
            },
        };
        if simulated {
            result.backends.push(backend);
            continue;
        }
        match fragment_revision(managed) {
            Ok(revision) => backend.revision = revision,
            Err(_) => result
                .warnings
                .push("Managed fragment is inaccessible or is a symlink."),
        }
        if samba {
            let main = state
                .config
                .linux_authority
                .as_ref()
                .map_or("/etc/samba/smb.conf", |authority| {
                    authority.settings.samba_config.as_str()
                });
            let main_data = read_file(Path::new(main)).ok().flatten();
            backend.setup_required = !main_data
                .as_ref()
                .is_some_and(|text| literal_include(text, &backend.managed_file));
            if backend.setup_required {
                result.warnings.push(
                    "Samba main configuration is unavailable or lacks the literal managed include.",
                );
            }
            let definitions = if available && main_data.is_some() {
                capture(&state.runner, "testparm", &["--suppress-prompt", main])
                    .await
                    .and_then(|text| parse_samba(&text))
                    .unwrap_or_else(|_| {
                        result
                            .warnings
                            .push("Effective Samba configuration could not be inspected.");
                        Vec::new()
                    })
            } else {
                Vec::new()
            };
            let connected = capture(
                &state.runner,
                "smbstatus",
                &["--json", "--shares", "--configfile", main],
            )
            .await
            .and_then(|text| connections(&text))
            .ok();
            if connected.is_none() {
                result
                    .warnings
                    .push("Samba connection state is unavailable.");
            }
            let mut reported = BTreeSet::new();
            for definition in definitions {
                let key = share_key(&definition.name);
                reported.insert(key.clone());
                let count = connected.as_ref().map(|map| *map.get(&key).unwrap_or(&0));
                let runtime = match count {
                    Some(0) => "not_observed",
                    Some(_) => "connected",
                    None => "unknown",
                };
                result
                    .shares
                    .push(native_row(definition, protocol, true, runtime, count));
            }
            for (name, count) in connected.into_iter().flatten() {
                if count > 0 && !reported.contains(&name) {
                    result.shares.push(native_row(
                        Definition {
                            name,
                            read_only: true,
                            ..Definition::default()
                        },
                        protocol,
                        false,
                        "connected",
                        Some(count),
                    ));
                }
            }
        } else {
            let directory = managed.parent().unwrap_or(Path::new("/etc/exports.d"));
            backend.setup_required = !directory.is_dir();
            let configured =
                nfs_configured(Path::new("/etc/exports"), directory).unwrap_or_else(|_| {
                    result
                        .warnings
                        .push("NFS configured exports are unavailable or incomplete.");
                    Vec::new()
                });
            let active = if available {
                capture(&state.runner, "exportfs", &["-s"])
                    .await
                    .and_then(|text| parse_exports(&text))
                    .ok()
            } else {
                None
            };
            if active.is_none() {
                result
                    .warnings
                    .push("NFS registered export table is unavailable.");
            }
            let configured_keys: BTreeSet<_> = configured.iter().map(Export::key).collect();
            let active_keys: BTreeSet<_> = active.iter().flatten().map(Export::key).collect();
            let mut seen = BTreeSet::new();
            for export in configured
                .into_iter()
                .chain(active.iter().flatten().cloned())
            {
                let key = export.key();
                if !seen.insert(key.clone()) {
                    continue;
                }
                let runtime = if active.is_none() {
                    "unknown"
                } else if active_keys.contains(&key) {
                    "exported"
                } else {
                    "not_exported"
                };
                let definition = Definition {
                    name: export.path.clone(),
                    path: export.path,
                    read_only: option_flag(&export.options, "ro", "rw", true),
                    root_squash: option_flag(
                        &export.options,
                        "root_squash",
                        "no_root_squash",
                        true,
                    ),
                    security: option_value(&export.options, "sec", "sys").into(),
                    clients: vec![export.client],
                    ..Definition::default()
                };
                result.shares.push(native_row(
                    definition,
                    protocol,
                    configured_keys.contains(&key),
                    runtime,
                    None,
                ));
            }
        }
        result.backends.push(backend);
    }
    result.shares.sort_by(|left, right| {
        left.protocol
            .cmp(right.protocol)
            .then_with(|| left.definition.name.cmp(&right.definition.name))
    });
    // Native inventory must be bounded even when the configuration has large values.
    if serde_json::to_vec(&result).map_or(true, |bytes| bytes.len() > LIMIT) {
        result.shares.clear();
        result
            .warnings
            .push("Native share inventory exceeds the response size limit.");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samba_inherits_defaults_and_does_not_expose_unmodeled_options() {
        let definitions = parse_samba("[global]\nread only = yes\nvalid users = alice, @media\n[Docs]\npath = /srv/docs\nread only = no\nunmodeled = SECRET\n[Public]\npath = /srv/pub\nguest ok = yes\n").unwrap();
        assert_eq!(definitions.len(), 2);
        assert_eq!(definitions[0].name, "Docs");
        assert_eq!(definitions[0].users, ["alice", "@media"]);
        assert!(!definitions[0].read_only);
        assert!(definitions[1].read_only && definitions[1].guest);
        assert!(
            !serde_json::to_string(&definitions)
                .unwrap()
                .contains("SECRET")
        );
    }

    #[test]
    fn nfs_parses_escaped_paths_defaults_and_canonical_selectors() {
        let exports =
            parse_exports("/srv/my\\040disk -ro 192.168.1.4/24(rw,sec=sys) 2001:db8::1(ro)\n")
                .unwrap();
        assert_eq!(exports.len(), 2);
        assert_eq!(exports[0].path, "/srv/my disk");
        assert_eq!(exports[0].client, "192.168.1.0/24");
        assert_eq!(exports[1].client, "2001:db8::1/128");
        assert!(!option_flag(&exports[0].options, "ro", "rw", true));
        assert!(parse_exports("\"/srv/missing *(rw)").is_err());
        assert!(parse_exports("/srv/data *(rw").is_err());
    }

    #[test]
    fn literal_samba_include_and_fragment_metadata_are_conservative() {
        assert!(literal_include(
            "[global]\ninclude = /etc/managed.conf",
            "/etc/managed.conf"
        ));
        assert!(!literal_include(
            "include = /etc/managed.conf\ninclude=/etc/managed.conf",
            "/etc/managed.conf"
        ));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fragment");
        let missing = fragment_revision(&path).unwrap();
        fs::write(&path, "").unwrap();
        assert_ne!(fragment_revision(&path).unwrap(), missing);
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", &path).unwrap();
        assert!(fragment_revision(&path).is_err());
    }
}
