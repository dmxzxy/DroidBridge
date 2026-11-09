//! 单个投屏会话：推送并启动 scrcpy-server，读取 H.264 视频流转发到 WebSocket，
//! 把 WebSocket 发来的控制指令写入控制通道。
//!
//! 发往浏览器的二进制消息格式：[flags u8][pts u64 BE][payload...]
//!   flags bit0 = 配置包（SPS/PPS），bit1 = 关键帧
//! 文本消息为 JSON：{"type":"status"|"meta"|"session"|"error"|"clipboard"|"log", ...}

use crate::{adb, control, AppState};
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket};
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

pub const DEVICE_JAR: &str = "/data/local/tmp/scrcpy-server.jar";

#[derive(Debug, Clone)]
pub struct StreamOpts {
    pub max_size: u32,
    pub bit_rate: u32,
    pub max_fps: u32,
    pub stay_awake: bool,
}

type Out = mpsc::Sender<Message>;

fn text(v: serde_json::Value) -> Message {
    Message::Text(v.to_string().into())
}

async fn status(out: &Out, msg: &str) {
    let _ = out.send(text(json!({"type":"status","msg":msg}))).await;
}

/// 会话结束时移除 adb forward
struct Forward {
    adb: PathBuf,
    serial: String,
    port: u16,
}

impl Drop for Forward {
    fn drop(&mut self) {
        let adb = self.adb.clone();
        let serial = self.serial.clone();
        let spec = format!("tcp:{}", self.port);
        tokio::spawn(async move {
            let _ = adb::run(&adb, &["-s", &serial, "forward", "--remove", &spec]).await;
        });
    }
}

pub async fn run_session(state: Arc<AppState>, serial: String, opts: StreamOpts, socket: WebSocket) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(64);
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if ws_tx.send(m).await.is_err() {
                break;
            }
        }
        let _ = ws_tx.close().await;
    });

    tracing::info!("[{serial}] session start {opts:?}");
    match session(&state, &serial, &opts, &out_tx, &mut ws_rx).await {
        Ok(()) => tracing::info!("[{serial}] session end (client closed)"),
        Err(e) => {
            tracing::warn!("[{serial}] session end: {e}");
            let _ = out_tx.send(text(json!({"type":"error","msg":e}))).await;
        }
    }
    drop(out_tx);
    let _ = writer.await;
}

