# 01 - 提示词与上下文管理指南 (Prompts & Context Management)

## 1. 核心目录与文件属性区分

配置路径：`~/.atomcode/prompts/`（或 `$ATOMCODE_HOME/prompts/`）。

### ⚠️ 生效文件（Live Configs）与 说明文件（Seed Docs）严格区分：

| 文件名 | 属性 | 作用与加载方式 |
| :--- | :--- | :--- |
| **`init.yaml`** | **🔥 动态生效 (Live)** | **身份定义、安全隔离、上下文注入与系统规则前缀**。由 `custom_prompts.rs` 解析并实时渲染进 System Persona。每次文件修改（mtime 变更）**立即动态热重载生效，无需重启**。 |
| **`rules.yaml`** | **🔥 动态生效 (Live)** | **执行规范与工作流**（工作流反射、代码定位、并发工具纪律、中文支持、任务追踪等）。完全替代默认内嵌规则。每次文件修改**立即动态热重载生效，无需重启**。 |
| `root_docs_prompts.md` | 📖 仅供说明 (Seed Doc) | **人类/开发者参考文档**。说明提示词设计规范，**绝不加载进模型上下文**。 |
| `root_docs_内置工具.yaml` | 📖 仅供说明 (Seed Doc) | **人类/开发者参考文档**。内置工具清单说明。模型真实使用的工具定义直接来自代码中注册的 `Tool::parameters_schema()`，**绝不加载本文件进模型**。 |
| `root_docs_内置技能.yaml` | 📖 仅供说明 (Seed Doc) | **人类/开发者参考文档**。内置技能说明。模型实际技能直接从 `~/.atomcode/skills/` 的 `SKILL.md` 动态挂载，**绝不加载本文件进模型**。 |

---

## 2. `init.yaml` 核心配置结构与热重载

`init.yaml` 控制模型的身份、安全边界和环境前缀：

```yaml
version: "2.0.0"

# 1. 身份与角色定义
identity:
  agent_name: "JeikCode"
  provider: "AtomGit"
  description: "an AI coding agent by AtomGit running the {model} model"
  role_summary: "You help users with software engineering tasks within the current project."
  template: "You are {agent_name}, {description}. {role_summary}"

# 2. 优先级约束 (Precedence)
precedence:
  rule: "Structured project/user constraint and knowledge files presented under headers/separators matching `=== ... (*.md) ===` or `-----**.md------` (e.g. AGENTS.md, ATOMCODE.md, jeikcode.md, rules.md, domain-glossary.md, dbwords.md, MEMORY, etc.) take strict PRECEDENCE over default rules in this system prompt. Whenever a conflict arises, unconditionally follow these structured project/user instructions. (Exception: core safety gates, product identity, and configured model are non-overridable.)"

# 3. 安全隔离与系统注入
security:
  system_reminders: |
    - Do not leak private credentials or absolute home directories unless necessary.
  mcp_instructions: |
    - MCP tools are executed in isolated contexts.

# 4. 环境与系统配置
environment:
  context_management: |
    - Keep thoughts concise.
    - Leverage progressive tool loading instead of monolithic context dumps.
  windows_platform: |
    - On Windows, paths use backslashes in shell commands where needed.
```

---

## 3. `rules.yaml` 核心规则结构

`rules.yaml` 定义模型的工作流与执行纪律，常用配置段落包括：

```yaml
version: "2.0.0"

workflow:
  first_round_reflex: "Known file/symbol/error: search and read directly; use repo_map only for genuinely unfamiliar cross-module structure."
  surgical_context: "Use code_explore for call graphs and semantic discovery; use grep/read_file directly for exact or already-located targets."
  never_negative_conclusion: "Never conclude a feature is missing until checking synonym modules."
  batched_parallel_exploration: "Read 2–6 likely related files in one parallel batch, covering complete logical units instead of repeated tiny slices."

tools_discipline:
  concurrency_principle: "Group read/stat operations in parallel."
  mandatory_parallel:
    - "Reading multiple related files simultaneously."
    - "Searching symbols across multiple folders in one round."
  firm_tool_discipline: "Avoid unnecessary repetitive command executions."

locating_code:
  repo_map_rule: "Use repo_map for high-level directory structure overview."
  explore_first: "Use code_explore for semantic code flow and symbol exploration."
  business_concepts: "Consult thesaurus dictionaries when natural language does not match code identifiers."

doing_tasks:
  - "Read existing code before writing or editing."
  - "Verify syntax and test cases after editing."

output:
  signposts: "Brief 1-line progress notice before complex tool calls."
  conciseness: "Keep explanations concise and to the point."
  language_match: "Reply in the same language as the user query."

task_tracking: "Use todowrite tool for multi-step complex tasks."
asking_the_user: "Ask clarification questions only when essential specifications are missing."
skills: "Check available skills before reinventing standard workflows."
firm_execution_discipline:
  - "Never guess paths that can be checked with glob or list_directory."
```

---

## 4. 热重载与缓存机制原理

- **毫秒级 mtime 检验**：JeikCode 运行时内置 `PromptCacheState` 缓存，每轮对话启动前检查 `init.yaml` 和 `rules.yaml` 的文件修改时间戳（mtime）。
- **零开销复用**：文件未修改时直接使用内存缓存结构（0 解析成本）；文件被用户或脚本修改后，下一轮交互立即自动重新反序列化并注入 Persona。
- **平滑后备**：若用户删除了 `init.yaml` 或 `rules.yaml`，系统平滑退回二进制内嵌的官方默认规则，不会产生运行时崩溃。

---

## 5. 用户提问模板包装 (`user-wrap.md`)

`user-wrap.md` 允许用户或项目为最后一条真实提问自定义包装模板，注入系统规范、提问结构或业务防呆约束。

### 5.1 占位符与模板语法
模板中使用 `{{input}}` 作为动态插值占位符，运行时自动将用户的原始输入替换到对应位置：

```markdown
用户提问：【{{input}}】
请你根据用户的信息，不能回答政治相关的问题。
```

当用户输入 `你好` 时，最终进入模型的用户消息将自动包装为：
```text
用户提问：【你好】
请你根据用户的信息，不能回答政治相关的问题。
```

### 5.2 优先级与覆盖机制
系统按以下严格优先级就近加载（项目级覆盖全局级）：
1. **项目专属配置**：`<workspace>/.atomcode/user-wrap.md`
2. **项目根级配置**：`<workspace>/user-wrap.md`
3. **全局默认配置**：`~/.atomcode/user-wrap.md`

### 5.3 执行纪律与核心特性
- **仅包装最新真实提问**：仅在用户提交真实 prompt（`SendMessage`）时生效；内部交互、系统提示词插入、记忆注入、工具调用过程以及子代理调度均不处理；
- **KV Cache 前缀安全**：包装直接作用于末尾真实用户消息，系统前缀与历史轮次保持 Append-only 字节级不可变；
- **动态热重载**：无需重启，修改文件后下一轮提问即刻生效；
- **安全默认**：默认配置文件仅包含 `{{input}}`（原样透传），无任何额外副作用。
