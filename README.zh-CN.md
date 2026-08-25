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
  <a href="#一jeikcode-是什么">定位与渊源</a> ·
  <a href="#二主流-ai-coding-agent-功能与机制深度对比">机制对比</a> ·
  <a href="#三原生-codeexplore-与-repomap-深度图谱检索">CodeExplore</a> ·
  <a href="#四核心机制与体验亮点">核心亮点</a> ·
  <a href="#五安装与快速上手">快速上手</a> ·
  <a href="#六快捷键与常用命令">快捷键与命令</a> ·
  <a href="#七多项目知识库配置">知识包配置</a>
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

## 一、JeikCode 是什么？

**JeikCode 是基于 AtomCode 基础进行深度拆解开发、架构重构和优化增强的高性能 AI 编程智能体。**

在演进过程中，JeikCode 深度融合了业内顶级 Agent 的优秀机制，并完成了关键架构创新：

- 🛡️ **借鉴 Grok Build 的硬核控制策略**：引入强大的提示词优先级配置（Precedence）、多层工具参数容错自愈修复链（Repair Chain）、结构化诊断回喂以及防止重复调用的熔断机制（Loop Guard）；
- 🌐 **借鉴 OpenCode 的强大远程扩展性**：支持多实例远程无头运行（Serve）、Web 控制台网关（WebUI Gateway）与轻量化跨端实时同步；
- 🔍 **启蒙并超越 CodeGraph 的原生 CodeExplore**：针对 CodeGraph 仅对符号检索有效、完全缺乏语义理解的局限，JeikCode 自研了**加权 AST 向量 + 中英混合代码与注释多重语义检索 + 加权排行算法**，检索效率大幅提升 **60% - 70%**，准确率高达 **90%+**；
- ⚡ **自研高前缀命中率缓存架构**：通过 `sacred_floor` 记忆防压缩保护 + `user-wrap.md` 动态末尾包裹，保障系统前缀严格 Append-only 不可变，彻底解决跨模型与多平台 KV 缓存击穿的行业痛点；
- 🧠 **智能体自配置与全量外置提示词**：内置 8 模块 Teaches 知识库与 `jeikcode_config_guide` 工具，智能体可自主调阅配置规范；所有核心提示词修改毫秒级热重载，无需重启。

---

## 二、主流 AI Coding Agent 功能与机制深度对比

以下对比基于真实的官方架构与实测表现，客观呈现 **JeikCode**、**Claude Code**、**OpenCode** 与 **Grok Build** 各自的机制实现与功能优势：

### 1. 核心功能与机制对比矩阵

