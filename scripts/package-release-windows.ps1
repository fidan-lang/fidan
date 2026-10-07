param(
  [Parameter(Mandatory = $true)]
  [ValidateSet("build-installer", "stage-winget-release", "prepare-winget", "submit-winget")]
  [string]$Mode,

  [Parameter(Mandatory = $true)]
  [string]$Version,

  [string]$OutputRoot = "dist/release",
  [string]$HostTriple = "x86_64-pc-windows-msvc",
  [string]$BootstrapScriptUrl = "https://fidan.dev/install.ps1",
  [string]$WingetManifestRoot = "config/winget/manifest"
)

$ErrorActionPreference = "Stop"

$hostPlatformHelperPath = Join-Path $PSScriptRoot "shared/host-platform.ps1"
if (-not (Test-Path -LiteralPath $hostPlatformHelperPath)) {
  throw "Missing host platform helper: '$hostPlatformHelperPath'"
}

. $hostPlatformHelperPath

$windowsVcRedistHelperPath = Join-Path $PSScriptRoot "shared/windows-vc-redist.ps1"
if (-not (Test-Path -LiteralPath $windowsVcRedistHelperPath)) {
  throw "Missing Windows VC++ redistributable helper: '$windowsVcRedistHelperPath'"
}

. $windowsVcRedistHelperPath

$isWindowsHost = [bool](Get-HostPlatformFlags).IsWindowsHost

if (-not $isWindowsHost) {
  throw "scripts/package-release-windows.ps1 can only run on Windows."
}

function Add-DirectoryToPath {
  param([string]$Path)

  if (-not (Test-Path -LiteralPath $Path)) {
    return
  }

  $existing = $env:PATH -split ';' | Where-Object { $_ -eq $Path }
  if (-not $existing) {
    $env:PATH = "$Path;$env:PATH"
  }
}

function Install-CommandIfMissing {
  param(
    [string]$Name,
    [scriptblock]$InstallAction
  )

  if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
    & $InstallAction
  }

  if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
    throw "Required command '$Name' is not available on PATH"
  }
}

function Assert-SignToolAvailable {
  if (Get-Command signtool.exe -ErrorAction SilentlyContinue) {
    return
  }

  $signtools = Get-ChildItem -Path "C:\Program Files (x86)\Windows Kits\10\bin" -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
  Where-Object { $_.FullName -match '\\x64\\' }

  $signtool = $signtools | Sort-Object {
    if ($_.FullName -match '\\bin\\(?<ver>\d+\.\d+\.\d+\.\d+)\\x64\\') {
      [version]$matches['ver']
    }
    else {
      [version]'0.0.0.0'
    }
  } -Descending | Select-Object -First 1

  if ($signtool) {
    Add-DirectoryToPath -Path $signtool.DirectoryName
  }

  if (-not (Get-Command signtool.exe -ErrorAction SilentlyContinue)) {
    throw "signtool.exe (x64) not found."
  }
}

function Install-WindowsInstallerDependencies {
  Add-DirectoryToPath -Path "C:\Program Files (x86)\Inno Setup 6"

  Install-CommandIfMissing -Name "iscc.exe" -InstallAction {
    $installer = Join-Path $env:TEMP "innosetup-installer.exe"
    Invoke-WebRequest -Uri "https://jrsoftware.org/download.php/is.exe" -OutFile $installer
    Start-Process -FilePath $installer -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait
    Add-DirectoryToPath -Path "C:\Program Files (x86)\Inno Setup 6"
  }

  Assert-SignToolAvailable
}

function Get-RequiredEnvironmentVariable {
  param([string]$Name)

  $value = [System.Environment]::GetEnvironmentVariable($Name)
  if ([string]::IsNullOrWhiteSpace($value)) {
    throw "Required environment variable '$Name' is missing or empty."
  }

  return $value
}

