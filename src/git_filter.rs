//! Git-based file filtering
//! Uses `git ls-files` to get tracked files, automatically excluding node_modules, dist, etc.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tracing::{debug, info, warn};

/// Git tracked files cache with optimized lookup
/// 
/// Stores both the full file paths and their parent directories for O(1) lookup
pub struct GitTrackedFiles {
    /// Full paths of tracked files
    files: HashSet<PathBuf>,
    /// All parent directories of tracked files (for prefix matching)
    directories: HashSet<PathBuf>,
}

impl GitTrackedFiles {
    /// Create from a set of tracked file paths
    pub fn new(files: HashSet<PathBuf>) -> Self {
        let mut directories = HashSet::new();
        
        // Pre-compute all parent directories for O(1) lookup
        for file in &files {
            let mut current = file.parent();
            while let Some(dir) = current {
                if !directories.insert(dir.to_path_buf()) {
                    // Already seen this directory and all its parents
                    break;
                }
                current = dir.parent();
            }
        }
        
        Self { files, directories }
    }
    
    /// Check if a path is tracked (file or within tracked directory)
    /// O(path_depth) complexity instead of O(n)
    pub fn is_tracked(&self, path: &Path) -> bool {
        // Direct file match - O(1)
        if self.files.contains(path) {
            return true;
        }
        
        // Check if path is a tracked directory - O(1)
        if self.directories.contains(path) {
            return true;
        }
        
        // Check if any ancestor is a tracked file (rare case: checking subpath of a file)
        // This handles the case where tracked.starts_with(path)
        // O(path_depth) - typically very small
        let mut current = path.parent();
        while let Some(dir) = current {
            if self.files.contains(dir) {
                return true;
            }
            current = dir.parent();
        }
        
        false
    }
    
    /// Get the number of tracked files
    pub fn len(&self) -> usize {
        self.files.len()
    }
    
    /// Check if empty
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Get list of git-tracked files for a workspace root (async version)
pub async fn get_git_tracked_files(root: &Path) -> Option<GitTrackedFiles> {
    // Check if this is a git repository
    if !root.join(".git").exists() {
        debug!("Not a git repository: {}", root.display());
        return None;
    }

    // Run git ls-files asynchronously
    let output = match Command::new("git")
        .arg("ls-files")
        .arg("--cached")
        .arg("--others")
        .arg("--exclude-standard")
        .current_dir(root)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            warn!("Failed to run git ls-files: {}", e);
            return None;
        }
    };

    if !output.status.success() {
        warn!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let files: HashSet<PathBuf> = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| root.join(line))
        .collect();

    let file_count = files.len();
    let tracked = GitTrackedFiles::new(files);

    info!(
        "Git filter: found {} tracked files in {} (cached {} directories)",
        file_count,
        root.display(),
        tracked.directories.len()
    );

    Some(tracked)
}

/// Legacy function for backward compatibility
/// Prefer using GitTrackedFiles::is_tracked() directly
pub fn is_git_tracked(path: &Path, tracked_files: &GitTrackedFiles) -> bool {
    tracked_files.is_tracked(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_tracked_files_basic() {
        let mut files = HashSet::new();
        files.insert(PathBuf::from("/project/src/main.rs"));
        files.insert(PathBuf::from("/project/Cargo.toml"));
        
        let tracked = GitTrackedFiles::new(files);

        assert!(tracked.is_tracked(Path::new("/project/src/main.rs")));
        assert!(tracked.is_tracked(Path::new("/project/Cargo.toml")));
        assert!(!tracked.is_tracked(Path::new("/project/node_modules/foo.js")));
    }
    
    #[test]
    fn test_git_tracked_directories() {
        let mut files = HashSet::new();
        files.insert(PathBuf::from("/project/src/lib.rs"));
        files.insert(PathBuf::from("/project/src/utils/helper.rs"));
        
        let tracked = GitTrackedFiles::new(files);
        
        // Parent directories should be tracked
        assert!(tracked.is_tracked(Path::new("/project/src")));
        assert!(tracked.is_tracked(Path::new("/project/src/utils")));
        assert!(tracked.is_tracked(Path::new("/project")));
        
        // Unrelated directories should not be tracked
        assert!(!tracked.is_tracked(Path::new("/project/node_modules")));
        assert!(!tracked.is_tracked(Path::new("/other")));
    }
    
    #[test]
    fn test_legacy_is_git_tracked() {
        let mut files = HashSet::new();
        files.insert(PathBuf::from("/project/src/main.rs"));
        
        let tracked = GitTrackedFiles::new(files);
        
        assert!(is_git_tracked(Path::new("/project/src/main.rs"), &tracked));
        assert!(!is_git_tracked(Path::new("/project/node_modules/foo.js"), &tracked));
    }
    
    #[test]
    fn test_empty_tracked_files() {
        let tracked = GitTrackedFiles::new(HashSet::new());
        assert!(tracked.is_empty());
        assert_eq!(tracked.len(), 0);
        assert!(!tracked.is_tracked(Path::new("/any/path")));
    }
}
