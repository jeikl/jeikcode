//! `StatusReminderHook` — a per-turn `<system-reminder>` tail carrying live runtime status
//! (date + round budget) so the model can pace itself and resolve relative dates ("yesterday")
//! into concrete `after`/`before` for [`recall`](super::recall). This is the **sole**
//! model-facing calendar date (the persona no longer freezes a duplicate `Today's date`).
//! Deliberately DATE-only (no wall-clock time) and with NO context-usage gauge — context
//! pressure is handled silently by auto-compaction, never pushed to the model (see `render`).
//!
//! Cache-safety: appended once per user turn in [`LifecycleHooks::turn_start`] to the
//! **BOTTOM of the current real user message**. This keeps one wire-level user block,
//! avoids role confusion from a separate synthetic user instruction, and remains
//! append-only across tool-loop rounds. An existing date reminder is never rewritten
//! (keyword list changes land on the *next* user turn only). Session-temp long-bash
//! keywords are included only when that overlay is non-empty — same empty-hides
//! lifecycle as the WebUI todo panel. Global `long_bash_command_keyword` is never
//! injected here.
//!
//! The body is wrapped in `<system-reminder>…</system-reminder>` so the model reads it as
//! INJECTED CONTEXT, not the user's own words (matching `PlanModeReminderHook`'s convention).
//! Wall-clock lives in L1 (the kernel is clock-free); this reads the system-local time.

use async_trait::async_trait;
use atomcode_kernel::hook::{LifecycleHooks, TurnCtx};
use atomcode_kernel::message::{Conversation, Role};
use chrono::{DateTime, Local};

/// Injects a `<system-reminder>` status tail from round 2 of each turn onward.
pub struct StatusReminderHook;

impl StatusReminderHook {
    pub fn new() -> Self {
        Self
    }

    /// Build the `<system-reminder>` body from wall-clock `now` and the turn context. Pure
    /// (clock + ctx injected) so it is unit-testable without a running agent.
    fn render(now: DateTime<Local>, ctx: &TurnCtx) -> String {
        let mut lines = Vec::with_capacity(2);
        // Date + weekday only — NO wall-clock time. The minute-level clock made chatty weak
        // models (e.g. deepseek-v4-flash) editorialize about the hour ("要休息了吗？快 1 点了")
        // instead of working, and relative-date resolution for `recall` needs only the date.
        lines.push(format!(
            "Current date: {} ({})",
            now.format("%Y-%m-%d"),
            now.format("%a")
        ));
        // NO context-window usage line. Pushing a running "X / Y tokens used (Z%)" gauge to the
        // model is a net negative: weak models (deepseek-v4-flash) read it as "almost full" and
        // either rush to a FALSE completion or start reminding the USER to compact — the exact
        // failure we kept seeing. Context pressure is handled where it belongs: SILENTLY, by the
        // auto-compaction trigger (`should_compact` / `AUTO_DRAIN_UTILIZATION`), never surfaced to
        // the model. The user keeps full visibility via the footer gauge and `/context`. This
        // matches codex (auto-compact at a token limit, model never told the %) and opencode
        // (`isOverflow` → silent summarize, model never told remaining tokens). `TurnCtx` still
        // carries `context_window`/`used_tokens` for other consumers (e.g. cc-hooks).
        // Turn round counter — the CURRENT round only, deliberately WITHOUT the
        // `of {max} (max)` ceiling. Surfacing the ceiling ("48 of 50 (max)")
        // reads as a countdown that pressures weaker models (deepseek-v4-flash)
        // into rushing to declare the task "done" before running out of rounds
        // — a FALSE completion that unravels on the next question. A bare
        // progress counter keeps pacing awareness without the countdown.
        // (codex exposes remaining budget as an on-demand tool, not a per-turn
        // push; opencode injects no countdown at all. The anti-false-completion
        // guardrail lives in the persona's EXECUTION DISCIPLINE section.)
        // (`ctx.max_rounds` stays available to other hooks — e.g. cc-hooks in
        // hooks.rs — it is simply no longer surfaced to the model here.)
        lines.push(format!("Turn round: {}", ctx.round));
        #[cfg(feature = "tools")]
        append_session_long_keyword_lines(&mut lines);
        crate::reminder::system_reminder(&lines.join("\n"))
    }

    fn render_turn_start(now: DateTime<Local>) -> String {
        crate::reminder::system_reminder(&Self::date_and_session_keywords(now))
    }

