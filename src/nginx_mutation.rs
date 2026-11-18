//! Rollback boundary shared by object create/update/action/delete workflows.
use crate::{
    api::{ApiError, nginx as api_nginx},
    config::AppConfig,
    models::NginxObjectKind,
    nginx,
    nginx_render::FileTransaction,
    state::AppState,
};
use anyhow::{Result, bail};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

struct Snapshot {
    config: AppConfig,
    files: FileTransaction,
    links: BTreeMap<PathBuf, Option<PathBuf>>,
    complete: bool,
}

impl Snapshot {
    fn capture(
        config: &AppConfig,
        kind: NginxObjectKind,
        domain: &str,
        name: &str,
    ) -> Result<Self> {
        let mut files = BTreeSet::from([
            nginx::available_path(config, kind, domain, name)?,
            crate::nginx_sites::path(config),
            config.nginx.domain_upstream_map_path.clone(),
            config.nginx.subdomain_upstream_map_path.clone(),
            config.nginx.subfolder_upstream_map_path.clone(),
            config.nginx.http_forwarder_path.clone(),
        ]);
        for name in [
            "domains.conf",
            "subdomains.conf",
            "subfolders.conf",
            "servers.conf",
        ] {
            files.insert(config.nginx.inputs_dir.join(name));
            files.insert(config.nginx.root_dir.join("settings").join(name));
        }
        for name in [
            "ban-shield/default-drop.conf",
            "conf.d/00-ingress-access.conf",
            "snippets/tls-policy.conf",
        ] {
            files.insert(config.nginx.root_dir.join(name));
        }
        let mut links = BTreeMap::new();
        let root = config.nginx.domains_available_dir.join(domain);
        crate::nginx_render::ensure_managed_path(config, &root)?;
        if root.exists() {
            for entry in walkdir::WalkDir::new(&root).follow_links(false) {
                let entry = entry?;
                if entry.file_type().is_file() {
                    files.insert(entry.path().to_path_buf());
                } else if entry.file_type().is_symlink() {
                    links.insert(
                        entry.path().to_path_buf(),
                        Some(std::fs::read_link(entry.path())?),
                    );
                }
            }
        }
        for (kind, name) in std::iter::once((kind, name.to_string())).chain(
            nginx::list_objects(config, false)?
                .into_iter()
                .filter(|o| o.domain == domain)
                .map(|o| (o.kind, o.name)),
        ) {
            let path = nginx::enabled_path(config, kind, domain, &name)?;
            nginx::ensure_no_symlink_ancestors(&config.nginx.root_dir, path.parent().unwrap())?;
            let target = match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    Some(std::fs::read_link(&path)?)
                }
                Ok(_) => bail!("enabled nginx path is not a symlink: {}", path.display()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            links.insert(path, target);
        }
        Ok(Self {
            config: config.clone(),
            files: FileTransaction::capture(config, files)?,
            links,
            complete: false,
        })
    }

    fn changed(&self) -> bool {
        self.files.changed()
            || self
                .links
                .iter()
                .any(|(path, target)| std::fs::read_link(path).ok().as_ref() != target.as_ref())
    }

    fn restore(&mut self) -> Result<()> {
        self.files.rollback()?;
        for (path, target) in &self.links {
            nginx::ensure_no_symlink_ancestors(
                &self.config.nginx.root_dir,
                path.parent().unwrap(),
            )?;
            let current = std::fs::read_link(path).ok();
            if &current == target {
                continue;
            }
            if std::fs::symlink_metadata(path).is_ok() {
                if !std::fs::symlink_metadata(path)?.file_type().is_symlink() {
                    bail!("refusing to replace non-symlink enabled path");
                }
                std::fs::remove_file(path)?;
            }
            if let Some(target) = target {
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::os::unix::fs::symlink(target, path)?;
            }
        }
        self.complete = true;
        Ok(())
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        if !self.complete {
            if let Err(error) = self.restore() {
                tracing::error!(%error,"nginx object rollback failed");
            }
        }
    }
}

