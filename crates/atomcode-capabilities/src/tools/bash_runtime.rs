//! Live bash registry, user-defined long-job keywords, and idle-decision
//! sentinels. Short commands that print then go silent yield to the model with
//! a `bashid` instead of being killed; `long_bash_keyword_add` / `bash_kill_by_id`
//! act on that id while output keeps streaming on the original tool pane.

use atomcode_kernel::tool::ProgressSink;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio_util::sync::CancellationToken;

/// Marker in bash output / tool result: the process is still running and the
/// model must promote or kill it. WebUI/TUI keep the original pane inflight.
pub const AWAIT_DECISION_MARK: &str = "[bash-await-decision]";
/// Written to the original pane when `bash_kill_by_id` wins.
pub const KILLED_BY_TOOL_MARK: &str = "[task was canceled by bash kill tool]";
/// Written to the original pane when a keyword promote lands on a live task.
pub const PROMOTED_MARK: &str = "[bash promoted to long job]";

pub struct LiveBash {
    pub bashid: String,
    pub command: String,
    pub promoted: AtomicBool,
    pub kill: CancellationToken,
    pub progress: ProgressSink,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static REGISTRY: Mutex<Vec<Arc<LiveBash>>> = Mutex::new(Vec::new());
static LIVE_KEYWORDS: RwLock<Vec<String>> = RwLock::new(Vec::new());
static KEYWORDS_SEEDED: AtomicBool = AtomicBool::new(false);

pub fn new_bashid() -> String {
    let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("b-{n:08x}")
}

pub fn register_live_bash(entry: Arc<LiveBash>) {
    REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(entry);
}

pub fn unregister_live_bash(bashid: &str) {
    REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|e| e.bashid != bashid);
}

pub fn find_live_bash(bashid: &str) -> Option<Arc<LiveBash>> {
    REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|e| e.bashid == bashid)
        .cloned()
}

fn seed_keywords_from_disk() {
    if KEYWORDS_SEEDED.swap(true, Ordering::SeqCst) {
        return;
    }
    let from_disk = crate::tools::bash::resolve_bash_timeout_config().long_bash_command_keyword;
    let mut g = LIVE_KEYWORDS.write().unwrap_or_else(|e| e.into_inner());
    if g.is_empty() {
        *g = from_disk;
    }
}

pub fn live_long_keywords() -> Vec<String> {
    seed_keywords_from_disk();
    let live = LIVE_KEYWORDS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if !live.is_empty() {
        return live;
    }
    crate::tools::bash::resolve_bash_timeout_config().long_bash_command_keyword
}

pub fn set_live_long_keywords(keywords: Vec<String>) {
    KEYWORDS_SEEDED.store(true, Ordering::SeqCst);
    *LIVE_KEYWORDS.write().unwrap_or_else(|e| e.into_inner()) = keywords;
}

pub fn add_live_long_keyword(keyword: &str) -> bool {
    let keyword = keyword.trim();
    if keyword.is_empty() {
        return false;
    }
    seed_keywords_from_disk();
    let mut g = LIVE_KEYWORDS.write().unwrap_or_else(|e| e.into_inner());
    if g.iter().any(|k| k.eq_ignore_ascii_case(keyword)) {
        return false;
    }
    g.push(keyword.to_string());
    true
}

/// Whole-word match (alphanumeric / `_` / `-` / `.`). `a` does not match `cat`.
pub fn command_matches_keyword(command: &str, keyword: &str) -> bool {
    let k = keyword.trim();
    if k.is_empty() {
        return false;
    }
    command
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'))
        .any(|w| !w.is_empty() && w.eq_ignore_ascii_case(k))
}

pub fn command_matches_any_keyword(command: &str, keywords: &[String]) -> bool {
    keywords
        .iter()
        .any(|k| command_matches_keyword(command, k))
}

/// Promote every live bash whose command contains `keyword`. Returns how many
/// were newly promoted (already-promoted entries are skipped).
pub fn promote_matching(keyword: &str) -> usize {
    let snapshot: Vec<Arc<LiveBash>> = REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let mut n = 0;
    for e in snapshot {
        if command_matches_keyword(&e.command, keyword)
            && !e.promoted.swap(true, Ordering::SeqCst)
        {
            e.progress.emit(format!("{PROMOTED_MARK} keyword={keyword}\n"));
            n += 1;
        }
    }
    n
}

pub fn kill_by_id(bashid: &str) -> bool {
    if let Some(e) = find_live_bash(bashid) {
        e.kill.cancel();
        true
    } else {
        false
    }
}

pub fn decision_prompt(bashid: &str, idle_secs: u64, suggested_keyword: &str) -> String {
    format!(
        "{AWAIT_DECISION_MARK}\n\
         bashid: {bashid}\n\
         This command already printed output, then went silent for {idle_secs}s. \
         It is still running in the background and may have entered a long-job phase.\n\
         Decide NOW (do not start a replacement bash):\n\
         - `long_bash_keyword_add` with {{\"bashkeyword\":\"{suggested_keyword}\"}} \
to promote this running task to a long job immediately (output continues in this pane).\n\
         - `bash_kill_by_id` with {{\"bashid\":\"{bashid}\"}} to stop it.\n\
         Output of add/kill is independent; this pane keeps the original command stream."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_is_whole_word() {
        assert!(command_matches_keyword("systemctl status foo", "status"));
        assert!(command_matches_keyword("./ninja -C build", "ninja"));
        assert!(!command_matches_keyword("cat file", "a"));
        assert!(!command_matches_keyword("catch me", "cat"));
        assert!(command_matches_keyword("CAT file", "cat"));
    }
}
