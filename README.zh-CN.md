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
  <a href="#二多方-ai-coding-agent-功能与机制深度对比">机制对比</a> ·
  <a href="#三10-大核心硬核机制深度拆解">核心机制</a> ·
  <a href="#四安装与快速上手">快速上手</a> ·
  <a href="#五快捷键与常用命令">快捷键与命令</a> ·
  <a href="#六多项目知识库与规则最高裁量权">知识包配置</a>
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

在设计与演进过程中，JeikCode 融合了业内顶级开源与商业 Agent 的核心优势，并完成了关键架构创新：
- 🛡️ **吸收 Grok Build 的硬核容错与提示词策略**：引入强大的提示词优先级裁决（Precedence）、多层工具容错自愈修复链（Repair Chain）、结构化诊断回喂与防死循环调用熔断（Loop Guard）；
- 🌐 **吸收 OpenCode 的远程扩展架构**：构建了强大的多实例远程无头运行（Serve）、Web 控制台网关（WebUI Gateway）与轻量化跨端实时同步；
- ⚡ **自主研发核心架构突破**：
  - **高前缀命中率缓存架构（High Cache Hit Prefix Architecture）**：`sacred_floor` 记忆防压缩保护 + `user-wrap.md` 动态末尾包裹，保证会话前缀字节级 Append-only 不可变，彻底解决 LLM 服务商 KV 缓存击穿痛点；
  - **CodeIntel 2.0 全景图谱与双语词林检索**：Tree-Sitter 全栈 AST 分析 + 6 类拓扑流向 + 9 大内置中英双语领域词林 + BM25/向量混合召回 + `units.v4.bin` (zstd) 进程级共享索引；
  - **智能体自配置体系（Teaches 知识库 + `jeikcode_config_guide`）**：内置 8 大模块化知识库，赋予 Agent 原生自查与指导系统配置的能力；
  - **提示词全量自配置与毫秒级热重载**：`init.yaml`、`rules.yaml` 与 `user-wrap.md` 无需重启即刻生效。

---

## 二、多方 AI Coding Agent 功能与机制深度对比

以下对比完全聚焦于 **Agent 核心功能、代码检索、上下文管理、工具容错与模型协议等纯技术机制**：

