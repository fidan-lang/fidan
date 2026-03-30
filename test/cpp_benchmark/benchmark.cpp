// benchmark.cpp — C++ equivalent of test/examples/parallel_benchmark.fdn
//
// Measures the time taken by a sequential sqrt-sum loop (N = 800,000,000 iterations)
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
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <future>
#include <thread>
#include <vector>

static const long long N = 800'000'000LL;
static const unsigned TASKS = 4u;

static double sequential_sum(long long n)
{
  double acc = 0.0;
  for (long long i = 0; i < n; ++i)
    acc += std::sqrt(static_cast<double>(i) + 1.0);
  return acc;
}

static double parallel_sum(long long n)
{
  const long long chunk = n / static_cast<long long>(TASKS);
  std::vector<std::future<double>> futs;
  futs.reserve(TASKS);
  for (unsigned t = 0; t < TASKS; ++t)
  {
    const long long lo = static_cast<long long>(t) * chunk;
    const long long hi = (t + 1 == TASKS) ? n : lo + chunk;
    futs.push_back(std::async(std::launch::async, [lo, hi]() {
      double acc = 0.0;
      for (long long i = lo; i < hi; ++i)
        acc += std::sqrt(static_cast<double>(i) + 1.0);
      return acc;
    }));
  }

  double total = 0.0;
  for (auto &f : futs)
    total += f.get();
  return total;
}

using Clock = std::chrono::high_resolution_clock;
using Ms = std::chrono::milliseconds;

int main()
{
  std::printf("C++ Benchmark (N = %lld)\n", N);
  std::printf("─────────────────────────────────────────\n");

  {
    auto t0 = Clock::now();
    double result = sequential_sum(N);
    auto t1 = Clock::now();
    long long ms = std::chrono::duration_cast<Ms>(t1 - t0).count();
    std::printf("sequential  result=%.12f  time=%lldms\n", result, ms);
  }

  {
    auto t0 = Clock::now();
    double result = parallel_sum(N);
    auto t1 = Clock::now();
    long long ms = std::chrono::duration_cast<Ms>(t1 - t0).count();
    std::printf("parallel    result=%.12f  time=%lldms  threads=%u\n", result, ms, TASKS);
  }

  return 0;
}