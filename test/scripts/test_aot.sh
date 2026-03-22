#!/usr/bin/env bash
# ── Fidan AOT Golden-File Test Runner (Unix) ──────────────────────────────────
#
# Compiles each .fdn file that has a corresponding test/golden/*.expected file
# through an AOT backend, runs the resulting binary, and diffs the
# output against the golden file.
#
# Usage:
#   test/scripts/test_aot.sh [--release] [--backend <auto|cranelift|llvm>] [--lto <off|full>] [--case <file.fdn>]
#
# Options:
#   --release            Build in release mode (default: release)
#   --backend <backend>  AOT backend to test (default: cranelift)
#   --lto <mode>         LLVM link-time optimization mode (default: off)
#   --case <file.fdn>    Run only one golden-file case
#
# Dependencies:
#   - Rust / Cargo installed
#   - A C linker (cc / ld) on PATH
#
# Exit code: 0 = all tests passed, 1 = one or more failures
# ─────────────────────────────────────────────────────────────────────────────

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
GOLDEN_DIR="$REPO_ROOT/test/golden"
EXAMPLES_DIR="$REPO_ROOT/test/examples"
TMP_DIR="$REPO_ROOT/target/aot_tests"

BUILD_MODE="--release"
BIN_DIR="$REPO_ROOT/target/release"
AOT_BACKEND="cranelift"
AOT_LTO="off"
AOT_CASE=""

while [ "$#" -gt 0 ]; do
    case "$1" in
        --release)
            BUILD_MODE="--release"
            BIN_DIR="$REPO_ROOT/target/release"
            shift
            ;;
        --backend)
            shift
            if [ "$#" -eq 0 ]; then
                echo "[FAIL] missing value for --backend" >&2
                exit 1
            fi
            AOT_BACKEND="$1"
            shift
            ;;
        --lto)
            shift
            if [ "$#" -eq 0 ]; then
                echo "[FAIL] missing value for --lto" >&2
                exit 1
            fi
            AOT_LTO="$1"
            shift
            ;;
        --case)
            shift
            if [ "$#" -eq 0 ]; then
                echo "[FAIL] missing value for --case" >&2
                exit 1
            fi
            AOT_CASE="$1"
            shift
            ;;
        *)
            shift
            ;;
    esac
done

case "$AOT_BACKEND" in
    auto|cranelift|llvm) ;;
    *)
        echo "[FAIL] unsupported backend '$AOT_BACKEND' (expected: auto, cranelift, llvm)" >&2
        exit 1
        ;;
esac

case "$AOT_LTO" in
    off|full) ;;
    *)
        echo "[FAIL] unsupported lto mode '$AOT_LTO' (expected: off, full)" >&2
        exit 1
        ;;
esac

FIDAN="$BIN_DIR/fidan"
PASS=0; FAIL=0; SKIP=0

echo ""
echo "════════════════════════════════════════════════════════"
echo " Fidan AOT Golden-File Test Suite"
echo "════════════════════════════════════════════════════════"
echo " backend: $AOT_BACKEND"
echo " lto: $AOT_LTO"
[ -n "$AOT_CASE" ] && echo " case: $AOT_CASE"
echo ""

# ── Step 1: Build fidan ────────────────────────────────────────────────────────
echo "[build] cargo build $BUILD_MODE -p fidan-cli ..."
cargo build $BUILD_MODE -p fidan-cli 2>/dev/null
echo "[build] OK — $FIDAN"
echo ""

# ── Step 2: Create temp directory ─────────────────────────────────────────────
mkdir -p "$TMP_DIR"

# ── Step 3: Run each golden test ───────────────────────────────────────────────
for EXPECTED in "$GOLDEN_DIR"/*.expected; do
    [ -f "$EXPECTED" ] || continue

    # Derive the source filename: "test.fdn.expected" → "test.fdn"
    BASENAME="$(basename "$EXPECTED" .expected)"
    SRC="$EXAMPLES_DIR/$BASENAME"

    if [ -n "$AOT_CASE" ] && [ "$BASENAME" != "$AOT_CASE" ]; then
        continue
    fi

    if [ ! -f "$SRC" ]; then
        echo "[SKIP] $BASENAME — source file not found"
        SKIP=$((SKIP + 1))
        continue
    fi

    # Strip the .fdn extension for the binary name
    STEM="${BASENAME%.fdn}"
    BIN="$TMP_DIR/${STEM}_aot"
    ACTUAL="$TMP_DIR/${STEM}_actual.txt"

    # Compile via AOT
    if ! "$FIDAN" build --backend "$AOT_BACKEND" --lto "$AOT_LTO" "$SRC" -o "$BIN" 2>/dev/null; then
        echo "[FAIL] $BASENAME — AOT compile failed"
        FAIL=$((FAIL + 1))
        continue
    fi

    # Run the binary and capture stdout
    if ! "$BIN" > "$ACTUAL" 2>/dev/null; then
        echo "[FAIL] $BASENAME — binary exited with error"
        FAIL=$((FAIL + 1))
        continue
    fi

    # Diff actual vs expected
    if diff -q "$ACTUAL" "$EXPECTED" >/dev/null 2>&1; then
        echo "[PASS] $BASENAME"
        PASS=$((PASS + 1))
    else
        echo "[FAIL] $BASENAME — output mismatch"
        echo "  === expected ==="
        cat "$EXPECTED"
        echo "  === actual ==="
        cat "$ACTUAL"
        echo "  === diff ==="
        diff "$EXPECTED" "$ACTUAL" || true
        FAIL=$((FAIL + 1))
    fi
done

# ── Summary ────────────────────────────────────────────────────────────────────
echo ""
echo "════════════════════════════════════════════════════════"
if [ "$FAIL" -eq 0 ]; then
    echo " AOT Tests: $PASS passed, $SKIP skipped — ALL PASS"
else
    echo " AOT Tests: $PASS passed, $FAIL FAILED, $SKIP skipped"
fi
echo "════════════════════════════════════════════════════════"
echo ""

[ "$FAIL" -eq 0 ]
