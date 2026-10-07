$ErrorActionPreference = "Stop"

if (-not $IsWindows) { throw "WinGet preparation requires Windows." }

try {
  Install-PackageProvider -Name NuGet -Force -Scope CurrentUser -ErrorAction Stop | Out-Null
  Install-Module -Name Microsoft.WinGet.Client -Repository PSGallery -Scope CurrentUser -Force -AllowClobber -ErrorAction Stop
  Import-Module Microsoft.WinGet.Client -ErrorAction Stop
  $repairResult = Repair-WinGetPackageManager -AllUsers -Latest -Force -ErrorAction Stop
  if (@($repairResult | Where-Object { $_ -is [int] -and $_ -ne 0 }).Count) {
    throw "Repair-WinGetPackageManager returned failure code: $repairResult"
  }

  $windowsApps = Join-Path $env:LOCALAPPDATA "Microsoft/WindowsApps"
  $env:PATH = "$windowsApps$([IO.Path]::PathSeparator)$env:PATH"
  $winget = Get-Command winget.exe -ErrorAction Stop
  & $winget.Source --info
  if ($LASTEXITCODE -ne 0) { throw "winget --info failed with exit code $LASTEXITCODE" }
} catch {
  throw "WinGet bootstrap/repair failed; manifest validation and submission were not started. $($_.Exception.Message)"
}
