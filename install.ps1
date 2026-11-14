<#
.SYNOPSIS
    Installs Wand Lite Rust on this PC.

.DESCRIPTION
    Two modes:

      * Portable / release zip  - if wand-rebuild-ui.exe sits next to this
        script, it is installed as-is (no Rust toolchain required).
      * From source             - from a repository checkout, builds with Cargo
        (installing the Rust toolchain first if it is missing).

    The application itself is a single self-contained .exe (static C runtime).

    Wand Lite does NOT bundle Wand's trainer runtime. It drives the runtime from
    an installed copy of the Wand desktop app, so that app must be installed and
    signed in once, and each game's trainer must be cached once by opening its
    trainer page in Wand.

.PARAMETER InstallDir
    Target directory. Default: %LOCALAPPDATA%\Programs\WandLite

.PARAMETER FromSource
    Force a source build even if a prebuilt exe is present.

.PARAMETER NoShortcut
    Do not create a Start Menu shortcut.

.PARAMETER DesktopShortcut
    Also create a Desktop shortcut.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\install.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\install.ps1 -DesktopShortcut
#>
[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\WandLite'),
    [switch]$FromSource,
    [switch]$NoShortcut,
    [switch]$DesktopShortcut
)

$ErrorActionPreference = 'Stop'

function Write-Step([string]$Message) { Write-Host "==> $Message" -ForegroundColor Cyan }
function Write-Ok([string]$Message) { Write-Host "    $Message" -ForegroundColor Green }
function Write-Warn([string]$Message) { Write-Host "    $Message" -ForegroundColor Yellow }

if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'Wand Lite requires 64-bit Windows.'
}

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$repositoryRoot = $scriptRoot

# ---------------------------------------------------------------------------
# 1. Locate a binary: prebuilt next to the script, or build from source.
# ---------------------------------------------------------------------------
$sourceBinary = Join-Path $scriptRoot 'wand-rebuild-ui.exe'
$binary = $null

if (-not $FromSource -and (Test-Path -LiteralPath $sourceBinary)) {
    Write-Step 'Using the bundled prebuilt binary'
    $binary = $sourceBinary
} else {
    $manifest = Join-Path $repositoryRoot 'rust\Cargo.toml'
    if (-not (Test-Path -LiteralPath $manifest)) {
        throw @'
No prebuilt wand-rebuild-ui.exe found next to this script and no rust\Cargo.toml
to build from.

Download the release zip (which contains the .exe), or run this script from a
repository checkout.
'@
    }

    Write-Step 'Checking the Rust toolchain'
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargo) {
        Write-Warn 'Rust not found; attempting to install it'
        $winget = Get-Command winget -ErrorAction SilentlyContinue
        if ($winget) {
            & winget install --id Rustlang.Rustup -e --accept-package-agreements --accept-source-agreements --silent
        } else {
            Write-Warn 'winget unavailable; downloading rustup-init.exe'
            $rustup = Join-Path $env:TEMP 'rustup-init.exe'
            Invoke-WebRequest -Uri 'https://win.rustup.rs/x86_64' -OutFile $rustup -UseBasicParsing
            & $rustup -y --profile minimal --default-toolchain stable
        }
        $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
        if (Test-Path -LiteralPath $cargoBin) { $env:PATH = "$cargoBin;$env:PATH" }
        $cargo = Get-Command cargo -ErrorAction SilentlyContinue
        if (-not $cargo) {
            throw 'Rust was installed but cargo is not on PATH. Open a new terminal and re-run this script.'
        }
    }
    Write-Ok ("cargo {0}" -f (& cargo --version))

    Write-Step 'Building Wand Lite (release)'
    Push-Location (Join-Path $repositoryRoot 'rust')
    try {
        & cargo build --package wand-rebuild-ui --release
        if ($LASTEXITCODE -ne 0) { throw 'cargo build failed.' }
    } finally {
        Pop-Location
    }

    $targetDir = $env:CARGO_TARGET_DIR
    if (-not $targetDir) { $targetDir = Join-Path $repositoryRoot 'rust\target' }
    $binary = Join-Path $targetDir 'release\wand-rebuild-ui.exe'
    if (-not (Test-Path -LiteralPath $binary)) {
        throw "Build finished but $binary was not produced."
    }
}

