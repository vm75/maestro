use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{Extension, Request, State, rejection::JsonRejection},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    auth::{bearer_token, token_matches},
    config::AppConfig,
};

use super::tools::error;

const COOKIE: &str = "maestro_session";
const LIFETIME: Duration = Duration::from_secs(8 * 60 * 60);
const MAX_SESSIONS: usize = 128;
type SessionKey = [u8; 32];

#[derive(Clone)]
pub(super) struct Sessions {
    config: Arc<AppConfig>,
    active: Arc<Mutex<HashMap<SessionKey, Instant>>>,
}

impl Sessions {
    pub(super) fn new(config: Arc<AppConfig>) -> Self {
        Self {
            config,
            active: Arc::default(),
        }
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
            .is_some_and(|expiry| *expiry > Instant::now())
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

    let token = Uuid::new_v4().simple().to_string();
    let now = Instant::now();
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
    active.insert(key(&token), now + LIFETIME);
    session_response("signed in", &token, LIFETIME.as_secs(), secure)
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
        sessions.active.lock().remove(&key);
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
    (tokens.next().is_none() && token.len() == 32).then(|| key(token))
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
