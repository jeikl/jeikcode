//! 本地 webui 一次性 token 存储与鉴权中间件。
//!
//! Phase 1：token 随 `/webui` 启动生成，仅存内存、随进程退出失效。
//! Phase 2（官方中转隧道）会把账号 token 接入同一条 `is_valid` 校验链——
//! 故鉴权统一收口在本模块，路由层只调中间件。

use axum::{
    extract::State,
    http::{
        header::{AUTHORIZATION, COOKIE},
        StatusCode,
    },
    middleware::Next,
    response::Response,
};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// `X-Atom-User-Id` 请求头名，App 端通过中继透传桌面端进行双向校验。
const APP_USER_ID_HEADER: &str = "x-atom-user-id";

/// Name of the HttpOnly cookie that carries the webui token after the
/// `/?token=` handoff (see `serve_webui_index` in lib.rs). Keeping the
/// credential in an HttpOnly cookie — rather than the URL — prevents a
/// malicious browser extension from reading it off `location.search`
/// (CWE-598 / CWE-522).
/// Base cookie name. Intentionally `pub(crate)` and NOT used directly as a
/// cookie name anywhere — the real per-instance name is [`webui_cookie_name`]
/// (port-scoped). Reading/writing the bare base name would 401 against a
/// port-scoped instance, so it's kept crate-private to prevent that footgun.
pub(crate) const WEBUI_COOKIE: &str = "atomcode_webui";

/// Per-instance webui cookie name: `atomcode_webui_<port>`.
///
/// Cookies on `localhost` ignore the PORT (RFC 6265 — port is not part of a
/// cookie's identity), so the bare `atomcode_webui` name was SHARED across every
/// localhost port. A second `/webui` instance's `/?token=` handoff therefore
/// overwrote the first page's cookie in the shared jar, and the first page's
/// (now-wrong-token) requests all 401'd. Scoping the name by the instance's
/// actual bound port keeps each instance's cookie distinct, so they coexist.
pub fn webui_cookie_name(port: u16) -> String {
    format!("{WEBUI_COOKIE}_{port}")
}

/// 进程内有效 webui token 集合。线程安全，可放进 `AppState`。
#[derive(Clone, Default)]
pub struct WebuiTokenStore {
    inner: Arc<RwLock<HashSet<String>>>,
}

impl WebuiTokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 生成并登记一个新 token，返回其字符串。
    pub fn mint(&self) -> String {
        let token = Uuid::new_v4().simple().to_string();
        self.inner.write().unwrap().insert(token.clone());
        token
    }

    /// 登记一个调用方指定的固定 token（`atomcode serve --token …`）。
    ///
    /// 空串 / 纯空白被拒绝并返回 `false`。已存在的 token 再次 register 仍返回 `true`
    ///（幂等）。客户端把同一字符串当作 OpenAI `api_key` / Anthropic `api_key`
    ///（`Authorization: Bearer` 或 `x-api-key` / `api-key`）。
    pub fn register(&self, token: &str) -> bool {
        let token = token.trim();
        if token.is_empty() {
            return false;
        }
        self.inner.write().unwrap().insert(token.to_string());
        true
    }

    /// 校验 token 是否有效。空串始终无效。
    pub fn is_valid(&self, token: &str) -> bool {
        if token.is_empty() {
            return false;
        }
        self.inner.read().unwrap().contains(token)
    }
}

