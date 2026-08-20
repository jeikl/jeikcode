//! `todowrite` — session task list. STATELESS execute: the tool validates + echoes;
//! current state is DERIVED by folding transcript `todowrite`/`todo` calls
//! ([`reduce_todos`]). Preferred shape is `actions[]` (or a single `action`);
//! `{"todos":[…]}` is resume/legacy full-list replace only. Non-destructive ⇒ `Safe`.

use super::{err, ok};
use async_trait::async_trait;
use atomcode_kernel::message::Message;
use atomcode_kernel::tool::{Tool, ToolContext, ToolRegistry, ToolResult};
use serde::Deserialize;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    fn parse(s: &str) -> Option<TodoStatus> {
        match s {
            "pending" => Some(TodoStatus::Pending),
            "in_progress" => Some(TodoStatus::InProgress),
            "completed" => Some(TodoStatus::Completed),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

/// Terminal-safe status glyph. Unicode `[•]`/`[✓]` gated behind `unicode`; ASCII
/// `[~]`/`[x]` fallback (mirrors the spinner / hint-marker unicode gating).
pub fn todo_glyph(status: TodoStatus, unicode: bool) -> &'static str {
    match (status, unicode) {
        (TodoStatus::Pending, _) => "[ ]",
        (TodoStatus::InProgress, true) => "[\u{2022}]",
        (TodoStatus::InProgress, false) => "[~]",
        (TodoStatus::Completed, true) => "[\u{2713}]",
        (TodoStatus::Completed, false) => "[x]",
    }
}

/// One line per item: `<glyph> <content>`. Empty list → "(no tasks)".
pub fn render_todos_text(todos: &[TodoItem], unicode: bool) -> String {
    if todos.is_empty() {
        return "(no tasks)".to_string();
    }
    todos
        .iter()
        .map(|t| format!("{} {}", todo_glyph(t.status, unicode), t.content))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One line per item WITH its 1-based id: `<glyph> <id>. <content>`. This is the shape the
/// TUI title cache (`parse_todo_titles_into`) reads to learn `id → title`, so an incremental
/// `todo update id=N` row can render the task NAME, not just `#N`. Empty list → "(no tasks)".
pub fn render_todos_numbered(todos: &[TodoItem], unicode: bool) -> String {
    if todos.is_empty() {
        return "(no tasks)".to_string();
    }
    todos
        .iter()
        .enumerate()
        .map(|(i, t)| format!("{} {}. {}", todo_glyph(t.status, unicode), i + 1, t.content))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Deserialize)]
struct RawItem {
    content: String,
    status: String,
}

#[derive(Deserialize)]
struct Args {
    todos: Vec<RawItem>,
}

/// Parse + validate the full-list args. Enforces: valid status enum, non-empty
/// content, at most one `in_progress`. Returns a human-readable reason on failure
/// (fed back to the model so it can correct and resend).
pub fn parse_todos(args: &str) -> Result<Vec<TodoItem>, String> {
    let mut value: serde_json::Value = serde_json::from_str(args)
        .map_err(|e| format!("todowrite: invalid arguments: {e}. Expected {{\"todos\":[{{\"content\":\"…\",\"status\":\"pending|in_progress|completed\"}}]}}."))?;
    // Keep transcript-derived state aligned with RepairToolArgsMiddleware. The
    // kernel stores the model's original tool call before middleware rewriting,
    // so a provider that emits `{"todos":"[...]"}` must be tolerated here too:
    // live execution, TodoHook, replay, and resume all converge on parse_todos.
    // Decode exactly one layer and only for `todos`, whose public schema is an
    // array; malformed, double-stringified, or wrong-container values still fail.
    if let Some(raw) = value.get("todos").and_then(serde_json::Value::as_str) {
        if let Ok(decoded) = serde_json::from_str::<serde_json::Value>(raw) {
            if decoded.is_array() {
                value["todos"] = decoded;
            }
        }
    }
    let a: Args = serde_json::from_value(value)
        .map_err(|e| format!("todowrite: invalid arguments: {e}. Expected {{\"todos\":[{{\"content\":\"…\",\"status\":\"pending|in_progress|completed\"}}]}}."))?;
    let mut out = Vec::with_capacity(a.todos.len());
    let mut in_progress = 0usize;
    for item in a.todos {
        if item.content.trim().is_empty() {
            return Err("todowrite: every task needs non-empty `content`.".to_string());
        }
        let status = TodoStatus::parse(&item.status).ok_or_else(|| {
            format!(
                "todowrite: `status` must be one of pending|in_progress|completed (got `{}`).",
                item.status
            )
        })?;
        if status == TodoStatus::InProgress {
            in_progress += 1;
        }
        out.push(TodoItem {
            content: item.content,
            status,
        });
    }
    if in_progress > 1 {
        return Err("todowrite: keep exactly ONE task `in_progress` at a time.".to_string());
    }
    Ok(out)
}

/// `(completed, in_progress, total)` for a todo list. The footer progress
/// indicator renders the counts in its header. `total == 0` ⇒ caller omits
/// the indicator (no todos yet). All three counts are derived from a single
/// pass so callers don't need separate `filter` scans.
pub fn todo_counts(todos: &[TodoItem]) -> (usize, usize, usize) {
    let mut completed = 0;
    let mut in_progress = 0;
    for t in todos {
        match t.status {
            TodoStatus::Completed => completed += 1,
            TodoStatus::InProgress => in_progress += 1,
            TodoStatus::Pending => {}
        }
    }
    (completed, in_progress, todos.len())
}

/// Apply ONE incremental `todo` call (`action` or `actions[]`) to `list`.
/// `id` is the 1-based position: `add` appends (existing ids stay), `update`
/// patches in place, `insert`/`delete`/`remove` shift later ids. Malformed /
/// unknown-id / illegal-mix calls are IGNORED (the tool already returned an
/// error; derived state must stay consistent). `update` to `in_progress` first
/// clears any OTHER in_progress, so the "exactly one in_progress" invariant
/// holds regardless of the model.
pub fn apply_todo_action(list: &mut Vec<TodoItem>, args: &str) {
    let _ = try_apply_todo_args(list, args);
}

/// Outcome of applying one todowrite payload. `Unchanged` is a successful no-op
/// (e.g. update to the status the item already has) — not an error, so the model
/// should not retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    Changed,
    Unchanged,
}

/// Apply `action` / `actions[]`. `todos` full-list replace is NOT handled here
/// (that is a fold baseline / execute plan path). Malformed JSON is `Err`.
pub fn try_apply_todo_args(list: &mut Vec<TodoItem>, args: &str) -> Result<ApplyOutcome, String> {
    let v = serde_json::from_str::<serde_json::Value>(args)
        .map_err(|e| format!("todowrite: invalid JSON arguments: {e}"))?;
    if let Some(arr) = v.get("actions").and_then(|a| a.as_array()) {
        if arr.is_empty() {
            return Err("todowrite: `actions` must be a non-empty array.".into());
        }
        let next = apply_actions_batch(list, arr)?;
        if next == *list {
            return Ok(ApplyOutcome::Unchanged);
        }
        *list = next;
        return Ok(ApplyOutcome::Changed);
    }
    if v.get("todos").is_some() {
        return Ok(ApplyOutcome::Unchanged);
    }
    let before = list.clone();
    try_apply_one_action(list, &v)?;
    if *list == before {
        Ok(ApplyOutcome::Unchanged)
    } else {
        Ok(ApplyOutcome::Changed)
    }
}

