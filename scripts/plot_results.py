#!/usr/bin/env python3
"""
KCP 性能对比结果绘图脚本
读取 results/*.txt 文件，生成对比图表保存到 results/charts/
"""

import os
import re
import math
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import matplotlib.font_manager as fm
import numpy as np

RESULTS_DIR = os.path.join(os.path.dirname(__file__), '..', 'results')
CHARTS_DIR = os.path.join(RESULTS_DIR, 'charts')
os.makedirs(CHARTS_DIR, exist_ok=True)

# ---------- 中文字体 ----------
for font_path in [
    '/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc',
    '/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc',
]:
    if os.path.exists(font_path):
        fm.fontManager.addfont(font_path)
        break

plt.rcParams.update({
    'font.sans-serif': ['Noto Sans CJK SC', 'Noto Sans CJK JP', 'WenQuanYi WenQuanYi Bitmap Song', 'DejaVu Sans'],
    'font.family': 'sans-serif',
    'axes.unicode_minus': False,
    'figure.dpi': 150,
    'font.size': 11,
    'axes.titlesize': 13,
    'axes.labelsize': 11,
    'legend.fontsize': 9,
    'xtick.labelsize': 9,
    'ytick.labelsize': 9,
    'figure.figsize': (10, 5),
})
COLORS = {'C (ikcp)': '#E74C3C', 'Rust (kcp2)': '#3498DB', 'C++ (test.cpp)': '#E67E22'}
HATCHES = {'C (ikcp)': '', 'Rust (kcp2)': '///', 'C++ (test.cpp)': '\\\\'}


# ===================== 数据解析 =====================

def parse_c_bench(path):
    """解析 c_bench_results.txt"""
    data = {}
    with open(path) as f:
        text = f.read()

    # 测试1: Segment
    m = re.search(r'每次操作:\s*([\d.]+)\s*ns', text)
    if m: data['segment_codec_ns'] = float(m.group(1))

    # 测试2: 发送吞吐量
    m = re.search(r'小数据包.*?吞吐量:\s*([\d.]+)\s*MB/s', text, re.DOTALL)
    if m: data['send_100b_mbs'] = float(m.group(1))
    m = re.search(r'大数据包.*?吞吐量:\s*([\d.]+)\s*MB/s', text, re.DOTALL)
    if m: data['send_10kb_mbs'] = float(m.group(1))

    # 测试3: Input
    m = re.search(r'每次 input:\s*([\d.]+)\s*ns', text)
    if m: data['input_ns'] = float(m.group(1))
    m = re.search(r'吞吐量:\s*([\d.]+)\s*MB/s', text)
    if m: data['input_mbs'] = float(m.group(1))

    # 测试4: 回环
    m = re.search(r'每次回环:\s*([\d.]+)\s*ns', text)
    if m: data['loopback_ns'] = float(m.group(1))
    m = re.search(r'测试4.*?吞吐量:\s*([\d.]+)\s*MB/s', text, re.DOTALL)
    if m: data['loopback_mbs'] = float(m.group(1))

    # 测试5: 多连接
    m = re.search(r'每个数据包:\s*([\d.]+)\s*ns', text)
    if m: data['multi_pkt_ns'] = float(m.group(1))

    # 测试7: 多尺寸发送
    data['send_sweep'] = {}
    for size in [64, 256, 1024, 1400]:
        m = re.search(rf'send/{size}:\s*([\d.]+)\s*MB/s', text)
        if m: data['send_sweep'][size] = float(m.group(1))

    # 测试6: 网络模拟
    data['net_sim'] = {}
    for mode, name in [(0, 'default'), (1, 'normal'), (2, 'fast')]:
        pattern = rf'{name} mode result.*?avgrtt=(\d+)\s+maxrtt=(\d+)\s+tx=(\d+)'
        m = re.search(pattern, text, re.DOTALL)
        if m:
            data['net_sim'][mode] = {
                'name': name,
                'avgrtt': int(m.group(1)),
                'maxrtt': int(m.group(2)),
                'tx': int(m.group(3)),
            }
    return data