/// 从 `Authorization` 头解析 Bearer token（大小写不敏感前缀）。
///
/// - OpenAI 官方 SDK：`Authorization: Bearer <api_key>`
/// - 部分网关 / 代理：裸 `Authorization: <api_key>`（无 scheme）
pub fn token_from_header(value: Option<&str>) -> Option<String> {
    let v = value?;
    let rest = v
        .strip_prefix("Bearer ")
        .or_else(|| v.strip_prefix("bearer "))
        // Some clients send the raw key without a scheme.
        .or_else(|| if v.contains(' ') { None } else { Some(v) })?;
    let rest = rest.trim();
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

/// 从裸 API-key 类请求头解析 token（值即 key 本身）。
///
/// 用于：
/// - OpenAI / Azure：`api-key: <key>`
/// - Anthropic 官方 SDK：`x-api-key: <key>`
///
/// HTTP 层会折叠 header 名大小写；本函数只处理**值**。
pub fn token_from_api_key_header(value: Option<&str>) -> Option<String> {
    let v = value?.trim();
    if v.is_empty() {
        None
    } else {
        Some(v.to_string())
    }
}

/// Anthropic 官方 SDK 形：`x-api-key: <api_key>`（别名，语义同 [`token_from_api_key_header`]）。
#[inline]
pub fn token_from_x_api_key_header(value: Option<&str>) -> Option<String> {
    token_from_api_key_header(value)
}

/// 从 `Cookie` 头解析 `atomcode_webui=<token>`。Cookie 是 `/?token=` 交接后
/// 凭证的主要载体（HttpOnly，前端 JS / 浏览器插件读不到），EventSource/同源
/// fetch 会自动携带。空值视为无。
pub fn token_from_cookie(value: Option<&str>, cookie_name: &str) -> Option<String> {
    let v = value?;
    let prefix = format!("{cookie_name}=");
    for pair in v.split(';') {
        let pair = pair.trim();
        if let Some(tok) = pair.strip_prefix(&prefix) {
            let tok = tok.trim();
            if !tok.is_empty() {
                return Some(tok.to_string());
            }
        }
    }
    None
}

/// Axum 中间件：校验请求是否携带有效的 serve / webui access token。
///
/// 接受（按优先级）：
/// 1. `Authorization: Bearer <token>` — OpenAI SDK / curl / 部分 Anthropic 网关
/// 2. `x-api-key: <token>` — Anthropic 官方 SDK
/// 3. `api-key: <token>` — Azure OpenAI / 部分 OpenAI 兼容客户端
/// 4. HttpOnly webui cookie（`/?token=` 交接后）
///
/// 签名与 `activity_tracker_middleware`（同 crate）保持一致——
/// `axum::extract::Request` + `axum::middleware::Next`，适用于 axum 0.7。
/// 路由层通过 `axum::middleware::from_fn_with_state` 挂载（Task 8）。
pub async fn require_webui_token(
    State(state): State<crate::AppState>,
    req: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if !state.enforce_token {
        // 独立 daemon / VSCode 实例：不强制 token，保持原行为。
        return Ok(next.run(req).await);
    }
    let header = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok());
    // Anthropic: x-api-key；OpenAI Azure: api-key
    let x_api_key = req.headers().get("x-api-key").and_then(|h| h.to_str().ok());
    let api_key = req.headers().get("api-key").and_then(|h| h.to_str().ok());
    let cookie = req.headers().get(COOKIE).and_then(|h| h.to_str().ok());
    // Read THIS instance's port-scoped cookie name so a sibling `/webui` on a
    // different localhost port (which shares the cookie jar) can't shadow us.
    let token = token_from_header(header)
        .or_else(|| token_from_x_api_key_header(x_api_key))
        .or_else(|| token_from_api_key_header(api_key))
        .or_else(|| token_from_cookie(cookie, &state.webui_cookie_name));
    match token {
        Some(tok) if state.webui_tokens.is_valid(&tok) => Ok(next.run(req).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Axum 中间件：校验 `X-Atom-User-Id` 请求头是否与桌面端当前登录账号一致。
///
/// 仅 `/app` 模式下启用（`state.app_user_id` 非空），webui / 独立 daemon 不受影响。
/// App 端经中继透传此头（中继只过滤 `x-atom-token`，`x-atom-user-id` 原样通过），
/// 桌面 daemon 借此验证请求确实来自同一账号的手机客户端。
pub async fn require_app_user_id(
    State(state): State<crate::AppState>,
    req: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let expected = &state.app_user_id;
    if expected.is_empty() {
        // app_user_id 未设置（非 /app 模式 / 桌面未登录）：不校验，放行。
        return Ok(next.run(req).await);
    }

    // 从请求头读取 App 端传来的 user_id
    let actual = req
        .headers()
        .get(APP_USER_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if actual == *expected {
        Ok(next.run(req).await)
    } else {
        tracing::warn!(
            "app user_id mismatch: expected={:?}, actual={:?}",
            expected,
            actual,
        );
        Err(StatusCode::UNAUTHORIZED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bearer_token() {
        assert_eq!(
            token_from_header(Some("Bearer abc123")),
            Some("abc123".to_string())
        );
        assert_eq!(
            token_from_header(Some("bearer abc123")),
            Some("abc123".to_string())
        );
        // OpenAI-style raw api_key without scheme (some proxies).
        assert_eq!(
            token_from_header(Some("sk-fixed-token")),
            Some("sk-fixed-token".to_string())
        );
        assert_eq!(token_from_header(Some("Basic x")), None);
        assert_eq!(token_from_header(None), None);
    }

    #[test]
    fn extracts_api_key_header() {
        assert_eq!(
            token_from_api_key_header(Some("  my-secret  ")),
            Some("my-secret".to_string())
        );
        assert_eq!(token_from_api_key_header(Some("")), None);
        assert_eq!(token_from_api_key_header(None), None);
    }

    #[test]
    fn extracts_anthropic_x_api_key_header() {
        // Anthropic official SDK sends x-api-key (same value parsing as api-key).
        assert_eq!(
            token_from_x_api_key_header(Some("sk-ant-fixed")),
            Some("sk-ant-fixed".to_string())
        );
        assert_eq!(
            token_from_x_api_key_header(Some("  sk-ant-fixed  ")),
            Some("sk-ant-fixed".to_string())
        );
        assert_eq!(token_from_x_api_key_header(Some("")), None);
        assert_eq!(token_from_x_api_key_header(None), None);
    }

    #[test]
    fn mints_and_validates_token() {
        let store = WebuiTokenStore::new();
        let tok = store.mint();
        assert!(store.is_valid(&tok), "freshly minted token must validate");
    }

    #[test]
    fn registers_fixed_token() {
        let store = WebuiTokenStore::new();
        assert!(store.register("sk-my-fixed-key"));
        assert!(store.is_valid("sk-my-fixed-key"));
        assert!(store.register("sk-my-fixed-key"), "register is idempotent");
        assert!(!store.register(""));
        assert!(!store.register("   "));
        assert!(!store.is_valid("other"));
    }

    #[test]
    fn rejects_unknown_token() {
        let store = WebuiTokenStore::new();
        store.mint();
        assert!(!store.is_valid("not-a-real-token"));
        assert!(!store.is_valid(""));
    }

    #[test]
    fn mint_is_unique() {
        let store = WebuiTokenStore::new();
        assert_ne!(store.mint(), store.mint());
    }

    #[test]
    fn extracts_token_from_cookie() {
        assert_eq!(
            token_from_cookie(Some("atomcode_webui=abc123"), "atomcode_webui"),
            Some("abc123".to_string())
        );
        // Among other cookies, any order.
        assert_eq!(
            token_from_cookie(
                Some("foo=1; atomcode_webui=abc123; bar=2"),
                "atomcode_webui"
            ),
            Some("abc123".to_string())
        );
        assert_eq!(
            token_from_cookie(Some("bar=2; atomcode_webui=xyz"), "atomcode_webui"),
            Some("xyz".to_string())
        );
    }

    #[test]
    fn cookie_without_token_is_none() {
        assert_eq!(token_from_cookie(None, "atomcode_webui"), None);
        assert_eq!(token_from_cookie(Some(""), "atomcode_webui"), None);
        assert_eq!(
            token_from_cookie(Some("foo=1; bar=2"), "atomcode_webui"),
            None
        );
        assert_eq!(
            token_from_cookie(Some("atomcode_webui="), "atomcode_webui"),
            None
        );
        assert_eq!(
            token_from_cookie(Some("atomcode_webui= "), "atomcode_webui"),
            None
        );
        // Must not match a different cookie that merely ends with the name.
        assert_eq!(
            token_from_cookie(Some("x_atomcode_webui=nope"), "atomcode_webui"),
            None
        );
    }

    #[test]
    fn cookie_name_is_port_scoped() {
        assert_eq!(webui_cookie_name(54321), "atomcode_webui_54321");
        assert_ne!(
            webui_cookie_name(1111),
            webui_cookie_name(2222),
            "different ports must yield different cookie names"
        );
    }

    #[test]
    fn two_instances_on_different_ports_do_not_collide() {
        // The bug: the browser's shared localhost cookie jar holds BOTH instances'
        // cookies (cookies ignore the port). Each server must read ONLY its own
        // port-scoped name, so instance A still sees token A even after B set B.
        let jar = "atomcode_webui_1111=tokenA; atomcode_webui_2222=tokenB";
        assert_eq!(
            token_from_cookie(Some(jar), &webui_cookie_name(1111)),
            Some("tokenA".to_string()),
            "instance on port 1111 reads its own cookie, not the other's"
        );
        assert_eq!(
            token_from_cookie(Some(jar), &webui_cookie_name(2222)),
            Some("tokenB".to_string()),
            "instance on port 2222 reads its own cookie"
        );
        // A third instance whose cookie was never set sees nothing (→ 401, correct).
        assert_eq!(token_from_cookie(Some(jar), &webui_cookie_name(3333)), None);
    }
}
