"""Compose a display-oriented *reasoning* timeline.

Controlled by :class:`~atomcode_sdk.events.ReasoningEffort`:

- **low**: no reasoning pane (content only)
- **medium**: tools + subagents, hide model thinking detail
- **max**: thinking + tools + subagents

Example (medium)::

    正在调用 read
    正在调用 task
    子代理 explore#1: 分析 auth 模块 [glm]
    子代理 worker#2: 改代码中
    工具 task 完成 (1200ms)

Example (max) also prepends model thinking deltas.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Optional

from .events import ReasoningEffort, StreamEvent, StreamEventType, SubagentState, ToolState


def _tool_label(name: str, call_id: str) -> str:
    return name or call_id or "tool"


def _compact_args_preview(arguments: str, max_len: int = 90) -> str:
    """One-line preview of tool arguments.

    - ``bash``: the full ``command`` is shown untruncated (the command IS the
      call; cutting it hides what actually ran).
    - other tools: flattened JSON, capped at ``max_len``.
    """
    if not arguments or not arguments.strip():
        return ""
    text = arguments.strip()
    try:
        import json as _json

        obj = _json.loads(text)
        if isinstance(obj, dict):
            if isinstance(obj.get("command"), str) and obj["command"].strip():
                return obj["command"].strip()
            preview = " ".join(f"{k}={v}" for k, v in list(obj.items())[:2])
        elif isinstance(obj, list):
            preview = text
        else:
            preview = str(obj)
    except Exception:
        preview = text
    preview = " ".join(preview.split())
    if len(preview) > max_len:
        preview = preview[: max_len - 1] + "…"
    return preview


def format_tool_start(name: str, call_id: str = "") -> str:
    label = _tool_label(name, call_id)
    # Always lead with \n — do not rely on previous delta's trailing newline
    # (many stream UIs trim trailing whitespace per chunk).
    return f"\n正在调用 {label}\n"


def format_tool_args_line(arguments: str, name: str = "", call_id: str = "") -> str:
    """Compact one-line tool arguments preview, emitted once when args arrive."""
    preview = _compact_args_preview(arguments)
    if not preview:
        return ""
    return f"  · {preview}\n"


def format_tool_output_line(name: str, chunk: str, call_id: str = "") -> str:
    """Optional compact tool stdout line for reasoning (usually omitted; keep short)."""
    text = chunk.strip()
    if not text:
        return ""
    one = text.splitlines()[0][:120]
    label = _tool_label(name, call_id)
    return f"\n  · {label}: {one}\n"


def format_tool_done(
    name: str,
    success: bool,
    call_id: str = "",
    duration_ms: Optional[int] = None,
) -> str:
    label = _tool_label(name, call_id)
    status = "完成" if success else "失败"
    extra = f" ({duration_ms}ms)" if duration_ms is not None else ""
    return f"\n工具 {label} {status}{extra}\n"


def format_subagent_progress(sub: SubagentState) -> str:
    """Single-line status for one subagent (no stacking newlines).

    Callers that want in-place refresh should upsert by subagent id rather than
    appending every return value as a new log line.
    """
    sid = sub.id or sub.label or "subagent"
    state_zh = {
        "queued": "排队中",
        "running": "运行中",
        "completed": "完成",
        "failed": "失败",
    }.get(sub.state, sub.state)
    msg = (sub.message or "").strip()
    detail = msg if msg else state_zh
    sep = " \u00b7 "
    if sep in detail or " · " in detail:
        parts = detail.replace(" · ", sep).split(sep)
        if len(parts) >= 2:
            detail = parts[-1].strip() or state_zh
    if detail.startswith(sid):
        body = detail
    else:
        body = detail if detail else state_zh
    body = " ".join(body.split())
    if len(body) > 56:
        body = body[:56] + "…"
    return f"子代理 {sid}: {body}"


@dataclass
class ReasoningComposer:
    """Stateful composer: raw protocol events → reasoning/content deltas.

    Parameters
    ----------
    effort:
        Display intensity (see :class:`ReasoningEffort`). Default ``medium``.
    include_tool_output:
        If True **and** effort is not ``low``, append short tool stdout teasers.
        Only meaningful for ``medium``/``max``.
    include_tool_done:
        Append 「工具 xxx 完成/失败」 when effort is ``medium`` or ``max``.
    """

    effort: ReasoningEffort = ReasoningEffort.MEDIUM
    include_tool_output: bool = False
    include_tool_done: bool = True
    reasoning: str = ""
    content: str = ""
    tools: dict[str, ToolState] = field(default_factory=dict)
    _announced_tools: set[str] = field(default_factory=set)
    _args_announced: set[str] = field(default_factory=set)
    _last_sub_line: dict[tuple[str, str], str] = field(default_factory=dict)
    # Dedup server-mirrored progress lines (reasoning_content) vs structured tool_calls.
    _seen_progress_lines: set[str] = field(default_factory=set)

    @property
    def show_thinking(self) -> bool:
        return self.effort == ReasoningEffort.MAX

    @property
    def show_tools_and_subagents(self) -> bool:
        return self.effort in (ReasoningEffort.MEDIUM, ReasoningEffort.MAX)

    @staticmethod
    def _is_progress_timeline_line(text: str) -> bool:
        """Server may mirror tools/subagents into ``reasoning_content`` for OpenAI SDK."""
        t = text.lstrip("\n").lstrip()
        return (
            t.startswith("正在调用")
            or t.startswith("子代理 ")
            or t.startswith("工具 ")
            or t.startswith("  · ")
        )

    def _append_reasoning_line(self, line: str) -> str:
        """Append unique progress/thinking line; return delta (may be empty if deduped)."""
        if not line:
            return ""
        body = " ".join(line.strip().split())
        if not body:
            return ""
        # Dedupe by body text (ignore surrounding newlines).
        if body in self._seen_progress_lines:
            return ""
        self._seen_progress_lines.add(body)
        # Always lead with \n so a client that trims trailing whitespace
        # on the *previous* chunk still starts a new line.
        if self.reasoning.endswith("\n"):
            emit = f"{body}\n"
        else:
            emit = f"\n{body}\n"
        self.reasoning += emit
        return emit

    def on_thinking(self, text: str) -> StreamEvent:
        """Model reasoning / thinking tokens (and server-mirrored tool timeline)."""
        if not text:
            return StreamEvent(
                type=StreamEventType.REASONING,
                reasoning=self.reasoning,
                content=self.content,
            )
        # Server may send `\r子代理 id: status` for in-place row refresh.
        if "\r" in text and "子代理 " in text:
            part = text.split("\r")[-1].strip()
            if part.startswith("子代理 "):
                # Pass through for DualPanePrinter; update cumulative best-effort.
                label_status = part
                # Replace any previous line with same label prefix.
                if ":" in part:
                    label = part.split(":", 1)[0]
                    self.reasoning = re.sub(
                        re.escape(label) + r":[^\n]*",
                        label_status,
                        self.reasoning,
                        count=1,
                    )
                    if label not in self.reasoning:
                        if self.reasoning and not self.reasoning.endswith("\n"):
                            self.reasoning += "\n"
                        self.reasoning += label_status + "\n"
                return StreamEvent(
                    type=StreamEventType.REASONING,
                    reasoning_delta=text if self.show_tools_and_subagents else "",
                    reasoning=self.reasoning,
                    content=self.content,
                )

        # Medium: keep tool/subagent timeline mirrored into reasoning_content;
        # drop pure model thinking detail.
        if not self.show_thinking:
            if self.show_tools_and_subagents and self._is_progress_timeline_line(text):
                delta = self._append_reasoning_line(text if text.endswith("\n") else text)
                return StreamEvent(
                    type=StreamEventType.REASONING,
                    reasoning_delta=delta,
                    reasoning=self.reasoning,
                    content=self.content,
                )
            return StreamEvent(
                type=StreamEventType.REASONING,
                reasoning_delta="",
                reasoning=self.reasoning,
                content=self.content,
            )
        # Max: full thinking; still dedupe mirrored progress lines.
        if self._is_progress_timeline_line(text):
            delta = self._append_reasoning_line(text if text.endswith("\n") else text)
        else:
            self.reasoning += text
            delta = text
        return StreamEvent(
            type=StreamEventType.REASONING,
            reasoning_delta=delta,
            reasoning=self.reasoning,
            content=self.content,
        )

    def on_content(self, text: str) -> StreamEvent:
        if not text:
            return StreamEvent(
                type=StreamEventType.CONTENT,
                reasoning=self.reasoning,
                content=self.content,
            )
        self.content += text
        return StreamEvent(
            type=StreamEventType.CONTENT,
            content_delta=text,
            reasoning=self.reasoning,
            content=self.content,
        )

    def on_tool_update(self, tool: ToolState, *, output_delta: str = "") -> list[StreamEvent]:
        """Apply a tool card update; may emit reasoning lines + TOOL event."""
        prev = self.tools.get(tool.id)
        self.tools[tool.id] = tool
        events: list[StreamEvent] = []
        show = self.show_tools_and_subagents

        # Announce start once (dedupe against server-mirrored reasoning_content lines)
        if (
            show
            and tool.id not in self._announced_tools
            and (tool.name or tool.arguments or tool.status == "in_progress")
        ):
            if tool.name or tool.status == "in_progress":
                line = format_tool_start(tool.name, tool.id)
                self._announced_tools.add(tool.id)
                delta = self._append_reasoning_line(line)
                if delta:
                    events.append(
                        StreamEvent(
                            type=StreamEventType.REASONING,
                            reasoning_delta=delta,
                            reasoning=self.reasoning,
                            content=self.content,
                            tool=tool,
                        )
                    )

        # Announce a compact arguments preview once they arrive (Anthropic fills
        # `input_json_delta` after the tool_use block start, so the start line
        # above is name-only; this makes the actual command visible).
        if show and tool.arguments and tool.id not in self._args_announced:
            args_line = format_tool_args_line(tool.arguments, tool.name, tool.id)
            if args_line:
                self._args_announced.add(tool.id)
                delta = self._append_reasoning_line(args_line)
                if delta:
                    events.append(
                        StreamEvent(
                            type=StreamEventType.REASONING,
                            reasoning_delta=delta,
                            reasoning=self.reasoning,
                            content=self.content,
                            tool=tool,
                        )
                    )

        # Subagent children: one logical row per id; refresh via \r (no stack).
        for sid, child in tool.children.items():
            child.parent_call_id = tool.id
            key = (tool.id, sid)
            line = format_subagent_progress(child)
            if self._last_sub_line.get(key) == line:
                continue
            prev_line = self._last_sub_line.get(key)
            self._last_sub_line[key] = line
            if show:
                # In-place: CR refresh if we already showed this subagent.
                if prev_line is not None:
                    delta = f"\r{line}                    "
                    # Update cumulative reasoning: replace previous line text.
                    if prev_line in self.reasoning:
                        self.reasoning = self.reasoning.replace(prev_line, line, 1)
                    else:
                        if self.reasoning and not self.reasoning.endswith("\n"):
                            self.reasoning += "\n"
                        self.reasoning += line + "\n"
                else:
                    delta = self._append_reasoning_line(line)
                if delta:
                    events.append(
                        StreamEvent(
                            type=StreamEventType.REASONING,
                            reasoning_delta=delta,
                            reasoning=self.reasoning,
                            content=self.content,
                            tool=tool,
                            subagent=child,
                        )
                    )
            # Structured subagent event always available (UI cards), even on low
            events.append(
                StreamEvent(
                    type=StreamEventType.SUBAGENT,
                    reasoning=self.reasoning,
                    content=self.content,
                    tool=tool,
                    subagent=child,
                )
            )

        if show and output_delta and self.include_tool_output:
            line = format_tool_output_line(tool.name, output_delta, tool.id)
            if line:
                delta = self._append_reasoning_line(line)
                if delta:
                    events.append(
                        StreamEvent(
                            type=StreamEventType.REASONING,
                            reasoning_delta=delta,
                            reasoning=self.reasoning,
                            content=self.content,
                            tool=tool,
                        )
                    )

        # Completion line
        if show and tool.status in ("completed", "failed") and self.include_tool_done:
            was_terminal = prev is not None and prev.status in ("completed", "failed")
            if not was_terminal:
                ok = tool.success if tool.success is not None else (tool.status == "completed")
                line = format_tool_done(tool.name, bool(ok), tool.id, tool.duration_ms)
                delta = self._append_reasoning_line(line)
                if delta:
                    events.append(
                        StreamEvent(
                            type=StreamEventType.REASONING,
                            reasoning_delta=delta,
                            reasoning=self.reasoning,
                            content=self.content,
                            tool=tool,
                        )
                    )

        # Structured tool event always (optional for UIs)
        events.append(
            StreamEvent(
                type=StreamEventType.TOOL,
                reasoning=self.reasoning,
                content=self.content,
                tool=tool,
            )
        )
        return events

    def on_subagent_only(self, parent_id: str, sub: SubagentState) -> list[StreamEvent]:
        """Progress without a full tool card rebuild."""
        tool = self.tools.get(parent_id) or ToolState(id=parent_id, name="task")
        tool.children[sub.id] = sub
        self.tools[parent_id] = tool
        return self.on_tool_update(tool)
