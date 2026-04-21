param(
  [Parameter(Mandatory = $true)]
  [ValidateSet("build-installer", "prepare-winget", "submit-winget")]
  [string]$Mode,

  [Parameter(Mandatory = $true)]
  [string]$Version,

  [string]$OutputRoot = "dist/release",
  [string]$HostTriple = "x86_64-pc-windows-msvc",
  [string]$BootstrapScriptUrl = "https://fidan.dev/install.ps1",
  [string]$WingetManifestRoot = "config/winget/manifest",
  [string]$BinaryPath = "target/release/fidan.exe"
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
  Add-DirectoryToPath -Path "C:\ProgramData\chocolatey\bin"
  Add-DirectoryToPath -Path "C:\ProgramData\chocolatey\lib\upx\tools"

  Install-CommandIfMissing -Name "iscc.exe" -InstallAction {
    $installer = Join-Path $env:TEMP "innosetup-installer.exe"
    Invoke-WebRequest -Uri "https://jrsoftware.org/download.php/is.exe" -OutFile $installer
    Start-Process -FilePath $installer -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait
    Add-DirectoryToPath -Path "C:\Program Files (x86)\Inno Setup 6"
  }

  Install-CommandIfMissing -Name "upx.exe" -InstallAction {
    if (-not (Get-Command choco.exe -ErrorAction SilentlyContinue)) {
      throw "UPX is mandatory but Chocolatey is not available to install it automatically."
    }

    choco install upx --no-progress --yes
    Add-DirectoryToPath -Path "C:\ProgramData\chocolatey\bin"
    Add-DirectoryToPath -Path "C:\ProgramData\chocolatey\lib\upx\tools"
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

function Compress-BinaryWithUpx {
  param([string]$ResolvedBinaryPath)

  if (-not (Test-Path -LiteralPath $ResolvedBinaryPath)) {
    throw "Expected binary for UPX compression at '$ResolvedBinaryPath'"
  }

  $upxOutput = & upx --best --lzma $ResolvedBinaryPath 2>&1
  $upxExitCode = $LASTEXITCODE
  $upxText = ($upxOutput | Out-String).Trim()

  if ($upxExitCode -eq 0) {
    if (-not [string]::IsNullOrWhiteSpace($upxText)) {
      Write-Host $upxText
    }
    return
  }

  if ($upxText -match "AlreadyPackedException|already packed by UPX") {
    Write-Host "UPX skipped: '$ResolvedBinaryPath' is already packed."
    return
  }

  throw "UPX compression failed for '$ResolvedBinaryPath'`n$upxText"
}

function Build-WindowsInstaller {
  param(
    [string]$ResolvedVersion,
    [string]$ResolvedOutputRoot,
    [string]$ResolvedHostTriple,
    [string]$ResolvedBootstrapScriptUrl,
    [string]$ResolvedBinaryPath
  )

  Install-WindowsInstallerDependencies
  Compress-BinaryWithUpx -ResolvedBinaryPath $ResolvedBinaryPath

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

  $wingetDir = Join-Path $ResolvedOutputRoot "winget"
  New-Item -ItemType Directory -Force -Path $wingetDir | Out-Null

  $repo = if ($env:GITHUB_REPOSITORY) { $env:GITHUB_REPOSITORY } else { "fidan-lang/fidan" }
  $releaseTag = if ($env:GITHUB_REF_NAME -and $env:GITHUB_REF_NAME.StartsWith("v")) { $env:GITHUB_REF_NAME } else { "v$ResolvedVersion" }
  $installerSha256 = (Get-FileHash -LiteralPath $payloadInstallerPath -Algorithm SHA256).Hash
  $installerUrl = "https://github.com/$repo/releases/download/$releaseTag/$installerName"
  $vcRedistMetadata = Get-WindowsVcRedistReleaseMetadata -HostTriple $ResolvedHostTriple

  $wingetInfo = [ordered]@{
    version       = $ResolvedVersion
    release_tag   = $releaseTag
    installer     = $installerName
    installer_url = $installerUrl
    sha256        = $installerSha256
    vc_redist     = [ordered]@{
      package_identifier = $vcRedistMetadata.PackageIdentifier
      minimum_version    = $vcRedistMetadata.MinimumVersion
    }
  }

  $wingetInfoPath = Join-Path $wingetDir "windows-installer.json"
  $wingetInfo | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $wingetInfoPath -Encoding UTF8

  Write-Host "Built Windows bootstrap installer: $payloadInstallerPath"
  Write-Host "Winget metadata: $wingetInfoPath"
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
  $releaseTag = if ($env:GITHUB_REF_NAME -and $env:GITHUB_REF_NAME.StartsWith("v")) { $env:GITHUB_REF_NAME } else { "v$ResolvedVersion" }
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

  $winget = (Get-Command winget.exe -ErrorAction SilentlyContinue).Source
  if (-not $winget) {
    throw "winget.exe not found on PATH"
  }

  & $winget validate --manifest $manifestDir --verbose-logs
  if ($LASTEXITCODE -ne 0) {
    throw "winget validate failed"
  }

  if ($SkipSubmit) {
    Write-Host "Prepared and validated winget manifests at '$manifestDir' (submission skipped)."
    return
  }

  if (-not $env:WINGET_GITHUB_TOKEN) {
    throw "WINGET_GITHUB_TOKEN is required for winget submission."
  }

  $wingetCreateExe = Join-Path (Resolve-Path ".") "wingetcreate.exe"
  Invoke-WebRequest -Uri "https://github.com/microsoft/winget-create/releases/latest/download/wingetcreate.exe" -OutFile $wingetCreateExe

  & $wingetCreateExe submit $manifestDir --token $env:WINGET_GITHUB_TOKEN
  if ($LASTEXITCODE -ne 0) {
    throw "wingetcreate submit failed"
  }

  Write-Host "Submitted winget manifests from '$manifestDir'"
}

switch ($Mode) {
  "build-installer" {
    Build-WindowsInstaller -ResolvedVersion $Version -ResolvedOutputRoot $OutputRoot -ResolvedHostTriple $HostTriple -ResolvedBootstrapScriptUrl $BootstrapScriptUrl -ResolvedBinaryPath $BinaryPath
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
