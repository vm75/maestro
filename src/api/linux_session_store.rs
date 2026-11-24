//! Persistent non-root browser sessions. Raw cookies never enter the state
//! file; only SHA-256 digests and bounded Unix expiry timestamps are stored.
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub type SessionKey = [u8; 32];
const MAX_FILE: usize = 16 * 1024;
pub const LIFETIME_SECONDS: u64 = 30 * 24 * 60 * 60;
pub const MAX_SESSIONS: usize = 32;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    scope: String,
    sessions: BTreeMap<String, u64>,
}

pub struct SessionStore {
    directory: PathBuf,
    path: PathBuf,
    scope: String,
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn hex(value: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value {
        encoded.push(DIGITS[usize::from(byte >> 4)] as char);
        encoded.push(DIGITS[usize::from(byte & 15)] as char);
    }
    encoded
}

fn key_from_hex(value: &str) -> Result<SessionKey> {
    ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid session digest"
    );
    let mut key = [0_u8; 32];
    for (i, slot) in key.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16)
            .context("invalid browser session digest")?;
    }
    Ok(key)
}

fn private_metadata(meta: &fs::Metadata, directory: bool) -> Result<()> {
    ensure!(
        if directory {
            meta.is_dir()
        } else {
            meta.is_file()
        },
        "invalid session state type"
    );
    ensure!(
        meta.uid() == unsafe { libc::geteuid() },
        "browser session state has an unexpected owner"
    );
    ensure!(
        meta.permissions().mode() & 0o077 == 0,
        "browser session state is not owner-only"
    );
    Ok(())
}

impl SessionStore {
    pub fn open(
        data_dir: &std::path::Path,
        token: &str,
    ) -> Result<(Self, HashMap<SessionKey, u64>)> {
        let directory = data_dir.join("linux-auth");
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        private_metadata(&fs::symlink_metadata(&directory)?, true)?;
        let path = directory.join("browser-sessions.json");
        let store = Self {
            directory,
            path,
            scope: hex(&Sha256::digest(token.as_bytes())),
        };
        let sessions = store.load()?;
        Ok((store, sessions))
    }

    fn load(&self) -> Result<HashMap<SessionKey, u64>> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        private_metadata(&metadata, false)?;
        ensure!(
            metadata.len() <= MAX_FILE as u64,
            "browser sessions file is oversized"
        );
        let mut data = Vec::new();
        file.take((MAX_FILE + 1) as u64).read_to_end(&mut data)?;
        ensure!(data.len() <= MAX_FILE, "browser sessions file is oversized");
        let stored: Stored =
            serde_json::from_slice(&data).context("invalid browser sessions file")?;
        ensure!(
            stored.sessions.len() <= MAX_SESSIONS,
            "too many stored sessions"
        );
        if stored.scope != self.scope {
            return Ok(HashMap::new());
        }
        let now = now()?;
        let limit = now.saturating_add(LIFETIME_SECONDS);
        stored
            .sessions
            .into_iter()
            .filter_map(|(key, expiry)| {
                if expiry <= now || expiry > limit {
                    return None;
                }
                Some(key_from_hex(&key).map(|key| (key, expiry)))
            })
            .collect()
    }

    /// Atomic, owner-only and durable. Any failure prevents new sessions from
    /// being advertised to the browser and blocks logout from claiming success.
    pub fn save(&self, active: &HashMap<SessionKey, u64>) -> Result<()> {
        private_metadata(&fs::symlink_metadata(&self.directory)?, true)?;
        let current = now()?;
        let records = Stored {
            scope: self.scope.clone(),
            sessions: active
                .iter()
                .filter(|(_, expiry)| **expiry > current)
                .map(|(key, expiry)| (hex(key), *expiry))
                .collect(),
        };
        ensure!(
            records.sessions.len() <= MAX_SESSIONS,
            "session limit exceeded"
        );
        let data = serde_json::to_vec(&records)?;
        ensure!(
            data.len() <= MAX_FILE,
            "browser sessions state exceeds limit"
        );
        let temp = self
            .directory
            .join(format!(".browser-sessions-{}", Uuid::new_v4().simple()));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temp)?;
            file.write_all(&data)?;
            file.sync_all()?;
            drop(file);
            match fs::symlink_metadata(&self.path) {
                Ok(metadata) => private_metadata(&metadata, false)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            fs::rename(&temp, &self.path)?;
            File::open(&self.directory)?.sync_all()?;
            Ok(())
        })();
        let _ = fs::remove_file(&temp);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_survive_restart_and_token_rotation_invalidates_them() {
        let temp = tempfile::tempdir().unwrap();
        let (store, empty) = SessionStore::open(temp.path(), "original token").unwrap();
        assert!(empty.is_empty());
        let mut sessions = HashMap::new();
        sessions.insert([7_u8; 32], now().unwrap() + 500);
        store.save(&sessions).unwrap();
        let (_, reopened) = SessionStore::open(temp.path(), "original token").unwrap();
        assert_eq!(reopened.len(), 1);
        assert_eq!(reopened.get(&[7_u8; 32]), sessions.get(&[7_u8; 32]));
        let (_, invalidated) = SessionStore::open(temp.path(), "different token").unwrap();
        assert!(invalidated.is_empty());
        let bytes = fs::read(temp.path().join("linux-auth/browser-sessions.json")).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("original token"));
    }

    #[test]
    fn invalid_or_symlinked_session_state_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _) = SessionStore::open(temp.path(), "token").unwrap();
        fs::write(&store.path, b"{broken json").unwrap();
        assert!(SessionStore::open(temp.path(), "token").is_err());
        fs::remove_file(&store.path).unwrap();
        let target = temp.path().join("other");
        fs::write(&target, "{}").unwrap();
        std::os::unix::fs::symlink(&target, &store.path).unwrap();
        assert!(SessionStore::open(temp.path(), "token").is_err());
    }

    #[test]
    fn malformed_session_digests_return_errors_without_panicking() {
        for value in ["aé".repeat(21) + "a", "g".repeat(64), "0".repeat(63)] {
            assert!(key_from_hex(&value).is_err());
        }
        assert_eq!(key_from_hex(&"ab".repeat(32)).unwrap(), [0xab; 32]);
    }
}
