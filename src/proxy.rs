//! MCP Proxy - main proxy logic coordinating stdio, routing, and backends

use crate::backend::BackendInstance;
use crate::config::Config;
use crate::error::{ProxyError, ERROR_BACKEND_SPAWN_FAILED, ERROR_BACKEND_UNAVAILABLE, ERROR_INTERNAL_ERROR};
use crate::git_filter::{self, GitTrackedFiles};
use crate::jsonrpc::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use crate::throttle::EventThrottler;
use lru::LruCache;
use percent_encoding::percent_decode_str;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

#[cfg(windows)]
use crate::job_object::JobObject;

#[cfg(unix)]
use crate::process_group::ProcessGroup;

/// MCP Proxy managing communication between IDE and backend(s)
pub struct McpProxy {
    config: Config,
    /// Known workspace roots from IDE
    roots: Vec<PathBuf>,
    /// Backend instances managed by LRU cache (automatically evicts least recently used)
    backends: LruCache<PathBuf, BackendInstance>,
    /// Default/fallback root when routing fails
    default_root: Option<PathBuf>,
    /// Windows Job Object for process cleanup (Arc for sharing with backends)
    #[cfg(windows)]
    job_object: Option<Arc<JobObject>>,
    /// Unix ProcessGroup for process cleanup (Arc for sharing with backends)
    #[cfg(unix)]
    process_group: Option<Arc<ProcessGroup>>,
    /// Server capabilities to report
    server_capabilities: serde_json::Value,
    /// Whether we're shutting down
    shutting_down: bool,
    /// Optional global inflight limiter
    global_inflight: Option<Arc<Semaphore>>,
    /// Event throttler for file change notifications
    event_throttler: Option<EventThrottler>,
    /// Git tracked files cache per root
    git_tracked_cache: HashMap<PathBuf, GitTrackedFiles>,
    /// Git cache timestamps for TTL
    git_cache_timestamps: HashMap<PathBuf, Instant>,
    /// Metrics: total requests processed
    metrics_total_requests: u64,
    /// Metrics: total errors
    metrics_total_errors: u64,
    /// Metrics: start time for uptime calculation
    metrics_start_time: Instant,
}

