//! Direct port of the reference implementation shares/validate.go, definitions.go and files.go.
//! Shared typed definitions and exact managed-fragment ownership checks.
use std::{
    collections::BTreeSet,
    fs,
    io::{ErrorKind, Read},
    net::IpAddr,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};

use super::native_shares::{Definition, canonical_client};

pub(crate) const MAX_FILE: usize = 1 << 20;
const HEADER: &str = "# Managed by maestro. Do not edit.\n# maestro-format: 1\n";
const METADATA: &str = "# maestro-share: ";

pub(crate) fn decode(raw: &str) -> Result<Vec<Definition>> {
    use serde::Deserialize;
    let mut decoder = serde_json::Deserializer::from_str(raw);
    let definitions = Option::<Vec<Definition>>::deserialize(&mut decoder)
        .map_err(|_| anyhow::anyhow!("invalid share definitions"))?;
    ensure!(decoder.end().is_ok(), "expected one share definition array");
    Ok(definitions.unwrap_or_default())
}

fn valid_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 4096
        && path.trim() == path
        && !path
            .chars()
            .any(|c| c.is_control() || "\\%\"'[]#;(),".contains(c))
        && (path == "/"
            || path[1..]
                .split('/')
                .all(|part| !matches!(part, "" | "." | "..")))
}

fn client(value: &str) -> Result<String> {
    if value == "*" {
        return Ok(value.into());
    }
    let valid = match value.split_once('/') {
        None => value.parse::<IpAddr>().is_ok(),
        Some((addr, bits)) => addr.parse::<IpAddr>().is_ok_and(|addr| {
            !bits.is_empty()
                && bits.bytes().all(|c| c.is_ascii_digit())
                && bits
                    .parse::<u32>()
                    .is_ok_and(|bits| bits <= if addr.is_ipv4() { 32 } else { 128 })
        }),
    };
    ensure!(
        valid,
        "NFS clients must be IP addresses, CIDR networks or an explicit *"
    );
    Ok(canonical_client(value))
}

fn normalize_one(protocol: &str, mut d: Definition, check_path: bool) -> Result<Definition> {
    let name = regex::Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,63}$").unwrap();
    ensure!(
        name.is_match(&d.name)
            && !matches!(
                d.name.to_ascii_lowercase().as_str(),
                "global" | "homes" | "printers" | "ipc$"
            ),
        "Share name must be a portable identifier; reserved Samba sections are unsupported"
    );
    d.name.make_ascii_lowercase();
    ensure!(
        valid_path(&d.path),
        "Share path must be a clean absolute literal directory path without configuration metacharacters"
    );
    if check_path {
        let real = fs::canonicalize(&d.path).map_err(|_| {
            anyhow::anyhow!("Share directory is inaccessible or resolves to an unsupported path")
        })?;
        let real = real
            .to_str()
            .filter(|path| valid_path(path))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Share directory is inaccessible or resolves to an unsupported path"
                )
            })?;
        ensure!(
            Path::new(real).is_dir(),
            "Share path must be an existing directory"
        );
        d.path = real.into();
    }
    if protocol == "samba" {
        ensure!(
            d.clients.is_empty() && d.security.is_empty() && !d.root_squash,
            "NFS settings cannot be used in a Samba share"
        );
        ensure!(
            d.users.len() <= 32 && (d.guest || !d.users.is_empty()),
            "Samba requires explicit users/groups unless guest access is enabled; limit 32"
        );
        ensure!(
            !d.guest || d.users.is_empty(),
            "Guest shares must not also specify a valid-users restriction"
        );
        let user = regex::Regex::new(r"^@?[a-zA-Z_][a-zA-Z0-9_.-]{0,63}\$?$").unwrap();
        ensure!(
            d.users.iter().all(|name| user.is_match(name)),
            "Samba users must be simple account names or @group names"
        );
        d.users.sort();
        d.users.dedup();
    } else {
        ensure!(
            !d.guest && !d.browseable && d.users.is_empty(),
            "Samba settings cannot be used in an NFS export"
        );
        ensure!(
            matches!(d.security.as_str(), "sys" | "krb5" | "krb5i" | "krb5p"),
            "Unsupported NFS security flavor"
        );
        ensure!(
            (1..=16).contains(&d.clients.len()),
            "NFS requires 1 to 16 explicit clients/networks"
        );
        d.clients = d
            .clients
            .iter()
            .map(|value| client(value))
            .collect::<Result<_>>()?;
        d.clients.sort();
        d.clients.dedup();
    }
    Ok(d)
}

