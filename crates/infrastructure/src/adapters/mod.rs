//! Tool adapters: one module per supported AI tool.
//!
//! Every adapter is read-only over the tool's storage and idempotent.
//! Format knowledge is documented in FORMATS.md at the repo root — update
//! it whenever an adapter learns something new about a tool's storage.

pub mod antigravity;
pub mod claude_code;
