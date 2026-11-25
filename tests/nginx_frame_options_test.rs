//! Exercise the Rust-rendered framing policy against a real, isolated nginx.
use axum::{
    Router,
    http::{HeaderMap, HeaderValue, StatusCode, Uri},
};
use maestro::{
    models::NginxObjectKind,
    nginx_render::{GlobalSettings, render_site},
};
use serde_json::json;
use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Nginx(Child);

impl Drop for Nginx {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn backend(uri: Uri) -> (StatusCode, HeaderMap, &'static str) {
    let mut headers = HeaderMap::new();
    if uri.path() == "/protected" {
        headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    }
    let status = if uri.path().ends_with("/missing") {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::OK
    };
    (status, headers, "backend")
}

#[tokio::test]
async fn effective_frame_options_are_enforced_on_http_responses() {
    let Some(nginx) = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("nginx"))
        .find(|path| path.is_file())
    else {
        return;
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(backend))
            .await
            .unwrap();
    });

    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join("log/nginx")).unwrap();
    for name in ["client-body", "proxy", "fastcgi", "uwsgi", "scgi"] {
        fs::create_dir_all(root.join(name)).unwrap();
    }
    let global = GlobalSettings {
        shared: json!({"var_dir": root}).as_object().unwrap().clone(),
        ..Default::default()
    };
    let upstream_policy = json!({
        "enable_lanonly": false,
        "proxy_transport": "http",
        "x_frame_options": "upstream",
        "routes": [
            {"path": "/denied", "x_frame_options": "deny", "children": [
                {"path": "/denied/connector", "x_frame_options": "upstream"}
            ]},
            {"path": "/spa", "spa_fallback": "/index.html"},
            {"path": "/cors", "cache_policy": "no_cache", "cors_origin": "*"}
        ]
    });
    let default_policy = json!({"enable_lanonly": false, "proxy_transport": "http"});
    let app = render_site(
        &global,
        upstream_policy.as_object().unwrap(),
        NginxObjectKind::Subfolder,
        "example.test",
        "app",
        &[],
    )
    .unwrap();
    let normal = render_site(
        &global,
        default_policy.as_object().unwrap(),
        NginxObjectKind::Subfolder,
        "example.test",
        "normal",
        &[],
    )
    .unwrap();
    let legacy = include_str!("../config/nginx/snippets/security-headers.conf");
    let config = format!(
        r#"daemon off;
master_process off;
pid nginx.pid;
error_log error.log;
events {{ worker_connections 64; }}
http {{
    client_body_temp_path client-body;
    proxy_temp_path proxy;
    fastcgi_temp_path fastcgi;
    uwsgi_temp_path uwsgi;
    scgi_temp_path scgi;
    map $uri $subfolder_upstream {{ default http://{upstream}; }}
    map $host $log_normal_access {{ default 0; }}
    map $host $log_traps_only {{ default 0; }}
    map $host $any_trap {{ default 0; }}
    log_format f2b_access '$remote_addr';
    log_format f2b_trap '$remote_addr';
    limit_req_zone $binary_remote_addr zone=global_rate:1m rate=30r/s;
    limit_conn_zone $binary_remote_addr zone=per_client_conn:1m;
    server {{
        listen 127.0.0.1:{port};
        server_name example.test;
        {legacy}
        location / {{ return 404; }}
        {app}
        {normal}
    }}
}}
"#
    );
    fs::write(root.join("nginx.conf"), config).unwrap();
    let log: PathBuf = root.join("startup.log");
    let mut nginx = Nginx(
        Command::new(nginx)
            .args(["-p", root.to_str().unwrap(), "-c", "nginx.conf", "-e"])
            .arg(&log)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut ready = false;
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            ready = true;
            break;
        }
        assert!(
            nginx.0.try_wait().unwrap().is_none(),
            "nginx exited: {}",
            fs::read_to_string(&log).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ready, "nginx failed to start");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    for (path, status, expected) in [
        ("/", 404, Some("SAMEORIGIN")),
        ("/normal/connector", 200, Some("SAMEORIGIN")),
        ("/app/connector", 200, None),
        ("/app/protected", 200, Some("DENY")),
        ("/app/denied", 200, Some("DENY")),
        ("/app/denied/connector", 200, None),
        ("/app/spa/missing", 200, None),
        ("/app/cors", 200, None),
        ("/app/missing", 404, None),
    ] {
        let response = client
            .get(format!("http://127.0.0.1:{port}{path}"))
            .header("Host", "example.test")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status, "{path}");
        let headers = response.headers();
        let framing: Vec<_> = headers
            .get_all("x-frame-options")
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(framing, expected.into_iter().collect::<Vec<_>>(), "{path}");
        assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
        if path == "/app/cors" {
            assert_eq!(headers.get("access-control-allow-origin").unwrap(), "*");
            assert!(
                headers
                    .get("cache-control")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains("no-store")
            );
        }
    }
    server.abort();
}
