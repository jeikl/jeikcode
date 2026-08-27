//! The `<system-reminder>` convention — the ONE place that owns it.
//!
//! Runtime context is always wrapped in `<system-reminder>…</system-reminder>`.
//! Most mode/todo notices remain synthetic `Role::User` messages immediately above
//! the real query. The per-turn calendar date from [`StatusReminderHook`] is the
//! deliberate exception: it is appended to the bottom of that real user block so it
//! cannot become an independent user-authored instruction on the wire.
//!
//! EVERY injector MUST build its reminder through [`system_reminder`] — that is the whole
//! point of centralizing the convention: the wrapper can no longer be forgotten. The bug
//! that motivated this: the date line was once emitted BARE (no wrapper), so the model read
//! it as the user's own words.
//!
//! [`StatusReminderHook`]: crate::session::StatusReminderHook

/// The reminder tag name (without angle brackets) — the single source of truth. A matcher
/// that wants to detect a reminder builds its needle from this (e.g. `format!("<{TAG}>")`).
pub const SYSTEM_REMINDER_TAG: &str = "system-reminder";

/// Wrap `body` in a `<system-reminder>…</system-reminder>` block. THE constructor for every
/// runtime reminder injected into the conversation — call this instead of writing the tags
/// by hand so the wrapper can never be forgotten.
pub fn system_reminder(body: &str) -> String {
    format!("<{SYSTEM_REMINDER_TAG}>\n{body}\n</{SYSTEM_REMINDER_TAG}>")
}

/// Whether `text` is a system-injected reminder block — i.e. begins with the
/// canonical opening tag. Reminders are pushed as ordinary `Role::User`
/// messages (see `cc_hooks.rs`, `plan_mode.rs`, …), so user-facing consumers
/// that scan for "the first user message" (session auto-naming, AI title
/// generation) must skip them or they would name the session after ambient
/// runtime context instead of the user's own words.
pub fn is_system_reminder(text: &str) -> bool {
    let opening = format!("<{SYSTEM_REMINDER_TAG}>");
    text.trim_start().starts_with(&opening)
}

/// Insert `item` immediately before the last real (non-synthetic) user message.
///
/// Grok Build sends ambient user blocks first and the `<user_query>` last. Putting
/// the reminder at the tail used to steal recency; inserting here keeps the query
/// as the last user turn. If there is no real user yet, the item is appended.
pub fn insert_before_last_real_user(
    messages: &mut Vec<atomcode_kernel::message::Message>,
    item: atomcode_kernel::message::Message,
) {
    use atomcode_kernel::message::Role;
    let pos = messages
        .iter()
        .rposition(|m| m.role == Role::User && !m.synthetic)
        .unwrap_or(messages.len());
    messages.insert(pos, item);
}

/// True when the slot immediately before the last real user is already a reminder
/// matching `pred`. Lets `turn_start` injectors stay idempotent.
pub fn reminder_already_before_last_real_user(
    messages: &[atomcode_kernel::message::Message],
    pred: impl Fn(&str) -> bool,
) -> bool {
    use atomcode_kernel::message::Role;
    let Some(i) = messages
        .iter()
        .rposition(|m| m.role == Role::User && !m.synthetic)
    else {
        return false;
    };
    i > 0
        && messages[i - 1].synthetic
        && is_system_reminder(&messages[i - 1].text)
        && pred(&messages[i - 1].text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_body_in_the_canonical_tag() {
        let r = system_reminder("line one\nline two");
        assert_eq!(
            r,
            "<system-reminder>\nline one\nline two\n</system-reminder>"
        );
        assert!(r.starts_with(&format!("<{SYSTEM_REMINDER_TAG}>")));
        assert!(r.ends_with(&format!("</{SYSTEM_REMINDER_TAG}>")));
    }

    #[test]
    fn detects_wrapped_reminders_but_not_plain_user_text() {
        assert!(is_system_reminder(&system_reminder("日期：2026-08-09")));
        assert!(is_system_reminder(
            "  <system-reminder>\n注意\n</system-reminder>"
        ));
        assert!(!is_system_reminder("我提到了 <system-reminder> 这个词"));
        assert!(!is_system_reminder("修复登录错误"));
        assert!(!is_system_reminder(""));
    }

    #[test]
    fn insert_before_last_real_user_puts_reminder_above_query() {
        use atomcode_kernel::message::Message;
        let mut msgs = vec![Message::system("sys"), Message::user("the query")];
        insert_before_last_real_user(
            &mut msgs,
            Message::synthetic_user(system_reminder("ambient")),
        );
        assert_eq!(msgs.len(), 3);
        assert!(msgs[1].synthetic && is_system_reminder(&msgs[1].text));
        assert_eq!(msgs[2].text, "the query");
        assert!(!msgs[2].synthetic);
    }

    #[test]
    fn insert_before_last_real_user_skips_earlier_real_users() {
        use atomcode_kernel::message::Message;
        let mut msgs = vec![
            Message::user("first"),
            Message::assistant("ok", vec![]),
            Message::user("second"),
        ];
        insert_before_last_real_user(&mut msgs, Message::synthetic_user(system_reminder("r")));
        assert_eq!(msgs[0].text, "first");
        assert!(msgs[2].synthetic);
        assert_eq!(msgs[3].text, "second");
    }
}
