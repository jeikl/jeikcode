/**
 * UUID v4 for request / correlation ids.
 *
 * `crypto.randomUUID()` is only defined in **secure contexts** (HTTPS or
 * localhost). `atomcode serve` is plain HTTP on a LAN IP for remote clients
 * (phones / other PCs), so calling `crypto.randomUUID` there throws
 * `TypeError: crypto.randomUUID is not a function` and the send path aborts.
 *
 * Capture any *native* implementation at module load (before we polyfill),
 * then fall back to `getRandomValues` / Math.random. Never call through
 * `crypto.randomUUID` after polyfilling — that would recurse forever.
 */

/** Browser-native impl, if present at load time (secure contexts only). */
const nativeRandomUUID: (() => string) | null = (() => {
  try {
    const c = globalThis.crypto;
    if (c && typeof c.randomUUID === 'function') {
      return c.randomUUID.bind(c);
    }
  } catch {
    /* ignore */
  }
  return null;
})();

function uuidFromRandomValues(): string {
  const c = globalThis.crypto;
  if (c && typeof c.getRandomValues === 'function') {
    const bytes = new Uint8Array(16);
    c.getRandomValues(bytes);
    // RFC 4122 version 4 + variant 10xx
    bytes[6] = (bytes[6]! & 0x0f) | 0x40;
    bytes[8] = (bytes[8]! & 0x3f) | 0x80;
    const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
    return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
  }
  return 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, (ch) => {
    const r = (Math.random() * 16) | 0;
    const v = ch === 'x' ? r : (r & 0x3) | 0x8;
    return v.toString(16);
  });
}

export function randomUUID(): string {
  if (nativeRandomUUID) return nativeRandomUUID();
  return uuidFromRandomValues();
}

/** Install `crypto.randomUUID` when missing (non-secure HTTP / LAN IP). */
export function ensureRandomUUIDPolyfill(): void {
  if (nativeRandomUUID) return;
  const c = globalThis.crypto;
  if (!c || typeof c.randomUUID === 'function') return;
  try {
    // Point at the fallback generator — NOT `randomUUID`, which would re-enter
    // via crypto.randomUUID after we install this property.
    Object.defineProperty(c, 'randomUUID', {
      value: uuidFromRandomValues,
      configurable: true,
      writable: true,
    });
  } catch {
    // Some environments freeze crypto; callers should use randomUUID() directly.
  }
}
