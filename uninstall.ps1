<#
.SYNOPSIS
    Uninstalls Wand Lite Rust.

.DESCRIPTION
    Removes the installed executable and shortcuts. Cached catalog, artwork and
    preferences under %LOCALAPPDATA%\WandRebuild are kept unless -RemoveData is
    passed. The installed Wand application is never touched.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\uninstall.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\uninstall.ps1 -RemoveData
#>
[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\WandLite'),
    [switch]$RemoveData
)

$ErrorActionPreference = 'Stop'
function Write-Step([string]$Message) { Write-Host "==> $Message" -ForegroundColor Cyan }

Write-Step 'Closing Wand Lite if it is running'
Get-Process -Name 'wand-rebuild-ui' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500

Write-Step 'Removing shortcuts'
foreach ($link in @(
        (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Wand Lite.lnk'),
        (Join-Path ([Environment]::GetFolderPath('Desktop')) 'Wand Lite.lnk')
    )) {
    if (Test-Path -LiteralPath $link) {
        Remove-Item -LiteralPath $link -Force
        Write-Host "    removed $link"
    }
}

Write-Step "Removing $InstallDir"
if (Test-Path -LiteralPath $InstallDir) {
    Remove-Item -LiteralPath $InstallDir -Recurse -Force
    Write-Host '    removed'
} else {
    Write-Host '    not present'
}

if ($RemoveData) {
    $data = Join-Path $env:LOCALAPPDATA 'WandRebuild'
    Write-Step "Removing cached data in $data"
    if (Test-Path -LiteralPath $data) {
        Remove-Item -LiteralPath $data -Recurse -Force
        Write-Host '    removed'
    } else {
        Write-Host '    not present'
    }
}

Write-Host ''
Write-Host 'Wand Lite uninstalled.' -ForegroundColor Green
Write-Host 'Your installed Wand application and its trainer cache were not modified.'
