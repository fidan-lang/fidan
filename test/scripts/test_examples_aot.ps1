param(
    [ValidateSet("auto", "cranelift", "llvm")]
    [string]$Backend = "llvm",
    [ValidateSet("off", "full")]
    [string]$Lto = "off",
    [switch]$Release,
    [string]$Case,
    [string]$FidanHome,
    [int]$DefaultTimeoutSeconds = 10,
    [int]$BenchmarkProbeSeconds = 5
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

function Read-TextFile {
    param([string]$Path)
    if (-not (Test-Path $Path)) {
        return ""
    }

    $text = Get-Content -Path $Path -Raw
    if ($null -eq $text) {
        return ""
    }
    return [string]$text
}

function Invoke-ProgramWithTimeout {
    param(
        [string]$ExePath,
        [string[]]$Arguments,
        [string]$WorkingDirectory,
        [string]$StdoutPath,
        [string]$StderrPath,
        [int]$TimeoutMs,
        [string[]]$StdinLines,
        [string]$Label = "process"
    )

    function Format-ArgumentForProcess {
        param([string]$Value)
        if ($null -eq $Value) {
            return '""'
        }
        if ($Value.Length -eq 0) {
            return '""'
        }
        if ($Value -notmatch '[\s"]') {
            return $Value
        }

        $escaped = $Value -replace '(\\*)"', '$1$1\"'
        $escaped = $escaped -replace '(\\+)$', '$1$1'
        return '"' + $escaped + '"'
    }

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $ExePath
    $startInfo.Arguments = (($Arguments | ForEach-Object { Format-ArgumentForProcess $_ }) -join ' ')
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.RedirectStandardInput = $null -ne $StdinLines

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $null = $process.Start()
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()

    if ($null -ne $StdinLines) {
        foreach ($line in $StdinLines) {
            $process.StandardInput.WriteLine($line)
        }
        $process.StandardInput.Close()
    }

    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $nextHeartbeatMs = 15000
    $timedOut = $false
    while (-not $process.WaitForExit(200)) {
        if ($stopwatch.ElapsedMilliseconds -ge $nextHeartbeatMs) {
            $elapsedSeconds = [Math]::Floor($stopwatch.ElapsedMilliseconds / 1000)
            Write-Host "[running] $Label - ${elapsedSeconds}s elapsed"
            $nextHeartbeatMs += 15000
        }
        if ($stopwatch.ElapsedMilliseconds -ge $TimeoutMs) {
            $timedOut = $true
            try {
                $process.Kill($true)
            } catch {
            }
            break
        }
    }

    $process.WaitForExit()
    $stdoutTask.Wait()
    $stderrTask.Wait()
    $stdout = $stdoutTask.Result
    $stderr = $stderrTask.Result
    Set-Content -Path $StdoutPath -Value $stdout -NoNewline
    Set-Content -Path $StderrPath -Value $stderr -NoNewline
    if ($timedOut) {
        return 124
    }
    return $process.ExitCode
}

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\\..")).Path
$buildMode = if ($Release) { "--release" } else { "" }
$binDir = Join-Path $repoRoot ($(if ($Release) { "target\\release" } else { "target\\debug" }))
$fidanName = if ($IsWindows) { "fidan.exe" } else { "fidan" }
$fidan = Join-Path $binDir $fidanName
$outDir = Join-Path $repoRoot "target\\aot_examples"
$binarySuffix = if ($IsWindows) { ".exe" } else { "" }

if ($FidanHome) {
    $env:FIDAN_HOME = (Resolve-Path $FidanHome).Path
}

Write-Host ""
Write-Host "========================================================"
Write-Host " Fidan AOT Example Sweep"
Write-Host "========================================================"
Write-Host " backend: $Backend"
Write-Host " lto: $Lto"
if ($Case) { Write-Host " case: $Case" }
if ($env:FIDAN_HOME) { Write-Host " FIDAN_HOME: $env:FIDAN_HOME" }
Write-Host ""

Push-Location $repoRoot
try {
    Write-Host "[build] cargo build $buildMode -p fidan-cli ..."
    if ($Release) {
        cargo build --release -p fidan-cli
    } else {
        cargo build -p fidan-cli
    }
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed"
    }

    New-Item -ItemType Directory -Force -Path $outDir | Out-Null

    $examples = Get-ChildItem (Join-Path $repoRoot "test") -Recurse -Filter *.fdn | Sort-Object FullName
    if ($examples.Count -eq 0) {
        throw "No .fdn examples were found under '$repoRoot\\test'"
    }
    $pass = 0
    $fail = 0
    $skip = 0
    $matched = 0

    foreach ($file in $examples) {
        $rel = $file.FullName.Substring($repoRoot.Length + 1)
        $baseName = $file.Name

        if ($Case -and $baseName -ne $Case) {
            continue
        }

        $matched += 1

        $stem = [IO.Path]::GetFileNameWithoutExtension($baseName)
        $bin = Join-Path $outDir ($stem + "_aot" + $binarySuffix)
        $stdout = Join-Path $outDir ($stem + "_stdout.txt")
        $stderr = Join-Path $outDir ($stem + "_stderr.txt")
        $compileOut = Join-Path $outDir ($stem + "_compile.out.txt")
        $compileErr = Join-Path $outDir ($stem + "_compile.err.txt")

        Write-Host "=== $rel ==="

        $compileExit = Invoke-ProgramWithTimeout `
            -ExePath $fidan `
            -Arguments @("build", "--backend", $Backend, "--lto", $Lto, $file.FullName, "-o", $bin) `
            -WorkingDirectory $repoRoot `
            -StdoutPath $compileOut `
            -StderrPath $compileErr `
            -TimeoutMs 600000 `
            -StdinLines $null `
            -Label "compile $rel"

        if ($compileExit -eq 124) {
            Write-Host "[FAIL] $rel - compile timed out"
            $fail += 1
            continue
        }

        if ($compileExit -ne 0) {
            Write-Host "[FAIL] $rel - compile failed"
            $compileStdout = Read-TextFile $compileOut
            $compileText = Read-TextFile $compileErr
            if ($Backend -eq "llvm" -and $compileText -match "required LLVM backend feature") {
                Write-Host "[SKIP] $rel - LLVM helper install lacks the required backend feature set"
                if ($compileStdout) {
                    Write-Host $compileStdout
                }
                if ($compileText) {
                    Write-Host $compileText
                }
                $skip += 1
                continue
            }
            if ($compileStdout) {
                Write-Host $compileStdout
            }
            if ($compileText) {
                Write-Host $compileText
            }
            $fail += 1
            continue
        }

        $timeoutMs = $DefaultTimeoutSeconds * 1000
        $stdinLines = $null
        $allowTimeout = $false
        $expectFailure = $false

        switch ($baseName) {
            "parallel_benchmark.fdn" {
                $timeoutMs = $BenchmarkProbeSeconds * 1000
                $allowTimeout = $true
            }
            "release_mega_1_0.fdn" {
                $timeoutMs = [Math]::Max($timeoutMs, 30000)
            }
            "replay_demo.fdn" {
                $stdinLines = @("6", "3")
            }
            "trace_demo.fdn" {
                $expectFailure = $true
            }
        }

        $exitCode = Invoke-ProgramWithTimeout `
            -ExePath $bin `
            -Arguments @() `
            -WorkingDirectory $repoRoot `
            -StdoutPath $stdout `
            -StderrPath $stderr `
            -TimeoutMs $timeoutMs `
            -StdinLines $stdinLines `
            -Label "run $rel"

        if ($allowTimeout -and $exitCode -eq 124) {
            Write-Host "[PASS] $rel - long-running benchmark reached timeout window"
            $pass += 1
            continue
        }

        if ($exitCode -eq 124) {
            Write-Host "[FAIL] $rel - timed out after $DefaultTimeoutSeconds s"
            $fail += 1
            continue
        }

        if ($exitCode -ne 0) {
            $stderrText = Read-TextFile $stderr
            if ($expectFailure) {
                Write-Host "[PASS] $rel - failed as expected"
                if ($stderrText) {
                    Write-Host $stderrText
                }
                $pass += 1
                continue
            } else {
                Write-Host "[FAIL] $rel - exited with code $exitCode"
                if ($stderrText) {
                    Write-Host $stderrText
                }
                $fail += 1
                continue
            }
        }

        if ($expectFailure) {
            Write-Host "[FAIL] $rel - was expected to fail"
            $fail += 1
            continue
        }

        $stderrText = Read-TextFile $stderr
        if ($stderrText.Trim().Length -gt 0) {
            Write-Host "[FAIL] $rel - wrote to stderr"
            Write-Host $stderrText
            $fail += 1
            continue
        }

        Write-Host "[PASS] $rel"
        $pass += 1
    }

    if ($Case -and $matched -eq 0) {
        throw "No example matched case '$Case'"
    }
    if ($matched -eq 0) {
        throw "Example sweep matched zero test cases"
    }

    Write-Host ""
    Write-Host "========================================================"
    if ($fail -eq 0) {
        Write-Host " Example Sweep: $pass passed, $skip skipped - ALL PASS"
    } else {
        Write-Host " Example Sweep: $pass passed, $fail failed, $skip skipped"
    }
    Write-Host "========================================================"
    Write-Host ""

    if ($fail -gt 0) {
        exit 1
    }
} finally {
    Pop-Location
}
