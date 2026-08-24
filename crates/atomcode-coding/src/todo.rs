//! `TodoHook` — injects the current todo list as a stored `<system-reminder>`
//! immediately ABOVE the current real user query (Grok Build order). Tool-loop
//! rounds do not rewind a tail reminder, so the previous request stays a prefix
//! of the next. The originating todowrite result remains in history; this
//! reminder re-surfaces the list after compaction.

use async_trait::async_trait;
use atomcode_capabilities::reminder::{
    insert_before_last_real_user, reminder_already_before_last_real_user, system_reminder,
};
use atomcode_capabilities::tools::todo::{
    derive_current_todos, render_todos_numbered, TodoItem, TodoLive, TodoStatus,
};
use atomcode_kernel::hook::{LifecycleHooks, TurnCtx};
use atomcode_kernel::message::{Conversation, Message, Role};
use atomcode_kernel::provider::{ChatOptions, ToolChoice};

use atomcode_config::config::TodoEagerness;

/// Injected when the model tries to STOP while the task list still has open items — the
/// residual weak-model gap after incremental `todo` updates land: it does the last item's work
/// (e.g. the closing summary) then ends WITHOUT marking it completed. Mirrors
/// `VerifyCadenceHook`'s `offer_continuation` cadence; nudges at most ONCE per real-user turn
/// (and the kernel `max_continuations` fuse bounds it), so it can never spin.
const TODO_COMPLETION_NUDGE: &str = "Before you finish: the task list still has open items. \
If you have actually completed them, mark each one done now with `todowrite` \
(`{\"action\":\"update\",\"id\":<id>,\"status\":\"completed\"}`). If some are NOT done, keep working \
through them. Only stop with open items if you genuinely need approval/input, are stuck, or the \
request is ambiguous — in that case say so briefly.";

pub struct TodoHook {
    live: Option<TodoLive>,
}

impl Default for TodoHook {
    fn default() -> Self {
        Self::new()
    }
}

impl TodoHook {
    pub fn new() -> Self {
        Self { live: None }
    }

    pub fn with_live(live: TodoLive) -> Self {
        Self { live: Some(live) }
    }

    fn sync_live(&self, todos: &[TodoItem]) {
        if let Some(live) = &self.live {
            *live.lock().unwrap_or_else(|e| e.into_inner()) = todos.to_vec();
        }
    }
}

/// High-recency todo activation policy. Unlike `TodoHook`, this only acts on
/// round one of a real user turn and only while no structured list exists.
///
/// The nudge is stored in [`LifecycleHooks::turn_start`] immediately ABOVE the
/// real query (same placement as `StatusReminderHook` / `SkillFirstHook`). It
/// must NOT land in `pre_request`: inserting above the last user there rewrites
/// the outgoing prefix and trips the kernel's append-only cache-prefix guard.
pub struct TodoEagerHook {
    eagerness: TodoEagerness,
}

impl TodoEagerHook {
    pub fn new(model: &str, provider_type: &str, configured: TodoEagerness) -> Self {
        let normalized = model.to_ascii_lowercase().replace(['_', ' '], "-");
        let mut eagerness = match configured {
            TodoEagerness::Auto
                if normalized.contains("deepseek")
                    && normalized.contains("v4")
                    && normalized.contains("flash") =>
            {
                TodoEagerness::Preferred
            }
            TodoEagerness::Auto => TodoEagerness::Auto,
            other => other,
        };
        if eagerness == TodoEagerness::Always && provider_type.eq_ignore_ascii_case("ollama") {
            eprintln!(
                "[todo] eager=always is unsupported by provider type ollama; using preferred"
            );
            eagerness = TodoEagerness::Preferred;
        }
        Self { eagerness }
    }

    fn should_nudge(&self, messages: &[Message]) -> bool {
        self.eagerness != TodoEagerness::Auto
            && derive_current_todos(messages)
                .iter()
                .all(|todo| todo.status == TodoStatus::Completed)
    }

    fn should_activate(&self, messages: &[Message], ctx: &TurnCtx) -> bool {
        ctx.round == 1 && self.should_nudge(messages)
    }

