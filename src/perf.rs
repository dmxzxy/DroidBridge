//! 性能浮层数据：/ws/perf?serial=...
//!
//! 每秒在设备上执行一次采集脚本，推送前台应用的帧率、CPU、内存，以及整机 CPU、内存、温度、电量：
//!   {"type":"perf", "pkg":"com.x", "fps":59.8, "cpu":23.1, "app_cpu":8.2, "mem_used":5120, "mem_total":7672,
//!    "app_mem":412, "cpu_temp":43.2, "batt_temp":29.4, "level":64, "charging":true}
//!   {"type":"end","msg":".."}   设备断开等
//!
//! 帧率优先用 SurfaceFlinger 的帧时间戳（游戏、SurfaceView 也能统计），取不到时用 dumpsys gfxinfo 的累计帧数。

use crate::{adb, AppState};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 设备端采集脚本（变量 SINCE：上次统计到的帧时间戳；PSS=1 时读取应用 PSS，较慢）
const SCRIPT: &str = r#"
P=$(dumpsys window 2>/dev/null | grep -E 'mCurrentFocus|mFocusedApp' | sed -nE 's/.* ([A-Za-z0-9_.]+)\/.*/\1/p' | head -1)
echo "P $P"
echo "N $(nproc 2>/dev/null)"
head -1 /proc/stat
grep -E '^(MemTotal|MemAvailable):' /proc/meminfo
dumpsys battery 2>/dev/null | grep -E '^  (level|temperature|status):'
dumpsys thermalservice 2>/dev/null | sed -n '/Current temperatures from HAL/,/^[^ \t]/p' | grep -m12 'Temperature{'
if [ -n "$P" ]; then
  PID=$(pidof $P | cut -d' ' -f1)
  if [ -n "$PID" ]; then
    echo "T $PID $(cut -d' ' -f14,15 /proc/$PID/stat)"
    echo "R $(sed -nE 's/^VmRSS:[^0-9]*([0-9]+).*/\1/p' /proc/$PID/status)"
    [ "$PSS" = 1 ] && echo "S $(dumpsys meminfo $PID 2>/dev/null | sed -nE 's/.*TOTAL PSS:[^0-9]*([0-9]+).*/\1/p;s/^ *TOTAL:? +([0-9]+).*/\1/p' | head -1)"
  fi
  dumpsys gfxinfo $P 2>/dev/null | grep -m1 'Total frames rendered' | sed -nE 's/.*: ([0-9]+).*/G \1/p'
  echo "U $(cut -d' ' -f1 /proc/uptime)"
  dumpsys SurfaceFlinger --list 2>/dev/null | sed -E 's/^RequestedLayerState\{//; s/ parentId=.*$//' | grep -F "$P/" | grep -vE 'ActivityRecord|InputSink|Background for' | head -8 | while IFS= read -r L; do
    dumpsys SurfaceFlinger --latency "$L" 2>/dev/null | awk -v s="$SINCE" 'NR>1 && $2>s && $2<9e18 {c++; if(!a||$2<a)a=$2; if($2>m)m=$2} END{printf "F %d %.0f %.0f\n", c, a, m}'
  done
fi
"#;

/// 一次采样的原始结果
#[derive(Debug, Default, PartialEq)]
struct Sample {
    pkg: String,
    ncpu: u32,
    /// /proc/stat：(总时间, 空闲时间)
    cpu: Option<(u64, u64)>,
    mem_total_kb: Option<u64>,
    mem_avail_kb: Option<u64>,
    level: Option<u32>,
    batt_temp: Option<f64>,
    charging: Option<bool>,
    cpu_temp: Option<f64>,
    /// (pid, utime+stime)
    proc_ticks: Option<(u32, u64)>,
    rss_kb: Option<u64>,
    pss_kb: Option<u64>,
    gfx_frames: Option<u64>,
    uptime: Option<f64>,
    /// 帧数最多的图层：(帧数, 最早, 最晚) 时间戳 ns
    frames: Option<(u64, f64, f64)>,
}