    /// One reminder body: date, then the session long-bash list only when
    /// that overlay has entries. Never a second `<system-reminder>` wrap.
    fn date_and_session_keywords(now: DateTime<Local>) -> String {
        let mut lines = vec![format!(
            "Current date: {} ({})",
            now.format("%Y-%m-%d"),
            now.format("%a")
        )];
        #[cfg(feature = "tools")]
        append_session_long_keyword_lines(&mut lines);
        lines.join("\n")
    }
}

#[cfg(feature = "tools")]
fn append_session_long_keyword_lines(lines: &mut Vec<String>) {
    // Session overlay only. Config.toml `long_bash_command_keyword` is global
    // and must not appear here. Empty list → omit (same lifecycle as WebUI todos).
    let kws = crate::tools::bash_runtime::session_long_keywords();
    if kws.is_empty() {
        return;
    }
    lines.push(format!(
        "暂存长bash列表：{}（如果长bash运行效果不达预期，可通过 long_bash_keyword_actions action=delete 取消）",
        kws.join("、")
    ));
}

impl Default for StatusReminderHook {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LifecycleHooks for StatusReminderHook {
    async fn turn_start(&self, convo: &mut Conversation) {
        let Some(query) = convo
            .messages
            .iter_mut()
            .rfind(|m| m.role == Role::User && !m.synthetic)
        else {
            return;
        };
        if query.text.contains("<system-reminder>\nCurrent date:") {
            return;
        }
        query.text.push_str("\n\n");
        query.text.push_str(&Self::render_turn_start(Local::now()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomcode_kernel::message::Message;
    use chrono::TimeZone;

    fn ctx(round: u32, window: u32, used: u32) -> TurnCtx {
        TurnCtx {
            round,
            max_rounds: Some(50),
            context_window: window,
            used_tokens: used,
            ..Default::default()
        }
    }

    #[test]
    fn render_has_date_context_and_round_wrapped() {
        let dt = Local
            .with_ymd_and_hms(2026, 6, 15, 17, 34, 0)
            .single()
            .unwrap();
        let s = StatusReminderHook::render(dt, &ctx(3, 128_000, 40_000));
        assert!(
            s.starts_with("<system-reminder>") && s.ends_with("</system-reminder>"),
            "must be wrapped so the model knows it's injected: {s}"
        );
        // Date + weekday only (no wall-clock HH:MM): the minute-level time made chatty weak
        // models (deepseek-v4-flash) editorialize ("要休息了吗？快 1 点了"), and relative-date
        // resolution for `recall` needs only the date.
        assert!(s.contains("Current date: 2026-06-15 (Mon)"), "{s}");
        assert!(
            !s.contains("local time"),
            "must not carry wall-clock time: {s}"
        );
        assert!(!s.contains("17:34"), "must not carry wall-clock time: {s}");
        // NO context-usage gauge is pushed to the model, even when the window IS known —
        // pressure is handled silently by auto-compaction (matches codex/opencode).
        assert!(
            !s.contains("Context window"),
            "must not push a context-usage gauge: {s}"
        );
        assert!(!s.contains('%'), "must not push any usage percentage: {s}");
        // Round counter shows the CURRENT round only — the `of N (max)` ceiling
        // is deliberately NOT surfaced (countdown pressures weak models into
        // false completions; see render() + persona EXECUTION DISCIPLINE).
        assert!(s.contains("Turn round: 3"), "round counter: {s}");
        assert!(!s.contains("(max)"), "no countdown ceiling: {s}");
        assert!(!s.contains("of 50"), "no `of N` countdown framing: {s}");
    }

    #[test]
    fn render_never_injects_context_usage_even_at_high_pressure() {
        // The window is known AND nearly full — the exact case the old code injected a scary
        // "Context window: … (95%)". It must NOT be surfaced to the model: pressure is handled
        // silently by auto-compaction, and pushing the gauge made weak models false-complete or
        // nag the user to compact. Only date + round remain.
        let dt = Local
            .with_ymd_and_hms(2026, 6, 15, 9, 0, 0)
            .single()
            .unwrap();
        let s = StatusReminderHook::render(dt, &ctx(2, 128_000, 121_600));
        assert!(
            !s.contains("Context window"),
            "no context-usage gauge pushed to the model: {s}"
        );
        assert!(
            !s.contains('%'),
            "no usage percentage pushed to the model: {s}"
        );
        assert!(s.contains("Current date"), "date is still carried: {s}");
        assert!(s.contains("Turn round: 2"), "{s}");
        assert!(!s.contains("(max)"), "no countdown ceiling: {s}");
    }

    #[test]
    #[cfg(feature = "tools")]
    fn render_injects_session_long_keywords_when_present() {
        let prev = crate::tools::bash_runtime::session_long_keywords();
        crate::tools::bash_runtime::set_live_long_keywords(vec![
            "ninja".into(),
            "webpack".into(),
        ]);
        let dt = Local
            .with_ymd_and_hms(2026, 6, 15, 9, 0, 0)
            .single()
            .unwrap();
        let s = StatusReminderHook::render(dt, &ctx(2, 128_000, 1_000));
        crate::tools::bash_runtime::set_live_long_keywords(prev);
        assert_eq!(
            s.matches("<system-reminder>").count(),
            1,
            "date and long-bash list must share one reminder: {s}"
        );
        assert!(
            s.contains("Current date: 2026-06-15 (Mon)"),
            "date stays in the same reminder: {s}"
        );
        assert!(
            s.contains("暂存长bash列表：ninja、webpack")
                && s.contains("long_bash_keyword_actions")
                && s.contains("action=delete"),
            "{s}"
        );
    }

    #[test]
    #[cfg(feature = "tools")]
    fn render_omits_session_keywords_when_overlay_is_empty() {
        let prev = crate::tools::bash_runtime::session_long_keywords();
        crate::tools::bash_runtime::set_live_long_keywords(Vec::new());
        let dt = Local
            .with_ymd_and_hms(2026, 6, 15, 9, 0, 0)
            .single()
            .unwrap();
        let s = StatusReminderHook::render(dt, &ctx(2, 128_000, 1_000));
        crate::tools::bash_runtime::set_live_long_keywords(prev);
        assert!(s.contains("Current date"), "{s}");
        assert!(
            !s.contains("暂存长bash列表"),
            "empty session overlay must not inject: {s}"
        );
    }

    #[tokio::test]
    #[cfg(feature = "tools")]
    async fn turn_start_keeps_prefix_stable_when_keywords_appear_later() {
        let prev = crate::tools::bash_runtime::session_long_keywords();
        crate::tools::bash_runtime::set_live_long_keywords(Vec::new());
        let hook = StatusReminderHook::new();
        let mut convo = Conversation::default();
        convo.messages = vec![Message::user("你好")];
        hook.turn_start(&mut convo).await;
        let first = convo.messages[0].text.clone();
        assert!(
            first.starts_with("你好\n\n<system-reminder>\nCurrent date:")
                && first.matches("<system-reminder>").count() == 1
                && !first.contains("暂存长bash列表"),
            "first injection is date-only in one reminder: {first}"
        );

        crate::tools::bash_runtime::set_live_long_keywords(vec!["ninja".into()]);
        hook.turn_start(&mut convo).await;
        assert_eq!(
            convo.messages[0].text, first,
            "already-written reminder must not be rewritten (prefix stability)"
        );

        convo.messages.push(Message::user("下一句"));
        hook.turn_start(&mut convo).await;
        let second = &convo.messages[1].text;
        assert!(
            second.starts_with("下一句\n\n<system-reminder>\nCurrent date:")
                && second.contains("暂存长bash列表：ninja")
                && second.matches("<system-reminder>").count() == 1,
            "next user turn gets date + list in the same reminder: {second}"
        );
        assert_eq!(convo.messages[0].text, first);
        crate::tools::bash_runtime::set_live_long_keywords(prev);
    }

    #[tokio::test]
    async fn turn_start_appends_date_to_query_bottom() {
        let hook = StatusReminderHook::new();
        let mut convo = Conversation::default();
        convo.messages = vec![Message::system("s"), Message::user("hi")];
        hook.turn_start(&mut convo).await;
        assert_eq!(convo.messages.len(), 2);
        assert!(!convo.messages[1].synthetic);
        assert!(
            convo.messages[1]
                .text
                .starts_with("hi\n\n<system-reminder>")
                && convo.messages[1].text.contains("Current date"),
            "date reminder is appended inside the real query block: {:?}",
            convo.messages[1].text
        );
        assert!(!convo.messages[1].text.contains("Turn round"));
        let before = convo.messages.clone();
        hook.turn_start(&mut convo).await;
        assert_eq!(
            convo.messages, before,
            "second turn_start on the same query is idempotent"
        );
    }
}
