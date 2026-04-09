param(
  [string]$Kind = "llvm",
  [string]$ToolchainVersion = "",
  [Parameter(Mandatory = $true)]
  [string]$ToolVersion,
  [string]$SupportedFidanVersions = "",
  [int]$BackendProtocolVersion = 0,
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

function Get-HelperPackageName {
  param([string]$Kind)

  switch ($Kind) {
    "llvm" { return "fidan-llvm-helper" }
    "ai-analysis" { return "fidan-ai-analysis-helper" }
    default { throw "Unsupported toolchain kind '$Kind'" }
  }
}

function Get-HelperVersion {
  param([string]$Kind)
  $packageName = Get-HelperPackageName -Kind $Kind
  $cargoToml = Get-Content "crates/$packageName/Cargo.toml" -Raw
  $match = [regex]::Match($cargoToml, '(?m)^version\s*=\s*"([^"]+)"')
  if (-not $match.Success) {
    throw "Failed to determine version from crates/$packageName/Cargo.toml"
  }
  return $match.Groups[1].Value
}

function Get-HelperBinaryName {
  param([string]$Kind)

  $baseName = Get-HelperPackageName -Kind $Kind
  if ($IsWindows) {
    return "$baseName.exe"
  }
  return $baseName
}

function Get-ExecCommandRegistrations {
  param([string]$Kind)

  switch ($Kind) {
    "ai-analysis" {
      return @(
        @{
          namespace   = "ai"
          description = "AI analysis helper commands"
        }
      )
    }
    default {
      return @()
    }
  }
}

function Get-LlvmBackendProtocolVersion {
  $source = Get-Content "crates/fidan-driver/src/llvm_helper.rs" -Raw
  $match = [regex]::Match(
    $source,
    'pub\s+const\s+LLVM_BACKEND_PROTOCOL_VERSION\s*:\s*u32\s*=\s*(\d+)\s*;'
  )
  if (-not $match.Success) {
    throw "Failed to determine LLVM backend protocol version from crates/fidan-driver/src/llvm_helper.rs"
  }
  return [int]$match.Groups[1].Value
}

function Get-AiAnalysisBackendProtocolVersion {
  $source = Get-Content "crates/fidan-driver/src/ai_analysis.rs" -Raw
  $match = [regex]::Match(
    $source,
    'pub\s+const\s+AI_ANALYSIS_HELPER_PROTOCOL_VERSION\s*:\s*u32\s*=\s*(\d+)\s*;'
  )
  if (-not $match.Success) {
    throw "Failed to determine AI analysis helper protocol version from crates/fidan-driver/src/ai_analysis.rs"
  }
  return [int]$match.Groups[1].Value
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
    $localPath = $SourceUrl.Substring(7)
    if ($localPath -notmatch '^[A-Za-z]:[\\/]' ) {
      $uri = [Uri]$SourceUrl
      if (-not $uri.IsFile) {
        throw "Unsupported file URL '$SourceUrl'"
      }
      $localPath = $uri.LocalPath
      if ($IsWindows -and $localPath.StartsWith("/") -and $localPath.Length -ge 3 -and $localPath[2] -eq ':') {
        $localPath = $localPath.Substring(1)
      }
    }
    Copy-Item -LiteralPath $localPath -Destination $Destination
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
  if ($LASTEXITCODE -ne 0) {
    throw "Failed to extract archive '$ArchivePath'"
  }
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

function Copy-UpstreamLegalFiles {
  param(
    [string]$SourceRoot,
    [string]$StageDir,
    [string]$Kind
  )

  $licensesDir = Join-Path $StageDir "licenses"
  $copied = $false

  $licenseCandidates = @(
    "LICENSE.TXT",
    "LICENSE.txt",
    "LICENSE",
    "NOTICE.TXT",
    "NOTICE.txt",
    "NOTICE"
  )

  foreach ($candidate in $licenseCandidates) {
    $sourcePath = Join-Path $SourceRoot $candidate
    if (-not (Test-Path -LiteralPath $sourcePath)) {
      continue
    }

    if (-not $copied) {
      New-Item -ItemType Directory -Force -Path $licensesDir | Out-Null
      $copied = $true
    }

    $targetName = "$($Kind.ToUpperInvariant())-$candidate"
    Copy-Item -LiteralPath $sourcePath -Destination (Join-Path $licensesDir $targetName) -Force
  }

  if ((-not $copied) -and $Kind -eq "llvm") {
    $fallbackLicense = Join-Path $PSScriptRoot "..\\assets\\licenses\\llvm\\LICENSE.txt"
    if (Test-Path -LiteralPath $fallbackLicense) {
      New-Item -ItemType Directory -Force -Path $licensesDir | Out-Null
      Copy-Item -LiteralPath $fallbackLicense -Destination (Join-Path $licensesDir "LLVM-LICENSE.txt") -Force
    }
  }
}

function Get-WindowsLlvmBinKeepList {
  return @(
    "lld-link.exe",
    "llvm-strip.exe"
  )
}

function Get-LinuxLlvmBinKeepList {
  return @(
    "clang",
    "ld.lld",
    "llvm-strip"
  )
}

function Get-MacOsLlvmBinKeepList {
  return @(
    "clang",
    "ld.lld",
    "llvm-strip"
  )
}

function Get-UnixLlvmBinKeepList {
  if ($IsLinux) {
    return Get-LinuxLlvmBinKeepList
  }

  return Get-MacOsLlvmBinKeepList
}

function Add-UnixSymlinkClosure {
  param(
    [string]$DirectoryPath,
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
    $path = Join-Path $DirectoryPath $name
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

      $directoryRoot = [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath($DirectoryPath))
      $resolvedTarget = [IO.Path]::GetFullPath($targetPath)
      if (-not $resolvedTarget.StartsWith($directoryRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
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
    Add-UnixSymlinkClosure -DirectoryPath $binDir -InitialNames (Get-UnixLlvmBinKeepList)
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
    if ($IsLinux) {
      [void]$keepLibTopLevel.Add("x86_64-unknown-linux-gnu")
    }
    $libKeepCandidates = @(
      Get-ChildItem -LiteralPath $libDir -Force |
      Where-Object {
        if ($_.PSIsContainer) {
          return $false
        }

        return (
          $_.Name -like "libclang.so" -or
          $_.Name -like "libclang.so.*" -or
          $_.Name -like "libclang-cpp.so" -or
          $_.Name -like "libclang-cpp.so.*" -or
          $_.Name -like "libLTO.so" -or
          $_.Name -like "libLTO.so.*" -or
          $_.Name -like "libclang.dylib" -or
          $_.Name -like "libclang.dylib.*" -or
          $_.Name -like "libclang-cpp.dylib" -or
          $_.Name -like "libclang-cpp.dylib.*" -or
          $_.Name -like "libLTO.dylib" -or
          $_.Name -like "libLTO.dylib.*"
        )
      } |
      Select-Object -ExpandProperty Name
    )
    $libKeep = Add-UnixSymlinkClosure -DirectoryPath $libDir -InitialNames $libKeepCandidates
    $keepLibNames = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($name in $libKeep) {
      [void]$keepLibNames.Add($name)
    }

    foreach ($entry in (Get-ChildItem -LiteralPath $libDir -Force)) {
      if ($entry.PSIsContainer) {
        if (-not $keepLibTopLevel.Contains($entry.Name)) {
          Remove-Item -LiteralPath $entry.FullName -Force -Recurse
        }
        continue
      }

      if (-not $keepLibNames.Contains($entry.Name)) {
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
    $libxml2sPath = Join-Path $libPath "libxml2s.lib"
    if ((Test-Path -LiteralPath $xml2sPath) -and (Test-Path -LiteralPath $libxml2sPath)) {
      continue
    }

    $libxml2Path = Join-Path $libPath "libxml2.lib"
    if (Test-Path -LiteralPath $libxml2Path) {
      if (-not $created) {
        New-Item -ItemType Directory -Force -Path $shimDir | Out-Null
        $created = $true
      }
      Copy-Item -LiteralPath $libxml2Path -Destination (Join-Path $shimDir "xml2s.lib") -Force
      Copy-Item -LiteralPath $libxml2Path -Destination (Join-Path $shimDir "libxml2s.lib") -Force
    }
  }

  if ($created) {
    return $shimDir
  }

  return $null
}

function Get-UnixHostToolchainCommand {
  param(
    [Parameter(Mandatory = $true)]
    [string[]]$Candidates,
    [string]$XcrunFind = ""
  )

  if ($IsMacOS -and $XcrunFind) {
    try {
      $resolved = & xcrun --find $XcrunFind 2>$null
      if ($LASTEXITCODE -eq 0 -and $resolved) {
        return $resolved.Trim()
      }
    }
    catch {
    }
  }

  foreach ($candidate in $Candidates) {
    $command = Get-Command $candidate -ErrorAction SilentlyContinue
    if ($command) {
      return $command.Source
    }
  }

  throw "Failed to locate a host toolchain command (tried: $($Candidates -join ', '))"
}

function Invoke-HelperBuild {
  param(
    [string]$Kind,
    [string]$HelperPackage,
    [string]$HelperBinary,
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
  $previousCc = $env:CC
  $hadCc = [bool](Test-Path Env:CC)
  $previousCxx = $env:CXX
  $hadCxx = [bool](Test-Path Env:CXX)
  $previousAr = $env:AR
  $hadAr = [bool](Test-Path Env:AR)
  $previousRanlib = $env:RANLIB
  $hadRanlib = [bool](Test-Path Env:RANLIB)
  $linkerEnvName = "CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER"
  $hadMacOsLinker = [bool](Test-Path "Env:$linkerEnvName")
  $previousMacOsLinker = if ($hadMacOsLinker) { (Get-Item "Env:$linkerEnvName").Value } else { "" }
  $previousRustFlags = $env:RUSTFLAGS
  $hadRustFlags = [bool](Test-Path Env:RUSTFLAGS)
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
    $helperPath = Join-Path "target/release" $HelperBinary
    if (Test-Path -LiteralPath $helperPath) {
      Remove-Item -LiteralPath $helperPath -Force
    }

    if ($Kind -eq "llvm") {
      if (-not $LlvmRoot) {
        throw "LLVM helper build requires -LlvmRoot"
      }

      $llvmBinDir = Join-Path $LlvmRoot "bin"
      $llvmLibDir = Join-Path $LlvmRoot "lib"
      if (Test-Path -LiteralPath $llvmBinDir) {
        if ($IsWindows) {
          $env:PATH = "$llvmBinDir$([System.IO.Path]::PathSeparator)$previousPath"
        }
        else {
          $env:PATH = "$previousPath$([System.IO.Path]::PathSeparator)$llvmBinDir"
        }
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

      if (-not $IsWindows) {
        $env:CC = Get-UnixHostToolchainCommand -Candidates @("cc", "clang", "gcc") -XcrunFind "clang"
        $env:CXX = Get-UnixHostToolchainCommand -Candidates @("c++", "clang++", "g++") -XcrunFind "clang++"
        $env:AR = Get-UnixHostToolchainCommand -Candidates @("ar", "llvm-ar") -XcrunFind "ar"
        $env:RANLIB = Get-UnixHostToolchainCommand -Candidates @("ranlib", "llvm-ranlib") -XcrunFind "ranlib"
      }
      if ($IsMacOS) {
        if (Test-Path -LiteralPath $llvmLibDir) {
          Get-ChildItem -LiteralPath $llvmLibDir -Force |
          Where-Object {
            -not $_.PSIsContainer -and (
              $_.Name -like "libc++*.dylib*" -or
              $_.Name -like "libc++abi*.dylib*" -or
              $_.Name -like "libunwind*.dylib*"
            )
          } |
          Remove-Item -Force
        }

        # Use the host C++ driver for the helper binary itself so it links against
        # the platform runtime, while still routing through ld64.lld via RUSTFLAGS.
        # Using the packaged LLVM clang here can bake in @rpath/libc++.1.dylib
        # without an LC_RPATH on the helper binary.
        Set-Item -Path "Env:$linkerEnvName" -Value $env:CXX

        $macOsRustFlags = @(
          "-Clink-arg=-fuse-ld=lld",
          "-Clink-arg=-lc++abi"
        )
        if ($hadRustFlags -and $previousRustFlags) {
          $env:RUSTFLAGS = $previousRustFlags
          foreach ($flag in $macOsRustFlags) {
            if ($env:RUSTFLAGS -notmatch [regex]::Escape($flag)) {
              $env:RUSTFLAGS = "$($env:RUSTFLAGS) $flag"
            }
          }
        }
        else {
          $env:RUSTFLAGS = ($macOsRustFlags -join " ")
        }
      }
    }

    if ($HelperCargoFeatures) {
      cargo build -p $HelperPackage --release --features $HelperCargoFeatures
    }
    else {
      cargo build -p $HelperPackage --release
    }
    if ($LASTEXITCODE -ne 0) {
      throw "Failed to build $HelperPackage"
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

    if ($hadCc) {
      $env:CC = $previousCc
    }
    else {
      Remove-Item Env:CC -ErrorAction SilentlyContinue
    }

    if ($hadCxx) {
      $env:CXX = $previousCxx
    }
    else {
      Remove-Item Env:CXX -ErrorAction SilentlyContinue
    }

    if ($hadAr) {
      $env:AR = $previousAr
    }
    else {
      Remove-Item Env:AR -ErrorAction SilentlyContinue
    }

    if ($hadRanlib) {
      $env:RANLIB = $previousRanlib
    }
    else {
      Remove-Item Env:RANLIB -ErrorAction SilentlyContinue
    }

    if ($hadMacOsLinker) {
      Set-Item -Path "Env:$linkerEnvName" -Value $previousMacOsLinker
    }
    else {
      Remove-Item "Env:$linkerEnvName" -ErrorAction SilentlyContinue
    }

    if ($hadRustFlags) {
      $env:RUSTFLAGS = $previousRustFlags
    }
    else {
      Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
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

$hasUpstreamArchive = ($UpstreamArchivePath -ne "") -or ($UpstreamArchiveUrl -ne "")
if (($Kind -eq "llvm") -and (-not $hasUpstreamArchive)) {
  throw "Either -UpstreamArchivePath or -UpstreamArchiveUrl is required for LLVM toolchain packaging"
}

if (-not $ToolchainVersion) {
  $ToolchainVersion = Get-HelperVersion -Kind $Kind
}
if (-not $SupportedFidanVersions) {
  $SupportedFidanVersions = "^$(Get-WorkspaceVersion)"
}
if ($BackendProtocolVersion -le 0) {
  if ($Kind -eq "llvm") {
    $BackendProtocolVersion = Get-LlvmBackendProtocolVersion
  }
  elseif ($Kind -eq "ai-analysis") {
    $BackendProtocolVersion = Get-AiAnalysisBackendProtocolVersion
  }
  else {
    $BackendProtocolVersion = 1
  }
}
if ($hasUpstreamArchive -and (-not $UpstreamArchiveSha256)) {
  throw "-UpstreamArchiveSha256 is required when packaging a toolchain from an upstream archive"
}

$hostTriple = Get-HostTriple
$helperPackage = Get-HelperPackageName -Kind $Kind
$helperBinary = Get-HelperBinaryName -Kind $Kind
$execCommands = @(Get-ExecCommandRegistrations -Kind $Kind)

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
elseif ($UpstreamArchiveUrl) {
  Split-Path -Leaf ([Uri]$UpstreamArchiveUrl).AbsolutePath
}
if ($hasUpstreamArchive -and (-not $upstreamName)) {
  $upstreamName = "$Kind-upstream.tar.gz"
}
$localArchivePath = if ($hasUpstreamArchive) {
  Join-Path $inputDir $upstreamName
}
else {
  ""
}

try {
  $helperDir = Join-Path $stageDir "helper"
  New-Item -ItemType Directory -Force -Path $helperDir | Out-Null

  $payloadRoot = $null
  if ($hasUpstreamArchive) {
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

    $payloadRoot = Join-Path $stageDir $Kind
    Move-ArchiveRootContents -ExtractRoot $extractDir -Destination $payloadRoot
    Copy-UpstreamLegalFiles -SourceRoot $payloadRoot -StageDir $stageDir -Kind $Kind
  }

  if (-not $SkipBuild) {
    Invoke-HelperBuild `
      -Kind $Kind `
      -HelperPackage $helperPackage `
      -HelperBinary $helperBinary `
      -LlvmRoot $payloadRoot `
      -HelperCargoFeatures $HelperCargoFeatures `
      -LlvmSysPrefixEnvVar $LlvmSysPrefixEnvVar `
      -HelperAdditionalLibPaths $HelperAdditionalLibPaths
  }

  $helperPath = Join-Path "target/release" $helperBinary
  if (-not (Test-Path -LiteralPath $helperPath)) {
    throw "Expected helper binary at '$helperPath'"
  }
  Copy-Item -LiteralPath $helperPath -Destination (Join-Path $helperDir $helperBinary)

  if (($Kind -eq "llvm") -and $payloadRoot) {
    Remove-LlvmPayload -LlvmRoot $payloadRoot
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
    exec_commands            = $execCommands
  }
  $metadata | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $stageDir "metadata.json") -Encoding UTF8

  tar -czf $archivePath -C $stageDir .
  if ($LASTEXITCODE -ne 0) {
    throw "Failed to create archive '$archivePath'"
  }
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
      exec_commands            = $execCommands
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
