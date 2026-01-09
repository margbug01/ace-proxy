use clap::Parser;
use serde::Deserialize;
use std::path::PathBuf;
use tracing::{info, warn};

/// JSON config file structure
#[derive(Deserialize, Default, Debug)]
struct FileConfig {
    node: Option<PathBuf>,
    auggie_entry: Option<PathBuf>,
    mode: Option<String>,
    max_backends: Option<usize>,
    idle_ttl_seconds: Option<u64>,
    log_level: Option<String>,
    default_root: Option<PathBuf>,
    debounce_ms: Option<u64>,
    cpu_affinity: Option<u64>,
    low_priority: Option<bool>,
    git_filter: Option<bool>,
}

/// Rust MCP Proxy for Augment Context Engine
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct Config {
    /// Path to node.exe
    #[arg(long, env = "MCP_PROXY_NODE_PATH")]
    pub node: Option<PathBuf>,

    /// Path to auggie entry.js
    #[arg(long, env = "MCP_PROXY_AUGGIE_ENTRY")]
    pub auggie_entry: Option<PathBuf>,

    /// Auggie mode (default, minimal, etc.)
    #[arg(long, default_value = "default")]
    pub mode: String,

    /// Maximum number of backend instances
    #[arg(long, default_value = "3")]
    pub max_backends: usize,

    /// Idle timeout in seconds before backend is shut down
    #[arg(long, default_value = "600")]
    pub idle_ttl_seconds: u64,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info", env = "MCP_PROXY_LOG")]
    pub log_level: String,

    /// Spawn timeout in seconds
    #[arg(long, default_value = "30")]
    pub spawn_timeout_seconds: u64,

    /// Request timeout in seconds
    #[arg(long, default_value = "120")]
    pub request_timeout_seconds: u64,

    #[arg(long, default_value = "0")]
    pub max_inflight_global: usize,

    /// Default workspace root (used when no root is provided)
    #[arg(long, env = "MCP_PROXY_DEFAULT_ROOT")]
    pub default_root: Option<PathBuf>,

    /// Pre-spawn backend for default root during initialize (disabled by default for cold start)
    #[arg(long, default_value_t = false)]
    pub prewarm_default_root: bool,

    /// Event debounce window in milliseconds (0 to disable)
    #[arg(long, default_value = "500")]
    pub debounce_ms: u64,

    /// CPU affinity mask for backend processes (e.g., 0x03 = cores 0,1). 0 means no affinity.
    #[arg(long, default_value = "0")]
    pub cpu_affinity: u64,

    /// Set backend processes to Below Normal priority
    #[arg(long, default_value_t = true)]
    pub low_priority: bool,

    /// Use git ls-files to filter indexed files (excludes node_modules, dist, etc.)
    #[arg(long, default_value_t = true)]
    pub git_filter: bool,

    /// Enable single instance lock (prevents multiple proxy instances)
    #[arg(long, default_value_t = false)]
    pub single_instance: bool,
}

impl Config {
    /// Load config from file and merge with CLI args
    /// Priority: CLI args > env vars > config file > auto-detect
    pub fn with_auto_detect(mut self) -> Self {
        // Try to load config file
        let file_config = Self::load_config_file();
        
        // Merge file config (lower priority than CLI/env)
        if let Some(fc) = file_config {
            if self.node.is_none() {
                self.node = fc.node;
            }
            if self.auggie_entry.is_none() {
                self.auggie_entry = fc.auggie_entry;
            }
            if self.default_root.is_none() {
                self.default_root = fc.default_root;
            }
            if let Some(mode) = fc.mode {
                if self.mode == "default" {
                    self.mode = mode;
                }
            }
            if let Some(v) = fc.max_backends {
                if self.max_backends == 3 { self.max_backends = v; }
            }
            if let Some(v) = fc.idle_ttl_seconds {
                if self.idle_ttl_seconds == 600 { self.idle_ttl_seconds = v; }
            }
            if let Some(v) = fc.log_level {
                if self.log_level == "info" { self.log_level = v; }
            }
            if let Some(v) = fc.debounce_ms {
                if self.debounce_ms == 500 { self.debounce_ms = v; }
            }
            if let Some(v) = fc.cpu_affinity {
                if self.cpu_affinity == 0 { self.cpu_affinity = v; }
            }
            if let Some(v) = fc.low_priority {
                self.low_priority = v;
            }
            if let Some(v) = fc.git_filter {
                self.git_filter = v;
            }
        }
        
        // Validate configured paths exist, fallback to auto-detect if not
        if let Some(ref path) = self.node {
            if !path.exists() {
                warn!("Configured node path does not exist: {}, falling back to auto-detect", path.display());
                self.node = None;
            }
        }
        if let Some(ref path) = self.auggie_entry {
            if !path.exists() {
                warn!("Configured auggie_entry path does not exist: {}, falling back to auto-detect", path.display());
                self.auggie_entry = None;
            }
        }
        
        // Auto-detect remaining missing values
        if self.node.is_none() {
            self.node = Self::detect_node_path();
        }
        if self.auggie_entry.is_none() {
            self.auggie_entry = Self::detect_auggie_entry();
        }
        
        // Log detection results
        if let Some(ref node) = self.node {
            info!("Node.js: {}", node.display());
        } else {
            info!("⚠️ Node.js not found - please install Node.js or set --node");
        }
        if let Some(ref entry) = self.auggie_entry {
            info!("Auggie: {}", entry.display());
        } else {
            info!("⚠️ Auggie not found - please run: npm install -g @augmentcode/auggie");
        }
        
        self
    }

