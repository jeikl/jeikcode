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
  if (kinds.has('clear') && kinds.size > 1) return false;
  if (kinds.has('delete') && [...kinds].some((kind) => kind !== 'delete')) return false;
  if (kinds.has('insert') && [...kinds].some((kind) => kind !== 'insert' && kind !== 'update')) {
    return false;
  }
  return true;
}

function parsePlanItems(todos: unknown): TodoItemData[] | undefined {
  if (!Array.isArray(todos)) return undefined;
  const items: TodoItemData[] = [];
  let inProgress = 0;
  for (const rawItem of todos) {
    if (!isRecord(rawItem)
      || typeof rawItem.content !== 'string'
      || rawItem.content.trim().length === 0
      || typeof rawItem.status !== 'string'
      || !TODO_STATUSES.has(rawItem.status as TodoStatus)) {
      return undefined;
    }
    const status = rawItem.status as TodoStatus;
    if (status === 'in_progress') inProgress += 1;
    items.push({ content: rawItem.content, status });
  }
  return inProgress <= 1 ? items : undefined;
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
    if (typeof value.content !== 'string' || value.content.trim().length === 0) return undefined;
    return [...items, { content: value.content.trim(), status: 'pending' }];
  }
  if (kind === 'insert') {
    if (typeof value.content !== 'string' || value.content.trim().length === 0) return undefined;
    const status = typeof value.status === 'string' && TODO_STATUSES.has(value.status as TodoStatus)
      ? value.status as TodoStatus
      : 'pending';
    const pos = insertPosition(value);
    const idx = pos === undefined
      ? items.length
      : (pos <= 1 ? 0 : (pos - 1 <= items.length ? pos - 1 : items.length));
    const next = items.map((item) => (
      status === 'in_progress' && item.status === 'in_progress'
        ? { ...item, status: 'pending' as const }
        : item
    ));
    next.splice(idx, 0, { content: value.content.trim(), status });
    return next;
  }
  if (kind === 'update' || kind === 'delete') {
    const id = jsonId(value);
    if (id === undefined || id > items.length) return undefined;
    if (kind === 'delete') {
      return items.filter((_, index) => index !== id - 1);
    }
    if (typeof value.status !== 'string' || !TODO_STATUSES.has(value.status as TodoStatus)) {
      return undefined;
    }
    return withSingleInProgress(items, id - 1, value.status as TodoStatus);
  }
  if (kind === 'clear') return [];
  return undefined;
}

function applyBatch(items: TodoItemData[], actions: Record<string, unknown>[]): TodoItemData[] | undefined {
  if (!validateActionsMix(actions)) return undefined;
  const kinds = new Set(actions.map(actionKind));
  if (kinds.has('clear')) return [];

  if (kinds.has('delete')) {
    const ids: number[] = [];
    for (const action of actions) {
      const id = jsonId(action);
      if (id === undefined || id > items.length) return undefined;
      if (!ids.includes(id)) ids.push(id);
    }
    ids.sort((a, b) => b - a);
    const next = items.slice();
    for (const id of ids) next.splice(id - 1, 1);
    return next;
  }

  let next = items;
  for (const action of actions) {
    if (actionKind(action) !== 'add') continue;
    const applied = applyOne(next, action);
    if (!applied) return undefined;
    next = applied;
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
    const applied = applyOne(next, action);
    if (!applied) return undefined;
    next = applied;
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
