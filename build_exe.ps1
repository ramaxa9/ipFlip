param(
    [ValidateSet("release", "debug")]
    [string]$Profile = "release"
)

$ErrorActionPreference = "Stop"

$projectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $projectRoot

Write-Host "Building Rust app ($Profile)..."
cargo build --profile $Profile

$exeName = "rust_ipflip.exe"
$sourceExe = Join-Path $projectRoot "target\$Profile\$exeName"
if (-not (Test-Path $sourceExe)) {
    throw "Build completed but executable not found: $sourceExe"
}

$distRoot = Join-Path $projectRoot "dist"
if (Test-Path $distRoot) {
    Remove-Item $distRoot -Recurse -Force
}
New-Item -ItemType Directory -Path $distRoot | Out-Null

$targetExe = Join-Path $distRoot "ipFlip-rust.exe"
Copy-Item $sourceExe $targetExe -Force

foreach ($asset in @("icon.png", "logo.svg")) {
    $localAsset = Join-Path $projectRoot $asset
    $parentAsset = Join-Path (Split-Path $projectRoot -Parent) $asset

    if (Test-Path $localAsset) {
        Copy-Item $localAsset (Join-Path $distRoot $asset) -Force
    }
    elseif (Test-Path $parentAsset) {
        Copy-Item $parentAsset (Join-Path $distRoot $asset) -Force
    }
}

$sizeMb = [Math]::Round(((Get-ChildItem -Path $distRoot -File | Measure-Object -Property Length -Sum).Sum / 1MB), 2)
Write-Host "Build complete."
Write-Host "Output: $distRoot"
Write-Host "Main EXE: $targetExe"
Write-Host "Payload size: $sizeMb MB"
