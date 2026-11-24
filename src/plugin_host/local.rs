//! 宿主本地实现（不经过主进程）：文件、存储、运行本机程序、HTTP、剪贴板等。
//!
//! 这里的函数都是阻塞的，由驱动线程放到后台线程里执行后再恢复协程。

use super::state::Ctl;
use serde_json::{Map, Value as J, json};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

const PLUGIN_SCHEME: &str = "plugin://";

/// 检查目标路径（或其最深已存在父目录）经 canonicalize 后仍落在 root 内部，防止符号链接逃逸
pub fn within_canonical(path: &Path, root: &Path) -> bool {
    let Ok(canon_root) = std::fs::canonicalize(root) else {
        return false;
    };
    let mut cur = path;
    let mut tail = Vec::new();
    while !cur.exists() {
        let Some(name) = cur.file_name() else { return false };
        tail.push(name.to_os_string());
        let Some(parent) = cur.parent() else { return false };
        cur = parent;
    }
    let Ok(mut canon) = std::fs::canonicalize(cur) else {
        return false;
    };
    for part in tail.into_iter().rev() {
        canon.push(part);
    }
    canon.starts_with(&canon_root)
}

/// 把插件给的路径解析成绝对路径，并做权限与符号链接边界检查。
pub fn resolve(ctl: &Ctl, path: &str, write: bool) -> Result<PathBuf, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("路径为空".into());
    }
    // plugin:// 只读，且不允许通过内部符号链接逃出插件目录
    if let Some(rest) = path.strip_prefix(PLUGIN_SCHEME) {
        if write {
            return Err("插件目录是只读的".into());
        }
        check_relative(rest)?;
        let full = ctl.info.dir.join(rest.replace('\\', "/"));
        if !ctl.has_perm("fs.read") && full.exists() && !within_canonical(&full, &ctl.info.dir) {
            return Err("读取该路径需要 fs.read 权限".into());
        }
        return Ok(full);
    }
    let p = Path::new(path);
    // 绝对路径也不能带 ..，否则 `<dataDir>/../../x` 会被当成插件自己的目录
    check_relative(path)?;
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        ctl.info.data_dir.join(p)
    };
    let in_data = full.starts_with(&ctl.info.data_dir) && within_canonical(&full, &ctl.info.data_dir);
    let in_dir = !write && full.starts_with(&ctl.info.dir) && (!full.exists() || within_canonical(&full, &ctl.info.dir));
    // 每个插件只能免权限读自己的临时目录 `.tmp/<id>/`，以及用户通过文件选择框显式上传的具体文件 `.tmp/uploads/<file>`（禁止列目录）
    let own_tmp = !write
        && ctl.info.dir.parent().is_some_and(|root| {
            let my_tmp = root.join(".tmp").join(&ctl.info.id);
            full.starts_with(&my_tmp) && (!full.exists() || within_canonical(&full, &my_tmp))
        });
    let upload_file = !write
        && ctl.info.dir.parent().is_some_and(|root| {
            let up_dir = root.join(".tmp").join("uploads");
            full.parent() == Some(up_dir.as_path()) && full.is_file() && within_canonical(&full, &up_dir)
        });
    if write {
        // 插件目录始终只读
        if full.starts_with(&ctl.info.dir) && !full.starts_with(&ctl.info.data_dir) {
            return Err("插件目录是只读的".into());
        }
        if !in_data && !ctl.has_perm("fs") {
            return Err("写入该路径需要 fs 权限".into());
        }
    } else if !in_data && !in_dir && !own_tmp && !upload_file && !ctl.has_perm("fs.read") {
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
pub(super) fn kill_tree(child: &mut std::process::Child) {
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

type Resp = ureq::http::Response<ureq::Body>;

fn send(url: &str, method: &str, headers: &[(String, String)], body: Option<&str>, timeout: Duration) -> Result<Resp, String> {
    let cfg = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build();
    let agent: ureq::Agent = cfg.into();
    let method = method.to_ascii_uppercase();
    let resp = match method.as_str() {
        "POST" | "PUT" | "PATCH" => {
            let mut req = match method.as_str() {
                "POST" => agent.post(url),
                "PUT" => agent.put(url),
                _ => agent.patch(url),
            };
            for (k, v) in headers {
                req = req.header(k.as_str(), v.as_str());
            }
            req.send(body.unwrap_or(""))
        }
        "GET" | "DELETE" | "HEAD" => {
            let mut req = match method.as_str() {
                "GET" => agent.get(url),
                "DELETE" => agent.delete(url),
                _ => agent.head(url),
            };
            for (k, v) in headers {
                req = req.header(k.as_str(), v.as_str());
            }
            req.call()
        }
        m => return Err(format!("不支持的 HTTP 方法: {m}")),
    };
    resp.map_err(|e| format!("请求失败: {e}"))
}

fn header_map(resp: &Resp) -> Map<String, J> {
    let mut hs = Map::new();
    for (k, v) in resp.headers() {
        if let Ok(s) = v.to_str() {
            hs.insert(k.as_str().to_string(), J::String(s.to_string()));
        }
    }
    hs
}

pub fn http(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    timeout: Duration,
) -> Result<J, String> {
    let mut resp = send(url, method, headers, body, timeout)?;
    let status = resp.status().as_u16();
    let hs = header_map(&resp);
    let text = resp
        .body_mut()
        .with_config()
        .limit(32 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| format!("读取响应失败: {e}"))?;
    Ok(json!({"status": status, "headers": hs, "body": text}))
}

/// GET 到文件（流式写入，不限大小）；非 2xx 时不留文件
pub fn download(url: &str, headers: &[(String, String)], dst: &Path, timeout: Duration) -> Result<J, String> {
    let mut resp = send(url, "GET", headers, None, timeout)?;
    let status = resp.status().as_u16();
    let hs = header_map(&resp);
    if !(200..300).contains(&status) {
        return Ok(json!({"status": status, "headers": hs, "size": 0}));
    }
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut part = dst.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let written = std::fs::File::create(&part)
        .map_err(|e| format!("创建文件失败 {}: {e}", part.display()))
        .and_then(|f| {
            let mut w = std::io::BufWriter::new(f);
            let n = std::io::copy(&mut resp.body_mut().as_reader(), &mut w).map_err(|e| format!("下载失败: {e}"))?;
            w.flush().map_err(|e| format!("写入失败: {e}"))?;
            Ok(n)
        });
    let size = match written {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };
    std::fs::rename(&part, dst).map_err(|e| format!("保存失败 {}: {e}", dst.display()))?;
    Ok(json!({"status": status, "headers": hs, "size": size}))
}

// ---------------------------------------------------------------- 目录遍历与打包

fn mtime_of(meta: &std::fs::Metadata) -> f64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as f64 / 1000.0)
        .unwrap_or(0.0)
}

