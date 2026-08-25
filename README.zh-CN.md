<div align="center">
<pre>
       _      _ _     ____          _
      | | ___(_) | __/ ___|___   __| | ___
   _  | |/ _ \ | |/ / |   / _ \ / _` |/ _ \
  | |_| |  __/ |   <| |__| (_) | (_| |  __/
   \___/ \___|_|_|\_\\____\___/ \__,_|\___|
</pre>
</div>

<p align="center">
  <strong>用 Rust 打造的极致高性能开源终端 AI 编码智能体 (Agentic AI Coding Assistant)</strong>
</p>

<p align="center">
  <a href="./README.md">English</a> · 简体中文
</p>

<p align="center">
  <a href="#快速开始">快速开始</a> ·
  <a href="#多方-ai-coding-agent-深度对比">四方对比</a> ·
  <a href="#核心架构与技术突破">架构突破</a> ·
  <a href="#核心功能特性">功能特性</a> ·
  <a href="#安装指南">安装指南</a> ·
  <a href="#快捷键与命令">快捷键与命令</a> ·
  <a href="#项目级知识库与规则">知识库与规则</a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/version-6.0.26-blue.svg" alt="version">
  <img src="https://img.shields.io/badge/rust-1.88%2B-orange.svg" alt="rust">
  <img src="https://img.shields.io/badge/license-MIT-green.svg" alt="license">
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows%20%7C%20HarmonyOS-lightgrey.svg" alt="platform">
  <a href="https://github.com/jeikl/jeikcode" target="_blank">
    <img src="https://img.shields.io/github/stars/jeikl/jeikcode?style=social" alt="GitHub Stars"/>
  </a>
</p>

---

**JeikCode** 是一款运行在终端里的新一代极速、高自主性 AI 编程智能体。采用纯 **Rust** 构建，具备毫秒级冷启动、极致内存控制与零运行时依赖。只要用自然语言输入任务，JeikCode 就会自主阅读代码拓扑、探索语义图谱、批量修改代码、执行测试并自我验证修复，全程闭环推进工程交付。

无论是作为日常主力终端 Agent，还是作为无头服务集成到 CI/CD、WebUI 与 IDE 中，JeikCode 都提供了对标甚至超越 **Claude Code**、**OpenCode** 与 **Grok Build** 的卓越工程体验。

---

## 🌟 多方 AI Coding Agent 深度对比

为清晰展现各主流开源与商业 AI Coding Agent 的机制差异，下表基于对各框架底层架构、源码实现（含兄弟项目代码深度扫描）及实测表现整理：

| 对比维度 | **JeikCode (本项目)** | **Claude Code (Anthropic)** | **OpenCode (OpenCode AI)** | **Grok Build (SpaceXAI)** | **早期 AtomCode (基线)** |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **开发语言与运行时** | **纯 Rust 原生编译**<br>• 内存占用 <30MB<br>• 毫秒级启动、零 GC 抖动<br>• 单一自包含二进制 | **TypeScript / Node.js**<br>• 内存占用 ~200MB+<br>• 依赖 V8/Node 运行环境<br>• npm 全局安装 | **TypeScript / Bun / Effect**<br>• 内存占用 ~150MB<br>• 依赖 Bun / Effect-TS 栈<br>• SQLite 会话持久化 | **纯 Rust 多 Crate 架构**<br>• 内存占用 <50MB<br>• 76+ Crate 复杂单体<br>• 依赖 DotSlash / protoc | **Rust 早期架构**<br>• 单会话基础运行<br>• 缺少深层图谱与分层隔离 |
| **核心架构分层** | **严格 L0/L1/L2 解耦**<br>• L0: 纯净 Kernel 循环<br>• L1: 复用能力与图谱工具<br>• L2: Coding 状态机与生命周期<br>• Drivers: TUI / WebUI / Serve / ACP | **单体 CLI 管道**<br>• 紧密围绕 Claude API 闭环<br>• 流程硬编码于 TS 脚本中<br>• 单一终端交互模式 | **五层单体模块**<br>• Schema → Protocol → Core → Server → Client<br>• 跨进程 Effect 管道交互 | **单体 Pager 体系**<br>• PTY 管道与 Pager TUI<br>• 围绕 xAI 专有后端高度定制<br>• 深度绑定内部中间件 | **双层弱解耦**<br>• Core + Bridge 遗留依赖<br>• 业务逻辑与驱动轻度耦合 |
| **KV Cache 与上下文保护** | **全流程 Append-only 保证**<br>• `user-wrap.md` 动态末尾包裹<br>• `sacred_floor` 记忆永不压缩<br>• 初态 Git 快照防止缓存击穿<br>• 毫秒级 mtime 提示词热重载 | **Ephemeral Cache 机制**<br>• 依赖 Anthropic 缓存标头<br>• 仅限 Claude 系列生效<br>• 缺少本地模板动态包装 | **基础消息数组**<br>• 依赖各模型服务商原生缓存<br>• 动态 Prompt 插入易破坏前缀<br>• 上下文压缩可能截断关键规则 | **Transcript 压缩流**<br>• 基于 SQLite 的日志追踪<br>• 依赖专用 Compaction Transcripts<br>• 无本地动态 user-wrap 机制 | **基础静态前缀**<br>• 动态 Reminder 会产生前缀扰动<br>• 压缩策略较粗糙 |
| **代码图谱与全景检索** | **CodeIntel 2.0 深度图谱**<br>• Tree-Sitter 全栈 AST 分析<br>• 6 类拓扑全景 (连通/父链/子树等)<br>• **9 领域中英双语词林对齐**<br>• BM25 + 概念向量混合检索<br>• `zstd` 二进制索引共享缓存 | **传统搜索 (Grep/Glob/View)**<br>• 无本地 AST 依赖图谱<br>• 大仓探索极度消耗 Token<br>• 缺少领域语义词林映射 | **文件检索 + 基础 LSP**<br>• 依赖标准 ripgrep 与 LSP<br>• 缺少全局拓扑流向分析<br>• 无中文多对多概念词典 | **xai-codebase-graph**<br>• Rust 实现代码图谱与模糊搜索<br>• 支持文件变更通知 (fsnotify)<br>• 无领域双语对齐词林 | **基础词林与哈希向量**<br>• 单会话独立索引，冷启动慢<br>• 探索工具容易产生零命中<br>• 目录树易被折叠截断 |
| **工具容错与修复机制** | **五级修复链 + 3次熔断防御**<br>• 宽松 JSON 修复 + 正则提取<br>• Windows 反斜杠路径抢救<br>• Schema 类型自动强转 (`"3"`→`3`)<br>• 结构化诊断反馈回喂<br>• 循环重复调用熔断 (Loop Guard) | **基础错误反馈**<br>• 模型解析失败直接返回报错<br>• 容易陷入重复同构错误调用<br>• 依赖模型自我纠偏 | **Effect Schema 校验**<br>• 基于 Zod / Effect 强类型检查<br>• 校验失败中断并返回原因<br>• 缺少多级自动降级抢救链 | **结构化诊断 + Loop Guard**<br>• 具备参数类型纠偏与熔断<br>• 针对 Grok 模型调用做了优化<br>• Windows 路径兼容性一般 | **三层基础修复**<br>• 基础尾逗号/引号修复<br>• 缺少 Schema 类型层自愈能力<br>• 容易因 Windows 路径报错 |
| **首 Token 活性与超时守护** | **独立双臂超时机制**<br>• **First-Token 独立计时 (60s×3)**<br>• 完美适配 DeepSeek-R1 / O1 / Grok 3 深度思考静默<br>• 编译测试 900s 专属超时 | **统一 Stream 超时**<br>• 依靠全局请求超时控制<br>• 超长推理模型可能误判中断 | **请求级 Timeout**<br>• 由 Effect 运行时超时控制<br>• 难以细分首 Token 与流间隙 | **PTY / 进程级看门狗**<br>• 具备完善的进程级中断控制<br>• 与 xAI 后端专有协议协同 | **单一流空闲超时**<br>• 首 Token 等待时间过长时会触发全局超时挂起 |
| **模型生态与思考档位** | **完全解耦，支持全协议**<br>• **OpenAI Responses (/v1/responses)**<br>• Chat Completions / Anthropic / Ollama<br>• **思考档位实时切换 (4档/自定义)**<br>• 动态拉取上游 `/models` 列表<br>• 视觉代答分流 (Vision Preprocessor) | **高度绑定 Anthropic**<br>• 专为 Claude 3.5/3.7 设计<br>• 深度集成 Thinking Budget<br>• 接入第三方模型需代理转换 | **广泛多模型支持**<br>• 支持 OpenAI / Anthropic / Gemini / OpenRouter / Ollama<br>• 具备模型选择界面<br>• 需在前端手动配置参数 | **高度绑定 xAI Grok**<br>• 围绕 Grok 2/3 模型定制<br>• 深度集成专有推理解析<br>• 不便于任意私有化模型接入 | **基础 OpenAI 兼容**<br>• 账号与模型未完全解耦<br>• 不支持 Responses 协议思考回传<br>• 切换模型可能残留旧绑定 |
| **交互形态与终端体验** | **多元驱动与防误触设计**<br>• TUI: **双击 ESC/Ctrl+C 防误触**<br>• Linux 回合结束主动夺回 TTY<br>• **WebUI Gateway** (实时Token详情)<br>• **无头 Remote Serve 多实例**<br>• ACP 协议集成 | **纯 Terminal CLI**<br>• 交互精炼简洁<br>• 无原生 Web 界面<br>• 缺少防误触二次确认设计 | **全平台客户端矩阵**<br>• Terminal TUI<br>• Desktop 桌面应用 (Tauri/Electron)<br>• Web Console + Slack Bot<br>• 界面丰富但体积较大 | **Pager 风格终端 TUI**<br>• 优秀的终端文本滚动与 Diff<br>• 深度定制的按键绑定<br>• 缺少独立轻量 WebUI | **基础 TUI**<br>• 单击 ESC/Ctrl+C 易误触中断<br>• Linux 下偶发 TTY 挂起或无法输入 |
| **系统配置与自更新** | **Teaches 知识库 + 无感更新**<br>• 内置 8 模块渐进式文档<br>• `jeikcode_config_guide` 智能体自查<br>• **GitHub Releases 单步无感升级**<br>• 交互式配置差异安全合并 | **npm 全局升级**<br>• `npm update -g @anthropic-ai/claude-code`<br>• 静态在线文档 | **多渠道包管理升级**<br>• Homebrew / Scoop / npm / Nix<br>• 在线文档站 | **源码同步 / DotSlash**<br>• 依赖单体仓库同步或脚本安装<br>• 在线专有文档 | **基础二进制替换**<br>• 依赖特定源，升级需手动覆盖<br>• 无内置配置自检工具 |
| **开源度与私有化** | **100% 开源 (MIT)**<br>• 允许完全自由商用、二次修改<br>• 完全本地私有部署，零隐私外泄 | **部分闭源**<br>• npm 发行为混淆代码<br>• 核心后端调度位于云端 | **100% 开源 (MIT)**<br>• 社区活跃，插件生态丰富 | **官方同步开源**<br>• 部分基础设施依赖 xAI 专有云 | **开源 (MIT)** |

---

## 🚀 核心架构与 30 天技术突破（v6.0.0 ~ v6.0.26）

```text
┌────────────────────────────────────────────────────────────────────────┐
│               JeikCode 统一运行时调用链路 (CodingRuntime)                │
└────────────────────────────────────────────────────────────────────────┘
     CLI / TUIX  │  WebUI Gateway  │  Remote Serve  │  Daemon  │  ACP
                 └───────────────┬──────────────────┘
                                 │
                                 ▼
                   JeikCode CodingRuntime (L2)
       ┌──────────────────────────────────────────────────┐
       │ • 会话生命周期与状态机 (Session Machine)            │
       │ • 动态 Prompt 模板与 user-wrap.md 热重载          │
       │ • Sacred Floor 上下文压缩保护 (Memory / Rules)    │
       │ • First-Token 活性超时守护与重试                   │
       │ • 思考档位 (Reasoning Effort) 与 Responses 协议   │
       │ • 子代理调度 (Subagent Dispatcher)                │
       └─────────────────────────┬────────────────────────┘
                                 │
                                 ▼
                   atomcode-capabilities (L1)
       ┌──────────────────────────────────────────────────┐
       │ • CodeIntel 2.0 (Tree-Sitter AST / 6类拓扑 / 9词林)│
       │ • 五级容错参数修复链 (Repair Chain & Loop Guard) │
       │ • jeikcode_config_guide 知识库自查工具           │
       │ • 跨平台文件系统 (Windows \\?\ 修复 / UTF-8 BOM)  │
       └─────────────────────────┬────────────────────────┘
                                 │
                                 ▼
                   atomcode-kernel (L0 纯净核心)
       ┌──────────────────────────────────────────────────┐
       │ • 中立 Agent 执行循环 (Autonomous Loop)           │
       │ • 流式 Token 分发与观察回传 (Observation Sink)    │
       │ • 严格单向依赖，零平台/业务特化                  │
       └──────────────────────────────────────────────────┘
