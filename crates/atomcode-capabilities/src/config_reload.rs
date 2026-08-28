//! Process-wide request that `config.toml` / MCP / skills should be remounted.
//!
//! Set by the `jeikcode_config_reload` tool (L1). Consumed by drivers at an idle
//! boundary: TUI capability reload, daemon MCP cache rebuild, live
//! `reload_capabilities`. Lives outside the `tools` feature so daemon/TUI can
//! observe the flag without pulling the full toolset.

use std::sync::atomic::{AtomicBool, Ordering};

static PENDING_LIVE: AtomicBool = AtomicBool::new(false);
static PENDING_MCP_CACHE: AtomicBool = AtomicBool::new(false);

/// Mark that `config.toml` / `mcp.json` / skills should be remounted after this turn.
pub fn request_config_reload() {
    PENDING_LIVE.store(true, Ordering::Release);
    PENDING_MCP_CACHE.store(true, Ordering::Release);
}

/// Consume the live-runtime reprepare request (TUI idle / daemon TurnFinished).
pub fn take_pending_live_reload() -> bool {
    PENDING_LIVE.swap(false, Ordering::AcqRel)
}

/// Consume the daemon MCP-registry cache rebuild request (`/mcp/status`, `/chat`).
pub fn take_pending_mcp_cache_reload() -> bool {
    PENDING_MCP_CACHE.swap(false, Ordering::AcqRel)
}

#[cfg(test)]
pub fn pending_config_reload() -> bool {
    PENDING_LIVE.load(Ordering::Acquire) || PENDING_MCP_CACHE.load(Ordering::Acquire)
}

#[cfg(test)]
pub fn clear_pending_config_reload() {
    PENDING_LIVE.store(false, Ordering::Release);
    PENDING_MCP_CACHE.store(false, Ordering::Release);
}
