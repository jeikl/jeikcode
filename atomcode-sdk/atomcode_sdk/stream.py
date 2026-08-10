"""SSE parsers for AtomCode serve compatible protocols."""

from __future__ import annotations

import json
from enum import Enum
from typing import Any, Iterable, Iterator, Optional

from .events import (
    ReasoningEffort,
    StreamEvent,
    StreamEventType,
    SubagentState,
    ToolState,
    parse_reasoning_effort,
)
from .reasoning import ReasoningComposer


class Protocol(str, Enum):
    CHAT_COMPLETIONS = "chat_completions"  # POST /v1/chat/completions
    RESPONSES = "responses"  # POST /v1/responses
    MESSAGES = "messages"  # POST /v1/messages (Anthropic)


def iter_sse_lines(raw_lines: Iterable[str]) -> Iterator[tuple[Optional[str], str]]:
    """Yield ``(event_name, data)`` from raw SSE text lines.

    Handles:
    - ``data: {...}``
    - ``event: name`` + following ``data:``
    - multi-line data joined with ``\\n``
    - ``data: [DONE]``
    """
    event_name: Optional[str] = None
    data_parts: list[str] = []

    def flush() -> Optional[tuple[Optional[str], str]]:
        nonlocal event_name, data_parts
        if not data_parts and event_name is None:
            return None
        data = "\n".join(data_parts)
        pair = (event_name, data)
        event_name = None
        data_parts = []
        return pair

    for line in raw_lines:
        if line is None:
            continue
        line = line.rstrip("\r\n")
        if line == "":
            item = flush()
            if item is not None:
                yield item
            continue
        if line.startswith(":"):
            # comment / ping
            continue
        if line.startswith("event:"):
            event_name = line[6:].strip()
            continue
        if line.startswith("data:"):
            data_parts.append(line[5:].lstrip())
            continue
        # bare JSON line fallback
        data_parts.append(line)

    item = flush()
    if item is not None:
        yield item


def _as_dict(data: str) -> Optional[dict[str, Any]]:
    data = data.strip()
    if not data or data == "[DONE]":
        return None
    try:
        obj = json.loads(data)
    except json.JSONDecodeError:
        return None
    return obj if isinstance(obj, dict) else None


def _merge_tool(
    tools: dict[str, ToolState],
    *,
    call_id: str,
    index: Optional[int] = None,
    name: Optional[str] = None,
    arguments: Optional[str] = None,
    status: Optional[str] = None,
    output_delta: Optional[str] = None,
    output: Optional[str] = None,
    success: Optional[bool] = None,
    duration_ms: Optional[int] = None,
    batch_id: Optional[str] = None,
    children: Optional[list[dict[str, Any]]] = None,
    progress: Optional[dict[str, Any]] = None,
) -> tuple[ToolState, str]:
    """Update tool map; return (tool, output_delta_for_composer)."""
    tool = tools.get(call_id) or ToolState(id=call_id)
    if index is not None:
        try:
            tool.index = int(index)
        except (TypeError, ValueError):
            pass
    if name:
        tool.name = name
    if arguments is not None:
        # Full replace if looks complete; else append (OpenAI-style streaming args)
        if not tool.arguments or arguments.startswith("{") or arguments.startswith("["):
            if arguments:
                tool.arguments = arguments
        else:
            tool.arguments += arguments
    if status:
        tool.status = status
    if output_delta:
        tool.output += output_delta
    if output is not None:
        tool.output = output
    if success is not None:
        tool.success = success
    if duration_ms is not None:
        tool.duration_ms = int(duration_ms)
    if batch_id:
        tool.batch_id = batch_id

    if children:
        for ch in children:
            sid = str(ch.get("id") or ch.get("subtask_id") or "")
            if not sid:
                continue
            prev = tool.children.get(sid) or SubagentState(id=sid, parent_call_id=call_id)
            prev.state = str(ch.get("state") or prev.state)
            prev.label = str(ch.get("label") or prev.label or sid)
            prev.message = str(ch.get("message") or prev.message)
            if ch.get("model") is not None:
                prev.model = str(ch["model"])
            if ch.get("description") is not None:
                prev.description = str(ch["description"])
            if ch.get("tokens") is not None:
                try:
                    prev.tokens = int(ch["tokens"])
                except (TypeError, ValueError):
                    pass
            prev.parent_call_id = call_id
            tool.children[sid] = prev

    if progress and isinstance(progress, dict):
        sid = str(progress.get("subtask_id") or progress.get("id") or "")
        if sid:
            prev = tool.children.get(sid) or SubagentState(id=sid, parent_call_id=call_id)
            prev.state = str(progress.get("state") or prev.state or "running")
            prev.label = str(progress.get("label") or prev.label or sid)
            prev.message = str(progress.get("message") or prev.message)
            if progress.get("model") is not None:
                prev.model = str(progress["model"])
            if progress.get("tokens") is not None:
                try:
                    prev.tokens = int(progress["tokens"])
                except (TypeError, ValueError):
                    pass
            prev.parent_call_id = call_id
            tool.children[sid] = prev

    tools[call_id] = tool
    return tool, output_delta or ""


