# AtomCode Self — 安装教程(各平台安装与源码开发模式)

> 适用范围:本 fork(`jeikls/atomcode`,维护分支 `local-dev`)的**安装**流程。
> 分两种安装方式:
> 1. **一键安装**(二进制,自动指向 local-dev 更新渠道)—— 服务器/普通机器用;
> 2. **源码开发模式**(本地构建,注册为系统 `atomcode` 命令)—— 开发机用。
>
> 配套脚本:`scripts/install-self.{sh,ps1}`、`scripts/dev-install.{sh,ps1}`。

---

## 一、一键安装(二进制,推荐服务器/普通机器)

安装脚本会:自动检测平台 → 从你的仓库 local-dev 渠道下载最新 release → 校验 → 装到 PATH → 提示开启自动更新。

### macOS / Linux / HarmonyOS PC(Unix 系)

```bash
curl -fsSL https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install-self.sh | sh
```

没有 curl 时用 wget:

```bash
wget -qO- https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install-self.sh | sh
```

### Windows

在 PowerShell 里执行:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install-self.ps1 | iex"
```

> Git-Bash / MSYS / Cygwin 用户也可用上面的 Unix 命令(脚本会自动装原生 Windows 版 `.exe`)。

### 安装参数(可选)

| 环境变量 | 作用 | 默认 |
|---|---|---|
| `ATOMCODE_VERSION` | 指定安装版本(tag,如 `0.0.0-dev.2`) | 自动从 latest.json 检测 |
| `ATOMCODE_PREFIX` | 安装目录 | Unix: `/usr/local/bin`(可写时)或 `~/.local/bin`;Windows: `~\.local\bin` |
| `ATOMCODE_MANIFEST_URL` | 覆盖 latest.json 地址 | 内置 local-dev 渠道 |
| `ATOMCODE_DOWNLOAD_BASE` | 覆盖下载基址 | 内置 local-dev 渠道 |

示例:指定版本 + 自定义目录

```bash
ATOMCODE_VERSION=0.0.0-dev.2 ATOMCODE_PREFIX=$HOME/bin \
  curl -fsSL https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install-self.sh | sh
```

### 安装后

```bash
atomcode --version    # 验证
atomcode             # 首次启动: 自动写入词林/mcp.json/.codegraphignore/builtin-tools
```

**开启自动无感更新**(可选,推荐服务器):

```bash
# 编辑 ~/.atomcode/config.toml,加一行:
auto_update = true
```

之后每小时检测 local-dev 渠道,发现新版本自动下载暂存,重启生效。

---

## 二、源码开发模式(开发机 / 本地改代码用)

作用:把本地 `cargo build --release` 的产物注册为系统 `atomcode` 命令,
免敲全量 `target/release/atomcode` 路径;wrapper 设 `ATOMCODE_DEV=1`,
防止自动更新覆盖本地构建。

### Unix(macOS / Linux / HarmonyOS)

```bash
cd /path/to/atomcode
./scripts/dev-install.sh            # 构建 + 注册(首次较慢,之后增量)
# 或
./scripts/dev-install.sh --skip-build   # 已有构建时只注册
```

### Windows

```powershell
cd C:\path\to\atomcode
powershell -ExecutionPolicy Bypass -File scripts\dev-install.ps1
# 或 -SkipBuild 跳过构建
```

### 使用

```bash
atomcode            # 等价于 /path/to/atomcode/target/release/atomcode
```

源码更新后重新跑一次脚本即可(增量构建很快):

```bash
./scripts/dev-install.sh
```

### 卸载 dev wrapper

```bash
./scripts/dev-install.sh --uninstall    # Unix
powershell -ExecutionPolicy Bypass -File scripts\dev-install.ps1 -Uninstall   # Windows
```

---

## 三、安装后验证清单

| 检查 | 命令 | 期望 |
|---|---|---|
| 版本 | `atomcode --version` | 显示安装/构建的版本 |
| 词林已写入 | `ls ~/.atomcode/thesaurus/` | 9 个领域词林文件 |
| 内置工具清单 | `ls ~/.atomcode/builtin-tools.txt` | 存在 |
| MCP 默认接线 | `ls ~/.atomcode/mcp.json` | 存在(首次启动写入) |
| 图谱忽略规则 | `ls ~/.atomcode/.codegraphignore` | 存在(首次启动写入) |
| 更新渠道 | `atomcode upgrade` | 显示"already on latest"(或开始下载) |

---

## 四、常见问题

| 问题 | 原因 / 解决 |
|---|---|
| `curl: command not found` | 用 wget 版本命令,或先装 curl |
| `Permission denied`(Unix 装到 /usr/local/bin) | 脚本自动用 sudo 重试;或设 `ATOMCODE_PREFIX=$HOME/.local/bin` |
| Windows `Move-Item` 失败 | atomcode.exe 正在运行(NTFS 文件锁)→ 关闭后重装 |
| 下载 404 | 该平台还没发布(见发版教程 §三);或指定 `ATOMCODE_VERSION` |
| 首次启动没有写入词林 | 检查 `~/.atomcode/thesaurus/`;手动复制 `crates/atomcode-capabilities/assets/thesaurus/` 下的文件 |
| 想装回官方版 | 用官方 `install.sh` 重装(会覆盖本 fork 二进制;词林等配置保留) |

---

## 五、更新渠道说明(本 fork 内置)

- **默认渠道**:`https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/latest.json` + `https://github.com/jeikl/jeikcode/releases/download`(内置,无需配置);
- **覆盖方式**(按优先级):环境变量 `ATOMCODE_UPDATE_MANIFEST_URL` / `ATOMCODE_UPDATE_DOWNLOAD_BASE` > config.toml `[config] update_manifest_url` / `update_download_base` > 内置;
- 详见 `docs/release-tutorial.md` 与 `README-self.md`。
