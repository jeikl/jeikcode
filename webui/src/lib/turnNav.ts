/** One user question in the session outline (DeepSeek-style right rail). */
export interface TurnNavItem {
  /** Stable DOM id on the user message wrapper. */
  id: string;
  /** Index into the messages array. */
  index: number;
  label: string;
}

const DEFAULT_MAX_LABEL = 28;

export function turnNavId(index: number): string {
  return `turn-nav-${index}`;
}

/** Collapse whitespace and truncate for the outline label. */
export function truncateTurnNavLabel(text: string, max = DEFAULT_MAX_LABEL): string {
  const compact = text.replace(/\s+/g, ' ').trim();
  if (!compact) return '';
  if (compact.length <= max) return compact;
  return compact.slice(0, Math.max(1, max - 1)) + '…';
}

export function buildTurnNavItems(
  messages: { role: string; text: string }[],
): TurnNavItem[] {
  const items: TurnNavItem[] = [];
  for (let i = 0; i < messages.length; i++) {
    const msg = messages[i];
    if (msg.role !== 'user') continue;
    const label = truncateTurnNavLabel(msg.text);
    if (!label) continue;
    items.push({ id: turnNavId(i), index: i, label });
  }
  return items;
}
