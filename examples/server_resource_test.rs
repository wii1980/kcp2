//! KCP server resource consumption test with rrdtool graph generation.
//!
//! **Platform: Linux only** — uses `/proc/self/{status,stat}` and `libc::sysconf`.
//!
//! Phases: idle → N real echo connections (with traffic) → disconnect → recovery
//!
//! Usage:
//!   cargo run --example server_resource_test [conns] [total_secs]
//!
//!   conns      — number of client connections (default: 100)
//!   total_secs — approximate total test duration (default: 60)
//!
//! Phase allocation: ~15% idle, ~60% loaded with traffic, ~25% recovery
//!
//! Output (under target/):
//!   server_resource_test.rrd     — raw RRD data
//!   server_perf_main.png         — Connections & CPU%
//!   server_perf_memory.png       — Memory (RSS)

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kcp2::{KcpConfig, KcpConnector, KcpListener};
use tokio::time::sleep;

// ── /proc metrics ───────────────────────────────────────────────────────────

struct ProcMetrics {
    rss_kb: u64,
    cpu_user_ms: u64,
    cpu_sys_ms: u64,
}

impl ProcMetrics {
    #[allow(unsafe_code)]
    fn read() -> Self {
        let mut rss_kb = 0u64;
        let mut cpu_user_ms = 0u64;
        let mut cpu_sys_ms = 0u64;

        if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
            for line in s.lines() {
                if let Some(v) = line.strip_prefix("VmRSS:") {
                    rss_kb = v.split_whitespace().next().and_then(|x| x.parse().ok()).unwrap_or(0);
                }
            }
        }

        if let Ok(s) = std::fs::read_to_string("/proc/self/stat") {
            let f: Vec<&str> = s.split_whitespace().collect();
            let tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
            if tck > 0 {
                if let Some(v) = f.get(13).and_then(|x| x.parse::<u64>().ok()) {
                    cpu_user_ms = v * 1000 / tck;
                }
                if let Some(v) = f.get(14).and_then(|x| x.parse::<u64>().ok()) {
                    cpu_sys_ms = v * 1000 / tck;
                }
            }
        }

        Self { rss_kb, cpu_user_ms, cpu_sys_ms }
    }
}

fn now_epoch() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn find_free_addr() -> String {
    let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let a = s.local_addr().unwrap();
    drop(s);
    format!("127.0.0.1:{}", a.port())
}

// ── rrdtool wrapper ─────────────────────────────────────────────────────────

struct Rrd {
    path: PathBuf,
    prev_cpu_user: u64,
    prev_cpu_sys: u64,
    prev_ts: u64,
}

impl Rrd {
    fn create(rrd_path: &Path, total_secs: u64) -> io::Result<Self> {
        if let Some(parent) = rrd_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if rrd_path.exists() {
            std::fs::remove_file(rrd_path)?;
        }

        let hb = 10u64;
        let rows = total_secs + 30;
        let status = Command::new("rrdtool")
            .args([
                "create",
                rrd_path.to_str().unwrap(),
                "--step",
                "1",
                &format!("DS:rss_kb:GAUGE:{hb}:U:U"),
                &format!("DS:conns:GAUGE:{hb}:U:U"),
                &format!("DS:cpu_pct:GAUGE:{hb}:U:U"),
                &format!("RRA:AVERAGE:0.5:1:{rows}"),
                &format!("RRA:MAX:0.5:1:{rows}"),
            ])
            .status()?;
        if !status.success() {
            return Err(io::Error::other("rrdtool create failed"));
        }
        Ok(Self { path: rrd_path.to_owned(), prev_cpu_user: 0, prev_cpu_sys: 0, prev_ts: now_epoch() })
    }