fn action_kind(v: &serde_json::Value) -> Option<&'static str> {
    match v.get("action").and_then(|a| a.as_str()) {
        Some("add") => Some("add"),
        Some("insert") => Some("insert"),
        Some("update") => Some("update"),
        Some("delete") | Some("remove") => Some("delete"),
        Some("clear") => Some("clear"),
        _ => None,
    }
}

/// Legal `actions` mixes (id-shifting ops cannot share a batch with a different kind):
/// - `add` + `update` (add only appends; existing ids stay)
/// - `clear` + `add` + `update` (`clear` always runs first, then add, then update)
/// - `insert` + `update` (internally: all inserts first, then updates by post-insert id)
/// - `delete` only (any order; ids are pre-batch)
/// - `clear` only / `update` only / `add` only
fn validate_actions_mix(arr: &[serde_json::Value]) -> Result<(), String> {
    let mut kinds = std::collections::BTreeSet::new();
    for (i, item) in arr.iter().enumerate() {
        match action_kind(item) {
            Some(k) => {
                kinds.insert(k);
            }
            None => {
                return Err(format!(
                    "todowrite: actions[{i}] has unknown or missing `action`."
                ));
            }
        }
    }
    let has = |k: &str| kinds.contains(k);
    if has("delete") && kinds.iter().any(|k| *k != "delete") {
        return Err(
            "todowrite: `delete` can only be batched with other deletes (ids would shift)."
                .into(),
        );
    }
    if has("insert") && kinds.iter().any(|k| *k != "insert" && *k != "update") {
        return Err(
            "todowrite: `insert` can only be batched with other inserts and/or updates."
                .into(),
        );
    }
    if has("clear") && kinds.iter().any(|k| *k != "clear" && *k != "add" && *k != "update") {
        return Err(
            "todowrite: `clear` can only be batched with `add` and/or `update` (`clear` runs first)."
                .into(),
        );
    }
    Ok(())
}

/// A non-empty list whose every item is `completed` is a closed plan.
/// The next `add` starts a new plan at id 1 instead of appending as 7,8,9…
/// (models often skip `clear` when pivoting). An in-progress / pending list
/// is left alone — that is still the current task.
fn maybe_auto_clear_finished(list: &mut Vec<TodoItem>) {
    if !list.is_empty() && list.iter().all(|t| t.status == TodoStatus::Completed) {
        list.clear();
    }
}

/// Batch apply. Id-shifting work runs first; `update` always sees the list
/// *after* those shifts so the model can address the post-change numbering.
fn apply_actions_batch(
    list: &[TodoItem],
    arr: &[serde_json::Value],
) -> Result<Vec<TodoItem>, String> {
    validate_actions_mix(arr)?;
    let mut tmp = list.to_vec();
    let kinds: std::collections::BTreeSet<&str> =
        arr.iter().filter_map(action_kind).collect();

    if kinds.contains("clear") {
        tmp.clear();
    }

    if kinds.contains("delete") {
        let mut delete_ids: Vec<usize> = Vec::new();
        for item in arr {
            let id = json_id(item)
                .ok_or_else(|| "todowrite: `delete` needs an `id`.".to_string())?;
            if id == 0 || (id as usize) > tmp.len() {
                return Err(format!("todowrite: unknown task id {id}."));
            }
            let id = id as usize;
            if !delete_ids.contains(&id) {
                delete_ids.push(id);
            }
        }
        delete_ids.sort_unstable_by(|a, b| b.cmp(a));
        for id in delete_ids {
            tmp.remove(id - 1);
        }
        return Ok(tmp);
    }

    // Adds first: append-only, existing 1..n stay put. A finished list (every
    // item completed) is treated as a closed plan — auto-clear so a new batch
    // of adds restarts at id 1 instead of appending as 7,8,9…
    if arr.iter().any(|item| action_kind(item) == Some("add")) {
        maybe_auto_clear_finished(&mut tmp);
    }
    for item in arr {
        if action_kind(item) == Some("add") {
            try_apply_one_action(&mut tmp, item)?;
        }
    }

    // Inserts next (id-shifting), so later updates use post-insert ids.
    // `position` is the 1-based slot on the list *before this batch's inserts*
    // (after any adds). Apply high→low so two original positions don't scramble.
    let mut inserts: Vec<&serde_json::Value> = arr
        .iter()
        .filter(|a| action_kind(a) == Some("insert"))
        .collect();
    inserts.sort_by(|a, b| {
        let pa = insert_position(a).unwrap_or(usize::MAX);
        let pb = insert_position(b).unwrap_or(usize::MAX);
        pb.cmp(&pa)
    });
    for item in inserts {
        try_apply_one_action(&mut tmp, item)?;
    }

    // Updates last, against the list after add/insert. Array order does not
    // matter except when two updates set `in_progress` (later one wins).
    for item in arr {
        if action_kind(item) == Some("update") {
            try_apply_one_action(&mut tmp, item)?;
        }
    }
    Ok(tmp)
}

fn json_u64(x: &serde_json::Value) -> Option<u64> {
    x.as_u64()
        .or_else(|| x.as_i64().and_then(|i| u64::try_from(i).ok()))
        .or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))
}

fn json_id(v: &serde_json::Value) -> Option<u64> {
    v.get("id").and_then(json_u64)
}

fn insert_position(v: &serde_json::Value) -> Option<usize> {
    v.get("position")
        .or_else(|| v.get("id"))
        .and_then(json_u64)
        .map(|p| p as usize)
        .or_else(|| {
            v.get("after")
                .or_else(|| v.get("after_id"))
                .and_then(json_u64)
                .map(|p| (p + 1) as usize)
        })
}

