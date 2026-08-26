# 03 - MCP 与 Skills 技能配置指南 (MCP & Skills)

## 1. MCP (Model Context Protocol) 外部工具集成

JeikCode 支持连接任何遵循标准 MCP 协议的外部工具服务。

### 1.1 配置文件定位与优先级
1. **工作区项目级**：`<workspace>/.mcp.json`（仅对当前项目生效，版本控制友好）。
2. **用户全局级**：`~/.atomcode/mcp.json`（全局共享，所有项目均可访问）。
3. 优先级：工作区同名 MCP 服务覆盖全局 MCP 服务。

### 1.2 `mcp.json` 标准配置格式

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "E:/code"],
      "env": {
        "DEBUG": "false"
      },
      "disabled": false
    },
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": {
        "GITHUB_PERSONAL_ACCESS_TOKEN": "ghp_xxxxxxxxxxxxxxxxxxxx"
      },
      "disabled": false
    }
  }
}
```

### 1.3 核心交互与生命周期
- **后台连接、首轮就绪保护**：MCP 服务仍在后台异步连接；交互式长驻运行时可先展示界面，但 daemon 的短生命周期 `/chat` 请求会等待 MCP 工具目录完成首次发布。若连接超时或失败，本次消息会明确失败且不会在缺少 MCP 工具的情况下静默发送给模型。
- **项目级共享实例**：daemon 的非同步 `/chat` 路径按工作目录缓存 MCP Registry，同一项目的短生命周期聊天复用同一组连接与工具目录，不会为每条消息重复启动 MCP 进程；不同项目各自隔离。缓存最多保留 5 个项目，按最近使用时间淘汰，并主动取消被淘汰实例的连接任务。WebUI 的同步 `/live` 路径使用自身的长驻 CodingRuntime，在该运行时生命周期内持续复用其 MCP Registry。
- **单次工具发现**：同一连接的 `tools/list` 结果会缓存，并对并发首次发现进行合并；状态面板与聊天挂载共享该快照，不会反复请求每个 MCP 服务。
- **原子发布**：只有初始连接达到成功或明确失败的终态后才把权威工具快照发布给 Agent，避免冷启动时先把空工具集误标记为“已就绪”。
- **热重载命令**：在 TUI 界面中输入 `/mcp reload` 可重新读取配置文件并重新建立连接。
- **所有权规则**：daemon 拥有并负责取消共享 Registry；单个短生命周期会话退出只撤销自己的工具挂载，不会关闭其他会话仍在使用的 MCP 连接。

---

## 2. Skills 技能系统

Skills 允许以纯 Markdown + YAML 描述的方式教给 Agent 专精的工程工作流。

### 2.1 技能存储目录与加载优先级
1. **工作区技能**：`<workspace>/.skills/<skill-name>/SKILL.md`（最高优先级）。
2. **全局用户技能**：`~/.atomcode/skills/<skill-name>/SKILL.md`。
3. **插件市场技能**：`~/.atomcode/plugins/marketplaces/...`（以 `<namespace>:<skill-name>` 命名）。

### 2.2 `SKILL.md` 标准格式与规范

每个技能是一个独立目录，核心入口文件必须命名为 `SKILL.md`：

```markdown
---
name: my-feature-expert
description: 专精于某业务模块的排查、测试与重构规范。当用户询问某功能实现或重构时激活此技能。
---

# 技能执行指南

## 1. 核心流程
1. 首先使用 `code_explore` 搜索模块关键词。
2. 遵循测试驱动原则，修改前先核对对应单元测试。

## 2. 约束规则
- 保持向后兼容。
- 遵循领域特定错误码规范。
```

### 2.3 渐进式子目录设计（推荐）
为了避免单个 `SKILL.md` 过长占用模型上下文，可在技能目录下建立分层：
- `SKILL.md`：核心流程与触发描述（精简，约 100~300 tokens）。
- `references/`：放详细技术参考、API 文档、架构说明，由 Agent 按需使用 `read_file` 查阅。
- `scripts/`：放辅助脚本或模板。

---

## 3. Plugins 插件生态

- 插件主目录：`~/.atomcode/plugins/`。
- **自动初始化**：首次启动自动同步官方插件市场并创建 `.plugin_bootstrap_v2` 标记。
- **命令管理**：在 TUI 中使用 `/plugin` 即可交互式浏览、安装或卸载第三方插件与技能集。