    fn update(&mut self, rss_kb: u64, conns: usize, cpu_user_ms: u64, cpu_sys_ms: u64) {
        let ts = now_epoch();
        let dt_ms = ts.saturating_sub(self.prev_ts).max(1) * 1000;
        let cpu_delta_ms = (cpu_user_ms.saturating_sub(self.prev_cpu_user))
            + (cpu_sys_ms.saturating_sub(self.prev_cpu_sys));
        let cpu_pct = cpu_delta_ms as f64 / dt_ms as f64 * 100.0;
        self.prev_cpu_user = cpu_user_ms;
        self.prev_cpu_sys = cpu_sys_ms;
        self.prev_ts = ts;

        let _ = Command::new("rrdtool")
            .args([
                "update",
                self.path.to_str().unwrap(),
                &format!("{ts}:{rss_kb}:{conns}:{cpu_pct:.2}"),
            ])
            .output();
    }

    fn query_vdef(&self, ds: &str, cf: &str, vdef: &str, start: u64, end: u64) -> f64 {
        let rrd_file = self.path.to_str().unwrap_or("?");
        let out = Command::new("rrdtool")
            .args([
                "graph",
                "/dev/null",
                "--start",
                &start.to_string(),
                "--end",
                &(end + 1).to_string(),
                &format!("DEF:v={rrd_file}:{ds}:{cf}"),
                &format!("VDEF:vval=v,{vdef}"),
                "PRINT:vval:%lf",
            ])
            .output();
        let stdout = match &out {
            Ok(o) => String::from_utf8_lossy(&o.stdout),
            Err(_) => return 100.0,
        };
        for line in stdout.lines().rev() {
            if let Ok(f) = line.trim().parse::<f64>() {
                return f;
            }
        }
        100.0
    }

    fn query_max(&self, ds: &str, start: u64, end: u64) -> f64 {
        self.query_vdef(ds, "MAX", "MAXIMUM", start, end)
    }

    fn query_min(&self, ds: &str, start: u64, end: u64) -> f64 {
        self.query_vdef(ds, "MIN", "MINIMUM", start, end)
    }

