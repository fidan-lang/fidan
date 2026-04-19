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
  brazilianportuguese = 'pt-BR'
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
  portuguese          = 'pt-PT'
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

$targets | ForEach-Object -Parallel {
  $ErrorActionPreference = 'Stop'

  $name = $_
  $dir = $using:dir
  $codes = $using:codes
  $messages = $using:messages
  $context = 'This is text for a Windows installer UI. Use short, natural wording.'

  function Protect-InnoSetupTokens {
    param(
      [string]$Text
    )

    return $Text.
    Replace('%1', '__T0__').
    Replace('"latest"', '__T1__').
    Replace('"fidan self install"', '__T2__').
    Replace('"fidan self use"', '__T3__').
    Replace('bootstrap.ps1', '__T4__').
    Replace('PowerShell', '__T5__').
    Replace('PATH', '__T6__')
  }

  function Restore-InnoSetupTokens {
    param(
      [string]$Text
    )

    return $Text.
    Replace('__T0__', '%1').
    Replace('__T1__', '"latest"').
    Replace('__T2__', '"fidan self install"').
    Replace('__T3__', '"fidan self use"').
    Replace('__T4__', 'bootstrap.ps1').
    Replace('__T5__', 'PowerShell').
    Replace('__T6__', 'PATH')
  }

  function ConvertTo-InnoSetupBatch {
    param(
      [string[]]$TextLines,
      [string]$Target
    )

    $protectedLines = foreach ($line in $TextLines) {
      Protect-InnoSetupTokens -Text $line
    }

    $batchBody = $protectedLines -join "`n"
    $p = "$context ||| $batchBody"

    $q = [uri]::EscapeDataString($p)
    $url = "https://translate.googleapis.com/translate_a/single?client=gtx&sl=en&tl=$Target&dt=t&q=$q"

    $resp = Invoke-RestMethod -Uri $url -TimeoutSec 30

    $out = ''
    foreach ($seg in $resp[0]) {
      $out += [string]$seg[0]
    }

    # Remove the context prefix up to the first marker from the full response.
    $out = [regex]::Replace($out, '(?s)^.*?\|\|\|\s*', '')

    $rawTranslatedLines = $out -split "`n"
    $rawTranslatedLines = foreach ($line in $rawTranslatedLines) {
      # Some languages keep the context phrase and marker on the first translated line.
      [regex]::Replace($line, '^.*?\|\|\|\s*', '')
    }

    if ($rawTranslatedLines.Count -gt $TextLines.Count) {
      # If an extra prologue line remains, keep the trailing lines that map to messages.
      $start = $rawTranslatedLines.Count - $TextLines.Count
      $rawTranslatedLines = $rawTranslatedLines[$start..($rawTranslatedLines.Count - 1)]
    }

    $translatedLines = @()
    for ($i = 0; $i -lt $TextLines.Count; $i++) {
      if (($i -lt $rawTranslatedLines.Count) -and -not [string]::IsNullOrWhiteSpace($rawTranslatedLines[$i])) {
        $translatedLines += (Restore-InnoSetupTokens -Text $rawTranslatedLines[$i])
      }
      else {
        # Keep original text when the API returns fewer/malformed lines.
        $translatedLines += (Restore-InnoSetupTokens -Text $protectedLines[$i])
      }
    }

    return $translatedLines
  }

  $code = $codes[$name]
  $path = Join-Path $dir ($name + '.isl')
  $messageKeys = @($messages.Keys | Sort-Object)

  if ($name -eq 'english') {
    $lines = @('[CustomMessages]')
    foreach ($k in $messageKeys) {
      $v = $messages[$k] -replace "`r|`n", ' '
      $lines += ('{0}={1}' -f $k, $v)
    }

    Set-Content -LiteralPath $path -Value $lines -Encoding UTF8
    Write-Host "Updated $name"
    return
  }

  $sourceValues = foreach ($k in $messageKeys) {
    [string]$messages[$k]
  }

  try {
    $translatedValues = ConvertTo-InnoSetupBatch -TextLines $sourceValues -Target $code
  }
  catch {
    Write-Warning "[$name] Batch translation failed: $($_.Exception.Message)"
    $translatedValues = $sourceValues
  }

  $lines = @('[CustomMessages]')
  for ($i = 0; $i -lt $messageKeys.Count; $i++) {
    $lines += ('{0}={1}' -f $messageKeys[$i], ($translatedValues[$i] -replace "`r|`n", ' '))
  }

  Set-Content -LiteralPath $path -Value $lines -Encoding UTF8
  Write-Host "Updated $name"
} -ThrottleLimit $languageThrottle
