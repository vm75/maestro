//! Frozen the reference implementation server-setting contract, independent of workload validity.
//! Startup can inspect settings while leaving broken workload definitions repairable.
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
};

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub listen: String,
    pub libvirt_uri: String,
    pub systemd_scope: String,
    pub samba_config: String,
    pub samba_managed_file: String,
    pub package_manager: String,
    pub state_dir: String,
    pub token_file: String,
    pub admin_username: String,
    pub admin_password_hash: String,
    pub public_origin: String,
    pub sample_interval_seconds: u64,
}

impl Settings {
    pub fn defaults(config_dir: &Path, state_dir: &Path) -> Self {
        Self {
            listen: "127.0.0.1:8080".into(),
            libvirt_uri: "qemu:///system".into(),
            systemd_scope: "system".into(),
            samba_config: "/etc/samba/smb.conf".into(),
            samba_managed_file: "/etc/samba/maestro.conf".into(),
            package_manager: "auto".into(),
            state_dir: state_dir.to_string_lossy().into_owned(),
            token_file: config_dir.join("token").to_string_lossy().into_owned(),
            admin_username: String::new(),
            admin_password_hash: String::new(),
            public_origin: String::new(),
            sample_interval_seconds: 2,
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.admin_username.is_empty() == self.admin_password_hash.is_empty(),
            "administrator username and hash must be configured together"
        );
        if !self.admin_username.is_empty() {
            ensure!(
                valid_username(&self.admin_username)
                    && valid_password_hash(&self.admin_password_hash),
                "invalid administrator username or password hash"
            );
        }
        ensure!(
            matches!(
                self.package_manager.as_str(),
                "auto" | "apt" | "dnf" | "pacman" | "zypper"
            ),
            "unsupported package manager"
        );
        ensure!(
            (1..=10).contains(&self.sample_interval_seconds),
            "sample interval must be between 1 and 10 seconds"
        );
        ensure!(
            matches!(
                self.libvirt_uri.as_str(),
                "qemu:///system" | "qemu:///session"
            ),
            "libvirt URI must be local"
        );
        ensure!(
            matches!(self.systemd_scope.as_str(), "system" | "user"),
            "invalid systemd scope"
        );
        ensure!(
            Path::new(&self.state_dir).is_absolute() && Path::new(&self.token_file).is_absolute(),
            "state and token paths must be absolute"
        );
        for path in [&self.samba_config, &self.samba_managed_file] {
            ensure!(
                literal_path(path)
                    && !matches!(
                        path.as_str(),
                        "/etc/exports" | "/etc/exports.d/maestro.exports"
                    ),
                "invalid Samba configuration path"
            );
        }
        ensure!(
            self.samba_config != self.samba_managed_file,
            "Samba managed fragment must differ from its main configuration"
        );
        let address = if let Some(port) = self.listen.strip_prefix("localhost:") {
            format!("127.0.0.1:{port}")
        } else {
            self.listen.clone()
        };
        let address: SocketAddr = address
            .parse()
            .map_err(|_| anyhow::anyhow!("listen must be a valid host:port"))?;
        ensure!(
            address.port() != 0,
            "listen port must be between 1 and 65535"
        );
        let wildcard = address.ip() == IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);
        ensure!(
            address.ip().is_loopback() || wildcard,
            "listen must be loopback or the IPv4 wildcard"
        );
        ensure!(
            !wildcard || !self.public_origin.is_empty(),
            "wildcard listen requires public origin"
        );
        if !self.public_origin.is_empty() {
            let origin = url::Url::parse(&self.public_origin)
                .map_err(|_| anyhow::anyhow!("invalid public origin"))?;
            let raw = self
                .public_origin
                .split_once("://")
                .map(|(_, rest)| rest)
                .ok_or_else(|| anyhow::anyhow!("invalid public origin"))?;
            let (authority, path) = raw.split_once('/').unwrap_or((raw, ""));
            ensure!(
                matches!(origin.scheme(), "http" | "https")
                    && origin.host_str().is_some()
                    && origin.username().is_empty()
                    && origin.password().is_none()
                    && origin.path() == "/"
                    && path.is_empty()
                    && !authority.contains('@')
                    && origin.query().is_none()
                    && origin.fragment().is_none()
                    && !self.public_origin.ends_with(':'),
                "public origin must be an HTTP(S) origin without a path, credentials, query or fragment"
            );
            if origin.scheme() == "http" {
                let authority: axum::http::uri::Authority = authority
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid public origin authority"))?;
                let ip = authority
                    .host()
                    .trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .ok();
                ensure!(
                    wildcard
                        && authority.port_u16() == Some(address.port())
                        && ip.is_some_and(global_unicast),
                    "HTTP public origin requires the wildcard listener, same explicit port and a non-loopback IP"
                );
            }
        }
        Ok(())
    }
}

