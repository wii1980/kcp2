#include "bench_util.h"
#include <stdlib.h>
#include <stdio.h>

// ========== Monotonic Clock ==========

uint64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC_RAW, &ts);
    return ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

double now_sec(void) {
    return (double)now_ns() / 1e9;
}

// ========== Xorshift PRNG ==========

void prng_init(prng_t *rng, uint32_t seed) {
    rng->state = seed != 0 ? seed : 42;
}

uint32_t prng_u32(prng_t *rng) {
    uint32_t x = rng->state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    rng->state = x;
    return x;
}

int prng_range(prng_t *rng, int max) {
    return (int)((uint64_t)prng_u32(rng) * (uint32_t)max >> 32);
}

// ========== Statistics Collector ==========

static int stats_compare(const void *a, const void *b) {
    double da = *(const double *)a;
    double db = *(const double *)b;
    return (da > db) - (da < db);
}

void stats_init(stats_t *s, int max_samples) {
    s->samples = (double *)malloc((size_t)max_samples * sizeof(double));
    s->count = 0;
    s->capacity = max_samples;
}

void stats_record(stats_t *s, double value_ns) {
    s->samples[s->count++] = value_ns;
}

void stats_report(const stats_t *s, const char *name, double bytes_per_op) {
    if (s->count == 0) {
        printf("  %s: no samples\n", name);
        return;
    }

    int n = s->count;

    // Sort a copy so we don't mutate the original if caller needs it
    double *sorted = (double *)malloc((size_t)n * sizeof(double));
    for (int i = 0; i < n; i++) {
        sorted[i] = s->samples[i];
    }
    qsort(sorted, (size_t)n, sizeof(double), stats_compare);

    double min_val = sorted[0];
    double max_val = sorted[n - 1];

    double sum = 0.0;
    for (int i = 0; i < n; i++) {
        sum += sorted[i];
    }
    double mean = sum / n;

    double p50 = sorted[n / 2];
    double p95 = sorted[(int)(n * 0.95)];
    double p99 = sorted[(int)(n * 0.99)];

    double variance = 0.0;
    for (int i = 0; i < n; i++) {
        double diff = sorted[i] - mean;
        variance += diff * diff;
    }
    double stddev = sqrt(variance / n);

    double throughput = bytes_per_op > 0.0
        ? bytes_per_op / (mean / 1e9) / (1024.0 * 1024.0)
        : 0.0;

    printf("  %s: min=%7.2f ns  mean=%7.2f ns  p50=%7.2f ns  p95=%7.2f ns  p99=%7.2f ns  max=%7.2f ns  stddev=%7.2f ns  throughput=%7.2f MB/s\n",
           name, min_val, mean, p50, p95, p99, max_val, stddev, throughput);

    free(sorted);
}

void stats_free(stats_t *s) {
    free(s->samples);
    s->samples = NULL;
    s->count = 0;
    s->capacity = 0;
}
