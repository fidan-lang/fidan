$ErrorActionPreference = "Stop"

$workspaceRoot = Resolve-Path (Join-Path $PSScriptRoot "..\..")
Set-Location $workspaceRoot

# Cargo discovers unlabelled Rust examples and honors target doctest settings.
& cargo test -q --workspace --locked --doc
exit $LASTEXITCODE
