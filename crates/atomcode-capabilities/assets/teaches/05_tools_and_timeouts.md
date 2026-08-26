# 05 - 工具策略、超时与系统控制配置指南 (Tools, Timeouts & Policies)

全局配置文件路径：`~/.atomcode/config.toml`。

---

## 1. Bash 命令执行策略与超时设置 (`[tools.bash]`)

控制 Agent 执行 shell 命令（`bash` 工具）的生命周期与超时熔断：

```toml
[tools.bash]
default_timeout_secs = 120      # 短命令默认超时（秒）：未显式指定超时时的默认挂钟时间
max_timeout_secs = 1800         # 长命令最高上限（秒）：编译、测试、依赖安装（cargo, npm, make 等）的超时上限（默认 30 分钟）
silent_kill_secs = 60           # 静默终止（秒）：短命令若无任何 stdout/stderr 输出持续超过该时间，自动触发空闲 kill
```

Windows 上 `bash` 工具支持可选的 `shell` 调用参数：

- `"shell": "default"`：默认行为，使用 Git Bash/MSYS2；未安装时回退到 `cmd.exe`。
- `"shell": "powershell"`：直接启动 PowerShell，并通过 UTF-16LE `EncodedCommand` 传递脚本，不经过 Bash/cmd 二次解析。访问 UNC 或隐藏共享时应配合单引号和 `-LiteralPath`，例如 `Get-ChildItem -LiteralPath '\\192.168.5.50\erp_code$'`。

原生 PowerShell 模式只解决解释器边界与参数保真问题；工具的超时、取消、进程树回收及危险/写入型命令审批策略保持不变。

---

## 2. 工具大输出折叠与预览策略 (`[tools.tool_output]`)

防止 `bash` 或大型工具输出瞬间占满上下文窗口：

```toml
[tools.tool_output]
max_bytes = 65536               # 输出折叠阈值（默认 64KiB = 65536 字节）。超过此大小的输出将自动折叠为头尾预览并存入临时 Artifact（可通过 fetch_output 提取完整内容）；设为 0 完全禁用折叠
no_fold_tools = [               # 白名单工具列表：以下工具的输出无论多大均直达模型，绝不折叠
    "fetch_output",
    "repo_map",
    "code_explore",
    "find_symbol",
    "trace_chain",
    "blast_radius",
    "web_fetch",
    "web_search"
]
```

---

## 3. 任务清单策略 (`[tools.todo]`)

```toml
[tools.todo]
enabled = true                  # 是否开启任务清单机制（支持 ATOMCODE_TODO 环境变量覆盖）
eager = "auto"                  # 积极程度："auto" (按需模型识别) | "preferred" (高 recency 提醒) | "always" (首轮强制创建)
```

---

## 4. 主 Agent 轮次与首 Token 超时控制 (`[coding]`)

```toml
[coding]
max_rounds = 200                # 单轮会话模型思考交互的硬上限（检查点门限，0 表示无限制，可通过 ATOMCODE_TURN_MAX_ROUNDS 覆盖）
first_token_timeout_secs = 60   # 首 Token 响应超时（秒）：等待模型返回首个数据块的最大耗时（防止大推理模型静默死锁）
first_token_timeout_retries = 3 # 首 Token 超时后的自动重试次数
```

---

## 5. 子代理并发与轮次 (`[subagent]`)

针对 `task` 工具派生的并发子 Agent 限制：

```toml
[subagent]
max_concurrent = 3              # 最大并发子代理数（默认 3）
max_rounds = 200                # 每个子代理执行任务的最大交互轮次（0 表示无限制）
```

---

## 6. 网络代理与语言服务器

### 6.1 网络代理 (`[network.proxy]`)
```toml
[network.proxy]
mode = "follow_system"          # 代理模式："follow_system" (跟随系统) | "default_proxy" | "no_proxy"
# http = "http://127.0.0.1:7890"
# https = "http://127.0.0.1:7890"
```

### 6.2 语言服务器 LSP (`[lsp]`)
```toml
[lsp]
enabled = false                 # 是否启用深度 LSP 静态分析（rust-analyzer, gopls 等）
auto_detect = false             # 自动检测工作区语言服务器
diagnostics_settle_delay_ms = 150 # 诊断结果等待延迟（毫秒）
```

---

## 7. 会话审计、UI 与中断保护

```toml
# 顶层中断保护开关（必须置于顶层）
keep_interrupted_context = true # 按 Ctrl+C 中断时，保留已生成的上下文并安全闭合 tool_calls，方便下一句无缝续接

[datalog]
enabled = true                  # 是否记录全量结构化执行审计日志
dir = "~/.atomcode/datalog"

[ui]
theme = "auto"                  # 终端主题："auto" | "dark" | "light"
ai_session_naming = true        # 首轮自动通过 AI 为会话生成简明标题
terminal_status_glyph = true    # 终端标题栏显示状态圆点（🟢空闲/🟡运行/🔴待审批）
truncate_resumed_history = true # 恢复长会话时截断超长历史展示以防终端卡顿
```
