#!/usr/bin/env bash
# ============================================================
#  cpp_benchmark.sh — Build and compare C++ vs Fidan AOT
#  Usage:  ./test/scripts/cpp_benchmark.sh
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
OUT="$ROOT/target/aot_tests"
CPP_SRC="$ROOT/test/cpp_benchmark/benchmark.cpp"
CPP_BIN="$OUT/cpp_bench"
FDN_SRC="$ROOT/test/examples/parallel_benchmark.fdn"
FDN_BIN="$OUT/fidan_bench"

PASS=0; FAIL=0; SKIP=0

echo ""
echo "============================================================"
echo " Fidan vs C++ Benchmark Comparison"
echo "============================================================"
echo ""

mkdir -p "$OUT"

# ── Build Fidan release binary ────────────────────────────────
echo "[1/3] Building Fidan release binary..."
cd "$ROOT"
cargo build --release -p fidan-cli 2>/dev/null
echo "      done."

# ── Build Fidan parallel_benchmark ───────────────────────────
echo ""
echo "[2/3] Compiling Fidan benchmark: $FDN_SRC"
FDN_OK=0
if [ -f "$FDN_SRC" ]; then
    if "$ROOT/target/release/fidan" build "$FDN_SRC" -o "$FDN_BIN" 2>/dev/null; then
        echo "      Compiled to $FDN_BIN"
        FDN_OK=1
    else
        echo "FAIL: fidan build returned error"
    fi
else
    echo "SKIP: $FDN_SRC not found"
fi

# ── Build C++ benchmark ───────────────────────────────────────
echo ""
echo "[3/3] Compiling C++ benchmark: $CPP_SRC"
CPP_OK=0
if command -v g++ &>/dev/null; then
    if g++ -O2 -std=c++17 -pthread -o "$CPP_BIN" "$CPP_SRC" 2>/dev/null; then
        echo "      Compiled with g++ -O2"
        CPP_OK=1
    fi
elif command -v clang++ &>/dev/null; then
    if clang++ -O2 -std=c++17 -pthread -o "$CPP_BIN" "$CPP_SRC" 2>/dev/null; then
        echo "      Compiled with clang++ -O2"
        CPP_OK=1
    fi
else
    echo "SKIP: no C++ compiler found (g++ or clang++)"
fi

# ── Run benchmarks ────────────────────────────────────────────
echo ""
echo "============================================================"
echo " Results"
echo "============================================================"

if [ "$FDN_OK" -eq 1 ]; then
    echo ""
    echo "[Fidan AOT]"
    "$FDN_BIN"
fi

if [ "$CPP_OK" -eq 1 ]; then
    echo ""
    echo "[C++ -O2]"
    "$CPP_BIN"
fi

echo ""
echo "============================================================"
echo " Done. Compare the timing lines above."
echo "============================================================"
echo ""
