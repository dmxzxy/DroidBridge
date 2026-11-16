//! 插件宿主子进程：启动、NDJSON 收发、ping 看门狗与崩溃重启（SPEC 第 3、4 节）。

use super::*;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// 给宿主进程的控制命令
#[derive(Debug, Clone, Copy)]
pub(super) enum HostCmd {
    Stop,
    Reload,
}

/// 一次宿主进程的退出原因
pub(super) enum Exit {
    Stop,
    Reload,
    Crash(String),
}

/// 宿主进程句柄：请求/回复、控制命令、看门狗时间戳
pub(crate) struct Host {
    /// 写入宿主 stdin 的消息队列
    pub(super) out: mpsc::Sender<Value>,
    /// 等待宿主回复的请求
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next: AtomicU64,
    ping_seq: AtomicU64,
    cmd: mpsc::UnboundedSender<HostCmd>,
    stop: AtomicBool,
    reload: AtomicBool,
    ready: AtomicBool,
    last_pong: Mutex<Instant>,
}

impl Host {
    fn new(out: mpsc::Sender<Value>, cmd: mpsc::UnboundedSender<HostCmd>) -> Self {
        Self {
            out,
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            ping_seq: AtomicU64::new(1),
            cmd,
            stop: AtomicBool::new(false),
            reload: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            last_pong: Mutex::new(Instant::now()),
        }
    }

    pub(super) fn control(&self, cmd: HostCmd) {
        match cmd {
            HostCmd::Stop => self.stop.store(true, Ordering::SeqCst),
            HostCmd::Reload => self.reload.store(true, Ordering::SeqCst),
        }
        let _ = self.cmd.send(cmd);
    }

    /// 向宿主发起一个请求（`req`），等待 `res` 回复
    pub(super) async fn request(&self, m: &str, p: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if self.out.send(json!({"t": "req", "id": id, "m": m, "p": p})).await.is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err(tr!("插件没有运行", "The plugin is not running"));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(tr!("插件已停止", "The plugin stopped")),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(tr!("插件没有响应（{m}）", "The plugin did not respond ({m})"))
            }
        }
    }

    fn resolve(&self, id: u64, r: Result<Value, String>) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(r);
        }
    }

    /// 所有等待中的请求都以“插件已停止”结束
    fn drain(&self) {
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Err(tr!("插件已停止", "The plugin stopped")));
        }
    }
}

/// 管理一个插件的宿主进程：崩溃按 1s/2s/4s 重启，60 秒内 3 次则 failed
pub(super) async fn supervise(app: Arc<AppState>, ph: Arc<Plugin>) {
    loop {
        if !ph.enabled() || app.cfg.no_plugins {
            let state = if ph.enabled() { PluginState::Stopped } else { PluginState::Disabled };
            app.plugins.set_state(&ph, state, None);
            ph.spawned.store(false, Ordering::SeqCst);
            return;
        }
        app.plugins.set_state(&ph, PluginState::Starting, None);
        match run_one(&app, &ph).await {
            Exit::Stop => {
                // 停用的插件显示 disabled；权限变多时显示 needs_grant，其余为 stopped
                let man = ph.manifest();
                let state = if !ph.enabled() {
                    PluginState::Disabled
                } else if manifest::needs_grant(&man.permissions, &ph.granted()) {
                    PluginState::NeedsGrant
                } else {
                    PluginState::Stopped
                };
                app.plugins.set_state(&ph, state, None);
                ph.spawned.store(false, Ordering::SeqCst);
                return;
            }
            Exit::Reload => continue,
            Exit::Crash(reason) => {
                cleanup(&app, &ph);
                app.plugins.set_state(&ph, PluginState::Crashed, Some(reason.clone()));
                app.plugins.console(&ph, "error", &tr!("插件宿主退出：{reason}", "The plugin host exited: {reason}"), None);
                let n = {
                    let mut c = ph.crashes.lock().unwrap();
                    c.retain(|t| t.elapsed() < Duration::from_secs(60));
                    c.push(Instant::now());
                    c.len()
                };
                if n >= 3 {
                    app.plugins.set_state(&ph, PluginState::Failed, Some(reason));
                    let name = ph.manifest().name.clone();
                    app.plugins.toast(&ph.id, tr!("插件 {name} 反复崩溃，已停止", "Plugin {name} keeps crashing and was stopped"), "error");
                    ph.spawned.store(false, Ordering::SeqCst);
                    return;
                }
                tokio::time::sleep(Duration::from_secs(1 << (n - 1))).await;
            }
        }
    }
}

