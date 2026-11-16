//! 宿主本地实现（不经过主进程）：文件、存储、运行本机程序、HTTP、剪贴板等。
//!
//! 这里的函数都是阻塞的，由驱动线程放到后台线程里执行后再恢复协程。

use super::state::Ctl;
use serde_json::{Map, Value as J, json};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

const PLUGIN_SCHEME: &str = "plugin://";

/// 把插件给的路径解析成绝对路径，并做权限检查。
pub fn resolve(ctl: &Ctl, path: &str, write: bool) -> Result<PathBuf, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("路径为空".into());
    }
    // plugin:// 只读
    if let Some(rest) = path.strip_prefix(PLUGIN_SCHEME) {
        if write {
            return Err("插件目录是只读的".into());
        }
        check_relative(rest)?;
        return Ok(ctl.info.dir.join(rest.replace('\\', "/")));
    }
    let p = Path::new(path);
    // 绝对路径也不能带 ..，否则 `<dataDir>/../../x` 会被当成插件自己的目录
    check_relative(path)?;
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        ctl.info.data_dir.join(p)
    };
    let own = ctl.is_own_path(&full);
    // 用户通过对话框上传的文件、截图等临时文件（<plugins>/.tmp）可以读
    let tmp = ctl.info.dir.parent().is_some_and(|root| full.starts_with(root.join(".tmp")));
    if write {
        // 插件目录始终只读
        if full.starts_with(&ctl.info.dir) && !full.starts_with(&ctl.info.data_dir) {
            return Err("插件目录是只读的".into());
        }
        if !own && !ctl.has_perm("fs") {
            return Err("写入该路径需要 fs 权限".into());
        }
    } else if !own && !tmp && !ctl.has_perm("fs.read") {
        return Err("读取该路径需要 fs.read 权限".into());
    }
    Ok(full)
}

/// 拒绝相对路径里的 `..`
fn check_relative(p: &str) -> Result<(), String> {
    for c in Path::new(p).components() {
        if matches!(c, Component::ParentDir) {
            return Err("路径里不能包含 ..".into());
        }
    }
    Ok(())
}

pub fn fs_read_bytes(ctl: &Ctl, path: &str) -> Result<Vec<u8>, String> {
    let full = resolve(ctl, path, false)?;
    std::fs::read(&full).map_err(|e| format!("读取失败 {}: {e}", full.display()))
}


pub fn fs_write(ctl: &Ctl, path: &str, data: &[u8], append: bool) -> Result<(), String> {
    let full = resolve(ctl, path, true)?;
    if let Some(parent) = full.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let r = if append {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&full)
            .and_then(|mut f| f.write_all(data))
    } else {
        std::fs::write(&full, data)
    };
    r.map_err(|e| format!("写入失败 {}: {e}", full.display()))
}

pub fn fs_list(ctl: &Ctl, dir: &str) -> Result<J, String> {
    let full = resolve(ctl, dir, false)?;
    let rd = std::fs::read_dir(&full).map_err(|e| format!("读取目录失败 {}: {e}", full.display()))?;
    let mut items: Vec<(bool, String, u64)> = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().to_string();
        let meta = ent.metadata().ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        items.push((is_dir, name, size));
    }
    items.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    Ok(J::Array(
        items
            .into_iter()
            .map(|(dir, name, size)| json!({"name": name, "dir": dir, "size": size}))
            .collect(),
    ))
}

pub fn fs_exists(ctl: &Ctl, path: &str) -> bool {
    resolve(ctl, path, false).map(|p| p.exists()).unwrap_or(false)
}

