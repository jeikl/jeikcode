# AtomCode Self — 自建 fork 版说明

> 本 fork(`origin = jeikls/atomcode`,基于官方 `atomgit_atomcode/atomcode` fork,维护分支 **`local-dev`**)在官方基础上做了大量工程化增强。本文档说明**与官方的差异**:配置、代码、功能三个维度,以及自建无感更新的使用方式。

---

## 一、核心差异一句话

| 维度 | 官方 | 本 fork(local-dev) |
|---|---|---|
| 更新源 | 官方 `main` 分支 + 官方 releases | **内置 `local-dev` 分支 + 你的 releases**(env/config 可覆盖) |
| 工具参数容错 | 基础 repair | **五级修复链 + schema 类型层 + 结构化诊断 + 失败计数熔断** |
| 代码检索 | 基础词林 + 哈希向量 | **六类目录全景 + BM25 + 概念向量 + 查询缓存 + 并行评分** |
| 索引性能 | 单会话独立索引 + JSON | **进程级共享索引 + 二进制缓存(zstd)+ sidecar 落盘 + 增量保存** |
| 默认配置 | 官方模板 | **内嵌自建词林(9 个领域)+ builtin-tools + `prompts/` 模板,首次启动自动写入(不覆盖已有文件)** |
| 跨平台 | Windows/Linux/macOS/HarmonyOS | 同 + **路径/BOM/大小写全面适配修复** |

---

## 二、功能差异(本 fork 新增/增强)

### 1. 工具参数修复链(repair.rs)— 对标 grok 后的超越

- **五级本地修复**:直解析 → repair_json(尾逗号/未引号 key/单引号/markdown fence)→ edit_file 专用提取器 → schema 绑定字符串化解码 → key-value 兜底;
- **Windows 路径预转义**:`D:\test` 单反斜杠盘符在 serde 误解码前抢救(key 限定 `file_path`/`path`,幂等,不误伤 `\n`/`\t`);
- **schema 类型层修复**:`"quantity":"3"` → 3、`"retry":"true"` → true(含 string 的 union 视为歧义跳过);
- **结构化诊断回喂**:修复失败 → `Deny` + 字段级 schema 描述(`field: type`);
- **失败计数熔断**:同工具连续拒绝 3 次 → 明确 "STOP re-emitting, change approach"。

### 2. 代码检索(code_explore / codeintel)— 全覆盖混合检索

- **六类目录全景**:锚定/子树/父链/兄弟/图连通/路径词,带分数 + grep 兜底 + 溢出折叠;
- **BM25 召回**(`ATOMCODE_EXPLORE_BM25=1`)+ **概念向量路**(`ATOMCODE_EXPLORE_CONCEPT=1`,中文↔英文语义轴);
- **锚点软降权**:命名平淡的核心文件(`run_loop.rs`/`turn.rs`)不再被硬门槛挡掉;
- **查询结果缓存**(fingerprint+query+scope+max_files+开关 六元 key)+ 会话去重 root 隔离;
- **前端全覆盖**:Vue2/3/Svelte/Astro SFC 双解析(script+template 元素)、React/TSX JSX 元素、CSS/SCSS/LESS 样式类、HTML;
- **性能**:rayon 并行评分 + 进程级共享 CodeIndex + units.v4.bin(zstd)冷启动 + stats/dirindex sidecar 落盘。

### 3. 自建无感更新(本 fork 最核心的部署差异)

**更新源内置绑定** `https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/latest.json` + `https://github.com/jeikl/jeikcode/releases/download`,解析顺序:

```
env  ATOMCODE_UPDATE_MANIFEST_URL / ATOMCODE_UPDATE_DOWNLOAD_BASE   (最高)
  >  config.toml [config] update_manifest_url / update_download_base (可选)
  >  内置 local-dev 分支渠道(默认)
```