    fn render_graphs(&self, out_dir: &Path, start_ts: u64, end_ts: u64) -> io::Result<()> {
        std::fs::create_dir_all(out_dir)?;
        let rrd_file = self.path.to_str().unwrap_or("?");
        let main_png = out_dir.join("server_perf_main.png");
        let mem_png = out_dir.join("server_perf_memory.png");

        // Query max values for dynamic Y-axis scaling
        let cpu_max = self.query_max("cpu_pct", start_ts, end_ts);
        let cpu_upper = ((cpu_max * 1.3).max(1.0)).ceil();
        let conn_max = self.query_max("conns", start_ts, end_ts);
        let conn_upper = ((conn_max * 1.3).max(1.0)).ceil() as u64;

        // Connections + CPU%
        let right_ratio = if conn_upper > 0 { cpu_upper / conn_upper as f64 } else { 1.0 };
        let s1 = Command::new("rrdtool")
            .args([
                "graph",
                main_png.to_str().unwrap(),
                "--start",
                &start_ts.to_string(),
                "--end",
                &(end_ts + 1).to_string(),
                "--width",
                "1200",
                "--height",
                "500",
                "--title",
                "KCP Server - Connections & CPU Usage",
                "--vertical-label",
                "Connections",
                "--right-axis-label",
                "CPU %",
                "--right-axis",
                &format!("{right_ratio}:0"),
                "--right-axis-format",
                "%4.1lf",
                "--units-exponent",
                "0",
                "--upper-limit",
                &conn_upper.to_string(),
                "--rigid",
                "--slope-mode",
                "--watermark",
                "kcp2 server_resource_test",
                "--font",
                "TITLE:14:",
                "--font",
                "WATERMARK:8:",
                &format!("DEF:conns={rrd_file}:conns:AVERAGE"),
                &format!("DEF:cpu={rrd_file}:cpu_pct:MAX"),
                &format!("CDEF:cpu_scaled=cpu,{conn_upper},*,{cpu_upper},/"),
                "AREA:conns#3498DB30:Connections",
                "LINE2:conns#3498DB",
                "GPRINT:conns:MAX:  max %5.0lf",
                "GPRINT:conns:LAST:  now %5.0lf\\j",
                &format!("LINE2:cpu_scaled#E74C3C:CPU % (axis right, max {cpu_upper:.0}%)"),
                "GPRINT:cpu:MAX:  max %6.1lf%%",
                "GPRINT:cpu:LAST:  now %6.1lf%%\\j",
            ])
            .status()?;
        if !s1.success() {
            return Err(io::Error::other("rrdtool graph (main) failed"));
        }

        // Memory (RSS)
        let rss_max = self.query_max("rss_kb", start_ts, end_ts);
        let rss_min = self.query_min("rss_kb", start_ts, end_ts);
        let rss_range = rss_max - rss_min;
        let (rss_div, rss_unit): (f64, &str) = if rss_max >= 1024.0 * 1024.0 * 1024.0 {
            (1024.0 * 1024.0 * 1024.0, "GB")
        } else if rss_max >= 1024.0 * 1024.0 {
            (1024.0 * 1024.0, "MB")
        } else if rss_max >= 1024.0 {
            (1024.0, "MB")
        } else {
            (1.0, "KB")
        };
        let padding = rss_range * 0.1;
        let rss_lo = (rss_min - padding).max(0.0) / rss_div;
        let rss_hi = ((rss_max + padding) / rss_div).ceil();
        let rss_lo_str = format!("{rss_lo:.2}");
        let rss_hi_str = format!("{rss_hi:.0}");
        let s2 = Command::new("rrdtool")
            .args([
                "graph",
                mem_png.to_str().unwrap(),
                "--start",
                &start_ts.to_string(),
                "--end",
                &(end_ts + 1).to_string(),
                "--width",
                "1200",
                "--height",
                "300",
                "--title",
                "KCP Server - Memory (RSS)",
                "--vertical-label",
                rss_unit,
                "--units-exponent",
                "0",
                "--lower-limit",
                &rss_lo_str,
                "--upper-limit",
                &rss_hi_str,
                "--rigid",
                "--slope-mode",
                "--watermark",
                "kcp2 server_resource_test",
                "--font",
                "TITLE:12:",
                "--font",
                "WATERMARK:8:",
                &format!("DEF:rss_raw={rrd_file}:rss_kb:AVERAGE"),
                &format!("CDEF:rss=rss_raw,{rss_div},/"),
                &format!("AREA:rss#E74C3C30:RSS ({rss_unit})"),
                "LINE2:rss#E74C3C",
                &format!("GPRINT:rss:MIN:  min %6.1lf {rss_unit}"),
                &format!("GPRINT:rss:MAX:max %6.1lf {rss_unit}"),
                &format!("GPRINT:rss:LAST:  now %6.1lf {rss_unit}\\j"),
            ])
            .status()?;
        if !s2.success() {
            return Err(io::Error::other("rrdtool graph (memory) failed"));
        }

        Ok(())
    }
}

async fn sample_loop(rrd: &mut Rrd, listener: &KcpListener, secs: u64) {
    for _ in 0..secs {
        sleep(Duration::from_secs(1)).await;
        let m = ProcMetrics::read();
        let n = listener.connection_count();
        rrd.update(m.rss_kb, n, m.cpu_user_ms, m.cpu_sys_ms);
    }
}

fn make_config() -> KcpConfig {
    KcpConfig::default()
        .nodelay(true, 10, 2, true)
        .wndsize(256, 256)
        .timeout(Duration::from_secs(10))
}

// ── server echo handler ─────────────────────────────────────────────────────

async fn echo_handler(conn: Arc<kcp2::KcpConnection>) {
    let mut buf = vec![0u8; 2048];
    loop {
        match conn.recv(&mut buf).await {
            Ok(n) if n > 0 => {
                if let Err(e) = conn.send(&buf[..n]).await {
                    eprintln!("[server] echo send error (conv={}): {e}", conn.conv());
                    break;
                }
            }
            Ok(_) => {}
            Err(e) => {
                if !conn.is_dead().await {
                    eprintln!("[server] recv error (conv={}): {e}", conn.conv());
                }
                break;
            }
        }
    }
}

// ── client traffic generator ────────────────────────────────────────────────

