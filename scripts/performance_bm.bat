@echo off
:: performance_bm.bat  — Fidan parallel benchmark (Windows)
::
:: Runs test/examples/parallel_benchmark.fdn in release mode and prints the
:: sequential vs parallel speedup numbers.
::
:: Usage (from workspace root):
::   scripts\performance_bm.bat

setlocal

echo === Fidan Parallel Benchmark ===
echo.

:: Build release binary first so timing reflects interpreter only.
echo [1/2] Building release binary...
cargo build --release --quiet
if errorlevel 1 (
    echo ERROR: cargo build failed.
    exit /b 1
)
echo Build OK.
echo.

:: Run the benchmark and capture wall-clock time via PowerShell.
echo [2/2] Running benchmark...
echo.
powershell -NoProfile -Command ^
    "$sw = [System.Diagnostics.Stopwatch]::StartNew();" ^
    "& '.\target\release\fidan.exe' run 'test\examples\parallel_benchmark.fdn';" ^
    "$sw.Stop();" ^
    "Write-Host '';" ^
    "Write-Host ('Total wall-clock time: ' + $sw.Elapsed.TotalSeconds.ToString('F2') + ' s')"

endlocal