pub async fn apply<T>(
    state: &AppState,
    kind: NginxObjectKind,
    domain: &str,
    name: &str,
    operation: impl std::future::Future<Output = Result<T, ApiError>>,
) -> Result<T, ApiError> {
    let _lock = state.nginx_lock.lock().await;
    nginx::validate_name(domain, "domain").map_err(ApiError::bad_request)?;
    nginx::validate_name(name, "site name").map_err(ApiError::bad_request)?;
    let mut snapshot = Snapshot::capture(&state.config, kind, domain, name)?;
    let was_running = api_nginx::nginx_running(state).await;
    match operation.await {
        Ok(result) => {
            snapshot.complete = true;
            snapshot.files.mark_committed();
            Ok(result)
        }
        Err(mut error) => {
            let changed = snapshot.changed();
            if !changed {
                snapshot.complete = true;
                snapshot.files.mark_committed();
                return Err(error);
            }
            if let Err(rollback) = snapshot.restore() {
                return Err(ApiError::internal(format!(
                    "{error}; rollback failed: {rollback:#}"
                )));
            }
            if changed {
                let restore = if was_running {
                    api_nginx::nginx_reload(state).await
                } else if api_nginx::nginx_running(state).await {
                    api_nginx::nginx_stop(state).await
                } else {
                    return Err(error);
                };
                match restore {
                    Ok(result) if result.success => {}
                    Ok(result) => error
                        .append_detail(&format!("; runtime restoration failed: {}", result.stderr)),
                    Err(failure) => {
                        error.append_detail(&format!("; runtime restoration failed: {failure:#}"))
                    }
                }
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn rollback_restores_deleted_domain_custom_files_links_and_generated_outputs() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        config
            .apply_test_mode(
                &temp
                    .path()
                    .join("test-fixtures/etc/router-hub/router-hub.toml"),
            )
            .unwrap();
        config.ensure_directories().unwrap();
        let root = config.nginx.domains_available_dir.join("rollback.test");
        fs::create_dir_all(root.join("custom")).unwrap();
        fs::write(root.join("root.conf"), "server {}\n").unwrap();
        let secret = root.join("custom/private.key");
        fs::write(&secret, "original key").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        let authored_link = root.join("custom/key-link");
        std::os::unix::fs::symlink("private.key", &authored_link).unwrap();
        nginx::set_enabled(
            &config,
            NginxObjectKind::Domain,
            "rollback.test",
            "rollback.test",
            true,
        )
        .unwrap();
        let enabled = nginx::enabled_path(
            &config,
            NginxObjectKind::Domain,
            "rollback.test",
            "rollback.test",
        )
        .unwrap();
        let target = fs::read_link(&enabled).unwrap();
        let generated = config.nginx.root_dir.join("conf.d/00-ingress-access.conf");
        fs::create_dir_all(generated.parent().unwrap()).unwrap();
        fs::write(&generated, "original policies").unwrap();
        let created = config.nginx.root_dir.join("snippets/tls-policy.conf");
        let mut snapshot = Snapshot::capture(
            &config,
            NginxObjectKind::Domain,
            "rollback.test",
            "rollback.test",
        )
        .unwrap();
        fs::remove_file(&enabled).unwrap();
        fs::remove_dir_all(&root).unwrap();
        fs::write(&generated, "replacement policies").unwrap();
        fs::create_dir_all(created.parent().unwrap()).unwrap();
        fs::write(&created, "new output").unwrap();
        assert!(snapshot.changed());
        snapshot.restore().unwrap();
        assert_eq!(fs::read_to_string(&secret).unwrap(), "original key");
        assert_eq!(
            fs::metadata(&secret).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_link(&enabled).unwrap(), target);
        assert_eq!(
            fs::read_link(&authored_link).unwrap(),
            PathBuf::from("private.key")
        );
        assert_eq!(fs::read_to_string(&generated).unwrap(), "original policies");
        assert!(!created.exists());
    }
}
