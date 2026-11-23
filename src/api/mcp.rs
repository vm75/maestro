use std::sync::Arc;

use axum::{
    Json,
    extract::{Extension, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    core::{
        declarative::{CompositeProvider, Store},
        jobs::Jobs,
        monitor::Monitor,
    },
    state::AppState,
};

use super::tools;

#[derive(Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_mcp(
    State(state): State<AppState>,
    Extension(started_at): Extension<DateTime<Utc>>,
    Extension(jobs): Extension<Jobs>,
    Extension(monitor): Extension<Monitor>,
    Extension(store): Extension<Store>,
    Extension(provider): Extension<Arc<CompositeProvider>>,
    Json(request): Json<JsonRpcRequest>,
) -> Response {
    if request.jsonrpc != "2.0" {
        return Json(JsonRpcResponse {
            jsonrpc: "2.0",
            id: request.id,
            result: None,
            error: Some(json!({
                "code": -32600,
                "message": "Invalid Request: jsonrpc must be '2.0'"
            })),
        })
        .into_response();
    }

    match request.method.as_str() {
        "initialize" => Json(JsonRpcResponse {
            jsonrpc: "2.0",
            id: request.id,
            result: Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": { "listChanged": false }
                },
                "serverInfo": {
                    "name": "maestro",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })),
            error: None,
        })
        .into_response(),

        "notifications/initialized" => StatusCode::NO_CONTENT.into_response(),

        "tools/list" => {
            let tools_raw = tools::discover_tools(&state);
            let tools_mcp: Vec<_> = tools_raw
                .into_iter()
                .map(|t| {
                    json!({
                        "name": t.get("name").cloned().unwrap_or(Value::Null),
                        "description": t.get("description").cloned().unwrap_or(Value::Null),
                        "inputSchema": t.get("inputSchema").cloned().unwrap_or_else(|| json!({"type": "object"})),
                        "annotations": {
                            "readOnlyHint": t.get("readOnly").cloned().unwrap_or(Value::Bool(false)),
                            "destructiveHint": t.get("destructive").cloned().unwrap_or(Value::Bool(true))
                        },
                    })
                })
                .collect();
            Json(JsonRpcResponse {
                jsonrpc: "2.0",
                id: request.id,
                result: Some(json!({ "tools": tools_mcp })),
                error: None,
            })
            .into_response()
        }

        "tools/call" => {
            let params = match request.params {
                Value::Object(map) => map,
                _ => {
                    return Json(JsonRpcResponse {
                        jsonrpc: "2.0",
                        id: request.id,
                        result: None,
                        error: Some(json!({
                            "code": -32602,
                            "message": "Invalid params: expected object"
                        })),
                    })
                    .into_response();
                }
            };

            let tool_name = match params.get("name").and_then(Value::as_str) {
                Some(name) => name,
                None => {
                    return Json(JsonRpcResponse {
                        jsonrpc: "2.0",
                        id: request.id,
                        result: None,
                        error: Some(json!({
                            "code": -32602,
                            "message": "Missing tool name in params"
                        })),
                    })
                    .into_response();
                }
            };

            let arguments = match params.get("arguments") {
                Some(Value::Object(map)) => map.clone(),
                Some(Value::Null) | None => Map::new(),
                _ => {
                    return Json(JsonRpcResponse {
                        jsonrpc: "2.0",
                        id: request.id,
                        result: None,
                        error: Some(json!({
                            "code": -32602,
                            "message": "Arguments must be a JSON object"
                        })),
                    })
                    .into_response();
                }
            };

            let ctx = tools::ToolContext {
                state: &state,
                started_at,
                jobs: &jobs,
                monitor: &monitor,
                store: &store,
                provider: &provider,
            };
            let (status, value) = tools::execute_tool(&ctx, tool_name, arguments).await;

            let is_error = !status.is_success();
            let text_content = if is_error {
                value
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Tool execution failed")
                    .to_string()
            } else {
                serde_json::to_string_pretty(&value).unwrap_or_default()
            };

            Json(JsonRpcResponse {
                jsonrpc: "2.0",
                id: request.id,
                result: Some(json!({
                    "content": [
                        {
                            "type": "text",
                            "text": text_content
                        }
                    ],
                    "isError": is_error
                })),
                error: None,
            })
            .into_response()
        }

        _ => Json(JsonRpcResponse {
            jsonrpc: "2.0",
            id: request.id,
            result: None,
            error: Some(json!({
                "code": -32601,
                "message": format!("Method '{}' not found", request.method)
            })),
        })
        .into_response(),
    }
}