fn parse(out: &str) -> Sample {
    let mut s = Sample::default();
    let num = |t: Option<&str>| t.and_then(|v| v.trim().parse::<u64>().ok());
    for line in out.lines() {
        let line = line.trim_end_matches('\r');
        let t = line.trim_start();
        let mut it = t.split_whitespace();
        match it.next() {
            Some("P") => s.pkg = it.next().unwrap_or("").to_string(),
            Some("N") => s.ncpu = num(it.next()).unwrap_or(0) as u32,
            Some("cpu") => {
                let v: Vec<u64> = it.filter_map(|x| x.parse().ok()).collect();
                if v.len() >= 5 {
                    let total: u64 = v.iter().take(8).sum();
                    s.cpu = Some((total, v[3] + v[4]));
                }
            }
            Some("MemTotal:") => s.mem_total_kb = num(it.next()),
            Some("MemAvailable:") => s.mem_avail_kb = num(it.next()),
            Some("level:") => s.level = num(it.next()).map(|v| v as u32),
            Some("temperature:") => s.batt_temp = num(it.next()).map(|v| v as f64 / 10.0),
            // BatteryManager.BATTERY_STATUS_CHARGING = 2，FULL = 5
            Some("status:") => s.charging = num(it.next()).map(|v| v == 2 || v == 5),
            Some("T") => {
                let v: Vec<u64> = it.filter_map(|x| x.parse().ok()).collect();
                if v.len() == 3 {
                    s.proc_ticks = Some((v[0] as u32, v[1] + v[2]));
                }
            }
            Some("R") => s.rss_kb = num(it.next()),
            Some("S") => s.pss_kb = num(it.next()).filter(|v| *v > 0),
            Some("G") => s.gfx_frames = num(it.next()),
            Some("U") => s.uptime = it.next().and_then(|v| v.parse().ok()),
            Some("F") => {
                let v: Vec<f64> = it.filter_map(|x| x.parse().ok()).collect();
                if v.len() == 3 && v[0] > 0.0 && s.frames.is_none_or(|f| v[0] as u64 > f.0) {
                    s.frames = Some((v[0] as u64, v[1], v[2]));
                }
            }
            _ => {
                // Temperature{mValue=43.2, mType=0, mName=CPU, mStatus=0}，取 CPU（mType=0）中的最高值
                if let Some(rest) = t.strip_prefix("Temperature{mValue=") {
                    let val = rest.split(',').next().and_then(|v| v.parse::<f64>().ok());
                    if let Some(v) = val.filter(|_| t.contains("mType=0,")) {
                        s.cpu_temp = Some(s.cpu_temp.map_or(v, |c: f64| c.max(v)));
                    }
                }
            }
        }
    }
    s
}

/// 两次采样之间的状态
#[derive(Default)]
struct Prev {
    cpu: Option<(u64, u64)>,
    proc_ticks: Option<(String, u32, u64)>,
    uptime: Option<f64>,
    since: f64,
    gfx: Option<(String, u64)>,
    pss: Option<(String, u64)>,
}

/// SurfaceFlinger 每个图层只保留最近 127 帧，超过就按这些帧的时间跨度计算
const SF_HISTORY: u64 = 127;

