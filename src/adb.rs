//! adb 调用封装。

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

pub fn command(adb: &Path) -> Command {
    let mut c = Command::new(adb);
    c.stdin(Stdio::null());
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW：避免每次调用 adb 都弹出控制台窗口
        c.creation_flags(0x0800_0000);
    }
    c
}

/// 执行 adb 命令并返回 stdout；失败时返回 stderr/stdout 拼接的错误信息。
pub async fn run(adb: &Path, args: &[&str]) -> Result<String, String> {
    let out = command(adb)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("无法执行 adb（{}）：{e}", adb.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!("adb {} 失败：{}{}", args.join(" "), stderr.trim(), stdout.trim()))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub serial: String,
    /// device / unauthorized / offline / no permissions ...
    pub state: String,
    pub model: String,
    pub product: String,
    pub device: String,
    pub transport_id: String,
}

pub async fn list_devices(adb: &Path) -> Result<Vec<Device>, String> {
    let out = run(adb, &["devices", "-l"]).await?;
    Ok(parse_devices(&out))
}

fn parse_devices(out: &str) -> Vec<Device> {
    let mut list = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('*') || line.starts_with("List of devices") {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(serial) = parts.next() else { continue };
        // 状态是第二列；"no permissions ..." 由多个单词组成。其后可能跟无前缀的 USB 路径（如 macOS 的 "20-6"）
        let state = match parts.next() {
            Some("no") => "no permissions".to_string(),
            Some(s) => s.to_string(),
            None => continue,
        };
        let mut d = Device {
            serial: serial.to_string(),
            state: String::new(),
            model: String::new(),
            product: String::new(),
            device: String::new(),
            transport_id: String::new(),
        };
        for p in parts {
            if let Some((k, v)) = p.split_once(':') {
                match k {
                    "model" => d.model = v.replace('_', " "),
                    "product" => d.product = v.to_string(),
                    "device" => d.device = v.to_string(),
                    "transport_id" => d.transport_id = v.to_string(),
                    _ => {}
                }
            }
        }
        d.state = state;
        list.push(d);
    }
    list
}

/// 查找 adb：命令行参数 > 环境变量 ADB > exe 同目录 platform-tools > 开发目录 third_party > PATH。
pub fn find_adb(cli: Option<PathBuf>) -> PathBuf {
    if let Some(p) = cli {
        return p;
    }
    if let Ok(p) = std::env::var("ADB") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let exe_name = if cfg!(windows) { "adb.exe" } else { "adb" };
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("platform-tools").join(exe_name));
            candidates.push(dir.join(exe_name));
        }
    }
    // 仅开发版：仓库内 third_party（发布版不把构建机器的绝对路径编译进程序）
    #[cfg(debug_assertions)]
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("third_party")
            .join("platform-tools")
            .join(exe_name),
    );
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Ok(v) = std::env::var(var) {
            candidates.push(PathBuf::from(v).join("platform-tools").join(exe_name));
        }
    }
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("LOCALAPPDATA")) {
        let home = PathBuf::from(home);
        candidates.push(home.join("Library/Android/sdk/platform-tools").join(exe_name));
        candidates.push(home.join("Android/Sdk/platform-tools").join(exe_name));
    }
    for p in ["/usr/local/bin/adb", "/opt/homebrew/bin/adb", "/usr/bin/adb"] {
        candidates.push(PathBuf::from(p));
    }
    for c in candidates {
        if c.is_file() {
            return c;
        }
    }
    PathBuf::from(exe_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        let s = "List of devices attached\n\
                 R58M12345 device usb:1-1 product:beyond1 model:SM_G973F device:beyond1 transport_id:3\n\
                 192.168.1.5:5555 unauthorized transport_id:4\n\
                 emulator-5554 no permissions (user in plugdev group); see [http://x]\n\
                 0123456789ABCDEF       device 20-6 product:demo model:Demo_Phone device:demo transport_id:15\n";
        let d = parse_devices(s);
        assert_eq!(d.len(), 4);
        assert_eq!(d[3].state, "device");
        assert_eq!(d[3].model, "Demo Phone");
        assert_eq!(d[0].model, "SM G973F");
        assert_eq!(d[0].state, "device");
        assert_eq!(d[1].state, "unauthorized");
        assert!(d[2].state.starts_with("no permissions"));
    }
}
