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
  [string]$HelperCargoFeatures = "",
  [string]$LlvmSysPrefixEnvVar = "",
  [string[]]$HelperAdditionalLibPaths = @(),
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

function Move-ArchiveRootContents {
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
    Move-Item -LiteralPath $entry.FullName -Destination (Join-Path $Destination $entry.Name) -Force
  }
}

function Get-WindowsLlvmBinKeepList {
  return @(
    "lld-link.exe"
  )
}

function Get-UnixLlvmBinKeepList {
  return @(
    "clang",
    "clang++",
    "ld.lld",
    "lld",
    "llvm-ar",
    "llvm-as",
    "llvm-cov",
    "llvm-dis",
    "llvm-link",
    "llvm-lto",
    "llvm-lto2",
    "llvm-nm",
    "llvm-objcopy",
    "llvm-objdump",
    "llvm-profdata",
    "llvm-ranlib",
    "llvm-readelf",
    "llvm-readobj",
    "llvm-size",
    "llvm-strip",
    "llc",
    "opt"
  )
}

function Add-UnixSymlinkClosure {
  param(
    [string]$BinDir,
    [string[]]$InitialNames
  )

  $keep = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
  $queue = [System.Collections.Generic.Queue[string]]::new()

  foreach ($name in $InitialNames) {
    if ($name) {
      [void]$keep.Add($name)
      $queue.Enqueue($name)
    }
  }

  while ($queue.Count -gt 0) {
    $name = $queue.Dequeue()
    $path = Join-Path $BinDir $name
    if (-not (Test-Path -LiteralPath $path)) {
      continue
    }

    $entry = Get-Item -LiteralPath $path -Force
    if (-not ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
      continue
    }

    $targets = @($entry.Target | Where-Object { $_ })
    foreach ($target in $targets) {
      $targetPath = $target
      if (-not [IO.Path]::IsPathRooted($targetPath)) {
        $targetPath = [IO.Path]::GetFullPath((Join-Path $entry.DirectoryName $targetPath))
      }

      $binRoot = [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath($BinDir))
      $resolvedTarget = [IO.Path]::GetFullPath($targetPath)
      if (-not $resolvedTarget.StartsWith($binRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        continue
      }

      $targetName = Split-Path -Leaf $resolvedTarget
      if ($targetName -and $keep.Add($targetName)) {
        $queue.Enqueue($targetName)
      }
    }
  }

  return @($keep)
}

function Remove-LlvmPayload {
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
    New-Item -ItemType Directory -Force -Path $includeDir | Out-Null
  }

  $keep = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
  $binKeep = if ($IsWindows) {
    Get-WindowsLlvmBinKeepList
  }
  else {
    Add-UnixSymlinkClosure -BinDir $binDir -InitialNames (Get-UnixLlvmBinKeepList)
  }
  foreach ($name in $binKeep) {
    [void]$keep.Add($name)
  }

  foreach ($entry in (Get-ChildItem -LiteralPath $binDir -File -Force)) {
    if (-not $keep.Contains($entry.Name)) {
      Remove-Item -LiteralPath $entry.FullName -Force
    }
  }

  if (Test-Path -LiteralPath $includeDir) {
    Remove-Item -LiteralPath $includeDir -Force -Recurse
    New-Item -ItemType Directory -Force -Path $includeDir | Out-Null
  }

  if ($IsWindows) {
    foreach ($entry in (Get-ChildItem -LiteralPath $libDir -Force)) {
      Remove-Item -LiteralPath $entry.FullName -Force -Recurse
    }
  }
  else {
    $keepLibTopLevel = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    [void]$keepLibTopLevel.Add("clang")

    foreach ($entry in (Get-ChildItem -LiteralPath $libDir -Force)) {
      if ($entry.PSIsContainer) {
        if (-not $keepLibTopLevel.Contains($entry.Name)) {
          Remove-Item -LiteralPath $entry.FullName -Force -Recurse
        }
        continue
      }

      $name = $entry.Name
      $keepSharedLib = (
        $name -like "*.so" -or
        $name -like "*.so.*" -or
        $name -like "*.dylib" -or
        $name -like "*.dylib.*"
      )

      if (-not $keepSharedLib) {
        Remove-Item -LiteralPath $entry.FullName -Force
      }
    }
  }

  if (Test-Path -LiteralPath $shareDir) {
    Remove-Item -LiteralPath $shareDir -Force -Recurse
  }
  if (Test-Path -LiteralPath $libexecDir) {
    Remove-Item -LiteralPath $libexecDir -Force -Recurse
  }
}

