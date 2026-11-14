<#
.SYNOPSIS
    Debloat installer: adopts an existing Wand backend and installs Wand Lite.

.DESCRIPTION
    Wand Lite is a lightweight native replacement for the Electron Wand app, but
    it still needs the trainer backend that Wand installs: the injectable runtime
    DLLs, the downloaded trainer binaries and the trainer blueprints.

    This script copies that backend out of an existing Wand installation into
    Wand Lite's own data directory, installs Wand Lite, and verifies that it can
    see the adopted backend. Once that succeeds the original Electron app is no
    longer required and can optionally be removed with -RemoveWand.

    Nothing is redistributed: the backend files are your own licensed copies,
    moved locally on your machine.

.PARAMETER WandPath
    Explicit path to a Wand application directory (the folder containing
    Wand.exe and resources\app.asar.unpacked). Auto-detected when omitted.

.PARAMETER InstallDir
    Where Wand Lite is installed. Default: %LOCALAPPDATA%\Programs\WandLite

.PARAMETER RemoveWand
    After a successful adoption and verification, uninstall the Electron Wand
    app. Its cached data under %APPDATA%\wemod is left alone.

.PARAMETER DryRun
    Report what would be adopted and installed without changing anything.

.PARAMETER Force
    Re-adopt even if a complete backend already exists.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\debloat.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\debloat.ps1 -RemoveWand -DesktopShortcut

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\debloat.ps1 -WandPath "D:\Apps\Wand\app-12.30.1" -DryRun
#>
[CmdletBinding()]
param(
    [string]$WandPath,
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\WandLite'),
    [switch]$RemoveWand,
    [switch]$DryRun,
    [switch]$Force,
    [switch]$NoShortcut,
    [switch]$DesktopShortcut
)

$ErrorActionPreference = 'Stop'

function Write-Step([string]$Message) { Write-Host "==> $Message" -ForegroundColor Cyan }
function Write-Ok([string]$Message) { Write-Host "    $Message" -ForegroundColor Green }
function Write-Warn([string]$Message) { Write-Host "    $Message" -ForegroundColor Yellow }

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$backendRoot = Join-Path $env:LOCALAPPDATA 'WandRebuild\backend'
$requiredDlls = @(
    'TrainerLib_x64.dll', 'TrainerLib_x86.dll',
    'CELib_x64.dll', 'CELib_x86.dll',
    'TrainerLibPlugin_x64.dll', 'TrainerLibPlugin_x86.dll'
)

if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'Wand Lite requires 64-bit Windows.'
}

# ---------------------------------------------------------------------------
# 1. Find the existing Wand installation.
# ---------------------------------------------------------------------------
Write-Step 'Looking for an existing Wand installation'

function Get-WandCandidates {
    $candidates = @()
    if ($WandPath) { $candidates += $WandPath }

    $localWand = Join-Path $env:LOCALAPPDATA 'Wand'
    if (Test-Path -LiteralPath $localWand) {
        $candidates += Get-ChildItem -LiteralPath $localWand -Directory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -like 'app-*' } |
            ForEach-Object { $_.FullName }
    }
    foreach ($root in @(
            (Join-Path $env:LOCALAPPDATA 'Programs\Wand'),
            (Join-Path ${env:ProgramFiles} 'Wand'),
            (Join-Path ${env:ProgramFiles(x86)} 'Wand')
        )) {
        if ($root -and (Test-Path -LiteralPath $root)) { $candidates += $root }
    }
    return $candidates | Where-Object { $_ } | Select-Object -Unique
}

function Test-WandApp([string]$Path) {
    $trainerlib = Join-Path $Path 'resources\app.asar.unpacked\static\unpacked\trainerlib'
    if (-not (Test-Path -LiteralPath $trainerlib)) { return $null }
    $missing = $requiredDlls | Where-Object { -not (Test-Path -LiteralPath (Join-Path $trainerlib $_)) }
    if ($missing) { return $null }
    return [pscustomobject]@{
        AppDirectory = (Resolve-Path -LiteralPath $Path).Path
        TrainerLib   = (Resolve-Path -LiteralPath $trainerlib).Path
        Version      = (Split-Path -Leaf $Path)
    }
}

function Get-WandVersionRank([string]$Path) {
    $leaf = Split-Path -Leaf $Path
    if ($leaf -match '^app-(\d+)\.(\d+)(?:\.(\d+))?(?:\.(\d+))?$') {
        $parts = @([int]$Matches[1], [int]$Matches[2], [int]$Matches[3], [int]$Matches[4])
        return ($parts -join '.')
    }
    return '0.0.0.0'
}

# Newest runtime version wins, matching how the client itself resolves a runtime.
$ordered = Get-WandCandidates |
    ForEach-Object { [pscustomobject]@{ Path = $_; Rank = (Get-WandVersionRank $_) } } |
    Sort-Object { [version]$_.Rank } -Descending

