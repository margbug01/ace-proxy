//! Backend process management for auggie instances

use crate::config::Config;
use crate::error::ProxyError;
use crate::jsonrpc::{JsonRpcId, JsonRpcRequest, JsonRpcResponse};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, error, info, warn};

/// Global counter for generating unique proxy IDs
static PROXY_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Generate a new unique proxy ID
fn next_proxy_id() -> u64 {
    PROXY_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Backend instance state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendState {
    Spawning,
    Ready,
    Stopping,
    Dead,
}

/// Pending request info for ID mapping
struct PendingRequest {
    client_id: Option<JsonRpcId>,
    response_tx: oneshot::Sender<JsonRpcResponse>,
}

/// A single backend instance (auggie process)
pub struct BackendInstance {
    pub root: PathBuf,
    pub state: BackendState,
    pub last_used: Instant,
    child: Option<Child>,
    stdin_tx: Option<mpsc::Sender<String>>,
    pending: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    /// Request timeout duration
    request_timeout: Duration,
    /// Config for restart
    config: Config,
    /// Job object reference for Windows
    #[cfg(windows)]
    job_object_ptr: Option<*const crate::job_object::JobObject>,
    /// ProcessGroup reference for Unix
    #[cfg(unix)]
    process_group_ptr: Option<*const crate::process_group::ProcessGroup>,
}

impl BackendInstance {
    /// Spawn a new backend instance for the given workspace root
    #[cfg(windows)]
    pub async fn spawn(
        config: &Config,
        root: PathBuf,
        job_object: Option<&crate::job_object::JobObject>,
    ) -> Result<Self, ProxyError> {
        Self::spawn_internal(config, root, job_object).await
    }

    #[cfg(unix)]
    pub async fn spawn(
        config: &Config,
        root: PathBuf,
        process_group: Option<&crate::process_group::ProcessGroup>,
    ) -> Result<Self, ProxyError> {
        Self::spawn_internal(config, root, process_group).await
    }

    /// Internal spawn implementation
    #[cfg(windows)]
    async fn spawn_internal(
        config: &Config,
        root: PathBuf,
        job_object: Option<&crate::job_object::JobObject>,
    ) -> Result<Self, ProxyError> {
        let node_path = config
            .node
            .as_ref()
            .ok_or_else(|| ProxyError::ConfigError("Node path not configured".to_string()))?;

        let auggie_entry = config
            .auggie_entry
            .as_ref()
            .ok_or_else(|| ProxyError::ConfigError("Auggie entry path not configured".to_string()))?;

        info!(
            "Spawning backend for root: {} with node: {:?}, entry: {:?}",
            root.display(),
            node_path,
            auggie_entry
        );

        // Build command - bypass .cmd to avoid cmd.exe shell issues
        let mut cmd = Command::new(node_path);
        cmd.arg(auggie_entry)
            .arg("--mcp")
            .arg("-m")
            .arg(&config.mode)
            .arg("--workspace-root")
            .arg(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // Let backend stderr pass through for debugging
            .env("AUGMENT_DISABLE_AUTO_UPDATE", "1");

        // On Windows, don't create a window
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            // creation_flags is available on tokio::process::Command on Windows
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = cmd.spawn().map_err(|e| {
            ProxyError::BackendSpawnFailed(format!(
                "Failed to spawn backend: {}. Node: {:?}, Entry: {:?}",
                e, node_path, auggie_entry
            ))
        })?;

        // Assign to job object on Windows and set process priority/affinity
        #[cfg(windows)]
        if let Some(pid) = child.id() {
            debug!("Backend process spawned with PID: {}", pid);
            
            // Assign to job object
            if let Some(job) = job_object {
                match job.assign_process_by_pid(pid) {
                    Ok(_) => info!("Process {} assigned to Job Object", pid),
                    Err(e) => warn!("Failed to assign process to Job Object: {} - process cleanup may not work correctly", e),
                }
            }
            
            // Set process priority and CPU affinity
            Self::configure_process_resources(pid, config);
        }

        let stdin = child.stdin.take().ok_or_else(|| {
            ProxyError::BackendSpawnFailed("Failed to get stdin handle".to_string())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ProxyError::BackendSpawnFailed("Failed to get stdout handle".to_string())
        })?;

        // Create channel for sending requests to backend
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(100);

        // Pending requests map
        let pending: Arc<Mutex<HashMap<u64, PendingRequest>>> = Arc::new(Mutex::new(HashMap::new()));
        let pending_clone = pending.clone();

        // Spawn task to write to backend stdin
        let mut stdin_writer = stdin;
        tokio::spawn(async move {
            while let Some(line) = stdin_rx.recv().await {
                if let Err(e) = stdin_writer.write_all(line.as_bytes()).await {
                    error!("Failed to write to backend stdin: {}", e);
                    break;
                }
                if let Err(e) = stdin_writer.write_all(b"\n").await {
                    error!("Failed to write newline to backend stdin: {}", e);
                    break;
                }
                if let Err(e) = stdin_writer.flush().await {
                    error!("Failed to flush backend stdin: {}", e);
                    break;
                }
            }
            debug!("Stdin writer task ended");
        });

        // Spawn task to read backend stdout and dispatch responses
        let mut reader = BufReader::new(stdout);
        tokio::spawn(async move {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        debug!("Backend stdout closed (EOF)");
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        
                        debug!("Backend response: {}", trimmed);
                        
                        match serde_json::from_str::<JsonRpcResponse>(trimmed) {
                            Ok(response) => {
                                // Extract proxy_id from response
                                if let Some(ref id) = response.id {
                                    let proxy_id = match id {
                                        JsonRpcId::Number(n) => *n as u64,
                                        JsonRpcId::String(s) => {
                                            s.parse().unwrap_or(0)
                                        }
                                    };
                                    
                                    let mut pending_guard = pending_clone.lock().await;
                                    if let Some(req) = pending_guard.remove(&proxy_id) {
                                        // Restore original client ID
                                        let mut final_response = response;
                                        final_response.id = req.client_id;
                                        
                                        if req.response_tx.send(final_response).is_err() {
                                            warn!("Failed to send response - receiver dropped");
                                        }
                                    } else {
                                        warn!("Received response for unknown proxy_id: {}", proxy_id);
                                    }
                                }
                            }
                            Err(e) => {
                                // Might be a notification or malformed
                                debug!("Failed to parse backend response: {} - {}", e, trimmed);
                            }
                        }
                    }
                    Err(e) => {
                        error!("Error reading backend stdout: {}", e);
                        break;
                    }
                }
            }
            debug!("Stdout reader task ended");
        });

        Ok(Self {
            root,
            state: BackendState::Ready,
            last_used: Instant::now(),
            child: Some(child),
            stdin_tx: Some(stdin_tx),
            pending,
            request_timeout: Duration::from_secs(config.request_timeout_seconds),
            config: config.clone(),
            #[cfg(windows)]
            job_object_ptr: job_object.map(|j| j as *const _),
        })
    }

    /// Internal spawn implementation for Unix (macOS/Linux)
    #[cfg(unix)]
    async fn spawn_internal(
        config: &Config,
        root: PathBuf,
        process_group: Option<&crate::process_group::ProcessGroup>,
    ) -> Result<Self, ProxyError> {
        let node_path = config
            .node
            .as_ref()
            .ok_or_else(|| ProxyError::ConfigError("Node path not configured".to_string()))?;

        let auggie_entry = config
            .auggie_entry
            .as_ref()
            .ok_or_else(|| ProxyError::ConfigError("Auggie entry path not configured".to_string()))?;

        info!(
            "Spawning backend for root: {} with node: {:?}, entry: {:?}",
            root.display(),
            node_path,
            auggie_entry
        );

        // Build command
        let mut cmd = Command::new(node_path);
        cmd.arg(auggie_entry)
            .arg("--mcp")
            .arg("-m")
            .arg(&config.mode)
            .arg("--workspace-root")
            .arg(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .env("AUGMENT_DISABLE_AUTO_UPDATE", "1");

        let mut child = cmd.spawn().map_err(|e| {
            ProxyError::BackendSpawnFailed(format!(
                "Failed to spawn backend: {}. Node: {:?}, Entry: {:?}",
                e, node_path, auggie_entry
            ))
        })?;

        // Add to process group on Unix and configure resources
        if let Some(pid) = child.id() {
            debug!("Backend process spawned with PID: {}", pid);
            
            // Add to process group
            if let Some(pg) = process_group {
                match pg.add_process(pid) {
                    Ok(_) => info!("Process {} added to ProcessGroup", pid),
                    Err(e) => warn!("Failed to add process to ProcessGroup: {} - process cleanup may not work correctly", e),
                }
            }
            
            // Set process priority on Unix (nice value)
            Self::configure_process_resources_unix(pid, config);
        }

        let stdin = child.stdin.take().ok_or_else(|| {
            ProxyError::BackendSpawnFailed("Failed to get stdin handle".to_string())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ProxyError::BackendSpawnFailed("Failed to get stdout handle".to_string())
        })?;

        // Create channel for sending requests to backend
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(100);

        // Pending requests map
        let pending: Arc<Mutex<HashMap<u64, PendingRequest>>> = Arc::new(Mutex::new(HashMap::new()));
        let pending_clone = pending.clone();

        // Spawn task to write to backend stdin
        let mut stdin_writer = stdin;
        tokio::spawn(async move {
            while let Some(line) = stdin_rx.recv().await {
                if let Err(e) = stdin_writer.write_all(line.as_bytes()).await {
                    error!("Failed to write to backend stdin: {}", e);
                    break;
                }
                if let Err(e) = stdin_writer.write_all(b"\n").await {
                    error!("Failed to write newline to backend stdin: {}", e);
                    break;
                }
                if let Err(e) = stdin_writer.flush().await {
                    error!("Failed to flush backend stdin: {}", e);
                    break;
                }
            }
            debug!("Stdin writer task ended");
        });

        // Spawn task to read backend stdout and dispatch responses
        let mut reader = BufReader::new(stdout);
        tokio::spawn(async move {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        debug!("Backend stdout closed (EOF)");
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        
                        debug!("Backend response: {}", trimmed);
                        
                        match serde_json::from_str::<JsonRpcResponse>(trimmed) {
                            Ok(response) => {
                                if let Some(ref id) = response.id {
                                    let proxy_id = match id {
                                        JsonRpcId::Number(n) => *n as u64,
                                        JsonRpcId::String(s) => {
                                            s.parse().unwrap_or(0)
                                        }
                                    };
                                    
                                    let mut pending_guard = pending_clone.lock().await;
                                    if let Some(req) = pending_guard.remove(&proxy_id) {
                                        let mut final_response = response;
                                        final_response.id = req.client_id;
                                        
                                        if req.response_tx.send(final_response).is_err() {
                                            warn!("Failed to send response - receiver dropped");
                                        }
                                    } else {
                                        warn!("Received response for unknown proxy_id: {}", proxy_id);
                                    }
                                }
                            }
                            Err(e) => {
                                debug!("Failed to parse backend response: {} - {}", e, trimmed);
                            }
                        }
                    }
                    Err(e) => {
                        error!("Error reading backend stdout: {}", e);
                        break;
                    }
                }
            }
            debug!("Stdout reader task ended");
        });

        Ok(Self {
            root,
            state: BackendState::Ready,
            last_used: Instant::now(),
            child: Some(child),
            stdin_tx: Some(stdin_tx),
            pending,
            request_timeout: Duration::from_secs(config.request_timeout_seconds),
            config: config.clone(),
            process_group_ptr: process_group.map(|pg| pg as *const _),
        })
    }

    /// Configure process resources (priority) on Unix
    #[cfg(unix)]
    fn configure_process_resources_unix(pid: u32, config: &Config) {
        use nix::sys::resource::{setpriority, Which};
        
        // Set lower priority (higher nice value) if enabled
        if config.low_priority {
            // Nice value 10 is "below normal" equivalent
            match setpriority(Which::Process(nix::unistd::Pid::from_raw(pid as i32)), 10) {
                Ok(_) => info!("Process {} set to low priority (nice 10)", pid),
                Err(e) => warn!("Failed to set priority for process {}: {}", pid, e),
            }
        }
        
        // Note: CPU affinity on macOS requires different APIs (thread_policy_set)
        // and is more complex. For now, we skip CPU affinity on Unix.
        if config.cpu_affinity != 0 {
            #[cfg(target_os = "linux")]
            {
                // On Linux, we can use sched_setaffinity
                warn!("CPU affinity configuration is not yet implemented on Linux");
            }
            #[cfg(target_os = "macos")]
            {
                // macOS doesn't support process-level CPU affinity in the same way
                debug!("CPU affinity is not supported on macOS, ignoring");
            }
        }
    }

    /// Send a request to this backend and wait for response
    pub async fn send_request(
        &mut self,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, ProxyError> {
        self.last_used = Instant::now();

        let stdin_tx = self.stdin_tx.as_ref().ok_or_else(|| {
            ProxyError::BackendUnavailable("Backend stdin not available".to_string())
        })?;

        if request.is_notification() {
            return Err(ProxyError::RoutingFailed(
                "send_request called with notification (id is None)".to_string(),
            ));
        }

        // Generate proxy ID and setup response channel
        let proxy_id = next_proxy_id();
        let (response_tx, response_rx) = oneshot::channel();

        // Register pending request
        {
            let mut pending = self.pending.lock().await;
            pending.insert(
                proxy_id,
                PendingRequest {
                    client_id: request.id.clone(),
                    response_tx,
                },
            );
        }

        // Replace ID with proxy ID
        let mut backend_request = request.clone();
        backend_request.id = Some(JsonRpcId::Number(proxy_id as i64));

        let json = serde_json::to_string(&backend_request)?;
        debug!(
            "Sending request to backend: {} (proxy_id: {})",
            request.method, proxy_id
        );

        stdin_tx.send(json).await.map_err(|e| {
            ProxyError::BackendUnavailable(format!("Failed to send to backend: {}", e))
        })?;

        // Wait for response with timeout
        match tokio::time::timeout(self.request_timeout, response_rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => {
                // Channel closed - backend probably died
                let mut pending = self.pending.lock().await;
                pending.remove(&proxy_id);
                self.state = BackendState::Dead;
                Err(ProxyError::BackendUnavailable(
                    "Backend response channel closed".to_string(),
                ))
            }
            Err(_) => {
                // Timeout - remove pending and mark backend as potentially unhealthy
                warn!("Request {} timed out after {:?}", request.method, self.request_timeout);
                let mut pending = self.pending.lock().await;
                pending.remove(&proxy_id);
                Err(ProxyError::BackendTimeout(format!(
                    "Request timed out after {} seconds",
                    self.request_timeout.as_secs()
                )))
            }
        }
    }

    pub async fn send_notification(&mut self, notification: JsonRpcRequest) -> Result<(), ProxyError> {
        self.last_used = Instant::now();

        if !notification.is_notification() {
            return Err(ProxyError::RoutingFailed(
                "send_notification called with request (id is Some)".to_string(),
            ));
        }

        let stdin_tx = self.stdin_tx.as_ref().ok_or_else(|| {
            ProxyError::BackendUnavailable("Backend stdin not available".to_string())
        })?;

        let json = serde_json::to_string(&notification)?;
        debug!("Sending notification to backend: {}", notification.method);
        stdin_tx.send(json).await.map_err(|e| {
            ProxyError::BackendUnavailable(format!("Failed to send to backend: {}", e))
        })?;

        Ok(())
    }

    /// Check if backend has pending requests
    pub async fn has_pending(&self) -> bool {
        let pending = self.pending.lock().await;
        !pending.is_empty()
    }

    /// Check if backend is dead/crashed
    pub fn is_dead(&self) -> bool {
        self.state == BackendState::Dead
    }

    /// Configure process resources (priority and CPU affinity) on Windows
    #[cfg(windows)]
    fn configure_process_resources(pid: u32, config: &Config) {
        use windows::Win32::System::Threading::{
            OpenProcess, SetPriorityClass, SetProcessAffinityMask,
            BELOW_NORMAL_PRIORITY_CLASS, PROCESS_SET_INFORMATION, PROCESS_QUERY_INFORMATION,
        };
        use windows::Win32::Foundation::CloseHandle;

        unsafe {
            let handle = match OpenProcess(PROCESS_SET_INFORMATION | PROCESS_QUERY_INFORMATION, false, pid) {
                Ok(h) if !h.is_invalid() => h,
                Ok(_) => {
                    warn!("OpenProcess returned invalid handle for PID {}", pid);
                    return;
                }
                Err(e) => {
                    warn!("Failed to open process {} for resource configuration: {}", pid, e);
                    return;
                }
            };

            // Set below normal priority if enabled
            if config.low_priority {
                match SetPriorityClass(handle, BELOW_NORMAL_PRIORITY_CLASS) {
                    Ok(_) => info!("Process {} set to Below Normal priority", pid),
                    Err(e) => warn!("Failed to set priority for process {}: {}", pid, e),
                }
            }

            // Set CPU affinity if specified (non-zero)
            if config.cpu_affinity != 0 {
                match SetProcessAffinityMask(handle, config.cpu_affinity as usize) {
                    Ok(_) => info!("Process {} CPU affinity set to 0x{:X}", pid, config.cpu_affinity),
                    Err(e) => warn!("Failed to set CPU affinity for process {}: {}", pid, e),
                }
            }

            let _ = CloseHandle(handle);
        }
    }

    /// Restart the backend process
    #[cfg(windows)]
    pub async fn restart(&mut self) -> Result<(), ProxyError> {
        info!("Restarting backend for root: {}", self.root.display());
        
        // Shutdown existing process
        self.shutdown().await;
        
        // Get job object from raw pointer (unsafe but necessary for restart)
        let job_object = self.job_object_ptr.map(|ptr| unsafe { &*ptr });
        
        // Respawn
        let mut new_instance = Self::spawn(&self.config, self.root.clone(), job_object).await?;
        
        // Take ownership of fields from new instance using std::mem::take
        self.state = new_instance.state;
        self.child = std::mem::take(&mut new_instance.child);
        self.stdin_tx = std::mem::take(&mut new_instance.stdin_tx);
        self.pending = std::mem::take(&mut new_instance.pending);
        self.last_used = Instant::now();
        
        // Prevent new_instance Drop from killing the process we just took
        new_instance.state = BackendState::Dead;
        
        info!("Backend restarted successfully for root: {}", self.root.display());
        Ok(())
    }

    #[cfg(unix)]
    pub async fn restart(&mut self) -> Result<(), ProxyError> {
        info!("Restarting backend for root: {}", self.root.display());
        
        // Shutdown existing process
        self.shutdown().await;
        
        // Get process group from raw pointer (unsafe but necessary for restart)
        let process_group = self.process_group_ptr.map(|ptr| unsafe { &*ptr });
        
        // Respawn
        let mut new_instance = Self::spawn(&self.config, self.root.clone(), process_group).await?;
        
        // Take ownership of fields from new instance using std::mem::take
        self.state = new_instance.state;
        self.child = std::mem::take(&mut new_instance.child);
        self.stdin_tx = std::mem::take(&mut new_instance.stdin_tx);
        self.pending = std::mem::take(&mut new_instance.pending);
        self.last_used = Instant::now();
        
        // Prevent new_instance Drop from killing the process we just took
        new_instance.state = BackendState::Dead;
        
        info!("Backend restarted successfully for root: {}", self.root.display());
        Ok(())
    }

    /// Send request with automatic retry on failure (crash recovery)
    pub async fn send_request_with_retry(
        &mut self,
        request: JsonRpcRequest,
        max_retries: u32,
    ) -> Result<JsonRpcResponse, ProxyError> {
        let mut last_error = None;
        
        for attempt in 0..=max_retries {
            // Check if backend is dead and needs restart
            if self.is_dead() && attempt > 0 {
                warn!("Backend is dead, attempting restart (attempt {}/{})", attempt, max_retries);
                if let Err(e) = self.restart().await {
                    error!("Failed to restart backend: {}", e);
                    last_error = Some(e);
                    continue;
                }
            }
            
            match self.send_request(request.clone()).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if attempt < max_retries {
                        warn!(
                            "Request failed (attempt {}/{}): {}, will retry",
                            attempt + 1,
                            max_retries + 1,
                            e
                        );
                        last_error = Some(e);
                        // Mark as dead to trigger restart on next attempt
                        if self.state != BackendState::Dead {
                            self.state = BackendState::Dead;
                        }
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        
        Err(last_error.unwrap_or_else(|| ProxyError::BackendUnavailable("All retries exhausted".to_string())))
    }

    /// Shutdown the backend
    pub async fn shutdown(&mut self) {
        info!("Shutting down backend for root: {}", self.root.display());
        self.state = BackendState::Stopping;
        
        // Close stdin channel to signal shutdown
        self.stdin_tx.take();
        
        // Kill the child process
        if let Some(mut child) = self.child.take() {
            if let Err(e) = child.kill().await {
                warn!("Failed to kill backend process: {}", e);
            }
        }
        
        self.state = BackendState::Dead;
    }
}

impl Drop for BackendInstance {
    fn drop(&mut self) {
        // Ensure process is killed on drop
        if let Some(ref mut child) = self.child {
            // Use start_kill for sync drop context
            let _ = child.start_kill();
        }
    }
}
