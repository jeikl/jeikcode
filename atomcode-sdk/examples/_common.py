"""Shared helpers for AtomCode SDK examples (not part of the installable package)."""

from __future__ import annotations

import argparse
import os
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable, Optional

from atomcode_sdk import ReasoningEffort, StreamEvent, StreamEventType, TurnResult, parse_reasoning_effort


def env(name: str, default: Optional[str] = None) -> Optional[str]:
    v = os.environ.get(name)
    if v is None or v.strip() == "":
        return default
    return v.strip()


def add_common_args(p: argparse.ArgumentParser) -> None:
    p.add_argument(
        "--base",
        default=env("ATOMCODE_BASE", "http://127.0.0.1:4096"),
        help="AtomCode serve base URL (env ATOMCODE_BASE)",
    )
    p.add_argument(
        "--token",
        default=env("ATOMCODE_TOKEN"),
        help="Bearer token (env ATOMCODE_TOKEN); omit for --no-token servers",
    )
    p.add_argument(
        "--user",
        default=env("ATOMCODE_USER"),
        help="Session key / OpenAI user field (env ATOMCODE_USER)",
    )
    p.add_argument(
        "--model",
        default=env("ATOMCODE_MODEL"),
        help="Model selection id from GET /v1/models (env ATOMCODE_MODEL)",
    )
    p.add_argument(
        "--system",
        "-s",
        default=env("ATOMCODE_SYSTEM"),
        help="System / instructions text (env ATOMCODE_SYSTEM)",
    )
    p.add_argument(
        "--system-file",
        type=Path,
        default=None,
        help="Read system text from a file",
    )
    p.add_argument(
        "--prompt",
        "-p",
        default=None,
        help="User prompt. If omitted, use positional PROMPT or stdin",
    )
    p.add_argument(
        "--prompt-file",
        type=Path,
        default=None,
        help="Read user prompt from a file",
    )
    p.add_argument(
        "positional_prompt",
        nargs="?",
        default=None,
        help="User prompt (positional)",
    )
    p.add_argument(
        "--include-tool-output",
        action="store_true",
        help="Also fold tool stdout teasers into reasoning (medium/max only)",
    )
    p.add_argument(
        "--reasoning-effort",
        "--effort",
        dest="reasoning_effort",
        default=env("ATOMCODE_REASONING_EFFORT", "medium"),
        choices=["low", "medium", "max", "min", "med", "high", "full", "default"],
        help=(
            "Display intensity for the reasoning pane (env ATOMCODE_REASONING_EFFORT). "
            "low=content only; medium=tools+subagents no thinking detail (default); "
            "max=full thinking+tools+subagents"
        ),
    )
    p.add_argument(
        "--quiet-meta",
        action="store_true",
        help="Do not print [done]/session headers",
    )


def resolve_system(args: argparse.Namespace) -> Optional[str]:
    if args.system_file:
        return Path(args.system_file).read_text(encoding="utf-8")
    return args.system


def resolve_prompt(args: argparse.Namespace) -> str:
    if args.prompt_file:
        return Path(args.prompt_file).read_text(encoding="utf-8")
    if args.prompt:
        return args.prompt
    if args.positional_prompt:
        return args.positional_prompt
    if not sys.stdin.isatty():
        return sys.stdin.read()
    return "用一句话介绍当前工作目录对应的项目"


def resolve_effort(args: argparse.Namespace) -> ReasoningEffort:
    return parse_reasoning_effort(getattr(args, "reasoning_effort", None) or "medium")


# CSI sequences used by the server subagent panel redraw.
_CSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")


