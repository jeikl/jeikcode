# AtomCode Self — 发版教程(发布新版本到你的仓库)

> 适用范围:本 fork(`jeikls/atomcode`,维护分支 `local-dev`)的**版本发布**流程。
> 发版后,所有已部署的机器(设了 `auto_update = true` 或手动 `atomcode upgrade`)会自动从你的仓库**检测 → 下载 → SHA256 校验 → 备份替换 → 重启生效**。
>
> 配套脚本:`scripts/release-self-update.sh`(交叉编译 + 生成 latest.json)。

---

## 一、发版前置条件

| 项 | 要求 |
|---|---|
| 本机 | 已 clone `jeikls/atomcode`,能 `cargo build` |
| 交叉编译 target | 需发几个平台就 `rustup target add` 几个(见下) |
| 发布渠道账号 | 有 `jeikls/atomcode` 仓库的 push / release 权限 |
| 工具 | `python3`(生成 latest.json)、`gh` 或网页(上传 Release) |

**可选 target 列表**(按需添加):

```bash
rustup target add x86_64-unknown-linux-gnu      # Linux x64
rustup target add aarch64-unknown-linux-gnu     # Linux arm64
rustup target add x86_64-apple-darwin           # macOS x64(需 macOS 或交叉工具链)
rustup target add aarch64-apple-darwin          # macOS arm64
rustup target add x86_64-pc-windows-msvc        # Windows x64
# 鸿蒙(可选): rustup target add aarch64-unknown-linux-ohos
```

> 提示:macOS 交叉编译需要额外工具链;若只想发 Linux/Windows,删掉脚本里对应的
> `build_target` 行即可,`latest.json` 只会包含实际生成的平台。

---

## 二、发版步骤(标准流程)

### 1. 更新版本号 + 提交

```bash
cd /path/to/atomcode
# 更新 workspace 版本(Cargo.toml workspace.package.version)
# 例如: 0.0.0-dev.2

git add -A && git commit -m "release: 0.0.0-dev.2" && git push origin local-dev
```

> 版本号必须是**合法 semver**(如 `0.0.0-dev.2`、`1.2.3`),`is_newer` 按数字比较;
> **不要**带 `v` 前缀进 version 字段(latest.json 的 `version` 是纯数字段)。

### 2. 运行发版脚本

```bash
./scripts/release-self-update.sh 0.0.0-dev.2
```

脚本会:
1. 交叉编译各平台 release 二进制到 `dist/`;
2. 计算每个二进制的 `sha256` + `size`,生成 `latest.json`(只含实际编译出的平台);
3. 打印下一步上传指引。

输出示例:

```
==> 交叉编译 release 二进制(按需启用 target; 先 rustup target add <target>)
    building x86_64-unknown-linux-gnu -> atomcode-linux-x64
    ...
==> latest.json 已生成(5 个 target)
    linux-x64: 12.3MB bytes, sha256 3f9a...
```

### 3. 上传 Release 资产

```bash
# 用 gh(推荐)
gh release create 0.0.0-dev.2 dist/* --title "0.0.0-dev.2"

# 或用网页: 仓库 → Releases → Draft a new release → tag 0.0.0-dev.2
# 把 dist/ 下所有 atomcode-* 文件拖上去
```

> **关键**:Release 的 **tag 名必须与版本号一致**(如 `0.0.0-dev.2`),因为
> updater 的下载 URL 是 `https://atomgit.com/jeikls/atomcode/releases/download/<version>/atomcode-<version>-<target>`。

### 4. 推送 latest.json 到 local-dev 分支

```bash
git add latest.json && git commit -m "release: 0.0.0-dev.2" && git push origin local-dev
```

> **关键**:updater 从 `https://raw.atomgit.com/jeikls/atomcode/raw/local-dev/latest.json`
> 读清单 —— **latest.json 必须推到 local-dev 分支**(不是 main,不是 Release 资产)。

### 5. 验证

```bash
# 本机模拟升级(会下载刚发的版本并替换当前二进制)
atomcode upgrade

# 或检查清单可读
curl -s https://raw.atomgit.com/jeikls/atomcode/raw/local-dev/latest.json | head -5
```

---

## 三、latest.json 格式说明

```json
{
  "version": "0.0.0-dev.2",
  "released_at": "2026-08-17",
  "binaries": {
    "darwin-arm64": { "sha256": "<sha256>", "size": 12345678 },
    "darwin-x64":   { "sha256": "<sha256>", "size": 12345678 },
    "linux-x64":    { "sha256": "<sha256>", "size": 12345678 },
    "linux-arm64":  { "sha256": "<sha256>", "size": 12345678 },
    "windows-x64":  { "sha256": "<sha256>", "size": 12345678 },
    "ohos-arm64":   { "sha256": "<sha256>", "size": 12345678 }
  }
}
```

- `binaries` 的 key 必须与 updater `detect_target()` 完全一致:`darwin-arm64/x64`、`linux-x64/arm64`、`windows-x64`、`ohos-arm64`;
- `sha256` 必须与上传的二进制**完全一致**(升级时校验,不匹配拒收并自动回滚);
- 只列出实际发布的平台即可(缺失平台 = 该平台不升级)。

---

## 四、发版后的更新生效

| 方式 | 触发时机 |
|---|---|
| **自动无感更新** | 机器上 `~/.atomcode/config.toml` 设 `auto_update = true` → 每小时检测,发现新版本下载并暂存,下次启动应用 |
| **手动更新** | 任何机器执行 `atomcode upgrade`(走同一渠道) |
| **强制重装** | `atomcode upgrade --force` |

自动更新流程:检测 → 下载 → SHA256 校验 → 备份 `.bak` → 原子替换 → 重启生效。
若新版本损坏,启动时自动回滚到 `.bak`(官方机制,自建渠道同样享受)。

---

## 五、常见问题

| 问题 | 原因 / 解决 |
|---|---|
| `atomcode upgrade` 报 "already on latest" | 本地版本 ≥ latest.json 版本 → 确认 latest.json 已推 local-dev;或 `--force` |
| 下载 404 | Release tag 或资产名与版本不一致(检查 §三 URL 拼法) |
| 校验失败拒绝安装 | sha256 与上传二进制不匹配 → 重新生成 latest.json 再推 |
| 某个平台不升级 | latest.json 缺该平台 entry → 用脚本重新生成(会包含实际编译的平台) |
| 服务器无 `curl`/`wget` | 安装脚本需要其一;更新机制内部用 reqwest,与 shell 无关 |

---

## 六、回滚

- **升级后想退回上一版**:`atomcode upgrade rollback`(官方机制,`/.bak` 切换);
- **发错版本**:删掉对应 Release tag + 把 latest.json 改回上一版本并推 local-dev(机器会按版本号比较自动停在旧版)。