async fn session(
    state: &AppState,
    serial: &str,
    opts: &StreamOpts,
    out: &Out,
    ws_rx: &mut SplitStream<WebSocket>,
) -> Result<(), String> {
    let adb = &state.cfg.adb;
    let jar = state.cfg.server_jar.to_string_lossy().into_owned();

    // 启动阶段串行化，避免并发推送同一个 jar
    let start_guard = state.start_lock.lock().await;

    status(out, &tr!("推送 scrcpy-server…", "Pushing scrcpy-server…")).await;
    adb::run(adb, &["-s", serial, "push", &jar, DEVICE_JAR])
        .await
        .map_err(|e| tr!("推送 scrcpy-server 失败：{e}", "Failed to push scrcpy-server: {e}"))?;

    let scid = crate::util::random_scid();
    let local = format!("localabstract:scrcpy_{scid:08x}");
    let port: u16 = adb::run(adb, &["-s", serial, "forward", "tcp:0", &local])
        .await
        .map_err(|e| tr!("adb forward 失败：{e}", "adb forward failed: {e}"))?
        .trim()
        .parse()
        .map_err(|_| tr!("adb forward 未返回端口（adb 版本过旧？）", "adb forward returned no port (adb too old?)"))?;
    let _forward = Forward { adb: adb.clone(), serial: serial.to_string(), port };

    status(out, &tr!("启动 scrcpy-server…", "Starting scrcpy-server…")).await;
    let mut args: Vec<String> = [
        "-s",
        serial,
        "shell",
        &format!("CLASSPATH={DEVICE_JAR}"),
        "app_process",
        "/",
        "com.genymobile.scrcpy.Server",
        crate::scrcpy_version(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(format!("scid={scid:08x}"));
    args.push("log_level=info".into());
    args.push("tunnel_forward=true".into());
    args.push("audio=false".into());
    args.push("control=true".into());
    args.push("cleanup=true".into());
    args.push("video_codec=h264".into());
    args.push("clipboard_autosync=true".into());
    if opts.max_size > 0 {
        args.push(format!("max_size={}", opts.max_size));
    }
    if opts.bit_rate > 0 {
        args.push(format!("video_bit_rate={}", opts.bit_rate));
    }
    if opts.max_fps > 0 {
        args.push(format!("max_fps={}", opts.max_fps));
    }
    if opts.stay_awake {
        args.push("stay_awake=true".into());
    }

    let mut child = adb::command(adb)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| tr!("启动 adb shell 失败：{e}", "Failed to start adb shell: {e}"))?;

    // 设备端日志：写入本地日志，错误行同时推给客户端
    for pipe in [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
    ]
    .into_iter()
    .flatten()
    {
        let out = out.clone();
        let serial = serial.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(pipe).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!("[{serial}] device: {line}");
                if line.contains("ERROR") || line.contains("Exception") || line.contains("WARN") {
                    let _ = out.send(text(json!({"type":"log","msg":line}))).await;
                }
            }
        });
    }

    // 连接视频通道：设备端 socket 就绪前，adb 会接受连接后立刻断开，需要重试直到读到 dummy byte
    let mut video = None;
    for _ in 0..100 {
        if let Ok(Some(st)) = child.try_wait() {
            return Err(tr!("scrcpy-server 启动失败并退出（{st}），请检查设备是否已授权 USB 调试", "scrcpy-server exited ({st}); make sure USB debugging is authorized on the device"));
        }
        if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)).await {
            let mut b = [0u8; 1];
            if let Ok(Ok(_)) = tokio::time::timeout(Duration::from_secs(2), s.read_exact(&mut b)).await {
                video = Some(s);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut video = video.ok_or_else(|| tr!("连接 scrcpy-server 超时", "Timed out connecting to scrcpy-server"))?;
    let control = TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|e| tr!("连接控制通道失败：{e}", "Failed to open control channel: {e}"))?;
    let _ = video.set_nodelay(true);
    let _ = control.set_nodelay(true);
    drop(start_guard);

    // 设备名（64 字节，\0 结尾）+ 视频编码 ID
    let mut name = [0u8; 64];
    video.read_exact(&mut name).await.map_err(|e| tr!("读取设备信息失败：{e}", "Failed to read device info: {e}"))?;
    let end = name.iter().position(|&b| b == 0).unwrap_or(64);
    let device_name = String::from_utf8_lossy(&name[..end]).into_owned();

    let mut cid = [0u8; 4];
    video.read_exact(&mut cid).await.map_err(|e| tr!("读取编码信息失败：{e}", "Failed to read codec info: {e}"))?;
    let codec_id = u32::from_be_bytes(cid);
    match codec_id {
        0 => return Err(tr!("设备禁用了视频流", "The device disabled the video stream")),
        1 => return Err(tr!("设备端视频配置错误（可尝试降低分辨率）", "Video configuration error on the device (try a lower resolution)")),
        _ => {}
    }
    let codec = String::from_utf8_lossy(&cid).trim_matches('\0').to_string();
    let _ = out
        .send(text(json!({"type":"meta","device_name":device_name,"codec":codec,"serial":serial})))
        .await;

    let (ctl_r, mut ctl_w) = control.into_split();
    let mut video_task = tokio::spawn(read_video(video, out.clone()));
    let ctl_task = tokio::spawn(read_device_msgs(ctl_r, out.clone()));

    let result = tokio::select! {
        r = &mut video_task => match r {
            Ok(Err(e)) => Err(e),
            _ => Err(tr!("视频流已结束", "Video stream ended")),
        },
        r = forward_input(ws_rx, &mut ctl_w, out) => r,
        st = child.wait() => { let st = st.map(|s| s.to_string()).unwrap_or_default(); Err(tr!("scrcpy-server 已退出（{st}）", "scrcpy-server exited ({st})")) },
    };

    video_task.abort();
    ctl_task.abort();
    let _ = child.kill().await;
    result
}

