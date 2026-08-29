# JeikCode installer for Windows — PowerShell
#
#   irm https://raw.githubusercontent.com/jeikl/jeikcode/local-dev/scripts/install.ps1 | iex
#
# Env overrides:
#   $env:ATOMCODE_VERSION    release tag (default: latest from fork latest.json)
#   $env:ATOMCODE_PREFIX     install dir (default: $HOME\.local\bin)
#   $env:ATOMCODE_MANIFEST_URL / $env:ATOMCODE_DOWNLOAD_BASE  override update channel (optional)

$ErrorActionPreference = "Stop"

$ManifestBase = if ($env:ATOMCODE_MANIFEST_URL) { $env:ATOMCODE_MANIFEST_URL.TrimEnd('/') } else { "https://raw.githubusercontent.com/jeikl/jeikcode/local-dev" }
$RepoBase     = if ($env:ATOMCODE_DOWNLOAD_BASE) { $env:ATOMCODE_DOWNLOAD_BASE.TrimEnd('/') } else { "https://github.com/jeikl/jeikcode/releases/download" }
$DefaultVersion = "6.0.44"

# --- detect platform ---
$os = "windows"
$RealArch = if ($env:PROCESSOR_ARCHITEW6432) {
    $env:PROCESSOR_ARCHITEW6432
} else {
    $env:PROCESSOR_ARCHITECTURE
}
switch ($RealArch) {
    "AMD64" { $arch = "x64" }
    "ARM64" { $arch = "arm64" }
    default {
        Write-Host "Unsupported architecture: $RealArch (supported: AMD64, ARM64)" -ForegroundColor Red
        exit 1
    }
}
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
$BinName = "jeikcode-${Version}-${os}-${arch}${ext}"
$Url = "$RepoBase/$Version/$BinName"
$Dest = Join-Path $env:TEMP $BinName

Write-Host "==> Downloading $BinName"
Write-Host "    from $Url"
try {
    Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing
} catch {
    $AltName = "atomcode-${Version}-${os}-${arch}${ext}"
    $AltUrl = "$RepoBase/$Version/$AltName"
    Write-Host "==> Retrying with $AltName"
    Write-Host "    from $AltUrl"
    Invoke-WebRequest -Uri $AltUrl -OutFile $Dest -UseBasicParsing
}

# Sanity check: not an HTML 404 page
$head = [System.IO.File]::ReadAllBytes($Dest)[0..3]
$isHtml = ($head -contains 0x3C)  # '<'
if ($isHtml) {
    Write-Error "Download looks like an HTML page, not a binary. URL: $Url"
    exit 1
}

# --- install ---
$Target = Join-Path $Prefix "jeikcode$ext"
Write-Host "==> Installing to $Target"
try {
    Move-Item -Force $Dest $Target -ErrorAction Stop
} catch {
    Write-Host "Error: could not write $Target." -ForegroundColor Red
    Write-Host "       If jeikcode is already running, close it and re-run this installer." -ForegroundColor Red
    exit 1
}

$Alias = Join-Path $Prefix "atomcode$ext"
Copy-Item -Force $Target $Alias
Write-Host ""
Write-Host "Installed: $Target"
Write-Host "Alias:     $Alias"
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
Write-Host "==> JeikCode uses the local-dev update channel. To enable auto-update, add to ~/.atomcode/config.toml:"
Write-Host "    auto_update = true"
