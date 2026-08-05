//! `SessionContextHook` — injects the per-session "context block" (environment + project
//! instructions + domain glossary + git snapshot) as ONE leading `Role::System` message.
//!
//! ## Hot-reload (every user turn)
//!
//! On **each** user message (`turn_start`), env + GLOBAL/PROJECT/USER instructions +
//! DOMAIN GLOSSARY are re-read from disk and the SESSION CONTEXT system message is
//! rewritten in place. Edit `AGENTS.md` / `.atomcode/glossary.md` mid-session and the
//! next send picks them up without restart.
//!
//! - Unchanged files → re-render is byte-identical → prefix cache still holds.
//! - Changed instruction/glossary bytes → prefix cache invalidates for that turn (intended).
//! - **Git section stays frozen** at session-start (or resume) snapshot: live `git status`
//!   drifts every commit and would bust the cache every turn if refreshed.
//!
//! Fresh sessions inject the full block at `session_start`; resume refreshes
//! instructions the same way while preserving the saved git section.
//!
//! The wire adapter coalesces persona + this block + memory into a single system message
//! (commit `3956f9fc`). Identified by [`CONTEXT_HEADER`]. `/cd` is a NEW SESSION.

use super::instructions::render_instructions;
use async_trait::async_trait;
use atomcode_kernel::hook::LifecycleHooks;
use atomcode_kernel::message::{Conversation, Message, Role};
use std::path::PathBuf;

/// First line of the rendered block — how refresh/resume locate it for in-place rewrite.
const CONTEXT_HEADER: &str = "=== SESSION CONTEXT ===";

/// Separator + marker that begins the git sub-section (always the LAST section, joined onto
/// the base with a blank line). On resume / turn hot-reload the saved git bytes — from this
/// marker to the end — are spliced back verbatim so the frozen snapshot survives while
/// env/instructions refresh.
const GIT_SECTION_SEP: &str = "\n\n=== GIT STATUS";

/// Injects environment + project-instructions + git-status context; hot-reloads instruction
/// tiers on every user turn.
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
    /// Appended after project instructions + knowledge packs, before the frozen git section.
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

    /// Render the full context block. Always non-empty (the env sub-section is
    /// unconditional), so the session always carries a context message.
    fn render(&self) -> String {
        match self.git_snapshot() {
            Some(git) => format!("{}\n\n{}", self.render_base(), git),
            None => self.render_base(),
        }
    }

    /// The NON-git portion — header + env + project instructions (+ glossary) + optional
    /// client system append. Re-read from disk on every call (hot-reload).
    fn render_base(&self) -> String {
        let mut out = vec![CONTEXT_HEADER.to_string(), self.env_block()];
        let instr = render_instructions(&self.home, &self.working_dir);
        if !instr.is_empty() {
            out.push(instr);
        }
        if let Some(extra) = &self.extra_append {
            out.push(format!("{CLIENT_SYSTEM_HEADER}\n{extra}"));
        }
        out.join("\n\n")
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

    /// Re-read instruction/glossary files from disk and rewrite the SESSION CONTEXT
    /// system message in place. Git section (if any) is preserved from the saved block.
    ///
    /// Used by resume and by every `turn_start` (hot-reload).
    fn refresh_instructions_in_place(&self, convo: &mut Conversation) {
        // Scope to the leading system run so a later user/assistant echo of the header
        // cannot suppress or overwrite the real block.
        let leading = convo
            .messages
            .iter()
            .take_while(|m| m.role == Role::System)
            .count();
        match convo.messages[..leading]
            .iter()
            .position(|m| m.text.starts_with(CONTEXT_HEADER))
        {
            Some(i) => {
                let saved = &convo.messages[i].text;
                let refreshed = match saved.rfind(GIT_SECTION_SEP) {
                    // Splice frozen git bytes (marker → end) onto a freshly rendered base.
                    // `+ 2` skips the "\n\n" the separator carries so the join isn't doubled.
                    Some(sep) => format!("{}\n\n{}", self.render_base(), &saved[sep + 2..]),
                    None => self.render_base(),
                };
                convo.messages[i] = Message::system(refreshed);
            }
            // Legacy session or missing block — insert after leading system run.
            None => convo
                .messages
                .insert(leading, Message::system(self.render())),
        }
    }
}

#[async_trait]
impl LifecycleHooks for SessionContextHook {
    async fn session_start(&self, convo: &mut Conversation, resumed: bool) {
        if !resumed {
            // FRESH: land right after the leading-system run (persona, and any context hook
            // registered before this one), before the first user message.
            let at = convo
                .messages
                .iter()
                .take_while(|m| m.role == Role::System)
                .count();
            convo.messages.insert(at, Message::system(self.render()));
            return;
        }
        // RESUME: hot-refresh instructions/glossary; freeze saved git section.
        self.refresh_instructions_in_place(convo);
    }

