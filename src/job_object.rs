//! Windows Job Object for process lifecycle management
//! Ensures all child processes are killed when the proxy exits

use crate::error::ProxyError;
use tracing::{debug, info, warn};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// Wrapper around Windows Job Object
/// When this is dropped, all processes in the job are terminated
pub struct JobObject {
    handle: HANDLE,
}

impl JobObject {
    /// Create a new Job Object with KILL_ON_JOB_CLOSE flag
    pub fn new() -> Result<Self, ProxyError> {
        unsafe {
            // Create unnamed job object
            let handle = CreateJobObjectW(None, None)
                .map_err(|e| ProxyError::JobObjectError(format!("CreateJobObjectW failed: {}", e)))?;

            if handle.is_invalid() {
                return Err(ProxyError::JobObjectError(
                    "CreateJobObjectW returned invalid handle".to_string(),
                ));
            }

            // Set KILL_ON_JOB_CLOSE limit
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            let result = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );

            if let Err(e) = result {
                CloseHandle(handle).ok();
                return Err(ProxyError::JobObjectError(format!(
                    "SetInformationJobObject failed: {}",
                    e
                )));
            }

            info!("Job Object created with KILL_ON_JOB_CLOSE flag");
            Ok(Self { handle })
        }
    }

    /// Assign a child process to this job object by PID
    /// This is useful when we only have the process ID (e.g., from tokio::process::Child)
    pub fn assign_process_by_pid(&self, pid: u32) -> Result<(), ProxyError> {
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_ALL_ACCESS};
        
        unsafe {
            // Open handle to the process
            let process_handle = OpenProcess(PROCESS_ALL_ACCESS, false, pid)
                .map_err(|e| ProxyError::JobObjectError(format!("OpenProcess failed for PID {}: {}", pid, e)))?;
            
            if process_handle.is_invalid() {
                return Err(ProxyError::JobObjectError(format!(
                    "OpenProcess returned invalid handle for PID {}",
                    pid
                )));
            }

            let result = AssignProcessToJobObject(self.handle, process_handle);
            
            // Close the process handle we opened (job object keeps its own reference)
            let _ = CloseHandle(process_handle);
            
            result.map_err(|e| {
                // Error code 5 (ACCESS_DENIED) often means the process is already in another job
                if e.code().0 as u32 == 5 {
                    warn!("Process {} already in a job object (ACCESS_DENIED). This is expected on some Windows configurations.", pid);
                    ProxyError::JobObjectError(format!(
                        "AssignProcessToJobObject failed for PID {} (process may already be in a job): {}",
                        pid, e
                    ))
                } else {
                    ProxyError::JobObjectError(format!("AssignProcessToJobObject failed for PID {}: {}", pid, e))
                }
            })?;

            debug!("Process PID {} assigned to Job Object", pid);
            Ok(())
        }
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        unsafe {
            if !self.handle.is_invalid() {
                info!("Closing Job Object - all child processes will be terminated");
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

// Job Object is Send + Sync because the handle is thread-safe
unsafe impl Send for JobObject {}
unsafe impl Sync for JobObject {}
