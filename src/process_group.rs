//! Unix Process Group for process lifecycle management
//! Ensures all child processes are killed when the proxy exits

use crate::error::ProxyError;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::collections::HashSet;
use std::sync::Mutex;
use tracing::{debug, info, warn};

/// Wrapper around Unix Process Group management
/// Tracks child PIDs and kills them on drop
pub struct ProcessGroup {
    /// Set of child process PIDs to manage
    children: Mutex<HashSet<i32>>,
}

impl ProcessGroup {
    /// Create a new ProcessGroup
    pub fn new() -> Result<Self, ProxyError> {
        info!("ProcessGroup created for child process management");
        Ok(Self {
            children: Mutex::new(HashSet::new()),
        })
    }

    /// Add a child process to the group by PID
    pub fn add_process(&self, pid: u32) -> Result<(), ProxyError> {
        let mut children = self.children.lock().map_err(|e| {
            ProxyError::JobObjectError(format!("Failed to lock children set: {}", e))
        })?;
        
        children.insert(pid as i32);
        debug!("Process PID {} added to ProcessGroup", pid);
        Ok(())
    }

    /// Remove a process from tracking (called when process exits normally)
    pub fn remove_process(&self, pid: u32) {
        if let Ok(mut children) = self.children.lock() {
            children.remove(&(pid as i32));
            debug!("Process PID {} removed from ProcessGroup", pid);
        }
    }

    /// Kill all tracked child processes
    fn kill_all(&self) {
        if let Ok(children) = self.children.lock() {
            for &pid in children.iter() {
                let nix_pid = Pid::from_raw(pid);
                
                // First try SIGTERM for graceful shutdown
                match kill(nix_pid, Signal::SIGTERM) {
                    Ok(_) => debug!("Sent SIGTERM to process {}", pid),
                    Err(nix::errno::Errno::ESRCH) => {
                        // Process already dead, ignore
                        debug!("Process {} already terminated", pid);
                    }
                    Err(e) => warn!("Failed to send SIGTERM to process {}: {}", pid, e),
                }
            }
            
            // Give processes a moment to terminate gracefully
            std::thread::sleep(std::time::Duration::from_millis(100));
            
            // Then SIGKILL any remaining
            for &pid in children.iter() {
                let nix_pid = Pid::from_raw(pid);
                match kill(nix_pid, Signal::SIGKILL) {
                    Ok(_) => debug!("Sent SIGKILL to process {}", pid),
                    Err(nix::errno::Errno::ESRCH) => {}
                    Err(e) => warn!("Failed to send SIGKILL to process {}: {}", pid, e),
                }
            }
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        info!("Dropping ProcessGroup - killing all child processes");
        self.kill_all();
    }
}

// ProcessGroup is Send + Sync because it uses Mutex internally
unsafe impl Send for ProcessGroup {}
unsafe impl Sync for ProcessGroup {}
