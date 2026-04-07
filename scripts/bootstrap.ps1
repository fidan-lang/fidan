param(
  [string]$Version = "latest",
  [string]$ManifestUrl = "",
  [string]$InstallRoot = "",
  [switch]$SkipPathUpdate,
  [switch]$AllowExistingInstall,
  [switch]$Help
)

$ErrorActionPreference = "Stop"
$BannerUrl = "https://raw.githubusercontent.com/fidan-lang/fidan/main/assets/github/banner.txt"
$script:BannerTextCache = $null

function Get-BannerText {
  if ($null -ne $script:BannerTextCache) {
    return $script:BannerTextCache
  }

  $localBannerPath = $null
  if ($PSScriptRoot) {
    $localBannerPath = Join-Path (Split-Path -Parent $PSScriptRoot) "assets/github/banner.txt"
  }

  if ($localBannerPath -and (Test-Path -LiteralPath $localBannerPath)) {
    $script:BannerTextCache = Get-Content -LiteralPath $localBannerPath -Raw
    return $script:BannerTextCache
  }

  try {
    $script:BannerTextCache = (Invoke-WebRequest -Uri $BannerUrl).Content
    return $script:BannerTextCache
  }
  catch {
    $script:BannerTextCache = "FIDAN`n"
    return $script:BannerTextCache
  }
}

function Show-Banner {
  $bannerText = Get-BannerText
  $trimmedBanner = $bannerText.TrimEnd("`r", "`n")
  Write-Host ""
  if ($trimmedBanner) {
    foreach ($line in ($trimmedBanner -split "`r?`n")) {
      Write-Host $line
    }
  }
  else {
    Write-Host "FIDAN"
  }
  Write-Host ""
}

function Show-Usage {
  Write-Host "Fidan bootstrap installer"
  Write-Host ""
  Write-Host "Options:"
  Write-Host "  -Version <version>             Install a specific released version (default: latest)"
  Write-Host "  -ManifestUrl <url>             Override the distribution manifest URL"
  Write-Host "  -InstallRoot <path>            Override the self-managed install root"
  Write-Host "  -SkipPathUpdate                Do not modify the user PATH"
  Write-Host "  -AllowExistingInstall          Permit bootstrapping into an existing Fidan install root"
  Write-Host "  -Help                          Show this help text"
  Write-Host ""
  Write-Host "Bootstrap is intended for first install. If Fidan is already installed,"
  Write-Host "prefer 'fidan self install' and 'fidan self use'."
}

function Test-ExistingInstall {
  param([string]$InstallRootPath)

  $versionsDir = Join-Path $InstallRootPath "versions"
  $metadataDir = Join-Path $InstallRootPath "metadata"
  $currentDir = Join-Path $InstallRootPath "current"

  if (Test-Path -LiteralPath $currentDir) {
    return $true
  }

  if (Test-Path -LiteralPath (Join-Path $metadataDir "installs.json")) {
    return $true
  }

  if (Test-Path -LiteralPath (Join-Path $metadataDir "active-version.json")) {
    return $true
  }

  if (Test-Path -LiteralPath $versionsDir) {
    $installedVersions = @(Get-ChildItem -LiteralPath $versionsDir -Directory -ErrorAction SilentlyContinue)
    if ($installedVersions.Count -gt 0) {
      return $true
    }
  }

  return $false
}

Show-Banner

if ($Help) {
  Show-Usage
  if ($PSCommandPath) {
    exit 0
  }
  return
}

function Resolve-InstallRoot {
  param([string]$Explicit)
  if ($Explicit) {
    return $Explicit
  }

  if ($IsWindows) {
    $local = [Environment]::GetFolderPath("LocalApplicationData")
    if (-not $local) {
      throw "LOCALAPPDATA is not available"
    }
    return (Join-Path $local "Programs\Fidan")
  }

  if ($IsMacOS) {
    return (Join-Path $HOME "Applications/Fidan")
  }

  if ($env:XDG_DATA_HOME) {
    return (Join-Path $env:XDG_DATA_HOME "fidan/installs")
  }

  return (Join-Path $HOME ".local/share/fidan/installs")
}

