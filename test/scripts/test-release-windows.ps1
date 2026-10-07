$ErrorActionPreference = "Stop"
if (-not $IsWindows) { throw "Windows release regressions require Windows." }

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
$scratch = Join-Path $repoRoot ("target/release-pipeline-test-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $scratch | Out-Null
$savedEnvironment = @{}
foreach ($name in @("PATH", "VERSION", "ROOT_DIR", "BOOTSTRAP_SCRIPT_SIZE", "BOOTSTRAP_SCRIPT_SHA256", "FIDAN_BUILD_INSTALLER", "FIDAN_RELEASE_PROBE_MUTATE", "GITHUB_REF_NAME", "WINGET_CREATE_GITHUB_TOKEN")) {
  $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
}
$previousExit = $global:LASTEXITCODE
$checks = 0
function Assert-ReleaseCheck {
  param([bool]$Condition, [string]$Message)
  if (-not $Condition) { throw $Message }
  $script:checks++
}
function Assert-ReleaseFailure {
  param([scriptblock]$Action, [string]$Pattern)
  $message = ""
  try { & $Action } catch { $message = $_.Exception.Message }
  Assert-ReleaseCheck ($message -like $Pattern) "Expected '$Pattern', got '$message'"
}

# Exercise real release functions with only host tools/network/signing stubbed.
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile((Join-Path $repoRoot "scripts/package-release-windows.ps1"), [ref]$tokens, [ref]$parseErrors)
if ($parseErrors) { throw $parseErrors }
foreach ($definition in $ast.FindAll({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] }, $false)) {
  . ([scriptblock]::Create($definition.Extent.Text))
}
. (Join-Path $repoRoot "scripts/shared/windows-vc-redist.ps1")
function Install-WindowsInstallerDependencies {}
function Resolve-BootstrapScriptMetadata { [pscustomobject]@{ Size = 10; Sha256 = 'a' * 64 } }
function New-TemporarySigningMaterial { [pscustomobject]@{ TempDir = "signing-probe"; PfxPath = "probe.pfx" } }
function Get-IsccSignToolOverrideArgument { 'probe-sign-command' }
function Remove-TemporarySigningMaterial { $script:signingCleanups++ }
function Get-WindowsVcRedistReleaseMetadata { [pscustomobject]@{ PackageIdentifier = "Microsoft.VCRedist.2015+.x64"; MinimumVersion = "14.99.0.0" } }
function Start-Process {
  param($FilePath, $ArgumentList, [switch]$NoNewWindow, [switch]$Wait, [switch]$PassThru)
  Assert-ReleaseCheck ($FilePath -eq 'iscc.exe' -and $ArgumentList[0] -eq 'probe-sign-command') "Installer signing command was not preserved"
  New-Item -ItemType Directory -Force dist/innosetup/installers | Out-Null
  Set-Content dist/innosetup/installers/fidan_windows_bootstrap_v1.0.15.exe "installer"
  [pscustomobject]@{ ExitCode = $script:isccExit }
}

