# JeikCode / AtomCode 项目全局开发约束

## 1. 架构分层与状态所有权

当前 coding agent 的唯一运行时调用链路：

```text
CLI / TUI / daemon / background / ACP / clix
                    │
                    ▼
       CodingRuntimeHandle / DriverCommand
                    │
                    ▼
          atomcode-coding (CodingRuntime)
                    │
                    ▼
          atomcode-kernel (Neutral Agent)
```

- **`atomcode-kernel` (L0)**：纯净中立的 Agent 执行循环，不包含任何 coding 业务、provider 选择、session 或文件操作特化。
- **`atomcode-capabilities` (L1)**：提供可复用的中立工具（文件读写、Bash、CodeIntel 图谱检索、JeikCode 配置指南等）与会话 Hook，严格保持无前端、无 L2 反向依赖。
- **`atomcode-coding` (L2)**：业务生命周期的唯一所有者（CodingRuntime）。管理 Provider 组装、Prompt Persona、任务规划、子代理调度、会话压缩与终端状态机。
- **Driver / UI 层**：负责交互、输入输出、终端渲染与通信协议，禁止另建第二套 Live Agent 生命周期。

---

## 2. 核心机制与开发不变量

### 2.1 提示词热重载与优先级裁决 (Precedence)
- **动态生效 (Live)**：`prompts/init.yaml`（身份/环境）、`prompts/rules.yaml`（工作流/工具纪律）以及 `user-wrap.md`（提问包装模板）基于 mtime 自动热重载，修改立即生效无须重启；
- **种子说明文件 (Seed Docs)**：`root_docs_*` 仅作为开发者参考文档，严禁加载进模型上下文；
- **用户提问包装 (`user-wrap.md`)**：支持全局（`~/.atomcode/`）与项目级（`./.atomcode/` 或 `./`）配置，通过 `{{input}}` 动态包裹用户最后一条真实提问，项目级覆盖全局；
- **项目级规则最高裁量权**：凡是带有结构化标记的项目规范（`=== ... (*.md) ===` 或 `-----**.md------`，如 `AGENTS.md`、`ATOMCODE.md`、`rules.md`、`dbwords.md` 等），在模型决策中**严格优先于 System 默认规则**。

### 2.2 KV Cache 前缀稳定性与上下文压缩保护 (Prompt Caching & Sacred Floor)
- 会话前缀必须保持 **Append-only** 字节级不可变性；
- `SessionContextHook` 注入的项目指令与环境事实在会话首部紧凑合并；Git 状态维持会话初快照以防止缓存击穿；
- 记忆（`memory.md`）作为 `synthetic User` 注入，受 `sacred_floor` 保护，压缩时永不丢失。

### 2.3 CodeIntel 图谱探索与词林双语检索 (Thesaurus)
- 功能与链路探索优先使用 `repo_map`（全景文件树）与 `code_explore`（调用图谱+源码），禁止多轮低效的 grep-and-wander；
- 中文代码检索依赖 `~/.atomcode/thesaurus/*.txt` 领域词林进行双语多对多对齐，新增领域术语应优先补充词林词典。

### 2.4 模型与提供商解耦 (Provider & Models)
- 采用 `[provider_accounts.*]`（账号/凭据）与 `[models.*]`（模型参数/协议）解耦架构；
- 支持 `reasoning_history`（`"include"` / `"exclude"`）、`reasoning_effort` 档位切换与 `vision_preprocessor_provider` 视觉代答。

---

## 3. ~/.atomcode 配置与 Teaches 知识库同步规范

`crates/atomcode-capabilities/assets/teaches/`（及宿主机 `~/.atomcode/teaches/`）中的渐进式模块化文档是编译后成品中 **`jeikcode_config_guide` 工具的直接知识源**：

1. **同变同更硬性约束**：凡修改了 `~/.atomcode` 相关配置项、解析逻辑、参数默认值、超时机制、模型协议或目录结构，**必须同步修改对应的 `teaches/` 分类文档**（`01_prompts_and_context.md` 至 `07_project_constraints_and_rules.md`）；
2. **构建打包自动同步**：`crates/atomcode-cli/build.rs` 会在编译时自动抓取宿主机 `~/.atomcode` 最新资产注入成品，并保持配置更新的交互式勾选与用户模型保护机制。

---

## 4. 验证与交付

- 修改过程中优先运行单元测试；涉及多 crate 或公共协议变更时运行 `cargo check --workspace`；
- 修改提示词、配置项或文档时，必须核对 `teaches/` 与实现代码的一致性。
