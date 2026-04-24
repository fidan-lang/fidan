function ConvertTo-WindowsVcRedistVersionString {
  param([string]$VersionString)

  if (-not $VersionString) {
    return ""
  }

  $trimmed = $VersionString.Trim()
  if ($trimmed -match '^\D*(\d+(?:\.\d+){1,3})\D*$') {
    return $matches[1]
  }

  return ""
}

function ConvertTo-WindowsVcRedistVersion {
  param([string]$VersionString)

  $normalized = ConvertTo-WindowsVcRedistVersionString -VersionString $VersionString
  if (-not $normalized) {
    return $null
  }

  return [version]$normalized
}

function Get-WindowsVcRedistArchitecture {
  param([string]$HostTriple = "")

  $resolvedHostTriple = $HostTriple
  if (-not $resolvedHostTriple) {
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
    switch ($arch) {
      "x64" { return "x64" }
      "arm64" { return "arm64" }
      default { throw "Unsupported Windows VC++ Redistributable architecture: '$arch'" }
    }
  }

  if ($resolvedHostTriple -like "x86_64-*") {
    return "x64"
  }
  if ($resolvedHostTriple -like "aarch64-*") {
    return "arm64"
  }

  throw "Unsupported Windows host triple for VC++ Redistributable metadata: '$resolvedHostTriple'"
}

function Get-WindowsVcRedistPackageIdentifier {
  param([string]$Architecture)

  switch ($Architecture) {
    "x64" { return "Microsoft.VCRedist.2015+.x64" }
    "arm64" { return "Microsoft.VCRedist.2015+.arm64" }
    default { throw "Unsupported Windows VC++ Redistributable architecture: '$Architecture'" }
  }
}

function Get-WindowsVcRedistRegistryPaths {
  param([string]$Architecture)

  return @(
    "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\$Architecture",
    "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\VisualStudio\14.0\VC\Runtimes\$Architecture"
  )
}

function Get-WindowsVcRedistState {
  param([string]$Architecture)

  foreach ($path in (Get-WindowsVcRedistRegistryPaths -Architecture $Architecture)) {
    if (-not (Test-Path -LiteralPath $path)) {
      continue
    }

    $props = Get-ItemProperty -LiteralPath $path
    $versionString = ""
    if ($props.Version) {
      $versionString = [string]$props.Version
    }

    return [pscustomobject]@{
      Installed         = ($props.Installed -eq 1)
      Version           = $versionString
      NormalizedVersion = ConvertTo-WindowsVcRedistVersionString -VersionString $versionString
      RegistryPath      = $path
    }
  }

  return [pscustomobject]@{
    Installed         = $false
    Version           = ""
    NormalizedVersion = ""
    RegistryPath      = ""
  }
}

function Get-WindowsVcRedistReleaseMetadata {
  param([string]$HostTriple)

  $architecture = Get-WindowsVcRedistArchitecture -HostTriple $HostTriple
  $state = Get-WindowsVcRedistState -Architecture $architecture
  if (-not $state.Installed -or -not $state.NormalizedVersion) {
    throw "Unable to determine installed Microsoft Visual C++ Redistributable version for architecture '$architecture'."
  }

  return [pscustomobject]@{
    Architecture      = $architecture
    PackageIdentifier = Get-WindowsVcRedistPackageIdentifier -Architecture $architecture
    MinimumVersion    = $state.NormalizedVersion
  }
}