function New-TemporarySigningMaterial {
  $pfxBase64 = Get-RequiredEnvironmentVariable -Name "CERT_PFX_BASE64"

  $tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("fidan-signing-" + [Guid]::NewGuid().ToString("N"))
  New-Item -ItemType Directory -Force -Path $tempDir | Out-Null

  $pfxPath = Join-Path $tempDir "codesign.pfx"
  try {
    $pfxBytes = [Convert]::FromBase64String($pfxBase64)
  }
  catch {
    throw "CERT_PFX_BASE64 is not valid base64."
  }

  [System.IO.File]::WriteAllBytes($pfxPath, $pfxBytes)

  return [PSCustomObject]@{
    TempDir = $tempDir
    PfxPath = $pfxPath
  }
}

function Remove-TemporarySigningMaterial {
  param([string]$TempDir)

  if (-not $TempDir) {
    return
  }

  if (Test-Path -LiteralPath $TempDir) {
    Remove-Item -LiteralPath $TempDir -Force -Recurse -ErrorAction SilentlyContinue
  }
}

function Get-IsccSignToolOverrideArgument {
  param([string]$PfxPath)

  $signtoolPath = (Get-Command signtool.exe -ErrorAction SilentlyContinue).Source
  if (-not $signtoolPath) {
    throw "signtool.exe not found on PATH"
  }

  $certPassword = Get-RequiredEnvironmentVariable -Name "CERT_PASSWORD"
  $certDescription = [System.Environment]::GetEnvironmentVariable("CERT_DESCRIPTION")
  $certWebsite = [System.Environment]::GetEnvironmentVariable("CERT_WEBSITE")
  $timestampUrl = [System.Environment]::GetEnvironmentVariable("CERT_TIMESTAMP_URL")

  if ([string]::IsNullOrWhiteSpace($certDescription)) {
    $certDescription = "Fidan"
  }
  if ([string]::IsNullOrWhiteSpace($certWebsite)) {
    $certWebsite = "https://fidan.dev"
  }
  if ([string]::IsNullOrWhiteSpace($timestampUrl)) {
    $timestampUrl = "http://timestamp.digicert.com"
  }

  $quotedSigntoolPath = '"' + $signtoolPath + '"'
  $quotedPfxPath = '"' + $PfxPath + '"'
  $quotedCertPassword = '"' + $certPassword + '"'
  $quotedCertDescription = '"' + $certDescription + '"'
  $quotedCertWebsite = '"' + $certWebsite + '"'
  $quotedTimestampUrl = '"' + $timestampUrl + '"'

  $signCommand = @(
    $quotedSigntoolPath,
    "sign",
    "/f $quotedPfxPath",
    "/p $quotedCertPassword",
    "/d $quotedCertDescription",
    "/du $quotedCertWebsite",
    "/fd SHA256",
    "/tr $quotedTimestampUrl",
    "/td SHA256",
    "/a `$f"
  ) -join " "

  # ISCC parsing is sensitive here: quote the entire /SCertForge switch token
  # and double embedded quotes so it receives one logical sign-command value.
  $escapedSignCommand = $signCommand.Replace('"', '""')
  return '"/SCertForge=""' + $escapedSignCommand + '"""'
}

function Resolve-BootstrapScriptMetadata {
  param([string]$Url)

  $tempFile = Join-Path ([System.IO.Path]::GetTempPath()) ("fidan-install-" + [Guid]::NewGuid().ToString("N") + ".ps1")

  try {
    Invoke-WebRequest -Uri $Url -OutFile $tempFile
    return [PSCustomObject]@{
      Size   = (Get-Item -LiteralPath $tempFile).Length
      Sha256 = (Get-FileHash -LiteralPath $tempFile -Algorithm SHA256).Hash.ToLowerInvariant()
    }
  }
  finally {
    if (Test-Path -LiteralPath $tempFile) {
      Remove-Item -LiteralPath $tempFile -Force -ErrorAction SilentlyContinue
    }
  }
}

