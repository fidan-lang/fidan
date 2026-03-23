@echo off
setlocal enabledelayedexpansion
cd /d "%~dp0..\.."

cargo build -q 2>&1
if errorlevel 1 exit /b 1

cargo test -q --workspace --lib --bins --tests 2>&1
if errorlevel 1 exit /b 1

REM Run doctests only for crates that actually contain Rust doc examples.
cargo test -q --doc -p fidan-diagnostics -p fidan-typeck -p fidan-fmt -p fidan-lsp 2>&1
if errorlevel 1 exit /b 1

for /R ".\test" %%F in (*.fdn) do (
    if /I "%%~nxF"=="trace_demo.fdn" (
        echo(
        echo === Running %%F with full trace ===
        echo(
        .\target\debug\fidan run "%%F" --trace full
    ) else (
        echo(
        echo === Running %%F ===
        echo(
        .\target\debug\fidan run "%%F"
    )
)

endlocal
