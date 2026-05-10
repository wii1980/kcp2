#ifndef BENCH_UTIL_H
#define BENCH_UTIL_H

#include <stdint.h>
#include <stddef.h>
#include <math.h>
#include <time.h>

#ifdef __cplusplus
extern "C" {
#endif

// ========== Monotonic Clock ==========
// Returns current time in nanoseconds using CLOCK_MONOTONIC_RAW.
// Avoids gettimeofday's wall-clock issues (ntpd slewing, DST, etc.)
uint64_t now_ns(void);

// Returns current time in seconds as double (convenience wrapper)
double now_sec(void);

// ========== Black Box (prevent dead-code elimination) ==========
// Use: DO_NOT_OPTIMIZE(var) after a write or before a read that the
// compiler might otherwise eliminate.
#ifndef DO_NOT_OPTIMIZE
#define DO_NOT_OPTIMIZE(x) __asm__ volatile("" : : "r,m"(x) : "memory")
#endif

// ========== Xorshift PRNG (lock-free, deterministic) ==========
// Replaces glibc rand() which has internal locking and long period issues.
// Simple 32-bit xorshift: x ^= x << 13; x ^= x >> 17; x ^= x << 5;

typedef struct {
    uint32_t state;
} prng_t;

// Initialize with a seed. Use a fixed seed (e.g. 42) for reproducibility,
// or read from environment variable BENCH_SEED.
void prng_init(prng_t *rng, uint32_t seed);

// Returns a random uint32_t
uint32_t prng_u32(prng_t *rng);

// Returns a random integer in [0, max) (max is exclusive)
int prng_range(prng_t *rng, int max);

// ========== Statistics Collector ==========
// Collects samples from multiple measurement rounds and computes
// min, mean, median (p50), p95, p99, max, stddev.

typedef struct {
    double *samples;
    int count;
    int capacity;
} stats_t;

// Initialize stats with capacity for `max_samples` values
void stats_init(stats_t *s, int max_samples);

// Record a sample value (in nanoseconds typically)
void stats_record(stats_t *s, double value_ns);

// Print a formatted report: name, min, mean, p50, p95, p99, max, stddev
// All values in nanoseconds.
void stats_report(const stats_t *s, const char *name, double bytes_per_op);

// Free allocated memory
void stats_free(stats_t *s);

#ifdef __cplusplus
}
#endif

#endif // BENCH_UTIL_H
