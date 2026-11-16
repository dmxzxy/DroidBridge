//! 多人共享投屏的端到端测试（不需要真机）：
//! - `adb` 用 examples/fake_adb.rs（把每次调用的参数写进 `$FAKE_ADB_LOG`，`forward tcp:0` 返回
//!   `$FAKE_ADB_PORT`，`app_process` 常驻）；
//! - 设备端是本文件里的假 scrcpy-server（tokio TcpListener，按 视频 / [声音] / 控制 的顺序接受连接，
//!   发配置包与关键帧、记录浏览器发来的控制消息）。
//!
//! 环境变量是进程级的，所以所有场景都放在同一个 `#[tokio::test]` 里顺序执行。

use droidbridge::Config;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Notify};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// 设备 serial（假 adb 不校验）
const SERIAL: &str = "FAKE";
/// 等待一条消息 / 一个状态的通用上限
const TIMEOUT: Duration = Duration::from_secs(5);
/// 需要等定时器（闲置 3 秒释放、会话重启）时的上限
const SLOW: Duration = Duration::from_secs(20);
/// 设备端视频尺寸
const W: u32 = 1080;
const H: u32 = 1920;

// ---------------------------------------------------------------- WebSocket 小工具

async fn connect(port: u16, vid: &str, who: &str, extra: &str) -> Ws {
    let url = format!("ws://127.0.0.1:{port}/ws/stream?serial={SERIAL}&vid={vid}&who={who}{extra}");
    let (ws, _) = tokio_tungstenite::connect_async(url.as_str()).await.unwrap_or_else(|e| panic!("连接 {url} 失败：{e}"));
    ws
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::text(v.to_string())).await.expect("发送控制指令失败");
}

fn json_of(m: &Message) -> Option<Value> {
    match m {
        Message::Text(t) => serde_json::from_str(t.as_str()).ok(),
        _ => None,
    }
}

/// `viewers` 文本消息
fn viewers_msg(m: &Message) -> Option<Value> {
    json_of(m).filter(|v| v["type"] == "viewers")
}

fn is_type(m: &Message, ty: &str) -> bool {
    json_of(m).is_some_and(|v| v["type"] == ty)
}

/// 二进制视频包的 flags（bit0 配置包、bit1 关键帧、bit2 声音）
fn flags(m: &Message) -> Option<u8> {
    match m {
        Message::Binary(b) => b.first().copied(),
        _ => None,
    }
}

fn names(v: &Value) -> Vec<String> {
    v["list"].as_array().map(|l| l.iter().map(|e| e["name"].as_str().unwrap_or("").to_string()).collect()).unwrap_or_default()
}

fn ids(v: &Value) -> Vec<String> {
    v["list"].as_array().map(|l| l.iter().map(|e| e["id"].as_str().unwrap_or("").to_string()).collect()).unwrap_or_default()
}

fn has_config(msgs: &[Message]) -> bool {
    msgs.iter().any(|m| flags(m).is_some_and(|f| f & 1 != 0))
}

fn has_key(msgs: &[Message]) -> bool {
    msgs.iter().any(|m| flags(m).is_some_and(|f| f & 2 != 0))
}

/// 一直读到 `done` 对“已收到的所有消息”成立为止（不会漏掉先到的消息）
async fn recv_all_until(ws: &mut Ws, what: &str, within: Duration, mut done: impl FnMut(&[Message]) -> bool) -> Vec<Message> {
    let deadline = tokio::time::Instant::now() + within;
    let mut got: Vec<Message> = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            panic!("等待 {what} 超时（{within:?}），已收到 {} 条消息", got.len());
        }
        match tokio::time::timeout(left, ws.next()).await {
            Ok(Some(Ok(m))) => {
                got.push(m);
                if done(&got) {
                    return got;
                }
            }
            Ok(Some(Err(e))) => panic!("等待 {what} 时 WebSocket 出错：{e}"),
            Ok(None) => panic!("等待 {what} 时 WebSocket 已关闭"),
            Err(_) => panic!("等待 {what} 超时（{within:?}）"),
        }
    }
}

