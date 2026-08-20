import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  applyTodoAction,
  foldTodoToolCall,
  parseTodoPlan,
  reduceTodosFromCalls,
  todoCounts,
} from './todos.ts';

test('parseTodoPlan accepts full list shape', () => {
  const items = parseTodoPlan(
    JSON.stringify({
      todos: [
        { content: 'a', status: 'completed' },
        { content: 'b', status: 'in_progress' },
        { content: 'c', status: 'pending' },
      ],
    }),
  );
  assert.equal(items?.length, 3);
  assert.equal(items?.[1]?.status, 'in_progress');
});

test('applyTodoAction handles batch actions[] payload', () => {
  let list = applyTodoAction([], JSON.stringify({
    actions: [
      { action: 'add', content: 'task 1' },
      { action: 'add', content: 'task 2' },
      { action: 'update', id: 1, status: 'in_progress' },
    ],
  }));
  assert.equal(list.length, 2);
  assert.equal(list[0]?.content, 'task 1');
  assert.equal(list[0]?.status, 'in_progress');
  assert.equal(list[1]?.content, 'task 2');
  assert.equal(list[1]?.status, 'pending');

  list = applyTodoAction(list, JSON.stringify({
    actions: [
      { action: 'update', id: 1, status: 'completed' },
      { action: 'update', id: 2, status: 'in_progress' },
    ],
  }));
  assert.equal(list[0]?.status, 'completed');
  assert.equal(list[1]?.status, 'in_progress');
});

test('foldTodoToolCall handles actions batch updates in live stream', () => {
  let cur = foldTodoToolCall(
    null,
    'todowrite',
    JSON.stringify({
      actions: [
        { action: 'add', content: 'Step 1' },
        { action: 'add', content: 'Step 2' },
        { action: 'update', id: 1, status: 'in_progress' },
      ],
    }),
  );
  assert.equal(cur?.length, 2);
  assert.equal(cur?.[0]?.status, 'in_progress');

  cur = foldTodoToolCall(
    cur,
    'todowrite',
    JSON.stringify({
      actions: [
        { action: 'update', id: 1, status: 'completed' },
        { action: 'update', id: 2, status: 'in_progress' },
      ],
    }),
  );
  assert.equal(cur?.length, 2);
  assert.equal(cur?.[0]?.status, 'completed');
  assert.equal(cur?.[1]?.status, 'in_progress');
});
test('reduceTodosFromCalls uses last plan then actions', () => {
  const list = reduceTodosFromCalls([
    {
      name: 'todowrite',
      args: JSON.stringify({
        todos: [
          { content: 'one', status: 'pending' },
          { content: 'two', status: 'pending' },
        ],
      }),
    },
    {
      name: 'todowrite',
      args: JSON.stringify({ action: 'update', id: 1, status: 'in_progress' }),
    },
    {
      name: 'todowrite',
      args: JSON.stringify({ action: 'update', id: 1, status: 'completed' }),
    },
  ]);
  assert.equal(list[0]?.status, 'completed');
  assert.equal(list[1]?.status, 'pending');
  assert.deepEqual(todoCounts(list), { completed: 1, inProgress: 0, total: 2 });
});

test('foldTodoToolCall replaces on re-plan and patches on action', () => {
  let cur = foldTodoToolCall(
    null,
    'todowrite',
    JSON.stringify({ todos: [{ content: 'x', status: 'pending' }] }),
  );
  assert.equal(cur?.length, 1);
  cur = foldTodoToolCall(
    cur,
    'todowrite',
    JSON.stringify({ action: 'update', id: 1, status: 'in_progress' }),
  );
  assert.equal(cur?.[0]?.status, 'in_progress');
  cur = foldTodoToolCall(
    cur,
    'todowrite',
    JSON.stringify({ todos: [{ content: 'y', status: 'pending' }] }),
  );
  assert.equal(cur?.[0]?.content, 'y');
});

