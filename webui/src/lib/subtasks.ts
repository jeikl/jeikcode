/**
 * Parse `task` tool fan-out progress into a parallel subtask panel model.
 *
 * Mirrors the TUI's `subtask_progress_from_args` / `update_subtask_progress`
 * (event_loop/mod.rs): the `task` tool emits marker-prefixed progress lines
 * like `explore#1 · reading files · tokens=384`. Webui previously overwrote a
 * single progress string so explore#1 / explore#3 alternated; here we keep
 * one row per subtask and update them independently — same parallel view.
 */

/** Same marker the task tool prefixes onto live progress (U+001E). */
export const SUBAGENT_ACTIVITY_MARKER = '\u{1e}';

export type SubtaskStatus = 'pending' | 'running' | 'completed' | 'failed';

export interface SubtaskItem {
  label: string;
  description: string;
  model: string;
  activity: string;
  outputTokens: number;
  status: SubtaskStatus;
}

/** Seed the panel from `task` tool arguments (`{ tasks: [...] }`). */
export function subtasksFromTaskArgs(argsJson: string): SubtaskItem[] | null {
  let value: unknown;
  try {
    value = JSON.parse(argsJson);
  } catch {
    return null;
  }
  if (!value || typeof value !== 'object') return null;
  const tasks = (value as { tasks?: unknown }).tasks;
  if (!Array.isArray(tasks) || tasks.length === 0) return null;
  return tasks.map((task, index) => {
    const t = task && typeof task === 'object' ? (task as Record<string, unknown>) : {};
    const kind =
      typeof t.subagent_type === 'string' && t.subagent_type
        ? t.subagent_type
        : 'explore';
    const description =
      typeof t.description === 'string' ? t.description : '';
    return {
      label: `${kind}#${index + 1}`,
      description,
      model: '',
      activity: '',
      outputTokens: 0,
      status: 'pending' as const,
    };
  });
}

/**
 * Fold one progress chunk into the subtask list. Returns a new array when
 * something changed, otherwise the same reference.
 */
export function applySubtaskProgress(
  items: SubtaskItem[],
  rawChunk: string,
): SubtaskItem[] {
  const chunk = rawChunk.startsWith(SUBAGENT_ACTIVITY_MARKER)
    ? rawChunk.slice(SUBAGENT_ACTIVITY_MARKER.length)
    : rawChunk;
  if (!chunk.trim()) return items;

  // TUI splits on " · " (U+00B7 middle dot with spaces).
  const parts = chunk.split(' \u{b7} ');
  const first = parts[0] ?? '';

  let label = '';
  let model: string | undefined;
  let activity: string | undefined;
  let status: SubtaskStatus = 'running';

  if (first === '\u{25cb} queued') {
    label = parts[1] ?? '';
    model = parts[2];
    activity = 'waiting';
    status = 'pending';
  } else if (first.startsWith('\u{21bb} ')) {
    label = first.slice('\u{21bb} '.length);
    model = parts[1];
    activity = 'running';
    status = 'running';
  } else if (first === '\u{2713} done') {
    label = parts[1] ?? '';
    model = parts[2];
    activity = 'completed';
    status = 'completed';
  } else if (first.startsWith('\u{2717} failed')) {
    label = parts[1] ?? '';
    model = parts[2];
    activity = first;
    status = 'failed';
  } else if (/^[a-z]+#\d+$/i.test(first)) {
    // Mid-flight activity: `explore#1 · reading files · tokens=384`
    label = first;
    activity = parts[1];
    status = 'running';
  } else {
    // Unstructured progress — leave list unchanged; caller may still show it.
    return items;
  }

  if (!label) return items;
  const idx = items.findIndex((item) => item.label === label);
  if (idx < 0) return items;

  const prev = items[idx]!;
  if (prev.status === 'completed' || prev.status === 'failed') {
    // Terminal states are sticky (TUI idempotency).
    return items;
  }

  let outputTokens = prev.outputTokens;
  for (const part of parts) {
    if (part.startsWith('tokens=')) {
      const n = Number(part.slice('tokens='.length));
      if (Number.isFinite(n)) outputTokens = Math.max(outputTokens, n);
    }
  }

  const next: SubtaskItem = {
    ...prev,
    status,
    activity: activity && activity.length ? activity : prev.activity,
    model: model && model.length ? model : prev.model,
    outputTokens,
  };

  if (
    next.status === prev.status &&
    next.activity === prev.activity &&
    next.model === prev.model &&
    next.outputTokens === prev.outputTokens
  ) {
    return items;
  }

  const copy = items.slice();
  copy[idx] = next;
  return copy;
}