| 机制与功能维度 | **JeikCode (本项目)** | **Claude Code (Anthropic)** | **OpenCode (OpenCode AI)** | **Grok Build (SpaceXAI)** | **早期 AtomCode (基线)** |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **1. Agent Loop 调度与闭环** | **L0/L1/L2 三层解耦调度**<br>• 动态步数预算自适应调节<br>• 读改测自闭环验证回路<br>• 多轮状态机与子代理解耦 | **单体 CLI 执行循环**<br>• 针对 Claude API 紧耦合<br>• 依赖服务端交互推进<br>• 单一任务推进通道 | **Effect-TS 管道调度**<br>• 基于 Effect 状态机与 Fiber<br>• 具备多 Agent 协作插件<br>• 异步事件总线分发 | **PTY 进程级执行流**<br>• 专有 Agent 状态机推进<br>• 具备完善的取消与中断处理<br>• 与 xAI 云端紧密协同 | **双层弱解耦循环**<br>• 依赖 Bridge 层胶水代码<br>• 步数控制相对机械<br>• 缺少深层状态隔离 |
| **2. 工具参数多级容错与修复链** | **五级修复链 + 3次熔断防御**<br>• 直解析 → 宽松 JSON → 正则捕获<br>• **Windows 路径反斜杠救赎 (`D:\...`)**<br>• **Schema 类型强转 (`"3"`→`3`)**<br>• 字段级结构化诊断提示回喂<br>• 同工具 3 次失败 Loop Guard 熔断 | **基础错误字符串回传**<br>• 工具执行失败返回原始报错<br>• 依赖模型自身多轮猜测重试<br>• 无本地参数自动强转抢救 | **Effect / Zod Schema 校验**<br>• 严格类型校验，失败即中断<br>• 依靠提示词要求模型纠正<br>• 无多级自愈降级链路 | **结构化诊断 + Loop Guard**<br>• 具备参数类型修正与诊断回喂<br>• 具备防重复调用熔断机制<br>• Windows 路径兼容处理较弱 | **三层基础修复**<br>• 仅修复基础尾逗号与引号<br>• 无法处理 Schema 类型错配<br>• Windows 路径容易反序列化失败 |
| **3. 代码图谱与全景拓扑探索** | **CodeIntel 2.0 深度图谱**<br>• **Tree-Sitter 全栈 AST 分析**<br>• 6 类拓扑流向 (连通/父链/子树等)<br>• Vue/React/Svelte/Astro SFC 支持<br>• `repo_map` 首轮完整目录树不截断 | **文件过滤搜索 (Grep/Glob)**<br>• 无本地 AST 依赖拓扑图谱<br>• 大仓探索极度依赖反复 Grep<br>• 易超出上下文窗口限制 | **基础 LSP + 文件搜索**<br>• 依赖标准 ripgrep 与 LSP 工具<br>• 无全局拓扑连通流向分析<br>• 缺少前端组件专属 AST 图谱 | **xai-codebase-graph**<br>• Rust 实现代码图谱与模糊搜索<br>• 支持文件变更通知 (fsnotify)<br>• 侧重于通用代码符号索引 | **基础词林与哈希向量**<br>• 拓扑维度单一，易产生零命中<br>• 目录树易被强制折叠截断<br>• 前端 SFC 组件解析能力薄弱 |
| **4. 上下文稳定前缀与超高缓存命中** | **全流程 Append-only 保证**<br>• **`user-wrap.md` 动态末尾包裹**<br>• **`sacred_floor` 记忆永不压缩**<br>• 初态 Git 快照防止缓存击穿<br>• 提示词热重载不破坏系统前缀 | **Ephemeral 临时缓存标头**<br>• 依赖 Anthropic 缓存断点指令<br>• 仅限 Claude 系列生效<br>• 中途动态注入容易破坏命中 | **标准消息数组流**<br>• 依赖各服务商原生缓存机制<br>• 动态 Prompt 插入易打乱前缀<br>• 上下文压缩容易丢失关键设定 | **Transcript Compaction**<br>• 基于 SQLite 的日志追踪流<br>• 专有 Compaction Transcripts 机制<br>• 无本地动态 user-wrap 机制 | **基础静态前缀**<br>• 动态 Reminder 插入破坏前缀缓存<br>• 上下文压缩策略较粗糙 |
| **5. 提示词全量自配置与动态热重载** | **完全外置 + 毫秒级 mtime 热重载**<br>• `init.yaml` (身份/安全/前缀)<br>• `rules.yaml` (工作流/执行纪律)<br>• `user-wrap.md` (提问包装模板)<br>• 修改文件**立即生效，无需重启** | **内置提示词 + 部分外置**<br>• 核心 Prompt 固化在 npm 包内<br>• 支持 CLAUDE.md 项目指令<br>• 无法深度修改底层执行纪律 | **外置配置 + 环境变量**<br>• 支持自定义 System Prompt<br>• 规则修改需重新加载会话<br>• 缺少细粒度 YAML 规则分层 | **内置提示词 + 优先级配置**<br>• 拥有成熟的系统提示词层级<br>• 支持规则优先级覆盖<br>• 修改核心提示词需重新构建 | **半静态配置**<br>• 部分规则硬编码于二进制中<br>• 提示词修改需要重启应用 |
| **6. 模型协议、思考档位与账号解耦** | **全协议适配 + 四档思考努力度**<br>• **OpenAI Responses (/v1/responses)**<br>• Chat Completions / Anthropic / Ollama<br>• **思考档位随时切换 (4档/自定义)**<br>• 账号与模型解耦，动态拉取 `/models` | **高度绑定 Anthropic**<br>• 深度绑定 Claude 3.5/3.7 系列<br>• 原生集成 Thinking Budget<br>• 切换第三方模型需复杂代理 | **广泛多模型支持**<br>• 覆盖主流商业与本地模型<br>• 提供模型选择界面<br>• 思考参数需在前端手动调节 | **高度绑定 xAI Grok**<br>• 围绕 Grok 2/3 模型定制<br>• 深度集成专有推理解析<br>• 不便于任意私有化模型接入 | **基础 OpenAI 兼容**<br>• 账号与模型未解耦<br>• 不支持 Responses 协议思考回传<br>• 切换模型容易产生残留旧绑定 |
| **7. CodeExplore 索引性能与共享存储** | **`units.v4.bin` 共享二进制缓存**<br>• **zstd 极速压缩** + Sidecar 落盘<br>• **Rayon 多核并行打分**，查询 <1ms<br>• 进程级共享索引，大仓秒级冷启动<br>• 增量更新与抖动防重排 | **无本地持久化索引**<br>• 每次按需执行实时搜索<br>• 无全局索引落盘与共享机制 | **内存/文件临时缓存**<br>• 基于 Node/Bun 内存缓存<br>• 多项目切换需重新构建索引 | **Rust 进程级图谱索引**<br>• 具备高性能索引与文件监听<br>• 支持内部图谱增量更新 | **单会话 JSON 索引**<br>• 单会话独立索引，内存开销大<br>• 冷启动缓慢，大仓易卡顿 |
| **8. 图谱词林中英双语对齐与高度自定义** | **9 领域内置词林 + 项目专属词林**<br>• 覆盖 AI Agent/全栈/电商/管理/医疗等<br>• 项目级 `<project>/.atomcode/thesaurus/`<br>• **多对多中英文语义双向映射** | **无词林机制**<br>• 依赖 LLM 自身的自然语言理解<br>• 中文业务词难以精准定位英文代码 | **无词林机制**<br>• 依赖符号搜索与 LLM 泛化能力<br>• 缺少业务术语到符号的字典映射 | **无词林机制**<br>• 侧重于代码语法符号图谱分析<br>• 无多领域中英双语概念对齐 | **基础内置词林**<br>• 仅包含少数基础词典<br>• 不支持项目级独立词林扩展 |
| **9. 多项目 MD 知识包与规则最高裁量权** | **4 层多知识包 + 项目级最高裁量权**<br>• `AGENTS.md` / `ATOMCODE.md` (主规范)<br>• `rules.md` (业务规则) · `dbwords.md` (库表)<br>• `glossary.md` (业务词表) · 每轮热重载<br>• **结构化规则严格优先于 System 提示词** | **单文件约定 (CLAUDE.md)**<br>• 仅加载单个 `CLAUDE.md` 文件<br>• 无多维度业务知识包拆分<br>• 规则优先级由模型自行权衡 | **多文件配置支持**<br>• 支持项目规则与说明注入<br>• 规则由上下文拼接加载<br>• 无结构化分级最高裁量权保障 | **AGENTS.md / 规则层级**<br>• 具备完善的项目规则解析<br>• 具备结构化提示词优先级机制 | **单文件匹配**<br>• 只认单个 `.atomcode.md`<br>• 无 DbWords / Rules / Glossary 拆分 |
| **10. 智能体自配置与内置 Teaches 知识库** | **内置 8 模块 Teaches + `jeikcode_config_guide`**<br>• 涵盖提示词/模型/图谱/更新等全套知识<br>• 智能体可**自主调用工具排查系统配置**<br>• 编译期与宿主机资产自动同步 | **静态在线文档**<br>• 依赖用户手动查阅官方手册<br>• 智能体无原生配置自检工具 | **在线文档站**<br>• 社区维护的 Markdown 文档<br>• 智能体无内置配置指导工具 | **内置专有文档**<br>• 包含丰富的用户使用手册<br>• 针对内部环境与命令行规范 | **无内置自查工具**<br>• 依赖外部 README 说明<br>• 遇到配置疑问无法自主检索 |
| **11. Skills 与 MCP 生态集成** | **动态 Skill 挂载 + MCP 标准协议**<br>• `~/.atomcode/skills/` 动态热加载<br>• 标准 MCP (Model Context Protocol) 接入<br>• 渐进式工具加载，不污染核心上下文 | **MCP 深度支持**<br>• 支持官方与社区 MCP Server<br>• 标准 Tool 注入 | **插件与 MCP 生态**<br>• 拥有丰富的插件机制与 MCP 集成<br>• 支持社区扩展工具 | **内置 Tools + MCP**<br>• 支持丰富的内置工具集与扩展<br>• 适配内部工具生态 | **基础 Skills**<br>• 仅支持基础本地 Skill 加载<br>• MCP 隔离与容错较弱 |
| **12. 多语言全栈适配与中英文语义检索** | **全栈语言覆盖 + 双路混合召回**<br>• Vue SFC/TSX/JSX/Svelte/Astro/SCSS/Rust/Go/Java/C++<br>• **BM25 词频 + 概念向量双路并行检索**<br>• 锚点软降权与跨语言代码流动追踪 | **通用文本检索**<br>• 基于文件正则与文本匹配<br>• 缺少组件化语言专用解析器 | **标准 LSP 多语言支持**<br>• 依靠各语言 LSP Server<br>• 混合检索依赖客户端实现 | **Rust / C++ / Python 重点优化**<br>• 针对后端语言语法深度优化<br>• 前端复杂 SFC 组件支持一般 | **基础多语言**<br>• 前端 Vue/React SFC 识别率低<br>• 缺少 BM25 与概念向量混合评分 |

