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
        .map_err(|e| { let p = adb.display(); tr!("无法执行 adb（{p}）：{e}", "Cannot run adb ({p}): {e}") })?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let (a, e, o) = (args.join(" "), stderr.trim(), stdout.trim());
        Err(tr!("adb {a} 失败：{e}{o}", "adb {a} failed: {e}{o}"))
    }
}

/// 同 run，带超时（无线连接、配对等可能长时间无响应）
pub async fn run_timeout(adb: &Path, args: &[&str], timeout: std::time::Duration) -> Result<String, String> {
    match tokio::time::timeout(timeout, run_merged(adb, args)).await {
        Ok(r) => r,
        Err(_) => {
            let a = args.join(" ");
            Err(tr!("adb {a} 超时", "adb {a} timed out"))
        }
    }
}

/// 执行并返回原始 stdout（二进制输出，如 exec-out screencap），带超时
pub async fn run_bytes(adb: &Path, args: &[&str], timeout: std::time::Duration) -> Result<Vec<u8>, String> {
    let fut = command(adb).args(args).kill_on_drop(true).output();
    let out = match tokio::time::timeout(timeout, fut).await {
        Ok(r) => r.map_err(|e| { let p = adb.display(); tr!("无法执行 adb（{p}）：{e}", "Cannot run adb ({p}): {e}") })?,
        Err(_) => {
            let a = args.join(" ");
            return Err(tr!("adb {a} 超时", "adb {a} timed out"));
        }
    };
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let (a, e) = (args.join(" "), String::from_utf8_lossy(&out.stderr).trim().to_string());
        Err(tr!("adb {a} 失败：{e}", "adb {a} failed: {e}"))
    }
}

/// 执行并返回 stdout+stderr（adb connect / pair 把结果写在不同的流里）
async fn run_merged(adb: &Path, args: &[&str]) -> Result<String, String> {
    let out = command(adb)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| { let p = adb.display(); tr!("无法执行 adb（{p}）：{e}", "Cannot run adb ({p}): {e}") })?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() { Ok(text) } else { Err(text.trim().to_string()) }
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
    /// 无线连接（ip:port 或 mDNS 名称）
    pub wireless: bool,
    /// 稳定的设备标识（ro.serialno），用于断线后识别同一台设备；无线设备重连后 serial 可能变化
    pub uid: String,
    /// 同一台设备同时连着的其他方式（USB 与无线的 serial），列表里只显示其中一条
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<String>,
}