| 核心功能与机制 | **JeikCode (本项目)** | **Claude Code (Anthropic)** | **OpenCode (OpenCode AI)** | **Grok Build (SpaceXAI)** | **早期 AtomCode (基线)** |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **Agent 自主读/改/测/验闭环 (Loop)** | ✅ **L0/L1/L2 动态步数状态机** | ✅ **优秀的 Unix 管道式自主闭环** | ✅ **Effect-TS 异步事件流推进** | ✅ **PTY 进程级执行流推进** | ⚠️ 弱隔离胶水层 |
| **工具 5 级自愈修复链 (含 Windows 反斜杠抢救)** | ✅ **5 级自愈 (正则/反序列化/路径抢救)** | ⚠️ 依赖 Claude 顶级推理自纠偏 | ⚠️ Schema 校验失败即中断报错 | ✅ 结构化纠偏与诊断回喂 | ❌ 易反序列化失败 |
| **工具 Schema 类型自动强转 (`"3"`→`3`)** | ✅ **客户端类型层自动纠正** | ⚠️ 依靠模型下一轮重新生成 | ⚠️ 依靠模型下一轮重新生成 | ✅ 支持部分参数纠正 | ❌ 无法自动强转 |
| **工具连续失败 3 次防死循环熔断 (Loop Guard)** | ✅ **3 次失败强制中断换方案** | ⚠️ 依赖上下文截断或模型自省 | ⚠️ 依赖上下文截断或人工打断 | ✅ **具备 Loop Guard 熔断机制** | ❌ 易陷入同构死循环 |
| **代码检索：加权 AST + 中英代码注释语义检索** | ✅ **CodeExplore 自研加权多向量** | ⚠️ ripgrep / Glob 全文本搜索 | ⚠️ LSP 符号 + ripgrep 搜索 | ⚠️ xai-codebase-graph 语法图谱 | ❌ 基础向量易零命中 |
| **repo_map 完整目录树结构先行 (不暴力截断)** | ✅ **首轮完整目录结构概览** | ⚠️ 视项目大小按需探查 | ⚠️ 基础目录列表 | ✅ 具备目录全景支持 | ❌ 易被强制截断折叠 |
| **次相关代码以极小 Token 预算压缩折叠推荐** | ✅ **核心置顶 + 次相关极小预算** | ⚠️ 依靠模型调用工具筛选 | ⚠️ 基础文件折叠 | ✅ 具备输出预算控制 | ❌ 粗暴截断 |
| **KV Cache 前缀防击穿 (Append-only 保证)** | ✅ **`user-wrap.md` 动态末尾包裹** | ✅ **Anthropic 原生 Ephemeral Cache** | ⚠️ 依赖各服务商原生缓存 | ⚠️ 基于 SQLite 日志转录 | ❌ 动态提醒击穿缓存 |
| **`sacred_floor` 核心记忆/规则防压缩丢失** | ✅ **压缩永不截断核心规则** | ✅ **autoDream 记忆固化与压缩** | ⚠️ 依靠滑动窗口截断 | ✅ Compaction Transcript 转录 | ❌ 压缩丢失关键记忆 |
| **提示词全量外置与毫秒级热重载 (无需重启)** | ✅ **`init/rules/wrap` mtime 热重载** | ⚠️ 支持 `CLAUDE.md`，核心提示词内置 | ⚠️ 支持自定义 Prompt，需重新载入 | ⚠️ 支持优先级，改核心需重构 | ❌ 需重启进程生效 |
| **多项目知识包体系 (`rules/dbwords/glossary`)** | ✅ **4 层知识包且严格优先于 System** | ✅ **支持 `CLAUDE.md` 项目指令** | ✅ **支持项目规则与上下文拼接** | ✅ **支持项目级规则配置** | ❌ 仅单文件匹配 |
| **智能体自配置与排障工具 (`jeikcode_config_guide`)** | ✅ **内置 8 模块知识库与自检工具** | ❌ 依赖官方静态在线文档 | ❌ 依赖社区在线文档站 | ❌ 依赖内部专有使用手册 | ❌ 无自查工具 |
| **MCP (Model Context Protocol) 与 Skills 生态** | ✅ **原生 MCP + 动态 Skills 挂载** | ✅ **深度集成 MCP 与 Skills/Hooks** | ✅ **丰富的插件体系与 MCP 生态** | ✅ **内置 Tools 与 MCP 扩展** | ⚠️ 仅基础本地 Skills |
| **多协议支持 (Responses / Completions / Anthropic)** | ✅ **4 大主流协议原生支持** | ⚠️ 深度绑定 Claude 官方协议 | ✅ **覆盖主流商业/开源模型协议** | ⚠️ 深度绑定 xAI Grok 协议 | ❌ 缺少 Responses 协议 |
| **4 档思考努力程度实时切换 (`low/med/high/xhigh`)** | ✅ **随时通过 `/effort` 或 WebUI** | ✅ **深度集成 Claude 3.7 Thinking** | ⚠️ 前端面板手动配置思考参数 | ✅ **深度集成 Grok 推理档位** | ❌ 切换易残留旧绑定 |
| **独立首 Token 活性超时守护 (解决长推理假死)** | ✅ **60s × 3 独立计时防挂起** | ⚠️ 全局统一 Stream 请求超时 | ⚠️ Effect 统一请求超时 | ✅ 进程级看门狗与中断协同 | ❌ 易触发流超时假死 |
| **多实例远程无头服务 (Serve) + WebUI Gateway** | ✅ **纯 Rust 高并发 Serve + Web 控制台** | ❌ 纯终端 CLI (专为 CLI 打造) | ✅ **具备 Web 控制台与桌面端应用** | ❌ 纯终端 Pager TUI 模式 | ⚠️ 仅简单本地 WebUI |
| **终端防误触与 TTY 前台控制权保护** | ✅ **双击 ESC/Ctrl+C + 抢回 TTY** | ⚠️ 单击 ESC 取消流式回复 | ⚠️ 基础快捷键中断 | ✅ **具备成熟的 PTY 终端控制** | ❌ Linux 易挂起锁死 |