def _vt_supported() -> bool:
    """Best-effort: does this stdout support ANSI cursor control?

    Windows Terminal / modern conhost (with VT enabled) / POSIX TTYs → True.
    Old cmd.exe hosts or redirected pipes → False, so subagent panel redraw
    degrades to append-one-line-per-change instead of cursor-up in-place
    (which would otherwise stack rows on top of each other).
    """
    if not (hasattr(sys.stdout, "isatty") and sys.stdout.isatty()):
        return False
    if sys.platform.startswith("win"):
        # On Windows 10+, cmd & PowerShell default to VT-capable conhosts.
        # We try to ENABLE_VIRTUAL_TERMINAL_PROCESSING explicitly (idempotent),
        # but fix the ctypes signatures for 64-bit Python (HANDLE is a pointer,
        # not an int — the default truncation broke GetConsoleMode before).
        try:
            import ctypes
            from ctypes import wintypes

            kernel32 = ctypes.windll.kernel32
            kernel32.GetStdHandle.restype = wintypes.HANDLE
            kernel32.GetStdHandle.argtypes = [wintypes.DWORD]
            kernel32.GetConsoleMode.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
            kernel32.SetConsoleMode.argtypes = [wintypes.HANDLE, wintypes.DWORD]
            kernel32.GetConsoleMode.restype = wintypes.BOOL
            kernel32.SetConsoleMode.restype = wintypes.BOOL

            STD_OUTPUT_HANDLE = 0xFFFFFFF5  # (DWORD)-11
            h = kernel32.GetStdHandle(STD_OUTPUT_HANDLE)
            mode = wintypes.DWORD(0)
            if kernel32.GetConsoleMode(h, ctypes.byref(mode)) == 0:
                # GetConsoleMode failed (redirected / no real console).
                return False
            new_mode = mode.value | 0x4  # ENABLE_VIRTUAL_TERMINAL_PROCESSING
            kernel32.SetConsoleMode(h, new_mode)
            # Re-read to confirm the bit actually stuck (old hosts swallow the call).
            mode = wintypes.DWORD(0)
            kernel32.GetConsoleMode(h, ctypes.byref(mode))
            return bool(mode.value & 0x4)
        except Exception:
            return False
    return True


def _is_noisy_status(status: str) -> bool:
    t = (status or "").strip()
    if not t:
        return True
    if "\n" in t or len(t) > 72:
        return True
    if t[:1] in "#├│└`*-—":
        return True
    if t.startswith("##") or "目录树" in t or t.startswith("```"):
        return True
    return False


