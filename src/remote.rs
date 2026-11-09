//! 通过 SSH 在远程电脑（macOS / Linux）上安装并启动 DroidBridge 服务版。
//!
//! 流程：探测系统 → 上传程序（有同平台缓存时直接上传二进制，否则上传源码在对方编译）
//! → 查找/安装 adb → 注册为登录启动服务（macOS launchd / Linux systemd --user）→ 等待就绪 → 加入列表。
//! 使用本机 `ssh` 命令与 ~/.ssh 中的密钥 / config，不保存任何密码。

use crate::{peers, AppState};
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

macro_rules! src {
    ($p:literal) => {
        ($p, include_bytes!(concat!("../", $p)) as &[u8])
    };
}

/// 在对方电脑上编译所需的全部源码（编译期嵌入）
const SOURCES: &[(&str, &[u8])] = &[
    src!("Cargo.toml"),
    src!("Cargo.lock"),
    src!("build.rs"),
    src!("src/lib.rs"),
    src!("src/main.rs"),
    src!("src/adb.rs"),
    src!("src/control.rs"),
    src!("src/discovery.rs"),
    src!("src/wireless.rs"),
    src!("src/i18n.rs"),
    src!("src/icon.rs"),
    src!("src/logcat.rs"),
    src!("src/media.rs"),
    src!("src/peers.rs"),
    src!("src/remote.rs"),
    src!("src/scrcpy.rs"),
    src!("src/util.rs"),
    src!("src/bin/desktop.rs"),
    src!("web/index.html"),
    src!("third_party/scrcpy-server"),
    src!("third_party/scrcpy-version.txt"),
];

/// 源码指纹：用于判断对方是否已运行相同版本、以及二进制缓存是否可复用
pub fn build_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for (p, b) in SOURCES {
            for x in p.as_bytes().iter().chain(b.iter()) {
                h ^= *x as u64;
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
        format!("{h:016x}")
    })
}

// ---------------------------------------------------------------- 任务状态

#[derive(Clone, Serialize, Default)]
pub struct Job {
    /// running / done / failed
    pub state: String,
    pub steps: Vec<String>,
    pub log: Vec<String>,
    /// 已被丢弃的旧日志行数
    pub log_base: usize,
    pub error: Option<String>,
    pub peer: Option<serde_json::Value>,
}

struct Ctx {
    st: Arc<AppState>,
    id: String,
}

impl Ctx {
    fn with(&self, f: impl FnOnce(&mut Job)) {
        if let Some(j) = self.st.jobs.lock().unwrap().get_mut(&self.id) {
            f(j);
        }
    }
    fn step(&self, s: &str) {
        tracing::info!("[install {}] {s}", self.id);
        self.with(|j| j.steps.push(s.to_string()));
    }
    fn log(&self, line: &str) {
        let line = line.trim_end();
        if line.is_empty() {
            return;
        }
        self.with(|j| {
            j.log.push(line.to_string());
            if j.log.len() > 400 {
                j.log.drain(..100);
                j.log_base += 100;
            }
        });
    }
}

#[derive(Deserialize)]
pub struct InstallReq {
    pub addr: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub ssh_port: Option<u16>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub token: Option<String>,
    /// 登录方式：agent（本机 SSH 密钥 / ssh-agent / ~/.ssh/config，默认）、password、key（指定密钥文件）
    #[serde(default)]
    pub auth: String,
    /// 密码，或密钥文件的口令；只在本次安装中使用，不保存
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub key_file: String,
    /// 密码登录成功后，把本机公钥加入对方 authorized_keys，以后免密码
    #[serde(default)]
    pub remember: bool,
}

#[derive(Clone, PartialEq)]
enum Auth {
    Agent,
    Password(String),
    Key { path: String, passphrase: String },
}

struct Target {
    host: String,
    user: Option<String>,
    port: Option<u16>,
    auth: Auth,
}

/// SSH_ASKPASS 入口：ssh 需要密码 / 口令时会以本程序为 askpass 运行。
/// 在 main 的最开始调用；返回 true 表示已作为 askpass 处理完毕，应直接退出。
pub fn askpass_hook() -> bool {
    if std::env::var("DROIDBRIDGE_ASKPASS").as_deref() != Ok("1") {
        return false;
    }
    let prompt: String = std::env::args().skip(1).collect::<Vec<_>>().join(" ").to_ascii_lowercase();
    // 主机密钥确认等是/否问题一律拒绝（已用 accept-new 处理首次连接）
    if prompt.contains("yes/no") || prompt.contains("(yes") {
        println!("no");
    } else {
        println!("{}", std::env::var("DROIDBRIDGE_ASKPASS_SECRET").unwrap_or_default());
    }
    true
}