---

### 2. 编程语言检索与解析支持矩阵

各大 Agent 均能处理主流编程语言，JeikCode 原生 CodeExplore 在 AST 语法解析与自然语言语义对齐上做了深度特化：

| 语言与框架 | **JeikCode (CodeExplore)** | **Claude Code** | **OpenCode** | **Grok Build** |
| :--- | :---: | :---: | :---: | :---: |
| **Java** | ✅ **AST 语法图谱 + 中英语义** | ✅ **ripgrep 全文本 / 正则检索** | ✅ **LSP 符号 + 文本搜索** | ✅ **语法图谱 + 模糊匹配** |
| **C / C++** | ✅ **AST 语法图谱 + 中英语义** | ✅ **ripgrep 全文本 / 正则检索** | ✅ **LSP 符号 + 文本搜索** | ✅ **原生 PTY + 语法图谱** |
| **Python** | ✅ **AST 语法图谱 + 中英语义** | ✅ **ripgrep 全文本 / 正则检索** | ✅ **LSP 符号 + 文本搜索** | ✅ **语法图谱 + 模糊匹配** |
| **Vue (Vue2/3 SFC)** | ✅ **Template + Script 双 AST 解析** | ✅ **ripgrep 全文本 / 正则检索** | ⚠️ 依赖通用文件检索 | ⚠️ 依赖通用文件检索 |
| **TypeScript / JavaScript** | ✅ **JSX / TSX 元素级语义解析** | ✅ **ripgrep 全文本 / 正则检索** | ✅ **LSP 符号 + 文本搜索** | ✅ **语法图谱 + 模糊匹配** |
| **Rust** | ✅ **AST 语法图谱 + 中英语义** | ✅ **ripgrep 全文本 / 正则检索** | ✅ **LSP 符号 + 文本搜索** | ✅ **原生深度支持** |
| **Go** | ✅ **AST 语法图谱 + 中英语义** | ✅ **ripgrep 全文本 / 正则检索** | ✅ **LSP 符号 + 文本搜索** | ✅ **语法图谱 + 模糊匹配** |
| **Svelte / Astro / SCSS** | ✅ **组件结构与样式类解析** | ✅ **ripgrep 全文本 / 正则检索** | ⚠️ 依赖通用文件检索 | ⚠️ 依赖通用文件检索 |

---

## 三、原生 CodeExplore 与 repo_map 深度图谱检索

### 1. 启蒙与进化：从 CodeGraph 到 CodeExplore

开源项目 **CodeGraph** 带来了优秀的符号索引思路，但 JeikCode 在实战中发现其存在明显短板：**它只懂硬编码的符号语言，完全没有自然语言语义理解能力**。开发者一旦用业务语言提问（例如“找一下处理退款回调的逻辑”），纯符号检索往往很难直接定位。

JeikCode 由此获得启蒙，彻底自研了原生的 **`CodeExplore`** 与 **`repo_map`** 体系：

1. **加权 AST 向量 + 中英混合多重语义检索**：
   - 提取代码结构（AST 符号、函数调用链路、结构体定义）；
   - 提取中英文注释与函数文档（Docstring / Comment）；
   - 将代码逻辑与中英文业务语义进行多重向量化与词林加权对齐。
2. **加权排行置顶最相关代码**：
   - 通过相关度综合评分算法，将最核心的代码段和实现细节**直接置顶展示**给智能体，杜绝无效翻找。
