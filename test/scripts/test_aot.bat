@echo off
:: Fidan AOT Golden-File Test Runner (Windows)
::
:: Compiles each .fdn file that has a corresponding test/golden/*.expected file
:: through the Cranelift AOT backend, runs the resulting binary, and diffs the
:: output against the golden file.
::
:: Usage:
::   test\scripts\test_aot.bat [--release]
::
:: Options:
::   --release   Build in release mode (default: debug)
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
for %%A in (%*) do (
    if "%%A"=="--release" (
        set BUILD_MODE=--release
        set BIN_DIR=%REPO_ROOT%\target\release
    )
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

    if not exist "!SRC!" (
        echo [SKIP] !BASENAME! - source file not found
        set /a SKIP+=1
    ) else (
        set BIN=%TMP_DIR%\!BASENAME:~0,-4!_aot.exe
        set ACTUAL=%TMP_DIR%\!BASENAME:~0,-4!_actual.txt
        set STDERR=%TMP_DIR%\!BASENAME:~0,-4!_actual.err.txt

        "%FIDAN%" build "!SRC!" -o "!BIN!" 2>nul
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
