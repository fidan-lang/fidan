function Get-HostPlatform {
  # PowerShell Core (6/7+) exposes the clean cross-platform built-in flags.
  if ($PSVersionTable.PSEdition -eq 'Core') {
    if ($IsWindows) {
      return 'Windows'
    }
    if ($IsMacOS) {
      return 'MacOS'
    }
    if ($IsLinux) {
      return 'Linux'
    }
  }

  # Windows PowerShell (Desktop) runs on Windows only.
  if ($env:OS -eq 'Windows_NT') {
    return 'Windows'
  }

  # Defensive fallback for unusual hosts.
  try {
    if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Windows)) {
      return 'Windows'
    }
    if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::OSX)) {
      return 'MacOS'
    }
    if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Linux)) {
      return 'Linux'
    }
  }
  catch {
  }

  return 'Unknown'
}

function Get-HostPlatformFlags {
  $hostPlatform = Get-HostPlatform

  return [pscustomobject]@{
    HostPlatform  = $hostPlatform
    IsWindowsHost = ($hostPlatform -eq 'Windows')
    IsMacOSHost   = ($hostPlatform -eq 'MacOS')
    IsLinuxHost   = ($hostPlatform -eq 'Linux')
  }
}