/// Apply a single action object. `id` is the 1-based position **at this step**
/// (left-to-right in a batch). Returns Err on schema / unknown-id so a batch
/// can roll back.
fn try_apply_one_action(list: &mut Vec<TodoItem>, v: &serde_json::Value) -> Result<(), String> {
    match v.get("action").and_then(|a| a.as_str()) {
        Some("add") => {
            if let Some(content) = v.get("content").and_then(|c| c.as_str()) {
                let content = content.trim();
                if !content.is_empty() {
                    maybe_auto_clear_finished(list);
                    list.push(TodoItem {
                        content: content.to_string(),
                        status: TodoStatus::Pending,
                    });
                    return Ok(());
                }
            }
            Err("todowrite: `add` needs non-empty `content`.".into())
        }
        Some("insert") => {
            if let Some(content) = v.get("content").and_then(|c| c.as_str()) {
                let content = content.trim();
                if !content.is_empty() {
                    let status = v
                        .get("status")
                        .and_then(|s| s.as_str())
                        .and_then(TodoStatus::parse)
                        .unwrap_or(TodoStatus::Pending);
                    if status == TodoStatus::InProgress {
                        for it in list.iter_mut() {
                            if it.status == TodoStatus::InProgress {
                                it.status = TodoStatus::Pending;
                            }
                        }
                    }
                    let pos = insert_position(v);
                    let idx = match pos {
                        Some(p) if p <= 1 => 0,
                        Some(p) if p - 1 <= list.len() => p - 1,
                        _ => list.len(),
                    };
                    list.insert(
                        idx,
                        TodoItem {
                            content: content.to_string(),
                            status,
                        },
                    );
                    return Ok(());
                }
            }
            Err("todowrite: `insert` needs non-empty `content`.".into())
        }
        Some("update" | "delete" | "remove") => {
            let (Some(id), status) = (
                json_id(v),
                v.get("status")
                    .and_then(|x| x.as_str())
                    .and_then(TodoStatus::parse),
            ) else {
                return Err("todowrite: `update`/`delete` needs a valid `id`.".into());
            };
            if id == 0 || (id as usize) > list.len() {
                return Err(format!("todowrite: unknown task id {id}."));
            }
            let idx = (id - 1) as usize;
            match v.get("action").and_then(|a| a.as_str()) {
                Some("delete") | Some("remove") => {
                    list.remove(idx);
                }
                _ => {
                    if status.is_none() {
                        return Err(
                            "todowrite: `update` needs a `status` of pending|in_progress|completed."
                                .into(),
                        );
                    }
                    if status == Some(TodoStatus::InProgress) {
                        for it in list.iter_mut() {
                            if it.status == TodoStatus::InProgress {
                                it.status = TodoStatus::Pending;
                            }
                        }
                    }
                    list[idx].status = status.unwrap_or(TodoStatus::Pending);
                }
            }
            Ok(())
        }
        Some("clear") => {
            list.clear();
            Ok(())
        }
        _ => Err("todowrite: `action` must be `add`, `insert`, `update`, `delete`/`remove`, or `clear`.".into()),
    }
}

/// Whether a todo call's args are the FULL-LIST (re)plan shape (`{"todos":[…]}`) vs the
/// incremental `{"action":…}` / `{"actions":[…]}` shape. `todowrite` accepts both — distinguished
/// by shape, NOT by tool name. A payload that carries `actions` is NEVER a plan, even if a
/// leftover `todos` field also parses (execute/apply prefer `actions`; the fold must match).
pub fn is_todo_plan(args: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return false;
    };
    if v.get("actions").and_then(|a| a.as_array()).is_some() {
        return false;
    }
    parse_todos(args).is_ok()
}

/// Incremental patch shape: a single `action` or a non-absent `actions` array.
/// False for full-list `todos` plans (use [`is_todo_plan`]).
pub fn is_todo_action_args(args: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return false;
    };
    if is_todo_plan(args) {
        return false;
    }
    v.get("action").and_then(|a| a.as_str()).is_some()
        || v.get("actions").and_then(|a| a.as_array()).is_some()
}

/// Fold an ORDERED stream of `(tool_name, args)` todo-affecting calls into the current list.
/// Baseline = the LAST call carrying a valid full LIST (`{"todos":[…]}`; positions become the
/// stable 1-based ids); then every incremental `{"action":…}` call AFTER that baseline is
/// applied in order. Decided by ARG SHAPE, not tool name, so the merged `todowrite` (which
/// sends the list shape to plan and the action shape to patch) folds correctly — and a resumed
/// session's legacy `todo`-named calls fold identically. An invalid list never wipes an earlier
/// valid one; action events before the baseline are void (a re-plan resets the ids). THE single
/// source of truth for the fold — both the kernel-message reducer and the TUI panel derivation
/// use this shape rule, so live / replay / injected views never diverge.
pub fn reduce_todos<'a>(calls: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<TodoItem> {
    // Keep both names so a resumed transcript (legacy `todo` + `todowrite`) folds the same.
    let calls: Vec<(&str, &str)> = calls
        .into_iter()
        .filter(|(n, _)| *n == "todowrite" || *n == "todo")
        .collect();
    let baseline = calls.iter().rposition(|(_, a)| is_todo_plan(a));
    let (mut list, start) = match baseline {
        Some(i) => (parse_todos(calls[i].1).unwrap_or_default(), i + 1),
        None => (Vec::new(), 0),
    };
    for (_, a) in &calls[start..] {
        apply_todo_action(&mut list, a); // no-op unless the args carry an `action`
    }
    list
}

/// The current todo list, folded over the transcript (see [`reduce_todos`]). Returns `vec![]`
/// if there is no valid `todowrite` and no `todo` events.
pub fn derive_current_todos(messages: &[Message]) -> Vec<TodoItem> {
    reduce_todos(
        messages
            .iter()
            .flat_map(|m| m.tool_calls.iter())
            .map(|c| (c.name.as_str(), c.arguments.as_str())),
    )
}

/// Live list handle shared with the coding `TodoHook` so `execute` validates ids
/// against the same fold the transcript uses. Transcript remains the source of
/// truth; the hook refreshes this from [`derive_current_todos`] each request.
pub type TodoLive = Arc<Mutex<Vec<TodoItem>>>;

/// Register (or replace) `todowrite` and return the live list for [`TodoHook`].
pub fn bind_todowrite(reg: &mut ToolRegistry) -> TodoLive {
    let tool = TodoTool::new();
    let live = tool.live();
    reg.register(Arc::new(tool));
    live
}

/// `todowrite` tool. Execute applies against the live snapshot (empty until the
/// hook syncs, or until earlier `execute`s on this instance). Transcript fold
/// via [`reduce_todos`] remains authoritative.
#[derive(Clone)]
pub struct TodoTool {
    live: TodoLive,
}

impl Default for TodoTool {
    fn default() -> Self {
        Self::new()
    }
}

