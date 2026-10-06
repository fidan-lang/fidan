$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

# A failing test block must fail the real runner even when `fidan run` succeeds.
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
$probePath = Join-Path $repoRoot ("test/runner-probe-" + [Guid]::NewGuid().ToString("N") + ".fdn")
$logPath = Join-Path $repoRoot "target/test-runner-coverage.log"
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $logPath) | Out-Null
if (Test-Path -LiteralPath $probePath) {
    throw "Refusing to overwrite an existing test: $probePath"
}

try {
    [IO.File]::WriteAllText($probePath, @'
test "runner coverage" {
    assert_eq(1, 2, "intentional runner failure")
}
'@, [Text.UTF8Encoding]::new($false))
    & (Join-Path $PSScriptRoot "test.bat") *> $logPath
    $runnerExit = $LASTEXITCODE
    $output = Get-Content -LiteralPath $logPath -Raw
    if ($runnerExit -eq 0 -or
        -not $output.Contains("intentional runner failure") -or
        -not $output.Contains("0 passed, 1 failed") -or
        -not $output.Contains("$probePath test blocks failed")) {
        throw "The runner did not report the failing Fidan test block; see $logPath"
    }
} finally {
    Remove-Item -LiteralPath $probePath -Force
}

Write-Host "Test runner correctly propagates failing Fidan test blocks."
exit 0
