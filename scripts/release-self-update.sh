#!/usr/bin/env bash
# =============================================================
# JeikCode 一键发版脚本
#
# 用法:
#   ./scripts/release-self-update.sh <version>   # 例: ./scripts/release-self-update.sh 6.0.35
#
# 功能:
#   1. 自动更新 Cargo.toml workspace.package.version
#   2. 编译 webui 静态资源
#   3. 交叉编译 linux-x64 + windows-x64 release 二进制
#   4. 生成正确的 latest.json (sha256 + size 各自独立)
#   5. 打印 Release 上传命令 (gh release create)
# =============================================================
set -euo pipefail

VERSION="${1:?用法: $0 <version>   # 例: $0 6.0.35}"

# 校验版本格式 (纯数字+点, 如 6.0.34, 不要 v 前缀)
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$ ]]; then
    echo "错误: 版本号格式不合法: '$VERSION'"
    echo "       请使用纯数字版本如 6.0.35, 不要带 v 前缀"
    exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "=========================================="
echo "  JeikCode 发版 v${VERSION}"
echo "=========================================="
echo ""

# --- 1. 更新版本号 ---
echo "[1/5] 更新版本号到 ${VERSION} ..."
if ! grep -q "^\[workspace\.package\]" Cargo.toml; then
    echo "错误: Cargo.toml 中没有 [workspace.package] 段"
    exit 1
fi
# 用 sed 替换 version 字段 (只改 workspace.package.version)
sed -i "s/^version = \".*\"$/version = \"${VERSION}\"/" Cargo.toml
# 验证是否更新成功
CURRENT=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "//;s/"//')
if [ "$CURRENT" != "$VERSION" ]; then
    echo "错误: Cargo.toml 版本号更新失败"
    exit 1
fi
echo "  -> Cargo.toml version = \"${VERSION}\""

# --- 2. 编译 webui ---
echo ""
echo "[2/5] 编译 webui 静态资源..."
if [ -d "$ROOT/webui" ]; then
    (cd "$ROOT/webui" && npm run build)
    echo "  -> webui/dist/ 已更新"
else
    echo "  !! 跳过: webui/ 目录不存在"
fi

# --- 3. 交叉编译 ---
echo ""
echo "[3/5] 交叉编译 release 二进制..."

# 设置 zig 工具链 (Linux musl cross-compile on Windows)
if [ -f "$ROOT/tools/zig-cc.cmd" ]; then
    export CC_x86_64_unknown_linux_musl="$ROOT/tools/zig-cc.cmd"
    export CFLAGS_x86_64_unknown_linux_musl="-fPIC"
    export AR_x86_64_unknown_linux_musl="$ROOT/tools/zig-ar.cmd"
fi

rm -rf dist
mkdir -p dist
echo "  dist/ 目录已清空"

BUILD_TARGETS=(
    "x86_64-unknown-linux-musl linux-x64"
    "x86_64-pc-windows-gnu windows-x64"
)

for entry in "${BUILD_TARGETS[@]}"; do
    read -r TARGET PLATFORM <<< "$entry"
    echo "  编译 ${TARGET} ..."
    if cargo build --release --target "$TARGET" --bin atomcode 2>&1; then
        SRC="target/${TARGET}/release/atomcode"
        [ -f "${SRC}.exe" ] && SRC="${SRC}.exe"
        cp "$SRC" "dist/atomcode-${VERSION}-${PLATFORM}.exe" 2>/dev/null || \
        cp "$SRC"  "dist/atomcode-${VERSION}-${PLATFORM}"
        echo "    -> dist/atomcode-${VERSION}-${PLATFORM} ($(du -h dist/atomcode-${VERSION}-${PLATFORM} | cut -f1))"
    else
        echo "    !! 跳过 ${TARGET} (缺 target 或链接失败: rustup target add ${TARGET})"
    fi
done

# --- 4. 生成 latest.json ---
echo ""
echo "[4/5] 生成 latest.json ..."

# 用 Node.js 生成 (比 Python 更可靠，项目本身就有 node)
NODE_BIN="${NODE:-node}"
if ! command -v "$NODE_BIN" >/dev/null 2>&1; then
    echo "错误: 找不到 node，请安装 Node.js"
    exit 1
fi

node -e "
const fs = require('fs');
const crypto = require('crypto');
const version = '$VERSION';
const binaries = {};

const targets = [
  ['linux-x64',    'atomcode-' + version + '-linux-x64'],
  ['windows-x64',  'atomcode-' + version + '-windows-x64.exe'],
];

for (const [key, asset] of targets) {
  const p = 'dist/' + asset;
  if (!fs.existsSync(p)) {
    console.log('  !! 缺失: ' + asset);
    continue;
  }
  const data = fs.readFileSync(p);
  const h = crypto.createHash('sha256').update(data).digest('hex');
  binaries[key] = { sha256: h, size: data.length };
  console.log('  ' + key + ': ' + (data.length / 1048576).toFixed(2) + ' MB, sha256 ' + h.slice(0, 12) + '...');
}

if (Object.keys(binaries).length === 0) {
  console.error('!! dist/ 下没有任何二进制文件');
  process.exit(1);
}

const manifest = {
  version: version,
  released_at: new Date().toISOString().slice(0, 10),
  binaries: binaries
};
fs.writeFileSync('latest.json', JSON.stringify(manifest, null, 2) + '\n');
console.log('  -> latest.json 已生成 (' + Object.keys(binaries).length + ' 个 target)');
"

echo ""
cat latest.json

# --- 5. 上传指引 ---
echo ""
echo "[5/5] 发版产物准备完成!"
echo "=========================================="
echo ""
echo "下一步操作:"
echo ""
echo "  # 1. 提交代码"
echo "  git add Cargo.toml latest.json dist/"
echo "  git commit -m \"release: v${VERSION} for linux-x64 and windows-x64\""
echo "  git push origin local-dev"
echo ""
echo "  # 2. 创建 GitHub Release (需要 gh CLI)"
echo "  gh release create ${VERSION} dist/* --title \"v${VERSION}\""
echo ""
echo "  # 3. 验证升级源"
echo "  curl -s https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/latest.json"
echo ""
echo "  # 4. 本机测试升级"
echo "  atomcode upgrade"
echo ""
echo "=========================================="
