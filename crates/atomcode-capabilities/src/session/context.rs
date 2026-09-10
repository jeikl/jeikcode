//! `SessionContextHook` — injects Block 5 (Authoritative Project Instructions & Knowledge)
//! as a frozen synthetic User message inside `sacred_floor`, and Block 6 (Session Baseline)
//! as an independent leading `Role::System` message.
//!
//! ## Hot-reload (every user turn)
//!
//! On **each** user message (`turn_start`), GLOBAL/PROJECT/USER instructions +
//! DOMAIN GLOSSARY are re-read from disk and Block 5 is reconciled in place.
//! Edit `AGENTS.md` / `.atomcode/glossary.md` mid-session and the next send picks them
//! up without restart.
//!
//! - Unchanged files → re-render is byte-identical → prefix cache still holds.
//! - Changed instruction/glossary bytes → Block 5 invalidates, but earlier blocks (1-4)
//!   remain cached!
//! - **Block 6 (Baseline + Git snapshot) stays 100% frozen** at session-start: live `git status`
//!   drifts every commit and would bust the cache every turn if refreshed.

use super::instructions::render_instructions;
use async_trait::async_trait;
use atomcode_kernel::hook::LifecycleHooks;
use atomcode_kernel::message::{Conversation, Message, Role};
use std::path::PathBuf;

/// Header for Block 5: Authoritative Project Instructions & Knowledge
pub const INSTRUCTIONS_HEADER: &str = super::instructions::INSTRUCTIONS_HEADER;

/// Header for Block 6: Session Baseline (environment + git snapshot)
pub const BASELINE_HEADER: &str = "=== SESSION BASELINE ===";

/// Legacy context header from monolithic format (for backward-compatible resume)
pub const LEGACY_CONTEXT_HEADER: &str = "=== SESSION CONTEXT ===";

/// Separator marker for git sub-section when migrating legacy blocks
#[allow(dead_code)]
const GIT_SECTION_SEP: &str = "\n\n=== GIT STATUS";

/// Header for optional client-supplied system text (OpenAI/Anthropic compat API).
/// Appended after AGENTS / glossary / db packs so it sits at the bottom of the
/// instruction stack without overriding project knowledge.
pub const CLIENT_SYSTEM_HEADER: &str = "=== CLIENT SYSTEM INSTRUCTIONS ===";

pub struct SessionContextHook {
    working_dir: PathBuf,
    /// Config root (`~/.atomcode`) for the GLOBAL instructions tier. Defaults to
    /// [`crate::paths::config_dir`]; the env honors `$ATOMCODE_HOME` there.
    home: PathBuf,
    /// Optional client system prompt (e.g. from OpenAI/Anthropic `messages[].role=system`).
    /// Appended after project instructions + knowledge packs.
    extra_append: Option<String>,
}