fn global_unicast(ip: IpAddr) -> bool {
    !ip.is_unspecified()
        && !ip.is_loopback()
        && !ip.is_multicast()
        && match ip {
            IpAddr::V4(ip) => !ip.is_link_local() && ip != std::net::Ipv4Addr::BROADCAST,
            IpAddr::V6(ip) => (ip.segments()[0] & 0xffc0) != 0xfe80,
        }
}

fn literal_path(path: &str) -> bool {
    path.starts_with('/')
        && path.trim() == path
        && !path.contains(['\\', '%'])
        && !path.chars().any(char::is_control)
        && (path == "/"
            || path[1..]
                .split('/')
                .all(|part| !matches!(part, "" | "." | "..")))
}

pub fn valid_username(name: &str) -> bool {
    name.len() <= 64
        && name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

pub fn valid_password_hash(encoded: &str) -> bool {
    let parts: Vec<_> = encoded.split('$').collect();
    parts.len() == 4
        && parts[0] == "pbkdf2-sha256"
        && parts[1] == "600000"
        && STANDARD_NO_PAD
            .decode(parts[2])
            .is_ok_and(|bytes| bytes.len() == 16)
        && STANDARD_NO_PAD
            .decode(parts[3])
            .is_ok_and(|bytes| bytes.len() == 32)
}

/// Validate document structure and server settings without interpreting resources.
/// Defaults are supplied by the caller so inspection never depends on process env.
pub fn read(input: &[u8], defaults: &Settings) -> Result<Option<Settings>> {
    ensure!(
        input.len() <= super::MAX_CONFIG_BYTES,
        "configuration exceeds 1 MiB"
    );
    super::yaml::check(input)?;
    let value: Value = serde_yaml::from_slice(input)
        .map_err(|_| anyhow::anyhow!("invalid or duplicate YAML mapping key"))?;
    ensure!(
        value.is_mapping() && super::string_keys(&value),
        "configuration must be a string-keyed mapping"
    );
    let root = value.as_mapping().unwrap();
    for key in root.keys() {
        ensure!(
            matches!(
                key.as_str(),
                Some(
                    "version"
                        | "server"
                        | "services"
                        | "storage"
                        | "shares"
                        | "bindings"
                        | "secrets"
                )
            ),
            "unknown configuration field"
        );
    }
    ensure!(
        value.get("version").and_then(Value::as_i64) == Some(1),
        "unsupported configuration version"
    );
    let Some(server) = value.get("server").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let server = server
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("server settings must be a mapping"))?;
    let mut settings = serde_yaml::to_value(defaults)?;
    for (key, value) in server {
        // Retired gates remain readable in existing authorities, but have no effect.
        if matches!(key.as_str(), Some("allow_writes" | "privileged_operations")) {
            ensure!(value.is_bool(), "retired server flags must be booleans");
            continue;
        }
        settings
            .as_mapping_mut()
            .unwrap()
            .insert(key.clone(), value.clone());
    }
    let settings: Settings = serde_yaml::from_value(settings)
        .map_err(|_| anyhow::anyhow!("unknown server field or incorrect field type"))?;
    settings.validate()?;
    Ok(Some(settings))
}

