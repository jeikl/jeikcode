# AtomCode Self — fork 版一键安装(Windows / PowerShell)
#
#   powershell -ExecutionPolicy Bypass -c "irm https://raw.atomgit.com/jeikls/atomcode/raw/local-dev/scripts/install-self.ps1 | iex"
#
# Env overrides:
#   $env:ATOMCODE_VERSION    release tag(默认从 fork latest.json 检测)
#   $env:ATOMCODE_PREFIX     安装目录(默认 $HOME\.local\bin)
#   $env:ATOMCODE_MANIFEST_URL / $env:ATOMCODE_DOWNLOAD_BASE  覆盖更新渠道(可选)
#
# 与官方 install.ps1 结构一致,仅下载源指向 fork 的 local-dev 渠道。

$ErrorActionPreference = "Stop"

$ManifestBase = if ($env:ATOMCODE_MANIFEST_URL) { $env:ATOMCODE_MANIFEST_URL.TrimEnd('/') } else { "https://raw.atomgit.com/jeikls/atomcode/raw/local-dev" }
$RepoBase     = if ($env:ATOMCODE_DOWNLOAD_BASE) { $env:ATOMCODE_DOWNLOAD_BASE.TrimEnd('/') } else { "https://atomgit.com/jeikls/atomcode/releases/download" }
$DefaultVersion = "v0.0.0-dev.1"

# --- detect platform ---
$os = "windows"
$arch = if ([Environment]::Is64BitOperatingSystem) { "x64" } else { "x86_64" }
$ext = ".exe"

# --- install dir ---
$Prefix = if ($env:ATOMCODE_PREFIX) { $env:ATOMCODE_PREFIX } else { Join-Path $HOME ".local\bin" }
New-Item -ItemType Directory -Force -Path $Prefix | Out-Null

# --- resolve version ---
if ($env:ATOMCODE_VERSION) {
    $Version = $env:ATOMCODE_VERSION
} else {
    Write-Host "==> Detecting latest version ($ManifestBase/latest.json)"
    try {
        $manifest = Invoke-RestMethod -Uri "$ManifestBase/latest.json" -TimeoutSec 10
        $Version = $manifest.version
    } catch {
        $Version = $DefaultVersion
    }
}

# --- download ---
$BinName = "atomcode-${Version}-${os}-${arch}${ext}"
$Url = "$RepoBase/$Version/$BinName"
$Dest = Join-Path $env:TEMP $BinName

Write-Host "==> Downloading $BinName"
Write-Host "    from $Url"
Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing

# Sanity check: not an HTML 404 page
$head = [System.IO.File]::ReadAllBytes($Dest)[0..3]
$isHtml = ($head -contains 0x3C)  # '<'
if ($isHtml) {
    Write-Error "Download looks like an HTML page, not a binary. URL: $Url"
    exit 1
}

# --- install ---
$Target = Join-Path $Prefix "atomcode$ext"
Write-Host "==> Installing to $Target"
try {
    Move-Item -Force $Dest $Target -ErrorAction Stop
} catch {
    Write-Host "Error: could not write $Target." -ForegroundColor Red
    Write-Host "       If atomcode is already running, close it and re-run this installer." -ForegroundColor Red
    exit 1
}

Write-Host ""
Write-Host "Installed: $Target"
& $Target --version 2>$null

# --- PATH ---
$currentPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($currentPath -notlike "*$Prefix*") {
    [Environment]::SetEnvironmentVariable("Path", "$Prefix;$currentPath", "User")
    Write-Host "Added $Prefix to user PATH (new shells will pick it up)."
} else {
    Write-Host "$Prefix already on user PATH."
}

Write-Host ""
Write-Host "==> 本 fork 已内置 local-dev 更新渠道; 想自动无感更新, 在 ~/.atomcode/config.toml 加:"
Write-Host "    auto_update = true"
