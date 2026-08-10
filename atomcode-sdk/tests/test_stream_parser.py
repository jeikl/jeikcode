"""Unit tests for AtomCode stream parser + reasoning composer (no network)."""

from __future__ import annotations

from atomcode_sdk.events import StreamEventType
from atomcode_sdk.stream import Protocol, StreamParser


def _sse_chat_chunk(delta: dict) -> str:
    import json

    body = {
        "id": "chatcmpl-x",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": "m",
        "choices": [{"index": 0, "delta": delta, "finish_reason": None}],
    }
    return f"data: {json.dumps(body, ensure_ascii=False)}\n\n"


def test_reasoning_includes_thinking_tools_and_subagents():
    raw = "".join(
        [
            _sse_chat_chunk({"role": "assistant"}),
            _sse_chat_chunk({"reasoning_content": "先读 README。\n"}),
            _sse_chat_chunk(
                {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "call_read",
                            "type": "function",
                            "status": "in_progress",
                            "function": {"name": "read", "arguments": "{}"},
                        }
                    ]
                }
            ),
            _sse_chat_chunk(
                {
                    "tool_calls": [
                        {
                            "index": 1,
                            "id": "call_task",
                            "type": "function",
                            "status": "in_progress",
                            "function": {
                                "name": "task",
                                "arguments": '{"tasks":[]}',
                            },
                            "children": [
                                {
                                    "id": "explore#1",
                                    "state": "queued",
                                    "label": "explore#1",
                                }
                            ],
                        }
                    ]
                }
            ),
            _sse_chat_chunk(
                {
                    "tool_calls": [
                        {
                            "index": 1,
                            "id": "call_task",
                            "type": "function",
                            "status": "in_progress",
                            "progress": {
                                "subtask_id": "explore#1",
                                "state": "running",
                                "label": "explore#1",
                                "message": "分析 auth 模块",
                                "model": "glm",
                            },
                            "children": [
                                {
                                    "id": "explore#1",
                                    "state": "running",
                                    "label": "explore#1",
                                    "message": "分析 auth 模块",
                                    "model": "glm",
                                }
                            ],
                        }
                    ]
                }
            ),
            _sse_chat_chunk(
                {
                    "tool_calls": [
                        {
                            "index": 1,
                            "id": "call_task",
                            "status": "in_progress",
                            "progress": {
                                "subtask_id": "worker#2",
                                "state": "running",
                                "message": "改代码中",
                            },
                            "children": [
                                {
                                    "id": "worker#2",
                                    "state": "running",
                                    "message": "改代码中",
                                }
                            ],
                        }
                    ]
                }
            ),
            _sse_chat_chunk({"content": "这是正式回答。"}),
            "data: [DONE]\n\n",
        ]
    )

    # max: full thinking + tools + subagents
    parser = StreamParser(Protocol.CHAT_COMPLETIONS, reasoning_effort="max")
    events = list(parser.feed_sse_text(raw))
    reasoning = "".join(e.reasoning_delta for e in events if e.reasoning_delta)
    content = "".join(e.content_delta for e in events if e.content_delta)

    assert "先读 README" in reasoning
    assert "正在调用 read" in reasoning
    assert "正在调用 task" in reasoning
    assert "子代理 explore#1" in reasoning
    assert "分析 auth" in reasoning
    assert "子代理 worker#2" in reasoning
    assert "改代码中" in reasoning
    assert content == "这是正式回答。"
    assert any(e.type == StreamEventType.DONE for e in events)


def test_parallel_tools_keep_separate_ids():
    raw = "".join(
        [
            _sse_chat_chunk(
                {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "a",
                            "status": "in_progress",
                            "function": {"name": "read", "arguments": "{}"},
                        },
                        {
                            "index": 1,
                            "id": "b",
                            "status": "in_progress",
                            "function": {"name": "grep", "arguments": "{}"},
                        },
                    ]
                }
            ),
            "data: [DONE]\n\n",
        ]
    )
    parser = StreamParser(Protocol.CHAT_COMPLETIONS)
    list(parser.feed_sse_text(raw))
    assert "a" in parser.tools and "b" in parser.tools
    assert parser.tools["a"].name == "read"
    assert parser.tools["b"].name == "grep"


def test_responses_function_call_progress():
    lines = [
        "event: response.created",
        'data: {"type":"response.created","response":{"id":"r1","model":"m","status":"in_progress"}}',
        "",
        "event: response.reasoning.delta",
        'data: {"type":"response.reasoning.delta","delta":"想一下\\n"}',
        "",
        "event: response.output_item.added",
        'data: {"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"c1","name":"task","status":"in_progress"}}',
        "",
        "event: response.function_call_progress",
        'data: {"type":"response.function_call_progress","call_id":"c1","output_index":0,"progress":{"subtask_id":"explore#1","state":"running","message":"搜符号"},"children":[{"id":"explore#1","state":"running","message":"搜符号"}]}',
        "",
        "event: response.output_text.delta",
        'data: {"type":"response.output_text.delta","delta":"答案"}',
        "",
        "event: response.completed",
        'data: {"type":"response.completed","response":{"id":"r1","status":"completed","atomcode":{"session_id":"s1","user":"u1"}}}',
        "",
    ]
    parser = StreamParser(Protocol.RESPONSES, reasoning_effort="max")
    events = list(parser.feed_sse_lines(lines))
    reasoning = "".join(e.reasoning_delta for e in events if e.reasoning_delta)
    assert "想一下" in reasoning
    assert "正在调用 task" in reasoning
    assert "子代理 explore#1" in reasoning
    assert "搜符号" in reasoning
    content = "".join(e.content_delta for e in events if e.content_delta)
    assert content == "答案"
    done = [e for e in events if e.type == StreamEventType.DONE][0]
    assert done.session_id == "s1"
    assert done.user == "u1"


