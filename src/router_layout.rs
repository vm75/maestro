//! Offline migration of router YAML-era configuration and durable state.
//! The active YAML is published last; source files are retired only after
//! their destinations and private backup copies have been durably published.
use std::{
    fs::{self, DirBuilder, File, Metadata},
    io::ErrorKind,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use uuid::Uuid;

use crate::{config::AppConfig, nginx, nginx_render};

const OLD_DATA: &str = "/opt/etc/maestro";
const NEW_DATA: &str = "/opt/var/lib/maestro";
const STATE_FILES: &[&str] = &[
    "certificates.json",
    "wol-machines.json",
    "adguard.json",
    "ban-attack-state.json",
];
const HTTP_FILES: &[&str] = &[
    "http01-session.json",
    "http01-status.json",
    "http01-wan-owner.json",
];
const AUTHORED: &[&str] = &[
    "nginx.yaml",
    "sites.yaml",
    "access_log_suppress.conf",
    "security_trap_allow.conf",
    "domains.conf",
    "subdomains.conf",
    "subfolders.conf",
];
const DERIVED: &[&str] = &["allowed_ips.txt", "free_subdomains.txt", "servers.conf"];
const POLICY: &str = "firewall-policy.json";

fn regular(path: &Path) -> Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file(),
                "layout migration requires a regular file: {}",
                path.display()
            );
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

// Compare directory-entry identity without following the final link.
fn same_entry(a: &Path, b: &Path) -> bool {
    a == b
        || (a.file_name() == b.file_name()
            && a.parent()
                .and_then(|p| p.canonicalize().ok())
                .zip(b.parent().and_then(|p| p.canonicalize().ok()))
                .is_some_and(|(a, b)| a == b))
}

fn source_file(path: &Path) -> Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_symlink() => {
            crate::util::trusted_file_path(path)?;
            Ok(Some(metadata))
        }
        Ok(metadata) => {
            ensure!(
                metadata.is_file(),
                "not a regular migration file: {}",
                path.display()
            );
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b
        || a.canonicalize()
            .ok()
            .zip(b.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
}

fn parent(path: &Path) -> Result<&Path> {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| path.is_relative().then_some(Path::new(".")))
        .context("layout path has no parent")
}

fn sync_parent(path: &Path) -> Result<()> {
    File::open(parent(path)?)?.sync_all()?;
    Ok(())
}

fn write_file(path: &Path, bytes: &[u8], metadata: &Metadata, replace: bool) -> Result<()> {
    let target = crate::util::trusted_file_path(path)?;
    crate::util::publish_physical_file(&target, bytes, metadata.permissions().mode(),
        Some((metadata.uid(),metadata.gid())), replace)
}

fn copy_checked(source: &Path, destination: &Path) -> Result<()> {
    let metadata = regular(source)?.context("migration source disappeared")?;
    let bytes = fs::read(source)?;
    if regular(destination)?.is_some() {
        ensure!(
            fs::read(destination)? == bytes,
            "layout migration conflict: {} differs from {}; preserve both and resolve manually",
            source.display(),
            destination.display()
        );
        return Ok(());
    }
    write_file(destination, &bytes, &metadata, false)
}

fn archive(source: &Path, directory: &Path, name: &str) -> Result<()> {
    // Archive the resolved bytes, not a dangling link into the old layout.
    // Retain the original link text separately for manual rollback.
    if fs::symlink_metadata(source)?.is_symlink() {
        let target = fs::read_link(source)?;
        let link_record = directory.join(format!("{name}.link"));
        use std::os::unix::ffi::OsStrExt;
        if !link_record.exists() {
            write_file(
                &link_record,
                target.as_os_str().as_bytes(),
                &fs::metadata(source)?,
                false,
            )?;
        }
        let resolved = crate::util::trusted_file_path(source)?;
        return archive(&resolved, directory, name);
    }
    let mut destination = directory.join(name);
    if regular(&destination)?.is_some() && fs::read(&destination)? != fs::read(source)? {
        destination = directory.join(format!("{name}-{}", Uuid::new_v4().simple()));
    }
    copy_checked(source, &destination)
}

fn remove(source: &Path) -> Result<()> {
    crate::util::unlink_trusted_entry(source)
}

struct Transfer {
    source: PathBuf,
    destination: PathBuf,
    label: String,
    preserve_link: bool,
}

