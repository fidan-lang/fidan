$ErrorActionPreference = 'Stop'

$dir = 'config/innosetup/languages'

$targets = @(
  'english',
  'armenian',
  'brazilianportuguese',
  'bulgarian',
  'catalan',
  'corsican',
  'czech',
  'danish',
  'dutch',
  'finnish',
  'french',
  'german',
  'hungarian',
  'italian',
  'japanese',
  'norwegian',
  'polish',
  'portuguese',
  'russian',
  'slovak',
  'slovenian',
  'spanish',
  'turkish',
  'ukrainian'
)

$codes = @{
  english             = 'en'
  armenian            = 'hy'
  brazilianportuguese = 'pt'
  bulgarian           = 'bg'
  catalan             = 'ca'
  corsican            = 'co'
  czech               = 'cs'
  danish              = 'da'
  dutch               = 'nl'
  finnish             = 'fi'
  french              = 'fr'
  german              = 'de'
  hungarian           = 'hu'
  italian             = 'it'
  japanese            = 'ja'
  norwegian           = 'no'
  polish              = 'pl'
  portuguese          = 'pt'
  russian             = 'ru'
  slovak              = 'sk'
  slovenian           = 'sl'
  spanish             = 'es'
  turkish             = 'tr'
  ukrainian           = 'uk'
}

$messages = [ordered]@{
  UsageTitle                        = 'Fidan bootstrap installer'
  UsageOptionsHeader                = 'Options:'
  UsageOptionVersion                = 'Install a specific released version (default: %1; use "latest" for newest published release)'
  UsageOptionManifestUrl            = 'Override the distribution manifest URL'
  UsageOptionInstallRoot            = 'Override the self-managed install root'
  UsageOptionSkipPathUpdate         = 'Do not modify the user PATH'
  UsageOptionAllowExistingInstall   = 'Permit bootstrapping into an existing Fidan install root'
  UsageOptionHelp                   = 'Show this help text'
  UsageFooterLine1                  = 'Bootstrap is intended for first install. If Fidan is already installed,'
  UsageFooterLine2                  = 'prefer "fidan self install" and "fidan self use".'
  ErrMissingValueAfter              = 'Missing value after %1'
  ErrUnknownArgument                = 'Unknown argument: %1'
  WizardBootstrapOptionsTitle       = 'Bootstrap Options'
  WizardBootstrapOptionsDescription = 'Configure bootstrap parameters'
  WizardBootstrapOptionsSubCaption  = 'These options are equivalent to bootstrap command-line arguments.'
  WizardBootstrapOptionsHint        = 'Leave Manifest URL and Install root empty to use bootstrap defaults.'
  WizardFieldVersion                = 'Version (default: %1 or "latest"):'
  WizardFieldManifestUrl            = 'Manifest URL override (optional):'
  WizardFieldInstallRoot            = 'Install root override (optional):'
  WizardFlagsTitle                  = 'Bootstrap Flags'
  WizardFlagsDescription            = 'Choose optional bootstrap flags'
  WizardFlagsSubCaption             = 'These map directly to bootstrap command-line switch behavior.'
  WizardFlagSkipPathUpdate          = 'Skip PATH update'
  WizardFlagAllowExistingInstall    = 'Allow existing install root'
  ErrNoPowerShellRuntime            = 'No supported PowerShell runtime was found.'
  ErrNoPowerShellRuntimeLine2       = 'Install PowerShell (pwsh) or ensure Windows PowerShell is available, then rerun setup.'
  ErrBootstrapScriptMissing         = 'Downloaded bootstrap script was not found in temporary directory.'
  ErrBootstrapScriptMissingLine2    = 'Please retry setup. If the issue persists, check network/proxy settings.'
  ErrFailedToLaunchPowerShell       = 'Failed to launch %1 to run bootstrap.ps1.'
  ErrBootstrapFailedHeader          = 'Fidan bootstrap failed with exit code %1'
  ErrPowerShellOutputHeader         = 'PowerShell output:'
  ErrNoPowerShellOutput             = 'No PowerShell output was captured.'
  ErrBootstrapFailedFooter          = 'If needed, rerun with logging enabled and check the installer log.'
  ErrBootstrapProcessFailed         = 'Bootstrap process failed.'
  OutputTruncated                   = '... (output truncated)'
  StatusBootstrapping               = 'Bootstrapping Fidan...'
}

