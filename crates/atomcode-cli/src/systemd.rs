//! Linux systemd service generation, environment capture, and interactive wizard.
//!
//! Provides automated creation and management of systemd services for `--host` serve mode,
//! solving the common issue where systemd services lack the user's interactive login shell
//! environment (such as nvm, node, bun, cargo, go, pyenv in PATH).
//!
//! Fully cross-platform compatible (Windows / macOS / Linux / HarmonyOS):
//! - Only activates the systemd wizard on Linux with an active systemd runtime and interactive TTY.
//! - The wizard runs **before** the foreground listener binds, using line-buffered
//!   stdin (no crossterm raw mode). Prompting after bind+banner races job-control
//!   (`SIGTTOU`/`SIGTTIN` → shell `[Stopped]`) and leaves the port occupied.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use anyhow::{anyhow, Context, Result};
use is_terminal::IsTerminal;

/// Options required to render a systemd service unit.
#[derive(Debug, Clone)]
pub struct SystemdServiceOpts {
    pub service_name: String,
    pub host: String,
    pub port: u16,
    pub workdir: PathBuf,
    pub no_token: bool,
    pub fixed_token: Option<String>,
    pub yolo: bool,
    pub no_telemetry: bool,
}

/// Captured environment from the current user session.
#[derive(Debug, Clone)]
pub struct CapturedEnv {
    pub user: String,
    pub home: String,
    pub shell: String,
    pub lang: String,
    pub path: String,
}

/// Check whether systemd is active and available on this host.
pub fn is_systemd_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new("/run/systemd/system").exists()
            || std::process::Command::new("systemctl")
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Resolves the absolute path to the executable to put into systemd `ExecStart`.
/// If running from a build target, checks if an installed binary (`/usr/local/bin/jeikcode`
/// or `/usr/local/bin/atomcode` or `~/.local/bin/jeikcode`) exists.
pub fn resolve_service_exe() -> PathBuf {
    let current = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("jeikcode"));

    // If current executable is already in a standard system location, use it
    let current_str = current.to_string_lossy();
    if current_str.starts_with("/usr/") || current_str.starts_with("/bin/") || current_str.contains("/.local/bin/") {
        return current;
    }

    // Otherwise, check for standard install paths
    for candidate in [
        "/usr/local/bin/jeikcode",
        "/usr/local/bin/atomcode",
        "/usr/bin/jeikcode",
        "/usr/bin/atomcode",
    ] {
        let p = Path::new(candidate);
        if p.exists() {
            return p.to_path_buf();
        }
    }

    if let Some(home) = dirs::home_dir() {
        let local_jeikcode = home.join(".local/bin/jeikcode");
        if local_jeikcode.exists() {
            return local_jeikcode;
        }
        let local_atomcode = home.join(".local/bin/atomcode");
        if local_atomcode.exists() {
            return local_atomcode;
        }
    }

    current
}

/// Capture the current user's complete environment, enriching PATH with
/// common toolchain directories (nvm/node, cargo, bun, go, .local/bin) if present.
pub fn capture_current_environment() -> CapturedEnv {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "root".to_string());

    let home = dirs::home_dir()
        .map(|h| h.display().to_string())
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| "/root".to_string());

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let lang = std::env::var("LANG").unwrap_or_else(|_| "en_US.UTF-8".to_string());

    // Build comprehensive PATH: existing PATH + any installed developer toolchains in $HOME
    let mut paths: Vec<PathBuf> = Vec::new();

    if let Some(existing) = std::env::var_os("PATH") {
        for p in std::env::split_paths(&existing) {
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
    }

    if let Some(home_path) = dirs::home_dir() {
        let mut candidates = vec![
            home_path.join(".local/bin"),
            home_path.join("bin"),
            home_path.join(".cargo/bin"),
            home_path.join(".grok/bin"),
            home_path.join(".bun/bin"),
            home_path.join("go/bin"),
        ];

        let nvm_node = home_path.join(".nvm/versions/node");
        if let Ok(entries) = std::fs::read_dir(&nvm_node) {
            for entry in entries.flatten() {
                let bin_dir = entry.path().join("bin");
                if bin_dir.is_dir() {
                    candidates.push(bin_dir);
                }
            }
        }
        candidates.push(home_path.join(".nvm/current/bin"));
        candidates.push(home_path.join(".fnm/current/bin"));
        candidates.push(home_path.join(".volta/bin"));

        for c in candidates {
            if c.is_dir() && !paths.contains(&c) {
                paths.push(c);
            }
        }
    }

    for sys in [
        "/usr/local/bin",
        "/usr/local/sbin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        let pb = PathBuf::from(sys);
        if pb.is_dir() && !paths.contains(&pb) {
            paths.push(pb);
        }
    }

    let joined_path = std::env::join_paths(paths)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|_| std::env::var("PATH").unwrap_or_default());

    CapturedEnv {
        user,
        home,
        shell,
        lang,
        path: joined_path,
    }
}