- `auto_update = true` 时:每小时检测 → 下载 → SHA256 校验 → 备份 `.bak` → 原子替换,**重启自动生效**,跨平台(含鸿蒙 `ohos-arm64`);
- 移除了官方的 `signer_available()` 门控(官方渠道专用,会阻塞自建源);
- 发版脚本:`scripts/release-self-update.sh <version>`(编译各平台 → 生成 latest.json → 上传指引)。

### 3.5 首 token 活性超时(fork 独有)

模型发送后迟迟不吐任何 token(高延迟 / 隐藏推理静默过长)时,不再干等满 300s 流空闲超时:

- 独立于 `stream_timeout`(300s 管流内任意两次事件间间隔):**`first_token_timeout` 只对每个 round 的首个 token 计时**(首个 reasoning/tool-call/text),一旦有内容即失效,交回流空闲超时 —— 互补不重复;
- 超时(尚未产 token)自动重发,至多 `first_token_timeout_retries` 次;满预算以 **"模型延迟过高,请稍后再试"** 终止回合;
- 可在 `config.toml [coding] first_token_timeout_secs / first_token_timeout_retries` 配置,env `ATOMCODE_FIRST_TOKEN_TIMEOUT_SECS` / `ATOMCODE_FIRST_TOKEN_RETRIES` 覆盖;`secs=0` 关闭该臂。

### 4. 默认配置扩充(首次启动自动写入)

首次启动幂等 seed(不覆盖用户已有文件):

- `CodeExploreTool::new`:`~/.atomcode/thesaurus/` 9 个领域词林(agent_core / ai_agent / computer_science / web_http / fullstack_dev / ecommerce / admin_system / medical / robotics)、`builtin-tools.txt`、`mcp.json`、`.codegraphignore`;
- Persona 组装:`~/.atomcode/prompts/` 写入 `init.yaml`(JeikCode 身份)+ `rules.yaml`(工作流/并发纪律,含 `code_explore` 禁止根 path)+ `prompts.md` + `内置工具.yaml` / `内置技能.yaml`(文档/种子,不覆盖线上 schema)。已有文件永不覆盖。

### 5. 跨平台修复(与官方代码差异)

- `canonical()` 统一剥离 Windows `\\?\` 扩展长度前缀(修 strip_prefix/scope/去重/落盘污染);
- 6 处源码读取点剥离 UTF-8 BOM(Windows 记事本/VS Code 带 BOM 文件);
- 路径匹配统一 `→\` + 小写,落盘 key 保留原大小写,大小写语义按平台处理。

---

## 三、配置差异(config.toml)

| 字段 | 官方 | 本 fork |
|---|---|---|
| `auto_update` | 默认 false | 同(默认 false,**你手动开**) |
| `[config] update_manifest_url` | ❌ 无 | ✅ 新增:覆盖更新源(env 优先) |
| `[config] update_download_base` | ❌ 无 | ✅ 新增 |
| `[tools.tool_output] no_fold_tools` | ❌ 无 | ✅ 默认白名单(含 `fetch_output` / `repo_map` / `code_explore` 等) |
| `[tools.tool_output] max_bytes` | ✅ | ✅ 默认 65536 |
| `[tools.bash]` 超时 | 180 / 300 | **60 / 90**(default / silent_kill) |
| `[datalog] enabled` | 默认 false | **默认 true** |
| `[ui] auto_copy_on_select` | 非 Windows 默认 true | **默认 false** |
| `~/.atomcode/prompts/` | ❌ 无 | ✅ 首次启动写入可编辑模板 |
| 词林 | 官方内置 | ✅ 9 个领域词林自动 seed |

**推荐白名单**(已是代码默认,写不写行为一致):
```toml
[tools.tool_output]
no_fold_tools = ["fetch_output", "repo_map", "code_explore", "find_symbol", "trace_chain", "blast_radius", "web_fetch", "web_search"]

[config]  # 可选,想换渠道时
# update_manifest_url = "https://.../latest.json"
# update_download_base = "https://.../releases/download"