---

## 三、10 大核心硬核机制深度拆解

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
       │ • First-Token 活性超时守护 (60s × 3)              │
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

### 1. 高前缀命中率缓存架构（High Cache Hit Prefix Architecture）
- **前缀字节级不可变性**：JeikCode 严格遵守 Append-only 规则。系统角色定义、`MEMORY` 记忆条目、`SKILLS` 技能描述、`MCP` 工具集以及项目约束（`AGENTS.md`、`rules.md`、`dbwords.md`）紧凑合并在会话首部，并在会话初记录 Git 快照，防止环境差异破坏缓存。
- **`user-wrap.md` 动态末尾包裹**：通过 `{{input}}` 插值仅对末尾真实用户消息动态包装，完全不扰动已缓存的系统前缀与历史轮次。
- **`sacred_floor` 保护区**：执行 `/compact` 上下文压缩时，底层的核心规则与记忆条目受 `sacred_floor` 保护，永不被截断丢弃。
- **UI 纯净还原**：WebUI 与 TUIX 自动对输入执行 unwrap 还原，用户看到的是干净的提问，而大模型接收的是严密包裹的工程指令。

### 2. CodeIntel 2.0 全景图谱与双语词林
- **全栈语言 Tree-Sitter AST 解析**：不仅支持 Rust、Go、Python、Java、C++，更深度适配前端全栈体系（Vue2/3 SFC `template`+`script` 双解析、React TSX/JSX 元素提取、Svelte、Astro、CSS/SCSS/LESS 类选择器、HTML）。
- **6 类图谱拓扑全景**：以锚定点为基准，全景式呈现锚定文件、子树模块、父链依赖、兄弟同级、图连通流向与路径关联词，辅以 BM25 词频与中英双语概念向量混合打分。
- **9 大领域中英双语词林**：内置计算机科学、AI Agent、Web 开发、全栈工程、电商系统、后台管理、医疗、机器人等领域词林，实现中文业务提问与英文代码符号的多对多对齐。
- **`units.v4.bin` 共享二进制索引**：进程级 zstd 压缩缓存 + Rayon 多核并行打分，大仓秒级冷启动，单次检索耗时 <1ms。

