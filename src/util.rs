use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};

pub fn safe_relative_path(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if value.is_empty() || path.is_absolute() {
        bail!("path must be a non-empty relative path");
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            _ => bail!("path traversal is not allowed"),
        }
    }
    Ok(path.to_path_buf())
}

pub fn validate_simple_name(value: &str, field: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    {
        bail!("{field} contains invalid characters");
    }
    Ok(())
}

pub fn tail_lines(input: &str, count: usize) -> String {
    let lines: Vec<&str> = input.lines().collect();
    lines[lines.len().saturating_sub(count)..].join("\n")
}

pub fn parse_mac(value: &str) -> Result<[u8; 6]> {
    let normalized = value.replace('-', ":");
    let parts: Vec<&str> = normalized.split(':').collect();
    if parts.len() != 6 {
        bail!("MAC address must contain six octets");
    }
    let mut mac = [0_u8; 6];
    for (index, part) in parts.iter().enumerate() {
        mac[index] =
            u8::from_str_radix(part, 16).map_err(|_| anyhow::anyhow!("invalid MAC address"))?;
    }
    Ok(mac)
}

/// Resolve an administrator-selected file or fixed-name router state file.
/// Existing links (including directory aliases) are retained; broken links,
/// cycles and special files are errors, not missing configuration.
pub(crate) fn trusted_file_path(path: &Path) -> Result<PathBuf> {
    use anyhow::{Context, ensure};
    use std::{fs, io::ErrorKind};

    match fs::symlink_metadata(path) {
        Ok(_) => {
            let target = path
                .canonicalize()
                .with_context(|| format!("cannot resolve configured file {}", path.display()))?;
            ensure!(
                fs::metadata(&target)?.is_file(),
                "not a regular file: {}",
                path.display()
            );
            Ok(target)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            // Distinguish a genuinely absent file from a missing target of an
            // ancestor symlink. Resolve the nearest existing directory first.
            fn directory(path: &Path) -> Result<PathBuf> {
                match fs::symlink_metadata(path) {
                    Ok(_) => {
                        let resolved = path.canonicalize()?;
                        ensure!(resolved.is_dir(), "not a directory: {}", path.display());
                        Ok(resolved)
                    }
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        let parent = path
                            .parent()
                            .filter(|p| !p.as_os_str().is_empty())
                            .unwrap_or_else(|| Path::new("."));
                        let name = path.file_name().context("missing directory name")?;
                        Ok(directory(parent)?.join(name))
                    }
                    Err(error) => Err(error.into()),
                }
            }
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            Ok(directory(parent)?.join(path.file_name().context("missing file name")?))
        }
        Err(error) => Err(error.into()),
    }
}

/// Atomic publication to the resolved regular file, not over its symlink.
/// Call only for trusted configured paths, never for arbitrary API paths.
pub(crate) fn write_trusted_file(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<()> {
    use anyhow::{Context, ensure};
    use std::{
        fs::{self, File, OpenOptions},
        io::{ErrorKind, Write},
        os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, chown},
    };
    let target = trusted_file_path(path)?;
    let parent = target.parent().context("missing file parent")?;
    fs::create_dir_all(parent)?;
    let existing = match fs::symlink_metadata(&target) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file(),
                "not a regular file: {}",
                target.display()
            );
            Some(metadata)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let temp = parent.join(format!(".maestro-write-{}", uuid::Uuid::new_v4().simple()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp)?;
        if let Some(metadata) = &existing {
            let current = file.metadata()?;
            if current.uid() != metadata.uid() || current.gid() != metadata.gid() {
                chown(&temp, Some(metadata.uid()), Some(metadata.gid()))?;
            }
        }
        let permissions = mode
            .or_else(|| existing.as_ref().map(|m| m.permissions().mode()))
            .unwrap_or(0o600);
        file.set_permissions(fs::Permissions::from_mode(permissions))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        ensure!(
            trusted_file_path(path)? == target,
            "configured file changed during write: {}",
            path.display()
        );
        fs::rename(&temp, &target)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// A private temporary file, removed even if the caller returns early.
pub(crate) struct PrivateTemp(pub(crate) PathBuf);

impl PrivateTemp {
    pub(crate) fn new(directory: &Path, bytes: &[u8]) -> Result<Self> {
        use std::{fs, io::Write, os::unix::fs::OpenOptionsExt};
        fs::create_dir_all(directory)?;
        let path = directory.join(format!(".maestro-sync-{}", uuid::Uuid::new_v4().simple()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let temporary = Self(path);
        file.write_all(bytes)?;
        Ok(temporary)
    }
}

impl Drop for PrivateTemp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_paths() {
        assert!(safe_relative_path("../secret").is_err());
        assert!(safe_relative_path("conf/site.conf").is_ok());
    }

    #[test]
    fn trusted_write_preserves_aliases_and_rejects_broken_links() {
        use std::{fs, os::unix::fs::symlink};
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        fs::write(&target, "old").unwrap();
        let link = temp.path().join("config");
        symlink("target", &link).unwrap();
        write_trusted_file(&link, b"new", Some(0o600)).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("target"));
        fs::remove_file(&target).unwrap();
        assert!(write_trusted_file(&link, b"invalid", None).is_err());
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        let cycle = temp.path().join("cycle");
        symlink("cycle", &cycle).unwrap();
        assert!(trusted_file_path(&cycle).is_err());
        let nested = temp.path().join("missing/child.json");
        write_trusted_file(&nested, b"new", None).unwrap();
        assert_eq!(fs::read(nested).unwrap(), b"new");
    }

    #[test]
    fn private_temporary_is_unique_and_removed() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let first = PrivateTemp::new(temp.path(), b"secret").unwrap();
        let second = PrivateTemp::new(temp.path(), b"other").unwrap();
        assert_ne!(first.0, second.0);
        assert_eq!(
            fs::metadata(&first.0).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let path = first.0.clone();
        drop(first);
        assert!(!path.exists());
    }

    #[test]
    fn parses_mac_addresses() {
        assert_eq!(
            parse_mac("00:11:22:aa:bb:cc").unwrap(),
            [0, 17, 34, 170, 187, 204]
        );
    }
}
