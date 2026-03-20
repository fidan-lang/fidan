param(
  [string]$Version = "",
  [string]$BaseUrl = "https://releases.fidan.dev",
  [string]$OutputRoot = "dist/release",
  [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

function Get-WorkspaceVersion {
  $cargoToml = Get-Content "Cargo.toml" -Raw
  $match = [regex]::Match($cargoToml, '(?ms)\[workspace\.package\].*?version\s*=\s*"([^"]+)"')
  if (-not $match.Success) {
    throw "Failed to determine workspace version from Cargo.toml"
  }
  return $match.Groups[1].Value
}

function Get-HostTriple {
  $osPart = if ($IsWindows) {
    "pc-windows-msvc"
  }
  elseif ($IsMacOS) {
    "apple-darwin"
  }
  else {
    "unknown-linux-gnu"
  }

  $archPart = switch -Regex ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()) {
    "^x64$" { "x86_64"; break }
    "^arm64$" { "aarch64"; break }
    default { throw "Unsupported architecture: $($_)" }
  }

  return "$archPart-$osPart"
}

function Initialize-CleanDirectory {
  param([string]$Path)
  if (Test-Path -LiteralPath $Path) {
    Remove-Item -LiteralPath $Path -Force -Recurse
  }
  New-Item -ItemType Directory -Force -Path $Path | Out-Null
}

if (-not $Version) {
  $Version = Get-WorkspaceVersion
}

$hostTriple = Get-HostTriple
$binaryName = if ($IsWindows) { "fidan.exe" } else { "fidan" }

if (-not $SkipBuild) {
  cargo build -p fidan-cli --release
}

$binaryPath = Join-Path "target/release" $binaryName
if (-not (Test-Path -LiteralPath $binaryPath)) {
  throw "Expected release binary at '$binaryPath'"
}

$payloadDir = Join-Path $OutputRoot "payload"
$artifactDir = Join-Path (Join-Path $payloadDir "fidan/$Version") $hostTriple
$fragmentDir = Join-Path $OutputRoot "fragments"
$archiveName = "fidan-$Version-$hostTriple.tar.gz"
$archivePath = Join-Path $artifactDir $archiveName
$binaryRelPath = $binaryName

New-Item -ItemType Directory -Force -Path $artifactDir | Out-Null
New-Item -ItemType Directory -Force -Path $fragmentDir | Out-Null

$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("fidan-release-" + [Guid]::NewGuid().ToString("N"))
$stageDir = Join-Path $tempRoot "package"
Initialize-CleanDirectory -Path $stageDir

Copy-Item -LiteralPath $binaryPath -Destination (Join-Path $stageDir $binaryName)
if (Test-Path -LiteralPath "README.md") {
  Copy-Item -LiteralPath "README.md" -Destination (Join-Path $stageDir "README.md")
}
if (Test-Path -LiteralPath "LICENSE") {
  Copy-Item -LiteralPath "LICENSE" -Destination (Join-Path $stageDir "LICENSE")
}

try {
  tar -czf $archivePath -C $stageDir .
}
finally {
  if (Test-Path -LiteralPath $tempRoot) {
    Remove-Item -LiteralPath $tempRoot -Force -Recurse
  }
}

$sha256 = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
$releaseUrl = "$($BaseUrl.TrimEnd('/'))/fidan/$Version/$hostTriple/$archiveName"

$fragment = @{
  schema_version = 1
  fidan_versions = @(
    @{
      version        = $Version
      host_triple    = $hostTriple
      url            = $releaseUrl
      sha256         = $sha256
      binary_relpath = $binaryRelPath
    }
  )
  toolchains     = @()
}

$fragmentPath = Join-Path $fragmentDir "fidan-$Version-$hostTriple.json"
$fragment | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $fragmentPath -Encoding UTF8

Write-Host "Packaged Fidan $Version for $hostTriple"
Write-Host "Archive: $archivePath"
Write-Host "Fragment: $fragmentPath"