    fn body(&self) -> &'static str {
        if self.eagerness == TodoEagerness::Always {
            "You MUST call `todowrite` now, before any other tool or prose, to create the task list."
        } else {
            "Before acting, decide whether this task benefits from a todo list. If it has multiple requests, phases, files, dependencies, ambiguity, or requires investigation plus changes, call `todowrite` now. Skip it only for a genuinely simple one-step or purely informational request."
        }
    }
}

fn is_eager_todo_reminder(text: &str) -> bool {
    text.contains("You MUST call `todowrite` now")
        || text.contains("whether this task benefits from a todo list")
}

#[async_trait]
impl LifecycleHooks for TodoEagerHook {
    async fn turn_start(&self, convo: &mut Conversation) {
        if !self.should_nudge(&convo.messages) {
            return;
        }
        if reminder_already_before_last_real_user(&convo.messages, is_eager_todo_reminder) {
            return;
        }
        insert_before_last_real_user(
            &mut convo.messages,
            Message::synthetic_user(system_reminder(self.body())),
        );
    }

    async fn pre_request_options(
        &self,
        messages: &[Message],
        options: &mut ChatOptions,
        ctx: &TurnCtx,
    ) {
        if self.eagerness == TodoEagerness::Always && self.should_activate(messages, ctx) {
            options.tool_choice = ToolChoice::Specific("todowrite".to_string());
        }
    }
}

/// Index of the current real-user turn's start (last non-synthetic user message).
fn current_real_user_start(convo: &Conversation) -> usize {
    convo
        .messages
        .iter()
        .rposition(|m| m.role == Role::User && !m.synthetic)
        .unwrap_or(0)
}

/// True iff the completion nudge was already injected in the CURRENT real-user turn — so we
/// nudge at most once; if the model stops again with open items, we let it end.
fn completion_nudge_already_present(convo: &Conversation) -> bool {
    let start = current_real_user_start(convo);
    convo.messages[start..].iter().any(|m| {
        m.role == Role::User
            && m.synthetic
            && m.text.trim_start().starts_with(TODO_COMPLETION_NUDGE)
    })
}

/// True iff the model actively MANAGED the task list this turn (a `todo`/`todowrite` call after
/// the last real-user message). We only nudge when it did — so a stop where the model is asking
/// the user something unrelated to a STALE list from an earlier turn isn't hijacked into a
/// continuation. Mirrors `VerifyCadenceHook`'s narrow "only right after an edit" scoping.
fn managed_todos_this_turn(convo: &Conversation) -> bool {
    let start = current_real_user_start(convo);
    convo.messages[start..].iter().any(|m| {
        m.tool_calls
            .iter()
            .any(|c| c.name == "todo" || c.name == "todowrite")
    })
}

/// The mid-work "reconcile your pointer" anchor prepended to the per-request reminder.
/// Weak models DRIFT: they leave `in_progress` on a task they already finished or moved
/// past (e.g. still on #4 while actually editing #6's code), or work with nothing marked
/// in_progress at all. The full numbered list is already injected below, but the
/// in_progress status is just a `[~]` glyph buried in it — low salience for weak models.
/// This surfaces the current pointer as an explicit imperative every turn so the model
/// re-confronts it BEFORE acting. Deterministic: reads only the derived state, never
/// guesses which task the model "should" be on.
/// - An `in_progress` task → name its `#<id>` + title and force a reconcile.
/// - No `in_progress` but open (pending) items remain → tell it to mark what it's on.
/// - Otherwise (all completed) → `None` (nothing to reconcile; don't add noise).
/// `id` is the 1-based position, matching `render_todos_numbered`.
fn todo_anchor_line(todos: &[TodoItem]) -> Option<String> {
    if let Some(i) = todos
        .iter()
        .position(|t| t.status == TodoStatus::InProgress)
    {
        return Some(format!(
            ">> You are currently ON task #{} \"{}\". Before your NEXT action, reconcile: if it \
is actually DONE, mark it completed now (`{{\"action\":\"update\",\"id\":{},\"status\":\"completed\"}}`); \
if you have moved on to a DIFFERENT task, switch in_progress to THAT id FIRST. Do not leave \
in_progress pointing at a task you are no longer working on.",
            i + 1,
            todos[i].content,
            i + 1
        ));
    }
    if todos.iter().any(|t| t.status == TodoStatus::Pending) {
        return Some(
            ">> NOTHING is in_progress but tasks remain. Before you act, mark the task you are \
actually working on as in_progress (`{\"action\":\"update\",\"id\":<id>,\"status\":\"in_progress\"}`)."
                .to_string(),
        );
    }
    None
}

