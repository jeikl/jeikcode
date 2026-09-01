# JeikCode 配置文件（config.toml）Agent 指南与配置教程

> **目标受众**：AI Agent / 自动化配置脚本 / 开发者。
> **文档用途**：当用户请求配置模型、添加 Provider、配置代理或调整运行时参数时，Agent 必须严格依据本文档中的真实代码 Schema、内置预设库与协议规范，安全且精准地读取或修改 `~/.atomcode/config.toml`。

---

## 1. 配置文件定位与核心语法约束

### 1.1 文件路径
- **默认全局配置路径**：`~/.atomcode/config.toml`
  - Windows: `%USERPROFILE%\.atomcode\config.toml`（例如 `C:\Users\<username>\.atomcode\config.toml`）
  - Linux / macOS: `~/.atomcode/config.toml`（例如 `/home/<username>/.atomcode/config.toml`）
- **环境变量覆盖**：若设置了 `ATOMCODE_HOME`，优先读取 `$ATOMCODE_HOME/config.toml`。

### 1.2 TOML 顶层键关键规则（⚠️ 极度重要）
1. **顶层标量必须置于最前**：
   `default_model`、`default_provider`、`language`、`auto_update`、`auto_commit`、`keep_interrupted_context`、`offline_mode` 等顶层键，**必须位于文件最顶部（任何 `[table]` 表段之前）**。
   > ❌ **错误示范**：写在 `[providers.xxx]` 或 `[models.xxx]` 之后，会被 TOML 误解析为该子表的属性，导致全局默认配置失效。
2. **增量保留与合并**：
   Agent 修改 `config.toml` 时，应增量追加或更新指定项，**严禁误删用户已配置好的其他模型、账号或自定义配置**。
3. **文件编码**：必须以 **UTF-8（无 BOM）** 保存。

---

## 2. 提供商体系与底层通信协议

JeikCode 采用分层解耦的 Provider 体系，支持 **内置预设提供商** 与 **底层自定义协议提供商**：

### 2.1 内置预设提供商列表（Built-in Presets）
直接引用内置提供商 ID 时，系统会自动填充默认的 `base_url`、环境变量后备与协议映射：

| 预设 ID (`provider` / `type`) | 厂商名称 / 显示名 | 协议类型 | 默认 Base URL | 默认环境变量 Key |
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
| `taotoken` | TaoToken | OpenAI 兼容 | `https://taotoken.net/api/v1` | - |
| `xiaomi-mimo` | 小米 MiMo | OpenAI 兼容 | - | - |

---

### 2.2 自定义提供商底层协议（Custom Protocols）
对于自建网关、中转站或未在内置列表中的供应商，可通过以下标准协议接入：
1. **`openai-compatible` / `openai`**：标准 OpenAI Chat Completions 协议（`POST /chat/completions`）。
2. **`responses-compatible` / `responses` / `openai-responses`**：OpenAI 新一代 Responses 协议（`POST /v1/responses`）。
3. **`anthropic-compatible` / `anthropic` / `claude`**：Anthropic Messages 协议（`POST /v1/messages`）。
4. **`ollama`**：Ollama 本地原生流式接口（`POST /api/chat`）。

---

## 3. 配置文件支持的两种组织结构

系统无缝兼容两种配置方式，Agent 可按需生成或解析：

### 结构 A：解耦推荐架构（`provider_accounts` 账号 + `models` 模型）
**优势**：同一账号的 API Key / Base URL 仅需配置一次，可挂载多个不同模型（如 Chat、Reasoner、Fast 档位）。

