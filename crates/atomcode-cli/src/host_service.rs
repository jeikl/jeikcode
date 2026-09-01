//! Cross-platform host service scanning and uninstall for `--host` serve mode.
//!
//! Each platform has its own service mechanism:
//! - Linux: systemd units in `/etc/systemd/system/jeikcode-*.service`
//! - macOS: launchd plists in `~/Library/LaunchAgents/com.jeikcode-*.plist`
//! - Windows: schtasks entries named `JeikCode-*`
//!
//! Service naming convention:
//! - Linux:   `jeikcode-{port}`
//! - macOS:   `com.jeikcode-{port}`
//! - Windows: `JeikCode-{port}`

use std::path::PathBuf;
use anyhow::{bail, Context, Result};

// ── Public types ──────────────────────────────────────────────────────────────

/// A discovered host service entry.
#[derive(Debug, Clone)]
pub struct HostServiceEntry {
    /// Display index (1-based, assigned by `list_services`).
    pub id: u32,
    /// Platform-native service name (e.g. `jeikcode-4096.service`).
    pub service_name: String,
    /// The port this service listens on.
    pub port: u16,
    /// Current status.
    pub status: ServiceStatus,
    /// Platform identifier: `"systemd"`, `"launchd"`, or `"schtasks"`.
    pub platform: &'static str,
    /// Filesystem path to the unit file (if applicable).
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceStatus {
    Running,
    Stopped,
    Loaded,
    Unknown,
}

impl std::fmt::Display for ServiceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Stopped => write!(f, "stopped"),
            Self::Loaded => write!(f, "loaded"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Scan the current platform for all JeikCode host services and return them
/// with 1-based display IDs.
pub fn list_services() -> Vec<HostServiceEntry> {
    let mut entries = Vec::new();

    #[cfg(target_os = "linux")]
    entries.extend(scan_systemd());

    #[cfg(target_os = "macos")]
    entries.extend(scan_launchd());

    #[cfg(target_os = "windows")]
    entries.extend(scan_schtasks());

    // Assign 1-based IDs
    for (i, entry) in entries.iter_mut().enumerate() {
        entry.id = (i + 1) as u32;
    }

    entries
}

/// Uninstall a host service by its platform-native name.
pub fn uninstall_service(entry: &HostServiceEntry) -> Result<()> {
    match entry.platform {
        "systemd" => uninstall_systemd(&entry.service_name),
        "launchd" => uninstall_launchd(&entry.service_name),
        "schtasks" => uninstall_schtasks(&entry.service_name),
        _ => bail!("unsupported platform: {}", entry.platform),
    }
}

/// Parse the port number from a JeikCode service name.
/// Accepts: `jeikcode-4096`, `com.jeikcode-3000`, `JeikCode-5000`
fn parse_port_from_name(name: &str) -> Option<u16> {
    // Find the last occurrence of a digit sequence at the end (possibly after a dot or dash)
    let suffix = name.rsplit(|c: char| c == '-' || c == '.').next()?;
    suffix.parse().ok()
}

// ── Linux (systemd) ───────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn scan_systemd() -> Vec<HostServiceEntry> {
    let dir = PathBuf::from("/etc/systemd/system");
    let mut entries = Vec::new();

    if let Ok(read_dir) = std::fs::read_dir(&dir) {
        for entry in read_dir.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            // Match jeikcode-*.service (but NOT atomcode-schedule-*.service)
            if file_name.starts_with("jeikcode-") && file_name.ends_with(".service") {
                let port = parse_port_from_name(&file_name).unwrap_or(0);
                let path = entry.path();
                let status = check_systemd_active(&file_name);
                entries.push(HostServiceEntry {
                    id: 0, // assigned later
                    service_name: file_name.replace(".service", ""),
                    port,
                    status,
                    platform: "systemd",
                    path: Some(path.display().to_string()),
                });
            }
        }
    }

    entries
}

