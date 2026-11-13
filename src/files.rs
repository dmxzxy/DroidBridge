//! 设备文件管理：浏览、上传、下载、新建文件夹、重命名、删除。
//!
//! 只允许访问共享存储（/sdcard 及其真实路径 /storage/emulated/0），路径中的 `..` 与引号会被拒绝，
//! 避免拼进设备端 shell 时被利用。

use crate::{adb, AppState};
use std::path::PathBuf;
use axum::body::Body;
use axum::response::Response;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

/// 允许浏览的根目录（前者是链接，后者是真实路径）
const ROOTS: &[&str] = &["/sdcard", "/storage/emulated/0"];

#[derive(Deserialize)]
pub struct ListReq {
    pub serial: String,
    #[serde(default)]
    pub path: String,
}

#[derive(Deserialize)]
pub struct TwoReq {
    pub serial: String,
    pub path: String,
    #[serde(default)]
    pub to: String,
}

#[derive(Deserialize)]
pub struct DeleteReq {
    pub serial: String,
    pub paths: Vec<String>,
}

pub fn err_resp(code: axum::http::StatusCode, msg: &str) -> Response {
    (code, [(axum::http::header::CONTENT_TYPE, "application/json")], json!({"error": msg}).to_string()).into_response()
}

use axum::response::IntoResponse;

/// 校验并规范化设备路径；不在允许范围内时返回错误说明
pub fn check_path(p: &str, allow_root: bool) -> Result<String, String> {
    let p = p.trim();
    if p.is_empty() {
        return Ok("/sdcard".into());
    }
    if !p.starts_with('/') {
        return Err(tr!("路径必须是以 / 开头的绝对路径", "Path must be absolute (start with /)"));
    }
    if p.contains('\'') || p.contains('\\') || p.contains('\n') || p.contains('\r') || p.contains('`') || p.contains('$') {
        return Err(tr!("路径包含不支持的字符", "Path contains unsupported characters"));
    }
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => return Err(tr!("路径不能包含 ..", "Path cannot contain ..")),
            s => parts.push(s),
        }
    }
    let norm = format!("/{}", parts.join("/"));
    if !ROOTS.iter().any(|r| norm == *r || norm.starts_with(&format!("{r}/"))) {
        return Err(tr!("只能浏览共享存储（/sdcard）", "Only shared storage (/sdcard) can be browsed"));
    }
    if !allow_root && ROOTS.contains(&norm.as_str()) {
        return Err(tr!("不能对该目录执行此操作", "This operation is not allowed on this folder"));
    }
    Ok(norm)
}

fn parent_of(p: &str) -> Option<String> {
    let p = p.trim_end_matches('/');
    if ROOTS.contains(&p) {
        return None;
    }
    Some(match p.rfind('/') {
        Some(0) | None => "/sdcard".to_string(),
        Some(i) => p[..i].to_string(),
    })
}

/// 文件名不能包含 / 和控制字符
fn check_name(n: &str) -> Result<String, String> {
    let n = n.trim();
    if n.is_empty() {
        return Err(tr!("名称不能为空", "Name cannot be empty"));
    }
    if n == "." || n == ".." || n.contains('/') || n.chars().any(|c| c.is_control()) {
        return Err(tr!("名称不合法", "Invalid name"));
    }
    Ok(n.to_string())
}

async fn sh(st: &AppState, serial: &str, cmd: &str, secs: u64) -> Result<String, String> {
    adb::run_timeout(&st.cfg.adb, &["-s", serial, "shell", cmd], Duration::from_secs(secs)).await
}

/// 名称、大小、修改时间（秒）
#[derive(Debug, PartialEq)]
struct Entry {
    name: String,
    dir: bool,
    size: u64,
    mtime: u64,
}

/// 解析 `stat -c '%F|%s|%Y|%n'` 的输出（toybox stat）。
/// 文件名里可能有 |，所以只按前 3 个 | 切分；符号链接按文件处理。
fn parse_list(out: &str) -> Vec<Entry> {
    out.lines()
        .filter_map(|l| {
            let l = l.strip_suffix('\r').unwrap_or(l);
            if l.starts_with("stat: ") {
                return None; // 通配符没匹配到时 stat 的报错
            }
            let mut it = l.splitn(4, '|');
            let (Some(kind), Some(size), Some(mtime), Some(name)) = (it.next(), it.next(), it.next(), it.next()) else { return None };
            if name.is_empty() || name == "." || name == ".." {
                return None;
            }
            Some(Entry { name: name.to_string(), dir: kind == "directory", size: size.parse().unwrap_or(0), mtime: mtime.parse().unwrap_or(0) })
        })
        .collect()
}