auto_update = true  # 打开后重启自动无感更新(默认关,按需开)
```

---

## 四、与官方代码的对应关系

| 官方文件 | 本 fork 增强 |
|---|---|
| `crates/atomcode-capabilities/src/tools/repair.rs` | 类型层 + 诊断 + 计数熔断(61 单测) |
| `crates/atomcode-capabilities/src/codeintel/{explore,index,mod,bilingual_nlp}.rs` | 六类目录/BM25/概念向量/共享索引/二进制缓存/seed 词林 |
| `crates/atomcode-capabilities/src/codeintel/retrieval/` | **新增**:stats/bm25/concepts/dirindex |
| `crates/atomcode-capabilities/src/codeintel/queries/tsx.scm` | **新增**:JSX 元素捕获(独立于 TS) |
| `crates/atomcode-config/src/{endpoints,config/mod}.rs` | 更新源指向 local-dev + 覆盖字段 |
| `crates/atomcode-updater/src/lib.rs` | env>config>内置 解析 |
| `crates/atomcode-cli/src/main.rs` | 移除 signer 门控 |
| `crates/atomcode-capabilities/assets/` | **新增**:内嵌词林 + builtin-tools |
| `scripts/release-self-update.sh` | **新增**:自建发版脚本 |
| `docs/performance-multi-session-guide.md` | **新增**:性能优化指南 |

---

## 四·五、项目级自定义指令:与官方 md 的差异(⚠ 更新时别忘)

**官方**:项目指令只认三/四层 Markdown 约定 —— `<project>/.atomcode.md` / `ATOMCODE.md` / `AGENTS.md` / `CLAUDE.md`(`instructions.rs` 查找顺序,首中即止),外加用户层 `.atomcode.user.md`。

**本 fork 额外支持:Project Knowledge Packs(多 md 加载)** —— 提交 `1ff6bc68f`(2026-08-03,"project knowledge packs with per-turn hot-reload",作者 Jeik 即本 fork 维护者),实现在 `crates/atomcode-capabilities/src/session/instructions.rs`:

> 在 AGENTS 级指令之外,**附加加载三组 knowledge md,每组多路径首中即止,互不替代**;每次用户回合热重载(无大小上限);术语表还指导 find_symbol 做符号升级定位(业务词 → 代码符号)。

| 知识包 | 用途 | 候选路径(首中即止) |
|---|---|---|
| **Glossary(业务词表)** | 业务术语 → 代码别名;提示模型"用户说业务词时先扩成代码词再 find_symbol" | `.atomcode/glossary.md` · `.atomcode/domain-glossary.md` · `docs/domain-glossary.md` · `docs/glossary.md` · `domain-glossary.md` · `DOMAIN.md` |
| **Rules(业务规则)** | 组织结构/审批流/业务约束,实现时视为权威 | `.atomcode/rules.md` · `.atomcode/business-rules.md` · `docs/rules.md` · `docs/business-rules.md` · `rules.md` |
| **DbWords(库表/字段词)** | 数据库 schema / 表 / 字段的业务词 | `.atomcode/dbwords.md` · `.atomcode/db-words.md` · `.atomcode/schema.md` · `docs/dbwords.md` · `docs/db-words.md` |

> **这就是你记忆中的 "db.md"**:不是单个 `db.md` 文件,而是本 fork 的 **DbWords 知识包**(候选含 `.atomcode/dbwords.md` / `.atomcode/db-words.md` / `.atomcode/schema.md`)—— 官方只加载一个 md,本 fork 加载**多组多个 md**。这是官方分支之外的 fork 独有改动,升级/合入官方时不要丢。
>
> 补充:此外还有项目级词林 `<project>/.atomcode/thesaurus/*.txt`(explore.rs:311-319,查询侧业务词扩展,与 knowledge packs 互补:词林管检索命中、knowledge 管上下文注入)。

## 四·六、用户独有默认配置(新电脑参考模板,⚠ 更新时别忘)

以下配置都是**官方分支之外**的 fork 独有项,默认值已在代码里内置(写不写行为一致),但**在新电脑上需要知道自己有哪些可调项**,故完整列出(仅结构,不含密钥):

```toml
auto_update = false
auto_commit = false
keep_interrupted_context = true   # Ctrl-C 保留部分上下文; false 则回滚该回合

# ── fork 独有:codeintel 图谱工具可见性 ──────────────────────────
[codeintel]
# "unified"(默认)= 只暴露 repo_map + code_explore(报告/探索两件套)
# "full" = 暴露全部底层图谱工具(find_symbol/trace_*/blast_radius/file_deps…)
mode = "unified"

