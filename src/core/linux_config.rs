//! Translate the single private Linux YAML authority into runtime adapters.
use super::declarative::{
    Store,
    server::{self, Settings},
};
use crate::config::{AppConfig, Platform};
use anyhow::{Result, ensure};
use std::{
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub struct Authority {
    pub path: PathBuf,
    pub settings: Settings,
    pub defaults: Settings,
    pub credentials: std::sync::Arc<parking_lot::Mutex<Option<(String, String)>>>,
}
impl std::fmt::Debug for Authority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authority")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

fn quadlet_directory(home: &Path, xdg_config: Option<&std::ffi::OsStr>) -> Result<PathBuf> {
    let base = xdg_config
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    ensure!(
        base.is_absolute(),
        "rootless Quadlet configuration directory must be absolute"
    );
    Ok(base.join("containers/systemd/maestro"))
}

/// Create a private initial authority without replacing existing operator files.
pub fn initialize(path: &Path) -> Result<()> {
    use std::{io::Write, os::unix::fs::DirBuilderExt};
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "initialize Linux configuration as the non-root workload account"
    );
    let store = Store::at_path(path)?;
    if store.read()?.is_some() {
        load(path)?;
        return Ok(());
    }
    let parent = path.parent().expect("absolute file has parent");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    store.ensure_directory()?;
    let token_path = parent.join("token");
    if !token_path.exists() {
        let temporary = parent.join(format!(".maestro-token-{}.tmp", uuid::Uuid::new_v4()));
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let cleanup = Cleanup(temporary.clone());
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        let mut random = [0; 32];
        use ring::rand::SecureRandom;
        ring::rand::SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| anyhow::anyhow!("secure randomness unavailable"))?;
        use base64::Engine;
        file.write_all(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(random)
                .as_bytes(),
        )?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        match std::fs::hard_link(&temporary, &token_path) {
            Ok(()) => std::fs::File::open(parent)?.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        drop(cleanup);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is required for Linux defaults"))?;
    let state_root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state"));
    let document = serde_json::json!({"version":1,"server":{
        "listen":"127.0.0.1:8080", "token_file":token_path, "state_dir":state_root.join("maestro"),
        "allow_writes":true, "privileged_operations":false
    }});
    let yaml = serde_yaml::to_string(&document)?;
    // Validate the existing/new token and defaults before publishing YAML.
    let defaults = Settings::defaults(parent, &state_root.join("maestro"));
    let settings = server::read(yaml.as_bytes(), &defaults)?.expect("server mapping");
    read_token(Path::new(&settings.token_file))?;
    match store.save(yaml.as_bytes(), super::declarative::MISSING_REVISION) {
        Ok(_) => {}
        Err(error)
            if error
                .downcast_ref::<super::declarative::RevisionConflict>()
                .is_some() =>
        {
            load(path)?;
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

fn read_token(path: &Path) -> Result<String> {
    let token = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let meta = token.metadata()?;
    ensure!(
        meta.is_file() && meta.mode() & 0o077 == 0 && meta.uid() == unsafe { libc::geteuid() },
        "token must be a private regular file owned by the workload account"
    );
    let mut bytes = Vec::new();
    token.take(8193).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 8192, "token file exceeds its limit");
    let token = std::str::from_utf8(&bytes)
        .map_err(|_| anyhow::anyhow!("token is not UTF-8"))?
        .trim()
        .to_owned();
    ensure!(
        token.len() >= 24
            && !token.contains("CHANGE-ME")
            && !token.contains("REPLACE-WITH")
            && !token.chars().any(char::is_control),
        "token must contain at least 24 non-control bytes"
    );
    Ok(token)
}

pub fn load(path: &Path) -> Result<AppConfig> {
    Store::at_path(path)?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is required for Linux defaults"))?;
    let state_root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state"));
    let defaults = Settings::defaults(path.parent().unwrap(), &state_root.join("maestro"));
    let settings = server::load(path, &defaults)?;
    let token = read_token(Path::new(&settings.token_file))?;
    let listen = settings
        .listen
        .replacen("localhost:", "127.0.0.1:", 1)
        .parse::<std::net::SocketAddr>()?;
    let root = PathBuf::from(&settings.state_dir);
    let mut config = AppConfig {
        platform: Platform::Linux,
        ..Default::default()
    };
    config.server.bind = listen.ip().to_string();
    config.server.port = listen.port();
    config.server.auth_token = token;
    config.server.daemonize = false;
    config.server.allowed_origins = vec![if settings.public_origin.is_empty() {
        format!("http://{}", settings.listen)
    } else {
        settings.public_origin.clone()
    }];
    config.asus_ui.enabled = false;
    config.adguard.enabled = false;
    config.paths.data_dir = root.clone();
    config.paths.runtime_dir = root.join("run");
    config.paths.log_file = root.join("logs/maestro.log");
    config.paths.storage_dir = root.join("storage");
    let home = std::env::var_os("HOME")
        .ok_or_else(|| anyhow::anyhow!("HOME is required for the rootless Quadlet directory"))?;
    config.paths.quadlet_dir = Some(quadlet_directory(
        Path::new(&home),
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
    )?);
    config.paths.applications_dir = root.join("applications");
    config.paths.samba_conf = settings.samba_managed_file.clone().into();
    config.paths.exports_file = "/etc/exports.d/maestro.exports".into();
    config.services.init_dir = root.join("init.d");
    config.services.log_dirs = vec![root.join("logs")];
    config.firewall.log_dirs = vec![root.join("logs/nginx")];
    config.certificates.certs_dir = root.join("certificates");
    config.certificates.dns01_hook = root.join("dehydrated-dns01-hook.sh");
    let nginx = root.join("nginx");
    config.nginx.root_dir = nginx.clone();
    config.nginx.inputs_dir = root.join("nginx-inputs");
    config.nginx.config_path = nginx.join("nginx.conf");
    config.nginx.prefix_path = Some(nginx.clone());
    config.nginx.pid_path = root.join("run/nginx.pid");
    config.nginx.domains_available_dir = nginx.join("domains-available");
    config.nginx.domains_enabled_dir = nginx.join("domains-enabled");
    config.nginx.subdomain_upstream_map_path = nginx.join("conf.d/03_subdomain_upstream_map.conf");
    config.nginx.domain_upstream_map_path = nginx.join("conf.d/02_domain_upstream_map.conf");
    config.nginx.subfolder_upstream_map_path = nginx.join("conf.d/04_subfolder_upstream_map.conf");
    config.nginx.http_forwarder_path = nginx.join("conf.d/05-http-to-https.conf");
    config.nginx.log_dir = root.join("logs/nginx");
    config.system.allow_workload_writes = settings.allow_writes;
    config.system.allow_package_writes = settings.allow_writes && settings.privileged_operations;
    config.system.allow_share_writes = settings.allow_writes && settings.privileged_operations;
    let credentials = (!settings.admin_username.is_empty()).then(|| {
        (
            settings.admin_username.clone(),
            settings.admin_password_hash.clone(),
        )
    });
    config.linux_authority = Some(Authority {
        path: path.to_path_buf(),
        settings,
        defaults,
        credentials: std::sync::Arc::new(parking_lot::Mutex::new(credentials)),
    });
    config.validate()?;
    Ok(config)
}

pub fn restart_required(config: &AppConfig, bytes: &[u8]) -> Result<bool> {
    let Some(authority) = &config.linux_authority else {
        return Ok(false);
    };
    let desired =
        server::read(bytes, &authority.defaults)?.unwrap_or_else(|| authority.defaults.clone());
    let mut running = authority.settings.clone();
    if let Some((username, hash)) = authority.credentials.lock().as_ref() {
        running.admin_username.clone_from(username);
        running.admin_password_hash.clone_from(hash);
    }
    Ok(running != desired)
}

pub fn mutation_ready(config: &AppConfig, store: &Store) -> Result<()> {
    super::declarative::backup_capture::ensure_idle(store)?;
    if let Some(snapshot) = store.read()? {
        ensure!(
            !restart_required(config, &snapshot.bytes)?,
            "server settings changed; restart Maestro before further managed operations"
        );
    }
    Ok(())
}

pub fn preview(
    config: &AppConfig,
    store: &Store,
    bytes: &[u8],
    expected: &str,
) -> Result<super::declarative::Plan> {
    let mut plan = store.preview(bytes, expected)?;
    if config.linux_authority.is_none() {
        ensure!(
            !plan.restart_required,
            "server settings require unified YAML startup"
        );
    }
    plan.restart_required = restart_required(config, bytes)?;
    ensure!(
        !plan.restart_required || plan.changes.is_empty(),
        "save server settings separately and restart Maestro before changing managed resources"
    );
    Ok(plan)
}

pub fn store(config: &AppConfig) -> Result<Store> {
    match &config.linux_authority {
        Some(authority) => Store::at_path(&authority.path),
        None => Ok(Store::new(config.paths.data_dir.join("workloads"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    async fn auth_request(
        app: &axum::Router,
        method: &str,
        path: &str,
        body: serde_json::Value,
        cookie: Option<&str>,
    ) -> (u16, Option<String>, serde_json::Value) {
        use tower::ServiceExt;
        let mut request = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:8181")
            .header("origin", "http://127.0.0.1:8181")
            .header("content-type", "application/json");
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        let response = app
            .clone()
            .oneshot(
                request
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status().as_u16();
        let cookie = response
            .headers()
            .get("set-cookie")
            .map(|h| h.to_str().unwrap().split(';').next().unwrap().to_owned());
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (status, cookie, serde_json::from_slice(&bytes).unwrap())
    }

    #[test]
    fn initialization_is_private_idempotent_and_preserves_existing_authority() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private/maestro.yaml");
        initialize(&path).unwrap();
        let config = load(&path).unwrap();
        assert_eq!(config.platform, Platform::Linux);
        assert!(config.system.allow_workload_writes);
        assert!(!config.system.allow_package_writes);
        let yaml = std::fs::read(&path).unwrap();
        let token = std::fs::read(path.parent().unwrap().join("token")).unwrap();
        for file in [&path, &path.parent().unwrap().join("token")] {
            assert_eq!(std::fs::metadata(file).unwrap().mode() & 0o077, 0);
        }
        let edited = [yaml.as_slice(), b"# retain operator comment\n"].concat();
        std::fs::write(&path, &edited).unwrap();
        initialize(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), edited);
        assert_eq!(
            std::fs::read(path.parent().unwrap().join("token")).unwrap(),
            token
        );
        std::fs::set_permissions(
            path.parent().unwrap().join("token"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(initialize(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), edited);
    }

    #[test]
    fn linux_yaml_without_a_server_section_uses_native_defaults() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("maestro.yaml");
        let token = root.path().join("token");
        std::fs::write(&path, "version: 1\n").unwrap();
        std::fs::write(&token, "private-linux-default-token-with-24-characters").unwrap();
        for file in [&path, &token] {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let config = AppConfig::load(&path).unwrap();
        assert_eq!(config.platform, Platform::Linux);
        assert_eq!(config.server.port, 8080);
        assert!(config.linux_authority.is_some());
    }

    #[test]
    fn initializer_rejects_symlinked_token_without_replacing_it() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = root.path().join("outside");
        std::fs::write(&target, "retain this file").unwrap();
        let token = root.path().join("token");
        std::os::unix::fs::symlink(&target, &token).unwrap();
        let path = root.path().join("maestro.yaml");
        assert!(initialize(&path).is_err());
        assert!(!path.exists());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "retain this file");
        assert!(
            std::fs::symlink_metadata(token)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn rootless_quadlet_directory_honors_config_home_without_creating_files() {
        let home = Path::new("/workload-home");
        assert_eq!(
            quadlet_directory(home, None).unwrap(),
            home.join(".config/containers/systemd/maestro")
        );
        assert_eq!(
            quadlet_directory(home, Some(std::ffi::OsStr::new("/lab/config"))).unwrap(),
            Path::new("/lab/config/containers/systemd/maestro")
        );
        assert!(quadlet_directory(home, Some(std::ffi::OsStr::new("relative"))).is_err());
    }

    #[tokio::test]
    async fn reviewed_server_save_requires_restart_before_more_managed_writes() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().join("maestro.yaml");
        let token = root.path().join("token");
        std::fs::write(&token, "private-linux-token-with-more-than-24-characters").unwrap();
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        let yaml = format!(
            "version: 1\nserver:\n  listen: '127.0.0.1:8181'\n  token_file: '{}'\n  state_dir: '{}'\n  sample_interval_seconds: 2\n  allow_writes: true\n",
            token.display(),
            root.path().join("state").display()
        );
        std::fs::write(&path, &yaml).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let config = AppConfig::load(&path).unwrap();
        config.ensure_directories().unwrap();
        let stores = crate::storage::Stores::load(&config).await.unwrap();
        let firewall = crate::firewall::FirewallManager::new(config.clone(), stores.clone())
            .await
            .unwrap();
        let app = crate::api::app(crate::state::AppState::new(
            config.clone(),
            stores,
            firewall,
        ))
        .unwrap();
        let (_, cookie, _) = auth_request(
            &app,
            "POST",
            "/api/session",
            serde_json::json!({"token":config.server.auth_token}),
            None,
        )
        .await;
        let cookie = cookie.as_deref();
        let store = store(&config).unwrap();
        let revision = store.read().unwrap().unwrap().revision;
        let candidate = yaml.replace("sample_interval_seconds: 2", "sample_interval_seconds: 5");
        let equivalent = yaml.replace("  sample_interval_seconds: 2\n", "")
            + "storage: {data: {kind: directory, path: data}}\n";
        assert!(
            !preview(&config, &store, equivalent.as_bytes(), &revision)
                .unwrap()
                .restart_required
        );
        let (status, _, result) = auth_request(
            &app,
            "POST",
            "/api/tools/managed_config_plan",
            serde_json::json!({"content":candidate,"revision":revision}),
            cookie,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(result["plan"]["restartRequired"], true);
        assert_eq!(result["plan"]["changes"], serde_json::json!([]));
        let combined = candidate.clone() + "storage: {data: {kind: directory, path: data}}\n";
        let (status, _, _) = auth_request(
            &app,
            "POST",
            "/api/tools/managed_config_plan",
            serde_json::json!({"content":combined,"revision":revision}),
            cookie,
        )
        .await;
        assert_eq!(
            status, 409,
            "combined server/resource mutation was accepted"
        );
        let (status, _, started) = auth_request(&app, "POST", "/api/tools/managed_config_save", serde_json::json!({"content":candidate,"revision":revision,"mode":"next_start","confirm":true}), cookie).await;
        assert_eq!(status, 200);
        let id = started["job"]["id"].clone();
        let completed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let (_, _, job) = auth_request(
                    &app,
                    "POST",
                    "/api/tools/jobs_get",
                    serde_json::json!({"id":id}),
                    cookie,
                )
                .await;
                if job["state"] != "running" {
                    break job;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(completed["state"], "succeeded", "{completed}");
        assert_eq!(completed["result"]["restartRequired"], true);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), candidate);
        assert_eq!(
            config
                .linux_authority
                .as_ref()
                .unwrap()
                .settings
                .sample_interval_seconds,
            2
        );
        assert!(mutation_ready(&config, &store).is_err());
        let (status, _, failure) = auth_request(&app, "POST", "/api/tools/managed_config_save", serde_json::json!({"content":candidate,"revision":completed["result"]["revision"],"mode":"now","confirm":true}), cookie).await;
        assert_eq!(status, 409);
        assert_eq!(failure["error"]["code"], "restart_required");
        let reloaded = AppConfig::load(&path).unwrap();
        assert_eq!(
            reloaded
                .linux_authority
                .as_ref()
                .unwrap()
                .settings
                .sample_interval_seconds,
            5
        );
        assert!(mutation_ready(&reloaded, &store).is_ok());
        assert!(
            !preview(
                &reloaded,
                &store,
                candidate.as_bytes(),
                &store.read().unwrap().unwrap().revision
            )
            .unwrap()
            .restart_required
        );
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../test-fixtures/phase2/maestro-tools.json"
        ))
        .unwrap();
        let (_, _, discovery) =
            auth_request(&app, "GET", "/api/tools", serde_json::Value::Null, cookie).await;
        for name in [
            "managed_config_plan",
            "managed_config_save",
            "managed_config_history",
            "managed_config_history_read",
            "managed_apply_status",
            "managed_apply_retry",
            "managed_config_revert_plan",
            "managed_config_revert",
        ] {
            let actual = discovery["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == name)
                .unwrap();
            let expected = reference["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == name)
                .unwrap();
            assert_eq!(actual["inputSchema"], expected["inputSchema"]);
        }
    }

    #[tokio::test]
    async fn startup_and_workload_store_use_the_same_private_yaml() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().join("maestro.yaml");
        let token = root.path().join("token");
        let state = root.path().join("state");
        std::fs::write(&token, "private-linux-token-with-more-than-24-characters\n").unwrap();
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        let yaml = format!(
            "version: 1\nserver:\n  listen: '127.0.0.1:8181'\n  token_file: '{}'\n  state_dir: '{}'\n  allow_writes: true\n",
            token.display(),
            state.display()
        );
        std::fs::write(&path, &yaml).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let config = AppConfig::load(&path).unwrap();
        assert_eq!(config.platform, Platform::Linux);
        assert_eq!(config.server.port, 8181);
        assert_eq!(config.paths.data_dir, state);
        assert!(config.system.allow_workload_writes);
        assert!(!config.system.allow_package_writes);
        let store = store(&config).unwrap();
        let snapshot = store.read().unwrap().unwrap();
        assert_eq!(snapshot.bytes, yaml.as_bytes());
        assert!(snapshot.config().unwrap().server.is_some());
        let edited = yaml.clone() + "# authored workload comment\n";
        let plan = store
            .preview(edited.as_bytes(), &snapshot.revision)
            .unwrap();
        assert!(plan.changes.is_empty());
        store.save(edited.as_bytes(), &snapshot.revision).unwrap();
        assert_eq!(AppConfig::load(&path).unwrap().server.port, 8181);
        assert!(!state.join("workloads/workloads.yaml").exists());
        let changed_settings = edited.replace("8181", "8182");
        assert!(
            preview(
                &config,
                &store,
                changed_settings.as_bytes(),
                &store.read().unwrap().unwrap().revision
            )
            .unwrap()
            .restart_required
        );
        let broken = edited + "services: {broken: {unknown: repair-me}}\n";
        std::fs::write(&path, broken).unwrap();
        assert!(AppConfig::load(&path).is_ok());
        assert!(store.read().unwrap().unwrap().config().is_err());
        // Read the invalid authoring document through the authenticated HTTP
        // bridge to prove it selects the startup authority, not a second store.
        config.ensure_directories().unwrap();
        let stores = crate::storage::Stores::load(&config).await.unwrap();
        let firewall = crate::firewall::FirewallManager::new(config.clone(), stores.clone())
            .await
            .unwrap();
        let state = crate::state::AppState::new(config.clone(), stores, firewall);
        let app = crate::api::app(state).unwrap();
        use tower::ServiceExt;
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/tools/workloads_get")
                    .header("host", "127.0.0.1:8181")
                    .header(
                        "authorization",
                        format!("Bearer {}", config.server.auth_token),
                    )
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value["yaml"].as_str().unwrap().contains("repair-me"));
        assert_eq!(value["revision"], store.read().unwrap().unwrap().revision);
        let (status, bootstrap_cookie, _) = auth_request(
            &app,
            "POST",
            "/api/session",
            serde_json::json!({"token": config.server.auth_token}),
            None,
        )
        .await;
        assert_eq!(status, 200);
        let (status, admin_cookie, _) = auth_request(&app, "POST", "/api/auth/setup",
            serde_json::json!({"token": config.server.auth_token, "username": "Admin", "password": "a long administrator password"}), None).await;
        assert_eq!(status, 200);
        let (status, _, _) = auth_request(
            &app,
            "GET",
            "/api/status",
            serde_json::Value::Null,
            bootstrap_cookie.as_deref(),
        )
        .await;
        assert_eq!(status, 401, "bootstrap cookie survived setup");
        assert!(
            mutation_ready(&config, &store).is_ok(),
            "bootstrap caused a false restart requirement"
        );
        let (status, _, mode) =
            auth_request(&app, "GET", "/api/auth/mode", serde_json::Value::Null, None).await;
        assert_eq!(status, 200);
        assert_eq!(mode["adminConfigured"], true);
        let (status, _, _) = auth_request(
            &app,
            "POST",
            "/api/session",
            serde_json::json!({"token": config.server.auth_token}),
            None,
        )
        .await;
        assert_eq!(
            status, 401,
            "token browser login bypassed administrator password"
        );
        let (status, _, _) = auth_request(
            &app,
            "POST",
            "/api/session",
            serde_json::json!({"username":"Admin", "password":"incorrect"}),
            None,
        )
        .await;
        assert_eq!(status, 401);
        let (status, cookie, _) = auth_request(
            &app,
            "POST",
            "/api/session",
            serde_json::json!({"username":"Admin", "password":"a long administrator password"}),
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert!(cookie.is_some());
        let reloaded = AppConfig::load(&path).unwrap();
        let stores = crate::storage::Stores::load(&reloaded).await.unwrap();
        let firewall = crate::firewall::FirewallManager::new(reloaded.clone(), stores.clone())
            .await
            .unwrap();
        let restarted =
            crate::api::app(crate::state::AppState::new(reloaded, stores, firewall)).unwrap();
        let (status, _, _) = auth_request(
            &restarted,
            "GET",
            "/api/status",
            serde_json::Value::Null,
            admin_cookie.as_deref(),
        )
        .await;
        assert_eq!(status, 200, "administrator session did not survive restart");
        let (status, _, _) = auth_request(
            &restarted,
            "DELETE",
            "/api/session",
            serde_json::Value::Null,
            admin_cookie.as_deref(),
        )
        .await;
        assert_eq!(status, 200);
        let (status, _, _) = auth_request(
            &restarted,
            "GET",
            "/api/status",
            serde_json::Value::Null,
            admin_cookie.as_deref(),
        )
        .await;
        assert_eq!(status, 401, "logout did not revoke administrator session");
        for _ in 0..5 {
            let (status, _, _) = auth_request(
                &restarted,
                "POST",
                "/api/session",
                serde_json::json!({"username":"Admin", "password":"incorrect"}),
                None,
            )
            .await;
            assert_eq!(status, 401);
        }
        let (status, _, _) = auth_request(
            &restarted,
            "POST",
            "/api/session",
            serde_json::json!({"username":"Admin", "password":"incorrect"}),
            None,
        )
        .await;
        assert_eq!(status, 429, "password attempts were not bounded");
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(AppConfig::load(&path).is_err());
    }
}
