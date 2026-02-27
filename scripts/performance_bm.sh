valgrind --tool=callgrind ./bin/fidan TEST/test.fdn
kcachegrind callgrind.out.*
rm callgrind.out.*