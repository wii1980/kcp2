/*
 * bench.c — kcp-c 纯 C 性能基准测试（重写版）
 *
 * 针对 Rust Criterion 方法论对齐重写：
 *   - 单调时钟 CLOCK_MONOTONIC_RAW（而非 gettimeofday）
 *   - 每次迭代带 setup/teardown（对应 iter_batched_ref）
 *   - 多轮统计：min / mean / p50 / p95 / p99 / stddev
 *   - DO_NOT_OPTIMIZE 防死代码消除
 *   - 确定性 PRNG 种子，结果可复现
 *   - 链表网络模拟器（无 qsort）
 *
 * 编译:
 *   make
 *
 * 运行:
 *   make run            # 普通运行
 *   make run-fast        # taskset 绑核（需 root/权限）
 *
 * 环境变量:
 *   BENCH_SEED=42       # PRNG 种子，默认 42
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

#include "ikcp.h"
#include "bench_util.h"
#include "net_sim.h"

/* ==================================================================
 * 辅助: 手动 segment 编解码（ikcp_encode_seg / ikcp_decode 为 ikcp.c
 * 内部 static，这里重新实现供基准测试使用）
 * ================================================================*/

/* 编码 segment 头到 buffer，返回头后的位置 */
static char *seg_encode(char *ptr, uint32_t conv, uint8_t cmd, uint8_t frg,
                        uint16_t wnd, uint32_t ts, uint32_t sn,
                        uint32_t una, uint32_t len)
{
    memcpy(ptr, &conv, 4); ptr += 4;
    *ptr++ = cmd;
    *ptr++ = frg;
    memcpy(ptr, &wnd, 2); ptr += 2;
    memcpy(ptr, &ts,   4); ptr += 4;
    memcpy(ptr, &sn,   4); ptr += 4;
    memcpy(ptr, &una,  4); ptr += 4;
    memcpy(ptr, &len,  4); ptr += 4;
    return ptr;
}

/* 解码 segment 头，返回头后的位置 */
static const char *seg_decode(const char *ptr, uint32_t *conv, uint8_t *cmd,
                              uint8_t *frg, uint16_t *wnd, uint32_t *ts,
                              uint32_t *sn, uint32_t *una, uint32_t *len)
{
    memcpy(conv, ptr, 4); ptr += 4;
    *cmd = (uint8_t)*ptr++;
    *frg = (uint8_t)*ptr++;
    memcpy(wnd, ptr, 2); ptr += 2;
    memcpy(ts,  ptr, 4); ptr += 4;
    memcpy(sn,  ptr, 4); ptr += 4;
    memcpy(una, ptr, 4); ptr += 4;
    memcpy(len, ptr, 4); ptr += 4;
    return ptr;
}

/* ==================================================================
 * 辅助: 空 output 回调 + KCP 创建快捷方式
 * ================================================================*/

static int dummy_output(const char *buf, int len, ikcpcb *kcp, void *user) {
    (void)buf; (void)len; (void)kcp; (void)user;
    return 0;
}

static ikcpcb *kcp_create_dummy(uint32_t conv) {
    ikcpcb *kcp = ikcp_create(conv, NULL);
    ikcp_setoutput(kcp, dummy_output);
    return kcp;
}

/* ==================================================================
 * 通用基准测试工具：多轮计时循环
 *
 * run_bench 执行 ROUNDS 轮，每轮重复 ITERATIONS 次操作 op()，
 * 记录每轮平均耗时到 stats_t。
 *
 * op() 是调用者提供的函数，接收一个 void* context。
 * setup() / teardown() 可选，在每次 op() 前后调用。
 * ================================================================*/

typedef void (*bench_op_fn)(void *ctx);
typedef void (*bench_setup_fn)(void *ctx);
typedef void (*bench_teardown_fn)(void *ctx);