pub(crate) fn normalize(
    protocol: &str,
    definitions: Vec<Definition>,
    check_paths: bool,
) -> Result<Vec<Definition>> {
    ensure!(
        matches!(protocol, "samba" | "nfs"),
        "unsupported share protocol"
    );
    ensure!(definitions.len() <= 64, "share limit exceeded");
    let mut names = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut result = Vec::new();
    for definition in definitions {
        let d = normalize_one(protocol, definition, check_paths)?;
        ensure!(
            names.insert(d.name.clone()) && (protocol != "nfs" || paths.insert(d.path.clone())),
            "duplicate share name or NFS export path"
        );
        result.push(d);
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

pub(crate) fn options(d: &Definition) -> String {
    format!(
        "{},sync,{},no_all_squash,no_subtree_check,secure,sec={}",
        if d.read_only { "ro" } else { "rw" },
        if d.root_squash {
            "root_squash"
        } else {
            "no_root_squash"
        },
        d.security
    )
}

pub(crate) fn render(protocol: &str, definitions: &[Definition]) -> Result<Vec<u8>> {
    use std::fmt::Write;
    ensure!(
        definitions.len() <= 64,
        "Managed fragments are limited to 64 shares"
    );
    let mut definitions: Vec<_> = definitions.iter().collect();
    definitions.sort_by(|a, b| a.name.cmp(&b.name));
    let mut text = HEADER.to_owned();
    for d in definitions {
        // Go encoding/json escapes HTML and the two Unicode line separators.
        let data = serde_json::to_string(d)?
            .replace('&', "\\u0026")
            .replace('<', "\\u003c")
            .replace('>', "\\u003e")
            .replace('\u{2028}', "\\u2028")
            .replace('\u{2029}', "\\u2029");
        writeln!(text, "{METADATA}{data}")?;
        if protocol == "samba" {
            let yes = |value| if value { "yes" } else { "no" };
            writeln!(
                text,
                "[{}]\n\tpath = {}\n\tread only = {}\n\tguest ok = {}\n\tbrowseable = {}\n\tvalid users = {}",
                d.name,
                d.path,
                yes(d.read_only),
                yes(d.guest),
                yes(d.browseable),
                d.users.join(" ")
            )?;
        } else {
            text.push_str(&d.path.replace(' ', r"\040"));
            for selector in &d.clients {
                write!(text, " {selector}({})", options(d))?;
            }
            text.push('\n');
        }
        text.push('\n');
    }
    ensure!(text.len() <= MAX_FILE, "Managed fragment exceeds 1 MiB");
    Ok(text.into_bytes())
}

#[derive(Default)]
pub(crate) struct Snapshot {
    pub data: Vec<u8>,
    pub exists: bool,
    pub mode: u32,
}

impl Snapshot {
    pub(crate) fn revision(&self) -> String {
        let mut hash = Sha256::new();
        hash.update([u8::from(self.exists)]);
        hash.update(&self.data);
        format!("{:x}", hash.finalize())
    }
}

pub(crate) fn read_file(path: &Path, no_symlink: bool) -> Result<Snapshot> {
    let flags = libc::O_NONBLOCK | libc::O_CLOEXEC | if no_symlink { libc::O_NOFOLLOW } else { 0 };
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Snapshot::default()),
        Err(error) => return Err(error.into()),
    };
    let info = file.metadata()?;
    ensure!(
        info.is_file() && info.len() <= MAX_FILE as u64,
        "configuration must be a regular file at most 1 MiB"
    );
    let mut data = Vec::new();
    file.take(MAX_FILE as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|_| anyhow::anyhow!("configuration could not be read within 1 MiB"))?;
    ensure!(
        data.len() <= MAX_FILE,
        "configuration could not be read within 1 MiB"
    );
    Ok(Snapshot {
        data,
        exists: true,
        mode: info.mode() & 0o777,
    })
}