### 3. 五级工具参数容错与 3 次熔断防御（吸收 Grok 并超越）
- **五级自愈修复链**：
  1. `直解析`：标准 JSON 解析；
  2. `宽松 JSON 修复`：自动补全尾逗号、未加引号 key、去除 Markdown 代码块包裹标记；
  3. `edit_file 正则提取`：专门抢救编辑工具输出的复杂多行代码块；
  4. `Schema 绑定字符串解码`：纠正转义嵌套的字符串化 JSON；
  5. `Key-Value 兜底`：最终参数名提取保底。
- **Windows 路径反斜杠抢救**：在 Serde 反序列化前精准识别并转义 `D:\project\src` 单反斜杠，避免 Windows 路径被误认为控制字符。
- **Schema 类型层自动强转**：将 `"quantity":"3"` 自动强转为数值 `3`，`"retry":"true"` 强转为布尔 `true`。
- **结构化诊断回喂与 Loop Guard**：失败时回喂字段级 Schema 提示（如 `file_path: string`）；同一工具连续 3 次失败触发 **Loop Guard 熔断**，终止无效重试并强制模型更换解决路径。

### 4. 提示词全量自配置与毫秒级热重载
所有核心系统提示词完全外置于 `~/.atomcode/prompts/`，运行时采用基于 mtime 的零开销缓存检验：
- **`init.yaml`**：定义 Agent 身份、优先级规则、安全隔离与环境配置；
- **`rules.yaml`**：定义工作流反射、代码定位纪律、并发工具调用规范与输出标准；
- **`user-wrap.md`**：自定义提问包裹结构（项目级优先于全局级）；
- **动态生效**：修改上述任意文件，下一轮对话**立即生效，无需重启 Agent**。

### 5. 多项目 MD 知识包体系（Knowledge Packs）与最高裁量权
支持多维度项目级知识库，每次用户回合热重载，**结构化项目规则严格优先于 System 默认规则**：
- **主工程规范**：`AGENTS.md` 或 `ATOMCODE.md`；
- **业务词表 (`Glossary`)**：`.atomcode/glossary.md`，指导 Agent 将业务词扩展为代码符号；
- **业务规则 (`Rules`)**：`.atomcode/rules.md`，明确权限、审批流与业务约束；
- **数据库词典 (`DbWords`)**：`.atomcode/dbwords.md`，明确表结构、字段含义与 SQL 规则。

### 6. 智能体自配置与内置 Teaches 知识库
- 内置 8 大模块化渐进指南（`01_prompts_and_context.md` 至 `08_updates_and_releases.md`），打包期与宿主机配置资产双向同步。
- 原生提供 **`jeikcode_config_guide`** 工具：当 Agent 或用户遇到配置疑问时，Agent 可自主调阅对应章节知识库，提供权威的配置指导与排错建议。

