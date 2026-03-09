// benchmark.cpp — C++ equivalent of test/examples/parallel_benchmark.fdn
//
// Measures the time taken by a sequential sum loop (N = 800,000,000 iterations)
// and by a parallel version (4 tasks, each summing N/4 iterations).
//
// Compile:
//   g++ -O2 -std=c++17 -pthread -o benchmark benchmark.cpp
// or on MSVC:
//   cl /O2 /std:c++17 benchmark.cpp
//
// Run:
//   ./benchmark

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <future>
#include <numeric>
#include <thread>
#include <vector>

static const long long N = 800'000'000LL;

// ── Sequential sum ────────────────────────────────────────────────────────────

static long long sequential_sum(long long n)
{
    long long acc = 0;
    for (long long i = 1; i <= n; ++i)
        acc += i;
    return acc;
}

// ── Parallel sum (std::async, 4 tasks) ────────────────────────────────────────

static long long parallel_sum(long long n)
{
    const unsigned tasks = std::thread::hardware_concurrency()
                               ? std::min(std::thread::hardware_concurrency(), 8u)
                               : 4u;
    long long chunk = n / tasks;
    std::vector<std::future<long long>> futs;
    futs.reserve(tasks);
    for (unsigned t = 0; t < tasks; ++t)
    {
        long long lo = (long long)t * chunk + 1;
        long long hi = (t + 1 == tasks) ? n : lo + chunk - 1;
        futs.push_back(std::async(std::launch::async, [lo, hi]()
                                  {
            long long acc = 0;
            for (long long i = lo; i <= hi; ++i) acc += i;
            return acc; }));
    }
    long long total = 0;
    for (auto &f : futs)
        total += f.get();
    return total;
}

// ── Helpers ───────────────────────────────────────────────────────────────────

using Clock = std::chrono::high_resolution_clock;
using Ms = std::chrono::milliseconds;

int main()
{
    std::printf("C++ Benchmark (N = %lld)\n", N);
    std::printf("─────────────────────────────────────────\n");

    // Sequential
    {
        auto t0 = Clock::now();
        long long result = sequential_sum(N);
        auto t1 = Clock::now();
        long long ms = std::chrono::duration_cast<Ms>(t1 - t0).count();
        std::printf("sequential  result=%lld  time=%lldms\n", result, ms);
    }

    // Parallel
    {
        auto t0 = Clock::now();
        long long result = parallel_sum(N);
        auto t1 = Clock::now();
        long long ms = std::chrono::duration_cast<Ms>(t1 - t0).count();
        std::printf("parallel    result=%lld  time=%lldms  threads=%u\n",
                    result, ms, std::thread::hardware_concurrency());
    }

    return 0;
}
