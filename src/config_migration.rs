//! Explicit, one-time migration. Legacy files are never runtime fallbacks.
use anyhow::{Result, ensure};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

fn yaml_value(value: toml::Value) -> serde_yaml::Value {
    match value {
        toml::Value::String(v) => v.into(),
        toml::Value::Integer(v) => v.into(),
        toml::Value::Float(v) => serde_yaml::to_value(v).expect("float"),
        toml::Value::Boolean(v) => v.into(),
        toml::Value::Datetime(v) => v.to_string().into(),
        toml::Value::Array(v) => {
            serde_yaml::Value::Sequence(v.into_iter().map(yaml_value).collect())
        }
        toml::Value::Table(v) => serde_yaml::Value::Mapping(
            v.into_iter()
                .map(|(k, v)| (k.into(), yaml_value(v)))
                .collect(),
        ),
    }
}
fn read(path: &Path) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.uid() == unsafe { libc::geteuid() } && meta.len() <= 1024 * 1024,
        "migration requires a bounded regular file owned by the invoking account"
    );
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1024 * 1024, "configuration exceeds 1 MiB");
    Ok(bytes)
}
fn trusted_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("migration requires a file path"))?;
    let meta = fs::symlink_metadata(parent)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o022 == 0,
        "migration directory is not trusted"
    );
    Ok(())
}

fn publish(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.symlink_metadata().is_ok() {
        ensure!(
            fs::symlink_metadata(path)?.mode() & 0o077 == 0,
            "migration destination must be private"
        );
        ensure!(
            read(path)? == bytes,
            "migration destination already exists with different contents"
        );
        return Ok(());
    }
    let parent = path.parent().unwrap();
    let meta = fs::symlink_metadata(parent)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o022 == 0,
        "migration directory is not trusted"
    );
    let temporary = parent.join(format!(".maestro-migrate-{}.tmp", uuid::Uuid::new_v4()));
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _cleanup = Cleanup(temporary.clone());
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::hard_link(&temporary, path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn migrate_file(path: &Path, primary: bool) -> Result<PathBuf> {
    ensure!(
        path.extension().is_some_and(|ext| ext == "toml"),
        "migration source must be TOML"
    );
    trusted_parent(path)?;
    let target = path.with_extension("yaml");
    let backup = path.with_extension("toml.bak");
    let missing = match path.symlink_metadata() {
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.into()),
    };
    if missing && backup.try_exists()? && target.try_exists()? {
        read(&backup)?;
        read(&target)?;
        return Ok(target);
    }
    let bytes = read(path)?;
    let original: toml::Value = toml::from_str(std::str::from_utf8(&bytes)?)
        .map_err(|_| anyhow::anyhow!("invalid legacy TOML configuration"))?;
    let mut document = yaml_value(original);
    if primary {
        let root = document
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("configuration must be a mapping"))?;
        root.entry("platform".into()).or_insert("asuswrt".into());
    }
    let yaml = serde_yaml::to_string(&document)?;
    let _: serde_yaml::Value = crate::config_yaml::decode(&yaml)?;
    if primary {
        let _: crate::config::AppConfig = crate::config_yaml::decode(&yaml)?;
    }
    // Publish a private exact-byte backup first; conflicts never overwrite files.
    publish(&backup, &bytes)?;
    publish(&target, yaml.as_bytes())?;
    ensure!(
        read(path)? == bytes,
        "legacy configuration changed during migration"
    );
    fs::remove_file(path)?;
    fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(target)
}

pub fn migrate(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let target = if absolute.extension().is_some_and(|ext| ext == "toml") {
        migrate_file(&absolute, true)?
    } else {
        absolute
    };
    let config = crate::config::AppConfig::load(&target)?;
    for directory in [
        &config.nginx.inputs_dir,
        &config.nginx.root_dir.join("settings"),
    ] {
        for name in ["nginx.toml", "sites.toml"] {
            let path = directory.join(name);
            if path.symlink_metadata().is_ok() {
                migrate_file(&path, false)?;
            }
        }
    }
    crate::nginx_render::migrate_inputs(&config)?;
    Ok(target)
}

