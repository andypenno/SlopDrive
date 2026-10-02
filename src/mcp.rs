use crate::helpers::{required, rpc_error, tool_error};
use crate::{AppError, AppState, get_page_url, upload, versions};
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

pub async fn handle(State(state): State<AppState>, body: Bytes) -> Response {
    let request: Value = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                rpc_error(Value::Null, -32700, "invalid JSON"),
            )
                .into_response();
        }
    };
    let batch = request.is_array();
    let messages = if let Some(messages) = request.as_array() {
        messages.as_slice()
    } else {
        std::slice::from_ref(&request)
    };
    if messages.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            rpc_error(Value::Null, -32600, "empty batch"),
        )
            .into_response();
    }
    if messages.iter().any(|message| {
        message.get("result").is_some()
            || message.get("error").is_some()
            || (batch && message.get("method").and_then(Value::as_str) == Some("initialize"))
            || (message.get("id").is_none()
                && (request_error(message).is_some()
                    || message.get("method").and_then(Value::as_str) == Some("initialize")))
    }) {
        return (
            StatusCode::BAD_REQUEST,
            rpc_error(Value::Null, -32600, "unsupported message"),
        )
            .into_response();
    }
    let mut responses = Vec::new();
    for message in messages {
        if let Some(error) = request_error(message) {
            responses.push(error);
        } else if message.get("id").is_some() {
            responses.push(dispatch(&state, message).await);
        }
    }
    if responses.is_empty() {
        StatusCode::ACCEPTED.into_response()
    } else if batch {
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("event: message\ndata: {}\n\n", Value::Array(responses)),
        )
            .into_response()
    } else {
        Json(responses.pop().unwrap()).into_response()
    }
}

fn request_error(request: &Value) -> Option<Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    if !id.is_null() && !id.is_string() && !id.is_number() {
        return Some(rpc_error(Value::Null, -32600, "invalid request ID").0);
    }
    if request
        .get("params")
        .is_some_and(|params| !params.is_object() && !params.is_array())
    {
        return Some(rpc_error(id, -32600, "invalid params").0);
    }
    if request.get("jsonrpc") != Some(&json!("2.0"))
        || request.get("method").and_then(Value::as_str).is_none()
    {
        return Some(rpc_error(id, -32600, "invalid request").0);
    }
    None
}

async fn dispatch(state: &AppState, request: &Value) -> Value {
    let id = request["id"].clone();
    let empty = json!({});
    let params = request.get("params").unwrap_or(&empty);
    let result = match request["method"].as_str().unwrap() {
        "initialize" => {
            if !params.get("protocolVersion").is_some_and(Value::is_string)
                || !params.get("capabilities").is_some_and(Value::is_object)
                || !params.get("clientInfo").is_some_and(Value::is_object)
                || !params["clientInfo"]
                    .get("name")
                    .is_some_and(Value::is_string)
                || !params["clientInfo"]
                    .get("version")
                    .is_some_and(Value::is_string)
            {
                return rpc_error(
                    id,
                    -32602,
                    "initialize requires protocolVersion, capabilities, and clientInfo",
                )
                .0;
            }
            Ok(json!({
            "protocolVersion": "2025-03-26",
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "slopbox", "version": env!("CARGO_PKG_VERSION") }
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => tool_call(state, params).await,
        _ => return rpc_error(id, -32601, "method not found").0,
    };
    match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => rpc_error(id, -32602, &error).0,
    }
}

fn tool_definitions() -> &'static Value {
    static DEFINITIONS: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
        json!([
            {
                "name": "upload_html",
                "description": "Publish a new self-contained HTML page.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "content": { "type": "string" } },
                    "required": ["content"],
                    "additionalProperties": false
                }
            },
            {
                "name": "update_html",
                "description": "Publish a new version of a known page.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "upload_id": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["upload_id", "content"],
                    "additionalProperties": false
                }
            },
            {
                "name": "list_versions",
                "description": "List versions of a known page.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "upload_id": { "type": "string" } },
                    "required": ["upload_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "get_html_url",
                "description": "Get the latest or a specific version URL of a known page.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "upload_id": { "type": "string" },
                        "version_hash": { "type": "string" }
                    },
                    "required": ["upload_id"],
                    "additionalProperties": false
                }
            }
        ])
    });
    &DEFINITIONS
}

async fn tool_call(state: &AppState, params: &Value) -> Result<Value, String> {
    let name = required(params, "name")?;
    let empty = json!({});
    let arguments = params.get("arguments").unwrap_or(&empty);
    let definitions = tool_definitions();
    let definition = definitions
        .as_array()
        .expect("tool definitions array")
        .iter()
        .find(|tool| tool["name"] == name)
        .ok_or("unknown tool")?;
    let fields = arguments.as_object().ok_or("arguments must be an object")?;
    let properties = &definition["inputSchema"]["properties"];
    for (field, value) in fields {
        if properties.get(field).is_none() {
            return Err(format!("unknown argument {field}"));
        }
        if !value.is_string() {
            return Err(format!("{field} must be a string"));
        }
    }
    let result: Result<Value, AppError> = match name {
        "upload_html" => upload(state, None, required(arguments, "content")?)
            .await
            .map(|value| serde_json::to_value(value).expect("serializable response")),
        "update_html" => upload(
            state,
            Some(required(arguments, "upload_id")?),
            required(arguments, "content")?,
        )
        .await
        .map(|value| serde_json::to_value(value).expect("serializable response")),
        "list_versions" => versions(state, required(arguments, "upload_id")?)
            .await
            .map(|value| serde_json::to_value(value).expect("serializable response")),
        "get_html_url" => {
            let id = required(arguments, "upload_id")?;
            let hash = arguments.get("version_hash").and_then(Value::as_str);
            get_page_url(state, id, hash)
                .await
                .map(|url| json!({ "url": url }))
        }
        _ => return Err("unknown tool".into()),
    };
    match result {
        Ok(value) => Ok(json!({
            "content": [{ "type": "text", "text": value.to_string() }],
            "structuredContent": value,
            "isError": false
        })),
        Err(error) => Ok(tool_error(error)),
    }
}
