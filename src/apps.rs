//! 应用管理：列出已安装应用，启动、强制停止、清除数据、卸载。
//!
//! 应用名称由 scrcpy-server 的 list_apps 在设备上读取（adb 本身拿不到应用名称）；
//! 失败时（如很旧的系统）退回 pm list packages，只显示包名。

use crate::{adb, logcat::valid_pkg, AppState};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashSet;
use std::time::Duration;

/// 与投屏使用的 jar 分开放，避免和正在启动的投屏互相覆盖、删除
const APPS_JAR: &str = "/data/local/tmp/droidbridge-apps.jar";

#[derive(Deserialize)]
pub struct ListReq {
    pub serial: String,
}

#[derive(Deserialize)]
pub struct ActionReq {
    pub serial: String,
    pub pkg: String,
    /// launch / stop / clear / uninstall
    pub action: String,
}

#[derive(Debug, PartialEq)]
struct App {
    pkg: String,
    name: String,
    system: bool,
}

/// 解析 list_apps 的输出：" * 设置      com.android.settings"（* 系统应用，- 用户应用）
fn parse_list_apps(out: &str) -> Vec<App> {
    out.lines()
        .filter_map(|l| {
            let l = l.trim_end_matches('\r');
            let (system, rest) = if let Some(r) = l.strip_prefix(" * ") {
                (true, r)
            } else {
                (false, l.strip_prefix(" - ")?)
            };
            let rest = rest.trim_end();
            let (name, pkg) = rest.rsplit_once(char::is_whitespace)?;
            valid_pkg(pkg).then(|| App { pkg: pkg.to_string(), name: name.trim().to_string(), system })
        })
        .collect()
}

fn parse_pm(out: &str) -> Vec<String> {
    out.lines().filter_map(|l| l.trim().strip_prefix("package:")).filter(|p| valid_pkg(p)).map(String::from).collect()
}

/// 正在运行的进程名（取每行最后一列，兼容只支持旧版 ps 的系统）
fn parse_ps(out: &str) -> HashSet<String> {
    out.lines().filter_map(|l| l.split_whitespace().last()).map(|n| n.split(':').next().unwrap_or(n).to_string()).collect()
}

pub async fn list(st: &AppState, r: ListReq) -> Result<serde_json::Value, String> {
    let adb_path = &st.cfg.adb;
    let s = r.serial.as_str();
    let jar = st.cfg.server_jar.to_string_lossy().into_owned();
    let mut apps = Vec::new();
    if adb::run_timeout(adb_path, &["-s", s, "push", &jar, APPS_JAR], Duration::from_secs(30)).await.is_ok() {
        let cmd = format!(
            "CLASSPATH={APPS_JAR} app_process / com.genymobile.scrcpy.Server {} list_apps=true log_level=info; rm -f {APPS_JAR}",
            crate::scrcpy_version()
        );
        if let Ok(out) = adb::run_timeout(adb_path, &["-s", s, "shell", &cmd], Duration::from_secs(60)).await {
            apps = parse_list_apps(&out);
        }
    }
    let named = !apps.is_empty();
    if !named {
        let user: HashSet<String> = parse_pm(&adb::run(adb_path, &["-s", s, "shell", "pm list packages -3"]).await?).into_iter().collect();
        let all = parse_pm(&adb::run(adb_path, &["-s", s, "shell", "pm list packages"]).await?);
        apps = all.into_iter().map(|p| App { system: !user.contains(&p), name: String::new(), pkg: p }).collect();
    }
    let running = adb::run(adb_path, &["-s", s, "shell", "ps -A -o NAME 2>/dev/null || ps"]).await.map(|o| parse_ps(&o)).unwrap_or_default();
    let list: Vec<_> = apps
        .iter()
        .map(|a| json!({"pkg": a.pkg, "name": a.name, "system": a.system, "running": running.contains(&a.pkg)}))
        .collect();
    Ok(json!({"apps": list, "named": named}))
}

/// 错误输出只取第一行有内容的部分（去掉 Java 堆栈）
fn first_line(s: &str) -> String {
    let l = s.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("Exception occurred")).unwrap_or("");
    l.chars().take(200).collect()
}

