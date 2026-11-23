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
    linux_session_store::{SessionKey, SessionStore, LIFETIME_SECONDS, MAX_SESSIONS},
    tools::error,
};

const COOKIE: &str = "maestro_session";
#[derive(Clone)]
pub(super) struct Sessions {
    config: Arc<AppConfig>,
    active: Arc<Mutex<HashMap<SessionKey, u64>>>,
    store: Arc<SessionStore>,
}

impl Sessions {
    pub(super) fn new(config: Arc<AppConfig>) -> anyhow::Result<Self> {
        let (store, active) = SessionStore::open(&config.paths.data_dir, &config.server.auth_token)?;
        Ok(Self {
            config,
            active: Arc::new(Mutex::new(active)),
            store: Arc::new(store),
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

pub(super) async fn mode() -> Json<Value> {
    Json(json!({"adminConfigured": false}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Credentials {
    token: String,
}

pub(super) async fn login(
    Extension(sessions): Extension<Sessions>,
    headers: HeaderMap,
    credentials: Result<Json<Credentials>, JsonRejection>,
) -> Response {
    let Some(secure) = cookie_security(&headers) else {
        return error(
            StatusCode::FORBIDDEN,
            "forbidden_origin",
            "Sign in from the local or HTTPS browser interface.",
        );
    };
    let Ok(Json(credentials)) = credentials else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_arguments",
            "Expected a token.",
        );
    };
    if !token_matches(&credentials.token, &sessions.config.server.auth_token) {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Invalid credentials.",
        );
    }

    let Ok(token) = random_token() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "Could not issue a secure browser session.");
    };
    let now = unix_now();
    let mut active = sessions.active.lock();
    active.retain(|_, expiry| *expiry > now);
    if let Some(key) = cookie_key(&headers) {
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
    if sessions.store.save(&active).is_err() {
        active.remove(&key);
        return error(StatusCode::SERVICE_UNAVAILABLE, "unavailable",
            "Could not persist browser session securely.");
    }
    session_response("signed in", &token, LIFETIME_SECONDS, secure)
}

pub(super) async fn logout(
    Extension(sessions): Extension<Sessions>,
    headers: HeaderMap,
) -> Response {
    let Some(secure) = cookie_security(&headers) else {
        return error(
            StatusCode::FORBIDDEN,
            "forbidden_origin",
            "Sign out from the local or HTTPS browser interface.",
        );
    };
    if let Some(key) = cookie_key(&headers) {
        let mut active = sessions.active.lock();
        let previous = active.remove(&key);
        if sessions.store.save(&active).is_err() {
            if let Some(expiry) = previous { active.insert(key, expiry); }
            return error(StatusCode::SERVICE_UNAVAILABLE, "unavailable",
                "Could not persist session revocation.");
        }
    }
    session_response("signed out", "", 0, secure)
}

fn cookie_security(headers: &HeaderMap) -> Option<bool> {
    // The origin middleware has already matched this origin to the request authority.
    let origin = url::Url::parse(headers.get(header::ORIGIN)?.to_str().ok()?).ok()?;
    match (origin.scheme(), origin.host()?) {
        ("https", _) => Some(true),
        ("http", url::Host::Domain("localhost")) => Some(false),
        ("http", url::Host::Ipv4(ip)) if ip.is_loopback() => Some(false),
        ("http", url::Host::Ipv6(ip)) if ip.is_loopback() => Some(false),
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
    if tokens.next().is_some() || token.len() != 43 { return None; }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(token).ok()?;
    (bytes.len() == 32).then(|| key(token))
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_secs())
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
