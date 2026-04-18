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

if (-not $IsWindows) {
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

function Ensure-Command {
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

function Ensure-SignTool {
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

function Ensure-WindowsInstallerDependencies {
  Add-DirectoryToPath -Path "C:\Program Files (x86)\Inno Setup 6"
  Add-DirectoryToPath -Path "C:\ProgramData\chocolatey\bin"
  Add-DirectoryToPath -Path "C:\ProgramData\chocolatey\lib\upx\tools"

  Ensure-Command -Name "iscc.exe" -InstallAction {
    $installer = Join-Path $env:TEMP "innosetup-installer.exe"
    Invoke-WebRequest -Uri "https://jrsoftware.org/download.php/is.exe" -OutFile $installer
    Start-Process -FilePath $installer -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait
    Add-DirectoryToPath -Path "C:\Program Files (x86)\Inno Setup 6"
  }

  Ensure-Command -Name "upx.exe" -InstallAction {
    if (-not (Get-Command choco.exe -ErrorAction SilentlyContinue)) {
      throw "UPX is mandatory but Chocolatey is not available to install it automatically."
    }

    choco install upx --no-progress --yes
    Add-DirectoryToPath -Path "C:\ProgramData\chocolatey\bin"
    Add-DirectoryToPath -Path "C:\ProgramData\chocolatey\lib\upx\tools"
  }

  Ensure-SignTool

  if (-not (Get-Command certforge -ErrorAction SilentlyContinue)) {
    if (-not (Get-Command python -ErrorAction SilentlyContinue)) {
      throw "CertForge is required for installer signing but Python is not available."
    }

    python -m pip install --upgrade pip
    python -m pip install certforge
  }

  if (-not (Get-Command certforge -ErrorAction SilentlyContinue)) {
    throw "CertForge is required for installer signing but was not found."
  }
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

  & upx --best --lzma $ResolvedBinaryPath
  if ($LASTEXITCODE -ne 0) {
    throw "UPX compression failed for '$ResolvedBinaryPath'"
  }
}

function Build-WindowsInstaller {
  param(
    [string]$ResolvedVersion,
    [string]$ResolvedOutputRoot,
    [string]$ResolvedHostTriple,
    [string]$ResolvedBootstrapScriptUrl,
    [string]$ResolvedBinaryPath
  )

  Ensure-WindowsInstallerDependencies
  Compress-BinaryWithUpx -ResolvedBinaryPath $ResolvedBinaryPath

  $metadata = Resolve-BootstrapScriptMetadata -Url $ResolvedBootstrapScriptUrl

  $env:VERSION = $ResolvedVersion
  $env:ROOT_DIR = (Resolve-Path ".").Path
  $env:BOOTSTRAP_SCRIPT_SIZE = [string]$metadata.Size
  $env:BOOTSTRAP_SCRIPT_SHA256 = $metadata.Sha256

  iscc .\config\innosetup\installer.iss

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

  $wingetInfo = [ordered]@{
    version       = $ResolvedVersion
    release_tag   = $releaseTag
    installer     = $installerName
    installer_url = $installerUrl
    sha256        = $installerSha256
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

  foreach ($manifest in $manifestFiles) {
    (Get-Content -LiteralPath $manifest.FullName) -replace '(?i)^(?<indent>\s*)(?<key>PackageVersion:\s*).*$' , ('${indent}${key}' + $ResolvedVersion) |
      Set-Content -LiteralPath $manifest.FullName -Encoding UTF8

    (Get-Content -LiteralPath $manifest.FullName) -replace '(?i)^(?<indent>\s*)(?<key>InstallerSha256:\s*).*$' , ('${indent}${key}' + $installerSha256) |
      Set-Content -LiteralPath $manifest.FullName -Encoding UTF8

    (Get-Content -LiteralPath $manifest.FullName) -replace '(?i)^(?<indent>\s*)(?<key>InstallerUrl:\s*).*$' , ('${indent}${key}' + $installerUrl) |
      Set-Content -LiteralPath $manifest.FullName -Encoding UTF8
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
