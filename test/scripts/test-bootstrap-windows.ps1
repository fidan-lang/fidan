$ErrorActionPreference = "Stop"

if (-not $IsWindows) {
  throw "test/scripts/test-bootstrap-windows.ps1 can only run on Windows."
}

function Get-HostTripleForTest {
  $archPart = switch -Regex ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()) {
    "^x64$" { "x86_64"; break }
    "^arm64$" { "aarch64"; break }
    default { throw "Unsupported test host architecture: $($_)" }
  }

  return "$archPart-pc-windows-msvc"
}

function Assert-PathExists {
  param(
    [string]$Path,
    [string]$Description
  )

  if (-not (Test-Path -LiteralPath $Path)) {
    throw "$Description not found at '$Path'"
  }
}

$scratchRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("fidan-bootstrap-test-" + [Guid]::NewGuid().ToString("N"))
$archiveStageDir = Join-Path $scratchRoot "stage"
$payloadDir = Join-Path $scratchRoot "payload"
$installRoot = Join-Path $scratchRoot "install-root"

try {
  New-Item -ItemType Directory -Force -Path $archiveStageDir | Out-Null
  New-Item -ItemType Directory -Force -Path $payloadDir | Out-Null

  $dummyExePath = Join-Path $archiveStageDir "fidan.exe"
  [System.IO.File]::WriteAllBytes($dummyExePath, [byte[]](0x4D, 0x5A, 0x90, 0x00))

  $hostTriple = Get-HostTripleForTest
  $version = "0.0.0-test"
  $archivePath = Join-Path $payloadDir "fidan-$version-$hostTriple.tar.gz"
  tar -czf $archivePath -C $archiveStageDir .
  if ($LASTEXITCODE -ne 0) {
    throw "Failed to create bootstrap test archive."
  }

  $manifestPath = Join-Path $scratchRoot "manifest.json"
  $archiveSha = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
  $manifest = @{
    schema_version = 1
    fidan_versions = @(
      @{
        version        = $version
        host_triple    = $hostTriple
        url            = "file://$archivePath"
        sha256         = $archiveSha
        binary_relpath = "fidan.exe"
        vc_redist_min_version = "14.0.0.0"
      }
    )
    toolchains     = @()
  }
  $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $manifestPath -Encoding UTF8

  $repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
  $bootstrapPath = Join-Path $repoRoot "scripts/bootstrap.ps1"
  $bootstrapArgs = @(
    "-NoLogo",
    "-NoProfile",
    "-ExecutionPolicy",
    "Bypass",
    "-File",
    $bootstrapPath,
    "-Version",
    $version,
    "-ManifestUrl",
    "file://$manifestPath",
    "-InstallRoot",
    $installRoot,
    "-SkipPathUpdate"
  )

  $previousModulePath = $env:PSModulePath
  $machineModulePath = [System.Environment]::GetEnvironmentVariable("PSModulePath", "Machine")
  $userModulePath = [System.Environment]::GetEnvironmentVariable("PSModulePath", "User")
  $normalizedModulePathParts = @()
  if ($machineModulePath) {
    $normalizedModulePathParts += $machineModulePath
  }
  if ($userModulePath) {
    $normalizedModulePathParts += $userModulePath
  }
  $normalizedModulePath = $normalizedModulePathParts -join ';'

  if ($normalizedModulePath) {
    $env:PSModulePath = $normalizedModulePath
  }

  try {
    $process = Start-Process -FilePath "powershell.exe" -ArgumentList $bootstrapArgs -Wait -PassThru -NoNewWindow
  }
  finally {
    $env:PSModulePath = $previousModulePath
  }

  if ($process.ExitCode -ne 0) {
    throw "bootstrap.ps1 smoke test failed with exit code $($process.ExitCode)."
  }

  $versionDir = Join-Path (Join-Path $installRoot "versions") $version
  $currentDir = Join-Path $installRoot "current"
  $installsPath = Join-Path $installRoot "metadata/installs.json"
  $activePath = Join-Path $installRoot "metadata/active-version.json"

  Assert-PathExists -Path $versionDir -Description "Installed version directory"
  Assert-PathExists -Path (Join-Path $versionDir "fidan.exe") -Description "Installed Fidan executable"
  Assert-PathExists -Path $currentDir -Description "Current junction"
  Assert-PathExists -Path (Join-Path $currentDir "fidan.exe") -Description "Current Fidan executable"
  Assert-PathExists -Path $installsPath -Description "Bootstrap installs metadata"
  Assert-PathExists -Path $activePath -Description "Bootstrap active-version metadata"

  $activeMetadata = Get-Content -LiteralPath $activePath -Raw | ConvertFrom-Json
  if ($activeMetadata.active_version -ne $version) {
    throw "Expected active bootstrap version '$version', got '$($activeMetadata.active_version)'."
  }
}
finally {
  if (Test-Path -LiteralPath $scratchRoot) {
    Remove-Item -LiteralPath $scratchRoot -Force -Recurse
  }
}

Write-Host "Bootstrap Windows smoke test passed."