function Write-WindowsInstallerWingetMetadata {
  param(
    [string]$ResolvedVersion,
    [string]$ResolvedOutputRoot,
    [string]$InstallerPath,
    [object]$vcppRedistManifest
  )

  $wingetDir = Join-Path $ResolvedOutputRoot "winget"
  New-Item -ItemType Directory -Force -Path $wingetDir | Out-Null
  $repo = if ($env:GITHUB_REPOSITORY) { $env:GITHUB_REPOSITORY } else { "fidan-lang/fidan" }
  $installerName = Split-Path -Leaf $InstallerPath
  $wingetInfo = [ordered]@{
    version       = $ResolvedVersion
    release_tag   = "v$ResolvedVersion"
    installer     = $installerName
    installer_url = "https://github.com/$repo/releases/download/v$ResolvedVersion/$installerName"
    sha256        = (Get-FileHash -LiteralPath $InstallerPath -Algorithm SHA256).Hash
    vc_redist     = [ordered]@{
      package_identifier = $vcppRedistManifest.PackageIdentifier
      minimum_version    = $vcppRedistManifest.MinimumVersion
    }
  }
  $wingetInfoPath = Join-Path $wingetDir "windows-installer.json"
  $wingetInfo | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $wingetInfoPath -Encoding UTF8
  Write-Host "Winget metadata: $wingetInfoPath"
}

function Build-WindowsInstaller {
  param(
    [string]$ResolvedVersion,
    [string]$ResolvedOutputRoot,
    [string]$ResolvedHostTriple,
    [string]$ResolvedBootstrapScriptUrl
  )

  Install-WindowsInstallerDependencies

  $metadata = Resolve-BootstrapScriptMetadata -Url $ResolvedBootstrapScriptUrl

  $env:VERSION = $ResolvedVersion
  $env:ROOT_DIR = (Resolve-Path ".").Path
  $env:BOOTSTRAP_SCRIPT_SIZE = [string]$metadata.Size
  $env:BOOTSTRAP_SCRIPT_SHA256 = $metadata.Sha256

  $signingMaterial = $null
  try {
    $signingMaterial = New-TemporarySigningMaterial
    $isccSignOverrideArgument = Get-IsccSignToolOverrideArgument -PfxPath $signingMaterial.PfxPath

    $isccProcess = Start-Process -FilePath "iscc.exe" -ArgumentList @(
      $isccSignOverrideArgument,
      ".\config\innosetup\installer.iss"
    ) -NoNewWindow -Wait -PassThru

    if ($isccProcess.ExitCode -ne 0) {
      throw "ISCC.exe failed with exit code $($isccProcess.ExitCode)"
    }
  }
  finally {
    Remove-TemporarySigningMaterial -TempDir $signingMaterial?.TempDir
  }

  $installerName = "fidan_windows_bootstrap_v$ResolvedVersion.exe"
  $builtInstallerPath = Join-Path "dist/innosetup/installers" $installerName
  if (-not (Test-Path -LiteralPath $builtInstallerPath)) {
    throw "Installer artifact not found at '$builtInstallerPath'"
  }

  $payloadInstallerDir = Join-Path (Join-Path $ResolvedOutputRoot "payload/fidan/$ResolvedVersion") $ResolvedHostTriple
  New-Item -ItemType Directory -Force -Path $payloadInstallerDir | Out-Null

  $payloadInstallerPath = Join-Path $payloadInstallerDir $installerName
  Copy-Item -LiteralPath $builtInstallerPath -Destination $payloadInstallerPath -Force

  $vcRedistMetadata = Get-WindowsVcRedistReleaseMetadata -HostTriple $ResolvedHostTriple
  Write-WindowsInstallerWingetMetadata -ResolvedVersion $ResolvedVersion -ResolvedOutputRoot $ResolvedOutputRoot -InstallerPath $payloadInstallerPath -vcppRedistManifest $vcRedistMetadata

  Write-Host "Built Windows bootstrap installer: $payloadInstallerPath"
}