/// Render the systemd service unit file content.
pub fn render_systemd_unit(opts: &SystemdServiceOpts, env: &CapturedEnv) -> String {
    let exe = resolve_service_exe();
    let mut args = vec![
        format!("--host {}", opts.host),
        format!("--port {}", opts.port),
    ];

    if opts.no_token {
        args.push("--no-token".to_string());
    } else if let Some(ref tok) = opts.fixed_token {
        if !tok.is_empty() {
            args.push(format!("--token {}", tok));
        }
    }

    if opts.yolo {
        args.push("--yolo".to_string());
    }
    if opts.no_telemetry {
        args.push("--no-telemetry".to_string());
    }

    let workdir_str = opts.workdir.display().to_string();
    args.push(format!("--dir \"{}\"", workdir_str));

    let exec_start = format!("{} {}", exe.display(), args.join(" "));

    format!(
        r#"[Unit]
Description=JeikCode AI Coding Agent Service ({service_name})
After=network.target network-online.target
Wants=network-online.target

[Service]
Type=simple
User={user}
WorkingDirectory={workdir}
ExecStart={exec_start}
Restart=always
RestartSec=5
Environment=HOME={home}
Environment=SHELL={shell}
Environment=LANG={lang}
Environment=PATH={path}

[Install]
WantedBy=multi-user.target
"#,
        service_name = opts.service_name,
        user = env.user,
        workdir = workdir_str,
        exec_start = exec_start,
        home = env.home,
        shell = env.shell,
        lang = env.lang,
        path = env.path,
    )
}

#[cfg(unix)]
fn run_systemctl(args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("systemctl")
        .args(args)
        .output();

    match output {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("Access denied") || stderr.contains("interactive authentication") || stderr.contains("permission") {
                // Try sudo
                let sudo_out = std::process::Command::new("sudo")
                    .arg("systemctl")
                    .args(args)
                    .output()
                    .with_context(|| format!("failed to run sudo systemctl {}", args.join(" ")))?;
                if sudo_out.status.success() {
                    return Ok(());
                }
                let sudo_err = String::from_utf8_lossy(&sudo_out.stderr);
                return Err(anyhow!("sudo systemctl {} failed: {}", args.join(" "), sudo_err.trim()));
            }
            Err(anyhow!("systemctl {} failed: {}", args.join(" "), stderr.trim()))
        }
        Err(e) => {
            // Try sudo directly
            let sudo_out = std::process::Command::new("sudo")
                .arg("systemctl")
                .args(args)
                .output()
                .with_context(|| format!("failed to run sudo systemctl {}: {e}", args.join(" ")))?;
            if sudo_out.status.success() {
                return Ok(());
            }
            let sudo_err = String::from_utf8_lossy(&sudo_out.stderr);
            Err(anyhow!("sudo systemctl {} failed: {}", args.join(" "), sudo_err.trim()))
        }
    }
}

#[cfg(not(unix))]
#[allow(dead_code)]
fn run_systemctl(_args: &[&str]) -> Result<()> {
    Err(anyhow!("systemctl is only supported on Linux"))
}

/// Install the service unit file into `/etc/systemd/system/{service_name}.service` and enable/start it.
pub fn install_and_start_systemd_service(service_name: &str, unit_content: &str) -> Result<()> {
    #[cfg(not(unix))]
    {
        let _ = (service_name, unit_content);
        return Err(anyhow!("systemd services are only supported on Linux"));
    }

    #[cfg(unix)]
    {
        let service_file_name = format!("{}.service", service_name);
        let target_path = PathBuf::from("/etc/systemd/system").join(&service_file_name);

        // Write file directly or via sudo
        let write_res = std::fs::write(&target_path, unit_content);
        if let Err(e) = write_res {
            // Fallback: write to temp and sudo cp
            let tmp_path = std::env::temp_dir().join(format!(".{}.service", service_name));
            std::fs::write(&tmp_path, unit_content)
                .with_context(|| format!("failed to write temporary service file at {}", tmp_path.display()))?;

            let cp_res = std::process::Command::new("sudo")
                .args(["cp", tmp_path.to_str().unwrap(), target_path.to_str().unwrap()])
                .output();

            let _ = std::fs::remove_file(&tmp_path);

            match cp_res {
                Ok(out) if out.status.success() => {}
                Ok(out) => {
                    let err = String::from_utf8_lossy(&out.stderr);
                    return Err(anyhow!("sudo cp service file to {} failed: {}", target_path.display(), err.trim()));
                }
                Err(sudo_err) => {
                    return Err(anyhow!("could not write {} (direct error: {e}, sudo error: {sudo_err})", target_path.display()));
                }
            }
        }

        // Set 0644 permissions on the service file
        let _ = std::process::Command::new("sudo")
            .args(["chmod", "644", target_path.to_str().unwrap()])
            .output();

        // Reload systemd daemon
        run_systemctl(&["daemon-reload"])
            .context("reloading systemd daemon")?;

        // Enable service on boot
        run_systemctl(&["enable", &service_file_name])
            .context("enabling systemd service")?;

        // Restart/start the service now
        run_systemctl(&["restart", &service_file_name])
            .context("starting systemd service")?;

        Ok(())
    }
}

