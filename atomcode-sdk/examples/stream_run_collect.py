#!/usr/bin/env python3
"""Non-interactive: stream one turn and print full reasoning + content JSON-ish.

Useful for piping / CI smoke tests.

  python examples/stream_run_collect.py chat -s "简洁" -p "hi"
  python examples/stream_run_collect.py responses -p "一句话"
  python examples/stream_run_collect.py messages --system-file ./sys.txt -p "hi"
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

_EXAMPLES = Path(__file__).resolve().parent
_ROOT = _EXAMPLES.parent
for _p in (_ROOT, _EXAMPLES):
    if str(_p) not in sys.path:
        sys.path.insert(0, str(_p))

from atomcode_sdk import AtomCodeClient  # noqa: E402

from _common import (  # noqa: E402
    add_common_args,
    print_result_summary,
    resolve_effort,
    resolve_prompt,
    resolve_system,
)


def main() -> int:
    p = argparse.ArgumentParser(description="Collect one streamed turn")
    p.add_argument("format", choices=("chat", "responses", "messages"))
    add_common_args(p)
    p.add_argument("--json", action="store_true", help="Print TurnResult.to_dict() as JSON")
    p.add_argument("--no-user", action="store_true")
    args = p.parse_args()

    system = resolve_system(args)
    prompt = resolve_prompt(args)
    user = None if args.no_user else args.user
    effort = resolve_effort(args)

    with AtomCodeClient(args.base, token=args.token) as client:
        try:
            if args.format == "chat":
                result = client.chat.run(
                    model=args.model or None,
                    user=user,
                    system=system,
                    messages=[{"role": "user", "content": prompt}],
                    include_tool_output_in_reasoning=args.include_tool_output,
                    reasoning_effort=effort,
                )
            elif args.format == "responses":
                result = client.responses.run(
                    model=args.model or None,
                    user=user,
                    instructions=system,
                    input=prompt,
                    include_tool_output_in_reasoning=args.include_tool_output,
                    reasoning_effort=effort,
                )
            else:
                result = client.messages.run(
                    model=args.model or None,
                    user=user,
                    system=system,
                    messages=[{"role": "user", "content": prompt}],
                    include_tool_output_in_reasoning=args.include_tool_output,
                    reasoning_effort=effort,
                )
        except Exception as e:
            print(f"[error] {type(e).__name__}: {e}", file=sys.stderr)
            return 1

    if args.json:
        print(json.dumps(result.to_dict(), ensure_ascii=False, indent=2))
    else:
        print("=== reasoning ===")
        print(result.reasoning)
        print("=== content ===")
        print(result.content)
        print_result_summary(result)

    return 0 if result.ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
