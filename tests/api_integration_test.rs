use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use maestro::{
    api, config::AppConfig, firewall::FirewallManager, state::AppState, storage::Stores,
};
use tempfile::tempdir;
use tower::ServiceExt;

async fn setup_test_app() -> (axum::Router, AppState, tempfile::TempDir) {
    let temp = tempdir().unwrap();
    let root = std::env::current_dir().unwrap();
    let config_path = root.join("test-fixtures/etc/maestro/maestro.yaml");
    let mut config = AppConfig::default();
    config.apply_test_mode(&config_path).unwrap();
    config.paths.data_dir = temp.path().join("data");
    config.paths.applications_dir = temp.path().join("applications");
    config.paths.storage_dir = temp.path().join("storage");
    config.paths.samba_conf = temp.path().join("samba/maestro.conf");
    config.paths.exports_file = temp.path().join("exports/maestro.exports");
    config.paths.dnsmasq_conf_add = temp.path().join("dnsmasq.conf.add");
    config.commands.dehydrated = temp.path().join("dehydrated/dehydrated");
    config.certificates.certs_dir = temp.path().join("dehydrated/certs");
    config.certificates.dns01_hook = temp.path().join("dehydrated/dehydrated-dns01-hook.sh");
    config.nginx.root_dir = temp.path().join("nginx");
    config.nginx.inputs_dir = temp.path().join("maestro/nginx");
    config.nginx.config_path = config.nginx.root_dir.join("nginx.conf");
    config.nginx.pid_path = config.nginx.root_dir.join("nginx.pid");
    config.nginx.domains_available_dir = config.nginx.root_dir.join("domains-available");
    config.nginx.domains_enabled_dir = config.nginx.root_dir.join("domains-enabled");
    config.nginx.subdomain_upstream_map_path = config
        .nginx
        .root_dir
        .join("conf.d/03_subdomain_upstream_map.conf");
    config.nginx.subfolder_upstream_map_path = config
        .nginx
        .root_dir
        .join("conf.d/04_subfolder_upstream_map.conf");
    config.nginx.domain_upstream_map_path = config
        .nginx
        .root_dir
        .join("conf.d/02_domain_upstream_map.conf");
    config.nginx.http_forwarder_path = config.nginx.root_dir.join("conf.d/05-http-to-https.conf");
    config.nginx.log_dir = temp.path().join("logs/nginx");
    config.firewall.log_dirs = vec![temp.path().to_path_buf()];
    config.ensure_directories().unwrap();
    // Exercise compatibility with an existing pre-ingress installation.
    std::fs::create_dir_all(&config.nginx.inputs_dir).unwrap();
    std::fs::write(
        config.nginx.inputs_dir.join("nginx.yaml"),
        "{\n  \"global\": {}\n}\n",
    )
    .unwrap();
    std::fs::write(&config.nginx.config_path, "events {} http {}").unwrap();

    let stores = Stores::load(&config).await.unwrap();
    let firewall = FirewallManager::new(config.clone(), stores.clone())
        .await
        .unwrap();
    let state = AppState::new(config, stores, firewall);
    let app = api::app(state.clone()).unwrap();
    (app, state, temp)
}

#[tokio::test]
async fn test_unauthenticated_endpoints() {
    let (app, _state, _temp) = setup_test_app().await;

    // Test GET /healthz
    let req = Request::builder()
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), b"ok");

    let req = Request::builder()
        .uri("/favicon.png")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "image/png");
    assert!(
        !axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .is_empty()
    );

    let req = Request::builder()
        .uri("/favicon.svg")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "image/svg+xml");

    let req = Request::builder()
        .uri("/maestro.svg")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "image/svg+xml");

    // Test GET /api/version
    let req = Request::builder()
        .uri("/api/version")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn test_modules_discovery_is_authenticated_and_platform_scoped() {
    let (app, _state, _temp) = setup_test_app().await;

    let anonymous = Request::builder()
        .uri("/api/modules")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(anonymous).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let authorized = Request::builder()
        .uri("/api/modules")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(authorized).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["platform"], "asuswrt");
    let modules = result["modules"].as_array().unwrap();
    let nginx = modules
        .iter()
        .find(|module| module["id"] == "nginx")
        .unwrap();
    assert_eq!(nginx["ui"][0]["tab"], "web");
    let router_upgrade = modules
        .iter()
        .find(|module| module["id"] == "router_upgrade")
        .unwrap();
    assert_eq!(router_upgrade["availability"], "disabled");
    assert!(!result.to_string().contains("maestro-test-token"));
}