```

### 1. KV Cache 前缀稳定性与动态用户包装 (`user-wrap.md`)
- **前缀字节级不可变**：将 `MEMORY`、`SKILLS`、`MCP` 以及项目约束（`AGENTS.md`、`rules.md`、`dbwords.md`）紧凑合并在会话首部，受 `sacred_floor` 严格保护，在 `/compact` 压缩时绝不丢失。
- **`user-wrap.md` 模板插值**：通过 `{{input}}` 动态包裹用户最后一条真实提问（支持全局 `~/.atomcode/user-wrap.md` 与项目级 `./user-wrap.md` 就近覆盖），毫秒级 mtime 热重载，既能注入业务规范，又完全不击穿服务商的 KV 缓存。
- **UI 纯净还原**：WebUI 与 TUIX 自动对历史消息执行 unwrap，用户看到的永远是清爽的原始提问，而模型接收的是经过严密防护的完整指令。

### 2. CodeIntel 2.0 全景图谱与双语词林检索
- **全语言 Tree-Sitter 支持**：深度解析 Rust、Go、Python、Java、C++，以及前端全栈（Vue2/3 SFC、React TSX/JSX、Svelte、Astro、CSS/SCSS/LESS、HTML、YAML/JSON 插件）。
- **6 类图谱拓扑全景**：以锚定点为核心，联动探索子树、父链、兄弟模块、图连通流向与路径关联词，杜绝传统 Grep 的盲目漫游。
- **9 领域双语词林**：内置计算机科学、AI Agent、Web开发、全栈工程、电商系统、后台管理、医疗、机器人等领域词林，实现自然语言到代码符号的多对多对齐。
- **`units.v4.bin` 共享索引**：进程级 zstd 压缩缓存 + Rayon 多核并行打分，大仓秒级加载，查询耗时 <1ms。

### 3. 五级工具参数容错与循环熔断防御（超越 Grok）
- **五级自愈修复链**：直解析 → 宽松 JSON 修复（处理尾逗号、未加引号 key、Markdown 代码块标记）→ `edit_file` 专用正则提取 → Schema 绑定字符串化解码 → Key-Value 兜底。
- **Windows 路径反斜杠救赎**：在 Serde 反序列化前抢救 `D:\project\test` 单反斜杠，避免转义污染。
- **Schema 类型层强制纠偏**：自动将 `"quantity":"3"` 纠正为数值 `3`，`"retry":"true"` 纠正为布尔 `true`。
- **失败诊断与熔断保护**：调用失败回喂字段级 Schema 提示；同一工具连续失败 3 次触发 **Loop Guard 熔断**，强令模型调整思路。

### 4. 首 Token 活性超时守护（First-Token Liveness Timeout）
- 针对 DeepSeek-R1、Grok 3 等深度思考模型在吐出首个 Token 前长达数十秒的推理静默，建立**独立的 `first_token_timeout` 计时器**（默认 60s × 3 次自动重试）。与流间隙超时互补，彻底告别假死等待。

### 5. 全协议思考档位与模型账号解耦
- **四大协议原生适配**：支持 OpenAI Responses（`/v1/responses`）、Chat Completions、Anthropic 与 Ollama。
- **4 档思考努力程度**：随时通过 `/effort` 或 WebUI 切换 low / medium / high / xhigh。
- **模型与凭证彻底解耦**：`[provider_accounts.*]` 管理凭据，`[models.*]` 管理模型参数；打开 `/modeladd` 或 `/provider` 自动拉取上游 `/models` 列表并智能获焦。

### 6. 渐进式 Teaches 知识库与智能体自查
- 内置 8 大模块化知识库（`01_prompts_and_context.md` 至 `08_updates_and_releases.md`），编译期与宿主机资产自动同步。
- 原生内置 `jeikcode_config_guide` 工具，智能体遇到配置或使用疑问时可自主调阅规范。

---

## 🛠️ 核心功能特性

### 1. 终端 TUIX 极速体验
- **防误触设计**：严格双击 `ESC` 或 `Ctrl+C` 取消正在执行的回合并返回输入框。
- **TTY 前台控制权保护**：Linux 回合结束后主动夺回 TTY 前台，彻底忽略挂起信号，防止键盘锁死。
- **多行输入与代码高亮**：支持 `\` + `Enter` 或 `Shift+Enter` 换行，支持 `base16-ocean.dark` 语法高亮与 Markdown 实时渲染。
- **剪贴板图片直贴**：支持 `Alt+V` / `Ctrl+Alt+V` / `/paste` 直接发送截图给多模态模型。

### 2. WebUI Gateway 与远程服务
- **本地 WebUI**：输入 `/webui` 或 `jeikcode webui`，即可在浏览器中开启可视化控制台。
- **实时 Token 详情浮层**：清晰展示提示词 Token、推理 Token、缓存命中 Token 与 Sacred Floor 保护状态。
- **远程无头服务 (Serve)**：
  ```bash
  # 在指定端口启动服务（支持多实例并行）
  jeikcode serve --host 0.0.0.0 --port 4096 --token sk-my-secret

  # 从另一台机器连接
  jeikcode attach http://192.168.1.100:4096 --token sk-my-secret
  ```

### 3. 多模式自主执行
- **Plan 模式 (`/plan`)**：只读探索模式，智能体只分析图谱、提出方案，不修改任何文件。
- **Build 模式 (`/build`)**：全功能执行模式，自主编辑、编译、测试与修复。
- **Goal 模式 (`/goal <目标>`)**：设定最终完成准则，智能体跨轮次自动迭代推进，直至达成目标。
- **后台会话 (`/bg`)**：长任务脱机后台执行，前端 TUI 继续处理其他任务。

---

## 📦 安装指南

### 方式 1：GitHub Releases 预编译二进制（推荐）

前往 [GitHub Releases](https://github.com/jeikl/jeikcode/releases) 下载对应系统的预编译包：

```bash
# Linux / macOS 一键安装
curl -fsSL https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install.sh | bash

