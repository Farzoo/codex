param(
  [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$RepoRoot = $PSScriptRoot
$codexRsRoot = Join-Path $RepoRoot "codex-rs"
$codexCliRoot = Join-Path $RepoRoot "codex-cli"
$triple = "x86_64-pc-windows-gnu"
$vendorTarget = "x86_64-pc-windows-msvc"

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

# Find existing vendor
$vendorSrc = $null
$npmRoot = (npm root -g).Trim()
if ($npmRoot) {
  $globalVendor = Join-Path $npmRoot "@openai\codex\vendor"
  if (Test-Path $globalVendor) {
    $vendorSrc = $globalVendor
  }
}
if (-not $vendorSrc) {
  throw "Vendor not found. Run: npm install -g @openai/codex"
}

$vendorTargetSrc = Join-Path $vendorSrc $vendorTarget
if (-not (Test-Path $vendorTargetSrc)) {
  throw "Vendor target not found: $vendorTargetSrc"
}

# Build temp vendor with custom codex.exe
$vendorTmp = Join-Path $env:TEMP "codex-vendor-$hash"
if (Test-Path $vendorTmp) { Remove-Item -Recurse -Force $vendorTmp }
New-Item -ItemType Directory -Force -Path $vendorTmp | Out-Null
Copy-Item -Recurse -Force $vendorTargetSrc (Join-Path $vendorTmp $vendorTarget)

$destExe = Join-Path $vendorTmp "$vendorTarget\codex\codex.exe"
Copy-Item $srcExe $destExe -Force

# Stage npm package
$packageJson = Get-Content (Join-Path $codexCliRoot "package.json") -Raw | ConvertFrom-Json
$version = "$($packageJson.version -replace '\+.*$', '')+local.$hash"

$stageDir = Join-Path $env:TEMP "codex-npm-stage-$hash"
if (Test-Path $stageDir) { Remove-Item -Recurse -Force $stageDir }

$buildScript = Join-Path $codexCliRoot "scripts\build_npm_package.py"
Push-Location $codexCliRoot
python $buildScript --version $version --package codex --vendor-src $vendorTmp --staging-dir $stageDir
Pop-Location
if ($LASTEXITCODE -ne 0) { throw "build_npm_package failed" }

# Install directly from staging dir
npm install -g $stageDir
if ($LASTEXITCODE -ne 0) { throw "npm install -g failed" }

Write-Host "Installed: $version"

# Cleanup
ls $stageDir
ls "$stageDir\bin"