fn sort_entries(list: &mut [Entry]) {
    list.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.name.cmp(&b.name)));
}

pub async fn list(st: &AppState, r: ListReq) -> Result<serde_json::Value, String> {
    let path = check_path(&r.path, true)?;
    // 一次 adb 调用：进入目录、一次 stat 读出所有项（含隐藏文件），再带上剩余空间
    let cmd = format!(
        "if [ ! -e '{path}' ]; then echo '@@missing'; elif ! cd '{path}' 2>/dev/null; then echo '@@denied'; else stat -c '%F|%s|%Y|%n' -- * .[!.]* ..?* 2>/dev/null; echo '@@df'; df -k /sdcard 2>/dev/null | tail -1; fi; true"
    );
    let out = sh(st, &r.serial, &cmd, 30).await?;
    if out.contains("@@missing") {
        return Err(tr!("文件夹不存在：{path}", "Folder not found: {path}"));
    }
    if out.contains("@@denied") {
        return Err(tr!("没有权限打开：{path}（安卓 11 起应用的私有目录无法通过 adb 访问）", "Permission denied: {path} (since Android 11, app-private folders are not reachable over adb)"));
    }
    let (listing, df) = out.split_once("@@df").unwrap_or((out.as_str(), ""));
    let mut entries = parse_list(listing);
    sort_entries(&mut entries);
    let items: Vec<_> = entries.iter().map(|e| json!({"name": e.name, "dir": e.dir, "size": e.size, "mtime": e.mtime})).collect();
    // 剩余空间（读不到就不显示）；df -k 的列：文件系统 1K 块总数 已用 可用
    let v: Vec<u64> = df.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
    let space = (v.len() >= 3).then(|| json!({"total": v[0] * 1024, "free": v[2] * 1024}));
    Ok(json!({"path": path, "parent": parent_of(&path), "items": items, "space": space}))
}

pub async fn mkdir(st: &AppState, r: TwoReq) -> Result<serde_json::Value, String> {
    let dir = check_path(&r.path, true)?;
    let name = check_name(&r.to)?;
    let target = format!("{dir}/{name}");
    // mkdir 的退出码不可靠（目录已存在也算失败），直接看结果
    let _ = sh(st, &r.serial, &format!("mkdir -p '{target}' 2>&1"), 20).await;
    let out = sh(st, &r.serial, &format!("[ -d '{target}' ] && echo ok"), 15).await?;
    if !out.contains("ok") {
        return Err(tr!("无法创建文件夹（可能没有权限或名称重复）", "Cannot create the folder (no permission or the name exists)"));
    }
    Ok(json!({"ok": true, "path": target}))
}

pub async fn rename(st: &AppState, r: TwoReq) -> Result<serde_json::Value, String> {
    let from = check_path(&r.path, false)?;
    let dir = parent_of(&from).unwrap_or_else(|| "/sdcard".into());
    let name = check_name(&r.to)?;
    let to = format!("{dir}/{name}");
    if to == from {
        return Ok(json!({"ok": true, "path": to}));
    }
    let out = sh(st, &r.serial, &format!("mv '{from}' '{to}' 2>&1 && echo ok"), 30).await?;
    if !out.contains("ok") {
        let e = out.trim();
        return Err(tr!("重命名失败：{e}", "Rename failed: {e}"));
    }
    Ok(json!({"ok": true, "path": to}))
}

pub async fn delete(st: &AppState, r: DeleteReq) -> Result<serde_json::Value, String> {
    if r.paths.is_empty() {
        return Err(tr!("没有选中文件", "Nothing selected"));
    }
    let mut failed = Vec::new();
    for p in &r.paths {
        let p = check_path(p, false)?;
        let out = sh(st, &r.serial, &format!("rm -rf '{p}' 2>&1; [ -e '{p}' ] && echo still-there"), 30).await?;
        if out.contains("still-there") {
            failed.push(p);
        }
    }
    if !failed.is_empty() {
        let list = failed.join(", ");
        return Err(tr!("无法删除：{list}", "Cannot delete: {list}"));
    }
    Ok(json!({"ok": true}))
}