fn compute(s: &Sample, p: &mut Prev) -> serde_json::Value {
    let elapsed = match (s.uptime, p.uptime) {
        (Some(a), Some(b)) if a > b => Some(a - b),
        _ => None,
    };
    let cpu = match (s.cpu, p.cpu) {
        (Some((t, i)), Some((pt, pi))) if t > pt => Some(100.0 * (1.0 - (i.saturating_sub(pi)) as f64 / (t - pt) as f64)),
        _ => None,
    };
    // 应用 CPU：占整机的百分比（与系统 CPU 同一口径）
    let app_cpu = match (&s.proc_ticks, &p.proc_ticks, elapsed) {
        (Some((pid, tk)), Some((ppkg, ppid, ptk)), Some(e)) if *ppkg == s.pkg && pid == ppid && tk >= ptk => {
            let n = s.ncpu.max(1) as f64;
            Some(((tk - ptk) as f64 / 100.0 / e / n * 100.0).min(100.0))
        }
        _ => None,
    };
    let fps = match (s.frames, elapsed) {
        (Some((n, a, m)), _) if p.since > 0.0 && n >= SF_HISTORY - 1 && m > a => Some((n - 1) as f64 / ((m - a) / 1e9)),
        (Some((n, _, _)), Some(e)) if p.since > 0.0 => Some(n as f64 / e),
        (None, Some(e)) if p.since > 0.0 => match (s.gfx_frames, &p.gfx) {
            (Some(g), Some((pkg, pg))) if *pkg == s.pkg && g >= *pg => Some((g - pg) as f64 / e),
            _ => Some(0.0),
        },
        _ => None,
    };
    if let Some(v) = s.pss_kb.filter(|_| !s.pkg.is_empty()) {
        p.pss = Some((s.pkg.clone(), v));
    }
    let app_mem_kb = p.pss.as_ref().filter(|(pkg, _)| *pkg == s.pkg).map(|(_, v)| *v).or(s.rss_kb);

    p.cpu = s.cpu.or(p.cpu);
    p.proc_ticks = s.proc_ticks.map(|(pid, tk)| (s.pkg.clone(), pid, tk));
    p.uptime = s.uptime.or(p.uptime);
    p.gfx = s.gfx_frames.map(|g| (s.pkg.clone(), g));
    // 首次采样也记下最新帧时间，下一次只统计之后的帧；没有帧时用一个很小的正数表示“已初始化”
    p.since = s.frames.map(|f| f.2).unwrap_or(p.since).max(p.since).max(1.0);

    let r1 = |v: f64| (v * 10.0).round() / 10.0;
    json!({
        "type": "perf",
        "pkg": s.pkg,
        "fps": fps.map(r1),
        "cpu": cpu.map(r1),
        "app_cpu": app_cpu.map(r1),
        "mem_total": s.mem_total_kb.map(|v| v / 1024),
        "mem_used": s.mem_total_kb.zip(s.mem_avail_kb).map(|(t, a)| t.saturating_sub(a) / 1024),
        "app_mem": app_mem_kb.map(|v| v / 1024),
        "cpu_temp": s.cpu_temp.map(r1),
        "batt_temp": s.batt_temp,
        "level": s.level,
        "charging": s.charging,
    })
}

