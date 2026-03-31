@echo off
setlocal EnableExtensions EnableDelayedExpansion

rem ============================================================
rem  cpp_benchmark_suite.bat - Multi-case Fidan vs C++ comparison
rem  Usage: test\scripts\cpp_benchmark_suite.bat [int_n] [call_n] [float_n]
rem ============================================================

set "ROOT=%~dp0..\.."
for %%I in ("%ROOT%") do set "ROOT=%%~fI"
set "OUT=%ROOT%\target\perf_suite"
set "RUN_ID=%RANDOM%%RANDOM%%RANDOM%"
set "CPP_SRC=%ROOT%\test\cpp_benchmark\performance_suite.cpp"
set "CPP_BIN=%OUT%\cpp_perf_suite_%RUN_ID%.exe"
set "FDN_SRC=%ROOT%\test\examples\performance_suite.fdn"
set "FDN_CR=%OUT%\fidan_perf_cranelift_%RUN_ID%.exe"
set "FDN_LL=%OUT%\fidan_perf_llvm_%RUN_ID%.exe"
if "%~1"=="" (
    set "RUN_ARGS=120000000 60000000 80000000"
) else (
    set "RUN_ARGS=%*"
)

echo.
echo ============================================================
echo  Fidan vs C++ Performance Suite
echo ============================================================
echo.

if not exist "%OUT%" mkdir "%OUT%"

pushd "%ROOT%" >nul || (
    echo FAIL: unable to enter repo root "%ROOT%"
    exit /b 1
)

echo [1/4] Building Fidan release binary...
cargo build --release -p fidan-cli
if errorlevel 1 (
    echo FAIL: cargo build failed
    popd >nul
    exit /b 1
)
echo       done.

echo.
echo [2/4] Compiling Fidan suite ^(Cranelift^): %FDN_SRC%
set "FDN_CR_OK=0"
"%ROOT%\target\release\fidan.exe" build --backend cranelift --target-cpu native "%FDN_SRC%" -o "%FDN_CR%"
if errorlevel 1 (
    echo FAIL: fidan build --backend cranelift returned error
) else (
    echo       Compiled to %FDN_CR%
    set "FDN_CR_OK=1"
)

echo.
echo [3/4] Compiling Fidan suite ^(LLVM^): %FDN_SRC%
set "FDN_LL_OK=0"
"%ROOT%\target\release\fidan.exe" build --backend llvm --target-cpu native "%FDN_SRC%" -o "%FDN_LL%"
if errorlevel 1 (
    echo SKIP: LLVM backend unavailable or build failed
) else (
    echo       Compiled to %FDN_LL%
    set "FDN_LL_OK=1"
)

echo.
echo [4/4] Compiling C++ suite: %CPP_SRC%
set "CPP_OK=0"
set "GXX_PATH="
set "CLANGXX_PATH="

where g++ >nul 2>&1
if not errorlevel 1 (
    g++ -O2 -march=native -std=c++17 -pthread -o "%CPP_BIN%" "%CPP_SRC%"
    if not errorlevel 1 (
        echo       Compiled with g++ -O2
        set "CPP_OK=1"
    )
)

if "!CPP_OK!"=="0" (
    for /f "usebackq delims=" %%I in (`powershell -NoProfile -Command "(Get-Command g++ -ErrorAction SilentlyContinue).Source"`) do set "GXX_PATH=%%I"
    if defined GXX_PATH (
        for %%I in ("!GXX_PATH!") do set "GXX_DIR=%%~dpI"
        set "PATH=!GXX_DIR!;!PATH!"
        "!GXX_PATH!" -O2 -march=native -std=c++17 -pthread -o "%CPP_BIN%" "%CPP_SRC%"
        if not errorlevel 1 (
            echo       Compiled with g++ -O2 via PowerShell-discovered toolchain
            set "CPP_OK=1"
        )
    )
)

if "!CPP_OK!"=="0" (
    where clang++ >nul 2>&1
    if not errorlevel 1 (
        clang++ -O2 -march=native -std=c++17 -pthread -o "%CPP_BIN%" "%CPP_SRC%"
        if not errorlevel 1 (
            echo       Compiled with clang++ -O2
            set "CPP_OK=1"
        )
    )
)

if "!CPP_OK!"=="0" (
    for /f "usebackq delims=" %%I in (`powershell -NoProfile -Command "(Get-Command clang++ -ErrorAction SilentlyContinue).Source"`) do set "CLANGXX_PATH=%%I"
    if defined CLANGXX_PATH (
        for %%I in ("!CLANGXX_PATH!") do set "CLANGXX_DIR=%%~dpI"
        set "PATH=!CLANGXX_DIR!;!PATH!"
        "!CLANGXX_PATH!" -O2 -march=native -std=c++17 -pthread -o "%CPP_BIN%" "%CPP_SRC%"
        if not errorlevel 1 (
            echo       Compiled with clang++ -O2 via PowerShell-discovered toolchain
            set "CPP_OK=1"
        )
    )
)

if "!CPP_OK!"=="0" (
    where cl >nul 2>&1
    if not errorlevel 1 (
        cl /nologo /O2 /std:c++17 /EHsc "%CPP_SRC%" /Fe:"%CPP_BIN%"
        if not errorlevel 1 (
            echo       Compiled with cl /O2
            set "CPP_OK=1"
        )
    )
)

if "!CPP_OK!"=="0" (
    echo SKIP: no usable C++ compiler found ^(g++ on PATH or cl in a Developer Command Prompt^)
)

echo.
echo ============================================================
echo  Results
echo ============================================================
if not "%RUN_ARGS%"=="" echo  args: %RUN_ARGS%

if "!FDN_CR_OK!"=="1" (
    echo.
    echo [Fidan AOT - Cranelift]
    "%FDN_CR%" %RUN_ARGS%
)

if "!FDN_LL_OK!"=="1" (
    echo.
    echo [Fidan AOT - LLVM]
    "%FDN_LL%" %RUN_ARGS%
)

if "!CPP_OK!"=="1" (
    echo.
    echo [C++ -O2]
    "%CPP_BIN%" %RUN_ARGS%
)

echo.
echo ============================================================
echo  Done. Compare the BENCH / SPEEDUP lines above.
echo ============================================================
echo.

popd >nul
endlocal
exit /b 0