/// 上传：写到本机临时文件，再 adb push 到设备（先推送到临时名，成功后再改名，避免半截文件）
pub async fn upload(st: &AppState, serial: &str, dir: &str, name: &str, body: &[u8]) -> Result<serde_json::Value, String> {
    let dir = check_path(dir, true)?;
    let name = check_name(name)?;
    if body.is_empty() {
        return Err(tr!("文件为空", "File is empty"));
    }
    let tmp = st.cfg.data_dir.join("uploads");
    let _ = tokio::fs::create_dir_all(&tmp).await;
    let local = tmp.join(format!("up-{:08x}.bin", crate::util::random_scid()));
    tokio::fs::write(&local, body).await.map_err(|e| tr!("写入临时文件失败：{e}", "Cannot write the temporary file: {e}"))?;
    let local_s = local.to_string_lossy().into_owned();
    let part = format!("{dir}/.{name}.droidbridge-part");
    let dest = format!("{dir}/{name}");
    let r = adb::run_timeout(&st.cfg.adb, &["-s", serial, "push", &local_s, &part], Duration::from_secs(600)).await;
    let _ = tokio::fs::remove_file(&local).await;
    r?;
    let out = sh(st, serial, &format!("mv '{part}' '{dest}' 2>&1 && echo ok"), 30).await?;
    if !out.contains("ok") {
        let _ = sh(st, serial, &format!("rm -f '{part}'"), 15).await;
        return Err(tr!("无法写入 {dest}（可能没有权限）", "Cannot write {dest} (maybe no permission)"));
    }
    Ok(json!({"ok": true, "path": dest}))
}

/// 安装设备上的 APK（pm install 直接读设备上的文件，不用先下载）
pub async fn install(st: &AppState, r: TwoReq) -> Result<serde_json::Value, String> {
    let path = check_path(&r.path, false)?;
    if !path.to_ascii_lowercase().ends_with(".apk") {
        return Err(tr!("只能安装 .apk 文件", "Only .apk files can be installed"));
    }
    // /sdcard 下的文件 pm 不一定有权限直接读：先复制到 /data/local/tmp
    let tmp = format!("/data/local/tmp/droidbridge-{:08x}.apk", crate::util::random_scid());
    let out = sh(st, &r.serial, &format!("cp '{path}' '{tmp}' && pm install -r -t '{tmp}' 2>&1; rm -f '{tmp}'"), 300).await?;
    if out.contains("Success") {
        Ok(json!({"ok": true, "msg": tr!("已安装", "Installed")}))
    } else {
        let e = out.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        Err(tr!("安装失败：{e}", "Install failed: {e}"))
    }
}

/// 预览 / 缩略图：直接用 exec-out cat 读到内存（不写临时文件，比 pull 快），
/// 图片按 size 缩放成 JPEG。文本最多读 512 KB，原图最多 40 MB。
pub async fn preview(st: &AppState, serial: &str, path: &str, thumb: Option<u32>) -> Result<Response, String> {
    let path = check_path(path, false)?;
    let lower = path.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    let is_img = matches!(ext, "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp");
    let limit: u64 = if is_img { 40 << 20 } else { 512 << 10 };
    // head -c 限制读取量；toybox 的 head 支持 -c
    let cmd = format!("head -c {limit} '{path}'");
    let data = adb::run_bytes(&st.cfg.adb, &["-s", serial, "exec-out", &cmd], Duration::from_secs(60)).await?;
    let mime = match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "json" => "application/json; charset=utf-8",
        _ => "text/plain; charset=utf-8",
    };
    let (body, mime) = match thumb.filter(|_| is_img) {
        Some(size) => match image::load_from_memory(&data) {
            Ok(img) => {
                let small = img.thumbnail(size, size);
                let mut jpg = Vec::new();
                small
                    .to_rgb8()
                    .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut std::io::Cursor::new(&mut jpg), 78))
                    .map_err(|e| e.to_string())?;
                (jpg, "image/jpeg")
            }
            Err(_) => (data, mime),
        },
        None => (data, mime),
    };
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, mime.to_string()),
            // 同一路径的缩略图在会话期间可以复用
            (axum::http::header::CACHE_CONTROL, if thumb.is_some() { "private, max-age=600".to_string() } else { "no-cache".to_string() }),
        ],
        body,
    )
        .into_response())
}