```toml
# 顶层默认选择
default_model = "deepseek/chat"
language = "zh-CN"

# -----------------------------------------------------------------------------
# 1. 账号连接与凭据定义 [provider_accounts.<account_id>]
# -----------------------------------------------------------------------------
[provider_accounts.deepseek]
provider = "deepseek"                       # 可填内置预设 ID (如 deepseek, zhipu, aliyun, openai) 或 openai-compatible
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxx"
# base_url = "https://api.deepseek.com/v1" # 若填了内置预设则默认自带，也可在此显式覆盖

[provider_accounts.my-custom-proxy]
provider = "openai-compatible"              # 自定义 OpenAI 兼容中转/网关
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxx"
base_url = "https://api.your-proxy.com/v1"

# -----------------------------------------------------------------------------
# 2. 模型档案定义 [models."<account_id>/<model_alias>"]
# -----------------------------------------------------------------------------
[models."deepseek/chat"]
account = "deepseek"                        # 关联对应的 provider_account
model = "deepseek-chat"                     # 真实发往后端的模型代号
context_window = 64000                      # 上下文窗口 Token 数
max_tokens = 8192

[models."deepseek/reasoner"]
account = "deepseek"
model = "deepseek-reasoner"
context_window = 64000
reasoning_model = true                      # 声明为推理/思考模型
reasoning_history = "exclude"               # 推理历史回显策略："include" | "exclude"

[models."custom/gpt4o"]
account = "my-custom-proxy"
model = "gpt-4o"
context_window = 128000
image_input = true                          # 开启图片直接输入（多模态支持）
```

---

### 结构 B：经典直连架构（`providers.<id>` 单表模式）
**优势**：单表直连，开箱即用。

```toml
# 顶层默认选择
default_provider = "deepseek"
language = "zh-CN"

# -----------------------------------------------------------------------------
# [providers.<id>] 直连配置
# -----------------------------------------------------------------------------
# 示例 1: DeepSeek 官方
[providers.deepseek]
type = "openai"                             # 或填 "deepseek"
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxx"
model = "deepseek-chat"
base_url = "https://api.deepseek.com/v1"
context_window = 64000
max_tokens = 8192

# 示例 2: 智谱 GLM-5.2
[providers.glm]
type = "openai"                             # 或填 "zhipu"
api_key = "xxxxxxxxxxxxxxxx.xxxxxxxx"
model = "glm-5.2"
base_url = "https://open.bigmodel.cn/api/paas/v4"
context_window = 1000000
max_tokens = 131072

# 示例 3: 阿里百炼通义千问 (DashScope)
[providers.qwen]
type = "openai"                             # 或填 "aliyun"
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxx"
model = "qwen-max"
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"
context_window = 131072

# 示例 4: 硅基流动 SiliconFlow
[providers.siliconflow]
type = "openai"                             # 或填 "siliconflow"
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxx"
model = "deepseek-ai/DeepSeek-V3"
base_url = "https://api.siliconflow.cn/v1"
context_window = 64000

# 示例 5: Anthropic Claude 原生
[providers.claude]
type = "claude"                             # 或 "anthropic"
api_key = "sk-ant-xxxxxxxxxxxxxxxxxxxx"
model = "claude-3-7-sonnet-20250219"
base_url = "https://api.anthropic.com/v1"
context_window = 200000
image_input = true

# 示例 6: OpenAI Responses 新协议
[providers.openai-resp]
type = "responses"                          # 或 "openai-responses" / "responses-compatible"
api_key = "sk-proj-xxxxxxxxxxxxxxxxxxxx"
model = "gpt-4o"
base_url = "https://api.openai.com/v1"
context_window = 128000

# 示例 7: 本地 Ollama (无须 API Key)
[providers.ollama]
type = "ollama"
model = "qwen2.5-coder:14b"
base_url = "http://localhost:11434"
context_window = 32768
```

---

## 4. 关键可选参数与高级微调

在 `[models.*]` 或 `[providers.*]` 中可按需添加以下控制项：

- `image_input` (布尔值，别名 `supports_vision`):
  是否支持图片直接输入。对多模态大模型设为 `true`；纯文本模型建议设为 `false`（默认）。
- `reasoning_model` (布尔值):
  是否为思考/推理大模型。
- `reasoning_history` (`"include"` | `"exclude"`):
  推理过程历史回显策略（DeepSeek R1/V3 通常使用 `"exclude"`，Kimi K2 / DeepSeek V4 thinking 常用 `"include"`）。
- `reasoning_effort` (`"low"` | `"medium"` | `"high"` | `"max"`):
  设定推理深度/思考强度档位。
