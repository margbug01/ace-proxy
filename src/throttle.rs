//! Event throttling/debouncing for file change notifications
//! Prevents event storms from overwhelming the backend

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, info};

/// Throttled event ready to be processed
#[derive(Debug, Clone)]
pub struct ThrottledEvent {
    pub paths: Vec<PathBuf>,
}

/// Event throttler that batches and deduplicates file change events
pub struct EventThrottler {
    /// Pending paths to be processed
    pending_paths: HashSet<PathBuf>,
    /// Last flush time
    last_flush: Instant,
    /// Debounce window duration
    debounce_duration: Duration,
    /// Channel to send throttled events
    event_tx: mpsc::Sender<ThrottledEvent>,
}

impl EventThrottler {
    /// Create a new event throttler
    pub fn new(debounce_ms: u64) -> (Self, mpsc::Receiver<ThrottledEvent>) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let throttler = Self {
            pending_paths: HashSet::new(),
            last_flush: Instant::now(),
            debounce_duration: Duration::from_millis(debounce_ms),
            event_tx,
        };
        (throttler, event_rx)
    }

    /// Add a path to the pending set
    pub fn add_path(&mut self, path: PathBuf) {
        self.pending_paths.insert(path);
        debug!("Throttler: added path, pending count: {}", self.pending_paths.len());
    }

    /// Check if we should flush (debounce window expired)
    pub fn should_flush(&self) -> bool {
        !self.pending_paths.is_empty() 
            && self.last_flush.elapsed() >= self.debounce_duration
    }

    /// Flush pending events
    pub async fn flush(&mut self) -> Option<ThrottledEvent> {
        if self.pending_paths.is_empty() {
            return None;
        }

        let paths: Vec<PathBuf> = self.pending_paths.drain().collect();
        self.last_flush = Instant::now();

        info!("Throttler: flushing {} paths", paths.len());

        let event = ThrottledEvent { paths };

        // Try to send, but don't block if channel is full
        let _ = self.event_tx.try_send(event.clone());

        Some(event)
    }

    /// Get pending count
    pub fn pending_count(&self) -> usize {
        self.pending_paths.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_throttler_basic() {
        let (mut throttler, _rx) = EventThrottler::new(100);
        
        throttler.add_path(PathBuf::from("/test/file1.rs"));
        throttler.add_path(PathBuf::from("/test/file2.rs"));
        throttler.add_path(PathBuf::from("/test/file1.rs")); // duplicate
        
        assert_eq!(throttler.pending_count(), 2);
    }
}
