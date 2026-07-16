//! Debounced file watching over the adapters' watch roots.
//!
//! Uses FSEvents on macOS via `notify`. Events are debounced in a small
//! window so a burst of writes to one file triggers a single re-ingest
//! (ingestion is idempotent, so over-triggering is safe, just wasteful).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use orangebox_application::{ArchiveError, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

pub struct FsWatcher {
    // Held so the underlying OS watches stay alive.
    _watcher: RecommendedWatcher,
    rx: mpsc::Receiver<notify::Result<notify::Event>>,
    debounce: Duration,
}

impl FsWatcher {
    pub fn watch(roots: &[PathBuf], debounce: Duration) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(tx)
            .map_err(|e| ArchiveError::Storage(format!("watcher init: {e}")))?;
        for root in roots {
            if root.exists() {
                watcher
                    .watch(root, RecursiveMode::Recursive)
                    .map_err(|e| ArchiveError::Storage(format!("watch {}: {e}", root.display())))?;
                tracing::info!(root = %root.display(), "watching");
            } else {
                tracing::warn!(root = %root.display(), "watch root missing, skipped");
            }
        }
        Ok(Self { _watcher: watcher, rx, debounce })
    }

    /// Block until at least one change arrives, then keep collecting until
    /// the debounce window passes quietly. Returns the deduplicated set of
    /// changed paths, or `None` when the channel closed.
    pub fn next_changed_paths(&self) -> Option<Vec<PathBuf>> {
        let first = self.rx.recv().ok()?;
        let mut paths: HashSet<PathBuf> = HashSet::new();
        collect(first, &mut paths);
        loop {
            match self.rx.recv_timeout(self.debounce) {
                Ok(event) => collect(event, &mut paths),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        Some(paths.into_iter().collect())
    }
}

fn collect(event: notify::Result<notify::Event>, into: &mut HashSet<PathBuf>) {
    if let Ok(event) = event {
        for path in event.paths {
            into.insert(path);
        }
    }
}