### 7. 全协议模型适配与 4 档思考努力度
- **四协议原生适配**：支持 OpenAI Responses（`/v1/responses`）、Chat Completions、Anthropic 与 Ollama。
- **思考档位灵活调节**：支持 4 档思考努力程度（`reasoning_effort`: `low` / `medium` / `high` / `xhigh`），随时通过 `/effort` 或 WebUI 切换。
- **账号与模型彻底解耦**：`[provider_accounts.*]` 维护凭据，`[models.*]` 维护模型参数；打开 `/modeladd` 或 `/provider` 自动拉取上游 `/models` 列表并智能获焦。

### 8. 独立首 Token 活性超时守护（First-Token Liveness Timeout）
- 针对 DeepSeek-R1、Grok 3 等深度思考模型在生成首个 Token 前长达数十秒的推理静默，建立**独立的 `first_token_timeout` 计时器**（默认 60s × 3 次自动重试），与常规流间隙超时互补，彻底告别假死挂起。

### 9. 远程无头服务 (Serve) 与 WebUI Gateway（吸收 OpenCode 并原生 Rust 化）
- **本地 WebUI**：输入 `/webui` 或 `jeikcode webui`，即可在浏览器中开启可视化控制台。
- **实时 Token 详情浮层**：清晰展示提示词 Token、推理 Token、缓存命中 Token 与 Sacred Floor 保护状态。
- **多实例远程 Serve**：
  ```bash
  # 启动多实例无头服务
  jeikcode serve --host 0.0.0.0 --port 4096 --token sk-my-secret

  # 客户端从局域网直连
  jeikcode attach http://192.168.1.100:4096 --token sk-my-secret
  ```

### 10. 终端防误触与 TTY 前台控制权保护
- **防误触设计**：严格双击 `ESC` 或 `Ctrl+C` 取消当前思考与执行回合并恢复输入框，防止单次误触丢失会话。
- **TTY 前台控制权夺回**：Linux 回合结束后主动夺回 TTY 控制权，彻底忽略 `SIGTTIN`/`SIGTTOU`/`SIGTSTP` 挂起信号，杜绝终端键盘锁死。

---

## 四、安装与快速上手

### 1. 一键安装预编译包（推荐）

前往 [GitHub Releases](https://github.com/jeikl/jeikcode/releases) 下载对应系统的预编译二进制：

```bash
# Linux / macOS 一键安装
curl -fsSL https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install.sh | bash

# Windows PowerShell 一键安装
irm https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install.ps1 | iex
```

### 2. 从源码编译安装

环境需求：**Rust 1.88+**（[rustup.rs](https://rustup.rs/)）

```bash
git clone https://github.com/jeikl/jeikcode.git
cd jeikcode

# 编译并安装二进制到 Cargo bin 目录
cargo install --path crates/atomcode-cli --bin jeikcode --locked

# 验证安装
jeikcode --version
```

### 3. 配置模型与快速启动

在项目目录下直接运行 `jeikcode`，首次启动会引导配置 Provider。配置文件位于 `~/.atomcode/config.toml`：

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

常用命令：
```bash
# 进入指定目录启动
jeikcode -C /path/to/project

# 指定运行模型
jeikcode --model deepseek-reasoner

# Headless 模式（适合脚本与自动化 CI）
jeikcode -p "排查并修复 OAuth 回调 404 错误"

# 继续上一会话
jeikcode -c
```

---

## 五、快捷键与常用命令

### 1. 终端核心快捷键

| 快捷键 | 功能说明 |
| :--- | :--- |
| `Enter` | 发送当前输入消息 |
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

## 六、多项目知识库与规则最高裁量权

JeikCode 具备业内领先的**多知识包加载与项目级规则最高裁量权**。在项目中放置以下 Markdown 文件，系统会在每轮交互前热重载并紧凑合并至前缀首部，**其约束效力严格优先于 System 默认规则**：

```text
your-project/
  ├── AGENTS.md                  # 主工程规范 (技术栈、规范、测试要求)
  ├── user-wrap.md               # 项目级提问动态包裹模板 (含 {{input}})
  └── .atomcode/
      ├── rules.md               # 业务规则与审批约束
      ├── dbwords.md             # 数据库表结构与字段语义
      ├── glossary.md            # 业务专有名词到代码符号的映射表
      └── thesaurus/             # 项目级专属领域词林 (*.txt)
```

---

## 七、开源许可证与社区

本项目基于 [MIT License](LICENSE) 开源，允许完全自由的商业与私有化使用。

<p align="center">
  Crafted with Rust, Tree-Sitter, Ratatui, and Passion for Engineering Excellence.
</p>
