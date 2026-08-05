//! Streaming projectors for OpenAI Chat Completions / Responses / Anthropic Messages.
//!
//! Parallel-safe tool events (stable call `id`) + structured `task` subagent progress.

use serde_json::{json, Value};

use crate::ChatEvent;

use super::{now_unix, WireFormat};

pub(super) struct SseChunk {
    pub event: Option<String>,
    pub data: String,
}

/// Parsed `task` subagent progress line (from `SUBAGENT_ACTIVITY_MARKER` / `\u{1e}`).
#[derive(Debug, Clone)]
struct SubtaskProgressPatch {
    subtask_id: String,
    state: &'static str,
    label: String,
    model: Option<String>,
    message: String,
    description: Option<String>,
    tokens: Option<u64>,
}

fn parse_subtask_progress(raw: &str) -> Option<SubtaskProgressPatch> {
    let chunk = raw
        .strip_prefix(atomcode_capabilities::tools::task::SUBAGENT_ACTIVITY_MARKER)
        .unwrap_or(raw)
        .trim();
    if chunk.is_empty() {
        return None;
    }
    let parts: Vec<&str> = chunk.split(" \u{b7} ").collect();
    if parts.is_empty() {
        return None;
    }
    let head = parts[0];

    // ○ queued · explore#1 · model · desc
    if head == "\u{25cb} queued" || head.ends_with(" queued") {
        let label = parts.get(1).unwrap_or(&"").to_string();
        if label.is_empty() {
            return None;
        }
        return Some(SubtaskProgressPatch {
            subtask_id: label.clone(),
            state: "queued",
            label: label.clone(),
            model: parts.get(2).map(|s| (*s).to_string()),
            message: chunk.to_string(),
            description: parts.get(3).map(|s| (*s).to_string()),
            tokens: None,
        });
    }
    // ↻ explore#1 · model · desc
    if let Some(label) = head.strip_prefix("\u{21bb} ") {
        return Some(SubtaskProgressPatch {
            subtask_id: label.to_string(),
            state: "running",
            label: label.to_string(),
            model: parts.get(1).map(|s| (*s).to_string()),
            message: chunk.to_string(),
            description: parts.get(2).map(|s| (*s).to_string()),
            tokens: None,
        });
    }
    // ✓ done · explore#1 · model · desc  (head may be "✓ done" after split)
    if head.starts_with("\u{2713} done") || head == "\u{2713} done" {
        let label = parts
            .get(1)
            .map(|s| (*s).to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "subtask".into());
        return Some(SubtaskProgressPatch {
            subtask_id: label.clone(),
            state: "completed",
            label: label.clone(),
            model: parts.get(2).map(|s| (*s).to_string()),
            message: chunk.to_string(),
            description: parts.get(3).map(|s| (*s).to_string()),
            tokens: None,
        });
    }
    // ✗ failed (…) · explore#1
    if head.starts_with("\u{2717}") {
        let label = parts
            .get(1)
            .map(|s| (*s).to_string())
            .unwrap_or_else(|| "subtask".into());
        return Some(SubtaskProgressPatch {
            subtask_id: label.clone(),
            state: "failed",
            label,
            model: parts.get(2).map(|s| (*s).to_string()),
            message: chunk.to_string(),
            description: None,
            tokens: None,
        });
    }
    // explore#1 · activity · tokens=N
    if head.contains('#')
        && (head.starts_with("explore")
            || head.starts_with("worker")
            || head.contains("explore#")
            || head.contains("worker#"))
    {
        let tokens = parts.iter().find_map(|p| {
            p.strip_prefix("tokens=")
                .and_then(|n| n.parse::<u64>().ok())
        });
        let activity = parts.get(1).map(|s| (*s).to_string());
        return Some(SubtaskProgressPatch {
            subtask_id: head.to_string(),
            state: "running",
            label: head.to_string(),
            model: None,
            message: activity.unwrap_or_else(|| chunk.to_string()),
            description: None,
            tokens,
        });
    }
    None
}

fn initial_children_from_task_args(arguments: &str) -> Vec<Value> {
    let repaired = atomcode_capabilities::tools::repair::repair_tool_args("task", arguments);
    let Ok(value) = serde_json::from_str::<Value>(&repaired) else {
        return Vec::new();
    };
    let Some(tasks) = value.get("tasks").and_then(|t| t.as_array()) else {
        return Vec::new();
    };
    tasks
        .iter()
        .enumerate()
        .map(|(index, task)| {
            let kind = task
                .get("subagent_type")
                .and_then(|v| v.as_str())
                .unwrap_or("explore");
            let description = task
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let id = format!("{kind}#{}", index + 1);
            json!({
                "id": id,
                "state": "queued",
                "label": id,
                "description": description,
            })
        })
        .collect()
}

fn progress_to_json(p: &SubtaskProgressPatch) -> Value {
    let mut v = json!({
        "subtask_id": p.subtask_id,
        "state": p.state,
        "label": p.label,
        "message": p.message,
    });
    if let Some(m) = &p.model {
        v["model"] = json!(m);
    }
    if let Some(d) = &p.description {
        v["description"] = json!(d);
    }
    if let Some(t) = p.tokens {
        v["tokens"] = json!(t);
    }
    v
}

