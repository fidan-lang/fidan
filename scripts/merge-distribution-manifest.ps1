param(
  [string]$FragmentsRoot = "dist/release/fragments",
  [string]$ExistingManifestPath = "",
  [string]$OutputPath = "dist/release/manifest.json"
)

$ErrorActionPreference = "Stop"

function New-Manifest {
  return [ordered]@{
    schema_version = 1
    fidan_versions = @()
    toolchains     = @()
  }
}

function ConvertTo-Array {
  param([object]$Value)

  if ($null -eq $Value) {
    return @()
  }

  if ($Value -is [System.Array]) {
    return @($Value)
  }

  if ($Value -is [System.Collections.IEnumerable] -and -not ($Value -is [string]) -and -not ($Value -is [hashtable])) {
    return @($Value)
  }

  return @($Value)
}

function ConvertTo-JsonArray {
  param([object]$Value)

  $items = @(ConvertTo-Array $Value)
  if ($items.Count -eq 0) {
    return "[]"
  }

  return $items | ConvertTo-Json -Depth 12 -AsArray
}

function Merge-ReleaseList {
  param(
    [System.Collections.IEnumerable]$Existing,
    [System.Collections.IEnumerable]$Incoming,
    [scriptblock]$KeySelector
  )

  $table = @{}
  foreach ($item in (ConvertTo-Array $Existing)) {
    $table[(& $KeySelector $item)] = $item
  }
  foreach ($item in (ConvertTo-Array $Incoming)) {
    $table[(& $KeySelector $item)] = $item
  }
  return @($table.Values)
}

function Get-SemverDescending {
  param(
    [System.Collections.IEnumerable]$Items,
    [string]$VersionProperty,
    [string]$TieBreakerProperty
  )

  return @(
    $Items | Sort-Object `
      -Property @{ Expression = {
        try { [version]($_.$VersionProperty) } catch { [version]"0.0.0" }
      }; Descending           = $true
    }, `
    @{ Expression = { $_.$TieBreakerProperty }; Descending = $false }
  )
}

$manifest = if ($ExistingManifestPath -and (Test-Path -LiteralPath $ExistingManifestPath)) {
  Get-Content -LiteralPath $ExistingManifestPath -Raw | ConvertFrom-Json -AsHashtable
}
else {
  New-Manifest
}

$fragments = @(Get-ChildItem -LiteralPath $FragmentsRoot -Filter *.json -File -Recurse -ErrorAction Stop)
if ($fragments.Count -eq 0) {
  throw "No manifest fragments found in '$FragmentsRoot'"
}

foreach ($fragmentFile in $fragments) {
  $fragment = Get-Content -LiteralPath $fragmentFile.FullName -Raw | ConvertFrom-Json -AsHashtable
  $manifest.fidan_versions = @(Merge-ReleaseList `
      -Existing $manifest.fidan_versions `
      -Incoming $fragment.fidan_versions `
      -KeySelector { param($item) "$($item.version)|$($item.host_triple)" })
  $manifest.toolchains = @(Merge-ReleaseList `
      -Existing $manifest.toolchains `
      -Incoming $fragment.toolchains `
      -KeySelector { param($item) "$($item.kind)|$($item.toolchain_version)|$($item.host_triple)" })
}

$manifest.schema_version = 1
$manifest.fidan_versions = @(Get-SemverDescending `
    -Items $manifest.fidan_versions `
    -VersionProperty "version" `
    -TieBreakerProperty "host_triple")
$manifest.toolchains = @(Get-SemverDescending `
    -Items $manifest.toolchains `
    -VersionProperty "toolchain_version" `
    -TieBreakerProperty "host_triple")

$outputDir = Split-Path -Parent $OutputPath
if ($outputDir) {
  New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
}

$outputManifest = [pscustomobject]@{
  schema_version = $manifest.schema_version
  fidan_versions = @(ConvertTo-Array $manifest.fidan_versions)
  toolchains     = @(ConvertTo-Array $manifest.toolchains)
}

$fidanJson = ConvertTo-JsonArray $outputManifest.fidan_versions
$toolchainsJson = ConvertTo-JsonArray $outputManifest.toolchains
$schemaVersion = $outputManifest.schema_version
$json = @"
{
  "schema_version": $schemaVersion,
  "fidan_versions": $fidanJson,
  "toolchains": $toolchainsJson
}
"@
Set-Content -LiteralPath $OutputPath -Encoding UTF8 -Value $json
Write-Host "Wrote merged manifest to $OutputPath"