test('applyTodoAction add appends', () => {
  const next = applyTodoAction(
    [{ content: 'a', status: 'pending' }],
    JSON.stringify({ action: 'add', content: 'b' }),
  );
  assert.equal(next.length, 2);
  assert.equal(next[1]?.content, 'b');
});

test('applyTodoAction insert places item between elements', () => {
  const list = [
    { content: 'a', status: 'pending' as const },
    { content: 'c', status: 'pending' as const },
  ];
  const next = applyTodoAction(
    list,
    JSON.stringify({ action: 'insert', position: 2, content: 'b' }),
  );
  assert.equal(next.length, 3);
  assert.equal(next[0]?.content, 'a');
  assert.equal(next[1]?.content, 'b');
  assert.equal(next[2]?.content, 'c');
});

test('applyTodoAction delete, remove, clear', () => {
  const list = [
    { content: 'a', status: 'pending' as const },
    { content: 'b', status: 'in_progress' as const },
    { content: 'c', status: 'completed' as const },
  ];
  const afterDel = applyTodoAction(list, JSON.stringify({ action: 'delete', id: 2 }));
  assert.equal(afterDel.length, 2);
  assert.equal(afterDel[0]?.content, 'a');
  assert.equal(afterDel[1]?.content, 'c');

  const afterRm = applyTodoAction(afterDel, JSON.stringify({ action: 'remove', id: 1 }));
  assert.equal(afterRm.length, 1);
  assert.equal(afterRm[0]?.content, 'c');

  const afterClear = applyTodoAction(afterRm, JSON.stringify({ action: 'clear' }));
  assert.equal(afterClear.length, 0);
});

test('foldTodoToolCall handles clear and delete', () => {
  let cur = foldTodoToolCall(
    null,
    'todowrite',
    JSON.stringify({ todos: [{ content: 'x', status: 'pending' }] }),
  );
  assert.equal(cur?.length, 1);
  cur = foldTodoToolCall(cur, 'todowrite', JSON.stringify({ action: 'clear' }));
  assert.equal(cur, null);
});

test('clear + add + update in one batch replaces the plan', () => {
  const list = applyTodoAction(
    [
      { content: 'old-1', status: 'completed' },
      { content: 'old-2', status: 'in_progress' },
    ],
    JSON.stringify({
      actions: [
        { action: 'clear' },
        { action: 'add', content: 'new-1' },
        { action: 'add', content: 'new-2' },
        { action: 'update', id: 1, status: 'in_progress' },
      ],
    }),
  );
  assert.deepEqual(list, [
    { content: 'new-1', status: 'in_progress' },
    { content: 'new-2', status: 'pending' },
  ]);
});

test('add on a finished list auto-clears so ids restart at 1', () => {
  const list = applyTodoAction(
    [
      { content: 'done-1', status: 'completed' },
      { content: 'done-2', status: 'completed' },
    ],
    JSON.stringify({
      actions: [
        { action: 'add', content: 'next-1' },
        { action: 'add', content: 'next-2' },
        { action: 'update', id: 1, status: 'in_progress' },
      ],
    }),
  );
  assert.deepEqual(list, [
    { content: 'next-1', status: 'in_progress' },
    { content: 'next-2', status: 'pending' },
  ]);
});

test('add on an unfinished list does not auto-clear', () => {
  const list = applyTodoAction(
    [
      { content: 'done', status: 'completed' },
      { content: 'open', status: 'pending' },
    ],
    JSON.stringify({ action: 'add', content: 'extra' }),
  );
  assert.equal(list.length, 3);
  assert.equal(list[2]?.content, 'extra');
});

test('clear + delete in one batch is rejected', () => {
  const before = [
    { content: 'a', status: 'pending' as const },
    { content: 'b', status: 'pending' as const },
  ];
  const list = applyTodoAction(
    before,
    JSON.stringify({
      actions: [
        { action: 'clear' },
        { action: 'delete', id: 1 },
      ],
    }),
  );
  assert.deepEqual(list, before);
});

