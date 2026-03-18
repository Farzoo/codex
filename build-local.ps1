param(
  [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$RepoRoot = $PSScriptRoot
$codexRsRoot = Join-Path $RepoRoot "codex-rs"
$codexCliRoot = Join-Path $RepoRoot "codex-cli"
$triple = "x86_64-pc-windows-gnu"

$hash = (git -C $RepoRoot rev-parse --short HEAD).Trim()

if (-not $SkipBuild) {
  Write-Host "Building codex (commit $hash) ..."
  Push-Location $codexRsRoot
  try {
    cargo +stable-x86_64-pc-windows-gnu build -p codex-cli --bin codex --release --target $triple
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
  } finally {
    Pop-Location
  }
}

$srcExe = Join-Path $codexRsRoot "target\$triple\release\codex.exe"
if (-not (Test-Path $srcExe)) {
  throw "Built binary not found: $srcExe"
}

# Find existing codex binary from npm global install (new structure)
$npmRoot = (npm root -g).Trim()
if (-not $npmRoot) {
  throw "npm root not found"
}

$win64PkgDir = Join-Path $npmRoot "@openai\codex\node_modules\@openai\codex-win32-x64"
if (-not (Test-Path $win64PkgDir)) {
  throw "codex-win32-x64 package not found at: $win64PkgDir`nRun: npm install -g @openai/codex"
}

# Find the existing codex.exe in the platform package
$existingExe = Get-ChildItem -Path $win64PkgDir -Filter "codex.exe" -Recurse | Select-Object -First 1
if (-not $existingExe) {
  throw "codex.exe not found in: $win64PkgDir"
}

Write-Host "Found existing binary: $($existingExe.FullName)"
Write-Host "Replacing with local build (commit $hash) ..."

# Backup original
$backupExe = "$($existingExe.FullName).bak"
if (-not (Test-Path $backupExe)) {
  Copy-Item $existingExe.FullName $backupExe
  Write-Host "Backed up original to: $backupExe"
}

# Replace with local build
Copy-Item $srcExe $existingExe.FullName -Force

Write-Host "Installed local codex (commit $hash) -> $($existingExe.FullName)"
codex --version