[codeintel.ignore]
enabled = true                                   # 编译产物/缓存过滤开关
ignore_file = "~/.atomcode/.codegraphignore"     # 全局忽略规则文件
patterns = [                                     # 额外自定义忽略通配符
    "*cache*", "*temp*", "node_modules/", "dist/", "target/",
    "bin/", "obj/", "__pycache__/", "*.min.js", "*.map",
]

# ── fork 独有:工具输出折叠阈值 + 不折叠白名单 ──────────────────
[tools.tool_output]
max_bytes = 65536                                # 超此字节折叠为头尾预览;0=禁用折叠
no_fold_tools = ["fetch_output", "repo_map", "code_explore", "find_symbol",
                 "trace_chain", "blast_radius", "web_fetch", "web_search"]

[tools.bash]
default_timeout_secs = 60
max_timeout_secs = 1800
silent_kill_secs = 90

# ── 官方字段但默认值常被自定义(保持手写以便新机可调) ──────────
[tools.todo]
enabled = true
eager = "auto"

[lsp]
enabled = false                                  # fork 默认关 LSP(快)
auto_detect = false

[subagent]
enabled = true
initial_turns = 4
max_turns = 12
max_concurrent = 3
timeout_secs = 900
max_rounds = 200

[loop_config]
max_rounds = 100
[coding]
max_rounds = 200
first_token_timeout_secs = 60    # fork 独有: 首个模型 token 的等待秒数,0 关闭
first_token_timeout_retries = 3  # fork 独有: 首token超时后重发次数,满则提示"模型延迟过高"

[datalog]
enabled = true
dir = "~/.atomcode/datalog"
[notifications]
enabled = true
min_duration_secs = 8
terminal = true
system = true
bell = true
background_only = true

[ui]
theme = "auto"
auto_copy_on_select = false
auto_copy_code_blocks = false
ai_session_naming = true
terminal_status_glyph = true
```

> **⚠ 这两节(`[codeintel]` 模式 / `[codeintel.ignore]` 存在但官方无对应字段,以及 `[tools.tool_output] no_fold_tools`)都是 fork 独有**:升级/合入官方、或换新电脑重建配置时,记得带过来;不写则用代码内置默认值(行为不变),但你就看不到这些可调项了。API key 等敏感字段不在本模板内,请自行从原 `~/.atomcode/config.toml` 迁移。

---

## 五、部署与更新

```bash
# 1. 发版(在本地开发机,推送你的仓库)
./scripts/release-self-update.sh 0.0.0-dev.1
gh release create 0.0.0-dev.1 dist/* --title "0.0.0-dev.1"
git add latest.json && git commit -m "release: 0.0.0-dev.1" && git push

# 2. 服务器/各机器:安装后打开自动更新(默认已指向 local-dev 渠道)
atomcode  # 首次启动自动写入词林/builtin-tools/prompts
# 手动: atomcode upgrade
# 自动: config.toml 加 auto_update = true → 重启无感更新
```

**更新流程**:检测(每小时)→ 下载 → SHA256 校验 → 备份 `.bak` → 原子替换 → 下次启动自动生效(跨平台,含鸿蒙)。

---

## 六、版本与分支

- **开发分支**:`local-dev`(本 fork 长期维护线,本文档所有差异的载体);
- **远程**:`origin = https://atomgit.com/jeikls/atomcode.git`、`upstream = 官方`;
- **同步官方**:`git fetch upstream && git merge upstream/main`(fork 关系,官方新特性可合入);
- **合入官方**(如需要):从 `upstream/main` 拉 `feat/<topic>` 分支,cherry-pick 增强,提 MR。
