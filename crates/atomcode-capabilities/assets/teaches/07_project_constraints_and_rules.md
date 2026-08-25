# 07 - 项目级约束、业务规则与知识包配置指南 (Project Constraints & Rules)

为了确保 JeikCode / AtomCode 能够精准遵循每个项目的专有架构、代码规范、业务逻辑与数据库结构，系统支持**三层指令继承体系**与**三大增量业务知识包**。

---

## 1. 三层项目指令继承体系（Instructions Tiers）

在每轮会话开始时，系统通过 `SessionContextHook` 自动读取并按层级注入项目指令。指令优先级高于模型的默认系统规则：

| 层级 | 查找文件名（按优先级命中首个） | 作用范围与说明 |
| :--- | :--- | :--- |
| **1. 全局层 (Global)** | `~/.atomcode/ATOMCODE.md` | 全局基础指令（跨所有项目生效）。 |
| **2. 项目层 (Project)** | `1. .atomcode.md`<br>`2. ATOMCODE.md`<br>`3. AGENTS.md`<br>`4. CLAUDE.md`<br>`5. claude.md` | **项目专属核心规范**（代码风格、架构边界、分支管理、提交规范等）。推荐在项目根目录创建 `AGENTS.md` 或 `ATOMCODE.md`。 |
| **3. 用户层 (User)** | `.atomcode.user.md` | **开发者个人本地偏好**（不提交至 Git，仅当前机器项目生效）。 |

---

## 2. 三大增量业务知识包（Knowledge Packs）

系统支持在项目内放置 3 类专属知识包 Markdown 文件，它们与 `AGENTS.md` 共存互补，每轮对话**实时热重载**：

### 2.1 领域专有名词词汇表（Domain Glossary）
- **候选文件路径（命中首个生效）**：
  - `.atomcode/glossary.md`
  - `.atomcode/domain-glossary.md`
  - `docs/domain-glossary.md`
  - `docs/glossary.md`
  - `domain-glossary.md`
  - `DOMAIN.md`
- **核心作用**：将业务术语映射为代码类型、接口或方法别名。当用户使用业务黑话提问时，Agent 自动展开为精确符号。

### 2.2 业务与组织规则（Business Rules）
- **候选文件路径（命中首个生效）**：
  - `.atomcode/rules.md`
  - `.atomcode/business-rules.md`
  - `docs/rules.md`
  - `docs/business-rules.md`
  - `rules.md`
- **核心作用**：规定组织架构、审批流、状态机流转、业务互斥等业务硬性约束，防止 Agent 编写违背产品规则的代码。

### 2.3 数据库表与字段词汇（DB Words / Schema）
- **候选文件路径（命中首个生效）**：
  - `.atomcode/dbwords.md`
  - `.atomcode/db-words.md`
  - `.atomcode/schema.md`
  - `docs/dbwords.md`
  - `docs/db-words.md`
  - `dbwords.md`
- **核心作用**：记录核心数据库表、关键字段、索引设计及中英文昵称对照，编写 SQL 或 ORM 时优先依据此文件。

---

## 3. 项目级专属能力扩展

每个独立项目还可在项目根目录下定义专享能力（自动覆盖全局配置）：

| 配置项 | 项目路径 | 作用 |
| :--- | :--- | :--- |
| **项目级专属技能** | `<workspace>/.skills/<skill-name>/SKILL.md` | 当前项目专有的工程重构或测试工作流。 |
| **项目级专属 MCP** | `<workspace>/.mcp.json` | 仅在当前项目生效的外部 MCP 工具服务。 |
| **项目级专属词林** | `<workspace>/.atomcode/thesaurus/*.txt` | 当前项目特定业务名词的双语检索词林。 |
| **项目级索引忽略** | `<workspace>/.codegraphignore` | 符号索引与图谱构建时忽略的特定文件或目录。 |

---

## 4. 项目配置最佳实践模板

在项目根目录快速初始化以下文件以获得最佳体验：
```text
my-project/
├── AGENTS.md                 # 核心架构边界与开发约束
├── .atomcode/
│   ├── rules.md              # 业务流程与领域逻辑规范
│   ├── dbwords.md            # 数据库核心表名与字段对照
│   └── thesaurus/
│       └── biz.txt           # 中英业务名词词林（如：会员等级 = member_level）
├── .skills/
│   └── deploy-check/
│       └── SKILL.md          # 专属部署检查技能
└── .mcp.json                 # 项目专属数据库或 API 连接
```
