$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
$probeRoot = Join-Path $repoRoot "target/helper-build-env"
New-Item -ItemType Directory -Force -Path $probeRoot | Out-Null
$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $repoRoot "scripts/package-toolchain.ps1"), [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count) { throw $parseErrors }
# Load the actual packaging functions without executing package/download setup.
foreach ($function in $ast.FindAll({ param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] }, $false)) {
    . ([scriptblock]::Create($function.Extent.Text))
}

# Stub only external Cargo: verify the environment passed to every command,
# including cleanup when that command fails, without rebuilding LLVM four times.
function cargo {
    $script:cargoLto.Add($env:CARGO_PROFILE_RELEASE_LTO)
    $global:LASTEXITCODE = if ($script:failCargo) { 17 } else { 0 }
}

$hadLto = Test-Path Env:CARGO_PROFILE_RELEASE_LTO
$previousLto = $env:CARGO_PROFILE_RELEASE_LTO
$previousExit = $global:LASTEXITCODE
Push-Location $repoRoot
try {
    foreach ($initialLto in @($null, "thin")) {
        foreach ($failCargo in @($false, $true)) {
            if ($null -eq $initialLto) { Remove-Item Env:CARGO_PROFILE_RELEASE_LTO -ErrorAction SilentlyContinue }
            else { $env:CARGO_PROFILE_RELEASE_LTO = $initialLto }
            $script:cargoLto = [Collections.Generic.List[string]]::new()
            $script:failCargo = $failCargo
            $failed = $false
            try {
                Invoke-HelperBuild -Kind llvm -HelperPackage fidan-llvm-helper `
                    -HelperBinary ("env-probe-" + [Guid]::NewGuid().ToString("N") + ".exe") `
                    -LlvmRoot $probeRoot -HelperCargoFeatures llvm-toolchain-23 `
                    -LlvmSysPrefixEnvVar LLVM_SYS_231_PREFIX -HelperAdditionalLibPaths @()
            } catch {
                if ($_.Exception.Message -notlike "Failed to build*") { throw }
                $failed = $true
            }
            $expectedCalls = if ($failCargo) { 1 } else { 3 }
            if ($failed -ne $failCargo -or $script:cargoLto.Count -ne $expectedCalls -or
                @($script:cargoLto | Where-Object { $_ -ne "false" }).Count -gt 0) {
                throw "Helper build/test/clippy did not consistently disable Rust LTO or propagate failure"
            }
            if ((Test-Path Env:CARGO_PROFILE_RELEASE_LTO) -ne ($null -ne $initialLto) -or
                $env:CARGO_PROFILE_RELEASE_LTO -ne $initialLto) {
                throw "Helper packaging did not restore the original LTO environment"
            }
        }
    }
} finally {
    if ($hadLto) { $env:CARGO_PROFILE_RELEASE_LTO = $previousLto }
    else { Remove-Item Env:CARGO_PROFILE_RELEASE_LTO -ErrorAction SilentlyContinue }
    $global:LASTEXITCODE = $previousExit
    Pop-Location
}
Write-Host "Helper Rust-LTO environment: 4 cases passed."
exit 0
