@echo off
:: ── Fidan AOT Golden-File Test Runner (Windows) ──────────────────────────────
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
::
:: Exit code: 0 = all tests passed, 1 = one or more failures
:: ─────────────────────────────────────────────────────────────────────────────

setlocal EnableDelayedExpansion

:: ── Configuration ──────────────────────────────────────────────────────────────
set REPO_ROOT=%~dp0..\..
set GOLDEN_DIR=%REPO_ROOT%\test\golden
set EXAMPLES_DIR=%REPO_ROOT%\test\examples
set TMP_DIR=%REPO_ROOT%\target\aot_tests

:: Build profile
set BUILD_MODE=--release
set BIN_DIR=%REPO_ROOT%\target\release
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

echo.
echo ════════════════════════════════════════════════════════
echo  Fidan AOT Golden-File Test Suite
echo ════════════════════════════════════════════════════════
echo.

:: ── Step 1: Build fidan ────────────────────────────────────────────────────────
echo [build] cargo build %BUILD_MODE% -p fidan-cli ...
cargo build %BUILD_MODE% -p fidan-cli 2>nul
if errorlevel 1 (
    echo [FAIL] cargo build failed
    exit /b 1
)
echo [build] OK — %FIDAN%
echo.

:: ── Step 2: Create temp directory ─────────────────────────────────────────────
if not exist "%TMP_DIR%" mkdir "%TMP_DIR%"

:: ── Step 3: Run each golden test ───────────────────────────────────────────────
for %%F in ("%GOLDEN_DIR%\*.expected") do (
    :: Derive the source filename from the .expected filename
    set EXPECTED=%%F
    set BASENAME=%%~nF
    :: Strip trailing .fdn from the basename (%%~nF gives "test.fdn" from "test.fdn.expected")
    set SRC=%EXAMPLES_DIR%\!BASENAME!

    if not exist "!SRC!" (
        echo [SKIP] !BASENAME! — source file not found
        set /a SKIP+=1
    ) else (
        set BIN=%TMP_DIR%\!BASENAME:~0,-4!_aot.exe
        set ACTUAL=%TMP_DIR%\!BASENAME:~0,-4!_actual.txt

        :: Compile via AOT
        "%FIDAN%" build "!SRC!" -o "!BIN!" 2>nul
        if errorlevel 1 (
            echo [FAIL] !BASENAME! — AOT compile failed
            set /a FAIL+=1
        ) else (
            :: Run the binary and capture stdout
            "!BIN!" > "!ACTUAL!" 2>nul
            if errorlevel 1 (
                echo [FAIL] !BASENAME! — binary exited with error
                set /a FAIL+=1
            ) else (
                :: Diff actual vs expected (fc returns 0 for identical)
                fc /a "!ACTUAL!" "!EXPECTED!" >nul 2>&1
                if errorlevel 1 (
                    echo [FAIL] !BASENAME! — output mismatch
                    echo   Expected:
                    type "!EXPECTED!"
                    echo   Actual:
                    type "!ACTUAL!"
                    set /a FAIL+=1
                ) else (
                    echo [PASS] !BASENAME!
                    set /a PASS+=1
                )
            )
        )
    )
)

:: ── Summary ────────────────────────────────────────────────────────────────────
echo.
echo ════════════════════════════════════════════════════════
if %FAIL% equ 0 (
    echo  AOT Tests: %PASS% passed, %SKIP% skipped — ALL PASS
) else (
    echo  AOT Tests: %PASS% passed, %FAIL% FAILED, %SKIP% skipped
)
echo ════════════════════════════════════════════════════════
echo.

if %FAIL% gtr 0 exit /b 1
exit /b 0