impl TodoTool {
    pub fn new() -> Self {
        Self {
            live: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn with_live(live: TodoLive) -> Self {
        Self { live }
    }

    pub fn live(&self) -> TodoLive {
        Arc::clone(&self.live)
    }

    fn lock_live(&self) -> std::sync::MutexGuard<'_, Vec<TodoItem>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }
}

const TODOWRITE_DESCRIPTION: &str = "Create and maintain a structured task list for the current coding session. \
Prefer ONE `actions` array per turn for every REAL change of the SAME kind you already know.\n\
Do NOT call this tool unless the list must change. Never re-mark an item already in that status \
(no-op — wasted turn). A successful result reprints the numbered list — use THOSE ids next; \
a failed result reprints the unchanged list — fix ids from it, do not retry blindly.\n\
Legal mixes only:\n\
- `add` + `update` (add appends; existing ids do not shift). First plan: add… + update #1 in_progress.\n\
- `clear` + `add` + `update` (`clear` ALWAYS runs first, then add, then update). Use this to replace a plan in ONE call.\n\
- `insert` + `update` (internally inserts first, then updates — `id` is AFTER inserts).\n\
- several `update` (any order; `id` is the current list).\n\
- several `delete` (any order; `id` is the current list). Do NOT mix delete with anything else.\n\
- `insert` stays with inserts/updates only. Do NOT mix insert with add/clear/delete.\n\
`id`/`position` are 1-based. update/delete order in the JSON does not matter.\n\
The MOMENT you START a task set it `in_progress`; the MOMENT it is verified done set it `completed`.\n\
Keep EXACTLY ONE task `in_progress` after the batch (enforced).";

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str {
        "todowrite"
    }
    fn description(&self) -> &str {
        TODOWRITE_DESCRIPTION
    }
    fn parallel_safe(&self, _args: &str) -> bool {
        // Writes the live list; same-batch todowrite calls must see each other.
        false
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "actions": {
                    "type": "array",
                    "description": "Batch of same-kind (or add+update / clear+add+update / insert+update) operations. delete-only. `clear` runs FIRST when mixed with add/update. insert stays with inserts/updates only. update/delete ids may be in any order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "action": { "type": "string", "enum": ["add", "insert", "update", "delete", "remove", "clear"] },
                            "id": { "type": "integer", "description": "1-based. update/delete-only: current list. insert+update: AFTER inserts. add+update: existing ids stay; new items are old_len+1…" },
                            "position": { "type": "integer", "description": "For insert: 1-based slot on the list before this batch's inserts (after any adds)." },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                            "content": { "type": "string", "description": "For add/insert: task text." }
                        },
                        "required": ["action"]
                    }
                },
                "action": { "type": "string", "enum": ["add", "insert", "update", "delete", "remove", "clear"], "description": "Single-action shorthand (same fields as one `actions` item)." },
                "position": { "type": "integer", "description": "For action=insert: 1-based position." },
                "id": { "type": "integer", "description": "For action=update/delete: 1-based task number." },
                "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                "content": { "type": "string", "description": "For action=add/insert: task text." },
                "todos": {
                    "type": "array",
                    "description": "Legacy resume-only full-list replace. Do not send on new work — use `clear` + `add`s in the same `actions` array.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
                        },
                        "required": ["content", "status"]
                    }
                }
            }
        })
    }
    // Never touches the filesystem → risk() defaults to Safe.
    fn always_grant_scope(&self, _args: &str) -> String {
        // Tool-wide: planning is harmless; one grant covers all calls this session.
        String::new()
    }
    async fn execute(&self, args: &str, _ctx: &ToolContext) -> ToolResult {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
            return err("todowrite: invalid JSON arguments.".to_string());
        };
        let mut list = self.lock_live().clone();

        // Legacy full-list replace (resume / old transcripts). Prefer `actions`.
        // `actions` wins if both fields are present (same as the fold).
        if v.get("actions").and_then(|a| a.as_array()).is_none() && v.get("todos").is_some() {
            return match parse_todos(args) {
                Ok(todos) => {
                    *self.lock_live() = todos.clone();
                    ok(render_todos_numbered(&todos, false))
                }
                Err(e) => err(with_current_list(e, &list)),
            };
        }

        if v.get("actions").and_then(|a| a.as_array()).is_none()
            && v.get("action").and_then(|a| a.as_str()).is_none()
        {
            return err(with_current_list(
                "todowrite: provide `actions` (preferred batch), a single `action`, or legacy `todos`.",
                &list,
            ));
        }

        let summary = match todo_change_summary(&v) {
            Ok(s) => s,
            Err(e) => return err(with_current_list(e, &list)),
        };

        match try_apply_todo_args(&mut list, args) {
            Ok(ApplyOutcome::Unchanged) => ok(format!(
                "No change — already in that state. Do not retry.\n\n{}",
                render_todos_numbered(&list, false)
            )),
            Ok(ApplyOutcome::Changed) => {
                *self.lock_live() = list.clone();
                ok(format!(
                    "{summary}\n\n{}",
                    render_todos_numbered(&list, false)
                ))
            }
            Err(e) => err(with_current_list(e, &self.lock_live())),
        }
    }
}

fn with_current_list(msg: impl std::fmt::Display, list: &[TodoItem]) -> String {
    format!(
        "{msg}\n\nCurrent list (unchanged):\n{}\nUse these 1-based ids. Do not retry a failed id.",
        render_todos_numbered(list, false)
    )
}

fn todo_change_summary(v: &serde_json::Value) -> Result<String, String> {
    if let Some(arr) = v.get("actions").and_then(|a| a.as_array()) {
        if arr.is_empty() {
            return Err("todowrite: `actions` must be a non-empty array.".into());
        }
        validate_actions_mix(arr)?;
        let mut lines = Vec::with_capacity(arr.len());
        for (i, item) in arr.iter().enumerate() {
            match summarize_todo_action(item) {
                Ok(line) => lines.push(line),
                Err(e) => {
                    return Err(format!(
                        "todowrite: actions[{i}] rejected; the list was NOT changed. {e}"
                    ));
                }
            }
        }
        return Ok(lines.join("\n"));
    }
    summarize_todo_action(v).map_err(|e| format!("todowrite: {e}"))
}

