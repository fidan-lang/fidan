#!/usr/bin/env bash
set -euo pipefail

BACKEND="llvm"
LTO="off"
RELEASE=0
CASE=""
FIDAN_HOME_OVERRIDE=""
DEFAULT_TIMEOUT_SECONDS=10
BENCHMARK_PROBE_SECONDS=5

while [ "$#" -gt 0 ]; do
    case "$1" in
        --backend)
            shift
            BACKEND="${1:?missing value for --backend}"
            shift
            ;;
        --lto)
            shift
            LTO="${1:?missing value for --lto}"
            shift
            ;;
        --release)
            RELEASE=1
            shift
            ;;
        --case)
            shift
            CASE="${1:?missing value for --case}"
            shift
            ;;
        --fidan-home)
            shift
            FIDAN_HOME_OVERRIDE="${1:?missing value for --fidan-home}"
            shift
            ;;
        --default-timeout-seconds)
            shift
            DEFAULT_TIMEOUT_SECONDS="${1:?missing value for --default-timeout-seconds}"
            shift
            ;;
        --benchmark-probe-seconds)
            shift
            BENCHMARK_PROBE_SECONDS="${1:?missing value for --benchmark-probe-seconds}"
            shift
            ;;
        *)
            echo "[FAIL] unknown option: $1" >&2
            exit 1
            ;;
    esac
done

case "$BACKEND" in
    auto|cranelift|llvm) ;;
    *)
        echo "[FAIL] unsupported backend '$BACKEND' (expected: auto, cranelift, llvm)" >&2
        exit 1
        ;;
esac

case "$LTO" in
    off|full) ;;
    *)
        echo "[FAIL] unsupported lto mode '$LTO' (expected: off, full)" >&2
        exit 1
        ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILD_MODE=""
BIN_DIR="$REPO_ROOT/target/debug"
if [ "$RELEASE" -eq 1 ]; then
    BUILD_MODE="--release"
    BIN_DIR="$REPO_ROOT/target/release"
fi
FIDAN="$BIN_DIR/fidan"
OUT_DIR="$REPO_ROOT/target/aot_examples"

if [ -n "$FIDAN_HOME_OVERRIDE" ]; then
    export FIDAN_HOME="$(cd "$FIDAN_HOME_OVERRIDE" && pwd)"
fi

run_with_timeout() {
    local timeout_secs="$1"
    local stdout_path="$2"
    local stderr_path="$3"
    shift 3

    python3 - "$timeout_secs" "$stdout_path" "$stderr_path" "$@" <<'PY'
import pathlib
import subprocess
import sys

timeout = int(sys.argv[1])
stdout_path = pathlib.Path(sys.argv[2])
stderr_path = pathlib.Path(sys.argv[3])
cmd = sys.argv[4:]

def normalize_text(value):
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return value

stdin_data = None
if cmd and cmd[0] == "--stdin-lines":
    stdin_data = cmd[1].replace("\\n", "\n") + "\n"
    cmd = cmd[2:]

try:
    completed = subprocess.run(
        cmd,
        input=stdin_data,
        text=True,
        capture_output=True,
        timeout=timeout,
        check=False,
    )
    stdout_path.write_text(normalize_text(completed.stdout))
    stderr_path.write_text(normalize_text(completed.stderr))
    sys.exit(completed.returncode)
except subprocess.TimeoutExpired as exc:
    stdout_path.write_text(normalize_text(exc.stdout))
    stderr_path.write_text(normalize_text(exc.stderr))
    sys.exit(124)
PY
}

echo ""
echo "========================================================"
echo " Fidan AOT Example Sweep"
echo "========================================================"
echo " backend: $BACKEND"
echo " lto: $LTO"
[ -n "$CASE" ] && echo " case: $CASE"
[ -n "${FIDAN_HOME:-}" ] && echo " FIDAN_HOME: $FIDAN_HOME"
echo ""

cd "$REPO_ROOT"
echo "[build] cargo build $BUILD_MODE -p fidan-cli ..."
if [ -n "$BUILD_MODE" ]; then
    cargo build --release -p fidan-cli
