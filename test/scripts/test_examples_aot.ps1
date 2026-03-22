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
        [string[]]$StdinLines
    )

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $ExePath
    foreach ($argument in $Arguments) {
        $null = $startInfo.ArgumentList.Add($argument)
    }
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.RedirectStandardInput = $null -ne $StdinLines

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $null = $process.Start()

    if ($null -ne $StdinLines) {
        foreach ($line in $StdinLines) {
            $process.StandardInput.WriteLine($line)
        }
        $process.StandardInput.Close()
    }

    if (-not $process.WaitForExit($TimeoutMs)) {
        try {
            $process.Kill($true)
        } catch {
        }
        $stdout = $process.StandardOutput.ReadToEnd()
        $stderr = $process.StandardError.ReadToEnd()
        Set-Content -Path $StdoutPath -Value $stdout -NoNewline
        Set-Content -Path $StderrPath -Value $stderr -NoNewline
        return 124
    }

    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    Set-Content -Path $StdoutPath -Value $stdout -NoNewline
    Set-Content -Path $StderrPath -Value $stderr -NoNewline
    return $process.ExitCode
}

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\\..")).Path
$buildMode = if ($Release) { "--release" } else { "" }
$binDir = Join-Path $repoRoot ($(if ($Release) { "target\\release" } else { "target\\debug" }))
$fidan = Join-Path $binDir "fidan.exe"
$outDir = Join-Path $repoRoot "target\\aot_examples"

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
    $pass = 0
    $fail = 0
    $skip = 0

    foreach ($file in $examples) {
        $rel = $file.FullName.Substring($repoRoot.Length + 1)
        $baseName = $file.Name

        if ($Case -and $baseName -ne $Case) {
            continue
        }

        $stem = [IO.Path]::GetFileNameWithoutExtension($baseName)
        $bin = Join-Path $outDir ($stem + "_aot.exe")
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
            -StdinLines $null

        if ($compileExit -eq 124) {
            Write-Host "[FAIL] $rel - compile timed out"
            $fail += 1
            continue
        }

        if ($compileExit -ne 0) {
            Write-Host "[FAIL] $rel - compile failed"
            $compileStdout = Read-TextFile $compileOut
            $compileText = Read-TextFile $compileErr
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

        switch ($baseName) {
            "parallel_benchmark.fdn" {
                $timeoutMs = $BenchmarkProbeSeconds * 1000
                $allowTimeout = $true
            }
            "replay_demo.fdn" {
                $stdinLines = @("6", "3")
            }
        }

        $exitCode = Invoke-ProgramWithTimeout `
            -ExePath $bin `
            -Arguments @() `
            -WorkingDirectory $repoRoot `
            -StdoutPath $stdout `
            -StderrPath $stderr `
            -TimeoutMs $timeoutMs `
            -StdinLines $stdinLines

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
            Write-Host "[FAIL] $rel - exited with code $exitCode"
            if ($stderrText) {
                Write-Host $stderrText
            }
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
