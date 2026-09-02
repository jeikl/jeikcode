import { test } from 'node:test';
import assert from 'node:assert/strict';
import { deleteSessionsStreaming } from './sessionDelete.ts';

const target = (id: string): { project_hash: string; id: string; name: string } => ({
  project_hash: '0123456789abcdef',
  id,
  name: `session-${id}`,
});

test('streaming delete reports each success before the batch finishes', async () => {
  const seen: string[] = [];
  let released: ((value?: unknown) => void) | null = null;
  const holdSecond = new Promise((resolve) => {
    released = resolve;
  });

  const done = deleteSessionsStreaming(
    [target('a'), target('b')],
    async (_hash, id) => {
      if (id === 'b') await holdSecond;
    },
    (id) => {
      seen.push(id);
    },
    1,
  );

  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.deepEqual(seen, ['a'], 'the first finished delete must surface before slower ones');
  released?.();
  const { failed } = await done;
  assert.deepEqual(failed, []);
  assert.deepEqual(seen, ['a', 'b']);
});

test('streaming delete keeps going after a per-item failure', async () => {
  const seen: string[] = [];
  const { failed } = await deleteSessionsStreaming(
    [target('ok'), target('bad'), target('ok2')],
    async (_hash, id) => {
      if (id === 'bad') throw new Error('nope');
    },
    (id) => {
      seen.push(id);
    },
    1,
  );
  assert.deepEqual(seen, ['ok', 'ok2']);
  assert.equal(failed.length, 1);
  assert.equal(failed[0].id, 'bad');
  assert.ok(failed[0].cause instanceof Error);
  assert.match((failed[0].cause as Error).message, /nope/);
});

test('worker pool runs more than one delete at a time', async () => {
  let inFlight = 0;
  let maxInFlight = 0;
  const { failed } = await deleteSessionsStreaming(
    [target('1'), target('2'), target('3'), target('4')],
    async () => {
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
      await new Promise((resolve) => setTimeout(resolve, 30));
      inFlight -= 1;
    },
    () => {},
    3,
  );
  assert.deepEqual(failed, []);
  assert.ok(maxInFlight >= 2, `expected overlapping deletes, max in-flight was ${maxInFlight}`);
});
