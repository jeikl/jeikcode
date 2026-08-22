//! `MemoryHook` — injects (and on resume REFRESHES) the merged `memory.md` entries.
//!
//! Lands as a **synthetic User** in the frozen prefix (after the leading System
//! run, before the first real query). User-owned memory is more important than
//! the persona, so it sits inside `sacred_floor` and compaction cannot drain it.
//! `synthetic = true` so the floor still anchors on the real task prompt.
//!
//! RESUME: reconcile the snapshot's copy with the CURRENT `memory.md` — refresh
//! in place, inject if the snapshot has none, or remove if empty. A leftover
//! `Role::System` copy from older sessions is converted to a synthetic user.
//! A resume is a NEW wire session, so this rewrite cannot break the within-session
//! append-only cache red-line; when `memory.md` is unchanged the refresh is
//! byte-identical.

use std::path::Path;

use async_trait::async_trait;
use atomcode_kernel::hook::LifecycleHooks;
use atomcode_kernel::message::Conversation;

use super::MemoryStore;

/// The fixed first line of `MemoryStore::merged_for_prompt` output — how the hook
/// recognizes ITS message in a resumed snapshot. Guarded against drift by a test.
const MEMORY_HEADER: &str = "=== MEMORY ===";

/// Pushes/refreshes `MemoryStore::merged_for_prompt(global, project, …)` as a
/// frozen synthetic-user prefix message at session start; silent when both
/// stores are empty. Read-only and infallible — a missing/unreadable `memory.md`
/// is an empty store, never an error.
pub struct MemoryHook {
    global: MemoryStore,
    project: MemoryStore,
    project_name: String,
}

impl MemoryHook {
    /// The standard wiring: the global `$ATOMCODE_HOME/memory.md` + the project's
    /// `<root>/.atomcode/memory.md`, labeled with the root's directory name (the
    /// literal `"project"` when the root has none — e.g. `/` — same as production).
    pub fn for_project(project_root: &Path) -> Self {
        let project_name = project_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".to_string());
        Self {
            global: MemoryStore::global(),
            project: MemoryStore::project(project_root),
            project_name,
        }
    }

    /// Caller-supplied stores (tests, or a non-standard layout).
    pub fn with_stores(
        global: MemoryStore,
        project: MemoryStore,
        project_name: impl Into<String>,
    ) -> Self {
        Self {
            global,
            project,
            project_name: project_name.into(),
        }
    }
}

