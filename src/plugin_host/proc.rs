//! 插件启动的常驻子进程（`db.proc.start`）。
//!
//! 每个进程配三个线程：写 stdin（通过通道，Lua 写入不会阻塞驱动线程）、按行读 stdout、按行读 stderr；
//! 还有一个线程等进程退出。读到的每一行、退出事件都以 `Msg::Proc` 投递给驱动线程，
//! 由驱动在新协程里调用 Lua 的 `H.proc_event`。
//! 插件停止时宿主会结束所有子进程；宿主被强杀时子进程的 stdin 也会关闭，
//! 按约定读到 EOF 就退出的程序会跟着退出。

use super::state::Msg;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 同时运行的子进程上限
pub const MAX_PROCS: usize = 8;

pub struct Proc {
    /// Some(数据) = 写入；None = 关闭 stdin
    stdin: Option<Sender<Option<Vec<u8>>>>,
    child: Arc<Mutex<Child>>,
}

#[derive(Default)]
pub struct Procs {
    list: HashMap<u64, Proc>,
    seq: u64,
}

impl Procs {
    pub fn start(
        &mut self,
        tx: &Sender<Msg>,
        program: &str,
        args: &[String],
        cwd: Option<&str>,
        env: &[(String, String)],
    ) -> Result<u64, String> {
        if self.list.len() >= MAX_PROCS {
            return Err(format!("同时运行的子进程最多 {MAX_PROCS} 个"));
        }
        let mut cmd = Command::new(program);
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
            cmd.current_dir(cwd);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().map_err(|e| format!("无法启动 {program}: {e}"))?;
        self.seq += 1;
        let id = self.seq;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // 写 stdin
        let (wtx, wrx) = mpsc::channel::<Option<Vec<u8>>>();
        if let Some(mut si) = stdin {
            std::thread::spawn(move || {
                while let Ok(Some(data)) = wrx.recv() {
                    if si.write_all(&data).and_then(|_| si.flush()).is_err() {
                        break;
                    }
                }
                // 通道关闭或收到 None：drop si，子进程读到 EOF
            });
        }

        let readers: Vec<std::thread::JoinHandle<()>> = [(stdout.map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), "out"), (stderr.map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), "err")]
            .into_iter()
            .filter_map(|(r, kind)| {
                let r = r?;
                let tx = tx.clone();
                Some(std::thread::spawn(move || {
                    let mut br = BufReader::new(r);
                    let mut buf = Vec::new();
                    loop {
                        buf.clear();
                        match br.read_until(b'\n', &mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                while matches!(buf.last(), Some(b'\n' | b'\r')) {
                                    buf.pop();
                                }
                                let line = String::from_utf8_lossy(&buf).into_owned();
                                if tx.send(Msg::Proc { id, kind, data: line, code: None }).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }))
            })
            .collect();

        let child = Arc::new(Mutex::new(child));
        {
            let child = child.clone();
            let tx = tx.clone();
            std::thread::spawn(move || {
                // 先等输出读完，保证“退出”排在最后一行之后
                for r in readers {
                    let _ = r.join();
                }
                let code = loop {
                    match child.lock().unwrap().try_wait() {
                        Ok(Some(st)) => break st.code(),
                        Ok(None) => {}
                        Err(_) => break None,
                    }
                    std::thread::sleep(Duration::from_millis(30));
                };
                let _ = tx.send(Msg::Proc { id, kind: "exit", data: String::new(), code: Some(code.unwrap_or(-1)) });
            });
        }

        self.list.insert(id, Proc { stdin: Some(wtx), child });
        Ok(id)
    }

    pub fn write(&mut self, id: u64, data: Vec<u8>) -> Result<(), String> {
        let p = self.list.get(&id).ok_or("子进程已退出")?;
        let tx = p.stdin.as_ref().ok_or("stdin 已关闭")?;
        tx.send(Some(data)).map_err(|_| "子进程已退出".to_string())
    }

    pub fn close_stdin(&mut self, id: u64) {
        if let Some(p) = self.list.get_mut(&id) {
            if let Some(tx) = p.stdin.take() {
                let _ = tx.send(None);
            }
        }
    }

    pub fn kill(&mut self, id: u64) {
        if let Some(mut p) = self.list.remove(&id) {
            if let Some(tx) = p.stdin.take() {
                let _ = tx.send(None);
            }
            let child = p.child.clone();
            // 先给 0.5 秒自己退出（读到 EOF），再强杀
            std::thread::spawn(move || {
                for _ in 0..10 {
                    if matches!(child.lock().unwrap().try_wait(), Ok(Some(_))) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                let mut c = child.lock().unwrap();
                super::local::kill_tree(&mut c);
            });
        }
    }

    /// 进程已退出：从表里移除（不再接受写入）
    pub fn exited(&mut self, id: u64) {
        self.list.remove(&id);
    }

    /// 插件停止：先关 stdin 给 0.3 秒自行退出，剩下的强杀（同步完成，宿主随后退出）
    pub fn kill_all(&mut self) {
        let all: Vec<Proc> = self.list.drain().map(|(_, p)| p).collect();
        for p in &all {
            if let Some(tx) = &p.stdin {
                let _ = tx.send(None);
            }
        }
        for _ in 0..6 {
            if all.iter().all(|p| matches!(p.child.lock().unwrap().try_wait(), Ok(Some(_)))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        for p in all {
            let mut c = p.child.lock().unwrap();
            if !matches!(c.try_wait(), Ok(Some(_))) {
                super::local::kill_tree(&mut c);
            }
        }
    }

    pub fn alive(&self, id: u64) -> bool {
        self.list.contains_key(&id)
    }
}
