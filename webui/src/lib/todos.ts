/**
 * Session todo list — TUI `active_todos` panel parity for WebUI.
 *
 * The `todowrite` / `todo` tools are stateless: the model either sends a full
 * plan (`{"todos":[…]}`) or an incremental action (`{"action":"add|update",…}`).
 * Current list is folded over the transcript the same way as
 * `atomcode_capabilities::tools::todo::reduce_todos`.
 */

export type TodoStatus = 'pending' | 'in_progress' | 'completed';

export interface TodoItem {
  content: string;
  status: TodoStatus;
}

export function isTodoTool(name: string): boolean {
  return name === 'todowrite' || name === 'todo';
}

function parseStatus(s: string): TodoStatus | null {
  if (s === 'pending' || s === 'in_progress' || s === 'completed') return s;
  return null;
}

/** Full-list plan shape. Returns null when args are not a valid plan. */
export function parseTodoPlan(args: string): TodoItem[] | null {
  let value: unknown;
  try {
    value = JSON.parse(args);
  } catch {
    return null;
  }
  if (!value || typeof value !== 'object') return null;
  let todos = (value as { todos?: unknown }).todos;
  // Tolerate a single-layer stringified array (same as Rust parse_todos).
  if (typeof todos === 'string') {
    try {
      const decoded = JSON.parse(todos);
      if (Array.isArray(decoded)) todos = decoded;
    } catch {
      return null;
    }
  }
  if (!Array.isArray(todos)) return null;
  const out: TodoItem[] = [];
  let inProgress = 0;
  for (const raw of todos) {
    if (!raw || typeof raw !== 'object') return null;
    const content = typeof (raw as { content?: unknown }).content === 'string'
      ? (raw as { content: string }).content.trim()
      : '';
    const status = typeof (raw as { status?: unknown }).status === 'string'
      ? parseStatus((raw as { status: string }).status)
      : null;
    if (!content || !status) return null;
    if (status === 'in_progress') inProgress += 1;
    out.push({ content, status });
  }
  if (inProgress > 1) return null;
  return out;
}

/** Apply one incremental action; returns a new array (or same ref if no-op). */
export function applyTodoAction(list: TodoItem[], args: string): TodoItem[] {
  let v: Record<string, unknown>;
  try {
    const parsed = JSON.parse(args);
    if (!parsed || typeof parsed !== 'object') return list;
    v = parsed as Record<string, unknown>;
  } catch {
    return list;
  }
  const action = typeof v.action === 'string' ? v.action : null;
  if (action === 'add') {
    const content = typeof v.content === 'string' ? v.content.trim() : '';
    if (!content) return list;
    return [...list, { content, status: 'pending' as const }];
  }
  if (action === 'update') {
    const id = typeof v.id === 'number' ? v.id : Number(v.id);
    const status = typeof v.status === 'string' ? parseStatus(v.status) : null;
    if (!Number.isFinite(id) || id < 1 || id > list.length || !status) return list;
    const next = list.map((item) => ({ ...item }));
    if (status === 'in_progress') {
      for (const item of next) {
        if (item.status === 'in_progress') item.status = 'pending';
      }
    }
    next[id - 1] = { ...next[id - 1]!, status };
    return next;
  }
  return list;
}

/**
 * Fold ordered todo-affecting tool calls into the current list.
 * Last full plan is the baseline; later action calls patch it.
 */
export function reduceTodosFromCalls(
  calls: Iterable<{ name: string; args: string }>,
): TodoItem[] {
  const filtered = Array.from(calls).filter((c) => isTodoTool(c.name));
  let baselineIdx = -1;
  for (let i = filtered.length - 1; i >= 0; i--) {
    if (parseTodoPlan(filtered[i]!.args)) {
      baselineIdx = i;
      break;
    }
  }
  let list: TodoItem[] = [];
  let start = 0;
  if (baselineIdx >= 0) {
    list = parseTodoPlan(filtered[baselineIdx]!.args) ?? [];
    start = baselineIdx + 1;
  }
  for (let i = start; i < filtered.length; i++) {
    list = applyTodoAction(list, filtered[i]!.args);
  }
  return list;
}

/** Apply one live tool call onto the current panel list. */
export function foldTodoToolCall(
  current: TodoItem[] | null,
  name: string,
  args: string,
): TodoItem[] | null {
  if (!isTodoTool(name)) return current;
  const plan = parseTodoPlan(args);
  if (plan) return plan.length > 0 ? plan : null;
  if (!current || current.length === 0) {
    // Incremental before any plan — ignore (same as reduce_todos).
    return current;
  }
  const next = applyTodoAction(current, args);
  return next.length > 0 ? next : null;
}

export function todoCounts(items: TodoItem[]): {
  completed: number;
  inProgress: number;
  total: number;
} {
  let completed = 0;
  let inProgress = 0;
  for (const t of items) {
    if (t.status === 'completed') completed += 1;
    else if (t.status === 'in_progress') inProgress += 1;
  }
  return { completed, inProgress, total: items.length };
}

/**
 * Attach folded todos to the last assistant message that owns todowrite calls.
 * Used when converting session history so completed turns keep a frozen list
 * under the reply (not a sticky panel).
 */
export function attachTodosToAssistantParts(
  parts: Array<{ kind: string; tool?: { name: string; args: string }; items?: TodoItem[] }>,
  items: TodoItem[],
): void {
  if (items.length === 0) return;
  // Strip any previous frozen list then append.
  for (let i = parts.length - 1; i >= 0; i--) {
    if (parts[i]!.kind === 'todo_list') parts.splice(i, 1);
  }
  parts.push({ kind: 'todo_list', items: items.map((i) => ({ ...i })) });
}