pub fn fs_stat(ctl: &Ctl, path: &str) -> Result<J, String> {
    let full = resolve(ctl, path, false)?;
    let meta = std::fs::metadata(&full).map_err(|e| format!("stat 失败 {}: {e}", full.display()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(json!({"size": meta.len(), "mtime": mtime, "dir": meta.is_dir()}))
}

pub fn fs_mkdir(ctl: &Ctl, path: &str) -> Result<(), String> {
    let full = resolve(ctl, path, true)?;
    std::fs::create_dir_all(&full).map_err(|e| format!("创建目录失败 {}: {e}", full.display()))
}

pub fn fs_remove(ctl: &Ctl, path: &str) -> Result<(), String> {
    let full = resolve(ctl, path, true)?;
    let r = if full.is_dir() {
        std::fs::remove_dir_all(&full)
    } else {
        std::fs::remove_file(&full)
    };
    r.map_err(|e| format!("删除失败 {}: {e}", full.display()))
}

pub fn fs_copy(ctl: &Ctl, from: &str, to: &str) -> Result<(), String> {
    let src = resolve(ctl, from, false)?;
    let dst = resolve(ctl, to, true)?;
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::copy(&src, &dst)
        .map(|_| ())
        .map_err(|e| format!("复制失败 {} → {}: {e}", src.display(), dst.display()))
}

// ---------------------------------------------------------------- store

fn store_path(ctl: &Ctl) -> PathBuf {
    ctl.info.data_dir.join("store.json")
}

fn load_store(ctl: &Ctl) -> J {
    if let Some(v) = ctl.store.borrow().as_ref() {
        return v.clone();
    }
    let v = std::fs::read_to_string(store_path(ctl))
        .ok()
        .and_then(|s| serde_json::from_str::<J>(&s).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| J::Object(Map::new()));
    *ctl.store.borrow_mut() = Some(v.clone());
    v
}

fn save_store(ctl: &Ctl, v: &J) -> Result<(), String> {
    let path = store_path(ctl);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建数据目录失败: {e}"))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, v.to_string().as_bytes()).map_err(|e| format!("写入 store 失败: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("保存 store 失败: {e}"))
}

pub fn store_get(ctl: &Ctl, key: &str) -> J {
    load_store(ctl).get(key).cloned().unwrap_or(J::Null)
}

pub fn store_set(ctl: &Ctl, key: &str, value: J) -> Result<(), String> {
    let mut v = load_store(ctl);
    let obj = match v.as_object_mut() {
        Some(o) => o,
        None => return Err("store 数据损坏".into()),
    };
    if value.is_null() {
        obj.remove(key);
    } else {
        obj.insert(key.to_string(), value);
    }
    *ctl.store.borrow_mut() = Some(v.clone());
    save_store(ctl, &v)
}

// ---------------------------------------------------------------- 进程

/// 运行本机程序，超时杀进程
pub fn exec(
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    timeout: Duration,
    env: &[(String, String)],
    input: Option<&str>,
) -> Result<J, String> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
        cmd.current_dir(cwd);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("无法启动 {program}: {e}"))?;

    // 写入 stdin（避免死锁放到单独线程）
    match input {
        Some(text) => {
            if let Some(mut si) = child.stdin.take() {
                let data = text.as_bytes().to_vec();
                std::thread::spawn(move || {
                    let _ = si.write_all(&data);
                });
            }
        }
        None => drop(child.stdin.take()),
    }

    let (otx, orx) = std::sync::mpsc::channel::<String>();
    let (etx, erx) = std::sync::mpsc::channel::<String>();
    if let Some(mut p) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = p.read_to_end(&mut buf);
            let _ = otx.send(String::from_utf8_lossy(&buf).to_string());
        });
    }
    if let Some(mut p) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = p.read_to_end(&mut buf);
            let _ = etx.send(String::from_utf8_lossy(&buf).to_string());
        });
    }

    let start = Instant::now();
    let mut killed = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    kill_tree(&mut child);
                    killed = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(e) => return Err(format!("等待 {program} 失败: {e}")),
        }
    };
    // 被杀的子进程可能还有孙进程占着管道：不要无限等
    let wait = if killed {
        Duration::from_millis(200)
    } else {
        Duration::from_secs(10)
    };
    let stdout = orx.recv_timeout(wait).unwrap_or_default();
    let mut stderr = erx.recv_timeout(wait).unwrap_or_default();
    let code = if killed {
        stderr.push_str("\n(命令超时被终止)");
        -1
    } else {
        status.and_then(|s| s.code()).unwrap_or(-1)
    };
    Ok(json!({"code": code, "stdout": stdout, "stderr": stderr}))
}

