#!/bin/sh
# AtomCode Self — fork 版一键安装脚本(指向本 fork 的 local-dev 渠道)
#
#   curl -fsSL https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install-self.sh | sh
#
# Env overrides:
#   ATOMCODE_VERSION   release tag 安装(默认 latest; 格式 v0.0.0 需与 latest.json 一致)
#   ATOMCODE_PREFIX    安装目录(默认 /usr/local/bin 可写则用,否则 ~/.local/bin; HarmonyOS 非 root → ~/.local/bin)
#   ATOMCODE_MANIFEST_URL / ATOMCODE_DOWNLOAD_BASE  覆盖更新渠道(可选)
#
# 与官方 install.sh 结构一致(平台检测/PATH 写入/Windows 提示),仅下载源指向 fork。
set -eu

# fork 渠道(默认: 本 fork 的 local-dev 分支 + releases)
MANIFEST_BASE="${ATOMCODE_MANIFEST_URL:-https://raw.githubusercontent.com/jeikl/jeikcode/local-dev}"
REPO_BASE="${ATOMCODE_DOWNLOAD_BASE:-https://github.com/jeikl/jeikcode/releases/download}"
DEFAULT_VERSION="v0.0.0-dev.1"

# --- detect platform ---
uname_s=$(uname -s)
uname_m=$(uname -m)
ext=""

case "$uname_s" in
    Darwin) os="darwin" ;;
    Linux)  os="linux"  ;;
    HarmonyOS) os="ohos" ;;
    MSYS*|MINGW*|CYGWIN*) os="windows"; ext=".exe" ;;
    *) echo "Unsupported OS: $uname_s (Windows 用户请用 install-self.ps1)"; exit 1 ;;
esac

case "$uname_m" in
    arm64|aarch64) arch="arm64" ;;
    x86_64|amd64)  arch="x64"   ;;
    *) echo "Unsupported arch: $uname_m"; exit 1 ;;
esac

# --- pick install dir ---
if [ -n "${ATOMCODE_PREFIX:-}" ]; then
    PREFIX="$ATOMCODE_PREFIX"
elif [ "$os" = "ohos" ] || [ "$os" = "windows" ]; then
    PREFIX="$HOME/.local/bin"
elif [ -w /usr/local/bin ] 2>/dev/null; then
    PREFIX="/usr/local/bin"
elif [ "$(id -u)" -eq 0 ]; then
    PREFIX="/usr/local/bin"
else
    PREFIX="$HOME/.local/bin"
fi
mkdir -p "$PREFIX"

# --- download tools ---
if command -v curl >/dev/null 2>&1; then
    _fetch="curl -sL --connect-timeout 5 --max-time 10"
    _down="curl -fL --progress-bar -o"
elif command -v wget >/dev/null 2>&1; then
    _fetch="wget -qO- --timeout=10 --tries=1"
    _down="wget --show-progress -O"
else
    echo "Error: need curl or wget." >&2
    exit 1
fi

# --- resolve version from fork latest.json ---
if [ -n "${ATOMCODE_VERSION:-}" ]; then
    VERSION="$ATOMCODE_VERSION"
else
    echo "==> Detecting latest version (${MANIFEST_BASE}/latest.json)"
    VERSION=$($_fetch "$MANIFEST_BASE/latest.json" | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
    [ -n "$VERSION" ] || VERSION="$DEFAULT_VERSION"
fi

# --- download ---
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
DEST="$TMP/atomcode${ext}"
BIN_NAME="atomcode-${VERSION}-${os}-${arch}${ext}"
URL="${REPO_BASE}/${VERSION}/${BIN_NAME}"

echo "==> Downloading $BIN_NAME"
echo "    from $URL"
$_down "$DEST" "$URL"

# Sanity check: must be a real binary, not an HTML 404 page
if head -c 4 "$DEST" | grep -q "<" 2>/dev/null; then
    echo "Error: download looks like an HTML page, not a binary."
    echo "       The release may not exist for your platform, or the URL is wrong."
    echo "       URL: $URL"
    exit 1
fi
chmod +x "$DEST"

# --- install ---
TARGET="$PREFIX/atomcode${ext}"
if [ "$os" = "windows" ]; then
    echo "==> Installing to $TARGET"
    if ! mv "$DEST" "$TARGET"; then
        echo "Error: could not write $TARGET." >&2
        echo "       If atomcode is already running, close it and re-run this installer." >&2
        exit 1
    fi
elif [ -e "$TARGET" ] && [ ! -w "$TARGET" ]; then
    echo "==> Installing to $TARGET (sudo required)"
    sudo mv "$DEST" "$TARGET"
elif [ ! -w "$PREFIX" ]; then
    echo "==> Installing to $TARGET (sudo required)"
    sudo mv "$DEST" "$TARGET"
else
    echo "==> Installing to $TARGET"
    mv "$DEST" "$TARGET"
fi

ALIAS="$PREFIX/jeikcode${ext}"
cp -f "$TARGET" "$ALIAS" 2>/dev/null || sudo cp -f "$TARGET" "$ALIAS" 2>/dev/null || true
echo ""
echo "Installed: $TARGET"
echo "Alias:     $ALIAS"
"$TARGET" --version 2>/dev/null || true

if [ "$os" = "windows" ]; then
    echo ""
    echo "Note: installed for this Unix shell (MSYS/MinGW/Git-Bash/Cygwin)."
    echo "      For a system-wide Windows install use instead:"
    echo "      powershell -c \"irm https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install-self.ps1 | iex\""
fi

# --- PATH (与官方一致) ---
case ":$PATH:" in
    *":$PREFIX:"*) ;;
    *)
        LINE="export PATH=\"$PREFIX:\$PATH\""
        RC=""
        if [ -n "${ZSH_VERSION:-}" ] || [ "$(basename "${SHELL:-}")" = "zsh" ]; then
            RC="$HOME/.zshrc"
        elif [ -n "${BASH_VERSION:-}" ] || [ "$(basename "${SHELL:-}")" = "bash" ]; then
            RC="$HOME/.bashrc"
        fi
        if [ -n "$RC" ]; then
            if [ -f "$RC" ] && grep -qxF "$LINE" "$RC" 2>/dev/null; then
                :
            else
                echo "" >> "$RC"
                echo "# Added by AtomCode Self installer" >> "$RC"
                echo "$LINE" >> "$RC"
                echo ""
                echo "Added $PREFIX to PATH in $RC"
                echo "    source $RC"
            fi
        else
            echo "Note: $PREFIX is not in your PATH. Add: $LINE"
        fi
        ;;
esac

echo ""
echo "==> 本 fork 已内置 local-dev 更新渠道; 想自动无感更新, 在 ~/.atomcode/config.toml 加:"
echo "    auto_update = true"