fn plan(
    transfers: &mut Vec<Transfer>,
    source: PathBuf,
    destination: PathBuf,
    label: String,
) -> Result<()> {
    // Operational session records are deleted during normal recovery and
    // managed nginx inputs reject leaf symlinks. Materialize their contents
    // rather than introducing links those consumers cannot safely manage.
    let preserve_link = !label.starts_with("http-") && !label.starts_with("settings-");
    if let Some(metadata) = source_file(&source)? {
        if same_entry(&source, &destination) {
            return Ok(());
        }
        if metadata.is_symlink() && preserve_link {
            // A relocated link must still point to the same existing target.
            // On retry, allow only a matching link, not a different file.
            let target = crate::util::trusted_file_path(&source)?;
            ensure!(
                !same_entry(&target, &destination),
                "migration would replace the source link's target: {}",
                destination.display()
            );
            if let Some(existing) = source_file(&destination)? {
                ensure!(
                    existing.is_symlink()
                        && crate::util::trusted_file_path(&destination)? == target,
                    "layout migration conflict: {} differs from {}",
                    source.display(),
                    destination.display()
                );
            }
        } else if regular(&destination)?.is_some() {
            ensure!(
                fs::read(&source)? == fs::read(&destination)?,
                "layout migration conflict: {} differs from {}",
                source.display(),
                destination.display()
            );
        }
        transfers.push(Transfer {
            source,
            destination,
            label,
            preserve_link,
        });
    }
    Ok(())
}

fn retire(files: &mut Vec<(PathBuf, String)>, source: PathBuf, label: String) -> Result<()> {
    if source_file(&source)?.is_some() {
        files.push((source, label));
    }
    Ok(())
}

fn check_roots(config: &AppConfig, data: &Path) -> Result<()> {
    nginx_render::ensure_managed_path(config, &config.nginx.inputs_dir)?;
    ensure!(
        !same_path(&config.nginx.inputs_dir, &config.nginx.root_dir)
            && !same_path(
                &config.nginx.inputs_dir,
                &config.nginx.root_dir.join("settings")
            ),
        "nginx.inputs_dir must be separate from native nginx configuration"
    );
    ensure!(
        !same_path(&config.nginx.inputs_dir, &data.join("nginx")),
        "nginx.inputs_dir overlaps the persistent HTTP-01 state directory"
    );
    nginx::ensure_no_symlink_ancestors(data, &data.join("nginx"))?;
    nginx::ensure_no_symlink_ancestors(data, &data.join("layout-v13-backup"))?;
    nginx::ensure_no_symlink_ancestors(
        &config.nginx.inputs_dir,
        &config.nginx.inputs_dir.join("settings"),
    )?;
    Ok(())
}

/// Fail closed on an unprepared layout instead of silently starting with empty
/// state or forgetting an active HTTP-01 firewall ownership record.
pub fn require_current(config: &AppConfig) -> Result<()> {
    let data = &config.paths.data_dir;
    let inputs = &config.nginx.inputs_dir;
    check_roots(config, data)?;
    let legacy_policy = data.join(POLICY);
    let policy = config.firewall_policy_path();
    source_file(&policy)?;
    let mut pending = same_path(data, Path::new(OLD_DATA)) && !same_path(data, Path::new(NEW_DATA))
        || (source_file(&legacy_policy)?.is_some() && !same_entry(&legacy_policy, &policy));
    for name in HTTP_FILES {
        let source = inputs.join(name);
        if source_file(&source)?.is_some() {
            pending |= !same_entry(&source, &config.http_state_dir().join(name));
        }
    }
    for name in AUTHORED.iter().chain(DERIVED) {
        pending |= source_file(&inputs.join("settings").join(name))?.is_some();
    }
    for name in DERIVED {
        pending |= source_file(&inputs.join(name))?.is_some();
    }
    ensure!(
        !pending,
        "router layout requires migration: stop Maestro and run migrate-router-layout with the selected YAML"
    );
    Ok(())
}

pub fn migrate(config_path: &Path, config: &AppConfig) -> Result<()> {
    migrate_with_defaults(
        config_path,
        config,
        Path::new(OLD_DATA),
        Path::new(NEW_DATA),
    )
}