fn summarize_todo_action(v: &serde_json::Value) -> Result<String, String> {
    match v.get("action").and_then(|a| a.as_str()) {
        Some("add") => match v.get("content").and_then(|c| c.as_str()) {
            Some(c) if !c.trim().is_empty() => Ok(format!("Added task: {}", c.trim())),
            _ => Err("`add` needs non-empty `content`.".into()),
        },
        Some("insert") => {
            let content = v
                .get("content")
                .and_then(|c| c.as_str())
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .ok_or_else(|| "`insert` needs non-empty `content`.".to_string())?;
            let pos = insert_position(v).unwrap_or(1).max(1);
            Ok(format!("#{pos} \u{2192} inserted: {content}"))
        }
        Some("update") => {
            let id = json_id(v);
            let status = v.get("status").and_then(|x| x.as_str());
            match (id, status.and_then(TodoStatus::parse)) {
                (Some(id), Some(_)) if id >= 1 => Ok(format!("#{} \u{2192} {}", id, status.unwrap())),
                (None, _) => Err("`update` needs an `id` (the task number).".into()),
                (Some(_), _) => {
                    Err("`update` needs a `status` of pending|in_progress|completed.".into())
                }
            }
        }
        Some("delete") | Some("remove") => match json_id(v) {
            Some(id) if id >= 1 => Ok(format!("#{id} \u{2192} removed")),
            _ => Err("`delete`/`remove` needs an `id` (the task number).".into()),
        },
        Some("clear") => Ok("all tasks cleared".to_string()),
        Some(other) => Err(format!(
            "`action` must be add|insert|update|delete|clear (got `{other}`)."
        )),
        None => Err("each item needs an `action` field.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomcode_kernel::message::Message;
    use atomcode_kernel::tool::ToolCall;
    use tokio_util::sync::CancellationToken;

    fn ctx() -> ToolContext {
        ToolContext {
            working_dir: std::path::PathBuf::from("."),
            cancel: CancellationToken::new(),
            progress: atomcode_kernel::tool::ProgressSink::noop(),
            requester: None,
        }
    }

    #[test]
    fn parse_valid_full_list() {
        let todos = parse_todos(r#"{"todos":[{"content":"a","status":"pending"},{"content":"b","status":"in_progress"}]}"#).unwrap();
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0].content, "a");
        assert_eq!(todos[1].status, TodoStatus::InProgress);
    }

    #[test]
    fn parse_accepts_one_stringified_todos_layer() {
        let todos =
            parse_todos(r#"{"todos":"[{\"content\":\"a\",\"status\":\"in_progress\"}]"}"#).unwrap();
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].content, "a");
        assert_eq!(todos[0].status, TodoStatus::InProgress);
    }

    #[test]
    fn parse_rejects_double_stringified_todos() {
        let error = parse_todos(r#"{"todos":"\"[]\""}"#).unwrap_err();
        assert!(error.contains("invalid arguments"), "{error}");
    }

    #[test]
    fn parse_rejects_bad_status() {
        let e = parse_todos(r#"{"todos":[{"content":"a","status":"done"}]}"#).unwrap_err();
        assert!(e.contains("pending"), "{e}");
    }

    #[test]
    fn parse_rejects_two_in_progress() {
        let e = parse_todos(r#"{"todos":[{"content":"a","status":"in_progress"},{"content":"b","status":"in_progress"}]}"#).unwrap_err();
        assert!(e.to_ascii_lowercase().contains("in_progress"), "{e}");
    }

    #[test]
    fn parse_rejects_empty_content() {
        let e = parse_todos(r#"{"todos":[{"content":"  ","status":"pending"}]}"#).unwrap_err();
        assert!(e.contains("content"), "{e}");
    }

    #[test]
    fn parse_empty_list_ok() {
        assert_eq!(parse_todos(r#"{"todos":[]}"#).unwrap().len(), 0);
    }

    #[test]
    fn glyph_unicode_vs_ascii() {
        assert_eq!(todo_glyph(TodoStatus::Pending, true), "[ ]");
        assert_eq!(todo_glyph(TodoStatus::InProgress, true), "[\u{2022}]"); // [•]
        assert_eq!(todo_glyph(TodoStatus::Completed, true), "[\u{2713}]"); // [✓]
        assert_eq!(todo_glyph(TodoStatus::Pending, false), "[ ]");
        assert_eq!(todo_glyph(TodoStatus::InProgress, false), "[~]");
        assert_eq!(todo_glyph(TodoStatus::Completed, false), "[x]");
    }

    #[test]
    fn render_text_ascii() {
        let todos = vec![
            TodoItem {
                content: "first".into(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                content: "second".into(),
                status: TodoStatus::InProgress,
            },
        ];
        let s = render_todos_text(&todos, false);
        assert!(s.contains("[x] first"), "{s}");
        assert!(s.contains("[~] second"), "{s}");
    }

    #[test]
    fn derive_finds_last_todowrite() {
        let msgs = vec![
            Message::user("hi"),
            Message::assistant(
                "",
                vec![ToolCall {
                    id: "1".into(),
                    name: "todowrite".into(),
                    arguments: r#"{"todos":[{"content":"old","status":"pending"}]}"#.into(),
                }],
            ),
            Message::assistant(
                "",
                vec![ToolCall {
                    id: "2".into(),
                    name: "todowrite".into(),
                    arguments: r#"{"todos":[{"content":"new","status":"in_progress"}]}"#.into(),
                }],
            ),
        ];
        let todos = derive_current_todos(&msgs);
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].content, "new"); // LAST wins
        assert_eq!(todos[0].status, TodoStatus::InProgress);
    }

    #[test]
    fn derive_recovers_stringified_todowrite_from_transcript() {
        let msgs = vec![
            Message::assistant(
                "",
                vec![ToolCall {
                    id: "1".into(),
                    name: "todowrite".into(),
                    arguments:
                        r#"{"todos":"[{\"content\":\"persisted\",\"status\":\"pending\"}]"}"#.into(),
                }],
            ),
            todo_call("2", r#"{"action":"update","id":1,"status":"completed"}"#),
        ];
        let todos = derive_current_todos(&msgs);
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].content, "persisted");
        assert_eq!(todos[0].status, TodoStatus::Completed);
    }

    #[test]
    fn counts_completed_over_total() {
        let todos = parse_todos(
            r#"{"todos":[
                {"content":"a","status":"completed"},
                {"content":"b","status":"completed"},
                {"content":"c","status":"in_progress"},
                {"content":"d","status":"pending"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(todo_counts(&todos), (2, 1, 4));
        assert_eq!(todo_counts(&[]), (0, 0, 0));
    }

    #[test]
    fn derive_none_when_no_todowrite() {
        let msgs = vec![Message::user("hi"), Message::assistant("hello", vec![])];
        assert!(derive_current_todos(&msgs).is_empty());
    }

    #[test]
    fn derive_skips_invalid_and_returns_last_valid() {
        // The LAST todowrite has two in_progress items (invalid); the earlier one
        // is valid. derive_current_todos must return the earlier valid list, not [].
        let msgs = vec![
            Message::assistant("", vec![ToolCall {
                id: "1".into(),
                name: "todowrite".into(),
                arguments: r#"{"todos":[{"content":"keep","status":"pending"}]}"#.into(),
            }]),
            Message::assistant("", vec![ToolCall {
                id: "2".into(),
                name: "todowrite".into(),
                // Invalid: two in_progress items.
                arguments: r#"{"todos":[{"content":"a","status":"in_progress"},{"content":"b","status":"in_progress"}]}"#.into(),
            }]),
        ];
        let todos = derive_current_todos(&msgs);
        assert_eq!(
            todos.len(),
            1,
            "should skip invalid call and return last valid list"
        );
        assert_eq!(todos[0].content, "keep");
        assert_eq!(todos[0].status, TodoStatus::Pending);
    }

    // ---- incremental `todo` action reducer ----------------------------------------------

    fn todo_call(id: &str, args: &str) -> Message {
        Message::assistant(
            "",
            vec![ToolCall {
                id: id.into(),
                name: "todo".into(),
                arguments: args.into(),
            }],
        )
    }
    fn write_call(id: &str, args: &str) -> Message {
        Message::assistant(
            "",
            vec![ToolCall {
                id: id.into(),
                name: "todowrite".into(),
                arguments: args.into(),
            }],
        )
    }
    const PLAN3: &str = r#"{"todos":[{"content":"a","status":"pending"},{"content":"b","status":"pending"},{"content":"c","status":"pending"}]}"#;

    #[test]
    fn reduce_add_appends_after_baseline() {
        let msgs = vec![
            write_call("1", PLAN3),
            todo_call("2", r#"{"action":"add","content":"d"}"#),
        ];
        let todos = derive_current_todos(&msgs);
        assert_eq!(todos.len(), 4);
        assert_eq!(todos[3].content, "d");
        assert_eq!(todos[3].status, TodoStatus::Pending);
    }

    #[test]
    fn reduce_update_flips_only_that_id() {
        let msgs = vec![
            write_call("1", PLAN3),
            todo_call("2", r#"{"action":"update","id":2,"status":"completed"}"#),
        ];
        let todos = derive_current_todos(&msgs);
        assert_eq!(todos[0].status, TodoStatus::Pending);
        assert_eq!(todos[1].status, TodoStatus::Completed); // #2 (1-based)
        assert_eq!(todos[2].status, TodoStatus::Pending);
    }

    #[test]
    fn reduce_in_progress_clears_previous_in_progress() {
        // Invariant enforced by the reducer: setting #3 in_progress clears #1.
        let msgs = vec![
            write_call("1", PLAN3),
            todo_call("2", r#"{"action":"update","id":1,"status":"in_progress"}"#),
            todo_call("3", r#"{"action":"update","id":3,"status":"in_progress"}"#),
        ];
        let todos = derive_current_todos(&msgs);
        assert_eq!(
            todos[0].status,
            TodoStatus::Pending,
            "#1 must revert to pending"
        );
        assert_eq!(
            todos[2].status,
            TodoStatus::InProgress,
            "#3 is the only in_progress"
        );
        assert_eq!(
            todos
                .iter()
                .filter(|t| t.status == TodoStatus::InProgress)
                .count(),
            1
        );
    }

    #[test]
    fn reduce_unknown_id_is_ignored() {
        let msgs = vec![
            write_call("1", PLAN3),
            todo_call("2", r#"{"action":"update","id":9,"status":"completed"}"#),
        ];
        let todos = derive_current_todos(&msgs);
        assert_eq!(todos.len(), 3);
        assert!(
            todos.iter().all(|t| t.status == TodoStatus::Pending),
            "unknown id must not change anything"
        );
    }

    #[test]
    fn reduce_replan_resets_ids_and_voids_stale_updates() {
        // An update BEFORE a re-plan is void; the re-plan is the new baseline.
        let msgs = vec![
            write_call("1", PLAN3),
            todo_call("2", r#"{"action":"update","id":1,"status":"completed"}"#), // pre-replan → void
            write_call(
                "3",
                r#"{"todos":[{"content":"x","status":"pending"},{"content":"y","status":"pending"}]}"#,
            ),
            todo_call("4", r#"{"action":"update","id":2,"status":"completed"}"#),
        ];
        let todos = derive_current_todos(&msgs);
        assert_eq!(todos.len(), 2, "list is the re-plan, not the old plan");
        assert_eq!(todos[0].content, "x");
        assert_eq!(
            todos[0].status,
            TodoStatus::Pending,
            "pre-replan update on old #1 is void"
        );
        assert_eq!(
            todos[1].status,
            TodoStatus::Completed,
            "post-replan update on new #2 applies"
        );
    }

    #[test]
    fn reduce_todo_only_from_empty() {
        // No todowrite at all — a session that plans purely with `todo add`.
        let msgs = vec![
            todo_call("1", r#"{"action":"add","content":"first"}"#),
            todo_call("2", r#"{"action":"add","content":"second"}"#),
            todo_call("3", r#"{"action":"update","id":1,"status":"in_progress"}"#),
        ];
        let todos = derive_current_todos(&msgs);
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0].content, "first");
        assert_eq!(todos[0].status, TodoStatus::InProgress);
        assert_eq!(todos[1].status, TodoStatus::Pending);
    }

    #[test]
    fn reduce_no_todo_events_matches_legacy_behavior() {
        // With zero `todo` events the reducer returns exactly the last todowrite list.
        let msgs = vec![write_call("1", PLAN3)];
        assert_eq!(derive_current_todos(&msgs).len(), 3);
    }

    #[tokio::test]
    async fn execute_echoes_normalized_list() {
        let t = TodoTool::new();
        let r = t
            .execute(
                r#"{"todos":[{"content":"task","status":"pending"}]}"#,
                &ctx(),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("task"), "{}", r.content);
    }

    #[tokio::test]
    async fn execute_accepts_stringified_todos_from_provider() {
        let result = TodoTool::new()
            .execute(
                r#"{"todos":"[{\"content\":\"task\",\"status\":\"pending\"}]"}"#,
                &ctx(),
            )
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("task"), "{}", result.content);
    }

    #[tokio::test]
    async fn execute_rejects_invalid() {
        let t = TodoTool::new();
        let r = t
            .execute(r#"{"todos":[{"content":"a","status":"nope"}]}"#, &ctx())
            .await;
        assert!(r.is_error);
        assert!(r.content.contains("pending"), "{}", r.content);
    }

    #[test]
    fn tool_name_is_todowrite() {
        assert_eq!(TodoTool::new().name(), "todowrite");
    }

    #[test]
    fn todowrite_result_is_numbered_for_title_cache() {
        // The result must carry `<glyph> <id>. <content>` so the TUI title cache learns ids.
        let out = render_todos_numbered(&parse_todos(PLAN3).unwrap(), false);
        assert!(out.contains("] 1. a"), "{out}");
        assert!(out.contains("] 3. c"), "{out}");
    }

    #[tokio::test]
    async fn todowrite_accepts_full_list_shape() {
        let t = TodoTool::new();
        assert_eq!(t.name(), "todowrite");
        let r = t.execute(PLAN3, &ctx()).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(
            r.content.contains("1. a"),
            "numbered list echoed: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn todowrite_accepts_incremental_action_shape() {
        // The reported confusion is gone: the SAME `todowrite` tool takes the {action} shape too,
        // so a model that sends `{action}` no longer hits `action must be add or update`.
        let t = TodoTool::new();
        let add = t
            .execute(r#"{"action":"add","content":"write tests"}"#, &ctx())
            .await;
        assert!(
            !add.is_error && add.content.contains("write tests"),
            "{}",
            add.content
        );
        // `#<id> → <status>` is the exact base the TUI enrich step splices a title into.
        // Live list after the add has only #1.
        let upd = t
            .execute(r#"{"action":"update","id":1,"status":"completed"}"#, &ctx())
            .await;
        assert!(!upd.is_error, "{}", upd.content);
        assert!(upd.content.contains("#1 \u{2192} completed"), "{}", upd.content);
        assert!(upd.content.contains("1. write tests"), "{}", upd.content);
        let bad = t
            .execute(r#"{"action":"update","id":9,"status":"completed"}"#, &ctx())
            .await;
        assert!(bad.is_error, "unknown id must fail at execute");
        assert!(bad.content.contains("Current list"), "{}", bad.content);
        assert!(bad.content.contains("Do not retry"), "{}", bad.content);
    }

    #[tokio::test]
    async fn todowrite_execute_accepts_delete_remove_clear() {
        let t = TodoTool::new();
        let _ = t
            .execute(
                r#"{"todos":[{"content":"a","status":"pending"},{"content":"b","status":"pending"},{"content":"c","status":"pending"}]}"#,
                &ctx(),
            )
            .await;
        let del = t
            .execute(r#"{"action":"delete","id":2}"#, &ctx())
            .await;
        assert!(!del.is_error, "{}", del.content);
        assert!(del.content.contains("#2 \u{2192} removed"), "{}", del.content);
        assert!(del.content.contains("1. a"), "{}", del.content);
        let rm = t
            .execute(r#"{"action":"remove","id":1}"#, &ctx())
            .await;
        assert!(!rm.is_error, "{}", rm.content);
        assert!(rm.content.contains("#1 \u{2192} removed"), "{}", rm.content);
        let clr = t.execute(r#"{"action":"clear"}"#, &ctx()).await;
        assert!(!clr.is_error, "{}", clr.content);
        assert!(clr.content.contains("all tasks cleared"), "{}", clr.content);
        let bad = t.execute(r#"{"action":"delete"}"#, &ctx()).await;
        assert!(bad.is_error);
        let ghost = t
            .execute(r#"{"action":"delete","id":2}"#, &ctx())
            .await;
        assert!(ghost.is_error, "delete on empty list must fail");
        assert!(ghost.content.contains("Current list"), "{}", ghost.content);
    }

    #[tokio::test]
    async fn apply_todo_action_delete_remove_clear_preserves_state() {
        // Fold the transcript: plan → delete → remove → clear.
        let plan = r#"{"todos":[{"content":"a","status":"pending"},{"content":"b","status":"in_progress"},{"content":"c","status":"completed"}]}"#;
        let mut list = parse_todos(plan).unwrap();
        assert_eq!(list.len(), 3);

        apply_todo_action(&mut list, r#"{"action":"delete","id":2}"#);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].content, "a");
        assert_eq!(list[1].content, "c", "delete renumbers remaining items");

        apply_todo_action(&mut list, r#"{"action":"remove","id":1}"#);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].content, "c");

        apply_todo_action(&mut list, r#"{"action":"clear"}"#);
        assert!(list.is_empty());

        // Unknown id is ignored (list unchanged, no panic).
        let mut list2 = parse_todos(plan).unwrap();
        apply_todo_action(&mut list2, r#"{"action":"delete","id":99}"#);
        assert_eq!(list2.len(), 3);
    }

    #[tokio::test]
    async fn todowrite_rejects_bad_args() {
        let t = TodoTool::new();
        assert!(
            t.execute(r#"{"action":"add","content":"  "}"#, &ctx())
                .await
                .is_error,
            "empty add"
        );
        assert!(
            t.execute(r#"{"action":"update","status":"completed"}"#, &ctx())
                .await
                .is_error,
            "no id"
        );
        assert!(
            t.execute(r#"{"action":"update","id":2}"#, &ctx())
                .await
                .is_error,
            "no status"
        );
        assert!(
            t.execute(r#"{"action":"update","id":2,"status":"nope"}"#, &ctx())
                .await
                .is_error,
            "bad status"
        );
        assert!(
            t.execute(r#"{"action":"frobnicate"}"#, &ctx())
                .await
                .is_error,
            "bad action"
        );
        assert!(
            t.execute(r#"{}"#, &ctx()).await.is_error,
            "neither todos nor action"
        );
    }

    #[tokio::test]
    async fn todowrite_accepts_actions_batch() {
        let t = TodoTool::new();
        let r = t
            .execute(
                r#"{"actions":[{"action":"add","content":"one"},{"action":"add","content":"two"},{"action":"update","id":1,"status":"in_progress"}]}"#,
                &ctx(),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("Added task: one"), "{}", r.content);
        assert!(r.content.contains("#1"), "{}", r.content);
        assert!(r.content.contains("1. one"), "reprints numbered list: {}", r.content);
        let noop = t
            .execute(
                r#"{"actions":[{"action":"update","id":1,"status":"in_progress"}]}"#,
                &ctx(),
            )
            .await;
        assert!(!noop.is_error, "{}", noop.content);
        assert!(noop.content.contains("No change"), "{}", noop.content);

        let mixed = t
            .execute(
                r#"{"actions":[{"action":"delete","id":1},{"action":"update","id":2,"status":"completed"}]}"#,
                &ctx(),
            )
            .await;
        assert!(mixed.is_error, "delete+update must be rejected: {}", mixed.content);
        assert!(mixed.content.contains("delete"), "{}", mixed.content);
    }

    #[test]
    fn apply_todo_actions_batch_is_transactional() {
        let mut list = vec![
            TodoItem {
                content: "a".into(),
                status: TodoStatus::Pending,
            },
            TodoItem {
                content: "b".into(),
                status: TodoStatus::Pending,
            },
        ];
        apply_todo_action(
            &mut list,
            r#"{"actions":[{"action":"update","id":1,"status":"completed"},{"action":"update","id":99,"status":"in_progress"}]}"#,
        );
        assert_eq!(list[0].status, TodoStatus::Pending, "failed batch must not apply");
        apply_todo_action(
            &mut list,
            r#"{"actions":[{"action":"update","id":1,"status":"completed"},{"action":"update","id":2,"status":"in_progress"}]}"#,
        );
        assert_eq!(list[0].status, TodoStatus::Completed);
        assert_eq!(list[1].status, TodoStatus::InProgress);
    }

    #[test]
    fn apply_todo_actions_updates_are_id_addressed_order_independent() {
        let mut list = vec![
            TodoItem {
                content: "a".into(),
                status: TodoStatus::Pending,
            },
            TodoItem {
                content: "b".into(),
                status: TodoStatus::Pending,
            },
            TodoItem {
                content: "c".into(),
                status: TodoStatus::Pending,
            },
        ];
        apply_todo_action(
            &mut list,
            r#"{"actions":[{"action":"update","id":3,"status":"completed"},{"action":"update","id":1,"status":"completed"},{"action":"update","id":2,"status":"in_progress"}]}"#,
        );
        assert_eq!(list[0].status, TodoStatus::Completed);
        assert_eq!(list[1].status, TodoStatus::InProgress);
        assert_eq!(list[2].status, TodoStatus::Completed);
        apply_todo_action(
            &mut list,
            r#"{"actions":[{"action":"delete","id":3},{"action":"delete","id":1}]}"#,
        );
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].content, "b");
        assert_eq!(list[0].status, TodoStatus::InProgress);
    }

    #[test]
    fn apply_todo_actions_rejects_mixed_id_shifting_kinds() {
        let mut list = vec![
            TodoItem {
                content: "a".into(),
                status: TodoStatus::Pending,
            },
            TodoItem {
                content: "b".into(),
                status: TodoStatus::Pending,
            },
        ];
        apply_todo_action(
            &mut list,
            r#"{"actions":[{"action":"delete","id":1},{"action":"update","id":2,"status":"completed"}]}"#,
        );
        assert_eq!(list.len(), 2, "illegal mix must not apply");
        apply_todo_action(
            &mut list,
            r#"{"actions":[{"action":"insert","position":1,"content":"x"},{"action":"add","content":"y"}]}"#,
        );
        assert_eq!(list.len(), 2, "insert+add must not apply");
        apply_todo_action(
            &mut list,
            r#"{"actions":[{"action":"clear"},{"action":"delete","id":1}]}"#,
        );
        assert_eq!(list.len(), 2, "clear+delete must not apply");
    }

    #[test]
    fn apply_todo_actions_clear_then_add_update_replaces_plan() {
        let mut list = vec![
            TodoItem {
                content: "old-1".into(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                content: "old-2".into(),
                status: TodoStatus::InProgress,
            },
        ];
        apply_todo_action(
            &mut list,
            r#"{"actions":[{"action":"clear"},{"action":"add","content":"new-1"},{"action":"add","content":"new-2"},{"action":"update","id":1,"status":"in_progress"}]}"#,
        );
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].content, "new-1");
        assert_eq!(list[0].status, TodoStatus::InProgress);
        assert_eq!(list[1].content, "new-2");
        assert_eq!(list[1].status, TodoStatus::Pending);
    }

    #[test]
    fn add_on_finished_list_auto_clears_so_ids_restart() {
        let mut list = vec![
            TodoItem {
                content: "done-1".into(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                content: "done-2".into(),
                status: TodoStatus::Completed,
            },
        ];
        apply_todo_action(
            &mut list,
            r#"{"actions":[{"action":"add","content":"next-1"},{"action":"add","content":"next-2"},{"action":"update","id":1,"status":"in_progress"}]}"#,
        );
        assert_eq!(list.len(), 2, "finished list must be replaced, not appended");
        assert_eq!(list[0].content, "next-1");
        assert_eq!(list[0].status, TodoStatus::InProgress);
        assert_eq!(list[1].content, "next-2");
    }

    #[test]
    fn add_on_unfinished_list_does_not_auto_clear() {
        let mut list = vec![
            TodoItem {
                content: "done".into(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                content: "still-open".into(),
                status: TodoStatus::Pending,
            },
        ];
        apply_todo_action(&mut list, r#"{"action":"add","content":"extra"}"#);
        assert_eq!(list.len(), 3);
        assert_eq!(list[2].content, "extra");
    }

    #[test]
    fn apply_todo_insert_then_update_uses_post_insert_ids() {
        let mut list = vec![
            TodoItem {
                content: "a".into(),
                status: TodoStatus::Pending,
            },
            TodoItem {
                content: "b".into(),
                status: TodoStatus::Pending,
            },
        ];
        // insert at 1 → [x, a, b]; update id=2 is original `a` (after insert).
        apply_todo_action(
            &mut list,
            r#"{"actions":[{"action":"update","id":2,"status":"in_progress"},{"action":"insert","position":1,"content":"x"}]}"#,
        );
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].content, "x");
        assert_eq!(list[1].content, "a");
        assert_eq!(list[1].status, TodoStatus::InProgress);
        assert_eq!(list[2].content, "b");
    }

    #[test]
    fn mixed_actions_and_todos_is_not_a_plan_baseline() {
        // Leftover `todos` must not reset the fold when `actions` is present —
        // execute/apply prefer actions; is_todo_plan must agree.
        let mixed = r#"{"actions":[{"action":"add","content":"from-actions"}],"todos":[{"content":"from-todos","status":"pending"}]}"#;
        assert!(!is_todo_plan(mixed), "actions wins over leftover todos");
        assert!(is_todo_action_args(mixed));
        let list = reduce_todos([
            (
                "todowrite",
                r#"{"todos":[{"content":"keep","status":"pending"}]}"#,
            ),
            ("todowrite", mixed),
        ]);
        assert_eq!(list.len(), 2, "must fold actions on top of prior plan, not replace with todos");
        assert_eq!(list[0].content, "keep");
        assert_eq!(list[1].content, "from-actions");
    }

    #[test]
    fn apply_accepts_string_ids() {
        let mut list = vec![TodoItem {
            content: "a".into(),
            status: TodoStatus::Pending,
        }];
        apply_todo_action(
            &mut list,
            r#"{"actions":[{"action":"update","id":"1","status":"completed"}]}"#,
        );
        assert_eq!(list[0].status, TodoStatus::Completed);
    }

    #[test]
    fn reducer_folds_actions_array() {
        let list = reduce_todos([
            (
                "todowrite",
                r#"{"actions":[{"action":"add","content":"a"},{"action":"add","content":"b"}]}"#,
            ),
            (
                "todowrite",
                r#"{"actions":[{"action":"update","id":1,"status":"completed"},{"action":"update","id":2,"status":"in_progress"}]}"#,
            ),
        ]);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].status, TodoStatus::Completed);
        assert_eq!(list[1].status, TodoStatus::InProgress);
    }

    #[test]
    fn reducer_folds_action_shape_under_todowrite_name() {
        // Merge regression: an incremental {action} carried by the `todowrite` tool name (not the
        // legacy `todo` name) must still fold — the baseline/patch decision is by SHAPE, not name.
        let list = reduce_todos([
            (
                "todowrite",
                r#"{"todos":[{"content":"a","status":"pending"},{"content":"b","status":"pending"}]}"#,
            ),
            (
                "todowrite",
                r#"{"action":"update","id":1,"status":"completed"}"#,
            ),
            ("todowrite", r#"{"action":"add","content":"c"}"#),
        ]);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].status, TodoStatus::Completed);
        assert_eq!(list[2].content, "c");
        // Legacy `todo`-named action still folds too (resume compatibility).
        let legacy = reduce_todos([
            (
                "todowrite",
                r#"{"todos":[{"content":"a","status":"pending"}]}"#,
            ),
            (
                "todo",
                r#"{"action":"update","id":1,"status":"in_progress"}"#,
            ),
        ]);
        assert_eq!(legacy[0].status, TodoStatus::InProgress);
    }

    #[test]
    fn description_covers_both_plan_and_update_modes() {
        let t = TodoTool::new();
        let d = t.description();
        assert!(
            d.contains("actions"),
            "prefers batch actions: {d}"
        );
        assert!(
            d.contains("add") && d.contains("update") && d.contains("insert"),
            "covers add/update/insert: {d}"
        );
        assert!(
            d.contains("in_progress"),
            "sets in_progress rule: {d}"
        );
        assert!(
            d.contains("already in that status") && d.contains("do not retry"),
            "discourages no-op / blind retry: {d}"
        );
    }

    #[tokio::test]
    async fn todowrite_execute_accepts_insert() {
        let t = TodoTool::new();
        let ins = t
            .execute(
                r#"{"action":"insert","position":2,"content":"intermediate step"}"#,
                &ctx(),
            )
            .await;
        assert!(!ins.is_error, "{}", ins.content);
        assert!(
            ins.content.contains("#2 \u{2192} inserted: intermediate step"),
            "{}",
            ins.content
        );

        let bad = t
            .execute(r#"{"action":"insert","content":""}"#, &ctx())
            .await;
        assert!(bad.is_error);
    }

    #[tokio::test]
    async fn apply_todo_action_insert_between_items() {
        let plan = r#"{"todos":[{"content":"a","status":"pending"},{"content":"c","status":"pending"}]}"#;
        let mut list = parse_todos(plan).unwrap();
        assert_eq!(list.len(), 2);

        // Insert at position 2 (between #1 'a' and #2 'c')
        apply_todo_action(
            &mut list,
            r#"{"action":"insert","position":2,"content":"b"}"#,
        );
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].content, "a");
        assert_eq!(list[1].content, "b");
        assert_eq!(list[2].content, "c");

        // Insert at position 1 (at head)
        apply_todo_action(
            &mut list,
            r#"{"action":"insert","position":1,"content":"head"}"#,
        );
        assert_eq!(list.len(), 4);
        assert_eq!(list[0].content, "head");
        assert_eq!(list[1].content, "a");

        // Insert at position beyond length (appends)
        apply_todo_action(
            &mut list,
            r#"{"action":"insert","position":99,"content":"tail"}"#,
        );
        assert_eq!(list.len(), 5);
        assert_eq!(list[4].content, "tail");
    }
}