3. **低相关代码极小 Token 预算推荐**：
   - 对于次相关或潜在依赖的文件，绝不暴力 dump 污染上下文，而是以**极小的 Token 预算**提炼推荐核心文件路径与结构摘要，兼顾全局视野与极低 Token 消耗。
4. **实测表现提升**：
   - 🚀 **检索效率提升 60% - 70%**：智能体在 1 轮内即可精准命中核心业务代码，无需反复 grep 试错；
   - 🎯 **检索准确率保持 90%+**：无论是中英文混合描述还是模糊业务需求，均能精准锁定实现位置。

> 💡 *当前 CodeExplore 原生支持中英文双语检索，后续将根据社区反馈逐步拓展更多自然语言！*

---

## 四、核心机制与体验亮点

### 1. 高前缀命中率缓存架构（KV Cache 保护）
- **Append-only 字节级不可变**：系统提示词、`MEMORY` 记忆、`SKILLS` 技能与项目知识规则在会话首部紧凑合并，初态 Git 快照防止环境扰动。
- **`user-wrap.md` 动态末尾包裹**：利用 `{{input}}` 仅对末尾真实用户提问进行动态包裹，修改模板毫秒级热重载，**完全不破坏已缓存的前缀**。
- **`sacred_floor` 防丢保护**：执行 `/compact` 上下文压缩时，底部的核心规则与记忆条目受 `sacred_floor` 保护，永不丢失。
- **UI 纯净还原**：WebUI 与终端界面自动 unwrap，用户看到的始终是干净的原始输入，而模型接收的是严谨的工程指令。

### 2. 五级工具容错与 3 次熔断防御（吸收 Grok 并超越）
- **五级自愈修复**：直解析 → 宽松 JSON 修复（尾逗号/未加引号 key/去掉 Markdown 标记）→ `edit_file` 正则提取 → Schema 字符串解码 → Key-Value 兜底。
- **Windows 路径反斜杠救赎**：在 Serde 反序列化前抢救 `D:\project\src` 单反斜杠，避免 Windows 路径被误转义崩溃。
- **Schema 类型自动强转**：`"quantity":"3"` 自动转为数值 `3`，`"retry":"true"` 自动转为布尔 `true`。
- **3 次失败 Loop Guard 熔断**：同一工具连续失败 3 次触发熔断，强令模型调整思路，拒绝无效重试。

### 3. 提示词全量外置与毫秒级热重载
提示词完全外置于 `~/.atomcode/prompts/`：
- **`init.yaml`**：身份定义、安全隔离与环境配置；
- **`rules.yaml`**：工作流规范、代码定位纪律与输出标准；
- **`user-wrap.md`**：提问包裹模板；
- **修改即刻生效**：修改任意文件，下一轮对话自动热重载，无需重启。

### 4. 多项目知识包最高裁量权
支持多维度工程知识包，**结构化规则严格优先于 System 默认规则**：
- `AGENTS.md` / `ATOMCODE.md`（主工程规范）
- `.atomcode/rules.md`（业务规则与审批约束）
- `.atomcode/dbwords.md`（数据库表结构与字段含义）
- `.atomcode/glossary.md`（业务专有名词映射）

### 5. 智能体自配置与内置 Teaches 知识库
- 内置 8 大模块化知识库（`01_prompts_and_context.md` 至 `08_updates_and_releases.md`）；
- 原生提供 **`jeikcode_config_guide`** 工具，智能体可自主调阅规范并指导用户排查配置。

### 6. 全协议模型适配与 4 档思考努力度
- 原生支持 OpenAI Responses（`/v1/responses`）、Chat Completions、Anthropic 与 Ollama 四大协议；
- 随时通过 `/effort` 切换 4 档思考努力程度（`low` / `medium` / `high` / `xhigh` / `off`）；
- 账号凭据与模型参数彻底解耦，打开 `/modeladd` 自动拉取上游 `/models` 列表。

### 7. 独立首 Token 活性超时守护（First-Token Timeout）
- 针对 DeepSeek-R1、Grok 3 等超大思考模型，建立**独立的 `first_token_timeout` 计时器**（默认 60s × 3 次自动重试），彻底告别长时间推理静默导致的假死挂起。

