//! 插件宿主的共享状态与消息定义。
//!
//! 宿主是单线程的：Lua 只在驱动线程里跑，后台任务线程只往通道里投递消息。

use mlua::Thread;
use serde_json::{Value as J, json};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::Sender;

/// 驱动的消息来源（stdin 读线程、后台任务线程、测试注入）。
pub enum Msg {
    /// stdin 的一行文本
    Line(String),
    /// stdin 已关闭
    Eof,
    /// 后台任务完成
    Bg { id: u64, ok: Result<J, String> },
    /// 常驻子进程的事件：kind = "out" | "err"（一行输出）| "exit"（code 为退出码）
    Proc { id: u64, kind: &'static str, data: String, code: Option<i32> },
}

/// 输出一行 NDJSON 的回调（生产环境写 stdout，测试里收集到内存）。
pub type Sink = Rc<dyn Fn(&str)>;

/// init 里的插件信息
#[derive(Clone, Debug, Default)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub dir: PathBuf,
    pub data_dir: PathBuf,
    pub main: String,
}

/// 宿主共享状态：Lua 回调与驱动线程都能访问（Rc + 内部可变，单线程）。
pub struct Ctl {
    pub tx: Sender<Msg>,
    pub sink: Sink,
    pub info: PluginInfo,
    pub granted: HashSet<String>,
    pub lang: String,
    pub node: String,
    pub settings: RefCell<J>,
    /// 当前正在执行的协程所属请求带的 client（dialog 自动附上）
    pub client: RefCell<Option<String>>,
    /// `__host.spawn` 提交的协程，驱动线程在下一个循环取走
    pub spawn_queue: RefCell<Vec<(Thread, Option<String>)>>,
    /// `__host.unpark` 提交的等待键（供 `db.parallel` 唤醒父协程）
    pub unpark_queue: RefCell<Vec<u64>>,
    /// 后台任务编号
    pub bg_seq: Cell<u64>,
    /// 宿主生成 id（progress 等）
    pub uid: Cell<u64>,
    /// 待建立的定时器 (id, 延迟, 是否重复)
    pub new_timers: RefCell<Vec<(u64, f64, bool)>>,
    pub timer_cancel: RefCell<Vec<u64>>,
    pub timer_seq: Cell<u64>,
    /// dataDir/store.json 的缓存
    pub store: RefCell<Option<J>>,
    /// stdout 已关闭（找不到写端）时不再尝试输出
    pub closed: Cell<bool>,
    /// db.proc 启动的常驻子进程
    pub procs: RefCell<super::proc::Procs>,
}

impl Ctl {
    /// 输出一条 NDJSON
    pub fn emit(&self, v: J) {
        if self.closed.get() {
            return;
        }
        let s = v.to_string();
        (self.sink)(&s);
    }

    /// 记录一条日志
    pub fn log(&self, level: &str, msg: &str, src: Option<&str>) {
        let mut v = json!({"t": "log", "level": level, "msg": msg});
        if let Some(src) = src {
            if !src.is_empty() {
                v["src"] = J::String(src.to_string());
            }
        }
        self.emit(v);
    }

    /// 权限检查（`fs` 隐含 `fs.read`；`mcp` 是 `ai` 的旧名）
    pub fn has_perm(&self, p: &str) -> bool {
        let has = |x: &str| self.granted.contains(x);
        has(p) || (p == "fs.read" && has("fs")) || (p == "ai" && has("mcp")) || (p == "mcp" && has("ai"))
    }

    /// 下一个宿主内的唯一编号
    pub fn next_uid(&self) -> u64 {
        let n = self.uid.get() + 1;
        self.uid.set(n);
        n
    }
}
