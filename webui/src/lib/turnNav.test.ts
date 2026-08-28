import { test } from 'node:test';
import assert from 'node:assert';
import {
  buildTurnNavItems,
  filterTurnNavItems,
  resolveActiveTurnId,
  truncateTurnNavLabel,
  turnNavId,
} from './turnNav.ts';

test('turnNavId is stable per message index', () => {
  assert.equal(turnNavId(3), 'turn-nav-3');
});

test('truncateTurnNavLabel collapses whitespace and ellipsizes', () => {
  assert.equal(truncateTurnNavLabel('  hello   world  '), 'hello world');
  assert.equal(truncateTurnNavLabel('abcdefghij', 6), 'abcde…');
  assert.equal(truncateTurnNavLabel('short', 28), 'short');
  assert.equal(truncateTurnNavLabel('   '), '');
});

test('buildTurnNavItems keeps user questions in order and skips empty/system', () => {
  const items = buildTurnNavItems([
    { role: 'user', text: '你好 今天天气' },
    { role: 'assistant', text: '晴' },
    { role: 'system', text: 'notice' },
    { role: 'user', text: '' },
    { role: 'user', text: '第二问' },
  ]);
  assert.deepEqual(
    items.map((i) => ({ id: i.id, index: i.index, label: i.label, text: i.text })),
    [
      { id: 'turn-nav-0', index: 0, label: '你好 今天天气', text: '你好 今天天气' },
      { id: 'turn-nav-4', index: 4, label: '第二问', text: '第二问' },
    ],
  );
});

test('filterTurnNavItems matches full question text, not only the truncated label', () => {
  const items = buildTurnNavItems([
    { role: 'user', text: '请根据用户需求配置 MCP 服务器' },
    { role: 'user', text: '现在搜索试一下' },
  ]);
  const hit = filterTurnNavItems(items, 'mcp');
  assert.equal(hit.length, 1);
  assert.equal(hit[0].index, 0);
  assert.deepEqual(filterTurnNavItems(items, '没有这句'), []);
  assert.equal(filterTurnNavItems(items, '  ').length, 2);
});

test('resolveActiveTurnId is monotonic and pins the last item at the bottom', () => {
  const items = [{ id: 'a' }, { id: 'b' }, { id: 'c' }];
  const tops: Record<string, number> = { a: 0, b: 400, c: 800 };
  const topOf = (id: string) => tops[id];
  const metrics = (scrollTop: number) => ({
    scrollTop,
    clientHeight: 400,
    scrollHeight: 1200,
  });

  assert.equal(resolveActiveTurnId(items, topOf, metrics(0)), 'a');
  assert.equal(resolveActiveTurnId(items, topOf, metrics(390)), 'b');
  // Near the bottom of the thread: last question, even if the marker sits on a neighbour.
  assert.equal(resolveActiveTurnId(items, topOf, metrics(780)), 'c');
  assert.equal(resolveActiveTurnId(items, topOf, metrics(800)), 'c');
});

test('resolveActiveTurnId does not skip a short question just below the marker', () => {
  const items = [{ id: 'a' }, { id: 'b' }];
  const tops: Record<string, number> = { a: 8, b: 80 };
  const topOf = (id: string) => tops[id];
  // Clicking A scrolls it to the top; a 24px marker must not jump to B at 80px.
  assert.equal(
    resolveActiveTurnId(items, topOf, { scrollTop: 0, clientHeight: 600, scrollHeight: 2000 }),
    'a',
  );
});
