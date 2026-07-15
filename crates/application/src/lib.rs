//! Application layer: ports (traits the outside world implements) and
//! use cases (the operations Blackbox performs).
//!
//! Nothing in this crate knows about SQLite, the filesystem layout of any
//! AI tool, or the CLI. Infrastructure implements the ports; the CLI wires
//! them into the use cases (composition root).

pub mod ports;
pub mod usecases;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("storage error: {0}")]
    Storage(String),
    #[error("adapter error ({tool}): {message}")]
    Adapter { tool: &'static str, message: String },
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

pub type Result<T> = std::result::Result<T, ArchiveError>;
