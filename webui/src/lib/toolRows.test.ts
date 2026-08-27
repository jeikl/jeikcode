import { test } from 'node:test';
import assert from 'node:assert';
import {
  appendToolOutput,
  MAX_LIVE_TOOL_OUTPUT,
  toolResultStatus,
  updateToolProgress,
  upsertToolPart,
  withTrailingTodoList,
  type MsgPart,
  type ToolRow,
} from './toolRows.ts';

const tool = (id: string, over: Partial<ToolRow> = {}): ToolRow => ({
  id,
  name: 'grep',
  args: '"foo"',
  status: 'pending',
  ...over,
});

test('a new tool id is appended as a tool part', () => {
  const parts: MsgPart[] = [{ kind: 'text', text: 'hi' }];
  const next = upsertToolPart(parts, tool('a'));
  assert.equal(next.length, 2);
  assert.deepEqual(next[1], { kind: 'tool', tool: tool('a') });
});

test('replaying the SAME tool id updates in place — no duplicate row', () => {
  // The webui bug: a duplicate /live subscription re-delivers the same
  // tool_start, which must NOT create a second row.
  const parts: MsgPart[] = [{ kind: 'tool', tool: tool('a') }];
  const again = upsertToolPart(parts, tool('a', { status: 'done', duration_ms: 12 }));
  assert.equal(again.length, 1, 'same id must not append a second row');
  assert.equal(again[0].kind, 'tool');
  const t = (again[0] as { kind: 'tool'; tool: ToolRow }).tool;
  assert.equal(t.status, 'done');
  assert.equal(t.duration_ms, 12);
  assert.equal(t.name, 'grep'); // prior fields preserved via merge
});

test('distinct tool ids each keep their own row (genuine repeat calls)', () => {
  // Different call_ids = different logical calls (e.g. across goal rounds) and
  // must stay as separate rows — dedup is by id, never by name+args.
  let parts: MsgPart[] = [];
  parts = upsertToolPart(parts, tool('a'));
  parts = upsertToolPart(parts, tool('b'));
  assert.equal(parts.length, 2);
});

test('appendToolOutput writes to the matching call id, not the latest row', () => {
  let parts: MsgPart[] = [
    { kind: 'tool', tool: tool('a', { name: 'bash' }) },
    { kind: 'tool', tool: tool('b', { name: 'bash' }) },
  ];
  parts = appendToolOutput(parts, 'a', 'hello');
  parts = appendToolOutput(parts, 'b', 'world');
  const a = parts[0]!.kind === 'tool' ? parts[0].tool : undefined;
  const b = parts[1]!.kind === 'tool' ? parts[1].tool : undefined;
  assert.equal(a?.output, 'hello');
  assert.equal(b?.output, 'world');
});

test('appendToolOutput falls back to the most recent tool when id is missing', () => {
  const parts = appendToolOutput(
    [
      { kind: 'text', text: 'hi' },
      { kind: 'tool', tool: tool('a') },
    ],
    undefined,
    'out',
  );
  assert.equal(parts[1]!.kind === 'tool' ? parts[1].tool.output : undefined, 'out');
});

test('appendToolOutput keeps a bounded tail on huge streams', () => {
  const huge = 'x'.repeat(MAX_LIVE_TOOL_OUTPUT + 50);
  const parts = appendToolOutput([{ kind: 'tool', tool: tool('a') }], 'a', huge);
  const output = parts[0]!.kind === 'tool' ? parts[0].tool.output ?? '' : '';
  assert.equal(output.length, MAX_LIVE_TOOL_OUTPUT);
  assert.equal(output, 'x'.repeat(MAX_LIVE_TOOL_OUTPUT));
});

test('upsertToolPart replaces live progress instead of accumulating it', () => {
  const first = updateToolProgress([{ kind: 'tool', tool: tool('a') }], 'a', 'round 1 · thinking');
  const second = updateToolProgress(first, 'a', 'round 2 · read_file');
  const current = second[0].kind === 'tool' ? second[0].tool : undefined;

  assert.equal(current?.progress, 'round 2 · read_file');
});

test('non-tool parts and ordering are preserved', () => {
  const parts: MsgPart[] = [
    { kind: 'text', text: 'before' },
    { kind: 'tool', tool: tool('a') },
  ];
  const next = upsertToolPart(parts, tool('a', { status: 'done' }));
  assert.equal(next.length, 2);
  assert.deepEqual(next[0], { kind: 'text', text: 'before' });
});

test('partial review result is incomplete rather than a clean success or generic failure', () => {
  assert.equal(toolResultStatus(false, 'Code review incomplete (MaxRounds)'), 'incomplete');
});

test('withTrailingTodoList appends or replaces a trailing todo_list part', () => {
  const items = [
    { content: 'a', status: 'completed' as const },
    { content: 'b', status: 'pending' as const },
  ];
  const base: MsgPart[] = [
    { kind: 'text', text: 'done' },
    { kind: 'tool', tool: tool('t1', { name: 'todowrite' }) },
  ];
  const once = withTrailingTodoList(base, items);
  assert.equal(once.length, 3);
  assert.equal(once[2]!.kind, 'todo_list');
  if (once[2]!.kind === 'todo_list') {
    assert.equal(once[2].items.length, 2);
    assert.equal(once[2].items[0]!.content, 'a');
  }
  const twice = withTrailingTodoList(once, [{ content: 'only', status: 'in_progress' }]);
  assert.equal(twice.filter((p) => p.kind === 'todo_list').length, 1);
  assert.equal(twice[twice.length - 1]!.kind, 'todo_list');
  if (twice[twice.length - 1]!.kind === 'todo_list') {
    assert.equal(twice[twice.length - 1].items[0]!.content, 'only');
  }
  assert.deepEqual(withTrailingTodoList(once, []), base);
});