/// 单次性能快照（采样两次、间隔 1 秒，得到帧率与 CPU）
pub async fn snapshot(st: &AppState, serial: &str) -> Result<serde_json::Value, String> {
    let mut prev = Prev::default();
    let mut last = serde_json::Value::Null;
    for round in 0..2 {
        let cmd = format!("SINCE={:.0}; PSS={}; {SCRIPT}", prev.since.max(0.0), round);
        let out = adb::run_timeout(&st.cfg.adb, &["-s", serial, "shell", &cmd], Duration::from_secs(8)).await?;
        last = compute(&parse(&out), &mut prev);
        if round == 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    Ok(last)
}

pub async fn run(st: Arc<AppState>, serial: String, socket: WebSocket) {
    let (mut tx, mut rx) = socket.split();
    let sampler = async {
        let mut prev = Prev::default();
        let mut fails = 0;
        for round in 0u64.. {
            let started = Instant::now();
            // PSS 较慢（约 0.3 秒），每 5 次读一次，其余时间沿用上次结果
            let pss = if round % 5 == 0 { 1 } else { 0 };
            let cmd = format!("SINCE={:.0}; PSS={pss}; {SCRIPT}", prev.since.max(0.0));
            match adb::run_timeout(&st.cfg.adb, &["-s", &serial, "shell", &cmd], Duration::from_secs(8)).await {
                Ok(out) => {
                    fails = 0;
                    let v = compute(&parse(&out), &mut prev);
                    if tx.send(Message::Text(v.to_string().into())).await.is_err() {
                        return;
                    }
                }
                Err(e) => {
                    fails += 1;
                    if fails >= 3 {
                        let msg = tr!("无法读取性能数据：{e}", "Cannot read performance data: {e}");
                        let _ = tx.send(Message::Text(json!({"type":"end","msg":msg}).to_string().into())).await;
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(1).saturating_sub(started.elapsed()).max(Duration::from_millis(300))).await;
        }
    };
    let input = async {
        while let Some(Ok(m)) = rx.next().await {
            if matches!(m, Message::Close(_)) {
                break;
            }
        }
    };
    tokio::select! {
        _ = sampler => {},
        _ = input => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT1: &str = "P com.android.launcher\r\nN 8\ncpu  1000 0 1000 7000 1000 0 0 0 0 0\nMemTotal:        7856764 kB\nMemAvailable:    2523584 kB\n  status: 2\n  level: 64\n  temperature: 294\n\tTemperature{mValue=43.237, mType=0, mName=CPU, mStatus=0}\n\tTemperature{mValue=44.5, mType=0, mName=CPU1, mStatus=0}\n\tTemperature{mValue=29.4, mType=2, mName=BATTERY, mStatus=0}\nT 2719 119870 139722\nR 313060\nS 381814\nG 10189\nU 1000.00\nF 126 196913000000000 196913370298733\n";
    const OUT2: &str = "P com.android.launcher\nN 8\ncpu  1200 0 1200 7400 1200 0 0 0 0 0\nMemTotal:        7856764 kB\nMemAvailable:    2541660 kB\n  status: 3\n  level: 63\n  temperature: 300\nT 2719 119950 139722\nR 306860\nG 10280\nU 1002.00\nF 60 196913400000000 196914100000000\n";

    #[test]
    fn parse_and_compute() {
        let s = parse(OUT1);
        assert_eq!(s.pkg, "com.android.launcher");
        assert_eq!(s.ncpu, 8);
        assert_eq!(s.cpu, Some((10000, 8000)));
        assert_eq!(s.cpu_temp, Some(44.5));
        assert_eq!(s.batt_temp, Some(29.4));
        assert_eq!(s.charging, Some(true));
        assert_eq!(s.proc_ticks, Some((2719, 259592)));
        assert_eq!(s.pss_kb, Some(381814));
        assert_eq!(s.frames.map(|f| f.0), Some(126));

        let mut p = Prev::default();
        let v1 = compute(&s, &mut p);
        assert!(v1["fps"].is_null(), "首次采样没有帧率");
        assert_eq!(v1["app_mem"], 372);
        assert_eq!(v1["mem_total"], 7672);

        let v2 = compute(&parse(OUT2), &mut p);
        assert_eq!(v2["fps"], 30.0); // 60 帧 / 2 秒
        assert_eq!(v2["cpu"], 40.0); // 总时间 +1000，空闲 +600
        assert_eq!(v2["app_cpu"], 5.0); // 80 tick / 100 / 2s / 8 核
        assert_eq!(v2["app_mem"], 372, "PSS 沿用上次结果");
        assert_eq!(v2["charging"], false);
    }

    #[test]
    fn fps_saturated_and_gfx_fallback() {
        let mut p = Prev { since: 1.0, uptime: Some(10.0), gfx: Some(("a.b".into(), 100)), ..Default::default() };
        // 缓冲区满（127 帧）：按时间跨度计算，126 个间隔 / 1.05 秒 = 120
        let s = Sample { pkg: "a.b".into(), uptime: Some(12.0), frames: Some((127, 0.0, 1.05e9)), ..Default::default() };
        assert_eq!(compute(&s, &mut p)["fps"], 120.0);
        // 没有 SurfaceFlinger 数据时用 gfxinfo 帧数差
        let mut p = Prev { since: 1.0, uptime: Some(10.0), gfx: Some(("a.b".into(), 100)), ..Default::default() };
        let s = Sample { pkg: "a.b".into(), uptime: Some(12.0), gfx_frames: Some(160), ..Default::default() };
        assert_eq!(compute(&s, &mut p)["fps"], 30.0);
    }
}
