@echo off
setlocal enabledelayedexpansion
cd /d "%~dp0..\.."

cargo build -q 2>&1
if errorlevel 1 exit /b 1

cargo test -q --workspace --lib --bins --tests 2>&1
if errorlevel 1 exit /b 1

REM Let Cargo discover every workspace doctest and honor target settings.
powershell -NoProfile -ExecutionPolicy Bypass -File ".\test\scripts\run-doctests.ps1"
if errorlevel 1 exit /b 1

set "FAILED=0"

for /R ".\test" %%F in (*.fdn) do (
    if /I "%%~nxF"=="replay_demo.fdn" (
        echo(
        echo === Running %%F with replay bundle ===
        echo(
        .\target\debug\fidan run "%%F" --replay ".\test\replays\replay_demo_success.bundle"
        if errorlevel 1 (
            echo [FAIL] %%F exited unexpectedly
            set "FAILED=1"
        )
    ) else if /I "%%~nxF"=="trace_demo.fdn" (
        echo(
        echo === Running %%F with full trace ^(expected failure^) ===
        echo(
        .\target\debug\fidan run "%%F" --trace full
        if errorlevel 1 (
            echo [PASS] %%F failed as expected
        ) else (
            echo [FAIL] %%F was expected to fail
            set "FAILED=1"
        )
    ) else (
        echo(
        echo === Running %%F ===
        echo(
        .\target\debug\fidan run "%%F"
        if errorlevel 1 (
            echo [FAIL] %%F exited unexpectedly
            set "FAILED=1"
        )
    )

    REM `run` does not execute test blocks. Let the compiler discover them
    REM for every source; files without test blocks return successfully.
    .\target\debug\fidan test "%%F"
    if errorlevel 1 (
        echo [FAIL] %%F test blocks failed
        set "FAILED=1"
    )
)

if "%FAILED%"=="1" exit /b 1

endlocal
