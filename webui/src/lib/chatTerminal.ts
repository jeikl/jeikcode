export interface ChatDoneTerminal {
  stopReason?: string;
  message?: string;
}

export type ChatDoneDisposition =
  | { kind: 'completed'; discardQueued: false }
  | { kind: 'incomplete'; discardQueued: true; detail: string };

/**
 * Classify the daemon's authoritative turn terminal.
 *
 * Older daemons did not include `stop_reason`, so an absent reason remains a
 * natural completion for wire compatibility. Once a reason is present, only
 * kernel `Stopped` (`stopped` on the wire) means the model finished normally;
 * every other current or future reason is incomplete and must fail closed.
 */
export function classifyChatDone(terminal: ChatDoneTerminal): ChatDoneDisposition {
  if (terminal.stopReason === undefined || terminal.stopReason === 'stopped') {
    return { kind: 'completed', discardQueued: false };
  }

  const message = terminal.message?.trim();
  return {
    kind: 'incomplete',
    discardQueued: true,
    detail: message || terminal.stopReason || 'unknown_terminal',
  };
}

export interface LiveLifecycleState {
  running: boolean;
  terminalConsumed: boolean;
}

export type LiveLifecycleEvent =
  | { type: 'snapshot' }
  | { type: 'input_accepted' }
  | { type: 'state'; running: boolean; stopReason?: string; message?: string }
  | { type: 'error'; message: string };

export interface LiveLifecycleTransition {
  state: LiveLifecycleState;
  terminal?: ChatDoneDisposition;
  diagnostic?: string;
}

export function createLiveLifecycleState(): LiveLifecycleState {
  return { running: false, terminalConsumed: false };
}

/**
 * Track only authoritative `/live` lifecycle observations.
 *
 * Kernel error events are diagnostics and cannot finish a turn. The following
 * `state { running: false }` owns the terminal, and duplicate idle states are
 * ignored so queue/persistence side effects happen once.
 */
export function reduceLiveLifecycle(
  current: LiveLifecycleState,
  event: LiveLifecycleEvent,
): LiveLifecycleTransition {
  switch (event.type) {
    case 'snapshot':
      return { state: createLiveLifecycleState() };
    case 'input_accepted':
      return { state: { running: true, terminalConsumed: false } };
    case 'error':
      return { state: current, diagnostic: event.message };
    case 'state':
      if (event.running) {
        return { state: { running: true, terminalConsumed: false } };
      }
      if (current.terminalConsumed) {
        return { state: { running: false, terminalConsumed: true } };
      }
      return {
        state: { running: false, terminalConsumed: true },
        terminal: classifyChatDone({
          stopReason: event.stopReason,
          message: event.message,
        }),
      };
  }
}

/** A snapshot is persisted history, not evidence that a turn is active. */
export function restoreLiveSnapshot<T>(messages: T[]): { messages: T[]; running: false } {
  return { messages, running: false };
}

type CanvasPart = { kind: string; text?: string };
type CanvasMessage = { role: string; parts: CanvasPart[] };

function canvasUserText(message: CanvasMessage | undefined): string | undefined {
  return message?.parts.find((part) => part.kind === 'text')?.text;
}

/** True when `userText` is already the latest user turn on the canvas.
 * Snapshot reconnect / `/chat/watch` replay both re-emit that echo; appending
 * it again creates duplicate bubbles. */
export function userMessageAlreadyOnCanvas(
  messages: CanvasMessage[],
  userText: string,
): boolean {
  if (!userText) return false;
  const last = messages[messages.length - 1];
  if (!last) return false;
  if (last.role === 'user' && canvasUserText(last) === userText) return true;
  if (last.role === 'assistant' && messages.length >= 2) {
    const prev = messages[messages.length - 2];
    if (prev?.role === 'user' && canvasUserText(prev) === userText) return true;
  }
  if (last.role === 'system' && messages.length >= 2) {
    const prev = messages[messages.length - 2];
    if (prev?.role === 'user' && canvasUserText(prev) === userText) return true;
    if (prev?.role === 'assistant' && messages.length >= 3) {
      const user = messages[messages.length - 3];
      if (user?.role === 'user' && canvasUserText(user) === userText) return true;
    }
  }
  return false;
}

export type LiveSnapshotQueueDisposition =
  | { discardQueued: false }
  | { discardQueued: true; reason: 'terminal_unknown' };

