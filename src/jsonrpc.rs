use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 Request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<JsonRpcId>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 Response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<JsonRpcId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC ID can be string, number, or null
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum JsonRpcId {
    Number(i64),
    String(String),
}

impl JsonRpcId {
    #[allow(dead_code)]
    pub fn as_string(&self) -> String {
        match self {
            JsonRpcId::Number(n) => n.to_string(),
            JsonRpcId::String(s) => s.clone(),
        }
    }
}

/// JSON-RPC Error object
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    #[allow(dead_code)]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

impl JsonRpcResponse {
    pub fn success(id: Option<JsonRpcId>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<JsonRpcId>, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

impl JsonRpcRequest {
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// Check if this is an initialize request
    pub fn is_initialize(&self) -> bool {
        self.method == "initialize"
    }

    /// Check if this is a shutdown request
    pub fn is_shutdown(&self) -> bool {
        self.method == "shutdown"
    }

    /// Check if this is an exit notification
    pub fn is_exit(&self) -> bool {
        self.method == "exit"
    }

    /// Try to extract workspace roots from initialize params
    pub fn get_roots(&self) -> Option<Vec<String>> {
        let params = self.params.as_ref()?;
        let roots = params.get("roots")?;
        let arr = roots.as_array()?;
        
        arr.iter()
            .filter_map(|v| {
                v.get("uri")
                    .and_then(|u| u.as_str())
                    .map(|s| s.to_string())
            })
            .collect::<Vec<_>>()
            .into()
    }

    /// Try to extract a URI from the request params (for routing)
    pub fn get_uri(&self) -> Option<String> {
        let params = self.params.as_ref()?;
        
        // Try common param structures
        if let Some(uri) = params.get("uri").and_then(|v| v.as_str()) {
            return Some(uri.to_string());
        }
        if let Some(uri) = params.get("textDocument").and_then(|td| td.get("uri")).and_then(|v| v.as_str()) {
            return Some(uri.to_string());
        }
        if params.get("information_request").and_then(|v| v.as_str()).is_some() {
            // For codebase-retrieval, the query itself might contain path hints
            return None;
        }
        
        None
    }
}

/// Generic JSON-RPC message (can be request, response, or notification)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Response(JsonRpcResponse),
}

impl JsonRpcMessage {
    #[allow(dead_code)]
    pub fn parse(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }

    #[allow(dead_code)]
    pub fn to_json_string(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_request() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "initialize");
        assert_eq!(req.id, Some(JsonRpcId::Number(1)));
        assert!(req.is_initialize());
    }

    #[test]
    fn test_parse_notification() {
        let json = r#"{"jsonrpc":"2.0","method":"exit"}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert!(req.is_notification());
        assert!(req.is_exit());
    }

    #[test]
    fn test_string_id() {
        let json = r#"{"jsonrpc":"2.0","id":"abc-123","method":"test"}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.id, Some(JsonRpcId::String("abc-123".to_string())));
    }
    
    #[test]
    fn test_response_success() {
        let response = JsonRpcResponse::success(
            Some(JsonRpcId::Number(1)),
            serde_json::json!({"result": "ok"})
        );
        assert_eq!(response.jsonrpc, "2.0");
        assert!(response.result.is_some());
        assert!(response.error.is_none());
    }
    
    #[test]
    fn test_response_error() {
        let response = JsonRpcResponse::error(
            Some(JsonRpcId::Number(1)),
            JsonRpcError::new(-32600, "Invalid Request")
        );
        assert!(response.result.is_none());
        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, -32600);
    }
    
    #[test]
    fn test_get_uri_from_params() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"test","params":{"uri":"file:///test.rs"}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.get_uri(), Some("file:///test.rs".to_string()));
    }
    
    #[test]
    fn test_get_uri_from_text_document() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"test","params":{"textDocument":{"uri":"file:///doc.rs"}}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.get_uri(), Some("file:///doc.rs".to_string()));
    }
    
    #[test]
    fn test_get_roots() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"roots":[{"uri":"file:///project1"},{"uri":"file:///project2"}]}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        let roots = req.get_roots().unwrap();
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0], "file:///project1");
        assert_eq!(roots[1], "file:///project2");
    }
    
    #[test]
    fn test_is_shutdown() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"shutdown"}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert!(req.is_shutdown());
        assert!(!req.is_initialize());
        assert!(!req.is_exit());
    }
    
    #[test]
    fn test_json_rpc_id_as_string() {
        let num_id = JsonRpcId::Number(42);
        assert_eq!(num_id.as_string(), "42");
        
        let str_id = JsonRpcId::String("test-id".to_string());
        assert_eq!(str_id.as_string(), "test-id");
    }
    
    #[test]
    fn test_error_with_data() {
        let error = JsonRpcError::new(-32000, "Custom error")
            .with_data(serde_json::json!({"details": "more info"}));
        assert!(error.data.is_some());
    }
    
    #[test]
    fn test_response_serialization() {
        let response = JsonRpcResponse::success(
            Some(JsonRpcId::Number(1)),
            serde_json::json!(null)
        );
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"id\":1"));
    }
}
