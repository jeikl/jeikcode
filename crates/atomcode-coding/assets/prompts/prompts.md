# AtomCode 4 大提示词与规则模板体系说明文档

本文档详细说明了 AtomCode 提示词系统的**四大核心模板**、加载优先级、热重载机制以及从用户发信到请求发出的**完整 8 步组装流水线**。

---

## 一、四大提示词模板架构

所有提示词与规则配置文件均位于全局配置目录（`~/.atomcode/prompts/` 或 `$ATOMCODE_HOME/prompts/`）：

| 模板文件名 | 核心职责 | 说明与包含内容 |
| :--- | :--- | :--- |
| `init.yaml` | **身份与优先级** | 定义 Agent 身份名称、模型占位符、用户/项目指令优先级规则、环境基础声明。 |
| `rules.yaml` | **执行规范与纪律** | 包含完整的 Workflow 流程、并发工具调用纪律、代码定位与业务词典、Doing Tasks 守则、报错处理、输出格式、中文支持、任务清单管理与子代理派生等常量规则。 |
| `内置工具.yaml` | **工具 Schema 定义（文档/种子）** | 内置工具参数说明与中文注释。运行时 schema 来自 Rust `Tool::description()` / `parameters_schema()`，本文件不会覆盖线上 schema。 |
| `内置技能.yaml` | **技能目录与触发规则（文档/种子）** | 已安装/扩展技能的名称、功能描述与语义触发关键词。运行时技能来自 `SKILL.md`，本文件不会覆盖技能目录。 |

---

## 二、加载优先级与零开销热重载（Hot-Reload）

1. **优先级机制**：
   * **最高优先级**：用户自定义 YAML 文件（`~/.atomcode/prompts/*.yaml`）。只要文件存在，系统 100% 优先加载并渲染您的规则。
   * **缺省回退**：若某个 YAML 文件不存在或解析异常，系统会自动平滑降级使用内核内置的默认常量。

2. **零性能开销 + 实时热重载**：
   * **内存缓存**：AtomCode 启动或首次请求时会将解析结果存入内存 `RwLock` 缓存中，后续对话直接从内存读取，**零磁盘 I/O，零 YAML 解析开销**。
   * **时间戳探测**：每次组装时轻量检查文件修改时间（`mtime`）。只要您在 IDE 或文本编辑器中修改并保存任意 YAML 文件，**下一轮对话无需重启即可自动热重载生效**！

---

## 三、发送一条消息后的 8 步提示词封装流水线

当您在客户端（CLI / IDE / WebUI）中键入一条消息并回车后，系统底层会依次执行以下 **8 步封装与构建流程**：

```
[1. 用户输入] ──► [2. Persona (init + rules) = System] ──► [3. 会话上下文 = System]
                                                                      │
[8. 网关发送] ◄── [7. 轮次提醒] ◄── [6. 真实 query] ◄── [4-5. 冻结 User 前缀: 记忆 → 技能 → MCP]
```

1. **第 1 步：输入预处理（Input Preprocessing）**
   * 拦截斜杠命令（如 `/clear`, `/loop`）并解析用户输入中的附件、图片（转为 Base64）与多模态数据。
2. **第 2 步：Persona 组装（`init.yaml` + `rules.yaml` → 唯一的 System 身份块）**
   * **加载 `init.yaml`**：渲染当前 Agent 身份、运行模型（如 `grok-4.6`）与优先级条款。
   * **加载 `rules.yaml`**：动态拼装工作流、并发纪律、代码定位规则（`repo_map` + `code_explore`）、行为守则与任务管理指引。
   * 这两份 YAML 只进 System。不要把记忆 / 技能目录 / MCP 写进这里。
3. **第 3 步：会话上下文注入（Session Context Hook → System）**
   * 自动探测当前工作区路径、操作系统与 Shell；附上 `AGENTS.md` / glossary；git 快照冻在 session 开始。
4. **第 4 步：长期记忆（Memory Hook → 冻结 User 前缀）**
   * 检索跨会话长期持久化记忆（`=== MEMORY ===`），作为第一条 synthetic User，插在 System 之后、真实 query 之前。
   * 用户自定义内容，落在 `sacred_floor` 内，压缩不会删。
5. **第 5 步：技能目录 + MCP（同样是冻结 User 前缀）**
   * 扫描已安装技能目录（`SKILL.md`）拼装 `=== AVAILABLE SKILLS ===`。`内置技能.yaml` 是文档/种子，不驱动运行时目录。
   * 已连接 MCP 服务器的说明包裹在 `<mcp-server-instructions>` 中。
   * 顺序：记忆 → 技能 → MCP，全部在真实 query 之上。
6. **第 6 步：真实用户 query**
   * 永远是这一轮的最后一条 User。Grok / Responses 允许它前面有连续 User 块。
7. **第 7 步：实时轮次提醒（Turn Status Reminder）**
   * 日期锚点包裹在 `<system-reminder>` 中，插在真实 query **上面**（不是下面）。
8. **第 8 步：工具 Schema 挂载与网络报文封包**
   * Function Calling JSON Schema **不是**从 `内置工具.yaml` 读的。运行时对每个已挂载工具调用 `Tool::description()` + `parameters_schema()`，再封进 `tools` 列表。
   * OpenAI 兼容协议会把连续 System 合成一条；记忆/技能/MCP 已经是 User，不会被卷进 persona。Responses 协议保持连续 User 分条发送（与 Grok Build 同构）。
