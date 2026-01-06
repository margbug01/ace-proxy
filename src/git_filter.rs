//! Git-based file filtering
//! Uses `git ls-files` to get tracked files, automatically excluding node_modules, dist, etc.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, info, warn};

/// Get list of git-tracked files for a workspace root
pub fn get_git_tracked_files(root: &Path) -> Option<HashSet<PathBuf>> {
    // Check if this is a git repository
    if !root.join(".git").exists() {
        debug!("Not a git repository: {}", root.display());
        return None;
    }

    // Run git ls-files
    let output = match Command::new("git")
        .arg("ls-files")
        .arg("--cached")
        .arg("--others")
        .arg("--exclude-standard")
        .current_dir(root)
        .output()
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

    info!(
        "Git filter: found {} tracked files in {}",
        files.len(),
        root.display()
    );

    Some(files)
}

/// Check if a path is in the git-tracked set
pub fn is_git_tracked(path: &Path, tracked_files: &HashSet<PathBuf>) -> bool {
    // Direct match
    if tracked_files.contains(path) {
        return true;
    }

    // Check if path is under a tracked directory
    for tracked in tracked_files {
        if path.starts_with(tracked) || tracked.starts_with(path) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_git_tracked() {
        let mut tracked = HashSet::new();
        tracked.insert(PathBuf::from("/project/src/main.rs"));
        tracked.insert(PathBuf::from("/project/Cargo.toml"));

        assert!(is_git_tracked(Path::new("/project/src/main.rs"), &tracked));
        assert!(!is_git_tracked(
            Path::new("/project/node_modules/foo.js"),
            &tracked
        ));
    }
}
