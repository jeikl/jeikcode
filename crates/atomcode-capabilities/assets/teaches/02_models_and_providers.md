# 02 - 模型、提供商与思考档位配置指南 (Models & Providers)

全局配置文件路径：`~/.atomcode/config.toml`（或 `$ATOMCODE_HOME/config.toml`）。

---

## 1. TOML 顶层语法铁律（⚠️ 极度重要）

1. **顶层标量必须置于文件最前**：
   `default_model`、`default_provider`、`language`、`auto_update`、`auto_commit` 等顶层键，**必须位于文件最顶部（任何 `[table]` 表段之前）**。
   > ❌ **常见错误**：写在 `[models.xxx]` 或 `[providers.xxx]` 下方，会被 TOML 误解析为该表的子属性，导致默认模型失效！
2. **增量保留与合并**：
   修改 `config.toml` 时，应增量追加或更新指定项，严禁覆盖或清除用户已配置好的其他模型与 API Key。
3. **编码规范**：必须以 **UTF-8（无 BOM）** 格式保存。

---

## 2. 推荐解耦架构（`provider_accounts` 账号 + `models` 模型）

**优势**：同一账号的 API 密钥 / Base URL 仅需配置一次，可挂载多款不同模型（Chat、Reasoner、Fast 档位）。

```toml
# =============================================================================
# 顶层全局默认选择（必须在最顶部）
# =============================================================================
default_model = "deepseek/chat"
language = "zh-CN"

# =============================================================================
# 1. 账号连接定义 [provider_accounts.<account_id>]
# =============================================================================
[provider_accounts.deepseek]
provider = "deepseek"                       # 内置预设 ID (deepseek, zhipu, aliyun, openai 等)
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxx"
# base_url = "https://api.deepseek.com/v1" # 内置预设自带默认值，也可在此显式覆盖

[provider_accounts.custom-proxy]
provider = "openai-compatible"              # 自定义 OpenAI 兼容中转/网关协议
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxx"
base_url = "https://api.your-proxy.com/v1"

# =============================================================================
# 2. 模型档案定义 [models."<account_id>/<model_alias>"]
# =============================================================================
[models."deepseek/chat"]
account = "deepseek"                        # 关联 provider_accounts
model = "deepseek-chat"                     # 真实模型 ID
context_window = 64000                      # 上下文窗口 Token 数
max_tokens = 8192                           # 最大输出 Token

[models."deepseek/reasoner"]
account = "deepseek"
model = "deepseek-reasoner"
context_window = 64000
reasoning_model = true                      # 声明为推理/思考模型
reasoning_history = "exclude"               # 推理思考是否回传："include" | "exclude"
reasoning_effort = "high"                   # 思考强度："low" | "medium" | "high" | "max"
reasoning_levels = ["low", "medium", "high", "max"] # Ctrl+T 循环切换档位列表

[models."custom/gpt4o"]
account = "custom-proxy"
model = "gpt-4o"
context_window = 128000
image_input = true                          # 开启图片直接输入（多模态视觉）
```

---

## 3. 内置预设提供商 ID 与通信协议

### 3.1 内置预设 ID (`provider` / `type`)

