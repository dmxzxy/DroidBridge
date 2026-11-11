//! 桌面版自我更新：检查 GitHub Releases 上是否有新版本，下载后替换程序并重启。
//!
//! 只用系统自带的 curl / PowerShell / tar，不引入额外的网络依赖。
//! 替换在程序退出后进行：先写一个临时脚本，脚本等本进程结束，解压新版本、覆盖安装目录，再启动。

use crate::AppState;
use serde_json::json;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

const REPO: &str = "https://github.com/dmxzxy/DroidBridge";

/// 当前版本
pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// "0.6.0" → [0, 6, 0]（忽略前缀 v 和后面的说明）
fn parts(v: &str) -> Vec<u32> {
    v.trim_start_matches('v')
        .split(['.', '-', '+', ' '])
        .take(3)
        .map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>())
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

/// a 是否比 b 新
pub fn newer(a: &str, b: &str) -> bool {
    let (a, b) = (parts(a), parts(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// 本平台桌面版的安装包文件名
pub fn asset_name(version: &str) -> Option<String> {
    let v = version.trim_start_matches('v');
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", _) => format!("DroidBridge-{v}-windows-x64.zip"),
        ("macos", _) => format!("DroidBridge-{v}-macos-universal.zip"),
        ("linux", "x86_64" | "amd64") => format!("droidbridge-desktop-{v}-linux-x86_64.tar.gz"),
        _ => return None,
    })
}

fn release_base() -> String {
    std::env::var("DROIDBRIDGE_RELEASE_BASE").unwrap_or_else(|_| REPO.to_string())
}

async fn curl(args: &[String]) -> Result<String, String> {
    let out = Command::new("curl")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| tr!("无法运行 curl：{e}", "Cannot run curl: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let e = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if e.is_empty() { tr!("网络请求失败（curl 退出码非 0）", "Request failed (curl returned an error)") } else { e })
    }
}

/// 重启时沿用原有的命令行参数
fn args_for_restart() -> Vec<String> {
    std::env::args().skip(1).collect()
}

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

/// 最新版本号：跟随 releases/latest 的重定向，避免使用有频率限制的 API
async fn latest() -> Result<String, String> {
    let url = format!("{}/releases/latest", release_base());
    let out = curl(&["-sS".into(), "-L".into(), "-o".into(), null_device().into(), "-w".into(), "%{url_effective}".into(), url]).await?;
    let tag = out.trim().rsplit('/').next().unwrap_or("").to_string();
    if tag.is_empty() || !tag.starts_with('v') || !tag.chars().nth(1).is_some_and(|c| c.is_ascii_digit()) {
        return Err(tr!("无法获取最新版本", "Cannot determine the latest version"));
    }
    Ok(tag)
}

/// 检查更新（不改动任何文件）
pub async fn check(_st: &AppState) -> Result<serde_json::Value, String> {
    let cur = current();
    let candidate = latest().await?;
    let ver = candidate.trim_start_matches('v').to_string();
    let asset = asset_name(&ver);
    Ok(json!({
        "current": cur,
        "latest": ver,
        "tag": candidate,
        "newer": newer(&ver, cur),
        "asset": asset,
        "supported": asset.is_some() && asset_supported(),
        "page": format!("{}/releases/tag/{candidate}", release_base()),
        "download": asset.map(|a| format!("{}/releases/download/{candidate}/{a}", release_base())),
    }))
}

/// 能否自动替换：需要能找到当前程序的安装目录，并且可写
fn asset_supported() -> bool {
    install_target().is_some()
}

fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("DroidBridge"))
}

/// 要替换的目标：Windows 为 exe 所在目录；macOS 为 .app 包；Linux 为 exe 所在目录
fn install_target() -> Option<PathBuf> {
    let exe = exe_path();
    if cfg!(target_os = "macos") {
        // .../DroidBridge.app/Contents/MacOS/DroidBridge → .../DroidBridge.app
        let app = exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app"))?;
        return writable(app.parent()?).then(|| app.to_path_buf());
    }
    let dir = exe.parent()?.to_path_buf();
    writable(&dir).then_some(dir)
}

/// 在目录里建一个临时文件测试是否可写
fn writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(format!(".droidbridge-write-test-{:08x}", crate::util::random_scid()));
    match std::fs::write(&probe, b"x") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 下载并安装新版本；成功后本进程会在脚本接管后退出