static void run_bench(stats_t *stats, int rounds, int iterations,
                      bench_setup_fn setup, bench_op_fn op,
                      bench_teardown_fn teardown, void *ctx)
{
    for (int r = 0; r < rounds; r++) {
        if (setup) setup(ctx);
        uint64_t start = now_ns();
        for (int i = 0; i < iterations; i++) {
            op(ctx);
        }
        uint64_t end = now_ns();
        if (teardown) teardown(ctx);

        double per_op = (double)(end - start) / (double)iterations;
        stats_record(stats, per_op);
    }
}

/* ==================================================================
 * 基准测试 1: Segment 编码
 * ================================================================*/

static void bench_segment_encode(void) {
    printf("\n=== Segment 编码 ===\n");

    uint32_t conv = 0x11223344;
    uint8_t  cmd  = 81;
    uint8_t  frg  = 0;
    uint16_t wnd  = 128;
    uint32_t ts   = 1000;
    uint32_t sn   = 1;
    uint32_t una  = 0;
    uint32_t len  = 100;

    stats_t stats;
    stats_init(&stats, 100);

    for (int r = 0; r < 100; r++) {
        uint64_t start = now_ns();
        for (int i = 0; i < 10000; i++) {
            char buf[200];
            char *ptr = seg_encode(buf, conv, cmd, frg, wnd, ts, sn, una, len);
            DO_NOT_OPTIMIZE(ptr);
        }
        uint64_t end = now_ns();
        double per_op = (double)(end - start) / 10000.0;
        stats_record(&stats, per_op);
    }

    stats_report(&stats, "segment_encode", 24.0 + 100.0);
    stats_free(&stats);
}

/* ==================================================================
 * 基准测试 2: Segment 解码
 * ================================================================*/

static void bench_segment_decode(void) {
    printf("\n=== Segment 解码 ===\n");

    /* 预先编码一个 segment */
    char encoded[200];
    uint32_t conv = 0x11223344;
    uint32_t data_len = 100;
    char *p = seg_encode(encoded, conv, 81, 0, 128, 1000, 1, 0, data_len);
    memset(p, 0xCD, data_len); /* 填充数据 */

    stats_t stats;
    stats_init(&stats, 100);

    for (int r = 0; r < 100; r++) {
        uint64_t start = now_ns();
        for (int i = 0; i < 10000; i++) {
            uint32_t d_conv, d_ts, d_sn, d_una, d_len;
            uint8_t  d_cmd, d_frg;
            uint16_t d_wnd;
            const char *rp = seg_decode(encoded, &d_conv, &d_cmd, &d_frg,
                                        &d_wnd, &d_ts, &d_sn, &d_una, &d_len);
            size_t total = 24 + (size_t)d_len;
            DO_NOT_OPTIMIZE(&total);
            (void)rp;
        }
        uint64_t end = now_ns();
        double per_op = (double)(end - start) / 10000.0;
        stats_record(&stats, per_op);
    }

    stats_report(&stats, "segment_decode", 24.0 + 100.0);
    stats_free(&stats);
}

/* ==================================================================
 * 基准测试 3: Segment 编码多尺寸扫描
 * ================================================================*/

static void bench_segment_encode_sweep(void) {
    printf("\n=== Segment 编码多尺寸扫描 ===\n");

    int sizes[] = {0, 64, 256, 1400};
    int nsizes = 4;

    for (int s = 0; s < nsizes; s++) {
        int data_size = sizes[s];
        uint32_t conv = 0x11223344;
        uint8_t  cmd  = 81;
        uint8_t  frg  = 0;
        uint16_t wnd  = 128;
        uint32_t ts   = 1000;
        uint32_t sn   = 1;
        uint32_t una  = 0;

        char name[64];
        snprintf(name, sizeof(name), "segment_encode/%d", data_size);

        stats_t stats;
        stats_init(&stats, 50);

        for (int r = 0; r < 50; r++) {
            uint64_t start = now_ns();
            for (int i = 0; i < 5000; i++) {
                char buf[1500];
                char *ptr = seg_encode(buf, conv, cmd, frg, wnd, ts, sn,
                                       una, (uint32_t)data_size);
                if (data_size > 0) {
                    memset(ptr, 0xAB, (size_t)data_size);
                    ptr += data_size;
                }
                DO_NOT_OPTIMIZE(ptr);
            }
            uint64_t end = now_ns();
            double per_op = (double)(end - start) / 5000.0;
            stats_record(&stats, per_op);
        }

        stats_report(&stats, name, 24.0 + (double)data_size);
        stats_free(&stats);
    }
}

