#!/bin/sh
# AtomCode Self — 源码开发模式系统命令安装(Unix/macOS/Linux/HarmonyOS)
#
# 作用: 把本仓库的 cargo 构建结果注册为系统 `atomcode` 命令,
#       免去每次敲全量 target/release/atomcode 路径。
#       同时写入 dev 模式环境,避免官方更新把本地构建覆盖掉。
#
# 用法:
#   ./scripts/dev-install.sh            # 构建 release 并注册(默认)
#   ./scripts/dev-install.sh --skip-build   # 不构建,只注册已有二进制
#   ./scripts/dev-install.sh --uninstall    # 移除注册(不删构建产物)
#
# 安装目标:
#   PREFIX 默认 ~/.local/bin; 若 /usr/local/bin 可写则优先(需 sudo 时自动提示)。
#   写入的 wrapper 会: ① 设置 ATOMCODE_DEV=1(禁用自动更新覆盖本地构建)
#                      ② exec 本仓库 target/release/atomcode
set -eu

# --- resolve repo root ---
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

SKIP_BUILD=0
UNINSTALL=0
for a in "$@"; do
    case "$a" in
        --skip-build) SKIP_BUILD=1 ;;
        --uninstall)  UNINSTALL=1 ;;
        *) echo "unknown arg: $a"; exit 1 ;;
    esac
done

# --- binary path ---
EXE="target/release/atomcode"
[ -f "$REPO_ROOT/$EXE.exe" ] && EXE="$EXE.exe"
EXE_ABS="$REPO_ROOT/$EXE"

# --- pick install dir ---
if [ -n "${ATOMCODE_PREFIX:-}" ]; then
    PREFIX="$ATOMCODE_PREFIX"
elif [ -w /usr/local/bin ] 2>/dev/null || [ "$(id -u)" -eq 0 ]; then
    PREFIX="/usr/local/bin"
else
    PREFIX="$HOME/.local/bin"
fi
mkdir -p "$PREFIX"

if [ "$UNINSTALL" = "1" ]; then
    rm -f "$PREFIX/atomcode" "$PREFIX/atomcode-self-dev"
    echo "==> removed dev wrapper from $PREFIX"
    exit 0
fi

# --- build ---
if [ "$SKIP_BUILD" = "0" ]; then
    echo "==> cargo build --release (首次较慢, 之后增量)"
    (cd "$REPO_ROOT" && cargo build --release --bin atomcode)
    [ -f "$EXE_ABS" ] || { echo "Error: build output missing: $EXE_ABS"; exit 1; }
else
    [ -f "$EXE_ABS" ] || { echo "Error: no existing build at $EXE_ABS (run without --skip-build first)"; exit 1; }
fi

# --- write wrapper ---
WRAPPER="$PREFIX/atomcode"
cat > "$WRAPPER" <<EOF
#!/bin/sh
# AtomCode Self dev wrapper — points at $EXE_ABS
# ATOMCODE_DEV=1 keeps the official updater from replacing the local build.
export ATOMCODE_DEV=1
export ATOMCODE_NO_UPDATE=1
exec "$EXE_ABS" "\$@"
EOF
chmod +x "$WRAPPER"

echo "==> installed dev wrapper: $WRAPPER"
echo "    -> $EXE_ABS"

# --- PATH ---
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
                echo "# Added by AtomCode Self dev-install" >> "$RC"
                echo "$LINE" >> "$RC"
            fi
            echo "Added $PREFIX to PATH in $RC (source $RC to use now)"
        else
            echo "Note: add to PATH: $LINE"
        fi
        ;;
esac

echo ""
echo "==> 完成! 现在直接运行: atomcode"
echo "    提示: 源码更新后重新执行 ./scripts/dev-install.sh 即可(增量构建很快)"