/// Read the private startup authority without following a final symlink or
/// blocking on a special file. Invalid workloads do not prevent settings repair.
pub fn load(path: &Path, defaults: &Settings) -> Result<Settings> {
    use std::{
        io::Read,
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };
    ensure!(path.is_absolute(), "configuration path must be absolute");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| anyhow::anyhow!("could not open private configuration"))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.mode() & 0o077 == 0
            && metadata.uid() == unsafe { libc::geteuid() },
        "configuration must be an owner-only regular file owned by the workload account"
    );
    ensure!(
        metadata.len() <= super::MAX_CONFIG_BYTES as u64,
        "configuration exceeds 1 MiB"
    );
    let mut bytes = Vec::new();
    file.take(super::MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let settings = read(&bytes, defaults)?.unwrap_or_else(|| defaults.clone());
    settings.validate()?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn defaults() -> Settings {
        Settings::defaults(Path::new("/private/config"), Path::new("/private/state"))
    }

    #[test]
    fn server_settings_remain_readable_with_broken_workloads() {
        let settings = read(b"version: 1\nserver: {listen: '127.0.0.1:8181'}\nservices: {broken: {unknown: repair-me}}\n", &defaults()).unwrap().unwrap();
        assert_eq!(settings.listen, "127.0.0.1:8181");
        assert_eq!(settings.token_file, "/private/config/token");
        assert!(read(b"version: 1\n", &defaults()).unwrap().is_none());
    }

    #[test]
    fn pinned_server_validation_cases() {
        for (server, valid) in [
            ("{}", true),
            ("{privileged_operations: true}", true),
            ("{privileged_operations: true, package_manager: apt}", true),
            ("{listen: '[::1]:8081'}", true),
            ("{listen: ':8080'}", false),
            ("{listen: '127.0.0.1:70000'}", false),
            ("{listen: '0.0.0.0:8080'}", false),
            (
                "{listen: '0.0.0.0:8080', public_origin: 'http://192.168.1.2:8080'}",
                true,
            ),
            (
                "{listen: '0.0.0.0:8080', public_origin: 'http://127.0.0.1:8080'}",
                false,
            ),
            ("{public_origin: 'https://server.example.com'}", true),
            (
                "{public_origin: 'https://server.example.com/private/..'}",
                false,
            ),
            ("{public_origin: 'https://@server.example.com'}", false),
            (
                "{listen: '0.0.0.0:80', public_origin: 'http://192.168.1.2:80'}",
                true,
            ),
            (
                "{public_origin: 'https://server.example.com/private'}",
                false,
            ),
            ("{public_origin: 'http://server.example.com'}", false),
            ("{state_dir: relative}", false),
            ("{sample_interval_seconds: 0}", false),
            ("{sample_interval_seconds: 11}", false),
            ("{sample_interval_seconds: 5}", true),
            ("{systemd_scope: user}", true),
            ("{systemd_scope: global}", false),
            ("{libvirt_uri: 'qemu+ssh://root@example/system'}", false),
            ("{libvirt_uri: 'qemu:///session'}", true),
            ("{samba_managed_file: '/srv/%U.conf'}", false),
            ("{samba_managed_file: '/etc/exports'}", false),
            (
                "{samba_config: '/srv/smb.conf', samba_managed_file: '/srv/smb.conf'}",
                false,
            ),
            ("{package_manager: dnf}", true),
            ("{password: private-value}", false),
            ("{admin_username: admin}", false),
        ] {
            let body = format!("version: 1\nserver: {server}\n");
            assert_eq!(
                read(body.as_bytes(), &defaults()).is_ok(),
                valid,
                "{server}"
            );
        }
    }

    #[test]
    fn retired_write_gates_are_ignored_and_not_serialized() {
        let settings = read(b"version: 1\nserver: {allow_writes: false, privileged_operations: false}\n", &defaults()).unwrap().unwrap();
        assert!(settings == defaults());
        let yaml = serde_yaml::to_string(&settings).unwrap();
        assert!(!yaml.contains("allow_writes"));
        assert!(!yaml.contains("privileged_operations"));
    }

    #[test]
    fn ambiguous_and_secret_bearing_errors_are_rejected_without_echoing_values() {
        for body in [
            "version: 1\nserver: {allow_writes: false, allow_writes: true}",
            "version: 1\nserver: &x {listen: private-value}",
            "version: 1\nserver: {listen: private-value}",
            "version: 1\nserver: {password: private-value}",
            "version: 1\n---\nversion: 1",
            "version: 1\nmanifestPath: private-value",
        ] {
            let error = read(body.as_bytes(), &defaults())
                .err()
                .expect("invalid input accepted");
            assert!(!error.to_string().contains("private-value"));
        }
    }

    #[test]
    fn administrator_hash_contract_is_fixed_and_bounded() {
        let hash = format!(
            "pbkdf2-sha256$600000${}${}",
            STANDARD_NO_PAD.encode([7; 16]),
            STANDARD_NO_PAD.encode([8; 32])
        );
        assert!(valid_password_hash(&hash));
        assert!(!valid_password_hash(&hash.replace("600000", "1")));
        assert!(!valid_password_hash(&(hash + "$extra")));
        assert!(valid_username("Admin-1.name"));
        assert!(!valid_username("1admin"));
        assert!(!valid_username("admin name"));
    }

    #[test]
    fn startup_authority_is_private_regular_and_not_a_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("maestro.yaml");
        std::fs::write(&path, "version: 1\nserver: {listen: '127.0.0.1:8181'}\nservices: {broken: {unknown: repair-me}}\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(load(&path, &defaults()).unwrap().listen, "127.0.0.1:8181");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(load(&link, &defaults()).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&path, &defaults()).is_err());
        assert!(load(root.path(), &defaults()).is_err());
    }
}
