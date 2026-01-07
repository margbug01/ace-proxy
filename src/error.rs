use thiserror::Error;

#[derive(Error, Debug)]
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

// JSON-RPC error codes - Only export codes that are actually used
pub const ERROR_BACKEND_SPAWN_FAILED: i32 = -32001;
pub const ERROR_BACKEND_UNAVAILABLE: i32 = -32002;
pub const ERROR_INTERNAL_ERROR: i32 = -32603;
