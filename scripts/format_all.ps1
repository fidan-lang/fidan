$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

$rustFiles = @(
    git ls-files -- '*.rs' |
        Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) }
)

$cppFiles = @(
    git ls-files -- '*.c' '*.cc' '*.cpp' '*.cxx' '*.h' '*.hh' '*.hpp' '*.hxx' |
        Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) }
)

if ($rustFiles.Count -eq 0 -and $cppFiles.Count -eq 0) {
    Write-Host 'No Rust or C/C++ files found to format.'
    exit 0
}

if ($rustFiles.Count -gt 0) {
    Write-Host "Formatting $($rustFiles.Count) Rust files..."
    & cargo fmt --all -- --check
}

if ($cppFiles.Count -gt 0) {
    Write-Host "Formatting $($cppFiles.Count) C/C++ files..."
    & clang-format -i @cppFiles
}

Write-Host 'Formatting complete.'
