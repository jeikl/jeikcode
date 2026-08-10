#!/usr/bin/env python3
"""Backward-compatible entry: Chat Completions streaming dual-pane.

Prefer ``stream_all_sync.py`` / ``stream_all_async.py`` for all three formats.

  python examples/stream_chat.py -s "用中文" "介绍项目"
  python examples/stream_chat.py --system-file sys.txt -p "hi"
"""

from __future__ import annotations

import sys
from pathlib import Path

_EXAMPLES = Path(__file__).resolve().parent
_ROOT = _EXAMPLES.parent
for _p in (_ROOT, _EXAMPLES):
    if str(_p) not in sys.path:
        sys.path.insert(0, str(_p))

from stream_all_sync import main as sync_main  # noqa: E402


def main() -> int:
    # Default format=chat when user runs stream_chat.py with old-style args.
    argv = list(sys.argv[1:])
    if not argv or argv[0] not in ("chat", "responses", "messages", "all"):
        argv = ["chat", *argv]
    return sync_main(argv)


if __name__ == "__main__":
    raise SystemExit(main())