pub(super) struct CompatProjector {
    format: WireFormat,
    id: String,
    model: String,
    created: u64,
    started: bool,
    next_block: usize,
    text_block: Option<usize>,
    thinking_block: Option<usize>,
    tool_blocks: std::collections::HashMap<String, usize>,
    tool_call_index: std::collections::HashMap<String, usize>,
    next_tool_index: usize,
    tool_names: std::collections::HashMap<String, String>,
    fc_item_ids: std::collections::HashMap<String, String>,
    response_id: String,
    session_key: Option<String>,
    /// Last emitted OpenAI `reasoning_content` did not end with `\n`.
    /// Used so tool/subagent timeline lines don't glue onto model thinking.
    reasoning_needs_nl: bool,
    /// Stable order of subagent rows in the live panel.
    subagent_order: Vec<String>,
    /// Latest short status text per subagent label (without `子代理 ` prefix).
    subagent_status: std::collections::HashMap<String, String>,
    /// How many terminal rows the last panel paint occupied (for ANSI cursor-up).
    subagent_panel_rows: usize,
}

impl CompatProjector {
    pub(super) fn openai_chat(
        id: String,
        model: String,
        created: u64,
        session_key: Option<String>,
    ) -> Self {
        Self::new(WireFormat::OpenAiChat, id, model, created, session_key)
    }

    pub(super) fn openai_responses(
        id: String,
        model: String,
        created: u64,
        session_key: Option<String>,
    ) -> Self {
        Self::new(WireFormat::OpenAiResponses, id, model, created, session_key)
    }

    pub(super) fn anthropic(id: String, model: String, session_key: Option<String>) -> Self {
        Self::new(WireFormat::Anthropic, id, model, now_unix(), session_key)
    }

    fn new(
        format: WireFormat,
        id: String,
        model: String,
        created: u64,
        session_key: Option<String>,
    ) -> Self {
        Self {
            format,
            id: id.clone(),
            model,
            created,
            started: false,
            next_block: 0,
            text_block: None,
            thinking_block: None,
            tool_blocks: Default::default(),
            tool_call_index: Default::default(),
            next_tool_index: 0,
            tool_names: Default::default(),
            fc_item_ids: Default::default(),
            response_id: id,
            session_key,
            reasoning_needs_nl: false,
            subagent_order: Vec::new(),
            subagent_status: Default::default(),
            subagent_panel_rows: 0,
        }
    }

    fn note_reasoning_tail(&mut self, text: &str) {
        self.reasoning_needs_nl = !text.is_empty() && !text.ends_with('\n');
    }

    /// Emit a standalone progress line into `reasoning_content`.
    ///
    /// Always uses a **leading** `\n` so lines stay separated even when the client
    /// trims trailing newlines from each SSE delta (common in OpenAI-style UIs).
    fn openai_progress_chunk(&mut self, line: &str) -> SseChunk {
        // Tool start/done close the subagent panel (leave final rows on screen).
        self.subagent_panel_rows = 0;
        let body = line.trim_matches(|c| c == '\n' || c == '\r');
        let text = if self.reasoning_needs_nl {
            format!("\n\n{body}\n")
        } else {
            format!("\n{body}\n")
        };
        self.reasoning_needs_nl = false;
        self.openai_chunk(json!({ "reasoning_content": text }), None)
    }

    /// True when `detail` looks like streamed answer/tree dump, not a short action.
    fn is_content_dump(detail: &str) -> bool {
        let t = detail.trim();
        if t.is_empty() {
            return false;
        }
        if t.contains('\n') || t.chars().count() > 72 {
            return true;
        }
        let first = t.chars().next().unwrap_or(' ');
        matches!(first, '#' | '├' | '│' | '└' | '`' | '*' | '-' | '—')
            || t.starts_with("##")
            || t.starts_with("```")
            || t.contains("目录树")
            || t.starts_with("import ")
            || t.starts_with("for ")
            || t.starts_with("time.sleep")
    }

    /// Short action-oriented status only (never dump file trees / long prose).
    fn subagent_status_text(state: &str, detail: &str) -> String {
        let d = detail.trim();
        let useful = !d.is_empty() && !Self::is_content_dump(d);
        let one = if useful {
            d.split_whitespace().collect::<Vec<_>>().join(" ")
        } else {
            String::new()
        };
        // Prefer verb phrases; drop pure content dumps → generic state.
        let raw = match state {
            "queued" => {
                if one.is_empty() {
                    "排队中".into()
                } else {
                    format!("排队中 · {one}")
                }
            }
            "running" => {
                if one.is_empty() {
                    "运行中".into()
                } else if one.starts_with("正在")
                    || one.starts_with("准备")
                    || one.starts_with("已完成")
                    || one.contains("执行")
                    || one.contains("分析")
                {
                    one
                } else {
                    // e.g. partial answer text — keep generic
                    "运行中".into()
                }
            }
            "completed" => "完成".into(),
            "failed" => {
                if one.is_empty() {
                    "失败".into()
                } else {
                    format!("失败 · {one}")
                }
            }
            other => {
                if one.is_empty() {
                    other.to_string()
                } else {
                    one
                }
            }
        };
        const MAX: usize = 48;
        if raw.chars().count() > MAX {
            let mut s: String = raw.chars().take(MAX).collect();
            s.push('…');
            s
        } else {
            raw
        }
    }

