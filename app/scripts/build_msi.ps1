#Requires -Version 5.1
<#
.SYNOPSIS
    Build the MSI installer for this rust-skeleton app. One command, any project.

.DESCRIPTION
    The homogeneous MSI builder inherited by every minted app. It:
      1. ensures `cargo-wix` (the driver) is installed,
      2. checks for the WiX Toolset v3 (the actual compiler cargo-wix invokes),
      3. release-builds and packages the .msi from `wix/main.wxs`.

    cargo-wix is only a front-end: without the WiX Toolset (candle/light) on the
    machine there is no way to produce an .msi. That dependency is the thing the
    skeleton previously failed to document.

.PARAMETER InstallWix
    Auto-install the WiX Toolset via Chocolatey if it's missing (needs choco).

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\scripts\build_msi.ps1
#>
[CmdletBinding()]
param([switch] $InstallWix)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
Write-Host "Project: $root"

# 1) cargo-wix (the driver) ------------------------------------------------
if (-not (Get-Command cargo-wix -ErrorAction SilentlyContinue)) {
    Write-Host "Installing cargo-wix..."
    cargo install cargo-wix
}

# 2) WiX Toolset v3 (provides candle/light that cargo-wix calls) -----------
function Test-Wix {
    if ($env:WIX -and (Test-Path (Join-Path $env:WIX 'bin\candle.exe'))) { return $true }
    if (Get-Command candle -ErrorAction SilentlyContinue) { return $true }
    return [bool](Get-ChildItem "C:\Program Files (x86)" -Directory -ErrorAction SilentlyContinue |
        Where-Object Name -like 'WiX Toolset*')
}
if (-not (Test-Wix)) {
    if ($InstallWix -and (Get-Command choco -ErrorAction SilentlyContinue)) {
        Write-Host "Installing WiX Toolset via Chocolatey..."
        choco install wixtoolset -y --no-progress
    }
    else {
        Write-Warning "WiX Toolset v3 not found - cargo-wix needs it to compile the .msi."
        Write-Host "Install it, then re-run this script:"
        Write-Host "  choco install wixtoolset"
        Write-Host "  winget install WiXToolset.WiXToolset"
        Write-Host "  https://github.com/wixtoolset/wix3/releases  (wix314.exe)"
        Write-Host "Or re-run with -InstallWix to auto-install via Chocolatey."
        exit 1
    }
}

# 3) Build the MSI ---------------------------------------------------------
Write-Host "Building release MSI (cargo wix)..."
cargo wix --nocapture

$msi = Get-ChildItem "$root\target\wix\*.msi" -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime | Select-Object -Last 1
if ($msi) {
    Write-Host ""
    Write-Host "MSI: $($msi.FullName)  ($([math]::Round($msi.Length / 1MB, 1)) MB)"
}
else {
    Write-Warning "cargo wix finished but no .msi was found under target\wix."
}