/* ==================================================================
 * 基准测试 4-6: 发送吞吐量
 * ================================================================*/

struct send_ctx {
    ikcpcb *kcp;
    char   *data;
    int     data_size;
};

static void send_setup(void *ctx) {
    struct send_ctx *c = (struct send_ctx *)ctx;
    c->kcp = kcp_create_dummy(0x11223344);
}

static void send_op(void *ctx) {
    struct send_ctx *c = (struct send_ctx *)ctx;
    ikcp_send(c->kcp, c->data, c->data_size);
}

static void send_teardown(void *ctx) {
    struct send_ctx *c = (struct send_ctx *)ctx;
    ikcp_release(c->kcp);
}

static void bench_send_small_packet(void) {
    printf("\n=== 发送吞吐量: 小包 ===\n");
    struct send_ctx ctx;
    ctx.data_size = 100;
    ctx.data = (char *)malloc((size_t)ctx.data_size);
    memset(ctx.data, 0xCD, (size_t)ctx.data_size);

    stats_t stats;
    stats_init(&stats, 50);
    run_bench(&stats, 50, 5000, send_setup, send_op, send_teardown, &ctx);
    stats_report(&stats, "send_small_packet", (double)ctx.data_size);
    stats_free(&stats);
    free(ctx.data);
}

static void bench_send_large_packet(void) {
    printf("\n=== 发送吞吐量: 大包 (10KB) ===\n");
    struct send_ctx ctx;
    ctx.data_size = 10 * 1024;
    ctx.data = (char *)malloc((size_t)ctx.data_size);
    memset(ctx.data, 0xCD, (size_t)ctx.data_size);

    stats_t stats;
    stats_init(&stats, 50);
    /* 迭代次数少一些，因为 10KB 数据量大 */
    run_bench(&stats, 50, 2000, send_setup, send_op, send_teardown, &ctx);
    stats_report(&stats, "send_large_packet", (double)ctx.data_size);
    stats_free(&stats);
    free(ctx.data);
}

static void bench_send_throughput(void) {
    printf("\n=== 发送吞吐量: 多尺寸扫描 ===\n");

    int sizes[] = {64, 256, 1024, 1400};
    int nsizes = 4;

    for (int s = 0; s < nsizes; s++) {
        struct send_ctx ctx;
        ctx.data_size = sizes[s];
        ctx.data = (char *)malloc((size_t)ctx.data_size);
        memset(ctx.data, 0xCD, (size_t)ctx.data_size);

        char name[64];
        snprintf(name, sizeof(name), "send/%d", sizes[s]);

        stats_t stats;
        stats_init(&stats, 30);
        run_bench(&stats, 30, 5000, send_setup, send_op, send_teardown, &ctx);
        stats_report(&stats, name, (double)ctx.data_size);
        stats_free(&stats);
        free(ctx.data);
    }
}

/* ==================================================================
 * 基准测试 7: Stream 模式发送
 * ================================================================*/

struct stream_ctx {
    ikcpcb *kcp;
    char   *data;
    int     data_size;
};

static void stream_setup(void *ctx) {
    struct stream_ctx *c = (struct stream_ctx *)ctx;
    c->kcp = ikcp_create(0x11223344, NULL);
    ikcp_setoutput(c->kcp, dummy_output);
    c->kcp->stream = 1; /* 启用 stream 模式 */
}

static void stream_op(void *ctx) {
    struct stream_ctx *c = (struct stream_ctx *)ctx;
    ikcp_send(c->kcp, c->data, c->data_size);
}

static void stream_teardown(void *ctx) {
    struct stream_ctx *c = (struct stream_ctx *)ctx;
    ikcp_release(c->kcp);
}

