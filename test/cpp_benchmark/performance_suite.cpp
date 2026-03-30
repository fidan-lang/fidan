// performance_suite.cpp
//
// C++ counterpart to test/examples/performance_suite.fdn.
// Usage:
//   performance_suite [int_n] [call_n] [float_n]

#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <future>
#include <string>
#include <vector>

using Clock = std::chrono::high_resolution_clock;
using Ms = std::chrono::milliseconds;

static long long parse_or(int argc, char** argv, int index, long long fallback)
{
    if (argc <= index)
        return fallback;
    char* end = nullptr;
    const long long value = std::strtoll(argv[index], &end, 10);
    return (end && *end == '\0') ? value : fallback;
}

static void print_case(const char* label, long long result, long long elapsed_ms)
{
    std::printf("BENCH %s result=%lld ms=%lld\n", label, result, elapsed_ms);
}

static void print_case_float(const char* label, double result, long long elapsed_ms)
{
    std::printf("BENCH %s result=%.12f ms=%lld\n", label, result, elapsed_ms);
}

static void print_speedup(const char* label, long long baseline_ms, long long candidate_ms)
{
    if (baseline_ms > 0 && candidate_ms > 0)
    {
        const double ratio = static_cast<double>(baseline_ms) / static_cast<double>(candidate_ms);
        std::printf("SPEEDUP %s ratio=%.6f\n", label, ratio);
    }
    else
    {
        std::printf("SPEEDUP %s ratio=n/a\n", label);
    }
}

static bool approx_equal(double a, double b, double rel_tol = 1e-12, double abs_tol = 1e-3)
{
    const double diff = std::fabs(a - b);
    const double scale = std::fmax(std::fabs(a), std::fabs(b));
    return diff <= abs_tol || diff <= scale * rel_tol;
}

static long long integer_kernel(long long n)
{
    long long acc = 0;
    for (long long i = 0; i < n; ++i)
        acc += ((i * 31LL) + 7LL) % 97LL;
    return acc;
}

static long long mix_step(long long x, long long i)
{
    return ((x * 1664525LL) + i + 1013904223LL) % 1000000007LL;
}

static long long call_kernel(long long n)
{
    long long acc = 0;
    for (long long i = 0; i < n; ++i)
        acc = mix_step(acc, i);
    return acc;
}

static double sqrt_range(long long from, long long to)
{
    double acc = 0.0;
    for (long long i = from; i < to; ++i)
        acc += std::sqrt(static_cast<double>(i) + 1.0);
    return acc;
}

static double sqrt_parallel(long long n, unsigned tasks)
{
    const long long chunk = n / static_cast<long long>(tasks);
    std::vector<std::future<double>> futures;
    futures.reserve(tasks);
    for (unsigned t = 0; t < tasks; ++t)
    {
        const long long lo = static_cast<long long>(t) * chunk;
        const long long hi = (t + 1 == tasks) ? n : lo + chunk;
        futures.push_back(std::async(std::launch::async, [lo, hi]() { return sqrt_range(lo, hi); }));
    }

    double total = 0.0;
    for (auto& future : futures)
        total += future.get();
    return total;
}

int main(int argc, char** argv)
{
    const long long int_n = parse_or(argc, argv, 1, 120000000LL);
    const long long call_n = parse_or(argc, argv, 2, 60000000LL);
    const long long float_n = parse_or(argc, argv, 3, 80000000LL);
    const unsigned tasks = 4u;

    (void)integer_kernel(32);
    (void)call_kernel(32);
    (void)sqrt_range(0, 32);

    std::printf("CONFIG int_n=%lld call_n=%lld float_n=%lld tasks=%u\n", int_n, call_n, float_n, tasks);

    {
        const auto t0 = Clock::now();
        const long long result = integer_kernel(int_n);
        const auto t1 = Clock::now();
        const long long ms = std::chrono::duration_cast<Ms>(t1 - t0).count();
        print_case("integer_loop", result, ms);
    }

    {
        const auto t0 = Clock::now();
        const long long result = call_kernel(call_n);
        const auto t1 = Clock::now();
        const long long ms = std::chrono::duration_cast<Ms>(t1 - t0).count();
        print_case("call_chain", result, ms);
    }

    long long sqrt_seq_ms = 0;
    double sqrt_seq_result = 0.0;
    {
        const auto t0 = Clock::now();
        sqrt_seq_result = sqrt_range(0, float_n);
        const auto t1 = Clock::now();
        sqrt_seq_ms = std::chrono::duration_cast<Ms>(t1 - t0).count();
        print_case_float("sqrt_seq", sqrt_seq_result, sqrt_seq_ms);
    }

    long long sqrt_par_ms = 0;
    double sqrt_par_result = 0.0;
    {
        const auto t0 = Clock::now();
        sqrt_par_result = sqrt_parallel(float_n, tasks);
        const auto t1 = Clock::now();
        sqrt_par_ms = std::chrono::duration_cast<Ms>(t1 - t0).count();
        print_case_float("sqrt_parallel", sqrt_par_result, sqrt_par_ms);
    }

    std::printf("CHECK sqrt_parallel_matches=%s\n", approx_equal(sqrt_seq_result, sqrt_par_result) ? "true" : "false");
    print_speedup("sqrt_parallel_vs_seq", sqrt_seq_ms, sqrt_par_ms);
    return 0;
}