/// 宿主退出/重启前清理：对话框、logwatch、adb 转发、界面贡献
fn cleanup(app: &Arc<AppState>, ph: &Arc<Plugin>) {
    app.plugins.cancel_dialogs(&ph.id);
    calls::stop_watches(app, ph);
    let forwards = std::mem::take(&mut *ph.forwards.lock().unwrap());
    if !forwards.is_empty() {
        let (adb, plugin) = (app.cfg.adb.clone(), ph.id.clone());
        tokio::spawn(async move {
            for f in forwards {
                calls::remove_forward(&adb, &plugin, &f).await;
            }
        });
    }
    ph.contributions.lock().unwrap().clear();
    app.plugins.emit_changed();
}

/// 启动一次宿主进程并运行到它退出
async fn run_one(app: &Arc<AppState>, ph: &Arc<Plugin>) -> Exit {
    let man = ph.manifest();
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return Exit::Crash(format!("current_exe: {e}")),
    };
    let data_dir = app.plugins.root().join(".data").join(&ph.id);
    let _ = tokio::fs::create_dir_all(&data_dir).await;

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("--plugin-host")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Exit::Crash(tr!("无法启动插件宿主：{e}", "Cannot start the plugin host: {e}")),
    };
    let pid = child.id().unwrap_or(0);
    tracing::debug!("plugin {}: host started (pid {pid})", ph.id);
    let (Some(mut stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill().await;
        return Exit::Crash(tr!("无法连接插件宿主", "Cannot talk to the plugin host"));
    };

    let (out_tx, mut out_rx) = mpsc::channel::<Value>(256);
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<HostCmd>();
    let host = Arc::new(Host::new(out_tx.clone(), cmd_tx));
    ph.set_host(Some(host.clone()));

    // 第一条消息必须是 init
    let init = json!({
        "t": "init",
        "plugin": {
            "id": ph.id,
            "name": man.name,
            "version": man.version,
            "dir": ph.dir.to_string_lossy(),
            "dataDir": data_dir.to_string_lossy(),
            "main": man.main,
        },
        "granted": ph.granted(),
        "settings": ph.settings(),
        "lang": crate::i18n::code(),
        "node": app.cfg.name,
        "adb": app.cfg.adb.to_string_lossy(),
    });
    let _ = out_tx.send(init).await;

    // stdin 写线程
    let writer = tokio::spawn(async move {
        while let Some(v) = out_rx.recv().await {
            let mut line = v.to_string();
            line.push('\n');
            if stdin.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if stdin.flush().await.is_err() {
                break;
            }
        }
    });

    // 宿主的 stderr 记为插件控制台的 error 日志
    let stderr_task = {
        let (app, ph) = (app.clone(), ph.clone());
        tokio::spawn(async move {
            let mut rd = BufReader::new(stderr);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match rd.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                        if !line.is_empty() {
                            app.plugins.console(&ph, "error", &line, None);
                        }
                    }
                }
            }
        })
    };

    let mut rd = BufReader::new(stdout);
    // 每 3 秒 ping 一次；超过 15 秒没有任何输出才算卡死（间隔与上限相同会因计时抖动误判）
    let mut ping = tokio::time::interval(Duration::from_secs(3));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await;
    let mut buf: Vec<u8> = Vec::new();
    let exit = loop {
        tokio::select! {
            r = rd.read_until(b'\n', &mut buf) => match r {
                Ok(0) | Err(_) => {
                    if host.stop.load(Ordering::SeqCst) || !ph.enabled() {
                        break Exit::Stop;
                    }
                    if host.reload.load(Ordering::SeqCst) {
                        break Exit::Reload;
                    }
                    let code = child.try_wait().ok().flatten().and_then(|s| s.code()).unwrap_or(-1);
                    break Exit::Crash(tr!("插件宿主意外退出（{code}）", "The plugin host exited unexpectedly ({code})"));
                }
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).trim().to_string();
                    buf.clear();
                    if line.is_empty() {
                        continue;
                    }
                    *host.last_pong.lock().unwrap() = Instant::now();
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        handle_msg(app, ph, &host, v);
                    }
                }
            },
            c = cmd_rx.recv() => match c {
                Some(HostCmd::Stop) | None => break Exit::Stop,
                Some(HostCmd::Reload) => break Exit::Reload,
            },
            _ = ping.tick() => {
                let limit = if host.ready.load(Ordering::SeqCst) { 15 } else { 60 };
                let silent = host.last_pong.lock().unwrap().elapsed();
                if silent > Duration::from_secs(limit) {
                    break Exit::Crash(tr!("插件 {limit} 秒没有响应", "The plugin did not respond for {limit} s"));
                }
                let id = (1u64 << 63) | host.ping_seq.fetch_add(1, Ordering::Relaxed);
                let _ = out_tx.send(json!({"t": "req", "id": id, "m": "ping", "p": {}})).await;
            }
        }
    };

    // 收尾：先断开与宿主的联系，再杀掉进程
    ph.set_host(None);
    host.drain();
    drop(out_tx);
    drop(host);
    let _ = tokio::time::timeout(Duration::from_secs(3), writer).await;
    let _ = child.kill().await;
    let _ = child.wait().await;
    let _ = tokio::time::timeout(Duration::from_secs(1), stderr_task).await;
    if !matches!(exit, Exit::Reload) {
        cleanup(app, ph);
    }
    exit
}