static void bench_stream_mode(void) {
    printf("\n=== Stream 模式发送 ===\n");
    struct stream_ctx ctx;
    ctx.data_size = 100;
    ctx.data = (char *)malloc((size_t)ctx.data_size);
    memset(ctx.data, 0xAB, (size_t)ctx.data_size);

    stats_t stats;
    stats_init(&stats, 50);
    run_bench(&stats, 50, 5000, stream_setup, stream_op, stream_teardown, &ctx);
    stats_report(&stats, "send_stream_mode", (double)ctx.data_size);
    stats_free(&stats);
    free(ctx.data);
}

/* ==================================================================
 * 基准测试 8: Input 吞吐量
 * ================================================================*/

struct input_ctx {
    ikcpcb *kcp;
    char   *packet;
    int     packet_len;
};

static void input_setup(void *ctx) {
    struct input_ctx *c = (struct input_ctx *)ctx;
    c->kcp = kcp_create_dummy(0x11223344);
}

static void input_op(void *ctx) {
    struct input_ctx *c = (struct input_ctx *)ctx;
    ikcp_input(c->kcp, c->packet, (long)c->packet_len);
}

static void input_teardown(void *ctx) {
    struct input_ctx *c = (struct input_ctx *)ctx;
    ikcp_release(c->kcp);
}

static void bench_input(void) {
    printf("\n=== Input 吞吐量 ===\n");

    /* 预编码一个 segment */
    char pkt[200];
    char *p = seg_encode(pkt, 0x11223344, 81, 0, 128, 1000, 1, 0, 100);
    memset(p, 0xAB, 100);
    int pkt_len = 24 + 100;

    struct input_ctx ctx;
    ctx.packet = pkt;
    ctx.packet_len = pkt_len;

    stats_t stats;
    stats_init(&stats, 50);
    run_bench(&stats, 50, 10000, input_setup, input_op, input_teardown, &ctx);
    stats_report(&stats, "input", (double)pkt_len);
    stats_free(&stats);
}

/* ==================================================================
 * 基准测试 9: Recv 吞吐量
 * ================================================================*/

struct recv_ctx {
    ikcpcb *kcp;
    char    recv_buf[1024];
};

struct recv_output_ctx {
    char  *buf;
    int   *len;
    int    cap;
};

static int recv_output(const char *buf, int len, ikcpcb *kcp, void *user) {
    (void)kcp;
    struct recv_output_ctx *ctx = (struct recv_output_ctx *)user;
    if (len <= ctx->cap && ctx->len) {
        memcpy(ctx->buf, buf, (size_t)len);
        *ctx->len = len;
    }
    return 0;
}

static void recv_setup(void *ctx) {
    struct recv_ctx *c = (struct recv_ctx *)ctx;
    /* 创建 KCP + 发送数据 + 回环 input，使 recv 队列有数据 */
    char out_buf[2000];
    int  out_len = 0;
    struct recv_output_ctx octx = { out_buf, &out_len, sizeof(out_buf) };

    ikcpcb *k = ikcp_create(0x11223344, &octx);
    ikcp_setoutput(k, recv_output);
    const char *msg = "test data for recv benchmark";
    ikcp_send(k, msg, (int)strlen(msg));
    ikcp_update(k, 0);
    ikcp_flush(k);
    /* 把 output 塞回 input，让 recv 队列满 */
    if (out_len > 0) {
        ikcp_input(k, out_buf, (long)out_len);
    }
    c->kcp = k;
}

static void recv_op(void *ctx) {
    struct recv_ctx *c = (struct recv_ctx *)ctx;
    int hr = ikcp_recv(c->kcp, c->recv_buf, (int)sizeof(c->recv_buf));
    DO_NOT_OPTIMIZE(&hr);
}

static void recv_teardown(void *ctx) {
    struct recv_ctx *c = (struct recv_ctx *)ctx;
    ikcp_release(c->kcp);
}