/// 下载：adb pull 到临时文件后以流的方式返回（大文件不占内存）
pub async fn download(st: &AppState, serial: &str, path: &str) -> Result<Response, String> {
    let path = check_path(path, false)?;
    let name = path.rsplit('/').next().unwrap_or("file").to_string();
    let tmp = st.cfg.data_dir.join("uploads");
    let _ = tokio::fs::create_dir_all(&tmp).await;
    let local = tmp.join(format!("dl-{:08x}.bin", crate::util::random_scid()));
    let local_s = local.to_string_lossy().into_owned();
    adb::run_timeout(&st.cfg.adb, &["-s", serial, "pull", &path, &local_s], Duration::from_secs(900))
        .await
        .map_err(|e| tr!("下载失败：{e}", "Download failed: {e}"))?;
    let file = tokio::fs::File::open(&local).await.map_err(|e| tr!("读取临时文件失败：{e}", "Cannot read the temporary file: {e}"))?;
    let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    // 流式发送；响应结束（或被中断）时由 Cleanup 删除临时文件
    struct Cleanup(Option<PathBuf>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(p) = self.0.take() {
                tokio::spawn(async move {
                    let _ = tokio::fs::remove_file(p).await;
                });
            }
        }
    }
    let guard = Cleanup(Some(local));
    let stream = futures_util::stream::unfold(Some((file, guard)), |state| async move {
        let (mut f, guard) = state?;
        let mut buf = vec![0u8; 64 * 1024];
        use tokio::io::AsyncReadExt;
        match f.read(&mut buf).await {
            Ok(0) | Err(_) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<Vec<u8>, std::io::Error>(buf), Some((f, guard))))
            }
        }
    });
    let ascii: String = name.chars().map(|c| if c.is_ascii() && c != '"' && c != '\\' { c } else { '_' }).collect();
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (axum::http::header::CONTENT_LENGTH, len.to_string()),
            (axum::http::header::CONTENT_DISPOSITION, format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{}", crate::util::url_encode(&name))),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(check_path("", true).unwrap(), "/sdcard");
        assert!(check_path("/sdcard/DCIM/../DCIM", true).is_err(), ".. 直接拒绝");
        assert_eq!(check_path("/sdcard//DCIM/", true).unwrap(), "/sdcard/DCIM");
        assert_eq!(check_path("/storage/emulated/0/Download/", true).unwrap(), "/storage/emulated/0/Download");
        assert!(check_path("/data/local/tmp", true).is_err());
        assert!(check_path("/sdcard/../data", true).is_err());
        assert!(check_path("/sdcard/a';reboot", true).is_err());
        assert!(check_path("/sdcard", false).is_err(), "根目录不能删除/重命名");
        assert_eq!(check_path("/sdcard/x", false).unwrap(), "/sdcard/x");
        assert_eq!(parent_of("/sdcard/DCIM"), Some("/sdcard".into()));
        assert_eq!(parent_of("/sdcard/DCIM/Camera"), Some("/sdcard/DCIM".into()));
        assert_eq!(parent_of("/sdcard"), None);
        assert_eq!(parent_of("/storage/emulated/0"), None);
        assert!(check_name("a/b").is_err());
        assert!(check_name("..").is_err());
        assert_eq!(check_name(" 照片 ").unwrap(), "照片");
    }

    #[test]
    fn listing() {
        let out = "directory|4096|1700000000|DCIM\r\nregular file|12345|1700000001|a.jpg\nregular empty file|0|1700000002|就是|名字 一.txt\nsymbolic link|20|1700000003|link\nstat: '.[!.]*': No such file or directory\nbroken line\n";
        let mut e = parse_list(out);
        assert_eq!(e.len(), 4);
        sort_entries(&mut e);
        assert!(e[0].dir && e[0].name == "DCIM");
        assert_eq!(e[1].name, "a.jpg");
        assert_eq!(e[1].size, 12345);
        assert_eq!(e[3].name, "就是|名字 一.txt", "名称里的 | 和空格保留");
    }
}