def parse_rust_perf(path):
    """解析 rust_perf_test_results.txt"""
    data = {}
    with open(path) as f:
        text = f.read()

    m = re.search(r'每次操作:\s*([\d.]+)\s*ns', text)
    if m: data['segment_codec_ns'] = float(m.group(1))

    m = re.search(r'小数据包.*?吞吐量:\s*([\d.]+)\s*MB/s', text, re.DOTALL)
    if m: data['send_100b_mbs'] = float(m.group(1))
    m = re.search(r'大数据包.*?吞吐量:\s*([\d.]+)\s*MB/s', text, re.DOTALL)
    if m: data['send_10kb_mbs'] = float(m.group(1))

    m = re.search(r'每次回环:\s*([\d.]+)\s*ns', text)
    if m: data['loopback_ns'] = float(m.group(1))
    m = re.search(r'回环.*?吞吐量:\s*([\d.]+)\s*MB/s', text, re.DOTALL)
    if m: data['loopback_mbs'] = float(m.group(1))

    m = re.search(r'每个数据包:\s*([\d.]+)\s*ns(?!.*连接)', text)
    if m: data['input_pkt_ns'] = float(m.group(1))

    m = re.search(r'多连接.*?每个数据包:\s*([\d.]+)\s*ns', text, re.DOTALL)
    if m: data['multi_pkt_ns'] = float(m.group(1))

    return data


def parse_rust_bench(path):
    """解析 rust_kcp_bench_results.txt (criterion)"""
    data = {}
    with open(path) as f:
        text = f.read()

    # segment_encode
    m = re.search(r'segment_encode\s+time:\s+\[[\d.]+\s*ns\s+([\d.]+)\s*ns', text)
    if m: data['segment_encode_ns'] = float(m.group(1))

    m = re.search(r'segment_decode\s+time:\s+\[[\d.]+\s*ns\s+([\d.]+)\s*ns', text)
    if m: data['segment_decode_ns'] = float(m.group(1))

    # send_small/large
    m = re.search(r'send_small_packet.*?time:\s+\[[\d.]+\s*ns\s+([\d.]+)\s*ns', text, re.DOTALL)
    if m: data['send_small_pkt_ns'] = float(m.group(1))

    m = re.search(r'send_large_packet.*?time:\s+\[[\d.]+\s*ns\s+([\d.]+)\s*ns', text, re.DOTALL)
    if m: data['send_large_pkt_ns'] = float(m.group(1))

    # send throughput by size (GiB/s -> MB/s)
    data['send_thrpt'] = {}
    for size in [64, 256, 1024, 1400]:
        m = re.search(rf'send/{size}.*?thrpt:\s+\[([\d.]+)\s+GiB/s', text, re.DOTALL)
        if m:
            data['send_thrpt'][size] = float(m.group(1)) * 1024  # GiB/s -> MB/s

    # input
    m = re.search(r'input_throughput/input.*?time:\s+\[[\d.]+\s*ns\s+([\d.]+)\s*ns', text, re.DOTALL)
    if m: data['input_ns'] = float(m.group(1))
    m = re.search(r'input_throughput/input.*?thrpt:\s+\[([\d.]+)\s+MiB/s', text, re.DOTALL)
    if m: data['input_mbs'] = float(m.group(1))

    # recv
    m = re.search(r'recv_throughput/recv.*?time:\s+\[[\d.]+\s*ns\s+([\d.]+)\s*ns', text, re.DOTALL)
    if m: data['recv_ns'] = float(m.group(1))

    # flush / update
    m = re.search(r'^flush\s+time:\s+\[[\d.]+\s*ns\s+([\d.]+)\s*ns', text, re.MULTILINE)
    if m: data['flush_ns'] = float(m.group(1))
    m = re.search(r'^update\s+time:\s+\[[\d.]+\s*ns\s+([\d.]+)\s*ns', text, re.MULTILINE)
    if m: data['update_ns'] = float(m.group(1))

    # loopback
    m = re.search(r'loopback.*?time:\s+\[[\d.]+\s*ns\s+([\d.]+)\s*ns', text, re.DOTALL)
    if m: data['loopback_ns'] = float(m.group(1))

    # out_of_order
    m = re.search(r'out_of_order.*?time:\s+\[[\d.]+\s*µs\s+([\d.]+)\s*µs', text, re.DOTALL)
    if m: data['ooo_us'] = float(m.group(1))

    # multi_connection
    m = re.search(r'multi_connection.*?time:\s+\[[\d.]+\s*µs\s+([\d.]+)\s*µs', text, re.DOTALL)
    if m: data['multi_us'] = float(m.group(1))

    return data