#[async_trait]
impl LifecycleHooks for MemoryHook {
    async fn session_start(&self, convo: &mut Conversation, _resumed: bool) {
        let merged =
            MemoryStore::merged_for_prompt(&self.global, &self.project, &self.project_name);
        let block = if merged.is_empty() {
            None
        } else {
            Some(merged)
        };
        convo.reconcile_frozen_user_block(MEMORY_HEADER, block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomcode_kernel::message::{Message, Role};
    use std::path::PathBuf;

    fn stores(dir: &Path, global: &str, project: &str) -> (MemoryStore, MemoryStore) {
        let g = MemoryStore::new(dir.join("memory.md"));
        let p = MemoryStore::new(dir.join("proj").join(".atomcode").join("memory.md"));
        if !global.is_empty() {
            g.append(global).unwrap();
        }
        if !project.is_empty() {
            p.append(project).unwrap();
        }
        (g, p)
    }

    fn hook(dir: &Path, global: &str, project: &str) -> MemoryHook {
        let (g, p) = stores(dir, global, project);
        MemoryHook::with_stores(g, p, "myproj")
    }

    /// Guards `MEMORY_HEADER` against drift from `merged_for_prompt`'s real output.
    #[test]
    fn header_const_matches_store_output() {
        let dir = tempfile::tempdir().unwrap();
        let (g, p) = stores(dir.path(), "x", "");
        let merged = MemoryStore::merged_for_prompt(&g, &p, "n");
        assert!(merged.starts_with(MEMORY_HEADER));
    }

    #[tokio::test]
    async fn fresh_session_injects_merged_memory_after_persona() {
        let dir = tempfile::tempdir().unwrap();
        let h = hook(dir.path(), "prefers tabs", "pnpm only");

        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        h.session_start(&mut convo, false).await;

        assert_eq!(convo.messages.len(), 2);
        let mem = &convo.messages[1];
        assert_eq!(mem.role, Role::User);
        assert!(mem.synthetic);
        assert!(mem.text.starts_with(MEMORY_HEADER));
        assert!(mem.text.contains("[Global]\n- prefers tabs"));
        assert!(mem.text.contains("[Project: myproj]\n- pnpm only"));
    }

    #[tokio::test]
    async fn empty_memory_injects_nothing() {
        let h = MemoryHook::with_stores(
            MemoryStore::new(PathBuf::from("/nonexistent/g.md")),
            MemoryStore::new(PathBuf::from("/nonexistent/p.md")),
            "myproj",
        );
        let mut convo = Conversation::new();
        h.session_start(&mut convo, false).await;
        assert!(convo.messages.is_empty(), "no memory → no message");
    }

    #[tokio::test]
    async fn resume_refreshes_stale_memory_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let h = hook(dir.path(), "old fact", "");

        // The snapshot carries the OLD injected message between persona and history.
        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        convo.push(Message::system(format!(
            "{MEMORY_HEADER}\nstale copy from snapshot"
        )));
        convo.push(Message::user("earlier turn"));

        // memory.md changed since the snapshot (/remember while the session slept).
        h.global.append("new fact").unwrap();
        h.session_start(&mut convo, true).await;

        assert_eq!(
            convo.messages.len(),
            3,
            "refresh replaces in place, no growth"
        );
        let mem = &convo.messages[1];
        assert_eq!(mem.role, Role::User);
        assert!(mem.synthetic, "legacy system memory converts to frozen user");
        assert!(mem.text.contains("- old fact") && mem.text.contains("- new fact"));
        assert!(!mem.text.contains("stale copy"));
        assert_eq!(convo.messages[2].text, "earlier turn", "history untouched");
    }

    #[tokio::test]
    async fn resume_is_byte_identical_when_memory_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let h = hook(dir.path(), "same fact", "");

        let frozen = MemoryStore::merged_for_prompt(&h.global, &h.project, &h.project_name);
        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        convo.push(Message::system(frozen.clone()));
        convo.push(Message::user("earlier turn"));

        h.session_start(&mut convo, true).await;
        assert_eq!(
            convo.messages[1].text, frozen,
            "unchanged memory → identical bytes"
        );
    }

    #[tokio::test]
    async fn resume_injects_when_snapshot_has_no_memory_message() {
        let dir = tempfile::tempdir().unwrap();
        // Session originally started with EMPTY stores → snapshot has no block.
        let h = hook(dir.path(), "", "");
        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        convo.push(Message::user("earlier turn"));

        // /remember happened since.
        h.global.append("learned later").unwrap();
        h.session_start(&mut convo, true).await;

        assert_eq!(convo.messages.len(), 3);
        assert_eq!(
            convo.messages[1].role,
            Role::User,
            "lands after the persona run as a frozen user prefix"
        );
        assert!(convo.messages[1].synthetic);
        assert!(convo.messages[1].text.contains("- learned later"));
        assert_eq!(convo.messages[2].text, "earlier turn");
    }

    #[tokio::test]
    async fn resume_removes_block_when_memory_now_empty() {
        let dir = tempfile::tempdir().unwrap();
        let h = hook(dir.path(), "", ""); // both stores empty NOW

        let mut convo = Conversation::new();
        convo.push(Message::system("persona"));
        convo.push(Message::system(format!(
            "{MEMORY_HEADER}\n- forgotten fact"
        )));
        convo.push(Message::user("earlier turn"));

        h.session_start(&mut convo, true).await;

        assert_eq!(
            convo.messages.len(),
            2,
            "/forget-emptied memory drops the block"
        );
        assert!(!convo
            .messages
            .iter()
            .any(|m| m.text.starts_with(MEMORY_HEADER)));
    }

    #[test]
    fn for_project_labels_with_directory_name() {
        let h = MemoryHook::for_project(Path::new("/tmp/some/repo-name"));
        assert_eq!(h.project_name, "repo-name");
        // Env-neutral: the default `.atomcode` dir is covered deterministically by
        // `store::project_memory_path_resolves_override`. Asserting the literal `.atomcode`
        // here would couple this test to `ATOMCODE_PROJECT_MEMORY_DIR` being unset, and
        // mutating the process env (remove_var) would race sibling tests that read it.
        assert!(h.project.path().ends_with("memory.md"));
    }

    #[test]
    fn for_project_root_without_name_falls_back_to_project_like_production() {
        let h = MemoryHook::for_project(Path::new("/"));
        assert_eq!(h.project_name, "project");
    }
}