async fn client_traffic_loop(conn: Arc<kcp2::KcpConnection>, id: usize, interval: Duration) {
    let mut buf = vec![0u8; 2048];
    let mut seq: u64 = 0;
    let mut lost_count: u64 = 0;
    let mut total_count: u64 = 0;

    loop {
        let payload = format!("ping-{id}-seq-{seq}");
        if let Err(e) = conn.send(payload.as_bytes()).await {
            if !conn.is_dead().await {
                eprintln!("[client-{id}] send err: {e}");
            }
            break;
        }

        match conn.recv(&mut buf).await {
            Ok(n) if n > 0 => {
                total_count += 1;
                let echoed = &buf[..n];
                if echoed != payload.as_bytes() {
                    lost_count += 1;
                    eprintln!(
                        "[client-{id}] MISMATCH seq={seq}: sent {} bytes, got {} bytes",
                        payload.len(),
                        n
                    );
                }
            }
            Ok(_) => {}
            Err(e) => {
                if !conn.is_dead().await {
                    eprintln!("[client-{id}] recv err (seq={seq}): {e}");
                }
                break;
            }
        }

        seq += 1;
        sleep(interval).await;

        if conn.is_dead().await {
            break;
        }
    }

    if total_count > 0 && lost_count > 0 {
        eprintln!(
            "[client-{id}] loss: {lost_count}/{total_count} ({:.2}%)",
            lost_count as f64 / total_count as f64 * 100.0
        );
    }
}