$wand = $null
foreach ($candidate in $ordered) {
    $found = Test-WandApp $candidate.Path
    if ($found) { $wand = $found; break }
}
if (-not $wand) {
    throw @'
No complete Wand installation was found.

The debloater needs an existing Wand install to adopt its trainer backend.
Install the official Wand app, sign in, and open each game's trainer page once,
then run this again. Use -WandPath to point at a non-standard location.
'@
}
Write-Ok ("found {0}" -f $wand.AppDirectory)

# ---------------------------------------------------------------------------
# 2. Find the trainer cache and blueprint database.
# ---------------------------------------------------------------------------
Write-Step 'Locating the trainer cache and blueprints'
$trainersSource = Join-Path $env:APPDATA 'wemod\App\trainers'
$blueprintsSource = Join-Path $env:APPDATA 'wemod\Local Storage\leveldb'

$trainerFiles = @()
if (Test-Path -LiteralPath $trainersSource) {
    $trainerFiles = Get-ChildItem -LiteralPath $trainersSource -File -Filter 'Trainer_*.dll' -ErrorAction SilentlyContinue
}
if (-not $trainerFiles) {
    Write-Warn 'No cached trainers found. Wand Lite will install, but open each'
    Write-Warn 'game''s trainer page in Wand once to download trainers.'
} else {
    Write-Ok ("{0} trainer(s), {1:N1} MB" -f $trainerFiles.Count, (($trainerFiles | Measure-Object Length -Sum).Sum / 1MB))
}

$blueprintFiles = @()
if (Test-Path -LiteralPath $blueprintsSource) {
    $blueprintFiles = Get-ChildItem -LiteralPath $blueprintsSource -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -notin @('LOCK') }
}
if ($blueprintFiles) {
    Write-Ok ("blueprint database, {0:N1} MB" -f (($blueprintFiles | Measure-Object Length -Sum).Sum / 1MB))
} else {
    Write-Warn 'No blueprint database found; game/trainer resolution may be limited.'
}

# ---------------------------------------------------------------------------
# 3. Stop Wand so its files can be copied.
# ---------------------------------------------------------------------------
$running = Get-Process -Name 'Wand', 'WandAuxiliaryService' -ErrorAction SilentlyContinue
if ($running) {
    if ($DryRun) {
        Write-Warn ("Wand is running ({0} process(es)); it would be closed" -f $running.Count)
    } else {
        Write-Step 'Closing Wand'
        $running | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 2
        Write-Ok 'closed'
    }
}

if ($DryRun) {
    Write-Host ''
    Write-Host 'DRY RUN - nothing was changed.' -ForegroundColor Yellow
    Write-Host "  would adopt runtime : $($wand.TrainerLib)"
    Write-Host "  would adopt trainers: $trainersSource ($($trainerFiles.Count) files)"
    Write-Host "  would adopt blueprints: $blueprintsSource ($($blueprintFiles.Count) files)"
    Write-Host "  into                : $backendRoot"
    Write-Host "  would install to    : $InstallDir"
    if ($RemoveWand) { Write-Host "  would uninstall Wand: $($wand.AppDirectory)" }
    return
}

# ---------------------------------------------------------------------------
# 4. Adopt the backend.
# ---------------------------------------------------------------------------
$existingManifest = Join-Path $backendRoot 'backend.json'
if ((Test-Path -LiteralPath $existingManifest) -and -not $Force) {
    Write-Step 'A backend was already adopted (use -Force to replace it)'
    Write-Ok $backendRoot
} else {
    Write-Step "Adopting backend into $backendRoot"
    $runtimeTarget = Join-Path $backendRoot ("runtime\" + $wand.Version + '\trainerlib')
    $trainersTarget = Join-Path $backendRoot 'trainers'
    $blueprintsTarget = Join-Path $backendRoot 'blueprints'

    New-Item -ItemType Directory -Path $runtimeTarget -Force | Out-Null
    Copy-Item -Path (Join-Path $wand.TrainerLib '*') -Destination $runtimeTarget -Recurse -Force
    Write-Ok ("runtime {0}" -f $wand.Version)

    if ($trainerFiles) {
        New-Item -ItemType Directory -Path $trainersTarget -Force | Out-Null
        foreach ($file in $trainerFiles) {
            Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $trainersTarget $file.Name) -Force
        }
        Write-Ok ("{0} trainer(s)" -f $trainerFiles.Count)
    }

    if ($blueprintFiles) {
        New-Item -ItemType Directory -Path $blueprintsTarget -Force | Out-Null
        foreach ($file in $blueprintFiles) {
            Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $blueprintsTarget $file.Name) -Force
        }
        Write-Ok ("{0} blueprint file(s)" -f $blueprintFiles.Count)
    }

    $manifest = [ordered]@{
        adoptedAt      = (Get-Date).ToUniversalTime().ToString('o')
        wandVersion    = $wand.Version
        sourceApp      = $wand.AppDirectory
        sourceTrainers = $trainersSource
        trainerCount   = $trainerFiles.Count
        runtimeFiles   = @($requiredDlls)
        tool           = 'wand-lite-rust debloat.ps1'
    }
    $manifest | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $existingManifest -Encoding utf8
    Write-Ok 'manifest written'
}

