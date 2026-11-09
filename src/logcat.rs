//! 设备日志（adb logcat）实时流：/ws/logcat?serial=...
//!
//! 服务端 → 浏览器（JSON 文本）：
//!   {"type":"lines","l":[...]}             日志行（logcat -v threadtime 原文）
//!   {"type":"filter","pkg":"..","pids":[]} 应用过滤：包名与当前进程号（进程重启后会更新）
//!   {"type":"notice","msg":".."}           提示
//!   {"type":"end","msg":".."}              日志流结束（设备断开等）
//! 浏览器 → 服务端：
//!   {"t":"pkg","name":"com.xxx"}           只看某个应用；"" 取消；"@top" 表示当前前台应用

use crate::{adb, AppState};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, watch};

type Out = mpsc::Sender<Message>;

async fn send(out: &Out, v: serde_json::Value) -> bool {
    out.send(Message::Text(v.to_string().into())).await.is_ok()
}

pub fn valid_pkg(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && !s.starts_with(['.', '-', ':'])
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "._:".contains(c))
}

pub async fn run(st: Arc<AppState>, serial: String, socket: WebSocket) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (out, mut out_rx) = mpsc::channel::<Message>(64);
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if ws_tx.send(m).await.is_err() {
                break;
            }
        }
        let _ = ws_tx.close().await;
    });

    let adb = st.cfg.adb.clone();
    let (pkg_tx, pkg_rx) = watch::channel(String::new());
    let stream = stream_logcat(adb.clone(), serial.clone(), out.clone());
    let pids = track_pids(adb.clone(), serial.clone(), pkg_rx, out.clone());
    let input = async {
        while let Some(Ok(m)) = ws_rx.next().await {
            let Message::Text(t) = m else {
                if matches!(m, Message::Close(_)) {
                    break;
                }
                continue;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(t.as_str()) else { continue };
            if v.get("t").and_then(|x| x.as_str()) != Some("pkg") {
                continue;
            }
            let mut name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if name == "@top" {
                match foreground(&adb, &serial).await {
                    Some(p) => name = p,
                    None => {
                        send(&out, json!({"type":"notice","msg":"没有识别到前台应用"})).await;
                        continue;
                    }
                }
            }
            if !name.is_empty() && !valid_pkg(&name) {
                send(&out, json!({"type":"notice","msg":"包名格式不正确"})).await;
                continue;
            }
            let _ = pkg_tx.send(name);
        }
    };
    tokio::select! {
        _ = stream => {},
        _ = pids => {},
        _ = input => {},
    }
    drop(out);
    let _ = writer.await;
}

async fn stream_logcat(adb_path: PathBuf, serial: String, out: Out) {
    // 先带 -T 取最近 500 行；个别旧系统不支持 -T 时退回普通模式
    let variants: [&[&str]; 2] = [&["logcat", "-v", "threadtime", "-T", "500"], &["logcat", "-v", "threadtime"]];
    for (i, args) in variants.iter().enumerate() {
        let mut child = match adb::command(&adb_path)
            .arg("-s")
            .arg(&serial)
            .args(*args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                send(&out, json!({"type":"end","msg":format!("无法启动 adb logcat：{e}")})).await;
                return;
            }
        };
        let mut rd = BufReader::new(child.stdout.take().unwrap());
        let mut got = 0usize;
        let mut batch: Vec<String> = Vec::new();
        let mut buf = Vec::new();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                r = rd.read_until(b'\n', &mut buf) => match r {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                        buf.clear();
                        got += 1;
                        batch.push(line);
                        if batch.len() >= 400 && !send(&out, json!({"type":"lines","l":std::mem::take(&mut batch)})).await {
                            return;
                        }
                    }
                },
                _ = tick.tick() => {
                    if !batch.is_empty() && !send(&out, json!({"type":"lines","l":std::mem::take(&mut batch)})).await {
                        return;
                    }
                }
            }
        }
        if !batch.is_empty() {
            send(&out, json!({"type":"lines","l":batch})).await;
        }
        let stderr = child
            .wait_with_output()
            .await
            .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
            .unwrap_or_default();
        if got == 0 && i == 0 && !stderr.contains("not found") {
            continue;
        }
        let msg = if stderr.is_empty() { "日志已结束（设备可能已断开）".to_string() } else { stderr };
        send(&out, json!({"type":"end","msg":msg})).await;
        return;
    }
}

/// 应用过滤：每 2 秒刷新一次包名对应的进程号，变化时通知浏览器
async fn track_pids(adb_path: PathBuf, serial: String, mut rx: watch::Receiver<String>, out: Out) {
    let mut last: (String, Vec<u32>) = (String::new(), Vec::new());
    loop {
        let pkg = rx.borrow_and_update().clone();
        let pids = if pkg.is_empty() { Vec::new() } else { pidof(&adb_path, &serial, &pkg).await };
        if (&pkg, &pids) != (&last.0, &last.1) {
            if !send(&out, json!({"type":"filter","pkg":pkg,"pids":pids})).await {
                return;
            }
            last = (pkg, pids);
        }
        tokio::select! {
            r = rx.changed() => if r.is_err() { return },
            _ = tokio::time::sleep(Duration::from_secs(2)) => {},
        }
    }
}

async fn pidof(adb_path: &std::path::Path, serial: &str, pkg: &str) -> Vec<u32> {
    // 已校验包名只含 [A-Za-z0-9._:]，可安全拼进设备端 shell
    match adb::run(adb_path, &["-s", serial, "shell", &format!("pidof {pkg}")]).await {
        Ok(o) => o.split_whitespace().filter_map(|t| t.parse().ok()).collect(),
        Err(_) => Vec::new(),
    }
}

async fn foreground(adb_path: &std::path::Path, serial: &str) -> Option<String> {
    for cmd in [
        "dumpsys activity activities | grep -E 'ResumedActivity|mFocusedApp'",
        "dumpsys window | grep -E 'mCurrentFocus|mFocusedApp'",
    ] {
        if let Ok(o) = adb::run(adb_path, &["-s", serial, "shell", cmd]).await {
            if let Some(p) = o.lines().find_map(component_pkg) {
                return Some(p);
            }
        }
    }
    None
}

/// 从 "ActivityRecord{1a2b u0 com.foo.bar/.MainActivity t6}" 之类的行中取出包名
fn component_pkg(line: &str) -> Option<String> {
    line.split_whitespace().find_map(|tok| {
        let tok = tok.trim_matches(|c| c == '{' || c == '}');
        let (p, _) = tok.split_once('/')?;
        (p.contains('.') && valid_pkg(p)).then(|| p.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkg() {
        assert_eq!(
            component_pkg("  topResumedActivity=ActivityRecord{1a2b u0 com.android.launcher3/.uioverrides.QuickstepLauncher t6}").as_deref(),
            Some("com.android.launcher3")
        );
        assert_eq!(component_pkg("  mCurrentFocus=Window{abc u0 com.x.y/com.x.y.Main}").as_deref(), Some("com.x.y"));
        assert_eq!(component_pkg("mFocusedApp=null"), None);
        assert!(!valid_pkg("com.x;rm -rf"));
        assert!(valid_pkg("com.x.y:remote"));
    }
}
