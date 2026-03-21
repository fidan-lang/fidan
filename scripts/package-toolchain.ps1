param(
  [string]$Kind = "llvm",
  [string]$ToolchainVersion = "",
  [Parameter(Mandatory = $true)]
  [string]$ToolVersion,
  [string]$SupportedFidanVersions = "",
  [int]$BackendProtocolVersion = 1,
  [string]$BaseUrl = "https://releases.fidan.dev",
  [string]$OutputRoot = "dist/release",
  [string]$UpstreamArchivePath = "",
  [string]$UpstreamArchiveUrl = "",
  [string]$UpstreamArchiveSha256 = "",
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

function Copy-ResourceToFile {
  param(
    [string]$SourceUrl,
    [string]$Destination
  )
  if ($SourceUrl.StartsWith("file://")) {
    Copy-Item -LiteralPath $SourceUrl.Substring(7) -Destination $Destination
    return
  }
  Invoke-WebRequest -Uri $SourceUrl -OutFile $Destination
}

function Expand-AnyArchive {
  param(
    [string]$ArchivePath,
    [string]$Destination
  )

  $lower = $ArchivePath.ToLowerInvariant()
  if ($lower.EndsWith(".zip")) {
    Expand-Archive -LiteralPath $ArchivePath -DestinationPath $Destination -Force
    return
  }

  tar -xf $ArchivePath -C $Destination
}

function Get-Sha256 {
  param([string]$Path)
  return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Copy-ArchiveRootContents {
  param(
    [string]$ExtractRoot,
    [string]$Destination
  )

  $entries = @(Get-ChildItem -LiteralPath $ExtractRoot -Force)
  $sourceRoot = $ExtractRoot

  if ($entries.Count -eq 1 -and $entries[0].PSIsContainer) {
    $sourceRoot = $entries[0].FullName
  }

  New-Item -ItemType Directory -Force -Path $Destination | Out-Null
  foreach ($entry in (Get-ChildItem -LiteralPath $sourceRoot -Force)) {
    Copy-Item -LiteralPath $entry.FullName -Destination (Join-Path $Destination $entry.Name) -Recurse -Force
  }
}

function Get-WindowsLlvmBinKeepList {
  return @(
    "clang.exe",
    "clang++.exe",
    "clang-cl.exe",
    "lld-link.exe",
    "lld.exe",
    "ld.lld.exe",
    "llvm-ar.exe",
    "llvm-lib.exe",
    "llvm-ranlib.exe",
    "llvm-rc.exe",
    "llvm-mt.exe",
    "llvm-nm.exe",
    "llvm-objcopy.exe",
    "llvm-objdump.exe",
    "llvm-strip.exe",
    "llvm-readobj.exe",
    "llvm-readelf.exe",
    "llvm-as.exe",
    "llvm-dis.exe",
    "llvm-link.exe",
    "llvm-cvtres.exe",
    "llc.exe",
    "opt.exe",
    "llvm-lto.exe",
    "llvm-lto2.exe",
    "libclang.dll",
    "LLVM-C.dll",
    "LTO.dll",
    "Remarks.dll",
    "libomp.dll",
    "libiomp5md.dll"
  )
}

function Prune-WindowsLlvmPayload {
  param([string]$LlvmRoot)

  $binDir = Join-Path $LlvmRoot "bin"
  $libDir = Join-Path $LlvmRoot "lib"
  $includeDir = Join-Path $LlvmRoot "include"
  $shareDir = Join-Path $LlvmRoot "share"
  $libexecDir = Join-Path $LlvmRoot "libexec"

  if (-not (Test-Path -LiteralPath $binDir)) {
    throw "Expected LLVM bin directory at '$binDir'"
  }
  if (-not (Test-Path -LiteralPath $libDir)) {
    throw "Expected LLVM lib directory at '$libDir'"
  }
  if (-not (Test-Path -LiteralPath $includeDir)) {
    throw "Expected LLVM include directory at '$includeDir'"
  }

  $keep = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
  foreach ($name in (Get-WindowsLlvmBinKeepList)) {
    [void]$keep.Add($name)
  }

  foreach ($entry in (Get-ChildItem -LiteralPath $binDir -File -Force)) {
    if (-not $keep.Contains($entry.Name)) {
      Remove-Item -LiteralPath $entry.FullName -Force
    }
  }

  if (Test-Path -LiteralPath $shareDir) {
    Remove-Item -LiteralPath $shareDir -Force -Recurse
  }
  if (Test-Path -LiteralPath $libexecDir) {
    Remove-Item -LiteralPath $libexecDir -Force -Recurse
  }
}

if (($UpstreamArchivePath -eq "") -and ($UpstreamArchiveUrl -eq "")) {
  throw "Either -UpstreamArchivePath or -UpstreamArchiveUrl is required"
}

if (-not $ToolchainVersion) {
  $ToolchainVersion = Get-WorkspaceVersion
}
if (-not $SupportedFidanVersions) {
  $SupportedFidanVersions = "^$ToolchainVersion"
}
if (-not $UpstreamArchiveSha256) {
  throw "-UpstreamArchiveSha256 is required for reproducible LLVM toolchain packaging"
}

$hostTriple = Get-HostTriple
$helperBinary = if ($IsWindows) { "fidan-llvm-helper.exe" } else { "fidan-llvm-helper" }

if (-not $SkipBuild) {
  cargo build -p fidan-cli --release --bin fidan-llvm-helper
}

$helperPath = Join-Path "target/release" $helperBinary
if (-not (Test-Path -LiteralPath $helperPath)) {
  throw "Expected helper binary at '$helperPath'"
}

$payloadDir = Join-Path $OutputRoot "payload"
$artifactDir = Join-Path (Join-Path (Join-Path $payloadDir "toolchains/$Kind") $ToolchainVersion) $hostTriple
$fragmentDir = Join-Path $OutputRoot "fragments"
$archiveName = "$Kind-toolchain-$ToolchainVersion-$hostTriple.tar.gz"
$archivePath = Join-Path $artifactDir $archiveName
$helperRelPath = "helper/$helperBinary"

New-Item -ItemType Directory -Force -Path $artifactDir | Out-Null
New-Item -ItemType Directory -Force -Path $fragmentDir | Out-Null

$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("fidan-toolchain-" + [Guid]::NewGuid().ToString("N"))
$inputDir = Join-Path $tempRoot "input"
$extractDir = Join-Path $tempRoot "extract"
$stageDir = Join-Path $tempRoot "package"
Initialize-CleanDirectory -Path $inputDir
Initialize-CleanDirectory -Path $extractDir
Initialize-CleanDirectory -Path $stageDir

$upstreamName = if ($UpstreamArchivePath) {
  Split-Path -Leaf $UpstreamArchivePath
}
else {
  Split-Path -Leaf ([Uri]$UpstreamArchiveUrl).AbsolutePath
}
if (-not $upstreamName) {
  $upstreamName = "$Kind-upstream.tar.gz"
}
$localArchivePath = Join-Path $inputDir $upstreamName

try {
  if ($UpstreamArchivePath) {
    Copy-Item -LiteralPath $UpstreamArchivePath -Destination $localArchivePath
  }
  else {
    Copy-ResourceToFile -SourceUrl $UpstreamArchiveUrl -Destination $localArchivePath
  }

  $actualSha = Get-Sha256 -Path $localArchivePath
  if ($actualSha -ne $UpstreamArchiveSha256.ToLowerInvariant()) {
    throw "SHA-256 mismatch for upstream archive (expected $UpstreamArchiveSha256, got $actualSha)"
  }

  Expand-AnyArchive -ArchivePath $localArchivePath -Destination $extractDir

  $helperDir = Join-Path $stageDir "helper"
  $llvmDir = Join-Path $stageDir "llvm"
  New-Item -ItemType Directory -Force -Path $helperDir | Out-Null
  Copy-Item -LiteralPath $helperPath -Destination (Join-Path $helperDir $helperBinary)
  Copy-ArchiveRootContents -ExtractRoot $extractDir -Destination $llvmDir

  if ($Kind -eq "llvm" -and $IsWindows) {
    Prune-WindowsLlvmPayload -LlvmRoot $llvmDir
  }

  $metadata = @{
    schema_version            = 1
    kind                      = $Kind
    toolchain_version         = $ToolchainVersion
    tool_version              = $ToolVersion
    host_triple               = $hostTriple
    supported_fidan_versions  = $SupportedFidanVersions
    backend_protocol_version  = $BackendProtocolVersion
    helper_relpath            = $helperRelPath
  }
  $metadata | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $stageDir "metadata.json") -Encoding UTF8

  tar -czf $archivePath -C $stageDir .
}
finally {
  if (Test-Path -LiteralPath $tempRoot) {
    Remove-Item -LiteralPath $tempRoot -Force -Recurse
  }
}

$sha256 = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
$releaseUrl = "$($BaseUrl.TrimEnd('/'))/toolchains/$Kind/$ToolchainVersion/$hostTriple/$archiveName"

$fragment = @{
  schema_version = 1
  fidan_versions = @()
  toolchains     = @(
    @{
      kind                     = $Kind
      toolchain_version        = $ToolchainVersion
      tool_version             = $ToolVersion
      host_triple              = $hostTriple
      url                      = $releaseUrl
      sha256                   = $sha256
      helper_relpath           = $helperRelPath
      supported_fidan_versions = $SupportedFidanVersions
      backend_protocol_version = $BackendProtocolVersion
    }
  )
}

$fragmentPath = Join-Path $fragmentDir "$Kind-toolchain-$ToolchainVersion-$hostTriple.json"
$fragment | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $fragmentPath -Encoding UTF8

Write-Host "Packaged $Kind toolchain $ToolchainVersion for $hostTriple"
Write-Host "Archive: $archivePath"
Write-Host "Fragment: $fragmentPath"
