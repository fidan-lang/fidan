@echo off
setlocal enabledelayedexpansion

cargo build -q 2>&1
cargo test -q 2>&1

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