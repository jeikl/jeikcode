import test from 'node:test';
import assert from 'node:assert/strict';
import {
  applyTodoCall,
  reduceTodosFromMessages,
  shouldRenderToolCall,
} from '../src/state/todo';
import type { ChatMessage, TodoItemData, ToolCallData } from '../src/state/types';

declare const require: {
  (id: string): typeof import('../src/state/reducer');
};

(globalThis as unknown as { document: { body: { dataset: { viewMode: string } } } }).document = {
  body: { dataset: { viewMode: 'sidebar' } },
};

const { chatReducer, initialState } = require('../src/state/reducer');

const plan = JSON.stringify({
  todos: [
    { content: '分析问题', status: 'completed' },
    { content: '实现功能', status: 'in_progress' },
    { content: '验证结果', status: 'pending' },
  ],
});

test('todo projection follows full-list and incremental tool semantics', () => {
  let items = applyTodoCall([], 'todowrite', plan);
  assert.deepEqual(items, [
    { content: '分析问题', status: 'completed' },
    { content: '实现功能', status: 'in_progress' },
    { content: '验证结果', status: 'pending' },
  ]);

  items = applyTodoCall(items, 'todowrite', '{"action":"update","id":3,"status":"in_progress"}');
  assert.deepEqual(items, [
    { content: '分析问题', status: 'completed' },
    { content: '实现功能', status: 'pending' },
    { content: '验证结果', status: 'in_progress' },
  ]);

  items = applyTodoCall(items, 'todo', '{"action":"add","content":"  补充文档  "}');
  assert.deepEqual(items.at(-1), { content: '补充文档', status: 'pending' });
});

test('actions batch is id-addressed, transactional, and wins over leftover todos', () => {
  let items = applyTodoCall([], 'todowrite', JSON.stringify({
    actions: [
      { action: 'add', content: 'a' },
      { action: 'add', content: 'b' },
      { action: 'add', content: 'c' },
    ],
  }));
  assert.deepEqual(items.map((item) => item.content), ['a', 'b', 'c']);

  items = applyTodoCall(items, 'todowrite', JSON.stringify({
    actions: [
      { action: 'update', id: 3, status: 'completed' },
      { action: 'update', id: 1, status: 'completed' },
      { action: 'update', id: 2, status: 'in_progress' },
    ],
  }));
  assert.equal(items[0].status, 'completed');
  assert.equal(items[1].status, 'in_progress');
  assert.equal(items[2].status, 'completed');

  const beforeBad = items.slice();
  items = applyTodoCall(items, 'todowrite', JSON.stringify({
    actions: [
      { action: 'update', id: 1, status: 'pending' },
      { action: 'update', id: 99, status: 'in_progress' },
    ],
  }));
  assert.deepEqual(items, beforeBad, 'failed batch must not apply');

  items = applyTodoCall(items, 'todowrite', JSON.stringify({
    actions: [
      { action: 'delete', id: 3 },
      { action: 'delete', id: 1 },
    ],
  }));
  assert.deepEqual(items, [{ content: 'b', status: 'in_progress' }]);

  const mixed = applyTodoCall(items, 'todowrite', JSON.stringify({
    actions: [{ action: 'add', content: 'from-actions' }],
    todos: [{ content: 'from-todos', status: 'pending' }],
  }));
  assert.deepEqual(mixed.map((item) => item.content), ['b', 'from-actions']);
});

test('clear + add + update in one batch replaces the plan', () => {
  const items = applyTodoCall(
    [
      { content: 'old-1', status: 'completed' },
      { content: 'old-2', status: 'in_progress' },
    ],
    'todowrite',
    JSON.stringify({
      actions: [
        { action: 'clear' },
        { action: 'add', content: 'new-1' },
        { action: 'add', content: 'new-2' },
        { action: 'update', id: 1, status: 'in_progress' },
      ],
    }),
  );
  assert.deepEqual(items, [
    { content: 'new-1', status: 'in_progress' },
    { content: 'new-2', status: 'pending' },
  ]);
});

