//! Always-on recorder service, one backend per platform:
//! launchd (macOS), Task Scheduler (Windows), systemd user units (Linux).
//!
//! Every backend exposes the same five functions, so the CLI stays
//! platform-agnostic:
//! - `install(binary) -> Result<String>` — register + start; returns a
//!   human-readable description of what was installed
//! - `uninstall() -> Result<()>` — stop + remove
//! - `is_installed() -> bool`, `is_running() -> bool`
//! - `log_hint() -> String` — where the daemon's logs live

#[cfg(target_os = "macos")]
mod launchd;
#[cfg(target_os = "macos")]
pub use launchd::{install, is_installed, is_running, log_hint, uninstall};

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::{install, is_installed, is_running, log_hint, uninstall};

#[cfg(target_os = "linux")]
mod systemd;
#[cfg(target_os = "linux")]
pub use systemd::{install, is_installed, is_running, log_hint, uninstall};

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod unsupported {
    use orangebox_application::{ArchiveError, Result};

    pub fn install(_binary: &str) -> Result<String> {
        Err(ArchiveError::Storage(
            "always-on recording is not supported on this platform yet".into(),
        ))
    }
    pub fn uninstall() -> Result<()> {
        Ok(())
    }
    pub fn is_installed() -> bool {
        false
    }
    pub fn is_running() -> bool {
        false
    }
    pub fn log_hint() -> String {
        "unsupported platform".into()
    }
}
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub use unsupported::{install, is_installed, is_running, log_hint, uninstall};
