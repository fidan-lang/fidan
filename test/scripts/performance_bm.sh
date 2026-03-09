cd "$(dirname "${BASH_SOURCE[0]}")/../.."
valgrind --tool=callgrind ./bin/fidan test/examples/test.fdn
kcachegrind callgrind.out.*
rm callgrind.out.*