/// 杀进程（Windows 上连同子进程一起杀，避免孙进程占着管道）
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &child.id().to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(0x0800_0000)
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// 打开文件 / 网址
pub fn open(target: &str) -> Result<(), String> {
    let mut cmd;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd = std::process::Command::new("cmd");
        cmd.args(["/C", "start", "", target]);
        cmd.creation_flags(0x0800_0000);
    }
    #[cfg(target_os = "macos")]
    {
        cmd = std::process::Command::new("open");
        cmd.arg(target);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        cmd = std::process::Command::new("xdg-open");
        cmd.arg(target);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn().map(|_| ()).map_err(|e| format!("打开失败: {e}"))
}

// ---------------------------------------------------------------- 剪贴板

pub fn clipboard_get() -> Result<String, String> {
    #[cfg(windows)]
    {
        let r = exec(
            "powershell",
            &["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), "Get-Clipboard -Raw".into()],
            None,
            Duration::from_secs(10),
            &[],
            None,
        )?;
        if r["code"].as_i64().unwrap_or(-1) != 0 {
            return Err(format!("读取剪贴板失败: {}", r["stderr"].as_str().unwrap_or("")));
        }
        let mut s = r["stdout"].as_str().unwrap_or("").to_string();
        while s.ends_with('\n') || s.ends_with('\r') {
            s.pop();
        }
        Ok(s)
    }
    #[cfg(target_os = "macos")]
    {
        run_text("pbpaste", &[], None)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        match run_text("wl-paste", &[], None) {
            Ok(s) => Ok(s),
            Err(_) => run_text("xclip", &["-selection".into(), "clipboard".into(), "-o".into()], None),
        }
    }
}

pub fn clipboard_set(text: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        let r = exec(
            "powershell",
            &[
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                "[Console]::In.ReadToEnd() | Set-Clipboard".into(),
            ],
            None,
            Duration::from_secs(10),
            &[],
            Some(text),
        )?;
        if r["code"].as_i64().unwrap_or(-1) != 0 {
            return Err(format!("写入剪贴板失败: {}", r["stderr"].as_str().unwrap_or("")));
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        run_text("pbcopy", &[], Some(text)).map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        match run_text("wl-copy", &[], Some(text)) {
            Ok(_) => Ok(()),
            Err(_) => run_text("xclip", &["-selection".into(), "clipboard".into(), "-i".into()], Some(text)).map(|_| ()),
        }
    }
}

#[cfg(not(windows))]
fn run_text(prog: &str, args: &[String], input: Option<&str>) -> Result<String, String> {
    let r = exec(prog, args, None, Duration::from_secs(10), &[], input)?;
    if r["code"].as_i64().unwrap_or(-1) != 0 {
        return Err(format!("{prog} 失败: {}", r["stderr"].as_str().unwrap_or("")));
    }
    let mut s = r["stdout"].as_str().unwrap_or("").to_string();
    while s.ends_with('\n') || s.ends_with('\r') {
        s.pop();
    }
    Ok(s)
}

// ---------------------------------------------------------------- HTTP

pub fn http(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    timeout: Duration,
) -> Result<J, String> {
    let cfg = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build();
    let agent: ureq::Agent = cfg.into();
    let method = method.to_ascii_uppercase();
    let resp = match method.as_str() {
        "POST" | "PUT" | "PATCH" => {
            let req = match method.as_str() {
                "POST" => agent.post(url),
                "PUT" => agent.put(url),
                _ => agent.patch(url),
            };
            let mut req = req;
            for (k, v) in headers {
                req = req.header(k.as_str(), v.as_str());
            }
            req.send(body.unwrap_or(""))
        }
        "GET" | "DELETE" | "HEAD" => {
            let req = match method.as_str() {
                "GET" => agent.get(url),
                "DELETE" => agent.delete(url),
                _ => agent.head(url),
            };
            let mut req = req;
            for (k, v) in headers {
                req = req.header(k.as_str(), v.as_str());
            }
            req.call()
        }
        m => return Err(format!("不支持的 HTTP 方法: {m}")),
    };
    let mut resp = resp.map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status().as_u16();
    let mut hs = Map::new();
    for (k, v) in resp.headers() {
        if let Ok(s) = v.to_str() {
            hs.insert(k.as_str().to_string(), J::String(s.to_string()));
        }
    }
    let text = resp
        .body_mut()
        .with_config()
        .limit(32 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| format!("读取响应失败: {e}"))?;
    Ok(json!({"status": status, "headers": hs, "body": text}))
}
