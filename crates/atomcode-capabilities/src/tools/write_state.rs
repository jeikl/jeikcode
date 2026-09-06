//! In-memory per-turn file state machine for `write_file`.
//!
//! Enforces safety discipline for overwriting existing files:
//! 1. When a target file does NOT exist on disk: `write_file` directly creates it (bypassing the state machine).
//! 2. When a target file DOES exist on disk:
//!    - Requires that before the first `write_file` in the current turn, the last file operation on that file was `read_file`.
//!    - Once confirmed read in a turn, the file can be repeatedly overwritten until the turn ends.
//!    - At the start of the next turn (`turn_start` or `advance_turn`), the read confirmation resets to 0 (unread).
//! 3. If an existing file is unread:
//!    - Intercepts `write_file` (refuses to mutate the file).
//!    - Informs the model to read and confirm before writing.
//!    - Directly returns the file's latest 1500 lines formatted with line anchors.
//!    - If total lines > 1500, informs how many lines remain and instructs reading the remainder via offset/limit.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use async_trait::async_trait;
use atomcode_kernel::hook::LifecycleHooks;
use atomcode_kernel::message::Conversation;

const DEFAULT_READ_LIMIT: usize = 1500;
const MAX_LINE_LEN: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOpKind {
    Read,
    Write,
    Edit,
}

#[derive(Debug, Clone, Default)]
pub struct FileTurnState {
    /// True if confirmed read in this turn and persists until this turn ends.
    pub read_confirmed: bool,
    /// Last file operation performed on this file in this turn.
    pub last_op: Option<FileOpKind>,
}

#[derive(Debug)]
struct WriteStateMachine {
    /// Workspace-specific turn counters for multi-session / parallel test isolation
    workspace_turns: HashMap<PathBuf, u64>,
    /// Global fallback turn counter
    global_turn: u64,
    /// Files: canonical_path -> (turn_id_when_recorded, FileTurnState)
    files: HashMap<PathBuf, (u64, FileTurnState)>,
}

impl Default for WriteStateMachine {
    fn default() -> Self {
        Self {
            workspace_turns: HashMap::new(),
            global_turn: 1,
            files: HashMap::new(),
        }
    }
}

static STATE_MACHINE: LazyLock<Mutex<WriteStateMachine>> = LazyLock::new(|| {
    Mutex::new(WriteStateMachine::default())
});

