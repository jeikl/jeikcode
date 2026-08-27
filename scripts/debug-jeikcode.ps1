# Build the full jeikcode product with the `dbg` profile (see workspace Cargo.toml).
# Usage (repo root):
#   powershell -ExecutionPolicy Bypass -File scripts/debug-jeikcode.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/debug-jeikcode.ps1 -- init --force
#
# Output: target\dbg\jeikcode.exe  (not target\debug — that cache stays for tests)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $RepoRoot
try {
    Write-Host "==> cargo build -p atomcode --bin jeikcode --profile dbg"
    cargo build -p atomcode --bin jeikcode --profile dbg
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $exe = Join-Path $RepoRoot "target\dbg\jeikcode.exe"
    Write-Host "==> $exe"
    if ($args.Count -gt 0) {
        Write-Host "==> running: jeikcode $($args -join ' ')"
        & $exe @args
        exit $LASTEXITCODE
    }
    Write-Host "    F5 in VS Code: configuration 'jeikcode (dbg)' (CodeLLDB)"
    Write-Host "    or:  $exe init --force"
} finally {
    Pop-Location
}
