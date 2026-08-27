import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  chatRecoveryPolicy,
  classifyChatDone,
  createLiveLifecycleState,
  isCurrentChatStream,
  liveDetachDisposition,
  liveSessionSwitchDisposition,
  liveSnapshotQueueDisposition,
  reduceChatRecovery,
  reduceLiveLifecycle,
  resolveUserInputRequest,
  restoreLiveSnapshot,
  syncAttachDisposition,
  resolveTokenCache,
  formatCacheHitRate,
  estimatePrefixCached,
  estimateLocalCached,
  estimateCacheFromHistoryPrompt,
  createTokenCacheState,
  resetTokenCacheState,
  userMessageAlreadyOnCanvas,
} from './chatTerminal.ts';

test('legacy done and stopped are natural completions that preserve queued messages', () => {
  assert.deepEqual(classifyChatDone({}), {
    kind: 'completed',
    discardQueued: false,
  });
  assert.deepEqual(classifyChatDone({ stopReason: 'stopped' }), {
    kind: 'completed',
    discardQueued: false,
  });
});

test('budget and loop-guard terminals are incomplete and discard queued messages', () => {
  assert.deepEqual(
    classifyChatDone({
      stopReason: 'max_rounds',
      message: 'Turn reached its round budget',
    }),
    {
      kind: 'incomplete',
      discardQueued: true,
      detail: 'Turn reached its round budget',
    },
  );

  for (const stopReason of ['max_continuations', 'repeat_loop', 'tool_loop_detected']) {
    assert.deepEqual(classifyChatDone({ stopReason }), {
      kind: 'incomplete',
      discardQueued: true,
      detail: stopReason,
    });
  }
});

test('unknown explicit stop reasons fail closed instead of being reported as success', () => {
  assert.deepEqual(classifyChatDone({ stopReason: 'future_terminal' }), {
    kind: 'incomplete',
    discardQueued: true,
    detail: 'future_terminal',
  });
});

test('live errors are diagnostic until one authoritative idle state consumes the terminal', () => {
  let state = createLiveLifecycleState();
  ({ state } = reduceLiveLifecycle(state, { type: 'state', running: true }));

  const diagnostic = reduceLiveLifecycle(state, {
    type: 'error',
    message: 'provider stream failed',
  });
  state = diagnostic.state;
  assert.equal(state.running, true);
  assert.equal(diagnostic.diagnostic, 'provider stream failed');
  assert.equal(diagnostic.terminal, undefined);

  const terminal = reduceLiveLifecycle(state, {
    type: 'state',
    running: false,
    stopReason: 'tool_loop_detected',
  });
  state = terminal.state;
  assert.equal(state.running, false);
  assert.deepEqual(terminal.terminal, {
    kind: 'incomplete',
    discardQueued: true,
    detail: 'tool_loop_detected',
  });

  const duplicate = reduceLiveLifecycle(state, {
    type: 'state',
    running: false,
    stopReason: 'tool_loop_detected',
  });
  assert.equal(duplicate.terminal, undefined);
});

test('live snapshot never infers running from a trailing user message', () => {
  const restored = restoreLiveSnapshot([{ role: 'user', text: 'persisted but idle' }]);
  assert.deepEqual(restored, {
    messages: [{ role: 'user', text: 'persisted but idle' }],
    running: false,
  });
});

test('user echo already on canvas is not appended again', () => {
  const user = { role: 'user', parts: [{ kind: 'text', text: '你好啊' }] };
  const assistant = { role: 'assistant', parts: [{ kind: 'text', text: '你好' }] };
  const notice = { role: 'system', parts: [{ kind: 'notice', text: 'sync' }] };
  assert.equal(userMessageAlreadyOnCanvas([user], '你好啊'), true);
  assert.equal(userMessageAlreadyOnCanvas([user, assistant], '你好啊'), true);
  assert.equal(userMessageAlreadyOnCanvas([user, notice], '你好啊'), true);
  assert.equal(userMessageAlreadyOnCanvas([user, assistant, notice], '你好啊'), true);
  assert.equal(userMessageAlreadyOnCanvas([user, assistant], '另一句'), false);
  assert.equal(userMessageAlreadyOnCanvas([], '你好啊'), false);
});

test('live replay input and running state restore an active turn after an idle snapshot', () => {
  let state = createLiveLifecycleState();
  ({ state } = reduceLiveLifecycle(state, { type: 'snapshot' }));
  assert.equal(state.running, false);

  ({ state } = reduceLiveLifecycle(state, { type: 'input_accepted' }));
  assert.equal(state.running, true);

  ({ state } = reduceLiveLifecycle(state, { type: 'state', running: true }));
  assert.equal(state.running, true);
});

test('an active live turn cannot be detached locally without a recoverable protocol', () => {
  assert.deepEqual(liveDetachDisposition(true), {
    allowed: false,
    reason: 'active_turn',
  });
  assert.deepEqual(liveDetachDisposition(false), { allowed: true });
});

test('switching the viewed session is always allowed; live tasks stay in the background', () => {
  assert.deepEqual(liveSessionSwitchDisposition(true), { allowed: true });
  assert.deepEqual(liveSessionSwitchDisposition(false), { allowed: true });
});