/// 一直读到某条消息满足条件为止
async fn recv_until(ws: &mut Ws, what: &str, f: impl Fn(&Message) -> bool) -> Message {
    let mut got = recv_all_until(ws, what, TIMEOUT, |msgs| f(msgs.last().unwrap())).await;
    got.pop().unwrap()
}

/// 在一段时间内确认没有收到满足条件的消息（用于“B 收不到”这类否定断言）
async fn expect_never_msg(ws: &mut Ws, what: &str, within: Duration, f: impl Fn(&Message) -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return;
        }
        match tokio::time::timeout(left, ws.next()).await {
            Ok(Some(Ok(m))) => assert!(!f(&m), "不应收到 {what}：{m:?}"),
            Ok(Some(Err(_))) | Ok(None) | Err(_) => return,
        }
    }
}

/// 极简 HTTP GET（只用于 `/api/mirrors`），返回响应里的 JSON
async fn http_json(port: u16, path: &str) -> Value {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.expect("连接服务器失败");
    let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let start = text.find('{').unwrap_or_else(|| panic!("{path} 没有返回 JSON：{text}"));
    let end = text.rfind('}').unwrap_or(start);
    serde_json::from_str(&text[start..=end]).unwrap_or_else(|e| panic!("{path} 返回的不是 JSON：{e}：{text}"))
}

