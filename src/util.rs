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

/// Open a physical absolute directory without following any new symlinks.
/// Callers resolve explicitly trusted aliases before entering this boundary.
fn physical_directory(path: &Path, create: bool) -> Result<std::os::fd::OwnedFd> {
    use anyhow::{Context, ensure};
    use rustix::fs::{self as rfs, Mode, OFlags};
    use rustix::io::Errno;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    ensure!(path.is_absolute(), "physical directory must be absolute");
    let mut directory = rfs::open("/", flags, Mode::empty())?;
    for part in path.components().skip(1) {
        let Component::Normal(name) = part else {
            bail!("invalid physical directory {}", path.display());
        };
        directory = match rfs::openat(&directory, name, flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) if create => {
                match rfs::mkdirat(&directory, name, Mode::from_raw_mode(0o755)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(error) => return Err(error.into()),
                }
                rfs::openat(&directory, name, flags, Mode::empty())?
            }
            Err(error) => return Err(error).with_context(|| format!("open {}", path.display())),
        };
    }
    Ok(directory)
}

pub(crate) fn open_physical_file(path: &Path) -> Result<std::fs::File> {
    use anyhow::{Context, ensure};
    use rustix::fs::{self as rfs, Mode, OFlags};
    let directory = physical_directory(path.parent().context("missing parent")?, false)?;
    let file = std::fs::File::from(rfs::openat(
        &directory,
        path.file_name().context("missing filename")?,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    ensure!(
        file.metadata()?.is_file(),
        "not a regular file: {}",
        path.display()
    );
    Ok(file)
}

/// Resolve a log only inside administrator-approved roots, including its
/// final symlink. Open the physical path with no-follow directory traversal.
pub(crate) fn open_contained_log(path: &Path, roots: &[PathBuf]) -> std::io::Result<std::fs::File> {
    let resolved = path.canonicalize()?;
    if !roots.iter().any(|root| {
        root.canonicalize()
            .is_ok_and(|root| resolved.starts_with(root))
    }) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("log target is outside configured roots: {}", path.display()),
        ));
    }
    open_physical_file(&resolved).map_err(|error| std::io::Error::other(error.to_string()))
}

/// Atomic publication in a pinned physical directory. With replace=false,
/// linkat publishes only if the destination does not exist.
pub(crate) fn publish_physical_file(
    target: &Path,
    bytes: &[u8],
    mode: u32,
    owner: Option<(u32, u32)>,
    replace: bool,
) -> Result<()> {
    use anyhow::{Context, ensure};
    use rustix::fs::{self as rfs, AtFlags, FileType, Mode, OFlags};
    use rustix::io::Errno;
    use std::{fs::File, io::Write, os::fd::AsRawFd};
    let directory = physical_directory(target.parent().context("missing parent")?, true)?;
    let name = target.file_name().context("missing filename")?;
    match rfs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => ensure!(
            replace && FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile,
            "refusing to replace non-regular or existing file: {}",
            target.display()
        ),
        Err(Errno::NOENT) => {}
        Err(error) => return Err(error.into()),
    }
    let temporary = format!(".maestro-write-{}", uuid::Uuid::new_v4().simple());
    let mut file = File::from(rfs::openat(
        &directory,
        &temporary,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?);
    let result = (|| -> Result<()> {
        if let Some((uid, gid)) = owner {
            use std::os::unix::fs::MetadataExt;
            let metadata = file.metadata()?;
            if metadata.uid() != uid || metadata.gid() != gid {
                // SAFETY: file owns the descriptor for the exclusively created file.
                if unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } != 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
            }
        }
        rfs::fchmod(&file, Mode::from_raw_mode(mode))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if replace {
            rfs::renameat(&directory, &temporary, &directory, name)?;
        } else {
            rfs::linkat(&directory, &temporary, &directory, name, AtFlags::empty())?;
            rfs::unlinkat(&directory, &temporary, AtFlags::empty())?;
        }
        rfs::fsync(&directory)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = rfs::unlinkat(&directory, &temporary, AtFlags::empty());
    }
    result
}

/// Atomic publication to the resolved regular file, not over its symlink.
pub(crate) fn write_trusted_file(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<()> {
    use std::{
        fs,
        io::ErrorKind,
        os::unix::fs::{MetadataExt, PermissionsExt},
    };
    let target = trusted_file_path(path)?;
    let existing = match fs::symlink_metadata(&target) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_file(),
                "not a regular file: {}",
                target.display()
            );
            Some(metadata)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let permissions = mode
        .or_else(|| existing.as_ref().map(|m| m.permissions().mode()))
        .unwrap_or(0o600);
    let owner = existing.as_ref().map(|m| (m.uid(), m.gid()));
    publish_physical_file(&target, bytes, permissions, owner, true)
}

/// Resolve only the parent; never follow the final directory entry on removal.
pub(crate) fn unlink_trusted_entry(path: &Path) -> Result<()> {
    use anyhow::Context;
    use rustix::fs::{self as rfs, AtFlags};
    let parent = path.parent().context("missing parent")?.canonicalize()?;
    let directory = physical_directory(&parent, false)?;
    rfs::unlinkat(
        &directory,
        path.file_name().context("missing filename")?,
        AtFlags::empty(),
    )?;
    rfs::fsync(&directory)?;
    Ok(())
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