pub async fn apply(st: &AppState) -> Result<serde_json::Value, String> {
    let info = check(st).await?;
    if !info["newer"].as_bool().unwrap_or(false) {
        return Err(tr!("已经是最新版本", "Already up to date"));
    }
    if !info["supported"].as_bool().unwrap_or(false) {
        let page = info["page"].as_str().unwrap_or(REPO);
        return Err(tr!(
            "当前安装位置不支持自动更新，请从 {page} 下载新版本",
            "This install location cannot be updated automatically; download the new version from {page}"
        ));
    }
    let Some(url) = info["download"].as_str().map(String::from) else {
        return Err(tr!("这个版本没有提供本平台的安装包", "This release has no package for this platform"));
    };
    let target = install_target().ok_or_else(|| tr!("无法确定安装目录", "Cannot determine the install directory"))?;
    let ver = info["latest"].as_str().unwrap_or("new").to_string();

    // 下载到数据目录
    let dir = st.cfg.data_dir.join("update");
    tokio::fs::create_dir_all(&dir).await.map_err(|e| tr!("无法创建目录：{e}", "Cannot create the directory: {e}"))?;
    let file = dir.join(url.rsplit('/').next().unwrap_or("update.zip"));
    let file_s = file.to_string_lossy().into_owned();
    curl(&["-sS".into(), "-L".into(), "--max-time".into(), "1800".into(), "-o".into(), file_s.clone(), url.clone()]).await?;
    let size = tokio::fs::metadata(&file).await.map(|m| m.len()).unwrap_or(0);
    if size < 1024 * 1024 {
        let _ = tokio::fs::remove_file(&file).await;
        return Err(tr!("下载的文件不完整（{size} 字节）", "The downloaded file is incomplete ({size} bytes)"));
    }

    let script = if cfg!(windows) { write_windows_script(&target, &file)? } else { write_unix_script(&target, &file)? };
    // 脱离父进程启动脚本，然后本进程退出
    let mut cmd = Command::new(if cfg!(windows) { "cmd" } else { "sh" });
    if cfg!(windows) {
        cmd.args(["/C", "start", "", "/MIN"]).arg(&script);
    } else {
        cmd.arg(&script);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.spawn().map_err(|e| tr!("无法启动更新脚本：{e}", "Cannot start the update script: {e}"))?;
    tracing::info!("update to {ver}: restarting via {}", script.display());

    let script = script.to_string_lossy().into_owned();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        tracing::info!("exiting for update: {script}");
        std::process::exit(0);
    });
    Ok(json!({"ok": true, "version": ver, "size": size}))
}

/// Windows：等本进程退出 → 解压 → 覆盖 → 启动
fn write_windows_script(target: &std::path::Path, archive: &std::path::Path) -> Result<PathBuf, String> {
    let exe = exe_path();
    let name = exe.file_name().and_then(|n| n.to_str()).unwrap_or("DroidBridge.exe").to_string();
    let tmp = std::env::temp_dir().join("droidbridge-update");
    let pid = std::process::id();
    let q = |p: &std::path::Path| p.to_string_lossy().replace('\'', "''");
    let script = format!(
        "@echo off\r\n\
         setlocal\r\n\
         set \"DEST={dest}\"\r\n\
         set \"ZIP={zip}\"\r\n\
         set \"TMP={tmp}\"\r\n\
         set \"EXE={name}\"\r\n\
         :wait\r\n\
         taskkill /F /PID {pid} >nul 2>&1\r\n\
         ping -n 2 127.0.0.1 >nul\r\n\
         if exist \"%DEST%\\%EXE%\" ren \"%DEST%\\%EXE%\" \"%EXE%.old\" >nul 2>&1\r\n\
         if exist \"%DEST%\\%EXE%\" goto wait\r\n\
         if exist \"%TMP%\" rmdir /s /q \"%TMP%\"\r\n\
         powershell -NoProfile -ExecutionPolicy Bypass -Command \"Expand-Archive -LiteralPath '{zipq}' -DestinationPath '{tmpq}' -Force\"\r\n\
         xcopy \"%TMP%\\*\" \"%DEST%\\\" /E /Y /I /Q >nul\r\n\
         del \"%DEST%\\%EXE%.old\" >nul 2>&1\r\n\
         start \"\" \"%DEST%\\%EXE%\" {args}\r\n\
         (goto) 2>nul & del \"%~f0\"\r\n",
        dest = target.display(),
        zip = archive.display(),
        tmp = tmp.display(),
        name = name,
        pid = pid,
        zipq = q(archive),
        tmpq = q(&tmp),
        args = args_for_restart().iter().map(|a| format!("\"{}\"", a.replace('"', ""))).collect::<Vec<_>>().join(" "),
    );
    let file = std::env::temp_dir().join("droidbridge-update.bat");
    std::fs::write(&file, script).map_err(|e| tr!("无法写入更新脚本：{e}", "Cannot write the update script: {e}"))?;
    Ok(file)
}