    /// Load config from file (searches multiple locations)
    fn load_config_file() -> Option<FileConfig> {
        let candidates = Self::get_config_file_candidates();
        
        for path in candidates {
            if path.exists() {
                match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        match serde_json::from_str::<FileConfig>(&content) {
                            Ok(config) => {
                                info!("Loaded config from: {}", path.display());
                                return Some(config);
                            }
                            Err(e) => {
                                eprintln!("Warning: Failed to parse {}: {}", path.display(), e);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Warning: Failed to read {}: {}", path.display(), e);
                    }
                }
            }
        }
        None
    }

    /// Get list of config file candidates in priority order
    fn get_config_file_candidates() -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        
        // 1. Current exe directory
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                candidates.push(exe_dir.join("mcp-proxy.json"));
            }
        }
        
        // 2. Current working directory
        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(cwd.join("mcp-proxy.json"));
        }
        
        // 3. User config directory
        #[cfg(windows)]
        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            candidates.push(PathBuf::from(&userprofile).join(".config").join("mcp-proxy.json"));
            candidates.push(PathBuf::from(&userprofile).join("mcp-proxy.json"));
        }
        
        #[cfg(not(windows))]
        if let Ok(home) = std::env::var("HOME") {
            candidates.push(PathBuf::from(&home).join(".config").join("mcp-proxy.json"));
            candidates.push(PathBuf::from(&home).join(".mcp-proxy.json"));
        }
        
        candidates
    }

    fn detect_node_path() -> Option<PathBuf> {
        // Try common locations
        #[cfg(windows)]
        {
            let candidates = [
                r"C:\Program Files\nodejs\node.exe",
                r"C:\Program Files (x86)\nodejs\node.exe",
            ];
            for path in candidates {
                let p = PathBuf::from(path);
                if p.exists() {
                    return Some(p);
                }
            }
            // Try PATH
            if let Ok(output) = std::process::Command::new("where").arg("node").output() {
                if output.status.success() {
                    if let Ok(s) = String::from_utf8(output.stdout) {
                        if let Some(line) = s.lines().next() {
                            return Some(PathBuf::from(line.trim()));
                        }
                    }
                }
            }
        }
        #[cfg(not(windows))]
        {
            if let Ok(output) = std::process::Command::new("which").arg("node").output() {
                if output.status.success() {
                    if let Ok(s) = String::from_utf8(output.stdout) {
                        return Some(PathBuf::from(s.trim()));
                    }
                }
            }
        }
        None
    }

    fn detect_auggie_entry() -> Option<PathBuf> {
        // Try to find auggie in common npm global locations
        #[cfg(windows)]
        {
            let mut candidates = Vec::new();
            
            // npm global (APPDATA)
            if let Ok(appdata) = std::env::var("APPDATA") {
                candidates.push(format!(r"{}\npm\node_modules\@augmentcode\auggie\augment.mjs", appdata));
                candidates.push(format!(r"{}\npm\node_modules\@augmentcode\auggie\dist\cli.js", appdata));
            }
            
            // pnpm global
            if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
                candidates.push(format!(r"{}\pnpm\global\5\node_modules\@augmentcode\auggie\augment.mjs", localappdata));
            }
            
            // yarn global
            if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
                candidates.push(format!(r"{}\Yarn\Data\global\node_modules\@augmentcode\auggie\augment.mjs", localappdata));
            }
            
            // Try npm root -g to find global modules
            if let Ok(output) = std::process::Command::new("npm").args(["root", "-g"]).output() {
                if output.status.success() {
                    if let Ok(root) = String::from_utf8(output.stdout) {
                        let root = root.trim();
                        candidates.push(format!(r"{}\@augmentcode\auggie\augment.mjs", root));
                        candidates.push(format!(r"{}\@augmentcode\auggie\dist\cli.js", root));
                    }
                }
            }
            
            for path in candidates {
                let p = PathBuf::from(&path);
                if p.exists() {
                    return Some(p);
                }
            }
        }
        
        #[cfg(not(windows))]
        {
            // Try npm root -g
            if let Ok(output) = std::process::Command::new("npm").args(["root", "-g"]).output() {
                if output.status.success() {
                    if let Ok(root) = String::from_utf8(output.stdout) {
                        let root = root.trim();
                        let candidates = [
                            format!("{}/augmentcode/auggie/augment.mjs", root),
                            format!("{}/@augmentcode/auggie/augment.mjs", root),
                        ];
                        for path in candidates {
                            let p = PathBuf::from(&path);
                            if p.exists() {
                                return Some(p);
                            }
                        }
                    }
                }
            }
        }
        
        None
    }
}
