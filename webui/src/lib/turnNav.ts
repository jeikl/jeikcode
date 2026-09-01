import {
  isInternalHistoryUserMessage,
  stripInjectedRemindersForDisplay,
} from './historyMessages.ts';

/** One user question in the session outline (DeepSeek-style right rail). */
export interface TurnNavItem {
  /** Stable DOM id on the user message wrapper. */
  id: string;
  /** Stable ordinal among real user questions. */
  ordinal: number;
  /** Absolute index in the raw persisted transcript, used for history loading. */
  index: number;
  /** Truncated label shown in the outline. */
  label: string;
  /** Full compacted question text for search. */
  text: string;
}

const DEFAULT_MAX_LABEL = 28;

export function turnNavId(ordinal: number): string {
  return `turn-nav-${ordinal}`;
}

export function compactTurnNavText(text: string): string {
  return text.replace(/\s+/g, ' ').trim();
}

/** Collapse whitespace and truncate for the outline label. */
export function truncateTurnNavLabel(text: string, max = DEFAULT_MAX_LABEL): string {
  const compact = compactTurnNavText(text);
  if (!compact) return '';
  if (compact.length <= max) return compact;
  return compact.slice(0, Math.max(1, max - 1)) + '…';
}

export function buildTurnNavItemsFromOutline(
  turns: { ordinal?: number; index: number; text: string }[],
): TurnNavItem[] {
  const items: TurnNavItem[] = [];
  for (const turn of turns) {
    // Older sessions may not mark injected user rows as synthetic. Keep the
    // server outline aligned with the same rows history rendering suppresses.
    if (isInternalHistoryUserMessage(turn.text)) continue;
    const text = compactTurnNavText(stripInjectedRemindersForDisplay(turn.text));
    const label = truncateTurnNavLabel(text);
    if (!label) continue;
    // New daemons provide an ordinal that stays stable when failed turns add
    // diagnostic rows to the raw transcript. Older daemons fall back to the
    // visible outline order, which has the same semantics.
    const ordinal = turn.ordinal ?? items.length;
    items.push({ id: turnNavId(ordinal), ordinal, index: turn.index, label, text });
  }
  return items;
}

export function buildTurnNavItems(
  messages: { role: string; text: string; sourceIndex?: number }[],
  windowOffset = 0,
): TurnNavItem[] {
  const items: TurnNavItem[] = [];
  for (let i = 0; i < messages.length; i++) {
    const msg = messages[i];
    if (msg.role !== 'user') continue;
    const text = compactTurnNavText(stripInjectedRemindersForDisplay(msg.text));
    const label = truncateTurnNavLabel(text);
    if (!label) continue;
    // History rendering filters synthetic messages and folds tool-result rows
    // into their assistant. Prefer the raw transcript index carried through
    // conversion; the visible index is only a fallback for live/legacy data.
    const index = msg.sourceIndex ?? windowOffset + i;
    const ordinal = items.length;
    items.push({ id: turnNavId(ordinal), ordinal, index, label, text });
  }
  return items;
}

/** Convert a target's viewport position into a scrollTop inside its chat root. */
export function turnNavScrollTop(
  rootScrollTop: number,
  rootViewportTop: number,
  targetViewportTop: number,
): number {
  return Math.max(0, rootScrollTop + targetViewportTop - rootViewportTop);
}

/** Case-insensitive substring filter over the full question text. */
export function filterTurnNavItems(items: TurnNavItem[], query: string): TurnNavItem[] {
  const q = query.trim().toLowerCase();
  if (!q) return items;
  return items.filter((it) => it.text.toLowerCase().includes(q));
}

export interface TurnNavScrollMetrics {
  scrollTop: number;
  clientHeight: number;
  scrollHeight: number;
}

/**
 * Stable outline spy: last question whose content top is still above a
 * near-top marker. Avoids IntersectionObserver flicker between neighbours.
 * At the bottom of the thread, always the last question (its answer fills the view).
 */
export function resolveActiveTurnId(
  items: { id: string }[],
  contentTopById: (id: string) => number | undefined,
  metrics: TurnNavScrollMetrics,
  opts?: { markerOffsetPx?: number; bottomSlack?: number },
): string | null {
  if (items.length === 0) return null;
  const markerOffsetPx = opts?.markerOffsetPx ?? 24;
  const bottomSlack = opts?.bottomSlack ?? 32;
  const { scrollTop, clientHeight, scrollHeight } = metrics;
  if (scrollTop + clientHeight >= scrollHeight - bottomSlack) {
    return items[items.length - 1].id;
  }
  const marker = scrollTop + markerOffsetPx;
  let active = items[0].id;
  for (const item of items) {
    const top = contentTopById(item.id);
    if (top == null || !Number.isFinite(top)) continue;
    if (top <= marker) active = item.id;
    else break;
  }
  return active;
}