test('add on a finished list auto-clears so ids restart at 1', () => {
  const items = applyTodoCall(
    [
      { content: 'done-1', status: 'completed' },
      { content: 'done-2', status: 'completed' },
    ],
    'todowrite',
    JSON.stringify({
      actions: [
        { action: 'add', content: 'next-1' },
        { action: 'add', content: 'next-2' },
        { action: 'update', id: 1, status: 'in_progress' },
      ],
    }),
  );
  assert.deepEqual(items, [
    { content: 'next-1', status: 'in_progress' },
    { content: 'next-2', status: 'pending' },
  ]);
});

test('add+update on a finished list accepts append-style ids', () => {
  const items = applyTodoCall(
    [{ content: '查询沉睡资源客户列表', status: 'completed' }],
    'todowrite',
    JSON.stringify({
      actions: [
        { action: 'add', content: '导出Excel', id: 2 },
        { action: 'update', id: 2, status: 'in_progress' },
      ],
    }),
  );
  assert.deepEqual(items, [{ content: '导出Excel', status: 'in_progress' }]);
});

test('duplicate titles upsert instead of duplicating', () => {
  const items = applyTodoCall(
    [{ content: '导出Excel', status: 'pending' }],
    'todowrite',
    JSON.stringify({ actions: [{ action: 'add', content: '导出Excel', status: 'in_progress' }] }),
  );
  assert.deepEqual(items, [{ content: '导出Excel', status: 'in_progress' }]);
});

test('add on an unfinished list does not auto-clear', () => {
  const items = applyTodoCall(
    [
      { content: 'done', status: 'completed' },
      { content: 'open', status: 'pending' },
    ],
    'todowrite',
    JSON.stringify({ action: 'add', content: 'extra' }),
  );
  assert.equal(items.length, 3);
  assert.equal(items.at(-1)?.content, 'extra');
});

test('valid re-plan replaces earlier state and an empty plan clears it', () => {
  let items = applyTodoCall([], 'todo', '{"action":"add","content":"旧任务"}');
  items = applyTodoCall(items, 'todowrite', '{"todos":[{"content":"新任务","status":"pending"}]}');
  assert.deepEqual(items, [{ content: '新任务', status: 'pending' }]);

  items = applyTodoCall(items, 'todowrite', '{"todos":[]}');
  assert.deepEqual(items, []);
});

test('malformed and unsupported todo calls do not corrupt prior state', () => {
  const initial: TodoItemData[] = [{ content: '保留任务', status: 'in_progress' }];
  const invalidCalls = [
    '{"todos":[{"content":"","status":"pending"}]}',
    '{"todos":[{"content":"   ","status":"pending"}]}',
    '{"action":"update","id":0,"status":"completed"}',
    '{"action":"update","id":9,"status":"completed"}',
    '{"action":"update","id":1,"status":"unknown"}',
    '{"action":"add","content":"   "}',
    '{"action":"frobnicate"}',
    'not-json',
  ];

  for (const args of invalidCalls) {
    assert.deepEqual(applyTodoCall(initial, 'todowrite', args), initial, args);
  }
  assert.deepEqual(applyTodoCall(initial, 'read_file', plan), initial);
});

test('history projection folds todo calls across assistant messages', () => {
  const messages: ChatMessage[] = [
    {
      id: 'm1',
      role: 'assistant',
      text: '',
      timestamp: 1,
      toolCalls: [{ id: 'p', name: 'todowrite', args: plan, status: 'done' }],
    },
    {
      id: 'm2',
      role: 'assistant',
      text: '',
      timestamp: 2,
      blocks: [{
        id: 'b1',
        type: 'tool',
        tool: {
          id: 'u',
          name: 'todo',
          args: '{"action":"update","id":2,"status":"completed"}',
          status: 'done',
        },
      }],
    },
  ];

  assert.deepEqual(reduceTodosFromMessages(messages), [
    { content: '分析问题', status: 'completed' },
    { content: '实现功能', status: 'completed' },
    { content: '验证结果', status: 'pending' },
  ]);
});

