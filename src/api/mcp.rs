//! Linux MCP transport adapter. Both HTTP and stdio dispatch into the same
//! validated Maestro management tools; no transport owns privileged commands.
use std::sync::Arc;

use axum::{
    Json,
    body::Bytes,
    extract::{Extension, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::tools;
use crate::{
    core::{
        declarative::{CompositeProvider, Store},
        jobs::Jobs,
        monitor::Monitor,
    },
    state::AppState,
};

pub const PROTOCOL_VERSION: &str = "2025-11-25";
pub const MAX_MESSAGE: usize = 1_064_960;

pub fn supports_protocol(version: &str) -> bool {
    matches!(version, "2025-11-25" | "2025-06-18" | "2025-03-26")
}

#[derive(Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    // Error replies have a JSON null ID if no valid ID was supplied.
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

fn response(id: Option<Value>, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        id: id.unwrap_or(Value::Null),
        result: Some(result),
        error: None,
    }
}

pub fn failure(id: Option<Value>, code: i32, message: &str) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        id: id.unwrap_or(Value::Null),
        result: None,
        error: Some(json!({"code": code, "message": message})),
    }
}

fn valid_id(id: &Value) -> bool {
    match id {
        Value::String(_) => true,
        Value::Number(number) => {
            number
                .as_i64()
                .is_some_and(|n| n.unsigned_abs() <= 9_007_199_254_740_991)
                || number.as_u64().is_some_and(|n| n <= 9_007_199_254_740_991)
        }
        _ => false,
    }
}

pub fn parse(bytes: &[u8]) -> Result<Option<JsonRpcRequest>, JsonRpcResponse> {
    if bytes.len() > MAX_MESSAGE {
        return Err(failure(None, -32600, "MCP message exceeds the size limit"));
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| failure(None, -32700, "Invalid JSON"))?;
    let Value::Object(object) = value else {
        return Err(failure(None, -32600, "Expected one JSON-RPC object"));
    };
    let request_id = object.get("id").cloned();
    if request_id.as_ref().is_some_and(|id| !valid_id(id)) {
        return Err(failure(
            None,
            -32600,
            "Request ID must be a string or safe integer",
        ));
    }
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(failure(request_id, -32600, "Invalid JSON-RPC version"));
    }
    if object.get("method").is_none() || object.get("method").and_then(Value::as_str) == Some("") {
        if request_id.is_some() && (object.contains_key("result") != object.contains_key("error")) {
            return Ok(None); // Responses from a client do not require a reply.
        }
        return Err(failure(request_id, -32600, "Request method is required"));
    }
    if object.contains_key("result") || object.contains_key("error") {
        return Err(failure(
            request_id,
            -32600,
            "Request cannot contain a result or error",
        ));
    }
    if object
        .get("params")
        .is_some_and(|params| !params.is_object())
    {
        if request_id.is_none() {
            return Ok(None);
        }
        return Err(failure(request_id, -32602, "Parameters must be an object"));
    }
    serde_json::from_value(Value::Object(object))
        .map(Some)
        .map_err(|_| failure(request_id, -32600, "Invalid JSON-RPC request"))
}

pub async fn dispatch(
    ctx: &tools::ToolContext<'_>,
    request: JsonRpcRequest,
) -> Option<JsonRpcResponse> {
    let id = request.id;
    if request.jsonrpc != "2.0" || id.as_ref().is_some_and(|id| !valid_id(id)) {
        return Some(failure(id, -32600, "Invalid JSON-RPC request"));
    }
    // Notifications are never replied to, even when unrecognized.
    id.as_ref()?;

    let result = match request.method.as_str() {
        "initialize" => {
            let Some(params) = request.params.as_object() else {
                return Some(failure(id, -32602, "Initialize requires an object"));
            };
            let Some(version) = params.get("protocolVersion").and_then(Value::as_str) else {
                return Some(failure(id, -32602, "Initialize requires protocolVersion"));
            };
            let client = params.get("clientInfo").and_then(Value::as_object);
            if client
                .and_then(|c| c.get("name"))
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
                || client
                    .and_then(|c| c.get("version"))
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                || !params.get("capabilities").is_some_and(Value::is_object)
            {
                return Some(failure(
                    id,
                    -32602,
                    "Initialize requires clientInfo and capabilities",
                ));
            }
            response(
                id,
                json!({
                    "protocolVersion": if supports_protocol(version) { version } else { PROTOCOL_VERSION },
                    "serverInfo": {
                        "name": "maestro",
                        "title": "Maestro",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": {"tools": {"listChanged": false}},
                    "instructions": "Use system_capabilities to inspect permissions. Writes run as tracked jobs; confirm destructive actions and poll jobs_get. The optional helper is never invoked for user-owned actions."
                }),
            )
        }
        "ping" => response(id, json!({})),
        "tools/list" => {
            if request
                .params
                .get("cursor")
                .is_some_and(|cursor| !cursor.is_null() && !cursor.is_string())
            {
                return Some(failure(id, -32602, "Invalid tool list parameters"));
            }
            if request
                .params
                .as_object()
                .and_then(|p| p.get("cursor"))
                .and_then(Value::as_str)
                .is_some_and(|cursor| !cursor.is_empty())
            {
                return Some(failure(id, -32602, "Unknown tool list cursor"));
            }
            let available = tools::discover_tools(ctx.state);
            let tools_mcp: Vec<Value> = available
                .into_iter()
                .map(|tool| {
                    json!({
                        "name": tool["name"],
                        "title": tool["title"],
                        "description": tool["description"],
                        "inputSchema": tool["inputSchema"],
                        "annotations": {
                            "readOnlyHint": tool["readOnly"],
                            "destructiveHint": tool["destructive"],
                            "idempotentHint": tool["readOnly"],
                            "openWorldHint": true
                        }
                    })
                })
                .collect();
            response(id, json!({"tools": tools_mcp}))
        }
        "tools/call" => {
            let Some(params) = request.params.as_object() else {
                return Some(failure(id, -32602, "Tool call requires an object"));
            };
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(failure(id, -32602, "Tool name is required"));
            };
            if params.get("task").is_some() {
                return Some(failure(
                    id,
                    -32602,
                    "Task-augmented calls are not supported",
                ));
            }
            if !tools::discover_tools(ctx.state)
                .iter()
                .any(|tool| tool["name"] == name)
            {
                return Some(failure(id, -32602, "Unknown tool"));
            }
            let arguments = match params.get("arguments") {
                Some(Value::Object(map)) => map.clone(),
                Some(Value::Null) | None => Map::new(),
                _ => return Some(failure(id, -32602, "Tool arguments must be an object")),
            };
            let (status, value) = tools::execute_tool(ctx, name, arguments).await;
            let data = if value.is_object() {
                value
            } else if value.is_array() {
                json!({"items": value})
            } else {
                json!({"value": value})
            };
            let serialized = serde_json::to_string(&data).unwrap_or_default();
            response(
                id,
                json!({
                    "content": [{"type":"text","text": serialized}],
                    "structuredContent": data,
                    "isError": !status.is_success()
                }),
            )
        }
        _ => return Some(failure(id, -32601, "Method not supported")),
    };
    Some(result)
}

