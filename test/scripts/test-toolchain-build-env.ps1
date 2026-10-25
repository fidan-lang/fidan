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

. (Join-Path $repoRoot "scripts/shared/cargo-artifacts.ps1")

# Pruning must preserve non-LLVM shared dependencies of official tool binaries.
$sharedProbe = Join-Path $probeRoot ("shared-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $sharedProbe | Out-Null
try {
    $expectedShared = @('libLLVM.so.23.1', 'libicui18n.so.70', 'libicuuc.so.70', 'libxml2.so.2', 'libsupport.dylib')
    foreach ($name in $expectedShared + @('libUnused.a', 'CMakeLists.txt')) {
        New-Item -ItemType File -Path (Join-Path $sharedProbe $name) | Out-Null
    }
    $actualShared = @(Get-UnixSharedLibraryNames -LibDir $sharedProbe)
    if (@(Compare-Object ($expectedShared | Sort-Object) ($actualShared | Sort-Object)).Count -ne 0) {
        throw 'Unix pruning discarded a shared dependency or retained non-runtime files'
    }
} finally {
    Remove-Item -LiteralPath $sharedProbe -Force -Recurse
}


# ldd output is data: missing/ambiguous dependencies must never produce a bundle.
$external = @(Get-LinuxLinkerDependencies -Lines @(
    ' linux-vdso.so.1 (0x00007fff00000000)',
    ' libicui18n.so.70 => /lib/x86_64-linux-gnu/libicui18n.so.70 (0x00007fff00000000)',
    ' libxml2.so.2 => /lib/x86_64-linux-gnu/libxml2.so.2 (0x00007fff00000000)',
    ' libc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x00007fff00000000)',
    ' libstdc++.so.6 => /lib/x86_64-linux-gnu/libstdc++.so.6 (0x00007fff00000000)',
    ' /lib64/ld-linux-x86-64.so.2 (0x00007fff00000000)'
))
if ($external.Count -ne 2 -or $external[0].Name -ne 'libicui18n.so.70' -or
    $external[1].Path -ne '/lib/x86_64-linux-gnu/libxml2.so.2') {
    throw 'Linker dependencies must retain non-base libraries without bundling the host C/C++ runtime'
}
foreach ($invalid in @(' libicui18n.so.70 => not found', ' libicu.so => unexpected')) {
    $rejected = $false
    try { Get-LinuxLinkerDependencies -Lines @($invalid) | Out-Null }
    catch { $rejected = $true }
    if (-not $rejected) { throw 'Invalid linker dependency was silently ignored' }
}

# Package attribution must resolve usrmerge aliases without trusting a basename.
function readlink {
    $global:LASTEXITCODE = 0
    $path = $args[-1]
    if ($path -like '*i386-linux-gnu*') { return $path }
    return '/usr/lib/x86_64-linux-gnu/libicui18n.so.70.1'
}
function dpkg-query {
    $global:LASTEXITCODE = 0
    return $script:packageRecords
}
try {
    $script:packageRecords = @(
        'libicu70:i386: /usr/lib/i386-linux-gnu/libicui18n.so.70',
        'libicu70:amd64: /usr/lib/x86_64-linux-gnu/libicui18n.so.70'
    )
    if ((Get-DebianLibraryPackage -LibraryPath '/lib/x86_64-linux-gnu/libicui18n.so.70' -LibraryName 'libicui18n.so.70') -ne 'libicu70:amd64') {
        throw 'usrmerge or multi-architecture library ownership was lost'
    }
    $script:packageRecords = @('unrelated: /usr/lib/i386-linux-gnu/libicui18n.so.70')
    $rejected = $false
    try { Get-DebianLibraryPackage -LibraryPath '/lib/x86_64-linux-gnu/libicui18n.so.70' -LibraryName 'libicui18n.so.70' | Out-Null }
    catch { $rejected = $true }
    if (-not $rejected) { throw 'A different library file was accepted as package ownership' }
} finally {
    Remove-Item Function:readlink, Function:dpkg-query
}

# Stub only external Cargo: verify the environment passed to every command,
# including cleanup when that command fails, without rebuilding LLVM four times.
function cargo {
    if ($args[0] -eq "metadata") {
        $global:LASTEXITCODE = 0
        return (@{ target_directory = (Join-Path $repoRoot "target") } | ConvertTo-Json)
    }
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