fn canon(path: &Path) -> PathBuf {
    crate::pathnorm::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn get_turn_for_path(p: &Path, state: &WriteStateMachine) -> u64 {
    let mut matched_turn = None;
    let mut longest_len = 0;
    for (ws, turn) in &state.workspace_turns {
        if p.starts_with(ws) && ws.as_os_str().len() > longest_len {
            longest_len = ws.as_os_str().len();
            matched_turn = Some(*turn);
        }
    }
    matched_turn.unwrap_or(state.global_turn)
}

/// Advance the turn counter for a specific workspace directory.
pub fn advance_turn_for_dir(dir: &Path) {
    let d = canon(dir);
    let mut lock = STATE_MACHINE.lock().unwrap();
    let entry = lock.workspace_turns.entry(d).or_insert(1);
    *entry += 1;
}

/// Advance the global turn counter (and all registered workspaces), resetting all read states to unread.
pub fn advance_turn() {
    let mut lock = STATE_MACHINE.lock().unwrap();
    lock.global_turn += 1;
    for turn in lock.workspace_turns.values_mut() {
        *turn += 1;
    }
}

/// Reset all state (useful for unit test isolation).
pub fn reset_all() {
    let mut lock = STATE_MACHINE.lock().unwrap();
    lock.files.clear();
    lock.workspace_turns.clear();
    lock.global_turn = 1;
}

/// Record a successful `read_file` on `path`.
pub fn record_read(path: &Path) {
    let p = canon(path);
    let mut lock = STATE_MACHINE.lock().unwrap();
    let turn = get_turn_for_path(&p, &lock);
    let entry = lock.files.entry(p).or_insert((turn, FileTurnState::default()));
    if entry.0 != turn {
        *entry = (turn, FileTurnState {
            read_confirmed: false,
            last_op: Some(FileOpKind::Read),
        });
    } else {
        entry.1.last_op = Some(FileOpKind::Read);
    }
}

/// Record a successful `edit_file` on `path`.
pub fn record_edit(path: &Path) {
    let p = canon(path);
    let mut lock = STATE_MACHINE.lock().unwrap();
    let turn = get_turn_for_path(&p, &lock);
    let entry = lock.files.entry(p).or_insert((turn, FileTurnState::default()));
    if entry.0 != turn {
        *entry = (turn, FileTurnState {
            read_confirmed: false,
            last_op: Some(FileOpKind::Edit),
        });
    } else {
        entry.1.last_op = Some(FileOpKind::Edit);
    }
}

/// Record a successful `write_file` on `path`.
pub fn record_write_success(path: &Path) {
    let p = canon(path);
    let mut lock = STATE_MACHINE.lock().unwrap();
    let turn = get_turn_for_path(&p, &lock);
    let entry = lock.files.entry(p).or_insert((turn, FileTurnState::default()));
    entry.0 = turn;
    entry.1.read_confirmed = true;
    entry.1.last_op = Some(FileOpKind::Write);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritePermission {
    Allowed,
    RefusedUnread,
}

/// Check whether `write_file` is permitted on `path`.
/// Returns `WritePermission::Allowed` if:
/// - Target file does NOT exist (new file creation)
/// - Target file exists and has been confirmed read in this turn (either prior write succeeded, or last_op is Read)
pub fn check_write_permitted(path: &Path) -> WritePermission {
    if !path.exists() {
        return WritePermission::Allowed;
    }
    let p = canon(path);
    let mut lock = STATE_MACHINE.lock().unwrap();
    let turn = get_turn_for_path(&p, &lock);
    let entry = lock.files.entry(p).or_insert((turn, FileTurnState::default()));
    if entry.0 != turn {
        // Reset for new turn
        *entry = (turn, FileTurnState::default());
        return WritePermission::RefusedUnread;
    }

    if entry.1.read_confirmed {
        // Persists until this turn ends
        return WritePermission::Allowed;
    }

    if entry.1.last_op == Some(FileOpKind::Read) {
        // First write in this turn, and the last operation before write was read_file!
        entry.1.read_confirmed = true;
        return WritePermission::Allowed;
    }

    WritePermission::RefusedUnread
}

/// Build the unread refusal message returning up to 1500 lines of the target file.
pub async fn build_unread_refusal_message(path: &Path, display_path: &str) -> String {
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(e) => {
            return format!(
                "❌ **[写入拦截：目标文件未读取确认]**\n\
                 欲写入的文件已存在（`{display_path}`），但处于【未读取状态】。\n\
                 请读取确认欲写入文件内容后再写入！（无法读取磁盘内容: {e}）"
            );
        }
    };

    if super::looks_binary(&bytes) {
        return format!(
            "❌ **[写入拦截：目标文件未读取确认]**\n\
             欲写入的文件已存在（`{display_path}`，二进制文件 {} 字节），且处于【未读取状态】。\n\
             请读取确认欲写入文件内容后再写入！",
            bytes.len()
        );
    }

    let text: std::borrow::Cow<str> = match std::str::from_utf8(&bytes) {
        Ok(s) => std::borrow::Cow::Borrowed(s),
        Err(_) => match crate::tools::encoding::decode_non_utf8_text(path, &bytes) {
            Some(s) => std::borrow::Cow::Owned(s),
            None => {
                return format!(
                    "❌ **[写入拦截：目标文件未读取确认]**\n\
                     欲写入的文件已存在（`{display_path}`，无法解析为文本），且处于【未读取状态】。\n\
                     请读取确认欲写入文件内容后再写入！"
                );
            }
        },
    };

    let total = text.lines().count();
    let mut rendered_lines = String::new();
    let display_count = total.min(DEFAULT_READ_LIMIT);

    for (i, line) in text.lines().take(display_count).enumerate() {
        let n = i + 1;
        let is_anchor = i == 0 || n % 10 == 0;
        let rendered = if line.chars().count() > MAX_LINE_LEN {
            let head: String = line.chars().take(MAX_LINE_LEN).collect();
            if is_anchor {
                format!("{n}→{head}... (line truncated to {MAX_LINE_LEN} chars)\n")
            } else {
                format!("{head}... (line truncated to {MAX_LINE_LEN} chars)\n")
            }
        } else if is_anchor {
            format!("{n}→{line}\n")
        } else {
            format!("{line}\n")
        };
        rendered_lines.push_str(&rendered);
    }

    let footer = if total > DEFAULT_READ_LIMIT {
        let remaining = total - DEFAULT_READ_LIMIT;
        format!(
            "[Showing lines 1-{display_count} of {total}. {remaining} lines remaining.]\n\
             ⚠️ **提示**：目标文件共有 {total} 行，当前已截断显示前 {display_count} 行，尚有 {remaining} 行未读取。\
             请全部读取完（使用 `read_file` 配合 offset={}, limit=1500）确认后再写入！",
            DEFAULT_READ_LIMIT + 1
        )
    } else {
        format!(
            "[Showing lines 1-{total} of {total} (全量内容)]\n\
             提示：以上为目标文件的全量内容（共 {total} 行）。请读取确认欲写入文件内容后再写入。"
        )
    };

    format!(
        "❌ **[写入拦截：目标文件未读取确认]**\n\
         欲写入的文件已存在（`{display_path}`），且当前处于【未读取状态】。\n\
         根据安全规范：在覆盖写入已存在的文件之前，必须先使用 `read_file` 读取确认该文件的当前内容！请读取确认欲写入文件内容后再写入。\n\n\
         已直接为你返回目标文件最新前 {display_count} 行内容：\n\
         ```\n\
         {rendered_lines}```\n\
         {footer}"
    )
}

/// Lifecycle hook that advances the turn counter on each turn start,
/// resetting all file read confirmation states.
#[derive(Clone, Default, Debug)]
pub struct WriteStateHook {
    working_dir: Option<PathBuf>,
}

impl WriteStateHook {
    pub fn new() -> Self {
        Self { working_dir: None }
    }

    pub fn with_dir(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: Some(working_dir.into()),
        }
    }
}

#[async_trait]
impl LifecycleHooks for WriteStateHook {
    async fn turn_start(&self, _convo: &mut Conversation) {
        if let Some(dir) = &self.working_dir {
            advance_turn_for_dir(dir);
        } else {
            advance_turn();
        }
    }
}
