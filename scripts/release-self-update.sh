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

mkdir -p dist
echo "==> 交叉编译 release 二进制(按需启用 target; 先 rustup target add <target>)"

build_target() { # <rust-target> <asset-name>
  local rt="$1" asset="$2"
  echo "    building ${rt} -> ${asset}"
  cargo build --release --target "$rt" >/dev/null 2>&1 || {
    echo "    !! 跳过 ${rt}(缺 target: rustup target add ${rt})"; return; }
  local src="target/${rt}/release/atomcode"
  [ -f "${src}.exe" ] && src="${src}.exe"
  cp "$src" "dist/${asset}"
}

# target key 与 updater detect_target() 完全一致(darwin-arm64/x64, linux-x64/arm64,
# windows-x64, ohos-arm64); 没打鸿蒙就去掉最后一行。
build_target x86_64-unknown-linux-gnu      atomcode-linux-x64
build_target aarch64-unknown-linux-gnu     atomcode-linux-arm64
build_target x86_64-apple-darwin           atomcode-darwin-x64
build_target aarch64-apple-darwin          atomcode-darwin-arm64
build_target x86_64-pc-windows-msvc        atomcode-windows-x64.exe
# build_target aarch64-unknown-linux-ohos    atomcode-ohos-arm64

echo "==> 生成 latest.json"
python3 - "$VERSION" <<'PY'
import hashlib, json, os, sys, datetime
version = sys.argv[1]
target_map = {
    "atomcode-darwin-arm64": "darwin-arm64",
    "atomcode-darwin-x64": "darwin-x64",
    "atomcode-linux-x64": "linux-x64",
    "atomcode-linux-arm64": "linux-arm64",
    "atomcode-windows-x64.exe": "windows-x64",
    "atomcode-ohos-arm64": "ohos-arm64",
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
