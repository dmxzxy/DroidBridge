//! 多人共享投屏会话：同一台设备只运行一个 scrcpy-server，所有观看者共享同一路画面。
//!
//! - 第一个观看者到来时推送并启动 scrcpy-server，之后的观看者加入同一个会话、不重启；
//! - 每个观看者有自己的有界发送队列，慢的观看者丢帧（只丢非关键帧），不影响别人；
//! - 同一时间只有一个人能操作（谁先动谁控制，停手 3 秒自动释放），剪贴板只同步给控制者；
//! - 最后一个观看者离开时停掉一切（杀进程、移除 adb forward、从 mirrors 中移除）。
//!
//! 发往浏览器的二进制消息格式：[flags u8][pts u64 BE][payload...]
//!   flags bit0 = 配置包（SPS/PPS），bit1 = 关键帧，bit2 = 声音
//! 文本消息为 JSON：{"type":"status"|"meta"|"session"|"viewers"|"busy"|"clipboard"|
//!                       "audio"|"error"|"agent"|"log", ...}

use crate::{adb, control, AppState};
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};

pub const DEVICE_JAR: &str = "/data/local/tmp/scrcpy-server.jar";

/// AI 操作时的特殊控制者 id
const AI: &str = "ai";
/// 每个观看者的发送队列容量
const QUEUE: usize = 64;
/// 闲置多久自动释放控制权
const IDLE_RELEASE: Duration = Duration::from_secs(3);
/// 安全上限：控制者长时间没动静时，即使还按着也允许别人接管
const SAFETY_RELEASE: Duration = Duration::from_secs(15);
/// reset_video 按会话限速
const RESET_INTERVAL: Duration = Duration::from_secs(1);
/// busy 提示按观看者限速
const BUSY_INTERVAL: Duration = Duration::from_secs(1);
/// 观看者 id 最长长度（字符）
const VID_MAX: usize = 32;
/// 观看者名称最长长度（字符）
const WHO_MAX: usize = 40;

#[derive(Debug, Clone)]
pub struct StreamOpts {
    pub max_size: u32,
    pub bit_rate: u32,
    pub max_fps: u32,
    pub stay_awake: bool,
    /// 声音转发（Android 11+）
    pub audio: bool,
    /// opus 或 raw（浏览器不支持 Opus 解码时用 raw PCM）
    pub audio_codec: String,
    /// 同时在手机上播放（Android 13+）
    pub audio_dup: bool,
}

/// 加入共享会话的观看者（来自 `/ws/stream` 的查询参数）
#[derive(Debug, Clone)]
pub struct Guest {
    /// 观看者 id（前端生成，重连保持不变）
    pub id: String,
    /// 显示名称
    pub name: String,
    /// 按我的画质设置重启共享会话
    pub force: bool,
    /// 连接者身份（哪台电脑 / 哪个浏览器，用于“谁连着这台电脑”）
    pub ident: Option<crate::access::Ident>,
}

type Out = mpsc::Sender<Message>;

fn text(v: Value) -> Message {
    Message::Text(v.to_string().into())
}

/// 清洗观看者 id：只允许 `[A-Za-z0-9_-]`、最长 32；缺失或非法时随机生成
pub fn sanitize_vid(raw: Option<&str>) -> String {
    match raw {
        Some(v) if !v.is_empty() && v.len() <= VID_MAX && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') => v.to_string(),
        _ => random_vid(),
    }
}

/// 清洗观看者显示名称：去掉控制字符、截断到 40 个字符；为空时用 `?`
pub fn sanitize_who(raw: Option<&str>) -> String {
    let cleaned: String = raw.unwrap_or("").chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return "?".to_string();
    }
    cleaned.chars().take(WHO_MAX).collect()
}

/// 随机观看者 id（12 位小写字母与数字）
fn random_vid() -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let base = CHARS.len() as u64;
    let mut r = (u64::from(crate::util::random_scid()) << 32) | u64::from(crate::util::random_scid());
    let mut s = String::with_capacity(12);
    for _ in 0..12 {
        s.push(CHARS[(r % base) as usize] as char);
        r /= base;
    }
    s
}

