param(
  [string]$Version = "",
  [string]$OutputRoot = "dist/release",
  [string]$HostTriple = "x86_64-pc-windows-msvc",
  [switch]$SkipBuild,
  [switch]$SkipPackaging,
  [switch]$StrictSignatureTrust
)

$ErrorActionPreference = "Stop"

if (-not $IsWindows) {
  throw "scripts/verify-release-windows.ps1 can only run on Windows."
}

function Get-WorkspaceVersion {
  $cargoToml = Get-Content "Cargo.toml" -Raw
  $match = [regex]::Match($cargoToml, '(?ms)\[workspace\.package\].*?version\s*=\s*"([^"]+)"')
  if (-not $match.Success) {
    throw "Failed to determine workspace version from Cargo.toml"
  }
  return $match.Groups[1].Value
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

if (-not $Version) {
  $Version = Get-WorkspaceVersion
}

if (-not $SkipPackaging) {
  $env:FIDAN_BUILD_INSTALLER = "1"

  for ($run = 1; $run -le 2; $run++) {
    Write-Host "Packaging run $run/2..."

    $scriptArgs = @("-Version", $Version)
    if ($SkipBuild) {
      $scriptArgs += "-SkipBuild"
    }

    & ./scripts/package-release.ps1 @scriptArgs
    if ($LASTEXITCODE -ne 0) {
      throw "package-release.ps1 failed on run $run with exit code $LASTEXITCODE"
    }
  }
}

$installerName = "fidan_windows_bootstrap_v$Version.exe"
$payloadInstallerPath = Join-Path (Join-Path $OutputRoot "payload/fidan/$Version") "$HostTriple/$installerName"
$distInstallerPath = Join-Path "dist/innosetup/installers" $installerName
$archivePath = Join-Path (Join-Path $OutputRoot "payload/fidan/$Version") "$HostTriple/fidan-$Version-$HostTriple.tar.gz"
$fragmentPath = Join-Path $OutputRoot "fragments/fidan-$Version-$HostTriple.json"
$wingetMetadataPath = Join-Path $OutputRoot "winget/windows-installer.json"

Assert-PathExists -Path $payloadInstallerPath -Description "Payload installer"
Assert-PathExists -Path $distInstallerPath -Description "Inno output installer"
Assert-PathExists -Path $archivePath -Description "Release archive"
Assert-PathExists -Path $fragmentPath -Description "Release fragment"
Assert-PathExists -Path $wingetMetadataPath -Description "Winget metadata"

$payloadSha256 = (Get-FileHash -LiteralPath $payloadInstallerPath -Algorithm SHA256).Hash
$distSha256 = (Get-FileHash -LiteralPath $distInstallerPath -Algorithm SHA256).Hash
if (-not [string]::Equals($payloadSha256, $distSha256, [System.StringComparison]::OrdinalIgnoreCase)) {
  throw "Payload installer hash does not match Inno output hash."
}

$wingetMetadata = Get-Content -LiteralPath $wingetMetadataPath -Raw | ConvertFrom-Json
if ($wingetMetadata.version -ne $Version) {
  throw "Winget metadata version mismatch. Expected '$Version', got '$($wingetMetadata.version)'."
}
if (-not [string]::Equals($wingetMetadata.sha256, $payloadSha256, [System.StringComparison]::OrdinalIgnoreCase)) {
  throw "Winget metadata sha256 does not match payload installer hash."
}

$signature = Get-AuthenticodeSignature -FilePath $payloadInstallerPath
if (-not $signature.SignerCertificate) {
  throw "Installer is not signed (no signer certificate)."
}
if ($signature.Status -eq [System.Management.Automation.SignatureStatus]::NotSigned) {
  throw "Installer is not signed (status: NotSigned)."
}

if ($StrictSignatureTrust) {
  $signtool = (Get-Command signtool.exe -ErrorAction SilentlyContinue).Source
  if (-not $signtool) {
    throw "signtool.exe not found on PATH"
  }

  & $signtool verify /pa /v $payloadInstallerPath
  if ($LASTEXITCODE -ne 0) {
    throw "signtool trust verification failed with exit code $LASTEXITCODE"
  }
}

Write-Host "Verification passed."
Write-Host "Version: $Version"
Write-Host "Installer: $payloadInstallerPath"
Write-Host "SHA256: $payloadSha256"
Write-Host "Signature status: $($signature.Status)"
Write-Host "Signer: $($signature.SignerCertificate.Subject)"