    async fn turn_start(&self, convo: &mut Conversation) {
        // Every user send: re-read GLOBAL / PROJECT / USER / DOMAIN GLOSSARY from disk.
        // Git stays frozen (see module docs).
        self.refresh_instructions_in_place(convo);
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
    async fn fresh_injects_env_after_persona() {
        let d = tempfile::tempdir().unwrap();
        let hook = SessionContextHook::with_home(d.path(), d.path().join("nohome"));
        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        hook.session_start(&mut convo, false).await;
        assert_eq!(convo.messages.len(), 2);
        assert_eq!(convo.messages[0].text, "persona");
        let ctx = &convo.messages[1];
        assert_eq!(ctx.role, Role::System);
        assert!(
            ctx.text.starts_with(CONTEXT_HEADER),
            "block leads with the header"
        );
        assert!(
            ctx.text.contains("Working directory:"),
            "env block always present"
        );
        assert!(ctx.text.contains("Platform:"));
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
        let ctx = &convo.messages[1].text;
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
    }

    #[tokio::test]
    async fn git_section_only_inside_a_repo() {
        // Not a repo → no git section.
        let bare = tempfile::tempdir().unwrap();
        let h1 = SessionContextHook::with_home(bare.path(), bare.path().join("nohome"));
        assert!(
            !h1.render().contains("GIT STATUS"),
            "no git section outside a repo"
        );

        // A repo → git section present.
        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        let h2 = SessionContextHook::with_home(repo.path(), repo.path().join("nohome"));
        assert!(
            h2.render().contains("=== GIT STATUS"),
            "git section inside a repo"
        );
    }

    #[tokio::test]
    async fn project_instructions_are_included() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("AGENTS.md"), "project rule X").unwrap();
        let hook = SessionContextHook::with_home(d.path(), d.path().join("nohome"));
        assert!(hook.render().contains("PROJECT INSTRUCTIONS"));
        assert!(hook.render().contains("project rule X"));
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
    async fn resume_freezes_git_but_refreshes_instructions() {
        // Not a git repo → the live render carries no git section; the ONLY git section is the
        // frozen one already in the saved block.
        let d = tempfile::tempdir().unwrap();
        // The user edited project instructions AFTER the session was saved.
        std::fs::write(d.path().join("AGENTS.md"), "new project rule Z").unwrap();
        let hook = SessionContextHook::with_home(d.path(), d.path().join("nohome"));
        // Saved block: a stale env/base + a frozen git section from an earlier HEAD.
        let saved = format!(
            "{CONTEXT_HEADER}\n\nWorking directory: /old\n\n=== GIT STATUS (snapshot at session start, not live) ===\nHEAD: oldsha frozen commit"
        );
        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        convo.push(Message::system(saved));
        convo.push(Message::user("earlier turn"));
        hook.session_start(&mut convo, true).await;
        assert_eq!(convo.messages.len(), 3, "no growth on resume");
        let block = &convo.messages[1].text;
        assert!(
            block.contains("new project rule Z"),
            "project instructions re-rendered from disk on resume: {block}"
        );
        assert!(
            block.contains("HEAD: oldsha frozen commit"),
            "saved git section frozen (not refreshed): {block}"
        );
        assert!(
            !block.contains("Working directory: /old"),
            "env re-rendered (stale env replaced): {block}"
        );
        assert_eq!(convo.messages[2].text, "earlier turn", "history untouched");
    }

    #[tokio::test]
    async fn resume_git_frozen_across_head_move_keeps_prefix_byte_stable() {
        // The core cache guarantee: even when the repo's HEAD moves between save and resume,
        // the resumed block is BYTE-IDENTICAL to the saved one (git frozen) so the cached
        // prefix survives.
        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        std::fs::write(repo.path().join("a.txt"), "1").unwrap();
        git_commit(repo.path(), "first");
        let hook = SessionContextHook::with_home(repo.path(), repo.path().join("nohome"));
        let saved = hook.render(); // captures HEAD #1
                                   // HEAD moves after the save.
        std::fs::write(repo.path().join("b.txt"), "2").unwrap();
        git_commit(repo.path(), "second");
        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        convo.push(Message::system(saved.clone()));
        convo.push(Message::user("t"));
        hook.session_start(&mut convo, true).await;
        assert_eq!(
            convo.messages[1].text, saved,
            "git frozen across a HEAD move ⇒ block byte-identical ⇒ prefix cache holds"
        );
    }

    #[tokio::test]
    async fn resume_inserts_when_absent() {
        // Snapshot predates the context hook → insert after the leading system run.
        let d = tempfile::tempdir().unwrap();
        let hook = SessionContextHook::with_home(d.path(), d.path().join("nohome"));
        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        convo.push(Message::user("earlier turn"));
        hook.session_start(&mut convo, true).await;
        assert_eq!(convo.messages.len(), 3);
        assert!(
            convo.messages[1].text.starts_with(CONTEXT_HEADER),
            "lands after persona"
        );
        assert_eq!(convo.messages[2].text, "earlier turn");
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
        assert!(convo.messages[1].text.contains("rule-v1"));
        assert!(convo.messages[1].text.contains("term-v1"));

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
        assert_eq!(convo.messages.len(), 3, "no extra messages; in-place rewrite");
    }

    #[tokio::test]
    async fn turn_start_keeps_git_frozen() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("AGENTS.md"), "a").unwrap();
        let hook = SessionContextHook::with_home(d.path(), d.path().join("nohome"));
        let saved = format!(
            "{CONTEXT_HEADER}\n\nWorking directory: /x\n\n=== GIT STATUS (snapshot at session start, not live) ===\nHEAD: frozen-abc"
        );
        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        convo.push(Message::system(saved));
        convo.push(Message::user("hi"));
        hook.turn_start(&mut convo).await;
        assert!(
            convo.messages[1].text.contains("HEAD: frozen-abc"),
            "git must stay frozen across turn hot-reload"
        );
        assert!(convo.messages[1].text.contains("PROJECT INSTRUCTIONS"));
    }
}
