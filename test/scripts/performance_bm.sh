#!/usr/bin/env bash
# Unix Callgrind profiling helper. Requires Valgrind and KCachegrind.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
for tool in valgrind kcachegrind; do
    if ! command -v "$tool" >/dev/null; then
        echo "Required profiling tool '$tool' is not installed" >&2
        exit 1
    fi
done

cd "$ROOT"
cargo build --release --locked -p fidan-cli
mkdir -p target/callgrind
valgrind --tool=callgrind --callgrind-out-file=target/callgrind/callgrind.out \
    ./target/release/fidan run test/examples/test.fdn
kcachegrind target/callgrind/callgrind.out