fn todo_reminder_body(todos: &[TodoItem]) -> String {
    // ASCII-safe body (the model doesn't need glyph prettiness; the TUI renders
    // the pretty version). The anchor line (mid-work drift backstop) leads, so the
    // current in_progress pointer is the first thing in this block — above the list.
    let anchor = todo_anchor_line(todos)
        .map(|a| format!("{a}\n\n"))
        .unwrap_or_default();
    format!(
        "{anchor}Current task list (each line is `#<id> <task>`) — keep it accurate and finish it:\n\
- The MOMENT you START or FINISH items: one `todowrite` with `actions` covering every status change you already know (e.g. complete #1 and set #2 `in_progress` in the SAME array).\n\
- Skip `todowrite` this turn if the list already matches reality. Never re-mark an item already in that status.\n\
- First plan / replace a plan: `actions` of `add`s (plus `clear` first if replacing). Do NOT resend a full `todos` list.\n\
- Do NOT stop, summarize, or hand back while ANY item is still pending or in_progress — keep working through them, unless you truly need approval, are genuinely stuck, or the request is ambiguous.\n\
{}",
        render_todos_numbered(todos, false)
    )
}

#[async_trait]
impl LifecycleHooks for TodoHook {
    async fn pre_request(&self, messages: &mut Vec<Message>, _ctx: &TurnCtx) {
        // Live TUI sync only. Injection happens in `turn_start` (stored, ABOVE the
        // real user query) so tool-loop rounds do not rewind a tail reminder.
        let todos = derive_current_todos(messages);
        self.sync_live(&todos);
    }

    async fn turn_start(&self, convo: &mut Conversation) {
        let todos = derive_current_todos(&convo.messages);
        self.sync_live(&todos);
        if todos.is_empty() {
            return;
        }
        if reminder_already_before_last_real_user(&convo.messages, |t| {
            t.contains("Current task list")
        }) {
            return;
        }
        insert_before_last_real_user(
            &mut convo.messages,
            Message::synthetic_user(system_reminder(&todo_reminder_body(&todos))),
        );
    }