test('reconnect snapshot discards an unresolved queue when terminal replay is unavailable', () => {
  assert.deepEqual(liveSnapshotQueueDisposition(true, 2), {
    discardQueued: true,
    reason: 'terminal_unknown',
  });
  assert.deepEqual(liveSnapshotQueueDisposition(false, 2), {
    discardQueued: false,
  });
  assert.deepEqual(liveSnapshotQueueDisposition(true, 0), {
    discardQueued: false,
  });
});

test('detached and unknown chat recovery states block sends and queue draining', () => {
  let state = reduceChatRecovery('ready', {
    type: 'session_switch',
    hasSession: true,
  });
  assert.equal(state, 'checking');
  assert.deepEqual(chatRecoveryPolicy(state), {
    allowSend: false,
    allowQueueDrain: false,
    allowStop: true,
  });

  state = reduceChatRecovery(state, { type: 'active_check_failed' });
  assert.equal(state, 'terminal_unknown');
  assert.equal(chatRecoveryPolicy(state).allowSend, false);

  state = reduceChatRecovery(state, { type: 'stop_failed' });
  assert.equal(state, 'terminal_unknown');
  state = reduceChatRecovery(state, { type: 'stop_succeeded' });
  assert.equal(state, 'ready');
});

test('detached active chat unlocks only after stop success or an authoritative terminal', () => {
  let state = reduceChatRecovery('checking', {
    type: 'active_check_succeeded',
    active: true,
  });
  assert.equal(state, 'detached_active');
  assert.equal(chatRecoveryPolicy(state).allowSend, false);
  assert.equal(chatRecoveryPolicy(state).allowQueueDrain, false);

  state = reduceChatRecovery(state, { type: 'authoritative_terminal' });
  assert.equal(state, 'ready');
  assert.deepEqual(chatRecoveryPolicy(state), {
    allowSend: true,
    allowQueueDrain: true,
    allowStop: false,
  });
});

test('sync cannot replace an active or unresolved non-sync transport', () => {
  assert.deepEqual(syncAttachDisposition(true, 'ready'), {
    allowed: false,
    reason: 'active_chat',
  });
  assert.deepEqual(syncAttachDisposition(false, 'terminal_unknown'), {
    allowed: false,
    reason: 'unresolved_chat',
  });
  assert.deepEqual(syncAttachDisposition(false, 'ready'), { allowed: true });
});

test('late chat events are rejected by request and session generation', () => {
  assert.equal(isCurrentChatStream('request-a', 3, 'request-a', 3, false), true);
  assert.equal(isCurrentChatStream('request-a', 3, 'request-b', 3, false), false);
  assert.equal(isCurrentChatStream('request-a', 3, 'request-a', 4, false), false);
  assert.equal(isCurrentChatStream('request-a', 3, 'request-a', 3, true), false);
});

test('a live user-input terminal clears only its matching prompt', () => {
  const current = { request_id: 42, question: 'Pick one' };
  assert.equal(resolveUserInputRequest(current, 42), null);
  assert.equal(resolveUserInputRequest(current, 41), current);
  assert.equal(resolveUserInputRequest(null, 42), null);
});

test('prefix cache estimate reuses prior request prompt on warm paths', () => {
  assert.equal(estimatePrefixCached(0, 10_000), 0);
  assert.equal(estimatePrefixCached(38_555, 0), 0);
  assert.equal(estimatePrefixCached(38_555, 32_540), 32_540);
  assert.equal(estimatePrefixCached(40_000, 38_555), 38_555);
  // Compaction / rewrite invalidates cache key.
  assert.equal(estimatePrefixCached(5_000, 38_555), 0);
});

test('resolveTokenCache prefers provider telemetry, otherwise estimates prefix', () => {
  let state = createTokenCacheState();
  let r = resolveTokenCache({ prompt: 10_000, cached: 0 }, state);
  assert.equal(r.cached, 0);
  assert.equal(r.cached_estimated, false);
  state = r.nextState;

  r = resolveTokenCache({ prompt: 38_555, cached: 0 }, state);
  assert.equal(r.cached, 10_000);
  assert.equal(r.cached_estimated, true);
  state = r.nextState;

  r = resolveTokenCache({ prompt: 38_555, cached: 32_540 }, state);
  assert.equal(r.cached, 32_540);
  assert.equal(r.cached_estimated, false);
  state = r.nextState;

  r = resolveTokenCache({ prompt: 40_000, cached: 0 }, state);
  assert.equal(r.cached, 0);
  assert.equal(r.cached_estimated, false);
});

test('local prompt bumps keep estimated cache aligned with prior usage', () => {
  const state = { lastPrompt: 32_540, providerReportsCache: false };
  const prev = { cached: 32_540, cached_estimated: true };
  const local = estimateLocalCached(state, 35_000, prev);
  assert.equal(local.cached, 32_540);
  assert.equal(local.cached_estimated, true);
});

test('history prompt estimate uses prefix before last user turn', () => {
  const cache = estimateCacheFromHistoryPrompt(38_555, 10_000);
  assert.equal(cache.cached, 10_000);
  assert.equal(cache.cached_estimated, true);
});

test('cache hit rate follows provider cached over total prompt tokens', () => {
  assert.equal(formatCacheHitRate(0, 10_000), null);
  assert.equal(formatCacheHitRate(6200, 10_000), '62%');
  assert.equal(formatCacheHitRate(9996, 10_000), '99.9%');
  assert.equal(formatCacheHitRate(10_000, 10_000), '100%');
});
