$ErrorActionPreference = "Stop"

$workspaceRoot = Resolve-Path (Join-Path $PSScriptRoot "..\..")
Set-Location $workspaceRoot

$packages = Get-ChildItem -LiteralPath (Join-Path $workspaceRoot "crates") -Directory |
ForEach-Object {
  $cargoToml = Join-Path $_.FullName "Cargo.toml"
  if (-not (Test-Path -LiteralPath $cargoToml)) {
    return
  }

  $cargoText = Get-Content -LiteralPath $cargoToml -Raw
  if ($cargoText -match '(?ms)^\[lib\].*?doctest\s*=\s*false') {
    return
  }

  $srcDir = Join-Path $_.FullName "src"
  if (-not (Test-Path -LiteralPath $srcDir)) {
    return
  }

  $hasRustDoc = @(Get-ChildItem -LiteralPath $srcDir -Recurse -Filter *.rs |
      Select-String -Pattern '```(?:rust|no_run|ignore)' -List)
  if ($hasRustDoc.Count -eq 0) {
    return
  }

  if ($cargoText -match '(?m)^name\s*=\s*"([^"]+)"') {
    $matches[1]
  }
} |
Sort-Object -Unique

if ($packages.Count -eq 0) {
  exit 0
}

$args = @("test", "-q", "--doc")
foreach ($package in $packages) {
  $args += @("-p", $package)
}

& cargo @args
exit $LASTEXITCODE