impl SessionContextHook {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
            home: crate::paths::config_dir(),
            extra_append: None,
        }
    }

    /// Test/embedder seam: supply an explicit config-root (global-instructions base).
    pub fn with_home(working_dir: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
            home: home.into(),
            extra_append: None,
        }
    }

    /// Append client-supplied system instructions after AGENTS.md / glossary / db packs.
    pub fn with_extra_append(mut self, extra: Option<String>) -> Self {
        self.extra_append = extra.and_then(|s| {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        });
        self
    }

    /// Render Block 5: Authoritative Project Instructions & Knowledge (if any).
    /// Returns None if no instructions or client extras are present.
    pub fn render_instructions_block(&self) -> Option<String> {
        let mut instr = render_instructions(&self.home, &self.working_dir);
        if let Some(extra) = &self.extra_append {
            if instr.is_empty() {
                instr = format!("{INSTRUCTIONS_HEADER}\n\n{CLIENT_SYSTEM_HEADER}\n{extra}");
            } else {
                instr.push_str(&format!("\n\n{CLIENT_SYSTEM_HEADER}\n{extra}"));
            }
        }
        if instr.trim().is_empty() {
            None
        } else {
            Some(instr)
        }
    }

    /// Render Block 6: Session Baseline (CWD + Platform + Shell + Git snapshot).
    pub fn render_baseline(&self) -> String {
        match self.git_snapshot() {
            Some(git) => format!("{BASELINE_HEADER}\n{}\n\n{git}", self.env_block()),
            None => format!("{BASELINE_HEADER}\n{}", self.env_block()),
        }
    }

    /// Helper for legacy callers and tests.
    pub fn render(&self) -> String {
        self.render_baseline()
    }

    fn env_block(&self) -> String {
        // Report the shell the `bash` tool ACTUALLY uses, so the model's env line agrees
        // with the tool description. On Windows that is Git Bash when present, else
        // cmd.exe (NOT `$SHELL`, which the tool ignores) — the old hard-coded "cmd.exe"
        // lied whenever Git Bash was installed, so the model emitted cmd syntax that then
        // ran in bash and broke. See `crate::tools::bash::windows_bash_active`.
        let shell = if cfg!(windows) {
            crate::tools::bash::windows_shell_label(crate::tools::bash::windows_bash_active())
                .to_string()
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "sh".into())
        };
        format!(
            "Working directory: {}\nPlatform: {}\nShell: {}",
            // Forward-slash on Windows so the model's cwd anchor is bash-safe
            // (matches the bash tool's "use forward slashes" guidance).
            crate::pathnorm::to_display(&self.working_dir),
            std::env::consts::OS,
            shell
        )
    }

    /// `Some(block)` when `working_dir` is inside a git work tree, else `None`. A
    /// session-start snapshot (NOT live) — `git status --short` capped at 20 lines.
    fn git_snapshot(&self) -> Option<String> {
        if self.git(&["rev-parse", "--is-inside-work-tree"])?.trim() != "true" {
            return None;
        }
        let branch = self
            .git(&["branch", "--show-current"])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(detached HEAD)".into());
        let head = self
            .git(&["log", "-1", "--format=%h %s"])
            .unwrap_or_default()
            .trim()
            .to_string();
        let raw = self.git(&["status", "--short"]).unwrap_or_default();
        let mut lines: Vec<&str> = raw.lines().collect();
        let status = if lines.len() > 20 {
            let extra = lines.len() - 20;
            lines.truncate(20);
            format!("{}\n... and {extra} more line(s)", lines.join("\n"))
        } else {
            lines.join("\n")
        };
        let status = if status.trim().is_empty() {
            "(working tree clean)".to_string()
        } else {
            status
        };
        Some(format!(
            "=== GIT STATUS (snapshot at session start, not live) ===\n\
             Branch: {branch}\nHEAD: {head}\n{status}\n\
             (This is a session-start snapshot — run `git status` for live state.)"
        ))
    }

    fn git(&self, args: &[&str]) -> Option<String> {
        let mut cmd = std::process::Command::new("git");
        cmd.args(args).current_dir(&self.working_dir);
        // No console-window flash for the session-start git snapshot when run from a
        // console-less daemon (mirrors core's ctx/env); no-op off Windows.
        crate::process_utils::suppress_console_window_sync(&mut cmd);
        let out = cmd.output().ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[async_trait]
impl LifecycleHooks for SessionContextHook {
    async fn session_start(&self, convo: &mut Conversation, _resumed: bool) {
        // Reconcile instructions as frozen synthetic user block (inside sacred_floor),
        // cleaning up any legacy System-role instructions.
        convo.reconcile_system_block(INSTRUCTIONS_HEADER, None);
        convo.reconcile_frozen_user_block(INSTRUCTIONS_HEADER, self.render_instructions_block());

        // Operating environment facts (Platform, Command habit, Working directory, Git branch)
        // are now directly injected into Block 1 (<environment>).
        // Remove/clean up the redundant system baseline block (=== SESSION BASELINE === / === SESSION CONTEXT ===).
        convo.reconcile_system_block(BASELINE_HEADER, None);
        convo.reconcile_system_block(LEGACY_CONTEXT_HEADER, None);
    }

    async fn turn_start(&self, convo: &mut Conversation) {
        // Every user send: re-read instructions & knowledge from disk.
        // Clean up legacy System block if present, and update frozen user block in-place!
        convo.reconcile_system_block(INSTRUCTIONS_HEADER, None);
        convo.reconcile_frozen_user_block(INSTRUCTIONS_HEADER, self.render_instructions_block());
        convo.reconcile_system_block(BASELINE_HEADER, None);
        convo.reconcile_system_block(LEGACY_CONTEXT_HEADER, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_init(dir: &std::path::Path) {
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
        ] {
            std::process::Command::new("git")
                .args(&args)
                .current_dir(dir)
                .output()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn fresh_does_not_inject_redundant_baseline_and_cleans_up_stale() {
        let d = tempfile::tempdir().unwrap();
        let hook = SessionContextHook::with_home(d.path(), d.path().join("nohome"));
        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        convo.push(Message::system(format!(
            "{BASELINE_HEADER}\nWorking directory: /old"
        )));
        hook.session_start(&mut convo, false).await;
        // Redundant baseline is removed because environment facts are in Block 1
        assert_eq!(convo.messages.len(), 1);
        assert_eq!(convo.messages[0].text, "persona");
    }

    #[tokio::test]
    async fn client_system_append_lands_after_agents() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("AGENTS.md"), "project-rule-A").unwrap();
        let hook = SessionContextHook::with_home(d.path(), d.path().join("nohome"))
            .with_extra_append(Some("client-sys-B".into()));
        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        hook.session_start(&mut convo, false).await;
        assert_eq!(convo.messages.len(), 2);
        let ctx = &convo.messages[1].text;
        assert_eq!(convo.messages[1].role, Role::User);
        assert!(convo.messages[1].synthetic);
        let agents_pos = ctx.find("project-rule-A").expect("AGENTS body present");
        let client_pos = ctx
            .find("client-sys-B")
            .expect("client system append present");
        assert!(
            client_pos > agents_pos,
            "client system must follow AGENTS: {ctx}"
        );
        assert!(
            ctx.contains(CLIENT_SYSTEM_HEADER),
            "client system header present: {ctx}"
        );
        assert!(
            ctx.starts_with(INSTRUCTIONS_HEADER),
            "instructions header present: {ctx}"
        );
    }

    #[tokio::test]
    async fn git_section_only_inside_a_repo() {
        // Not a repo → no git section.
        let bare = tempfile::tempdir().unwrap();
        let h1 = SessionContextHook::with_home(bare.path(), bare.path().join("nohome"));
        assert!(
            !h1.render_baseline().contains("GIT STATUS"),
            "no git section outside a repo"
        );

        // A repo → git section present.
        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        let h2 = SessionContextHook::with_home(repo.path(), repo.path().join("nohome"));
        assert!(
            h2.render_baseline().contains("=== GIT STATUS"),
            "git section inside a repo"
        );
    }

    #[tokio::test]
    async fn project_instructions_are_included() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("AGENTS.md"), "project rule X").unwrap();
        let hook = SessionContextHook::with_home(d.path(), d.path().join("nohome"));
        let instr = hook
            .render_instructions_block()
            .expect("instructions rendered");
        assert!(instr.contains("PROJECT INSTRUCTIONS"));
        assert!(instr.contains("project rule X"));
        assert!(instr.starts_with(INSTRUCTIONS_HEADER));
    }

    fn git_commit(dir: &std::path::Path, msg: &str) {
        for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", msg]] {
            std::process::Command::new("git")
                .args(&args)
                .current_dir(dir)
                .output()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn resume_cleans_up_baseline_and_refreshes_instructions() {
        let d = tempfile::tempdir().unwrap();
        // The user edited project instructions AFTER the session was saved.
        std::fs::write(d.path().join("AGENTS.md"), "new project rule Z").unwrap();
        let hook = SessionContextHook::with_home(d.path(), d.path().join("nohome"));
        // Saved block: a frozen baseline from an earlier HEAD.
        let saved_baseline = format!(
            "{BASELINE_HEADER}\nWorking directory: /old\nPlatform: windows\nShell: bash\n\n=== GIT STATUS (snapshot at session start, not live) ===\nHEAD: oldsha frozen commit"
        );
        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        convo.push(Message::system(saved_baseline));
        convo.push(Message::user("earlier turn"));
        hook.session_start(&mut convo, true).await;
        assert_eq!(
            convo.messages.len(),
            3,
            "instructions reconciled into frozen synthetic user run, baseline removed"
        );
        let instr_block = &convo.messages[1].text;
        assert_eq!(convo.messages[1].role, Role::User);
        assert!(convo.messages[1].synthetic);
        assert!(
            instr_block.contains("new project rule Z"),
            "project instructions re-rendered from disk on resume: {instr_block}"
        );
        assert_eq!(convo.messages[2].text, "earlier turn", "history untouched");
    }

    #[tokio::test]
    async fn render_baseline_git_frozen_across_head_move() {
        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        std::fs::write(repo.path().join("a.txt"), "1").unwrap();
        git_commit(repo.path(), "first");
        let hook = SessionContextHook::with_home(repo.path(), repo.path().join("nohome"));
        let saved = hook.render_baseline(); // captures HEAD #1
                                            // HEAD moves after the save.
        std::fs::write(repo.path().join("b.txt"), "2").unwrap();
        git_commit(repo.path(), "second");
        assert!(saved.contains("=== GIT STATUS"));
    }

    #[tokio::test]
    async fn resume_does_not_insert_baseline() {
        let d = tempfile::tempdir().unwrap();
        let hook = SessionContextHook::with_home(d.path(), d.path().join("nohome"));
        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        convo.push(Message::user("earlier turn"));
        hook.session_start(&mut convo, true).await;
        assert_eq!(convo.messages.len(), 2);
        assert_eq!(convo.messages[0].text, "persona");
        assert_eq!(convo.messages[1].text, "earlier turn");
    }

    #[tokio::test]
    async fn resume_migrates_legacy_session_context() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("AGENTS.md"), "project rule legacy").unwrap();
        let hook = SessionContextHook::with_home(d.path(), d.path().join("nohome"));
        let legacy_saved = format!(
            "{LEGACY_CONTEXT_HEADER}\nWorking directory: /legacy\nPlatform: windows\nShell: cmd.exe\n\n=== GIT STATUS (snapshot at session start, not live) ===\nHEAD: legacy-sha"
        );
        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        convo.push(Message::system(legacy_saved));
        convo.push(Message::user("hi"));
        hook.session_start(&mut convo, true).await;
        // Legacy context is removed, instructions extracted into synthetic user block
        assert_eq!(convo.messages.len(), 3);
        assert_eq!(convo.messages[0].text, "persona");
        assert!(convo.messages[1].text.starts_with(INSTRUCTIONS_HEADER));
        assert!(convo.messages[1].text.contains("project rule legacy"));
        assert_eq!(convo.messages[1].role, Role::User);
        assert!(convo.messages[1].synthetic);
        assert_eq!(convo.messages[2].text, "hi");
    }

    #[tokio::test]
    async fn turn_start_hot_reloads_agents_and_glossary() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".atomcode")).unwrap();
        std::fs::write(d.path().join("AGENTS.md"), "rule-v1").unwrap();
        std::fs::write(d.path().join(".atomcode/glossary.md"), "term-v1").unwrap();
        let hook = SessionContextHook::with_home(d.path(), d.path().join("nohome"));

        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        hook.session_start(&mut convo, false).await;
        assert_eq!(convo.messages.len(), 2); // persona, instructions (frozen synthetic user)
        assert!(convo.messages[1].text.contains("rule-v1"));
        assert!(convo.messages[1].text.contains("term-v1"));
        assert_eq!(convo.messages[1].role, Role::User);
        assert!(convo.messages[1].synthetic);

        // Mid-session edits on disk.
        std::fs::write(d.path().join("AGENTS.md"), "rule-v2-hot").unwrap();
        std::fs::write(d.path().join(".atomcode/glossary.md"), "term-v2-hot").unwrap();
        convo.push(Message::user("next turn"));
        hook.turn_start(&mut convo).await;

        let block = &convo.messages[1].text;
        assert!(
            block.contains("rule-v2-hot") && !block.contains("rule-v1"),
            "AGENTS.md hot-reloaded on turn_start: {block}"
        );
        assert!(
            block.contains("term-v2-hot") && !block.contains("term-v1"),
            "glossary hot-reloaded on turn_start: {block}"
        );
        assert_eq!(convo.messages[1].role, Role::User);
        assert!(convo.messages[1].synthetic);
        assert_eq!(
            convo.messages.len(),
            3,
            "no extra messages; in-place rewrite (persona, instructions, user)"
        );
    }

    #[tokio::test]
    async fn turn_start_purges_stale_baseline() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("AGENTS.md"), "a").unwrap();
        let hook = SessionContextHook::with_home(d.path(), d.path().join("nohome"));
        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        hook.session_start(&mut convo, false).await;
        let frozen_baseline = format!(
            "{BASELINE_HEADER}\nWorking directory: /x\nPlatform: windows\nShell: bash\n\n=== GIT STATUS (snapshot at session start, not live) ===\nHEAD: frozen-abc"
        );
        convo.messages.insert(1, Message::system(frozen_baseline));
        convo.push(Message::user("hi"));
        hook.turn_start(&mut convo).await;
        assert_eq!(convo.messages.len(), 3);
        assert_eq!(convo.messages[0].text, "persona");
        assert!(convo.messages[1].text.contains("PROJECT INSTRUCTIONS"));
        assert_eq!(convo.messages[1].role, Role::User);
        assert!(convo.messages[1].synthetic);
        assert_eq!(convo.messages[2].text, "hi");
    }
}