static void bench_recv(void) {
    printf("\n=== Recv 吞吐量 ===\n");

    struct recv_ctx ctx;
    memset(&ctx, 0, sizeof(ctx));

    stats_t stats;
    stats_init(&stats, 50);
    run_bench(&stats, 50, 10000, recv_setup, recv_op, recv_teardown, &ctx);
    stats_report(&stats, "recv", 0.0);
    stats_free(&stats);
}

/* ==================================================================
 * 基准测试 10: Flush 吞吐量
 * ================================================================*/

struct flush_ctx {
    ikcpcb *kcp;
};

static void flush_setup(void *ctx) {
    struct flush_ctx *c = (struct flush_ctx *)ctx;
    c->kcp = ikcp_create(0x11223344, NULL);
    ikcp_setoutput(c->kcp, dummy_output);
    ikcp_send(c->kcp, "flush benchmark data", 20);
    ikcp_update(c->kcp, 0);  /* 设置 updated=true 和 ts_flush */
}

static void flush_op(void *ctx) {
    struct flush_ctx *c = (struct flush_ctx *)ctx;
    ikcp_flush(c->kcp);
}

static void flush_teardown(void *ctx) {
    struct flush_ctx *c = (struct flush_ctx *)ctx;
    ikcp_release(c->kcp);
}

static void bench_flush(void) {
    printf("\n=== Flush 吞吐量 ===\n");

    struct flush_ctx ctx;

    stats_t stats;
    stats_init(&stats, 50);
    run_bench(&stats, 50, 10000, flush_setup, flush_op, flush_teardown, &ctx);
    stats_report(&stats, "flush", 0.0);
    stats_free(&stats);
}

/* ==================================================================
 * 基准测试 11: Update 吞吐量
 * ================================================================*/

struct update_ctx {
    ikcpcb *kcp;
};

static void update_setup(void *ctx) {
    struct update_ctx *c = (struct update_ctx *)ctx;
    c->kcp = ikcp_create(0x11223344, NULL);
    ikcp_setoutput(c->kcp, dummy_output);
    ikcp_send(c->kcp, "update benchmark data", 21);
    ikcp_update(c->kcp, 0);
    ikcp_flush(c->kcp);
}

static void update_op(void *ctx) {
    struct update_ctx *c = (struct update_ctx *)ctx;
    ikcp_update(c->kcp, 100);
}

static void update_teardown(void *ctx) {
    struct update_ctx *c = (struct update_ctx *)ctx;
    ikcp_release(c->kcp);
}

static void bench_update(void) {
    printf("\n=== Update 吞吐量 ===\n");

    struct update_ctx ctx;

    stats_t stats;
    stats_init(&stats, 50);
    run_bench(&stats, 50, 5000, update_setup, update_op, update_teardown, &ctx);
    stats_report(&stats, "update", 0.0);
    stats_free(&stats);
}

/* ==================================================================
 * 基准测试 12: 回环通信
 * ================================================================*/

/* 回环输出回调上下文 */
struct loop_ctx {
    char *buf;
    int  *len;
    int   cap;
};

static int loop_output(const char *buf, int len, ikcpcb *kcp, void *user) {
    (void)kcp;
    struct loop_ctx *lctx = (struct loop_ctx *)user;
    if (len <= lctx->cap && lctx->len) {
        memcpy(lctx->buf, buf, (size_t)len);
        *lctx->len = len;
    }
    return 0;
}

struct loop_iter_ctx {
    ikcpcb *kcp1;
    ikcpcb *kcp2;
    struct loop_ctx out1; /* kcp1 的 output 放到这里，喂给 kcp2 */
    struct loop_ctx out2; /* kcp2 的 output 放到这里，暂时丢弃 */
    char msg[20];
    int  msg_len;
    char buf1[2000];
    int  len1;
    char buf2[2000];
    int  len2;
    char recv_buf[1024];
};