$languageThrottle = 6
$messageThrottle = 12

$targets | ForEach-Object -Parallel {
  $ErrorActionPreference = 'Stop'

  $name = $_
  $dir = $using:dir
  $codes = $using:codes
  $messages = $using:messages
  $messageThrottle = $using:messageThrottle

  function ConvertTo-InnoSetupString {
    param(
      [string]$Text,
      [string]$Target
    )

    $p = $Text.
    Replace('%1', '__T0__').
    Replace('"latest"', '__T1__').
    Replace('"fidan self install"', '__T2__').
    Replace('"fidan self use"', '__T3__').
    Replace('bootstrap.ps1', '__T4__').
    Replace('PowerShell', '__T5__').
    Replace('PATH', '__T6__')

    $q = [uri]::EscapeDataString($p)
    $url = "https://translate.googleapis.com/translate_a/single?client=gtx&sl=en&tl=$Target&dt=t&q=$q"

    $resp = Invoke-RestMethod -Uri $url -TimeoutSec 30

    $out = ''
    foreach ($seg in $resp[0]) {
      $out += [string]$seg[0]
    }

    $out = $out.
    Replace('__T0__', '%1').
    Replace('__T1__', '"latest"').
    Replace('__T2__', '"fidan self install"').
    Replace('__T3__', '"fidan self use"').
    Replace('__T4__', 'bootstrap.ps1').
    Replace('__T5__', 'PowerShell').
    Replace('__T6__', 'PATH')

    return $out
  }

  $code = $codes[$name]
  $path = Join-Path $dir ($name + '.isl')

  if ($name -eq 'english') {
    $lines = @('[CustomMessages]')
    foreach ($k in $messages.Keys) {
      $v = $messages[$k] -replace "`r|`n", ' '
      $lines += ('{0}={1}' -f $k, $v)
    }

    Set-Content -LiteralPath $path -Value $lines -Encoding UTF8
    Write-Host "Updated $name"
    return
  }

  $messageEntries = foreach ($k in $messages.Keys) {
    [pscustomobject]@{
      Key   = $k
      Value = $messages[$k]
    }
  }

  $translated = $messageEntries | ForEach-Object -Parallel {
    $ErrorActionPreference = 'Stop'

    $entry = $_
    $code = $using:code
    $lang = $using:name

    function ConvertTo-InnoSetupString {
      param(
        [string]$Text,
        [string]$Target
      )

      $p = $Text.
      Replace('%1', '__T0__').
      Replace('"latest"', '__T1__').
      Replace('"fidan self install"', '__T2__').
      Replace('"fidan self use"', '__T3__').
      Replace('bootstrap.ps1', '__T4__').
      Replace('PowerShell', '__T5__').
      Replace('PATH', '__T6__')

      $q = [uri]::EscapeDataString($p)
      $url = "https://translate.googleapis.com/translate_a/single?client=gtx&sl=en&tl=$Target&dt=t&q=$q"

      $resp = Invoke-RestMethod -Uri $url -TimeoutSec 30

      $out = ''
      foreach ($seg in $resp[0]) {
        $out += [string]$seg[0]
      }

      $out = $out.
      Replace('__T0__', '%1').
      Replace('__T1__', '"latest"').
      Replace('__T2__', '"fidan self install"').
      Replace('__T3__', '"fidan self use"').
      Replace('__T4__', 'bootstrap.ps1').
      Replace('__T5__', 'PowerShell').
      Replace('__T6__', 'PATH')

      return $out
    }

    try {
      $translatedValue = ConvertTo-InnoSetupString -Text $entry.Value -Target $code
    }
    catch {
      Write-Warning "[$lang] Failed translating '$($entry.Key)': $($_.Exception.Message)"
      $translatedValue = $entry.Value
    }

    [pscustomobject]@{
      Key   = $entry.Key
      Value = ($translatedValue -replace "`r|`n", ' ')
    }
  } -ThrottleLimit $messageThrottle

  $translatedMap = @{}
  foreach ($item in $translated) {
    $translatedMap[$item.Key] = $item.Value
  }

  $lines = @('[CustomMessages]')
  foreach ($k in $messages.Keys) {
    $lines += ('{0}={1}' -f $k, $translatedMap[$k])
  }

  Set-Content -LiteralPath $path -Value $lines -Encoding UTF8
  Write-Host "Updated $name"
} -ThrottleLimit $languageThrottle