async fn read_video(mut s: TcpStream, out: Out) -> Result<(), String> {
    let mut hdr = [0u8; 12];
    loop {
        s.read_exact(&mut hdr).await.map_err(|_| tr!("视频流已断开", "Video stream disconnected"))?;
        if hdr[0] & 0x80 != 0 {
            // session 包：视频尺寸变化（旋转等）
            let w = u32::from_be_bytes(hdr[4..8].try_into().unwrap());
            let h = u32::from_be_bytes(hdr[8..12].try_into().unwrap());
            if out.send(text(json!({"type":"session","width":w,"height":h}))).await.is_err() {
                return Ok(());
            }
            continue;
        }
        let pts_flags = u64::from_be_bytes(hdr[0..8].try_into().unwrap());
        let len = u32::from_be_bytes(hdr[8..12].try_into().unwrap()) as usize;
        if len == 0 || len > 64 * 1024 * 1024 {
            return Err(tr!("无效的视频包长度：{len}", "Invalid video packet length: {len}"));
        }
        let config = pts_flags & (1 << 62) != 0;
        let key = pts_flags & (1 << 61) != 0;
        let pts = pts_flags & ((1 << 61) - 1);
        let mut buf = vec![0u8; 9 + len];
        buf[0] = u8::from(config) | (u8::from(key) << 1);
        buf[1..9].copy_from_slice(&pts.to_be_bytes());
        s.read_exact(&mut buf[9..]).await.map_err(|_| tr!("视频流已断开", "Video stream disconnected"))?;
        if out.send(Message::Binary(Bytes::from(buf))).await.is_err() {
            return Ok(());
        }
    }
}

/// 解析设备端发来的消息（剪贴板等）
async fn read_device_msgs(r: OwnedReadHalf, out: Out) {
    let mut r = BufReader::new(r);
    loop {
        let Ok(t) = r.read_u8().await else { return };
        match t {
            0 => {
                // clipboard: len u32 + utf8
                let Ok(len) = r.read_u32().await else { return };
                let mut buf = vec![0u8; len as usize];
                if r.read_exact(&mut buf).await.is_err() {
                    return;
                }
                let text_s = String::from_utf8_lossy(&buf).into_owned();
                let _ = out.send(text(json!({"type":"clipboard","text":text_s}))).await;
            }
            1 => {
                // ack clipboard: sequence u64
                if r.read_u64().await.is_err() {
                    return;
                }
            }
            2 => {
                // uhid output: id u16, size u16, data
                let Ok(_id) = r.read_u16().await else { return };
                let Ok(size) = r.read_u16().await else { return };
                let mut buf = vec![0u8; size as usize];
                if r.read_exact(&mut buf).await.is_err() {
                    return;
                }
            }
            other => {
                tracing::warn!("unknown device message type {other}; stop parsing");
                // 继续读空，避免设备端写阻塞
                let mut sink = [0u8; 4096];
                while let Ok(n) = r.read(&mut sink).await {
                    if n == 0 {
                        return;
                    }
                }
                return;
            }
        }
    }
}

async fn forward_input(
    ws_rx: &mut SplitStream<WebSocket>,
    ctl_w: &mut OwnedWriteHalf,
    out: &Out,
) -> Result<(), String> {
    while let Some(msg) = ws_rx.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(_) => return Ok(()),
        };
        match msg {
            Message::Text(t) => {
                let ctl: control::Ctl = match serde_json::from_str(t.as_str()) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = out.send(text(json!({"type":"log","msg":tr!("无效控制指令：{e}", "Invalid control message: {e}")}))).await;
                        continue;
                    }
                };
                match control::encode(&ctl) {
                    Ok(parts) => {
                        for p in parts {
                            ctl_w.write_all(&p).await.map_err(|e| tr!("写入控制通道失败：{e}", "Failed to write control channel: {e}"))?;
                        }
                    }
                    Err(e) => {
                        let _ = out.send(text(json!({"type":"log","msg":e}))).await;
                    }
                }
            }
            Message::Close(_) => return Ok(()),
            _ => {}
        }
    }
    Ok(())
}