# Windows PowerShell 一键安装
irm https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install.ps1 | iex
```

### 方式 2：从源码编译安装

需要安装 **Rust 1.88+**（[rustup.rs](https://rustup.rs/)）：

```bash
git clone https://github.com/jeikl/jeikcode.git
cd jeikcode

# 编译并安装到 Cargo bin 目录
cargo install --path crates/atomcode-cli --bin jeikcode --locked

# 验证安装
jeikcode --version
```

---

## 🏁 快速开始

### 1. 启动与配置模型

进入任意代码工程目录直接启动：

```bash
cd /path/to/your/project
jeikcode
```

首次运行会引导配置首个模型 Provider。配置文件保存在 `~/.atomcode/config.toml`：

```toml
default_provider = "deepseek"

[provider_accounts.deepseek]
api_key  = "sk-xxxxxxxxxxxxxxxxxxxxxxxx"
base_url = "https://api.deepseek.com/v1"

[models.deepseek-chat]
provider = "deepseek"
model    = "deepseek-chat"
protocol = "chat_completions"

[models.deepseek-reasoner]
provider         = "deepseek"
model            = "deepseek-reasoner"
protocol         = "chat_completions"
reasoning_effort = "high"
```

### 2. 常用命令行参数

```bash
# 启动并在指定目录工作
jeikcode -C /path/to/project

