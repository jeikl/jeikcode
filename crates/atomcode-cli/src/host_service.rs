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

use anyhow::{bail, Context, Result};
use is_terminal::IsTerminal;
use std::io;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
use std::io::Write;
use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

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
    let stem = name.trim_end_matches(".service").trim_end_matches(".plist");
    let suffix = stem.rsplit(|c: char| c == '-' || c == '.').next()?;
    suffix.parse().ok()
}

/// Inputs shared by the Linux systemd, macOS launchd, and Windows Task
/// Scheduler setup wizards. The caller must invoke the wizard before binding
/// the foreground listener so no service output can obscure the prompt.
pub struct HostServiceSetupOptions<'a> {
    pub host: &'a str,
    pub port: u16,
    pub workdir: &'a Path,
    pub no_token: bool,
    pub fixed_token: Option<&'a str>,
    pub display_token: Option<&'a str>,
    pub yolo: bool,
    pub no_telemetry: bool,
    pub banner: &'a str,
}

/// Prompt to install the current `--host` invocation as the native persistent
/// service for this platform. `Ok(true)` means the background service was
/// installed and the foreground process should exit.
pub fn prompt_host_service_setup(opts: &HostServiceSetupOptions<'_>) -> Result<bool> {
    if !io::stdin().is_terminal() {
        return Ok(false);
    }

    #[cfg(target_os = "linux")]
    {
        if !crate::systemd::is_systemd_available() {
            return Ok(false);
        }
        return crate::systemd::prompt_systemd_setup(
            opts.host,
            opts.port,
            opts.workdir,
            opts.no_token,
            opts.fixed_token,
            opts.display_token,
            opts.yolo,
            opts.no_telemetry,
            opts.banner,
        );
    }

    #[cfg(target_os = "macos")]
    {
        return prompt_launchd_setup(opts);
    }

    #[cfg(target_os = "windows")]
    {
        return prompt_schtasks_setup(opts);
    }

    #[allow(unreachable_code)]
    Ok(false)
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn read_prompt_line(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    io::stderr().flush().ok();
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .context("reading host service setup answer")?;
    Ok(line.trim().to_string())
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn user_confirmed(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn persistent_token(opts: &HostServiceSetupOptions<'_>) -> Option<String> {
    if opts.no_token {
        None
    } else {
        opts.fixed_token
            .filter(|token| !token.trim().is_empty())
            .or_else(|| opts.display_token.filter(|token| !token.trim().is_empty()))
            .map(str::to_string)
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn serve_args(opts: &HostServiceSetupOptions<'_>) -> Vec<String> {
    let mut args = vec![
        "--host".to_string(),
        opts.host.to_string(),
        "--port".to_string(),
        opts.port.to_string(),
        "--dir".to_string(),
        opts.workdir.display().to_string(),
    ];
    if opts.no_token {
        args.push("--no-token".to_string());
    } else if let Some(token) = persistent_token(opts) {
        args.push("--token".to_string());
        args.push(token);
    }
    if opts.yolo {
        args.push("--yolo".to_string());
    }
    if opts.no_telemetry {
        args.push("--no-telemetry".to_string());
    }
    args
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn print_banner(banner: &str) {
    print!("{banner}");
    if !banner.ends_with('\n') {
        println!();
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn valid_service_name(name: &str, required_prefix: &str) -> bool {
    name.len() > required_prefix.len()
        && name
            .get(..required_prefix.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(required_prefix))
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
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
    run_systemctl_quiet(&["daemon-reload"]).context("reloading systemd daemon after uninstall")?;

    Ok(())
}

#[cfg(target_os = "linux")]
fn run_systemctl_quiet(args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("systemctl").args(args).output();
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
                bail!(
                    "sudo systemctl {} failed: {}",
                    args.join(" "),
                    sudo_err.trim()
                );
            }
            bail!("systemctl {} failed: {}", args.join(" "), stderr.trim());
        }
        Err(e) => bail!("failed to run systemctl: {e}"),
    }
}

#[cfg(not(target_os = "linux"))]
fn uninstall_systemd(_service_name: &str) -> Result<()> {
    bail!("systemd is only supported on Linux")
}

// ── macOS (launchd) ───────────────────────────────────────────────────────────

#[cfg(any(target_os = "macos", test))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(any(target_os = "macos", test))]
fn render_launchd_plist(label: &str, executable: &Path, args: &[String], workdir: &Path) -> String {
    let mut program_arguments = format!(
        "    <string>{}</string>\n",
        xml_escape(&executable.display().to_string())
    );
    for arg in args {
        program_arguments.push_str(&format!("    <string>{}</string>\n", xml_escape(arg)));
    }
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
    let home = dirs::home_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{program_arguments}  </array>
  <key>WorkingDirectory</key>
  <string>{workdir}</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HOME</key>
    <string>{home}</string>
    <key>PATH</key>
    <string>{path}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
</dict>
</plist>
"#,
        label = xml_escape(label),
        workdir = xml_escape(&workdir.display().to_string()),
        home = xml_escape(&home),
        path = xml_escape(&path),
    )
}

#[cfg(target_os = "macos")]
fn prompt_launchd_setup(opts: &HostServiceSetupOptions<'_>) -> Result<bool> {
    let default_name = format!("com.jeikcode-{}", opts.port);
    eprintln!();
    eprintln!("JeikCode 即将在 {}:{} 启动。", opts.host, opts.port);
    let answer = read_prompt_line("是否配置为 macOS launchd 服务并在登录后自动运行？ [y/N]: ")?;
    if !user_confirmed(&answer) {
        eprintln!("✓ 保持前台运行模式 (按 Ctrl+C 可停止服务)\n");
        return Ok(false);
    }

    let input = read_prompt_line(&format!("请输入服务名 (直接回车默认: {default_name}): "))?;
    let label = if input.is_empty() {
        default_name
    } else {
        input
    };
    if !valid_service_name(&label, "com.jeikcode-") {
        bail!("launchd 服务名必须以 com.jeikcode- 开头，且只能包含字母、数字、点、横线或下划线")
    }

    let home = dirs::home_dir().context("cannot resolve home directory for LaunchAgents")?;
    let launch_agents = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&launch_agents)
        .with_context(|| format!("creating {}", launch_agents.display()))?;
    let plist_path = launch_agents.join(format!("{label}.plist"));
    let executable = crate::systemd::resolve_service_exe();
    let plist = render_launchd_plist(&label, &executable, &serve_args(opts), opts.workdir);
    std::fs::write(&plist_path, plist)
        .with_context(|| format!("writing {}", plist_path.display()))?;

    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .context("running id -u")?;
    let uid = String::from_utf8_lossy(&uid.stdout).trim().to_string();
    let domain = format!("gui/{uid}");
    let target = format!("{domain}/{label}");
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &target])
        .output();
    let bootstrap = std::process::Command::new("launchctl")
        .arg("bootstrap")
        .arg(&domain)
        .arg(&plist_path)
        .output()
        .context("running launchctl bootstrap")?;
    if !bootstrap.status.success() {
        bail!(
            "launchctl bootstrap failed: {}",
            String::from_utf8_lossy(&bootstrap.stderr).trim()
        );
    }
    let _ = std::process::Command::new("launchctl")
        .args(["kickstart", "-k", &target])
        .output();

    println!("\n========================================================================");
    println!("✨ launchd 服务 [{label}] 配置成功并已在后台运行！");
    println!("------------------------------------------------------------------------");
    print_banner(opts.banner);
    println!("------------------------------------------------------------------------");
    println!("📌 服务管理命令:");
    println!("  查看状态: launchctl print {target}");
    println!("  重启服务: launchctl kickstart -k {target}");
    println!("  卸载服务: jeikcode server uninstall <ID>");
    println!("========================================================================\n");
    Ok(true)
}

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
fn uninstall_launchd(_label: &str) -> Result<()> {
    bail!("launchd is only supported on macOS")
}

// ── Windows (schtasks) ────────────────────────────────────────────────────────

#[cfg(any(target_os = "windows", test))]
fn quote_windows_arg(value: &str) -> String {
    if !value.is_empty() && !value.chars().any(|ch| ch.is_whitespace() || ch == '"') {
        return value.to_string();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0usize;
    for ch in value.chars() {
        if ch == '\\' {
            backslashes += 1;
            continue;
        }
        if ch == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
            quoted.push('"');
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
            quoted.push(ch);
        }
        backslashes = 0;
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(any(target_os = "windows", test))]
fn windows_task_command(executable: &Path, args: &[String]) -> String {
    std::iter::once(executable.display().to_string())
        .chain(args.iter().cloned())
        .map(|arg| quote_windows_arg(&arg))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(target_os = "windows")]
fn prompt_schtasks_setup(opts: &HostServiceSetupOptions<'_>) -> Result<bool> {
    let default_name = format!("JeikCode-{}", opts.port);
    eprintln!();
    eprintln!("JeikCode 即将在 {}:{} 启动。", opts.host, opts.port);
    let answer = read_prompt_line("是否配置为 Windows 计划任务并在登录后自动运行？ [y/N]: ")?;
    if !user_confirmed(&answer) {
        eprintln!("✓ 保持前台运行模式 (按 Ctrl+C 可停止服务)\n");
        return Ok(false);
    }

    let input = read_prompt_line(&format!("请输入任务名 (直接回车默认: {default_name}): "))?;
    let task_name = if input.is_empty() {
        default_name
    } else {
        input
    };
    if !valid_service_name(&task_name, "JeikCode-") {
        bail!("Windows 任务名必须以 JeikCode- 开头，且只能包含字母、数字、点、横线或下划线")
    }

    let executable = crate::systemd::resolve_service_exe();
    let task_command = windows_task_command(&executable, &serve_args(opts));
    println!("==> 正在创建并启动计划任务 [{task_name}]...");
    let create = std::process::Command::new("schtasks")
        .args([
            "/Create",
            "/TN",
            &task_name,
            "/TR",
            &task_command,
            "/SC",
            "ONLOGON",
            "/RL",
            "LIMITED",
            "/F",
        ])
        .output()
        .context("running schtasks /Create")?;
    if !create.status.success() {
        bail!(
            "schtasks /Create failed: {}",
            String::from_utf8_lossy(&create.stderr).trim()
        );
    }
    let start = std::process::Command::new("schtasks")
        .args(["/Run", "/TN", &task_name])
        .output()
        .context("running schtasks /Run")?;
    if !start.status.success() {
        bail!(
            "schtasks /Run failed: {}",
            String::from_utf8_lossy(&start.stderr).trim()
        );
    }

    println!("\n========================================================================");
    println!("✨ Windows 计划任务 [{task_name}] 配置成功并已在后台运行！");
    println!("------------------------------------------------------------------------");
    print_banner(opts.banner);
    println!("------------------------------------------------------------------------");
    println!("📌 服务管理命令:");
    println!("  查看状态: schtasks /Query /TN \"{task_name}\" /V /FO LIST");
    println!("  立即运行: schtasks /Run /TN \"{task_name}\"");
    println!("  停止任务: schtasks /End /TN \"{task_name}\"");
    println!("  卸载服务: jeikcode server uninstall <ID>");
    println!("========================================================================\n");
    Ok(true)
}

#[cfg(target_os = "windows")]
fn scan_schtasks() -> Vec<HostServiceEntry> {
    let mut entries = Vec::new();

    // Query all tasks in a parseable format, filter for JeikCode-*
    let output = std::process::Command::new("schtasks")
        .args(["/Query", "/FO", "CSV", "/NH"])
        .output();

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        for line in stdout.lines() {
            // CSV format: "TaskName","Next Run Time","Status"
            let parts: Vec<&str> = line.split("\",\"").collect();
            if parts.len() < 1 {
                continue;
            }
            let task_name = parts[0]
                .trim_start_matches('"')
                .trim_start_matches('\\')
                .trim();
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
        assert_eq!(parse_port_from_name("jeikcode-4096.service"), Some(4096));
        assert_eq!(parse_port_from_name("com.jeikcode-3000"), Some(3000));
        assert_eq!(parse_port_from_name("com.jeikcode-3000.plist"), Some(3000));
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

    #[test]
    fn confirmation_requires_explicit_yes() {
        assert!(user_confirmed("y"));
        assert!(user_confirmed(" YES "));
        assert!(!user_confirmed(""));
        assert!(!user_confirmed("n"));
    }

    #[test]
    fn service_args_keep_token_and_workdir_as_separate_arguments() {
        let workdir = Path::new(r"C:\work dir\repo");
        let opts = HostServiceSetupOptions {
            host: "0.0.0.0",
            port: 4094,
            workdir,
            no_token: false,
            fixed_token: Some("token with space"),
            display_token: None,
            yolo: true,
            no_telemetry: true,
            banner: "banner",
        };
        assert_eq!(
            serve_args(&opts),
            vec![
                "--host",
                "0.0.0.0",
                "--port",
                "4094",
                "--dir",
                r"C:\work dir\repo",
                "--token",
                "token with space",
                "--yolo",
                "--no-telemetry",
            ]
        );
    }

    #[test]
    fn launchd_plist_escapes_values_and_keeps_program_arguments_distinct() {
        let args = vec!["--dir".into(), "/tmp/a & b".into()];
        let plist = render_launchd_plist(
            "com.jeikcode-4096",
            Path::new("/Applications/Jeik & Code/jeikcode"),
            &args,
            Path::new("/tmp/a & b"),
        );
        assert!(plist.contains("/Applications/Jeik &amp; Code/jeikcode"));
        assert!(plist.contains("<string>--dir</string>"));
        assert!(plist.contains("<string>/tmp/a &amp; b</string>"));
    }

    #[test]
    fn windows_task_command_quotes_paths_and_token_values() {
        let command = windows_task_command(
            Path::new(r"C:\Program Files\JeikCode\jeikcode.exe"),
            &["--token".into(), "a b".into()],
        );
        assert_eq!(
            command,
            r#""C:\Program Files\JeikCode\jeikcode.exe" --token "a b""#
        );
    }

    #[test]
    fn native_service_names_keep_discoverable_prefixes() {
        assert!(valid_service_name("JeikCode-4094", "JeikCode-"));
        assert!(valid_service_name("com.jeikcode-dev", "com.jeikcode-"));
        assert!(!valid_service_name("custom", "JeikCode-"));
        assert!(!valid_service_name("JeikCode-bad/name", "JeikCode-"));
    }
}
