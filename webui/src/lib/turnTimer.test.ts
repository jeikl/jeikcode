import { test } from 'node:test';
import assert from 'node:assert/strict';
import { formatTurnElapsed, stampLastAssistantElapsed } from './turnTimer.ts';

test('formatTurnElapsed stays in seconds until 60, then rolls to m:ss', () => {
  assert.equal(formatTurnElapsed(0), '0s');
  assert.equal(formatTurnElapsed(999), '0s');
  assert.equal(formatTurnElapsed(1000), '1s');
  assert.equal(formatTurnElapsed(59_000), '59s');
  assert.equal(formatTurnElapsed(60_000), '1:00');
  assert.equal(formatTurnElapsed(61_500), '1:01');
  assert.equal(formatTurnElapsed(12 * 60_000 + 3_000), '12:03');
  assert.equal(formatTurnElapsed(3600_000), '1:00:00');
  assert.equal(formatTurnElapsed(3600_000 + 65_000), '1:01:05');
});

test('stampLastAssistantElapsed writes the latest unstamped assistant only', () => {
  const msgs = [
    { role: 'user' as const },
    { role: 'assistant' as const },
    { role: 'system' as const },
  ];
  const next = stampLastAssistantElapsed(msgs, 4200, 1_700_000_000_000);
  assert.equal(next[1]!.elapsedMs, 4200);
  assert.equal(next[1]!.ts, 1_700_000_000_000);
  assert.equal(next[0]!.elapsedMs, undefined);
  const again = stampLastAssistantElapsed(next, 9999);
  assert.equal(again[1]!.elapsedMs, 4200);
  assert.equal(again, next);
});

test('stampLastAssistantElapsed is a no-op when there is no assistant', () => {
  const msgs = [{ role: 'user' as const }];
  assert.equal(stampLastAssistantElapsed(msgs, 1000), msgs);
});
