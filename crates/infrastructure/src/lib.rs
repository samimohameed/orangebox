//! Infrastructure layer: SQLite archive, tool adapters, and file watching.

pub mod adapters;
pub mod proto;
pub mod sqlite;
pub mod watcher;

use std::path::PathBuf;

/// Default location of Blackbox's own archive database.
pub fn default_db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("blackbox")
        .join("archive.db")
}