### 8. 远程无头服务 (Serve) 与 WebUI 控制台（吸收 OpenCode）
- **本地 WebUI**：输入 `/webui` 或 `jeikcode webui`，即刻在浏览器中开启可视化控制台（含 Token 动态分类浮层）。
- **多实例远程服务**：
  ```bash
  jeikcode serve --host 0.0.0.0 --port 4096 --token sk-my-secret
  jeikcode attach http://192.168.1.100:4096 --token sk-my-secret
  ```

### 9. 终端防误触与 TTY 控制权保护
- **双击防误触**：严格双击 `ESC` 或 `Ctrl+C` 取消当前回合执行并恢复输入框；
- **TTY 前台控制权夺回**：Linux 回合结束后主动夺回 TTY 控制权，忽略挂起信号，防止终端锁死。

---

## 五、安装与快速上手

### 1. 预编译二进制一键安装（推荐）

前往 [GitHub Releases](https://github.com/jeikl/jeikcode/releases) 下载：

```bash
# Linux / macOS 一键安装
curl -fsSL https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install.sh | bash

# Windows PowerShell 一键安装
irm https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install.ps1 | iex
```

### 2. 从源码编译安装

需要 **Rust 1.88+**（[rustup.rs](https://rustup.rs/)）：

```bash
git clone https://github.com/jeikl/jeikcode.git
cd jeikcode

cargo install --path crates/atomcode-cli --bin jeikcode --locked
jeikcode --version
```

### 3. 配置与启动

进入任意工程目录启动：

```bash
cd /path/to/your/project
jeikcode
```

配置文件保存在 `~/.atomcode/config.toml`：

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

常用运行方式：
```bash
# 指定目录启动
jeikcode -C /path/to/project

# 指定模型启动
jeikcode --model deepseek-reasoner

# Headless 模式（适合脚本与自动化 CI）
jeikcode -p "排查并修复 OAuth 登录 404 错误"

# 恢复上一会话
jeikcode -c
```

---

## 六、快捷键与常用命令

### 1. 终端核心快捷键

| 快捷键 | 功能说明 |
| :--- | :--- |
| `Enter` | 发送当前输入内容 |
| `\` + `Enter` | 换行（全终端通用兼容） |
| `Shift+Enter` / `Alt+Enter` | 换行（需终端协议支持） |
| `Esc` ×2 / `Ctrl+C` ×2 | **双击防误触取消**：终止当前执行并恢复输入框 |
| `Alt+V` / `Ctrl+Alt+V` | 粘贴剪贴板截图为多模态图片附件 |
| `Ctrl+Up` / `Ctrl+Down` | 向上 / 向下滚动对话区域 |
| `PageUp` / `PageDown` | 翻页滚动对话 |
| `Ctrl+L` | 清屏并保留上下文 |

### 2. 常用斜杠命令

| 命令分类 | 斜杠命令 | 详细功能 |
| :--- | :--- | :--- |
| **模式与自主** | `/plan` | 切换至只读探索模式（只调研不修改代码） |
| | `/build` | 切换至代码修改执行模式 |
| | `/goal <目标>` | 设定完成准则，开启多轮自主攻坚模式 |
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

## 七、多项目知识库配置

在项目根目录下放置以下文件，JeikCode 会在运行时动态合并注入，且**严格优先于 System 默认规则**：

```text
your-project/
  ├── AGENTS.md                  # 主工程规范 (架构约束、技术栈、测试命令)
  ├── user-wrap.md               # 项目级提问动态包裹模板 (含 {{input}})
  └── .atomcode/
      ├── rules.md               # 业务规则与审批约束
      ├── dbwords.md             # 数据库表结构与字段语义
      ├── glossary.md            # 业务专有名词映射表
      └── thesaurus/             # 项目专属领域词林 (*.txt)
```

---

## 八、开源许可证

本项目基于 [MIT License](LICENSE) 开源。

<p align="center">
  Crafted with Rust, Tree-Sitter, Ratatui, and Passion for Engineering Excellence.
</p>