@dataclass
class DualPanePrinter:
    """Print reasoning + content as two panes; accumulate full text.

    Subagent progress is a **fixed multi-line panel** (one row per subagent).
    Each status change rewrites the panel in place (ANSI cursor-up), never
    stacks history like a log.
    """

    quiet_meta: bool = False
    show_tool_cards: bool = False
    reasoning: str = ""
    content: str = ""
    session_id: Optional[str] = None
    user: Optional[str] = None
    model: Optional[str] = None
    error: Optional[str] = None
    # daemon base + token,用于 KeyboardInterrupt 时给 daemon 发 /chat/stop 清理
    # 残 turn,避免 Ctrl+C 后 session 仍 busy → 下次 409。
    base: Optional[str] = None
    token: Optional[str] = None
    _content_started: bool = False
    _reasoning_header: bool = False
    _thinking_closed: bool = False
    _process_after_answer: bool = False
    _content_resumed_after_process: bool = False
    _just_wrote_content: bool = False
    tools: dict = field(default_factory=dict)
    _subagent_status: dict = field(default_factory=dict)
    _subagent_order: list = field(default_factory=list)
    _subagent_block_lines: int = 0
    _tty: bool = field(default_factory=_vt_supported)

    # Section banners (easy to spot in terminal logs)
    MARK_THINK_START = "----思考内容开始----"
    MARK_THINK_END = "----思考内容结束----"
    MARK_ANSWER = "----正式回答----"

    def begin(self, title: str = "") -> None:
        if not self.quiet_meta and title:
            print(f"\n######## {title} ########\n", flush=True)
        if not self.quiet_meta:
            print(self.MARK_THINK_START, flush=True)
            print(flush=True)
        self._reasoning_header = True
        self._thinking_closed = False
        self._content_started = False

    def _close_thinking_open_answer(self) -> None:
        """Print end-of-thinking + answer banners once, before first content."""
        if self._thinking_closed:
            return
        self._thinking_closed = True
        # Leave subagent panel as-is; stop cursor-up tracking.
        if self._tty and self._subagent_block_lines > 0:
            self._subagent_block_lines = 0
        elif not self._tty and self._subagent_order:
            sys.stdout.write("\n")
        if self.reasoning and not self.reasoning.endswith("\n"):
            sys.stdout.write("\n")
            self.reasoning += "\n"
        if not self.quiet_meta:
            print(flush=True)
            print(self.MARK_THINK_END, flush=True)
            print(flush=True)
            print(self.MARK_ANSWER, flush=True)
            print(flush=True)

    def _parse_subagent_line(self, text: str) -> Optional[tuple[str, str]]:
        t = text.strip().lstrip("\r")
        if not t.startswith("子代理 "):
            return None
        rest = t[len("子代理 ") :]
        for sep in (":", "："):
            if sep in rest:
                label, status = rest.split(sep, 1)
                return label.strip(), status.strip()
        return None

    def _set_subagent(self, label: str, status: str) -> bool:
        """Update status map. Return True if display changed."""
        if _is_noisy_status(status):
            # Keep previous short status; ignore tree/answer dumps.
            if label not in self._subagent_status:
                status = "运行中"
            else:
                return False
        if label not in self._subagent_status:
            self._subagent_order.append(label)
        if self._subagent_status.get(label) == status:
            return False
        self._subagent_status[label] = status
        return True

    def _redraw_subagent_panel(self) -> None:
        if not self._subagent_order:
            return
        # Snapshot into reasoning accumulator (latest only).
        block = "\n".join(
            f"子代理 {k}: {self._subagent_status[k]}" for k in self._subagent_order
        )
        # Replace previous panel snapshot if present.
        marker = "\n【子代理】\n"
        if marker in self.reasoning:
            head, _, _ = self.reasoning.partition(marker)
            self.reasoning = head.rstrip("\n") + marker + block + "\n"
        else:
            if self.reasoning and not self.reasoning.endswith("\n"):
                self.reasoning += "\n"
            self.reasoning += marker + block + "\n"

        if self._tty:
            n = self._subagent_block_lines
            if n > 0:
                # 即便 VT 通了也重置计数 —— 答案开始后不再原地重绘。
                sys.stdout.write(f"\x1b[{n}A")
            for k in self._subagent_order:
                row = f"子代理 {k}: {self._subagent_status[k]}"
                sys.stdout.write(f"\r{row}\x1b[K\n")
            self._subagent_block_lines = len(self._subagent_order)
            sys.stdout.flush()
        else:
            # 无 VT 光标能力 (旧 cmd / pipe / 探测失败)：
            # 不和不支持的 \r 叠加 - 每次面板变更打印一条带时间戳的全量快照,
            # 保持「滚屏日志」而非「向下堆叠面板」(后者把 explore#1 状态重复 N 行)。
            summary = " | ".join(
                f"{k}: {self._subagent_status[k]}" for k in self._subagent_order
            )
            sys.stdout.write(f"[子代理] {summary}\n")
            sys.stdout.flush()

    def _ingest_subagent_delta(self, delta: str) -> bool:
        """Parse server panel / subagent lines; return True if handled."""
        plain = _CSI_RE.sub("", delta)
        changed = False
        for line in plain.replace("\r", "\n").splitlines():
            parsed = self._parse_subagent_line(line)
            if not parsed:
                continue
            label, status = parsed
            if self._set_subagent(label, status):
                changed = True
        if changed:
            self._redraw_subagent_panel()
            return True
        # Pure CSI panel redraw with no parseable change — still "handled".
        if "子代理 " in plain and ("\x1b[" in delta or "\r" in delta):
            return True
        return "子代理 " in plain and not plain.strip()

    def _maybe_open_process_after_answer(self) -> None:
        """Tools/subagents often continue after the model starts the formal reply."""
        if self._content_started and not self._process_after_answer and not self.quiet_meta:
            self._process_after_answer = True
            self._just_wrote_content = False
            print(flush=True)
            print("----过程补充（工具 / 子代理）----", flush=True)
            print(flush=True)

    def _write_reasoning_delta(self, delta: str) -> None:
        if not delta:
            return
        if self._ingest_subagent_delta(delta):
            self._maybe_open_process_after_answer()
            return
        stripped = _CSI_RE.sub("", delta).lstrip("\n\r")
        is_tool = stripped.startswith(("正在调用", "工具 ", "  · "))
        if is_tool:
            self._maybe_open_process_after_answer()
            # Finalize subagent panel (leave last paint; stop tracking for cursor-up).
            if self._tty and self._subagent_block_lines > 0:
                self._subagent_block_lines = 0
            elif not self._tty and self._subagent_order:
                sys.stdout.write("\n")
            # Tool lines must START on a fresh line. `self.reasoning` only tracks
            # prior reasoning (content isn't accumulated here), so track whether
            # CONTENT was the last thing written — otherwise the tool line jams
            # onto the last content chunk (「…验证。正在调用 bash」). Banner and
            # prior reasoning already end with \n, so only content needs the bump.
            if self._just_wrote_content:
                sys.stdout.write("\n")
                self.reasoning += "\n"
                self._just_wrote_content = False
            body = stripped.rstrip("\n\r")
            delta = f"{body}\n"
        elif self._content_started:
            # Model thinking after answer started
            self._maybe_open_process_after_answer()
        self.reasoning += delta
        sys.stdout.write(delta)
        self._just_wrote_content = False
        sys.stdout.flush()

    def on_event(self, ev: StreamEvent) -> None:
        if ev.subagent is not None and ev.subagent.id:
            sid = ev.subagent.id or ev.subagent.label
            msg = (ev.subagent.message or ev.subagent.state or "").strip()
            if " · " in msg or " \u00b7 " in msg:
                sep = " \u00b7 " if " \u00b7 " in msg else " · "
                msg = msg.split(sep)[-1].strip()
            msg = " ".join(msg.split()) or (ev.subagent.state or "运行中")
            if self._set_subagent(sid, msg):
                self._maybe_open_process_after_answer()
                self._redraw_subagent_panel()
            # Still allow non-subagent reasoning / content on same event.
            if ev.reasoning_delta and "子代理 " not in ev.reasoning_delta:
                self._write_reasoning_delta(ev.reasoning_delta)
        elif ev.reasoning_delta:
            self._write_reasoning_delta(ev.reasoning_delta)

        if ev.content_delta:
            if not self._content_started:
                self._content_started = True
                self._close_thinking_open_answer()
            # 工具/子代理行插在正文中间时,「过程补充」横幅已打印,后续正文继续
            # 会粘在横幅下方看起来像跑错栏 —— 补一个分隔标记,明确正文在继续。
            if self._process_after_answer and not self._content_resumed_after_process:
                self._content_resumed_after_process = True
                sys.stdout.write("\n（正文续）\n")
            self.content += ev.content_delta
            sys.stdout.write(ev.content_delta)
            self._just_wrote_content = True
            sys.stdout.flush()
        if ev.session_id:
            self.session_id = ev.session_id
        if ev.user:
            self.user = ev.user
        if ev.model:
            self.model = ev.model
        if ev.tool is not None:
            self.tools[ev.tool.id] = ev.tool
        if ev.type == StreamEventType.ERROR:
            self.error = ev.error or "unknown error"
            print(f"\n[error] {self.error}", file=sys.stderr, flush=True)
        if ev.type == StreamEventType.DONE and not self.quiet_meta:
            # No formal answer at all → still close the thinking section.
            if self._reasoning_header and not self._thinking_closed:
                self._close_thinking_open_answer()
                if not self.content:
                    print("（无正文）", flush=True)
            if self._tty and self._subagent_block_lines > 0:
                self._subagent_block_lines = 0
            elif not self._tty and self._subagent_order:
                sys.stdout.write("\n")
            print(
                f"\n\n[done] session={self.session_id} user={self.user} model={self.model}",
                flush=True,
            )

    def consume(self, events: Iterable[StreamEvent]) -> int:
        """Drain stream; return process exit code (0 ok, 1 error)."""
        try:
            for ev in events:
                self.on_event(ev)
                if ev.type == StreamEventType.ERROR:
                    return 1
        except KeyboardInterrupt:
            print("\n[interrupted]", file=sys.stderr)
            self._stop_daemon_turn()
            return 130
        except Exception as e:
            print(f"\n[http/client error] {type(e).__name__}: {e}", file=sys.stderr)
            return 1
        return 1 if self.error else 0

    def _stop_daemon_turn(self) -> None:
        """Best-effort POST /chat/stop so a Ctrl+C'd turn is cleaned up on the
        daemon side too. Without this the operation stays admitted → next send
        to the same session gets 409 SessionBusy."""
        sid = self.session_id
        if not sid or not self.base:
            return
        try:
            import urllib.request
            import json as _json

            url = f"{self.base.rstrip('/')}/chat/stop"
            data = _json.dumps({"session_id": sid}).encode("utf-8")
            req = urllib.request.Request(
                url,
                data=data,
                headers={
                    "Content-Type": "application/json",
                    **({"Authorization": f"Bearer {self.token}"} if self.token else {}),
                },
                method="POST",
            )
            urllib.request.urlopen(req, timeout=5).close()
            print(f"[stop] requested daemon to stop session {sid}", file=sys.stderr, flush=True)
        except Exception as e:
            # Best-effort; never fail the interrupt path on a cleanup error.
            print(f"[stop] cleanup failed (non-fatal): {e}", file=sys.stderr, flush=True)

    def to_result(self) -> TurnResult:
        return TurnResult(
            reasoning=self.reasoning,
            content=self.content,
            session_id=self.session_id,
            user=self.user,
            model=self.model,
            error=self.error,
            tools=self.tools,
        )


async def aconsume(printer: DualPanePrinter, aiter) -> int:
    """Async version of DualPanePrinter.consume."""
    try:
        async for ev in aiter:
            printer.on_event(ev)
            if ev.type == StreamEventType.ERROR:
                return 1
    except KeyboardInterrupt:
        print("\n[interrupted]", file=sys.stderr)
        printer._stop_daemon_turn()
        return 130
    except Exception as e:
        print(f"\n[http/client error] {type(e).__name__}: {e}", file=sys.stderr)
        return 1
    return 1 if printer.error else 0
