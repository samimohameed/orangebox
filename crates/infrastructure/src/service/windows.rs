//! Windows Task Scheduler integration for the always-on recorder.
//!
//! `install` registers a logon task that runs the daemon through a tiny
//! WScript shim — launching a console binary directly from Task Scheduler
//! would leave a visible console window open for the whole session; the
//! shim (`WScript.Shell.Run` with window style 0) starts it hidden.
//! Because Task Scheduler doesn't capture output, the daemon is passed
//! `--log-file` and writes its own log.

use std::path::PathBuf;
use std::process::Command;

use orangebox_application::{ArchiveError, Result};

pub const TASK_NAME: &str = "OrangeboxRecorder";

fn app_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_default().join("orangebox")
}

pub fn log_path() -> PathBuf {
    app_dir().join("orangebox.log")
}

pub fn log_hint() -> String {
    log_path().display().to_string()
}

fn shim_path() -> PathBuf {
    app_dir().join("run-daemon.vbs")
}

/// The hidden-window shim. VBScript quoting: quotes double inside a
/// quoted string.
pub fn render_shim(binary: &str, log: &str) -> String {
    format!(
        "CreateObject(\"WScript.Shell\").Run \"\"\"{binary}\"\" daemon --log-file \"\"{log}\"\"\", 0, False\r\n"
    )
}

fn schtasks(args: &[&str]) -> Result<std::process::Output> {
    Command::new("schtasks")
        .args(args)
        .output()
        .map_err(|e| ArchiveError::Storage(format!("schtasks: {e}")))
}

pub fn install(binary: &str) -> Result<String> {
    let dir = app_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| ArchiveError::Storage(format!("create {}: {e}", dir.display())))?;
    let shim = shim_path();
    std::fs::write(&shim, render_shim(binary, &log_path().display().to_string()))
        .map_err(|e| ArchiveError::Storage(format!("write {}: {e}", shim.display())))?;

    let run = format!("wscript.exe \"{}\"", shim.display());
    let out = schtasks(&[
        "/Create", "/F", "/SC", "ONLOGON", "/TN", TASK_NAME, "/TR", &run,
    ])?;
    if !out.status.success() {
        return Err(ArchiveError::Storage(format!(
            "schtasks /Create failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    // Start it now rather than waiting for the next logon.
    let _ = schtasks(&["/Run", "/TN", TASK_NAME]);
    Ok(format!("scheduled task \"{TASK_NAME}\" (runs at logon)"))
}

pub fn uninstall() -> Result<()> {
    let _ = schtasks(&["/End", "/TN", TASK_NAME]);
    let out = schtasks(&["/Delete", "/F", "/TN", TASK_NAME])?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        // Deleting a task that doesn't exist is fine.
        if !err.contains("cannot find") {
            return Err(ArchiveError::Storage(format!(
                "schtasks /Delete failed: {}",
                err.trim()
            )));
        }
    }
    let shim = shim_path();
    if shim.exists() {
        let _ = std::fs::remove_file(shim);
    }
    Ok(())
}

pub fn is_installed() -> bool {
    schtasks(&["/Query", "/TN", TASK_NAME])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn is_running() -> bool {
    match schtasks(&["/Query", "/TN", TASK_NAME, "/FO", "LIST", "/V"]) {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).contains("Running")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_quotes_binary_and_log_paths() {
        let shim = render_shim(r"C:\Users\x\.cargo\bin\orangebox.exe", r"C:\Users\x\log.log");
        assert!(shim.contains(r#""""C:\Users\x\.cargo\bin\orangebox.exe"" daemon"#));
        assert!(shim.contains(r#"--log-file ""C:\Users\x\log.log"""#));
        assert!(shim.contains(", 0, False"));
    }
}
