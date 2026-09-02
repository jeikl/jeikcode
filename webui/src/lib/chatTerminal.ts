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

/** Empty hub projections (view-only “new session”) must not wipe a canvas that
 * already has turns. A reconnect after the first Submit used to paint the
 * landing page over the live transcript. */
export function keepCanvasOnEmptyLiveSnapshot(
  snapshotMessageCount: number,
  canvasMessageCount: number,
  viewingThisSession: boolean,
): boolean {
  return viewingThisSession && snapshotMessageCount === 0 && canvasMessageCount > 0;
}

/** Keep the existing `/live` SSE when returning to the execution session.
 * Reconnecting would snapshot-replace the canvas (dropping in-flight bash
 * output) and replay trailing text/user events on top of the restored cache. */
export function shouldReuseLiveStream(input: {
  sync: boolean;
  sessionId: string | null;
  liveSessionId: string | null;
  streamOpen: boolean;
}): boolean {
  return Boolean(
    input.sync &&
      input.sessionId &&
      input.liveSessionId === input.sessionId &&
      input.streamOpen,
  );
}

/** Resume the turn stopwatch from a stamped assistant duration after switching
 * back to a still-running session. */
export function resumeTurnStartedAt(now: number, elapsedMs: number | undefined): number {
  const elapsed = Math.max(0, elapsedMs ?? 0);
  return now - elapsed;
}

/** New/draft sessions and not-yet-resolved ids should stay on the landing page
 * instead of flashing the empty-chat chrome or a “continue session” hint. */
export function stayOnNewSessionLanding(input: {
  sessionId: string | null;
  activeSession?: { id: string; message_count?: number; project_hash?: string } | null;
}): boolean {
  if (!input.sessionId) return true;
  const active = input.activeSession;
  if (!active || active.id !== input.sessionId || !active.project_hash) return true;
  return (active.message_count ?? 0) === 0;
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
  const start = Math.max(0, messages.length - 8);
  for (let i = messages.length - 1; i >= start; i--) {
    const message = messages[i];
    if (message?.role === 'user' && canvasUserText(message) === userText) return true;
  }
  return false;
}

type InFlightPart = {
  kind: string;
  text?: string;
  tool?: { status?: string; name?: string };
};

/** Trailing assistant still has a running tool, or an empty shell waiting for tokens. */
export function transcriptHasInFlightAssistant(
  messages: Array<{ role: string; parts: InFlightPart[] }>,
): boolean {
  for (let i = messages.length - 1; i >= 0; i--) {
    const message = messages[i];
    if (message.role !== 'assistant') continue;
    if (message.parts.length === 0) return true;
    return message.parts.some((part) => {
      if (
        part.kind === 'tool' &&
        (part.tool?.status === 'pending' || part.tool?.status === 'waiting_approval')
      ) {
        return true;
      }
      return false;
    });
  }
  return false;
}

/** A successful `/live/message` receipt means the runtime accepted the input.
 *  Never roll the optimistic bubble back just because the tab's busy flag
 *  missed a `state.running` event (frozen TUI → WebUI looks idle). */
export function liveSubmitKeepsTurn(disposition: 'started' | 'steered'): boolean {
  return disposition === 'started' || disposition === 'steered';
}

/** Disk history is turn-boundary stale. Keep the tab's in-flight canvas. */
export function shouldKeepCachedTranscript(input: {
  cacheLen: number;
  diskLen: number;
  cacheInFlight: boolean;
  turnActive: boolean;
}): boolean {
  if (input.cacheLen <= 0) return false;
  if (input.turnActive && input.cacheInFlight) return true;
  if (input.cacheLen > input.diskLen) return true;
  return input.cacheInFlight && input.cacheLen >= input.diskLen;
}

/** This tab started the turn or is attached to its live stream. */
export function thisTabOwnsTurn(input: {
  isLiveSession: boolean;
  isLocalTurn: boolean;
}): boolean {
  return input.isLiveSession || input.isLocalTurn;
}

/** `/webui` + TUI share one live runtime. Until the snapshot binds
 * `liveSessionId`, treat the viewed session as ours so `/chat/active`
 * cannot lock the composer as a foreign occupant. */