fn accepts(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|header| {
            header.split(',').any(|part| {
                let media = part.split(';').next().unwrap_or_default().trim();
                let disabled = part.split(';').skip(1).any(|p| p.trim() == "q=0");
                !disabled && (media == expected || media == "*/*")
            })
        })
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_mcp(
    State(state): State<AppState>,
    Extension(started_at): Extension<DateTime<Utc>>,
    Extension(jobs): Extension<Jobs>,
    Extension(monitor): Extension<Monitor>,
    Extension(store): Extension<Store>,
    Extension(provider): Extension<Arc<CompositeProvider>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(version) = headers
        .get("mcp-protocol-version")
        .and_then(|v| v.to_str().ok())
    {
        if !supports_protocol(version) {
            return (
                StatusCode::BAD_REQUEST,
                Json(failure(None, -32600, "Unsupported MCP-Protocol-Version")),
            )
                .into_response();
        }
    }
    if !accepts(&headers, "application/json") || !accepts(&headers, "text/event-stream") {
        return (
            StatusCode::NOT_ACCEPTABLE,
            Json(failure(
                None,
                -32600,
                "Accept must include application/json and text/event-stream",
            )),
        )
            .into_response();
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if content_type.split(';').next().unwrap_or_default().trim() != "application/json" {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(failure(
                None,
                -32600,
                "Content-Type must be application/json",
            )),
        )
            .into_response();
    }
    let request = match parse(&body) {
        Ok(Some(request)) => request,
        Ok(None) => return StatusCode::ACCEPTED.into_response(),
        Err(problem) => return (StatusCode::BAD_REQUEST, Json(problem)).into_response(),
    };
    let ctx = tools::ToolContext {
        state: &state,
        started_at,
        jobs: &jobs,
        monitor: &monitor,
        store: &store,
        provider: &provider,
    };
    match dispatch(&ctx, request).await {
        Some(result) => Json(result).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_rpc_ids_and_bad_envelopes() {
        assert!(parse(br#"[]"#).is_err());
        assert!(parse(br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#).is_err());
        assert!(parse(br#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#).is_err());
        assert!(parse(br#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#).is_err());
        assert!(parse(br#"{"jsonrpc":"2.0","id":1,"method":"ping","params":[]}"#).is_err());
        assert!(
            parse(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap()
                .is_some()
        );
        assert!(
            parse(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn negotiates_pinned_reference_protocol_revisions() {
        for version in ["2025-11-25", "2025-06-18", "2025-03-26"] {
            assert!(supports_protocol(version));
        }
        assert!(!supports_protocol("2024-11-05"));
    }

    #[test]
    fn distinguishes_responses_and_malformed_notifications_like_reference() {
        for message in [
            br#"{"jsonrpc":"2.0","id":1,"method":""}"#.as_slice(),
            br#"{"jsonrpc":"2.0","result":{}}"#,
            br#"{"jsonrpc":"2.0","id":1,"result":{},"error":{}}"#,
        ] {
            assert!(parse(message).is_err());
        }
        for message in [
            br#"{"jsonrpc":"2.0","id":1,"result":{}}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601}}"#,
            br#"{"jsonrpc":"2.0","method":"notifications/initialized","params":[]}"#,
        ] {
            assert!(parse(message).unwrap().is_none());
        }
    }
}
