mod config;
mod error;
mod jsonrpc;
mod backend;
mod proxy;
mod throttle;
mod git_filter;

#[cfg(windows)]
mod job_object;

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
    
    info!("MCP Proxy starting with config: {:?}", config);
    
    // Create and run proxy
    let mut proxy = McpProxy::new(config)?;
    proxy.run().await?;
    
    Ok(())
}
