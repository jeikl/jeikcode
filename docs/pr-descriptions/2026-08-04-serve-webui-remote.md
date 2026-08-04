# serve / WebUI 远程能力 — 发布与审查说明

> 对应 MR：serve/webui 多实例远程 + 局域网修复（叠在 image_input 前置 PR 上）。

## 相对官方 main 的能力

1. **`atomcode serve` / `attach`**：自定义 host/port/项目目录，多实例并行；默认 token，可选 `--no-token`。
2. **局域网 WebUI 可用**：鉴权双通道 + 私网 CORS + 非 secure context UUID。
3. **WebUI UX**：思考过程块、子代理并行面板、路径去 `\\?\`、启动连接信息沉底。

## 关于 `image_input`（前置 PR，非本 commit 引入）

审查机器人常把「openai/claude/anthropic 未设置时默认 `image_input = false`」标成破坏性事故。这是 **有意设计**，不是回归：

| 旧行为（模型名启发式） | 新行为（配置驱动） |
|------------------------|--------------------|
| 名含 gpt-4o / vl 等 → 猜能接图 | 显式 `image_input = true` 才直发 base64 |
| 自定义网关名不含关键字 → 只发 `[Image #N]`（bug） | 任意模型名只要打开开关即可多模态 |
| 纯文本兼容网关可能误收图 | 默认关，避免 400 |

升级后若需要贴图直发本模型，请：

```toml
# [providers.xxx] 或 [models.xxx]
image_input = true
```

或在 `/provider` 打开「支持图片输入」。贴图门控在未配置 vision/VL 时会拒绝并提示，而非静默吞图。详见 `docs/config.example.toml` 迁移说明。

## Token URL

serve 启动 banner 提示：完整 `?token=` 链接等同口令，勿分享。浏览器会在加载后用 `history.replaceState` 清地址栏；鉴权同时保留 HttpOnly cookie + Bearer。

## 建议自测（最短）

```bash
cargo test -p atomcode-daemon --lib cors_
cd webui && npx tsx --test src/lib/displayPath.test.ts src/lib/randomId.test.ts src/lib/subtasks.test.ts

atomcode serve --host 0.0.0.0 --port 4096
# 局域网设备打开 remote URL → 发消息有回复
# task 多 explore → 并排进度；思考模型 → 思考块
```
