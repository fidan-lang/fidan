#!/usr/bin/env bash
# ============================================================
#  cpp_benchmark_suite.sh — Multi-case Fidan vs C++ comparison
#  Usage:  ./test/scripts/cpp_benchmark_suite.sh [int_n] [call_n] [float_n]
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
OUT="$ROOT/target/perf_suite"
CPP_SRC="$ROOT/test/cpp_benchmark/performance_suite.cpp"
CPP_BIN="$OUT/cpp_perf_suite"
FDN_SRC="$ROOT/test/examples/performance_suite.fdn"
FDN_CR="$OUT/fidan_perf_cranelift"
FDN_LL="$OUT/fidan_perf_llvm"
RUN_ARGS=("$@")

echo ""
echo "============================================================"
echo " Fidan vs C++ Performance Suite"
echo "============================================================"
echo ""

mkdir -p "$OUT"

echo "[1/4] Building Fidan release binary..."
cd "$ROOT"
cargo build --release -p fidan-cli 2>/dev/null
echo "      done."

echo ""
echo "[2/4] Compiling Fidan suite (Cranelift): $FDN_SRC"
FDN_CR_OK=0
if "$ROOT/target/release/fidan" build --backend cranelift "$FDN_SRC" -o "$FDN_CR" 2>/dev/null; then
    echo "      Compiled to $FDN_CR"
    FDN_CR_OK=1
else
    echo "FAIL: fidan build --backend cranelift returned error"
fi

echo ""
echo "[3/4] Compiling Fidan suite (LLVM): $FDN_SRC"
FDN_LL_OK=0
if "$ROOT/target/release/fidan" build --backend llvm "$FDN_SRC" -o "$FDN_LL" 2>/dev/null; then
    echo "      Compiled to $FDN_LL"
    FDN_LL_OK=1
else
    echo "SKIP: LLVM backend unavailable or build failed"
fi

echo ""
echo "[4/4] Compiling C++ suite: $CPP_SRC"
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
fi

if [ "$CPP_OK" -eq 0 ]; then
    echo "SKIP: no C++ compiler found (g++ or clang++)"
fi

echo ""
echo "============================================================"
echo " Results"
echo "============================================================"
if [ "${#RUN_ARGS[@]}" -gt 0 ]; then
    echo " args: ${RUN_ARGS[*]}"
fi

if [ "$FDN_CR_OK" -eq 1 ]; then
    echo ""
    echo "[Fidan AOT - Cranelift]"
    "$FDN_CR" "${RUN_ARGS[@]}"
fi

if [ "$FDN_LL_OK" -eq 1 ]; then
    echo ""
    echo "[Fidan AOT - LLVM]"
    "$FDN_LL" "${RUN_ARGS[@]}"
fi

if [ "$CPP_OK" -eq 1 ]; then
    echo ""
    echo "[C++ -O2]"
    "$CPP_BIN" "${RUN_ARGS[@]}"
fi

echo ""
echo "============================================================"
echo " Done. Compare the BENCH / SPEEDUP lines above."
echo "============================================================"
echo ""