test('valid todo rows are replaced by the panel, but malformed and failed rows remain visible', () => {
  const tool = (args: string, status: ToolCallData['status']): ToolCallData => ({
    id: `${status}-${args}`,
    name: 'todowrite',
    args,
    status,
  });

  assert.equal(shouldRenderToolCall(tool(plan, 'queued')), false);
  assert.equal(shouldRenderToolCall(tool(plan, 'running')), false);
  assert.equal(shouldRenderToolCall(tool(plan, 'done')), false);
  assert.equal(shouldRenderToolCall(tool(plan, 'error')), true);
  assert.equal(shouldRenderToolCall(tool(plan, 'incomplete')), true);
  assert.equal(shouldRenderToolCall(tool('{"todos":"bad"}', 'done')), true);
  assert.equal(shouldRenderToolCall({ ...tool(plan, 'done'), name: 'read_file' }), true);
});

test('reducer applies todo mutation once when a queued call starts', () => {
  let state = chatReducer({ ...initialState, messages: [], activeTodos: [] }, { type: 'START_GENERATION' });
  state = chatReducer(state, {
    type: 'TOOL_BATCH_START',
    calls: [{ id: 'plan', name: 'todowrite', args: plan }],
  });
  assert.deepEqual(state.activeTodos, []);

  state = chatReducer(state, { type: 'TOOL_START', id: 'plan', name: 'todowrite', args: plan });
  assert.equal(state.activeTodos.length, 3);

  state = chatReducer(state, { type: 'TOOL_START', id: 'plan', name: 'todowrite', args: plan });
  assert.equal(state.activeTodos.length, 3, 'replayed TOOL_START must not apply the call twice');

  state = chatReducer(state, {
    type: 'TOOL_START',
    id: 'add',
    name: 'todowrite',
    args: '{"action":"add","content":"只添加一次"}',
  });
  state = chatReducer(state, {
    type: 'TOOL_START',
    id: 'add',
    name: 'todowrite',
    args: '{"action":"add","content":"只添加一次"}',
  });
  assert.equal(state.activeTodos.filter((item) => item.content === '只添加一次').length, 1);
});

test('replayed tool start cannot reopen a terminal todo call', () => {
  let state = chatReducer({ ...initialState, messages: [], activeTodos: [] }, { type: 'START_GENERATION' });
  state = chatReducer(state, { type: 'TOOL_START', id: 'plan', name: 'todowrite', args: plan });
  state = chatReducer(state, {
    type: 'TOOL_RESULT',
    id: 'plan',
    name: 'todowrite',
    output: 'runtime failure',
    success: false,
    durationMs: 1,
  });
  state = chatReducer(state, { type: 'TOOL_START', id: 'plan', name: 'todowrite', args: plan });

  assert.equal(state.messages[0].toolCalls?.[0]?.status, 'error');
  assert.equal(shouldRenderToolCall(state.messages[0].toolCalls![0]), true);
});

test('session load rebuilds projection, while switch and clear remove stale todos', () => {
  let state = chatReducer({ ...initialState, activeSessionId: 'old', activeTodos: [] }, {
    type: 'LOAD_SESSION_MESSAGES',
    messages: [
      {
        role: 'assistant',
        content: '',
        tool_calls: [{ id: 'plan', name: 'todowrite', arguments: plan }],
      },
      {
        role: 'assistant',
        content: '',
        tool_calls: [{
          id: 'update',
          name: 'todowrite',
          arguments: '{"action":"update","id":2,"status":"completed"}',
        }],
      },
    ],
  });
  assert.equal(state.activeTodos[1]?.status, 'completed');

  state = chatReducer(state, { type: 'SET_ACTIVE_SESSION', sessionId: 'new' });
  assert.deepEqual(state.activeTodos, []);

  state = { ...state, activeTodos: [{ content: '临时任务', status: 'pending' }] };
  state = chatReducer(state, { type: 'CLEAR_CHAT' });
  assert.deepEqual(state.activeTodos, []);
});
