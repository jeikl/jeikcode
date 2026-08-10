#!/usr/bin/env python3
"""Sync streaming demos for all three AtomCode serve protocols.

Covers:
  - chat_completions  → POST /v1/chat/completions
  - responses         → POST /v1/responses
  - messages          → POST /v1/messages (Anthropic)
  - system / instructions from flag or file
  - session key via --user
  - dual-pane: reasoning (thinking + tools + subagents) vs content

Examples
--------
  # Chat Completions + system prompt
  python examples/stream_all_sync.py chat \\
    --system "用中文，先思考再回答" \\
    -p "总结当前项目"

  # Responses API
  python examples/stream_all_sync.py responses \\
    --system-file ./sys.txt \\
    --user demo_thread_1 \\
    -p "列出工作区根目录要点"

  # Anthropic Messages
  python examples/stream_all_sync.py messages -s "简洁" -p "hi"

  # Run all three formats in sequence (same prompt)
  python examples/stream_all_sync.py all -s "用中文" -p "一句话介绍自己"

Env: ATOMCODE_BASE, ATOMCODE_TOKEN, ATOMCODE_MODEL, ATOMCODE_USER, ATOMCODE_SYSTEM
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

# Allow running without install: python examples/stream_all_sync.py
_EXAMPLES = Path(__file__).resolve().parent
_ROOT = _EXAMPLES.parent
for _p in (_ROOT, _EXAMPLES):
    if str(_p) not in sys.path:
        sys.path.insert(0, str(_p))

from atomcode_sdk import AtomCodeClient  # noqa: E402

from _common import (  # noqa: E402
    DualPanePrinter,
    add_common_args,
    resolve_effort,
    resolve_prompt,
    resolve_system,
)


FORMATS = ("chat", "responses", "messages", "all")


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        description="AtomCode SDK: stream chat / responses / messages (sync)",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    p.add_argument(
        "format",
        choices=FORMATS,
        help="Which wire protocol to use (all = run three in sequence)",
    )
    add_common_args(p)
    p.add_argument(
        "--no-user",
        action="store_true",
        help="Do not send user session key (ephemeral session each call)",
    )
    return p


def run_one(
    client: AtomCodeClient,
    fmt: str,
    *,
    model: str | None,
    user: str | None,
    system: str | None,
    prompt: str,
    include_tool_output: bool,
    reasoning_effort,
    quiet_meta: bool,
) -> int:
    printer = DualPanePrinter(quiet_meta=quiet_meta, base=client.base_url, token=client.token)
    title = {
        "chat": "Chat Completions  /v1/chat/completions",
        "responses": "Responses         /v1/responses",
        "messages": "Anthropic Messages /v1/messages",
    }[fmt]
    effort_label = getattr(reasoning_effort, "value", reasoning_effort)
    printer.begin(f"{title}  [effort={effort_label}]")

    kwargs = dict(
        model=model or None,
        user=user,
        include_tool_output_in_reasoning=include_tool_output,
        reasoning_effort=reasoning_effort,
    )

    if fmt == "chat":
        # system → messages[].role=system (prepended by SDK)
        stream = client.chat.stream(
            system=system,
            messages=[{"role": "user", "content": prompt}],
            **kwargs,
        )
    elif fmt == "responses":
        # system → instructions; user query → input string
        # (also supports messages= for chat-shaped clients)
        stream = client.responses.stream(
            instructions=system,
            input=prompt,
            **kwargs,
        )
    else:  # messages
        # system → Anthropic system field
        stream = client.messages.stream(
            system=system,
            messages=[{"role": "user", "content": prompt}],
            **kwargs,
        )

    return printer.consume(stream)


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        system = resolve_system(args)
        prompt = resolve_prompt(args)
    except SystemExit as e:
        print(e, file=sys.stderr)
        return 2

    user = None if args.no_user else args.user
    formats = list(FORMATS[:-1]) if args.format == "all" else [args.format]
    effort = resolve_effort(args)

    if not args.quiet_meta:
        print(
            f"base={args.base}\n"
            f"model={args.model or '(server default)'}\n"
            f"user={user or '(ephemeral)'}\n"
            f"reasoning_effort={effort.value}\n"
            f"system={'(none)' if not system else repr(system[:80] + ('…' if len(system) > 80 else ''))}\n"
            f"prompt={prompt!r}\n"
            f"formats={formats}",
            flush=True,
        )

    code = 0
    with AtomCodeClient(args.base, token=args.token) as client:
        # Optional: list models when model not set (helps first-time users)
        if not args.model and not args.quiet_meta:
            try:
                data = client.list_models()
                ids = [m.get("id") for m in (data.get("data") or []) if m.get("id")]
                if ids:
                    print(f"available models (sample): {ids[:8]}", flush=True)
            except Exception as e:
                print(f"[warn] list_models failed: {e}", file=sys.stderr)

        for fmt in formats:
            rc = run_one(
                client,
                fmt,
                model=args.model,
                user=user,
                system=system,
                prompt=prompt,
                include_tool_output=args.include_tool_output,
                reasoning_effort=effort,
                quiet_meta=args.quiet_meta,
            )
            if rc != 0:
                code = rc
                # continue other formats only when running "all"
                if args.format != "all":
                    return code
    return code


if __name__ == "__main__":
    raise SystemExit(main())