export function liveSyncOwnsViewedSession(input: {
  sync: boolean;
  viewedSessionId: string;
  liveSessionId: string | null;
}): boolean {
  if (!input.sync) return false;
  return input.liveSessionId == null || input.liveSessionId === input.viewedSessionId;
}

/** Only a foreign `/chat` owner should lock the composer as “occupied”. */
export function shouldLockSendAsDetached(input: {
  turnActive: boolean;
  thisTabOwnsTurn: boolean;
}): boolean {
  return input.turnActive && !input.thisTabOwnsTurn;
}

/** Events that prove a turn is actually streaming. Idle `/chat/watch` must not
 * upgrade to busy on `runtime_info` / leftover `user` replay / tokens — that
 * painted a stop button and blinking cursor on already-finished sessions. */
export function isWatchTurnActivationEvent(type: string): boolean {
  switch (type) {
    case 'text':
    case 'reasoning':
    case 'tool_start':
    case 'tool_output':
    case 'tool_progress':
    case 'tool_result':
    case 'permission_request':
    case 'user_input_request':
      return true;
    default:
      return false;
  }
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

/** Detach from the live stream ownership (rare). View switches use
 * {@link liveSessionSwitchDisposition} instead and are always allowed. */
export function liveDetachDisposition(running: boolean): LiveDetachDisposition {
  return running ? { allowed: false, reason: 'active_turn' } : { allowed: true };
}

/** Switching the WebUI view never rebinds or cancels a CodingRuntime.
 * OpenCode model: selected SessionKey is a client ViewBinding only. */
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

/** TUI/live can answer or decline `request_user_input` without a matching
 * `user_input_resolved` on the `/chat` bus. The tool result is the close. */
export function toolResultClearsUserInput(name?: string): boolean {
  return name === 'request_user_input';
}

/** Latest `request_user_input` tool on the canvas already has a result
 * ("No answer was provided", a real answer, etc.). `/chat/pending` must
 * not resurrect the card while TUI keeps chatting. */
export function transcriptLatestUserInputIsResolved(
  messages: Array<{ role: string; parts: InFlightPart[] }>,
): boolean {
  for (let i = messages.length - 1; i >= 0; i--) {
    const message = messages[i];
    if (message.role !== 'assistant') continue;
    for (let j = message.parts.length - 1; j >= 0; j--) {
      const part = message.parts[j];
      if (part.kind !== 'tool' || part.tool?.name !== 'request_user_input') continue;
      const status = part.tool?.status;
      return status !== 'pending' && status !== 'waiting_approval';
    }
  }
  return false;
}

/** Tracks prefix-cache estimation across successive provider usage events. */
export type TokenCacheState = {
  /** Prompt tokens from the immediately prior LLM usage event. */
  lastPrompt: number;
  /** True once the provider has reported `cached > 0` (telemetry is trusted). */
  providerReportsCache: boolean;
  /** Sum of per-step prompts in the current user turn (industrial loop denominator). */
  turnPromptSum: number;
  /** Sum of per-step cache hits in the current user turn (industrial loop numerator). */
  turnCachedSum: number;
};

export function createTokenCacheState(): TokenCacheState {
  return { lastPrompt: 0, providerReportsCache: false, turnPromptSum: 0, turnCachedSum: 0 };
}

export function resetTokenCacheState(baselinePrompt = 0): TokenCacheState {
  return {
    lastPrompt: Math.max(0, baselinePrompt),
    providerReportsCache: false,
    turnPromptSum: 0,
    turnCachedSum: 0,
  };
}

/** Keep prefix baseline across a new user turn; zero the loop accumulators. */
export function startTokenTurn(state: TokenCacheState): TokenCacheState {
  return { ...state, turnPromptSum: 0, turnCachedSum: 0 };
}

/** Provider KV-cache block size. A partial trailing block does not count as a hit. */
export const CACHE_BLOCK_TOKENS = 64;

/**
 * Industrial prefix-cache estimate for one LLM step:
 *   cached_n ≈ prompt_{n-1}  (previous request is the reusable prefix)
 *   hit_n    = cached_n / prompt_n
 * A ≥10% prompt drop invalidates the key (compaction / rewrite).
 * Hits are floored to `CACHE_BLOCK_TOKENS` so a near-equal prompt cannot
 * paint a fake 100% from min(current, previous).
 */
export function estimatePrefixCached(currentPrompt: number, previousPrompt: number): number {
  if (currentPrompt <= 0 || previousPrompt <= 0) return 0;
  if (currentPrompt < previousPrompt * 0.9) return 0;
  const raw = Math.min(currentPrompt, previousPrompt);
  const aligned = Math.floor(raw / CACHE_BLOCK_TOKENS) * CACHE_BLOCK_TOKENS;
  if (aligned <= 0) return 0;
  // Estimated hits must stay strictly below the current prompt so the footer
  // cannot show 100% unless the provider reported a full-prefix hit.
  return aligned >= currentPrompt ? Math.max(0, aligned - CACHE_BLOCK_TOKENS) : aligned;
}

/** Cached tokens cannot exceed the prompt they were read from. */
export function clampCachedToPrompt(cached: number, prompt: number): number {
  if (cached <= 0 || prompt <= 0) return 0;
  return Math.min(cached, prompt);
}

/** Resolve cache telemetry for the footer pill. Provider `cached > 0` wins;
 * once a provider has reported cache hits we also trust explicit zeros;
 * otherwise fall back to prefix estimation against the prior usage prompt.
 * `prompt === 0` usage events are ignored so they cannot wipe `lastPrompt`
 * (that made the first round of a new turn show no cache). */
export function resolveTokenCache(
  event: { prompt: number; cached?: number },
  state: TokenCacheState,
): { cached: number; cached_estimated: boolean; nextState: TokenCacheState } {
  const prompt = Math.max(0, event.prompt);
  if (prompt <= 0) {
    return { cached: 0, cached_estimated: false, nextState: state };
  }
  const reported = event.cached != null ? Math.max(0, event.cached) : null;
  let providerReportsCache = state.providerReportsCache;
  if (reported != null && reported > 0) providerReportsCache = true;

  let cached = 0;
  let cached_estimated = false;

  if (reported != null && reported > 0) {
    cached = clampCachedToPrompt(reported, prompt);
  } else {
    // Missing or explicit 0: first-round providers often omit cache telemetry.
    // Estimate from the previous request prefix (industrial step n ≈ prompt_{n-1}).
    const estimated = estimatePrefixCached(prompt, state.lastPrompt);
    if (estimated > 0) {
      cached = estimated;
      cached_estimated = true;
    }
  }

  return {
    cached,
    cached_estimated,
    nextState: {
      lastPrompt: prompt,
      providerReportsCache,
      turnPromptSum: state.turnPromptSum + prompt,
      turnCachedSum: state.turnCachedSum + cached,
    },
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
      turnPromptSum: currentPrompt,
      turnCachedSum: cached,
    },
  };
}

