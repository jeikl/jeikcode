/** Live turn elapsed-time helpers for the WebUI chat timeline. */

function pad2(n: number): string {
  return n < 10 ? '0' + n : '' + n;
}

/**
 * Format a duration for the in-turn stopwatch.
 * Seconds stay in seconds until 60, then the unit rolls to m:ss (and h:mm:ss).
 */
export function formatTurnElapsed(ms: number): string {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = totalSec % 60;
  if (h > 0) return `${h}:${pad2(m)}:${pad2(s)}`;
  if (m > 0) return `${m}:${pad2(s)}`;
  return `${s}s`;
}

/** Stamp `elapsedMs` onto the latest assistant message that does not already have one. */
export function stampLastAssistantElapsed<T extends { role: string; elapsedMs?: number }>(
  msgs: T[],
  elapsedMs: number,
): T[] {
  for (let i = msgs.length - 1; i >= 0; i--) {
    if (msgs[i]!.role !== 'assistant') continue;
    if (msgs[i]!.elapsedMs != null) return msgs;
    const next = msgs.slice();
    next[i] = { ...msgs[i]!, elapsedMs };
    return next;
  }
  return msgs;
}