class StreamParser:
    """Parse AtomCode SSE into unified :class:`StreamEvent` frames.

    ``reasoning_delta`` is filtered by :class:`~atomcode_sdk.events.ReasoningEffort`:

    - ``low``: empty reasoning pane (content only)
    - ``medium``: tools + subagents, no model thinking detail
    - ``max``: thinking + tools + subagents
    """

    def __init__(
        self,
        protocol: Protocol = Protocol.CHAT_COMPLETIONS,
        *,
        reasoning_effort: ReasoningEffort | str = ReasoningEffort.MEDIUM,
        include_tool_output_in_reasoning: bool = False,
        include_tool_done_in_reasoning: bool = True,
    ) -> None:
        self.protocol = protocol
        effort = parse_reasoning_effort(reasoning_effort)
        self.effort = effort
        self.composer = ReasoningComposer(
            effort=effort,
            include_tool_output=include_tool_output_in_reasoning,
            include_tool_done=include_tool_done_in_reasoning,
        )
        self.tools: dict[str, ToolState] = {}
        self.session_id: Optional[str] = None
        self.user: Optional[str] = None
        self.model: Optional[str] = None
        self._done = False
        self._anthropic_index_to_id: dict[int, str] = {}

    def feed_sse_lines(self, lines: Iterable[str]) -> Iterator[StreamEvent]:
        for event_name, data in iter_sse_lines(lines):
            yield from self.feed_sse_event(event_name, data)

    def feed_sse_text(self, text: str) -> Iterator[StreamEvent]:
        yield from self.feed_sse_lines(text.splitlines())

    def feed_sse_event(self, event_name: Optional[str], data: str) -> Iterator[StreamEvent]:
        if data.strip() == "[DONE]":
            if not self._done:
                self._done = True
                yield StreamEvent(
                    type=StreamEventType.DONE,
                    reasoning=self.composer.reasoning,
                    content=self.composer.content,
                    session_id=self.session_id,
                    user=self.user,
                    model=self.model,
                )
            return

        obj = _as_dict(data)
        if obj is None:
            return

        if self.protocol == Protocol.CHAT_COMPLETIONS:
            yield from self._parse_chat_completions(obj)
        elif self.protocol == Protocol.RESPONSES:
            yield from self._parse_responses(event_name, obj)
        else:
            yield from self._parse_messages(event_name, obj)

    # ── OpenAI Chat Completions ───────────────────────────────────────────

    def _parse_chat_completions(self, obj: dict[str, Any]) -> Iterator[StreamEvent]:
        if "model" in obj and isinstance(obj["model"], str):
            self.model = obj["model"]

        # atomcode meta at top-level choice delta
        choices = obj.get("choices") or []
        if not choices:
            # non-stream full body
            yield from self._parse_chat_completion_full(obj)
            return

        choice = choices[0] if isinstance(choices[0], dict) else {}
        delta = choice.get("delta") or {}
        finish = choice.get("finish_reason")

        atom = delta.get("atomcode") if isinstance(delta.get("atomcode"), dict) else None
        if atom and atom.get("type") == "done":
            self.session_id = atom.get("session_id") or self.session_id
            self.user = atom.get("user") or self.user
        if atom and atom.get("type") == "error":
            yield StreamEvent(
                type=StreamEventType.ERROR,
                error=str(atom.get("message") or "error"),
                reasoning=self.composer.reasoning,
                content=self.composer.content,
                raw=obj,
            )
            return
        if atom and atom.get("type") == "runtime_info":
            yield StreamEvent(
                type=StreamEventType.RUNTIME,
                model=str(atom.get("model") or self.model or ""),
                reasoning=self.composer.reasoning,
                content=self.composer.content,
                raw=atom,
            )

        # thinking
        reasoning = delta.get("reasoning_content") or delta.get("reasoning")
        if isinstance(reasoning, str) and reasoning:
            yield self.composer.on_thinking(reasoning)

        # answer
        content = delta.get("content")
        if isinstance(content, str) and content:
            yield self.composer.on_content(content)

        # tools (parallel)
        tool_calls = delta.get("tool_calls")
        if isinstance(tool_calls, list):
            for tc in tool_calls:
                if not isinstance(tc, dict):
                    continue
                yield from self._ingest_openai_tool_call(tc)

        if finish == "stop" and not self._done:
            # may still get [DONE]; don't force done yet unless atomcode.done already set session
            pass

    def _parse_chat_completion_full(self, obj: dict[str, Any]) -> Iterator[StreamEvent]:
        """Non-stream chat.completion body."""
        atom = obj.get("atomcode") if isinstance(obj.get("atomcode"), dict) else {}
        self.session_id = atom.get("session_id") or self.session_id
        self.user = atom.get("user") or self.user
        choices = obj.get("choices") or []
        if not choices:
            return
        msg = (choices[0] or {}).get("message") or {}
        rc = msg.get("reasoning_content") or ""
        if rc:
            yield self.composer.on_thinking(str(rc))
        # tools summary may be in atomcode_tools text — leave as content side channel
        tools_txt = msg.get("atomcode_tools")
        if tools_txt:
            yield self.composer.on_thinking(str(tools_txt) if str(tools_txt).endswith("\n") else str(tools_txt) + "\n")
        content = msg.get("content") or ""
        if content:
            yield self.composer.on_content(str(content))
        self._done = True
        yield StreamEvent(
            type=StreamEventType.DONE,
            reasoning=self.composer.reasoning,
            content=self.composer.content,
            session_id=self.session_id,
            user=self.user,
            model=self.model or obj.get("model"),
        )

    def _ingest_openai_tool_call(self, tc: dict[str, Any]) -> Iterator[StreamEvent]:
        call_id = str(tc.get("id") or tc.get("call_id") or "")
        if not call_id:
            return
        fn = tc.get("function") if isinstance(tc.get("function"), dict) else {}
        name = fn.get("name") if isinstance(fn, dict) else None
        arguments = fn.get("arguments") if isinstance(fn, dict) else None
        children = tc.get("children") if isinstance(tc.get("children"), list) else None
        progress = tc.get("progress") if isinstance(tc.get("progress"), dict) else None
        tool, out_delta = _merge_tool(
            self.tools,
            call_id=call_id,
            index=tc.get("index"),
            name=str(name) if name else None,
            arguments=str(arguments) if arguments is not None else None,
            status=str(tc.get("status") or "in_progress"),
            output_delta=str(tc["output_delta"]) if tc.get("output_delta") is not None else None,
            output=str(tc["output"]) if tc.get("output") is not None else None,
            success=tc.get("success") if isinstance(tc.get("success"), bool) else None,
            duration_ms=tc.get("duration_ms"),
            batch_id=str(tc["batch_id"]) if tc.get("batch_id") else None,
            children=children,
            progress=progress,
        )
        yield from self.composer.on_tool_update(tool, output_delta=out_delta)

    # ── OpenAI Responses ──────────────────────────────────────────────────

    def _parse_responses(self, event_name: Optional[str], obj: dict[str, Any]) -> Iterator[StreamEvent]:
        et = event_name or obj.get("type") or ""

        if et == "response.created":
            resp = obj.get("response") or {}
            self.model = resp.get("model") or self.model
            return

        if et in ("response.reasoning.delta", "response.reasoning_summary_text.delta"):
            delta = obj.get("delta") or ""
            if delta:
                yield self.composer.on_thinking(str(delta))
            return

        if et == "response.output_text.delta":
            delta = obj.get("delta") or ""
            if delta:
                yield self.composer.on_content(str(delta))
            return

        if et == "response.output_item.added":
            item = obj.get("item") or {}
            if item.get("type") == "function_call":
                call_id = str(item.get("call_id") or item.get("id") or "")
                if call_id:
                    tool, _ = _merge_tool(
                        self.tools,
                        call_id=call_id,
                        index=obj.get("output_index"),
                        name=str(item.get("name") or ""),
                        status="in_progress",
                        children=item.get("children") if isinstance(item.get("children"), list) else None,
                    )
                    yield from self.composer.on_tool_update(tool)
            return

        if et == "response.function_call_arguments.done":
            call_id = str(obj.get("call_id") or "")
            if call_id:
                tool, _ = _merge_tool(
                    self.tools,
                    call_id=call_id,
                    index=obj.get("output_index"),
                    arguments=str(obj.get("arguments") or ""),
                    status="in_progress",
                )
                # name may already be known
                if tool.name:
                    yield from self.composer.on_tool_update(tool)
            return

        if et == "response.function_call_output.delta":
            call_id = str(obj.get("call_id") or "")
            delta = str(obj.get("delta") or "")
            if call_id and delta:
                tool, out_delta = _merge_tool(
                    self.tools,
                    call_id=call_id,
                    index=obj.get("output_index"),
                    status="in_progress",
                    output_delta=delta,
                )
                yield from self.composer.on_tool_update(tool, output_delta=out_delta)
            return

        if et == "response.function_call_progress":
            call_id = str(obj.get("call_id") or "")
            if not call_id:
                return
            progress = obj.get("progress") if isinstance(obj.get("progress"), dict) else None
            children = obj.get("children") if isinstance(obj.get("children"), list) else None
            tool, _ = _merge_tool(
                self.tools,
                call_id=call_id,
                index=obj.get("output_index"),
                status="in_progress",
                progress=progress,
                children=children,
            )
            yield from self.composer.on_tool_update(tool)
            return

        if et == "response.function_call_output.done":
            call_id = str(obj.get("call_id") or "")
            if call_id:
                success = obj.get("success")
                if success is None:
                    success = True
                tool, _ = _merge_tool(
                    self.tools,
                    call_id=call_id,
                    index=obj.get("output_index"),
                    name=str(obj.get("name") or "") or None,
                    status="completed" if success else "failed",
                    output=str(obj.get("output") or ""),
                    success=bool(success),
                    duration_ms=obj.get("duration_ms"),
                )
                yield from self.composer.on_tool_update(tool)
            return

        if et == "response.completed":
            resp = obj.get("response") or {}
            atom = resp.get("atomcode") if isinstance(resp.get("atomcode"), dict) else {}
            self.session_id = atom.get("session_id") or self.session_id
            self.user = atom.get("user") or self.user
            self._done = True
            yield StreamEvent(
                type=StreamEventType.DONE,
                reasoning=self.composer.reasoning,
                content=self.composer.content,
                session_id=self.session_id,
                user=self.user,
                stop_reason=atom.get("stop_reason"),
                model=self.model or resp.get("model"),
                raw=obj,
            )
            return

        if et == "response.failed":
            resp = obj.get("response") or {}
            err = (resp.get("error") or {}).get("message") if isinstance(resp.get("error"), dict) else "failed"
            yield StreamEvent(
                type=StreamEventType.ERROR,
                error=str(err),
                reasoning=self.composer.reasoning,
                content=self.composer.content,
                raw=obj,
            )
            return

        # Non-stream full response object
        if obj.get("object") == "response" and obj.get("status") == "completed":
            yield from self._parse_responses_full(obj)

    def _parse_responses_full(self, obj: dict[str, Any]) -> Iterator[StreamEvent]:
        atom = obj.get("atomcode") if isinstance(obj.get("atomcode"), dict) else {}
        self.session_id = atom.get("session_id") or self.session_id
        self.user = atom.get("user") or self.user
        for item in obj.get("output") or []:
            if not isinstance(item, dict):
                continue
            t = item.get("type")
            if t == "reasoning":
                for c in item.get("content") or []:
                    if isinstance(c, dict) and c.get("text"):
                        yield self.composer.on_thinking(str(c["text"]))
            elif t in ("atomcode_tools",):
                for c in item.get("content") or []:
                    if isinstance(c, dict) and c.get("text"):
                        yield self.composer.on_thinking(str(c["text"]) + "\n")
            elif t == "message":
                for c in item.get("content") or []:
                    if isinstance(c, dict) and c.get("text"):
                        yield self.composer.on_content(str(c["text"]))
        self._done = True
        yield StreamEvent(
            type=StreamEventType.DONE,
            reasoning=self.composer.reasoning,
            content=self.composer.content,
            session_id=self.session_id,
            user=self.user,
            model=obj.get("model"),
        )

    # ── Anthropic Messages ────────────────────────────────────────────────

    def _parse_messages(self, event_name: Optional[str], obj: dict[str, Any]) -> Iterator[StreamEvent]:
        et = event_name or obj.get("type") or ""

        if et == "message_start":
            msg = obj.get("message") or {}
            self.model = msg.get("model") or self.model
            return

        if et == "content_block_start":
            block = obj.get("content_block") or {}
            btype = block.get("type")
            if btype == "tool_use":
                call_id = str(block.get("id") or "")
                if call_id:
                    tool, _ = _merge_tool(
                        self.tools,
                        call_id=call_id,
                        index=obj.get("index"),
                        name=str(block.get("name") or ""),
                        status="in_progress",
                        children=block.get("children") if isinstance(block.get("children"), list) else None,
                    )
                    # stash index → id for later deltas
                    self._anthropic_index_to_id[int(obj.get("index") or 0)] = call_id
                    yield from self.composer.on_tool_update(tool)
            return

        if et == "content_block_delta":
            idx = int(obj.get("index") or 0)
            delta = obj.get("delta") or {}
            dtype = delta.get("type")
            if dtype == "thinking_delta":
                text = delta.get("thinking") or ""
                if text:
                    yield self.composer.on_thinking(str(text))
            elif dtype == "text_delta":
                text = delta.get("text") or ""
                if text:
                    yield self.composer.on_content(str(text))
            elif dtype == "input_json_delta":
                call_id = self._anthropic_index_to_id.get(idx) or str(delta.get("tool_use_id") or "")
                partial = delta.get("partial_json") or ""
                if call_id and partial:
                    tool, _ = _merge_tool(
                        self.tools,
                        call_id=call_id,
                        index=idx,
                        arguments=str(partial),
                        status="in_progress",
                    )
                    # Don't re-announce start; composer dedupes by announced set
                    yield from self.composer.on_tool_update(tool)
            elif dtype == "tool_output_delta":
                call_id = str(delta.get("tool_use_id") or self._anthropic_index_to_id.get(idx) or "")
                partial = str(delta.get("partial_output") or "")
                if call_id and partial:
                    tool, out_delta = _merge_tool(
                        self.tools,
                        call_id=call_id,
                        index=idx,
                        status="in_progress",
                        output_delta=partial,
                    )
                    yield from self.composer.on_tool_update(tool, output_delta=out_delta)
            elif dtype == "tool_progress":
                call_id = str(delta.get("tool_use_id") or self._anthropic_index_to_id.get(idx) or "")
                if call_id:
                    tool, _ = _merge_tool(
                        self.tools,
                        call_id=call_id,
                        index=idx,
                        status="in_progress",
                        progress=delta.get("progress") if isinstance(delta.get("progress"), dict) else None,
                        children=delta.get("children") if isinstance(delta.get("children"), list) else None,
                    )
                    yield from self.composer.on_tool_update(tool)
            elif dtype == "tool_result":
                call_id = str(delta.get("tool_use_id") or self._anthropic_index_to_id.get(idx) or "")
                if call_id:
                    is_err = bool(delta.get("is_error"))
                    tool, _ = _merge_tool(
                        self.tools,
                        call_id=call_id,
                        index=idx,
                        name=str(delta.get("name") or "") or None,
                        status="failed" if is_err else "completed",
                        output=str(delta.get("content") or ""),
                        success=not is_err,
                        duration_ms=delta.get("duration_ms"),
                    )
                    yield from self.composer.on_tool_update(tool)
            return

        if et == "message_delta":
            atom = obj.get("atomcode") if isinstance(obj.get("atomcode"), dict) else {}
            self.session_id = atom.get("session_id") or self.session_id
            self.user = atom.get("user") or self.user
            return

        if et == "message_stop":
            self._done = True
            yield StreamEvent(
                type=StreamEventType.DONE,
                reasoning=self.composer.reasoning,
                content=self.composer.content,
                session_id=self.session_id,
                user=self.user,
                model=self.model,
                raw=obj,
            )
            return

        if et == "error":
            err = obj.get("error") if isinstance(obj.get("error"), dict) else {}
            yield StreamEvent(
                type=StreamEventType.ERROR,
                error=str(err.get("message") or "error"),
                reasoning=self.composer.reasoning,
                content=self.composer.content,
                raw=obj,
            )
            return

        # Non-stream Anthropic message
        if obj.get("type") == "message" and obj.get("role") == "assistant":
            yield from self._parse_messages_full(obj)

    def _parse_messages_full(self, obj: dict[str, Any]) -> Iterator[StreamEvent]:
        atom = obj.get("atomcode") if isinstance(obj.get("atomcode"), dict) else {}
        self.session_id = atom.get("session_id") or self.session_id
        self.user = atom.get("user") or self.user
        for block in obj.get("content") or []:
            if not isinstance(block, dict):
                continue
            btype = block.get("type")
            if btype == "thinking":
                yield self.composer.on_thinking(str(block.get("thinking") or ""))
            elif btype == "text":
                yield self.composer.on_content(str(block.get("text") or ""))
            elif btype == "tool_use":
                call_id = str(block.get("id") or "")
                if call_id:
                    tool, _ = _merge_tool(
                        self.tools,
                        call_id=call_id,
                        name=str(block.get("name") or ""),
                        arguments=json.dumps(block.get("input") or {}, ensure_ascii=False),
                        status="in_progress",
                    )
                    yield from self.composer.on_tool_update(tool)
        self._done = True
        yield StreamEvent(
            type=StreamEventType.DONE,
            reasoning=self.composer.reasoning,
            content=self.composer.content,
            session_id=self.session_id,
            user=self.user,
            model=obj.get("model"),
        )

