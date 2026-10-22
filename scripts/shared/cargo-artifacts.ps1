# Ask Cargo so environment and .cargo/config.toml target-dir settings agree.
function Get-CargoReleaseDirectory {
  $metadata = & cargo metadata --format-version 1 --no-deps --locked
  if ($LASTEXITCODE -ne 0) { throw "Failed to determine Cargo artifact directory" }
  $targetDirectory = ($metadata | ConvertFrom-Json).target_directory
  if (-not $targetDirectory -or -not [IO.Path]::IsPathRooted($targetDirectory)) {
    throw "Cargo returned an invalid artifact directory"
  }
  return Join-Path $targetDirectory "release"
}