/// 本机 ~/.ssh 下的私钥文件（供界面选择）
pub fn list_keys() -> Vec<String> {
    let Some(dir) = ssh_dir() else { return Vec::new() };
    let mut v: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_name()?.to_string_lossy().to_string();
            let skip = name.ends_with(".pub") || name.starts_with("known_hosts") || name.starts_with("config") || name == "authorized_keys";
            if skip || !p.is_file() {
                return None;
            }
            let head = std::fs::read(&p).ok()?;
            (head.starts_with(b"-----BEGIN") && String::from_utf8_lossy(&head[..head.len().min(80)]).contains("PRIVATE KEY")).then(|| p.to_string_lossy().to_string())
        })
        .collect();
    v.sort();
    v
}

fn ssh_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|h| std::path::PathBuf::from(h).join(".ssh"))
}

fn expand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        if let Some(h) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return std::path::PathBuf::from(h).join(rest).to_string_lossy().to_string();
        }
    }
    p.to_string()
}

fn valid_host(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:[]%".contains(c))
}

pub fn start(st: Arc<AppState>, req: InstallReq) -> Result<String, String> {
    let addr = peers::normalize_addr(&req.addr)?;
    let (host, _) = split_host_port(&addr);
    if !valid_host(&host) {
        return Err(tr!("地址格式不正确", "Invalid address"));
    }
    if let Some(u) = req.user.as_deref().filter(|u| !u.is_empty()) {
        if u.starts_with('-') || !u.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
            return Err(tr!("SSH 用户名格式不正确", "Invalid SSH user name"));
        }
    }
    let id = format!("j{:08x}", crate::util::random_scid());
    st.jobs.lock().unwrap().insert(id.clone(), Job { state: "running".into(), ..Default::default() });
    let ctx = Ctx { st, id: id.clone() };
    let en = crate::i18n::is_en();
    tokio::spawn(crate::i18n::REQUEST_EN.scope(en, async move {
        let r = install(&ctx, &req, &addr).await;
        ctx.with(|j| match r {
            Ok(p) => {
                j.state = "done".into();
                j.peer = Some(p);
            }
            Err(e) => {
                j.state = "failed".into();
                j.error = Some(e);
            }
        });
    }));
    Ok(id)
}

pub fn get(st: &AppState, id: &str, since: usize) -> Option<serde_json::Value> {
    let jobs = st.jobs.lock().unwrap();
    let j = jobs.get(id)?;
    let skip = since.saturating_sub(j.log_base).min(j.log.len());
    Some(serde_json::json!({
        "state": j.state,
        "steps": j.steps,
        "log": &j.log[skip..],
        "next": j.log_base + j.log.len(),
        "error": j.error,
        "peer": j.peer,
    }))
}

fn split_host_port(addr: &str) -> (String, u16) {
    let (h, p) = addr.rsplit_once(':').unwrap_or((addr, "8730"));
    (h.trim_start_matches('[').trim_end_matches(']').to_string(), p.parse().unwrap_or(peers::DEFAULT_PORT))
}

// ---------------------------------------------------------------- 安装流程

