/**
 * Session todo list — TUI `active_todos` panel parity for WebUI.
 *
 * The `todowrite` / `todo` tools are stateless: the model either sends a full
 * plan (`{"todos":[…]}`), an incremental batch (`{"actions":[…]}`), or a single
 * incremental action (`{"action":"add|update|...",…}`).
 * Current list is folded over the transcript the same way as
 * `atomcode_capabilities::tools::todo::reduce_todos`.
 */

export type TodoStatus = 'pending' | 'in_progress' | 'completed';

export interface TodoItem {
  content: string;
  status: TodoStatus;
}

type ActionKind = 'add' | 'insert' | 'update' | 'delete' | 'clear';

export function isTodoTool(name: string): boolean {
  return name === 'todowrite' || name === 'todo';
}

function parseStatus(s: string): TodoStatus | null {
  if (s === 'pending' || s === 'in_progress' || s === 'completed') return s;
  return null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function actionKind(value: Record<string, unknown>): ActionKind | null {
  const action = typeof value.action === 'string' ? value.action : null;
  if (action === 'add' || action === 'insert' || action === 'update' || action === 'clear') {
    return action;
  }
  if (action === 'delete' || action === 'remove') return 'delete';
  return null;
}

function parseNonNegInt(raw: unknown): number | null {
  if (typeof raw === 'number' && Number.isSafeInteger(raw) && raw >= 0) return raw;
  if (typeof raw === 'string') {
    const parsed = Number.parseInt(raw.trim(), 10);
    if (Number.isSafeInteger(parsed) && parsed >= 0) return parsed;
  }
  return null;
}

function jsonId(value: Record<string, unknown>): number | null {
  const id = parseNonNegInt(value.id);
  return id !== null && id >= 1 ? id : null;
}

function insertPosition(value: Record<string, unknown>): number | null {
  const direct = parseNonNegInt(value.position !== undefined ? value.position : value.id);
  if (direct !== null) return direct;
  const after = parseNonNegInt(value.after !== undefined ? value.after : value.after_id);
  return after === null ? null : after + 1;
}

function validateActionsMix(actions: Record<string, unknown>[]): boolean {
  const kinds = new Set<ActionKind>();
  for (const item of actions) {
    const kind = actionKind(item);
    if (!kind) return false;
    kinds.add(kind);
  }
  if (kinds.has('delete') && [...kinds].some((kind) => kind !== 'delete')) return false;
  if (kinds.has('insert') && [...kinds].some((kind) => kind !== 'insert' && kind !== 'update')) {
    return false;
  }
  if (kinds.has('clear') && [...kinds].some((kind) => kind !== 'clear' && kind !== 'add' && kind !== 'update')) {
    return false;
  }
  return true;
}

function maybeAutoClearFinished(list: TodoItem[]): TodoItem[] {
  if (list.length > 0 && list.every((item) => item.status === 'completed')) return [];
  return list;
}

function normalizeTodoContent(content: string): string {
  return content.split(/\s+/).filter(Boolean).join(' ');
}

function findTodoIndex(list: TodoItem[], content: string): number {
  return list.findIndex((item) => item.content === content);
}

function destinationIndex(position: number | null, len: number): number {
  if (position === null) return len;
  if (position <= 1) return 0;
  if (position - 1 <= len) return position - 1;
  return len;
}

function ensureSingleInProgress(list: TodoItem[], idx: number): TodoItem[] {
  return list.map((item, i) => {
    if (i === idx) return item;
    if (item.status === 'in_progress') return { ...item, status: 'pending' as const };
    return item;
  });
}

/** Same title never appears twice. `position` is 1-based rank; omit to keep/append. */
function upsertTodo(
  list: TodoItem[],
  content: string,
  status: TodoStatus | null,
  position: number | null,
): { list: TodoItem[]; landing: number } {
  const existing = findTodoIndex(list, content);
  if (existing >= 0 && position === null) {
    const next = list.map((item, i) => (
      i === existing && status ? { ...item, status } : item
    ));
    const ranked = status === 'in_progress' ? ensureSingleInProgress(next, existing) : next;
    return { list: ranked, landing: existing + 1 };
  }
  let working = list.map((item) => ({ ...item }));
  let item: TodoItem;
  if (existing >= 0) {
    item = working.splice(existing, 1)[0]!;
    if (status) item = { ...item, status };
  } else {
    item = { content, status: status ?? 'pending' };
  }
  const idx = destinationIndex(position, working.length);
  working.splice(idx, 0, item);
  if (working[idx]!.status === 'in_progress') working = ensureSingleInProgress(working, idx);
  return { list: working, landing: idx + 1 };
}

function parseActionStatus(value: Record<string, unknown>): TodoStatus | null {
  return typeof value.status === 'string' ? parseStatus(value.status) : null;
}

function resolveUpdateId(
  id: number,
  visibleLen: number,
  addLandings: number[],
  newLen: number,
): number | null {
  if (id < 1) return null;
  if (addLandings.length > 0 && id > visibleLen) {
    const k = id - visibleLen;
    if (k >= 1 && k <= addLandings.length) return addLandings[k - 1]!;
  }
  return id <= newLen ? id : null;
}

/** Full-list plan shape. Returns null when args are not a valid plan. */
export function parseTodoPlan(args: string): TodoItem[] | null {
  let value: unknown;
  try {
    value = JSON.parse(args);
  } catch {
    return null;
  }
  if (!isRecord(value)) return null;

  // `actions` takes precedence over leftover `todos` field (same as Rust backend).
  if (Array.isArray(value.actions)) return null;

  let todos = value.todos;
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
  for (const raw of todos) {
    if (!isRecord(raw)) return null;
    const content = typeof raw.content === 'string' ? normalizeTodoContent(raw.content) : '';
    const status = typeof raw.status === 'string' ? parseStatus(raw.status) : null;
    if (!content || !status) return null;
    const next = upsertTodo(out, content, status, null);
    out.length = 0;
    out.push(...next.list);
  }
  return out;
}

function applyOne(list: TodoItem[], v: Record<string, unknown>): TodoItem[] {
  const kind = actionKind(v);
  if (kind === 'add') {
    const content = typeof v.content === 'string' ? normalizeTodoContent(v.content) : '';
    if (!content) return list;
    return upsertTodo(maybeAutoClearFinished(list), content, parseActionStatus(v), null).list;
  }
  if (kind === 'insert') {
    const content = typeof v.content === 'string' ? normalizeTodoContent(v.content) : '';
    if (!content) return list;
    return upsertTodo(list, content, parseActionStatus(v), insertPosition(v)).list;
  }
  if (kind === 'update') {
    const id = jsonId(v);
    const status = parseActionStatus(v);
    const content = typeof v.content === 'string' ? normalizeTodoContent(v.content) : '';
    if (id === null || id < 1 || id > list.length || (!status && !content)) return list;
    return applyUpdateAt(list, id, status, content || null);
  }
  if (kind === 'delete') {
    const id = jsonId(v);
    if (id === null || id < 1 || id > list.length) return list;
    const next = list.map((item) => ({ ...item }));
    next.splice(id - 1, 1);
    return next;
  }
  if (kind === 'clear') {
    return [];
  }
  return list;
}

function applyUpdateAt(
  list: TodoItem[],
  id: number,
  status: TodoStatus | null,
  content: string | null,
): TodoItem[] {
  const idx = id - 1;
  let next = list.map((item) => ({ ...item }));
  if (content) {
    const other = findTodoIndex(next, content);
    if (other >= 0 && other !== idx) {
      if (status) next[other] = { ...next[other]!, status };
      next.splice(idx, 1);
      const kept = other > idx ? other - 1 : other;
      return status === 'in_progress' ? ensureSingleInProgress(next, kept) : next;
    }
    next[idx] = { ...next[idx]!, content };
  }
  if (status) {
    next[idx] = { ...next[idx]!, status };
    if (status === 'in_progress') next = ensureSingleInProgress(next, idx);
  }
  return next;
}

function applyBatch(list: TodoItem[], actions: Record<string, unknown>[]): TodoItem[] {
  if (!validateActionsMix(actions)) return list;
  const kinds = new Set(actions.map(actionKind));
  let next = kinds.has('clear') ? [] : list;

  if (kinds.has('delete')) {
    const ids: number[] = [];
    for (const action of actions) {
      const id = jsonId(action);
      if (id !== null && id <= list.length && !ids.includes(id)) {
        ids.push(id);
      }
    }
    ids.sort((a, b) => b - a);
    const deleted = next.slice();
    for (const id of ids) deleted.splice(id - 1, 1);
    return deleted;
  }

  const visibleLen = next.length;
  const addLandings: number[] = [];
  if (actions.some((action) => actionKind(action) === 'add')) {
    next = maybeAutoClearFinished(next);
    for (const action of actions) {
      if (actionKind(action) !== 'add') continue;
      const content = typeof action.content === 'string' ? normalizeTodoContent(action.content) : '';
      if (!content) return list;
      const added = upsertTodo(next, content, parseActionStatus(action), null);
      next = added.list;
      addLandings.push(added.landing);
    }
  }

  const inserts = actions
    .filter((action) => actionKind(action) === 'insert')
    .sort((a, b) => (insertPosition(b) ?? Number.POSITIVE_INFINITY) - (insertPosition(a) ?? Number.POSITIVE_INFINITY));
  for (const action of inserts) {
    next = applyOne(next, action);
  }

  for (const action of actions) {
    if (actionKind(action) !== 'update') continue;
    const id = jsonId(action);
    if (id === null) return list;
    const resolved = resolveUpdateId(id, visibleLen, addLandings, next.length);
    if (resolved === null) return list;
    const status = parseActionStatus(action);
    const content = typeof action.content === 'string' ? normalizeTodoContent(action.content) : '';
    if (!status && !content) return list;
    next = applyUpdateAt(next, resolved, status, content || null);
  }
  return next;
}

/** Apply one incremental action or actions batch; returns a new array (or same ref if no-op). */
export function applyTodoAction(list: TodoItem[], args: string): TodoItem[] {
  let v: Record<string, unknown>;
  try {
    const parsed = JSON.parse(args);
    if (!isRecord(parsed)) return list;
    v = parsed;
  } catch {
    return list;
  }

  if (Array.isArray(v.actions)) {
    const actions = v.actions.filter(isRecord);
    if (actions.length !== v.actions.length || actions.length === 0) return list;
    return applyBatch(list, actions);
  }

  return applyOne(list, v);
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
  const base = current ?? [];
  const next = applyTodoAction(base, args);
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