- `reasoning_levels` (字符串数组，例如 `["low", "medium", "high", "max"]`):
  TUI 中使用 `Ctrl+T` 循环切换思考强度的可用档位列表。
- `thinking_enabled` (布尔值，适用于 Claude):
  是否开启 Claude 扩展思考模式。
- `thinking_budget` (整数，默认 10000):
  Claude 思考 Token 预算上限。
- `skip_tls_verify` (布尔值，默认 false):
  内网或自签证书环境下跳过 TLS 证书检查。

---

## 5. 全局系统与工具链配置

```toml
# =============================================================================
# 全局设置（顶层）
# =============================================================================
language = "zh-CN"              # 界面语言："zh-CN" | "en"
auto_update = false             # 自动无感后台更新（true=开启, false=关闭）
auto_update_mins = 30           # 自动更新轮询间隔（单位：分钟，默认 30 分钟）
auto_commit = false             # 每轮任务完成后自动 git commit
keep_interrupted_context = true # 中断时保留模型已输出的部分上下文
offline_mode = "off"            # 离线环境："off" (联网), "on" (纯离线), "auto" (自动降级)

# =============================================================================
# 视觉预处理代答 (主模型为纯文本时，转发图片给 VL 模型解析)
# =============================================================================
# vision_preprocessor_provider = "claude"

# =============================================================================
# 工具链控制 [tools.*]
# =============================================================================
[tools.todo]
enabled = true                  # 是否开启任务清单跟踪
eager = "auto"                  # auto | preferred | always

[tools.bash]
default_timeout_secs = 120      # 仅 !cmd 默认墙钟
max_timeout_secs = 1800         # 所有 spawn 命令的工具共用硬寿命（秒）
silent_kill_secs = 60           # 第一档探测空闲
second_levell_secs = 120        # 第二档：有输出后的宽限 / 升档后空闲。磁盘/网络 IO 不自动升长任务，二轮后交给模型杀或临时升级

[tools.timeouts]
search_secs = 72                # grep / glob（Grok WSL 60s +20%）
web_connect_secs = 12           # HTTP connect（Grok 10s +20%）
web_request_secs = 72           # web_fetch 空闲 / API 整请求（Grok 60s +20%）
mcp_secs = 180                  # MCP 空闲；有 progress 则等到 max_timeout_secs
skill_cmd_secs = 40             # skill 模板 !`cmd`
hook_secs = 30                  # CC hook 默认+封顶
fs_gate_secs = 36               # 权限门 canonicalize（Grok 30s +20%）

[tools.tool_output]
max_bytes = 65536               # 输出折叠阈值（64KiB）
no_fold_tools = ["fetch_output", "repo_map", "code_explore", "web_fetch", "web_search"]

# =============================================================================
# 网络代理 [network.proxy]
# =============================================================================
[network.proxy]
mode = "follow_system"          # "follow_system" | "default_proxy" | "no_proxy"
# http = "http://127.0.0.1:7890"
# https = "http://127.0.0.1:7890"

# =============================================================================
# 会话日志 [datalog]
# =============================================================================
[datalog]
enabled = true
dir = "~/.atomcode/datalog"

# =============================================================================
# 终端 UI [ui]
# =============================================================================
[ui]
theme = "auto"                  # "auto" | "dark" | "light"
ai_session_naming = true        # AI 自动命名会话
terminal_status_glyph = true
```

---

## 6. Agent 操作 `config.toml` 实施准则

1. **先查后改**：修改前必须先读取当前文件的全部内容，确认已存在的 Provider 与默认模型键。
2. **顶层优先**：新增顶层键（如 `default_model`、`language`）时，始终确保其位于所有 `[table]` 之前。
3. **精准追加**：配置新模型时，在文件末尾追加对应的 `[provider_accounts.*]` + `[models.*]` 或 `[providers.*]`，避免覆盖用户已有的其他 API 凭据。
4. **生效机制**：配置完成后，已运行的 TUI 会话可输入 `/provider` 刷新，或重启终端自动秒进已配置的聊天主界面。