pub fn is_wireless(serial: &str) -> bool {
    serial.contains(':') || serial.contains("._adb-tls-connect")
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
            wireless: is_wireless(serial),
            uid: if is_wireless(serial) { String::new() } else { serial.to_string() },
            also: Vec::new(),
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

/// 同一台设备同时有 USB 和无线连接时只留一条（能用的优先，其次 USB），否则会显示成两台；
/// 靠 uid（USB 为 serial，无线为 ro.serialno）和型号判断是不是同一台，被合并掉的 serial 记在 also 里
pub fn merge_same(list: Vec<Device>) -> Vec<Device> {
    let rank = |d: &Device| (d.state == "device", !d.wireless);
    let mut out: Vec<Device> = Vec::with_capacity(list.len());
    for mut d in list {
        let same = |o: &Device| !d.uid.is_empty() && o.uid == d.uid && (d.model.is_empty() || o.model.is_empty() || d.model == o.model);
        let Some(i) = out.iter().position(same) else {
            out.push(d);
            continue;
        };
        if rank(&d) > rank(&out[i]) {
            std::mem::swap(&mut d, &mut out[i]);
        }
        out[i].also.push(d.serial.clone());
        out[i].also.append(&mut d.also);
    }
    out
}

/// 设备屏幕上是否画着导航按键（三键 / 两键导航栏）；无法判断时返回 None
pub async fn nav_buttons(adb: PathBuf, serial: String) -> Option<bool> {
    let script = "echo nm=$(settings get secure navigation_mode 2>/dev/null); \
                  echo fsg=$(settings get global force_fsg_nav_bar 2>/dev/null); \
                  dumpsys window windows";
    let out = run_timeout(&adb, &["-s", &serial, "shell", script], std::time::Duration::from_secs(4)).await.ok()?;
    parse_nav_buttons(&out)
}

/// 解析 nav_buttons 的输出：手势导航没有按键；否则看导航栏窗口是否存在且可见
fn parse_nav_buttons(out: &str) -> Option<bool> {
    let val = |k: &str| out.lines().find_map(|l| l.trim().strip_prefix(k)).map(str::trim);
    if val("nm=") == Some("2") || val("fsg=") == Some("1") {
        return Some(false);
    }
    let mut windows = false;
    let mut nav: Option<bool> = None;
    for line in out.lines().map(str::trim) {
        if line.starts_with("Window #") {
            windows = true;
            if nav.is_some() {
                break;
            }
            if line.contains("NavigationBar") {
                nav = Some(true);
            }
        } else if nav.is_some() {
            if let Some(v) = line.split_whitespace().find_map(|w| w.strip_prefix("mViewVisibility=")) {
                nav = Some(v == "0x0");
                break;
            }
        }
    }
    if windows { Some(nav.unwrap_or(false)) } else { None }
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
            // macOS 应用包：Contents/MacOS/程序 → Contents/Resources/platform-tools
            candidates.push(dir.join("../Resources/platform-tools").join(exe_name));
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

    #[test]
    fn merge_same_device() {
        let d = |serial: &str, state: &str, uid: &str, model: &str| Device {
            serial: serial.into(),
            state: state.into(),
            uid: uid.into(),
            wireless: is_wireless(serial),
            model: model.into(),
            product: String::new(),
            device: String::new(),
            transport_id: String::new(),
            also: Vec::new(),
        };
        // USB 优先，无线的 serial 记在 also
        let v = merge_same(vec![d("192.168.1.5:5555", "device", "ABC", "OPD2407"), d("ABC", "device", "ABC", "OPD2407")]);
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].serial.as_str(), v[0].also.clone()), ("ABC", vec!["192.168.1.5:5555".to_string()]));
        // USB 未授权时留能用的那条
        let v = merge_same(vec![d("ABC", "unauthorized", "ABC", "OPD2407"), d("192.168.1.5:5555", "device", "ABC", "OPD2407")]);
        assert_eq!((v.len(), v[0].serial.as_str(), v[0].also.clone()), (1, "192.168.1.5:5555", vec!["ABC".to_string()]));
        // 同一台的两条无线也合并
        let v = merge_same(vec![d("adb-x._adb-tls-connect._tcp.", "device", "ABC", "OPD2407"), d("192.168.1.5:5555", "device", "ABC", "")]);
        assert_eq!((v.len(), v[0].also.clone()), (1, vec!["192.168.1.5:5555".to_string()]));
        // 不同设备、uid 还没读到、型号不同：都不合并
        let v = merge_same(vec![
            d("ABC", "device", "ABC", "OPD2407"),
            d("DEF", "device", "DEF", "OPD2407"),
            d("10.0.0.8:5555", "device", "", "OPD2407"),
            d("10.0.0.9:5555", "device", "ABC", "SM_G973F"),
        ]);
        assert_eq!(v.len(), 4);
        assert!(v.iter().all(|x| x.also.is_empty()));
    }

    #[test]
    fn nav_buttons() {
        let win = |vis: &str| format!(
            "WINDOW MANAGER WINDOWS (dumpsys window windows)\n  Window #0 Window{{41b u0 NavigationBar}}:\n    mOwnerUid=10012\n    mViewVisibility={vis} mHaveFrame=true\n  Window #1 Window{{42c u0 StatusBar}}:\n    mViewVisibility=0x0\n"
        );
        // 老手机（没有 navigation_mode）：导航栏窗口可见 → 屏幕上有按键
        assert_eq!(parse_nav_buttons(&format!("nm=\nfsg=\n{}", win("0x0"))), Some(true));
        assert_eq!(parse_nav_buttons(&format!("nm=null\nfsg=null\n{}", win("0x8"))), Some(false));
        assert_eq!(parse_nav_buttons(&format!("nm=0\nfsg=null\n{}", win("0x0"))), Some(true));
        // 手势导航
        assert_eq!(parse_nav_buttons(&format!("nm=2\nfsg=null\n{}", win("0x0"))), Some(false));
        assert_eq!(parse_nav_buttons(&format!("nm=\nfsg=1\n{}", win("0x0"))), Some(false));
        // 实体按键：没有导航栏窗口
        assert_eq!(parse_nav_buttons("nm=\nfsg=\n  Window #0 Window{42c u0 StatusBar}:\n    mViewVisibility=0x0\n"), Some(false));
        // dumpsys 不可用
        assert_eq!(parse_nav_buttons("nm=\nfsg=\nCan't find service: window\n"), None);
    }
}