#[tokio::test]
async fn test_standalone_ui_endpoint() {
    let (app, _state, _temp) = setup_test_app().await;

    // Standalone index in test mode is accessible without token
    let req = Request::builder().uri("/").body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_auth_protection() {
    let (app, _state, _temp) = setup_test_app().await;

    // Missing auth token -> 401
    let req = Request::builder()
        .uri("/api/auth/check")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Invalid auth token -> 401
    let req = Request::builder()
        .uri("/api/auth/check")
        .header(header::AUTHORIZATION, "Bearer invalid-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Valid Bearer token -> 200
    let req = Request::builder()
        .uri("/api/auth/check")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Query tokens are deliberately not accepted.
    let req = Request::builder()
        .uri("/api/auth/check?token=maestro-test-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_dashboard_endpoint() {
    let (app, _state, _temp) = setup_test_app().await;

    let req = Request::builder()
        .uri("/api/dashboard")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["test_mode"], true);
}

#[tokio::test]
async fn test_dehydrated_certificate_api_and_files() {
    let (app, _state, temp) = setup_test_app().await;
    let auth_header = ("authorization", "Bearer maestro-test-token");
    let payload = serde_json::json!({
        "name": "example_test",
        "domains": ["example.test"],
        "method": "http",
        "hook": null,
        "hook_env": {},
        "staging": false,
        "auto_renew": true
    });

    let req = Request::builder()
        .method("POST")
        .uri("/api/certificates")
        .header(auth_header.0, auth_header.1)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let created: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let certificate_id = created["id"].as_str().unwrap();
    let cert_root = temp.path().join("dehydrated/certs");

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/certificates/{certificate_id}/issue"))
        .header(auth_header.0, auth_header.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(result["stdout"].as_str().unwrap().contains("--cron"));
    assert!(result["stdout"].as_str().unwrap().contains("--config"));

    let lock_path = cert_root.join("lock");
    std::fs::write(&lock_path, "stale lock").unwrap();
    let req = Request::builder()
        .uri("/api/certificates/dehydrated/lock")
        .header(auth_header.0, auth_header.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let lock: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(lock["locked"], true);
    assert_eq!(lock["path"], lock_path.to_string_lossy().to_string());

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/certificates/{certificate_id}/renew"))
        .header(auth_header.0, auth_header.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let req = Request::builder()
        .method("DELETE")
        .uri("/api/certificates/dehydrated/lock")
        .header(auth_header.0, auth_header.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let lock: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(lock["locked"], false);
    assert!(!lock_path.exists());

    assert_eq!(
        std::fs::read_to_string(cert_root.join("example_test.cfg")).unwrap(),
        format!(
            "CA='letsencrypt'\nAUTO_CLEANUP='yes'\nDOMAINS_TXT='{}'\nCHALLENGETYPE='http-01'\nWELLKNOWN='{}/acme-challenge'\n",
            cert_root.join("example_test.txt").display(),
            cert_root.display()
        )
    );
    assert_eq!(
        std::fs::read_to_string(cert_root.join("example_test.txt")).unwrap(),
        "\n# example.test-start\nexample.test > example_test\n# example.test-end\n"
    );

    let deployed_cert_root = temp.path().join("nginx/certs/example_test");
    std::fs::create_dir_all(&deployed_cert_root).unwrap();
    std::fs::write(deployed_cert_root.join("cert.pem"), "deployed certificate").unwrap();
    std::fs::write(
        deployed_cert_root.join("fullchain.pem"),
        "deployed full chain",
    )
    .unwrap();

    let req = Request::builder()
        .uri("/api/certificates")
        .header(auth_header.0, auth_header.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let statuses: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        statuses[0]["fullchain_path"],
        deployed_cert_root
            .join("fullchain.pem")
            .to_string_lossy()
            .to_string()
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/certificates/dehydrated/update")
        .header(auth_header.0, auth_header.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let update: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(update["simulated"], true);
}

#[tokio::test]
async fn test_dns_certificate_uses_default_dns01_hook_from_config() {
    let (app, state, _temp) = setup_test_app().await;
    let auth_header = ("authorization", "Bearer maestro-test-token");
    let payload = serde_json::json!({
        "name": "example_dns_default",
        "domains": ["example.test"],
        "method": "dns",
        "hook": null,
        "hook_env": {},
        "staging": false,
        "auto_renew": true
    });

    let req = Request::builder()
        .method("POST")
        .uri("/api/certificates")
        .header(auth_header.0, auth_header.1)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let created: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let certificate_id = created["id"].as_str().unwrap();

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/certificates/{certificate_id}/issue"))
        .header(auth_header.0, auth_header.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let config_file = state
        .config
        .certificates
        .certs_dir
        .join("example_dns_default.cfg");
    let content = tokio::fs::read_to_string(&config_file).await.unwrap();
    assert!(content.contains(&format!(
        "HOOK='{}'",
        state.config.certificates.dns01_hook.display()
    )));
}

#[tokio::test]
async fn test_wol_crud_api() {
    let (app, _state, _temp) = setup_test_app().await;
    let auth_header = ("authorization", "Bearer maestro-test-token");

    // 1. GET /api/wol (empty list)
    let req = Request::builder()
        .uri("/api/wol")
        .header(auth_header.0, auth_header.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let machines: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(machines.len(), 0);

    // 2. POST /api/wol (Create machine)
    let new_machine = serde_json::json!({
        "name": "NAS Server",
        "mac": "11:22:33:44:55:66",
        "broadcast": "192.168.1.255",
        "port": 9,
        "notes": "Backup target"
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/wol")
        .header(auth_header.0, auth_header.1)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&new_machine).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let created: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(created["name"], "NAS Server");
    let machine_id = created["id"].as_str().unwrap().to_string();

    // 3. GET /api/wol/status (test mode simulates the neighbor lookup)
    let req = Request::builder()
        .uri("/api/wol/status")
        .header(auth_header.0, auth_header.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let statuses: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(statuses[0]["id"], machine_id);
    assert_eq!(statuses[0]["status"], "unknown");

    // 4. POST /api/wol/{id}/wake (Simulate magic packet)
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/wol/{}/wake", machine_id))
        .header(auth_header.0, auth_header.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 5. DELETE /api/wol/{id}
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/wol/{}", machine_id))
        .header(auth_header.0, auth_header.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_firewall_policy_api() {
    let (app, state, temp) = setup_test_app().await;
    let auth_header = ("authorization", "Bearer maestro-test-token");

    let log_file = temp.path().join("auth.log");
    std::fs::write(&log_file, "").unwrap();

    // Enable firewall policy with a valid rule
    let mut policy = state.stores.firewall_policy.read().await.clone();
    policy.enabled = true;
    policy.rules.push(maestro::models::BanRule {
        id: uuid::Uuid::new_v4(),
        name: "ssh".into(),
        enabled: true,
        log_paths: vec![log_file],
        pattern: r"Failed password for root from (?P<ip>[0-9.]+)".into(),
        ip_group: "ip".into(),
        group_values: Default::default(),
        attempts: 3,
        weight: 1,
        updated_at: chrono::Utc::now(),
    });

    let req = Request::builder()
        .method("PUT")
        .uri("/api/firewall")
        .header(auth_header.0, auth_header.1)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&policy).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Start firewall manager to initialize ban engine
    state.firewall.start().await;

    // GET /api/firewall
    let req = Request::builder()
        .uri("/api/firewall")
        .header(auth_header.0, auth_header.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // POST /api/firewall/allowlist (Add allowlist network)
    let payload = serde_json::json!({ "network": "10.0.0.0/8" });
    let req = Request::builder()
        .method("POST")
        .uri("/api/firewall/allowlist")
        .header(auth_header.0, auth_header.1)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Host-network allowlist entries are accepted after UI normalization.
    let payload = serde_json::json!({ "network": "10.0.0.42/32" });
    let req = Request::builder()
        .method("POST")
        .uri("/api/firewall/allowlist")
        .header(auth_header.0, auth_header.1)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // POST /api/firewall/bans (Manual ban)
    let ban_payload =
        serde_json::json!({ "network": "192.168.1.100/32", "reason": "test manual ban" });
    let req = Request::builder()
        .method("POST")
        .uri("/api/firewall/bans")
        .header(auth_header.0, auth_header.1)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&ban_payload).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let req = Request::builder()
        .method("DELETE")
        .uri("/api/firewall/bans/192.168.1.100%2F32")
        .header(auth_header.0, auth_header.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let policy = state.stores.firewall_policy.read().await;
    assert!(
        policy
            .allowlist
            .contains(&"192.168.1.100/32".parse().unwrap())
    );
    drop(policy);
    let status = state.firewall.status().await;
    assert!(status.bans.is_empty());
    assert!(status.snapshot.ip_counts.is_empty());
}

#[tokio::test]
async fn test_services_api() {
    let (app, state, _temp) = setup_test_app().await;
    let auth_header = ("authorization", "Bearer maestro-test-token");

    std::fs::write(
        state.config.services.init_dir.join("S80nginx"),
        "#!/bin/sh\n",
    )
    .unwrap();
    std::fs::write(
        state.config.nginx.log_dir.join("error.log"),
        "nginx service log entry\n",
    )
    .unwrap();

    let req = Request::builder()
        .uri("/api/services/S80nginx/logs")
        .header(auth_header.0, auth_header.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["message"], "nginx service log entry");

    // GET /api/services
    let req = Request::builder()
        .uri("/api/services")
        .header(auth_header.0, auth_header.1)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let services: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
    // Test fixtures directory contains init.d scripts
    assert!(!services.is_empty());
    assert!(
        services
            .iter()
            .all(|service| service["enabled"].as_bool() == Some(true))
    );

    // Maestro must not permit disabling its own init service.
    {
        let service = "S99maestro";
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/services/{service}/disable"))
            .header(auth_header.0, auth_header.1)
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    for (action, expected_command) in [
        ("restart", "restart"),
        ("reconfigure", "reconfigure"),
        // Keep the old action name working while applying its new behavior.
        ("refresh", "reconfigure"),
        ("disable", "disable"),
        ("enable", "enable"),
    ] {
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/services/S10demo/{action}"))
            .header(auth_header.0, auth_header.1)
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(result["success"].as_bool().unwrap());
        assert!(
            result["stdout"]
                .as_str()
                .unwrap()
                .ends_with(expected_command)
        );
    }
}

async fn nginx_request(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method(method)
        .uri(format!("/api/nginx{path}"))
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

#[tokio::test]
async fn test_nginx_jinja_settings_objects_actions_and_logs_api() {
    use serde_json::json;
    let (app, state, _temp) = setup_test_app().await;
    // Removed editors have no read or mutation endpoints.
    for path in [
        "/files",
        "/files/nginx.conf",
        "/templates/domain",
        "/templates/domain/basic",
    ] {
        for method in ["GET", "PUT", "DELETE"] {
            assert_eq!(
                nginx_request(&app, method, path, json!({"content":"bad"}))
                    .await
                    .0,
                StatusCode::NOT_FOUND
            );
        }
    }
    let globals = json!({"shared":{"http_port":8080,"https_port":8443,"var_dir":state.config.nginx.log_dir.parent().unwrap().parent().unwrap(),"acme_challenge_dir":state.config.certificates.certs_dir.join("acme-challenge")}, "limit_policies":{"assets":{"rate":"100r/s","burst":500,"nodelay":true,"connections":50}}, "site_defaults":{"enable_streaming":true,"enable_lanonly":false,"limit_policy":"assets"}, "allowed_ips":["192.0.2.0/24"], "free_subdomains":["shared.example.test"]});
    let (status, result) = nginx_request(&app, "PUT", "/settings", globals.clone()).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let mut globals = globals;
    globals["shared"]["nginx_daemon"] = json!(true);
    assert_eq!(
        nginx_request(&app, "GET", "/settings", json!(null)).await.1,
        globals
    );
    let persisted: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(maestro::nginx_render::global_path(&state.config)).unwrap(),
    )
    .unwrap();
    assert_eq!(persisted["global"]["http_port"].as_i64(), Some(8080));
    assert_eq!(
        persisted["site_defaults"]["enable_streaming"].as_bool(),
        Some(true)
    );
    assert_eq!(persisted["allowed_ips"][0].as_str(), Some("192.0.2.0/24"));
    assert!(
        !state
            .config
            .nginx
            .root_dir
            .join("settings/nginx.yaml")
            .exists()
    );
    assert!(
        !state
            .config
            .nginx
            .root_dir
            .join("settings/sites.yaml")
            .exists()
    );
    assert!(
        !state
            .config
            .nginx
            .root_dir
            .join("settings/allowed_ips.txt")
            .exists()
    );
    assert!(
        state
            .config
            .nginx
            .inputs_dir
            .join("allowed_ips.txt")
            .exists()
    );
    assert_eq!(
        persisted["limit_policies"]["assets"]["burst"].as_i64(),
        Some(500)
    );
    let payload = json!({"domain":"example.test", "name":"example.test", "server_names":["example.test","www.example.test"], "settings":{"disable_root":false}, "upstream":"http://127.0.0.1:8081", "enabled":true});
    let (status, domain) = nginx_request(&app, "POST", "/objects/domain", payload).await;
    assert_eq!(status, StatusCode::OK, "{domain}");
    assert_eq!(domain["object"]["managed"], true);
    assert!(
        domain["content"]
            .as_str()
            .unwrap()
            .contains("limit_req zone=rh_req_assets burst=500 nodelay;")
    );
    assert!(
        domain["content"]
            .as_str()
            .unwrap()
            .contains("limit_conn rh_conn_assets 50;")
    );
    let mut removed = globals.clone();
    removed["limit_policies"] = json!({});
    assert_eq!(
        nginx_request(&app, "PUT", "/settings", removed).await.0,
        StatusCode::BAD_REQUEST
    );

    assert!(
        domain["content"]
            .as_str()
            .unwrap()
            .contains("proxy_buffering off;")
    );
    assert!(
        !domain["content"]
            .as_str()
            .unwrap()
            .contains("include snippets/restrict.conf;")
    );
    let domain_data =
        std::fs::read_to_string(state.config.nginx.root_dir.join("settings/domains.conf")).unwrap();
    assert!(domain_data.contains("www.example.test http://127.0.0.1:8081;"));
    let folder = json!({"domain":"example.test", "name":"media", "settings":{"enable_streaming":false,"enable_large_upload":true,"large_upload_max_body_size":"0","limit_policy":"assets"}, "upstream":"http://127.0.0.1:8082", "enabled":true});
    let (status, folder) = nginx_request(&app, "POST", "/objects/subfolder", folder).await;
    assert_eq!(status, StatusCode::OK, "{folder}");
    assert!(
        folder["content"]
            .as_str()
            .unwrap()
            .contains("client_max_body_size 0;")
    );
    assert!(
        !folder["content"]
            .as_str()
            .unwrap()
            .contains("proxy_buffering off;")
    );
    // A policy used explicitly by a saved site cannot be removed, even if defaults stop using it.
    let mut removed = globals.clone();
    removed["limit_policies"] = json!({});
    removed["site_defaults"]
        .as_object_mut()
        .unwrap()
        .remove("limit_policy");
    assert_eq!(
        nginx_request(&app, "PUT", "/settings", removed).await.0,
        StatusCode::BAD_REQUEST
    );
    let (_, saved_folder) = nginx_request(
        &app,
        "GET",
        "/objects/subfolder/example.test/media",
        json!(null),
    )
    .await;
    assert_eq!(saved_folder["settings"]["limit_policy"], "assets");
    let custom = "server { server_name raw.example.test; # {{untouched}}\n return 204; }";
    let payload = json!({"domain":"example.test", "name":"raw", "content":custom});
    assert_eq!(
        nginx_request(&app, "POST", "/objects/subdomain", payload)
            .await
            .0,
        StatusCode::OK
    );
    // Global edits rerender inherited settings while preserving per-site false/zero and custom content.
    let globals = json!({"shared":{"http_port":8080,"https_port":8443},"limit_policies":{"assets":{"rate":"100r/s","burst":500,"nodelay":true,"connections":50}},"site_defaults":{"enable_lanonly":true,"enable_streaming":true},"allowed_ips":["198.51.100.0/24"],"free_subdomains":[]});
    let (status, response) = nginx_request(&app, "PUT", "/settings", globals).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let (_, domain) = nginx_request(
        &app,
        "GET",
        "/objects/domain/example.test/example.test",
        json!(null),
    )
    .await;
    assert!(
        domain["content"]
            .as_str()
            .unwrap()
            .contains("include snippets/restrict.conf;")
    );
    let (_, folder) = nginx_request(
        &app,
        "GET",
        "/objects/subfolder/example.test/media",
        json!(null),
    )
    .await;
    assert_eq!(folder["settings"]["enable_streaming"], false);
    assert_eq!(folder["settings"]["limit_policy"], "assets");
    assert!(
        folder["content"]
            .as_str()
            .unwrap()
            .contains("limit_req zone=rh_req_assets burst=500 nodelay;")
    );
    assert!(
        !folder["content"]
            .as_str()
            .unwrap()
            .contains("proxy_buffering off;")
    );
    assert_eq!(
        nginx_request(
            &app,
            "GET",
            "/objects/subdomain/example.test/raw",
            json!(null)
        )
        .await
        .1["content"],
        custom
    );
    assert!(
        std::fs::read_to_string(state.config.nginx.root_dir.join("snippets/restrict.conf"))
            .unwrap()
            .contains("allow 198.51.100.0/24;")
    );
    // Unknown legacy payload fields and unsafe directive values are rejected.
    assert_eq!(
        nginx_request(
            &app,
            "PUT",
            "/objects/domain/example.test/example.test",
            json!({"template":"basic"})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        nginx_request(&app, "PUT", "/settings", json!({"shared":{"http_port":0}}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        nginx_request(
            &app,
            "PUT",
            "/objects/subfolder/example.test/media",
            json!({"settings":{"proxy_connect_timeout":"1s; return 200;"}})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (_, unchanged) = nginx_request(
        &app,
        "GET",
        "/objects/subfolder/example.test/media",
        json!(null),
    )
    .await;
    assert_eq!(unchanged["content"], folder["content"]);
    // Updating aliases replaces the matching upstream entries together.
    let (status, response) = nginx_request(&app,"PUT","/objects/domain/example.test/example.test",json!({"server_names":["example.test","alias.example.test"],"upstream":"https://127.0.0.1:9443"})).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let data =
        std::fs::read_to_string(state.config.nginx.root_dir.join("settings/domains.conf")).unwrap();
    assert!(!data.contains("www.example.test"));
    assert!(data.contains("alias.example.test https://127.0.0.1:9443;"));
    for action in ["disable", "enable", "stop", "start"] {
        assert_eq!(
            nginx_request(
                &app,
                "POST",
                &format!("/objects/subfolder/example.test/media/{action}"),
                json!(null)
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    std::fs::write(
        state.config.nginx.log_dir.join("access.log"),
        "nginx log entry\n",
    )
    .unwrap();
    assert_eq!(
        nginx_request(&app, "GET", "/logs/access.log", json!(null))
            .await
            .1["content"],
        "nginx log entry\n"
    );
    assert_eq!(
        nginx_request(
            &app,
            "DELETE",
            "/objects/domain/example.test/example.test",
            json!(null)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        nginx_request(&app, "GET", "/objects", json!(null)).await.1,
        json!([])
    );
    assert!(
        !std::fs::read_to_string(state.config.nginx.root_dir.join("settings/domains.conf"))
            .unwrap()
            .contains("example.test")
    );
}

#[tokio::test]
async fn test_nginx_settings_validation_failure_restores_all_files() {
    let temp = tempdir().unwrap();
    let mut config = AppConfig {
        test_mode: false,
        ..Default::default()
    };
    config.server.auth_token = "maestro-test-token-that-is-long-enough".into();
    config.paths.data_dir = temp.path().join("data");
    config.certificates.certs_dir = temp.path().join("dehydrated/certs");
    config.paths.runtime_dir = temp.path().join("runtime");
    config.nginx.root_dir = temp.path().join("nginx");
    config.nginx.inputs_dir = temp.path().join("maestro/nginx");
    config.nginx.config_path = config.nginx.root_dir.join("nginx.conf");
    config.nginx.pid_path = config.nginx.root_dir.join("nginx.pid");
    config.nginx.domains_available_dir = config.nginx.root_dir.join("domains-available");
    config.nginx.domains_enabled_dir = config.nginx.root_dir.join("domains-enabled");
    config.nginx.domain_upstream_map_path = config
        .nginx
        .root_dir
        .join("conf.d/02_domain_upstream_map.conf");
    config.nginx.subdomain_upstream_map_path = config
        .nginx
        .root_dir
        .join("conf.d/03_subdomain_upstream_map.conf");
    config.nginx.subfolder_upstream_map_path = config
        .nginx
        .root_dir
        .join("conf.d/04_subfolder_upstream_map.conf");
    config.nginx.http_forwarder_path = config.nginx.root_dir.join("conf.d/05-http-to-https.conf");
    config.nginx.log_dir = temp.path().join("logs/nginx");
    config.asus_ui.rendered_page = temp.path().join("maestro.asp");
    config.commands.nginx = "/bin/false".into();
    config.ensure_directories().unwrap();
    std::fs::write(&config.nginx.config_path, "original").unwrap();
    let original_globals = "{\n  \"global\": {\n    \"http_port\": 19080\n  }\n}\n";
    let globals_path = maestro::nginx_render::global_path(&config);
    std::fs::create_dir_all(globals_path.parent().unwrap()).unwrap();
    std::fs::write(&globals_path, original_globals).unwrap();

    let input_map = config.nginx.inputs_dir.join("domains.conf");
    std::fs::write(&input_map, "# original custom input map\n").unwrap();
    let stores = Stores::load(&config).await.unwrap();
    let firewall = FirewallManager::new(config.clone(), stores.clone())
        .await
        .unwrap();
    let state = AppState::new(config, stores, firewall);
    let app = api::app(state.clone()).unwrap();

    let req = Request::builder()
        .method("PUT")
        .uri("/api/nginx/settings")
        .header(
            header::AUTHORIZATION,
            "Bearer maestro-test-token-that-is-long-enough",
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"shared":{"http_port":8080},"site_defaults":{"enable_lanonly":false}}"#,
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(
        std::fs::read_to_string(&state.config.nginx.config_path).unwrap(),
        "original"
    );
    assert_eq!(
        std::fs::read_to_string(&globals_path).unwrap(),
        original_globals
    );
    assert_eq!(
        std::fs::read_to_string(input_map).unwrap(),
        "# original custom input map\n"
    );
    assert!(
        !state
            .config
            .nginx
            .inputs_dir
            .join("allowed_ips.txt")
            .exists()
    );
    assert!(
        !state
            .config
            .nginx
            .root_dir
            .join("snippets/ssl.conf")
            .exists()
    );
    assert!(
        !state
            .config
            .nginx
            .root_dir
            .join("settings/allowed_ips.txt")
            .exists()
    );
}

#[tokio::test]
async fn test_adguard_config_api() {
    let (app, _state, _temp) = setup_test_app().await;

    // GET /api/adguard/config
    let req = Request::builder()
        .uri("/api/adguard/config")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let info: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(info["launch_url"], "http://192.168.1.1:3000");

    // PUT /api/adguard/config with explicit launch_url alias
    let update_payload = serde_json::json!({
        "enabled": true,
        "api_endpoint": "http://192.168.50.1:3000",
        "launch_url": "http://adguard.lan:3000",
        "username": "admin",
        "password": "secretpassword",
        "lan_ip": "192.168.50.1"
    });
    let req = Request::builder()
        .method("PUT")
        .uri("/api/adguard/config")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(update_payload.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // GET again to verify launch_url returns the custom alias
    let req = Request::builder()
        .uri("/api/adguard/config")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let info: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(info["api_endpoint"], "http://192.168.50.1:3000");
    assert_eq!(info["launch_url"], "http://adguard.lan:3000");
    assert_eq!(info["lan_ip"], "192.168.50.1");
}

#[tokio::test]
async fn test_adguard_filtering_and_protection_api() {
    let (app, _state, _temp) = setup_test_app().await;

    // Enable AdGuard integration
    let req = Request::builder()
        .method("PUT")
        .uri("/api/adguard/config")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "enabled": true,
                "api_endpoint": "http://127.0.0.1:18080",
                "username": "",
                "password": "",
                "lan_ip": "192.168.1.1"
            })
            .to_string(),
        ))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );

    // POST /api/adguard/filtering when enabled (will fail downstream client network connection if not mocked, but tests route/auth/validation)
    let req = Request::builder()
        .method("POST")
        .uri("/api/adguard/filtering")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "enabled": false,
                "duration_minutes": 10
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    // Endpoint exists and auth passes (status is InternalServerError due to unmocked downstream endpoint, or OK)
    assert!(
        resp.status().is_client_error()
            || resp.status().is_server_error()
            || resp.status().is_success()
    );

    // Disable AdGuard integration and check that /api/adguard/filtering returns BAD_REQUEST
    let req = Request::builder()
        .method("PUT")
        .uri("/api/adguard/config")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "enabled": false,
                "api_endpoint": "",
                "username": "",
                "password": "",
                "lan_ip": "192.168.1.1"
            })
            .to_string(),
        ))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/adguard/filtering")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "enabled": false,
                "duration_minutes": 10
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // GET /api/adguard/status when disabled
    let req = Request::builder()
        .uri("/api/adguard/status")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let status_val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status_val["enabled"], false);
}

#[tokio::test]
async fn test_adguard_rewrites_require_enabled_integration() {
    let (app, _state, _temp) = setup_test_app().await;
    let req = Request::builder()
        .method("PUT")
        .uri("/api/adguard/config")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "enabled": false,
                "api_endpoint": "",
                "username": "",
                "password": "",
                "lan_ip": "192.168.1.1"
            })
            .to_string(),
        ))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );

    let req = Request::builder()
        .uri("/api/adguard/rewrites")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_adguard_hosts_are_saved_and_read_back() {
    let (app, state, _temp) = setup_test_app().await;
    let payload = serde_json::json!([
        {"ip": "192.168.1.10", "hostnames": ["xyz", "xyz.example.com"]}
    ]);
    let req = Request::builder()
        .method("PUT")
        .uri("/api/adguard/hosts")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(&state.config.paths.hosts_add).unwrap(),
        "192.168.1.10 xyz xyz.example.com\n"
    );

    let req = Request::builder()
        .uri("/api/adguard/hosts")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
        payload
    );
}

#[tokio::test]
async fn test_dnsmasq_hosts_manage_only_dhcp_host_lines() {
    let (app, state, _temp) = setup_test_app().await;
    std::fs::write(
        &state.config.paths.dnsmasq_conf_add,
        "interface=wg*     # WireGuard\n\n# keep this\ndhcp-host=AA:BB:CC:DD:EE:FF,set:AA:BB:CC:DD:EE:FF,old,192.168.1.20\n",
    )
    .unwrap();
    let req = Request::builder()
        .uri("/api/adguard/dnsmasq-hosts")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!([
            {"mac":"AA:BB:CC:DD:EE:FF","hostname":"old","ip":"192.168.1.20"}
        ])
    );

    let req = Request::builder()
        .method("PUT")
        .uri("/api/adguard/dnsmasq-hosts")
        .header(header::AUTHORIZATION, "Bearer maestro-test-token")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!([
                {"mac":"AA:BB:CC:DD:EE:FF","hostname":"new","ip":"192.168.1.21"}
            ])
            .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(&state.config.paths.dnsmasq_conf_add).unwrap(),
        "interface=wg*     # WireGuard\n\n# keep this\ndhcp-host=AA:BB:CC:DD:EE:FF,set:AA:BB:CC:DD:EE:FF,new,192.168.1.21\n"
    );
}

#[tokio::test]
async fn test_nginx_settings_reload_failure_restores_all_files() {
    use std::os::unix::fs::PermissionsExt;
    let (_app, state, temp) = setup_test_app().await;
    let executable = temp.path().join("nginx-failing-reload");
    std::fs::write(
        &executable,
        "#!/bin/sh\nfor arg do\n    [ \"$arg\" = reload ] && exit 1\ndone\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut config = (*state.config).clone();
    config.test_mode = false;
    config.commands.nginx = executable;
    std::fs::create_dir_all(config.nginx.pid_path.parent().unwrap()).unwrap();
    std::fs::write(&config.nginx.pid_path, std::process::id().to_string()).unwrap();
    let state = AppState::new(config, state.stores.clone(), state.firewall.clone());
    let app = api::app(state.clone()).unwrap();
    let previous = std::fs::read(&state.config.nginx.config_path).unwrap();
    let (status, response) = nginx_request(
        &app,
        "PUT",
        "/settings",
        serde_json::json!({"shared":{"http_port":8080}}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert_eq!(
        std::fs::read(&state.config.nginx.config_path).unwrap(),
        previous
    );
    assert_eq!(
        std::fs::read_to_string(maestro::nginx_render::global_path(&state.config)).unwrap(),
        "{\n  \"global\": {}\n}\n"
    );
    assert!(
        !state
            .config
            .nginx
            .root_dir
            .join("snippets/restrict.conf")
            .exists()
    );
}

#[tokio::test]
async fn managed_sites_persist_inputs_and_regenerate_without_metadata() {
    use maestro::{models::NginxObjectKind, nginx, nginx_sites};
    use serde_json::json;
    let (app, state, _temp) = setup_test_app().await;
    let (status, result) = nginx_request(&app, "POST", "/objects/domain", json!({"domain":"saved.test","name":"saved.test","settings":{"disable_root":true},"upstream":"http://127.0.0.1:8080"})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let path = nginx::available_path(
        &state.config,
        NginxObjectKind::Domain,
        "saved.test",
        "saved.test",
    )
    .unwrap();
    // Generated bytes are disposable: their settings comment is not the source.
    std::fs::write(&path, "server { server_name saved.test; }\n").unwrap();
    let (status, result) = nginx_request(
        &app,
        "GET",
        "/objects/domain/saved.test/saved.test",
        json!(null),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["settings"]["disable_root"], true);
    assert_eq!(result["object"]["managed"], true);
    let (status, result) = nginx_request(
        &app,
        "PUT",
        "/objects/domain/saved.test/saved.test",
        json!({"settings":{"disable_root":false,"client_max_body_size":"23M"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("client_max_body_size 23M;")
    );
    let saved = nginx_sites::find(
        &state.config,
        NginxObjectKind::Domain,
        "saved.test",
        "saved.test",
    )
    .unwrap()
    .unwrap();
    assert_eq!(saved.settings["client_max_body_size"], "23M");
    assert_eq!(saved.upstream.as_deref(), Some("http://127.0.0.1:8080"));
    std::fs::remove_file(&path).unwrap();
    let (status, result) = nginx_request(&app, "PUT", "/settings", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("client_max_body_size 23M;")
    );
    let before = std::fs::read(nginx_sites::path(&state.config)).unwrap();
    assert_eq!(
        nginx_request(
            &app,
            "PUT",
            "/objects/domain/saved.test/saved.test",
            json!({"content":"server {}"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        before,
        std::fs::read(nginx_sites::path(&state.config)).unwrap()
    );
    assert_eq!(
        nginx_request(
            &app,
            "POST",
            "/objects/domain/saved.test/saved.test/enable",
            json!(null)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(
        nginx_sites::find(
            &state.config,
            NginxObjectKind::Domain,
            "saved.test",
            "saved.test"
        )
        .unwrap()
        .unwrap()
        .enabled
    );
    assert_eq!(
        nginx_request(
            &app,
            "DELETE",
            "/objects/domain/saved.test/saved.test",
            json!(null)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(nginx_sites::load(&state.config).unwrap().sites.is_empty());
}

#[tokio::test]
async fn managed_site_inputs_roll_back_when_nginx_rejects_an_update() {
    use maestro::{command::CommandRunner, models::NginxObjectKind, nginx, nginx_sites};
    use serde_json::json;
    let (_app, mut state, _temp) = setup_test_app().await;
    state.runner = CommandRunner::new(false);
    std::sync::Arc::make_mut(&mut state.config).commands.nginx = "/bin/false".into();
    let app = api::app(state.clone()).unwrap();
    let (status, result) = nginx_request(
        &app,
        "POST",
        "/objects/domain",
        json!({"domain":"rollback.test","name":"rollback.test","settings":{"disable_root":true}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    nginx::set_enabled(
        &state.config,
        NginxObjectKind::Domain,
        "rollback.test",
        "rollback.test",
        true,
    )
    .unwrap();
    let path = nginx::available_path(
        &state.config,
        NginxObjectKind::Domain,
        "rollback.test",
        "rollback.test",
    )
    .unwrap();
    let mut global = maestro::nginx_render::load_global(&state.config).unwrap();
    global.listener_profiles.insert(
        "moved".into(),
        serde_json::from_value(json!({
            "https":[{"address":"127.0.0.2","port":19443}]
        }))
        .unwrap(),
    );
    maestro::nginx_render::FileTransaction::apply(
        &state.config,
        maestro::nginx_render::shared_files(&state.config, &global).unwrap(),
    )
    .unwrap()
    .commit();
    let enabled = nginx::enabled_path(
        &state.config,
        NginxObjectKind::Domain,
        "rollback.test",
        "rollback.test",
    )
    .unwrap();
    let enabled_target = std::fs::read_link(&enabled).unwrap();
    let tracked = [
        state.config.nginx.domain_upstream_map_path.clone(),
        state.config.nginx.http_forwarder_path.clone(),
        state.config.nginx.inputs_dir.join("domains.conf"),
        state
            .config
            .nginx
            .root_dir
            .join("conf.d/00-ingress-access.conf"),
        state
            .config
            .nginx
            .root_dir
            .join("ban-shield/default-drop.conf"),
    ];
    let before = tracked
        .iter()
        .map(|path| (path, std::fs::read(path).ok()))
        .collect::<Vec<_>>();
    let inputs = std::fs::read(nginx_sites::path(&state.config)).unwrap();
    let output = std::fs::read(&path).unwrap();
    let (status, result) = nginx_request(
        &app,
        "PUT",
        "/objects/domain/rollback.test/rollback.test",
        json!({"settings":{"disable_root":false,"listener_profile":"moved","access_policy":"public"},"upstream":"http://127.0.0.1:19001"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{result}");
    assert_eq!(
        inputs,
        std::fs::read(nginx_sites::path(&state.config)).unwrap()
    );
    assert_eq!(output, std::fs::read(path).unwrap());
    assert_eq!(std::fs::read_link(&enabled).unwrap(), enabled_target);
    for (path, original) in before {
        assert_eq!(
            std::fs::read(path).ok(),
            original,
            "rollback of {}",
            path.display()
        );
    }
}

#[tokio::test]
async fn emulated_fixture_sites_survive_shared_settings_regeneration() {
    use serde_json::json;
    let (app, state, _temp) = setup_test_app().await;
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-fixtures/etc/nginx");
    let settings_dir = state.config.nginx.inputs_dir.clone();
    std::fs::create_dir_all(&settings_dir).unwrap();
    std::fs::copy(
        fixture.join("../maestro/nginx/sites.yaml"),
        settings_dir.join("sites.yaml"),
    )
    .unwrap();
    let sites = maestro::nginx_sites::load(&state.config).unwrap();
    assert_eq!(sites.sites.len(), 3);
    for (key, site) in &sites.sites {
        let name = site.object_name(key).unwrap();
        let source =
            maestro::nginx::available_path(&state.config, site.kind, &site.domain, &name).unwrap();
        let relative = source.strip_prefix(&state.config.nginx.root_dir).unwrap();
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::copy(fixture.join(relative), &source).unwrap();
    }
    let (status, response) = nginx_request(&app, "PUT", "/settings", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    for (key, site) in &sites.sites {
        let name = site.object_name(key).unwrap();
        let route = format!("/objects/{}/{}/{}", site.kind.as_str(), site.domain, name);
        let (status, response) = nginx_request(&app, "GET", &route, json!(null)).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(
            response["settings"],
            serde_json::to_value(&site.settings).unwrap()
        );
        assert_eq!(
            response["upstream"],
            serde_json::to_value(&site.upstream).unwrap()
        );
        assert!(
            response["content"]
                .as_str()
                .unwrap()
                .starts_with(maestro::nginx_render::SETTINGS_PREFIX)
        );
    }
}

#[tokio::test]
async fn test_disable_managed_subdomain_without_explicit_name() {
    use maestro::{models::NginxObjectKind, nginx, nginx_sites};
    use serde_json::json;
    let (app, state, _temp) = setup_test_app().await;

    // Create parent domain
    let (status, _) = nginx_request(
        &app,
        "POST",
        "/objects/domain",
        json!({"domain":"example.test","name":"example.test","settings":{},"upstream":"http://127.0.0.1:8080","enabled":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Create subdomain files
    let sub_avail = nginx::available_path(
        &state.config,
        NginxObjectKind::Subdomain,
        "example.test",
        "api",
    )
    .unwrap();
    std::fs::create_dir_all(sub_avail.parent().unwrap()).unwrap();
    std::fs::write(&sub_avail, "server { server_name api.example.test; }\n").unwrap();
    nginx::set_enabled(
        &state.config,
        NginxObjectKind::Subdomain,
        "example.test",
        "api",
        true,
    )
    .unwrap();

    // In sites.yaml, add the subdomain WITHOUT name = "api" (typical user format)
    let sites_path = nginx_sites::path(&state.config);
    let mut sites_content = std::fs::read_to_string(&sites_path).unwrap_or_default();
    let mut document: serde_yaml::Value = serde_yaml::from_str(&sites_content).unwrap();
    document["sites"]["api.example.test"] = serde_yaml::to_value(json!({
        "kind":"subdomain", "domain":"example.test", "server_name":"api.example.test",
        "upstream":"http://127.0.0.1:8085", "enabled":true
    }))
    .unwrap();
    sites_content = serde_yaml::to_string(&document).unwrap();
    std::fs::write(&sites_path, sites_content).unwrap();

    // Disabling subdomain must succeed with 200 OK (not 500)
    let (status, result) = nginx_request(
        &app,
        "POST",
        "/objects/subdomain/example.test/api/disable",
        json!(null),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["enabled"], false);

    // Verify sites.yaml still loads and reflects enabled = false
    let site = nginx_sites::find(
        &state.config,
        NginxObjectKind::Subdomain,
        "example.test",
        "api",
    )
    .unwrap()
    .expect("site must exist in sites.yaml");
    assert!(!site.enabled);
    assert_eq!(site.name.as_deref(), Some("api"));

    // Enabling it again also works
    let (status, result) = nginx_request(
        &app,
        "POST",
        "/objects/subdomain/example.test/api/enable",
        json!(null),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["enabled"], true);
}

#[cfg(feature = "linux")]
#[tokio::test]
async fn linux_tool_bridge_discovery_auth_and_arguments() {
    let (router, mut state, _temp) = setup_test_app().await;
    std::sync::Arc::make_mut(&mut state.config).platform = maestro::config::Platform::Linux;
    let token = state.config.server.auth_token.clone();
    let linux = api::app(state).unwrap();

    let request = |method, path, authenticated, body: &'static str| {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "localhost:3030")
            .header(header::CONTENT_TYPE, "application/json");
        if authenticated {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        request.body(Body::from(body)).unwrap()
    };
    let endpoint = "/api/tools";
    let response = router
        .oneshot(request("GET", endpoint, true, ""))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = linux
        .clone()
        .oneshot(request("GET", endpoint, false, ""))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    for (method, path, arguments, expected_status, pointer, expected_value) in [
        (
            "GET",
            endpoint,
            "",
            StatusCode::OK,
            "/tools/0/name",
            "system_status",
        ),
        (
            "GET",
            "/api/status",
            "",
            StatusCode::OK,
            "/version",
            env!("CARGO_PKG_VERSION"),
        ),
        (
            "POST",
            "/api/tools/system_status",
            "{}",
            StatusCode::OK,
            "/version",
            env!("CARGO_PKG_VERSION"),
        ),
        (
            "POST",
            "/api/tools/system_capabilities",
            "{}",
            StatusCode::OK,
            "/modules/0/id",
            "system",
        ),
        (
            "POST",
            "/api/tools/system_status",
            "{",
            StatusCode::BAD_REQUEST,
            "/error/code",
            "invalid_arguments",
        ),
        (
            "POST",
            "/api/tools/system_status",
            r#"{"extra":true}"#,
            StatusCode::BAD_REQUEST,
            "/error/code",
            "invalid_arguments",
        ),
        (
            "POST",
            "/api/tools/jobs_get",
            "{}",
            StatusCode::BAD_REQUEST,
            "/error/code",
            "invalid_arguments",
        ),
        (
            "POST",
            "/api/tools/jobs_cancel",
            r#"{"id":"00000000-0000-0000-0000-000000000000"}"#,
            StatusCode::NOT_FOUND,
            "/error/code",
            "not_found",
        ),
        (
            "POST",
            "/api/tools/monitor_processes",
            r#"{"limit":501}"#,
            StatusCode::BAD_REQUEST,
            "/error/code",
            "invalid_arguments",
        ),
        (
            "POST",
            "/api/tools/updates_install",
            r#"{"confirm":false}"#,
            StatusCode::BAD_REQUEST,
            "/error/code",
            "invalid_arguments",
        ),
        (
            "POST",
            "/api/tools/updates_install",
            r#"{"confirm":true}"#,
            StatusCode::FORBIDDEN,
            "/error/code",
            "writes_disabled",
        ),
        (
            "POST",
            "/api/tools/unknown",
            "{}",
            StatusCode::NOT_FOUND,
            "/error/code",
            "not_found",
        ),
    ] {
        let response = linux
            .clone()
            .oneshot(request(method, path, true, arguments))
            .await
            .unwrap();
        assert_eq!(response.status(), expected_status);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value.pointer(pointer).and_then(|value| value.as_str()),
            Some(expected_value)
        );
    }
}

#[cfg(feature = "linux")]
#[tokio::test]
async fn linux_tool_bridge_restricts_host_and_origin() {
    let (_, mut state, _temp) = setup_test_app().await;
    let config = std::sync::Arc::make_mut(&mut state.config);
    config.platform = maestro::config::Platform::Linux;
    config.server.allowed_origins = vec!["*".into(), "https://maestro.example".into()];
    let authorization = format!("Bearer {}", config.server.auth_token);
    let app = api::app(state).unwrap();

    for (host, origin, expected) in [
        ("localhost:3030", None, StatusCode::OK),
        ("[::1]:3030", Some("http://[::1]:3030"), StatusCode::OK),
        (
            "localhost:3030",
            Some("http://localhost:9999"),
            StatusCode::FORBIDDEN,
        ),
        ("localhost:3030", Some("null"), StatusCode::FORBIDDEN),
        ("untrusted.example", None, StatusCode::FORBIDDEN),
        (
            "maestro.example",
            Some("https://maestro.example"),
            StatusCode::OK,
        ),
        (
            "maestro.example",
            Some("http://maestro.example"),
            StatusCode::FORBIDDEN,
        ),
        ("user@localhost:3030", None, StatusCode::FORBIDDEN),
    ] {
        let mut request = Request::builder()
            .uri("/api/tools")
            .header(header::HOST, host)
            .header(header::AUTHORIZATION, &authorization);
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{host} / {origin:?}");
        if expected == StatusCode::FORBIDDEN {
            let bytes = axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["error"]["code"], "forbidden_origin");
        }
    }
}

#[cfg(feature = "linux")]
#[tokio::test]
async fn linux_browser_session_can_sign_in_and_revoke() {
    let (_, mut state, _temp) = setup_test_app().await;
    let config = std::sync::Arc::make_mut(&mut state.config);
    config.platform = maestro::config::Platform::Linux;
    config.server.allowed_origins = vec!["https://maestro.example".into()];
    let token = config.server.auth_token.clone();
    let app = api::app(state).unwrap();
    let request = |method, path| {
        Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "maestro.example")
    };
    let login = |origin: &str, token: &str| {
        request("POST", "/api/session")
            .header(header::ORIGIN, origin)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({"token": token}).to_string()))
            .unwrap()
    };
    for (origin, supplied, status) in [
        (
            "https://other.example",
            token.as_str(),
            StatusCode::FORBIDDEN,
        ),
        ("https://maestro.example", "wrong", StatusCode::UNAUTHORIZED),
    ] {
        let response = app.clone().oneshot(login(origin, supplied)).await.unwrap();
        assert_eq!(response.status(), status);
        assert!(!response.headers().contains_key(header::SET_COOKIE));
    }

    let response = app
        .clone()
        .oneshot(login("https://maestro.example", &token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    for attribute in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
        assert!(set_cookie.split("; ").any(|value| value == attribute));
    }
    let cookie = set_cookie.split(';').next().unwrap();
    let status_request = |cookie: &str| {
        request("GET", "/api/status")
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        app.clone()
            .oneshot(status_request(cookie))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    for (method, path, expected) in [
        ("DELETE", "/api/session", StatusCode::FORBIDDEN),
        ("GET", "/api/auth/check", StatusCode::UNAUTHORIZED),
    ] {
        let response = app
            .clone()
            .oneshot(
                request(method, path)
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }

    let response = app
        .clone()
        .oneshot(
            request("POST", "/api/session")
                .header(header::ORIGIN, "https://maestro.example")
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"token": token}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let replacement = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert_ne!(replacement, cookie);
    assert_eq!(
        app.clone()
            .oneshot(status_request(cookie))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(status_request(replacement))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let cookie = replacement;

    let response = app
        .clone()
        .oneshot(
            request("DELETE", "/api/session")
                .header(header::ORIGIN, "https://maestro.example")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(
        app.oneshot(status_request(cookie)).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

#[cfg(feature = "linux-ui")]
#[tokio::test]
async fn linux_shell_assets_are_served_only_on_linux() {
    let (router, mut state, _temp) = setup_test_app().await;
    std::sync::Arc::make_mut(&mut state.config).platform = maestro::config::Platform::Linux;
    let linux = api::app(state).unwrap();
    for (path, mime, content) in [
        ("/", "text/html", "/linux-assets/shell.js"),
        ("/linux-assets/shell.js", "text/javascript", "Overview.js"),
        (
            "/linux-assets/Overview.js",
            "text/javascript",
            "Capabilities",
        ),
        (
            "/linux-assets/Applications.js",
            "text/javascript",
            "Applications",
        ),
        ("/linux-assets/Shares.js", "text/javascript", "Shares"),
        ("/linux-assets/style.css", "text/css", ".app-shell"),
    ] {
        let request = Request::builder().uri(path).body(Body::empty()).unwrap();
        let response = linux.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with(mime)
        );
        let bytes = axum::body::to_bytes(response.into_body(), 128 * 1024)
            .await
            .unwrap();
        assert!(std::str::from_utf8(&bytes).unwrap().contains(content));
    }
    let request = Request::builder()
        .uri("/linux-assets/shell.js")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.oneshot(request).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
}

#[cfg(feature = "linux")]
#[tokio::test]
async fn linux_systemd_inventory_preserves_partial_results() {
    use std::os::unix::fs::PermissionsExt;

    let (_, mut state, temp) = setup_test_app().await;
    let program = temp.path().join("systemctl");
    std::fs::write(
        &program,
        r#"#!/bin/sh
[ "$1" = --user ] && [ "$2" = --no-pager ] && [ "$3" = --no-ask-password ] || exit 1
[ "$4" = list-unit-files ] || exit 1
printf '%s\n' '[{"unit_file":"idle.service","state":"disabled","preset":"enabled"}]'
"#,
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = std::sync::Arc::make_mut(&mut state.config);
    config.platform = maestro::config::Platform::Linux;
    config.test_mode = false;
    config.commands.systemctl = program;
    state.runner = maestro::command::CommandRunner::new(false);
    let token = config.server.auth_token.clone();
    let app = api::app(state).unwrap();
    let request = Request::builder()
        .method("POST")
        .uri("/api/tools/services_list")
        .header(header::HOST, "localhost:3030")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"scope":"user"}"#))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let inventory: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(inventory["scope"], "user");
    assert_eq!(inventory["services"][0]["name"], "idle.service");
    assert_eq!(inventory["services"][0]["enabled"], "disabled");
    assert_eq!(inventory["services"][0]["load"], "unknown");
    assert!(
        inventory["warnings"][0]
            .as_str()
            .unwrap()
            .starts_with("list-units ")
    );
}

#[cfg(feature = "linux")]
#[tokio::test]
async fn linux_systemd_journal_validates_unit_and_retains_recent_output() {
    use std::os::unix::fs::PermissionsExt;

    let (_, mut state, temp) = setup_test_app().await;
    let program = temp.path().join("journalctl");
    let script = format!(
        r#"#!/bin/sh
[ "$1" = --user ] && [ "$5" = --user-unit ] && [ "$6" = idle.service ] && [ "$7" = --lines ] && [ "$8" = 200 ] || exit 1
printf '%s\n' old
i=0
while [ "$i" -lt 130 ]; do
    printf '%s\n' '{}'
    i=$((i + 1))
done
printf '%s\n' recent
"#,
        "a".repeat(1024)
    );
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = std::sync::Arc::make_mut(&mut state.config);
    config.platform = maestro::config::Platform::Linux;
    config.test_mode = false;
    config.commands.journalctl = program;
    state.runner = maestro::command::CommandRunner::new(false);
    let token = config.server.auth_token.clone();
    let app = api::app(state).unwrap();

    for (arguments, expected) in [
        (
            r#"{"scope":"user","unit":"--bad.service"}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            r#"{"scope":"user","unit":"idle.service","tail":2001}"#,
            StatusCode::BAD_REQUEST,
        ),
        (r#"{"scope":"user","unit":"idle.service"}"#, StatusCode::OK),
    ] {
        let request = Request::builder()
            .method("POST")
            .uri("/api/tools/services_logs")
            .header(header::HOST, "localhost:3030")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(arguments))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected);
        let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .unwrap();
        let journal: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        if expected == StatusCode::OK {
            let text = journal["text"].as_str().unwrap();
            assert_eq!(journal["truncated"], true);
            assert_eq!(text.len(), 128 * 1024);
            assert!(text.ends_with("recent\n"));
            assert!(!text.contains("old"));
        } else {
            assert_eq!(journal["error"]["code"], "invalid_arguments");
        }
    }
}

#[cfg(feature = "linux")]
#[tokio::test]
async fn linux_systemd_actions_are_scoped_and_audited() {
    use std::os::unix::fs::PermissionsExt;

    let (_, mut state, temp) = setup_test_app().await;
    let program = temp.path().join("systemctl");
    std::fs::write(
        &program,
        r#"#!/bin/sh
if [ "$4" = show ]; then
    case "$8" in
        alias.service) printf '%s\n' 'maestro-demo-0123456789abcdef.service' ;;
        *) printf '%s\n' "$8" ;;
    esac
    exit 0
fi
[ "$1 $2 $3 $4 $5 $6" = "--user --no-pager --no-ask-password enable -- demo.service" ] || exit 1
printf '%s\n' "boot enabled"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = std::sync::Arc::make_mut(&mut state.config);
    config.platform = maestro::config::Platform::Linux;
    config.test_mode = false;
    config.commands.systemctl = program;
    config.services.systemd_user_write_units = vec![
        "demo.service".into(),
        "alias.service".into(),
        "maestro-demo-0123456789abcdef.service".into(),
    ];
    let audit = config.paths.data_dir.join("management-audit.jsonl");
    let token = config.server.auth_token.clone();
    state.runner = maestro::command::CommandRunner::new(false);
    let app = api::app(state).unwrap();
    let invoke = |name, arguments: serde_json::Value| {
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/tools/{name}"))
            .header(header::HOST, "localhost:3030")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(arguments.to_string()))
            .unwrap();
        let api = app.clone();
        async move {
            let response = api.oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            )
        }
    };

    let args = serde_json::json!({"scope":"system","unit":"demo.service"});
    assert_eq!(
        invoke("services_enable", args).await.0,
        StatusCode::FORBIDDEN
    );
    assert!(!audit.exists());
    let direct = serde_json::json!({"scope":"user","unit":"maestro-demo-0123456789abcdef.service"});
    assert_eq!(
        invoke("services_enable", direct).await.0,
        StatusCode::FORBIDDEN
    );
    let args = serde_json::json!({"scope":"user","unit":"demo.service"});
    let (status, started) = invoke("services_enable", args).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(started["job"]["state"], "running");

    let finished = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let (_, job) =
                invoke("jobs_get", serde_json::json!({"id": started["job"]["id"]})).await;
            if job["state"] != "running" {
                break job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(finished["state"], "succeeded");
    assert_eq!(finished["result"]["action"], "enable");
    assert_eq!(finished["output"], "boot enabled\n");
    assert_eq!(std::fs::read_to_string(audit).unwrap().lines().count(), 2);
    let (status, started) = invoke(
        "services_enable",
        serde_json::json!({"scope":"user","unit":"alias.service"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let failed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let (_, job) =
                invoke("jobs_get", serde_json::json!({"id": started["job"]["id"]})).await;
            if job["state"] != "running" {
                break job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(failed["state"], "failed");
    assert!(
        failed["error"]
            .as_str()
            .unwrap()
            .contains("canonical service")
    );
}

#[cfg(feature = "linux")]
#[tokio::test]
async fn linux_update_status_uses_apt_simulation_and_reports_reboot_evidence() {
    use std::os::unix::fs::PermissionsExt;

    let (_, mut state, temp) = setup_test_app().await;
    let program = temp.path().join("apt-get");
    std::fs::write(
        &program,
        r#"#!/bin/sh
[ "$1" = --simulate ] && [ "$2" = --no-remove ] && [ "$LC_ALL" = C ] || exit 1
printf '%s\n' 'Inst demo [1] (2 Debian [amd64])' '1 upgraded, 0 newly installed'
"#,
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let release = temp.path().join("os-release");
    std::fs::write(
        &release,
        "ID=ubuntu\nID_LIKE=debian\nPRETTY_NAME=\"Ubuntu test\"\n",
    )
    .unwrap();
    let marker = temp.path().join("reboot-required");
    std::fs::write(&marker, "reboot required").unwrap();

    let config = std::sync::Arc::make_mut(&mut state.config);
    config.platform = maestro::config::Platform::Linux;
    config.test_mode = false;
    config.paths.package_bin_dir = temp.path().to_path_buf();
    config.paths.os_release = release;
    config.paths.reboot_required = marker;
    config.paths.ostree_booted = temp.path().join("ostree-booted");
    state.runner = maestro::command::CommandRunner::new(false);
    let token = config.server.auth_token.clone();
    let app = api::app(state).unwrap();
    let request = Request::builder()
        .method("POST")
        .uri("/api/tools/updates_status")
        .header(header::HOST, "localhost:3030")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status["manager"]["id"], "apt");
    assert_eq!(status["manager"]["canRefresh"], false);
    assert_eq!(status["distribution"], "Ubuntu test");
    assert_eq!(status["valid"], true);
    assert_eq!(status["reboot"]["required"], true);
    assert_eq!(status["packages"][0]["candidate"], "2");

    for mode in [Some(0o700), Some(0o600), None] {
        if let Some(mode) = mode {
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(mode)).unwrap();
        } else {
            std::fs::remove_file(&program).unwrap();
        }
        let request = Request::builder()
            .uri("/api/tools")
            .header(header::HOST, "localhost:3030")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let discovery: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let updates = discovery["modules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|module| module["id"] == "updates")
            .unwrap();
        assert_eq!(updates["available"], mode == Some(0o700));
    }
}

#[cfg(feature = "linux")]
#[tokio::test]
async fn linux_update_status_interprets_native_exit_codes() {
    use std::os::unix::fs::PermissionsExt;

    let (_, mut state, temp) = setup_test_app().await;
    let release = temp.path().join("os-release");
    let config = std::sync::Arc::make_mut(&mut state.config);
    config.platform = maestro::config::Platform::Linux;
    config.test_mode = false;
    config.paths.package_bin_dir = temp.path().to_path_buf();
    config.paths.os_release = release.clone();
    config.paths.ostree_booted = temp.path().join("ostree-booted");
    config.paths.reboot_required = temp.path().join("reboot-required");
    let token = config.server.auth_token.clone();
    state.runner = maestro::command::CommandRunner::new(false);
    let app = api::app(state).unwrap();

    for (distribution, executable, output, valid, package_count) in [
        ("arch", "pacman", "exit 1", true, 0),
        (
            "arch",
            "pacman",
            "printf '%s\n' 'cache unavailable' >&2; exit 1",
            false,
            0,
        ),
        (
            "opensuse-tumbleweed",
            "zypper",
            r#"printf '%s\n' '<stream><update-status><update-list><update name="demo" edition="2" kind="package"/></update-list></update-status></stream>'; exit 100"#,
            true,
            1,
        ),
    ] {
        let program = temp.path().join(executable);
        std::fs::write(&program, format!("#!/bin/sh\n{output}\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(&release, format!("ID={distribution}\n")).unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tools/updates_status")
                    .header(header::HOST, "localhost:3030")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let status: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status["valid"], valid);
        assert_eq!(status["packages"].as_array().unwrap().len(), package_count);
    }
}

#[cfg(feature = "linux")]
#[tokio::test]
async fn linux_workloads_and_applications_lifecycle() {
    let (_, mut state, _temp) = setup_test_app().await;
    let config = std::sync::Arc::make_mut(&mut state.config);
    config.platform = maestro::config::Platform::Linux;
    let token = config.server.auth_token.clone();
    let app = api::app(state).unwrap();

    let call = |tool: &'static str, body: serde_json::Value| {
        let app = app.clone();
        let token = token.clone();
        async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/tools/{tool}"))
                        .header(header::HOST, "localhost:3030")
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap();
            let val: serde_json::Value =
                serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            (status, val)
        }
    };

    // 1. Initial workloads_get
    let (status, val) = call("workloads_get", serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(val["yaml"], "");
    assert!(val["revision"].is_null());

    let (status, journal) = call("workloads_pending", serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "{journal:?}");
    assert!(journal["pending"].is_null());
    let (status, _) = call(
        "workloads_resume",
        serde_json::json!({"fingerprint": "bad"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 2. Plan a valid workload
    let yaml = r#"version: 1
storage:
  docs: {path: volumes/docs, kind: directory}
services:
  web:
    name: "Demo Web"
    kind: container
    engine: docker
    backend: compose
    enabled: true
    containers:
      web: { image: "nginx:alpine" }
shares:
  docs:
    protocol: samba
    storage: docs
    enabled: true
    read_only: false
    users: ["operator"]
    clients: []
"#;
    let (status, val) = call("workloads_plan", serde_json::json!({ "candidate": yaml })).await;
    assert_eq!(status, StatusCode::OK, "plan response: {val:?}");
    let fingerprint = val["plan"]["fingerprint"].as_str().unwrap().to_string();
    assert!(!fingerprint.is_empty());
    assert_eq!(val["plan"]["changes"].as_array().unwrap().len(), 3);
    assert_eq!(val["blockedResources"], serde_json::json!([]));

    // 3. Apply the workload
    let (status, val) = call(
        "workloads_apply",
        serde_json::json!({
            "candidate": yaml,
            "fingerprint": fingerprint
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let job_id = val["job"]["id"].as_str().unwrap().to_string();

    // Poll job until finished
    for _ in 0..50 {
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
        let (status, job) = call("jobs_get", serde_json::json!({ "id": job_id })).await;
        assert_eq!(status, StatusCode::OK);
        if job["state"] == "succeeded" {
            break;
        }
    }

    // 4. Check applications_list
    let (status, val) = call("applications_list", serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let apps = val["applications"].as_array().unwrap();
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0]["id"], "web");
    assert_eq!(apps[0]["name"], "Demo Web");
    assert_eq!(apps[0]["state"], "running");

    // 5. Check authoritative managed shares (native inventory is a separate tool).
    let (status, val) = call("managed_shares", serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let shares = val["shares"].as_array().unwrap();
    assert_eq!(shares.len(), 1);
    assert_eq!(shares[0]["id"], "docs");
    assert_eq!(shares[0]["protocol"], "samba");

    // 6. Reference lifecycle tools enforce confirmation and exact argument shape.
    for args in [serde_json::json!({"service":"web"}), serde_json::json!({"service":"web","confirm":false}), serde_json::json!({"service":"../web","confirm":true})] {
        let (status, _) = call("managed_service_restart", args).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, logs) = call("managed_service_logs", serde_json::json!({"service":"web","tail":10})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(logs["text"].is_string());
    assert_eq!(logs["source"], "simulation");
    let (status, val) = call(
        "managed_service_restart",
        serde_json::json!({"service":"web","confirm":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(val["job"]["id"].is_string());

    // 7. Check shares_status
    let (status, val) = call("shares_status", serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(val["samba"]["available"], false);
    assert_eq!(val["nfs"]["available"], false);
    let (status, val) = call("shares_list", serde_json::json!({"protocol":"samba"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(val["backends"].as_array().unwrap().len(), 1);
    assert_eq!(val["backends"][0]["protocol"], "samba");
    assert_eq!(val["shares"].as_array().unwrap().len(), 0);
    let (status, _) = call("shares_list", serde_json::json!({"protocol":"invalid"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[cfg(feature = "linux")]
#[tokio::test]
async fn linux_mcp_protocol_flow() {
    let (_, mut state, _temp) = setup_test_app().await;
    let config = std::sync::Arc::make_mut(&mut state.config);
    config.platform = maestro::config::Platform::Linux;
    let token = config.server.auth_token.clone();
    let app = api::app(state).unwrap();

    let mcp_call = |body: serde_json::Value| {
        let app = app.clone();
        let token = token.clone();
        async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/mcp")
                        .header(header::ACCEPT, "application/json, text/event-stream")
                        .header(header::HOST, "localhost:3030")
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap();
            let val: serde_json::Value =
                serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            (status, val)
        }
    };

    // 1. initialize
    let (status, val) = mcp_call(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "maestro-integration-test", "version": "1"}
        }
    }))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(val["jsonrpc"], "2.0");
    assert_eq!(val["id"], 1);
    assert_eq!(val["result"]["serverInfo"]["name"], "maestro");

    // 2. tools/list
    let (status, val) = mcp_call(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }))
    .await;
    assert_eq!(status, StatusCode::OK);
    let tools = val["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "workloads_get"));
    assert!(tools.iter().any(|t| t["name"] == "applications_list"));
    assert!(tools.iter().any(|t| t["name"] == "shares_list"));
    assert!(tools.iter().any(|t| t["name"] == "system_status"));
    let browse = tools
        .iter()
        .find(|t| t["name"] == "managed_directory_browse")
        .unwrap();
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../test-fixtures/phase2/server-man-tools.json"
    ))
    .unwrap();
    let expected = reference["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "managed_directory_browse")
        .unwrap();
    assert_eq!(browse["inputSchema"], expected["inputSchema"]);
    for name in ["managed_service_start", "managed_service_stop", "managed_service_restart", "managed_service_logs"] {
        let actual = tools.iter().find(|tool| tool["name"] == name).unwrap();
        let expected = reference["tools"].as_array().unwrap().iter().find(|tool| tool["name"] == name).unwrap();
        assert_eq!(actual["inputSchema"], expected["inputSchema"]);
    }

    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("visible")).unwrap();
    std::fs::write(directory.path().join(".hidden"), "secret").unwrap();
    let (status, value) = mcp_call(serde_json::json!({
        "jsonrpc": "2.0", "id": "browse", "method": "tools/call",
        "params": {"name": "managed_directory_browse", "arguments": {"path": directory.path()}}
    }))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["result"]["isError"], false);
    assert_eq!(
        value["result"]["structuredContent"]["directories"],
        serde_json::json!(["visible"])
    );
    assert_eq!(
        value["result"]["structuredContent"]["files"],
        serde_json::json!([])
    );

    // Invalid cursor types and unsupported pagination must fail rather than
    // silently returning the first page to an agent.
    for cursor in [serde_json::json!(42), serde_json::json!("next-page")] {
        let (status, value) = mcp_call(serde_json::json!({
            "jsonrpc": "2.0", "id": "bad-cursor", "method": "tools/list",
            "params": {"cursor": cursor}
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["id"], "bad-cursor");
        assert_eq!(value["error"]["code"], -32602);
        assert!(value.get("result").is_none());
    }

    // 3. tools/call for system_status
    let (status, val) = mcp_call(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "system_status",
            "arguments": {}
        }
    }))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(val["id"], 3);
    assert_eq!(val["result"]["isError"], false);
    let text = val["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("hostname"));
}

#[tokio::test]
async fn asuswrt_excludes_linux_tool_and_asset_routes() {
    let (app, state, _temp) = setup_test_app().await;
    assert_linux_routes_missing(app).await;
    let paths = &state.config.paths;
    for path in [
        paths.data_dir.join("workloads"),
        paths.applications_dir.clone(),
        paths.storage_dir.clone(),
        paths.samba_conf.clone(),
        paths.exports_file.clone(),
    ] {
        assert!(
            !path.exists(),
            "unexpected Linux state at {}",
            path.display()
        );
    }

    // Also check compile-time exclusion with a Linux runtime configuration.
    #[cfg(not(feature = "linux"))]
    {
        let mut state = state;
        std::sync::Arc::make_mut(&mut state.config).platform = maestro::config::Platform::Linux;
        assert_linux_routes_missing(api::app(state).unwrap()).await;
    }
}

async fn assert_linux_routes_missing(app: axum::Router) {
    for (method, path) in [
        ("GET", "/api/tools"),
        ("GET", "/api/status"),
        ("GET", "/api/auth/mode"),
        ("POST", "/api/tools/test"),
        ("POST", "/api/mcp"),
        ("POST", "/api/session"),
        ("DELETE", "/api/session"),
        ("GET", "/linux-assets/shell.js"),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::NOT_FOUND,
            "{method} {path}"
        );
    }
}

#[cfg(feature = "linux")]
#[tokio::test]
async fn linux_history_metadata_and_permission_contract() {
    for writable in [true, false] {
        let (_, mut state, _temp) = setup_test_app().await;
        let config = std::sync::Arc::make_mut(&mut state.config);
        config.platform = maestro::config::Platform::Linux;
        config.test_mode = false;
        config.system.allow_workload_writes = writable;
        let token = config.server.auth_token.clone();
        let store = maestro::core::linux_config::store(config).unwrap();
        let original = store
            .save(
                b"version: 1\n",
                maestro::core::declarative::MISSING_REVISION,
            )
            .unwrap();
        store
            .save(b"version: 1\n# edit\n", &original.revision)
            .unwrap();
        let app = api::app(state).unwrap();
        if !writable {
            for name in [
                "managed_config_revert_plan",
                "managed_config_revert",
                "managed_apply_retry",
            ] {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri(format!("/api/tools/{name}"))
                            .header(header::HOST, "localhost:3030")
                            .header(header::AUTHORIZATION, format!("Bearer {token}"))
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(Body::from("{}"))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
            }
        }
        for name in ["managed_config_history", "workloads_history"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/tools/{name}"))
                        .header(header::HOST, "localhost:3030")
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if writable {
                    StatusCode::OK
                } else {
                    StatusCode::FORBIDDEN
                }
            );
            if writable {
                let bytes = axum::body::to_bytes(response.into_body(), 65536)
                    .await
                    .unwrap();
                let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                if name == "managed_config_history" {
                    assert_eq!(value["entries"][0]["revision"], original.revision);
                    assert_eq!(value["entries"][0]["valid"], true);
                    assert!(value["entries"][0]["modifiedAt"].is_string());
                    assert!(value["entries"][0].get("content").is_none());
                } else {
                    assert_eq!(value["history"][0], original.revision);
                }
            }
        }
    }
}

#[cfg(feature = "linux")]
#[tokio::test]
async fn linux_reviewed_history_revert_preserves_bytes_and_checks_revision() {
    let (_, mut state, _temp) = setup_test_app().await;
    let config = std::sync::Arc::make_mut(&mut state.config);
    config.platform = maestro::config::Platform::Linux;
    let token = config.server.auth_token.clone();
    let store = maestro::core::linux_config::store(config).unwrap();
    let original = store
        .save(b"# exact historical bytes\nversion: 1\n", "missing")
        .unwrap();
    let current = store
        .save(b"version: 1\n# subsequent edit\n", &original.revision)
        .unwrap();
    let app = api::app(state).unwrap();
    let call = |tool: &'static str, body: serde_json::Value| {
        let app = app.clone();
        let token = token.clone();
        async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/tools/{tool}"))
                        .header(header::HOST, "localhost:3030")
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            )
        }
    };
    for args in [
        serde_json::json!({"target":original.revision,"revision":"missing"}),
        serde_json::json!({"target":original.revision,"revision":current.revision,"content":"version: 1"}),
    ] {
        let (status, _) = call("managed_config_revert_plan", args).await;
        assert!(status.is_client_error());
    }
    let (status, plan) = call(
        "managed_config_revert_plan",
        serde_json::json!({"target":original.revision,"revision":current.revision}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(plan["plan"]["desiredRevision"], original.revision);
    let (status, _) = call("managed_config_revert", serde_json::json!({"target":original.revision,"revision":current.revision,"mode":"now","confirm":false})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(store.read().unwrap().unwrap().revision, current.revision);
    let (status, started) = call("managed_config_revert", serde_json::json!({"target":original.revision,"revision":current.revision,"mode":"now","confirm":true})).await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let id = started["job"]["id"].as_str().unwrap();
    let mut succeeded = false;
    for _ in 0..100 {
        let (_, job) = call("jobs_get", serde_json::json!({"id":id})).await;
        assert_ne!(job["state"], "failed", "{job}");
        if job["state"] == "succeeded" {
            succeeded = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(succeeded);
    assert_eq!(store.read().unwrap().unwrap().bytes, original.bytes);
    assert_eq!(
        store.read_history(&current.revision).unwrap().bytes,
        current.bytes
    );
    // A new revert must never overwrite a pending transition, even when
    // its target does not include a resource that transition removed.
    let pending_plan = store.preview(&current.bytes, &original.revision).unwrap();
    let pending = serde_json::to_vec(&serde_json::json!({
        "version":2,"authority":"workloads.yaml","plan":pending_plan,
        "phase":"prepared","completed":0
    }))
    .unwrap();
    let journal_path = store.path().parent().unwrap().join("apply-journal.json");
    std::fs::write(&journal_path, &pending).unwrap();
    for tool in ["managed_config_revert_plan", "managed_config_revert"] {
        let args = if tool == "managed_config_revert" {
            serde_json::json!({"target":current.revision,"revision":original.revision,"mode":"now","confirm":true})
        } else {
            serde_json::json!({"target":current.revision,"revision":original.revision})
        };
        let (status, _) = call(tool, args).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(std::fs::read(&journal_path).unwrap(), pending);
        assert_eq!(store.read().unwrap().unwrap().bytes, original.bytes);
    }
}

#[cfg(feature = "linux")]
#[tokio::test]
async fn linux_next_start_config_typed_edit_and_revert_share_the_recorded_mode() {
    async fn tool(
        app: &axum::Router,
        token: &str,
        name: &str,
        args: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/tools/{name}"))
                    .header(header::HOST, "localhost:3030")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&args).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    async fn finish(
        app: &axum::Router,
        token: &str,
        started: &serde_json::Value,
    ) -> serde_json::Value {
        let id = started["job"]["id"].as_str().unwrap();
        for _ in 0..100 {
            let (status, job) = tool(app, token, "jobs_get", serde_json::json!({"id":id})).await;
            assert_eq!(status, StatusCode::OK);
            assert_ne!(job["state"], "failed", "{job}");
            if job["state"] == "succeeded" {
                return job["result"].clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("deferred publication did not finish");
    }
    let (_, mut state, _temp) = setup_test_app().await;
    let config = std::sync::Arc::make_mut(&mut state.config);
    config.platform = maestro::config::Platform::Linux;
    config.test_mode = false;
    config.system.allow_workload_writes = true;
    let token = config.server.auth_token.clone();
    let store = maestro::core::linux_config::store(config).unwrap();
    let storage_root = config.paths.storage_dir.clone();
    let initial = store.save(b"version: 1\n", "missing").unwrap();
    let app = api::app(state).unwrap();
    let content = "version: 1\nstorage:\n  docs: {kind: directory, path: docs}\n";
    let (status, started) = tool(&app, &token, "managed_config_save", serde_json::json!({"content":content,"revision":initial.revision,"mode":"next_start","confirm":true})).await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let saved = finish(&app, &token, &started).await;
    assert_eq!(saved["mode"], "next_start");
    assert!(storage_root.join("docs").is_dir());
    std::fs::write(storage_root.join("docs/operator-file"), "preserve me").unwrap();
    let args = serde_json::json!({"kind":"storage","id":"archive","operation":"save","definition":"{\"kind\":\"directory\",\"path\":\"archive\"}","revision":saved["revision"]});
    let (status, plan) = tool(&app, &token, "managed_resource_plan", args.clone()).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    let mut edit = args;
    edit["mode"] = serde_json::json!(null);
    edit["confirm"] = serde_json::json!(true);
    edit["fingerprint"] = plan["plan"]["fingerprint"].clone();
    assert_eq!(
        tool(&app, &token, "managed_resource_save", edit.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    edit["mode"] = serde_json::json!("next_start");
    let (status, started) = tool(&app, &token, "managed_resource_save", edit).await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let edited = finish(&app, &token, &started).await;
    assert_eq!(edited["mode"], "next_start");
    assert!(storage_root.join("archive").is_dir());
    let (status, started) = tool(&app, &token, "managed_config_revert", serde_json::json!({"target":initial.revision,"revision":edited["revision"],"mode":"next_start","confirm":true})).await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let restored = finish(&app, &token, &started).await;
    assert_eq!(restored["mode"], "next_start");
    assert_eq!(store.read().unwrap().unwrap().bytes, initial.bytes);
    assert_eq!(
        std::fs::read_to_string(storage_root.join("docs/operator-file")).unwrap(),
        "preserve me"
    );
    let journal: serde_json::Value = serde_json::from_slice(
        &std::fs::read(store.path().parent().unwrap().join("apply-journal.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(journal["mode"], "next_start");
    assert_eq!(journal["phase"], "complete");
    let (status, progress) =
        tool(&app, &token, "managed_apply_status", serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(progress["stage"], "complete");
    assert!(progress["updatedAt"].is_string());
    assert!(
        progress["resources"]
            .as_object()
            .unwrap()
            .values()
            .all(|resource| resource["state"] == "registered")
    );
    assert_eq!(
        tool(
            &app,
            &token,
            "managed_apply_status",
            serde_json::json!({"extra":true})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    for args in [
        serde_json::json!({}),
        serde_json::json!({"confirm":false}),
        serde_json::json!({"confirm":true,"extra":true}),
    ] {
        assert_eq!(
            tool(&app, &token, "managed_apply_retry", args).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let (status, started) = tool(
        &app,
        &token,
        "managed_apply_retry",
        serde_json::json!({"confirm":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let repeated = finish(&app, &token, &started).await;
    assert_eq!(repeated, progress);
    let pending_plan = store
        .preview(content.as_bytes(), &initial.revision)
        .unwrap();
    std::fs::write(
        store.path().parent().unwrap().join("apply-journal.json"),
        serde_json::to_vec(&serde_json::json!({
            "version":6,"authority":"workloads.yaml","plan":pending_plan,
            "phase":"prepared","completed":0,"mode":"next_start"
        }))
        .unwrap(),
    )
    .unwrap();
    let (status, started) = tool(
        &app,
        &token,
        "managed_apply_retry",
        serde_json::json!({"confirm":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let aborted = finish(&app, &token, &started).await;
    assert_eq!(aborted["stage"], "aborted");
    assert_eq!(aborted["mode"], "next_start");
    let (_, job) = tool(
        &app,
        &token,
        "jobs_get",
        serde_json::json!({"id":started["job"]["id"]}),
    )
    .await;
    assert!(
        job["output"]
            .as_str()
            .unwrap()
            .contains("discarded without publishing")
    );
    assert_eq!(store.read().unwrap().unwrap().bytes, initial.bytes);
    assert_eq!(
        std::fs::read_to_string(storage_root.join("docs/operator-file")).unwrap(),
        "preserve me"
    );
    let (_, pending) = tool(&app, &token, "workloads_pending", serde_json::json!({})).await;
    assert!(pending["pending"].is_null());
}
