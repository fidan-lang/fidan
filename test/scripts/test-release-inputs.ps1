$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
. (Join-Path $repo 'scripts/shared/release-inputs.ps1')
$checks = 0
function Assert-InputFailure {
  param([scriptblock]$Action, [string]$Pattern = ".")
  $failed = $false
  try { & $Action } catch { $failed = $true; if ($_.Exception.Message -notmatch $Pattern) { throw "Wrong failure: $($_.Exception.Message)" } }
  if (-not $failed) { throw 'Unsafe input was accepted.' }
  $script:checks++
}
foreach ($value in @('1.0.16', '23.1.2', '1.1.0-rc.1', '1.0.0+build.7')) { Assert-ReleaseVersion $value; $checks++ }
foreach ($value in @('', 'v1.0.16', '01.0.0', '1.0.0-01', '../1.0.0', '1.0.0/../../x', "1.0.0`nFORGED=1", "1.0.0`r", '1.0.0"; throw "INJECTED', '$(throw "INJECTED")', '`whoami`', '--help')) {
  Assert-InputFailure { Assert-ReleaseVersion $value }
}
foreach ($event in @('push', 'workflow_dispatch')) {
  foreach ($value in @('1.0.0"; throw "INJECTED', "1.0.0`nFORGED=1", '../../outside', '$(throw "INJECTED")')) {
    Assert-InputFailure { Get-WorkflowReleaseVersion -EventName $event -RefName ('v' + $value) -RequestedVersion $value }
  }
}
if ((Get-WorkflowReleaseVersion -EventName push -RefName v1.0.16) -ne '1.0.16') { throw 'Tag normalization changed.' }; $checks++
Assert-InputFailure { Get-WorkflowReleaseVersion -EventName pull_request -RefName v1.0.16 }
foreach ($value in @('../payload', '/tmp/x', 'x*/*', "x`nFORGED=1", '--flag', 'x?')) {
  Assert-InputFailure { Assert-ReleaseArtifactName $value -AllowPattern }
}
Assert-ReleaseArtifactName 'llvm-toolchain-*' -AllowPattern; $checks++
Assert-ReleaseArtifactName 'toolchain-distribution-bundle-llvm'; $checks++
$url = 'https://github.com/llvm/llvm-project/releases/download/llvmorg-23.1.2/LLVM-23.1.2-Linux-X64.tar.xz'
Assert-LlvmSourceMetadata $url ('a' * 64) 'llvm-toolchain-23' 'LLVM_SYS_231_PREFIX'; $checks++
Assert-InputFailure { Assert-LlvmSourceMetadata ($url + "`nBAD=1") ('a' * 64) 'llvm-toolchain-23' 'LLVM_SYS_231_PREFIX' }
Assert-InputFailure { Assert-LlvmSourceMetadata $url 'invalid' 'llvm-toolchain-23' 'LLVM_SYS_231_PREFIX' }
# Invalid data must fail at the actual entry points, before any build, download,
# signing, staging or subprocess. No real release tools/credentials are used.
foreach ($value in @('../escape', '1.0.0"; throw "INJECTED', "1.0.0`nFORGED=1")) {
  Assert-InputFailure { & (Join-Path $repo 'scripts/package-release.ps1') -Version $value -SkipBuild } -Pattern "SemVer"
  Assert-InputFailure { & (Join-Path $repo 'scripts/package-toolchain.ps1') -Kind ai-analysis -ToolVersion $value -ToolchainVersion '1.0.4' -SkipBuild } -Pattern "SemVer"
}
# An unsuccessful build cannot fall through to a previous release artifact.
$priorExit = $global:LASTEXITCODE
function cargo { $global:LASTEXITCODE = 42 }
try {
  Assert-InputFailure { & (Join-Path $repo 'scripts/package-release.ps1') -Version '1.0.16' } -Pattern 'Release build failed'
} finally {
  Remove-Item Function:cargo
  $global:LASTEXITCODE = $priorExit
}
Write-Output "Release input regressions: $checks checks passed."
