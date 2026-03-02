@echo off
cargo build -q 2>&1
cargo test -q 2>&1
.\target\debug\fidan run .\test\syntax.fdn
.\target\debug\fidan run .\test\examples\comprehensive.fdn
.\target\debug\fidan run .\test\examples\parallel_demo.fdn
.\target\debug\fidan run .\test\examples\spawn_method_test.fdn
.\target\debug\fidan run .\test\examples\stdlib_smoke.fdn
.\target\debug\fidan run .\test\examples\test.fdn
.\target\debug\fidan run .\test\examples\trace_demo.fdn --trace full