/** Hit rate = cached / prompt (industrial single-step, or loop if sums are passed).
 * 100% is reserved for a real full-prefix provider hit — never from rounding
 * or from min(current, previous) estimates. */
export function formatCacheHitRate(
  cached: number,
  prompt: number,
  estimated = false,
): string | null {
  if (cached <= 0 || prompt <= 0) return null;
  const ratio = (cached / prompt) * 100;
  if (cached >= prompt) return estimated ? '99%' : '100%';
  if (estimated && ratio >= 99) return '99%';
  if (ratio >= 99.5) return '99%';
  if (ratio >= 10) return `${Math.round(ratio)}%`;
  return `${ratio.toFixed(1)}%`;
}

/**
 * Detect persisted footer usage that is turn-cumulative billing rather than
 * last-request occupancy. Older daemons summed every LLM round's prompt/cache
 * into `token_usage`, so a restart painted e.g. 1.7M/1.0M (100% cache) until
 * the next live Usage event replaced it.
 */
export function isStackedTurnBillingUsage(
  usage: { prompt?: number; cached?: number } | null | undefined,
  contextLimit?: number | null,
): boolean {
  if (!usage || !contextLimit || contextLimit <= 0) return false;
  const prompt = usage.prompt ?? 0;
  const cached = usage.cached ?? 0;
  if (prompt <= contextLimit) return false;
  return cached >= prompt * 0.9;
}
