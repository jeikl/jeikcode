import { test } from 'node:test';
import assert from 'node:assert';
import { buildTurnNavItems, truncateTurnNavLabel, turnNavId } from './turnNav.ts';

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
    items.map((i) => ({ id: i.id, index: i.index, label: i.label })),
    [
      { id: 'turn-nav-0', index: 0, label: '你好 今天天气' },
      { id: 'turn-nav-4', index: 4, label: '第二问' },
    ],
  );
});