#[cfg(target_os = "linux")]
fn check_systemd_active(service_file: &str) -> ServiceStatus {
    let output = std::process::Command::new("systemctl")
        .args(["is-active", service_file])
        .output();
    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if stdout == "active" {
                ServiceStatus::Running
            } else if stdout == "inactive" || stdout == "failed" {
                ServiceStatus::Stopped
            } else {
                ServiceStatus::Unknown
            }
        }
        Err(_) => ServiceStatus::Unknown,
    }
}

#[cfg(target_os = "linux")]
fn uninstall_systemd(service_name: &str) -> Result<()> {
    let unit_file = format!("{}.service", service_name);
    let target_path = PathBuf::from("/etc/systemd/system").join(&unit_file);

    // Stop the service (best-effort)
    let _ = run_systemctl_quiet(&["stop", &unit_file]);

    // Disable on boot (best-effort)
    let _ = run_systemctl_quiet(&["disable", &unit_file]);

    // Remove the unit file
    if target_path.exists() {
        if let Err(e) = std::fs::remove_file(&target_path) {
            // Try sudo
            let sudo_out = std::process::Command::new("sudo")
                .args(["rm", target_path.to_str().unwrap_or_default()])
                .output();
            if sudo_out.map(|o| !o.status.success()).unwrap_or(true) {
                bail!("failed to remove {}: {e}", target_path.display());
            }
        }
    }

    // Reload systemd daemon
    run_systemctl_quiet(&["daemon-reload"])
        .context("reloading systemd daemon after uninstall")?;

    Ok(())
}

#[cfg(target_os = "linux")]
fn run_systemctl_quiet(args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("systemctl")
        .args(args)
        .output();
    match output {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("Access denied") || stderr.contains("permission") {
                // Try sudo
                let sudo_out = std::process::Command::new("sudo")
                    .arg("systemctl")
                    .args(args)
                    .output()?;
                if sudo_out.status.success() {
                    return Ok(());
                }
                let sudo_err = String::from_utf8_lossy(&sudo_out.stderr);
                bail!("sudo systemctl {} failed: {}", args.join(" "), sudo_err.trim());
            }
            bail!("systemctl {} failed: {}", args.join(" "), stderr.trim());
        }
        Err(e) => bail!("failed to run systemctl: {e}"),
    }
}

#[cfg(not(target_os = "linux"))]
fn scan_systemd() -> Vec<HostServiceEntry> {
    Vec::new()
}

// ── macOS (launchd) ───────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn scan_launchd() -> Vec<HostServiceEntry> {
    let mut entries = Vec::new();

    if let Some(home) = dirs::home_dir() {
        let launch_agents = home.join("Library/LaunchAgents");
        if let Ok(read_dir) = std::fs::read_dir(&launch_agents) {
            for entry in read_dir.flatten() {
                let file_name = entry.file_name().to_string_lossy().to_string();
                // Match com.jeikcode-*.plist
                if file_name.starts_with("com.jeikcode-") && file_name.ends_with(".plist") {
                    let port = parse_port_from_name(&file_name).unwrap_or(0);
                    let path = entry.path();
                    let label = file_name.replace(".plist", "");
                    let status = check_launchd_active(&label);
                    entries.push(HostServiceEntry {
                        id: 0,
                        service_name: label,
                        port,
                        status,
                        platform: "launchd",
                        path: Some(path.display().to_string()),
                    });
                }
            }
        }
    }

    entries
}

#[cfg(target_os = "macos")]
fn check_launchd_active(label: &str) -> ServiceStatus {
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .and_then(|o| Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()))
        .unwrap_or_else(|_| "0".to_string());

    let target = format!("gui/{}/{}", uid, label);
    let output = std::process::Command::new("launchctl")
        .args(["print", &target])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if stdout.contains("state = running") {
                ServiceStatus::Running
            } else {
                ServiceStatus::Loaded
            }
        }
        _ => ServiceStatus::Unknown,
    }
}