/// 处理宿主发来的一条消息
fn handle_msg(app: &Arc<AppState>, ph: &Arc<Plugin>, host: &Arc<Host>, v: Value) {
    match v.get("t").and_then(|x| x.as_str()).unwrap_or("") {
        "ready" => {
            host.ready.store(true, Ordering::SeqCst);
            app.plugins.set_state(ph, PluginState::Running, None);
            let (app, ph) = (app.clone(), ph.clone());
            tokio::spawn(async move { backfill_connected(&app, &ph).await });
        }
        "res" => {
            let id = v.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
            let r = match v.get("err") {
                Some(Value::String(e)) => Err(e.clone()),
                Some(other) => Err(other.to_string()),
                None => Ok(v.get("ok").cloned().unwrap_or(Value::Null)),
            };
            host.resolve(id, r);
        }
        "call" => {
            let id = v.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
            let m = v.get("m").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let p = v.get("p").cloned().unwrap_or_else(|| json!({}));
            calls::dispatch(app.clone(), ph.clone(), id, m, p);
        }
        "log" => {
            let level = v.get("level").and_then(|x| x.as_str()).unwrap_or("info");
            let msg = v.get("msg").and_then(|x| x.as_str()).unwrap_or("");
            let src = v.get("src").and_then(|x| x.as_str());
            app.plugins.console(ph, level, msg, src);
        }
        _ => {}
    }
}

/// 插件启动完成后，对本机与远程电脑上的在线设备补发一次 device.connected
async fn backfill_connected(app: &Arc<AppState>, ph: &Arc<Plugin>) {
    if !ph.is_subscribed("device.connected") {
        return;
    }
    let devs: Vec<PluginDev> = app.plugins.refresh_devices(app).await.into_iter().filter(|d| d.state == "device").collect();
    let subs = vec![ph.clone()];
    for dev in devs {
        if !app.plugins.device_allowed(app, ph, &dev.id).await {
            continue;
        }
        fire(app, &subs, "device.connected", vec![dev.json()]).await;
    }
}
