// Tool-row model + the dedup primitive shared by the chat view.
//
// Tool calls render as rows under an assistant message. The bug this guards
// against: a tool_start event for one logical call (same `id`/call_id) can reach
// the handler more than once — e.g. a leaked /live broadcast subscription
// re-delivers the whole turn — and naive appending then shows the SAME call as
// several identical rows. Dedup is by `id` (the kernel call_id), so genuinely
// distinct calls (different ids, even same name+args across goal rounds) stay
// separate, while a replayed event is coalesced. Mirrors the tuix
// coalesce-by-call_id fix (commit 80d6540f).

import type { SubtaskItem } from './subtasks';
import type { TodoItem } from './todos';

export interface ToolRow {
  id: string;
  name: string;
  args: string;
  status: 'pending' | 'done' | 'error' | 'incomplete' | 'waiting_approval';
  duration_ms?: number;
  output?: string;
  /** Ephemeral latest activity for a long-running tool. Replaced in place, never persisted. */
  progress?: string;
  /**
   * Parallel sub-agent rows for the `task` tool fan-out. Seeded from args and
   * updated by progress events so explore#1 / explore#3 show side-by-side
   * (TUI subtask panel parity) instead of alternating on one progress line.
   */
  subtasks?: SubtaskItem[];
}

/** One ordered conversation segment: a run of assistant text, one tool call, or a
 *  non-fatal advisory notice (e.g. "conversation compacted"). Arrival order is
 *  preserved so the text→tool→notice interleaving matches the TUI. */
export type MsgPart =
  | { kind: 'text'; text: string }
  /** Model reasoning / thinking stream — rendered as a collapsible block. */
  | { kind: 'reasoning'; text: string }
  | { kind: 'tool'; tool: ToolRow }
  | { kind: 'notice'; text: string }
  | { kind: 'rate_limited'; text: string }
  /**
   * Frozen session todo list attached to the previous assistant reply when
   * the user starts the next turn. Live updates use the sticky panel instead.
   */
  | { kind: 'todo_list'; items: TodoItem[] };

/** Append or replace a trailing `todo_list` part on an assistant message. */
export function withTrailingTodoList(parts: MsgPart[], items: TodoItem[]): MsgPart[] {
  const without = parts.filter((p) => p.kind !== 'todo_list');
  if (items.length === 0) return without;
  return [...without, { kind: 'todo_list', items: items.map((i) => ({ ...i })) }];
}

/**
 * Append `tool` as a new tool part, OR — when a tool part with the SAME `tool.id`
 * already exists — update that part in place (merging incoming fields over the
 * existing ones). Idempotent: replaying the same `tool_start` never adds a second
 * row. Pure; returns a new array (no mutation) for React state updates.
 */
export function upsertToolPart(parts: MsgPart[], tool: ToolRow): MsgPart[] {
  const idx = parts.findIndex((p) => p.kind === 'tool' && p.tool.id === tool.id);
  if (idx < 0) return [...parts, { kind: 'tool', tool }];
  const existing = (parts[idx] as { kind: 'tool'; tool: ToolRow }).tool;
  const next = parts.slice();
  next[idx] = { kind: 'tool', tool: { ...existing, ...tool } };
  return next;
}

export function updateToolProgress(
  parts: MsgPart[],
  id: string,
  progress: string,
  /** Optional full tool patch (e.g. updated subtasks list). */
  patch?: Partial<ToolRow>,
): MsgPart[] {
  return parts.map((part) =>
    part.kind === 'tool' && part.tool.id === id
      ? {
          kind: 'tool' as const,
          tool: { ...part.tool, progress, ...patch },
        }
      : part,
  );
}

/** Append a reasoning delta to the last reasoning part, or start a new one. */
export function appendReasoningPart(parts: MsgPart[], delta: string): MsgPart[] {
  if (!delta) return parts;
  const tail = parts[parts.length - 1];
  if (tail && tail.kind === 'reasoning') {
    const next = parts.slice();
    next[next.length - 1] = { kind: 'reasoning', text: tail.text + delta };
    return next;
  }
  return [...parts, { kind: 'reasoning', text: delta }];
}

export function toolResultStatus(success: boolean, output: string): ToolRow['status'] {
  if (success) return 'done';
  return output.startsWith('Code review incomplete') ? 'incomplete' : 'error';
}