def test_reasoning_effort_medium_hides_thinking():
    raw = "".join(
        [
            _sse_chat_chunk({"reasoning_content": "秘密思考细节\n"}),
            _sse_chat_chunk(
                {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "c1",
                            "status": "in_progress",
                            "function": {"name": "bash", "arguments": "{}"},
                        }
                    ]
                }
            ),
            _sse_chat_chunk(
                {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "c1",
                            "status": "in_progress",
                            "progress": {
                                "subtask_id": "explore#1",
                                "state": "running",
                                "message": "扫代码",
                            },
                            "children": [
                                {
                                    "id": "explore#1",
                                    "state": "running",
                                    "message": "扫代码",
                                }
                            ],
                        }
                    ]
                }
            ),
            _sse_chat_chunk({"content": "正文答案"}),
            "data: [DONE]\n\n",
        ]
    )
    med = StreamParser(Protocol.CHAT_COMPLETIONS, reasoning_effort="medium")
    r_med = "".join(e.reasoning_delta for e in med.feed_sse_text(raw) if e.reasoning_delta)
    c_med = "".join(e.content_delta for e in StreamParser(Protocol.CHAT_COMPLETIONS, reasoning_effort="medium").feed_sse_text(raw) if e.content_delta)
    assert "秘密思考细节" not in r_med
    assert "正在调用 bash" in r_med
    assert "子代理 explore#1" in r_med
    assert c_med == "正文答案"

    low = StreamParser(Protocol.CHAT_COMPLETIONS, reasoning_effort="low")
    events_low = list(low.feed_sse_text(raw))
    r_low = "".join(e.reasoning_delta for e in events_low if e.reasoning_delta)
    c_low = "".join(e.content_delta for e in events_low if e.content_delta)
    assert r_low == ""
    assert "正在调用" not in r_low
    assert c_low == "正文答案"

    mx = StreamParser(Protocol.CHAT_COMPLETIONS, reasoning_effort="max")
    r_max = "".join(e.reasoning_delta for e in mx.feed_sse_text(raw) if e.reasoning_delta)
    assert "秘密思考细节" in r_max
    assert "正在调用 bash" in r_max


def test_parse_reasoning_effort_aliases():
    from atomcode_sdk import ReasoningEffort, parse_reasoning_effort

    assert parse_reasoning_effort("low") is ReasoningEffort.LOW
    assert parse_reasoning_effort("min") is ReasoningEffort.LOW
    assert parse_reasoning_effort(None) is ReasoningEffort.MEDIUM
    assert parse_reasoning_effort("high") is ReasoningEffort.MAX
    assert parse_reasoning_effort("full") is ReasoningEffort.MAX


def test_collect_events_turn_result():
    from atomcode_sdk import collect_events

    raw = "".join(
        [
            _sse_chat_chunk({"reasoning_content": "思考A\n"}),
            _sse_chat_chunk(
                {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "c1",
                            "status": "in_progress",
                            "function": {"name": "bash", "arguments": "{}"},
                        }
                    ]
                }
            ),
            _sse_chat_chunk({"content": "答案B"}),
            "data: [DONE]\n\n",
        ]
    )
    parser = StreamParser(Protocol.CHAT_COMPLETIONS, reasoning_effort="max")
    result = collect_events(parser.feed_sse_text(raw))
    assert "思考A" in result.reasoning
    assert "正在调用 bash" in result.reasoning
    assert result.content == "答案B"
    assert result.ok


def test_anthropic_thinking_and_tool_progress():
    lines = [
        "event: message_start",
        'data: {"type":"message_start","message":{"id":"m1","model":"m","role":"assistant","content":[]}}',
        "",
        "event: content_block_start",
        'data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}',
        "",
        "event: content_block_delta",
        'data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"推理中"}}',
        "",
        "event: content_block_stop",
        'data: {"type":"content_block_stop","index":0}',
        "",
        "event: content_block_start",
        'data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"bash","input":{}}}',
        "",
        "event: content_block_delta",
        'data: {"type":"content_block_delta","index":1,"delta":{"type":"tool_progress","tool_use_id":"t1","progress":{"subtask_id":"explore#1","state":"running","message":"跑测试"},"children":[{"id":"explore#1","state":"running","message":"跑测试"}]}}',
        "",
        "event: message_stop",
        'data: {"type":"message_stop"}',
        "",
    ]
    parser = StreamParser(Protocol.MESSAGES, reasoning_effort="max")
    events = list(parser.feed_sse_lines(lines))
    reasoning = "".join(e.reasoning_delta for e in events if e.reasoning_delta)
    assert "推理中" in reasoning
    assert "正在调用 bash" in reasoning
    assert "子代理 explore#1" in reasoning
    assert "跑测试" in reasoning