function Get-LlvmConfigPath {
  param([string]$LlvmRoot)

  $llvmConfigName = if ($IsWindows) { "llvm-config.exe" } else { "llvm-config" }
  $llvmConfig = Join-Path (Join-Path $LlvmRoot "bin") $llvmConfigName
  if (Test-Path -LiteralPath $llvmConfig) {
    return $llvmConfig
  }
  return $null
}

function New-HelperLibraryShim {
  param(
    [string[]]$LibraryPaths
  )

  if (-not $IsWindows) {
    return $null
  }

  $resolvedPaths = @($LibraryPaths | Where-Object { $_ -and (Test-Path -LiteralPath $_) })
  if ($resolvedPaths.Count -eq 0) {
    return $null
  }

  $shimDir = Join-Path ([System.IO.Path]::GetTempPath()) ("fidan-helper-libshim-" + [Guid]::NewGuid().ToString("N"))
  $created = $false

  foreach ($libPath in $resolvedPaths) {
    $xml2sPath = Join-Path $libPath "xml2s.lib"
    if (Test-Path -LiteralPath $xml2sPath) {
      continue
    }

    $libxml2Path = Join-Path $libPath "libxml2.lib"
    if (Test-Path -LiteralPath $libxml2Path) {
      if (-not $created) {
        New-Item -ItemType Directory -Force -Path $shimDir | Out-Null
        $created = $true
      }
      Copy-Item -LiteralPath $libxml2Path -Destination (Join-Path $shimDir "xml2s.lib") -Force
    }
  }

  if ($created) {
    return $shimDir
  }

  return $null
}