| 预设 ID | 厂商 / 显示名 | 协议类型 | 默认 Base URL | 默认环境变量 Key |
| :--- | :--- | :--- | :--- | :--- |
| `deepseek` | DeepSeek 官方 | OpenAI 兼容 | `https://api.deepseek.com/v1` | `DEEPSEEK_API_KEY` |
| `zhipu` | 智谱 AI (GLM) | OpenAI 兼容 | `https://open.bigmodel.cn/api/paas/v4` | `ZHIPUAI_API_KEY` |
| `aliyun` | 阿里百炼 (DashScope) | OpenAI 兼容 | `https://dashscope.aliyuncs.com/compatible-mode/v1` | `DASHSCOPE_API_KEY` |
| `siliconflow` | 硅基流动 SiliconFlow | OpenAI 兼容 | `https://api.siliconflow.cn/v1` | `SILICONFLOW_API_KEY` |
| `volcengine` | 火山引擎 Ark (字节) | OpenAI 兼容 | `https://ark.cn-beijing.volces.com/api/v3` | `ARK_API_KEY` |
| `moonshot` | 月之暗面 Kimi | OpenAI 兼容 | `https://api.moonshot.cn/v1` | `MOONSHOT_API_KEY` |
| `minimax` | MiniMax (海螺) | OpenAI 兼容 | `https://api.minimaxi.com/v1` | `MINIMAX_API_KEY` |
| `openrouter` | OpenRouter | OpenAI 兼容 | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` |
| `openai` | OpenAI 官方 | OpenAI 兼容 | `https://api.openai.com/v1` | `OPENAI_API_KEY` |
| `anthropic` | Anthropic Claude 官方 | Anthropic Messages | `https://api.anthropic.com` | `ANTHROPIC_API_KEY` |
| `ollama` | 本地 Ollama | Ollama 原生 | `http://localhost:11434` | (无需密钥) |
| `atomgit` | AtomGit | OpenAI 兼容 | `https://llm-api.atomgit.com/v1` | (专属网关) |
| `taotoken` | TaoToken | OpenAI 兼容 | `https://taotoken.net/api/v1` | - |
| `xiaomi-mimo` | 小米 MiMo | OpenAI 兼容 | - | - |

### 3.2 自定义底层协议类型
- `openai-compatible` / `openai`：标准 OpenAI Chat Completions（`POST /chat/completions`）。
- `responses-compatible` / `responses` / `openai-responses`：OpenAI Responses 新协议（`POST /v1/responses`）。
- `anthropic-compatible` / `anthropic` / `claude`：Anthropic Messages 协议（`POST /v1/messages`）。
- `ollama`：Ollama 本地原生流式接口（`POST /api/chat`）。

---

## 4. 关键高频核心参数详解

| 参数名 | 类型 | 适用场景 | 详细说明 |
| :--- | :--- | :--- | :--- |
| `context_window` | 整数 | 所有模型 | **上下文总窗口 Token 限制**（如 64000, 128000, 1000000）。当接近阈值时自动触发平滑压缩。 |
| `max_tokens` | 整数 | 所有模型 | **单轮输出最大 Token 预算**（如 8192, 16384, 131072）。 |
| `reasoning_model` | 布尔值 | 推理模型 | 声明是否为深度思考模型（DeepSeek R1/V3/V4, GLM-5.2, Kimi K2 等）。 |
| **`reasoning_history`** | 字符串 | 推理模型 | **思考过程是否在多轮历史中回传**：<br>• `"exclude"`（推荐）：多轮交互时不把上一轮的 `<think>` 发回后端，大幅节省 Token 且防止某些模型报错。<br>• `"include"`：保留并回传思考历史（部分特殊 API 强制要求）。 |
| **`reasoning_effort`** | 字符串 | 思考模型 | **推理深度档位**：`"low"` \| `"medium"` \| `"high"` \| `"max"`。 |
| **`reasoning_levels`** | 数组 | 思考模型 | TUI 界面中通过快捷键 `Ctrl+T` 快速循环切换思考强度的候选列表，例如 `["low", "medium", "high", "max"]`。 |
| `thinking_enabled` | 布尔值 | Claude | 是否开启 Claude 3.7+ 扩展思考模式。 |
| `thinking_budget` | 整数 | Claude | Claude 思考 Token 预算上限（默认 10000）。 |
| `image_input` | 布尔值 | 视觉模型 | 别名 `supports_vision`。设为 `true` 时支持图片直接贴入或由 `read_file` 返回图像。 |
| `vision_preprocessor_provider` | 字符串 | 顶层全局 | **视觉预处理代答**：当主模型为纯文本模型时，粘贴图片将自动转给指定的 VL 视觉模型进行 OCR 提取。 |
| `skip_tls_verify` | 布尔值 | 账号/模型 | 内网自签证书环境下跳过 TLS 证书校验。 |

---

## 5. 经典单表直连架构（`[providers.<id>]`）

除推荐解耦架构外，亦支持单表直连模式：

```toml
[providers.glm5]
type = "zhipu"
api_key = "xxxxxxxxxxxxxxxx.xxxxxxxx"
model = "glm-5.2"
base_url = "https://open.bigmodel.cn/api/paas/v4"
context_window = 1000000
max_tokens = 131072
reasoning_model = true
reasoning_history = "exclude"
```
