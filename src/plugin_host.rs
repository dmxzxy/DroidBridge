//! 插件宿主进程（`--plugin-host`）。规格见 docs/plugins/SPEC.md 第 3、6 节。
//!
//! 设计：
//! - 单线程跑 Lua。所有 Lua 代码都在独立协程里，API 通过 `coroutine.yield(TOKEN, req)`
//!   把调度请求交给驱动线程，驱动线程发出 `call` 或启动后台任务，拿到结果后 resume。
//! - stdin 读线程、后台任务线程都往同一个 mpsc 通道投递消息；主循环用 `recv_timeout`
//!   驱动定时器与 `db.wait`。

mod api;
mod json;
pub(crate) mod local;
mod proc;
mod state;

use mlua::{Function, Lua, MultiValue, Table, Thread, Value};
use serde_json::{Map, Value as J, json};
use state::{Ctl, Msg, PluginInfo, Sink};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// 若命令行带 `--plugin-host`，运行插件宿主直到 stdin 关闭，返回 true；否则立即返回 false。
pub fn hook() -> bool {
    if !std::env::args().skip(1).any(|a| a == "--plugin-host") {
        return false;
    }
    run_stdio();
    true
}

fn run_stdio() {
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    let tx2 = tx.clone();
    // stdin 读线程：逐行读，EOF 后投递 Eof
    std::thread::spawn(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        let mut lock = stdin.lock();
        let mut buf = String::new();
        loop {
            buf.clear();
            match lock.read_line(&mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    let line = buf.trim_end_matches(['\r', '\n']);
                    if !line.is_empty() && tx2.send(Msg::Line(line.to_string())).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx2.send(Msg::Eof);
    });
    let sink: Sink = Rc::new(|line: &str| {
        use std::io::Write;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    });
    let mut driver = Driver::new(HostConfig::default(), tx, sink);
    driver.run(&rx);
}

/// 宿主限制
#[derive(Clone, Debug)]
struct HostConfig {
    /// 每次 resume 连续执行的上限
    cpu_limit: Duration,
    /// Lua 内存上限
    mem_limit: usize,
    /// 每多少条指令检查一次 CPU
    instr_step: u32,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            cpu_limit: Duration::from_secs(3),
            mem_limit: 256 * 1024 * 1024,
            instr_step: 10_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskKind {
    /// main.lua 顶层
    Boot,
    /// 主进程 req
    Req(u64),
    /// 定时器回调
    Timer(u64),
    /// db.spawn / 事件监听者
    Spawn,
}

struct Task {
    thread: Thread,
    kind: TaskKind,
    /// 发起该协程的请求带的 client（dialog 自动附上）
    client: Option<String>,
}

struct Timer {
    due: Instant,
    interval: Option<Duration>,
}

enum Outcome {
    /// 协程 yield 了
    Yield(MultiValue),
    /// 协程正常结束
    Done(MultiValue),
    /// 出错
    Err(String),
}

struct Driver {
    cfg: HostConfig,
    tx: Sender<Msg>,
    sink: Sink,
    lua: Option<Lua>,
    g: Option<Table>,
    h: Option<Table>,
    ctl: Option<Rc<Ctl>>,
    deadline: Rc<Cell<Option<Instant>>>,
    tasks: HashMap<u64, Task>,
    next_task: u64,
    /// 待 resume 的协程及 resume 参数
    ready: VecDeque<(u64, MultiValue)>,
    /// 等待唤醒的协程
    sleeping: Vec<(Instant, u64)>,
    /// 等待子协程完成的父协程（park key → task id）
    parked: HashMap<u64, u64>,
    timers: HashMap<u64, Timer>,
    /// 等待主进程 ret 的 call：call id → task id
    awaiting: HashMap<u64, u64>,
    /// 进行中的后台任务：bg id → task id
    bgs: HashMap<u64, u64>,
    next_call: u64,
    booted: bool,
    stop: bool,
}

impl Driver {
    fn new(cfg: HostConfig, tx: Sender<Msg>, sink: Sink) -> Self {
        Self {
            cfg,
            tx,
            sink,
            lua: None,
            g: None,
            h: None,
            ctl: None,
            deadline: Rc::new(Cell::new(None)),
            tasks: HashMap::new(),
            next_task: 0,
            ready: VecDeque::new(),
            sleeping: Vec::new(),
            parked: HashMap::new(),
            timers: HashMap::new(),
            awaiting: HashMap::new(),
            bgs: HashMap::new(),
            next_call: 0,
            booted: false,
            stop: false,
        }
    }

    fn emit(&self, v: J) {
        if let Some(ctl) = &self.ctl {
            ctl.emit(v);
        }
    }

    // ------------------------------------------------------------ 主循环

    fn run(&mut self, rx: &Receiver<Msg>) {
        loop {
            self.pump();
            if self.stop {
                break;
            }
            let timeout = self.next_wait();
            match rx.recv_timeout(timeout) {
                Ok(Msg::Line(line)) => self.on_line(&line),
                Ok(Msg::Eof) => break,
                Ok(Msg::Bg { id, ok }) => self.on_bg(id, ok),
                Ok(Msg::Proc { id, kind, data, code }) => self.on_proc(id, kind, data, code),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        self.shutdown();
    }

    /// 最早需要唤醒的时间
    fn next_wait(&self) -> Duration {
        let now = Instant::now();
        let mut best: Option<Instant> = None;
        for t in self.timers.values() {
            best = Some(best.map_or(t.due, |b: Instant| b.min(t.due)));
        }
        for (d, _) in &self.sleeping {
            best = Some(best.map_or(*d, |b: Instant| b.min(*d)));
        }
        match best {
            Some(d) => d.saturating_duration_since(now).max(Duration::from_millis(1)),
            None => Duration::from_secs(30),
        }
    }

    /// 反复处理，直到没有新的协程 / 定时器 / 唤醒要处理
    fn pump(&mut self) {
        for _ in 0..256 {
            if !self.pump_once() {
                break;
            }
        }
    }

    /// 处理一轮：新协程、定时器、睡眠、就绪协程；返回是否做了事
    fn pump_once(&mut self) -> bool {
        let mut did = false;
        // __host.spawn 提交的协程
        let spawns: Vec<(Thread, Option<String>)> = match &self.ctl {
            Some(ctl) => ctl.spawn_queue.borrow_mut().drain(..).collect(),
            None => Vec::new(),
        };
        did |= !spawns.is_empty();
        for (thread, client) in spawns {
            let id = self.next_task;
            self.next_task += 1;
            self.tasks.insert(id, Task { thread, kind: TaskKind::Spawn, client });
            self.ready.push_back((id, MultiValue::from_vec(vec![])));
        }
        // __host.unpark 唤醒等待中的父协程
        if let Some(ctl) = &self.ctl {
            let keys: Vec<u64> = ctl.unpark_queue.borrow_mut().drain(..).collect();
            for key in keys {
                if let Some(tid) = self.parked.remove(&key) {
                    did = true;
                    self.ready.push_back((tid, MultiValue::from_vec(vec![Value::Boolean(true)])));
                }
            }
        }
        // 新定时器 / 取消
        if let Some(ctl) = &self.ctl {
            let new: Vec<(u64, f64, bool)> = ctl.new_timers.borrow_mut().drain(..).collect();
            did |= !new.is_empty();
            let now = Instant::now();
            for (id, delay, repeat) in new {
                let due = now + Duration::from_secs_f64(delay);
                // 重复定时器至少 10ms，避免 0 间隔把宿主拖死
                let interval = repeat.then(|| Duration::from_secs_f64(delay.max(0.01)));
                self.timers.insert(id, Timer { due, interval });
            }
            let cancels: Vec<u64> = ctl.timer_cancel.borrow_mut().drain(..).collect();
            did |= !cancels.is_empty();
            for id in cancels {
                self.timers.remove(&id);
            }
        }
        // 到期定时器
        let now = Instant::now();
        let mut due = Vec::new();
        self.timers.retain(|id, t| {
            if t.due <= now {
                due.push((t.due, *id, t.interval));
                false
            } else {
                true
            }
        });
        did |= !due.is_empty();
        // 同时到期的按到期时间（再按创建顺序）触发，不能依赖 HashMap 的遍历顺序
        due.sort_by_key(|(d, id, _)| (*d, *id));
        for (_, id, interval) in due {
            match interval {
                Some(iv) => {
                    self.timers.insert(
                        id,
                        Timer {
                            due: Instant::now() + iv,
                            interval: Some(iv),
                        },
                    );
                    self.fire_timer(id, true);
                }
                None => self.fire_timer(id, false),
            }
        }
        // 睡眠到期
        let now = Instant::now();
        let mut wake = Vec::new();
        self.sleeping.retain(|(d, tid)| {
            if *d <= now {
                wake.push(*tid);
                false
            } else {
                true
            }
        });
        did |= !wake.is_empty();
        for tid in wake {
            self.ready.push_back((tid, MultiValue::from_vec(vec![Value::Boolean(true)])));
        }
        // 运行就绪协程
        while let Some((tid, args)) = self.ready.pop_front() {
            did = true;
            self.step(tid, args);
            if self.stop {
                break;
            }
        }
        did
    }

    /// 运行 / 恢复一个协程
    fn step(&mut self, tid: u64, args: MultiValue) {
        let Some(task) = self.tasks.remove(&tid) else { return };
        if self.lua.is_none() {
            return;
        }
        if let Some(ctl) = &self.ctl {
            *ctl.client.borrow_mut() = task.client.clone();
        }
        self.deadline.set(Some(Instant::now() + self.cfg.cpu_limit));
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| task.thread.resume::<MultiValue>(args)));
        self.deadline.set(None);
        let outcome = match r {
            Ok(Ok(vals)) => {
                // 第一个返回值是调度令牌 → 协程在等宿主
                if matches!(vals.get(0), Some(Value::String(s)) if s.as_bytes() == api::SCHED_TOKEN.as_bytes()) {
                    Outcome::Yield(vals)
                } else {
                    Outcome::Done(vals)
                }
            }
            Ok(Err(e)) => Outcome::Err(err_text(&e)),
            Err(p) => Outcome::Err(format!("panic: {}", panic_text(p.as_ref()))),
        };
        match outcome {
            Outcome::Yield(vals) => self.on_yield(tid, task, vals),
            Outcome::Done(vals) => self.finish(task, Ok(vals)),
            Outcome::Err(e) => self.finish(task, Err(e)),
        }
    }

    fn on_yield(&mut self, tid: u64, task: Task, vals: MultiValue) {
        // 只有第一个返回值是调度令牌才是调度请求
        let is_sched = matches!(vals.get(0), Some(Value::String(s)) if s.as_bytes() == api::SCHED_TOKEN.as_bytes());
        if !is_sched {
            // 插件自己 yield 出来的值：按结束处理
            self.finish(task, Ok(vals));
            return;
        }
        let req = match vals.get(1) {
            Some(Value::Table(t)) => t.clone(),
            _ => {
                self.finish(task, Err("无效的调度请求".into()));
                return;
            }
        };
        let k: String = req.get("k").unwrap_or_default();
        match k.as_str() {
            "call" => {
                let m: String = req.get("m").unwrap_or_default();
                let p = req.get::<Option<Value>>("p").ok().flatten().map(|v| json::to_json(&v));
                let mut p = p.unwrap_or_else(|| J::Object(Map::new()));
                if m == "dialog" || m == "uiopen" || m == "mirror" {
                    if let (Some(c), J::Object(o)) = (task.client.as_ref(), &mut p) {
                        o.insert("client".to_string(), J::String(c.clone()));
                    }
                }
                let id = self.next_call;
                self.next_call += 1;
                self.emit(json!({"t": "call", "id": id, "m": m, "p": p}));
                self.awaiting.insert(id, tid);
                self.tasks.insert(tid, task);
            }
            "sleep" => {
                let sec: f64 = req.get("sec").unwrap_or(0.0);
                let sec = sec.clamp(0.0, 3600.0);
                self.sleeping.push((Instant::now() + Duration::from_secs_f64(sec), tid));
                self.tasks.insert(tid, task);
            }
            "park" => {
                let key: u64 = req.get("key").unwrap_or(0);
                self.parked.insert(key, tid);
                self.tasks.insert(tid, task);
            }
            "exec" | "open" | "http" | "download" | "walk" | "tar" | "hash" | "clip" => match self.start_bg(&req, &k) {
                Ok(bg) => {
                    self.bgs.insert(bg, tid);
                    self.tasks.insert(tid, task);
                }
                Err(e) => self.finish(task, Err(e)),
            },
            other => self.finish(task, Err(format!("未知的调度请求: {other}"))),
        }
    }

    fn finish(&mut self, task: Task, result: Result<MultiValue, String>) {
        match task.kind {
            TaskKind::Boot => {
                if let Err(e) = &result {
                    self.log_error(e);
                }
                self.emit_ready();
            }
            TaskKind::Req(rid) => {
                let out = match result {
                    Ok(vals) => {
                        let v = vals.get(0).map(json::to_json).unwrap_or(J::Null);
                        json!({"t": "res", "id": rid, "ok": v})
                    }
                    Err(e) => {
                        self.log_error(&e);
                        json!({"t": "res", "id": rid, "err": first_line(&e)})
                    }
                };
                self.emit(out);
            }
            TaskKind::Timer(_) | TaskKind::Spawn => {
                if let Err(e) = result {
                    self.log_error(&e);
                }
            }
        }
    }

    fn emit_ready(&mut self) {
        if self.booted {
            return;
        }
        self.booted = true;
        self.emit(json!({"t": "ready"}));
    }

    fn log_error(&self, text: &str) {
        if let Some(ctl) = &self.ctl {
            let src = src_from_traceback(text);
            ctl.log("error", text, src.as_deref());
        }
    }

    // ------------------------------------------------------------ 消息

    fn on_line(&mut self, line: &str) {
        let v: J = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return,
        };
        match v.get("t").and_then(|t| t.as_str()).unwrap_or("") {
            "init" => self.boot(&v),
            "req" => self.on_req(&v),
            "ret" => {
                let id = v.get("id").and_then(|i| i.as_u64()).unwrap_or(0);
                let payload = v.clone();
                self.on_ret(id, &payload);
            }
            _ => {}
        }
    }

    /// 初始化：建立沙箱、加载 main.lua
    fn boot(&mut self, v: &J) {
        if self.lua.is_some() {
            return;
        }
        let p = v.get("plugin").cloned().unwrap_or(J::Null);
        let info = PluginInfo {
            id: p.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            name: p.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            version: p.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            dir: PathBuf::from(p.get("dir").and_then(|x| x.as_str()).unwrap_or("")),
            data_dir: PathBuf::from(p.get("dataDir").and_then(|x| x.as_str()).unwrap_or("")),
            main: p
                .get("main")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("main.lua")
                .replace('\\', "/"),
        };
        let _ = std::fs::create_dir_all(&info.data_dir);
        let granted: HashSet<String> = v
            .get("granted")
            .and_then(|g| g.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        let settings = v
            .get("settings")
            .cloned()
            .filter(|s| s.is_object())
            .unwrap_or_else(|| J::Object(Map::new()));
        let ctl = Rc::new(Ctl {
            tx: self.tx.clone(),
            sink: self.sink.clone(),
            info: info.clone(),
            granted,
            lang: v.get("lang").and_then(|x| x.as_str()).unwrap_or("zh").to_string(),
            node: v.get("node").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            settings: RefCell::new(settings),
            client: RefCell::new(None),
            spawn_queue: RefCell::new(Vec::new()),
            unpark_queue: RefCell::new(Vec::new()),
            bg_seq: Cell::new(0),
            uid: Cell::new(0),
            new_timers: RefCell::new(Vec::new()),
            timer_cancel: RefCell::new(Vec::new()),
            timer_seq: Cell::new(0),
            store: RefCell::new(None),
            closed: Cell::new(false),
            procs: RefCell::new(proc::Procs::default()),
        });
        let lua = Lua::new();
        if let Err(e) = lua.set_memory_limit(self.cfg.mem_limit) {
            ctl.log("error", &format!("无法设置内存上限: {e}"), None);
        }
        let (g, h) = match api::install(&lua, ctl.clone(), self.deadline.clone(), self.cfg.instr_step) {
            Ok(v) => v,
            Err(e) => {
                ctl.log("error", &format!("宿主初始化失败: {e}"), None);
                self.ctl = Some(ctl);
                self.emit_ready();
                return;
            }
        };
        self.ctl = Some(ctl.clone());
        self.g = Some(g.clone());
        self.h = Some(h.clone());
        self.lua = Some(lua);
        let rel = info.main.clone();
        let full = info.dir.join(&rel);
        let src = match std::fs::read_to_string(&full) {
            Ok(s) => s,
            Err(e) => {
                ctl.log("error", &format!("无法读取 {rel}: {e}"), Some(&rel));
                self.emit_ready();
                return;
            }
        };
        let chunk = {
            let lua = self.lua.as_ref().expect("lua");
            lua.load(&src)
                .set_name(&format!("@{rel}"))
                .set_environment(g)
                .into_function()
        };
        let chunk = match chunk {
            Ok(f) => f,
            Err(e) => {
                ctl.log("error", &err_text(&e), Some(&rel));
                self.emit_ready();
                return;
            }
        };
        if let Err(e) = self.new_task(chunk, None, TaskKind::Boot, None) {
            ctl.log("error", &e, Some(&rel));
            self.emit_ready();
        }
    }

    fn on_req(&mut self, v: &J) {
        let rid = v.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
        let m = v.get("m").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let p = v.get("p").cloned().unwrap_or_else(|| J::Object(Map::new()));
        let client = p.get("client").and_then(|c| c.as_str()).map(|s| s.to_string());
        let Some(lua) = self.lua.as_ref() else {
            self.emit(json!({"t": "res", "id": rid, "err": "插件宿主尚未初始化"}));
            return;
        };
        let Some(h) = self.h.clone() else { return };
        let pval = match json::to_lua(lua, &p) {
            Ok(v) => v,
            Err(e) => {
                self.emit(json!({"t": "res", "id": rid, "err": format!("参数转换失败: {e}")}));
                return;
            }
        };
        let f: Function = match h.get("dispatch") {
            Ok(f) => f,
            Err(_) => {
                self.emit(json!({"t": "res", "id": rid, "err": "缺少 dispatch"}));
                return;
            }
        };
        let args = match lua.create_table() {
            Ok(t) => t,
            Err(_) => return,
        };
        let _ = args.set(1, m);
        let _ = args.set(2, pval);
        if let Err(e) = self.new_task(f, Some(args), TaskKind::Req(rid), client) {
            self.emit(json!({"t": "res", "id": rid, "err": e}));
        }
    }

    fn on_ret(&mut self, id: u64, v: &J) {
        let Some(tid) = self.awaiting.remove(&id) else { return };
        let Some(lua) = self.lua.as_ref() else { return };
        let mv = match v.get("err") {
            Some(J::String(e)) => MultiValue::from_vec(vec![
                Value::Boolean(false),
                match lua.create_string(e) {
                    Ok(s) => Value::String(s),
                    Err(_) => Value::Nil,
                },
            ]),
            _ => {
                let okv = v.get("ok").cloned().unwrap_or(J::Null);
                let lv = json::to_lua(lua, &okv).unwrap_or(Value::Nil);
                MultiValue::from_vec(vec![Value::Boolean(true), lv])
            }
        };
        self.ready.push_back((tid, mv));
    }

    fn on_bg(&mut self, id: u64, result: Result<J, String>) {
        let Some(tid) = self.bgs.remove(&id) else { return };
        let Some(lua) = self.lua.as_ref() else { return };
        let mv = match result {
            Ok(v) => {
                let lv = json::to_lua(lua, &v).unwrap_or(Value::Nil);
                MultiValue::from_vec(vec![Value::Boolean(true), lv])
            }
            Err(e) => MultiValue::from_vec(vec![
                Value::Boolean(false),
                match lua.create_string(&e) {
                    Ok(s) => Value::String(s),
                    Err(_) => Value::Nil,
                },
            ]),
        };
        self.ready.push_back((tid, mv));
    }

    // ------------------------------------------------------------ 任务

    fn new_task(
        &mut self,
        f: Function,
        args: Option<Table>,
        kind: TaskKind,
        client: Option<String>,
    ) -> Result<u64, String> {
        let (Some(lua), Some(h)) = (self.lua.as_ref(), self.h.as_ref()) else {
            return Err("宿主尚未初始化".into());
        };
        let task_fn: Function = h.get("task_from").map_err(|e| err_text(&e))?;
        let body: Function = match args {
            Some(t) => task_fn.call((f, t)).map_err(|e| err_text(&e))?,
            None => task_fn.call((f, Value::Nil)).map_err(|e| err_text(&e))?,
        };
        let thread = lua.create_thread(body).map_err(|e| err_text(&e))?;
        api::set_thread_hook(&thread, &self.deadline, self.cfg.instr_step).map_err(|e| err_text(&e))?;
        let id = self.next_task;
        self.next_task += 1;
        self.tasks.insert(id, Task { thread, kind, client });
        self.ready.push_back((id, MultiValue::from_vec(vec![])));
        Ok(id)
    }

    /// 常驻子进程的一行输出 / 退出：在新协程里调用 H.proc_event(id, kind, data, code)
    fn on_proc(&mut self, id: u64, kind: &str, data: String, code: Option<i32>) {
        if kind == "exit" {
            if let Some(ctl) = &self.ctl {
                ctl.procs.borrow_mut().exited(id);
            }
        }
        let Some(h) = self.h.clone() else { return };
        let Some(lua) = self.lua.as_ref() else { return };
        let Ok(f) = h.get::<Function>("proc_event") else { return };
        let Ok(args) = lua.create_table() else { return };
        let _ = args.set(1, id);
        let _ = args.set(2, kind);
        match lua.create_string(&data) {
            Ok(s) => {
                let _ = args.set(3, s);
            }
            Err(_) => return,
        }
        if let Some(c) = code {
            let _ = args.set(4, c);
        }
        if let Err(e) = self.new_task(f, Some(args), TaskKind::Spawn, None) {
            self.log_error(&e);
        }
    }

    fn fire_timer(&mut self, id: u64, repeat: bool) {
        let Some(h) = self.h.clone() else { return };
        let Some(lua) = self.lua.as_ref() else { return };
        let Ok(f) = h.get::<Function>("fire_timer") else { return };
        let args = match lua.create_table() {
            Ok(t) => t,
            Err(_) => return,
        };
        let _ = args.set(1, id);
        let _ = args.set(2, repeat);
        if let Err(e) = self.new_task(f, Some(args), TaskKind::Timer(id), None) {
            self.log_error(&e);
        }
    }

    /// 启动后台任务，返回 bg id
    fn start_bg(&mut self, req: &Table, kind: &str) -> Result<u64, String> {
        let Some(ctl) = self.ctl.clone() else { return Err("宿主尚未初始化".into()) };
        let id = ctl.bg_seq.get() + 1;
        ctl.bg_seq.set(id);
        let tx = ctl.tx.clone();
        match kind {
            "exec" => {
                let program: String = req.get("program").map_err(|e| err_text(&e))?;
                let args = str_list(req.get::<Table>("args").ok().as_ref());
                let cwd = opt_string(req, "cwd");
                let env = str_pairs(req.get::<Table>("env").ok().as_ref());
                let input = opt_string(req, "input");
                let timeout = Duration::from_secs_f64(opt_f64(req, "timeout").unwrap_or(60.0).clamp(0.1, 600.0));
                std::thread::spawn(move || {
                    let r = local::exec(&program, &args, cwd.as_deref(), timeout, &env, input.as_deref());
                    let _ = tx.send(Msg::Bg { id, ok: r });
                });
            }
            "open" => {
                let target: String = req.get("target").map_err(|e| err_text(&e))?;
                std::thread::spawn(move || {
                    let r = local::open(&target).map(|_| J::Bool(true));
                    let _ = tx.send(Msg::Bg { id, ok: r });
                });
            }
            "http" => {
                let url: String = req.get("url").map_err(|e| err_text(&e))?;
                let method = opt_string(req, "method").unwrap_or_else(|| "GET".into());
                let headers = str_pairs(req.get::<Table>("headers").ok().as_ref());
                let body = opt_string(req, "body");
                let timeout = Duration::from_secs_f64(opt_f64(req, "timeout").unwrap_or(30.0).clamp(0.1, 600.0));
                std::thread::spawn(move || {
                    let r = local::http(&url, &method, &headers, body.as_deref(), timeout);
                    let _ = tx.send(Msg::Bg { id, ok: r });
                });
            }
            "download" => {
                let url: String = req.get("url").map_err(|e| err_text(&e))?;
                let dst = local::resolve(&ctl, &req.get::<String>("path").map_err(|e| err_text(&e))?, true)?;
                let headers = str_pairs(req.get::<Table>("headers").ok().as_ref());
                let timeout = Duration::from_secs_f64(opt_f64(req, "timeout").unwrap_or(1800.0).clamp(1.0, 7200.0));
                std::thread::spawn(move || {
                    let r = local::download(&url, &headers, &dst, timeout);
                    let _ = tx.send(Msg::Bg { id, ok: r });
                });
            }
            "walk" => {
                let dir = local::resolve(&ctl, &req.get::<String>("dir").map_err(|e| err_text(&e))?, false)?;
                let ext = str_list(req.get::<Table>("ext").ok().as_ref());
                let hidden = req.get::<bool>("hidden").unwrap_or(false);
                std::thread::spawn(move || {
                    let r = local::walk(&dir, &ext, hidden);
                    let _ = tx.send(Msg::Bg { id, ok: r });
                });
            }
            "tar" => {
                let dir = local::resolve(&ctl, &req.get::<String>("dir").map_err(|e| err_text(&e))?, false)?;
                let out = local::resolve(&ctl, &req.get::<String>("out").map_err(|e| err_text(&e))?, true)?;
                let files = req.get::<Table>("files").ok().map(|t| str_list(Some(&t)));
                let gzip = req.get::<bool>("gzip").unwrap_or(false);
                std::thread::spawn(move || {
                    let r = local::tar(&dir, files, &out, gzip);
                    let _ = tx.send(Msg::Bg { id, ok: r });
                });
            }
            "hash" => {
                let dir = local::resolve(&ctl, &req.get::<String>("dir").map_err(|e| err_text(&e))?, false)?;
                let files = str_list(req.get::<Table>("files").ok().as_ref());
                std::thread::spawn(move || {
                    let r = local::hash(&dir, &files);
                    let _ = tx.send(Msg::Bg { id, ok: r });
                });
            }
            "clip" => {
                let op = opt_string(req, "op").unwrap_or_else(|| "get".into());
                let text = opt_string(req, "text").unwrap_or_default();
                std::thread::spawn(move || {
                    let r = match op.as_str() {
                        "set" => local::clipboard_set(&text).map(|_| J::Bool(true)),
                        _ => local::clipboard_get().map(J::String),
                    };
                    let _ = tx.send(Msg::Bg { id, ok: r });
                });
            }
            other => return Err(format!("未知的后台任务: {other}")),
        }
        Ok(id)
    }

    // ------------------------------------------------------------ 退出

    fn shutdown(&mut self) {
        self.shutdown_lua();
        // plugin.unload 里可能还会用到子进程：最后再结束它们
        if let Some(ctl) = &self.ctl {
            ctl.procs.borrow_mut().kill_all();
        }
        self.stop = true;
    }

    fn shutdown_lua(&mut self) {
        if self.lua.is_none() {
            return;
        }
        // 触发 plugin.unload（最多等 1 秒，父进程会在 3 秒后强杀）
        let f = self.h.as_ref().and_then(|h| h.get::<Function>("dispatch").ok());
        if let Some(f) = f {
            let pval = self
                .lua
                .as_ref()
                .and_then(|lua| json::to_lua(lua, &json!({"name": "plugin.unload", "args": []})).ok())
                .unwrap_or(Value::Nil);
            let args = self.lua.as_ref().and_then(|lua| lua.create_table().ok());
            if let Some(args) = args {
                let _ = args.set(1, "event");
                let _ = args.set(2, pval);
                let _ = self.new_task(f, Some(args), TaskKind::Spawn, None);
            }
        }
        let end = Instant::now() + Duration::from_secs(1);
        while Instant::now() < end {
            self.pump();
            if self.ready.is_empty() && self.tasks.is_empty() {
                break;
            }
            if !self.ready.is_empty() {
                continue;
            }
            // 还有协程在等主进程 / 后台任务
            std::thread::sleep(Duration::from_millis(10));
        }
        self.stop = true;
    }
}

// ---------------------------------------------------------------- 工具

fn err_text(e: &mlua::Error) -> String {
    match e {
        mlua::Error::RuntimeError(m) => m.clone(),
        mlua::Error::MemoryError(m) => format!("memory error: {m}"),
        other => other.to_string(),
    }
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

fn first_line(s: &str) -> String {
    let l = s.lines().next().unwrap_or("").trim();
    let l = l.strip_prefix("runtime error: ").unwrap_or(l).trim();
    if l.is_empty() {
        "error".to_string()
    } else {
        l.to_string()
    }
}

/// 从错误文本（含 traceback）里提取第一处插件代码位置，形如 `main.lua:12`
fn src_from_traceback(text: &str) -> Option<String> {
    for raw in text.lines() {
        let line = raw.trim();
        let Some(pos) = line.find(".lua:") else { continue };
        let head = &line[..pos + 4];
        let start = head
            .rfind(|c: char| c == ' ' || c == '[' || c == '(' || c == '"' || c == '\t')
            .map(|i| i + 1)
            .unwrap_or(0);
        let name = &head[start..];
        let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
        if base == "db-prelude" || base.is_empty() {
            continue;
        }
        let digits: String = line[pos + 5..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        return Some(format!("{base}:{digits}"));
    }
    None
}

fn opt_string(req: &Table, key: &str) -> Option<String> {
    req.get::<Option<String>>(key).ok().flatten()
}

fn opt_f64(req: &Table, key: &str) -> Option<f64> {
    req.get::<Option<f64>>(key).ok().flatten()
}

pub(crate) fn str_list(v: Option<&Table>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(t) = v {
        for i in 1..=t.raw_len() {
            if let Ok(s) = t.raw_get::<Value>(i) {
                out.push(match s {
                    Value::String(s) => s.to_string_lossy().to_string(),
                    Value::Integer(i) => i.to_string(),
                    Value::Number(n) => n.to_string(),
                    _ => continue,
                });
            }
        }
    }
    out
}

pub(crate) fn str_pairs(v: Option<&Table>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(t) = v {
        for pair in t.clone().pairs::<Value, Value>() {
            if let Ok((k, val)) = pair {
                let key = match k {
                    Value::String(s) => s.to_string_lossy().to_string(),
                    _ => continue,
                };
                let value = match val {
                    Value::String(s) => s.to_string_lossy().to_string(),
                    Value::Integer(i) => i.to_string(),
                    Value::Number(n) => n.to_string(),
                    Value::Boolean(b) => b.to_string(),
                    _ => continue,
                };
                out.push((key, value));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
