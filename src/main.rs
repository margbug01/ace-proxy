mod config;
mod error;
mod jsonrpc;
mod backend;
mod proxy;
mod throttle;
mod git_filter;

#[cfg(windows)]
mod job_object;

#[cfg(unix)]
mod process_group;

use anyhow::Result;
use clap::Parser;
use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;

use config::Config;
use proxy::McpProxy;

#[cfg(windows)]
use windows::core::w;

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, ERROR_ALREADY_EXISTS};

#[cfg(windows)]
use windows::Win32::System::Threading::CreateMutexW;

#[cfg(windows)]
struct SingleInstanceMutex {
    handle: HANDLE,
}

#[cfg(windows)]
impl Drop for SingleInstanceMutex {
    fn drop(&mut self) {
        unsafe {
            if !self.handle.is_invalid() {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(windows)]
fn acquire_single_instance_mutex() -> Result<SingleInstanceMutex> {
    unsafe {
        let handle = CreateMutexW(None, false, w!("Global\\mcp_proxy_lock"))?;
        let last_error = GetLastError();
        if last_error == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            anyhow::bail!("mcp-proxy is already running (Global\\mcp_proxy_lock exists)");
        }
        Ok(SingleInstanceMutex { handle })
    }
}

#[cfg(unix)]
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

#[cfg(unix)]
struct SingleInstanceLock {
    _file: File,
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl Drop for SingleInstanceLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn acquire_single_instance_lock() -> Result<SingleInstanceLock> {
    let lock_path = std::env::var("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".mcp-proxy.lock"))
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/mcp-proxy.lock"));
    
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .mode(0o600)
        .open(&lock_path)?;
    
    // Use libc flock directly for simpler API
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(&file);
    let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    
    if result == 0 {
        Ok(SingleInstanceLock { _file: file, path: lock_path })
    } else {
        let errno = std::io::Error::last_os_error();
        if errno.raw_os_error() == Some(libc::EWOULDBLOCK) {
            anyhow::bail!("mcp-proxy is already running (lock file: {})", lock_path.display());
        } else {
            anyhow::bail!("Failed to acquire lock: {}", errno);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::parse();
    
    // Initialize logging
    let log_level = match config.log_level.as_str() {
        "trace" => Level::TRACE,
        "debug" => Level::DEBUG,
        "info" => Level::INFO,
        "warn" => Level::WARN,
        "error" => Level::ERROR,
        _ => Level::INFO,
    };
    
    FmtSubscriber::builder()
        .with_max_level(log_level)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    #[cfg(windows)]
    let _single_instance_mutex = if config.single_instance {
        match acquire_single_instance_mutex() {
            Ok(m) => Some(m),
            Err(e) => {
                error!("{}", e);
                return Err(e);
            }
        }
    } else {
        None
    };

    #[cfg(unix)]
    let _single_instance_lock = if config.single_instance {
        match acquire_single_instance_lock() {
            Ok(l) => Some(l),
            Err(e) => {
                error!("{}", e);
                return Err(e);
            }
        }
    } else {
        None
    };
    
    info!("MCP Proxy starting with config: {:?}", config);
    
    // Create and run proxy
    let mut proxy = McpProxy::new(config)?;
    proxy.run().await?;
    
    Ok(())
}