# 指定使用的模型
jeikcode --model deepseek-reasoner

# 单次 Headless 模式（输出结果至 stdout，适合脚本/CI）
jeikcode -p "排查并修复 OAuth 登录回调 404 错误"

# 从文件加载提示词
jeikcode --prompt-file task.md

# 恢复上一会话
jeikcode -c
```

---

## ⌨️ 快捷键与命令

### 1. 终端快捷键

| 快捷键 | 功能说明 |
| :--- | :--- |
| `Enter` | 发送当前输入内容 |
| `\` + `Enter` | 换行（全终端通用兼容） |
| `Shift+Enter` / `Alt+Enter` | 换行（需终端协议支持） |
| `Esc` ×2 / `Ctrl+C` ×2 | **双击防误触取消**：终止当前思考/执行并返回输入框 |
| `Alt+V` / `Ctrl+Alt+V` | 从剪贴板粘贴图片附件 |
| `Ctrl+Up` / `Ctrl+Down` | 向上 / 向下滚动对话区域 |
| `PageUp` / `PageDown` | 翻页滚动对话 |
| `Ctrl+L` | 清屏并保持上下文 |

### 2. 常用斜杠命令

| 命令分类 | 斜杠命令 | 详细功能 |
| :--- | :--- | :--- |
| **模式与自主** | `/plan` | 切换至只读探索模式 |
| | `/build` | 切换至代码修改执行模式 |
| | `/goal <目标>` | 设定目标并开启多轮自主攻坚模式 |
| | `/review` | 针对当前 Git 改动执行全方位代码审查 |
| | `/effort` | 切换思考努力程度（low / medium / high / xhigh / off） |
| **会话与后台** | `/resume` | 交互式恢复或切换历史会话 |
| | `/bg` | 查看或管理后台异步任务（`/bg list`） |
| | `/clear` | 清空对话上下文开启全新任务 |
| | `/compact` | 手动触发上下文压缩（保留 Sacred Floor 记忆） |
| **模型与工具** | `/model` | 快速切换当前生效模型 |
| | `/provider` | 管理 Provider 账号凭证 |
| | `/webui` | 启动本地 Web 控制台 Gateway |
| | `/diff` | 查看当前工作区的所有未提交改动 |
| | `/undo` | 撤销上一轮的文件修改操作 |
| **知识与指南** | `/guide <问题>` | 调阅 Teaches 知识库进行使用与配置指导 |
| | `/reload` | 热重载 `config.toml`、`init.yaml` 与 `rules.yaml` |

---

## 📚 项目级知识库与规则

JeikCode 具备业内领先的**项目级规则最高裁量权**。只要在工程中放置以下规范文件，JeikCode 会在运行时动态合并注入，且**严格优先于默认 System 规则**：

| 文件规范 | 存放路径 | 核心用途 |
| :--- | :--- | :--- |
| **主工程规范** | `AGENTS.md` 或 `ATOMCODE.md` | 项目架构定义、代码规范、测试准则 |
| **业务词表 (Glossary)** | `.atomcode/glossary.md` | 业务专有名词映射为代码符号别名 |
| **业务规则 (Rules)** | `.atomcode/rules.md` | 核心业务流、权限审批、状态机约束 |
| **数据库词典 (DbWords)**| `.atomcode/dbwords.md` | 表结构、字段含义与 SQL 编写规范 |
| **动态提问模板** | `user-wrap.md` | 自定义提问包装结构（含 `{{input}}`） |

---

## 🛡️ 架构安全与权限模型

1. **高危命令强制确认**：`rm -rf`、`git push --force`、`DROP TABLE` 等破坏性操作必须用户显式授权。
2. **跨目录读取限制**：对工作区之外的绝对路径访问实行严格的风险分级提示。
3. **源码删除防呆**：执行源码文件的删除操作绝不自动放行。
4. **即时文件回滚**：每一轮文件编辑均在内存中记录快照，随时可通过 `/undo` 一键回退。

---

## 🤝 参与贡献与开发

欢迎参与 JeikCode 的开发与建设！

```bash
# 克隆仓库
git clone https://github.com/jeikl/jeikcode.git
cd jeikcode

# 运行代码格式化与静态检查
cargo fmt --all
cargo clippy --all

# 运行核心单测
cargo test --workspace
```

- **新增工具**：在 `crates/atomcode-capabilities/src/tools/` 下实现 `Tool` trait；
- **新增检索与词林**：在 `crates/atomcode-capabilities/src/codeintel/` 扩展解析与领域词典；
- **扩展配置指南**：同步更新 `crates/atomcode-capabilities/assets/teaches/`。

---

## 📄 开源许可证

本项目基于 [MIT License](LICENSE) 开源。

<p align="center">
  Crafted with Rust, Tree-Sitter, Ratatui, and Passion for Engineering Excellence.
</p>
