@echo off
:: Fidan AOT Golden-File Test Runner (Windows)
::
:: Compiles each .fdn file that has a corresponding test/golden/*.expected file
:: through an AOT backend, runs the resulting binary, and diffs the
:: output against the golden file.
::
:: Usage:
::   test\scripts\test_aot.bat [--release] [--backend <auto|cranelift|llvm>] [--lto <off|full>] [--case <file.fdn>]
::
:: Options:
::   --release            Build in release mode (default: debug)
::   --backend <backend>  AOT backend to test (default: cranelift)
::   --lto <mode>         LLVM link-time optimization mode (default: off)
::   --case <file.fdn>    Run only one golden-file case
::
:: Dependencies:
::   - Rust / Cargo installed
::   - Windows linker (MSVC link.exe or lld-link) on PATH
::   - Windows Script Host (cscript.exe)
::
:: Exit code: 0 = all tests passed, 1 = one or more failures

setlocal EnableDelayedExpansion

:: Configuration
for %%I in ("%~dp0..\..") do set REPO_ROOT=%%~fI
set GOLDEN_DIR=%REPO_ROOT%\test\golden
set EXAMPLES_DIR=%REPO_ROOT%\test\examples
set TMP_DIR=%REPO_ROOT%\target\aot_tests
set TIMEOUT_HELPER=%REPO_ROOT%\test\scripts\run_with_timeout.js
set CSCRIPT=%SystemRoot%\System32\cscript.exe

:: Build profile
set BUILD_MODE=
set BIN_DIR=%REPO_ROOT%\target\debug
set AOT_BACKEND=cranelift
set AOT_LTO=off
set AOT_CASE=

set ARG_RELEASE=
set ARG_BACKEND_NEXT=
set ARG_LTO_NEXT=
set ARG_CASE_NEXT=
for %%A in (%*) do (
    if defined ARG_BACKEND_NEXT (
        set AOT_BACKEND=%%~A
        set ARG_BACKEND_NEXT=
    ) else if defined ARG_LTO_NEXT (
        set AOT_LTO=%%~A
        set ARG_LTO_NEXT=
    ) else if defined ARG_CASE_NEXT (
        set AOT_CASE=%%~A
        set ARG_CASE_NEXT=
    ) else if "%%A"=="--release" (
        set BUILD_MODE=--release
        set BIN_DIR=%REPO_ROOT%\target\release
    ) else if "%%A"=="--backend" (
        set ARG_BACKEND_NEXT=1
    ) else if "%%A"=="--lto" (
        set ARG_LTO_NEXT=1
    ) else if "%%A"=="--case" (
        set ARG_CASE_NEXT=1
    )
)

if /I not "%AOT_BACKEND%"=="auto" if /I not "%AOT_BACKEND%"=="cranelift" if /I not "%AOT_BACKEND%"=="llvm" (
    echo [FAIL] unsupported backend "%AOT_BACKEND%" ^(expected: auto, cranelift, llvm^)
    exit /b 1
)
if /I not "%AOT_LTO%"=="off" if /I not "%AOT_LTO%"=="full" (
    echo [FAIL] unsupported lto mode "%AOT_LTO%" ^(expected: off, full^)
    exit /b 1
)

set FIDAN=%BIN_DIR%\fidan.exe
set PASS=0
set FAIL=0
set SKIP=0
if not defined AOT_RUN_TIMEOUT_SECS set AOT_RUN_TIMEOUT_SECS=5

echo.
echo ========================================================
echo  Fidan AOT Golden-File Test Suite
echo ========================================================
echo  backend: %AOT_BACKEND%
echo  lto: %AOT_LTO%
if defined AOT_CASE echo  case: %AOT_CASE%
echo.

:: Step 1: Build fidan
echo [build] cargo build %BUILD_MODE% -p fidan-cli ...
cargo build %BUILD_MODE% -p fidan-cli 2>nul
if errorlevel 1 (
    echo [FAIL] cargo build failed
    exit /b 1
)
echo [build] OK - %FIDAN%
echo.

:: Step 2: Create temp directory
if not exist "%TMP_DIR%" mkdir "%TMP_DIR%"
if not exist "%TIMEOUT_HELPER%" (
    echo [FAIL] timeout helper not found: %TIMEOUT_HELPER%
    exit /b 1
)

:: Step 3: Run each golden test
for %%F in ("%GOLDEN_DIR%\*.expected") do (
    set EXPECTED=%%F
    set BASENAME=%%~nF
    set SRC=%EXAMPLES_DIR%\!BASENAME!
    set RUN_CASE=1

    if defined AOT_CASE if /I not "!BASENAME!"=="%AOT_CASE%" set RUN_CASE=

    if defined RUN_CASE (
        if not exist "!SRC!" (
            echo [SKIP] !BASENAME! - source file not found
            set /a SKIP+=1
        ) else (
            set BIN=%TMP_DIR%\!BASENAME:~0,-4!_aot.exe
            set ACTUAL=%TMP_DIR%\!BASENAME:~0,-4!_actual.txt
            set STDERR=%TMP_DIR%\!BASENAME:~0,-4!_actual.err.txt

            "%FIDAN%" build --backend "%AOT_BACKEND%" --lto "%AOT_LTO%" "!SRC!" -o "!BIN!" 2>nul
            if errorlevel 1 (
                echo [FAIL] !BASENAME! - AOT compile failed
                set /a FAIL+=1
            ) else (
                "%CSCRIPT%" //nologo "%TIMEOUT_HELPER%" %AOT_RUN_TIMEOUT_SECS%000 "%REPO_ROOT%" "!ACTUAL!" "!STDERR!" "!BIN!" >nul 2>&1
                if errorlevel 124 (
                    echo [FAIL] !BASENAME! - binary timed out after %AOT_RUN_TIMEOUT_SECS%s
                    set /a FAIL+=1
                ) else if errorlevel 1 (
                    echo [FAIL] !BASENAME! - binary exited with error
                    set /a FAIL+=1
                ) else (
                    fc /a "!ACTUAL!" "!EXPECTED!" >nul 2>&1
                    if errorlevel 1 (
                        echo [FAIL] !BASENAME! - output mismatch
                        echo   Expected:
                        type "!EXPECTED!"
                        echo   Actual:
                        type "!ACTUAL!"
                        if exist "!STDERR!" type "!STDERR!"
                        set /a FAIL+=1
                    ) else (
                        echo [PASS] !BASENAME!
                        set /a PASS+=1
                    )
                )
            )
        )
    )
)

:: Summary
echo.
echo ========================================================
if %FAIL% equ 0 (
    echo  AOT Tests: %PASS% passed, %SKIP% skipped - ALL PASS
) else (
    echo  AOT Tests: %PASS% passed, %FAIL% FAILED, %SKIP% skipped
)
echo ========================================================
echo.

if %FAIL% gtr 0 exit /b 1
exit /b 0
