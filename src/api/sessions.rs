use std::{
    collections::HashMap,
    io::Read,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json,
    extract::{Extension, Request, State, rejection::JsonRejection},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::Engine;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    auth::{bearer_token, token_matches},
    config::AppConfig,
};

use super::{
    linux_session_store::{LIFETIME_SECONDS, MAX_SESSIONS, SessionKey, SessionStore},
    tools::error,
};

const COOKIE: &str = "maestro_session";
#[derive(Clone)]
pub(super) struct Sessions {
    config: Arc<AppConfig>,
    active: Arc<Mutex<HashMap<SessionKey, u64>>>,
    store: Arc<Mutex<SessionStore>>,
    admin: Arc<Mutex<Option<(String, String)>>>,
    attempts: Arc<Mutex<HashMap<std::net::IpAddr, (u8, u64)>>>,
    verification: Arc<tokio::sync::Semaphore>,
}

impl Sessions {
    pub(super) fn browser_session(&self, headers: &HeaderMap) -> Option<SessionKey> {
        cookie_key(headers).filter(|key| self.session_active(key))
    }
    pub(super) fn session_active(&self, key: &SessionKey) -> bool {
        self.active
            .lock()
            .get(key)
            .is_some_and(|expiry| *expiry > unix_now())
    }
    pub(super) fn new(config: Arc<AppConfig>) -> anyhow::Result<Self> {
        let shared_admin = config.linux_authority.as_ref().map_or_else(
            || Arc::new(Mutex::new(None)),
            |authority| authority.credentials.clone(),
        );
        let admin = shared_admin.lock().clone();
        let scope = session_scope(&config.server.auth_token, admin.as_ref());
        let (store, active) = SessionStore::open(&config.paths.data_dir, &scope)?;
        Ok(Self {
            config,
            active: Arc::new(Mutex::new(active)),
            store: Arc::new(Mutex::new(store)),
            admin: shared_admin,
            attempts: Arc::default(),
            verification: Arc::new(tokio::sync::Semaphore::new(4)),
        })
    }
}

pub(super) async fn authorize(
    State(sessions): State<Sessions>,
    request: Request,
    next: Next,
) -> Response {
    let bearer_valid = bearer_token(&request)
        .is_some_and(|token| token_matches(&token, &sessions.config.server.auth_token));
    let session_valid = cookie_key(request.headers()).is_some_and(|key| {
        sessions
            .active
            .lock()
            .get(&key)
            .is_some_and(|expiry| *expiry > unix_now())
    });
    if session_valid
        && !bearer_valid
        && !matches!(*request.method(), Method::GET | Method::HEAD)
        && !request.headers().contains_key(header::ORIGIN)
    {
        return error(
            StatusCode::FORBIDDEN,
            "forbidden_origin",
            "Browser session mutations require an Origin header.",
        );
    }
    if bearer_valid || session_valid {
        return next.run(request).await;
    }
    error(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "Invalid credentials.",
    )
}

pub(super) async fn mode(Extension(sessions): Extension<Sessions>) -> Json<Value> {
    Json(json!({"adminConfigured": sessions.admin.lock().is_some(),
        "setupAvailable": sessions.config.linux_authority.is_some()}))
}

fn session_scope(token: &str, admin: Option<&(String, String)>) -> String {
    // Token-only sessions use the bearer token scope until administrator setup.
    admin.map_or_else(
        || token.to_owned(),
        |credentials| serde_json::to_string(&(token, credentials)).expect("serializable scope"),
    )
}

type Peer = Option<Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>;
fn peer_ip(peer: &Peer) -> std::net::IpAddr {
    peer.as_ref()
        .map_or(std::net::Ipv4Addr::UNSPECIFIED.into(), |Extension(peer)| {
            peer.0.ip()
        })
}

fn reserve_attempt(sessions: &Sessions, ip: std::net::IpAddr) -> bool {
    let now = unix_now();
    let mut attempts = sessions.attempts.lock();
    attempts.retain(|_, (_, expiry)| *expiry > now);
    if attempts.len() >= 128 && !attempts.contains_key(&ip) {
        return false;
    }
    let entry = attempts.entry(ip).or_insert((0, now + 300));
    if entry.0 >= 5 {
        return false;
    }
    entry.0 += 1;
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Credentials {
    #[serde(default)]
    token: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

pub(super) async fn login(
    Extension(sessions): Extension<Sessions>,
    peer: Peer,
    headers: HeaderMap,
    credentials: Result<Json<Credentials>, JsonRejection>,
) -> Response {
    let Some(secure) = cookie_security(&headers) else {
        return error(
            StatusCode::FORBIDDEN,
            "forbidden_origin",
            "Sign in from an allowed browser origin.",
        );
    };
    let Ok(Json(credentials)) = credentials else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_arguments",
            "Expected a token.",
        );
    };
    let ip = peer_ip(&peer);
    if !reserve_attempt(&sessions, ip) {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many login attempts; try again later.",
        );
    }
    let admin = sessions.admin.lock().clone();
    let valid = if let Some((username, hash)) = admin {
        let Ok(permit) = sessions.verification.clone().try_acquire_owned() else {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "Authentication is busy; retry shortly.",
            );
        };
        let supplied_username = credentials.username;
        let supplied_password = credentials.password;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crate::core::declarative::credentials::matches(&supplied_password, &hash)
                && token_matches(&supplied_username, &username)
        })
        .await
        .unwrap_or(false)
    } else {
        token_matches(&credentials.token, &sessions.config.server.auth_token)
    };
    if !valid {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Invalid credentials.",
        );
    }

    sessions.attempts.lock().remove(&ip);
    issue_session(&sessions, &headers, secure)
}