pub async fn action(st: &AppState, r: ActionReq) -> Result<serde_json::Value, String> {
    let adb_path = &st.cfg.adb;
    let (s, pkg) = (r.serial.as_str(), r.pkg.trim());
    if !valid_pkg(pkg) || pkg.contains(':') {
        return Err(tr!("包名格式不正确", "Invalid package name"));
    }
    // 包名已校验只含 [A-Za-z0-9._]，可以安全拼进设备端 shell
    let sh = |cmd: String| async move { adb::run_timeout(adb_path, &["-s", s, "shell", &cmd], Duration::from_secs(30)).await };
    let ok = |out: &str| out.contains("Success");
    let msg = match r.action.as_str() {
        "launch" => {
            let comp = sh(format!("cmd package resolve-activity --brief -c android.intent.category.LAUNCHER {pkg} 2>/dev/null | tail -1"))
                .await
                .unwrap_or_default();
            let comp = comp.trim();
            let started = if comp.starts_with(pkg) && comp.contains('/') {
                sh(format!("am start -n {comp}")).await.map(|o| !o.contains("Error")).unwrap_or(false)
            } else {
                // 旧系统没有 resolve-activity：用 monkey 启动
                sh(format!("monkey -p {pkg} -c android.intent.category.LAUNCHER 1 2>&1"))
                    .await
                    .map(|o| o.contains("Events injected"))
                    .unwrap_or(false)
            };
            if !started {
                return Err(tr!("{pkg} 没有可以启动的界面", "{pkg} has no launchable activity"));
            }
            tr!("已启动", "Launched")
        }
        "stop" => {
            sh(format!("am force-stop {pkg}")).await?;
            tr!("已强制停止", "Force stopped")
        }
        "clear" => {
            let out = sh(format!("pm clear {pkg} 2>&1")).await.unwrap_or_else(|e| e);
            if !ok(&out) {
                return Err(if out.contains("SecurityException") || out.contains("CLEAR_APP_USER_DATA") {
                    tr!(
                        "系统不允许通过 adb 清除数据。OPPO / 一加 / realme：在开发者选项中打开“禁止权限监控”；小米：打开“USB 调试（安全设置）”",
                        "The system does not allow clearing data over adb. OPPO / OnePlus / realme: turn on “Disable permission monitoring” in Developer options; Xiaomi: turn on “USB debugging (Security settings)”"
                    )
                } else {
                    let o = first_line(&out);
                    tr!("清除数据失败：{o}", "Failed to clear data: {o}")
                });
            }
            tr!("已清除数据", "Data cleared")
        }
        "uninstall" => {
            let out = adb::run_timeout(adb_path, &["-s", s, "uninstall", pkg], Duration::from_secs(60)).await.unwrap_or_else(|e| e);
            if !ok(&out) {
                let o = first_line(&out);
                return Err(if o.contains("DELETE_FAILED_INTERNAL_ERROR") || o.contains("Unknown package") {
                    tr!("无法卸载：应用不存在或是系统应用（{o}）", "Cannot uninstall: the app is not installed or is a system app ({o})")
                } else {
                    tr!("卸载失败：{o}", "Uninstall failed: {o}")
                });
            }
            tr!("已卸载", "Uninstalled")
        }
        _ => return Err(tr!("未知操作", "Unknown action")),
    };
    Ok(json!({"ok": true, "msg": msg}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        let out = "[server] INFO: Device: [Google] Google Pixel 7 (Android 14)\r\n[server] INFO: List of apps:\r\n * 设置                             com.android.settings\r\n - Notes                          com.example.notes\r\n - My App Pro                     net.x.y_z\r\n - bad                            rm;-rf\r\n";
        let a = parse_list_apps(out);
        assert_eq!(a.len(), 3);
        assert_eq!(a[0], App { pkg: "com.android.settings".into(), name: "设置".into(), system: true });
        assert_eq!(a[2].name, "My App Pro");
        assert!(!a[1].system);
        assert_eq!(parse_pm("package:com.a.b\r\npackage:x.y\n"), vec!["com.a.b", "x.y"]);
        let ps = parse_ps("NAME\ninit\ncom.a.b\ncom.a.b:remote\n");
        assert!(ps.contains("com.a.b") && ps.contains("init"));
    }
}
