#!/usr/bin/env bash
# =============================================================
# atomcode 自建无感更新发版脚本(不依赖官方更新源)
#
# 用法:
#   ./scripts/release-self-update.sh <version>   # 例: ... 0.0.0-dev.1
#
# 原理: 官方 updater 的更新源支持环境变量覆盖
#   ATOMCODE_UPDATE_MANIFEST_URL    (latest.json 清单地址)
#   ATOMCODE_UPDATE_DOWNLOAD_BASE   (二进制下载基址, 会拼 /<version>/<asset>)
# 所以自建仓库只需提供这两个东西, 服务器设置 env 后即可走官方
# 同一套"检测→下载→SHA256 校验→备份替换"的无感升级流程。
#
# 部署侧(每台服务器/机器, 一次性):
#   export ATOMCODE_UPDATE_MANIFEST_URL="https://raw.githubusercontent.com/<you>/<repo>/main/latest.json"
#   export ATOMCODE_UPDATE_DOWNLOAD_BASE="https://github.com/<you>/<repo>/releases/download"
#   (写入 ~/.bashrc / systemd Environment= / setx 永久生效)
# 之后 auto_update=true 即自动无感更新; 或手动 atomcode upgrade。
# =============================================================
set -euo pipefail

VERSION="${1:?usage: release-self-update.sh <version>}"
# ---------- 改成你自己的仓库 ----------
REPO_OWNER="<you>"
REPO_NAME="<repo>"
# -------------------------------------

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# C crates (ring / sqlite / zstd / tree-sitter) look up a target CC, not rustc's linker.
if [ -f "$ROOT/tools/zig-cc.cmd" ]; then
  export CC_x86_64_unknown_linux_musl="$ROOT/tools/zig-cc.cmd"
  export CFLAGS_x86_64_unknown_linux_musl="-fPIC"
  export AR_x86_64_unknown_linux_musl="$ROOT/tools/zig-ar.cmd"
fi

mkdir -p dist
echo "==> 交叉编译 release 二进制(按需启用 target; 先 rustup target add <target>)"

build_target() { # <rust-target> <versioned-asset>
  local rt="$1" asset="$2"
  echo "    building ${rt} -> ${asset}"
  cargo build --release --target "$rt" --bin atomcode || {
    echo "    !! 跳过 ${rt}(缺 target 或链接失败: rustup target add ${rt})"; return; }
  local src="target/${rt}/release/atomcode"
  [ -f "${src}.exe" ] && src="${src}.exe"
  cp "$src" "dist/${asset}"
  # Unversioned alias (same bytes) so older install scripts still resolve.
  local alias="${asset/${VERSION}-/}"
  if [ "$alias" != "$asset" ]; then
    cp "dist/${asset}" "dist/${alias}"
  fi
}

# target key 与 updater detect_target() 完全一致; 本 fork 仅发布两个平台.
# Linux: musl+zig（本机 .cargo/config.toml 已配），静态链到 linux-x64 资产名.
# Windows: gnu（WinLibs）；若本机还有 msvc target 则后写覆盖.
build_target x86_64-unknown-linux-musl     "atomcode-${VERSION}-linux-x64"
build_target x86_64-pc-windows-gnu         "atomcode-${VERSION}-windows-x64.exe"
build_target x86_64-pc-windows-msvc        "atomcode-${VERSION}-windows-x64.exe"

echo "==> 生成 latest.json"
PYTHON_BIN="${PYTHON:-python3}"
if ! command -v "$PYTHON_BIN" >/dev/null 2>&1; then
    if command -v py >/dev/null 2>&1; then
        PYTHON_BIN="py"
    elif [ -f "/f/Python/Python312/python.exe" ]; then
        PYTHON_BIN="/f/Python/Python312/python.exe"
    else
        PYTHON_BIN="python"
    fi
fi

"$PYTHON_BIN" - "$VERSION" <<'PY'
import hashlib, json, os, sys, datetime
version = sys.argv[1]
target_map = {
    f"atomcode-{version}-linux-x64": "linux-x64",
    f"atomcode-{version}-windows-x64.exe": "windows-x64",
}
binaries = {}
for asset, key in target_map.items():
    p = os.path.join("dist", asset)
    if not os.path.exists(p):
        continue
    h = hashlib.sha256(open(p, "rb").read()).hexdigest()
    binaries[key] = {"sha256": h, "size": os.path.getsize(p)}
    print(f"    {key}: {os.path.getsize(p)} bytes, sha256 {h[:12]}...")
if not binaries:
    sys.exit("!! dist/ 下没有任何二进制,检查编译步骤")
manifest = {
    "version": version,
    "released_at": datetime.date.today().isoformat(),
    "binaries": binaries,
}
open("latest.json", "w").write(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
print(f"==> latest.json 已生成({len(binaries)} 个 target)")
PY

cat <<EOF

==> 下一步:
  1. 上传 dist/* 为版本 ${VERSION} 的 Release 资产:
       gh release create ${VERSION} dist/* --title "${VERSION}"
  2. 把 latest.json 推送到仓库 main 分支(updater 从 raw 地址读):
       git add latest.json && git commit -m "release: ${VERSION}" && git push
  3. 服务器已设 env 时后台自动无感更新; 手动: atomcode upgrade
EOF