# ===================== 图表绘制 =====================

def chart_send_throughput(c_data, r_perf):
    """Chart 1: Send throughput (small/large) C vs Rust"""
    labels = ['100B 小包', '10KB 大包']
    c_vals = [c_data.get('send_100b_mbs', 0), c_data.get('send_10kb_mbs', 0)]
    r_vals = [r_perf.get('send_100b_mbs', 0), r_perf.get('send_10kb_mbs', 0)]

    x = np.arange(len(labels))
    w = 0.3

    fig, ax = plt.subplots()
    bars1 = ax.bar(x - w/2, c_vals, w, label='C (ikcp)', color=COLORS['C (ikcp)'])
    bars2 = ax.bar(x + w/2, r_vals, w, label='Rust (kcp2)', color=COLORS['Rust (kcp2)'])

    ax.set_ylabel('吞吐量 (MB/s)')
    ax.set_title('KCP 发送吞吐量对比: C vs Rust')
    ax.set_xticks(x)
    ax.set_xticklabels(labels)
    ax.legend()

    for bar in bars1:
        ax.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 20,
                f'{bar.get_height():.0f}', ha='center', va='bottom', fontsize=8)
    for bar in bars2:
        ax.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 20,
                f'{bar.get_height():.0f}', ha='center', va='bottom', fontsize=8)

    fig.tight_layout()
    fig.savefig(os.path.join(CHARTS_DIR, '01_send_throughput.png'))
    plt.close(fig)
    print('  [OK] 01_send_throughput.png')


def chart_multi_size_send(c_data, r_bench):
    """Chart 2: Multi-size send throughput"""
    sizes = [64, 256, 1024, 1400]
    c_vals = [c_data.get('send_sweep', {}).get(s, 0) for s in sizes]

    # Rust: use criterion bench data
    r_vals = []
    for s in sizes:
        v = r_bench.get('send_thrpt', {}).get(s, 0)
        r_vals.append(v)

    x = np.arange(len(sizes))
    w = 0.3

    fig, ax = plt.subplots()
    bars1 = ax.bar(x - w/2, c_vals, w, label='C (ikcp)', color=COLORS['C (ikcp)'])
    bars2 = ax.bar(x + w/2, r_vals, w, label='Rust (kcp2)', color=COLORS['Rust (kcp2)'])

    ax.set_ylabel('吞吐量 (MB/s)')
    ax.set_title('多尺寸发送吞吐量对比')
    ax.set_xticks(x)
    ax.set_xticklabels([f'{s}B' for s in sizes])
    ax.legend()

    for bar in bars1:
        ax.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 50,
                f'{bar.get_height():.0f}', ha='center', va='bottom', fontsize=7, rotation=45)
    for bar in bars2:
        ax.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 50,
                f'{bar.get_height():.0f}', ha='center', va='bottom', fontsize=7, rotation=45)

    fig.tight_layout()
    fig.savefig(os.path.join(CHARTS_DIR, '02_multi_size_send.png'))
    plt.close(fig)
    print('  [OK] 02_multi_size_send.png')


