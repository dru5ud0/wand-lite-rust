$ErrorActionPreference = 'Stop'
$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
if ($cargoCommand) {
    $cargo = $cargoCommand.Source
} else {
    $localCargo = $null
    $localToolRoot = $null
    $cursor = [IO.DirectoryInfo]$PSScriptRoot
    while ($cursor) {
        $candidate = Join-Path $cursor.FullName 'work\.cargo\bin\cargo.exe'
        if (Test-Path -LiteralPath $candidate) {
            $localCargo = $candidate
            $localToolRoot = $cursor.FullName
            break
        }
        $cursor = $cursor.Parent
    }
    if (-not $localCargo) {
        throw 'Rust Cargo is required. Install Rust from https://rust-lang.org/tools/install/ and run this script again.'
    }
    $env:RUSTUP_HOME = Join-Path $localToolRoot 'work\.rustup'
    $env:CARGO_HOME = Join-Path $localToolRoot 'work\.cargo'
    $env:CARGO_TARGET_DIR = Join-Path $localToolRoot 'work\rust-target'
    $cargo = $localCargo
}
& $cargo test --manifest-path (Join-Path $PSScriptRoot 'Cargo.toml') --workspace --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& $cargo build --manifest-path (Join-Path $PSScriptRoot 'Cargo.toml') --package wand-rebuild-ui --release
exit $LASTEXITCODE