function Resolve-HostTriple {
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

function Get-ManifestUrl {
  param([string]$Explicit)
  if ($Explicit) {
    return $Explicit
  }
  if ($env:FIDAN_DIST_MANIFEST) {
    return $env:FIDAN_DIST_MANIFEST
  }
  return "https://releases.fidan.dev/manifest.json"
}

function Read-TextResource {
  param([string]$Url)
  if ($Url.StartsWith("file://")) {
    $path = $Url.Substring(7)
    return Get-Content -LiteralPath $path -Raw
  }
  return (Invoke-WebRequest -Uri $Url).Content
}

function Save-ResourceToFile {
  param(
    [string]$Url,
    [string]$Destination
  )
  if ($Url.StartsWith("file://")) {
    Copy-Item -LiteralPath $Url.Substring(7) -Destination $Destination
    return
  }
  Invoke-WebRequest -Uri $Url -OutFile $Destination
}

function ConvertTo-CanonicalPath {
  param([string]$PathLike)
  return [System.IO.Path]::GetFullPath($PathLike)
}

function Get-Release {
  param(
    [object]$Manifest,
    [string]$RequestedVersion,
    [string]$HostTriple
  )

  $releases = @($Manifest.fidan_versions | Where-Object { $_.host_triple -eq $HostTriple })
  if (-not $releases -or $releases.Count -eq 0) {
    throw "No Fidan releases are available for host '$HostTriple' in the manifest"
  }

  $sorted = $releases | Sort-Object -Property `
  @{ Expression   = {
      $match = [regex]::Match($_.version, '^\d+')
      if ($match.Success) { [int]$match.Value } else { 0 }
    }; Descending = $true
  }, `
  @{ Expression   = {
      $segments = $_.version -split '\.'
      if ($segments.Count -gt 1) {
        $match = [regex]::Match($segments[1], '^\d+')
        if ($match.Success) { [int]$match.Value } else { 0 }
      }
      else {
        0
      }
    }; Descending = $true
  }, `
  @{ Expression   = {
      $segments = $_.version -split '\.'
      if ($segments.Count -gt 2) {
        $match = [regex]::Match($segments[2], '^\d+')
        if ($match.Success) { [int]$match.Value } else { 0 }
      }
      else {
        0
      }
    }; Descending = $true
  }, `
  @{ Expression   = {
      $segments = $_.version -split '\.'
      if ($segments.Count -gt 3) {
        $match = [regex]::Match($segments[3], '^\d+')
        if ($match.Success) { [int]$match.Value } else { 0 }
      }
      else {
        0
      }
    }; Descending = $true
  }, `
  @{ Expression   = {
      if ($_.version -match '[-+]') { 0 } else { 1 }
    }; Descending = $true
  }, `
  @{ Expression = { $_.version }; Descending = $true }

  if ($RequestedVersion -and $RequestedVersion -ne "latest") {
    $match = $sorted | Where-Object { $_.version -eq $RequestedVersion } | Select-Object -First 1
    if (-not $match) {
      throw "Fidan version '$RequestedVersion' is not available for '$HostTriple'"
    }
    return $match
  }

  return $sorted[0]
}

function Get-Sha256 {
  param([string]$Path)
  return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Update-Metadata {
  param(
    [string]$MetadataDir,
    [string]$VersionString,
    [bool]$MakeActive
  )

  New-Item -ItemType Directory -Force -Path $MetadataDir | Out-Null
  $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()

  $installsPath = Join-Path $MetadataDir "installs.json"
  $activePath = Join-Path $MetadataDir "active-version.json"

  $installs = if (Test-Path -LiteralPath $installsPath) {
    Get-Content -LiteralPath $installsPath -Raw | ConvertFrom-Json
  }
  else {
    [pscustomobject]@{
      schema_version  = 1
      installs        = @()
      updated_at_secs = $now
    }
  }

  $existing = @($installs.installs | Where-Object { $_.version -eq $VersionString })
  if ($existing.Count -eq 0) {
    $installs.installs += [pscustomobject]@{
      version           = $VersionString
      installed_at_secs = $now
    }
  }
  $installs.updated_at_secs = $now

  if ($MakeActive -or -not (Test-Path -LiteralPath $activePath)) {
    $active = @{
      schema_version  = 1
      active_version  = $VersionString
      updated_at_secs = $now
    }
    $active | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $activePath -Encoding UTF8
  }

  $installs | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $installsPath -Encoding UTF8
}

function Set-CurrentPointer {
  param(
    [string]$InstallRootPath,
    [string]$VersionString
  )

  $current = Join-Path $InstallRootPath "current"
  $target = Join-Path (Join-Path $InstallRootPath "versions") $VersionString

  if (Test-Path -LiteralPath $current) {
    Remove-Item -LiteralPath $current -Force -Recurse
  }

  New-Item -ItemType Junction -Path $current -Target $target | Out-Null
}

function Add-PathEntry {
  param([string]$CurrentDir)

  if ($SkipPathUpdate) {
    return
  }

  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  $entries = @()
  if ($userPath) {
    $entries = $userPath -split ";" | Where-Object { $_.Trim() -ne "" }
  }
  if ($entries -contains $CurrentDir) {
    return
  }

  $newPath = if ($userPath -and $userPath.Trim()) {
    "$userPath;$CurrentDir"
  }
  else {
    $CurrentDir
  }
  [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
  Write-Host "Added '$CurrentDir' to the user PATH. Open a new shell to pick it up."
}

try {
  $manifestUrl = Get-ManifestUrl -Explicit $ManifestUrl
  $installRootResolved = ConvertTo-CanonicalPath (Resolve-InstallRoot -Explicit $InstallRoot)
  $hostTriple = Resolve-HostTriple

  if ((-not $AllowExistingInstall) -and (Test-ExistingInstall -InstallRootPath $installRootResolved)) {
    throw "An existing self-managed Fidan installation was detected at '$installRootResolved'.`nUse 'fidan self install' or re-run bootstrap with -AllowExistingInstall if you really want to install into the same root."
  }

  Write-Host "Fetching manifest from $manifestUrl"
  $manifestText = Read-TextResource -Url $manifestUrl
  $manifest = $manifestText | ConvertFrom-Json
  if (-not $manifest.schema_version) {
    throw "Distribution manifest '$manifestUrl' has invalid schema_version 0"
  }

  $release = Get-Release -Manifest $manifest -RequestedVersion $Version -HostTriple $hostTriple
  $releaseVersion = $release.version
  $archiveUrl = $release.url
  $expectedSha = $release.sha256.ToLowerInvariant()
  $binaryRelPath = if ($release.binary_relpath) { $release.binary_relpath } elseif ($IsWindows) { "fidan.exe" } else { "fidan" }

  $versionsDir = Join-Path $installRootResolved "versions"
  $metadataDir = Join-Path $installRootResolved "metadata"
  $finalDir = Join-Path $versionsDir $releaseVersion
  $existingVersions = @()
  if (Test-Path -LiteralPath $versionsDir) {
    $existingVersions = @(Get-ChildItem -LiteralPath $versionsDir -Directory -ErrorAction SilentlyContinue)
  }
  $firstInstall = $existingVersions.Count -eq 0
  if (Test-Path -LiteralPath $finalDir) {
    throw "Fidan version '$releaseVersion' is already installed at '$finalDir'"
  }

  New-Item -ItemType Directory -Force -Path $versionsDir | Out-Null
  New-Item -ItemType Directory -Force -Path $metadataDir | Out-Null

  $tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("fidan-bootstrap-" + [Guid]::NewGuid().ToString("N"))
  $archivePath = Join-Path $tempRoot "fidan.tar.gz"
  $extractDir = Join-Path $tempRoot "extract"
  New-Item -ItemType Directory -Force -Path $extractDir | Out-Null

  try {
    Write-Host "Downloading Fidan $releaseVersion for $hostTriple"
    Save-ResourceToFile -Url $archiveUrl -Destination $archivePath

    $actualSha = Get-Sha256 -Path $archivePath
    if ($actualSha -ne $expectedSha) {
      throw "SHA-256 mismatch for '$archiveUrl' (expected $expectedSha, got $actualSha)"
    }

    tar -xzf $archivePath -C $extractDir

    $candidateRoot = $extractDir
    $candidateBinary = Join-Path $candidateRoot $binaryRelPath
    if (-not (Test-Path -LiteralPath $candidateBinary)) {
      $children = Get-ChildItem -LiteralPath $extractDir -Directory
      if ($children.Count -ne 1) {
        throw "Downloaded archive does not contain '$binaryRelPath' at the root or inside a single top-level directory"
      }
      $candidateRoot = $children[0].FullName
      $candidateBinary = Join-Path $candidateRoot $binaryRelPath
      if (-not (Test-Path -LiteralPath $candidateBinary)) {
        throw "Downloaded archive does not contain the expected file '$binaryRelPath'"
      }
    }

    Move-Item -LiteralPath $candidateRoot -Destination $finalDir
    Update-Metadata -MetadataDir $metadataDir -VersionString $releaseVersion -MakeActive:$firstInstall
    if ($firstInstall) {
      Set-CurrentPointer -InstallRootPath $installRootResolved -VersionString $releaseVersion
      Add-PathEntry -CurrentDir (Join-Path $installRootResolved "current")
      Write-Host "Installed Fidan $releaseVersion and made it active"
    }
    else {
      Write-Host "Installed Fidan $releaseVersion"
      Write-Host "Run 'fidan self use $releaseVersion' to activate it"
    }
    Write-Host "Install root: $installRootResolved"
  }
  finally {
    if (Test-Path -LiteralPath $tempRoot) {
      Remove-Item -LiteralPath $tempRoot -Force -Recurse
    }
  }
}
catch {
  $message = if ($_.Exception -and $_.Exception.Message) { $_.Exception.Message } else { $_.ToString() }
  Write-Host ""
  Write-Host "[X] Installation failed:" -ForegroundColor Red
  Write-Host $message -ForegroundColor Red
  if ($PSCommandPath) {
    exit 1
  }
  $global:LASTEXITCODE = 1
  return
}