def chart_latency_comparison(c_data, r_perf, r_bench):
    """Chart 3: Latency comparison (Input, Loopback, Recv, Flush, Update)"""
    metrics = ['Input', 'Loopback', 'Recv', 'Flush', 'Update']

    c_vals = [
        c_data.get('input_ns', 0) or 0,
        c_data.get('loopback_ns', 0) or 0,
        0, 0, 0,  # No C equivalents
    ]
    r_vals = [
        r_bench.get('input_ns', 0) or r_perf.get('input_pkt_ns', 0),
        r_bench.get('loopback_ns', 0) or r_perf.get('loopback_ns', 0),
        r_bench.get('recv_ns', 0),
        r_bench.get('flush_ns', 0),
        r_bench.get('update_ns', 0),
    ]
    labels = ['Input', 'Loopback', 'Recv', 'Flush', 'Update']

    x = np.arange(len(labels))
    w = 0.3

    fig, ax = plt.subplots()
    c_plot = [c_vals[i] if c_vals[i] > 0 else None for i in range(len(c_vals))]
    r_plot = r_vals

    bars1 = ax.bar(x - w/2, [v if v else 0 for v in c_vals], w,
                   label='C (ikcp)', color=COLORS['C (ikcp)'])
    bars2 = ax.bar(x + w/2, r_vals, w, label='Rust (kcp2)', color=COLORS['Rust (kcp2)'])

    ax.set_ylabel('延迟 (ns)')
    ax.set_title('协议操作延迟对比 (越低越好)')
    ax.set_xticks(x)
    ax.set_xticklabels(labels)
    ax.legend()

    for i, bar in enumerate(bars1):
        if c_vals[i] > 0:
            ax.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 5,
                    f'{c_vals[i]:.1f}', ha='center', va='bottom', fontsize=8)
    for i, bar in enumerate(bars2):
        ax.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 5,
                f'{r_vals[i]:.1f}', ha='center', va='bottom', fontsize=8)

    fig.tight_layout()
    fig.savefig(os.path.join(CHARTS_DIR, '03_latency_comparison.png'))
    plt.close(fig)
    print('  [OK] 03_latency_comparison.png')


def chart_loopback_throughput(c_data, r_perf):
    """Chart 4: Loopback throughput"""
    fig, ax = plt.subplots()
    labels = ['C (ikcp)', 'Rust (kcp2)']
    vals = [c_data.get('loopback_mbs', 0), r_perf.get('loopback_mbs', 0)]
    colors = [COLORS['C (ikcp)'], COLORS['Rust (kcp2)']]

    bars = ax.bar(labels, vals, color=colors, width=0.4)
    ax.set_ylabel('吞吐量 (MB/s)')
    ax.set_title('回环通信吞吐量对比')

    for bar in bars:
        ax.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 1,
                f'{bar.get_height():.1f} MB/s', ha='center', va='bottom', fontsize=10)

    fig.tight_layout()
    fig.savefig(os.path.join(CHARTS_DIR, '04_loopback_throughput.png'))
    plt.close(fig)
    print('  [OK] 04_loopback_throughput.png')


def chart_multi_connection(c_data, r_perf):
    """Chart 5: Multi-connection comparison"""
    fig, ax = plt.subplots()
    labels = ['C (ikcp)', 'Rust (kcp2)']
    vals = [c_data.get('multi_pkt_ns', 0), r_perf.get('multi_pkt_ns', 0)]
    colors = [COLORS['C (ikcp)'], COLORS['Rust (kcp2)']]

    bars = ax.bar(labels, vals, color=colors, width=0.4)
    ax.set_ylabel('每数据包耗时 (ns)')
    ax.set_title('多连接并发 (100连接 × 10包) 对比\n越低越好')

    for bar in bars:
        ax.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 5,
                f'{bar.get_height():.1f} ns', ha='center', va='bottom', fontsize=10)

    fig.tight_layout()
    fig.savefig(os.path.join(CHARTS_DIR, '05_multi_connection.png'))
    plt.close(fig)
    print('  [OK] 05_multi_connection.png')