/// 递归列出 root 下的文件：{path（相对，/ 分隔）, size, mtime}；ext 为空表示全部，hidden 为假时跳过 . 开头的文件和目录
pub fn walk(root: &Path, ext: &[String], hidden: bool) -> Result<J, String> {
    let ext: Vec<String> = ext.iter().map(|e| e.trim_start_matches('.').to_ascii_lowercase()).collect();
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let rd = std::fs::read_dir(&dir).map_err(|e| format!("读取目录失败 {}: {e}", dir.display()))?;
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if !hidden && name.starts_with('.') {
                continue;
            }
            let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let Ok(meta) = ent.metadata() else { continue };
            if meta.is_dir() {
                stack.push((ent.path(), path));
                continue;
            }
            if !ext.is_empty() {
                let e = Path::new(&name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                if !ext.contains(&e) {
                    continue;
                }
            }
            out.push(json!({"path": path, "size": meta.len(), "mtime": mtime_of(&meta)}));
        }
    }
    Ok(J::Array(out))
}

fn tar_octal(field: &mut [u8], v: u64) {
    let s = format!("{:0w$o}", v, w = field.len() - 1);
    field[..s.len()].copy_from_slice(s.as_bytes());
}

/// ustar 头；路径超过 100 字节时拆到 prefix
fn tar_header(path: &str, size: u64, mtime: u64, dir: bool) -> Result<[u8; 512], String> {
    let b = path.as_bytes();
    let (prefix, name) = if b.len() <= 100 {
        (&b[..0], b)
    } else {
        match (0..b.len()).rev().find(|&i| b[i] == b'/' && i <= 155 && b.len() - i - 1 <= 100) {
            Some(i) => (&b[..i], &b[i + 1..]),
            None => return Err(format!("路径太长，无法打包：{path}")),
        }
    };
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name);
    tar_octal(&mut h[100..108], if dir { 0o755 } else { 0o644 });
    tar_octal(&mut h[108..116], 0);
    tar_octal(&mut h[116..124], 0);
    tar_octal(&mut h[124..136], size);
    tar_octal(&mut h[136..148], mtime);
    h[156] = if dir { b'5' } else { b'0' };
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[345..345 + prefix.len()].copy_from_slice(prefix);
    h[148..156].fill(b' ');
    let sum: u32 = h.iter().map(|&c| c as u32).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    Ok(h)
}

