@echo off
setlocal enabledelayedexpansion

:: ============================================================
::  cpp_benchmark.bat — Build and compare C++ vs Fidan AOT
::  Usage:  test\scripts\cpp_benchmark.bat
:: ============================================================

set ROOT=%~dp0..\..
set OUT=%ROOT%\target\aot_tests
set CPP_SRC=%ROOT%\test\cpp_benchmark\benchmark.cpp
set CPP_BIN=%OUT%\cpp_bench.exe
set FDN_SRC=%ROOT%\test\examples\parallel_benchmark.fdn
set FDN_BIN=%OUT%\fidan_bench.exe

echo.
echo ============================================================
echo  Fidan vs C++ Benchmark Comparison
echo ============================================================
echo.

:: ── Create output directory ───────────────────────────────────
if not exist "%OUT%" mkdir "%OUT%"

:: ── Build Fidan release binary ────────────────────────────────
echo [1/3] Building Fidan release binary...
cd /d "%ROOT%"
cargo build --release -p fidan-cli 2>nul
if errorlevel 1 (
    echo FAIL: cargo build failed
    exit /b 1
)
echo       done.

:: ── Build Fidan parallel_benchmark ───────────────────────────
echo.
echo [2/3] Compiling Fidan benchmark: %FDN_SRC%
if not exist "%FDN_SRC%" (
    echo SKIP: %FDN_SRC% not found
    goto :cpp_build
)
"%ROOT%\target\release\fidan.exe" build "%FDN_SRC%" -o "%FDN_BIN%" 2>nul
if errorlevel 1 (
    echo FAIL: fidan build returned error
    set FDN_OK=0
) else (
    echo       Compiled to %FDN_BIN%
    set FDN_OK=1
)

:cpp_build
:: ── Build C++ benchmark ───────────────────────────────────────
echo.
echo [3/3] Compiling C++ benchmark: %CPP_SRC%
set CPP_OK=0

where g++ >nul 2>&1
if not errorlevel 1 (
    g++ -O2 -std=c++17 -pthread -o "%CPP_BIN%" "%CPP_SRC%"
    if not errorlevel 1 (
        echo       Compiled with g++ -O2
        set CPP_OK=1
    )
)

if "!CPP_OK!"=="0" (
    where cl >nul 2>&1
    if not errorlevel 1 (
        cl /O2 /std:c++17 /EHsc "%CPP_SRC%" /Fe:"%CPP_BIN%" >nul 2>&1
        if not errorlevel 1 (
            echo       Compiled with cl /O2
            set CPP_OK=1
        )
    )
)

if "!CPP_OK!"=="0" (
    echo SKIP: no C++ compiler found ^(g++ or cl^), skipping C++ benchmark
)

:: ── Run benchmarks ────────────────────────────────────────────
echo.
echo ============================================================
echo  Results
echo ============================================================

if "!FDN_OK!"=="1" (
    echo.
    echo [Fidan AOT]
    "%FDN_BIN%"
)

if "!CPP_OK!"=="1" (
    echo.
    echo [C++ -O2]
    "%CPP_BIN%"
)

echo.
echo ============================================================
echo  Done. Compare the timing lines above.
echo ============================================================
echo.

endlocal
exit /b 0