else
    cargo build -p fidan-cli
fi

mkdir -p "$OUT_DIR"
PASS=0
FAIL=0
SKIP=0
MATCHED=0

while IFS= read -r file; do
    [ -z "$file" ] && continue
    rel="${file#$REPO_ROOT/}"
    base_name="$(basename "$file")"

    if [ -n "$CASE" ] && [ "$base_name" != "$CASE" ]; then
        continue
    fi

    MATCHED=$((MATCHED + 1))

    stem="${base_name%.fdn}"
    bin="$OUT_DIR/${stem}_aot"
    stdout="$OUT_DIR/${stem}_stdout.txt"
    stderr="$OUT_DIR/${stem}_stderr.txt"
    compile_out="$OUT_DIR/${stem}_compile.out.txt"
    compile_err="$OUT_DIR/${stem}_compile.err.txt"

    echo "=== $rel ==="

    set +e
    run_with_timeout 600 "$compile_out" "$compile_err" \
        "$FIDAN" build --backend "$BACKEND" --lto "$LTO" "$file" -o "$bin"
    compile_exit=$?
    set -e
    if [ "$compile_exit" -eq 124 ]; then
        echo "[FAIL] $rel - compile timed out"
        FAIL=$((FAIL + 1))
        continue
    fi
    if [ "$compile_exit" -ne 0 ]; then
        echo "[FAIL] $rel - compile failed"
        [ -f "$compile_out" ] && cat "$compile_out"
        [ -f "$compile_err" ] && cat "$compile_err"
        FAIL=$((FAIL + 1))
        continue
    fi

    timeout_secs="$DEFAULT_TIMEOUT_SECONDS"
    allow_timeout=0
    stdin_flag=()
    case "$base_name" in
        parallel_benchmark.fdn)
            timeout_secs="$BENCHMARK_PROBE_SECONDS"
            allow_timeout=1
            ;;
        replay_demo.fdn)
            stdin_flag=(--stdin-lines "6\\n3")
            ;;
    esac

    set +e
    run_with_timeout "$timeout_secs" "$stdout" "$stderr" "${stdin_flag[@]}" "$bin"
    exit_code=$?
    set -e

    if [ "$allow_timeout" -eq 1 ] && [ "$exit_code" -eq 124 ]; then
        echo "[PASS] $rel - long-running benchmark reached timeout window"
        PASS=$((PASS + 1))
        continue
    fi
    if [ "$exit_code" -eq 124 ]; then
        echo "[FAIL] $rel - timed out after ${DEFAULT_TIMEOUT_SECONDS}s"
        FAIL=$((FAIL + 1))
        continue
    fi
    if [ "$exit_code" -ne 0 ]; then
        echo "[FAIL] $rel - exited with code $exit_code"
        [ -f "$stderr" ] && cat "$stderr"
        FAIL=$((FAIL + 1))
        continue
    fi

    if [ -s "$stderr" ]; then
        echo "[FAIL] $rel - wrote to stderr"
        cat "$stderr"
        FAIL=$((FAIL + 1))
        continue
    fi

    echo "[PASS] $rel"
    PASS=$((PASS + 1))
done < <(python3 - "$REPO_ROOT/test" <<'PY'
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
for path in sorted(root.rglob("*.fdn")):
    print(path)
PY
)

if [ -n "$CASE" ] && [ "$MATCHED" -eq 0 ]; then
    echo "[FAIL] no example matched case '$CASE'" >&2
    exit 1
fi
if [ "$MATCHED" -eq 0 ]; then
    echo "[FAIL] example sweep matched zero test cases" >&2
    exit 1
fi

echo ""
echo "========================================================"
if [ "$FAIL" -eq 0 ]; then
    echo " Example Sweep: $PASS passed, $SKIP skipped - ALL PASS"
else
    echo " Example Sweep: $PASS passed, $FAIL failed, $SKIP skipped"
fi
echo "========================================================"
echo ""

[ "$FAIL" -eq 0 ]
