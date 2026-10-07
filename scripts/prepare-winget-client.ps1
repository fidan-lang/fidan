$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "shared/host-platform.ps1")
if (-not (Get-HostPlatformFlags).IsWindowsHost) { throw "WinGet preparation requires Windows." }

try {
  # Windows PowerShell's PackageManagement bootstrap requires TLS 1.2.
  [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
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
