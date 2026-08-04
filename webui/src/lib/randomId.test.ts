import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ensureRandomUUIDPolyfill, randomUUID } from './randomId.ts';

const UUID_V4 =
  /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

test('randomUUID returns a RFC4122 v4-shaped id', () => {
  const id = randomUUID();
  assert.match(id, UUID_V4);
});

test('randomUUID produces distinct values', () => {
  const a = randomUUID();
  const b = randomUUID();
  assert.notEqual(a, b);
});

test('polyfill does not recurse through crypto.randomUUID', () => {
  // Regression: installing randomUUID as crypto.randomUUID used to blow the stack
  // because randomUUID() re-entered via crypto.randomUUID().
  ensureRandomUUIDPolyfill();
  const id = randomUUID();
  assert.match(id, UUID_V4);
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    assert.match(crypto.randomUUID(), UUID_V4);
  }
});
