"""Unified stream events produced by the parser."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Optional


class StreamEventType(str, Enum):
    """High-level event kinds for UI rendering."""

    REASONING = "reasoning"  # thinking + tool/subagent progress lines
    CONTENT = "content"  # final assistant answer
    TOOL = "tool"  # structured tool card update (optional for UIs)
    SUBAGENT = "subagent"  # structured subagent row update
    RUNTIME = "runtime"
    DONE = "done"
    ERROR = "error"
    RAW = "raw"  # unparsed / passthrough


class ReasoningEffort(str, Enum):
    """How much to put into the client-side ``reasoning`` display stream.

    This only filters **SDK display** (what goes into ``reasoning_delta``).
    The server still runs full thinking/tools; structured ``tool`` / ``subagent``
    events may still be emitted for richer UIs.

    - ``low``: only formal answer (``content``). No thinking, tools, or subagents
      in the reasoning pane.
    - ``medium`` (default): tool starts + subagent progress (+ optional tool done),
      **without** model thinking/reasoning detail.
    - ``max``: full timeline — thinking + tools + subagents.

    Aliases accepted by :func:`parse_reasoning_effort`: ``min``→low, ``high``→max,
    ``full``→max, ``default``→medium.
    """

    LOW = "low"
    MEDIUM = "medium"
    MAX = "max"


def parse_reasoning_effort(value: str | ReasoningEffort | None) -> ReasoningEffort:
    """Parse CLI/env strings into :class:`ReasoningEffort`."""
    if value is None or value == "":
        return ReasoningEffort.MEDIUM
    if isinstance(value, ReasoningEffort):
        return value
    key = str(value).strip().lower()
    aliases = {
        "low": ReasoningEffort.LOW,
        "min": ReasoningEffort.LOW,
        "minimal": ReasoningEffort.LOW,
        "medium": ReasoningEffort.MEDIUM,
        "med": ReasoningEffort.MEDIUM,
        "default": ReasoningEffort.MEDIUM,
        "max": ReasoningEffort.MAX,
        "maximum": ReasoningEffort.MAX,
        "high": ReasoningEffort.MAX,
        "full": ReasoningEffort.MAX,
    }
    if key not in aliases:
        raise ValueError(
            f"invalid reasoning_effort={value!r}; expected low|medium|max "
            f"(aliases: min, med, high, full)"
        )
    return aliases[key]


@dataclass
class ToolState:
    """One tool call card (parallel-safe by ``id``)."""

    id: str
    index: int = 0
    name: str = ""
    arguments: str = ""
    status: str = "in_progress"  # in_progress | completed | failed
    output: str = ""
    success: Optional[bool] = None
    duration_ms: Optional[int] = None
    batch_id: Optional[str] = None
    children: dict[str, "SubagentState"] = field(default_factory=dict)


@dataclass
class SubagentState:
    """One subagent (explore#1 / worker#2) under a parent ``task`` tool."""

    id: str
    state: str = "queued"  # queued | running | completed | failed
    label: str = ""
    message: str = ""
    model: Optional[str] = None
    description: Optional[str] = None
    tokens: Optional[int] = None
    parent_call_id: Optional[str] = None


@dataclass
class StreamEvent:
    """One incremental frame from a streaming turn.

    For chat UIs that only want two panes:

    - ``reasoning_delta`` — 思考过程 + 「正在调用…」+ 子代理进度（可拼进 reasoning 区）
    - ``content_delta`` — 正式回答

    Structured ``tool`` / ``subagent`` fields are filled when available so richer
    UIs can render cards without re-parsing text.
    """

    type: StreamEventType
    reasoning_delta: str = ""
    content_delta: str = ""
    # Cumulative snapshots (optional convenience)
    reasoning: str = ""
    content: str = ""
    tool: Optional[ToolState] = None
    subagent: Optional[SubagentState] = None
    session_id: Optional[str] = None
    user: Optional[str] = None
    stop_reason: Optional[str] = None
    model: Optional[str] = None
    error: Optional[str] = None
    raw: Optional[dict[str, Any]] = None

    @property
    def is_terminal(self) -> bool:
        return self.type in (StreamEventType.DONE, StreamEventType.ERROR)


@dataclass
class TurnResult:
    """Final aggregated result of one streaming turn.

    ``reasoning`` already merges model thinking + tool starts + subagent progress::

        先分析一下项目结构。
        正在调用 read
        正在调用 task
        子代理 explore#1: 分析 auth 模块 [glm]
        子代理 worker#2: 改代码中
        工具 task 完成 (1200ms)

    ``content`` is the formal assistant answer only.
    """

    reasoning: str = ""
    content: str = ""
    session_id: Optional[str] = None
    user: Optional[str] = None
    stop_reason: Optional[str] = None
    model: Optional[str] = None
    error: Optional[str] = None
    tools: dict[str, ToolState] = field(default_factory=dict)

    @property
    def ok(self) -> bool:
        return self.error is None

    def to_dict(self) -> dict[str, Any]:
        return {
            "reasoning": self.reasoning,
            "content": self.content,
            "session_id": self.session_id,
            "user": self.user,
            "stop_reason": self.stop_reason,
            "model": self.model,
            "error": self.error,
            "tools": {
                tid: {
                    "id": t.id,
                    "name": t.name,
                    "status": t.status,
                    "success": t.success,
                    "children": {
                        sid: {
                            "id": s.id,
                            "state": s.state,
                            "message": s.message,
                            "model": s.model,
                        }
                        for sid, s in t.children.items()
                    },
                }
                for tid, t in self.tools.items()
            },
        }


def collect_events(events: Any) -> TurnResult:
    """Drain an event iterator into :class:`TurnResult`."""
    result = TurnResult()
    tools: dict[str, ToolState] = {}
    for ev in events:
        if not isinstance(ev, StreamEvent):
            continue
        if ev.reasoning:
            result.reasoning = ev.reasoning
        if ev.content:
            result.content = ev.content
        if ev.session_id:
            result.session_id = ev.session_id
        if ev.user:
            result.user = ev.user
        if ev.stop_reason:
            result.stop_reason = ev.stop_reason
        if ev.model:
            result.model = ev.model
        if ev.error:
            result.error = ev.error
        if ev.tool is not None:
            tools[ev.tool.id] = ev.tool
    result.tools = tools
    return result