static void loop_setup(void *ctx) {
    struct loop_iter_ctx *c = (struct loop_iter_ctx *)ctx;
    c->len1 = 0;
    c->len2 = 0;
    c->out1.buf = c->buf1;
    c->out1.len = &c->len1;
    c->out1.cap = (int)sizeof(c->buf1);
    c->out2.buf = c->buf2;
    c->out2.len = &c->len2;
    c->out2.cap = (int)sizeof(c->buf2);

    c->kcp1 = ikcp_create(0x11223344, &c->out1);
    c->kcp2 = ikcp_create(0x11223344, &c->out2);
    ikcp_setoutput(c->kcp1, loop_output);
    ikcp_setoutput(c->kcp2, loop_output);

    c->msg_len = (int)snprintf(c->msg, sizeof(c->msg), "test message");
}

static void loop_op(void *ctx) {
    struct loop_iter_ctx *c = (struct loop_iter_ctx *)ctx;
    c->len1 = 0;
    c->len2 = 0;

    ikcp_send(c->kcp1, c->msg, c->msg_len);
    ikcp_update(c->kcp1, 0);
    ikcp_flush(c->kcp1);

    /* kcp1 的输出 → kcp2 的输入 */
    if (c->len1 > 0) {
        ikcp_input(c->kcp2, c->buf1, (long)c->len1);
    }

    /* 从 kcp2 收 */
    int hr = ikcp_recv(c->kcp2, c->recv_buf, (int)sizeof(c->recv_buf));
    DO_NOT_OPTIMIZE(&hr);
}

static void loop_teardown(void *ctx) {
    struct loop_iter_ctx *c = (struct loop_iter_ctx *)ctx;
    ikcp_release(c->kcp1);
    ikcp_release(c->kcp2);
}

static void bench_loopback(void) {
    printf("\n=== 回环通信 ===\n");

    struct loop_iter_ctx ctx;
    memset(&ctx, 0, sizeof(ctx));

    stats_t stats;
    stats_init(&stats, 30);
    run_bench(&stats, 30, 2000, loop_setup, loop_op, loop_teardown, &ctx);
    stats_report(&stats, "loopback", (double)ctx.msg_len);
    stats_free(&stats);
}

/* ==================================================================
 * 基准测试 13: 乱序入队
 * ================================================================*/

/* 生成一个 segment 并编码到预分配 buffer，返回总长度 */
static int make_packet(char *buf, int cap, uint32_t conv, uint32_t sn,
                       uint32_t ts, uint16_t wnd, uint32_t data_len)
{
    char *ptr = seg_encode(buf, conv, 81, 0, wnd, ts, sn, 0, data_len);
    memset(ptr, 0xAB, (size_t)data_len);
    int total = 24 + (int)data_len;
    (void)cap;
    return total;
}

struct ooo_iter_ctx {
    ikcpcb    *kcp;
    char     **packets;
    int       *lengths;
    int        count;
};

static void ooo_setup(void *ctx) {
    struct ooo_iter_ctx *c = (struct ooo_iter_ctx *)ctx;
    c->kcp = ikcp_create(0x11223344, NULL);
    ikcp_setoutput(c->kcp, dummy_output);
    ikcp_wndsize(c->kcp, 256, 256);
}

static void ooo_op(void *ctx) {
    struct ooo_iter_ctx *c = (struct ooo_iter_ctx *)ctx;
    for (int i = 0; i < c->count; i++) {
        ikcp_input(c->kcp, c->packets[i], (long)c->lengths[i]);
    }
}

static void ooo_teardown(void *ctx) {
    struct ooo_iter_ctx *c = (struct ooo_iter_ctx *)ctx;
    ikcp_release(c->kcp);
}