/**
 * A reconnect snapshot contains persisted history but no terminal reason. If
 * the previous connection observed an active turn, the daemon may have already
 * cleared that turn's replay window while the client was disconnected. Queued
 * input therefore cannot be drained safely until a fresh authoritative turn
 * lifecycle is observed.
 */
export function liveSnapshotQueueDisposition(
  wasRunning: boolean,
  queuedCount: number,
): LiveSnapshotQueueDisposition {
  return wasRunning && queuedCount > 0
    ? { discardQueued: true, reason: 'terminal_unknown' }
    : { discardQueued: false };
}

export type ChatRecoveryState =
  | 'ready'
  | 'checking'
  | 'detached_active'
  | 'terminal_unknown';

export type ChatRecoveryEvent =
  | { type: 'session_switch'; hasSession: boolean }
  | { type: 'active_check_succeeded'; active: boolean }
  | { type: 'active_check_failed' }
  | { type: 'transport_lost' }
  | { type: 'stop_succeeded' }
  | { type: 'stop_failed' }
  | { type: 'authoritative_terminal' };

export interface ChatRecoveryPolicy {
  allowSend: boolean;
  allowQueueDrain: boolean;
  allowStop: boolean;
}

/**
 * `/chat` has cancellation and active-operation discovery, but no stream
 * reattach protocol. Keep that recovery boundary explicit: until discovery or
 * cancellation proves the old operation is gone, neither direct sends nor
 * queued auto-sends are safe.
 */
export function reduceChatRecovery(
  _current: ChatRecoveryState,
  event: ChatRecoveryEvent,
): ChatRecoveryState {
  switch (event.type) {
    case 'session_switch':
      return event.hasSession ? 'checking' : 'ready';
    case 'active_check_succeeded':
      return event.active ? 'detached_active' : 'ready';
    case 'active_check_failed':
    case 'transport_lost':
    case 'stop_failed':
      return 'terminal_unknown';
    case 'stop_succeeded':
    case 'authoritative_terminal':
      return 'ready';
  }
}

export function chatRecoveryPolicy(state: ChatRecoveryState): ChatRecoveryPolicy {
  if (state === 'ready') {
    return { allowSend: true, allowQueueDrain: true, allowStop: false };
  }
  return { allowSend: false, allowQueueDrain: false, allowStop: true };
}

export type SyncAttachDisposition =
  | { allowed: true }
  | { allowed: false; reason: 'active_chat' | 'unresolved_chat' };

export function syncAttachDisposition(
  chatRunning: boolean,
  recoveryState: ChatRecoveryState,
): SyncAttachDisposition {
  if (chatRunning) return { allowed: false, reason: 'active_chat' };
  if (recoveryState !== 'ready') {
    return { allowed: false, reason: 'unresolved_chat' };
  }
  return { allowed: true };
}

/** Gate every `/chat` event, not only the promise terminal, against replacement. */
export function isCurrentChatStream(
  requestId: string,
  requestGeneration: number,
  currentRequestId: string | null,
  currentGeneration: number,
  aborted: boolean,
): boolean {
  return (
    !aborted &&
    requestId === currentRequestId &&
    requestGeneration === currentGeneration
  );
}

export type LiveDetachDisposition =
  | { allowed: true }
  | { allowed: false; reason: 'active_turn' };

export function liveDetachDisposition(running: boolean): LiveDetachDisposition {
  return running ? { allowed: false, reason: 'active_turn' } : { allowed: true };
}

/** Switching the WebUI view never rebinds the live runtime. The live stream
 * keeps publishing the task in the background; the sidebar only changes which
 * transcript is on screen. */
export function liveSessionSwitchDisposition(_running?: boolean): LiveDetachDisposition {
  return { allowed: true };
}

/** Clear only the structured-input prompt whose native request was resolved.
 * A late terminal for an older request must never dismiss a newer prompt. */
export function resolveUserInputRequest<T extends { request_id: number }>(
  current: T | null,
  resolvedRequestId: number,
): T | null {
  return current?.request_id === resolvedRequestId ? null : current;
}

/** Tracks prefix-cache estimation across successive provider usage events. */
export type TokenCacheState = {
  /** Prompt tokens from the immediately prior LLM usage event. */
  lastPrompt: number;
  /** True once the provider has reported `cached > 0` (telemetry is trusted). */
  providerReportsCache: boolean;
};

