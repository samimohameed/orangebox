//! systemd user-unit integration for the always-on recorder (Linux).
//!
//! Writes `~/.config/systemd/user/orangebox.service` and enables it via
//! `systemctl --user`. Logs go to the user journal
//! (`journalctl --user -u orangebox`).

use std::path::PathBuf;
use std::process::Command;

use orangebox_application::{ArchiveError, Result};

pub const UNIT: &str = "orangebox.service";

pub fn unit_path() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| ArchiveError::Storage("cannot resolve home directory".into()))?;
    Ok(home.join(".config/systemd/user").join(UNIT))
}

pub fn render_unit(binary: &str) -> String {
    format!(
        "[Unit]\n\
         Description=Orangebox — flight recorder for AI coding sessions\n\
         \n\
         [Service]\n\
         ExecStart={binary} daemon\n\
         Restart=always\n\
         RestartSec=10\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

fn systemctl(args: &[&str]) -> Result<std::process::Output> {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| ArchiveError::Storage(format!("systemctl: {e}")))
}

pub fn log_hint() -> String {
    format!("journalctl --user -u {UNIT}")
}

pub fn install(binary: &str) -> Result<String> {
    let unit = unit_path()?;
    if let Some(parent) = unit.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ArchiveError::Storage(format!("create {}: {e}", parent.display())))?;
    }
    std::fs::write(&unit, render_unit(binary))
        .map_err(|e| ArchiveError::Storage(format!("write {}: {e}", unit.display())))?;

    let _ = systemctl(&["daemon-reload"]);
    let out = systemctl(&["enable", "--now", UNIT])?;
    if !out.status.success() {
        return Err(ArchiveError::Storage(format!(
            "systemctl enable --now failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(format!("systemd user unit: {}", unit.display()))
}

pub fn uninstall() -> Result<()> {
    let _ = systemctl(&["disable", "--now", UNIT]);
    let unit = unit_path()?;
    if unit.exists() {
        std::fs::remove_file(&unit)
            .map_err(|e| ArchiveError::Storage(format!("remove {}: {e}", unit.display())))?;
    }
    let _ = systemctl(&["daemon-reload"]);
    Ok(())
}

pub fn is_installed() -> bool {
    unit_path().map(|p| p.exists()).unwrap_or(false)
}

pub fn is_running() -> bool {
    systemctl(&["is-active", "--quiet", UNIT])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_contains_daemon_command_and_restart_policy() {
        let unit = render_unit("/usr/local/bin/orangebox");
        assert!(unit.contains("ExecStart=/usr/local/bin/orangebox daemon"));
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("WantedBy=default.target"));
    }
}