impl McpProxy {
    pub fn new(config: Config) -> Result<Self, ProxyError> {
        let config = config.with_auto_detect();
        
        // Create Job Object on Windows
        #[cfg(windows)]
        let job_object = match JobObject::new() {
            Ok(job) => Some(Arc::new(job)),
            Err(e) => {
                warn!("Failed to create Job Object: {}. Process cleanup may not work correctly.", e);
                None
            }
        };

        // Create ProcessGroup on Unix
        #[cfg(unix)]
        let process_group = match ProcessGroup::new() {
            Ok(pg) => Some(Arc::new(pg)),
            Err(e) => {
                warn!("Failed to create ProcessGroup: {}. Process cleanup may not work correctly.", e);
                None
            }
        };

        let default_root = config.default_root.clone();

        let global_inflight = if config.max_inflight_global > 0 {
            Some(Arc::new(Semaphore::new(config.max_inflight_global)))
        } else {
            None
        };

        let server_capabilities = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {
                    "listChanged": false
                }
            },
            "serverInfo": {
                "name": "mcp-proxy",
                "version": env!("CARGO_PKG_VERSION")
            }
        });

        let event_throttler = if config.debounce_ms > 0 {
            info!("Event throttler enabled with {}ms debounce window", config.debounce_ms);
            Some(EventThrottler::new(config.debounce_ms))
        } else {
            None
        };

        // Create LRU cache for backends with configured max capacity
        let backends_capacity = NonZeroUsize::new(config.max_backends.max(1))
            .unwrap_or(NonZeroUsize::new(3).unwrap());
        info!("Backend LRU cache initialized with capacity: {}", backends_capacity);

        Ok(Self {
            config,
            roots: Vec::new(),
            backends: LruCache::new(backends_capacity),
            default_root,
            #[cfg(windows)]
            job_object,
            #[cfg(unix)]
            process_group,
            server_capabilities,
            shutting_down: false,
            global_inflight,
            event_throttler,
            git_tracked_cache: HashMap::new(),
            git_cache_timestamps: HashMap::new(),
            metrics_total_requests: 0,
            metrics_total_errors: 0,
            metrics_start_time: Instant::now(),
        })
    }

    /// Main run loop - read from stdin, process, write to stdout
    pub async fn run(&mut self) -> Result<(), ProxyError> {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        
        let mut reader = BufReader::new(stdin);
        let mut writer = stdout;
        let mut msg = String::new();

        info!("MCP Proxy started, waiting for requests on stdin");

        let idle_ttl = Duration::from_secs(self.config.idle_ttl_seconds);
        let cleanup_interval = Duration::from_secs(60);
        let mut cleanup_tick = tokio::time::interval(cleanup_interval);
        cleanup_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        cleanup_tick.tick().await;

        let throttle_interval = Duration::from_millis(self.config.debounce_ms.max(100));
        let mut throttle_tick = tokio::time::interval(throttle_interval);
        throttle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        throttle_tick.tick().await;
        
        loop {
            msg.clear();
            
            tokio::select! {
                result = Self::read_next_message(&mut reader, &mut msg) => {
                    match result {
                        Ok(None) => {
                            info!("Stdin closed (EOF), shutting down");
                            break;
                        }
                        Ok(Some(())) => {
                            let trimmed = msg.trim();
                            if trimmed.is_empty() {
                                continue;
                            }

                            debug!("Received from IDE: {}", trimmed);

                            match self.handle_message(trimmed).await {
                                Ok(Some(response)) => {
                                    let response_json = serde_json::to_string(&response)?;
                                    debug!("Sending to IDE: {}", response_json);
                                    writer.write_all(response_json.as_bytes()).await?;
                                    writer.write_all(b"\n").await?;
                                    writer.flush().await?;
                                }
                                Ok(None) => {
                                    // Notification - no response needed
                                }
                                Err(e) => {
                                    error!("Error handling message: {}", e);
                                }
                            }

                            if self.shutting_down {
                                info!("Exit requested, shutting down");
                                break;
                            }
                        }
                        Err(e) => {
                            error!("Error reading stdin: {}", e);
                            break;
                        }
                    }
                }

                _ = cleanup_tick.tick() => {
                    self.cleanup_idle_backends(idle_ttl).await;
                }

                _ = throttle_tick.tick() => {
                    self.flush_throttled_events().await;
                }
            }
        }

        // Cleanup all backends on exit
        self.shutdown_all_backends().await;
        
        info!("MCP Proxy exiting");
        Ok(())
    }

    /// Handle a single JSON-RPC message
    async fn handle_message(&mut self, message: &str) -> Result<Option<JsonRpcResponse>, ProxyError> {
        // Strip BOM and other invisible characters
        let message = message.trim_start_matches('\u{feff}').trim();
        
        debug!("Parsing message (len={}): first 100 chars = {:?}", 
               message.len(), 
               &message.chars().take(100).collect::<String>());
        
        let request: JsonRpcRequest = match serde_json::from_str(message) {
            Ok(req) => req,
            Err(e) => {
                warn!("Failed to parse JSON-RPC request: {} | Raw bytes: {:?}", e, message.as_bytes().iter().take(50).collect::<Vec<_>>());
                return Ok(Some(JsonRpcResponse::error(
                    None,
                    JsonRpcError::new(-32700, format!("Parse error: {}", e)),
                )));
            }
        };

        info!("Handling request: {} (id: {:?})", request.method, request.id);
        
        // Record metrics
        self.record_request();

        // Handle protocol-level messages
        if request.is_initialize() {
            return Ok(Some(self.handle_initialize(&request).await?));
        }
        
        if request.is_shutdown() {
            return Ok(Some(self.handle_shutdown(&request).await?));
        }
        
        if request.is_exit() {
            self.shutting_down = true;
            return Ok(None);
        }

        // Handle roots/workspace changed notifications
        if request.method == "notifications/roots/listChanged" {
            self.handle_roots_changed(&request).await;
            return Ok(None);
        }

        // JSON-RPC notifications must not receive a response
        if request.is_notification() {
            // Check if this is a file change notification that should be throttled
            if self.should_throttle_notification(&request) {
                if let Some(uri) = request.get_uri() {
                    if let Some(path) = Self::uri_to_path(&uri) {
                        // Apply git filter if enabled
                        if self.config.git_filter {
                            if !self.is_path_git_tracked(&path).await {
                                debug!("Ignoring non-git-tracked file: {}", path.display());
                                return Ok(None);
                            }
                        }
                        
                        if let Some(throttler) = self.event_throttler.as_mut() {
                            throttler.add_path(path);
                            debug!("File change throttled, pending: {}", throttler.pending_count());
                            return Ok(None);
                        }
                    }
                }
            }
            // Forward non-throttled notifications directly
            if let Err(e) = self.forward_notification_to_backend(request).await {
                warn!("Failed to forward notification: {}", e);
                self.record_error();
            }
            return Ok(None);
        }

        // Route to backend
        let response = match self.route_to_backend(request).await {
            Ok(resp) => resp,
            Err(e) => {
                self.record_error();
                return Err(e);
            }
        };
        Ok(Some(response))
    }

    /// Handle initialize request
    async fn handle_initialize(&mut self, request: &JsonRpcRequest) -> Result<JsonRpcResponse, ProxyError> {
        info!("Handling initialize request");
        
        // Extract roots if provided
        if let Some(roots) = request.get_roots() {
            info!("Received roots: {:?}", roots);
            self.roots = roots
                .into_iter()
                .filter_map(|uri| Self::uri_to_path(&uri))
                .collect();
            
            // Set default root to first root if not configured
            if self.default_root.is_none() && !self.roots.is_empty() {
                self.default_root = Some(self.roots[0].clone());
            }
        }

        // Optionally pre-spawn backend for default root during initialize
        if self.config.prewarm_default_root {
            if let Some(ref root) = self.default_root.clone() {
                if !self.backends.contains(root) {
                    info!("Pre-spawning backend for default root: {}", root.display());
                    match self.get_or_create_backend(root.clone()).await {
                        Ok(_) => info!("Backend ready for default root"),
                        Err(e) => warn!("Failed to pre-spawn backend: {}", e),
                    }
                }
            }
        }

        Ok(JsonRpcResponse::success(
            request.id.clone(),
            self.server_capabilities.clone(),
        ))
    }

    /// Handle shutdown request
    async fn handle_shutdown(&mut self, request: &JsonRpcRequest) -> Result<JsonRpcResponse, ProxyError> {
        info!("Handling shutdown request");
        self.shutting_down = true;
        
        // Gracefully shutdown all backends
        self.shutdown_all_backends().await;
        
        Ok(JsonRpcResponse::success(request.id.clone(), serde_json::Value::Null))
    }

    /// Handle roots changed notification
    async fn handle_roots_changed(&mut self, request: &JsonRpcRequest) {
        if let Some(roots) = request.get_roots() {
            info!("Roots changed: {:?}", roots);
            self.roots = roots
                .into_iter()
                .filter_map(|uri| Self::uri_to_path(&uri))
                .collect();
        }
    }

    /// Route a request to the appropriate backend
    async fn route_to_backend(&mut self, request: JsonRpcRequest) -> Result<JsonRpcResponse, ProxyError> {
        let _permit = match self.global_inflight.clone() {
            Some(sem) => Some(sem.acquire_owned().await.map_err(|_| {
                ProxyError::BackendUnavailable("Global inflight limiter closed".to_string())
            })?),
            None => None,
        };

        // Determine which root to use
        let root = self.determine_root(&request);
        
        info!("Routing {} to root: {:?}", request.method, root);

        let root = match root {
            Some(r) => r,
            None => {
                return Ok(JsonRpcResponse::error(
                    request.id.clone(),
                    JsonRpcError::new(
                        ERROR_BACKEND_UNAVAILABLE,
                        "No workspace root available for routing",
                    ),
                ));
            }
        };

        // Get or create backend for this root
        let backend = match self.get_or_create_backend(root.clone()).await {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to get backend: {}", e);
                let code = match e {
                    ProxyError::BackendUnavailable(_) => ERROR_BACKEND_UNAVAILABLE,
                    _ => ERROR_BACKEND_SPAWN_FAILED,
                };
                return Ok(JsonRpcResponse::error(
                    request.id.clone(),
                    JsonRpcError::new(code, e.to_string()),
                ));
            }
        };

        // Send request to backend with retry (max 1 retry for crash recovery)
        match backend.send_request_with_retry(request.clone(), 1).await {
            Ok(response) => Ok(response),
            Err(e) => {
                error!("Backend request failed after retries: {}", e);
                Ok(JsonRpcResponse::error(
                    request.id.clone(),
                    JsonRpcError::new(ERROR_INTERNAL_ERROR, e.to_string()),
                ))
            }
        }
    }

    /// Determine which root to use for a request
    fn determine_root(&self, request: &JsonRpcRequest) -> Option<PathBuf> {
        // Try to extract URI from request and match to a root
        if let Some(uri) = request.get_uri() {
            if let Some(path) = Self::uri_to_path(&uri) {
                // Find longest prefix match among known roots
                let matched = self.roots.iter()
                    .filter(|root| path.starts_with(root))
                    .max_by_key(|root| root.as_os_str().len());
                
                if let Some(root) = matched {
                    return Some(root.clone());
                }
                
                // Auto-detect git root from file path
                if let Some(git_root) = Self::find_git_root(&path) {
                    info!("Auto-detected git root from URI: {}", git_root.display());
                    return Some(git_root);
                }
            }
        }

        // Fall back to default root if configured
        if let Some(ref root) = self.default_root {
            return Some(root.clone());
        }
        
        // Fall back to first known root
        if !self.roots.is_empty() {
            return Some(self.roots[0].clone());
        }
        
        None
    }
    
    /// Find git root by walking up from the given path
    fn find_git_root(path: &Path) -> Option<PathBuf> {
        let mut current = if path.is_file() {
            path.parent()?.to_path_buf()
        } else {
            path.to_path_buf()
        };
        
        loop {
            let git_dir = current.join(".git");
            if git_dir.exists() {
                return Some(current);
            }
            
            if !current.pop() {
                break;
            }
        }
        
        None
    }

    /// Get existing backend or create new one for the given root
    async fn get_or_create_backend(&mut self, root: PathBuf) -> Result<&mut BackendInstance, ProxyError> {
        // LRU cache handles eviction automatically when capacity is exceeded
        // But we need to ensure evicted backends are properly shut down
        // Check if we need to make room (LRU will auto-evict, but we want graceful shutdown)
        if self.backends.len() >= self.backends.cap().get() && !self.backends.contains(&root) {
            // Evict LRU backend gracefully before LRU auto-evicts
            if !self.evict_lru_backend().await {
                return Err(ProxyError::BackendUnavailable(
                    "All backends are busy (pending requests), cannot evict LRU".to_string(),
                ));
            }
        }

        // Create backend if it doesn't exist
        if !self.backends.contains(&root) {
            info!("Creating new backend for root: {}", root.display());
            
            #[cfg(windows)]
            let backend = BackendInstance::spawn(
                &self.config,
                root.clone(),
                self.job_object.clone(),
            ).await?;
            
            #[cfg(unix)]
            let backend = BackendInstance::spawn(
                &self.config,
                root.clone(),
                self.process_group.clone(),
            ).await?;
            
            // put() returns the evicted entry if any (but we already handled eviction above)
            self.backends.put(root.clone(), backend);
        }

        // get() promotes to most recently used
        Ok(self.backends.get_mut(&root).unwrap())
    }

    /// Evict the least recently used backend (with graceful shutdown)
    async fn evict_lru_backend(&mut self) -> bool {
        // Peek at LRU entries without promoting them
        let mut candidates: Vec<PathBuf> = self
            .backends
            .iter()
            .map(|(k, _)| k.clone())
            .collect();

        // Iterate from LRU (oldest) to MRU (newest) - LruCache iter is MRU-first, so reverse
        candidates.reverse();

        for root in candidates {
            // Check if backend has pending requests (peek doesn't promote)
            let has_pending = match self.backends.peek(&root) {
                Some(b) => b.has_pending().await,
                None => continue,
            };

            if has_pending {
                continue;
            }

            info!("Evicting LRU backend: {}", root.display());
            if let Some(mut backend) = self.backends.pop(&root) {
                backend.shutdown().await;
            }
            return true;
        }

        false
    }

    async fn forward_notification_to_backend(&mut self, request: JsonRpcRequest) -> Result<(), ProxyError> {
        let root = match self.determine_root(&request) {
            Some(r) => r,
            None => {
                warn!("Dropping notification {} because no workspace root is available", request.method);
                return Ok(());
            }
        };

        let backend = self.get_or_create_backend(root).await?;
        backend.send_notification(request).await
    }

    async fn read_next_message<R: tokio::io::AsyncBufRead + Unpin>(
        reader: &mut R,
        out: &mut String,
    ) -> Result<Option<()>, ProxyError> {
        out.clear();

        let mut first_line = String::new();

        loop {
            first_line.clear();
            let n = reader.read_line(&mut first_line).await?;
            if n == 0 {
                return Ok(None);
            }

            let line = first_line.trim_end_matches(&['\r', '\n'][..]);
            if line.is_empty() {
                continue;
            }

            if line.to_ascii_lowercase().starts_with("content-length:") {
                let len_str = line.splitn(2, ':').nth(1).unwrap_or("").trim();
                let content_length: usize = len_str.parse().map_err(|e| {
                    ProxyError::JsonRpcParseError(format!("Invalid Content-Length header: {}", e))
                })?;

                // Read remaining headers until blank line
                loop {
                    let mut header_line = String::new();
                    let hn = reader.read_line(&mut header_line).await?;
                    if hn == 0 {
                        return Ok(None);
                    }
                    if header_line == "\n" || header_line == "\r\n" {
                        break;
                    }
                }

                let mut buf = vec![0u8; content_length];
                reader.read_exact(&mut buf).await?;
                *out = String::from_utf8_lossy(&buf).to_string();
                return Ok(Some(()));
            }

            out.push_str(line);
            return Ok(Some(()));
        }
    }

    /// Check if a path is git-tracked (with caching, TTL, and size limit)
    async fn is_path_git_tracked(&mut self, path: &PathBuf) -> bool {
        const GIT_CACHE_TTL_SECS: u64 = 60;
        const GIT_CACHE_MAX_ENTRIES: usize = 10;
        
        // Find the root for this path
        let root = self.roots.iter()
            .filter(|r| path.starts_with(r))
            .max_by_key(|r| r.as_os_str().len())
            .cloned()
            .or_else(|| self.default_root.clone());

        let root = match root {
            Some(r) => r,
            None => return true, // No root found, allow by default
        };

        // Check if cache is expired (TTL)
        let cache_expired = self.git_cache_timestamps
            .get(&root)
            .map(|ts| ts.elapsed().as_secs() > GIT_CACHE_TTL_SECS)
            .unwrap_or(true);
        
        if cache_expired {
            self.git_tracked_cache.remove(&root);
            self.git_cache_timestamps.remove(&root);
        }

        // Evict oldest entries if cache is too large
        while self.git_tracked_cache.len() >= GIT_CACHE_MAX_ENTRIES {
            // Find the oldest entry
            if let Some(oldest_root) = self.git_cache_timestamps
                .iter()
                .min_by_key(|(_, ts)| *ts)
                .map(|(k, _)| k.clone())
            {
                debug!("Git cache full, evicting: {}", oldest_root.display());
                self.git_tracked_cache.remove(&oldest_root);
                self.git_cache_timestamps.remove(&oldest_root);
            } else {
                break;
            }
        }

        // Check cache or populate it
        if !self.git_tracked_cache.contains_key(&root) {
            if let Some(tracked) = git_filter::get_git_tracked_files(&root).await {
                info!("Git filter cache populated for {}: {} files", root.display(), tracked.len());
                self.git_tracked_cache.insert(root.clone(), tracked);
                self.git_cache_timestamps.insert(root.clone(), Instant::now());
            } else {
                // Not a git repo or git failed, allow all files
                return true;
            }
        }

        if let Some(tracked) = self.git_tracked_cache.get(&root) {
            git_filter::is_git_tracked(path, tracked)
        } else {
            true
        }
    }

    /// Check if a notification should be throttled
    fn should_throttle_notification(&self, request: &JsonRpcRequest) -> bool {
        // Only throttle if throttler is enabled
        if self.event_throttler.is_none() {
            return false;
        }
        
        // Throttle file change related notifications
        matches!(request.method.as_str(),
            "notifications/file/didChange" |
            "notifications/file/didCreate" |
            "notifications/file/didDelete" |
            "textDocument/didChange" |
            "textDocument/didSave"
        )
    }

    /// Flush throttled events to backends (batched by root)
    async fn flush_throttled_events(&mut self) {
        let throttler = match self.event_throttler.as_mut() {
            Some(t) => t,
            None => return,
        };

        if !throttler.should_flush() {
            return;
        }

        if let Some(event) = throttler.flush() {
            debug!("Flushing {} throttled file change events", event.paths.len());
            
            // Group paths by root for batch notifications
            let mut paths_by_root: HashMap<PathBuf, Vec<String>> = HashMap::new();
            
            for path in &event.paths {
                let root = self.roots.iter()
                    .filter(|r| path.starts_with(r))
                    .max_by_key(|r| r.as_os_str().len())
                    .cloned()
                    .or_else(|| self.default_root.clone());

                if let Some(root) = root {
                    let uri = format!("file:///{}", path.display().to_string().replace('\\', "/"));
                    paths_by_root.entry(root).or_default().push(uri);
                }
            }
            
            // Send batch notification per root
            for (root, uris) in paths_by_root {
                if let Some(backend) = self.backends.get_mut(&root) {
                    let notification = JsonRpcRequest {
                        jsonrpc: "2.0".to_string(),
                        method: "notifications/files/didChange".to_string(),
                        id: None,
                        params: Some(serde_json::json!({
                            "uris": uris
                        })),
                    };
                    debug!("Sending batch notification with {} uris to {}", uris.len(), root.display());
                    if let Err(e) = backend.send_notification(notification).await {
                        warn!("Failed to send throttled notification: {}", e);
                    }
                }
            }
        }
    }

    /// Cleanup idle backends and unhealthy backends
    async fn cleanup_idle_backends(&mut self, idle_ttl: Duration) {
        let now = Instant::now();
        
        // First, collect backends to check
        let roots_to_check: Vec<_> = self.backends
            .iter()
            .map(|(k, _)| k.clone())
            .collect();

        let mut roots_to_remove = Vec::new();

        for root in roots_to_check {
            if let Some(backend) = self.backends.peek_mut(&root) {
                // Check health first
                if !backend.health_check().await {
                    info!("Backend {} failed health check, marking for removal", root.display());
                    roots_to_remove.push(root.clone());
                    continue;
                }

                // Check idle timeout
                if now.duration_since(backend.last_used) > idle_ttl {
                    if !backend.has_pending().await {
                        info!("Backend {} is idle, marking for removal", root.display());
                        roots_to_remove.push(root.clone());
                    } else {
                        debug!("Backend {} has pending requests, skipping cleanup", root.display());
                    }
                }
            }
        }

        // Remove marked backends
        for root in roots_to_remove {
            info!("Cleaning up backend: {}", root.display());
            if let Some(mut backend) = self.backends.pop(&root) {
                backend.shutdown().await;
            }
        }
    }

    /// Shutdown all backends
    async fn shutdown_all_backends(&mut self) {
        info!("Shutting down all backends");
        // Drain all entries from LRU cache
        while let Some((root, mut backend)) = self.backends.pop_lru() {
            info!("Shutting down backend: {}", root.display());
            backend.shutdown().await;
        }
    }

    /// Convert file URI to path (with URL decoding for special characters)
    fn uri_to_path(uri: &str) -> Option<PathBuf> {
        let decoded_uri = percent_decode_str(uri)
            .decode_utf8()
            .ok()?;
        let uri = decoded_uri.as_ref();
        
        if uri.starts_with("file:///") {
            #[cfg(windows)]
            {
                // file:///C:/path -> C:/path
                let path = uri.strip_prefix("file:///")?;
                Some(PathBuf::from(path.replace('/', "\\")))
            }
            #[cfg(not(windows))]
            {
                // file:///path -> /path
                let path = uri.strip_prefix("file://")?;
                Some(PathBuf::from(path))
            }
        } else if uri.starts_with("file://") {
            let path = uri.strip_prefix("file://")?;
            Some(PathBuf::from(path))
        } else {
            // Assume it's already a path
            Some(PathBuf::from(uri))
        }
    }

    /// Get current metrics as a JSON value
    #[allow(dead_code)]
    pub fn get_metrics(&self) -> serde_json::Value {
        serde_json::json!({
            "uptime_seconds": self.metrics_start_time.elapsed().as_secs(),
            "total_requests": self.metrics_total_requests,
            "total_errors": self.metrics_total_errors,
            "active_backends": self.backends.len(),
            "max_backends": self.backends.cap().get(),
            "git_cache_entries": self.git_tracked_cache.len(),
        })
    }

    /// Increment request counter
    fn record_request(&mut self) {
        self.metrics_total_requests += 1;
    }

    /// Increment error counter
    fn record_error(&mut self) {
        self.metrics_total_errors += 1;
    }
}