export function createTokenCacheState(): TokenCacheState {
  return { lastPrompt: 0, providerReportsCache: false };
}

export function resetTokenCacheState(baselinePrompt = 0): TokenCacheState {
  return { lastPrompt: Math.max(0, baselinePrompt), providerReportsCache: false };
}

/**
 * Prefix-cache estimate when upstream omits cached telemetry.
 *
 * Model: each request reuses the longest matching prefix from the prior request.
 * So on warm paths, `cached ≈ min(current_prompt, previous_prompt)`.
 * A sharp prompt drop (compaction / rewrite) invalidates the cache key.
 */
export function estimatePrefixCached(currentPrompt: number, previousPrompt: number): number {
  if (currentPrompt <= 0 || previousPrompt <= 0) return 0;
  if (currentPrompt < previousPrompt * 0.9) return 0;
  return Math.min(currentPrompt, previousPrompt);
}

/** Resolve cache telemetry for the footer pill. Provider `cached > 0` wins;
 * once a provider has reported cache hits we also trust explicit zeros;
 * otherwise fall back to prefix estimation against the prior usage prompt. */
export function resolveTokenCache(
  event: { prompt: number; cached?: number },
  state: TokenCacheState,
): { cached: number; cached_estimated: boolean; nextState: TokenCacheState } {
  const prompt = Math.max(0, event.prompt);
  const reported = event.cached != null ? Math.max(0, event.cached) : null;
  let providerReportsCache = state.providerReportsCache;
  if (reported != null && reported > 0) providerReportsCache = true;

  let cached = 0;
  let cached_estimated = false;

  if (reported != null && reported > 0) {
    cached = reported;
  } else if (reported === 0 && providerReportsCache) {
    // Provider may report cached=0 on the first usage event of a new turn
    // before prefix cache warms up. Prefer prefix estimation when plausible.
    const estimated = estimatePrefixCached(prompt, state.lastPrompt);
    if (estimated > 0) {
      cached = estimated;
      cached_estimated = true;
    } else {
      cached = 0;
    }
  } else {
    const estimated = estimatePrefixCached(prompt, state.lastPrompt);
    if (estimated > 0) {
      cached = estimated;
      cached_estimated = true;
    }
  }

  return {
    cached,
    cached_estimated,
    nextState: { lastPrompt: prompt, providerReportsCache },
  };
}

/** Recompute cache while locally growing prompt between provider usage events. */
export function estimateLocalCached(
  state: TokenCacheState,
  prompt: number,
  prev: { cached?: number; cached_estimated?: boolean } | null | undefined,
): { cached: number; cached_estimated: boolean } {
  const prevCached = prev?.cached ?? 0;
  if (prev && prevCached > 0) {
    return {
      cached: prevCached,
      cached_estimated: Boolean(prev.cached_estimated),
    };
  }
  if (state.providerReportsCache && prev && prevCached === 0) {
    return { cached: 0, cached_estimated: false };
  }
  const estimated = estimatePrefixCached(prompt, state.lastPrompt);
  return { cached: estimated, cached_estimated: estimated > 0 };
}

/** Offline cache estimate when reloading a session from message history only. */
export function estimateCacheFromHistoryPrompt(
  currentPrompt: number,
  promptBeforeLastUserTurn: number,
): { cached: number; cached_estimated: boolean; cacheState: TokenCacheState } {
  const cached = estimatePrefixCached(currentPrompt, promptBeforeLastUserTurn);
  return {
    cached,
    cached_estimated: cached > 0,
    cacheState: {
      lastPrompt: currentPrompt,
      providerReportsCache: false,
    },
  };
}

/** Provider-style cache hit rate: cached_input / total_prompt_tokens.
 * Kernel `prompt` already includes cached tokens for Anthropic/OpenAI adapters. */
export function formatCacheHitRate(cached: number, prompt: number): string | null {
  if (cached <= 0 || prompt <= 0) return null;
  if (cached >= prompt) return '100%';
  const ratio = (cached / prompt) * 100;
  if (ratio >= 99.95) return '99.9%';
  if (ratio >= 10) return `${Math.round(ratio)}%`;
  return `${ratio.toFixed(1)}%`;
}
