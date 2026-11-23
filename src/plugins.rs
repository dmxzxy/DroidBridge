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

use crate::{adb, agent, AppState};
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

/// 每秒 50 次的限流（令牌桶）
struct Rate {
    tokens: f64,
    last: Instant,
}

impl Rate {
    fn new() -> Self {
        Self { tokens: 50.0, last: Instant::now() }
    }

    /// 取一个令牌；需要等待时返回等待时长
    fn take(&mut self) -> Duration {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * 50.0).min(50.0);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 - self.tokens) / 50.0)
        }
    }
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
    /// 每个插件同时最多 4 个设备操作
    sem: Semaphore,
    rate: Mutex<Rate>,
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
            sem: Semaphore::new(4),
            rate: Mutex::new(Rate::new()),
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

    /// 无人使用本次调用的令牌（每秒最多 50 次，超出时排队等待）
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

    /// 审计日志（会改变设备或运行程序的操作）
    pub(crate) fn audit(&self, plugin: &str, op: &str, detail: Value) {
        let path = self.root.join(".audit.log");
        let line = json!({
            "time": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            "plugin": plugin,
            "op": op,
            "detail": detail,
        })
        .to_string()
            + "\n";
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            if let Ok(mut f) = tokio::fs::OpenOptions::new().append(true).create(true).open(&path).await {
                let _ = f.write_all(line.as_bytes()).await;
            }
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
        self.dev_sems.lock().unwrap().entry(serial.to_string()).or_insert_with(|| Arc::new(Semaphore::new(2))).clone()
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

/// 清掉 `.tmp` 里 24 小时前的上传/截图文件
async fn cleanup_tmp(root: &Path) {
    let dir = root.join(".tmp");
    let Ok(mut rd) = tokio::fs::read_dir(&dir).await else { return };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let Ok(meta) = entry.metadata().await else { continue };
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
}
