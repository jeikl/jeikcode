import type { ChatMessage, TodoItemData, TodoStatus, ToolCallData } from './types';

type ActionKind = 'add' | 'insert' | 'update' | 'delete' | 'clear';

type TodoOperation =
  | { kind: 'plan'; items: TodoItemData[] }
  | { kind: 'batch'; actions: Record<string, unknown>[] }
  | { kind: 'single'; action: Record<string, unknown> }
  | { kind: 'list' };

const TODO_STATUSES = new Set<TodoStatus>(['pending', 'in_progress', 'completed']);

export function isTodoToolName(name: string): boolean {
  return name === 'todowrite' || name === 'todo';
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function actionKind(value: Record<string, unknown>): ActionKind | undefined {
  const action = value.action;
  if (action === 'add' || action === 'insert' || action === 'update' || action === 'clear') {
    return action;
  }
  if (action === 'delete' || action === 'remove') return 'delete';
  return undefined;
}

function jsonId(value: Record<string, unknown>): number | undefined {
  const id = parseNonNegInt(value.id);
  return id !== undefined && id >= 1 ? id : undefined;
}

function parseNonNegInt(raw: unknown): number | undefined {
  if (typeof raw === 'number' && Number.isSafeInteger(raw) && raw >= 0) return raw;
  if (typeof raw === 'string') {
    const parsed = Number.parseInt(raw.trim(), 10);
    if (Number.isSafeInteger(parsed) && parsed >= 0) return parsed;
  }
  return undefined;
}

function insertPosition(value: Record<string, unknown>): number | undefined {
  const direct = parseNonNegInt(value.position ?? value.id);
  if (direct !== undefined) return direct;
  const after = parseNonNegInt(value.after ?? value.after_id);
  return after === undefined ? undefined : after + 1;
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

function maybeAutoClearFinished(items: TodoItemData[]): TodoItemData[] {
  if (items.length > 0 && items.every((item) => item.status === 'completed')) return [];
  return items;
}

function normalizeTodoContent(content: string): string {
  return content.split(/\s+/).filter(Boolean).join(' ');
}

function findTodoIndex(list: TodoItemData[], content: string): number {
  return list.findIndex((item) => item.content === content);
}

function destinationIndex(position: number | undefined, len: number): number {
  if (position === undefined) return len;
  if (position <= 1) return 0;
  if (position - 1 <= len) return position - 1;
  return len;
}

function ensureSingleInProgress(items: TodoItemData[], idx: number): TodoItemData[] {
  return items.map((item, i) => {
    if (i === idx) return item;
    if (item.status === 'in_progress') return { ...item, status: 'pending' as const };
    return item;
  });
}

function upsertTodo(
  list: TodoItemData[],
  content: string,
  status: TodoStatus | undefined,
  position: number | undefined,
): { list: TodoItemData[]; landing: number } {
  const existing = findTodoIndex(list, content);
  if (existing >= 0 && position === undefined) {
    const next = list.map((item, i) => (
      i === existing && status ? { ...item, status } : item
    ));
    const ranked = status === 'in_progress' ? ensureSingleInProgress(next, existing) : next;
    return { list: ranked, landing: existing + 1 };
  }
  let working = list.map((item) => ({ ...item }));
  let item: TodoItemData;
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

function parseActionStatus(value: Record<string, unknown>): TodoStatus | undefined {
  return typeof value.status === 'string' && TODO_STATUSES.has(value.status as TodoStatus)
    ? value.status as TodoStatus
    : undefined;
}

function resolveUpdateId(
  id: number,
  visibleLen: number,
  addLandings: number[],
  newLen: number,
): number | undefined {
  if (id < 1) return undefined;
  if (addLandings.length > 0 && id > visibleLen) {
    const k = id - visibleLen;
    if (k >= 1 && k <= addLandings.length) return addLandings[k - 1];
  }
  return id <= newLen ? id : undefined;
}

function parsePlanItems(todos: unknown): TodoItemData[] | undefined {
  if (!Array.isArray(todos)) return undefined;
  const items: TodoItemData[] = [];
  for (const rawItem of todos) {
    if (!isRecord(rawItem)
      || typeof rawItem.content !== 'string'
      || typeof rawItem.status !== 'string'
      || !TODO_STATUSES.has(rawItem.status as TodoStatus)) {
      return undefined;
    }
    const content = normalizeTodoContent(rawItem.content);
    if (!content) return undefined;
    const next = upsertTodo(items, content, rawItem.status as TodoStatus, undefined);
    items.length = 0;
    items.push(...next.list);
  }
  return items;
}

function parseTodoOperation(name: string, args: string): TodoOperation | undefined {
  if (!isTodoToolName(name)) return undefined;

  let value: unknown;
  try {
    value = JSON.parse(args);
  } catch {
    return undefined;
  }
  if (!isRecord(value)) return undefined;

  // `actions` wins over a leftover `todos` field — same precedence as Rust.
  if (Array.isArray(value.actions)) {
    const actions = value.actions.filter(isRecord);
    if (actions.length !== value.actions.length || actions.length === 0) return undefined;
    if (!validateActionsMix(actions)) return undefined;
    return { kind: 'batch', actions };
  }

  if ('todos' in value) {
    const items = parsePlanItems(value.todos);
    return items ? { kind: 'plan', items } : undefined;
  }

  if (name === 'todo' && value.action === 'list') return { kind: 'list' };

  if (actionKind(value)) return { kind: 'single', action: value };
  return undefined;
}

function withSingleInProgress(items: TodoItemData[], index: number, status: TodoStatus): TodoItemData[] {
  return items.map((item, i) => {
    if (i === index) return { ...item, status };
    if (status === 'in_progress' && item.status === 'in_progress') {
      return { ...item, status: 'pending' };
    }
    return item;
  });
}

function applyOne(items: TodoItemData[], value: Record<string, unknown>): TodoItemData[] | undefined {
  const kind = actionKind(value);
  if (kind === 'add') {
    if (typeof value.content !== 'string') return undefined;
    const content = normalizeTodoContent(value.content);
    if (!content) return undefined;
    return upsertTodo(maybeAutoClearFinished(items), content, parseActionStatus(value), undefined).list;
  }
  if (kind === 'insert') {
    if (typeof value.content !== 'string') return undefined;
    const content = normalizeTodoContent(value.content);
    if (!content) return undefined;
    return upsertTodo(items, content, parseActionStatus(value), insertPosition(value)).list;
  }
  if (kind === 'update' || kind === 'delete') {
    const id = jsonId(value);
    if (id === undefined || id > items.length) return undefined;
    if (kind === 'delete') {
      return items.filter((_, index) => index !== id - 1);
    }
    const status = parseActionStatus(value);
    const content = typeof value.content === 'string' ? normalizeTodoContent(value.content) : '';
    if (!status && !content) return undefined;
    return applyUpdateAt(items, id, status, content || undefined);
  }
  if (kind === 'clear') return [];
  return undefined;
}

function applyUpdateAt(
  items: TodoItemData[],
  id: number,
  status: TodoStatus | undefined,
  content: string | undefined,
): TodoItemData[] {
  const idx = id - 1;
  let next = items.map((item) => ({ ...item }));
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
  if (status) next = withSingleInProgress(next, idx, status);
  return next;
}

function applyBatch(items: TodoItemData[], actions: Record<string, unknown>[]): TodoItemData[] | undefined {
  if (!validateActionsMix(actions)) return undefined;
  const kinds = new Set(actions.map(actionKind));
  let next = kinds.has('clear') ? [] : items;

  if (kinds.has('delete')) {
    const ids: number[] = [];
    for (const action of actions) {
      const id = jsonId(action);
      if (id === undefined || id > items.length) return undefined;
      if (!ids.includes(id)) ids.push(id);
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
      if (typeof action.content !== 'string') return undefined;
      const content = normalizeTodoContent(action.content);
      if (!content) return undefined;
      const added = upsertTodo(next, content, parseActionStatus(action), undefined);
      next = added.list;
      addLandings.push(added.landing);
    }
  }

  const inserts = actions
    .filter((action) => actionKind(action) === 'insert')
    .sort((a, b) => (insertPosition(b) ?? Number.POSITIVE_INFINITY) - (insertPosition(a) ?? Number.POSITIVE_INFINITY));
  for (const action of inserts) {
    const applied = applyOne(next, action);
    if (!applied) return undefined;
    next = applied;
  }

  for (const action of actions) {
    if (actionKind(action) !== 'update') continue;
    const id = jsonId(action);
    if (id === undefined) return undefined;
    const resolved = resolveUpdateId(id, visibleLen, addLandings, next.length);
    if (resolved === undefined) return undefined;
    const status = parseActionStatus(action);
    const content = typeof action.content === 'string' ? normalizeTodoContent(action.content) : '';
    if (!status && !content) return undefined;
    next = applyUpdateAt(next, resolved, status, content || undefined);
  }
  return next;
}

export function applyTodoCall(
  items: TodoItemData[],
  name: string,
  args: string,
): TodoItemData[] {
  const operation = parseTodoOperation(name, args);
  if (!operation || operation.kind === 'list') return items;
  if (operation.kind === 'plan') return operation.items;
  const applied = operation.kind === 'batch'
    ? applyBatch(items, operation.actions)
    : applyOne(items, operation.action);
  return applied ?? items;
}

export function reduceTodosFromMessages(messages: ChatMessage[]): TodoItemData[] {
  let items: TodoItemData[] = [];
  for (const message of messages) {
    const calls = message.toolCalls && message.toolCalls.length > 0
      ? message.toolCalls
      : (message.blocks ?? [])
          .filter((block) => block.type === 'tool')
          .map((block) => block.tool);
    for (const call of calls) {
      items = applyTodoCall(items, call.name, call.args);
    }
  }
  return items;
}

export function shouldRenderToolCall(tool: ToolCallData): boolean {
  if (!isTodoToolName(tool.name)) return true;
  if (tool.status === 'error' || tool.status === 'incomplete') return true;
  return parseTodoOperation(tool.name, tool.args) === undefined;
}