function Copy-PublishedWingetRelease {
  param(
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$ResolvedVersion,
    [string]$ResolvedOutputRoot,
    [string]$ResolvedWingetManifestRoot
  )

  $repo = if ($env:GITHUB_REPOSITORY) { $env:GITHUB_REPOSITORY } else { "fidan-lang/fidan" }
  $tag = "v$ResolvedVersion"
  $installerName = "fidan_windows_bootstrap_v$ResolvedVersion.exe"
  $releaseJson = & gh release view $tag --repo $repo --json tagName, isDraft, assets
  if ($LASTEXITCODE -ne 0) { throw "Failed to read published GitHub release '$tag'." }
  $release = $releaseJson | ConvertFrom-Json
  if ($release.isDraft -or $release.tagName -ne $tag) { throw "Release '$tag' is not published." }
  $assets = @($release.assets | Where-Object { $_.name -eq $installerName })
  if ($assets.Count -ne 1) { throw "Published release '$tag' must contain '$installerName'." }

  # Use the original release's VC++ requirement, not the retry runner's runtime.
  $manifest = Invoke-RestMethod -Uri "https://releases.fidan.dev/manifest.json" -TimeoutSec 60
  $entries = @($manifest.fidan_versions | Where-Object {
      $_.version -eq $ResolvedVersion -and $_.host_triple -eq "x86_64-pc-windows-msvc"
    })
  $minimumVersion = $null
  if ($entries.Count -ne 1 -or -not [version]::TryParse([string]$entries[0].vc_redist_min_version, [ref]$minimumVersion)) {
    throw "Published manifest lacks a valid Windows VC++ requirement for '$ResolvedVersion'."
  }

  $installerDir = Join-Path $ResolvedOutputRoot "payload/fidan/$ResolvedVersion/x86_64-pc-windows-msvc"
  New-Item -ItemType Directory -Force -Path $installerDir | Out-Null
  & gh release download $tag --repo $repo --pattern $installerName --dir $installerDir --clobber
  if ($LASTEXITCODE -ne 0) { throw "Failed to download published installer '$installerName'." }
  $installerPath = Join-Path $installerDir $installerName
  $hash = (Get-FileHash -LiteralPath $installerPath -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($assets[0].digest -and $assets[0].digest -ne "sha256:$hash") {
    throw "Published installer SHA256 does not match the GitHub release digest."
  }
  $vcRedistMetadata = [pscustomobject]@{
    PackageIdentifier = Get-WindowsVcRedistPackageIdentifier -Architecture x64
    MinimumVersion    = [string]$entries[0].vc_redist_min_version
  }
  Write-WindowsInstallerWingetMetadata -ResolvedVersion $ResolvedVersion -ResolvedOutputRoot $ResolvedOutputRoot -InstallerPath $installerPath -vcppRedistManifest $vcRedistMetadata

  $sourceDir = Resolve-WingetManifestDirectory -ResolvedWingetManifestRoot $ResolvedWingetManifestRoot -ResolvedVersion $ResolvedVersion -PackageIdentifier "Fidan.Fidan"
  $manifestDir = Join-Path $ResolvedOutputRoot "winget/manifests"
  New-Item -ItemType Directory -Force -Path $manifestDir | Out-Null
  Copy-Item -LiteralPath (Get-ChildItem -LiteralPath $sourceDir -Filter "Fidan.Fidan*.yaml" -File).FullName -Destination $manifestDir -Force
  Write-Host "Staged published release '$tag' for WinGet without rebuilding or uploading release assets."
}

function Resolve-WingetManifestDirectory {
  param(
    [string]$ResolvedWingetManifestRoot,
    [string]$ResolvedVersion,
    [string]$PackageIdentifier
  )

  if (-not (Test-Path -LiteralPath $ResolvedWingetManifestRoot)) {
    throw "Winget manifest directory not found: '$ResolvedWingetManifestRoot'"
  }

  $installerManifestName = "$PackageIdentifier.installer.yaml"
  $rootInstallerManifest = Join-Path $ResolvedWingetManifestRoot $installerManifestName
  if (Test-Path -LiteralPath $rootInstallerManifest) {
    return (Resolve-Path -LiteralPath $ResolvedWingetManifestRoot).Path
  }

  $candidateInstallerManifests = Get-ChildItem -Path $ResolvedWingetManifestRoot -Recurse -File -Filter $installerManifestName
  if (-not $candidateInstallerManifests) {
    throw "No '$installerManifestName' found under '$ResolvedWingetManifestRoot'"
  }

  $candidateDirs = $candidateInstallerManifests | Select-Object -ExpandProperty DirectoryName -Unique
  if ($candidateDirs.Count -eq 1) {
    return $candidateDirs[0]
  }

  $versionCandidates = @()
  foreach ($dir in $candidateDirs) {
    if ((Split-Path -Leaf $dir) -eq $ResolvedVersion) {
      $versionCandidates += $dir
      continue
    }

    $versionManifestPath = Join-Path $dir "$PackageIdentifier.yaml"
    if (Test-Path -LiteralPath $versionManifestPath) {
      $versionManifestContent = Get-Content -LiteralPath $versionManifestPath -Raw
      $versionPattern = '^PackageVersion:\s*' + [regex]::Escape($ResolvedVersion) + '\s*$'
      if ([regex]::IsMatch($versionManifestContent, $versionPattern, [System.Text.RegularExpressions.RegexOptions]::IgnoreCase -bor [System.Text.RegularExpressions.RegexOptions]::Multiline)) {
        $versionCandidates += $dir
      }
    }
  }

  if ($versionCandidates.Count -eq 1) {
    return $versionCandidates[0]
  }

  $candidateList = ($candidateDirs | ForEach-Object { " - $_" }) -join "`n"
  throw "Ambiguous winget manifest directories under '$ResolvedWingetManifestRoot'. Candidates:`n$candidateList`nPass a more specific -WingetManifestRoot path."
}

function Submit-WingetManifest {
  param(
    [string]$ResolvedVersion,
    [string]$ResolvedOutputRoot,
    [string]$ResolvedWingetManifestRoot,
    [switch]$SkipSubmit
  )

  $packageIdentifier = "Fidan.Fidan"
  $manifestDir = Resolve-WingetManifestDirectory -ResolvedWingetManifestRoot $ResolvedWingetManifestRoot -ResolvedVersion $ResolvedVersion -PackageIdentifier $packageIdentifier

  $manifestFiles = Get-ChildItem -Path $manifestDir -Filter "$packageIdentifier*.yaml" -File
  if (-not $manifestFiles) {
    $manifestFiles = Get-ChildItem -Path $manifestDir -Filter *.yaml -File
  }
  if (-not $manifestFiles) {
    throw "No .yaml files found in winget manifest directory '$manifestDir'"
  }

  $installer = Get-ChildItem -Path (Join-Path $ResolvedOutputRoot "payload/fidan/$ResolvedVersion") -Recurse -Filter "fidan_windows_bootstrap_v$ResolvedVersion.exe" -File | Select-Object -First 1
  if (-not $installer) {
    throw "Installer artifact not found under '$ResolvedOutputRoot/payload/fidan/$ResolvedVersion'"
  }

  $installerSha256 = (Get-FileHash -LiteralPath $installer.FullName -Algorithm SHA256).Hash
  $repo = if ($env:GITHUB_REPOSITORY) { $env:GITHUB_REPOSITORY } else { "fidan-lang/fidan" }
  $releaseTag = "v$ResolvedVersion"
  $installerUrl = "https://github.com/$repo/releases/download/$releaseTag/$($installer.Name)"
  $wingetInfoPath = Join-Path $ResolvedOutputRoot "winget/windows-installer.json"
  if (-not (Test-Path -LiteralPath $wingetInfoPath)) {
    throw "Winget installer metadata not found at '$wingetInfoPath'"
  }
  $wingetInfo = Get-Content -LiteralPath $wingetInfoPath -Raw | ConvertFrom-Json
  if (-not $wingetInfo.vc_redist -or -not $wingetInfo.vc_redist.package_identifier -or -not $wingetInfo.vc_redist.minimum_version) {
    throw "Winget installer metadata is missing VC++ redistributable dependency information."
  }
  $vcRedistPackageIdentifier = [string]$wingetInfo.vc_redist.package_identifier
  $vcRedistMinimumVersion = [string]$wingetInfo.vc_redist.minimum_version

  foreach ($manifest in $manifestFiles) {
    $content = Get-Content -LiteralPath $manifest.FullName -Raw
    $content = $content -replace '(?im)^(?<indent>\s*)(?<key>PackageVersion:\s*).*$' , ('${indent}${key}' + $ResolvedVersion)
    $content = $content -replace '(?im)^(?<indent>\s*)(?<key>InstallerSha256:\s*).*$' , ('${indent}${key}' + $installerSha256)
    $content = $content -replace '(?im)^(?<indent>\s*)(?<key>InstallerUrl:\s*).*$' , ('${indent}${key}' + $installerUrl)
    $content = $content -replace '(?im)^(?<indent>\s*-\s*PackageIdentifier:\s*).*$' , ('${indent}' + $vcRedistPackageIdentifier)
    $content = $content -replace '(?im)^(?<indent>\s*)(?<key>MinimumVersion:\s*).*$' , ('${indent}${key}' + $vcRedistMinimumVersion)
    Set-Content -LiteralPath $manifest.FullName -Value $content -Encoding UTF8
  }

  if ($SkipSubmit) {
    # Local preparation can validate with WinGet; CI submission needs only WingetCreate.
    $winget = (Get-Command winget.exe -ErrorAction SilentlyContinue).Source
    if (-not $winget) {
      throw "winget.exe not found on PATH (required only for local prepare-winget validation)"
    }
    & $winget validate --manifest $manifestDir --verbose-logs
    if ($LASTEXITCODE -ne 0) {
      throw "winget validate failed"
    }
    Write-Host "Prepared and validated winget manifests at '$manifestDir' (submission skipped)."
    return
  }

  if ([string]::IsNullOrWhiteSpace($env:WINGET_CREATE_GITHUB_TOKEN)) {
    throw "WINGET_CREATE_GITHUB_TOKEN is required for winget submission."
  }

  $wingetCreateExe = Join-Path (Resolve-Path ".") "wingetcreate.exe"
  Invoke-WebRequest -Uri "https://aka.ms/wingetcreate/latest" -OutFile $wingetCreateExe -TimeoutSec 60 -ErrorAction Stop

  & $wingetCreateExe submit $manifestDir --no-open
  if ($LASTEXITCODE -ne 0) {
    throw "wingetcreate submit failed with exit code $LASTEXITCODE"
  }

  Write-Host "Submitted winget manifests from '$manifestDir'"
}

switch ($Mode) {
  "build-installer" {
    Build-WindowsInstaller -ResolvedVersion $Version -ResolvedOutputRoot $OutputRoot -ResolvedHostTriple $HostTriple -ResolvedBootstrapScriptUrl $BootstrapScriptUrl
  }
  "stage-winget-release" {
    Copy-PublishedWingetRelease -ResolvedVersion $Version -ResolvedOutputRoot $OutputRoot -ResolvedWingetManifestRoot $WingetManifestRoot
  }
  "prepare-winget" {
    Submit-WingetManifest -ResolvedVersion $Version -ResolvedOutputRoot $OutputRoot -ResolvedWingetManifestRoot $WingetManifestRoot -SkipSubmit
  }
  "submit-winget" {
    Submit-WingetManifest -ResolvedVersion $Version -ResolvedOutputRoot $OutputRoot -ResolvedWingetManifestRoot $WingetManifestRoot
  }
  default {
    throw "Unsupported mode: $Mode"
  }
}
