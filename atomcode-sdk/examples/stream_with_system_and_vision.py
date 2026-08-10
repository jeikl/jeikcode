#!/usr/bin/env python3
"""Chat Completions with system prompt + optional image (data URL or path).

  # text only + system file
  python examples/stream_with_system_and_vision.py \\
    --system-file ./sys.txt -p "按系统提示回答"

  # image path (png/jpg) → data URL
  python examples/stream_with_system_and_vision.py \\
    -s "描述图片" --image ./shot.png -p "图里有什么？"

  # already a data: or https URL
  python examples/stream_with_system_and_vision.py \\
    --image-url "https://example.com/a.png" -p "描述"
"""

from __future__ import annotations

import argparse
import base64
import mimetypes
import sys
from pathlib import Path

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


def file_to_data_url(path: Path) -> str:
    if not path.is_file():
        raise SystemExit(f"image not found: {path}")
    mime, _ = mimetypes.guess_type(str(path))
    if not mime or not mime.startswith("image/"):
        mime = "image/png"
    raw = path.read_bytes()
    b64 = base64.standard_b64encode(raw).decode("ascii")
    return f"data:{mime};base64,{b64}"


def build_user_content(prompt: str, image_url: str | None) -> str | list:
    if not image_url:
        return prompt
    return [
        {"type": "text", "text": prompt},
        {"type": "image_url", "image_url": {"url": image_url}},
    ]


def main() -> int:
    p = argparse.ArgumentParser(description="Stream chat with system + optional image")
    add_common_args(p)
    p.add_argument("--image", type=Path, default=None, help="Local image file → data URL")
    p.add_argument("--image-url", default=None, help="data: or https image URL")
    p.add_argument("--no-user", action="store_true")
    args = p.parse_args()

    system = resolve_system(args)
    prompt = resolve_prompt(args)
    user = None if args.no_user else args.user

    image_url = args.image_url
    if args.image is not None:
        image_url = file_to_data_url(args.image)

    content = build_user_content(prompt, image_url)
    messages = [{"role": "user", "content": content}]

    with AtomCodeClient(args.base, token=args.token) as client:
        printer = DualPanePrinter(quiet_meta=args.quiet_meta, base=client.base_url, token=client.token)
        printer.begin("Chat Completions + system + vision")
        return printer.consume(
            client.chat.stream(
                model=args.model or None,
                user=user,
                system=system,
                messages=messages,
                include_tool_output_in_reasoning=args.include_tool_output,
                reasoning_effort=resolve_effort(args),
            )
        )


if __name__ == "__main__":
    raise SystemExit(main())