#[cfg(target_os = "macos")]
fn uninstall_launchd(label: &str) -> Result<()> {
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .and_then(|o| Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()))
        .unwrap_or_else(|_| "0".to_string());

    let target = format!("gui/{}/{}", uid, label);

    // Best-effort bootout (unload the service)
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &target])
        .output();

    // Remove the plist file
    if let Some(home) = dirs::home_dir() {
        let plist_path = home
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", label));
        if plist_path.exists() {
            std::fs::remove_file(&plist_path)
                .with_context(|| format!("removing {}", plist_path.display()))?;
        }
    }

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn scan_launchd() -> Vec<HostServiceEntry> {
    Vec::new()
}

#[cfg(not(target_os = "macos"))]
fn uninstall_launchd(_label: &str) -> Result<()> {
    bail!("launchd is only supported on macOS")
}

// ── Windows (schtasks) ────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn scan_schtasks() -> Vec<HostServiceEntry> {
    let mut entries = Vec::new();

    // Query all tasks in a parseable format, filter for JeikCode-*
    let output = std::process::Command::new("schtasks")
        .args(["/Query", "/FO", "CSV", "/NH"])
        .output();

    if let Ok(out) = out {
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        for line in stdout.lines() {
            // CSV format: "TaskName","Next Run Time","Status"
            let parts: Vec<&str> = line.split("\",\"").collect();
            if parts.len() < 1 {
                continue;
            }
            let task_name = parts[0].trim_start_matches('"').trim();
            // Match JeikCode-* (case-insensitive)
            if task_name.to_lowercase().starts_with("jeikcode-") {
                let port = parse_port_from_name(task_name).unwrap_or(0);
                let status = if parts.len() >= 3 {
                    let s = parts[2].trim_end_matches('"').trim().to_lowercase();
                    if s.contains("running") || s.contains("ready") {
                        ServiceStatus::Running
                    } else if s.contains("disabled") {
                        ServiceStatus::Stopped
                    } else {
                        ServiceStatus::Unknown
                    }
                } else {
                    ServiceStatus::Unknown
                };
                entries.push(HostServiceEntry {
                    id: 0,
                    service_name: task_name.to_string(),
                    port,
                    status,
                    platform: "schtasks",
                    path: None,
                });
            }
        }
    }

    entries
}

#[cfg(target_os = "windows")]
fn uninstall_schtasks(task_name: &str) -> Result<()> {
    let output = std::process::Command::new("schtasks")
        .args(["/Delete", "/TN", task_name, "/F"])
        .output()
        .with_context(|| format!("running schtasks /Delete for {task_name}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("schtasks /Delete failed for {task_name}: {}", stderr.trim());
    }

    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn scan_schtasks() -> Vec<HostServiceEntry> {
    Vec::new()
}

#[cfg(not(target_os = "windows"))]
fn uninstall_schtasks(_task_name: &str) -> Result<()> {
    bail!("schtasks is only supported on Windows")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_port_from_service_name() {
        assert_eq!(parse_port_from_name("jeikcode-4096"), Some(4096));
        assert_eq!(parse_port_from_name("com.jeikcode-3000"), Some(3000));
        assert_eq!(parse_port_from_name("JeikCode-5000"), Some(5000));
        assert_eq!(parse_port_from_name("atomcode-schedule-foo"), None);
        assert_eq!(parse_port_from_name("random-service"), None);
    }

    #[test]
    fn list_returns_vec_with_1based_ids() {
        let entries = list_services();
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.id, (i + 1) as u32);
        }
    }

    #[test]
    fn uninstall_unsupported_platform_returns_error() {
        let entry = HostServiceEntry {
            id: 1,
            service_name: "test".into(),
            port: 9999,
            status: ServiceStatus::Unknown,
            platform: "fake",
            path: None,
        };
        assert!(uninstall_service(&entry).is_err());
    }
}
