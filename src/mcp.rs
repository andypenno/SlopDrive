use crate::{AppError, AppState, get_page_url, upload, versions};
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

pub async fn handle(State(state): State<AppState>, body: Bytes) -> Response {
    let request: Value = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => return rpc_error(Value::Null, -32700, "invalid JSON").into_response(),
    };
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str);
    if id
        .as_ref()
        .is_some_and(|id| !id.is_null() && !id.is_string() && !id.is_number())
    {
        return rpc_error(Value::Null, -32600, "invalid request ID").into_response();
    }
    if request
        .get("params")
        .is_some_and(|params| !params.is_object() && !params.is_array())
    {
        return rpc_error(id.unwrap_or(Value::Null), -32600, "invalid params").into_response();
    }
    if request.get("jsonrpc") != Some(&json!("2.0")) || method.is_none() {
        return rpc_error(id.unwrap_or(Value::Null), -32600, "invalid request").into_response();
    }
    if id.is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let id = id.unwrap();
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = match method.unwrap() {
        "initialize" => Ok(json!({
            "protocolVersion": "2025-03-26",
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "slopbox", "version": env!("CARGO_PKG_VERSION") }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => tool_call(&state, &params),
        _ => return rpc_error(id, -32601, "method not found").into_response(),
    };
    match result {
        Ok(result) => Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response(),
        Err(error) => rpc_error(id, -32602, &error).into_response(),
    }
}

fn rpc_error(id: Value, code: i32, message: &str) -> Json<Value> {
    Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))
}

fn tool_definitions() -> Value {
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
}

fn required<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, String> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing {name}"))
}

fn tool_call(state: &AppState, params: &Value) -> Result<Value, String> {
    let name = required(params, "name")?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
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
        "upload_html" => upload(state, None, required(&arguments, "content")?)
            .map(|value| serde_json::to_value(value).expect("serializable response")),
        "update_html" => upload(
            state,
            Some(required(&arguments, "upload_id")?),
            required(&arguments, "content")?,
        )
        .map(|value| serde_json::to_value(value).expect("serializable response")),
        "list_versions" => versions(state, required(&arguments, "upload_id")?)
            .map(|value| serde_json::to_value(value).expect("serializable response")),
        "get_html_url" => {
            let id = required(&arguments, "upload_id")?;
            let hash = arguments.get("version_hash").and_then(Value::as_str);
            get_page_url(state, id, hash).map(|url| json!({ "url": url }))
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

fn tool_error(error: AppError) -> Value {
    let message = match error {
        AppError::BadRequest(message) => message,
        AppError::NotFound => "not found".to_owned(),
        AppError::Unauthorized => "unauthorized".to_owned(),
        AppError::Internal(message) => {
            tracing::error!(error = %message, "MCP tool failed");
            "internal error".to_owned()
        }
    };
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_errors_return_client_messages_without_internal_details() {
        for (error, message) in [
            (
                AppError::BadRequest("invalid content".into()),
                "invalid content",
            ),
            (AppError::NotFound, "not found"),
            (AppError::Unauthorized, "unauthorized"),
            (
                AppError::Internal("private storage detail".into()),
                "internal error",
            ),
        ] {
            assert_eq!(
                tool_error(error),
                json!({"content":[{"type":"text","text":message}],"isError":true})
            );
        }
    }
}