fn issue_session(sessions: &Sessions, headers: &HeaderMap, secure: bool) -> Response {
    let Ok(token) = random_token() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "Could not issue a secure browser session.",
        );
    };
    let now = unix_now();
    let mut active = sessions.active.lock();
    active.retain(|_, expiry| *expiry > now);
    if let Some(key) = cookie_key(headers) {
        active.remove(&key);
    }
    if active.len() >= MAX_SESSIONS {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_sessions",
            "Too many active browser sessions.",
        );
    }
    let key = key(&token);
    active.insert(key, now.saturating_add(LIFETIME_SECONDS));
    if sessions.store.lock().save(&active).is_err() {
        active.remove(&key);
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "Could not persist browser session securely.",
        );
    }
    session_response("signed in", &token, LIFETIME_SECONDS, secure)
}

pub(super) async fn setup(
    Extension(sessions): Extension<Sessions>,
    peer: Peer,
    headers: HeaderMap,
    credentials: Result<Json<Credentials>, JsonRejection>,
) -> Response {
    let Some(secure) = cookie_security(&headers) else {
        return error(
            StatusCode::FORBIDDEN,
            "forbidden_origin",
            "Setup requires an allowed browser origin.",
        );
    };
    let Ok(Json(credentials)) = credentials else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_arguments",
            "Expected bootstrap token, username and password.",
        );
    };
    let ip = peer_ip(&peer);
    if !reserve_attempt(&sessions, ip) {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many login attempts; try again later.",
        );
    }
    if !token_matches(&credentials.token, &sessions.config.server.auth_token) {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Invalid credentials.",
        );
    }
    if sessions.admin.lock().is_some() {
        return error(
            StatusCode::CONFLICT,
            "admin_configured",
            "Administrator already configured.",
        );
    }
    let Some(authority) = &sessions.config.linux_authority else {
        return error(
            StatusCode::BAD_REQUEST,
            "unavailable",
            "Administrator setup requires a unified YAML configuration.",
        );
    };
    if !crate::core::declarative::server::valid_username(&credentials.username)
        || !(12..=256).contains(&credentials.password.len())
    {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_arguments",
            "Use a valid administrator username and a password of 12 to 256 bytes.",
        );
    }
    let store = match crate::core::declarative::Store::at_path(&authority.path) {
        Ok(store) => store,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "Private configuration unavailable.",
            );
        }
    };
    let username = credentials.username;
    let password = credentials.password;
    let Ok(permit) = sessions.verification.clone().try_acquire_owned() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Authentication is busy; retry shortly.",
        );
    };
    let prepared = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let _permit = permit;
        let hash = crate::core::declarative::credentials::hash(&password)?;
        store.setup_admin(&username, &hash)?;
        Ok((username, hash))
    })
    .await;
    let Ok(Ok(admin)) = prepared else {
        return error(
            StatusCode::CONFLICT,
            "setup_conflict",
            "Could not complete setup; reload configuration and retry.",
        );
    };
    // Credentials are durable before replacing the bootstrap session scope.
    *sessions.admin.lock() = Some(admin.clone());
    let scope = session_scope(&sessions.config.server.auth_token, Some(&admin));
    let mut active = sessions.active.lock();
    active.clear();
    match SessionStore::open(&sessions.config.paths.data_dir, &scope) {
        Ok((store, _)) => *sessions.store.lock() = store,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "Administrator saved; could not initialize browser sessions.",
            );
        }
    }
    drop(active);
    sessions.attempts.lock().remove(&ip);
    issue_session(&sessions, &headers, secure)
}

pub(super) async fn logout(
    Extension(sessions): Extension<Sessions>,
    headers: HeaderMap,
) -> Response {
    let Some(secure) = cookie_security(&headers) else {
        return error(
            StatusCode::FORBIDDEN,
            "forbidden_origin",
            "Sign out from an allowed browser origin.",
        );
    };
    if let Some(key) = cookie_key(&headers) {
        let mut active = sessions.active.lock();
        let previous = active.remove(&key);
        if sessions.store.lock().save(&active).is_err() {
            if let Some(expiry) = previous {
                active.insert(key, expiry);
            }
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "Could not persist session revocation.",
            );
        }
    }
    session_response("signed out", "", 0, secure)
}

fn cookie_security(headers: &HeaderMap) -> Option<bool> {
    // The origin middleware has already matched this origin to the request authority
    // and required an explicit allowed origin for non-loopback hosts.
    let origin = url::Url::parse(headers.get(header::ORIGIN)?.to_str().ok()?).ok()?;
    match (origin.scheme(), origin.host()?) {
        ("https", _) => Some(true),
        ("http", _) => Some(false),
        _ => None,
    }
}

fn cookie_key(headers: &HeaderMap) -> Option<SessionKey> {
    let mut tokens = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .filter_map(|(name, value)| (name == COOKIE).then_some(value));
    let token = tokens.next()?;
    if tokens.next().is_some() || token.len() != 43 {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .ok()?;
    (bytes.len() == 32).then(|| key(token))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn random_token() -> std::io::Result<String> {
    let mut random = [0_u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random))
}

fn key(token: &str) -> SessionKey {
    Sha256::digest(token.as_bytes()).into()
}

fn session_response(status: &str, token: &str, max_age: u64, secure: bool) -> Response {
    let secure = if secure { "; Secure" } else { "" };
    (
        [(
            header::SET_COOKIE,
            format!(
                "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}"
            ),
        )],
        Json(json!({"status": status})),
    )
        .into_response()
}
