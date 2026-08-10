#!/usr/bin/env python3
"""Async streaming demos for all three AtomCode serve protocols.

Same CLI surface as stream_all_sync.py, but uses AsyncAtomCodeClient.

  python examples/stream_all_async.py chat -s "用中文" -p "hi"
  python examples/stream_all_async.py all --system-file ./sys.txt -p "总结 README"
  python examples/stream_all_async.py responses --user async_demo_1 -p "列目录"

Env: ATOMCODE_BASE, ATOMCODE_TOKEN, ATOMCODE_MODEL, ATOMCODE_USER, ATOMCODE_SYSTEM
"""

from __future__ import annotations

import argparse
import asyncio
import sys
from pathlib import Path

_EXAMPLES = Path(__file__).resolve().parent
_ROOT = _EXAMPLES.parent
for _p in (_ROOT, _EXAMPLES):
    if str(_p) not in sys.path:
        sys.path.insert(0, str(_p))

from atomcode_sdk import AsyncAtomCodeClient  # noqa: E402

from _common import (  # noqa: E402
    DualPanePrinter,
    aconsume,
    add_common_args,
    resolve_effort,
    resolve_prompt,
    resolve_system,
)


FORMATS = ("chat", "responses", "messages", "all")


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        description="AtomCode SDK: stream chat / responses / messages (async)",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    p.add_argument("format", choices=FORMATS)
    add_common_args(p)
    p.add_argument("--no-user", action="store_true")
    return p


async def run_one(
    client: AsyncAtomCodeClient,
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
        "chat": "Chat Completions  /v1/chat/completions  [async]",
        "responses": "Responses         /v1/responses  [async]",
        "messages": "Anthropic Messages /v1/messages  [async]",
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
        aiter = client.chat.stream(
            system=system,
            messages=[{"role": "user", "content": prompt}],
            **kwargs,
        )
    elif fmt == "responses":
        aiter = client.responses.stream(
            instructions=system,
            input=prompt,
            **kwargs,
        )
    else:
        aiter = client.messages.stream(
            system=system,
            messages=[{"role": "user", "content": prompt}],
            **kwargs,
        )

    return await aconsume(printer, aiter)


async def amain(argv: list[str] | None = None) -> int:
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
            f"[async] base={args.base} model={args.model or '(default)'} "
            f"user={user or '(ephemeral)'} effort={effort.value} formats={formats}\n"
            f"system={'(none)' if not system else repr(system[:60])}\n"
            f"prompt={prompt!r}",
            flush=True,
        )

    code = 0
    async with AsyncAtomCodeClient(args.base, token=args.token) as client:
        for fmt in formats:
            rc = await run_one(
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
                if args.format != "all":
                    return code
    return code


def main(argv: list[str] | None = None) -> int:
    try:
        return asyncio.run(amain(argv))
    except KeyboardInterrupt:
        print("\n[interrupted]", file=sys.stderr)
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
