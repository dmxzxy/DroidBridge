//! 插件管理器（主进程侧）。规格见 docs/plugins/SPEC.md。
//!
//! 每个已启用的插件运行在自己的宿主子进程（`<exe> --plugin-host`）里，主进程与宿主通过
//! stdin/stdout 交换 NDJSON（SPEC 第 3 节）。本模块负责：扫描与校验清单、状态文件、
//! 权限授予、子进程生命周期（崩溃重启、ping 看门狗、热重载）、宿主发起的 call（SPEC 3.3）、
//! 设备与前台应用事件、控制台与审计日志、以及 HTTP/WS 接口（SPEC 4.1）。

mod calls;
mod devop;
mod host;
mod http;
mod manifest;
mod sources;

pub use devop::plugins_devop;
pub use http::{
    plugin_web_file, plugin_web_sdk, plugins_console, plugins_contrib, plugins_dialog, plugins_get, plugins_invoke, plugins_manage,
    plugins_mcp_list, plugins_metrics, plugins_panel, plugins_upload, plugins_web, plugins_ws,
};
pub use manifest::Manifest;
pub use sources::{run as sources_run, sources_get, sources_post};

use crate::{adb, agent, control, AppState};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, oneshot, Semaphore};

/// 插件状态（SPEC 第 4 节）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PluginState {
    Stopped,
    Starting,
    Running,
    Crashed,
    Failed,
    NeedsGrant,
    Disabled,
    Invalid,
}

impl PluginState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Crashed => "crashed",
            Self::Failed => "failed",
            Self::NeedsGrant => "needs_grant",
            Self::Disabled => "disabled",
            Self::Invalid => "invalid",
        }
    }
}

/// 插件注册的界面贡献（SPEC 3.3 的 register）
#[derive(Default)]
pub(crate) struct Contribs {
    menus: Vec<Value>,
    /// (是否 webpanel, webpanel 的入口文件, def)
    panels: Vec<(bool, String, Value)>,
    status: Vec<Value>,
    metrics: Vec<Value>,
    loghl: Vec<Value>,
    mcps: Vec<Value>,
    events: BTreeSet<String>,
    /// db.export 提供给其他插件调用的函数名
    exports: BTreeSet<String>,
    badges: HashMap<String, Value>,
    overlays: HashMap<String, Value>,
}

impl Contribs {
    fn list(&mut self, kind: &str) -> Option<&mut Vec<Value>> {
        match kind {
            "menu" => Some(&mut self.menus),
            "status" => Some(&mut self.status),
            "metric" => Some(&mut self.metrics),
            "loghl" => Some(&mut self.loghl),
            "mcp" => Some(&mut self.mcps),
            _ => None,
        }
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

/// 插件自己建立的 adb 转发（插件停止时自动移除）
#[derive(Clone, Debug)]
pub(crate) struct Forward {
    pub serial: String,
    pub local: String,
    pub reverse: bool,
}

/// 一台设备的插件视图（SPEC 7.1）：`id` 是插件看到的标识（远程为 `<peerId>/<serial>`）
#[derive(Clone, Debug)]
pub(crate) struct PluginDev {
    pub id: String,
    /// 原始 serial（不含 peerId），match 用它
    pub serial: String,
    /// 所在远程电脑（本机为 None）
    pub peer: Option<String>,
    pub computer: String,
    pub model: String,
    pub name: String,
    pub state: String,
}

impl PluginDev {
    /// 事件参数里的 dev 对象（SPEC 7.1）
    pub(crate) fn json(&self) -> Value {
        json!({
            "serial": self.id,
            "model": self.model,
            "name": self.name,
            "state": self.state,
            "computer": self.computer,
            "remote": self.peer.is_some(),
        })
    }
}

/// 设备标识 → (远程电脑 id, 原始 serial)；不含 `/` 的是本机设备（SPEC 7.1）
pub(crate) fn split_dev(id: &str) -> Option<(&str, &str)> {
    let (peer, serial) = id.split_once('/')?;
    (!peer.is_empty() && !serial.is_empty()).then_some((peer, serial))
}

/// match 规则与 filter.serials 用的原始 serial（去掉 peerId）
pub(crate) fn raw_serial(id: &str) -> &str {
    split_dev(id).map(|(_, s)| s).unwrap_or(id)
}

/// 每秒 `per_sec` 次的限流（令牌桶）
struct Rate {
    per_sec: f64,
    tokens: f64,
    last: Instant,
}

impl Rate {
    fn new(per_sec: f64) -> Self {
        Self { per_sec, tokens: per_sec, last: Instant::now() }
    }

    /// 取一个令牌；需要等待时返回等待时长
    fn take(&mut self) -> Duration {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * self.per_sec).min(self.per_sec);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 - self.tokens) / self.per_sec)
        }
    }
}

/// 插件触控 move 帧每秒上限（够 4 台设备 60Hz 跟手），超出的帧直接丢弃
const MOVE_PER_SEC: f64 = 240.0;
/// 每个插件待派发的投屏输入事件上限，插件处理不过来时丢弃新事件
const INPUT_QUEUE: usize = 512;

/// 待派发给某个插件的一次投屏人工输入
struct InputJob {
    dev_id: String,
    dev_json: Value,
    raw: Option<Value>,
    act: Option<Value>,
}

/// 跟踪用户在投屏窗口中的触控与按键序列，合成为语义动作（`device.action`）与节流后的实时流（`device.input`）
#[derive(Default)]
struct InputTracker {
    pointers: HashMap<(String, i64), PointerTrack>,
}

struct PointerTrack {
    start_x: i32,
    start_y: i32,
    w: u16,
    h: u16,
    down_at: Instant,
    last_emit: Instant,
    moved: bool,
}

impl InputTracker {
    /// 处理一条用户控制消息，返回 `(实时原始事件 device.input, 语义动作事件 device.action)`
    fn ingest(&mut self, dev_id: &str, ctl: &control::Ctl, session: Option<(u32, u32)>, now: Instant) -> (Option<Value>, Option<Value>) {
        let fallback_wh = || session.map(|(a, b)| (a.min(u16::MAX as u32) as u16, b.min(u16::MAX as u32) as u16)).unwrap_or((0, 0));
        match ctl {
            control::Ctl::Touch { action, id, x, y, w, h, p, .. } => {
                let (vw, vh) = if *w > 0 && *h > 0 { (*w, *h) } else { fallback_wh() };
                let key = (dev_id.to_string(), *id);
                match *action {
                    0 => {
                        self.pointers.insert(
                            key,
                            PointerTrack { start_x: *x, start_y: *y, w: vw, h: vh, down_at: now, last_emit: now, moved: false },
                        );
                        (Some(json!({"t": "touch", "action": 0, "id": id, "x": x, "y": y, "w": vw, "h": vh, "p": p})), None)
                    }
                    2 => {
                        let mut emit = false;
                        if let Some(trk) = self.pointers.get_mut(&key) {
                            if vw > 0 && vh > 0 {
                                trk.w = vw;
                                trk.h = vh;
                            }
                            let thresh = ((trk.w.max(trk.h) as i32) / 45).max(12);
                            if (*x - trk.start_x).abs() > thresh || (*y - trk.start_y).abs() > thresh {
                                trk.moved = true;
                            }
                            if now.saturating_duration_since(trk.last_emit) >= Duration::from_millis(16) {
                                trk.last_emit = now;
                                emit = true;
                            }
                        } else {
                            emit = true;
                        }
                        let raw = emit.then(|| json!({"t": "touch", "action": 2, "id": id, "x": x, "y": y, "w": vw, "h": vh, "p": p}));
                        (raw, None)
                    }
                    1 => {
                        let raw = Some(json!({"t": "touch", "action": 1, "id": id, "x": x, "y": y, "w": vw, "h": vh, "p": 0.0}));
                        let act = self.pointers.remove(&key).map(|trk| {
                            let (ew, eh) = if vw > 0 && vh > 0 { (vw, vh) } else { (trk.w, trk.h) };
                            let thresh = ((ew.max(eh) as i32) / 45).max(12);
                            let moved = trk.moved || (*x - trk.start_x).abs() > thresh || (*y - trk.start_y).abs() > thresh;
                            let dur = now.saturating_duration_since(trk.down_at).as_millis().min(10_000) as u64;
                            if moved {
                                json!({"kind": "swipe", "x": trk.start_x, "y": trk.start_y, "x2": x, "y2": y, "w": ew, "h": eh, "duration": dur.max(60)})
                            } else if dur >= 500 {
                                json!({"kind": "longpress", "x": trk.start_x, "y": trk.start_y, "w": ew, "h": eh, "duration": dur})
                            } else {
                                json!({"kind": "tap", "x": x, "y": y, "w": ew, "h": eh})
                            }
                        });
                        (raw, act)
                    }
                    3 => {
                        self.pointers.remove(&key);
                        (Some(json!({"t": "touch", "action": 3, "id": id, "x": x, "y": y, "w": vw, "h": vh, "p": 0.0})), None)
                    }
                    _ => (None, None),
                }
            }
            control::Ctl::Scroll { x, y, w, h, hs, vs } => {
                let (vw, vh) = if *w > 0 && *h > 0 { (*w, *h) } else { fallback_wh() };
                (
                    Some(json!({"t": "scroll", "x": x, "y": y, "w": vw, "h": vh, "hs": hs, "vs": vs})),
                    Some(json!({"kind": "scroll", "x": x, "y": y, "w": vw, "h": vh, "hs": hs, "vs": vs})),
                )
            }
            control::Ctl::Key { action, code, repeat, meta } => {
                if *repeat > 0 {
                    return (None, None);
                }
                let kname = agent::key_name(*code).map(String::from).unwrap_or_else(|| code.to_string());
                let raw = Some(json!({"t": "key", "action": action, "code": code, "key": kname, "meta": meta}));
                let act = (*action == 0).then(|| json!({"kind": "key", "key": kname, "code": code, "meta": meta}));
                (raw, act)
            }
            control::Ctl::Back { action } => {
                let raw = Some(json!({"t": "back", "action": action}));
                let act = (*action == 0).then(|| json!({"kind": "key", "key": "back", "code": 4, "meta": 0}));
                (raw, act)
            }
            control::Ctl::Text { text } => {
                if text.is_empty() {
                    return (None, None);
                }
                (Some(json!({"t": "text", "text": text})), Some(json!({"kind": "text", "text": text})))
            }
            control::Ctl::Clipboard { text, paste, .. } => {
                let raw = Some(json!({"t": "clipboard", "text": text, "paste": paste}));
                let act = (*paste && !text.is_empty()).then(|| json!({"kind": "text", "text": text}));
                (raw, act)
            }
            control::Ctl::Power { on } => (Some(json!({"t": "power", "on": on})), Some(json!({"kind": "power", "on": on}))),
            control::Ctl::Cmd { name } if name != "reset_video" => {
                (Some(json!({"t": "cmd", "name": name})), Some(json!({"kind": "cmd", "name": name})))
            }
            _ => (None, None),
        }
    }
}