/// 等到 `f` 连续成立 `settle` 这么久：用来确认状态变化（旧连接的清理）已经过去、没有再变
async fn wait_stable_json(port: u16, path: &str, what: &str, within: Duration, settle: Duration, mut f: impl FnMut(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + within;
    let mut since: Option<tokio::time::Instant> = None;
    loop {
        let v = http_json(port, path).await;
        if f(&v) {
            if since.get_or_insert_with(tokio::time::Instant::now).elapsed() >= settle {
                return v;
            }
        } else {
            since = None;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("等待 {path} 满足 {what} 并稳定 {settle:?} 超时：{v}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_json(port: u16, path: &str, what: &str, within: Duration, mut done: impl FnMut(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let v = http_json(port, path).await;
        if done(&v) {
            return v;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("等待 {path} 满足 {what} 超时：{v}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

// ---------------------------------------------------------------- 控制消息

/// 触摸事件（type 2，32 字节）：返回 (action, id)
fn touch_of(m: &[u8]) -> Option<(u8, i64)> {
    if m.len() != 32 || m[0] != 2 {
        return None;
    }
    Some((m[1], i64::from_be_bytes(m[2..10].try_into().unwrap())))
}

fn has_touch(msgs: &[Vec<u8>], action: u8, id: i64) -> bool {
    msgs.iter().any(|m| touch_of(m) == Some((action, id)))
}

fn touch_json(action: u8, id: i64, x: i32, y: i32) -> Value {
    json!({"t":"touch","action":action,"id":id,"x":x,"y":y,"w":W,"h":H})
}

// ---------------------------------------------------------------- 假 scrcpy 设备

struct DeviceInner {
    log: PathBuf,
    /// 收到的控制消息（原始字节）
    msgs: Mutex<Vec<Vec<u8>>>,
    /// 有新控制消息时通知（等待断言用）
    notify: Notify,
    /// 当前会话的视频关键帧请求（RESET_VIDEO）
    key_out: tokio::sync::Mutex<Option<mpsc::UnboundedSender<()>>>,
    /// 当前会话的控制通道写端（测试可以直接给浏览器发消息，如剪贴板）
    ctl_out: tokio::sync::Mutex<Option<OwnedWriteHalf>>,
}

#[derive(Clone)]
struct Device {
    port: u16,
    inner: Arc<DeviceInner>,
}

impl Device {
    async fn start(log: PathBuf) -> Device {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("假设备监听失败");
        let port = listener.local_addr().unwrap().port();
        let dev = Device {
            port,
            inner: Arc::new(DeviceInner {
                log,
                msgs: Mutex::new(Vec::new()),
                notify: Notify::new(),
                key_out: tokio::sync::Mutex::new(None),
                ctl_out: tokio::sync::Mutex::new(None),
            }),
        };
        let inner = dev.inner.clone();
        tokio::spawn(accept_loop(listener, inner));
        dev
    }

    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.inner.log).unwrap_or_default()
    }

    fn log_lines(&self) -> Vec<String> {
        self.log_text().lines().map(str::to_string).collect()
    }

    fn lines_containing(&self, needle: &str) -> Vec<String> {
        self.log_lines().into_iter().filter(|l| l.contains(needle)).collect()
    }

    /// 假 adb 启动 scrcpy-server 的次数
    fn app_process_lines(&self) -> Vec<String> {
        self.lines_containing("app_process")
    }

    fn snapshot(&self) -> Vec<Vec<u8>> {
        self.inner.msgs.lock().unwrap().clone()
    }

    fn touch_count(&self) -> usize {
        self.snapshot().iter().filter(|m| touch_of(m).is_some()).count()
    }

    /// 等设备侧收到满足条件的控制消息
    async fn wait_ctl(&self, what: &str, within: Duration, f: impl Fn(&[Vec<u8>]) -> bool) -> Vec<Vec<u8>> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let snap = self.snapshot();
            if f(&snap) {
                return snap;
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                panic!("等待设备收到 {what} 超时，设备已收到 {} 条控制消息", snap.len());
            }
            let _ = tokio::time::timeout(left, self.inner.notify.notified()).await;
        }
    }

    /// 在一段时间内确认设备没有收到满足条件的控制消息
    async fn expect_never_ctl(&self, what: &str, within: Duration, f: impl Fn(&[Vec<u8>]) -> bool) {
        let deadline = tokio::time::Instant::now() + within;
        while tokio::time::Instant::now() < deadline {
            assert!(!f(&self.snapshot()), "设备不应收到 {what}");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn wait_log(&self, what: &str, within: Duration, f: impl Fn(&[String]) -> bool) -> Vec<String> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let lines = self.log_lines();
            if f(&lines) {
                return lines;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("等待 adb 日志 {what} 超时：{lines:?}");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// 设备端剪贴板消息（type 0 + len u32 BE + utf8）
    async fn send_clipboard(&self, text: &str) {
        let mut guard = self.inner.ctl_out.lock().await;
        let w = guard.as_mut().expect("假设备还没有控制通道");
        let mut buf = vec![0u8];
        buf.extend_from_slice(&(text.len() as u32).to_be_bytes());
        buf.extend_from_slice(text.as_bytes());
        w.write_all(&buf).await.expect("写剪贴板失败");
    }
}

/// 按 视频 / [声音] / 控制 的顺序接受连接；每轮 app_process 重启都重新开始
async fn accept_loop(listener: TcpListener, inner: Arc<DeviceInner>) {
    #[derive(Clone, Copy, PartialEq)]
    enum Await {
        Video,
        Audio,
        Control,
    }
    let mut awaiting = Await::Video;
    let mut seen_sessions = 0usize;
    loop {
        let Ok((sock, _)) = listener.accept().await else { return };
        match awaiting {
            Await::Video => {
                // 视频连接一定在每个 scrcpy-server 启动之后：用日志里的 audio= 判断要不要声音通道
                let line = wait_new_app_process(&inner.log, &mut seen_sessions).await;
                let audio = line.split_whitespace().any(|a| a == "audio=true");
                let (r, w) = sock.into_split();
                drop(r); // 视频通道是单向的（设备 → 浏览器）
                let (key_tx, key_rx) = mpsc::unbounded_channel();
                *inner.key_out.lock().await = Some(key_tx);
                tokio::spawn(video_task(w, key_rx));
                awaiting = if audio { Await::Audio } else { Await::Control };
            }
            Await::Audio => {
                let (_r, mut w) = sock.into_split();
                tokio::spawn(async move {
                    if w.write_all(b"opus").await.is_err() {
                        return;
                    }
                    // 不发音频包：只保持连接，验证“要声音的观看者”要到的通道
                    std::future::pending::<()>().await;
                });
                awaiting = Await::Control;
            }
            Await::Control => {
                let (r, w) = sock.into_split();
                *inner.ctl_out.lock().await = Some(w);
                let inner = inner.clone();
                tokio::spawn(control_task(r, inner));
                awaiting = Await::Video;
            }
        }
    }
}

/// 等日志里出现比 `seen` 更多次 app_process，返回最新一行的参数
async fn wait_new_app_process(log: &PathBuf, seen: &mut usize) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        let lines: Vec<&str> = text.lines().filter(|l| l.contains("app_process")).collect();
        if lines.len() > *seen {
            *seen = lines.len();
            return lines.last().unwrap().to_string();
        }
        assert!(tokio::time::Instant::now() < deadline, "等待 scrcpy-server 启动（app_process）超时，日志：{text}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 视频通道：dummy byte + 设备名 + 编码 ID + session 包 + 配置包 + 关键帧，然后每 30ms 一帧
async fn video_task(mut w: OwnedWriteHalf, mut key_rx: mpsc::UnboundedReceiver<()>) {
    let mut head = vec![0u8]; // dummy byte（scrcpy 用来表示 socket 已就绪）
    let mut name = [0u8; 64];
    name[.."FakePhone".len()].copy_from_slice(b"FakePhone");
    head.extend_from_slice(&name);
    head.extend_from_slice(b"h264");
    if w.write_all(&head).await.is_err() {
        return;
    }
    // session 包：hdr[0] bit 0x80，宽高在 [4..8] / [8..12]（大端）
    let mut hdr = [0u8; 12];
    hdr[0] = 0x80;
    hdr[4..8].copy_from_slice(&W.to_be_bytes());
    hdr[8..12].copy_from_slice(&H.to_be_bytes());
    if w.write_all(&hdr).await.is_err() {
        return;
    }
    if !write_frame(&mut w, true, false, b"SPSPPS").await || !write_frame(&mut w, false, true, b"KEY").await {
        return;
    }
    loop {
        tokio::select! {
            r = key_rx.recv() => {
                if r.is_none() {
                    return;
                }
                // RESET_VIDEO：立刻再给一个配置包 + 关键帧
                if !write_frame(&mut w, true, false, b"SPSPPS").await || !write_frame(&mut w, false, true, b"KEY").await {
                    return;
                }
            }
            () = tokio::time::sleep(Duration::from_millis(30)) => {
                if !write_frame(&mut w, false, false, b"DELTA").await {
                    return;
                }
            }
        }
    }
}

/// 一个视频包：pts_flags u64 BE + len u32 BE + payload
async fn write_frame(w: &mut OwnedWriteHalf, config: bool, key: bool, payload: &[u8]) -> bool {
    let mut pts = 0u64;
    if config {
        pts |= 1 << 62;
    }
    if key {
        pts |= 1 << 61;
    }
    let mut buf = Vec::with_capacity(12 + payload.len());
    buf.extend_from_slice(&pts.to_be_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf).await.is_ok()
}

/// 控制通道：按 scrcpy 控制协议解析并记录每条消息；收到 RESET_VIDEO 时让视频任务补关键帧
async fn control_task(mut r: OwnedReadHalf, inner: Arc<DeviceInner>) {
    loop {
        let mut t = [0u8; 1];
        if r.read_exact(&mut t).await.is_err() {
            return;
        }
        let Some(msg) = read_ctl(&mut r, t[0]).await else { return };
        if t[0] == 17 {
            let tx = inner.key_out.lock().await.clone();
            if let Some(tx) = tx {
                let _ = tx.send(());
            }
        }
        inner.msgs.lock().unwrap().push(msg);
        inner.notify.notify_one();
    }
}

async fn read_n(r: &mut OwnedReadHalf, out: &mut Vec<u8>, n: usize) -> bool {
    let mut b = vec![0u8; n];
    if r.read_exact(&mut b).await.is_err() {
        return false;
    }
    out.extend_from_slice(&b);
    true
}

/// u32 长度前缀 + 内容（文本、剪贴板尾部）
async fn read_u32_tail(r: &mut OwnedReadHalf, out: &mut Vec<u8>) -> bool {
    let mut len = [0u8; 4];
    if r.read_exact(&mut len).await.is_err() {
        return false;
    }
    out.extend_from_slice(&len);
    read_n(r, out, u32::from_be_bytes(len) as usize).await
}

/// u8 长度前缀 + 内容（start_app）
async fn read_u8_tail(r: &mut OwnedReadHalf, out: &mut Vec<u8>) -> bool {
    let mut len = [0u8; 1];
    if r.read_exact(&mut len).await.is_err() {
        return false;
    }
    out.extend_from_slice(&len);
    read_n(r, out, len[0] as usize).await
}

/// 按类型读取一条控制消息（返回完整原始字节）
async fn read_ctl(r: &mut OwnedReadHalf, t: u8) -> Option<Vec<u8>> {
    let mut msg = vec![t];
    let ok = match t {
        0 => read_n(r, &mut msg, 13).await,                              // keycode
        1 => read_u32_tail(r, &mut msg).await,                           // text
        2 => read_n(r, &mut msg, 31).await,                              // touch（共 32 字节）
        3 => read_n(r, &mut msg, 20).await,                              // scroll（共 21 字节）
        4 | 10 => read_n(r, &mut msg, 1).await,                          // back / power
        5 | 6 | 7 | 11 | 15 | 17 => true,                                // 单字节命令
        9 => read_n(r, &mut msg, 13).await && read_u32_tail(r, &mut msg).await, // clipboard
        16 => read_u8_tail(r, &mut msg).await,                           // start app
        _ => false,
    };
    ok.then_some(msg)
}

// ---------------------------------------------------------------- 测试

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("droidbridge-shared-mirror-{}-{:08x}", std::process::id(), droidbridge::util::random_scid()));
    std::fs::create_dir_all(&dir).expect("创建临时目录失败");
    dir
}

/// `target/<profile>/deps/shared_mirror-*.exe` → `target/<profile>/examples/fake_adb(.exe)`
fn fake_adb() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let name = if cfg!(windows) { "fake_adb.exe" } else { "fake_adb" };
    exe.parent().and_then(|deps| deps.parent()).expect("target 目录").join("examples").join(name)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_mirror_end_to_end() {
    let dir = temp_dir();
    let log = dir.join("adb.log");
    std::fs::write(&log, b"").unwrap();
    let adb = fake_adb();
    assert!(adb.is_file(), "假 adb 没有构建：{}（先运行 cargo build --example fake_adb，或直接 cargo test）", adb.display());

    let dev = Device::start(log.clone()).await;
    // 进程级环境变量：假 adb 用它写日志、读假设备的端口
    unsafe {
        std::env::set_var("FAKE_ADB_LOG", &log);
        std::env::set_var("FAKE_ADB_PORT", dev.port.to_string());
    }

    let started = droidbridge::start(Config {
        bind: "127.0.0.1".into(),
        port: 0,
        https_port: 0,
        token: None,
        name: "TestNode".into(),
        adb,
        data_dir: dir.clone(),
        server_jar: dir.join("scrcpy-server"),
        discovery: false,
        media_dir: None,
        open: false,
        allow_hosts: vec![],
        no_plugins: true,
    })
    .await
    .expect("启动 DroidBridge 失败");
    let port = started.port;

    // ---------------------------------------------------------------- a
    // 第一个观看者：启动 scrcpy-server，收到 meta / session / 配置包 / 关键帧 / viewers(1)
    let mut a = connect(port, "vidA", "Alice", "").await;
    let got = recv_all_until(&mut a, "a 的 meta+session+配置包+关键帧+viewers", SLOW, |msgs| {
        let meta = msgs.iter().any(|m| is_type(m, "meta"));
        let session = msgs.iter().any(|m| is_type(m, "session"));
        let viewers1 = msgs.iter().any(|m| viewers_msg(m).is_some_and(|v| names(&v).len() == 1));
        meta && session && has_config(msgs) && has_key(msgs) && viewers1
    })
    .await;
    let meta = got.iter().find_map(|m| json_of(m).filter(|v| v["type"] == "meta")).expect("a 没有收到 meta");
    assert_eq!(meta["device_name"], "FakePhone");
    assert_eq!(meta["codec"], "h264");
    assert_eq!(meta["serial"], SERIAL);
    let session = got.iter().find_map(|m| json_of(m).filter(|v| v["type"] == "session")).expect("a 没有收到 session");
    assert_eq!(session["width"].as_u64(), Some(W as u64));
    assert_eq!(session["height"].as_u64(), Some(H as u64));
    let v = got.iter().find_map(viewers_msg).expect("a 没有收到 viewers");
    assert_eq!(v["you"], "vidA");
    assert_eq!(names(&v), vec!["Alice"]);
    assert_eq!(v["controller"], Value::Null);
    let lines = dev.app_process_lines();
    assert_eq!(lines.len(), 1, "第一个观看者应该只启动一个 scrcpy-server：{lines:?}");

    // ---------------------------------------------------------------- b
    // 第二个观看者加入同一个会话：补发 meta / session / 配置包，不重启 scrcpy-server
    let mut b = connect(port, "vidB", "Bob", "").await;
    let got = recv_all_until(&mut b, "b 的 meta+session+配置包+关键帧+viewers", SLOW, |msgs| {
        let viewers2 = msgs.iter().any(|m| viewers_msg(m).is_some_and(|v| names(&v).len() == 2));
        msgs.iter().any(|m| is_type(m, "meta")) && msgs.iter().any(|m| is_type(m, "session")) && has_config(msgs) && has_key(msgs) && viewers2
    })
    .await;
    let v = got.iter().find_map(viewers_msg).filter(|v| names(v).len() == 2).expect("b 没有收到 viewers(2)");
    assert_eq!(v["you"], "vidB");
    assert_eq!(names(&v), vec!["Alice", "Bob"]);
    let got_a = recv_all_until(&mut a, "a 的 viewers(2)", TIMEOUT, |msgs| {
        viewers_msg(msgs.last().unwrap()).is_some_and(|v| names(&v).len() == 2)
    })
    .await;
    let va = viewers_msg(got_a.last().unwrap()).unwrap();
    assert_eq!(va["you"], "vidA", "每个观看者收到的 you 应该是自己的 vid");
    assert_eq!(names(&va), vec!["Alice", "Bob"]);
    let lines = dev.app_process_lines();
    assert_eq!(lines.len(), 1, "第二个观看者不应该重启会话：{lines:?}");

    // ---------------------------------------------------------------- c
    // 谁先动谁控制：A 按下后取得控制权，B 的按下被拒绝（busy），移动 / 抬起被静默丢弃
    send(&mut a, touch_json(0, -2, 10, 10)).await;
    dev.wait_ctl("A 的按下", TIMEOUT, |msgs| has_touch(msgs, 0, -2)).await;
    recv_all_until(&mut a, "A 成为控制者", TIMEOUT, |msgs| {
        viewers_msg(msgs.last().unwrap()).is_some_and(|v| v["controller"] == "vidA" && v["controller_name"] == "Alice")
    })
    .await;

    send(&mut b, touch_json(0, -3, 20, 30)).await;
    let busy = recv_until(&mut b, "busy", |m| is_type(m, "busy")).await;
    assert_eq!(json_of(&busy).unwrap()["by"], "Alice");
    // B 的按下没有被转发到设备（服务端返回 Busy 就说明没写进控制通道）
    dev.expect_never_ctl("B 的按下", Duration::from_millis(300), |msgs| has_touch(msgs, 0, -3)).await;
    // B 的移动 / 抬起静默丢弃
    send(&mut b, touch_json(2, -3, 25, 35)).await;
    send(&mut b, touch_json(1, -3, 25, 35)).await;
    dev.expect_never_ctl("B 的移动 / 抬起", Duration::from_millis(300), |msgs| msgs.iter().any(|m| touch_of(m).is_some_and(|(_, id)| id == -3)))
        .await;
    assert_eq!(dev.touch_count(), 1, "设备只应该收到 A 的那一次按下");

    // ---------------------------------------------------------------- d
    // A 抬手，闲置 3 秒后自动释放控制权；随后 B 可以操作
    send(&mut a, touch_json(1, -2, 10, 10)).await;
    dev.wait_ctl("A 的抬起", TIMEOUT, |msgs| has_touch(msgs, 1, -2)).await;
    let released = recv_all_until(&mut a, "闲置释放控制权", SLOW, |msgs| {
        viewers_msg(msgs.last().unwrap()).is_some_and(|v| v["controller"].is_null() && names(&v).len() == 2)
    })
    .await;
    let vr = viewers_msg(released.last().unwrap()).unwrap();
    assert_eq!(vr["controller_name"], Value::Null);
    send(&mut b, touch_json(0, -2, 40, 50)).await;
    dev.wait_ctl("闲置释放后 B 的按下", TIMEOUT, |msgs| has_touch(msgs, 0, -2)).await;
    recv_all_until(&mut b, "B 成为控制者", TIMEOUT, |msgs| viewers_msg(msgs.last().unwrap()).is_some_and(|v| v["controller"] == "vidB")).await;

    // ---------------------------------------------------------------- e
    // B 按着不放时断线：设备收到 touch cancel（action 3），手机上不会留下“按住”状态
    drop(b);
    dev.wait_ctl("B 断线时的 touch cancel", TIMEOUT, |msgs| has_touch(msgs, 3, -2)).await;
    recv_all_until(&mut a, "B 离开后控制者为空", TIMEOUT, |msgs| {
        viewers_msg(msgs.last().unwrap()).is_some_and(|v| v["controller"].is_null() && names(&v) == vec!["Alice"])
    })
    .await;

    // ---------------------------------------------------------------- f
    // 第三个观看者 force=1 + 新画质：重启 scrcpy-server，A 不掉线并收到新会话的配置包 / 关键帧
    let app_process_before = dev.app_process_lines().len();
    let c = connect(port, "vidC", "Carol", "&force=1&max_size=720").await;
    recv_all_until(&mut a, "重启后的 meta", SLOW, |msgs| is_type(msgs.last().unwrap(), "meta")).await;
    // meta 之后的消息一定属于新会话：配置包 → 关键帧
    let got = recv_all_until(&mut a, "重启后的配置包", TIMEOUT, has_config).await;
    assert!(has_config(&got), "重启后 A 没有收到配置包");
    let got = recv_all_until(&mut a, "重启后的关键帧", TIMEOUT, has_key).await;
    assert!(has_key(&got), "重启后 A 没有收到关键帧");
    let lines = dev.app_process_lines();
    assert_eq!(lines.len(), app_process_before + 1, "force=1 应该重启一次 scrcpy-server：{lines:?}");
    assert!(lines[lines.len() - 1].contains("max_size=720"), "重启应该使用新的画质参数：{}", lines[lines.len() - 1]);
    assert!(lines[lines.len() - 1].contains("audio=false"), "没有人要声音：{}", lines[lines.len() - 1]);

    // ---------------------------------------------------------------- g
    // 同一个 vid 重连（旧 socket 还开着）：viewers 里只有一条记录，旧连接关闭不会移除新连接
    let mut a2 = connect(port, "vidA", "Alice", "").await;
    let got = recv_all_until(&mut a2, "重连后的 viewers", TIMEOUT, |msgs| {
        viewers_msg(msgs.last().unwrap()).is_some_and(|v| v["you"] == "vidA")
    })
    .await;
    let v = viewers_msg(got.last().unwrap()).unwrap();
    let all = ids(&v);
    assert_eq!(all.iter().filter(|i| *i == "vidA").count(), 1, "同一个 vid 只应该有一条记录：{all:?}");
    assert_eq!(all.len(), 2, "应当只有 Carol 和重连后的 Alice：{all:?}");
    // 关闭旧连接：之后状态必须一直保持“只有一个 Alice”，旧连接的 leave 不会移除新连接
    drop(a);
    let m = wait_stable_json(port, "/api/mirrors", "Alice 只出现一次", TIMEOUT, Duration::from_millis(700), |v| {
        let names: Vec<String> = v["list"]
            .as_array()
            .map(|l| l.iter().flat_map(|e| e["viewers"].as_array().cloned().unwrap_or_default()).filter_map(|n| n.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        names.iter().filter(|n| *n == "Alice").count() == 1 && names.iter().filter(|n| *n == "Carol").count() == 1
    })
    .await;
    let list = m["list"].as_array().unwrap();
    assert_eq!(list.len(), 1, "一台设备只有一个共享会话：{m}");
    assert_eq!(list[0]["serial"], SERIAL);

    // ---------------------------------------------------------------- h
    // 最后一个观看者离开：移除 adb forward、会话从 /api/mirrors 消失；新观看者重新启动 scrcpy-server
    let removes_before = dev.lines_containing("forward --remove").len();
    drop(a2);
    drop(c);
    dev.wait_log("forward --remove", TIMEOUT, |lines| {
        lines.iter().filter(|l| l.contains("forward --remove")).count() > removes_before
    })
    .await;
    wait_json(port, "/api/mirrors", "空列表", TIMEOUT, |v| v["list"].as_array().is_some_and(|l| l.is_empty())).await;
    let app_process_before = dev.app_process_lines().len();
    let mut a3 = connect(port, "vidA", "Alice", "").await;
    recv_all_until(&mut a3, "新会话的 meta", SLOW, |msgs| is_type(msgs.last().unwrap(), "meta")).await;
    let lines = dev.app_process_lines();
    assert_eq!(lines.len(), app_process_before + 1, "新观看者应该启动新的 scrcpy-server：{lines:?}");

    // ---------------------------------------------------------------- i
    // 剪贴板只同步给控制者
    let mut b3 = connect(port, "vidB", "Bob", "").await;
    recv_all_until(&mut b3, "b3 的 viewers", TIMEOUT, |msgs| viewers_msg(msgs.last().unwrap()).is_some_and(|v| v["you"] == "vidB")).await;
    send(&mut a3, touch_json(0, -8, 10, 10)).await;
    dev.wait_ctl("a3 的按下", TIMEOUT, |msgs| has_touch(msgs, 0, -8)).await;
    recv_all_until(&mut a3, "a3 成为控制者", TIMEOUT, |msgs| viewers_msg(msgs.last().unwrap()).is_some_and(|v| v["controller"] == "vidA")).await;
    dev.send_clipboard("hello-clipboard").await;
    let clip = recv_until(&mut a3, "clipboard", |m| is_type(m, "clipboard")).await;
    assert_eq!(json_of(&clip).unwrap()["text"], "hello-clipboard");
    expect_never_msg(&mut b3, "clipboard（B 不是控制者）", Duration::from_millis(700), |m| is_type(m, "clipboard")).await;

    let _ = std::fs::remove_dir_all(&dir);
}