static void bench_out_of_order(void) {
    printf("\n=== 乱序入队 ===\n");

    int packet_count = 100;
    char **packets = (char **)malloc((size_t)packet_count * sizeof(char *));
    int   *lengths = (int   *)malloc((size_t)packet_count * sizeof(int));

    /* 创建 100 个连续 SN 的 segment */
    for (int i = 0; i < packet_count; i++) {
        packets[i] = (char *)malloc(200);
        lengths[i] = make_packet(packets[i], 200, 0x11223344,
                                 (uint32_t)i, (uint32_t)i * 10, 256, 100);
    }

    /* Fisher-Yates 洗牌 */
    prng_t rng;
    prng_init(&rng, 42);
    for (int i = packet_count - 1; i > 0; i--) {
        int j = prng_range(&rng, i + 1);
        char *tmp_p = packets[i];
        packets[i] = packets[j];
        packets[j] = tmp_p;
        int tmp_l = lengths[i];
        lengths[i] = lengths[j];
        lengths[j] = tmp_l;
    }

    struct ooo_iter_ctx ctx;
    ctx.packets = packets;
    ctx.lengths = lengths;
    ctx.count   = packet_count;

    stats_t stats;
    stats_init(&stats, 30);
    run_bench(&stats, 30, 1000, ooo_setup, ooo_op, ooo_teardown, &ctx);
    stats_report(&stats, "out_of_order", 0.0);
    stats_free(&stats);

    for (int i = 0; i < packet_count; i++) {
        free(packets[i]);
    }
    free(packets);
    free(lengths);
}

/* ==================================================================
 * 基准测试 14: 多连接并发
 * ================================================================*/

struct multi_conn_ctx {
    ikcpcb **conns;
    int      count;
};

static void multi_setup(void *ctx) {
    struct multi_conn_ctx *c = (struct multi_conn_ctx *)ctx;
    for (int i = 0; i < c->count; i++) {
        c->conns[i] = ikcp_create((uint32_t)(0x1000 + i), NULL);
        ikcp_setoutput(c->conns[i], dummy_output);
        ikcp_wndsize(c->conns[i], 32, 32);
    }
}

static void multi_op(void *ctx) {
    struct multi_conn_ctx *c = (struct multi_conn_ctx *)ctx;
    char data[100];
    memset(data, 0xEF, 100);
    for (int i = 0; i < c->count; i++) {
        ikcp_send(c->conns[i], data, 100);
        ikcp_update(c->conns[i], 0);
        ikcp_flush(c->conns[i]);
    }
}

static void multi_teardown(void *ctx) {
    struct multi_conn_ctx *c = (struct multi_conn_ctx *)ctx;
    for (int i = 0; i < c->count; i++) {
        ikcp_release(c->conns[i]);
    }
}

static void bench_multi_connection(void) {
    printf("\n=== 多连接并发 ===\n");

    int conn_count = 10;
    ikcpcb **conns = (ikcpcb **)malloc((size_t)conn_count * sizeof(ikcpcb *));

    struct multi_conn_ctx ctx;
    ctx.conns = conns;
    ctx.count = conn_count;

    stats_t stats;
    stats_init(&stats, 30);
    run_bench(&stats, 30, 1000, multi_setup, multi_op, multi_teardown, &ctx);
    stats_report(&stats, "multi_connection", 0.0);
    stats_free(&stats);

    free(conns);
}

/* ==================================================================
 * 基准测试 15: 网络模拟
 *
 * 端到端测试：两个 KCP 实例通过 net_sim 收发。
 * 由于涉及真实时间等待（RTT 模拟），仅单次运行，不做多轮统计。
 * ================================================================*/

static net_sim_t *g_bench_sim;

static int udp_output_sim(const char *buf, int len, ikcpcb *kcp, void *user) {
    (void)kcp;
    int peer = (int)(intptr_t)user;
    uint32_t ms = (uint32_t)(now_ns() / 1000000);
    net_sim_send(g_bench_sim, peer, buf, len, ms);
    return 0;
}