def chart_network_simulation(c_data):
    """Chart 6: Network simulation RTT comparison"""
    modes = ['Default', 'Normal', 'Fast']

    c_avgrtt = [c_data['net_sim'][m]['avgrtt'] for m in [0, 1, 2]]
    c_maxrtt = [c_data['net_sim'][m]['maxrtt'] for m in [0, 1, 2]]

    x = np.arange(len(modes))
    w = 0.3

    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(12, 5))

    # Left: avg RTT
    bars1 = ax1.bar(x, c_avgrtt, w, color=COLORS['C (ikcp)'])
    ax1.set_ylabel('平均 RTT (ms)')
    ax1.set_title('网络模拟: 平均 RTT')
    ax1.set_xticks(x)
    ax1.set_xticklabels(modes)
    for bar in bars1:
        ax1.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 5,
                 f'{bar.get_height()}ms', ha='center', va='bottom', fontsize=9)

    # Right: max RTT
    bars2 = ax2.bar(x, c_maxrtt, w, color=COLORS['C (ikcp)'])
    ax2.set_ylabel('最大 RTT (ms)')
    ax2.set_title('网络模拟: 最大 RTT')
    ax2.set_xticks(x)
    ax2.set_xticklabels(modes)
    for bar in bars2:
        ax2.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 5,
                 f'{bar.get_height()}ms', ha='center', va='bottom', fontsize=9)

    fig.suptitle('C (bench.c) 网络模拟 RTT 统计 (10%丢包, 60~125ms RTT)', fontsize=13)
    fig.tight_layout()
    fig.savefig(os.path.join(CHARTS_DIR, '06_network_sim_rtt.png'))
    plt.close(fig)
    print('  [OK] 06_network_sim_rtt.png')


def chart_segment_codec(c_data, r_perf, r_bench):
    """Chart 7: Segment codec comparison"""
    fig, ax = plt.subplots()

    # C test 1: ikcp_send (includes alloc + queue)
    # Rust perf test 1: Segment::encode + Segment::decode
    # Rust bench: segment_encode, segment_decode separately
    labels = ['Send/Encode\n(整体)', 'Encode\n(纯编码)', 'Decode\n(纯解码)']
    c_val = c_data.get('segment_codec_ns', 0)
    r_perf_val = r_perf.get('segment_codec_ns', 0)
    r_enc = r_bench.get('segment_encode_ns', 0)
    r_dec = r_bench.get('segment_decode_ns', 0)

    x = np.arange(len(labels))
    w = 0.3

    # For 'Send/Encode' we have C and Rust perf_test, for Encode/Decode we only have Rust bench
    c_group = [c_val, 0, 0]
    r_group = [r_perf_val, r_enc, r_dec]

    bars1 = ax.bar(x - w/2, [c_group[0], 0, 0], w,
                   label='C (ikcp)', color=COLORS['C (ikcp)'])
    bars2 = ax.bar(x + w/2, r_group, w,
                   label='Rust (kcp2)', color=COLORS['Rust (kcp2)'])

    # Stack bar for C with N/A for columns 2,3
    for i in [1, 2]:
        ax.text(x[i] - w/2, 1, 'N/A', ha='center', va='bottom', fontsize=8, color='gray', fontstyle='italic')

    ax.set_ylabel('耗时 (ns)')
    ax.set_title('Segment 编码/解码延迟对比 (越低越好)')
    ax.set_xticks(x)
    ax.set_xticklabels(labels)
    ax.legend()

    for bar in bars1:
        if bar.get_height() > 0:
            ax.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 0.3,
                    f'{bar.get_height():.1f} ns', ha='center', va='bottom', fontsize=9)
    for bar in bars2:
        if bar.get_height() > 0:
            ax.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 0.3,
                    f'{bar.get_height():.1f} ns', ha='center', va='bottom', fontsize=9)

    fig.tight_layout()
    fig.savefig(os.path.join(CHARTS_DIR, '07_segment_codec.png'))
    plt.close(fig)
    print('  [OK] 07_segment_codec.png')


