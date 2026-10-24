# Declarative release inputs are data, never shell source. Validate before path,
# workflow-output, compiler, signing or publication operations.
function Assert-ReleaseVersion {
  param([AllowEmptyString()][string]$Value, [string]$Name = 'Version')
  $number = '(0|[1-9][0-9]*)'
  $pattern = '\A' + $number + '\.' + $number + '\.' + $number + '(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\z'
  if ($Value.Length -gt 128 -or -not [regex]::IsMatch($Value, $pattern)) {
    throw "$Name must be a valid SemVer without a tag prefix, whitespace or path components."
  }
  $prerelease = [regex]::Match($Value, $pattern).Groups[4].Value
  foreach ($part in $prerelease.Split('.')) {
    if ($part -match '\A0[0-9]+\z') { throw "$Name has a leading-zero prerelease number." }
  }
}

function Get-WorkflowReleaseVersion {
  param([string]$EventName, [string]$RefName, [string]$RequestedVersion)
  if ($EventName -eq 'push') {
    if (-not $RefName.StartsWith('v', [StringComparison]::Ordinal)) { throw 'Release tag must start with v.' }
    $version = $RefName.Substring(1)
    Assert-ReleaseVersion -Value $version -Name 'Release tag'
    return $version
  }
  if ($EventName -ne 'workflow_dispatch') { throw 'Unsupported release event.' }
  if ($RequestedVersion -ne '') { Assert-ReleaseVersion -Value $RequestedVersion }
  return $RequestedVersion
}

function Assert-ReleaseArtifactName {
  param([string]$Value, [switch]$AllowPattern)
  $pattern = if ($AllowPattern) { '\A[A-Za-z0-9][A-Za-z0-9_.-]{0,126}\*?\z' } else { '\A[A-Za-z0-9][A-Za-z0-9_.-]{0,127}\z' }
  if (-not [regex]::IsMatch($Value, $pattern) -or $Value.Contains('..')) { throw 'Unsafe release artifact identifier.' }
}

function Assert-LlvmSourceMetadata {
  param([string]$Url, [string]$Sha256, [string]$Feature, [string]$PrefixEnv)
  if (-not [regex]::IsMatch($Url, '\Ahttps://github\.com/llvm/llvm-project/releases/download/llvmorg-[A-Za-z0-9.+-]+/[A-Za-z0-9%_.+-]+\z')) { throw 'LLVM source must be an official HTTPS release asset.' }
  if (-not [regex]::IsMatch($Sha256, '\A[a-fA-F0-9]{64}\z')) { throw 'LLVM source requires SHA-256.' }
  if (-not [regex]::IsMatch($Feature, '\Allvm-toolchain-[0-9]+\z')) { throw 'Invalid LLVM backend feature.' }
  if (-not [regex]::IsMatch($PrefixEnv, '\ALLVM_SYS_[0-9]+_PREFIX\z')) { throw 'Invalid LLVM prefix environment variable.' }
}
