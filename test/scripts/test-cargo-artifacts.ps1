$ErrorActionPreference = "Stop"
$repo = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
. (Join-Path $repo "scripts/shared/cargo-artifacts.ps1")
$probe = Join-Path $repo ("target/cargo-path-probe-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force $probe | Out-Null
$prior = $env:CARGO_TARGET_DIR
Push-Location $probe
try {
  Set-Content Cargo.toml '[package]
name = "cargo-path-probe"
version = "0.0.0"
edition = "2024"
[lib]
path = "probe.rs"
[workspace]'
  Set-Content probe.rs ''
  & cargo generate-lockfile --offline
  if ($LASTEXITCODE -ne 0) { throw "Fixture lockfile failed" }
  Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
  if ((Get-CargoReleaseDirectory) -ne (Join-Path $probe "target/release")) { throw "Default target directory differs" }
  $env:CARGO_TARGET_DIR = "relative artifacts"
  if ((Get-CargoReleaseDirectory) -ne (Join-Path $probe "relative artifacts/release")) { throw "Relative target directory differs" }
  $env:CARGO_TARGET_DIR = Join-Path $probe "absolute artifacts"
  if ((Get-CargoReleaseDirectory) -ne (Join-Path $probe "absolute artifacts/release")) { throw "Absolute target directory differs" }
  Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
  New-Item -ItemType Directory .cargo | Out-Null
  Set-Content .cargo/config.toml '[build]
target-dir = "configured artifacts"'
  if ((Get-CargoReleaseDirectory) -ne (Join-Path $probe "configured artifacts/release")) { throw "Configured target directory differs" }
} finally {
  $env:CARGO_TARGET_DIR = $prior
  Pop-Location
}
Write-Host "Cargo artifact paths: 4 cases passed."
