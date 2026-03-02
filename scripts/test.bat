@echo off
setlocal enabledelayedexpansion

cargo build -q 2>&1
cargo test -q 2>&1

for /R ".\test" %%F in (*.fdn) do (
    if /I "%%~nxF"=="trace_demo.fdn" (
        echo Running %%F with trace
        .\target\debug\fidan run "%%F" --trace full
    ) else (
        echo Running %%F
        .\target\debug\fidan run "%%F"
    )
)

endlocal