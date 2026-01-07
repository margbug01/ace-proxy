//! Event throttling/debouncing for file change notifications
//! Prevents event storms from overwhelming the backend

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::{debug, info};

/// Throttled event ready to be processed
#[derive(Debug, Clone)]
pub struct ThrottledEvent {
    pub paths: Vec<PathBuf>,
}

/// Event throttler that batches and deduplicates file change events
/// 
/// This is a simple synchronous throttler that collects paths and flushes them
/// when the debounce window expires. The caller is responsible for checking
/// `should_flush()` periodically and calling `flush()` to get batched events.
pub struct EventThrottler {
    /// Pending paths to be processed
    pending_paths: HashSet<PathBuf>,
    /// Last flush time
    last_flush: Instant,
    /// Debounce window duration
    debounce_duration: Duration,
}

impl EventThrottler {
    /// Create a new event throttler with the specified debounce window
    pub fn new(debounce_ms: u64) -> Self {
        Self {
            pending_paths: HashSet::new(),
            last_flush: Instant::now(),
            debounce_duration: Duration::from_millis(debounce_ms),
        }
    }

    /// Add a path to the pending set (duplicates are automatically deduplicated)
    pub fn add_path(&mut self, path: PathBuf) {
        self.pending_paths.insert(path);
        debug!("Throttler: added path, pending count: {}", self.pending_paths.len());
    }

    /// Check if we should flush (debounce window expired and have pending paths)
    pub fn should_flush(&self) -> bool {
        !self.pending_paths.is_empty() 
            && self.last_flush.elapsed() >= self.debounce_duration
    }

    /// Flush pending events and return them
    /// Returns None if there are no pending paths
    pub fn flush(&mut self) -> Option<ThrottledEvent> {
        if self.pending_paths.is_empty() {
            return None;
        }

        let paths: Vec<PathBuf> = self.pending_paths.drain().collect();
        self.last_flush = Instant::now();

        info!("Throttler: flushing {} paths", paths.len());

        Some(ThrottledEvent { paths })
    }

    /// Get the number of pending paths
    pub fn pending_count(&self) -> usize {
        self.pending_paths.len()
    }
    
    /// Clear all pending paths without flushing
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.pending_paths.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_throttler_basic() {
        let mut throttler = EventThrottler::new(100);
        
        throttler.add_path(PathBuf::from("/test/file1.rs"));
        throttler.add_path(PathBuf::from("/test/file2.rs"));
        throttler.add_path(PathBuf::from("/test/file1.rs")); // duplicate
        
        assert_eq!(throttler.pending_count(), 2);
    }
    
    #[test]
    fn test_throttler_flush() {
        let mut throttler = EventThrottler::new(0); // 0ms debounce for immediate flush
        
        throttler.add_path(PathBuf::from("/test/file1.rs"));
        throttler.add_path(PathBuf::from("/test/file2.rs"));
        
        assert!(throttler.should_flush());
        
        let event = throttler.flush();
        assert!(event.is_some());
        assert_eq!(event.unwrap().paths.len(), 2);
        assert_eq!(throttler.pending_count(), 0);
    }
    
    #[test]
    fn test_throttler_empty_flush() {
        let mut throttler = EventThrottler::new(0);
        assert!(!throttler.should_flush());
        assert!(throttler.flush().is_none());
    }
    
    #[test]
    fn test_throttler_debounce_window() {
        let mut throttler = EventThrottler::new(10000); // 10 second debounce
        
        throttler.add_path(PathBuf::from("/test/file1.rs"));
        
        // Should not flush immediately due to debounce window
        assert!(!throttler.should_flush());
    }
}
