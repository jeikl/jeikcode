"""AtomCode Python SDK — stream OpenAI/Anthropic-compatible serve endpoints.

Typical usage (reasoning pane merges thinking + tools + subagents)::

    from atomcode_sdk import AtomCodeClient

    client = AtomCodeClient("http://127.0.0.1:4096", token="...")
    for ev in client.chat.stream(
        messages=[{"role": "user", "content": "看一下 README"}],
        user="alice_proj1",
        model="my-model",
    ):
        if ev.reasoning_delta:
            print(ev.reasoning_delta, end="", flush=True)
        if ev.content_delta:
            print(ev.content_delta, end="", flush=True)

Or collect the whole turn::

    result = client.chat.run(messages=[...], user="alice_proj1", model="m")
    print(result.reasoning)  # 思考 + 正在调用 + 子代理进度
    print(result.content)    # 正式回答
"""

from .client import AsyncAtomCodeClient, AtomCodeClient
from .events import (
    ReasoningEffort,
    StreamEvent,
    StreamEventType,
    SubagentState,
    ToolState,
    TurnResult,
    collect_events,
    parse_reasoning_effort,
)
from .reasoning import ReasoningComposer
from .stream import Protocol, StreamParser

__all__ = [
    "AtomCodeClient",
    "AsyncAtomCodeClient",
    "StreamEvent",
    "StreamEventType",
    "ReasoningEffort",
    "parse_reasoning_effort",
    "ToolState",
    "SubagentState",
    "TurnResult",
    "collect_events",
    "StreamParser",
    "Protocol",
    "ReasoningComposer",
]

__version__ = "0.1.0"