Push-Location $scratch
try {
  New-Item -ItemType Directory -Force target/release | Out-Null
  $binary = Join-Path $scratch "target/release/fidan.exe"
  [IO.File]::WriteAllBytes($binary, [byte[]](0x4D, 0x5A, 0x90, 0))
  $originalHash = (Get-FileHash $binary).Hash
  $script:signingCleanups = 0
  $script:isccExit = 0
  Build-WindowsInstaller -ResolvedVersion 1.0.15 -ResolvedOutputRoot dist/installer-probe -ResolvedHostTriple x86_64-pc-windows-msvc -ResolvedBootstrapScriptUrl https://fidan.dev/install.ps1
  Assert-ReleaseCheck ((Get-FileHash $binary).Hash -eq $originalHash -and $script:signingCleanups -eq 1) "Installer modified Fidan or skipped signing cleanup"
  $script:isccExit = 42
  Assert-ReleaseFailure { Build-WindowsInstaller -ResolvedVersion 1.0.15 -ResolvedOutputRoot dist/installer-probe -ResolvedHostTriple x86_64-pc-windows-msvc } '*ISCC.exe failed with exit code 42*'
  Assert-ReleaseCheck ($script:signingCleanups -eq 2) "Failed installer did not clean signing material"

  # Run the complete archive script in an isolated repository skeleton.
  New-Item -ItemType Directory -Force scripts/shared, crates/libfidan/include, crates/libfidan/examples/embed_c | Out-Null
  Copy-Item (Join-Path $repoRoot "scripts/package-release.ps1") scripts/
  Copy-Item (Join-Path $repoRoot "scripts/shared/host-platform.ps1") scripts/shared/
  Set-Content scripts/shared/windows-vc-redist.ps1 'function Get-WindowsVcRedistReleaseMetadata { [pscustomobject]@{ MinimumVersion = "14.0.0.0" } }'
  Set-Content scripts/package-release-windows.ps1 @'
param($Mode, $Version, $OutputRoot, $HostTriple, $BootstrapScriptUrl)
if ($env:FIDAN_RELEASE_PROBE_MUTATE -eq "1") { Set-Content target/release/fidan.exe "mutated" }
'@
  foreach ($name in @("fidan_runtime.lib", "fidan_runtime.dll", "fidan_runtime.dll.lib", "libfidan.lib", "libfidan.dll", "libfidan.dll.lib")) {
    Set-Content (Join-Path target/release $name) "artifact"
  }
  Set-Content crates/libfidan/include/fidan.h "header"
  Set-Content crates/libfidan/examples/embed_c/main.c "example"
  $env:FIDAN_BUILD_INSTALLER = "1"
  $env:FIDAN_RELEASE_PROBE_MUTATE = "0"
  & ./scripts/package-release.ps1 -Version 1.0.15 -SkipBuild -OutputRoot dist/archive-probe
  $archive = Get-ChildItem dist/archive-probe -Recurse -Filter *.tar.gz | Select-Object -First 1
  New-Item -ItemType Directory extracted | Out-Null
  & tar -xzf $archive.FullName -C extracted ./fidan.exe
  Assert-ReleaseCheck ($LASTEXITCODE -eq 0 -and (Get-FileHash extracted/fidan.exe).Hash -eq $originalHash -and (Get-FileHash $binary).Hash -eq $originalHash) "Archive differs from the compiled binary"
  $env:FIDAN_RELEASE_PROBE_MUTATE = "1"
  Assert-ReleaseFailure { & ./scripts/package-release.ps1 -Version 1.0.15 -SkipBuild -OutputRoot dist/mutation-probe } '*installer packaging modified*'
  Assert-ReleaseCheck (-not (Test-Path dist/mutation-probe)) "Mutated binary was packaged"

  # Published-release staging must use immutable assets and original VC metadata.
  $script:publishedDigest = "sha256:$originalHash".ToLowerInvariant()
  $script:publishedDraft = $false
  $script:ghFailure = ""
  $script:vcMinimum = "14.51.36247.00"
  function gh {
    $global:LASTEXITCODE = if ($args[1] -eq $script:ghFailure) { 17 } else { 0 }
    if ($args[1] -eq "view") {
      @{ tagName = "v1.0.15"; isDraft = $script:publishedDraft; assets = @(@{ name = "fidan_windows_bootstrap_v1.0.15.exe"; digest = $script:publishedDigest }) } | ConvertTo-Json -Depth 4
    } elseif ($args[1] -eq "download" -and $global:LASTEXITCODE -eq 0) {
      $dir = $args[[array]::IndexOf($args, '--dir') + 1]
      [IO.File]::WriteAllBytes((Join-Path (Resolve-Path $dir).Path "fidan_windows_bootstrap_v1.0.15.exe"), [byte[]](0x4D, 0x5A, 0x90, 0))
    }
  }
  function Invoke-RestMethod { @{ fidan_versions = @(@{ version = "1.0.15"; host_triple = "x86_64-pc-windows-msvc"; vc_redist_min_version = $script:vcMinimum }) } }
  $env:GITHUB_REF_NAME = "v9.9.9"
  $stageArgs = @{ ResolvedVersion = "1.0.15"; ResolvedOutputRoot = "dist/published"; ResolvedWingetManifestRoot = (Join-Path $repoRoot "config/winget/manifest") }
  Copy-PublishedWingetRelease @stageArgs
  $metadata = Get-Content dist/published/winget/windows-installer.json -Raw | ConvertFrom-Json
  Assert-ReleaseCheck ($metadata.release_tag -eq "v1.0.15" -and $metadata.vc_redist.minimum_version -eq $script:vcMinimum -and $metadata.sha256 -eq $originalHash -and (Test-Path dist/published/winget/manifests/Fidan.Fidan.installer.yaml)) "Published asset/VC metadata was replaced by retry host metadata"
  $script:publishedDraft = $true
  Assert-ReleaseFailure { Copy-PublishedWingetRelease @stageArgs } '*is not published*'
  $script:publishedDraft = $false
  $script:publishedDigest = 'sha256:' + ('0' * 64)
  Assert-ReleaseFailure { Copy-PublishedWingetRelease @stageArgs } '*SHA256 does not match*'
  $script:publishedDigest = "sha256:$originalHash".ToLowerInvariant()
  foreach ($phase in @("view", "download")) {
    $script:ghFailure = $phase
    Assert-ReleaseFailure { Copy-PublishedWingetRelease @stageArgs } '*Failed to*'
  }
  $script:ghFailure = ""
  $script:vcMinimum = "invalid"
  Assert-ReleaseFailure { Copy-PublishedWingetRelease @stageArgs } '*lacks a valid Windows VC++ requirement*'

  # Submit the actual staged manifests without provisioning or invoking winget.exe.
  $submissionState = @{ DownloadFailure = $false; ExitCode = 0; Downloads = 0; Calls = 0; Arguments = @() }
  function Get-Command { throw "CI submission must not look up winget.exe" }
  function Invoke-WebRequest {
    param($Uri, $OutFile, $TimeoutSec, $ErrorAction)
    $submissionState.Downloads++
    if ($Uri -ne 'https://aka.ms/wingetcreate/latest' -or $TimeoutSec -le 0 -or $ErrorAction -ne 'Stop') {
      throw "Expected fail-closed download from Microsoft's standalone endpoint"
    }
    if ($submissionState.DownloadFailure) { throw "WingetCreate download probe failure" }
    Set-Content -LiteralPath $OutFile 'standalone executable probe'
  }
  function Invoke-WingetCreateProbe {
    $submissionState.Calls++
    $submissionState.Arguments = $args
    if ($env:WINGET_CREATE_GITHUB_TOKEN -ne 'test-token-not-a-credential') { throw "Environment token missing" }
    $global:LASTEXITCODE = $submissionState.ExitCode
  }
  $wingetCreateProbePath = Join-Path $scratch 'wingetcreate.exe'
  # Resolve the real executable call to a probe, without launching any submission.
  Set-Alias -Name $wingetCreateProbePath -Value Invoke-WingetCreateProbe
  $submitArgs = @{ ResolvedVersion = '1.0.15'; ResolvedOutputRoot = 'dist/published'; ResolvedWingetManifestRoot = 'dist/published/winget/manifests' }
  $expectedManifestDir = (Resolve-Path $submitArgs.ResolvedWingetManifestRoot).Path
  foreach ($token in @('', '   ')) {
    $env:WINGET_CREATE_GITHUB_TOKEN = $token
    Assert-ReleaseFailure { Submit-WingetManifest @submitArgs } '*WINGET_CREATE_GITHUB_TOKEN is required*'
  }
  Assert-ReleaseCheck ($submissionState.Downloads -eq 0 -and $submissionState.Calls -eq 0) "Missing token did not stop download/submission"
  $env:WINGET_CREATE_GITHUB_TOKEN = 'test-token-not-a-credential'
  $submissionState.DownloadFailure = $true
  Assert-ReleaseFailure { Submit-WingetManifest @submitArgs } '*WingetCreate download probe failure*'
  Assert-ReleaseCheck ($submissionState.Calls -eq 0) "Failed download still submitted manifests"
  $submissionState.DownloadFailure = $false
  $submissionState.ExitCode = 17
  Assert-ReleaseFailure { Submit-WingetManifest @submitArgs } '*wingetcreate submit failed with exit code 17*'
  $submissionState.ExitCode = 0
  Submit-WingetManifest @submitArgs
  Assert-ReleaseCheck ($submissionState.Calls -eq 2 -and ($submissionState.Arguments -join '|') -eq "submit|$expectedManifestDir|--no-open") "Manifest directory or token-free submission arguments changed"

  $manifests = Get-ChildItem dist/published/winget/manifests -Filter *.yaml -File
  foreach ($manifest in $manifests) {
    $content = Get-Content -LiteralPath $manifest.FullName -Raw
    Assert-ReleaseCheck ($content -match '(?m)^PackageVersion: 1\.0\.15\s*$') "Manifest version was not rewritten"
  }
  $installerManifest = Get-Content dist/published/winget/manifests/Fidan.Fidan.installer.yaml -Raw
  $repo = if ($env:GITHUB_REPOSITORY) { $env:GITHUB_REPOSITORY } else { 'fidan-lang/fidan' }
  $expectedUrl = "https://github.com/$repo/releases/download/v1.0.15/fidan_windows_bootstrap_v1.0.15.exe"
  Assert-ReleaseCheck ($installerManifest.Contains("InstallerUrl: $expectedUrl") -and $installerManifest.Contains("InstallerSha256: $originalHash")) "Published installer URL/digest was not preserved in manifests"
  Assert-ReleaseCheck ($installerManifest.Contains('PackageIdentifier: Microsoft.VCRedist.2015+.x64') -and $installerManifest.Contains('MinimumVersion: 14.51.36247.00')) "Original VC++ manifest dependency was not preserved"
  $stagedInstaller = Get-ChildItem dist/published/payload -Recurse -Filter *.exe | Select-Object -First 1
  Assert-ReleaseCheck ((Get-FileHash $stagedInstaller.FullName).Hash -eq $originalHash) "Submission modified the staged release installer"

  # Optional local preparation validates without downloading/submitting WingetCreate.
  function Get-Command { [pscustomobject]@{ Source = 'Invoke-WinGetValidationProbe' } }
  function Invoke-WinGetValidationProbe {
    if (($args -join '|') -ne "validate|--manifest|$expectedManifestDir|--verbose-logs") { throw "Unexpected local validation arguments" }
    $global:LASTEXITCODE = $script:validationExit
  }
  $downloadsBefore = $submissionState.Downloads
  $callsBefore = $submissionState.Calls
  $env:WINGET_CREATE_GITHUB_TOKEN = ''
  $script:validationExit = 0
  Submit-WingetManifest @submitArgs -SkipSubmit
  Assert-ReleaseCheck ($submissionState.Downloads -eq $downloadsBefore -and $submissionState.Calls -eq $callsBefore) "Local validation attempted a submission"
  $script:validationExit = 17
  Assert-ReleaseFailure { Submit-WingetManifest @submitArgs -SkipSubmit } '*winget validate failed*'
  function Get-Command { $null }
  Assert-ReleaseFailure { Submit-WingetManifest @submitArgs -SkipSubmit } '*winget.exe not found on PATH*'

} finally {
  Pop-Location
  foreach ($name in $savedEnvironment.Keys) { [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name]) }
  $global:LASTEXITCODE = $previousExit
  if ($wingetCreateProbePath) { Remove-Item -LiteralPath ("Alias:" + $wingetCreateProbePath) -ErrorAction SilentlyContinue }
  # scratch is a resolved child of the repository's target directory.
  if (-not $scratch.StartsWith((Join-Path $repoRoot "target/") , [StringComparison]::OrdinalIgnoreCase)) { throw "Unexpected cleanup path: $scratch" }
  Remove-Item -LiteralPath $scratch -Recurse -Force
}
Write-Host "Windows release pipeline: $checks checks passed."
exit 0
