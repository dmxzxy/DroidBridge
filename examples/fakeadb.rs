//! 测试用的假 adb：用 JSON 模拟设备文件系统，验证文件管理的完整链路（不连接真机）。
//! cargo build --example fakeadb，然后 droidbridge --adb target/debug/examples/fakeadb.exe
//! 状态文件：$FAKEADB_DIR/fs.json —— {"dirs":["/sdcard","/sdcard/DCIM"],"files":{"/sdcard/a.txt":"base64…"}}

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(std::env::var("FAKEADB_DIR").unwrap_or_else(|_| ".".into()))
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Fs {
    dirs: Vec<String>,
    files: BTreeMap<String, String>,
}

fn load() -> Fs {
    let p = root().join("fs.json");
    let mut fs: Fs = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    if fs.dirs.is_empty() {
        fs.dirs = vec!["/sdcard".into()];
    }
    fs
}

fn save(fs: &Fs) {
    let _ = std::fs::write(root().join("fs.json"), serde_json::to_string_pretty(fs).unwrap());
}

fn b64(data: &[u8]) -> String {
    droidbridge::util::base64(data)
}

fn unb64(s: &str) -> Vec<u8> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut n = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let Some(v) = T.iter().position(|&t| t == c) else { continue };
        n = (n << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((n >> bits) as u8);
        }
    }
    out
}

/// 从命令中取出所有 'xx' 形式的参数
fn quoted(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = cmd;
    while let Some(i) = rest.find('\'') {
        let after = &rest[i + 1..];
        let Some(j) = after.find('\'') else { break };
        out.push(after[..j].to_string());
        rest = &after[j + 1..];
    }
    out
}

fn parent(p: &str) -> String {
    match p.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => p[..i].to_string(),
    }
}

fn shell(fs: &mut Fs, cmd: &str) -> String {
    let q = quoted(cmd);
    // 列目录
    if cmd.contains("printf '%s\\t%s\\t%s\\t%s\\n'") {
        let Some(dir) = q.first() else { return String::new() };
        if !fs.dirs.contains(dir) {
            return "DroidBridge: cannot open".into();
        }
        let mut rows = String::new();
        let has = |p: String| fs.dirs.contains(&p) || fs.files.contains_key(&p);
        let mut names: Vec<(String, bool)> = fs
            .dirs
            .iter()
            .filter(|d| parent(d) == *dir && *d != dir)
            .map(|d| (d.rsplit('/').next().unwrap().to_string(), true))
            .chain(fs.files.keys().filter(|f| parent(f) == *dir).map(|f| (f.rsplit('/').next().unwrap().to_string(), false)))
            .collect();
        names.sort();
        for (n, is_dir) in names {
            let full = format!("{}/{n}", dir.trim_end_matches('/'));
            if !has(full) {
                continue;
            }
            let size = fs.files.get(&format!("{}/{}", dir.trim_end_matches('/'), n)).map(|v| unb64(v).len()).unwrap_or(4096);
            rows.push_str(&format!("{}\t{}\t{}\t{}\n", if is_dir { "d" } else { "f" }, size, 1700000000u64, n));
        }
        return rows;
    }
    if cmd.starts_with("df -k") {
        return "Filesystem     1K-blocks      Used Available Use% Mounted on\n/dev/fuse      100000000  40000000  60000000  40% /storage/emulated\n".into();
    }
    if cmd.contains("mkdir -p") {
        if let Some(p) = q.first().filter(|p| !fs.dirs.contains(p)) {
            fs.dirs.push(p.clone());
        }
        return String::new();
    }
    if cmd.trim_start().starts_with("[ -d") {
        let ok = q.first().map(|p| fs.dirs.contains(p)).unwrap_or(false);
        return if ok { "ok\n".into() } else { String::new() };
    }
    if cmd.contains("mv ") && q.len() >= 2 {
        {
            let (from, to) = (q[0].clone(), q[1].clone());
            if let Some(v) = fs.files.remove(&from) {
                fs.files.insert(to, v);
                return "ok\n".into();
            }
            if fs.dirs.contains(&from) {
                let old = from.clone();
                fs.dirs.retain(|d| d != &from && !d.starts_with(&format!("{from}/")));
                fs.dirs.push(to.clone());
                let files: Vec<(String, String)> = fs.files.iter().filter(|(k, _)| k.starts_with(&format!("{old}/"))).map(|(k, v)| (k.clone(), v.clone())).collect();
                for (k, v) in files {
                    fs.files.remove(&k);
                    fs.files.insert(k.replacen(&old, &to, 1), v);
                }
                return "ok\n".into();
            }
            return "mv: No such file or directory".into();
        }
    }
    if cmd.contains("rm -rf") {
        if let Some(p) = q.first() {
            fs.files.remove(p);
            fs.dirs.retain(|d| d != p && !d.starts_with(&format!("{p}/")));
        }
        return String::new();
    }
    if cmd.trim_start().starts_with("[ -e") {
        let exists = q.first().map(|p| fs.files.contains_key(p) || fs.dirs.contains(p)).unwrap_or(false);
        return if exists { "still-there\n".into() } else { String::new() };
    }
    if cmd.starts_with("dumpsys battery") {
        return "  level: 100\n".into();
    }
    String::new()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("devices") {
        println!("List of devices attached\nfakedevice            device product:Fake model:Fake device:fake transport_id:1");
        return;
    }
    // -s <serial> <command…>
    let idx = args.iter().position(|a| a == "-s").unwrap_or(usize::MAX);
    let rest: Vec<&str> = args.iter().skip(idx + 2).map(String::as_str).collect();
    let mut fs = load();
    match rest.as_slice() {
        ["shell", cmd] => print!("{}", shell(&mut fs, cmd)),
        ["push", local, remote] => {
            let data = std::fs::read(Path::new(local)).unwrap_or_default();
            if !fs.dirs.contains(&parent(remote)) {
                eprintln!("adb: error: remote path does not exist");
                std::process::exit(1);
            }
            fs.files.insert(remote.to_string(), b64(&data));
        }
        ["pull", remote, local] => {
            let Some(v) = fs.files.get(*remote) else {
                eprintln!("adb: error: remote object '{remote}' does not exist");
                std::process::exit(1);
            };
            std::fs::write(local, unb64(v)).unwrap();
        }
        _ => {
            let mut buf = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut buf);
        }
    }
    save(&fs);
}