/// macOS / Linux：等本进程退出 → 解压 → 覆盖 → 启动
fn write_unix_script(target: &std::path::Path, archive: &std::path::Path) -> Result<PathBuf, String> {
    let exe = exe_path();
    let tmp = std::env::temp_dir().join("droidbridge-update");
    let pid = std::process::id();
    let mac = cfg!(target_os = "macos");
    let dq = |p: &std::path::Path| p.to_string_lossy().replace('"', "\\\"");
    let body = if mac {
        // target 是 .app 包
        format!(
            "TARGET=\"{target}\"\nARCHIVE=\"{archive}\"\nTMP=\"{tmp}\"\n\
             while kill -0 {pid} 2>/dev/null; do sleep 1; done\n\
             rm -rf \"$TMP\" && mkdir -p \"$TMP\" && unzip -q \"$ARCHIVE\" -d \"$TMP\"\n\
             rm -rf \"$TARGET\" && cp -R \"$TMP/DroidBridge.app\" \"$TARGET\"\n\
             \"$TARGET/Contents/MacOS/DroidBridge\" {args} >/dev/null 2>&1 &\n",
            target = dq(target),
            archive = dq(archive),
            tmp = dq(&tmp),
            args = args_for_restart().iter().map(|x| format!("\"{x}\"")).collect::<Vec<_>>().join(" "),
        )
    } else {
        format!(
            "TARGET=\"{target}\"\nARCHIVE=\"{archive}\"\nTMP=\"{tmp}\"\nEXE=\"{exe}\"\n\
             while kill -0 {pid} 2>/dev/null; do sleep 1; done\n\
             rm -rf \"$TMP\" && mkdir -p \"$TMP\" && tar -xzf \"$ARCHIVE\" -C \"$TMP\"\n\
             SRCDIR=$(find \"$TMP\" -maxdepth 2 -name droidbridge-desktop -exec dirname {{}} \\; | head -1)\n\
             [ -n \"$SRCDIR\" ] && cp -Rf \"$SRCDIR\"/. \"$TARGET\"/\n\
             chmod +x \"$EXE\" 2>/dev/null\n\
             (cd \"$TARGET\" && nohup \"$EXE\" {args} >/dev/null 2>&1 &)\n",
            target = dq(target),
            archive = dq(archive),
            tmp = dq(&tmp),
            exe = dq(&exe),
            args = args_for_restart().iter().map(|a| format!("'{}'", a.replace('\'', "'\\''"))).collect::<Vec<_>>().join(" "),
        )
    };
    let file = std::env::temp_dir().join("droidbridge-update.sh");
    std::fs::write(&file, format!("#!/bin/sh\n{body}rm -f \"$0\"\n")).map_err(|e| tr!("无法写入更新脚本：{e}", "Cannot write the update script: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("v0.7.0", "0.6.0"));
        assert!(newer("0.10.1", "0.9.9"));
        assert!(newer("1.0", "0.99.9"));
        assert!(!newer("0.6.0", "0.6.0"));
        assert!(!newer("0.6.0", "0.7.0"));
        assert!(!newer("v0.6.0-beta", "0.6.0"));
        assert_eq!(parts("v1.2.3-beta"), vec![1, 2, 3]);
    }

    #[test]
    fn assets() {
        let a = asset_name("0.7.0").unwrap();
        if cfg!(windows) {
            assert_eq!(a, "DroidBridge-0.7.0-windows-x64.zip");
        }
        assert_eq!(asset_name("v0.7.0").unwrap(), asset_name("0.7.0").unwrap());
    }
}