def chart_overall_radar(c_data, r_perf, r_bench):
    """Chart 8: Overall performance radar (normalized)"""
    categories = ['Send\n小包', 'Send\n大包', 'Loopback\n吞吐', '多连接\n效率', 'Input\n延迟']

    # Higher is better -> normalize to [0, 1]
    c_send_small = c_data.get('send_100b_mbs', 1) / 2000
    r_send_small = r_perf.get('send_100b_mbs', 1) / 2000

    c_send_large = c_data.get('send_10kb_mbs', 1) / 4000
    r_send_large = r_perf.get('send_10kb_mbs', 1) / 4000

    c_loopback = c_data.get('loopback_mbs', 1) / 150
    r_loopback = r_perf.get('loopback_mbs', 1) / 150

    # Multi-connection efficiency (lower ns = better, invert)
    c_multi = max(0, 1 - c_data.get('multi_pkt_ns', 0) / 600)
    r_multi = max(0, 1 - r_perf.get('multi_pkt_ns', 0) / 600)

    # Input latency (lower ns = better, invert)
    c_input = max(0, 1 - c_data.get('input_ns', 0) / 300)
    r_input = max(0, 1 - r_bench.get('input_ns', 200) / 300)

    c_vals = [c_send_small, c_send_large, c_loopback, c_multi, c_input]
    r_vals = [r_send_small, r_send_large, r_loopback, r_multi, r_input]

    # Clamp to [0, 1]
    c_vals = [max(0, min(1, v)) for v in c_vals]
    r_vals = [max(0, min(1, v)) for v in r_vals]

    n = len(categories)
    angles = np.linspace(0, 2 * np.pi, n, endpoint=False).tolist()
    angles += angles[:1]
    c_vals += c_vals[:1]
    r_vals += r_vals[:1]

    fig, ax = plt.subplots(figsize=(7, 7), subplot_kw=dict(polar=True))
    ax.plot(angles, c_vals, 'o-', linewidth=2, label='C (ikcp)', color=COLORS['C (ikcp)'])
    ax.fill(angles, c_vals, alpha=0.1, color=COLORS['C (ikcp)'])
    ax.plot(angles, r_vals, 'o-', linewidth=2, label='Rust (kcp2)', color=COLORS['Rust (kcp2)'])
    ax.fill(angles, r_vals, alpha=0.1, color=COLORS['Rust (kcp2)'])

    ax.set_xticks(angles[:-1])
    ax.set_xticklabels(categories, fontsize=10)
    ax.set_ylim(0, 1)
    ax.set_title('C vs Rust 综合性能雷达图\n(数值越大越好)', fontsize=13, pad=20)
    ax.legend(loc='upper right', bbox_to_anchor=(1.2, 1.1))

    fig.tight_layout()
    fig.savefig(os.path.join(CHARTS_DIR, '08_overall_radar.png'))
    plt.close(fig)
    print('  [OK] 08_overall_radar.png')


def chart_criterion_bench(r_bench):
    """Chart 9: Rust criterion benchmark summary"""
    fig, axes = plt.subplots(1, 2, figsize=(12, 5))

    # Left: Protocol operation latency
    ops = ['Encode', 'Decode', 'Send(64B)', 'Send(1400B)', 'Input', 'Recv', 'Flush', 'Update', 'Loopback']
    vals = [
        r_bench.get('segment_encode_ns', 0),
        r_bench.get('segment_decode_ns', 0),
        r_bench.get('send_thrpt', {}).get(64, 0) / 1024 * 1000,  # Convert back to ns for consistency
        r_bench.get('send_thrpt', {}).get(1400, 0) / 1024 * 1000,
        r_bench.get('input_ns', 0),
        r_bench.get('recv_ns', 0),
        r_bench.get('flush_ns', 0),
        r_bench.get('update_ns', 0),
        r_bench.get('loopback_ns', 0),
    ]

    # Actually for send it's better to show throughput not latency
    axes[0].barh(ops, vals, color=COLORS['Rust (kcp2)'])
    axes[0].set_xlabel('耗时 (ns)')
    axes[0].set_title('Rust (kcp2) 协议操作延迟')

    # Right: Throughput
    thrpt_items = [
        ('segment_encode/0', r_bench.get('segment_encode_ns', 0)),
        ('send/64', r_bench.get('send_thrpt', {}).get(64, 0)),
        ('send/256', r_bench.get('send_thrpt', {}).get(256, 0)),
        ('send/1024', r_bench.get('send_thrpt', {}).get(1024, 0)),
        ('send/1400', r_bench.get('send_thrpt', {}).get(1400, 0)),
        ('input', r_bench.get('input_mbs', 0)),
    ]

    thrpt_labels = [t[0] for t in thrpt_items]
    thrpt_vals = [t[1] for t in thrpt_items]

    bars = axes[1].barh(thrpt_labels, thrpt_vals, color=COLORS['Rust (kcp2)'])
    axes[1].set_xlabel('吞吐量 (MB/s)')
    axes[1].set_title('Rust (kcp2) 吞吐量')
    for bar, v in zip(bars, thrpt_vals):
        if v > 0:
            axes[1].text(bar.get_width() + 50, bar.get_y() + bar.get_height()/2,
                         f'{v:.0f}', ha='left', va='center', fontsize=7)

    fig.tight_layout()
    fig.savefig(os.path.join(CHARTS_DIR, '09_rust_bench_summary.png'))
    plt.close(fig)
    print('  [OK] 09_rust_bench_summary.png')


