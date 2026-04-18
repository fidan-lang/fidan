param(
  [string]$Version = "",
  [string]$BaseUrl = "https://releases.fidan.dev",
  [string]$OutputRoot = "dist/release",
  [switch]$SkipBuild,
  [switch]$PrepareWinget,
  [switch]$SubmitWinget,
  [string]$WingetManifestRoot = "config/winget/manifest",
  [string]$BootstrapScriptUrl = "https://fidan.dev/install.ps1"
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

function Invoke-WindowsReleaseHelper {
  param(
    [ValidateSet("build-installer", "prepare-winget", "submit-winget")]
    [string]$Mode,
    [string]$ResolvedVersion,
    [string]$ResolvedOutputRoot,
    [string]$ResolvedHostTriple = "",
    [string]$ResolvedBootstrapScriptUrl = "",
    [string]$ResolvedWingetManifestRoot = "",
    [string]$ResolvedBinaryPath = ""
  )

  if (-not $IsWindows) {
    throw "Windows release helper mode '$Mode' can only run on Windows."
  }

  $helperScript = Join-Path $PSScriptRoot "package-release-windows.ps1"
  if (-not (Test-Path -LiteralPath $helperScript)) {
    throw "Missing helper script: '$helperScript'"
  }

  $scriptArgs = @{
    Mode       = $Mode
    Version    = $ResolvedVersion
    OutputRoot = $ResolvedOutputRoot
  }

  if ($Mode -eq "build-installer") {
    $scriptArgs["HostTriple"] = $ResolvedHostTriple
    $scriptArgs["BootstrapScriptUrl"] = $ResolvedBootstrapScriptUrl
    $scriptArgs["BinaryPath"] = $ResolvedBinaryPath
  }

  if ($Mode -eq "submit-winget" -or $Mode -eq "prepare-winget") {
    $scriptArgs["WingetManifestRoot"] = $ResolvedWingetManifestRoot
  }

  & $helperScript @scriptArgs
}

function Get-RuntimeArtifactNames {
  if ($IsWindows) {
    return @(
      "fidan_runtime.lib",
      "fidan_runtime.dll",
      "fidan_runtime.dll.lib"
    )
  }

  if ($IsMacOS) {
    return @(
      "libfidan_runtime.a",
      "libfidan_runtime.dylib"
    )
  }

  return @(
    "libfidan_runtime.a",
    "libfidan_runtime.so"
  )
}

function Get-LibFidanArtifactNames {
  if ($IsWindows) {
    return @(
      @{ Source = "libfidan.lib"; Destination = "libfidan.lib" },
      @{ Source = "libfidan.dll"; Destination = "libfidan.dll" },
      @{ Source = "libfidan.dll.lib"; Destination = "libfidan.dll.lib" }
    )
  }

  if ($IsMacOS) {
    return @(
      @{ Source = "liblibfidan.a"; Destination = "libfidan.a" },
      @{ Source = "liblibfidan.dylib"; Destination = "libfidan.dylib" }
    )
  }

  return @(
    @{ Source = "liblibfidan.a"; Destination = "libfidan.a" },
    @{ Source = "liblibfidan.so"; Destination = "libfidan.so" }
  )
}

function Test-EnvVarTruthy {
  param (
    [Parameter(Mandatory = $true)]
    [string]$Name
  )

  $value = [System.Environment]::GetEnvironmentVariable($Name)

  if (-not $value) {
    return $false
  }

  $truthyValues = @("true", "yes", "y", "1")

  return $truthyValues -contains $value.ToLower()
}

if (-not $Version) {
  $Version = Get-WorkspaceVersion
}

if ($PrepareWinget -and $SubmitWinget) {
  throw "Use either -PrepareWinget or -SubmitWinget, not both."
}

if ($PrepareWinget) {
  Invoke-WindowsReleaseHelper -Mode "prepare-winget" -ResolvedVersion $Version -ResolvedOutputRoot $OutputRoot -ResolvedWingetManifestRoot $WingetManifestRoot
  return
}

if ($SubmitWinget) {
  Invoke-WindowsReleaseHelper -Mode "submit-winget" -ResolvedVersion $Version -ResolvedOutputRoot $OutputRoot -ResolvedWingetManifestRoot $WingetManifestRoot
  return
}

$hostTriple = Get-HostTriple
$binaryName = if ($IsWindows) { "fidan.exe" } else { "fidan" }
$shouldBuildWindowsInstaller = $IsWindows -and (
  (Test-EnvVarTruthy -Name "GITHUB_ACTIONS") -or
  (Test-EnvVarTruthy -Name "FIDAN_BUILD_INSTALLER")
)

if (-not $SkipBuild) {
  cargo build -p fidan-cli -p fidan-runtime -p libfidan --release
}

$binaryPath = Join-Path "target/release" $binaryName
if (-not (Test-Path -LiteralPath $binaryPath)) {
  throw "Expected release binary at '$binaryPath'"
}

if ($shouldBuildWindowsInstaller) {
  Invoke-WindowsReleaseHelper -Mode "build-installer" -ResolvedVersion $Version -ResolvedOutputRoot $OutputRoot -ResolvedHostTriple $hostTriple -ResolvedBootstrapScriptUrl $BootstrapScriptUrl -ResolvedBinaryPath $binaryPath
}

$runtimeArtifacts = @()
foreach ($name in (Get-RuntimeArtifactNames)) {
  $path = Join-Path "target/release" $name
  if (-not (Test-Path -LiteralPath $path)) {
    throw "Expected runtime artifact at '$path'"
  }
  $runtimeArtifacts += $path
}

$libfidanArtifacts = @()
foreach ($artifact in (Get-LibFidanArtifactNames)) {
  $path = Join-Path "target/release" $artifact.Source
  if (-not (Test-Path -LiteralPath $path)) {
    throw "Expected libfidan artifact at '$path'"
  }
  $libfidanArtifacts += [PSCustomObject]@{
    SourcePath      = $path
    DestinationName = $artifact.Destination
  }
}

$libfidanHeader = "crates/libfidan/include/fidan.h"
if (-not (Test-Path -LiteralPath $libfidanHeader)) {
  throw "Expected libfidan header at '$libfidanHeader'"
}

$libfidanExample = "crates/libfidan/examples/embed_c/main.c"
if (-not (Test-Path -LiteralPath $libfidanExample)) {
  throw "Expected libfidan example at '$libfidanExample'"
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
foreach ($artifact in $runtimeArtifacts) {
  Copy-Item -LiteralPath $artifact -Destination (Join-Path $stageDir (Split-Path -Leaf $artifact))
}
foreach ($artifact in $libfidanArtifacts) {
  Copy-Item -LiteralPath $artifact.SourcePath -Destination (Join-Path $stageDir $artifact.DestinationName)
}
New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "include") | Out-Null
Copy-Item -LiteralPath $libfidanHeader -Destination (Join-Path $stageDir "include/fidan.h")
New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "examples/embed_c") | Out-Null
Copy-Item -LiteralPath $libfidanExample -Destination (Join-Path $stageDir "examples/embed_c/main.c")
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