pub(crate) fn load(protocol: &str, snapshot: &Snapshot) -> Result<Vec<Definition>> {
    if !snapshot.exists {
        return Ok(Vec::new());
    }
    ensure!(
        snapshot.data.starts_with(HEADER.as_bytes()),
        "managed file is not owned by maestro; preserve it and choose an empty unused location"
    );
    let mut definitions = Vec::new();
    let mut names = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for line in String::from_utf8_lossy(&snapshot.data).split('\n') {
        let Some(raw) = line.strip_prefix(METADATA) else {
            continue;
        };
        let d: Definition = serde_json::from_str(raw)
            .map_err(|_| anyhow::anyhow!("managed share metadata is invalid"))?;
        let d = normalize_one(protocol, d, false)
            .map_err(|_| anyhow::anyhow!("managed share metadata is unsupported"))?;
        ensure!(
            names.insert(d.name.clone()) && (protocol != "nfs" || paths.insert(d.path.clone())),
            "duplicate managed share identity or export path"
        );
        definitions.push(d);
    }
    ensure!(
        render(protocol, &definitions).is_ok_and(|bytes| bytes == snapshot.data),
        "managed file was edited outside maestro; changes will not be overwritten"
    );
    Ok(definitions)
}

/// Temporary native configuration in its destination directory. Create without
/// following links, and clean up on every return, including cancelled futures.
pub(crate) struct Candidate(std::path::PathBuf);

impl Candidate {
    pub(crate) fn new(directory: &Path, data: &[u8], mode: u32) -> Result<Self> {
        Self::new_for_owner(directory, data, mode, 0)
    }

    fn new_for_owner(directory: &Path, data: &[u8], mode: u32, owner: u32) -> Result<Self> {
        use std::io::Write;
        let metadata = fs::symlink_metadata(directory)?;
        ensure!(
            metadata.is_dir() && metadata.uid() == owner && metadata.mode() & 0o022 == 0,
            "native share configuration directory is not trusted"
        );
        let path = directory.join(format!(".maestro-{}", uuid::Uuid::new_v4()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        let candidate = Self(path);
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.write_all(data)?;
        file.sync_all()?;
        Ok(candidate)
    }

    /// Atomic rename followed by directory fsync. Callers must handle the
    /// possibility that the rename succeeded but directory sync failed.
    pub(crate) fn publish(&self, path: &Path) -> Result<()> {
        ensure!(
            self.0.parent() == path.parent(),
            "candidate destination directory changed"
        );
        fs::rename(&self.0, path)?;
        sync_dir(path)
    }

    pub(crate) fn text(&self) -> &str {
        self.0
            .to_str()
            .expect("fixed native configuration directory and ASCII candidate name")
    }
}

impl Drop for Candidate {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn sync_dir(path: &Path) -> Result<()> {
    fs::File::open(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("missing configuration directory"))?,
    )?
    .sync_all()?;
    Ok(())
}

pub(crate) fn same_file(path: &Path, want: &Snapshot) -> bool {
    read_file(path, true).is_ok_and(|current| current.revision() == want.revision())
}

pub(crate) fn restore(path: &Path, old: &Snapshot, published: &[u8]) -> Result<()> {
    restore_for_owner(path, old, published, 0)
}

fn restore_for_owner(path: &Path, old: &Snapshot, published: &[u8], owner: u32) -> Result<()> {
    let current = read_file(path, true)?;
    ensure!(
        current.exists && current.data == published,
        "rollback refused because the managed file changed externally"
    );
    if !old.exists {
        fs::remove_file(path)?;
        return sync_dir(path);
    }
    let candidate = Candidate::new_for_owner(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("missing configuration directory"))?,
        &old.data,
        old.mode,
        owner,
    )?;
    // Do not overwrite an administrator's intervening edit while staging rollback.
    ensure!(
        same_file(path, &current),
        "rollback refused because the managed file changed externally"
    );
    candidate.publish(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_publication_and_rollback_preserve_bytes_and_mode() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let owner = fs::metadata(directory.path()).unwrap().uid();
        let path = directory.path().join("managed.conf");
        fs::write(&path, b"before").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let old = read_file(&path, true).unwrap();
        let candidate =
            Candidate::new_for_owner(directory.path(), b"after", old.mode, owner).unwrap();
        assert!(same_file(&path, &old));
        candidate.publish(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"after");
        restore_for_owner(&path, &old, b"after", owner).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"before");
        assert_eq!(read_file(&path, true).unwrap().mode, 0o640);

        let candidate =
            Candidate::new_for_owner(directory.path(), b"after", old.mode, owner).unwrap();
        candidate.publish(&path).unwrap();
        fs::write(&path, b"external edit").unwrap();
        assert!(restore_for_owner(&path, &old, b"after", owner).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"external edit");
    }

