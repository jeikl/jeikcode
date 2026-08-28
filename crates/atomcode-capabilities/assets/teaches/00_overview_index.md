# JeikCode 配置知识库导航索引 (Overview Index)

本目录为 JeikCode 全局与工作区配置体系的渐进式分层指南，旨在以最低 Token 成本让 AI Agent 及开发者快速获取精准配置语法与参数。

## 1. 核心指南模块速查表

| 编号 | 模块分类 (`topic`) | 对应文件路径 | 核心覆盖范围 |
| :--- | :--- | :--- | :--- |
| **01** | `prompts` | `teaches/01_prompts_and_context.md` | `prompts/init.yaml` 与 `rules.yaml` 动态热重载机制、生效文件 vs 种子说明文件区分、上下文与身份注入 |
| **02** | `models` / `providers` | `teaches/02_models_and_providers.md` | `config.toml` 顶层标量位置规范、账号与模型解耦架构、思考档位、思考历史回传、Token 限制、视觉预处理 |
| **03** | `mcp` / `skills` | `teaches/03_mcp_and_skills.md` | `mcp.json` 结构与重载、`skills/<name>/SKILL.md` 编写与目录优先级、插件市场与命名空间 |
| **04** | `thesaurus` / `cilin` | `teaches/04_thesaurus_and_retrieval.md` | 词林 `thesaurus/*.txt` 格式、双语代码检索相关性、领域专业词库增强 `code_explore` / `repo_map` |
| **05** | `tools` / `timeouts` | `teaches/05_tools_and_timeouts.md` | Bash 命令超时与静默终止、工具输出 64KB 折叠与白名单、Todo 清单策略、子代理并发与轮次、代理设置 |
| **06** | `directories` / `files` | `teaches/06_directories_and_system.md` | `~/.atomcode/` 下所有目录与文件作用、生命周期、安全边界与运维清理建议 |
| **07** | `project` / `rules` | `teaches/07_project_constraints_and_rules.md` | 项目级约束（AGENTS.md、ATOMCODE.md、.atomcode.user.md）、业务规则（rules.md）、名词表（glossary.md）、数据库结构（dbwords.md） |
| **08** | `updates` / `upgrade` | `teaches/08_updates_and_releases.md` | 默认更新源配置（GitHub 主仓/latest.json）、自升级机制（/upgrade）、环境变量覆盖与 Windows/Linux 交叉编译发版流程 |

---

## 2. 渐进式按需读取建议

- **想修改系统提示词或 Agent 规则**：优先查阅 `topic: "prompts"`。
- **想添加模型提供商、配置 API Key 或调整思考强度**：优先查阅 `topic: "models"`。
- **想接入外部 MCP 工具或编写 Agent Skill**：优先查阅 `topic: "mcp"` 或 `topic: "skills"`。
- **想优化中文代码检索准确率**：优先查阅 `topic: "thesaurus"`。
- **想调整执行超时、大输出截断或网络代理**：优先查阅 `topic: "tools"`。
- **想了解 `~/.atomcode` 某个未知目录/文件的作用**：优先查阅 `topic: "directories"`。
- **想了解项目级约束规范与业务/数据库知识包**：优先查阅 `topic: "project"`。
- **想了解自升级源、版本检查、/upgrade 命令或编译发版流程**：优先查阅 `topic: "updates"`。

---

## 3. 写完配置后如何让运行时生效

修改 `config.toml`、`mcp.json` 或 skills 后，**不要要求用户重启 JeikCode**。按界面调用对应重载：

| 入口 | 作用 |
| :--- | :--- |
| 工具 `jeikcode_config_reload` | Agent 写完配置后调用；当前回合结束后重载，新 MCP 工具在下一轮用户消息可用 |
| WebUI / TUI `/reload` | 从磁盘重载 `config.toml`，并重挂 MCP / skills |
| WebUI / TUI `/mcp` | 列出 MCP 服务器状态 |
| WebUI / TUI `/mcp reload` | 重新读取 `mcp.json` 并后台重连 |
| WebUI 侧栏 MCP 菜单的刷新按钮 | 与 `/mcp reload` 相同，点按即可，无需重启 |

提示词 `prompts/init.yaml`、`prompts/rules.yaml` 仍按 mtime **自动热重载**，不必走上述命令。
