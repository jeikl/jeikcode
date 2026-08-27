# Local Windows 成品：只编 jeikcode（+ atomcode 同 main.rs），不 clean、不 musl、不 npm ci。
# 用法（仓库根）:
#   powershell -ExecutionPolicy Bypass -File scripts/build-release-fast.ps1
#
# 产出: target\release\jeikcode.exe
# 第一次仍可能数分钟（tree-sitter C）；之后只改 .rs 应掉到大约 1 分钟内。
# 不要改 Cargo.toml 的 version 再跑 —— workspace version 一变会整仓作废。

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $RepoRoot
try {
    Write-Host "==> cargo build --release -p atomcode --bin jeikcode"
    cargo build --release -p atomcode --bin jeikcode
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $exe = Join-Path $RepoRoot "target\release\jeikcode.exe"
    Write-Host "==> $exe"
} finally {
    Pop-Location
}
