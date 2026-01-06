use thiserror::Error;

#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum ProxyError {
    #[error("Backend spawn failed: {0}")]
    BackendSpawnFailed(String),

    #[error("Backend unavailable: {0}")]
    BackendUnavailable(String),

    #[error("Backend timeout: {0}")]
    BackendTimeout(String),

    #[error("Routing failed: {0}")]
    RoutingFailed(String),

    #[error("JSON-RPC parse error: {0}")]
    JsonRpcParseError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Job object error: {0}")]
    JobObjectError(String),
}

// JSON-RPC error codes as defined in plan.md
#[allow(dead_code)]
pub const ERROR_BACKEND_SPAWN_FAILED: i32 = -32001;
pub const ERROR_BACKEND_UNAVAILABLE: i32 = -32002;
#[allow(dead_code)]
pub const ERROR_BACKEND_TIMEOUT: i32 = -32003;
#[allow(dead_code)]
pub const ERROR_ROUTING_FAILED: i32 = -32004;

// Standard JSON-RPC error codes
#[allow(dead_code)]
pub const ERROR_PARSE_ERROR: i32 = -32700;
#[allow(dead_code)]
pub const ERROR_INVALID_REQUEST: i32 = -32600;
#[allow(dead_code)]
pub const ERROR_METHOD_NOT_FOUND: i32 = -32601;
#[allow(dead_code)]
pub const ERROR_INVALID_PARAMS: i32 = -32602;
pub const ERROR_INTERNAL_ERROR: i32 = -32603;