async fn install(ctx: &Ctx, req: &InstallReq, addr: &str) -> Result<serde_json::Value, String> {
    let (host, port) = split_host_port(addr);
    let auth = match req.auth.as_str() {
        "password" => {
            if req.password.is_empty() {
                return Err(tr!("请填写 SSH 密码", "Please enter the SSH password"));
            }
            Auth::Password(req.password.clone())
        }
        "key" => {
            let path = expand_home(req.key_file.trim());
            if path.is_empty() || !std::path::Path::new(&path).is_file() {
                return Err(tr!("找不到密钥文件：{path}", "Key file not found: {path}"));
            }
            Auth::Key { path, passphrase: req.password.clone() }
        }
        _ => Auth::Agent,
    };
    let t = Target {
        host: host.clone(),
        user: req.user.clone().filter(|u| !u.is_empty()),
        port: req.ssh_port,
        auth,
    };
    let token = req.token.clone().filter(|t| !t.is_empty());

    ctx.step(&tr!("连接 SSH", "Connecting over SSH"));
    let probe = ssh_run(
        ctx,
        &t,
        "uname -s; uname -m; echo \"$HOME\"; (scutil --get ComputerName 2>/dev/null || hostname) | head -1",
        None,
        Duration::from_secs(30),
        false,
    )
    .await?;
    let lines: Vec<&str> = probe.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.len() < 3 {
        return Err(tr!("无法识别对方系统：{probe}", "Cannot detect the remote system: {probe}"));
    }
    let (os, arch, home) = (lines[0], lines[1], lines[2]);
    let remote_name = lines.get(3).copied().unwrap_or(&host).to_string();
    if os != "Darwin" && os != "Linux" {
        return Err(tr!("暂只支持通过 SSH 安装到 macOS / Linux（对方系统：{os}）。Windows 电脑请直接运行安装包", "SSH install supports macOS / Linux only (remote: {os}). On Windows, run the installer package instead"));
    }
    ctx.log(&format!("{remote_name}：{os} {arch}"));
    if req.remember && matches!(t.auth, Auth::Password(_)) {
        match remember_key(ctx, &t).await {
            Ok(m) => ctx.log(&m),
            Err(e) => ctx.log(&tr!("未能保存免密登录：{e}", "Could not set up key login: {e}")),
        }
    }
    let dir = format!("{home}/.droidbridge");

    // 已在运行相同版本时跳过安装
    let probe_peer = peers::Peer { id: String::new(), name: String::new(), addr: addr.to_string(), token: token.clone() };
    let same = match peers::get_json(&probe_peer, "/api/info").await {
        Ok(v) => v.get("build").and_then(|b| b.as_str()) == Some(build_id()),
        Err(_) => false,
    };

    if same && token.is_none() {
        ctx.log(&tr!("对方已在运行相同版本，跳过安装", "Same version already running; skipping install"));
    } else {
        let cache = ctx
            .st
            .cfg
            .data_dir
            .join("remote-bin")
            .join(format!("{}-{}-{}", os.to_lowercase(), arch, build_id()))
            .join("droidbridge");
        if let Ok(bin) = tokio::fs::read(&cache).await {
            ctx.step(&tr!("上传程序", "Uploading program"));
            let kb = bin.len() / 1024;
            ctx.log(&tr!("使用已编译的 {os} {arch} 程序（{kb} KB）", "Using cached {os} {arch} build ({kb} KB)"));
            ssh_run(
                ctx,
                &t,
                "set -e; D=\"$HOME/.droidbridge\"; mkdir -p \"$D/bin\"; cat > \"$D/bin/droidbridge.new\"; chmod +x \"$D/bin/droidbridge.new\"; mv -f \"$D/bin/droidbridge.new\" \"$D/bin/droidbridge\"",
                Some(bin),
                Duration::from_secs(300),
                true,
            )
            .await?;
        } else {
            ctx.step(&tr!("上传源码", "Uploading source"));
            ssh_run(ctx, &t, "sh -s", Some(upload_script().into_bytes()), Duration::from_secs(300), true).await?;
            ctx.step(&tr!("在对方电脑上编译（首次约 2–5 分钟）", "Building on the remote computer (2–5 min the first time)"));
            ssh_run(ctx, &t, "sh -s", Some(BUILD_SCRIPT.as_bytes().to_vec()), Duration::from_secs(40 * 60), true)
                .await
                .map_err(|e| tr!("编译失败：{e}", "Build failed: {e}"))?;
            // 取回二进制缓存，下次安装到同平台电脑时无需再编译
            match ssh_bytes(&t, "cat \"$HOME/.droidbridge/bin/droidbridge\"").await {
                Ok(b) if b.len() > 100_000 => {
                    if let Some(p) = cache.parent() {
                        let _ = tokio::fs::create_dir_all(p).await;
                    }
                    let _ = tokio::fs::write(&cache, b).await;
                }
                _ => {}
            }
        }
    }

    ctx.step(&tr!("配置 adb", "Setting up adb"));
    let out = ssh_run(ctx, &t, "sh -s", Some(ADB_SCRIPT.as_bytes().to_vec()), Duration::from_secs(600), true).await?;
    let adb = out
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("ADB="))
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if adb.is_empty() {
        ctx.log(&tr!("警告：对方电脑没有找到 adb，服务会启动，但无法列出设备。请安装 Android platform-tools", "Warning: adb not found on the remote computer; the server will start but cannot list devices. Please install Android platform-tools"));
    } else {
        ctx.log(&format!("adb：{adb}"));
    }

    ctx.step(&tr!("启动服务", "Starting service"));
    let name = if req.name.trim().is_empty() { remote_name.clone() } else { req.name.trim().to_string() };
    let mut args = vec![
        format!("{dir}/bin/droidbridge"),
        "--data".into(),
        format!("{dir}/data"),
        "--port".into(),
        port.to_string(),
        "--name".into(),
        name.clone(),
    ];
    if !adb.is_empty() {
        args.push("--adb".into());
        args.push(adb);
    }
    if let Some(tk) = &token {
        args.push("--token".into());
        args.push(tk.clone());
    }
    let script = if os == "Darwin" { mac_start_script(&dir, &args) } else { linux_start_script(&dir, &args) };
    ssh_run(ctx, &t, "sh -s", Some(script.into_bytes()), Duration::from_secs(60), true).await?;

    ctx.step(&tr!("等待服务就绪", "Waiting for service"));
    let mut ready = false;
    for _ in 0..30 {
        if let Ok(v) = peers::get_json(&probe_peer, "/api/info").await {
            if v.get("build").and_then(|b| b.as_str()) == Some(build_id()) {
                ready = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    if !ready {
        return Err(tr!(
            "服务已启动但无法从本机访问 {addr}。请检查对方防火墙是否允许 droidbridge 接收连接（macOS：系统设置 → 网络 → 防火墙）",
            "The service started but {addr} is not reachable from here. Check that the remote firewall allows incoming connections for droidbridge"
        ));
    }

    ctx.step(&tr!("添加到列表", "Adding to list"));
    crate::save_peer(&ctx.st, None, addr, &req.name, token).await
}

// ---------------------------------------------------------------- SSH

/// 把本机公钥加入对方 ~/.ssh/authorized_keys；本机没有密钥时用 ssh-keygen 生成 ed25519 密钥
async fn remember_key(ctx: &Ctx, t: &Target) -> Result<String, String> {
    let dir = ssh_dir().ok_or("no home directory")?;
    let find = || ["id_ed25519.pub", "id_ecdsa.pub", "id_rsa.pub"].iter().map(|n| dir.join(n)).find(|p| p.is_file());
    let pubfile = match find() {
        Some(p) => p,
        None => {
            let _ = std::fs::create_dir_all(&dir);
            let key = dir.join("id_ed25519");
            let mut c = Command::new("ssh-keygen");
            c.args(["-q", "-t", "ed25519", "-N", "", "-C", "droidbridge", "-f"]).arg(&key).stdin(Stdio::null());
            #[cfg(windows)]
            c.creation_flags(0x0800_0000);
            let st = c.status().await.map_err(|e| e.to_string())?;
            if !st.success() {
                return Err(format!("ssh-keygen: {st}"));
            }
            find().ok_or("ssh-keygen produced no key")?
        }
    };
    let key = std::fs::read_to_string(&pubfile).map_err(|e| e.to_string())?.trim().to_string();
    if !key.starts_with("ssh-") && !key.starts_with("ecdsa-") {
        return Err("invalid public key".into());
    }
    let script = "umask 077; mkdir -p ~/.ssh; touch ~/.ssh/authorized_keys; read -r K; grep -qxF \"$K\" ~/.ssh/authorized_keys || echo \"$K\" >> ~/.ssh/authorized_keys";
    ssh_run(ctx, t, script, Some(format!("{key}
").into_bytes()), Duration::from_secs(30), false).await?;
    let name = pubfile.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Ok(tr!("已把本机公钥（{name}）加入对方，以后可直接用本机密钥登录", "Added this computer's public key ({name}); later logins can use the key"))
}

fn ssh_cmd(t: &Target, remote: &str) -> Command {
    let mut c = Command::new("ssh");
    let secret = match &t.auth {
        Auth::Agent => None,
        Auth::Password(p) => {
            // Windows 版 OpenSSH 的 keyboard-interactive 不走 askpass（会卡在等待控制台输入），只用 password
            let methods = if cfg!(windows) { "PreferredAuthentications=password" } else { "PreferredAuthentications=password,keyboard-interactive" };
            c.args(["-o", methods, "-o", "PubkeyAuthentication=no"]);
            Some(p.clone())
        }
        Auth::Key { path, passphrase } => {
            c.args(["-i", path, "-o", "IdentitiesOnly=yes", "-o", "PreferredAuthentications=publickey"]);
            Some(passphrase.clone())
        }
    };
    match secret {
        // 由本程序充当 askpass 提供密码 / 口令（不经过终端，也不写入磁盘）
        Some(sec) => {
            c.args(["-o", "BatchMode=no", "-o", "NumberOfPasswordPrompts=1"]);
            if let Ok(exe) = std::env::current_exe() {
                c.env("SSH_ASKPASS", exe);
            }
            c.env("SSH_ASKPASS_REQUIRE", "force").env("DISPLAY", ":0").env("DROIDBRIDGE_ASKPASS", "1").env("DROIDBRIDGE_ASKPASS_SECRET", sec);
        }
        None => {
            c.args(["-o", "BatchMode=yes"]);
        }
    }
    c.args([
        "-o", "StrictHostKeyChecking=accept-new",
        "-o", "ConnectTimeout=8",
        "-o", "ServerAliveInterval=15",
        "-o", "ServerAliveCountMax=4",
    ]);
    if let Some(p) = t.port {
        c.arg("-p").arg(p.to_string());
    }
    if let Some(u) = &t.user {
        c.arg("-l").arg(u);
    }
    c.arg(&t.host).arg(remote);
    c.kill_on_drop(true);
    #[cfg(windows)]
    {
        c.creation_flags(0x0800_0000);
    }
    c
}

fn explain_ssh_error(t: &Target, stderr: &str) -> Option<String> {
    let s = stderr.to_ascii_lowercase();
    if s.contains("permission denied") && matches!(t.auth, Auth::Password(_)) {
        Some(tr!("SSH 登录失败：用户名或密码不正确（对方也可能禁用了密码登录）", "SSH login failed: wrong user name or password (password login may also be disabled on the remote computer)"))
    } else if s.contains("permission denied") && matches!(t.auth, Auth::Key { .. }) {
        Some(tr!("SSH 登录失败：对方不接受这个密钥，或密钥口令不正确", "SSH login failed: the key was not accepted, or its passphrase is wrong"))
    } else if s.contains("permission denied") {
        Some(tr!("SSH 认证失败：本机 ~/.ssh 中的密钥未被对方接受。请把本机公钥（~/.ssh/id_rsa.pub 等）加入对方的 ~/.ssh/authorized_keys，或填写正确的 SSH 用户名", "SSH authentication failed: no key in ~/.ssh was accepted. Add your public key (~/.ssh/id_rsa.pub etc.) to the remote ~/.ssh/authorized_keys, or set the right SSH user"))
    } else if s.contains("connection refused") {
        Some(tr!("对方没有开启 SSH。macOS：系统设置 → 通用 → 共享 → 打开“远程登录”；Linux：安装并启动 openssh-server", "SSH is not enabled on the remote computer. macOS: System Settings → General → Sharing → Remote Login; Linux: install and start openssh-server"))
    } else if s.contains("timed out") || s.contains("no route to host") || s.contains("could not resolve") {
        Some(tr!("无法通过 SSH 连接对方，请确认地址正确且在同一局域网", "Cannot reach the remote computer over SSH; check the address and network"))
    } else if s.contains("host key verification failed") || s.contains("remote host identification has changed") {
        Some(tr!("对方的 SSH 主机密钥与本机记录不一致（~/.ssh/known_hosts），请确认后删除旧记录再试", "The remote SSH host key does not match ~/.ssh/known_hosts; verify it and remove the old entry"))
    } else {
        None
    }
}

/// 执行远程命令；stdout/stderr 逐行写入任务日志（log_output=false 时只收集不显示）
async fn ssh_run(
    ctx: &Ctx,
    t: &Target,
    remote: &str,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
    log_output: bool,
) -> Result<String, String> {
    let mut child = ssh_cmd(t, remote)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| tr!("无法执行 ssh（本机需要 OpenSSH 客户端）：{e}", "Cannot run ssh (an OpenSSH client is required): {e}"))?;

    if let (Some(data), Some(mut w)) = (stdin, child.stdin.take()) {
        tokio::spawn(async move {
            let _ = w.write_all(&data).await;
            let _ = w.shutdown().await;
        });
    }
    let stderr = child.stderr.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let errbuf = Arc::new(std::sync::Mutex::new(String::new()));

    let run = async {
        let eb = errbuf.clone();
        let st = ctx.st.clone();
        let id = ctx.id.clone();
        let err_task = tokio::spawn(async move {
            let ctx2 = Ctx { st, id };
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if log_output {
                    ctx2.log(&l);
                }
                let mut e = eb.lock().unwrap();
                e.push_str(&l);
                e.push('\n');
            }
        });
        let mut out = String::new();
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            if log_output {
                ctx.log(&l);
            }
            out.push_str(&l);
            out.push('\n');
        }
        let _ = err_task.await;
        let status = child.wait().await;
        (out, status)
    };
    let (out, status) = tokio::time::timeout(timeout, run).await.map_err(|_| tr!("远程命令超时", "Remote command timed out"))?;
    let status = status.map_err(|e| e.to_string())?;
    if status.success() {
        return Ok(out);
    }
    let err = errbuf.lock().unwrap().clone();
    if status.code() == Some(255) {
        if let Some(msg) = explain_ssh_error(t, &err) {
            return Err(msg);
        }
    }
    let last = err.lines().chain(out.lines()).filter(|l| !l.trim().is_empty()).last().unwrap_or("").to_string();
    let tail = if last.is_empty() { String::new() } else { format!(": {last}") };
    Err(tr!("远程命令失败（{status}）{tail}", "Remote command failed ({status}){tail}"))
}

async fn ssh_bytes(t: &Target, remote: &str) -> Result<Vec<u8>, String> {
    let mut child = ssh_cmd(t, remote)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut buf).await.map_err(|e| e.to_string())?;
    let st = child.wait().await.map_err(|e| e.to_string())?;
    if st.success() { Ok(buf) } else { Err(st.to_string()) }
}

// ---------------------------------------------------------------- 远程脚本

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len() * 4 / 3 + data.len() / 57 + 4);
    for (i, c) in data.chunks(3).enumerate() {
        if i > 0 && i % 19 == 0 {
            out.push('\n');
        }
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out.push('\n');
    out
}

