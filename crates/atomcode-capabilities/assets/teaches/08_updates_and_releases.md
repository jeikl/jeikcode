# JeikCode / AtomCode 升级与发版配置指南 (Updates & Releases Guide)

本文档详细说明 JeikCode / AtomCode 的自升级机制、默认更新源、环境变量覆盖、发版打包流程与交叉编译规范。

---

## 1. 核心默认更新源架构

JeikCode 采用集中式端点解析机制（位于 `crates/atomcode-config/src/endpoints.rs`），自升级模块完全脱离上游托管依赖，默认指向 GitHub 主仓与 Release 资产库：

| 配置项 | 默认地址 (Default URL) | 环境变量覆盖 (Env Override) | 用途说明 |
| :--- | :--- | :--- | :--- |
| **版本清单 (Manifest)** | `https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/latest.json` | `ATOMCODE_UPDATE_MANIFEST_URL` | 包含最新版本号、发布时间、全平台 SHA256 校验和与文件大小 |
| **下载基址 (Download Base)** | `https://github.com/jeikl/jeikcode/releases/download` | `ATOMCODE_UPDATE_DOWNLOAD_BASE` | 发版二进制下载基址，拼接规则为 `{base}/{version}/{asset_name}` |
| **官方代码仓 (Repository)** | `https://github.com/jeikl/jeikcode` | - | 官方源码、Issue 与 Release 追踪主页 |

---

## 2. 客户端自升级机制 (`/upgrade`)

### 2.1 触发方式
- **交互式命令**：在 TUI 终端中输入 `/upgrade` 或运行 `atomcode upgrade`（如有）；
- **版本检查**：客户端启动时在后台静默轮询 `latest.json`，发现新版本时在状态栏或终端顶部提示。

### 2.2 `latest.json` 结构规范
升级检查器通过解析 `latest.json` 匹配本地当前平台 Target：
```json
{
  "version": "6.0.26",
  "released_at": "2026-08-25",
  "binaries": {
    "windows-x64": {
      "sha256": "8381f402e8faa3c6187a81721e2099be0e2b9fec99d08c96542511c9b13e6592",
      "size": 63383040
    },
    "linux-x64": {
      "sha256": "d8169e78e5eb1cfe18f669df1722b217db24b1376fb1d0840b9aabec666760cf",
      "size": 53956368
    }
  }
}
```

### 2.3 安全与原子更新保障
1. **SHA256 强校验**：下载二进制后必须计算本地 SHA256，与清单不匹配立即回滚并报错；
2. **原子文件替换**：新二进制下载至临时文件，通过同卷重命名覆盖正在运行的二进制，保证即便中断也不会损坏旧版。

---

## 3. 自定义更新源与多层级配置优先级 (Configuration & Precedence)

JeikCode 严格支持三层更新源配置裁决，优先顺序如下：

```text
┌────────────────────────────────────────────────────────┐
│ 1. 环境变量 (最高优先级，CI / 临时重定向)              │
│    ATOMCODE_UPDATE_MANIFEST_URL                        │
│    ATOMCODE_UPDATE_DOWNLOAD_BASE                       │
├────────────────────────────────────────────────────────┤
│ 2. ~/.atomcode/config.toml 顶层标量配置 (持久化配置)   │
│    update_manifest_url = "..."                         │
│    update_download_base = "..."                        │
│    auto_update = false                                 │
├────────────────────────────────────────────────────────┤
│ 3. 编译期内置默认源 (官方 GitHub 仓库)                 │
│    Manifest: https://raw.githubusercontent.com/...     │
│    Download: https://github.com/jeikl/jeikcode/...     │
└────────────────────────────────────────────────────────┘
```

### 3.1 在 `~/.atomcode/config.toml` 中持久化配置

在 `config.toml` **最顶层（任何 `[` 表头之前）** 或通过顶层键直接指定自定义源与自动更新：

```toml
# ==============================================================================
# 自升级与更新源配置 (顶层标量配置，位于所有 [table] 之前)
# ==============================================================================

# 自定义版本清单 Manifest 地址 (JSON 格式)
update_manifest_url = "https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/latest.json"

# 自定义发版二进制下载基址 (会自动拼接 /<version>/<asset_name>)
update_download_base = "https://github.com/jeikl/jeikcode/releases/download"

# 是否在启动时自动静默应用更新 (默认 false，手动运行 /upgrade 随时可用)
auto_update = false
```

### 3.2 环境变量临时覆盖

在局域网、CI/CD 自动化构建或临时联调时，可通过环境变量立即生效，无需修改 `config.toml`：

```bash
# Linux / macOS
export ATOMCODE_UPDATE_MANIFEST_URL="https://my-internal-repo.corp.com/atomcode/latest.json"
export ATOMCODE_UPDATE_DOWNLOAD_BASE="https://my-internal-repo.corp.com/atomcode/releases"

# Windows PowerShell
$env:ATOMCODE_UPDATE_MANIFEST_URL = "https://my-internal-repo.corp.com/atomcode/latest.json"
$env:ATOMCODE_UPDATE_DOWNLOAD_BASE = "https://my-internal-repo.corp.com/atomcode/releases"
```

---

## 4. 编译发版与交叉编译标准流程

发版前必须严格遵循以下构建链条：

### 4.1 生产前端构建 (WebUI)
```bash
cd webui && npm run build
```
*（必须在 Rust 编译前运行，产物位于 `webui/dist/`，通过 `rust_embed` 直接编译内嵌进二进制）*

### 4.2 Windows 本地 Release 编译
```powershell
cargo build --release --bin atomcode
```

### 4.3 Windows 下交叉编译 Linux musl 静态二进制
利用内置 Zig 工具链与 `.cargo/config.toml` 配置：
```powershell
$env:CC_x86_64_unknown_linux_musl = "E:\code\agents\atomcode\tools\zig-cc.cmd"
$env:CFLAGS_x86_64_unknown_linux_musl = "-fPIC"
$env:AR_x86_64_unknown_linux_musl = "E:\code\agents\atomcode\tools\zig-ar.cmd"
cargo build --release --target x86_64-unknown-linux-musl --bin atomcode
```

### 4.4 归档与 GitHub 发版
```powershell
# 1. 复制产物至 dist/ 目录并生成哈希
Copy-Item .\target\release\atomcode.exe .\dist\atomcode-6.0.26-windows-x64.exe -Force
Copy-Item .\target\release\atomcode.exe .\dist\atomcode-windows-x64.exe -Force
Copy-Item .\target\x86_64-unknown-linux-musl\release\atomcode .\dist\atomcode-6.0.26-linux-x64 -Force
Copy-Item .\target\x86_64-unknown-linux-musl\release\atomcode .\dist\atomcode-linux-x64 -Force

# 2. 更新 latest.json 与 RELEASE_NOTES
# 3. 使用 gh CLI 快速发布 Release
gh release create 6.0.26 .\dist\atomcode-6.0.26-windows-x64.exe .\dist\atomcode-windows-x64.exe .\dist\atomcode-6.0.26-linux-x64 .\dist\atomcode-linux-x64 --repo jeikl/jeikcode --title "6.0.26" --notes-file .\dist\RELEASE_NOTES_6.0.26.md
```
