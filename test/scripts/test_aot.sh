#!/usr/bin/env bash
# ── Fidan AOT Golden-File Test Runner (Unix) ──────────────────────────────────
#
# Compiles each .fdn file that has a corresponding test/golden/*.expected file
# through the Cranelift AOT backend, runs the resulting binary, and diffs the
# output against the golden file.
#
# Usage:
#   test/scripts/test_aot.sh [--release]
#
# Options:
#   --release   Build in release mode (default: release)
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

for arg in "$@"; do
    case "$arg" in
        --release) BUILD_MODE="--release"; BIN_DIR="$REPO_ROOT/target/release" ;;
    esac
done

FIDAN="$BIN_DIR/fidan"
PASS=0; FAIL=0; SKIP=0

echo ""
echo "════════════════════════════════════════════════════════"
echo " Fidan AOT Golden-File Test Suite"
echo "════════════════════════════════════════════════════════"
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
    if ! "$FIDAN" build "$SRC" -o "$BIN" 2>/dev/null; then
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