fn upload_script() -> String {
    let mut s = String::from(
        "set -e\nD=\"$HOME/.droidbridge/src\"\nrm -rf \"$D\"\nmkdir -p \"$D/src/bin\" \"$D/web\" \"$D/third_party\"\n\
         b64() { if base64 -d </dev/null >/dev/null 2>&1; then base64 -d; else base64 -D; fi; }\n",
    );
    for (path, data) in SOURCES {
        s.push_str(&format!("b64 > \"$D/{path}\" <<'__DB_EOF__'\n"));
        s.push_str(&base64(data));
        s.push_str("__DB_EOF__\n");
    }
    s.push_str(&format!("echo \"Uploaded {} files\"\n", SOURCES.len()));
    s
}

const BUILD_SCRIPT: &str = r#"set -e
export PATH="$HOME/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:$PATH"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
D="$HOME/.droidbridge"
if [ "$(uname -s)" = Darwin ] && ! xcrun -f cc >/dev/null 2>&1; then
  echo "Xcode command line tools are missing; run xcode-select --install on that Mac and retry" >&2; exit 3
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust not found; installing rustup for the current user…"
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
  . "$HOME/.cargo/env"
fi
v=$(rustc --version | awk '{print $2}' | cut -d. -f2)
if [ "${v:-0}" -lt 85 ]; then echo "Rust is too old ($(rustc --version)); updating…"; rustup update stable --no-self-update; fi
echo "$(rustc --version)"
cd "$D/src"
CARGO_TARGET_DIR="$D/target" cargo build --release --bin droidbridge --color never 2>&1
mkdir -p "$D/bin"
cp "$D/target/release/droidbridge" "$D/bin/droidbridge.new"
chmod +x "$D/bin/droidbridge.new"
mv -f "$D/bin/droidbridge.new" "$D/bin/droidbridge"
echo "Build finished"
"#;