# ---------------------------------------------------------------------------
# 2. Warn about the runtime dependency (not redistributable).
# ---------------------------------------------------------------------------
Write-Step 'Checking for the installed Wand application'
$wandRoot = Join-Path $env:LOCALAPPDATA 'Wand'
$wandRuntime = $null
if (Test-Path -LiteralPath $wandRoot) {
    $wandRuntime = Get-ChildItem -LiteralPath $wandRoot -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like 'app-*' -and (Test-Path (Join-Path $_.FullName 'Wand.exe')) } |
        Sort-Object Name -Descending |
        Select-Object -First 1
}
if ($wandRuntime) {
    Write-Ok ("found runtime {0}" -f $wandRuntime.Name)
} else {
    Write-Warn 'Wand desktop app not found.'
    Write-Warn 'Wand Lite needs its trainer runtime. Install Wand, sign in, and open each'
    Write-Warn 'game''s trainer page once so the trainer is cached, then re-run this app.'
}

# ---------------------------------------------------------------------------
# 3. Install files.
# ---------------------------------------------------------------------------
Write-Step "Installing to $InstallDir"
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
$installedExe = Join-Path $InstallDir 'wand-rebuild-ui.exe'
Copy-Item -LiteralPath $binary -Destination $installedExe -Force

foreach ($extra in @('LICENSE', 'NOTICE.md')) {
    $candidate = Join-Path $scriptRoot $extra
    if (Test-Path -LiteralPath $candidate) {
        Copy-Item -LiteralPath $candidate -Destination (Join-Path $InstallDir $extra) -Force
    }
}
$uninstaller = Join-Path $scriptRoot 'uninstall.ps1'
if (Test-Path -LiteralPath $uninstaller) {
    Copy-Item -LiteralPath $uninstaller -Destination (Join-Path $InstallDir 'uninstall.ps1') -Force
}
Write-Ok ("installed {0:N1} MB single-file executable" -f ((Get-Item $installedExe).Length / 1MB))

# ---------------------------------------------------------------------------
# 4. Shortcuts.
# ---------------------------------------------------------------------------
function New-Shortcut([string]$LinkPath) {
    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut($LinkPath)
    $link.TargetPath = $installedExe
    $link.WorkingDirectory = $InstallDir
    $link.Description = 'Wand Lite - native trainer client'
    $link.IconLocation = "$installedExe,0"
    $link.Save()
}

if (-not $NoShortcut) {
    $startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
    New-Item -ItemType Directory -Path $startMenu -Force | Out-Null
    $link = Join-Path $startMenu 'Wand Lite.lnk'
    New-Shortcut $link
    Write-Ok "start menu shortcut: $link"
}
if ($DesktopShortcut) {
    $desktop = [Environment]::GetFolderPath('Desktop')
    $link = Join-Path $desktop 'Wand Lite.lnk'
    New-Shortcut $link
    Write-Ok "desktop shortcut: $link"
}

# ---------------------------------------------------------------------------
# 5. Done.
# ---------------------------------------------------------------------------
Write-Host ''
Write-Host 'Wand Lite is installed.' -ForegroundColor Green
Write-Host "  executable : $installedExe"
Write-Host ''
Write-Host 'How to use it:'
Write-Host '  1. Open each game''s trainer page in Wand once (caches the trainer), then close Wand.'
Write-Host '  2. Start your game.'
Write-Host '  3. Run Wand Lite, select the game, press "Load trainer".'
Write-Host ''
Write-Host "Diagnostics: `"$installedExe`" --self-check"