fn migrate_with_defaults(
    config_path: &Path,
    config: &AppConfig,
    old_default: &Path,
    new_default: &Path,
) -> Result<()> {
    let old_data = &config.paths.data_dir;
    let change_data = same_path(old_data, old_default) && !same_path(old_data, new_default);
    let data = if change_data { new_default } else { old_data };
    let inputs = &config.nginx.inputs_dir;
    let policy = config.firewall_policy_path();
    let old_policy = old_data.join(POLICY);
    check_roots(config, data)?;
    source_file(&policy)?;

    let mut transfers = Vec::new();
    let mut retired = Vec::new();
    if change_data {
        for name in STATE_FILES {
            plan(
                &mut transfers,
                old_data.join(name),
                data.join(name),
                format!("state-{name}"),
            )?;
        }
        // Older installers seeded an unused second policy in NEW_DATA even
        // when the active data directory was OLD_DATA. Preserve it for audit,
        // but never let it take precedence over the effective old policy.
        let unused_policy = data.join(POLICY);
        if source_file(&unused_policy)?.is_some()
            && !same_entry(&unused_policy, &old_policy)
            && !same_entry(&unused_policy, &policy)
        {
            retire(
                &mut retired,
                unused_policy,
                "inactive-firewall-policy.json".into(),
            )?;
        }
    }
    plan(
        &mut transfers,
        old_policy,
        policy.clone(),
        "old-firewall-policy.json".into(),
    )?;

    for name in HTTP_FILES {
        plan(
            &mut transfers,
            inputs.join(name),
            data.join("nginx").join(name),
            format!("http-{name}"),
        )?;
    }
    for name in AUTHORED {
        plan(
            &mut transfers,
            inputs.join("settings").join(name),
            inputs.join(name),
            format!("settings-{name}"),
        )?;
    }
    for name in DERIVED {
        retire(&mut retired, inputs.join(name), format!("derived-{name}"))?;
        retire(
            &mut retired,
            inputs.join("settings").join(name),
            format!("settings-derived-{name}"),
        )?;
    }

    // A custom YAML could previously rely on the fixed global policy fallback.
    // Copy it only if neither the previous primary nor new authority exists.
    let fallback = old_default.join(POLICY);
    let import_fallback = source_file(&policy)?.is_none()
        && !transfers
            .iter()
            .any(|transfer| transfer.destination == policy)
        && !same_path(&fallback, &policy)
        && source_file(&fallback)?.is_some();

    if !change_data && transfers.is_empty() && retired.is_empty() && !import_fallback {
        return Ok(());
    }

    let backup = data.join("layout-v13-backup");
    fs::create_dir_all(data)?;
    if !backup.exists() {
        DirBuilder::new().mode(0o700).create(&backup)?;
        sync_parent(&backup)?;
    }
    let backup_meta = fs::symlink_metadata(&backup)?;
    ensure!(
        backup_meta.is_dir() && backup_meta.permissions().mode() & 0o077 == 0,
        "layout backup must be a private real directory: {}",
        backup.display()
    );

    for transfer in &transfers {
        if transfer.preserve_link && fs::symlink_metadata(&transfer.source)?.is_symlink() {
            let target = crate::util::trusted_file_path(&transfer.source)?;
            // Do not retire another file that a migrated link depends on.
            ensure!(
                !transfers
                    .iter()
                    .any(|other| same_entry(&other.source, &target))
                    && !retired
                        .iter()
                        .any(|(source, _)| same_entry(source, &target)),
                "migration link target is also being retired: {}",
                target.display()
            );
            if source_file(&transfer.destination)?.is_none() {
                fs::create_dir_all(parent(&transfer.destination)?)?;
                std::os::unix::fs::symlink(&target, &transfer.destination)?;
                sync_parent(&transfer.destination)?;
            }
            ensure!(
                crate::util::trusted_file_path(&transfer.destination)? == target,
                "migration link target changed: {}",
                transfer.destination.display()
            );
        } else {
            copy_checked(
                &crate::util::trusted_file_path(&transfer.source)?,
                &transfer.destination,
            )?;
        }
        archive(&transfer.source, &backup, &transfer.label)?;
    }
    for (source, label) in &retired {
        archive(source, &backup, label)?;
    }
    if import_fallback {
        copy_checked(&crate::util::trusted_file_path(&fallback)?, &policy)?;
    }

    // A flattened YAML tree must validate before retiring its former inputs.
    let global = nginx_render::load_global(config)?;
    let sites = crate::nginx_sites::load(config)?;
    crate::nginx_ingress::validate_tree(&global, &sites)?;

    let yaml_update = if change_data {
        let resolved = crate::util::trusted_file_path(config_path)?;
        let metadata = regular(&resolved)?.context("router YAML disappeared")?;
        let raw = crate::config_yaml::read(&resolved)?;
        let mut document: serde_yaml::Value = crate::config_yaml::decode(&raw)?;
        document["paths"]["data_dir"] = serde_yaml::to_value(data)?;
        let encoded = serde_yaml::to_string(&document)?;
        // Validate the rewritten document with the same strict decoder.
        let _: AppConfig = crate::config_yaml::decode(&encoded)?;
        archive(config_path, &backup, "maestro.yaml.before-layout")?;
        ensure!(
            crate::util::trusted_file_path(config_path)? == resolved
                && crate::config_yaml::read(&resolved)? == raw,
            "router YAML changed during migration; stop Maestro and retry"
        );
        Some((resolved, encoded, metadata, raw))
    } else {
        None
    };

    // Destinations and private backups are already durable. Retire old
    // entries before publishing the YAML so a crash still leaves the old
    // configuration selecting the migration on retry. Startup refuses that
    // old layout until migration finishes.
    for transfer in &transfers {
        remove(&transfer.source)?;
    }
    for (source, _) in &retired {
        remove(source)?;
    }
    let nested = inputs.join("settings");
    if nested.is_dir() && fs::read_dir(&nested)?.next().is_none() {
        fs::remove_dir(&nested)?;
        sync_parent(&nested)?;
    }
    if let Some((resolved, encoded, metadata, raw)) = yaml_update {
        ensure!(
            crate::util::trusted_file_path(config_path)? == resolved
                && crate::config_yaml::read(&resolved)? == raw,
            "router YAML changed during migration; stop Maestro and retry"
        );
        write_file(&resolved, encoded.as_bytes(), &metadata, true)?;
    }
    println!("previous router layout saved in {}", backup.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn fixture(root: &Path, custom: bool) -> (PathBuf, PathBuf, PathBuf, AppConfig) {
        let old = root.join("etc/maestro");
        let new = root.join("var/lib/maestro");
        let data = if custom {
            root.join("custom")
        } else {
            old.clone()
        };
        let inputs = old.join("nginx");
        fs::create_dir_all(&inputs).unwrap();
        fs::create_dir_all(&data).unwrap();
        let path = old.join("maestro.yaml");
        fs::write(
            &path,
            format!(
                "platform: asuswrt\nserver:\n  auth_token: preserved-token-for-layout-tests\npaths:\n  data_dir: {}\nnginx:\n  inputs_dir: {}\n  root_dir: {}\n",
                data.display(),
                inputs.display(),
                root.join("native-nginx").display()
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let config = AppConfig::load(&path).unwrap();
        (path, old, new, config)
    }

    #[test]
    fn migrate_preserves_default_layout_and_retries() {
        let temp = tempfile::tempdir().unwrap();
        let (path, old, new, config) = fixture(temp.path(), false);
        let original = fs::read(&path).unwrap();
        let inputs = &config.nginx.inputs_dir;
        for name in STATE_FILES {
            fs::write(old.join(name), b"saved state").unwrap();
        }
        for name in HTTP_FILES {
            fs::write(inputs.join(name), b"saved recovery").unwrap();
        }
        fs::write(old.join(POLICY), b"effective policy").unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join(POLICY), b"inactive installer copy").unwrap();
        fs::create_dir_all(inputs.join("settings")).unwrap();
        fs::write(inputs.join("settings/domains.conf"), b"custom upstream").unwrap();
        fs::write(inputs.join("settings/unknown.conf"), b"keep unknown").unwrap();
        for name in DERIVED {
            fs::write(inputs.join(name), b"stale").unwrap();
        }

        migrate_with_defaults(&path, &config, &old, &new).unwrap();
        let migrated = AppConfig::load(&path).unwrap();
        assert_eq!(migrated.paths.data_dir, new);
        assert_eq!(migrated.server.auth_token, config.server.auth_token);
        assert_eq!(migrated.nginx.inputs_dir, config.nginx.inputs_dir);
        assert_eq!(fs::read(old.join(POLICY)).unwrap(), b"effective policy");
        assert!(!new.join(POLICY).exists());
        for name in STATE_FILES {
            assert_eq!(fs::read(new.join(name)).unwrap(), b"saved state");
            assert!(!old.join(name).exists());
        }
        for name in HTTP_FILES {
            assert_eq!(
                fs::read(new.join("nginx").join(name)).unwrap(),
                b"saved recovery"
            );
            assert!(!inputs.join(name).exists());
        }
        assert_eq!(
            fs::read(inputs.join("domains.conf")).unwrap(),
            b"custom upstream"
        );
        assert!(inputs.join("settings/unknown.conf").exists());
        for name in DERIVED {
            assert!(!inputs.join(name).exists());
        }
        assert_eq!(
            fs::read(new.join("layout-v13-backup/maestro.yaml.before-layout")).unwrap(),
            original
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let rewritten = fs::read(&path).unwrap();
        migrate_with_defaults(&path, &migrated, &old, &new).unwrap();
        assert_eq!(fs::read(&path).unwrap(), rewritten);
    }

    #[test]
    fn conflicts_and_symlinks_leave_original_state_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let (path, old, new, config) = fixture(temp.path(), false);
        fs::create_dir_all(&new).unwrap();
        fs::write(old.join("certificates.json"), b"old").unwrap();
        fs::write(new.join("certificates.json"), b"new").unwrap();
        let original = fs::read(&path).unwrap();
        assert!(migrate_with_defaults(&path, &config, &old, &new).is_err());
        assert_eq!(fs::read(old.join("certificates.json")).unwrap(), b"old");
        assert_eq!(fs::read(new.join("certificates.json")).unwrap(), b"new");
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_file(new.join("certificates.json")).unwrap();
        symlink(old.join("certificates.json"), new.join("certificates.json")).unwrap();
        assert!(migrate_with_defaults(&path, &config, &old, &new).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read(old.join("certificates.json")).unwrap(), b"old");
        assert_eq!(
            fs::read_link(new.join("certificates.json")).unwrap(),
            old.join("certificates.json")
        );
    }

    #[test]
    fn migration_preserves_linked_state_and_yaml() {
        let temp = tempfile::tempdir().unwrap();
        let (path, old, new, _) = fixture(temp.path(), false);
        let target = temp.path().join("external-state.json");
        fs::write(&target, b"saved").unwrap();
        symlink(&target, old.join("certificates.json")).unwrap();
        let yaml = temp.path().join("router.yaml");
        fs::rename(&path, &yaml).unwrap();
        symlink(&yaml, &path).unwrap();
        let config = AppConfig::load(&path).unwrap();
        let http = temp.path().join("external-http.json");
        fs::write(&http, b"recovery").unwrap();
        symlink(&http, config.nginx.inputs_dir.join("http01-wan-owner.json")).unwrap();
        migrate_with_defaults(&path, &config, &old, &new).unwrap();
        assert_eq!(fs::read_link(&path).unwrap(), yaml);
        assert_eq!(
            fs::read(new.join("nginx/http01-wan-owner.json")).unwrap(),
            b"recovery"
        );
        assert!(
            !fs::symlink_metadata(new.join("nginx/http01-wan-owner.json"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(fs::read(&http).unwrap(), b"recovery");
        assert_eq!(
            fs::read_link(new.join("certificates.json")).unwrap(),
            target
        );
        assert_eq!(fs::read(&target).unwrap(), b"saved");
        assert!(!old.join("certificates.json").exists());
        let updated = AppConfig::load(&path).unwrap();
        assert_eq!(updated.paths.data_dir, new);
        migrate_with_defaults(&path, &updated, &old, &new).unwrap();
    }

    #[test]
    fn custom_data_path_is_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let (path, old, new, config) = fixture(temp.path(), true);
        let original = fs::read(&path).unwrap();
        let data = &config.paths.data_dir;
        fs::write(data.join(POLICY), b"custom policy").unwrap();
        fs::write(data.join("wol-machines.json"), b"custom state").unwrap();
        fs::write(
            config.nginx.inputs_dir.join("http01-wan-owner.json"),
            b"owner",
        )
        .unwrap();
        migrate_with_defaults(&path, &config, &old, &new).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read(old.join(POLICY)).unwrap(), b"custom policy");
        assert!(!data.join(POLICY).exists());
        assert_eq!(
            fs::read(data.join("wol-machines.json")).unwrap(),
            b"custom state"
        );
        assert_eq!(
            fs::read(data.join("nginx/http01-wan-owner.json")).unwrap(),
            b"owner"
        );
        assert!(!new.exists());
    }
}