/// reset_video 控制消息（不需要控制权）
fn reset_packet() -> Vec<u8> {
    match control::encode(&control::Ctl::Cmd { name: "reset_video".to_string() }) {
        Ok(mut parts) => parts.pop().unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// 触摸取消（action 3）：控制者离开、被接管或会话重启时，避免手机上留下“按住”的状态
fn touch_cancel(id: i64, x: i32, y: i32, w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w.min(u32::from(u16::MAX)) as u16, h.min(u32::from(u16::MAX)) as u16);
    let ctl = control::Ctl::Touch { action: 3, id, x, y, w, h, p: 1.0, buttons: 0 };
    match control::encode(&ctl) {
        Ok(mut parts) => parts.pop().unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// 控制消息对“按下的触点 / 按键”数量的影响：按下 +1，抬起或取消 -1
fn down_delta(ctl: &control::Ctl) -> i32 {
    match ctl {
        control::Ctl::Touch { action, .. } => match action {
            0 => 1,
            1 | 3 => -1,
            _ => 0,
        },
        control::Ctl::Key { action, .. } | control::Ctl::Back { action } => match action {
            0 => 1,
            1 => -1,
            _ => 0,
        },
        _ => 0,
    }
}

// ---------------------------------------------------------------- 控制权仲裁

/// 控制权仲裁（纯逻辑，不碰 socket，方便单测）：谁先动谁控制，停手 3 秒自动释放。
#[derive(Debug, Default)]
pub struct Arbiter {
    controller: Option<String>,
    /// 最近一次真正的控制者（剪贴板兜底用，AI 不算）
    last: Option<String>,
    /// 控制者当前按下的触点 / 按键数量
    down: i32,
    last_input: Option<Instant>,
}

impl Arbiter {
    pub fn controller(&self) -> Option<&str> {
        self.controller.as_deref()
    }

    pub fn down(&self) -> i32 {
        self.down
    }

    /// 最近一次的控制者（可能已经离开会话）
    pub fn last(&self) -> Option<&str> {
        self.last.as_deref()
    }

    /// 控制权现在是否可以被别人拿走
    pub fn available(&self, now: Instant) -> bool {
        let Some(t) = self.last_input else { return true };
        let idle = now.saturating_duration_since(t);
        idle >= SAFETY_RELEASE || (idle >= IDLE_RELEASE && self.down <= 0)
    }

    /// 尝试取得控制权；返回 false 表示被别人占着（调用者应提示 busy）
    pub fn acquire(&mut self, vid: &str, now: Instant) -> bool {
        if self.controller.as_deref() == Some(vid) {
            // 自己继续操作：不清空 down 计数
            self.last_input = Some(now);
            return true;
        }
        if self.controller.is_some() && !self.available(now) {
            return false;
        }
        self.controller = Some(vid.to_string());
        self.last = Some(vid.to_string());
        self.down = 0;
        self.last_input = Some(now);
        true
    }

    /// 按 down 规则计数（只有已取得控制权的消息才计数）
    pub fn count(&mut self, delta: i32) {
        self.down = (self.down + delta).max(0);
    }

    /// 闲置释放（每秒检查一次）：控制者停手 ≥3 秒且没有按下的触点时释放
    pub fn release_idle(&mut self, now: Instant) -> Option<String> {
        let c = self.controller.clone()?;
        if self.down > 0 {
            return None;
        }
        if !self.last_input.is_none_or(|t| now.saturating_duration_since(t) >= IDLE_RELEASE) {
            return None;
        }
        self.controller = None;
        self.down = 0;
        Some(c)
    }

    /// 强制释放控制权
    pub fn release(&mut self) -> Option<String> {
        self.down = 0;
        self.controller.take()
    }

    /// 只有控制者是 vid 时才释放（观看者离开、AI 暂停）
    pub fn release_if(&mut self, vid: &str) -> Option<String> {
        if self.controller.as_deref() == Some(vid) { self.release() } else { None }
    }

    /// AI 正在操作：controller = "ai"
    pub fn set_ai(&mut self, now: Instant) {
        self.controller = Some(AI.to_string());
        self.down = 0;
        self.last_input = Some(now);
    }
}

// ---------------------------------------------------------------- 丢帧恢复

/// 观看者队列满后的丢帧状态：丢掉后续非关键帧，直到关键帧或配置包才恢复
#[derive(Debug, Default, Clone, Copy)]
pub struct LagState {
    lagging: bool,
}

impl LagState {
    pub fn lagging(&self) -> bool {
        self.lagging
    }

    /// 这个视频包是否应该尝试发送（丢帧期间只发关键帧 / 配置包）
    pub fn wants(&self, config: bool, key: bool) -> bool {
        !self.lagging || config || key
    }

    /// 发送成功：关键帧或配置包后恢复正常
    pub fn sent(&mut self, config: bool, key: bool) {
        if config || key {
            self.lagging = false;
        }
    }

    /// 发送失败（队列满 / 已关闭）：进入丢帧状态
    pub fn failed(&mut self) {
        self.lagging = true;
    }
}

// ---------------------------------------------------------------- 会话

/// 一个观看者的发送队列与状态
struct Viewer {
    /// 连接编号（同一观看者重连时 id 不变，但连接编号不同）
    conn: u64,
    id: String,
    name: String,
    /// 连接者身份
    ident: Option<crate::access::Ident>,
    /// 是否要声音
    audio: bool,
    tx: Out,
    /// 队列满时的丢帧恢复状态
    lag: LagState,
    /// busy 提示限速
    busy_sent: Option<Instant>,
    /// 会话结束（error、最后一个观看者离开）时通知该观看者的读取循环退出
    done: Arc<tokio::sync::Notify>,
}

/// 会话共享状态（锁内只做同步操作）
struct MirrorState {
    /// 会话代数：过期会话的清理会被忽略
    generation: u64,
    /// 当前画质与声音参数（重启时换成新观看者的）
    opts: StreamOpts,
    running: bool,
    /// 控制通道（会话运行中）
    ctl: Option<mpsc::UnboundedSender<Vec<u8>>>,
    /// 观看者（按加入顺序）
    viewers: Vec<Viewer>,
    /// 缓存的最近一次 meta 文本
    meta: Option<Value>,
    /// 缓存的最近一次画面尺寸
    session: Option<(u32, u32)>,
    /// 缓存的最近一个视频配置包（SPS/PPS）
    config: Option<Bytes>,
    /// 缓存的声音状态（on / unsupported / error），补发给后来要声音的观看者
    audio_state: Option<Value>,
    /// 控制者当前按下的触点：id → 最后位置（离开 / 被接管时逐个取消）
    pointers: HashMap<i64, (i32, i32)>,
    /// reset_video 限速
    last_reset: Option<Instant>,
    /// 控制权
    arb: Arbiter,
}

impl MirrorState {
    /// 取消控制者所有按下的触点（返回要写入控制通道的消息），并清空记录
    fn cancel_pointers(&mut self) -> Vec<Vec<u8>> {
        let Some((w, h)) = self.session else {
            self.pointers.clear();
            return Vec::new();
        };
        self.pointers.drain().map(|(id, (x, y))| touch_cancel(id, x, y, w, h)).collect()
    }

    /// 控制者 id（`ai` 表示 AI）
    fn controller_id(&self) -> Value {
        match self.arb.controller() {
            None => Value::Null,
            Some(id) => json!(id),
        }
    }

    /// 控制者显示名称
    fn controller_name(&self) -> Value {
        match self.arb.controller() {
            None => Value::Null,
            Some(AI) => json!("AI"),
            Some(id) => self.viewers.iter().find(|v| v.id == id).map_or(Value::Null, |v| json!(v.name)),
        }
    }

    /// `viewers` 文本消息：每个观看者看到的 `you` 是自己的 id
    fn viewers_msg(&self, you: &str) -> Value {
        let list: Vec<Value> = self.viewers.iter().map(|v| json!({"id": v.id, "name": v.name, "audio": v.audio})).collect();
        json!({
            "type": "viewers",
            "you": you,
            "list": list,
            "controller": self.controller_id(),
            "controller_name": self.controller_name(),
        })
    }
}

/// 一次正在运行的 scrcpy-server 会话任务
struct SessionTask {
    handle: tokio::task::JoinHandle<()>,
    stop: Option<oneshot::Sender<()>>,
}

/// 一个设备的共享投屏会话：一个 scrcpy-server + 多个观看者
pub struct Mirror {
    pub serial: String,
    /// 生命周期串行化：同一 serial 的创建 / 加入 / 离开 / 重启不会交叠
    life: tokio::sync::Mutex<()>,
    st: Mutex<MirrorState>,
    /// 当前运行中的会话任务
    task: Mutex<Option<SessionTask>>,
}

/// 本机所有共享会话（serial → Mirror）
#[derive(Default)]
pub struct Mirrors {
    map: Mutex<HashMap<String, Arc<Mirror>>>,
}

impl Mirrors {
    /// 取该设备的会话；没有则创建一个（此时还没有启动 scrcpy-server）
    pub fn get_or_create(&self, serial: &str, opts: &StreamOpts) -> Arc<Mirror> {
        let mut map = self.map.lock().unwrap();
        map.entry(serial.to_string()).or_insert_with(|| Arc::new(Mirror::new(serial, opts.clone()))).clone()
    }

    pub fn get(&self, serial: &str) -> Option<Arc<Mirror>> {
        self.map.lock().unwrap().get(serial).cloned()
    }

    /// 这个实例是否仍是该设备的当前会话（可能已被清理，随后会有新会话）
    fn is_current(&self, serial: &str, m: &Arc<Mirror>) -> bool {
        self.map.lock().unwrap().get(serial).is_some_and(|cur| Arc::ptr_eq(cur, m))
    }

    /// 移除会话（只有仍是同一个实例时才移除）
    fn remove(&self, serial: &str, m: &Arc<Mirror>) {
        let mut map = self.map.lock().unwrap();
        let same = map.get(serial).is_some_and(|cur| Arc::ptr_eq(cur, m));
        if same {
            map.remove(serial);
        }
    }

    /// 所有会话（供 `/api/mirrors`）
    pub fn list(&self) -> Vec<Arc<Mirror>> {
        self.map.lock().unwrap().values().cloned().collect()
    }
}

/// `GET /api/mirrors` 的内容：本机所有共享会话的概况
pub fn mirrors_json(state: &AppState) -> Value {
    let list: Vec<Value> = state.mirrors.list().iter().map(|m| m.summary()).collect();
    json!({"list": list})
}

/// 加入会话的结果
enum JoinError {
    /// 会话正在关闭（已从 mirrors 移除）：用新会话重试
    Gone,
    /// 启动失败等：已经给所有观看者发过 error 并关闭了它们的 WebSocket
    Failed(String),
}

/// 需要控制权的消息的处理结果
enum SendResult {
    Sent { changed: bool },
    /// 不是控制者的触摸移动 / 抬起：静默丢弃（它的按下已经被拒绝过）
    Dropped,
    Busy,
    NoSession,
}

impl Mirror {
    fn new(serial: &str, opts: StreamOpts) -> Self {
        Self {
            serial: serial.to_string(),
            life: tokio::sync::Mutex::new(()),
            st: Mutex::new(MirrorState {
                generation: 0,
                opts,
                running: false,
                ctl: None,
                viewers: Vec::new(),
                meta: None,
                session: None,
                config: None,
                audio_state: None,
                pointers: HashMap::new(),
                last_reset: None,
                arb: Arbiter::default(),
            }),
            task: Mutex::new(None),
        }
    }

    /// 概况（`/api/mirrors`）
    /// 每个观看者：连接者身份、显示名称、是否正在操作（“谁连着这台电脑”）
    pub fn views(&self) -> Vec<Value> {
        let st = self.st.lock().unwrap();
        st.viewers
            .iter()
            .map(|v| {
                let (key, name, addr) = v.ident.as_ref().map(|i| (i.key.as_str(), i.name.as_str(), i.addr.as_str())).unwrap_or(("", "", ""));
                json!({"ident": key, "ident_name": name, "addr": addr, "who": v.name, "controlling": st.arb.controller() == Some(v.id.as_str())})
            })
            .collect()
    }

    pub fn summary(&self) -> Value {
        let st = self.st.lock().unwrap();
        let viewers: Vec<Value> = st.viewers.iter().map(|v| json!(v.name)).collect();
        json!({"serial": self.serial, "viewers": viewers, "controller": st.controller_name()})
    }

    /// 当前投屏视频帧尺寸 `(width, height)`；会话尚未出画时为 `None`
    pub fn session_size(&self) -> Option<(u32, u32)> {
        self.st.lock().unwrap().session
    }

    /// 插件侧直接向已运行的 scrcpy 控制通道注入指令（自动按当前视频尺寸缩放坐标，不抢占人工仲裁锁）
    pub fn inject_ctl(&self, ctl: &control::Ctl) -> bool {
        let (tx, session) = {
            let st = self.st.lock().unwrap();
            if !st.running {
                return false;
            }
            let Some(tx) = st.ctl.clone() else { return false };
            (tx, st.session)
        };
        let scaled = match session {
            Some((tw, th)) if tw > 0 && th > 0 => ctl.scale(tw, th),
            _ => match ctl {
                control::Ctl::Touch { w, h, .. } | control::Ctl::Scroll { w, h, .. } if *w == 0 || *h == 0 => return false,
                _ => ctl.clone(),
            },
        };
        let Ok(parts) = control::encode(&scaled) else { return false };
        let mut ok = false;
        for p in parts {
            if tx.send(p).is_ok() {
                ok = true;
            }
        }
        ok
    }

    /// 广播一条消息（队列满时丢弃）
    fn broadcast(&self, m: &Message) {
        let st = self.st.lock().unwrap();
        for v in &st.viewers {
            let _ = v.tx.try_send(m.clone());
        }
    }

    /// 广播状态文字（启动、重启等）
    fn broadcast_status(&self, msg: &str) {
        self.broadcast(&text(json!({"type":"status","msg":msg})));
    }

    /// 广播观看者列表与控制者
    fn broadcast_viewers(&self) {
        let st = self.st.lock().unwrap();
        for v in &st.viewers {
            let _ = v.tx.try_send(text(st.viewers_msg(&v.id)));
        }
    }

    /// 设备名与编码信息：缓存并广播
    fn publish_meta(&self, meta: Value) {
        let m = text(meta.clone());
        self.st.lock().unwrap().meta = Some(meta);
        self.broadcast(&m);
    }

    /// 画面尺寸（旋转等）：缓存并广播
    fn publish_session(&self, w: u32, h: u32) {
        let m = text(json!({"type":"session","width":w,"height":h}));
        self.st.lock().unwrap().session = Some((w, h));
        self.broadcast(&m);
    }

    /// 视频包分发给所有观看者；队列满的观看者只收关键帧 / 配置包
    fn publish_video(&self, pkt: Bytes, config: bool, key: bool) {
        let now = Instant::now();
        let mut st = self.st.lock().unwrap();
        if config {
            st.config = Some(pkt.clone());
        }
        let mut lagging = false;
        for v in st.viewers.iter_mut() {
            if !v.lag.wants(config, key) {
                continue;
            }
            match v.tx.try_send(Message::Binary(pkt.clone())) {
                Ok(()) => v.lag.sent(config, key),
                Err(_) => {
                    v.lag.failed();
                    lagging = true;
                }
            }
        }
        // 有人跟不上：请设备尽快给出关键帧（限速，避免反复请求）
        if lagging && st.last_reset.is_none_or(|t| now.duration_since(t) >= RESET_INTERVAL) {
            let ctl = st.ctl.clone();
            if let Some(ctl) = ctl {
                let _ = ctl.send(reset_packet());
                st.last_reset = Some(now);
            }
        }
    }

    /// 声音包只发给要声音的观看者；队列满时直接丢弃
    fn publish_audio(&self, pkt: Bytes) {
        let st = self.st.lock().unwrap();
        for v in st.viewers.iter().filter(|v| v.audio) {
            let _ = v.tx.try_send(Message::Binary(pkt.clone()));
        }
    }

    /// 声音状态（不支持 / 出错 / 已开启）发给要声音的观看者
    fn publish_audio_state(&self, v: Value) {
        let m = text(v.clone());
        let mut st = self.st.lock().unwrap();
        st.audio_state = Some(v);
        for w in st.viewers.iter().filter(|w| w.audio) {
            let _ = w.tx.try_send(m.clone());
        }
    }

    /// 设备端剪贴板：只发给当前控制者，没有则发给最近一次的控制者
    fn send_clipboard(&self, s: String) {
        let st = self.st.lock().unwrap();
        let target = st
            .arb
            .controller()
            .filter(|c| *c != AI)
            .or_else(|| st.arb.last())
            .map(str::to_string);
        let Some(id) = target else { return };
        if let Some(v) = st.viewers.iter().find(|v| v.id == id) {
            let _ = v.tx.try_send(text(json!({"type":"clipboard","text":s})));
        }
    }

    /// 给某个观看者发 log 文本
    fn send_log(&self, vid: &str, msg: &str) {
        let st = self.st.lock().unwrap();
        if let Some(v) = st.viewers.iter().find(|v| v.id == vid) {
            let _ = v.tx.try_send(text(json!({"type":"log","msg":msg})));
        }
    }

    /// 转发 reset_video（按会话限速 1 秒一次）
    fn request_reset(&self) -> bool {
        let now = Instant::now();
        let mut st = self.st.lock().unwrap();
        if st.last_reset.is_some_and(|t| now.duration_since(t) < RESET_INTERVAL) {
            return false;
        }
        let Some(ctl) = st.ctl.clone() else { return false };
        let _ = ctl.send(reset_packet());
        st.last_reset = Some(now);
        true
    }

    /// 需要控制权的消息：仲裁通过后写入控制通道
    fn send_control(&self, vid: &str, ctl: &control::Ctl, parts: Vec<Vec<u8>>, now: Instant) -> SendResult {
        let delta = down_delta(ctl);
        let mut st = self.st.lock().unwrap();
        let Some(tx) = st.ctl.clone() else { return SendResult::NoSession };
        let before = st.arb.controller().map(str::to_string);
        // 只有“按下”才能取得控制权；别人的移动 / 抬起（按下时被拒绝了）直接丢弃
        if before.as_deref() != Some(vid) && matches!(ctl, control::Ctl::Touch { action, .. } if *action != 0) {
            return SendResult::Dropped;
        }
        if !st.arb.acquire(vid, now) {
            return SendResult::Busy;
        }
        let changed = before.as_deref() != Some(vid);
        if changed {
            // 接管：前一个控制者还按着的触点先取消
            for p in st.cancel_pointers() {
                let _ = tx.send(p);
            }
        }
        st.arb.count(delta);
        if let control::Ctl::Touch { action, id, x, y, .. } = ctl {
            match action {
                0 | 2 => {
                    st.pointers.insert(*id, (*x, *y));
                }
                _ => {
                    st.pointers.remove(id);
                }
            }
        }
        for p in parts {
            let _ = tx.send(p);
        }
        SendResult::Sent { changed }
    }

    /// 提示“XXX 正在操作”（同一观看者 1 秒内最多一次）
    fn notify_busy(&self, vid: &str) {
        let now = Instant::now();
        let mut st = self.st.lock().unwrap();
        let by = st.controller_name();
        let Some(v) = st.viewers.iter_mut().find(|v| v.id == vid) else { return };
        if v.busy_sent.is_some_and(|t| now.duration_since(t) < BUSY_INTERVAL) {
            return;
        }
        v.busy_sent = Some(now);
        let _ = v.tx.try_send(text(json!({"type":"busy","by": by})));
    }

    /// 观看者发来的文本消息（控制指令）；成功送入控制通道时返回解析后的指令供插件监听
    fn handle_text(&self, vid: &str, s: &str) -> Option<control::Ctl> {
        let ctl: control::Ctl = match serde_json::from_str(s) {
            Ok(c) => c,
            Err(e) => {
                self.send_log(vid, &tr!("无效控制指令：{e}", "Invalid control message: {e}"));
                return None;
            }
        };
        // reset_video 不需要控制权（任何观看者都可以发，按会话限速）
        let needs_control = !matches!(&ctl, control::Ctl::Cmd { name } if name == "reset_video");
        if !needs_control {
            self.request_reset();
            return None;
        }
        let parts = match control::encode(&ctl) {
            Ok(p) => p,
            Err(e) => {
                self.send_log(vid, &e);
                return None;
            }
        };
        match self.send_control(vid, &ctl, parts, Instant::now()) {
            SendResult::Sent { changed } => {
                if changed {
                    self.broadcast_viewers();
                }
                Some(ctl)
            }
            SendResult::Busy => {
                self.notify_busy(vid);
                None
            }
            SendResult::Dropped => None,
            SendResult::NoSession => {
                self.send_log(vid, &tr!("共享会话尚未就绪，请稍候", "The shared session is not ready yet"));
                None
            }
        }
    }

    /// 闲置超过 3 秒的控制者：释放并广播
    fn release_idle(&self) -> bool {
        let now = Instant::now();
        let mut st = self.st.lock().unwrap();
        st.arb.release_idle(now).is_some()
    }

    /// AI 事件：转发给所有观看者，并接管 / 释放控制权
    fn on_agent(&self, ev: Value) {
        let action = ev.get("action").and_then(Value::as_str).unwrap_or("").to_string();
        let changed = {
            let mut st = self.st.lock().unwrap();
            let before = st.arb.controller().map(str::to_string);
            match action.as_str() {
                "paused" => {
                    st.arb.release_if(AI);
                }
                "resumed" | "blocked" => {}
                _ => {
                    if st.arb.controller() != Some(AI) {
                        let cancels = st.cancel_pointers();
                        if let Some(ctl) = st.ctl.clone() {
                            for p in cancels {
                                let _ = ctl.send(p);
                            }
                        }
                    }
                    st.arb.set_ai(Instant::now());
                }
            }
            before != st.arb.controller().map(str::to_string)
        };
        self.broadcast(&text(ev));
        if changed {
            self.broadcast_viewers();
        }
    }

    /// 会话重启前：控制者还按着的话给设备补一条 touch cancel，并释放控制权
    fn cancel_touch(&self) {
        let mut st = self.st.lock().unwrap();
        let cancels = st.cancel_pointers();
        if let Some(ctl) = st.ctl.clone() {
            for p in cancels {
                let _ = ctl.send(p);
            }
        }
        st.arb = Arbiter::default();
    }

    /// 给新加入的观看者补发缓存的 meta / session / 配置包，并请求一次关键帧
    fn replay(&self, vid: &str) {
        let now = Instant::now();
        let mut st = self.st.lock().unwrap();
        let Some(i) = st.viewers.iter().position(|v| v.id == vid) else { return };
        let mut msgs: Vec<Message> = Vec::new();
        if let Some(m) = &st.meta {
            msgs.push(text(m.clone()));
        }
        if let Some((w, h)) = st.session {
            msgs.push(text(json!({"type":"session","width":w,"height":h})));
        }
        if let Some(c) = &st.config {
            msgs.push(Message::Binary(c.clone()));
        }
        if st.viewers[i].audio {
            if let Some(a) = &st.audio_state {
                msgs.push(text(a.clone()));
            }
        }
        for m in msgs {
            let _ = st.viewers[i].tx.try_send(m);
        }
        let ctl = st.ctl.clone();
        if let Some(ctl) = ctl {
            let _ = ctl.send(reset_packet());
            st.last_reset = Some(now);
        }
    }

    /// 观看者加入会话（必要时启动 / 重启 scrcpy-server）。自己拿 life 锁。
    async fn join(self: &Arc<Self>, state: &Arc<AppState>, guest: &Guest, conn: u64, opts: &StreamOpts, tx: Out, done: Arc<tokio::sync::Notify>) -> Result<(), JoinError> {
        let _life = self.life.lock().await;
        if !state.mirrors.is_current(&self.serial, self) {
            return Err(JoinError::Gone);
        }
        let (was_running, restart) = {
            let mut st = self.st.lock().unwrap();
            // 同一观看者重连：旧连接可能还没来得及离开，直接替换（控制权保留给它）
            if let Some(pos) = st.viewers.iter().position(|v| v.id == guest.id) {
                let old = st.viewers.remove(pos);
                old.done.notify_one();
            }
            st.viewers.push(Viewer {
                conn,
                id: guest.id.clone(),
                name: guest.name.clone(),
                ident: guest.ident.clone(),
                audio: opts.audio,
                tx,
                lag: LagState::default(),
                busy_sent: None,
                done,
            });
            let was_running = st.running;
            let mut restart = false;
            if !was_running {
                // 第一个观看者：按它的设置启动
                st.opts = opts.clone();
                st.meta = None;
                st.session = None;
                st.config = None;
            st.audio_state = None;
            st.pointers.clear();
                st.last_reset = None;
                st.arb = Arbiter::default();
            } else if guest.force {
                // 要求按我的画质设置重启
                st.opts = opts.clone();
                restart = true;
            } else if opts.audio && !st.opts.audio {
                // 后来的人要声音：开启声音（用它指定的编码）
                st.opts.audio = true;
                st.opts.audio_codec = opts.audio_codec.clone();
                st.opts.audio_dup = opts.audio_dup;
                restart = true;
            }
            // 重启时的 audio 取所有观看者中是否有人要声音
            st.opts.audio = st.viewers.iter().any(|v| v.audio);
            (was_running, restart)
        };

        if was_running {
            if restart {
                self.broadcast_status(&tr!("正在按新的画质设置重启共享会话…", "Restarting the shared session with the new settings…"));
                self.cancel_touch();
                self.stop_session().await;
                let opts = { self.st.lock().unwrap().opts.clone() };
                if let Err(e) = self.start_session(state, &opts).await {
                    return Err(JoinError::Failed(e));
                }
            } else {
                // 已在运行：补发缓存并请求关键帧
                self.replay(&guest.id);
            }
        } else {
            let opts = { self.st.lock().unwrap().opts.clone() };
            if let Err(e) = self.start_session(state, &opts).await {
                return Err(JoinError::Failed(e));
            }
        }
        self.broadcast_viewers();
        Ok(())
    }

    /// 观看者离开会话；最后一人离开时停掉一切。自己拿 life 锁。
    async fn leave(self: &Arc<Self>, state: &Arc<AppState>, vid: &str, conn: u64) {
        let _life = self.life.lock().await;
        let (removed, last) = {
            let mut st = self.st.lock().unwrap();
            // 按连接编号移除：已被同一观看者的新连接替换时什么都不做
            let removed = match st.viewers.iter().position(|v| v.conn == conn) {
                Some(pos) => {
                    let v = st.viewers.remove(pos);
                    v.done.notify_one();
                    true
                }
                None => false,
            };
            if removed && st.arb.controller() == Some(vid) && !st.viewers.iter().any(|v| v.id == vid) {
                // 控制者离开：还按着的触点先取消，避免手机上留下“按住”状态
                let cancels = st.cancel_pointers();
                st.arb.release();
                if let Some(ctl) = st.ctl.clone() {
                    for p in cancels {
                        let _ = ctl.send(p);
                    }
                }
            }
            (removed, st.viewers.is_empty())
        };
        if !removed {
            return;
        }
        if last {
            // 最后一人离开：停止会话并移除，之后有人来会创建新会话
            self.stop_session().await;
            let mut st = self.st.lock().unwrap();
            st.viewers.clear();
            drop(st);
            state.mirrors.remove(&self.serial, self);
        } else {
            self.broadcast_viewers();
        }
    }

    /// 停止当前会话（杀掉 scrcpy-server、移除 adb forward）。调用者需持有 life 锁。
    async fn stop_session(&self) {
        let task = self.task.lock().unwrap().take();
        if let Some(mut t) = task {
            if let Some(stop) = t.stop.take() {
                let _ = stop.send(());
            }
            let _ = t.handle.await;
        }
        let mut st = self.st.lock().unwrap();
        st.running = false;
        st.ctl = None;
        st.generation += 1;
    }

    /// 中止会话任务（启动卡住时用）。不动共享状态，由调用者随后清理。
    async fn abort_session(&self) {
        let task = self.task.lock().unwrap().take();
        if let Some(t) = task {
            t.handle.abort();
            let _ = t.handle.await;
        }
    }

    /// 启动 scrcpy-server 并等它握手完成。调用者需持有 life 锁。
    async fn start_session(self: &Arc<Self>, state: &Arc<AppState>, opts: &StreamOpts) -> Result<(), String> {
        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        let (stop_tx, stop_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let generation = {
            let mut st = self.st.lock().unwrap();
            st.generation += 1;
            st.running = true;
            st.ctl = Some(ctl_tx);
            st.meta = None;
            st.session = None;
            st.config = None;
            st.audio_state = None;
            st.pointers.clear();
            st.last_reset = None;
            st.arb = Arbiter::default();
            st.generation
        };
        let handle = tokio::spawn(session_task(self.clone(), state.clone(), opts.clone(), generation, ctl_rx, stop_rx, ready_tx));
        *self.task.lock().unwrap() = Some(SessionTask { handle, stop: Some(stop_tx) });
        let ready = match tokio::time::timeout(Duration::from_secs(120), ready_rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => Err(tr!("scrcpy-server 未能启动", "scrcpy-server did not start")),
            Err(_) => Err(tr!("连接 scrcpy-server 超时", "Timed out connecting to scrcpy-server")),
        };
        match ready {
            Ok(()) => Ok(()),
            Err(e) => {
                self.abort_session().await;
                self.fail_locked(state, generation, e.clone()).await;
                Err(e)
            }
        }
    }

    /// 会话失败：给所有观看者发 error 并关闭它们的 WebSocket，然后从 mirrors 移除。
    /// 调用者需持有 life 锁。
    async fn fail_locked(self: &Arc<Self>, state: &Arc<AppState>, generation: u64, msg: String) {
        let viewers = {
            let mut st = self.st.lock().unwrap();
            // 已经被停止 / 已有更新的会话：什么都不做
            if !st.running || st.generation != generation {
                return;
            }
            st.running = false;
            st.generation += 1;
            st.ctl = None;
            st.meta = None;
            st.session = None;
            st.config = None;
            st.audio_state = None;
            st.pointers.clear();
            st.last_reset = None;
            st.arb = Arbiter::default();
            std::mem::take(&mut st.viewers)
        };
        let err = text(json!({"type":"error","msg":msg}));
        for v in &viewers {
            let _ = v.tx.try_send(err.clone());
            v.done.notify_one();
        }
        // 发送端全部关闭：写任务收尾后关闭 WebSocket，前端会自动重连
        drop(viewers);
        state.mirrors.remove(&self.serial, self);
        let _ = self.task.lock().unwrap().take();
    }
}

// ---------------------------------------------------------------- 会话任务

/// 启动会话任务：握手 → 转发视频/声音/控制 → 结束时清理
async fn session_task(
    mirror: Arc<Mirror>,
    state: Arc<AppState>,
    opts: StreamOpts,
    generation: u64,
    ctl_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    stop_rx: oneshot::Receiver<()>,
    ready: oneshot::Sender<Result<(), String>>,
) {
    let serial = mirror.serial.clone();
    let mut ready = Some(ready);
    let parts = match setup(&mirror, &state, &serial, &opts).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("[{serial}] session start failed: {e}");
            if let Some(r) = ready.take() {
                let _ = r.send(Err(e.clone()));
            }
            let m = mirror.clone();
            let st = state.clone();
            tokio::spawn(async move {
                let _life = m.life.lock().await;
                m.fail_locked(&st, generation, e).await;
            });
            return;
        }
    };
    tracing::info!("[{serial}] session start {opts:?}");
    if let Some(r) = ready.take() {
        let _ = r.send(Ok(()));
    }
    match run_loop(&mirror, &state, &serial, parts, ctl_rx, stop_rx).await {
        Ok(()) => tracing::info!("[{serial}] session end (stopped)"),
        Err(e) => {
            tracing::warn!("[{serial}] session end: {e}");
            let m = mirror.clone();
            let st = state.clone();
            tokio::spawn(async move {
                let _life = m.life.lock().await;
                m.fail_locked(&st, generation, e).await;
            });
        }
    }
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

/// 启动阶段拿到的通道
struct Parts {
    child: tokio::process::Child,
    video: TcpStream,
    audio: Option<TcpStream>,
    control: TcpStream,
    /// 进程存活期间一直持有，结束时移除 adb forward
    _forward: Forward,
}

/// 推送 scrcpy-server、建立 adb forward 并启动它，然后连接视频 / 声音 / 控制通道
async fn setup(mirror: &Arc<Mirror>, state: &Arc<AppState>, serial: &str, opts: &StreamOpts) -> Result<Parts, String> {
    let adb = &state.cfg.adb;
    let jar = state.cfg.server_jar.to_string_lossy().into_owned();
    // 和启动并行：设备屏幕上是否自带导航按键
    let nav = tokio::spawn(adb::nav_buttons(adb.clone(), serial.to_string()));

    // 启动阶段串行化，避免并发推送同一个 jar
    let start_guard = state.start_lock.lock().await;

    mirror.broadcast_status(&tr!("推送 scrcpy-server…", "Pushing scrcpy-server…"));
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
    let forward = Forward { adb: adb.clone(), serial: serial.to_string(), port };

    mirror.broadcast_status(&tr!("启动 scrcpy-server…", "Starting scrcpy-server…"));
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
    if opts.audio {
        args.push("audio=true".into());
        args.push(format!("audio_codec={}", if opts.audio_codec == "raw" { "raw" } else { "opus" }));
        if opts.audio_dup {
            args.push("audio_dup=true".into());
        }
    } else {
        args.push("audio=false".into());
    }
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

    // 设备端日志：写入本地日志，错误行同时推给所有观看者
    for pipe in [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
    ]
    .into_iter()
    .flatten()
    {
        let mirror = mirror.clone();
        let serial = serial.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(pipe).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!("[{serial}] device: {line}");
                if line.contains("ERROR") || line.contains("Exception") || line.contains("WARN") {
                    mirror.broadcast(&text(json!({"type":"log","msg":line})));
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
    // 通道顺序：视频、声音、控制
    let audio = if opts.audio {
        Some(
            TcpStream::connect(("127.0.0.1", port))
                .await
                .map_err(|e| tr!("连接声音通道失败：{e}", "Failed to open audio channel: {e}"))?,
        )
    } else {
        None
    };
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
    let nav_buttons = tokio::time::timeout(Duration::from_secs(2), nav).await.ok().and_then(Result::ok).flatten();
    mirror.publish_meta(json!({"type":"meta","device_name":device_name,"codec":codec,"serial":serial,"nav_buttons":nav_buttons}));

    Ok(Parts { child, video, audio, control, _forward: forward })
}

/// 会话主循环一轮的结果
enum LoopEnd {
    /// 被要求停止（重启、最后一个观看者离开）
    Stop,
    /// 有控制指令要写入（None 表示控制通道已关闭）
    Ctl(Option<Vec<u8>>),
    /// 视频读任务结束
    Video(Result<Result<(), String>, tokio::task::JoinError>),
    /// scrcpy-server 退出
    Exit(std::io::Result<std::process::ExitStatus>),
}

/// 会话主循环：转发控制指令，等待停止信号 / 视频流断开 / scrcpy-server 退出
async fn run_loop(
    mirror: &Arc<Mirror>,
    state: &Arc<AppState>,
    serial: &str,
    parts: Parts,
    mut ctl_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut stop_rx: oneshot::Receiver<()>,
) -> Result<(), String> {
    let Parts { mut child, video, audio, control, _forward } = parts;
    let (ctl_r, mut ctl_w) = control.into_split();
    let mut video_task = tokio::spawn(read_video(mirror.clone(), video));
    let msg_task = tokio::spawn(read_device_msgs(mirror.clone(), ctl_r));
    let audio_task = audio.map(|a| {
        let _ = a.set_nodelay(true);
        tokio::spawn(read_audio(mirror.clone(), a))
    });
    let agent_task = tokio::spawn(agent_events(mirror.clone(), state.clone()));
    let tick_task = tokio::spawn(idle_ticker(mirror.clone()));

    let result = loop {
        // 先算出这一轮发生了什么事（select 结束后才能再用 ctl_rx）
        let end = tokio::select! {
            _ = &mut stop_rx => LoopEnd::Stop,
            m = ctl_rx.recv() => LoopEnd::Ctl(m),
            r = &mut video_task => LoopEnd::Video(r),
            exit = child.wait() => LoopEnd::Exit(exit),
        };
        match end {
            LoopEnd::Stop => {
                // 停止前先把已经排队的控制指令写完（例如重启前的 touch cancel）
                while let Ok(buf) = ctl_rx.try_recv() {
                    if ctl_w.write_all(&buf).await.is_err() {
                        break;
                    }
                }
                break Ok(());
            }
            LoopEnd::Ctl(Some(buf)) => {
                if let Err(e) = ctl_w.write_all(&buf).await {
                    break Err(tr!("写入控制通道失败：{e}", "Failed to write control channel: {e}"));
                }
            }
            LoopEnd::Ctl(None) => break Err(tr!("控制通道已关闭", "The control channel was closed")),
            LoopEnd::Video(r) => {
                break Err(match r {
                    Ok(Err(e)) => e,
                    _ => tr!("视频流已结束", "Video stream ended"),
                });
            }
            LoopEnd::Exit(exit) => {
                let st = exit.map(|s| s.to_string()).unwrap_or_default();
                break Err(tr!("scrcpy-server 已退出（{st}）", "scrcpy-server exited ({st})"));
            }
        }
    };

    video_task.abort();
    msg_task.abort();
    agent_task.abort();
    tick_task.abort();
    if let Some(t) = audio_task {
        t.abort();
    }
    let _ = child.kill().await;
    tracing::info!("[{serial}] session loop end");
    result
}

/// 声音流：编码 ID，然后是带 12 字节头的数据包。转发给浏览器时 flags 置 bit2（0x04）
async fn read_audio(mirror: Arc<Mirror>, mut s: TcpStream) {
    let mut cid = [0u8; 4];
    if s.read_exact(&mut cid).await.is_err() {
        return;
    }
    match u32::from_be_bytes(cid) {
        0 => {
            let msg = tr!("这台设备不支持声音转发（需要 Android 11 及以上；Android 11 需在开始投屏时解锁屏幕）", "This device cannot forward audio (requires Android 11+; on Android 11 the screen must be unlocked when mirroring starts)");
            mirror.publish_audio_state(json!({"type":"audio","state":"unsupported","msg":msg}));
            return;
        }
        1 => {
            let msg = tr!("设备端声音配置错误", "Audio configuration error on the device");
            mirror.publish_audio_state(json!({"type":"audio","state":"error","msg":msg}));
            return;
        }
        _ => {}
    }
    let codec = String::from_utf8_lossy(&cid).trim_matches(char::from(0)).to_string();
    mirror.publish_audio_state(json!({"type":"audio","state":"on","codec":codec}));
    let mut hdr = [0u8; 12];
    loop {
        if s.read_exact(&mut hdr).await.is_err() {
            return;
        }
        let pts_flags = u64::from_be_bytes(hdr[0..8].try_into().unwrap());
        let len = u32::from_be_bytes(hdr[8..12].try_into().unwrap()) as usize;
        if len == 0 || len > 4 * 1024 * 1024 {
            return;
        }
        let config = pts_flags & (1 << 62) != 0;
        let pts = pts_flags & ((1 << 61) - 1);
        let mut buf = vec![0u8; 9 + len];
        buf[0] = 0x04 | u8::from(config);
        buf[1..9].copy_from_slice(&pts.to_be_bytes());
        if s.read_exact(&mut buf[9..]).await.is_err() {
            return;
        }
        mirror.publish_audio(Bytes::from(buf));
    }
}

/// 视频流：session 包更新画面尺寸，其余转发给观看者
async fn read_video(mirror: Arc<Mirror>, mut s: TcpStream) -> Result<(), String> {
    let mut hdr = [0u8; 12];
    loop {
        s.read_exact(&mut hdr).await.map_err(|_| tr!("视频流已断开", "Video stream disconnected"))?;
        if hdr[0] & 0x80 != 0 {
            // session 包：视频尺寸变化（旋转等）
            let w = u32::from_be_bytes(hdr[4..8].try_into().unwrap());
            let h = u32::from_be_bytes(hdr[8..12].try_into().unwrap());
            mirror.publish_session(w, h);
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
        mirror.publish_video(Bytes::from(buf), config, key);
    }
}

/// 解析设备端发来的消息（剪贴板等）
async fn read_device_msgs(mirror: Arc<Mirror>, r: OwnedReadHalf) {
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
                let s = String::from_utf8_lossy(&buf).into_owned();
                mirror.send_clipboard(s);
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

/// AI 事件：serial 匹配的转发给所有观看者，并让它接管控制权
async fn agent_events(mirror: Arc<Mirror>, state: Arc<AppState>) {
    let mut rx = state.agent.events.subscribe();
    loop {
        match rx.recv().await {
            Ok((serial, v)) => {
                if serial == mirror.serial {
                    mirror.on_agent(v);
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// 每秒检查一次：控制者停手 3 秒后释放控制权，并及时让界面上的“XX 正在操作”消失
async fn idle_ticker(mirror: Arc<Mirror>) {
    let mut iv = tokio::time::interval(Duration::from_secs(1));
    loop {
        iv.tick().await;
        if mirror.release_idle() {
            mirror.broadcast_viewers();
        }
    }
}

// ---------------------------------------------------------------- 观看者

/// 处理一个观看者的 WebSocket：加入共享会话、转发控制指令、离开时清理
pub async fn run_session(state: Arc<AppState>, serial: String, opts: StreamOpts, guest: Guest, socket: WebSocket) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(QUEUE);
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if ws_tx.send(m).await.is_err() {
                break;
            }
        }
        let _ = ws_tx.close().await;
    });
    let done = Arc::new(tokio::sync::Notify::new());
    static CONN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let conn = CONN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // 加入会话；会话恰好在关闭时（已从 mirrors 移除）换一个新会话重试
    let mut joined = None;
    for _ in 0..4 {
        let mirror = state.mirrors.get_or_create(&serial, &opts);
        match mirror.join(&state, &guest, conn, &opts, out_tx.clone(), done.clone()).await {
            Ok(()) => {
                joined = Some(mirror);
                break;
            }
            Err(JoinError::Gone) => continue,
            Err(JoinError::Failed(e)) => {
                // 所有观看者（含自己）已经收到 error，WebSocket 会被关闭
                tracing::warn!("[{serial}] join failed: {e}");
                drop(out_tx);
                let _ = writer.await;
                return;
            }
        }
    }
    let Some(mirror) = joined else {
        let _ = out_tx.try_send(text(json!({"type":"error","msg":tr!("无法加入共享会话，请稍后重试", "Cannot join the shared session, please try again")})));
        drop(out_tx);
        let _ = writer.await;
        return;
    };

    let vid = guest.id;
    loop {
        tokio::select! {
            r = ws_rx.next() => match r {
                Some(Ok(Message::Text(t))) => {
                    if let Some(ctl) = mirror.handle_text(&vid, t.as_str()) {
                        state.plugins.on_user_ctl(&state, &serial, &ctl, mirror.session_size());
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(_)) => break,
                _ => {}
            },
            _ = done.notified() => break,
        }
    }
    mirror.leave(&state, &vid, conn).await;
    drop(out_tx);
    let _ = writer.await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> StreamOpts {
        StreamOpts { max_size: 0, bit_rate: 0, max_fps: 0, stay_awake: false, audio: false, audio_codec: "opus".into(), audio_dup: false }
    }

    fn viewer(id: &str, name: &str, audio: bool) -> (Viewer, mpsc::Receiver<Message>) {
        let (tx, rx) = mpsc::channel(4);
        let v = Viewer {
            conn: 0,
            id: id.into(),
            name: name.into(),
            ident: None,
            audio,
            tx,
            lag: LagState::default(),
            busy_sent: None,
            done: Arc::new(tokio::sync::Notify::new()),
        };
        (v, rx)
    }

    #[test]
    fn vid_sanitizing() {
        assert_eq!(sanitize_vid(Some("aB3_-")), "aB3_-");
        let full = "x".repeat(32);
        assert_eq!(sanitize_vid(Some(&full)), full);
        // 缺失或非法：随机生成 12 位 [a-z0-9]
        let a = sanitize_vid(None);
        assert_eq!(a.chars().count(), 12);
        assert!(a.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        assert_ne!(a, sanitize_vid(None));
        for bad in ["", "a b", "a/../b", "中文", "a.b"] {
            let v = sanitize_vid(Some(bad));
            assert_eq!(v.chars().count(), 12, "{bad}");
        }
        let long = "y".repeat(33);
        assert_eq!(sanitize_vid(Some(&long)).chars().count(), 12);
    }

    #[test]
    fn who_sanitizing() {
        assert_eq!(sanitize_who(Some("小王")), "小王");
        assert_eq!(sanitize_who(Some(" 小\u{7}王\n")), "小王");
        assert_eq!(sanitize_who(Some("   ")), "?");
        assert_eq!(sanitize_who(None), "?");
        assert_eq!(sanitize_who(Some("OPD2407 上的浏览器")), "OPD2407 上的浏览器");
        let long = "名".repeat(60);
        assert_eq!(sanitize_who(Some(&long)).chars().count(), WHO_MAX);
    }

    #[test]
    fn lag_drop_recovery() {
        let mut l = LagState::default();
        assert!(!l.lagging());
        assert!(l.wants(false, false));
        // 队列满：只放行关键帧与配置包
        l.failed();
        assert!(l.lagging());
        assert!(!l.wants(false, false));
        assert!(l.wants(false, true));
        assert!(l.wants(true, false));
        // 非关键帧即使发送成功也不恢复
        l.sent(false, false);
        assert!(l.lagging());
        // 关键帧发送成功后恢复
        l.sent(false, true);
        assert!(!l.lagging());
        assert!(l.wants(false, false));
        // 配置包同样能恢复
        l.failed();
        l.sent(true, false);
        assert!(!l.lagging());
    }

    #[test]
    fn control_first_come_wins() {
        let t0 = Instant::now();
        let mut a = Arbiter::default();
        assert!(a.acquire("A", t0));
        assert_eq!(a.controller(), Some("A"));
        assert_eq!(a.last(), Some("A"));
        // 刚动过：别人抢不到
        assert!(!a.acquire("B", t0 + Duration::from_millis(500)));
        assert_eq!(a.controller(), Some("A"));
        // 停手 3 秒：被接管
        assert!(a.acquire("B", t0 + IDLE_RELEASE));
        assert_eq!(a.controller(), Some("B"));
        assert_eq!(a.last(), Some("B"));
    }

    #[test]
    fn control_same_controller_keeps_down() {
        let t0 = Instant::now();
        let mut a = Arbiter::default();
        assert!(a.acquire("A", t0));
        a.count(down_delta(&control::Ctl::Touch { action: 0, id: -2, x: 0, y: 0, w: 1, h: 1, p: 1.0, buttons: 0 }));
        assert_eq!(a.down(), 1);
        // 自己继续操作：down 不清零
        assert!(a.acquire("A", t0 + Duration::from_secs(10)));
        assert_eq!(a.down(), 1);
        assert_eq!(a.controller(), Some("A"));
    }

    #[test]
    fn control_down_blocks_and_safety_takeover() {
        let t0 = Instant::now();
        let mut a = Arbiter::default();
        assert!(a.acquire("A", t0));
        a.count(1);
        // 按着不放（down > 0）：3 秒后也抢不走
        assert!(!a.acquire("B", t0 + Duration::from_secs(5)));
        // 15 秒安全上限：可以接管，且 down 归零
        assert!(a.acquire("B", t0 + SAFETY_RELEASE));
        assert_eq!(a.controller(), Some("B"));
        assert_eq!(a.down(), 0);
        // 计数不会小于 0
        a.count(-1);
        a.count(-1);
        assert_eq!(a.down(), 0);
    }

    #[test]
    fn control_idle_release() {
        let t0 = Instant::now();
        let mut a = Arbiter::default();
        assert_eq!(a.release_idle(t0), None);
        assert!(a.acquire("A", t0));
        assert_eq!(a.release_idle(t0 + Duration::from_secs(1)), None);
        assert_eq!(a.release_idle(t0 + IDLE_RELEASE), Some("A".to_string()));
        assert_eq!(a.controller(), None);
        // 按着的时候不释放
        assert!(a.acquire("B", t0 + Duration::from_secs(4)));
        a.count(1);
        assert_eq!(a.release_idle(t0 + Duration::from_secs(30)), None);
        assert_eq!(a.controller(), Some("B"));
    }

    #[test]
    fn control_ai() {
        let t0 = Instant::now();
        let mut a = Arbiter::default();
        a.set_ai(t0);
        assert_eq!(a.controller(), Some("ai"));
        // AI 不参与剪贴板兜底
        assert_eq!(a.last(), None);
        // AI 一直在操作：观看者抢不到
        assert!(!a.acquire("A", t0 + Duration::from_millis(800)));
        // AI 停手 3 秒后可以被接管
        assert!(a.acquire("A", t0 + IDLE_RELEASE));
        assert_eq!(a.controller(), Some("A"));
        // paused：AI 释放控制权
        a.set_ai(t0 + Duration::from_secs(5));
        assert_eq!(a.release_if("ai"), Some("ai".to_string()));
        assert_eq!(a.controller(), None);
        assert_eq!(a.release_if("ai"), None);
    }

    #[test]
    fn control_leave_release() {
        let t0 = Instant::now();
        let mut a = Arbiter::default();
        assert!(a.acquire("A", t0));
        a.count(1);
        // 不是控制者离开：不影响
        assert_eq!(a.release_if("B"), None);
        assert_eq!(a.controller(), Some("A"));
        // 控制者离开：释放并清空 down
        assert_eq!(a.release_if("A"), Some("A".to_string()));
        assert_eq!(a.controller(), None);
        assert_eq!(a.down(), 0);
        // 最近一次控制者仍然记得
        assert_eq!(a.last(), Some("A"));
    }

    #[test]
    fn down_delta_rules() {
        let touch = |action| control::Ctl::Touch { action, id: -2, x: 0, y: 0, w: 1, h: 1, p: 1.0, buttons: 0 };
        assert_eq!(down_delta(&touch(0)), 1);
        assert_eq!(down_delta(&touch(1)), -1);
        assert_eq!(down_delta(&touch(2)), 0);
        assert_eq!(down_delta(&touch(3)), -1);
        let key = |action| control::Ctl::Key { action, code: 4, repeat: 0, meta: 0 };
        assert_eq!(down_delta(&key(0)), 1);
        assert_eq!(down_delta(&key(1)), -1);
        assert_eq!(down_delta(&control::Ctl::Back { action: 0 }), 1);
        assert_eq!(down_delta(&control::Ctl::Back { action: 1 }), -1);
        assert_eq!(down_delta(&control::Ctl::Cmd { name: "rotate".into() }), 0);
    }

    #[test]
    fn viewers_message() {
        let m = Mirror::new("abc", opts());
        let (v1, _r1) = viewer("v1", "小王", false);
        let (v2, _r2) = viewer("v2", "OPD2407 上的浏览器", true);
        let mut st = m.st.lock().unwrap();
        st.viewers.push(v1);
        st.viewers.push(v2);
        st.arb.acquire("v1", Instant::now());
        let v = st.viewers_msg("v2");
        assert_eq!(v["type"], "viewers");
        assert_eq!(v["you"], "v2");
        assert_eq!(v["list"][0]["name"], "小王");
        assert_eq!(v["list"][0]["audio"], false);
        assert_eq!(v["list"][1]["audio"], true);
        assert_eq!(v["controller"], "v1");
        assert_eq!(v["controller_name"], "小王");
        // AI 控制
        st.arb.set_ai(Instant::now());
        assert_eq!(st.viewers_msg("v1")["controller"], "ai");
        assert_eq!(st.viewers_msg("v1")["controller_name"], "AI");
        // 没有人控制
        st.arb.release();
        assert_eq!(st.viewers_msg("v1")["controller"], Value::Null);
        assert_eq!(st.viewers_msg("v1")["controller_name"], Value::Null);
    }

    #[test]
    fn mirrors_registry() {
        let m = Mirrors::default();
        let a = m.get_or_create("abc", &opts());
        assert!(Arc::ptr_eq(&a, &m.get_or_create("abc", &opts())));
        assert!(m.is_current("abc", &a));
        let b = m.get_or_create("xyz", &opts());
        assert!(!m.is_current("abc", &b));
        // 不是当前实例：不删除
        m.remove("abc", &b);
        assert!(m.get("abc").is_some());
        m.remove("abc", &a);
        assert!(m.get("abc").is_none());
        assert_eq!(m.list().len(), 1);
        assert_eq!(a.summary()["serial"], "abc");
        assert_eq!(a.summary()["controller"], Value::Null);
    }
}