const ADB_SCRIPT: &str = r#"export PATH="/usr/local/bin:/opt/homebrew/bin:$PATH"
for p in "$ANDROID_HOME/platform-tools/adb" "$ANDROID_SDK_ROOT/platform-tools/adb" \
         "$HOME/Library/Android/sdk/platform-tools/adb" "$HOME/Android/Sdk/platform-tools/adb" \
         /usr/local/bin/adb /opt/homebrew/bin/adb /usr/bin/adb "$(command -v adb 2>/dev/null)"; do
  if [ -n "$p" ] && [ -x "$p" ]; then echo "ADB=$p"; exit 0; fi
done
if command -v brew >/dev/null 2>&1; then
  echo "adb not found; installing android-platform-tools with Homebrew…"
  brew install --cask android-platform-tools >/dev/null 2>&1 || brew install android-platform-tools >/dev/null 2>&1 || true
  p=$(command -v adb 2>/dev/null)
  if [ -n "$p" ]; then echo "ADB=$p"; exit 0; fi
fi
echo "ADB="
"#;

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

const LABEL: &str = "com.droidbridge.server";

fn mac_start_script(dir: &str, args: &[String]) -> String {
    let arr: String = args.iter().map(|a| format!("    <string>{}</string>\n", xml(a))).collect();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
{arr}  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{d}/server.log</string>
  <key>StandardErrorPath</key><string>{d}/server.log</string>
  <key>EnvironmentVariables</key>
  <dict><key>PATH</key><string>/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin</string></dict>
</dict>
</plist>
"#,
        d = xml(dir)
    );
    let cmdline: Vec<String> = args.iter().map(|a| sh_quote(a)).collect();
    format!(
        r#"D="$HOME/.droidbridge"; L="$HOME/Library/LaunchAgents"; P="$L/{LABEL}.plist"
mkdir -p "$L" "$D/data"
U=$(id -u)
launchctl bootout "gui/$U/{LABEL}" 2>/dev/null || true
pkill -f "$D/bin/droidbridge" 2>/dev/null || true
sleep 1
cat > "$P" <<'__DB_PLIST__'
{plist}__DB_PLIST__
if launchctl bootstrap "gui/$U" "$P" 2>/dev/null; then
  echo "Registered launchd agent {LABEL}; it starts automatically at login"
else
  nohup {cmd} > "$D/server.log" 2>&1 &
  echo "No GUI login session; started in background for now (reinstall after reboot)"
fi
"#,
        cmd = cmdline.join(" ")
    )
}