pub fn require_yaml(path: &Path) -> Result<()> {
    match path.with_extension("toml").symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => anyhow::bail!("legacy TOML input exists; run migrate-config once"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn private_tempdir() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        root
    }
    #[test]
    fn migration_preserves_values_original_bytes_and_relative_paths_once() {
        let root = private_tempdir();
        let source = root.path().join("maestro.toml");
        let original = b"# operator comment\ntest_mode = true\n[server]\nport = 3031\nauth_token = 'retain-this-token'\n[paths]\ndata_dir = './state'\n[nginx]\nroot_dir = './nginx'\ninputs_dir = './inputs'\n";
        fs::write(&source, original).unwrap();
        let target = migrate_file(&source, true).unwrap();
        assert_eq!(
            fs::read(source.with_extension("toml.bak")).unwrap(),
            original
        );
        assert!(!source.exists());
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o077, 0);
        let config = crate::config::AppConfig::load(&target).unwrap();
        assert_eq!(config.server.auth_token, "retain-this-token");
        assert_eq!(config.server.port, 3031);
        assert_eq!(config.paths.data_dir, root.path().join("state"));
        let edited = fs::read_to_string(&target).unwrap() + "# subsequent YAML edit\n";
        fs::write(&target, &edited).unwrap();
        assert_eq!(migrate_file(&source, true).unwrap(), target);
        assert_eq!(fs::read_to_string(&target).unwrap(), edited);
    }
    #[test]
    fn migration_refuses_conflicts_symlinks_and_malformed_sources() {
        let root = private_tempdir();
        let source = root.path().join("sites.toml");
        fs::write(&source, "domains = []\n").unwrap();
        let target = source.with_extension("yaml");
        fs::write(&target, "retain this existing file").unwrap();
        assert!(migrate_file(&source, false).is_err());
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "retain this existing file"
        );
        assert!(source.exists());
        fs::remove_file(&target).unwrap();
        let outside = root.path().join("outside");
        fs::write(&outside, "do not overwrite").unwrap();
        std::os::unix::fs::symlink(&outside, &target).unwrap();
        assert!(migrate_file(&source, false).is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "do not overwrite");
        fs::remove_file(&target).unwrap();
        fs::write(&source, "[broken").unwrap();
        assert!(migrate_file(&source, false).is_err());
        assert!(!target.exists());
    }
    #[test]
    fn complete_migration_converts_nginx_inputs_and_preserves_native_files() {
        let root = private_tempdir();
        let source = root.path().join("maestro.toml");
        let nginx = root.path().join("nginx");
        let inputs = root.path().join("inputs");
        let legacy = nginx.join("settings");
        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(&inputs).unwrap();
        for directory in [&nginx, &inputs, &legacy] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let config = format!(
            "test_mode = true\n[paths]\ndata_dir = '{}'\n[nginx]\nroot_dir = '{}'\ninputs_dir = '{}'\n",
            root.path().join("state").display(),
            nginx.display(),
            inputs.display()
        );
        fs::write(&source, &config).unwrap();
        fs::write(legacy.join("nginx.toml"), "[global]\nhttp_port = 18080\n").unwrap();
        fs::write(legacy.join("sites.toml"), "[sites]\n").unwrap();
        fs::write(nginx.join("custom.conf"), "# retain native config\n").unwrap();
        let target = migrate(&source).unwrap();
        let loaded = crate::config::AppConfig::load(&target).unwrap();
        assert_eq!(
            crate::nginx_render::load_global(&loaded).unwrap().shared["http_port"],
            serde_json::json!(18080)
        );
        assert!(crate::nginx_sites::load(&loaded).unwrap().sites.is_empty());
        assert!(inputs.join("nginx.yaml").exists());
        assert!(inputs.join("sites.yaml").exists());
        assert!(legacy.join("nginx.toml.bak").exists());
        assert!(legacy.join("sites.toml.bak").exists());
        assert_eq!(
            fs::read_to_string(nginx.join("custom.conf")).unwrap(),
            "# retain native config\n"
        );
        assert_eq!(migrate(&source).unwrap(), target);
    }

    #[test]
    fn matching_outputs_do_not_allow_migration_through_a_writable_directory() {
        use std::os::unix::fs::PermissionsExt;
        let root = private_tempdir();
        let source = root.path().join("sites.toml");
        fs::write(&source, "[sites]\n").unwrap();
        migrate_file(&source, false).unwrap();
        fs::write(&source, "[sites]\n").unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(migrate_file(&source, false).is_err());
        assert!(source.exists());
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn runtime_does_not_accept_active_toml_as_a_second_authority() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("maestro.toml");
        fs::write(&path, "test_mode = true\n").unwrap();
        assert!(crate::config::AppConfig::load(&path).is_err());
        fs::write(path.with_extension("yaml"), "test_mode: true\n").unwrap();
        assert!(crate::config::AppConfig::load(&path.with_extension("yaml")).is_err());
    }
}