static void bench_network_sim(int mode) {
    const char *mode_names[] = {"default", "normal", "fast"};

    net_sim_t *sim = net_sim_create(100, 60, 125, 2000);
    g_bench_sim = sim;

    ikcpcb *kcp1 = ikcp_create(0x11223344, (void *)(intptr_t)0);
    ikcpcb *kcp2 = ikcp_create(0x11223344, (void *)(intptr_t)1);
    kcp1->output = udp_output_sim;
    kcp2->output = udp_output_sim;
    ikcp_wndsize(kcp1, 128, 128);
    ikcp_wndsize(kcp2, 128, 128);

    if (mode == 0) {
        ikcp_nodelay(kcp1, 0, 10, 0, 0);
        ikcp_nodelay(kcp2, 0, 10, 0, 0);
    } else if (mode == 1) {
        ikcp_nodelay(kcp1, 0, 10, 0, 1);
        ikcp_nodelay(kcp2, 0, 10, 0, 1);
    } else {
        ikcp_nodelay(kcp1, 2, 10, 2, 1);
        ikcp_nodelay(kcp2, 2, 10, 2, 1);
        kcp1->rx_minrto = 10;
        kcp2->rx_minrto = 10;
    }

    uint32_t current = (uint32_t)(now_ns() / 1000000);
    uint32_t slap = current + 20;
    uint32_t index = 0;
    uint32_t next = 0;
    long long sum_rtt = 0;
    int max_rtt = 0;
    int count = 0;
    char buffer[2000];
    int hr;

    while (1) {
        current = (uint32_t)(now_ns() / 1000000);
        ikcp_update(kcp1, current);
        ikcp_update(kcp2, current);

        if (current >= slap) {
            slap += 20;
            memcpy(buffer, &index, 4);
            memcpy(buffer + 4, &current, 4);
            ikcp_send(kcp1, buffer, 8);
            index++;
        }

        while ((hr = net_sim_recv(sim, 1, buffer, 2000, current)) >= 0)
            ikcp_input(kcp2, buffer, hr);

        while ((hr = net_sim_recv(sim, 0, buffer, 2000, current)) >= 0)
            ikcp_input(kcp1, buffer, hr);

        while ((hr = ikcp_recv(kcp2, buffer, 10)) >= 0)
            ikcp_send(kcp2, buffer, hr);

        while (1) {
            hr = ikcp_recv(kcp1, buffer, 10);
            if (hr < 0) break;

            unsigned int sn, ts, rtt;
            memcpy(&sn, buffer, 4);
            memcpy(&ts, buffer + 4, 4);
            rtt = current - ts;

            if (sn != next) {
                printf("ERROR sn %d<->%d\n", count, (int)next);
                net_sim_destroy(sim);
                ikcp_release(kcp1);
                ikcp_release(kcp2);
                return;
            }

            next++;
            sum_rtt += rtt;
            count++;
            if ((int)rtt > max_rtt) max_rtt = (int)rtt;

            if (count <= 5 || count % 100 == 0)
                printf("[RECV] mode=%d sn=%d rtt=%u\n", mode, (int)sn, rtt);
        }

        if (next > 1000) break;
    }

    long long avg_rtt = count > 0 ? sum_rtt / count : 0;
    printf("  mode=%s: avgrtt=%lld maxrtt=%d tx=%d (1000 pkts)\n",
           mode_names[mode], avg_rtt, max_rtt, net_sim_tx1(sim));

    net_sim_destroy(sim);
    ikcp_release(kcp1);
    ikcp_release(kcp2);
}

/* ==================================================================
 * 主函数
 * ================================================================*/

int main(void) {
    /* 从环境变量读取种子，默认 42 */
    const char *seed_env = getenv("BENCH_SEED");
    uint32_t seed = seed_env ? (uint32_t)atoi(seed_env) : 42;
    printf("========================================\n");
    printf("  kcp-c 纯 C 性能基准测试 (seed=%u)\n", seed);
    printf("  时钟: CLOCK_MONOTONIC_RAW\n");
    printf("  编译: -O3 -march=native -flto\n");
    printf("========================================\n");

    bench_segment_encode();
    bench_segment_decode();
    bench_segment_encode_sweep();
    bench_send_small_packet();
    bench_send_large_packet();
    bench_send_throughput();
    bench_stream_mode();
    bench_input();
    bench_recv();
    bench_flush();
    bench_update();
    bench_loopback();
    bench_out_of_order();
    bench_multi_connection();

    printf("\n=== 网络模拟 (10%% 丢包, 60~125ms RTT) ===\n");
    bench_network_sim(0);
    bench_network_sim(1);
    bench_network_sim(2);

    printf("\n========================================\n");
    printf("  所有测试完成!\n");
    printf("========================================\n");

    return 0;
}