// ── main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> io::Result<()> {
    let conns: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(100);
    let total_secs: u64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(60);

    let idle_secs = (total_secs as f64 * 0.15).ceil() as u64;
    let loaded_secs = (total_secs as f64 * 0.60).ceil() as u64;
    let recovery_secs = total_secs - idle_secs - loaded_secs;
    let total_sample_secs = idle_secs + loaded_secs + recovery_secs + 40;

    let out_dir = PathBuf::from(
        std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into()),
    );
    let rrd_path = out_dir.join("server_resource_test.rrd");

    eprintln!("═══ KCP Server Resource Test ═══");
    eprintln!("  connections : {conns}");
    eprintln!("  total time  : {total_secs}s (idle={idle_secs}s, loaded={loaded_secs}s, recovery={recovery_secs}s)");
    eprintln!("  output      : {} + PNGs", rrd_path.display());
    eprintln!();

    // Start server
    let server_addr = find_free_addr();
    let listener = Arc::new(KcpListener::bind_with_config(&server_addr, make_config()).await?);
    eprintln!("[server] listening on {server_addr}");

    // Accept loop in background
    let listener_clone = listener.clone();
    let accept_handle = tokio::spawn(async move {
        loop {
            match listener_clone.accept().await {
                Ok((conn, _addr)) => {
                    tokio::spawn(echo_handler(conn));
                }
                Err(e) => {
                    eprintln!("[server] accept error: {e}");
                    break;
                }
            }
        }
    });

    let mut rrd = Rrd::create(&rrd_path, total_sample_secs)?;
    let start_ts = now_epoch();

    // ── Phase 1: idle ──
    eprintln!("[phase 1/idle] {idle_secs}s ...");
    sample_loop(&mut rrd, &listener, idle_secs).await;
    let idle_rss = ProcMetrics::read().rss_kb;
    eprintln!("[phase 1] idle RSS = {idle_rss} KB");

    // ── Phase 2: connect real clients with echo traffic ──
    eprintln!("[phase 2/load] connecting {conns} clients ...");
    let t0 = Instant::now();
    let mut sessions: Vec<kcp2::KcpSession> = Vec::with_capacity(conns);

    for i in 0..conns {
        let connector = KcpConnector::new(&server_addr)?
            .with_config(make_config())
            .conv((i + 1) as u32);

        match connector.connect().await {
            Ok(session) => {
                let conn = session.connection().clone();
                tokio::spawn(client_traffic_loop(conn, i, Duration::from_secs(3)));
                sessions.push(session);
            }
            Err(e) => {
                eprintln!("[phase 2] client-{i} connect failed: {e}");
            }
        }
    }
    let active_conns = sessions.len();
    let connect_dt = t0.elapsed();
    eprintln!(
        "[phase 2] {active_conns}/{conns} connected in {:.2?} ({:.0} conn/s)",
        connect_dt,
        active_conns as f64 / connect_dt.as_secs_f64()
    );

    // Let traffic run for a few seconds to warm up, then sample
    sleep(Duration::from_secs(2)).await;
    eprintln!("[phase 2/load] sampling {loaded_secs}s with active echo traffic ...");
    sample_loop(&mut rrd, &listener, loaded_secs).await;
    let loaded_rss = ProcMetrics::read().rss_kb;
    let delta_kb = loaded_rss.saturating_sub(idle_rss);
    let per_conn = if active_conns > 0 { delta_kb as f64 / active_conns as f64 } else { 0.0 };
    eprintln!("[phase 2] loaded RSS = {loaded_rss} KB (+{delta_kb} KB, {per_conn:.2} KB/conn)");

    // ── Phase 3: disconnect all + recovery ──
    eprintln!("[phase 3/unload] closing {active_conns} sessions ...");
    let t0 = Instant::now();
    let close_count = sessions.len();
    let convs_to_remove: Vec<u32> = sessions.iter().map(|s| s.connection().conv()).collect();
    for session in &sessions {
        session.close().await;
    }
    for conv in &convs_to_remove {
        listener.remove_connection(*conv);
    }
    drop(sessions);
    let close_dt = t0.elapsed();
    eprintln!(
        "[phase 3] {close_count} sessions closed in {:.2?} ({:.0} conn/s)",
        close_dt,
        close_count as f64 / close_dt.as_secs_f64()
    );

    sleep(Duration::from_secs(3)).await;
    eprintln!("[phase 3/recovery] sampling {recovery_secs}s ...");
    sample_loop(&mut rrd, &listener, recovery_secs).await;
    let recovery_rss = ProcMetrics::read().rss_kb;
    let recovery_pct = if delta_kb > 0 {
        let freed = loaded_rss.saturating_sub(recovery_rss);
        freed as f64 / delta_kb as f64 * 100.0
    } else {
        100.0
    };
    eprintln!("[phase 3] recovery RSS = {recovery_rss} KB ({recovery_pct:.1}% freed)");

    // Extra settle period — let reaper / allocator settle before finishing
    eprintln!("[settle] waiting 30s for cleanup ...");
    sample_loop(&mut rrd, &listener, 30).await;

    // Graph
    let end_ts = now_epoch();
    eprintln!("[graph] generating charts ...");
    rrd.render_graphs(&out_dir, start_ts, end_ts)?;

    accept_handle.abort();

    // Summary
    eprintln!();
    eprintln!("═════════════════════════════════════════════════════════");
    eprintln!("  KCP Server Resource Summary");
    eprintln!("═════════════════════════════════════════════════════════");
    eprintln!("  Connections        : {active_conns}");
    eprintln!("  Phase timing       : idle={idle_secs}s load={loaded_secs}s recovery={recovery_secs}s");
    eprintln!("  Idle RSS           : {idle_rss} KB");
    eprintln!("  Loaded RSS         : {loaded_rss} KB (+{delta_kb} KB)");
    eprintln!("  Per-connection     : {per_conn:.2} KB");
    eprintln!("  Recovery RSS       : {recovery_rss} KB ({recovery_pct:.1}% freed)");
    eprintln!("  Connect throughput : {:.0} conn/s", active_conns as f64 / connect_dt.as_secs_f64());
    eprintln!("  Close throughput   : {:.0} conn/s", close_count as f64 / close_dt.as_secs_f64());
    eprintln!("═════════════════════════════════════════════════════════");
    eprintln!("  RRD          : {}", rrd_path.display());
    eprintln!("  Main graph   : {}", out_dir.join("server_perf_main.png").display());
    eprintln!("  Memory graph : {}", out_dir.join("server_perf_memory.png").display());
    eprintln!("═════════════════════════════════════════════════════════");

    listener.close().await;
    Ok(())
}