    /// Paint the full subagent panel (one row each) with ANSI cursor-up redraw.
    ///
    /// Multi-subagent concurrent updates cannot use a single `\r` (they stomp each
    /// other). Instead we keep N fixed rows and rewrite the whole block in place:
    ///
    /// ```text
    /// 子代理 explore#1: 正在执行 list_directory
    /// 子代理 worker#2:  正在执行 bash find …
    /// 子代理 worker#3:  完成
    /// ```
    fn openai_subagent_panel_chunk(
        &mut self,
        label: &str,
        state: &str,
        detail: &str,
    ) -> Option<SseChunk> {
        let status = Self::subagent_status_text(state, detail);
        // Content-dump ticks while running: keep previous status (or "运行中").
        let status = if Self::is_content_dump(detail) && state == "running" {
            self.subagent_status
                .get(label)
                .cloned()
                .unwrap_or_else(|| "运行中".into())
        } else {
            status
        };
        if self.subagent_status.get(label).map(String::as_str) == Some(status.as_str()) {
            return None;
        }
        if !self.subagent_order.iter().any(|x| x == label) {
            self.subagent_order.push(label.to_string());
        }
        self.subagent_status.insert(label.to_string(), status);

        let mut body = String::new();
        if self.subagent_panel_rows > 0 {
            // Move cursor to the first row of the existing panel.
            body.push_str(&format!("\x1b[{}A", self.subagent_panel_rows));
        } else {
            // Open a new panel below current output.
            body.push('\n');
        }
        for id in &self.subagent_order {
            let st = self
                .subagent_status
                .get(id)
                .map(String::as_str)
                .unwrap_or("…");
            // \r start of line, \x1b[K clear to EOL, then newline.
            body.push_str(&format!("\r子代理 {id}: {st}\x1b[K\n"));
        }
        self.subagent_panel_rows = self.subagent_order.len();
        self.reasoning_needs_nl = false;
        Some(self.openai_chunk(json!({ "reasoning_content": body }), None))
    }

    fn tool_index_for(&mut self, call_id: &str) -> usize {
        if let Some(&idx) = self.tool_call_index.get(call_id) {
            return idx;
        }
        let idx = self.next_tool_index;
        self.next_tool_index += 1;
        self.tool_call_index.insert(call_id.to_string(), idx);
        idx
    }

    fn remember_tool(&mut self, call_id: &str, name: &str) -> usize {
        self.tool_names.insert(call_id.to_string(), name.to_string());
        self.tool_index_for(call_id)
    }

    fn fc_item_id(&mut self, call_id: &str) -> String {
        if let Some(id) = self.fc_item_ids.get(call_id) {
            return id.clone();
        }
        let id = format!("fc_{call_id}");
        self.fc_item_ids.insert(call_id.to_string(), id.clone());
        id
    }

    fn chat_tool_item(
        &mut self,
        call_id: &str,
        name: Option<&str>,
        status: &str,
        arguments: Option<&str>,
        output_delta: Option<&str>,
        output: Option<&str>,
        success: Option<bool>,
        duration_ms: Option<u64>,
        batch_id: Option<&str>,
        progress: Option<Value>,
        children: Option<Vec<Value>>,
    ) -> Value {
        let index = self.tool_index_for(call_id);
        let resolved_name = name
            .map(|s| s.to_string())
            .or_else(|| self.tool_names.get(call_id).cloned())
            .unwrap_or_default();
        let mut item = json!({
            "index": index,
            "id": call_id,
            "type": "function",
            "status": status,
        });
        if !resolved_name.is_empty() || arguments.is_some() {
            item["function"] = json!({
                "name": resolved_name,
                "arguments": arguments.unwrap_or(""),
            });
        }
        if let Some(d) = output_delta {
            item["output_delta"] = json!(d);
        }
        if let Some(o) = output {
            item["output"] = json!(o);
        }
        if let Some(s) = success {
            item["success"] = json!(s);
        }
        if let Some(ms) = duration_ms {
            item["duration_ms"] = json!(ms);
        }
        if let Some(b) = batch_id {
            item["batch_id"] = json!(b);
        }
        if let Some(p) = progress {
            item["progress"] = p;
        }
        if let Some(c) = children {
            item["children"] = json!(c);
        }
        item
    }

    pub(super) fn project(&mut self, event: ChatEvent) -> Vec<SseChunk> {
        match self.format {
            WireFormat::OpenAiChat => self.project_openai_chat(event),
            WireFormat::OpenAiResponses => self.project_openai_responses(event),
            WireFormat::Anthropic => self.project_anthropic(event),
        }
    }