# ---------------------------------------------------------------------------
# 5. Install Wand Lite.
# ---------------------------------------------------------------------------
Write-Step 'Installing Wand Lite'
$installer = Join-Path $scriptRoot 'install.ps1'
if (Test-Path -LiteralPath $installer) {
    $installArgs = @('-InstallDir', $InstallDir)
    if ($NoShortcut) { $installArgs += '-NoShortcut' }
    if ($DesktopShortcut) { $installArgs += '-DesktopShortcut' }
    & powershell -NoProfile -ExecutionPolicy Bypass -File $installer @installArgs
    if ($LASTEXITCODE -ne 0) { throw 'Wand Lite installation failed.' }
} else {
    $prebuilt = Join-Path $scriptRoot 'wand-rebuild-ui.exe'
    if (-not (Test-Path -LiteralPath $prebuilt)) {
        throw 'install.ps1 and wand-rebuild-ui.exe are both missing next to debloat.ps1.'
    }
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Copy-Item -LiteralPath $prebuilt -Destination (Join-Path $InstallDir 'wand-rebuild-ui.exe') -Force
    Write-Ok "copied executable to $InstallDir"
}

# ---------------------------------------------------------------------------
# 6. Verify the adopted backend is actually used.
# ---------------------------------------------------------------------------
Write-Step 'Verifying the adopted backend'
$installedExe = Join-Path $InstallDir 'wand-rebuild-ui.exe'
$report = & $installedExe --self-check 2>&1 | Out-String
$adoptedLine = ($report -split "`r?`n" | Where-Object { $_ -like 'Backend source:*' } | Select-Object -First 1)
if ($adoptedLine -match 'adopted') {
    Write-Ok $adoptedLine.Trim()
} else {
    Write-Warn ($adoptedLine.Trim())
    Write-Warn 'Wand Lite did not report an adopted backend; leaving Wand installed.'
    $RemoveWand = $false
}

# ---------------------------------------------------------------------------
# 7. Optionally remove the Electron app.
# ---------------------------------------------------------------------------
if ($RemoveWand) {
    Write-Step 'Removing the Electron Wand application'
    $uninstallers = @()
    foreach ($key in @(
            'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
            'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*',
            'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*'
        )) {
        $uninstallers += Get-ItemProperty $key -ErrorAction SilentlyContinue |
            Where-Object { $_.DisplayName -match '^(Wand|WeMod)( |$)' -and $_.UninstallString }
    }
    if ($uninstallers) {
        foreach ($entry in $uninstallers) {
            Write-Ok ("uninstalling {0}" -f $entry.DisplayName)
            $command = $entry.UninstallString
            if ($command -match 'msiexec') {
                $productCode = if ($command -match '\{[0-9A-Fa-f-]+\}') { $Matches[0] } else { $null }
                if ($productCode) {
                    Start-Process msiexec.exe -ArgumentList '/x', $productCode, '/qn', '/norestart' -Wait
                }
            } else {
                Start-Process -FilePath 'cmd.exe' -ArgumentList '/c', $command -Wait -WindowStyle Hidden
            }
        }
    } else {
        Write-Warn 'No uninstaller entry found; remove Wand through Windows Settings.'
    }
    # The app directory can be left behind by some uninstallers.
    if (Test-Path -LiteralPath $wand.AppDirectory) {
        Write-Warn ("application folder still present: {0}" -f $wand.AppDirectory)
        Write-Warn 'You may delete it manually; the adopted backend is independent.'
    }
}

# ---------------------------------------------------------------------------
# 8. Summary.
# ---------------------------------------------------------------------------
Write-Host ''
Write-Host 'Debloat complete.' -ForegroundColor Green
Write-Host "  Wand Lite      : $installedExe"
Write-Host "  adopted backend: $backendRoot"
Write-Host "  backend source : $($wand.AppDirectory)"
if (-not $RemoveWand) {
    Write-Host ''
    Write-Host 'The Electron Wand app is still installed but no longer needed.'
    Write-Host 'Remove it any time with:  .\debloat.ps1 -RemoveWand -Force'
}
Write-Host ''
Write-Host 'Next: start a game, run Wand Lite, select the game, press "Load trainer".'