/// 文件内容的 FNV-1a 64 位哈希（16 位十六进制）；不存在的文件跳过
pub fn hash(root: &Path, files: &[String]) -> Result<J, String> {
    let mut out = Map::new();
    for rel in files {
        check_rel(rel)?;
        let Ok(data) = std::fs::read(root.join(rel)) else { continue };
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in &data {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        out.insert(rel.clone(), J::String(format!("{h:016x}")));
    }
    Ok(J::Object(out))
}

fn check_rel(f: &str) -> Result<(), String> {
    if f.is_empty() || f.starts_with('/') || f.contains('\\') || f.split('/').any(|c| c == ".." || c.is_empty()) {
        return Err(format!("路径不合法：{f}"));
    }
    Ok(())
}

/// 把 root 下的文件打成 tar（含上级目录项，gzip 时为 .tar.gz）；files 为 None 时打包全部
pub fn tar(root: &Path, files: Option<Vec<String>>, out: &Path, gzip: bool) -> Result<J, String> {
    let mut files = match files {
        Some(f) => f,
        None => walk(root, &[], false)?
            .as_array()
            .map(|a| a.iter().filter_map(|v| v["path"].as_str().map(String::from)).collect())
            .unwrap_or_default(),
    };
    for f in &files {
        check_rel(f)?;
    }
    files.sort();
    files.dedup();
    let mut dirs: Vec<&str> = files.iter().flat_map(|f| f.match_indices('/').map(move |(i, _)| &f[..i])).collect();
    dirs.sort();
    dirs.dedup();

    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let f = std::fs::File::create(out).map_err(|e| format!("创建文件失败 {}: {e}", out.display()))?;
    let io = |e: std::io::Error| format!("写入失败 {}: {e}", out.display());
    let file = std::io::BufWriter::new(f);
    let total = if gzip {
        let mut gz = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let n = write_tar(&mut gz, root, &dirs, &files).map_err(io)?;
        gz.finish().and_then(|mut f| f.flush()).map_err(io)?;
        n
    } else {
        let mut file = file;
        let n = write_tar(&mut file, root, &dirs, &files).map_err(io)?;
        file.flush().map_err(io)?;
        n
    };
    Ok(json!({"count": files.len(), "size": total}))
}

fn write_tar(w: &mut impl Write, root: &Path, dirs: &[&str], files: &[String]) -> std::io::Result<u64> {
    let bad = |e: String| std::io::Error::other(e);
    let now = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    for d in dirs {
        w.write_all(&tar_header(d, 0, now, true).map_err(bad)?)?;
    }
    let mut total = 0u64;
    for rel in files {
        let src = root.join(rel);
        let data = std::fs::read(&src).map_err(|e| bad(format!("读取失败 {}: {e}", src.display())))?;
        let mtime = std::fs::metadata(&src).map(|m| mtime_of(&m) as u64).unwrap_or(now);
        w.write_all(&tar_header(rel, data.len() as u64, mtime, false).map_err(bad)?)?;
        w.write_all(&data)?;
        w.write_all(&vec![0u8; (512 - data.len() % 512) % 512])?;
        total += data.len() as u64;
    }
    w.write_all(&[0u8; 1024])?;
    Ok(total)
}
