param(
    [Parameter(Mandatory = $true)][string]$Destination,
    [string]$ZipPath
)

$ErrorActionPreference = 'Stop'
$sourceRoot = [IO.Path]::GetFullPath($PSScriptRoot)
$destinationRoot = [IO.Path]::GetFullPath($Destination)
if (Test-Path -LiteralPath $destinationRoot) {
    throw "Destination already exists: $destinationRoot"
}

$files = @(
    'Cargo.toml', 'Cargo.lock', 'build.ps1', 'package-source.ps1',
    '.gitignore', 'LICENSE', 'README.md',
    'core/Cargo.toml', 'ui/Cargo.toml', 'ui/build.rs', 'ui/ui/main.slint'
)
$files += Get-ChildItem -LiteralPath (Join-Path $sourceRoot 'core/src') -File -Filter '*.rs' |
    ForEach-Object { 'core/src/' + $_.Name }
$files += Get-ChildItem -LiteralPath (Join-Path $sourceRoot 'core/examples') -File -Filter '*.rs' |
    ForEach-Object { 'core/examples/' + $_.Name }
$files += Get-ChildItem -LiteralPath (Join-Path $sourceRoot 'ui/src') -File -Filter '*.rs' |
    ForEach-Object { 'ui/src/' + $_.Name }
$files += Get-ChildItem -LiteralPath (Join-Path $sourceRoot 'ui/assets/fonts') -File |
    ForEach-Object { 'ui/assets/fonts/' + $_.Name }

New-Item -ItemType Directory -Path $destinationRoot | Out-Null
foreach ($relative in $files) {
    $source = Join-Path $sourceRoot $relative
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "Required source file missing: $relative"
    }
    $target = Join-Path $destinationRoot $relative
    New-Item -ItemType Directory -Path (Split-Path $target -Parent) -Force | Out-Null
    Copy-Item -LiteralPath $source -Destination $target
}
if ($ZipPath) {
    $zip = [IO.Path]::GetFullPath($ZipPath)
    if (Test-Path -LiteralPath $zip) { throw "Zip already exists: $zip" }
    Compress-Archive -LiteralPath $destinationRoot -DestinationPath $zip
}
Write-Output "Packaged $($files.Count) source files in $destinationRoot"
