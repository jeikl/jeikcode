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
| 默认配置 | 官方模板 | **内嵌自建词林(9 个领域)+ builtin-tools 清单,首次启动自动写入** |
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

**更新源内置绑定** `https://raw.atomgit.com/jeikls/atomcode/raw/local-dev/latest.json` + `https://atomgit.com/jeikls/atomcode/releases/download`,解析顺序:

```
env  ATOMCODE_UPDATE_MANIFEST_URL / ATOMCODE_UPDATE_DOWNLOAD_BASE   (最高)
  >  config.toml [config] update_manifest_url / update_download_base (可选)
  >  内置 local-dev 分支渠道(默认)
```

- `auto_update = true` 时:每小时检测 → 下载 → SHA256 校验 → 备份 `.bak` → 原子替换,**重启自动生效**,跨平台(含鸿蒙 `ohos-arm64`);
- 移除了官方的 `signer_available()` 门控(官方渠道专用,会阻塞自建源);
- 发版脚本:`scripts/release-self-update.sh <version>`(编译各平台 → 生成 latest.json → 上传指引)。

### 4. 默认配置扩充(首次启动自动写入)

`CodeExploreTool::new` 时幂等 seed(不覆盖用户已有文件):

- `~/.atomcode/thesaurus/` 9 个领域词林:agent_core / ai_agent / computer_science / web_http / fullstack_dev / ecommerce / admin_system / medical / robotics;
- `~/.atomcode/builtin-tools.txt` 内置工具清单(查哪些工具可加 `no_fold_tools` 白名单)。

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
| `[tools.tool_output] no_fold_tools` | ❌ 无 | ✅ 新增:工具输出不折叠白名单 |
| `[tools.tool_output] max_bytes` | ✅ | ✅(预览已改为随阈值各半保留) |
| 词林 | 官方内置 | ✅ 9 个领域词林自动 seed |

**推荐白名单**(`~/.atomcode/config.toml`):
```toml
[tools.tool_output]
no_fold_tools = ["repo_map", "code_explore", "find_symbol", "trace_chain", "blast_radius", "web_fetch", "web_search"]

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

## 五、部署与更新

```bash
# 1. 发版(在本地开发机,推送你的仓库)
./scripts/release-self-update.sh 0.0.0-dev.1
gh release create 0.0.0-dev.1 dist/* --title "0.0.0-dev.1"
git add latest.json && git commit -m "release: 0.0.0-dev.1" && git push

# 2. 服务器/各机器:安装后打开自动更新(默认已指向 local-dev 渠道)
atomcode  # 首次启动自动写入词林/builtin-tools
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