/// 为插件创建投屏输入事件的派发任务：按到达顺序逐个发给插件，插件卸载后自动结束
fn input_worker(st: Arc<AppState>, ph: &Arc<Plugin>) -> mpsc::Sender<InputJob> {
    let (tx, mut rx) = mpsc::channel::<InputJob>(INPUT_QUEUE);
    let weak = Arc::downgrade(ph);
    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            let Some(ph) = weak.upgrade() else { break };
            if !st.plugins.device_allowed(&st, &ph, &job.dev_id).await {
                continue;
            }
            let Some(h) = ph.host() else { continue };
            if let Some(ev) = job.raw.filter(|_| ph.is_subscribed("device.input")) {
                let _ = h.request("event", json!({"name": "device.input", "args": [job.dev_json.clone(), ev]}), Duration::from_secs(5)).await;
            }
            if let Some(ev) = job.act.filter(|_| ph.is_subscribed("device.action")) {
                if let Err(e) = h.request("event", json!({"name": "device.action", "args": [job.dev_json, ev]}), Duration::from_secs(10)).await {
                    st.plugins.console(&ph, "warn", &tr!("事件 device.action 处理失败：{e}", "Event device.action failed: {e}"), None);
                }
            }
        }
    });
    tx
}

/// 解析 `wm size` 输出的屏幕宽高（优先取最后一行的 Override size）
pub(crate) fn parse_wm_size(out: &str) -> Option<(u32, u32)> {
    let mut found = None;
    for line in out.lines() {
        let s = line.rsplit(':').next().unwrap_or(line).trim();
        if let Some((a, b)) = s.split_once('x') {
            if let (Ok(w), Ok(h)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>()) {
                if w > 0 && h > 0 {
                    found = Some((w, h));
                }
            }
        }
    }
    found
}

/// 一个插件
pub(crate) struct Plugin {
    pub id: String,
    pub dir: PathBuf,
    man: Mutex<Arc<Manifest>>,
    err: Mutex<Option<String>>,
    state: Mutex<PluginState>,
    enabled: Mutex<bool>,
    granted: Mutex<Vec<String>>,
    values: Mutex<Map<String, Value>>,
    pub contributions: Mutex<Contribs>,
    console: Mutex<VecDeque<Value>>,
    host: Mutex<Option<Arc<host::Host>>>,
    spawned: AtomicBool,
    /// 轻量控制/查询并发上限（64 路）
    sem: Semaphore,
    /// 重型 I/O 操作并发上限（4 路：install / uninstall / push / pull / ui / screenshot）
    heavy_sem: Semaphore,
    rate: Mutex<Rate>,
    /// 触控 move 帧的独立限流
    move_rate: Mutex<Rate>,
    /// 投屏输入事件的有序派发队列（首次需要时创建）
    input_q: Mutex<Option<mpsc::Sender<InputJob>>>,
    crashes: Mutex<Vec<Instant>>,
    pub forwards: Mutex<Vec<Forward>>,
}