    fn openai_chunk(&self, delta: Value, finish: Option<&str>) -> SseChunk {
        SseChunk {
            event: None,
            data: json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": self.created,
                "model": self.model,
                "choices": [{
                    "index": 0,
                    "delta": delta,
                    "finish_reason": finish
                }]
            })
            .to_string(),
        }
    }

    fn ensure_openai_started(&mut self, out: &mut Vec<SseChunk>) {
        if !self.started {
            self.started = true;
            out.push(self.openai_chunk(json!({"role": "assistant"}), None));
        }
    }

    fn task_children(name: &str, arguments: &str) -> Option<Vec<Value>> {
        if name != "task" {
            return None;
        }
        let ch = initial_children_from_task_args(arguments);
        if ch.is_empty() {
            None
        } else {
            Some(ch)
        }
    }

    fn project_openai_chat(&mut self, event: ChatEvent) -> Vec<SseChunk> {
        let mut out = Vec::new();
        match event {
            ChatEvent::ReasoningDelta { content } => {
                self.ensure_openai_started(&mut out);
                // After a tool/subagent timeline line we already ended with `\n`.
                // Mid-stream model thinking is left as-is for continuity.
                self.note_reasoning_tail(&content);
                out.push(self.openai_chunk(json!({"reasoning_content": content}), None));
            }
            ChatEvent::TextDelta { content } => {
                self.ensure_openai_started(&mut out);
                // Separate answer from reasoning/tool timeline.
                let text = if self.reasoning_needs_nl {
                    self.reasoning_needs_nl = false;
                    format!("\n{content}")
                } else {
                    content
                };
                out.push(self.openai_chunk(json!({"content": text}), None));
            }
            ChatEvent::ToolBatchStarted { calls } => {
                self.ensure_openai_started(&mut out);
                let batch_id = uuid::Uuid::new_v4().to_string();
                let mut items = Vec::new();
                for c in &calls {
                    self.remember_tool(&c.id, &c.name);
                    out.push(self.openai_progress_chunk(&format!("正在调用 {}", c.name)));
                    items.push(self.chat_tool_item(
                        &c.id,
                        Some(&c.name),
                        "in_progress",
                        Some(&c.arguments),
                        None,
                        None,
                        None,
                        None,
                        Some(&batch_id),
                        None,
                        Self::task_children(&c.name, &c.arguments),
                    ));
                }
                out.push(self.openai_chunk(
                    json!({
                        "tool_calls": items,
                        "atomcode": {
                            "type": "tool_batch",
                            "batch_id": batch_id,
                            "count": calls.len()
                        }
                    }),
                    None,
                ));
            }
            ChatEvent::ToolCallStarted { id, name, arguments } => {
                self.ensure_openai_started(&mut out);
                self.remember_tool(&id, &name);
                out.push(self.openai_progress_chunk(&format!("正在调用 {name}")));
                let item = self.chat_tool_item(
                    &id,
                    Some(&name),
                    "in_progress",
                    Some(&arguments),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Self::task_children(&name, &arguments),
                );
                out.push(self.openai_chunk(json!({ "tool_calls": [item] }), None));
            }
            ChatEvent::ToolOutputChunk { id, chunk } => {
                self.ensure_openai_started(&mut out);
                let name = self.tool_names.get(&id).cloned();
                let item = self.chat_tool_item(
                    &id,
                    name.as_deref(),
                    "in_progress",
                    None,
                    Some(&chunk),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                );
                out.push(self.openai_chunk(json!({ "tool_calls": [item] }), None));
            }
            ChatEvent::ToolProgress { id, progress } => {
                self.ensure_openai_started(&mut out);
                let name = self.tool_names.get(&id).cloned();
                let parsed = parse_subtask_progress(&progress);
                let progress_json = parsed
                    .as_ref()
                    .map(progress_to_json)
                    .unwrap_or_else(|| json!({ "message": progress }));
                let children = parsed.as_ref().map(|p| {
                    vec![json!({
                        "id": p.subtask_id,
                        "state": p.state,
                        "label": p.label,
                        "message": p.message,
                        "model": p.model,
                        "tokens": p.tokens,
                    })]
                });
                // Mirror subagent progress: one row per subagent, refresh via \r.
                if let Some(p) = parsed.as_ref() {
                    let detail = if let Some(d) = p.description.as_ref().filter(|s| !s.is_empty()) {
                        d.clone()
                    } else if p.message.is_empty() {
                        String::new()
                    } else {
                        let raw = p.message.as_str();
                        raw.rsplit(" \u{b7} ")
                            .next()
                            .unwrap_or(raw)
                            .trim()
                            .to_string()
                    };
                    if let Some(chunk) =
                        self.openai_subagent_panel_chunk(&p.label, p.state, &detail)
                    {
                        out.push(chunk);
                    }
                } else if !progress.is_empty() {
                    // Non-subagent tool progress: keep sparse single lines.
                    let label = name.as_deref().unwrap_or("tool");
                    let one = progress.split_whitespace().collect::<Vec<_>>().join(" ");
                    out.push(self.openai_progress_chunk(&format!("  · {label}: {one}")));
                }
                let item = self.chat_tool_item(
                    &id,
                    name.as_deref(),
                    "in_progress",
                    None,
                    if parsed.is_none() {
                        Some(progress.as_str())
                    } else {
                        None
                    },
                    None,
                    None,
                    None,
                    None,
                    Some(progress_json),
                    children,
                );
                out.push(self.openai_chunk(json!({ "tool_calls": [item] }), None));
            }
            ChatEvent::ToolCallResult {
                id,
                name,
                output,
                success,
                duration_ms,
            } => {
                self.ensure_openai_started(&mut out);
                self.remember_tool(&id, &name);
                let status = if success { "完成" } else { "失败" };
                let extra = if duration_ms > 0 {
                    format!(" ({duration_ms}ms)")
                } else {
                    String::new()
                };
                out.push(self.openai_progress_chunk(&format!("工具 {name} {status}{extra}")));
                let item = self.chat_tool_item(
                    &id,
                    Some(&name),
                    if success { "completed" } else { "failed" },
                    Some(""),
                    None,
                    Some(&output),
                    Some(success),
                    Some(duration_ms),
                    None,
                    None,
                    None,
                );
                out.push(self.openai_chunk(json!({ "tool_calls": [item] }), None));
            }
            ChatEvent::Done {
                session_id,
                stop_reason,
                ..
            } => {
                self.ensure_openai_started(&mut out);
                out.push(self.openai_chunk(
                    json!({
                        "atomcode": {
                            "type": "done",
                            "session_id": session_id,
                            "user": self.session_key,
                            "stop_reason": stop_reason
                        }
                    }),
                    None,
                ));
                out.push(self.openai_chunk(json!({}), Some("stop")));
                out.push(SseChunk {
                    event: None,
                    data: "[DONE]".into(),
                });
            }
            ChatEvent::Error { message } => {
                self.ensure_openai_started(&mut out);
                out.push(self.openai_chunk(
                    json!({
                        "atomcode": { "type": "error", "message": message },
                        "content": format!("\n[error] {message}")
                    }),
                    Some("stop"),
                ));
                out.push(SseChunk {
                    event: None,
                    data: "[DONE]".into(),
                });
            }
            ChatEvent::Stopped => {
                self.ensure_openai_started(&mut out);
                out.push(self.openai_chunk(json!({}), Some("stop")));
                out.push(SseChunk {
                    event: None,
                    data: "[DONE]".into(),
                });
            }
            ChatEvent::RuntimeInfo { provider, model } => {
                self.ensure_openai_started(&mut out);
                out.push(self.openai_chunk(
                    json!({
                        "atomcode": {
                            "type": "runtime_info",
                            "provider": provider,
                            "model": model
                        }
                    }),
                    None,
                ));
            }
            ChatEvent::Warning { message } => {
                self.ensure_openai_started(&mut out);
                out.push(self.openai_chunk(
                    json!({ "atomcode": { "type": "warning", "message": message } }),
                    None,
                ));
            }
            _ => {}
        }
        out
    }

    fn responses_event(&self, event: &str, data: Value) -> SseChunk {
        SseChunk {
            event: Some(event.into()),
            data: data.to_string(),
        }
    }

    fn project_openai_responses(&mut self, event: ChatEvent) -> Vec<SseChunk> {
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(self.responses_event(
                "response.created",
                json!({
                    "type": "response.created",
                    "response": {
                        "id": self.response_id,
                        "object": "response",
                        "created_at": self.created,
                        "model": self.model,
                        "status": "in_progress"
                    }
                }),
            ));
        }
        match event {
            ChatEvent::ReasoningDelta { content } => {
                out.push(self.responses_event(
                    "response.reasoning.delta",
                    json!({
                        "type": "response.reasoning.delta",
                        "delta": content
                    }),
                ));
            }
            ChatEvent::TextDelta { content } => {
                out.push(self.responses_event(
                    "response.output_text.delta",
                    json!({
                        "type": "response.output_text.delta",
                        "delta": content
                    }),
                ));
            }
            ChatEvent::ToolBatchStarted { calls } => {
                for c in calls {
                    out.extend(self.responses_tool_started(&c.id, &c.name, &c.arguments));
                }
            }
            ChatEvent::ToolCallStarted { id, name, arguments } => {
                out.extend(self.responses_tool_started(&id, &name, &arguments));
            }
            ChatEvent::ToolOutputChunk { id, chunk } => {
                let index = self.tool_index_for(&id);
                let item_id = self.fc_item_id(&id);
                out.push(self.responses_event(
                    "response.function_call_output.delta",
                    json!({
                        "type": "response.function_call_output.delta",
                        "response_id": self.response_id,
                        "item_id": item_id,
                        "output_index": index,
                        "call_id": id,
                        "delta": chunk
                    }),
                ));
            }
            ChatEvent::ToolProgress { id, progress } => {
                let index = self.tool_index_for(&id);
                let item_id = self.fc_item_id(&id);
                let parsed = parse_subtask_progress(&progress);
                let body = if let Some(p) = parsed {
                    json!({
                        "type": "response.function_call_progress",
                        "response_id": self.response_id,
                        "item_id": item_id,
                        "output_index": index,
                        "call_id": id,
                        "progress": progress_to_json(&p),
                        "children": [{
                            "id": p.subtask_id,
                            "state": p.state,
                            "label": p.label,
                            "message": p.message,
                            "model": p.model,
                            "tokens": p.tokens,
                        }]
                    })
                } else {
                    json!({
                        "type": "response.function_call_progress",
                        "response_id": self.response_id,
                        "item_id": item_id,
                        "output_index": index,
                        "call_id": id,
                        "progress": { "message": progress }
                    })
                };
                out.push(self.responses_event("response.function_call_progress", body));
            }
            ChatEvent::ToolCallResult {
                id,
                name,
                output,
                success,
                duration_ms,
            } => {
                self.remember_tool(&id, &name);
                let index = self.tool_index_for(&id);
                let item_id = self.fc_item_id(&id);
                out.push(self.responses_event(
                    "response.function_call_output.done",
                    json!({
                        "type": "response.function_call_output.done",
                        "response_id": self.response_id,
                        "item_id": item_id,
                        "output_index": index,
                        "call_id": id,
                        "name": name,
                        "output": output,
                        "success": success,
                        "duration_ms": duration_ms
                    }),
                ));
                out.push(self.responses_event(
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "response_id": self.response_id,
                        "output_index": index,
                        "item": {
                            "type": "function_call",
                            "id": item_id,
                            "call_id": id,
                            "name": name,
                            "status": if success { "completed" } else { "failed" }
                        }
                    }),
                ));
            }
            ChatEvent::Done {
                session_id,
                stop_reason,
                ..
            } => {
                out.push(self.responses_event(
                    "response.completed",
                    json!({
                        "type": "response.completed",
                        "response": {
                            "id": self.response_id,
                            "object": "response",
                            "status": "completed",
                            "model": self.model,
                            "atomcode": {
                                "session_id": session_id,
                                "user": self.session_key,
                                "stop_reason": stop_reason
                            }
                        }
                    }),
                ));
            }
            ChatEvent::Error { message } => {
                out.push(self.responses_event(
                    "response.failed",
                    json!({
                        "type": "response.failed",
                        "response": {
                            "id": self.response_id,
                            "status": "failed",
                            "error": { "message": message }
                        }
                    }),
                ));
            }
            _ => {}
        }
        out
    }

    fn responses_tool_started(
        &mut self,
        call_id: &str,
        name: &str,
        arguments: &str,
    ) -> Vec<SseChunk> {
        let index = self.remember_tool(call_id, name);
        let item_id = self.fc_item_id(call_id);
        let children = Self::task_children(name, arguments);
        let mut out = Vec::new();
        let mut item = json!({
            "type": "function_call",
            "id": item_id,
            "call_id": call_id,
            "name": name,
            "arguments": "",
            "status": "in_progress"
        });
        if let Some(c) = &children {
            item["children"] = json!(c);
        }
        out.push(self.responses_event(
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "response_id": self.response_id,
                "output_index": index,
                "item": item
            }),
        ));
        let item_id = self.fc_item_id(call_id);
        out.push(self.responses_event(
            "response.function_call_arguments.done",
            json!({
                "type": "response.function_call_arguments.done",
                "response_id": self.response_id,
                "item_id": item_id,
                "output_index": index,
                "call_id": call_id,
                "arguments": arguments
            }),
        ));
        out
    }

    fn close_open_anthropic_blocks(&mut self, out: &mut Vec<SseChunk>) {
        if let Some(idx) = self.thinking_block.take() {
            out.push(SseChunk {
                event: Some("content_block_stop".into()),
                data: json!({ "type": "content_block_stop", "index": idx }).to_string(),
            });
        }
        if let Some(idx) = self.text_block.take() {
            out.push(SseChunk {
                event: Some("content_block_stop".into()),
                data: json!({ "type": "content_block_stop", "index": idx }).to_string(),
            });
        }
    }

    fn project_anthropic(&mut self, event: ChatEvent) -> Vec<SseChunk> {
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(SseChunk {
                event: Some("message_start".into()),
                data: json!({
                    "type": "message_start",
                    "message": {
                        "id": self.id,
                        "type": "message",
                        "role": "assistant",
                        "model": self.model,
                        "content": [],
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": { "input_tokens": 0, "output_tokens": 0 }
                    }
                })
                .to_string(),
            });
        }

        match event {
            ChatEvent::ReasoningDelta { content } => {
                if self.thinking_block.is_none() {
                    self.close_open_anthropic_blocks(&mut out);
                    let idx = self.next_block;
                    self.next_block += 1;
                    self.thinking_block = Some(idx);
                    out.push(SseChunk {
                        event: Some("content_block_start".into()),
                        data: json!({
                            "type": "content_block_start",
                            "index": idx,
                            "content_block": { "type": "thinking", "thinking": "" }
                        })
                        .to_string(),
                    });
                }
                if let Some(idx) = self.thinking_block {
                    out.push(SseChunk {
                        event: Some("content_block_delta".into()),
                        data: json!({
                            "type": "content_block_delta",
                            "index": idx,
                            "delta": { "type": "thinking_delta", "thinking": content }
                        })
                        .to_string(),
                    });
                }
            }
            ChatEvent::TextDelta { content } => {
                if self.thinking_block.is_some() {
                    self.close_open_anthropic_blocks(&mut out);
                }
                if self.text_block.is_none() {
                    let idx = self.next_block;
                    self.next_block += 1;
                    self.text_block = Some(idx);
                    out.push(SseChunk {
                        event: Some("content_block_start".into()),
                        data: json!({
                            "type": "content_block_start",
                            "index": idx,
                            "content_block": { "type": "text", "text": "" }
                        })
                        .to_string(),
                    });
                }
                if let Some(idx) = self.text_block {
                    out.push(SseChunk {
                        event: Some("content_block_delta".into()),
                        data: json!({
                            "type": "content_block_delta",
                            "index": idx,
                            "delta": { "type": "text_delta", "text": content }
                        })
                        .to_string(),
                    });
                }
            }
            ChatEvent::ToolBatchStarted { calls } => {
                self.close_open_anthropic_blocks(&mut out);
                for c in calls {
                    out.extend(self.anthropic_tool_started(&c.id, &c.name, &c.arguments));
                }
            }
            ChatEvent::ToolCallStarted { id, name, arguments } => {
                self.close_open_anthropic_blocks(&mut out);
                out.extend(self.anthropic_tool_started(&id, &name, &arguments));
            }
            ChatEvent::ToolOutputChunk { id, chunk } => {
                if let Some(&idx) = self.tool_blocks.get(&id) {
                    out.push(SseChunk {
                        event: Some("content_block_delta".into()),
                        data: json!({
                            "type": "content_block_delta",
                            "index": idx,
                            "delta": {
                                "type": "tool_output_delta",
                                "tool_use_id": id,
                                "partial_output": chunk
                            }
                        })
                        .to_string(),
                    });
                }
            }
            ChatEvent::ToolProgress { id, progress } => {
                if let Some(&idx) = self.tool_blocks.get(&id) {
                    let parsed = parse_subtask_progress(&progress);
                    let delta = if let Some(p) = parsed {
                        json!({
                            "type": "tool_progress",
                            "tool_use_id": id,
                            "progress": progress_to_json(&p),
                            "children": [{
                                "id": p.subtask_id,
                                "state": p.state,
                                "label": p.label,
                                "message": p.message,
                                "model": p.model,
                                "tokens": p.tokens,
                            }]
                        })
                    } else {
                        json!({
                            "type": "tool_progress",
                            "tool_use_id": id,
                            "progress": { "message": progress }
                        })
                    };
                    out.push(SseChunk {
                        event: Some("content_block_delta".into()),
                        data: json!({
                            "type": "content_block_delta",
                            "index": idx,
                            "delta": delta
                        })
                        .to_string(),
                    });
                }
            }
            ChatEvent::ToolCallResult {
                id,
                name,
                output,
                success,
                duration_ms,
            } => {
                if let Some(idx) = self.tool_blocks.remove(&id) {
                    out.push(SseChunk {
                        event: Some("content_block_delta".into()),
                        data: json!({
                            "type": "content_block_delta",
                            "index": idx,
                            "delta": {
                                "type": "tool_result",
                                "tool_use_id": id,
                                "name": name,
                                "content": output,
                                "is_error": !success,
                                "duration_ms": duration_ms
                            }
                        })
                        .to_string(),
                    });
                    out.push(SseChunk {
                        event: Some("content_block_stop".into()),
                        data: json!({ "type": "content_block_stop", "index": idx }).to_string(),
                    });
                }
            }
            ChatEvent::Done {
                session_id,
                stop_reason,
                ..
            } => {
                self.close_open_anthropic_blocks(&mut out);
                for (_, idx) in self.tool_blocks.drain() {
                    out.push(SseChunk {
                        event: Some("content_block_stop".into()),
                        data: json!({ "type": "content_block_stop", "index": idx }).to_string(),
                    });
                }
                out.push(SseChunk {
                    event: Some("message_delta".into()),
                    data: json!({
                        "type": "message_delta",
                        "delta": { "stop_reason": "end_turn", "stop_sequence": null },
                        "usage": { "output_tokens": 0 },
                        "atomcode": {
                            "session_id": session_id,
                            "user": self.session_key,
                            "stop_reason": stop_reason
                        }
                    })
                    .to_string(),
                });
                out.push(SseChunk {
                    event: Some("message_stop".into()),
                    data: json!({ "type": "message_stop" }).to_string(),
                });
            }
            ChatEvent::Error { message } => {
                out.push(SseChunk {
                    event: Some("error".into()),
                    data: json!({
                        "type": "error",
                        "error": {
                            "type": "api_error",
                            "message": message
                        }
                    })
                    .to_string(),
                });
            }
            _ => {}
        }
        out
    }

    fn anthropic_tool_started(
        &mut self,
        call_id: &str,
        name: &str,
        arguments: &str,
    ) -> Vec<SseChunk> {
        self.tool_names.insert(call_id.to_string(), name.to_string());
        let idx = self.next_block;
        self.next_block += 1;
        self.tool_blocks.insert(call_id.to_string(), idx);
        let children = Self::task_children(name, arguments);
        let mut block = json!({
            "type": "tool_use",
            "id": call_id,
            "name": name,
            "input": {}
        });
        if let Some(c) = children {
            block["children"] = json!(c);
        }
        let mut out = Vec::new();
        out.push(SseChunk {
            event: Some("content_block_start".into()),
            data: json!({
                "type": "content_block_start",
                "index": idx,
                "content_block": block
            })
            .to_string(),
        });
        out.push(SseChunk {
            event: Some("content_block_delta".into()),
            data: json!({
                "type": "content_block_delta",
                "index": idx,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": arguments
                }
            })
            .to_string(),
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_queued_and_running_subtasks() {
        let q = parse_subtask_progress("\u{25cb} queued \u{b7} explore#1 \u{b7} glm \u{b7} auth")
            .unwrap();
        assert_eq!(q.subtask_id, "explore#1");
        assert_eq!(q.state, "queued");

        let r = parse_subtask_progress("\u{21bb} worker#2 \u{b7} deepseek \u{b7} fix bug").unwrap();
        assert_eq!(r.subtask_id, "worker#2");
        assert_eq!(r.state, "running");

        let d = parse_subtask_progress("\u{2713} done \u{b7} explore#1 \u{b7} glm").unwrap();
        assert_eq!(d.state, "completed");
    }

    #[test]
    fn parallel_tool_calls_keep_distinct_indexes() {
        let mut p = CompatProjector::openai_chat("c".into(), "m".into(), 1, None);
        let a = p.project(ChatEvent::ToolCallStarted {
            id: "call_a".into(),
            name: "read".into(),
            arguments: "{}".into(),
        });
        let b = p.project(ChatEvent::ToolCallStarted {
            id: "call_b".into(),
            name: "grep".into(),
            arguments: "{}".into(),
        });
        let out_a = p.project(ChatEvent::ToolOutputChunk {
            id: "call_a".into(),
            chunk: "A".into(),
        });
        let out_b = p.project(ChatEvent::ToolOutputChunk {
            id: "call_b".into(),
            chunk: "B".into(),
        });
        let joined: String = a
            .iter()
            .chain(b.iter())
            .chain(out_a.iter())
            .chain(out_b.iter())
            .map(|c| c.data.as_str())
            .collect();
        assert!(joined.contains("call_a"));
        assert!(joined.contains("call_b"));
        assert!(joined.contains("\"index\":0"));
        assert!(joined.contains("\"index\":1"));
        assert!(joined.contains("\"output_delta\":\"A\""));
        assert!(joined.contains("\"output_delta\":\"B\""));
    }

    /// Extract OpenAI chat `delta.reasoning_content` strings from projected chunks.
    fn reasoning_texts(chunks: &[SseChunk]) -> Vec<String> {
        let mut out = Vec::new();
        for c in chunks {
            let Ok(v) = serde_json::from_str::<Value>(&c.data) else {
                continue;
            };
            let Some(rc) = v
                .pointer("/choices/0/delta/reasoning_content")
                .and_then(|x| x.as_str())
            else {
                continue;
            };
            out.push(rc.to_string());
        }
        out
    }

    #[test]
    fn openai_subagent_panel_redraws_in_place() {
        // Concurrent subagents: each update rewrites the whole panel via CSI-A
        // (cursor up) instead of appending a new history line.
        let mut p = CompatProjector::openai_chat("c".into(), "m".into(), 1, None);
        let mut all = Vec::new();
        all.extend(p.project(ChatEvent::ToolCallStarted {
            id: "call_task".into(),
            name: "task".into(),
            arguments: "{}".into(),
        }));
        for (id, msg) in [
            ("explore#1", "排队中 · 搜目录"),
            ("worker#2", "排队中 · 找 py"),
            ("explore#1", "正在执行 list_directory"),
            ("worker#2", "正在执行 bash find"),
            // content dump must NOT become a new stacked status
            ("explore#1", "## E:/Desktop 目录树\n├── a.lnk"),
            ("explore#1", "正在分析结果"),
            ("worker#2", "完成"),
        ] {
            let progress = if msg == "完成" {
                format!("\u{2713} done \u{b7} {id} \u{b7} auto")
            } else {
                format!("\u{21bb} {id} \u{b7} auto \u{b7} {msg}")
            };
            all.extend(p.project(ChatEvent::ToolProgress {
                id: "call_task".into(),
                progress,
            }));
        }

        let texts = reasoning_texts(&all);
        let panel_chunks: Vec<&String> = texts
            .iter()
            .filter(|t| t.contains("子代理 "))
            .collect();
        assert!(
            !panel_chunks.is_empty(),
            "expected panel chunks: {texts:?}"
        );
        // Later updates (after first panel paint) must include cursor-up CSI.
        let with_cup = panel_chunks
            .iter()
            .filter(|t| t.contains("\x1b["))
            .count();
        assert!(
            with_cup >= 1,
            "expected ANSI cursor-up panel redraw: {panel_chunks:?}"
        );
        // Directory tree dump must not appear as a status row.
        let joined = texts.join("");
        assert!(
            !joined.contains("目录树") && !joined.contains("├──"),
            "content dump leaked into panel: {joined}"
        );
        // Both agents appear in the latest panel snapshot.
        let last = panel_chunks.last().unwrap();
        assert!(
            last.contains("explore#1") && last.contains("worker#2"),
            "panel should list both agents: {last}"
        );
    }

    #[test]
    fn subagent_progress_on_parent_task() {
        let mut p = CompatProjector::openai_chat("c".into(), "m".into(), 1, None);
        let _ = p.project(ChatEvent::ToolCallStarted {
            id: "task_1".into(),
            name: "task".into(),
            arguments: r#"{"tasks":[{"description":"d","prompt":"p","subagent_type":"explore"}]}"#
                .into(),
        });
        let prog = p.project(ChatEvent::ToolProgress {
            id: "task_1".into(),
            progress: "\u{21bb} explore#1 \u{b7} glm \u{b7} look".into(),
        });
        let joined: String = prog.iter().map(|c| c.data.as_str()).collect();
        assert!(joined.contains("explore#1"));
        assert!(joined.contains("\"state\":\"running\""));
        assert!(joined.contains("\"progress\""));
    }

    #[test]
    fn responses_uses_output_item_and_call_id() {
        let mut p = CompatProjector::openai_responses("resp".into(), "m".into(), 1, None);
        let chunks = p.project(ChatEvent::ToolCallStarted {
            id: "call_x".into(),
            name: "bash".into(),
            arguments: r#"{"command":"ls"}"#.into(),
        });
        let joined: String = chunks.iter().map(|c| c.data.as_str()).collect();
        assert!(joined.contains("response.output_item.added") || chunks[0].event.as_deref() == Some("response.created") || chunks.iter().any(|c| c.event.as_deref() == Some("response.output_item.added")));
        assert!(chunks
            .iter()
            .any(|c| c.event.as_deref() == Some("response.output_item.added")));
        assert!(joined.contains("call_x"));
        assert!(joined.contains("function_call"));
    }

    #[test]
    fn anthropic_parallel_two_tool_use_blocks() {
        let mut p = CompatProjector::anthropic("msg".into(), "m".into(), None);
        let a = p.project(ChatEvent::ToolCallStarted {
            id: "toolu_1".into(),
            name: "read".into(),
            arguments: "{}".into(),
        });
        let b = p.project(ChatEvent::ToolCallStarted {
            id: "toolu_2".into(),
            name: "grep".into(),
            arguments: "{}".into(),
        });
        let joined: String = a.iter().chain(b.iter()).map(|c| c.data.as_str()).collect();
        assert!(joined.contains("toolu_1"));
        assert!(joined.contains("toolu_2"));
        assert!(joined.contains("tool_use"));
        assert!(joined.contains("\"index\":0") || joined.contains("\"index\": 0"));
    }
}
