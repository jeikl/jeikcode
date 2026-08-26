#!/usr/bin/env bash
# =============================================================
# atomcode 自建无感更新发版脚本(不依赖官方更新源)
#
# 用法:
#   ./scripts/release-self-update.sh <version>   # 例: ... 6.0.30
# =============================================================
set -euo pipefail

VERSION="${1:?usage: release-self-update.sh <version>}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# 1. 确保 webui 打包
if [ -d "$ROOT/webui" ]; then
  echo "==> 编译 webui 静态资源..."
  (cd "$ROOT/webui" && npm run build)
fi

# C crates (ring / sqlite / zstd / tree-sitter) look up a target CC, not rustc's linker.
if [ -f "$ROOT/tools/zig-cc.cmd" ]; then
  export CC_x86_64_unknown_linux_musl="$ROOT/tools/zig-cc.cmd"
  export CFLAGS_x86_64_unknown_linux_musl="-fPIC"
  export AR_x86_64_unknown_linux_musl="$ROOT/tools/zig-ar.cmd"
fi

rm -rf dist
mkdir -p dist
echo "==> 交叉编译 release 二进制(按需启用 target; 先 rustup target add <target>)"

build_target() { # <rust-target> <atomcode-asset> <jeikcode-asset>
  local rt="$1" asset1="$2" asset2="$3"
  echo "    building ${rt} -> ${asset1} & ${asset2}"
  cargo build --release --target "$rt" --bin atomcode || {
    echo "    !! 跳过 ${rt}(缺 target 或链接失败: rustup target add ${rt})"; return; }
  local src="target/${rt}/release/atomcode"
  [ -f "${src}.exe" ] && src="${src}.exe"
  cp "$src" "dist/${asset1}"
  cp "$src" "dist/${asset2}"
}

# target key 与 updater detect_target() 完全一致; 本 fork 仅发布两个平台.
build_target x86_64-unknown-linux-musl     "atomcode-${VERSION}-linux-x64"       "jeikcode-${VERSION}-linux-x64"
build_target x86_64-pc-windows-gnu         "atomcode-${VERSION}-windows-x64.exe" "jeikcode-${VERSION}-windows-x64.exe"
build_target x86_64-pc-windows-msvc        "atomcode-${VERSION}-windows-x64.exe" "jeikcode-${VERSION}-windows-x64.exe"

# 生成每个文件的 sha256
(
  cd dist
  for f in *; do
    [ -f "$f" ] && [[ ! "$f" =~ \.sha256$ ]] && sha256sum "$f" > "${f}.sha256"
  done
)

echo "==> 生成 latest.json"
PYTHON_BIN="${PYTHON:-python3}"
if ! command -v "$PYTHON_BIN" >/dev/null 2>&1; then
    if [ -f "/f/Python/Python312/python.exe" ]; then PYTHON_BIN="/f/Python/Python312/python.exe"; elif command -v py >/dev/null 2>&1; then
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

echo "==> 发版产物准备完成"
