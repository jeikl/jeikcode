# AtomCode Self — 源码开发模式系统命令安装(Windows / PowerShell)
#
# 作用: 把本仓库的 cargo 构建结果注册为系统 `atomcode` 命令,
#       免去每次敲全量 target\release\atomcode.exe 路径。
#       同时写入 dev 模式环境,避免官方更新把本地构建覆盖掉。
#
# 用法(在仓库根目录的 PowerShell 里):
#   powershell -ExecutionPolicy Bypass -File scripts\dev-install.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\dev-install.ps1 -SkipBuild
#   powershell -ExecutionPolicy Bypass -File scripts\dev-install.ps1 -Uninstall

param(
    [switch]$SkipBuild,
    [switch]$Uninstall
)
$ErrorActionPreference = "Stop"

# --- resolve repo root (脚本在 <repo>/scripts 下) ---
$RepoRoot = Split-Path -Parent $PSScriptRoot
$Exe = Join-Path $RepoRoot "target\release\atomcode.exe"

# --- install dir ---
$Prefix = if ($env:ATOMCODE_PREFIX) { $env:ATOMCODE_PREFIX } else { Join-Path $HOME ".local\bin" }
New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
$Wrapper = Join-Path $Prefix "atomcode.cmd"
$AliasWrapper = Join-Path $Prefix "jeikcode.cmd"

if ($Uninstall) {
    Remove-Item -Force $Wrapper -ErrorAction SilentlyContinue
    Remove-Item -Force $AliasWrapper -ErrorAction SilentlyContinue
    Write-Host "==> removed dev wrapper: $Wrapper"
    exit 0
}

# --- build ---
if (-not $SkipBuild) {
    Write-Host "==> cargo build --release (首次较慢, 之后增量)"
    Push-Location $RepoRoot
    try { cargo build --release --bin atomcode } finally { Pop-Location }
}
if (-not (Test-Path $Exe)) {
    Write-Error "Build output missing: $Exe (run without -SkipBuild first)"
    exit 1
}

# --- write .cmd wrapper ---
@"
@echo off
rem AtomCode Self dev wrapper - points at %~dp0..\..\target\release\atomcode.exe
rem ATOMCODE_DEV=1 keeps the official updater from replacing the local build.
set ATOMCODE_DEV=1
set ATOMCODE_NO_UPDATE=1
"$Exe" %*
"@ | Set-Content -Path $Wrapper -Encoding Ascii
Copy-Item -Force $Wrapper $AliasWrapper
Write-Host "==> installed dev wrapper: $Wrapper"
Write-Host "    alias: $AliasWrapper"
Write-Host "    -> $Exe"

# --- PATH ---
$currentPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($currentPath -notlike "*$Prefix*") {
    [Environment]::SetEnvironmentVariable("Path", "$Prefix;$currentPath", "User")
    Write-Host "Added $Prefix to user PATH (new shells will pick it up)."
} else {
    Write-Host "$Prefix already on user PATH."
}

Write-Host ""
Write-Host "==> 完成! 新开一个终端直接运行: atomcode  或  jeikcode"
Write-Host "    提示: 源码更新后重新执行 scripts\dev-install.ps1 即可(增量构建很快)"