export function subtaskCounts(items: SubtaskItem[]): {
  completed: number;
  running: number;
  pending: number;
  failed: number;
  total: number;
} {
  let completed = 0;
  let running = 0;
  let pending = 0;
  let failed = 0;
  for (const item of items) {
    if (item.status === 'completed') completed++;
    else if (item.status === 'running') running++;
    else if (item.status === 'failed') failed++;
    else pending++;
  }
  return { completed, running, pending, failed, total: items.length };
}

/**
 * After reload, `task` tool output is often persisted as XML-ish blocks:
 *   `<task id="worker#1" model="auto" state="completed">…</task>`
 * Fold those into the parallel subtask panel so history matches the live view
 * (instead of dumping raw JSON args + XML output).
 */
export function applySubtaskResultsFromOutput(
  items: SubtaskItem[],
  output: string,
): SubtaskItem[] {
  if (!items.length || !output) return items;

  // Prefer opening tags with attributes; also accept self-closing / bare.
  const tagRe = /<task\b([^>]*)>/gi;
  let copy = items;
  let changed = false;
  let match: RegExpExecArray | null;
  while ((match = tagRe.exec(output)) !== null) {
    const attrs = match[1] ?? '';
    const id = /(?:^|\s)id="([^"]+)"/i.exec(attrs)?.[1]
      ?? /(?:^|\s)id='([^']+)'/i.exec(attrs)?.[1];
    if (!id) continue;
    const stateRaw = (
      /(?:^|\s)state="([^"]+)"/i.exec(attrs)?.[1]
      ?? /(?:^|\s)state='([^']+)'/i.exec(attrs)?.[1]
      ?? 'completed'
    ).toLowerCase();
    const model =
      /(?:^|\s)model="([^"]+)"/i.exec(attrs)?.[1]
      ?? /(?:^|\s)model='([^']+)'/i.exec(attrs)?.[1]
      ?? '';

    const status: SubtaskStatus =
      stateRaw === 'failed' || stateRaw === 'error'
        ? 'failed'
        : stateRaw === 'running' || stateRaw === 'in_progress'
          ? 'running'
          : stateRaw === 'pending' || stateRaw === 'queued'
            ? 'pending'
            : 'completed';

    let idx = copy.findIndex((item) => item.label === id);
    if (idx < 0) {
      // Args seed as explore#1 / worker#1; output id may match either form.
      idx = copy.findIndex(
        (item) =>
          item.label.endsWith(`#${id}`) ||
          id.endsWith(item.label) ||
          item.label.replace(/^.*#/, '') === id.replace(/^.*#/, ''),
      );
    }
    if (idx < 0) continue;

    const prev = copy[idx]!;
    const activity =
      status === 'completed'
        ? 'completed'
        : status === 'failed'
          ? 'failed'
          : status === 'running'
            ? 'running'
            : prev.activity;
    if (
      prev.status === status &&
      (model ? prev.model === model : true) &&
      prev.activity === activity
    ) {
      continue;
    }
    if (copy === items) copy = items.slice();
    copy[idx] = {
      ...prev,
      status,
      model: model || prev.model,
      activity,
    };
    changed = true;
  }

  // If output exists but no tags matched, and every row is still pending,
  // mark all completed (legacy summaries without structured tags).
  if (!changed && output.trim() && items.every((i) => i.status === 'pending')) {
    return items.map((i) => ({
      ...i,
      status: 'completed' as const,
      activity: 'completed',
    }));
  }

  return changed ? copy : items;
}

/** Compact header detail for `task` (avoid dumping full JSON in the tool row). */
export function taskArgsSummary(argsJson: string): string {
  const items = subtasksFromTaskArgs(argsJson);
  if (!items || items.length === 0) return '';
  return `${items.length} subagents`;
}