/// Check if the service is currently active.
pub fn is_service_active(service_name: &str) -> bool {
    #[cfg(unix)]
    {
        let service_file_name = format!("{}.service", service_name);
        let out = std::process::Command::new("systemctl")
            .args(["is-active", &service_file_name])
            .output();

        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).trim() == "active",
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = service_name;
        false
    }
}

fn ignore_tty_job_control_stops() {
    #[cfg(unix)]
    unsafe {
        // tcsetattr / stdin read from a process that is not the foreground
        // group otherwise delivers SIGTTOU/SIGTTIN and the shell reports
        // `[Stopped]`, leaving the listen port occupied.
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        libc::signal(libc::SIGTTIN, libc::SIG_IGN);
    }
}

fn drain_pending_stdin() {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = io::stdin().as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 {
                return;
            }
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            let mut buf = [0u8; 256];
            while libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) > 0 {}
            libc::fcntl(fd, libc::F_SETFL, flags);
        }
    }
}

fn read_tty_line(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    io::stderr().flush().ok();
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|e| anyhow!("read tty: {e}"))?;
    Ok(line.trim().to_string())
}

/// Interactive wizard in `--host` serve mode. Call **before** binding the
/// foreground listener so raw-mode / job-control never races the server.
///
/// Prompts:
/// 1. "是否配置为 Linux 系统服务并开机自启？ [y/N]"
/// 2. If yes: service name (empty → `jeikcode-{port}`)
/// 3. Installs and starts the systemd unit, prints `banner`, returns `Ok(true)`
///    so the caller exits instead of serving in the foreground.
pub fn prompt_systemd_setup(
    host: &str,
    port: u16,
    workdir: &Path,
    no_token: bool,
    fixed_token: Option<&str>,
    display_token: Option<&str>,
    yolo: bool,
    no_telemetry: bool,
    banner: &str,
) -> Result<bool> {
    if !cfg!(target_os = "linux") || !io::stdin().is_terminal() || !is_systemd_available() {
        return Ok(false);
    }

    ignore_tty_job_control_stops();
    drain_pending_stdin();

    let default_service_name = format!("jeikcode-{}", port);
    eprintln!();
    eprintln!("JeikCode 即将在 {host}:{port} 启动。");
    let answer = read_tty_line("是否配置为 Linux 系统服务并开机自启？ [y/N]: ")?;
    let user_agreed = matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes");
    if !user_agreed {
        eprintln!("✓ 保持前台运行模式 (按 Ctrl+C 可停止服务)\n");
        return Ok(false);
    }

    let service_input = read_tty_line(&format!(
        "请输入系统服务名 (直接回车默认: {default_service_name}): "
    ))?;
    let service_name = if service_input.is_empty() {
        default_service_name
    } else {
        service_input
    };

    println!("==> 正在捕获当前环境并生成服务配置...");
    let env = capture_current_environment();
    let unit_token = if no_token {
        None
    } else {
        fixed_token
            .filter(|s| !s.is_empty())
            .or_else(|| display_token.filter(|s| !s.is_empty()))
            .map(str::to_string)
    };

    let opts = SystemdServiceOpts {
        service_name: service_name.clone(),
        host: host.to_string(),
        port,
        workdir: workdir.to_path_buf(),
        no_token,
        fixed_token: unit_token,
        yolo,
        no_telemetry,
    };

    let unit_content = render_systemd_unit(&opts, &env);

    println!("==> 正在安装并启动系统服务 [{}]...", service_name);

    if let Err(e) = install_and_start_systemd_service(&service_name, &unit_content) {
        eprintln!("\n❌ 安装系统服务失败: {e:#}");
        eprintln!("您可以手动创建 `/etc/systemd/system/{}.service` 并执行 systemctl start。", service_name);
        return Err(e);
    }

    let active = is_service_active(&service_name);

    // Print final retained output summary
    println!("\n========================================================================");
    if active {
        println!("✨ 系统服务 [{}] 配置成功并已在后台运行 (开机自启已就绪)！", service_name);
    } else {
        println!("⚠️ 系统服务 [{}] 已创建并已尝试启动，请检查状态。", service_name);
    }
    println!("------------------------------------------------------------------------");
    // Print the connection banner and URLs so the user doesn't lose them upon exit
    print!("{banner}");
    if !banner.ends_with('\n') {
        println!();
    }
    println!("------------------------------------------------------------------------");
    println!("📌 系统服务管理命令 (可随时在终端执行):");
    println!("  查看运行状态: sudo systemctl status {}", service_name);
    println!("  查看实时日志: sudo journalctl -u {} -f", service_name);
    println!("  重启后台服务: sudo systemctl restart {}", service_name);
    println!("  停止后台服务: sudo systemctl stop {}", service_name);
    println!("  禁用开机自启: sudo systemctl disable {}", service_name);
    println!("========================================================================");
    println!("已恢复终端输入态。\n");

    Ok(true)
}
