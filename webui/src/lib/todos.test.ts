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