fn linux_start_script(_dir: &str, args: &[String]) -> String {
    let exec: Vec<String> = args.iter().map(|a| format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\""))).collect();
    let cmdline: Vec<String> = args.iter().map(|a| sh_quote(a)).collect();
    format!(
        r#"D="$HOME/.droidbridge"; mkdir -p "$D/data"
pkill -f "$D/bin/droidbridge" 2>/dev/null || true
if command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
  mkdir -p "$HOME/.config/systemd/user"
  cat > "$HOME/.config/systemd/user/droidbridge.service" <<'__DB_UNIT__'
[Unit]
Description=DroidBridge
After=network-online.target

[Service]
ExecStart={exec}
Restart=always
RestartSec=3

[Install]
WantedBy=default.target
__DB_UNIT__
  systemctl --user daemon-reload
  systemctl --user enable droidbridge.service >/dev/null 2>&1 || true
  systemctl --user restart droidbridge.service
  echo "Registered systemd user service droidbridge.service"
else
  nohup setsid {cmd} > "$D/server.log" 2>&1 < /dev/null &
  echo "Started in background"
fi
"#,
        exec = exec.join(" "),
        cmd = cmdline.join(" ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64() {
        assert_eq!(base64(b"Man"), "TWFu\n");
        assert_eq!(base64(b"Ma"), "TWE=\n");
        assert_eq!(base64(b"M"), "TQ==\n");
        assert!(base64(&[0u8; 200]).lines().all(|l| l.len() <= 76));
    }

    #[test]
    fn hostport() {
        assert_eq!(split_host_port("10.0.0.2:8730"), ("10.0.0.2".to_string(), 8730));
        assert_eq!(split_host_port("[fe80::1]:9000"), ("fe80::1".to_string(), 9000));
        assert!(!valid_host("-oProxyCommand=x"));
    }
}