function Invoke-HelperBuild {
  param(
    [string]$LlvmRoot,
    [string]$HelperCargoFeatures,
    [string]$LlvmSysPrefixEnvVar,
    [string[]]$HelperAdditionalLibPaths
  )

  $previousPath = $env:PATH
  $previousLib = $env:LIB
  $hadLib = [bool](Test-Path Env:LIB)
  $previousLlvmConfigPath = $env:LLVM_CONFIG_PATH
  $hadLlvmConfigPath = [bool](Test-Path Env:LLVM_CONFIG_PATH)
  $previousReleaseLto = $env:CARGO_PROFILE_RELEASE_LTO
  $hadReleaseLto = [bool](Test-Path Env:CARGO_PROFILE_RELEASE_LTO)
  $helperLibShimDir = $null
  $hadLlvmSysPrefix = $false
  $previousLlvmSysPrefix = ""
  if ($LlvmSysPrefixEnvVar) {
    $hadLlvmSysPrefix = [bool](Test-Path "Env:$LlvmSysPrefixEnvVar")
    if ($hadLlvmSysPrefix) {
      $previousLlvmSysPrefix = (Get-Item "Env:$LlvmSysPrefixEnvVar").Value
    }
  }

  try {
    $llvmBinDir = Join-Path $LlvmRoot "bin"
    if (Test-Path -LiteralPath $llvmBinDir) {
      $env:PATH = "$llvmBinDir$([System.IO.Path]::PathSeparator)$previousPath"
    }

    if ($LlvmSysPrefixEnvVar) {
      Set-Item -Path "Env:$LlvmSysPrefixEnvVar" -Value $LlvmRoot
    }

    $effectiveLibPaths = @($HelperAdditionalLibPaths | Where-Object { $_ })
    $helperLibShimDir = New-HelperLibraryShim -LibraryPaths $effectiveLibPaths
    if ($helperLibShimDir) {
      $effectiveLibPaths = @($helperLibShimDir) + $effectiveLibPaths
    }

    if ($effectiveLibPaths.Count -gt 0) {
      $libPathValue = $effectiveLibPaths -join [System.IO.Path]::PathSeparator
      if ($libPathValue) {
        if ($hadLib -and $previousLib) {
          $env:LIB = "$libPathValue$([System.IO.Path]::PathSeparator)$previousLib"
        }
        else {
          $env:LIB = $libPathValue
        }
      }
    }

    $llvmConfigPath = Get-LlvmConfigPath -LlvmRoot $LlvmRoot
    if ($llvmConfigPath) {
      $env:LLVM_CONFIG_PATH = $llvmConfigPath
    }
    elseif ($hadLlvmConfigPath) {
      $env:LLVM_CONFIG_PATH = $previousLlvmConfigPath
    }
    else {
      Remove-Item Env:LLVM_CONFIG_PATH -ErrorAction SilentlyContinue
    }

    if ($HelperCargoFeatures) {
      $env:CARGO_PROFILE_RELEASE_LTO = "false"
      cargo build -p fidan-llvm-helper --release --features $HelperCargoFeatures
    }
    else {
      $env:CARGO_PROFILE_RELEASE_LTO = "false"
      cargo build -p fidan-llvm-helper --release
    }
  }
  finally {
    $env:PATH = $previousPath

    if ($hadLib) {
      $env:LIB = $previousLib
    }
    else {
      Remove-Item Env:LIB -ErrorAction SilentlyContinue
    }

    if ($hadLlvmConfigPath) {
      $env:LLVM_CONFIG_PATH = $previousLlvmConfigPath
    }
    else {
      Remove-Item Env:LLVM_CONFIG_PATH -ErrorAction SilentlyContinue
    }

    if ($hadReleaseLto) {
      $env:CARGO_PROFILE_RELEASE_LTO = $previousReleaseLto
    }
    else {
      Remove-Item Env:CARGO_PROFILE_RELEASE_LTO -ErrorAction SilentlyContinue
    }

    if ($LlvmSysPrefixEnvVar) {
      if ($hadLlvmSysPrefix) {
        Set-Item -Path "Env:$LlvmSysPrefixEnvVar" -Value $previousLlvmSysPrefix
      }
      else {
        Remove-Item "Env:$LlvmSysPrefixEnvVar" -ErrorAction SilentlyContinue
      }
    }

    if ($helperLibShimDir -and (Test-Path -LiteralPath $helperLibShimDir)) {
      Remove-Item -LiteralPath $helperLibShimDir -Force -Recurse
    }
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
  Remove-Item -LiteralPath $localArchivePath -Force
  $localArchivePath = ""

  $helperDir = Join-Path $stageDir "helper"
  $llvmDir = Join-Path $stageDir "llvm"
  New-Item -ItemType Directory -Force -Path $helperDir | Out-Null
  Move-ArchiveRootContents -ExtractRoot $extractDir -Destination $llvmDir

  if (-not $SkipBuild) {
    Invoke-HelperBuild `
      -LlvmRoot $llvmDir `
      -HelperCargoFeatures $HelperCargoFeatures `
      -LlvmSysPrefixEnvVar $LlvmSysPrefixEnvVar `
      -HelperAdditionalLibPaths $HelperAdditionalLibPaths
  }

  $helperPath = Join-Path "target/release" $helperBinary
  if (-not (Test-Path -LiteralPath $helperPath)) {
    throw "Expected helper binary at '$helperPath'"
  }
  Copy-Item -LiteralPath $helperPath -Destination (Join-Path $helperDir $helperBinary)

  if ($Kind -eq "llvm") {
    Remove-LlvmPayload -LlvmRoot $llvmDir
  }

  $metadata = @{
    schema_version           = 1
    kind                     = $Kind
    toolchain_version        = $ToolchainVersion
    tool_version             = $ToolVersion
    host_triple              = $hostTriple
    supported_fidan_versions = $SupportedFidanVersions
    backend_protocol_version = $BackendProtocolVersion
    helper_relpath           = $helperRelPath
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
