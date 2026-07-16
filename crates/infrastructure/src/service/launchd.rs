//! macOS launchd integration: install/uninstall the always-on recorder.
//!
//! `orangebox install` writes a LaunchAgent plist to
//! `~/Library/LaunchAgents` and bootstraps it, so the daemon starts at
//! login and launchd restarts it if it ever dies (`KeepAlive`). Logs go to
//! `~/Library/Logs/orangebox.log`.

use std::path::PathBuf;
use std::process::Command;

use orangebox_application::{ArchiveError, Result};

pub const LABEL: &str = "dev.orangebox.recorder";

pub fn plist_path() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| ArchiveError::Storage("cannot resolve home directory".into()))?;
    Ok(home.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

pub fn log_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join("Library/Logs/orangebox.log")
}

/// The LaunchAgent definition. `binary` is the absolute path of the
/// installed executable; it runs `orangebox daemon`.
pub fn render_plist(binary: &str) -> String {
    let log = log_path();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{binary}</string>
        <string>daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        log = log.display(),
    )
}

fn launchctl(args: &[&str]) -> Result<std::process::Output> {
    Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|e| ArchiveError::Storage(format!("launchctl: {e}")))
}

/// launchctl's modern per-user domain (`gui/<uid>`).
fn gui_domain() -> String {
    let uid = Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "501".into());
    format!("gui/{uid}")
}

/// Where the daemon's logs live.
pub fn log_hint() -> String {
    log_path().display().to_string()
}

/// Write the plist and (re)start the agent.
pub fn install(binary: &str) -> Result<String> {
    let plist = plist_path()?;
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ArchiveError::Storage(format!("create {}: {e}", parent.display())))?;
    }
    std::fs::write(&plist, render_plist(binary))
        .map_err(|e| ArchiveError::Storage(format!("write {}: {e}", plist.display())))?;

    // Re-bootstrap: remove any previous instance first (ignore failures —
    // it may simply not be loaded yet).
    let domain = gui_domain();
    let _ = launchctl(&["bootout", &format!("{domain}/{LABEL}")]);
    let out = launchctl(&["bootstrap", &domain, plist.to_str().unwrap_or_default()])?;
    if !out.status.success() {
        return Err(ArchiveError::Storage(format!(
            "launchctl bootstrap failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(format!("launchd agent: {}", plist.display()))
}

/// Stop the agent and remove the plist.
pub fn uninstall() -> Result<()> {
    let domain = gui_domain();
    let _ = launchctl(&["bootout", &format!("{domain}/{LABEL}")]);
    let plist = plist_path()?;
    if plist.exists() {
        std::fs::remove_file(&plist)
            .map_err(|e| ArchiveError::Storage(format!("remove {}: {e}", plist.display())))?;
    }
    Ok(())
}

/// Whether launchd currently has the agent loaded and running.
pub fn is_running() -> bool {
    let domain = gui_domain();
    match launchctl(&["print", &format!("{domain}/{LABEL}")]) {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).contains("state = running")
        }
        _ => false,
    }
}

/// Whether the agent is installed (plist present), regardless of state.
pub fn is_installed() -> bool {
    plist_path().map(|p| p.exists()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_contains_label_binary_and_keepalive() {
        let plist = render_plist("/usr/local/bin/orangebox");
        assert!(plist.contains(LABEL));
        assert!(plist.contains("<string>/usr/local/bin/orangebox</string>"));
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<true/>"));
    }
}