    /// The model wants to stop. If the task list still has OPEN items (pending or in_progress),
    /// inject a one-shot nudge to close them out (or keep working) and continue the turn — the
    /// residual gap where a weak model finishes the last item's work but forgets the final
    /// `todo update`. Fires at most once per real-user turn; `None` otherwise lets it stop.
    async fn offer_continuation(&self, convo: &Conversation) -> Option<String> {
        let todos = derive_current_todos(&convo.messages);
        let has_open = todos.iter().any(|t| t.status != TodoStatus::Completed);
        if !has_open || !managed_todos_this_turn(convo) || completion_nudge_already_present(convo) {
            return None;
        }
        Some(TODO_COMPLETION_NUDGE.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomcode_kernel::hook::{LifecycleHooks, TurnCtx};
    use atomcode_kernel::message::{Message, Role};
    use atomcode_kernel::tool::ToolCall;

    fn todowrite_msg(args: &str) -> Message {
        Message::assistant(
            "",
            vec![ToolCall {
                id: "1".into(),
                name: "todowrite".into(),
                arguments: args.into(),
            }],
        )
    }

    // ---- mid-work drift backstop: the anchor line ------------------------------------------

    fn item(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.into(),
            status,
        }
    }

    #[test]
    fn anchor_names_in_progress_id_and_title() {
        // #2 is in_progress → anchor must name that exact id + title and force a reconcile.
        let todos = vec![
            item("first", TodoStatus::Completed),
            item("do the thing", TodoStatus::InProgress),
            item("later", TodoStatus::Pending),
        ];
        let a = todo_anchor_line(&todos).expect("in_progress → anchor");
        assert!(a.contains("#2"), "must name the 1-based id: {a}");
        assert!(a.contains("do the thing"), "must name the title: {a}");
        assert!(
            a.contains("reconcile") && a.contains("moved on"),
            "must force reconcile: {a}"
        );
    }

    #[test]
    fn anchor_when_nothing_in_progress_but_open_items_remain() {
        // No in_progress, but a pending item exists → tell the model to mark what it's on.
        let todos = vec![
            item("first", TodoStatus::Completed),
            item("second", TodoStatus::Pending),
        ];
        let a = todo_anchor_line(&todos).expect("open + no in_progress → anchor");
        assert!(a.contains("NOTHING is in_progress"), "{a}");
        assert!(a.contains("in_progress"), "must tell it to mark one: {a}");
    }

    #[test]
    fn no_anchor_when_all_completed() {
        // Everything done → nothing to reconcile; don't add noise.
        let todos = vec![
            item("a", TodoStatus::Completed),
            item("b", TodoStatus::Completed),
        ];
        assert!(todo_anchor_line(&todos).is_none());
    }

    fn convo_with(msgs: Vec<Message>) -> Conversation {
        let mut c = Conversation::default();
        c.messages = msgs;
        c
    }

    fn reminder_before_query(convo: &Conversation) -> &Message {
        let i = convo
            .messages
            .iter()
            .rposition(|m| m.role == Role::User && !m.synthetic)
            .expect("real user query");
        assert!(i > 0, "reminder must sit above the query");
        &convo.messages[i - 1]
    }

    #[tokio::test]
    async fn turn_start_prepends_anchor_for_in_progress() {
        let mut convo = convo_with(vec![
            Message::user("do it"),
            todowrite_msg(r#"{"todos":[{"content":"step one","status":"in_progress"}]}"#),
        ]);
        TodoHook::new().turn_start(&mut convo).await;
        let reminder = reminder_before_query(&convo);
        assert!(
            reminder.text.contains("currently ON task #1"),
            "anchor must lead: {}",
            reminder.text
        );
        assert!(
            reminder.text.contains("step one"),
            "anchor must name the task: {}",
            reminder.text
        );
        assert!(
            reminder
                .text
                .contains("Skip `todowrite` this turn if the list already matches"),
            "must discourage no-op updates: {}",
            reminder.text
        );
        let anchor_at = reminder.text.find("currently ON task").unwrap();
        let list_at = reminder.text.find("Current task list").unwrap();
        assert!(
            anchor_at < list_at,
            "anchor must come before the list: {}",
            reminder.text
        );
        assert_eq!(convo.messages.last().unwrap().role, Role::Assistant);
    }

    #[tokio::test]
    async fn injects_reminder_above_the_user_query() {
        let mut convo = convo_with(vec![
            Message::user("do the thing"),
            todowrite_msg(r#"{"todos":[{"content":"step one","status":"in_progress"}]}"#),
        ]);
        let before = convo.messages.len();
        TodoHook::new().turn_start(&mut convo).await;
        assert_eq!(convo.messages.len(), before + 1, "one reminder inserted");
        let reminder = reminder_before_query(&convo);
        assert_eq!(reminder.role, Role::User);
        assert!(reminder.synthetic);
        assert!(
            reminder.text.contains("system-reminder"),
            "{}",
            reminder.text
        );
        assert!(reminder.text.contains("step one"), "{}", reminder.text);
        assert_eq!(convo.messages[1].text, "do the thing");
    }

    #[tokio::test]
    async fn hook_syncs_live_so_execute_rejects_unknown_ids() {
        use atomcode_capabilities::tools::TodoTool;
        use atomcode_kernel::tool::{Tool, ToolContext};
        use tokio_util::sync::CancellationToken;

        let tool = TodoTool::new();
        let hook = TodoHook::with_live(tool.live());
        let msgs = vec![
            Message::user("do it"),
            todowrite_msg(r#"{"todos":[{"content":"only","status":"pending"}]}"#),
        ];
        let mut convo = convo_with(msgs);
        hook.turn_start(&mut convo).await;

        let ctx = ToolContext {
            working_dir: std::path::PathBuf::from("."),
            cancel: CancellationToken::new(),
            progress: atomcode_kernel::tool::ProgressSink::noop(),
            requester: None,
        };
        let bad = tool
            .execute(r#"{"action":"update","id":3,"status":"completed"}"#, &ctx)
            .await;
        assert!(bad.is_error, "{}", bad.content);
        assert!(bad.content.contains("only"), "{}", bad.content);
        assert!(bad.content.contains("Current list"), "{}", bad.content);
    }

    #[tokio::test]
    async fn no_injection_when_no_list() {
        let mut convo = convo_with(vec![
            Message::user("hi"),
            Message::assistant("hello", vec![]),
        ]);
        let before = convo.messages.len();
        TodoHook::new().turn_start(&mut convo).await;
        assert_eq!(convo.messages.len(), before, "empty list → no injection");
    }

    #[tokio::test]
    async fn auto_prefers_deepseek_v4_flash_on_each_new_task() {
        let hook = TodoEagerHook::new("deepseek-v4-flash", "openai", TodoEagerness::Auto);
        let mut convo = convo_with(vec![Message::user("analyze and fix this")]);
        hook.turn_start(&mut convo).await;
        assert!(
            convo
                .messages
                .iter()
                .any(|m| m.synthetic && m.text.contains("todowrite")),
            "eager nudge must sit above the query: {:?}",
            convo
                .messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(convo.messages.last().unwrap().text, "analyze and fix this");
    }

    #[tokio::test]
    async fn auto_policy_is_resolved_again_for_a_model_generation() {
        let mut ordinary = convo_with(vec![Message::user("analyze and fix this")]);
        TodoEagerHook::new("ordinary-model", "openai", TodoEagerness::Auto)
            .turn_start(&mut ordinary)
            .await;
        assert_eq!(ordinary.messages.len(), 1, "ordinary Auto stays quiet");

        let mut deepseek = convo_with(vec![Message::user("analyze and fix this")]);
        TodoEagerHook::new("deepseek-v4-flash", "openai", TodoEagerness::Auto)
            .turn_start(&mut deepseek)
            .await;
        assert_eq!(
            deepseek.messages.len(),
            2,
            "new DeepSeek generation gets the nudge"
        );
        assert_eq!(
            deepseek.messages.last().unwrap().text,
            "analyze and fix this"
        );
    }

    #[tokio::test]
    async fn eager_nudge_is_stored_on_turn_start_and_pre_request_stays_append_only() {
        let hook = TodoEagerHook::new("deepseek-v4-flash", "openai", TodoEagerness::Auto);
        let mut convo = convo_with(vec![Message::user("analyze and fix this")]);
        hook.turn_start(&mut convo).await;
        let stored = convo.messages.clone();
        assert!(
            stored.len() > 1,
            "turn_start must persist the nudge so tool-loop rounds stay a prefix"
        );
        hook.turn_start(&mut convo).await;
        assert_eq!(
            convo.messages, stored,
            "second turn_start on the same query is idempotent"
        );

        let mut outgoing = stored.clone();
        hook.pre_request(
            &mut outgoing,
            &TurnCtx {
                round: 1,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            outgoing, stored,
            "pre_request must not rewrite history — inserting above the query poisons the prefix cache"
        );
    }

    #[tokio::test]
    async fn always_selects_todowrite_only_without_an_existing_list() {
        let hook = TodoEagerHook::new("any-model", "openai", TodoEagerness::Always);
        let ctx = TurnCtx {
            round: 1,
            ..Default::default()
        };
        let messages = vec![Message::user("do several things")];
        let mut options = ChatOptions::default();
        hook.pre_request_options(&messages, &mut options, &ctx)
            .await;
        assert_eq!(
            options.tool_choice,
            ToolChoice::Specific("todowrite".into())
        );

        let with_list = vec![
            Message::user("continue"),
            todowrite_msg(r#"{"todos":[{"content":"a","status":"pending"}]}"#),
        ];
        let mut options = ChatOptions::default();
        hook.pre_request_options(&with_list, &mut options, &ctx)
            .await;
        assert_eq!(options.tool_choice, ToolChoice::Auto);

        let completed_list = vec![
            Message::user("finish old task"),
            todowrite_msg(r#"{"todos":[{"content":"old","status":"completed"}]}"#),
            Message::user("start a different task"),
        ];
        let mut options = ChatOptions::default();
        hook.pre_request_options(&completed_list, &mut options, &ctx)
            .await;
        assert_eq!(
            options.tool_choice,
            ToolChoice::Specific("todowrite".into()),
            "a completed historical list must not suppress planning for a new task"
        );
    }

    #[test]
    fn always_degrades_explicitly_for_ollama() {
        let hook = TodoEagerHook::new("any-model", "ollama", TodoEagerness::Always);
        assert_eq!(hook.eagerness, TodoEagerness::Preferred);
    }

    #[test]
    fn always_remains_strict_for_supported_adapters() {
        let hook = TodoEagerHook::new("any-model", "openai", TodoEagerness::Always);
        assert_eq!(hook.eagerness, TodoEagerness::Always);
    }

    // ---- offer_continuation: close out the last item ---------------------------------------

    fn convo_of(msgs: Vec<Message>) -> Conversation {
        let mut c = Conversation::new();
        c.messages = msgs;
        c
    }

    #[tokio::test]
    async fn nudges_to_close_out_open_items_on_stop() {
        // The reported gap: the model produced its final summary but left an item open.
        let convo = convo_of(vec![
            Message::user("do the audit"),
            todowrite_msg(
                r#"{"todos":[{"content":"a","status":"completed"},{"content":"b","status":"in_progress"}]}"#,
            ),
            Message::assistant("here is the summary…", vec![]),
        ]);
        assert!(
            TodoHook::new().offer_continuation(&convo).await.is_some(),
            "open item on stop must nudge"
        );
    }

    #[tokio::test]
    async fn no_nudge_when_all_completed() {
        let convo = convo_of(vec![
            Message::user("do it"),
            todowrite_msg(
                r#"{"todos":[{"content":"a","status":"completed"},{"content":"b","status":"completed"}]}"#,
            ),
            Message::assistant("all done", vec![]),
        ]);
        assert!(
            TodoHook::new().offer_continuation(&convo).await.is_none(),
            "all completed → let it stop"
        );
    }

    #[tokio::test]
    async fn no_nudge_when_no_todos() {
        let convo = convo_of(vec![
            Message::user("hi"),
            Message::assistant("hi there", vec![]),
        ]);
        assert!(TodoHook::new().offer_continuation(&convo).await.is_none());
    }

    #[tokio::test]
    async fn no_nudge_when_list_untouched_this_turn() {
        // An open item lingers from a PRIOR turn, but this turn the model only answered a
        // question (no todo/todowrite call) → don't hijack the stop into a continuation.
        let convo = convo_of(vec![
            Message::user("plan it"),
            todowrite_msg(r#"{"todos":[{"content":"a","status":"in_progress"}]}"#),
            Message::assistant("planned", vec![]),
            Message::user("what does foo do?"),
            Message::assistant("foo does X.", vec![]),
        ]);
        assert!(
            TodoHook::new().offer_continuation(&convo).await.is_none(),
            "a stale open list not touched this turn must not force a continuation"
        );
    }

    #[tokio::test]
    async fn nudges_at_most_once_per_turn() {
        let mut convo = convo_of(vec![
            Message::user("do it"),
            todowrite_msg(r#"{"todos":[{"content":"a","status":"in_progress"}]}"#),
            Message::assistant("summary", vec![]),
        ]);
        assert!(
            TodoHook::new().offer_continuation(&convo).await.is_some(),
            "first stop nudges"
        );
        // Kernel injected the nudge as a synthetic user message; model stops again without closing.
        convo
            .messages
            .push(Message::synthetic_user(TODO_COMPLETION_NUDGE));
        convo
            .messages
            .push(Message::assistant("still open", vec![]));
        assert!(
            TodoHook::new().offer_continuation(&convo).await.is_none(),
            "already nudged this turn → let it stop (no spin)"
        );
    }
}