    #[test]
    fn rollback_removes_new_fragment_but_refuses_symlink() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let owner = fs::metadata(directory.path()).unwrap().uid();
        let path = directory.path().join("new.exports");
        let old = read_file(&path, true).unwrap();
        assert!(!old.exists);
        let candidate = Candidate::new_for_owner(directory.path(), b"new", 0o644, owner).unwrap();
        candidate.publish(&path).unwrap();
        restore_for_owner(&path, &old, b"new", owner).unwrap();
        assert!(!path.exists());
        let other = directory.path().join("administrator.exports");
        fs::write(&other, b"unmanaged").unwrap();
        std::os::unix::fs::symlink(&other, &path).unwrap();
        assert!(restore_for_owner(&path, &old, b"unmanaged", owner).is_err());
        assert_eq!(fs::read(&other).unwrap(), b"unmanaged");
    }

    #[test]
    fn reference_samba_fragment_and_ownership() {
        let definitions = normalize("samba", decode(r#"[{"name":"Docs","path":"/srv/docs","readOnly":true,"browseable":true,"users":["bob","alice","bob"]}]"#).unwrap(), false).unwrap();
        let data = render("samba", &definitions).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&data),
            concat!(
                "# Managed by maestro. Do not edit.\n# maestro-format: 1\n",
                "# maestro-share: {\"name\":\"docs\",\"path\":\"/srv/docs\",\"readOnly\":true,\"browseable\":true,\"users\":[\"alice\",\"bob\"]}\n",
                "[docs]\n\tpath = /srv/docs\n\tread only = yes\n\tguest ok = no\n\tbrowseable = yes\n\tvalid users = alice bob\n\n"
            )
        );
        let mut snapshot = Snapshot {
            data,
            exists: true,
            mode: 0o644,
        };
        assert_eq!(load("samba", &snapshot).unwrap().len(), 1);
        snapshot.data.extend(b"# external edit\n");
        assert!(load("samba", &snapshot).is_err());
    }

    #[test]
    fn reference_nfs_normalization_and_rejections() {
        let d = normalize("nfs", decode(r#"[{"name":"DATA","path":"/srv/my data","readOnly":false,"clients":["192.0.2.5/24","192.0.2.0/24","2001:db8::1"],"rootSquash":true,"security":"sys"}]"#).unwrap(), false).unwrap();
        assert_eq!(d[0].clients, ["192.0.2.0/24", "2001:db8::1/128"]);
        assert!(String::from_utf8(render("nfs", &d).unwrap()).unwrap().contains(r"/srv/my\040data 192.0.2.0/24(rw,sync,root_squash,no_all_squash,no_subtree_check,secure,sec=sys)"));
        assert!(decode(r#"[{"name":"x","arbitraryOption":"rw"}]"#).is_err());
        for path in ["/srv/../etc", "/srv//x", "/srv/x/", "/srv/x;include"] {
            assert!(!valid_path(path));
        }
        assert!(client("example.com").is_err());
        assert!(client("192.0.2.1/").is_err());
        assert_ne!(
            Snapshot::default().revision(),
            Snapshot {
                data: vec![],
                exists: true,
                mode: 0o644
            }
            .revision()
        );
    }
}
