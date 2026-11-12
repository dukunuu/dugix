//! The `tools/run` JSON-RPC envelope served at `/v1/tools/run`.

use serde::Serialize;

#[derive(Debug)]
pub(super) struct ToolRpcRequest {
    pub(super) id: serde_json::Value,
    pub(super) name: String,
    pub(super) args: serde_json::Value,
}

#[derive(Debug)]
pub(super) struct ToolRpcRequestError {
    pub(super) id: serde_json::Value,
    pub(super) error: ToolRpcError,
}

impl ToolRpcRequestError {
    fn new(id: serde_json::Value, error: ToolRpcError) -> Self {
        Self { id, error }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct ToolRunResult {
    pub(super) result: String,
    pub(super) label: String,
}

#[derive(Debug, Serialize)]
pub(super) struct ToolRpcError {
    pub(super) code: i32,
    message: String,
}

impl ToolRpcError {
    fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: message.into(),
        }
    }

    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
        }
    }

    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }

    fn parse_error() -> Self {
        Self {
            code: -32700,
            message: "invalid JSON".to_string(),
        }
    }

    pub(super) fn internal(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct ToolRpcResponse {
    jsonrpc: &'static str,
    id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<ToolRunResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ToolRpcError>,
}

impl ToolRpcResponse {
    pub(super) fn success(id: serde_json::Value, result: ToolRunResult) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub(super) fn error(id: serde_json::Value, error: ToolRpcError) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(error),
        }
    }
}

pub(super) fn parse_tool_rpc_request(body: &[u8]) -> Result<ToolRpcRequest, ToolRpcRequestError> {
    let value = serde_json::from_slice::<serde_json::Value>(body).map_err(|_| {
        ToolRpcRequestError::new(serde_json::Value::Null, ToolRpcError::parse_error())
    })?;
    let object = value.as_object().ok_or_else(|| {
        ToolRpcRequestError::new(
            serde_json::Value::Null,
            ToolRpcError::invalid_request("request must be a JSON object"),
        )
    })?;
    let id = object.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let fail = |error| ToolRpcRequestError::new(id.clone(), error);
    if object.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return Err(fail(ToolRpcError::invalid_request(
            "jsonrpc must be the string \"2.0\"",
        )));
    }
    let method = object
        .get("method")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| fail(ToolRpcError::invalid_request("method must be a string")))?;
    if method != "tools/run" {
        return Err(fail(ToolRpcError::method_not_found(method)));
    }
    let params = object
        .get("params")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| fail(ToolRpcError::invalid_params("params must be an object")))?;
    let name = params
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| {
            fail(ToolRpcError::invalid_params(
                "params.name must be a non-empty string",
            ))
        })?
        .to_string();
    let args = params
        .get("arguments")
        .or_else(|| params.get("args"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    Ok(ToolRpcRequest { id, name, args })
}
