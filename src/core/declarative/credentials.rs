//! Fixed-format password hashing compatible with the frozen Linux reference.
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use ring::{
    pbkdf2,
    rand::{SecureRandom, SystemRandom},
};
use std::num::NonZeroU32;
const ITERATIONS: NonZeroU32 = NonZeroU32::new(600_000).unwrap();

pub fn hash(password: &str) -> Result<String> {
    ensure!(
        (12..=256).contains(&password.len()),
        "administrator password must be 12 to 256 bytes"
    );
    let mut salt = [0; 16];
    SystemRandom::new()
        .fill(&mut salt)
        .map_err(|_| anyhow::anyhow!("secure randomness unavailable"))?;
    let mut key = [0; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        ITERATIONS,
        &salt,
        password.as_bytes(),
        &mut key,
    );
    Ok(format!(
        "pbkdf2-sha256$600000${}${}",
        STANDARD_NO_PAD.encode(salt),
        STANDARD_NO_PAD.encode(key)
    ))
}

pub fn matches(password: &str, encoded: &str) -> bool {
    if password.len() > 256 || !super::server::valid_password_hash(encoded) {
        return false;
    }
    let parts: Vec<_> = encoded.split('$').collect();
    let salt = STANDARD_NO_PAD.decode(parts[2]).expect("validated salt");
    let key = STANDARD_NO_PAD.decode(parts[3]).expect("validated key");
    pbkdf2::verify(
        pbkdf2::PBKDF2_HMAC_SHA256,
        ITERATIONS,
        &salt,
        password.as_bytes(),
        &key,
    )
    .is_ok()
}

impl super::Store {
    /// Bootstrap credentials in the same authority, even with invalid workloads.
    /// The caller verifies the bootstrap token before invoking this method.
    pub fn setup_admin(&self, username: &str, hash: &str) -> Result<()> {
        ensure!(
            super::server::valid_username(username) && super::server::valid_password_hash(hash),
            "invalid administrator credentials"
        );
        let directory = self.lock_directory()?;
        super::journal::ensure_idle(&directory)?;
        let before = super::store::current(&directory, &self.filename)?;
        let bytes = before
            .as_ref()
            .map_or(b"version: 1\n".as_slice(), |snapshot| {
                snapshot.bytes.as_slice()
            });
        let defaults =
            super::server::Settings::defaults(std::path::Path::new("/"), std::path::Path::new("/"));
        let settings = super::server::read(bytes, &defaults)?;
        ensure!(
            settings.is_none_or(|settings| settings.admin_username.is_empty()),
            "administrator already configured"
        );
        let candidate = super::yaml::with_admin(bytes, username, hash)?;
        super::server::read(&candidate, &defaults)?;
        let latest = super::store::current(&directory, &self.filename)?;
        ensure!(
            latest.as_ref().map(|s| &s.revision) == before.as_ref().map(|s| &s.revision),
            "configuration changed during administrator setup"
        );
        if let Some(before) = &before {
            super::store::remember(&directory, &self.history_name, before)?;
        }
        super::store::publish(&directory, &self.filename, &candidate, before.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_password_hash_and_one_time_setup_preserve_broken_resources() {
        // Python hashlib.pbkdf2_hmac fixture for the reference's fixed format.
        assert!(matches(
            "a sufficiently long password",
            "pbkdf2-sha256$600000$AAECAwQFBgcICQoLDA0ODw$qXOZvDy01GeMq44VItTr5/qfaPfFVEtkY1IyHndgOno"
        ));
        let hash = hash("a sufficiently long password").unwrap();
        assert!(matches("a sufficiently long password", &hash));
        assert!(!matches("wrong password", &hash));
        assert!(!matches("password", &hash.replace("600000", "1")));
        let root = tempfile::tempdir().unwrap();
        let store = crate::core::declarative::Store::new(root.path().join("private"));
        store.ensure_directory().unwrap();
        std::fs::write(store.path(), "# café administrator setup\nversion: 1\nserver: {listen: '127.0.0.1:8181'} # listener policy\nservices: {broken: {unknown: repair-me}} # repair this resource\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
        store.setup_admin("Admin", &hash).unwrap();
        let snapshot = store.read().unwrap().unwrap();
        let settings = crate::core::declarative::server::read(
            &snapshot.bytes,
            &crate::core::declarative::server::Settings::defaults(
                std::path::Path::new("/"),
                std::path::Path::new("/"),
            ),
        )
        .unwrap()
        .unwrap();
        assert_eq!(settings.admin_username, "Admin");
        for comment in [
            "# café administrator setup",
            "# listener policy",
            "# repair this resource",
        ] {
            assert!(String::from_utf8_lossy(&snapshot.bytes).contains(comment));
        }
        assert!(String::from_utf8_lossy(&snapshot.bytes).contains("repair-me"));
        assert!(store.setup_admin("Other", &hash).is_err());
    }
}