def chart_network_rtt_timeline():
    """Chart 10: RTT timeline from network sim (C bench.c)"""
    path = os.path.join(RESULTS_DIR, 'c_bench_results.txt')
    with open(path) as f:
        text = f.read()

    fig, axes = plt.subplots(3, 1, figsize=(10, 10), sharex=True)
    mode_names = ['default', 'normal', 'fast']

    for mode_idx in range(3):
        # Extract RTT data for this mode
        pattern = rf'\[RECV\] mode={mode_idx} sn=\d+ rtt=(\d+)'
        matches = re.findall(pattern, text)
        if not matches:
            continue
        rtts = [int(m) for m in matches]
        sns = list(range(len(rtts)))

        ax = axes[mode_idx]
        ax.plot(sns, rtts, '-', color=['#E74C3C', '#3498DB', '#2ECC71'][mode_idx],
                linewidth=0.8, alpha=0.7)
        ax.scatter(sns, rtts, s=8, color=['#E74C3C', '#3498DB', '#2ECC71'][mode_idx], alpha=0.5)

        # Calculate running average
        window = 10
        if len(rtts) >= window:
            running_avg = np.convolve(rtts, np.ones(window)/window, mode='valid')
            ax.plot(sns[window-1:], running_avg, 'k-', linewidth=1.5, alpha=0.8, label=f'移动平均 (n={window})')
            ax.legend(fontsize=8)

        ax.set_ylabel('RTT (ms)')
        ax.set_title(f'网络模拟: {mode_names[mode_idx]} 模式 RTT 时序')
        ax.grid(True, alpha=0.3)

    axes[-1].set_xlabel('数据包序号')
    fig.tight_layout()
    fig.savefig(os.path.join(CHARTS_DIR, '10_network_rtt_timeline.png'))
    plt.close(fig)
    print('  [OK] 10_network_rtt_timeline.png')


# ===================== 主流程 =====================

def main():
    print('=' * 50)
    print('KCP 性能对比结果绘图')
    print('=' * 50)

    # 解析数据
    c_path = os.path.join(RESULTS_DIR, 'c_bench_results.txt')
    r_perf_path = os.path.join(RESULTS_DIR, 'rust_perf_test_results.txt')
    r_bench_path = os.path.join(RESULTS_DIR, 'rust_kcp_bench_results.txt')

    c_data = parse_c_bench(c_path)
    r_perf = parse_rust_perf(r_perf_path)
    r_bench = parse_rust_bench(r_bench_path)

    print(f'\n  C 数据: {len(c_data)} 项')
    print(f'  Rust perf: {len(r_perf)} 项')
    print(f'  Rust bench: {len(r_bench)} 项')

    # 生成图表
    print('\n生成图表...')
    chart_send_throughput(c_data, r_perf)
    chart_multi_size_send(c_data, r_bench)
    chart_latency_comparison(c_data, r_perf, r_bench)
    chart_loopback_throughput(c_data, r_perf)
    chart_multi_connection(c_data, r_perf)
    chart_network_simulation(c_data)
    chart_segment_codec(c_data, r_perf, r_bench)
    chart_overall_radar(c_data, r_perf, r_bench)
    chart_criterion_bench(r_bench)
    chart_network_rtt_timeline()

    print(f'\n所有图表已保存至: {CHARTS_DIR}/')
    print(f'共生成 10 个图表文件')
    print('完成!')


if __name__ == '__main__':
    main()
