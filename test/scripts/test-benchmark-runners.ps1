param([string]$RunnerSourceDirectory = $PSScriptRoot)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false
if (-not $IsWindows) { throw "Batch benchmark regressions require Windows." }
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
$compiler = if ($env:FIDAN_BIN) { $env:FIDAN_BIN } else { Join-Path $repoRoot "target/debug/fidan.exe" }
if (-not (Test-Path -LiteralPath $compiler -PathType Leaf)) { throw "Build Fidan before testing benchmark runners." }
$scratch = Join-Path $repoRoot ("target/benchmark-runner-test-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path "$scratch/test/scripts", "$scratch/test/examples", "$scratch/target/release" | Out-Null
Copy-Item -LiteralPath $compiler -Destination "$scratch/target/release/fidan.exe"
# Real dependency-free Cargo build, without replacing the copied actual CLI.
[IO.File]::WriteAllText("$scratch/Cargo.toml", "[package]`nname = `"fidan-cli`"`nversion = `"0.0.0`"`nedition = `"2024`"`n[lib]`npath = `"probe.rs`"`n[workspace]`n")
[IO.File]::WriteAllText("$scratch/probe.rs", "")
[IO.File]::WriteAllText("$scratch/test/examples/parallel_benchmark.fdn", "var broken = )`n")
[IO.File]::WriteAllText("$scratch/test/examples/performance_suite.fdn", "var broken = )`n")
$previousTarget = $env:CARGO_TARGET_DIR
$previousExit = $global:LASTEXITCODE
$failures = @()
$checks = 0
$gitBash = Join-Path $env:ProgramFiles "Git/bin/bash.exe"
try {
    $env:CARGO_TARGET_DIR = "$scratch/target"
    & cargo generate-lockfile --offline --manifest-path "$scratch/Cargo.toml"
    if ($LASTEXITCODE -ne 0) { throw "Could not prepare isolated Cargo fixture" }
    $names = @("cpp_benchmark.bat", "cpp_benchmark_suite.bat")
    if (Test-Path -LiteralPath $gitBash) { $names += @("cpp_benchmark.sh", "cpp_benchmark_suite.sh") }
    else { Write-Host "SKIP Bash benchmark controls: Git Bash is not installed" }
    foreach ($name in $names) {
        Copy-Item -LiteralPath (Join-Path $RunnerSourceDirectory $name) -Destination "$scratch/test/scripts/$name"
        $output = "$scratch/$name.log"
        if ($name.EndsWith(".bat")) {
            & cmd.exe /d /c "`"$scratch/test/scripts/$name`"" *> $output
        } else {
            & $gitBash "$scratch/test/scripts/$name" *> $output
        }
        $status = $LASTEXITCODE
        $checks++
        $diagnostics = Get-Content -LiteralPath $output -Raw
        if ($status -eq 0 -or -not $diagnostics.Contains("error[E0000]")) {
            $failures += "$name did not propagate the intended real frontend failure; see $output"
        }
    }
} finally {
    if ($null -eq $previousTarget) { Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue }
    else { $env:CARGO_TARGET_DIR = $previousTarget }
    $global:LASTEXITCODE = $previousExit
}
# Keep native artifacts inside ignored target/: Windows can briefly retain an
# executed image; cleanup timing must not turn semantic checks into false failures.
if ($failures.Count) { throw ($failures -join "`n") }
Write-Host "Benchmark runner failure propagation: $checks cases passed."
