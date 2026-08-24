# root_docs_prompts.md

本文档不加载进模型。仅供人类阅读。

说明 AtomCode 提示词系统的**两类文件**、加载优先级、热重载，以及从用户发信到请求发出的组装流水线。

---

## 一、两类文件

所有文件均位于全局配置目录（`~/.atomcode/prompts/` 或 `$ATOMCODE_HOME/prompts/`）。

### 进模型（live）

| 模板文件名 | 核心职责 | 加载行为 |
| :--- | :--- | :--- |
| `init.yaml` | 身份、优先级、系统提醒、MCP 隔离、上下文管理、Windows 平台 | **全部字段进 System**。缺文件或缺键则回退编译期默认。 |
| `rules.yaml` | Workflow、工具纪律、代码定位、Doing Tasks、报错、输出、任务/提问/子代理/评审/技能 | **全部字段进 System**。文件存在则整段替换编译期 `RULES`（不 merge）。 |

### 不进模型（root_docs 种子文档）

| 模板文件名 | 核心职责 |
| :--- | :--- |
| `root_docs_prompts.md` | 本说明。 |
| `root_docs_内置工具.yaml` | 内置工具参数的人类注释。运行时 schema 来自 Rust `Tool::description()` / `parameters_schema()`。 |
| `root_docs_内置技能.yaml` | 技能说明草稿。运行时目录来自已安装的 `SKILL.md`。 |

---

## 二、加载优先级与热重载

1. **优先级**：`~/.atomcode/prompts/init.yaml` 与 `rules.yaml` 只要存在且能解析，就作为 live 来源。解析失败则回退编译期默认。`root_docs_*` 永不参与组装。
2. **热重载**：对 live YAML 检查 mtime；保存后下一轮对话生效。`root_docs_*` 改了也不会进模型。
3. **首次 seed**：上述五份文件写入 `prompts/`，已有文件永不覆盖。

---

## 三、发送一条消息后的组装流水线

```
[1. 用户输入] ──► [2. Persona (init + rules) = System] ──► [3. 会话上下文 = System]
                                                                      │
[8. 网关发送] ◄── [7. 轮次提醒] ◄── [6. 真实 query] ◄── [4-5. 冻结 User 前缀: 记忆 → 技能 → MCP]
```

1. **输入预处理**：斜杠命令、附件、图片。
2. **Persona**：`init.yaml`（身份/优先级/安全/环境）+ `rules.yaml`（执行规则）→ 唯一 System 身份块。不要把记忆 / 技能目录 / MCP 写进这两份 live YAML。
3. **会话上下文**：工作区、OS、Shell、`AGENTS.md` / glossary；git 快照冻在 session 开始。
4. **长期记忆**：`=== MEMORY ===` 冻结 User，压缩不删。
5. **技能目录 + MCP**：扫描 `SKILL.md` 拼 `=== AVAILABLE SKILLS ===`。`root_docs_内置技能.yaml` 不驱动目录。MCP 包在 `<mcp-server-instructions>`。顺序：记忆 → 技能 → MCP。
6. **真实用户 query**：这一轮最后一条 User。
7. **轮次提醒**：日期 `<system-reminder>` 插在真实 query 上面。
8. **工具 Schema**：对每个已挂载工具调用 `Tool::description()` + `parameters_schema()`。**不是**从 `root_docs_内置工具.yaml` 读的。

---

## 四、live YAML 字段必须都能渲染

`init.yaml` / `rules.yaml` 种子里的每个业务字段都有对应的反序列化键和渲染出口。不要往 live 文件里加加载器不认识的键：多出来的键会被忽略，不会进模型。
