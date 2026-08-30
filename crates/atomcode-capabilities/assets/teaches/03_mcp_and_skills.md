# 03 - MCP 与 Skills 技能配置指南 (MCP & Skills)

## 1. MCP (Model Context Protocol) 外部工具集成

JeikCode 支持连接任何遵循标准 MCP 协议的外部工具服务。

### 1.1 配置文件定位与优先级
1. **工作区项目级**：`<workspace>/.mcp.json`（仅对当前项目生效，版本控制友好）。
2. **用户全局级**：`~/.atomcode/mcp.json`（全局共享，所有项目均可访问）。
3. 优先级：工作区同名 MCP 服务覆盖全局 MCP 服务。

### 1.2 CLI 添加 MCP（`jeikcode mcp`，最快；仅 stdio）

二进制名 `jeikcode` 与 `atomcode` 等价。**同名会整段覆盖**该键（只写 `command`/`args`，原有 `env` 等字段不保留）。HTTP 型 server 请手写 JSON。

```bash
# 写进项目根 .mcp.json（默认当前目录）
jeikcode mcp add playwright npx @playwright/mcp@latest

# 写进用户级 ~/.atomcode/mcp.json
jeikcode mcp add playwright npx -y @playwright/mcp@latest --global

# 指定项目目录
jeikcode mcp add playwright npx @playwright/mcp@latest -C /path/to/repo

# GitHub 远程 MCP（只写配置，不登录）
jeikcode mcp add-github-oauth github --global
jeikcode mcp login github
jeikcode mcp logout github
```

写完后调用 `jeikcode_config_reload`，或 WebUI/TUI `/mcp reload`，或点侧栏 MCP 刷新按钮。**不要要求用户重启。**

| CLI | 作用 |
| :--- | :--- |
| `jeikcode mcp add <name> <command> [args…]` | 写入 stdio MCP（默认项目 `.mcp.json`） |
| `jeikcode mcp add … --global` | 写入 `~/.atomcode/mcp.json` |
| `jeikcode mcp add … -C <dir>` | 指定项目目录 |
| `jeikcode mcp add-github-oauth [name] [--global]` | 写入 GitHub HTTP+OAuth MCP |
| `jeikcode mcp login <name>` | OAuth 登录（弹浏览器） |
| `jeikcode mcp logout <name>` | 清除已存凭证 |

文件含 `//` 注释时 CLI 拒绝改写（避免抹掉注释），请手改 JSON。

### 1.3 `mcp.json` 标准配置格式

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

HTTP 型只能手写（或 `add-github-oauth`）。项目根 `.mcp.json` 或用户级 `~/.atomcode/mcp.json`，顶层键 `mcpServers`（兼容旧键 `servers`）。同名时项目级覆盖用户级。

### 1.4 斜杠命令（TUI / WebUI）与刷新

| 命令 | TUI | WebUI | 作用 |
| :--- | :--- | :--- | :--- |
| `/mcp` | ✅ | ✅ | 列出 server 及状态（含 failed / blocked） |
| `/mcp reload` | ✅ | ✅ | 重读两份配置并后台重连 |
| `/mcp trust` | ✅ | ✅ | 信任当前项目（项目级 `.mcp.json` 才能连） |
| `/mcp untrust` | ✅ | — | 撤销信任 |
| `/mcp tools <server>` | ✅ | — | 列出该 server 远端工具 |
| `/mcp login/logout <server>` | ✅ | CLI `jeikcode mcp login/logout` | OAuth |
| 侧栏 MCP 刷新按钮 | — | ✅ | 等价 `/mcp reload` |
| 工具 `jeikcode_config_reload` | ✅ | ✅ | Agent 写完配置后调用；当前回合结束后生效 |

**项目级 `.mcp.json` 在未信任项目里不会连**，状态 `blocked: untrusted project`。用户级 `~/.atomcode/mcp.json` 不受此限。

### 1.5 核心交互与生命周期
- **后台连接、首轮就绪保护**：MCP 服务仍在后台异步连接；交互式长驻运行时可先展示界面，但 daemon 的短生命周期 `/chat` 请求会等待 MCP 工具目录完成首次发布。若连接超时或失败，本次消息会明确失败且不会在缺少 MCP 工具的情况下静默发送给模型。
- **项目级共享实例**：同一 driver 进程内，`ProjectMcpPool` 按工作目录缓存 MCP transport（最多 5 个项目，LRU 淘汰并 `shutdown` 旧 stdio 进程树）。TUI 前台/后台 slot、CLI、daemon `/chat` 对同一项目共享一组连接，不会为每个 runtime 重复 `npm exec` 启动 MCP；不同项目各自隔离。`/mcp reload` 先 `shutdown` 池中旧 registry，再对所有相关 runtime 重挂 catalog。
- **单次工具发现**：同一连接的 `tools/list` 结果会缓存，并对并发首次发现进行合并；状态面板与聊天挂载共享该快照，不会反复请求每个 MCP 服务。
- **工具与挂载查询纪律**：当用户询问当前会话“挂载了哪些 MCP 或技能”时，Agent 应优先从当前上下文中检索已挂载的真实工具/技能，若未挂载则如实相告并提供配置指引建议，避免不必要地调用配置指南工具去遍历配置文件。
- **原子发布**：只有初始连接达到成功或明确失败的终态后才把权威工具快照发布给 Agent，避免冷启动时先把空工具集误标记为“已就绪”。
- **热重载命令**：
  - TUI / WebUI 输入 `/mcp` 列出服务器状态；`/mcp reload` 重新读取两份配置、重建共享池并后台重连，同时 remount 前台与同一项目下的后台 runtime。
  - WebUI 侧栏 **MCP** 菜单右上角有刷新按钮，改完 `mcp.json` 后点按即可，不必重启。
  - Agent 写完 `mcp.json` 后必须调用工具 `jeikcode_config_reload`（等价于 `/reload` + `/mcp reload`）。重载在**当前回合结束后**生效，新 MCP 工具在**下一轮用户消息**才挂到模型上。
- **WebUI `/mcp` 子命令**：`/mcp`（状态）、`/mcp reload`（重连）、`/mcp trust`（信任当前项目，与侧栏「信任本项目」相同）。
- **所有权规则**：`ProjectMcpPool` 拥有 transport 生命周期；`CodingRuntime` 仍拥有 model-facing catalog（ADR-0002）。reload 时池先 teardown，再由各 runtime remount adapter；单个会话退出只撤销自己的挂载，不会误关其他会话仍在使用的连接（同项目共享）。

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

### 2.4 生效与浏览
- WebUI / TUI 输入 `/skills` 浏览已加载技能；侧栏「技能」菜单可插入 `/<skill-name>`。
- 新建或修改 `SKILL.md` 后调用 `jeikcode_config_reload`（或 `/reload`），下一轮用户消息即可挂载。提示词 `init.yaml`/`rules.yaml` 仍按 mtime 自动热重载，技能目录不会。

---

## 3. Plugins 插件生态

- 插件主目录：`~/.atomcode/plugins/`。
- **自动初始化**：首次启动自动同步官方插件市场并创建 `.plugin_bootstrap_v2` 标记。
- **命令管理**：TUI `/plugin` 交互式浏览、安装或卸载；CLI `jeikcode plugin`（若已暴露）与市场命名空间 `<namespace>:<skill-name>` 一致。
- 安装/更新插件后同样调用 `jeikcode_config_reload`，不要让用户重启。