impl Plugin {
    fn new(man: Arc<Manifest>, dir: PathBuf, saved: StateEntry) -> Self {
        Self {
            id: man.id.clone(),
            dir,
            man: Mutex::new(man),
            err: Mutex::new(None),
            state: Mutex::new(PluginState::Stopped),
            enabled: Mutex::new(saved.enabled),
            granted: Mutex::new(saved.granted),
            values: Mutex::new(saved.settings),
            contributions: Mutex::new(Contribs::default()),
            console: Mutex::new(VecDeque::new()),
            host: Mutex::new(None),
            spawned: AtomicBool::new(false),
            sem: Semaphore::new(64),
            heavy_sem: Semaphore::new(4),
            rate: Mutex::new(Rate::new(500.0)),
            move_rate: Mutex::new(Rate::new(MOVE_PER_SEC)),
            input_q: Mutex::new(None),
            crashes: Mutex::new(Vec::new()),
            forwards: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn manifest(&self) -> Arc<Manifest> {
        self.man.lock().unwrap().clone()
    }

    pub(crate) fn set_manifest(&self, m: Arc<Manifest>) {
        *self.man.lock().unwrap() = m;
    }

    pub(crate) fn state(&self) -> PluginState {
        *self.state.lock().unwrap()
    }

    pub(crate) fn error(&self) -> Option<String> {
        self.err.lock().unwrap().clone()
    }

    pub(crate) fn enabled(&self) -> bool {
        *self.enabled.lock().unwrap()
    }

    pub(crate) fn granted(&self) -> Vec<String> {
        self.granted.lock().unwrap().clone()
    }

    pub(crate) fn has_perm(&self, need: &str) -> bool {
        manifest::has_perm(&self.granted.lock().unwrap(), need)
    }

    /// 当前设置值（清单里的 default 与保存的值合并）
    pub(crate) fn settings(&self) -> Map<String, Value> {
        settings_values(&self.manifest(), &self.values.lock().unwrap())
    }

    pub(crate) fn host(&self) -> Option<Arc<host::Host>> {
        self.host.lock().unwrap().clone()
    }

    pub(crate) fn set_host(&self, h: Option<Arc<host::Host>>) {
        *self.host.lock().unwrap() = h;
    }

    pub(crate) fn is_subscribed(&self, event: &str) -> bool {
        self.contributions.lock().unwrap().events.contains(event)
    }

    /// 取一个触控 move 帧的令牌；超出上限时返回 false（该帧应丢弃）
    pub(crate) fn take_move(&self) -> bool {
        self.move_rate.lock().unwrap().take().is_zero()
    }

    /// 取一次调用的令牌（每秒最多 500 次，超出时排队等待）
    pub(crate) async fn take_rate(&self) {
        loop {
            let d = self.rate.lock().unwrap().take();
            if d.is_zero() {
                return;
            }
            tokio::time::sleep(d).await;
        }
    }

}

/// `.state.json` 里一个插件的条目
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct StateEntry {
    enabled: bool,
    granted: Vec<String>,
    settings: Map<String, Value>,
}

/// 前端 WS 广播消息（client 为 Some 时只发给该界面）
#[derive(Clone)]
pub struct WsMsg {
    pub client: Option<String>,
    pub text: String,
}

struct PendingDialog {
    plugin: String,
    #[allow(dead_code)]
    client: Option<String>,
    tx: oneshot::Sender<Value>,
}

pub(crate) struct WatchHandle {
    pub plugin: String,
    pub stop: tokio::sync::watch::Sender<bool>,
}

/// 主进程事件（由其他模块触发）
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// app.installed / app.uninstalled
    App { name: &'static str, serial: String, package: String },
    /// media.saved
    Media { name: String },
}

pub struct Plugins {
    /// `<data_dir>/plugins`
    root: PathBuf,
    list: Mutex<Vec<Arc<Plugin>>>,
    ws: broadcast::Sender<WsMsg>,
    events: mpsc::UnboundedSender<AppEvent>,
    events_rx: Mutex<Option<mpsc::UnboundedReceiver<AppEvent>>>,
    dialogs: Mutex<HashMap<String, PendingDialog>>,
    watches: Mutex<HashMap<u64, WatchHandle>>,
    /// 每台设备同时最多 2 个插件操作
    dev_sems: Mutex<HashMap<String, Arc<Semaphore>>>,
    /// 设备包名缓存（match.apps / hasApp）
    pkgs: Mutex<HashMap<String, (Instant, Vec<String>)>>,
    /// 最近一次合并的设备列表（本机 + 远程，供 match 与事件用）
    devices: Mutex<Vec<PluginDev>>,
    /// 远程电脑上的设备（缓存 3 秒）
    remote_devs: Mutex<Option<(Instant, Vec<PluginDev>)>>,
    /// 设备 → 上次看到的前台应用
    foreground: Mutex<HashMap<String, String>>,
    /// 本机设备物理分辨率缓存（serial → (w, h)，供未开投屏时坐标缩放）
    screens: Mutex<HashMap<String, (u32, u32)>>,
    /// 投屏人工输入合成器（device.input / device.action）
    input_tracker: Mutex<InputTracker>,
    /// 串行化写入并轮转 `.audit.log` 的异步锁
    audit_lock: Arc<tokio::sync::Mutex<()>>,
    next_sub: AtomicU64,
    next_dialog: AtomicU64,
}

impl Plugins {
    pub fn new(data_dir: &Path) -> Self {
        let (ws, _) = broadcast::channel(512);
        let (events, events_rx) = mpsc::unbounded_channel();
        Self {
            root: data_dir.join("plugins"),
            list: Mutex::new(Vec::new()),
            ws,
            events,
            events_rx: Mutex::new(Some(events_rx)),
            dialogs: Mutex::new(HashMap::new()),
            watches: Mutex::new(HashMap::new()),
            dev_sems: Mutex::new(HashMap::new()),
            pkgs: Mutex::new(HashMap::new()),
            devices: Mutex::new(Vec::new()),
            remote_devs: Mutex::new(None),
            foreground: Mutex::new(HashMap::new()),
            screens: Mutex::new(HashMap::new()),
            input_tracker: Mutex::new(InputTracker::default()),
            audit_lock: Arc::new(tokio::sync::Mutex::new(())),
            next_sub: AtomicU64::new(1),
            next_dialog: AtomicU64::new(1),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn plugins(&self) -> Vec<Arc<Plugin>> {
        self.list.lock().unwrap().clone()
    }

    pub(crate) fn get(&self, id: &str) -> Option<Arc<Plugin>> {
        self.list.lock().unwrap().iter().find(|p| p.id == id).cloned()
    }

    fn get_by_dir(&self, dir: &Path) -> Option<Arc<Plugin>> {
        self.list.lock().unwrap().iter().find(|p| p.dir == dir).cloned()
    }

    // ------------------------------------------------------------ 广播

    pub(crate) fn emit(&self, client: Option<&str>, v: Value) {
        let _ = self.ws.send(WsMsg { client: client.map(String::from), text: v.to_string() });
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WsMsg> {
        self.ws.subscribe()
    }

    pub(crate) fn emit_changed(&self) {
        self.emit(None, json!({"type": "changed"}));
    }

    pub(crate) fn toast(&self, plugin: &str, text: String, kind: &str) {
        self.emit(None, json!({"type": "toast", "plugin": plugin, "text": text, "kind": kind}));
    }

    pub(crate) fn set_state(&self, ph: &Plugin, state: PluginState, error: Option<String>) {
        *ph.state.lock().unwrap() = state;
        *ph.err.lock().unwrap() = error.clone();
        self.emit(None, json!({"type": "state", "plugin": ph.id, "state": state.as_str(), "error": error}));
        self.emit_changed();
    }

    /// 追加一条控制台日志（保留最近 1000 条）
    pub(crate) fn console(&self, ph: &Plugin, level: &str, msg: &str, src: Option<&str>) {
        let entry = json!({"time": now_ms(), "level": level, "msg": msg, "src": src.unwrap_or("")});
        {
            let mut c = ph.console.lock().unwrap();
            if c.len() >= 1000 {
                c.pop_front();
            }
            c.push_back(entry.clone());
        }
        self.emit(None, json!({"type": "console", "plugin": ph.id, "entry": entry}));
    }

    pub(crate) fn console_list(&self, ph: &Plugin) -> Vec<Value> {
        ph.console.lock().unwrap().iter().cloned().collect()
    }

    /// 审计日志（会改变设备或运行程序的操作；串行化追加并在超过 5 MiB 时自动轮转为 `.audit.log.1`）
    pub(crate) fn audit(&self, plugin: &str, op: &str, detail: Value) {
        const AUDIT_MAX_BYTES: u64 = 5 * 1024 * 1024;
        let path = self.root.join(".audit.log");
        let lock = self.audit_lock.clone();
        let line = json!({
            "time": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            "plugin": plugin,
            "op": op,
            "detail": detail,
        })
        .to_string()
            + "\n";
        tokio::spawn(async move {
            append_audit_bounded(&lock, &path, &line, AUDIT_MAX_BYTES).await;
        });
    }

    // ------------------------------------------------------------ 设备

    /// 最近一次合并的设备列表（本机 + 远程）
    pub(crate) fn devices_snapshot(&self) -> Vec<PluginDev> {
        self.devices.lock().unwrap().clone()
    }

    /// 设备 id → (型号, 原始 serial)（用于 match / filter）
    fn device_info(&self, id: &str) -> Option<(String, String)> {
        self.devices.lock().unwrap().iter().find(|d| d.id == id).map(|d| (d.model.clone(), d.serial.clone()))
    }

    /// 本机设备 + 每个远程电脑的 `/api/devices`（远程缓存 3 秒，单台 3 秒超时，失败跳过）
    pub(crate) async fn all_devices(&self, st: &AppState) -> Vec<PluginDev> {
        let mut out: Vec<PluginDev> = match adb::list_devices(&st.cfg.adb).await {
            Ok(list) => list
                .into_iter()
                .map(|d| {
                    let name = if d.model.is_empty() { d.serial.clone() } else { d.model.clone() };
                    PluginDev { id: d.serial.clone(), serial: d.serial, peer: None, computer: st.cfg.name.clone(), model: d.model, name, state: d.state }
                })
                .collect(),
            Err(e) => {
                // adb 暂时不可用：沿用上一次的本机设备，避免误报设备断开
                tracing::debug!("plugins: cannot list local devices: {e}");
                self.devices_snapshot().into_iter().filter(|d| d.peer.is_none()).collect()
            }
        };
        out.extend(self.remote_devices(st).await);
        out
    }

    /// 刷新并保存设备快照
    pub(crate) async fn refresh_devices(&self, st: &AppState) -> Vec<PluginDev> {
        let list = self.all_devices(st).await;
        *self.devices.lock().unwrap() = list.clone();
        list
    }

    async fn remote_devices(&self, st: &AppState) -> Vec<PluginDev> {
        {
            let c = self.remote_devs.lock().unwrap();
            if let Some((t, v)) = c.as_ref() {
                if t.elapsed() < Duration::from_secs(3) {
                    return v.clone();
                }
            }
        }
        let mut tasks = Vec::new();
        for p in st.peers.list().await {
            let Some(peer) = st.peers.get(&p.id).await else { continue };
            tasks.push(tokio::spawn(fetch_peer_devices(peer, p.id.clone(), p.name.clone())));
        }
        let mut out = Vec::new();
        for t in tasks {
            if let Ok(v) = t.await {
                out.extend(v);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        *self.remote_devs.lock().unwrap() = Some((Instant::now(), out.clone()));
        out
    }

    /// 设备是否在插件的 match 范围内（`serial` 为 SPEC 7.1 的标识；match 用原始 serial 与 model）
    pub(crate) async fn device_allowed(&self, st: &AppState, ph: &Plugin, serial: &str) -> bool {
        let man = ph.manifest();
        let (model, raw) = match self.device_info(serial) {
            Some(v) => v,
            None => match self.refresh_devices(st).await.into_iter().find(|d| d.id == serial) {
                Some(d) => (d.model, d.serial),
                None => (String::new(), raw_serial(serial).to_string()),
            },
        };
        if man.match_rules.apps.is_empty() {
            return manifest::device_matches(&man.match_rules, &model, &raw, false);
        }
        let packages = self.packages(st, serial).await;
        let has_app = man.match_rules.apps.iter().any(|g| packages.iter().any(|p| manifest::glob_match(g, p)));
        manifest::device_matches(&man.match_rules, &model, &raw, has_app)
    }

    /// 设备上是否装了某个包（`packages` 缓存的结果，glob 也可以）
    pub(crate) async fn has_package(&self, st: &AppState, serial: &str, pkg: &str) -> bool {
        self.packages(st, serial).await.iter().any(|p| p == pkg || manifest::glob_match(pkg, p))
    }

    /// 设备上的包名列表（缓存 30 秒；设备离线时为空）。远程设备经 devop 在对方电脑上取。
    pub(crate) async fn packages(&self, st: &AppState, serial: &str) -> Vec<String> {
        {
            let c = self.pkgs.lock().unwrap();
            if let Some((t, v)) = c.get(serial) {
                if t.elapsed() < Duration::from_secs(30) {
                    return v.clone();
                }
            }
        }
        let out = match split_dev(serial) {
            Some((peer_id, raw)) => match st.peers.get(peer_id).await {
                Some(peer) => {
                    let p = json!({"cmd": "pm list packages -3", "timeout": 60});
                    match devop::json_call(&peer, raw, "shell", &p, &[], Duration::from_secs(90)).await {
                        Ok(v) => v.get("out").and_then(|o| o.as_str()).unwrap_or("").to_string(),
                        Err(e) => {
                            tracing::debug!("plugins: cannot list packages of {serial}: {e}");
                            String::new()
                        }
                    }
                }
                None => String::new(),
            },
            None => adb::run(&st.cfg.adb, &["-s", serial, "shell", "pm list packages"]).await.unwrap_or_default(),
        };
        let v: Vec<String> = out.lines().filter_map(|l| l.trim().strip_prefix("package:")).map(String::from).collect();
        self.pkgs.lock().unwrap().insert(serial.to_string(), (Instant::now(), v.clone()));
        v
    }

    pub(crate) fn device_sem(&self, serial: &str) -> Arc<Semaphore> {
        self.dev_sems.lock().unwrap().entry(serial.to_string()).or_insert_with(|| Arc::new(Semaphore::new(4))).clone()
    }

    /// 获取本机设备的屏幕分辨率（优先走缓存，未命中时查一次 `wm size`）
    pub(crate) async fn screen_size(&self, st: &AppState, serial: &str) -> Option<(u32, u32)> {
        if let Some(sz) = self.screens.lock().unwrap().get(serial).copied() {
            return Some(sz);
        }
        let out = adb::run_timeout(&st.cfg.adb, &["-s", serial, "shell", "wm size"], Duration::from_secs(5)).await.ok()?;
        let sz = parse_wm_size(&out)?;
        self.screens.lock().unwrap().insert(serial.to_string(), sz);
        Some(sz)
    }

    /// 用户在投屏窗口操作某台设备（含本机与远程投屏）：向订阅了 `device.input` / `device.action` 且拥有 `device.observe` 权限的插件派发事件
    pub(crate) fn on_user_ctl(&self, st: &Arc<AppState>, dev_id: &str, ctl: &control::Ctl, session: Option<(u32, u32)>) {
        let subs: Vec<Arc<Plugin>> = self
            .plugins()
            .into_iter()
            .filter(|ph| {
                ph.state() == PluginState::Running
                    && ph.has_perm("device.observe")
                    && (ph.is_subscribed("device.input") || ph.is_subscribed("device.action"))
            })
            .collect();
        if subs.is_empty() {
            return;
        }
        let (raw_ev, act_ev) = self.input_tracker.lock().unwrap().ingest(dev_id, ctl, session, Instant::now());
        if raw_ev.is_none() && act_ev.is_none() {
            return;
        }
        let dev_json = self
            .devices_snapshot()
            .into_iter()
            .find(|d| d.id == dev_id)
            .map(|d| d.json())
            .unwrap_or_else(|| {
                let raw = raw_serial(dev_id);
                json!({
                    "serial": dev_id,
                    "model": "",
                    "name": raw,
                    "state": "device",
                    "computer": if split_dev(dev_id).is_some() { "" } else { st.cfg.name.as_str() },
                    "remote": split_dev(dev_id).is_some(),
                })
            });
        for ph in subs {
            let job = InputJob { dev_id: dev_id.to_string(), dev_json: dev_json.clone(), raw: raw_ev.clone(), act: act_ev.clone() };
            let mut q = ph.input_q.lock().unwrap();
            let tx = q.get_or_insert_with(|| input_worker(st.clone(), &ph));
            // 队列满说明插件处理不过来，丢弃这一帧
            let _ = tx.try_send(job);
        }
    }

    // ------------------------------------------------------------ 接口数据

    /// `GET /api/plugins` 的返回内容
    pub(crate) fn list_json(&self, manage: bool, safe_mode: bool) -> Value {
        let list: Vec<Value> = self
            .plugins()
            .iter()
            .map(|ph| {
                let man = ph.manifest();
                let mut v = json!({
                    "id": ph.id,
                    "name": man.name,
                    "version": man.version,
                    "description": man.description,
                    "author": man.author,
                    "dir": ph.dir.to_string_lossy(),
                    "kind": if std::fs::symlink_metadata(&ph.dir).map(|m| m.file_type().is_symlink()).unwrap_or(false) { "link" }
                        else if sources::is_linked(&self.root, &ph.dir) { "outside" }
                        else if ph.dir.join(".source.json").exists() { "source" } else { "local" },
                    "state": ph.state().as_str(),
                    "enabled": ph.enabled(),
                    "permissions": man.permissions,
                    "granted": ph.granted(),
                    "settings": man.settings,
                    "values": ph.settings(),
                });
                if let Some(e) = ph.error() {
                    v["error"] = json!(e);
                }
                v
            })
            .collect();
        json!({
            "manage": manage,
            "dir": self.root.to_string_lossy(),
            "safeMode": safe_mode,
            "list": list,
        })
    }

    /// `GET /api/plugins/contrib` 的返回内容（给了 serial 时按插件 match 与贡献项 filter 过滤）
    pub(crate) async fn contrib_json(&self, st: &Arc<AppState>, serial: Option<&str>) -> Value {
        let (mut menus, mut panels, mut status, mut metrics, mut loghl) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut badges: Map<String, Value> = Map::new();
        let mut overlays: Map<String, Value> = Map::new();
        for ph in self.plugins() {
            if ph.state() != PluginState::Running {
                continue;
            }
            if let Some(s) = serial {
                if !self.device_allowed(st, &ph, s).await {
                    continue;
                }
            }
            // 先复制出来，避免在 await 期间持有锁
            let (m_list, p_list, s_list, t_list, l_list, b_list, o_list) = {
                let c = ph.contributions.lock().unwrap();
                (c.menus.clone(), c.panels.clone(), c.status.clone(), c.metrics.clone(), c.loghl.clone(), c.badges.clone(), c.overlays.clone())
            };
            for d in &m_list {
                if let Some(s) = serial {
                    if !self.filter_ok(st, s, d.get("filter")).await {
                        continue;
                    }
                }
                menus.push(with_plugin(d, &ph.id));
            }
            for (is_web, entry, d) in &p_list {
                if let Some(s) = serial {
                    if !self.filter_ok(st, s, d.get("filter")).await {
                        continue;
                    }
                }
                let mut v = with_plugin(d, &ph.id);
                if *is_web && d.get("url").and_then(|x| x.as_bool()) == Some(true) {
                    // 外部网页：前端调插件的 __url:<id> 取地址，只允许这些网站
                    let man = ph.manifest();
                    v["embed"] = json!(man.embed.iter().filter_map(|e| manifest::origin_of(e)).collect::<Vec<_>>());
                } else if *is_web {
                    v["web"] = json!(format!("/plugin-web/{}/{}", ph.id, entry));
                }
                panels.push(v);
            }
            for d in &t_list {
                if let Some(s) = serial {
                    if !self.filter_ok(st, s, d.get("filter")).await {
                        continue;
                    }
                }
                metrics.push(with_plugin(d, &ph.id));
            }
            for d in &s_list {
                status.push(with_plugin(d, &ph.id));
            }
            for d in &l_list {
                loghl.push(with_plugin(d, &ph.id));
            }
            for (serial, b) in b_list {
                let mut v = b.clone();
                v["plugin"] = json!(ph.id);
                if let Some(a) = badges.entry(serial).or_insert_with(|| json!([])).as_array_mut() {
                    a.push(v);
                }
            }
            for (serial, ov) in o_list {
                if let Some(a) = overlays.entry(serial).or_insert_with(|| json!([])).as_array_mut() {
                    a.push(json!({"plugin": ph.id, "shapes": ov["shapes"], "size": ov["size"]}));
                }
            }
        }
        json!({
            "menus": menus,
            "panels": panels,
            "status": status,
            "metrics": metrics,
            "loghl": loghl,
            "badges": badges,
            "overlays": overlays,
        })
    }

    /// 贡献项的 filter 是否匹配这台设备（hasApp / models / serials 由主进程判断）
    pub(crate) async fn filter_ok(&self, st: &AppState, serial: &str, filter: Option<&Value>) -> bool {
        let Some(f) = filter else { return true };
        let raw = raw_serial(serial);
        let model = match self.device_info(serial) {
            Some((m, _)) => m,
            None => self.refresh_devices(st).await.iter().find(|d| d.id == serial).map(|d| d.model.clone()).unwrap_or_default(),
        };
        let pats = |key: &str| -> Vec<String> { f.get(key).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).map(String::from).collect()).unwrap_or_default() };
        let models = pats("models");
        if !models.is_empty() && !manifest::any_glob(&models, &model) {
            return false;
        }
        let serials = pats("serials");
        if !serials.is_empty() && !manifest::any_glob(&serials, raw) {
            return false;
        }
        if let Some(app) = f.get("hasApp").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            if !self.has_package(st, serial, app).await {
                return false;
            }
        }
        true
    }

    // ------------------------------------------------------------ 状态文件

    fn state_path(&self) -> PathBuf {
        self.root.join(".state.json")
    }

    fn read_state(&self) -> HashMap<String, StateEntry> {
        let raw = std::fs::read(self.state_path()).unwrap_or_default();
        if raw.is_empty() {
            return HashMap::new();
        }
        serde_json::from_slice(&raw).unwrap_or_default()
    }

    /// 把启用状态、已授予的权限与设置值写回 `.state.json`
    pub(crate) fn save_state(&self) {
        let mut map = HashMap::new();
        for ph in self.plugins() {
            map.insert(
                ph.id.clone(),
                StateEntry { enabled: ph.enabled(), granted: ph.granted(), settings: ph.values.lock().unwrap().clone() },
            );
        }
        let data = match serde_json::to_vec_pretty(&map) {
            Ok(d) => d,
            Err(_) => return,
        };
        if let Some(dir) = self.state_path().parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(self.state_path(), data) {
            tracing::error!("cannot write plugin state: {e}");
        }
    }

    // ------------------------------------------------------------ 扫描

    /// 扫描插件目录，替换整个列表（不启动任何进程）
    pub(crate) fn load(&self) {
        let _ = std::fs::create_dir_all(self.root.join(".data"));
        let _ = std::fs::create_dir_all(self.root.join(".tmp"));
        let saved = self.read_state();
        let mut list: Vec<Arc<Plugin>> = Vec::new();
        let mut ids: HashSet<String> = HashSet::new();
        for (dir, res) in scan(&self.root) {
            let dirname = dir.file_name().and_then(|n| n.to_str()).unwrap_or("plugin").to_string();
            let (man, error) = match res {
                Ok(m) => (Arc::new(m), None),
                Err(e) => (
                    Arc::new(Manifest { id: dirname.clone(), name: dirname.clone(), ..Default::default() }),
                    Some(e),
                ),
            };
            let mut error = error;
            if ids.contains(&man.id) {
                let id = man.id.clone();
                error = Some(tr!("插件 id 重复：{id}", "Duplicate plugin id: {id}"));
            }
            ids.insert(man.id.clone());
            let entry = saved.get(&man.id).cloned().unwrap_or_default();
            let ph = Arc::new(Plugin::new(man.clone(), dir, entry));
            let state = initial_state(&man, &ph, error.as_deref());
            *ph.state.lock().unwrap() = state;
            *ph.err.lock().unwrap() = error;
            list.push(ph);
        }
        list.sort_by(|a, b| a.id.cmp(&b.id));
        *self.list.lock().unwrap() = list;
    }

    /// 重新扫描：已有插件只更新清单（进程不动），新增的加入列表，消失的停止并移除
    pub(crate) fn rescan(&self, st: &Arc<AppState>) {
        let saved = self.read_state();
        let found = scan(&self.root);
        let current = self.plugins();
        let mut list: Vec<Arc<Plugin>> = Vec::new();
        let mut keep: HashSet<String> = HashSet::new();
        for (dir, res) in found {
            let dirname = dir.file_name().and_then(|n| n.to_str()).unwrap_or("plugin").to_string();
            let (man, error) = match res {
                Ok(m) => (Arc::new(m), None),
                Err(e) => (Arc::new(Manifest { id: dirname.clone(), name: dirname.clone(), ..Default::default() }), Some(e)),
            };
            if let Some(ph) = current.iter().find(|p| p.dir == dir) {
                keep.insert(ph.id.clone());
                if let Some(err) = error {
                    ph.set_manifest(man.clone());
                    stop_host(ph);
                    self.set_state(ph, PluginState::Invalid, Some(err));
                } else if !same_manifest(&ph.manifest(), &man) {
                    ph.set_manifest(man.clone());
                    if manifest::needs_grant(&man.permissions, &ph.granted()) {
                        // 清单新增了权限：停止并等待用户重新确认
                        stop_host(ph);
                        self.set_state(ph, PluginState::NeedsGrant, None);
                    } else if matches!(ph.state(), PluginState::Invalid | PluginState::NeedsGrant) {
                        let state = initial_state(&man, ph, None);
                        self.set_state(ph, state, None);
                        self.start(st, ph);
                    }
                }
                list.push(ph.clone());
                continue;
            }
            let entry = saved.get(&man.id).cloned().unwrap_or_default();
            let ph = Arc::new(Plugin::new(man.clone(), dir, entry));
            let state = initial_state(&man, &ph, error.as_deref());
            *ph.state.lock().unwrap() = state;
            *ph.err.lock().unwrap() = error;
            self.start(st, &ph);
            list.push(ph);
        }
        for ph in current.iter().filter(|p| !keep.contains(&p.id)) {
            stop_host(ph);
        }
        list.sort_by(|a, b| a.id.cmp(&b.id));
        *self.list.lock().unwrap() = list;
    }

    // ------------------------------------------------------------ 生命周期

    /// 启动插件宿主（安全模式下不启动；已在运行的跳过）
    pub(crate) fn start(&self, st: &Arc<AppState>, ph: &Arc<Plugin>) {
        if st.cfg.no_plugins {
            return;
        }
        if !ph.enabled() {
            return;
        }
        if ph.state() == PluginState::Invalid || ph.state() == PluginState::NeedsGrant {
            return;
        }
        if ph.spawned.swap(true, Ordering::SeqCst) {
            return;
        }
        let (st, ph) = (st.clone(), ph.clone());
        tokio::spawn(async move {
            host::supervise(st, ph).await;
        });
    }

    /// 启用：同时授予当前清单里的权限（用户已确认）
    pub(crate) fn enable(&self, st: &Arc<AppState>, ph: &Arc<Plugin>) -> Result<(), String> {
        if ph.state() == PluginState::Invalid {
            return Err(tr!("插件清单有错误，无法启用", "The plugin manifest has errors and cannot be enabled"));
        }
        if st.cfg.no_plugins {
            return Err(tr!("安全模式（--no-plugins）下不能启用插件", "Plugins cannot be enabled in safe mode (--no-plugins)"));
        }
        let man = ph.manifest();
        *ph.enabled.lock().unwrap() = true;
        *ph.granted.lock().unwrap() = man.permissions.clone();
        self.save_state();
        // 已在运行的插件不重置状态
        if ph.host().is_none() {
            self.set_state(ph, PluginState::Stopped, None);
        }
        self.start(st, ph);
        Ok(())
    }

    pub(crate) fn disable(&self, ph: &Arc<Plugin>) -> Result<(), String> {
        *ph.enabled.lock().unwrap() = false;
        self.save_state();
        stop_host(ph);
        self.set_state(ph, PluginState::Disabled, None);
        Ok(())
    }

    /// 重新加载（立即重启宿主）
    pub(crate) fn reload(&self, st: &Arc<AppState>, ph: &Arc<Plugin>) {
        if let Some(h) = ph.host() {
            h.control(host::HostCmd::Reload);
            return;
        }
        if ph.enabled() && !st.cfg.no_plugins && matches!(ph.state(), PluginState::Failed | PluginState::Crashed | PluginState::Stopped) {
            *ph.crashes.lock().unwrap() = Vec::new();
            self.start(st, ph);
        }
    }

    // ------------------------------------------------------------ 对话框

    /// 打开一个对话框并等待回答（插件停止时返回 null）
    pub(crate) async fn ask(&self, ph: &Plugin, p: &Value, client: Option<&str>) -> Value {
        let id = format!("d{:x}", self.next_dialog.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.dialogs.lock().unwrap().insert(id.clone(), PendingDialog { plugin: ph.id.clone(), client: client.map(String::from), tx });
        let mut msg = p.clone();
        msg["id"] = json!(id);
        msg["plugin"] = json!(ph.id);
        msg["type"] = json!("dialog");
        self.emit(client, msg);
        match rx.await {
            Ok(v) => v,
            Err(_) => Value::Null,
        }
    }

    /// 前端回答对话框（第一个回答生效）
    pub(crate) fn answer(&self, id: &str, value: Value) -> bool {
        let Some(d) = self.dialogs.lock().unwrap().remove(id) else { return false };
        let _ = d.tx.send(value);
        self.emit(None, json!({"type": "dialog.close", "id": id}));
        true
    }

    /// 插件停止时取消它所有未回答的对话框（前端按“取消”处理）
    pub(crate) fn cancel_dialogs(&self, plugin: &str) {
        let ids: Vec<String> = {
            let mut d = self.dialogs.lock().unwrap();
            let ids: Vec<String> = d.iter().filter(|(_, v)| v.plugin == plugin).map(|(k, _)| k.clone()).collect();
            let mut answered = Vec::new();
            for id in &ids {
                if let Some(p) = d.remove(id) {
                    let _ = p.tx.send(Value::Null);
                    answered.push(id.clone());
                }
            }
            answered
        };
        for id in ids {
            self.emit(None, json!({"type": "dialog.close", "id": id}));
        }
    }

    // ------------------------------------------------------------ logwatch 子任务登记

    pub(crate) fn next_sub(&self) -> u64 {
        self.next_sub.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn add_watch(&self, id: u64, plugin: &str) -> tokio::sync::watch::Receiver<bool> {
        let (stop, rx) = tokio::sync::watch::channel(false);
        self.watches.lock().unwrap().insert(id, WatchHandle { plugin: plugin.to_string(), stop });
        rx
    }

    pub(crate) fn remove_watch(&self, id: u64) {
        self.watches.lock().unwrap().remove(&id);
    }
}

/// 复制一份贡献项定义，并加上所属插件
fn with_plugin(def: &Value, plugin: &str) -> Value {
    let mut v = def.clone();
    if let Some(o) = v.as_object_mut() {
        o.insert("plugin".into(), json!(plugin));
    }
    v
}

/// 判断两个清单是否有变化（重新序列化比较，忽略键顺序）
fn same_manifest(a: &Manifest, b: &Manifest) -> bool {
    serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
}

/// 合并清单默认值与保存的设置值
fn settings_values(man: &Manifest, values: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for s in &man.settings {
        out.insert(s.name.clone(), values.get(&s.name).cloned().unwrap_or_else(|| s.default.clone()));
    }
    out
}

fn initial_state(man: &Manifest, ph: &Plugin, error: Option<&str>) -> PluginState {
    if error.is_some() {
        return PluginState::Invalid;
    }
    if !ph.enabled() {
        return PluginState::Disabled;
    }
    if manifest::needs_grant(&man.permissions, &ph.granted()) {
        return PluginState::NeedsGrant;
    }
    PluginState::Stopped
}

/// 插件根目录下的插件目录（跳过 `.` 开头的目录），返回 (目录, 清单结果)
fn scan(root: &Path) -> Vec<(PathBuf, Result<Manifest, String>)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else { return out };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy().to_string();
        if name.starts_with('.') || !entry.path().is_dir() {
            continue;
        }
        let dir = entry.path();
        out.push((dir.clone(), manifest::load(&dir)));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 通知宿主停止（不等待）
fn stop_host(ph: &Plugin) {
    if let Some(h) = ph.host() {
        h.control(host::HostCmd::Stop);
    }
}

// ---------------------------------------------------------------- 主循环

/// 启动管理器：扫描插件、启动已启用的、每秒检查热重载、轮询设备与前台应用事件
pub async fn run(st: Arc<AppState>) {
    let plugs = &st.plugins;
    let _ = tokio::fs::create_dir_all(plugs.root.join(".data")).await;
    let _ = tokio::fs::create_dir_all(plugs.root.join(".tmp")).await;
    plugs.load();
    for ph in plugs.plugins() {
        plugs.start(&st, &ph);
    }
    let Some(mut events_rx) = plugs.events_rx.lock().unwrap().take() else { return };

    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut fingerprint = dir_fingerprints(&plugs.root);
    let mut t_fp = Instant::now();
    let mut t_dev = Instant::now() - Duration::from_secs(60);
    let mut t_fg = Instant::now() - Duration::from_secs(60);
    let mut t_tmp = Instant::now();

    loop {
        tokio::select! {
            ev = events_rx.recv() => {
                if let Some(ev) = ev {
                    on_event(&st, ev).await;
                }
            }
            _ = tick.tick() => {
                if t_fp.elapsed() >= Duration::from_secs(1) {
                    t_fp = Instant::now();
                    watch_files(&st, &mut fingerprint).await;
                }
                if t_dev.elapsed() >= Duration::from_secs(2) {
                    t_dev = Instant::now();
                    poll_devices(&st).await;
                }
                if t_fg.elapsed() >= Duration::from_secs(2) {
                    t_fg = Instant::now();
                    poll_foreground(&st).await;
                }
                if t_tmp.elapsed() >= Duration::from_secs(3600) {
                    t_tmp = Instant::now();
                    cleanup_tmp(&plugs.root).await;
                }
            }
        }
    }
}

/// 其他模块（文件管理器、拖放、插件 install）完成安装后调用
pub fn on_installed(st: &AppState, serial: &str, package: &str) {
    let _ = st.plugins.events.send(AppEvent::App { name: "app.installed", serial: serial.to_string(), package: package.to_string() });
}

/// 应用管理或插件卸载应用后调用
pub fn on_uninstalled(st: &AppState, serial: &str, package: &str) {
    let _ = st.plugins.events.send(AppEvent::App { name: "app.uninstalled", serial: serial.to_string(), package: package.to_string() });
}

/// 截图或录屏存入图库后调用
pub fn on_media_saved(st: &AppState, name: &str) {
    let _ = st.plugins.events.send(AppEvent::Media { name: name.to_string() });
}

/// 远程电脑上的设备：`GET /api/devices`（单台 3 秒超时，失败时返回空）
async fn fetch_peer_devices(peer: crate::peers::Peer, peer_id: String, computer: String) -> Vec<PluginDev> {
    let fut = crate::peers::get_json(&peer, "/api/devices");
    let Ok(Ok(v)) = tokio::time::timeout(Duration::from_secs(3), fut).await else {
        tracing::debug!("plugins: cannot list devices of {}: timeout", peer.addr);
        return Vec::new();
    };
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let serial = d.get("serial").and_then(|s| s.as_str())?.to_string();
            let model = d.get("model").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let state = d.get("state").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let name = if model.is_empty() { serial.clone() } else { model.clone() };
            Some(PluginDev { id: format!("{peer_id}/{serial}"), serial, peer: Some(peer_id.clone()), computer: computer.clone(), model, name, state })
        })
        .collect()
}

/// 安装前后对比包名列表，找出新装的包（拿不到时返回空）
pub fn new_packages(before: &str, after: &str) -> Vec<String> {
    let parse = |s: &str| -> HashSet<String> { s.lines().filter_map(|l| l.trim().strip_prefix("package:")).map(String::from).collect() };
    let (b, a) = (parse(before), parse(after));
    let mut v: Vec<String> = a.difference(&b).cloned().collect();
    v.sort();
    v
}

async fn on_event(st: &Arc<AppState>, ev: AppEvent) {
    let name = match &ev {
        AppEvent::App { name, .. } => *name,
        AppEvent::Media { .. } => "media.saved",
    };
    let subs: Vec<Arc<Plugin>> = st.plugins.plugins().into_iter().filter(|p| p.host().is_some() && p.is_subscribed(name)).collect();
    if subs.is_empty() {
        return;
    }
    let args = match ev {
        AppEvent::App { serial, package, .. } => {
            // 事件里的 dev 也带上 computer / remote（SPEC 7.1）
            let dev = st
                .plugins
                .devices_snapshot()
                .into_iter()
                .find(|d| d.id == serial || d.serial == serial)
                .map(|d| d.json())
                .unwrap_or_else(|| json!({"serial": serial}));
            vec![dev, json!(package)]
        }
        AppEvent::Media { name } => {
            let item = calls::media_items(st).await.unwrap_or_default().into_iter().find(|it| it["name"] == name.as_str());
            vec![item.unwrap_or_else(|| json!({"name": name}))]
        }
    };
    fire(st, &subs, name, args).await;
}

/// 缓存的设备目录指纹：目录 → (文件路径, 大小, mtime) 的哈希
fn dir_fingerprints(root: &Path) -> HashMap<PathBuf, u64> {
    use std::hash::{Hash, Hasher};
    let mut out = HashMap::new();
    let Ok(rd) = std::fs::read_dir(root) else { return out };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || !entry.path().is_dir() {
            continue;
        }
        let dir = entry.path();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        walk(&dir, &mut |path, size, mtime| {
            path.hash(&mut h);
            size.hash(&mut h);
            mtime.hash(&mut h);
        });
        out.insert(dir, h.finish());
    }
    out
}

/// 递归遍历插件目录（跳过 `.` 开头的子目录），最多 4 层
fn walk(dir: &Path, f: &mut impl FnMut(&str, u64, u64)) {
    fn inner(dir: &Path, depth: usize, f: &mut impl FnMut(&str, u64, u64)) {
        if depth > 4 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                inner(&path, depth + 1, f);
            } else {
                let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
                f(&path.to_string_lossy(), meta.len(), mtime);
            }
        }
    }
    inner(dir, 0, f);
}

/// 每秒检查文件变化：清单变了重新加载，文件变了重启插件并提示
async fn watch_files(st: &Arc<AppState>, fingerprint: &mut HashMap<PathBuf, u64>) {
    let now = dir_fingerprints(st.plugins.root());
    if now == *fingerprint {
        return;
    }
    let changed: Vec<PathBuf> = now
        .iter()
        .filter(|(dir, h)| fingerprint.get(*dir) != Some(*h))
        .map(|(dir, _)| dir.clone())
        .collect();
    let added = now.keys().any(|d| !fingerprint.contains_key(d));
    let removed = fingerprint.keys().any(|d| !now.contains_key(d));
    *fingerprint = now;
    if added || removed || !changed.is_empty() {
        // 重新读取清单：已有插件保留实例（正在运行的进程不受影响）
        st.plugins.rescan(st);
    }
    for dir in changed {
        let Some(ph) = st.plugins.get_by_dir(&dir) else { continue };
        if matches!(ph.state(), PluginState::Invalid | PluginState::NeedsGrant) {
            continue;
        }
        if !ph.enabled() {
            continue;
        }
        if ph.host().is_some() {
            st.plugins.reload(st, &ph);
            let name = ph.manifest().name.clone();
            st.plugins.toast(&ph.id, tr!("{name} 已重新加载", "{name} reloaded"), "info");
        } else {
            st.plugins.start(st, &ph);
        }
    }
    st.plugins.emit_changed();
}

/// 每 2 秒看一次设备列表（本机 + 远程），变化时给订阅的插件发事件
async fn poll_devices(st: &Arc<AppState>) {
    let list = st.plugins.all_devices(st).await;
    let online = |d: &PluginDev| d.state == "device";
    let prev: Vec<String> = st.plugins.devices_snapshot().iter().filter(|d| online(d)).map(|d| d.id.clone()).collect();
    let now: Vec<String> = list.iter().filter(|d| online(d)).map(|d| d.id.clone()).collect();
    *st.plugins.devices.lock().unwrap() = list.clone();
    let subs: Vec<Arc<Plugin>> = st.plugins.plugins().into_iter().filter(|p| p.host().is_some()).collect();
    for d in list.iter().filter(|d| online(d) && !prev.contains(&d.id)) {
        fire(st, &subs, "device.connected", vec![d.json()]).await;
    }
    for id in prev.iter().filter(|id| !now.contains(id)) {
        fire(st, &subs, "device.disconnected", vec![json!({"serial": id})]).await;
        st.plugins.foreground.lock().unwrap().remove(id);
    }
}

/// 一台设备的前台应用（远程设备经 devop 在对方电脑上查）
async fn foreground_of(st: &Arc<AppState>, id: &str) -> String {
    match split_dev(id) {
        Some((peer_id, raw)) => match st.peers.get(peer_id).await {
            Some(peer) => {
                let p = json!({});
                match devop::json_call(&peer, raw, "foreground", &p, &[], Duration::from_secs(20)).await {
                    Ok(Value::String(s)) => s,
                    _ => String::new(),
                }
            }
            None => String::new(),
        },
        None => agent::foreground(st, id).await.split('/').next().unwrap_or("").to_string(),
    }
}

/// 每 2 秒查一次前台应用（只在有插件订阅 app.foreground 时；覆盖远程设备）
async fn poll_foreground(st: &Arc<AppState>) {
    let subs: Vec<Arc<Plugin>> = st.plugins.plugins().into_iter().filter(|p| p.host().is_some() && p.is_subscribed("app.foreground")).collect();
    if subs.is_empty() {
        return;
    }
    let devices: Vec<PluginDev> = st.plugins.devices_snapshot().into_iter().filter(|d| d.state == "device").collect();
    for dev in devices {
        let serial = dev.id.clone();
        let pkg = foreground_of(st, &serial).await;
        let old = st.plugins.foreground.lock().unwrap().get(&serial).cloned().unwrap_or_default();
        if pkg == old {
            continue;
        }
        st.plugins.foreground.lock().unwrap().insert(serial.clone(), pkg.clone());
        if pkg.is_empty() {
            continue;
        }
        fire(st, &subs, "app.foreground", vec![dev.json(), json!(pkg)]).await;
    }
}

/// 给订阅了该事件的插件发 `event` 请求（各自独立，不等待完成）
pub(crate) async fn fire(st: &Arc<AppState>, subs: &[Arc<Plugin>], name: &str, args: Vec<Value>) {
    for ph in subs {
        if !ph.is_subscribed(name) {
            continue;
        }
        let Some(h) = ph.host() else { continue };
        let p = json!({"name": name, "args": args});
        let st2 = st.clone();
        let ph2 = ph.clone();
        let name2 = name.to_string();
        tokio::spawn(async move {
            if let Err(e) = h.request("event", p, Duration::from_secs(10)).await {
                st2.plugins.console(&ph2, "warn", &tr!("事件 {name2} 处理失败：{e}", "Event {name2} failed: {e}"), None);
            }
        });
    }
}

/// 串行化写入审计日志，并在当前文件达到 `max_bytes` 时轮转为 `<path>.1`（最多保留 1 份归档）
pub(crate) async fn append_audit_bounded(lock: &tokio::sync::Mutex<()>, path: &Path, line: &str, max_bytes: u64) {
    use tokio::io::AsyncWriteExt;
    let _guard = lock.lock().await;
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if max_bytes > 0 {
        if let Ok(meta) = tokio::fs::metadata(path).await {
            if meta.len() >= max_bytes {
                let mut bak_os = path.as_os_str().to_os_string();
                bak_os.push(".1");
                let bak = PathBuf::from(bak_os);
                let _ = tokio::fs::remove_file(&bak).await;
                let _ = tokio::fs::rename(path, &bak).await;
            }
        }
    }
    if let Ok(mut f) = tokio::fs::OpenOptions::new().append(true).create(true).open(path).await {
        let _ = f.write_all(line.as_bytes()).await;
    }
}

/// 清掉 `.tmp` 及其一级子目录（`.tmp/<id>`、`.tmp/uploads`）里 24 小时前的上传/截图文件
async fn cleanup_tmp(root: &Path) {
    let dir = root.join(".tmp");
    let Ok(mut rd) = tokio::fs::read_dir(&dir).await else { return };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let Ok(meta) = entry.metadata().await else { continue };
        if meta.is_dir() {
            let Ok(mut sub) = tokio::fs::read_dir(entry.path()).await else { continue };
            while let Ok(Some(child)) = sub.next_entry().await {
                let Ok(cmeta) = child.metadata().await else { continue };
                let Ok(modified) = cmeta.modified() else { continue };
                if modified.elapsed().map(|d| d.as_secs() > 24 * 3600).unwrap_or(false) {
                    let _ = tokio::fs::remove_file(child.path()).await;
                }
            }
            continue;
        }
        let Ok(modified) = meta.modified() else { continue };
        if modified.elapsed().map(|d| d.as_secs() > 24 * 3600).unwrap_or(false) {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

// ---------------------------------------------------------------- MCP

/// 本机所有运行中插件注册的 MCP 工具（名字为 `<插件id>__<名称>`）
pub fn mcp_tools(st: &AppState) -> Vec<Value> {
    let mut out = Vec::new();
    for ph in st.plugins.plugins() {
        if ph.state() != PluginState::Running {
            continue;
        }
        for t in ph.contributions.lock().unwrap().mcps.iter() {
            let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let mut required = t.get("required").cloned().unwrap_or(json!([]));
            if !required.is_array() {
                required = json!([]);
            }
            out.push(json!({
                "name": manifest::tool_name(&ph.id, name),
                "description": t.get("description").cloned().unwrap_or(json!("")),
                "inputSchema": {
                    "type": "object",
                    "properties": t.get("params").cloned().unwrap_or(json!({})),
                    "required": required,
                },
            }));
        }
    }
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out
}

/// 是否为本机插件工具（名字里带 `__`）
pub fn is_plugin_tool(st: &AppState, tool: &str) -> bool {
    st.plugins.plugins().iter().any(|ph| {
        ph.contributions
            .lock()
            .unwrap()
            .mcps
            .iter()
            .any(|t| manifest::tool_name(&ph.id, t.get("name").and_then(|v| v.as_str()).unwrap_or("")) == tool)
    })
}

/// 调用插件的 MCP 工具（SPEC 4 节：120 秒超时，结果转成 MCP text content）
pub async fn mcp_call(st: &AppState, tool: &str, args: &Value) -> Result<Vec<Value>, String> {
    let mut found = None;
    for ph in st.plugins.plugins() {
        for t in ph.contributions.lock().unwrap().mcps.iter() {
            let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if manifest::tool_name(&ph.id, &name) == tool {
                found = Some((ph.clone(), name));
            }
        }
    }
    let Some((ph, name)) = found else {
        return Err(tr!("未知工具：{tool}", "Unknown tool: {tool}"));
    };
    let Some(h) = ph.host() else {
        return Err(tr!("插件 {tool} 没有运行", "The plugin for {tool} is not running"));
    };
    let p = json!({"name": name, "args": args, "serial": args.get("serial").cloned().unwrap_or(Value::Null)});
    let r = h.request("mcp", p, Duration::from_secs(120)).await?;
    Ok(vec![json!({"type": "text", "text": tool_text(&r)})])
}

fn tool_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// 用系统文件管理器打开目录
pub(crate) fn open_dir(dir: &Path) -> Result<(), String> {
    let _ = std::fs::create_dir_all(dir);
    let d = dir.to_string_lossy().to_string();
    #[cfg(windows)]
    let res = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer").arg(&d).creation_flags(0x0800_0000).spawn()
    };
    #[cfg(target_os = "macos")]
    let res = std::process::Command::new("open").arg(&d).spawn();
    #[cfg(not(any(windows, target_os = "macos")))]
    let res = std::process::Command::new("xdg-open").arg(&d).spawn();
    res.map(|_| ()).map_err(|e| tr!("无法打开文件管理器：{e}", "Cannot open the file manager: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("droidbridge-plugins-{name}-{:08x}", crate::util::random_scid()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_plugin(root: &Path, dir: &str, id: &str) {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("plugin.json"), format!(r#"{{"id":"{id}","name":"{id}"}}"#)).unwrap();
        std::fs::write(d.join("main.lua"), "").unwrap();
    }

    #[test]
    fn scan_and_load() {
        let data = temp_root("load");
        let p = Plugins::new(&data);
        let root = p.root().to_path_buf();
        write_plugin(&root, "a", "alpha");
        write_plugin(&root, "b", "beta");
        // 清单错误
        std::fs::create_dir_all(root.join("c")).unwrap();
        std::fs::write(root.join("c/plugin.json"), "not json").unwrap();
        // 点开头的目录被忽略
        write_plugin(&root, ".hidden", "hidden");

        p.load();
        let list = p.plugins();
        assert_eq!(list.len(), 3, "{:?}", list.iter().map(|x| x.id.clone()).collect::<Vec<_>>());
        let alpha = p.get("alpha").unwrap();
        assert_eq!(alpha.state(), PluginState::Disabled);
        assert!(p.get("hidden").is_none());
        let bad = list.iter().find(|x| x.state() == PluginState::Invalid).unwrap();
        assert!(bad.error().is_some());
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn state_file_roundtrip() {
        let data = temp_root("state");
        let p = Plugins::new(&data);
        write_plugin(p.root(), "a", "alpha");
        p.load();
        let ph = p.get("alpha").unwrap();
        assert!(!ph.enabled());
        *ph.enabled.lock().unwrap() = true;
        *ph.granted.lock().unwrap() = vec!["device.shell".into()];
        p.save_state();

        let p2 = Plugins::new(&data);
        p2.load();
        let ph2 = p2.get("alpha").unwrap();
        assert!(ph2.enabled());
        assert_eq!(ph2.granted(), vec!["device.shell".to_string()]);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn fingerprint_changes() {
        let data = temp_root("fp");
        let p = Plugins::new(&data);
        write_plugin(p.root(), "a", "alpha");
        let f1 = dir_fingerprints(p.root());
        assert_eq!(f1.len(), 1);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(p.root().join("a/main.lua"), "-- changed").unwrap();
        let f2 = dir_fingerprints(p.root());
        assert_ne!(f1, f2);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn list_and_contrib_json() {
        let data = temp_root("json");
        let p = Plugins::new(&data);
        write_plugin(p.root(), "a", "alpha");
        p.load();
        let j = p.list_json(true, false);
        assert_eq!(j["manage"], json!(true));
        assert_eq!(j["safeMode"], json!(false));
        assert_eq!(j["list"][0]["id"], json!("alpha"));
        assert_eq!(j["list"][0]["state"], json!("disabled"));
        assert_eq!(j["list"][0]["permissions"], json!([]));
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn installed_packages_diff() {
        let before = "package:com.a\npackage:com.b\n";
        let after = "package:com.a\npackage:com.b\npackage:com.new\n";
        assert_eq!(new_packages(before, after), vec!["com.new".to_string()]);
        assert!(new_packages("", "package:x").len() == 1);
        assert!(new_packages("package:x", "package:x").is_empty());
    }

    #[test]
    fn device_ids() {
        // SPEC 7.1：含 `/` 的是远程设备，标识为 <peerId>/<serial>
        assert_eq!(split_dev("emulator-5554"), None);
        assert_eq!(split_dev("192.168.1.5:5555"), None);
        assert_eq!(split_dev("p1abc/192.168.1.5:5555"), Some(("p1abc", "192.168.1.5:5555")));
        assert_eq!(split_dev("p1abc/emulator-5554"), Some(("p1abc", "emulator-5554")));
        assert_eq!(split_dev("/serial"), None);
        assert_eq!(split_dev("peer/"), None);
        assert_eq!(split_dev(""), None);
        // match / filter 用原始 serial
        assert_eq!(raw_serial("p1abc/192.168.1.5:5555"), "192.168.1.5:5555");
        assert_eq!(raw_serial("emulator-5554"), "emulator-5554");
        let rules = manifest::MatchRules { serials: vec!["192.168.*".to_string()], ..Default::default() };
        assert!(manifest::device_matches(&rules, "Pixel", raw_serial("p1abc/192.168.1.5:5555"), false));
        assert!(!manifest::device_matches(&rules, "Pixel", "p1abc/192.168.1.5:5555", false));
    }

    #[test]
    fn device_json_fields() {
        let d = PluginDev {
            id: "p1/abc".into(),
            serial: "abc".into(),
            peer: Some("p1".into()),
            computer: "PC-B".into(),
            model: "Pixel".into(),
            name: "Pixel".into(),
            state: "device".into(),
        };
        let v = d.json();
        assert_eq!(v["serial"], json!("p1/abc"));
        assert_eq!(v["computer"], json!("PC-B"));
        assert_eq!(v["remote"], json!(true));
        let local = PluginDev { peer: None, ..d };
        assert_eq!(local.json()["remote"], json!(false));
    }

    #[test]
    fn settings_merge() {
        let man: Manifest = serde_json::from_str(
            r#"{"id":"x","name":"X","settings":[{"name":"a","type":"text","default":"d"},{"name":"b","type":"number"}]}"#,
        )
        .unwrap();
        let mut values = Map::new();
        values.insert("a".into(), json!("v"));
        let merged = settings_values(&man, &values);
        assert_eq!(merged["a"], json!("v"));
        assert_eq!(merged["b"], Value::Null);
    }

    #[test]
    fn wm_size_parsing() {
        assert_eq!(parse_wm_size("Physical size: 1080x2400\n"), Some((1080, 2400)));
        assert_eq!(parse_wm_size("Physical size: 1080x2400\nOverride size: 720x1600\n"), Some((720, 1600)));
        assert_eq!(parse_wm_size("garbage"), None);
    }

    #[test]
    fn input_tracker_synthesizes_tap_longpress_swipe_and_keys() {
        let mut trk = InputTracker::default();
        let t0 = Instant::now();

        // 1) 短按 → tap
        let (r0, a0) = trk.ingest("S1", &control::Ctl::Touch { action: 0, id: -2, x: 500, y: 800, w: 1080, h: 2400, p: 1.0, buttons: 0 }, None, t0);
        assert!(r0.is_some() && a0.is_none());
        let (r1, a1) = trk.ingest("S1", &control::Ctl::Touch { action: 1, id: -2, x: 505, y: 804, w: 1080, h: 2400, p: 0.0, buttons: 0 }, None, t0 + Duration::from_millis(80));
        assert!(r1.is_some());
        let act = a1.expect("tap action");
        assert_eq!(act["kind"], json!("tap"));
        assert_eq!((act["x"].clone(), act["y"].clone(), act["w"].clone(), act["h"].clone()), (json!(505), json!(804), json!(1080), json!(2400)));

        // 2) 原地按住 >= 500ms → longpress
        trk.ingest("S1", &control::Ctl::Touch { action: 0, id: -2, x: 300, y: 400, w: 1080, h: 2400, p: 1.0, buttons: 0 }, None, t0);
        let (_, a2) = trk.ingest("S1", &control::Ctl::Touch { action: 1, id: -2, x: 302, y: 401, w: 1080, h: 2400, p: 0.0, buttons: 0 }, None, t0 + Duration::from_millis(650));
        let act = a2.expect("longpress action");
        assert_eq!(act["kind"], json!("longpress"));
        assert_eq!(act["duration"], json!(650));

        // 3) 拖动 + move 节流 → swipe
        trk.ingest("S1", &control::Ctl::Touch { action: 0, id: -2, x: 500, y: 1800, w: 1080, h: 2400, p: 1.0, buttons: 0 }, None, t0);
        // 5ms 内的 move 被节流，20ms 后的 move 正常透传
        let (rm_fast, _) = trk.ingest("S1", &control::Ctl::Touch { action: 2, id: -2, x: 500, y: 1500, w: 1080, h: 2400, p: 1.0, buttons: 0 }, None, t0 + Duration::from_millis(5));
        assert!(rm_fast.is_none(), "16ms 内的 move 应节流");
        let (rm_ok, _) = trk.ingest("S1", &control::Ctl::Touch { action: 2, id: -2, x: 500, y: 1100, w: 1080, h: 2400, p: 1.0, buttons: 0 }, None, t0 + Duration::from_millis(20));
        assert!(rm_ok.is_some(), "超过 16ms 的 move 应发送");
        let (_, a3) = trk.ingest("S1", &control::Ctl::Touch { action: 1, id: -2, x: 500, y: 600, w: 1080, h: 2400, p: 0.0, buttons: 0 }, None, t0 + Duration::from_millis(220));
        let act = a3.expect("swipe action");
        assert_eq!(act["kind"], json!("swipe"));
        assert_eq!((act["x"].clone(), act["y"].clone(), act["x2"].clone(), act["y2"].clone()), (json!(500), json!(1800), json!(500), json!(600)));

        // 4) Back 与剪贴板粘贴
        let (_, ab) = trk.ingest("S1", &control::Ctl::Back { action: 0 }, None, t0);
        assert_eq!(ab.expect("back")["key"], json!("back"));
        let (_, bu) = trk.ingest("S1", &control::Ctl::Back { action: 1 }, None, t0);
        assert!(bu.is_none());
        let (_, at) = trk.ingest("S1", &control::Ctl::Clipboard { text: "你好".into(), paste: true, seq: 1 }, None, t0);
        assert_eq!(at.expect("paste")["text"], json!("你好"));
    }

    #[tokio::test]
    async fn audit_log_serializes_and_rotates() {
        let dir = std::env::temp_dir().join(format!("droidbridge-audit-{:08x}", crate::util::random_scid()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let path = dir.join(".audit.log");
        let lock = Arc::new(tokio::sync::Mutex::new(()));

        let mut handles = Vec::new();
        for i in 0..24 {
            let lock2 = lock.clone();
            let path2 = path.clone();
            handles.push(tokio::spawn(async move {
                append_audit_bounded(&lock2, &path2, &format!("{{\"seq\":{i},\"pad\":\"0123456789abcdef\"}}\n"), 220).await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let rotated = dir.join(".audit.log.1");
        assert!(rotated.exists(), "超出阈值后应生成轮转归档 .audit.log.1");
        assert!(path.exists(), "应继续写入新的 .audit.log");
        let cur = tokio::fs::read_to_string(&path).await.unwrap();
        let bak = tokio::fs::read_to_string(&rotated).await.unwrap();
        for line in cur.lines().chain(bak.lines()) {
            let v: Value = serde_json::from_str(line).expect("并发写入的每一行都应是完整合法 JSON");
            assert!(v.get("seq").is_some());
        }
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
