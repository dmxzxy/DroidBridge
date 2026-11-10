//! 截图、录屏与相册：媒体文件的存储、列表与上传。
//!
//! 媒体保存在当前节点（界面所连接的这台 DroidBridge）的媒体目录中，
//! 默认 `<data_dir>/media`，可用 `--media-dir` 或界面“设置”修改。
//! 修改目录与“在文件管理器中显示”只允许来自本机（127.0.0.1 / ::1）的请求。

use crate::AppState;
use axum::body::Bytes;
use axum::extract::connect_info::ConnectInfo;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tower::util::ServiceExt;

type Shared = Arc<AppState>;

/// 允许的录屏 / 截图扩展名（小写）
const SHOT_EXTS: &[&str] = &["png"];
const REC_EXTS: &[&str] = &["webm", "mp4"];
const MEDIA_EXTS: &[&str] = &["png", "webm", "mp4"];

/// 超过该时长（秒）没有写入的 `.part` 视为中断，改名为正式文件恢复。
const PART_RECOVER_SECS: u64 = 120;

#[derive(Serialize)]
pub struct MediaItem {
    pub name: String,
    pub kind: &'static str, // "shot" | "rec"
    pub device: String,
    pub size: u64,
    pub time: u64,
    pub url: String,
}

/// 把设备名做安全处理：只保留字母数字、`-`、`_`、中日韩文字，其余替换为 `_`，最长 40。
pub fn sanitize_device(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || is_cjk(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.chars().count() > 40 {
        out = out.chars().take(40).collect();
    }
    if out.is_empty() {
        out.push_str("device");
    }
    out
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF   // CJK 统一表意文字
        | 0x3400..=0x4DBF // 扩展 A
        | 0x3040..=0x30FF // 日文假名
        | 0xAC00..=0xD7AF // 谚文
    )
}

/// 校验媒体文件名：只接受单个文件名（不含 `/`、`\`、`..`、不以 `.` 开头），
/// 且扩展名属于允许的集合。
pub fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.starts_with('.') || name.len() > 255 {
        return false;
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return false;
    }
    // 只有一个 "."
    if name.matches('.').count() != 1 {
        return false;
    }
    let Some(ext) = name.rsplit('.').next() else {
        return false;
    };
    MEDIA_EXTS.contains(&ext.to_ascii_lowercase().as_str())
}

/// 本地时间 `YYYYMMDD-HHMMSS`（使用本地时区）
fn local_timestamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// 媒体目录：优先命令行 `--media-dir`，其次 settings.json，最后 `<data_dir>/media`。
pub async fn media_dir(st: &AppState) -> PathBuf {
    if let Some(d) = st.media_dir_override.clone() {
        return d;
    }
    load_settings(st).await.media_dir.unwrap_or_else(|| st.cfg.data_dir.join("media"))
}

#[derive(Default, Clone)]
pub struct Settings {
    pub media_dir: Option<PathBuf>,
}

fn settings_path(st: &AppState) -> PathBuf {
    st.cfg.data_dir.join("settings.json")
}

pub async fn load_settings(st: &AppState) -> Settings {
    let p = settings_path(st);
    let raw = tokio::fs::read(&p).await.ok();
    let media_dir = raw
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .and_then(|v| v.get("media_dir").and_then(|d| d.as_str()).map(PathBuf::from));
    Settings { media_dir }
}

async fn save_settings(st: &AppState, media_dir: PathBuf) -> Result<(), String> {
    let p = settings_path(st);
    if let Some(dir) = p.parent() {
        let _ = tokio::fs::create_dir_all(dir).await;
    }
    let v = serde_json::json!({ "media_dir": media_dir.to_string_lossy() });
    let data = serde_json::to_vec_pretty(&v).map_err(|e| e.to_string())?;
    tokio::fs::write(&p, data).await.map_err(|e| e.to_string())
}

/// 解析安全的文件名（校验目录穿越），并返回其在媒体目录中的绝对路径。
async fn resolve_path(st: &AppState, name: &str) -> Result<PathBuf, String> {
    if !valid_name(name) {
        return Err(tr!("无效的文件名", "Invalid file name"));
    }
    let dir = media_dir(st).await;
    let p = dir.join(name);
    let canon_dir = tokio::fs::canonicalize(&dir).await.unwrap_or_else(|_| dir.clone());
    let canon = tokio::fs::canonicalize(&p).await.unwrap_or_else(|_| p.clone());
    if !canon.starts_with(&canon_dir) {
        return Err(tr!("无效的文件名", "Invalid file name"));
    }
    Ok(p)
}

/// 判断地址是否为本机回环地址（127.0.0.1 / ::1）
pub fn is_loopback(addr: SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// 根据扩展名确定 kind
fn kind_of(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if SHOT_EXTS.contains(&ext.as_str()) {
        "shot"
    } else if REC_EXTS.contains(&ext.as_str()) {
        "rec"
    } else {
        "shot"
    }
}

fn content_type(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "webm" => "video/webm",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

fn file_time(p: &FsPath) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 从文件名提取设备名（去掉后缀与时间戳部分）
fn device_of(name: &str) -> String {
    let stem = &name[..name.rfind('.').unwrap_or(name.len())];
    match stem.rfind('_') {
        Some(i) => stem[..i].to_string(),
        None => stem.to_string(),
    }
}

// ---------------------------------------------------------------- 处理函数

/// 列出媒体目录中的文件（按时间倒序，忽略 .part 与其他扩展名）
pub async fn list(st: &AppState) -> Result<serde_json::Value, String> {
    let dir = media_dir(st).await;
    let _ = tokio::fs::create_dir_all(&dir).await;
    let mut items: Vec<MediaItem> = Vec::new();
    let mut rd = tokio::fs::read_dir(&dir).await.map_err(|e| e.to_string())?;
    while let Ok(Some(entry)) = rd.next_entry().await {
        let Some(name) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        if !valid_name(&name) {
            continue;
        }
        let Ok(meta) = entry.metadata().await else { continue };
        if !meta.is_file() {
            continue;
        }
        items.push(MediaItem {
            kind: kind_of(&name),
            device: device_of(&name),
            size: meta.len(),
            time: file_time(&entry.path()),
            url: format!("/api/media/file/{}", crate::util::url_encode(&name)),
            name,
        });
    }
    items.sort_by_key(|a| std::cmp::Reverse(a.time));
    Ok(serde_json::json!({
        "dir": dir.to_string_lossy(),
        "items": items,
    }))
}

/// 保存一张截图（PNG）。返回 MediaItem。
pub async fn save_shot(st: &AppState, device: &str, data: &[u8]) -> Result<MediaItem, String> {
    if data.is_empty() {
        return Err(tr!("文件为空", "File is empty"));
    }
    let dir = media_dir(st).await;
    let _ = tokio::fs::create_dir_all(&dir).await;
    let dev = sanitize_device(device);
    let (name, path) = unique_path(&dir, &dev, "png").await;
    tokio::fs::write(&path, data).await.map_err(|e| e.to_string())?;
    Ok(item_for(&name, &path))
}

/// 创建录屏 `.part` 文件，返回 id（即 base 文件名）。
pub async fn rec_start(st: &AppState, device: &str, ext: &str) -> Result<String, String> {
    if !REC_EXTS.contains(&ext) {
        return Err(tr!("不支持的录屏格式", "Unsupported recording format"));
    }
    let dir = media_dir(st).await;
    let _ = tokio::fs::create_dir_all(&dir).await;
    let dev = sanitize_device(device);
    let (name, path) = unique_path(&dir, &dev, ext).await;
    let part = path.with_file_name(format!("{name}.part"));
    tokio::fs::write(&part, &[]).await.map_err(|e| e.to_string())?;
    Ok(name)
}

/// 追加一块录屏数据。seq 从 0 开始，必须连续。
pub async fn rec_chunk(st: &AppState, id: &str, seq: u64, data: &[u8]) -> Result<(), String> {
    if !valid_name(id) {
        return Err(tr!("无效的文件名", "Invalid file name"));
    }
    let dir = media_dir(st).await;
    let part = dir.join(format!("{id}.part"));
    let count_path = dir.join(format!("{id}.part.seq"));
    let next = read_seq(&count_path).await;
    if seq != next {
        return Err("seq".to_string());
    }
    use tokio::io::AsyncWriteExt;
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&part)
        .await
        .map_err(|e| e.to_string())?;
    f.write_all(data).await.map_err(|e| e.to_string())?;
    write_seq(&count_path, seq + 1).await;
    Ok(())
}

async fn read_seq(p: &FsPath) -> u64 {
    tokio::fs::read_to_string(p)
        .await
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

async fn write_seq(p: &FsPath, v: u64) {
    let _ = tokio::fs::write(p, v.to_string()).await;
}

/// 结束录屏：把 `.part` 改名为正式文件，返回 item。
pub async fn rec_finish(st: &AppState, id: &str) -> Result<MediaItem, String> {
    if !valid_name(id) {
        return Err(tr!("无效的文件名", "Invalid file name"));
    }
    let dir = media_dir(st).await;
    let part = dir.join(format!("{id}.part"));
    let final_path = dir.join(id);
    tokio::fs::rename(&part, &final_path).await.map_err(|e| e.to_string())?;
    let count_path = dir.join(format!("{id}.part.seq"));
    let _ = tokio::fs::remove_file(&count_path).await;
    Ok(item_for(id, &final_path))
}

/// 放弃录屏并删除。
pub async fn rec_abort(st: &AppState, id: &str) -> Result<(), String> {
    if !valid_name(id) {
        return Err(tr!("无效的文件名", "Invalid file name"));
    }
    let dir = media_dir(st).await;
    let _ = tokio::fs::remove_file(dir.join(format!("{id}.part"))).await;
    let _ = tokio::fs::remove_file(dir.join(format!("{id}.part.seq"))).await;
    Ok(())
}

/// 删除一个媒体文件。
pub async fn delete(st: &AppState, name: &str) -> Result<(), String> {
    let p = resolve_path(st, name).await?;
    tokio::fs::remove_file(&p).await.map_err(|e| e.to_string())
}

/// 在系统文件管理器中打开并选中文件（仅本机）。
pub async fn reveal(st: &AppState, name: &str) -> Result<(), String> {
    let p = resolve_path(st, name).await?;
    let p = p.to_string_lossy().to_string();
    let res = if cfg!(windows) {
        std::process::Command::new("explorer").arg("/select,").arg(&p).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg("-R").arg(&p).spawn()
    } else {
        // Linux 无法在文件管理器中选中具体文件，打开所在目录
        let dir = std::path::Path::new(&p).parent().map(|d| d.to_string_lossy().to_string()).unwrap_or(p);
        std::process::Command::new("xdg-open").arg(dir).spawn()
    };
    match res {
        Ok(_) => Ok(()),
        Err(e) => Err(tr!("无法打开文件管理器：{e}", "Cannot open file manager: {e}")),
    }
}

/// 打开媒体目录本身（仅本机）。
pub async fn reveal_dir(st: &AppState) -> Result<(), String> {
    let dir = media_dir(st).await;
    let _ = tokio::fs::create_dir_all(&dir).await;
    let d = dir.to_string_lossy().to_string();
    let res = if cfg!(windows) {
        std::process::Command::new("explorer").arg(&d).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&d).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(&d).spawn()
    };
    match res {
        Ok(_) => Ok(()),
        Err(e) => Err(tr!("无法打开文件管理器：{e}", "Cannot open file manager: {e}")),
    }
}

fn item_for(name: &str, path: &FsPath) -> MediaItem {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    MediaItem {
        kind: kind_of(name),
        device: device_of(name),
        size,
        time: file_time(path),
        url: format!("/api/media/file/{}", crate::util::url_encode(name)),
        name: name.to_string(),
    }
}

/// 返回一个不冲突的文件路径与名字（同名时追加 `-2`、`-3`）。
/// 同时检查正式文件与 `.part`（同一秒内可能已经开始了一次同名录屏）。
async fn unique_path(dir: &FsPath, device: &str, ext: &str) -> (String, PathBuf) {
    let stem = format!("{device}_{}", local_timestamp());
    let mut name = format!("{stem}.{ext}");
    let mut n = 2;
    loop {
        let p = dir.join(&name);
        let part = dir.join(format!("{name}.part"));
        if tokio::fs::metadata(&p).await.is_err() && tokio::fs::metadata(&part).await.is_err() {
            break;
        }
        name = format!("{stem}-{n}.{ext}");
        n += 1;
    }
    (name.clone(), dir.join(&name))
}

/// 把超过 2 分钟没有写入的 `.part` 改名为正式文件（恢复中断的录屏），
/// 并删除对应的 `.part.seq`。截断后的分段 mp4 / webm 仍可播放。
pub async fn recover_stale_parts(st: &AppState) {
    let dir = media_dir(st).await;
    let Ok(mut rd) = tokio::fs::read_dir(&dir).await else { return };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let Some(name) = entry.file_name().to_str().map(String::from) else { continue };
        let path = entry.path();
        // 只处理 .part 文件；.part.seq 在对应的 .part 被恢复/删除时一并处理
        if !name.ends_with(".part") {
            continue;
        }
        let Ok(meta) = entry.metadata().await else { continue };
        let Ok(modified) = meta.modified() else { continue };
        let Ok(d) = SystemTime::now().duration_since(modified) else { continue };
        if d.as_secs() <= PART_RECOVER_SECS {
            continue;
        }
        // 去掉 .part 后缀得到正式文件名
        let id = &name[..name.len() - ".part".len()];
        if !valid_name(id) {
            // 非法文件名：无法恢复，直接删除残留的 .part 与 sidecar
            let _ = tokio::fs::remove_file(&path).await;
            let _ = tokio::fs::remove_file(dir.join(format!("{id}.part.seq"))).await;
            continue;
        }
        // 0 字节：开始录屏后一块数据都没传上来，无法播放，直接删除（连同 sidecar）
        if meta.len() == 0 {
            let _ = tokio::fs::remove_file(&path).await;
            let _ = tokio::fs::remove_file(dir.join(format!("{id}.part.seq"))).await;
            continue;
        }
        let final_path = dir.join(id);
        // 若同名正式文件已存在（理论上不会），删除这个残留的 .part
        if tokio::fs::metadata(&final_path).await.is_ok() {
            let _ = tokio::fs::remove_file(&path).await;
        } else if let Err(e) = tokio::fs::rename(&path, &final_path).await {
            tracing::warn!("failed to recover {name}: {e}");
        }
        let _ = tokio::fs::remove_file(dir.join(format!("{id}.part.seq"))).await;
    }
}

/// 后台任务：启动时先执行一次，之后每 60 秒扫描一次，恢复中断的录屏。
pub async fn recover_parts_loop(st: Shared) {
    loop {
        recover_stale_parts(&st).await;
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

// ---------------------------------------------------------------- HTTP 处理函数（注册到 router）

pub async fn media_list_handler(State(st): State<Shared>) -> Response {
    match list(&st).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_resp(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

pub async fn media_shot_handler(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let device = q.get("device").map(String::as_str).unwrap_or("");
    match save_shot(&st, device, &body).await {
        Ok(item) => Json(serde_json::json!(item)).into_response(),
        Err(e) => err_resp(StatusCode::BAD_REQUEST, &e),
    }
}

pub async fn media_rec_start_handler(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let device = q.get("device").map(String::as_str).unwrap_or("");
    let ext = q.get("ext").map(String::as_str).unwrap_or("webm");
    match rec_start(&st, device, ext).await {
        Ok(id) => Json(serde_json::json!({ "id": id })).into_response(),
        Err(e) => err_resp(StatusCode::BAD_REQUEST, &e),
    }
}

pub async fn media_rec_chunk_handler(
    State(st): State<Shared>,
    Path((id,)): Path<(String,)>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let seq: u64 = q.get("seq").and_then(|s| s.parse().ok()).unwrap_or(0);
    match rec_chunk(&st, &id, seq, &body).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) if e == "seq" => err_resp(
            StatusCode::CONFLICT,
            &tr!("分块序号不连续", "Chunk sequence out of order"),
        ),
        Err(e) => err_resp(StatusCode::BAD_REQUEST, &e),
    }
}

pub async fn media_rec_finish_handler(
    State(st): State<Shared>,
    Path((id,)): Path<(String,)>,
) -> Response {
    match rec_finish(&st, &id).await {
        Ok(item) => Json(serde_json::json!(item)).into_response(),
        Err(e) => err_resp(StatusCode::BAD_REQUEST, &e),
    }
}

pub async fn media_rec_abort_handler(
    State(st): State<Shared>,
    Path((id,)): Path<(String,)>,
) -> Response {
    match rec_abort(&st, &id).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => err_resp(StatusCode::BAD_REQUEST, &e),
    }
}

pub async fn media_delete_handler(
    State(st): State<Shared>,
    Path((name,)): Path<(String,)>,
) -> Response {
    match delete(&st, &name).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => err_resp(StatusCode::BAD_REQUEST, &e),
    }
}

pub async fn media_reveal_handler(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    if !is_loopback(addr) {
        return err_resp(
            StatusCode::FORBIDDEN,
            &tr!("仅本机可执行此操作", "Only local requests can do this"),
        );
    }
    let name = q.get("name").map(String::as_str).filter(|s| !s.is_empty());
    let result = match name {
        Some(n) => reveal(&st, n).await,
        None => reveal_dir(&st).await,
    };
    match result {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => err_resp(StatusCode::BAD_REQUEST, &e),
    }
}

pub async fn media_file_handler(
    State(st): State<Shared>,
    Path((name,)): Path<(String,)>,
    req: axum::extract::Request,
) -> Response {
    match resolve_path(&st, &name).await {
        Ok(p) => {
            let ct = content_type(&name);
            match tower_http::services::ServeFile::new(&p).oneshot(req).await {
                Ok(resp) => {
                    let (mut parts, body) = resp.into_parts();
                    parts.headers.insert(header::CONTENT_TYPE, ct.parse().unwrap());
                    parts.headers.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
                    Response::from_parts(parts, axum::body::Body::new(body))
                }
                Err(_) => err_resp(StatusCode::NOT_FOUND, &tr!("文件不存在", "File not found")),
            }
        }
        Err(e) => err_resp(StatusCode::BAD_REQUEST, &e),
    }
}

// ---------------------------------------------------------------- settings

pub async fn settings_get_handler(State(st): State<Shared>) -> Response {
    let dir = media_dir(&st).await;
    Json(serde_json::json!({ "media_dir": dir.to_string_lossy() })).into_response()
}

#[derive(serde::Deserialize)]
pub struct SettingsReq {
    pub media_dir: String,
}

pub async fn settings_put_handler(
    State(st): State<Shared>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<SettingsReq>,
) -> Response {
    if !is_loopback(addr) {
        return err_resp(
            StatusCode::FORBIDDEN,
            &tr!("仅本机可执行此操作", "Only local requests can do this"),
        );
    }
    if body.media_dir.trim().is_empty() {
        return err_resp(StatusCode::BAD_REQUEST, &tr!("目录不能为空", "Directory cannot be empty"));
    }
    let dir = PathBuf::from(body.media_dir.trim());
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return err_resp(StatusCode::BAD_REQUEST, &tr!("无法创建目录：{e}", "Cannot create directory: {e}"));
    }
    match save_settings(&st, dir.clone()).await {
        Ok(()) => Json(serde_json::json!({ "ok": true, "media_dir": dir.to_string_lossy() })).into_response(),
        Err(e) => err_resp(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

fn err_resp(code: StatusCode, msg: &str) -> Response {
    (code, Json(serde_json::json!({ "error": msg }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_device("Pixel 7"), "Pixel_7");
        assert_eq!(sanitize_device("中文设备-01"), "中文设备-01");
        assert_eq!(sanitize_device(""), "device");
        let long = "x".repeat(100);
        assert_eq!(sanitize_device(&long), "x".repeat(40));
        assert_eq!(sanitize_device("a/b\\c"), "a_b_c");
    }

    #[test]
    fn name_validation() {
        assert!(valid_name("dev_20260101-120000.png"));
        assert!(valid_name("dev_20260101-120000.webm"));
        assert!(valid_name("dev_20260101-120000.mp4"));
        assert!(!valid_name(""));
        assert!(!valid_name(".hidden.png"));
        assert!(!valid_name("a/b.png"));
        assert!(!valid_name("a\\b.png"));
        assert!(!valid_name("..png"));
        assert!(!valid_name("a..b.png"));
        assert!(!valid_name("a.txt"));
        assert!(!valid_name("a.png.exe"));
        assert!(!valid_name("a.b.png"));
    }

    #[test]
    fn device_extraction() {
        assert_eq!(device_of("Pixel_7_20260101-120000.png"), "Pixel_7");
        assert_eq!(device_of("中文-01_20260101-120000.webm"), "中文-01");
    }

    #[test]
    fn timestamp_format() {
        let ts = local_timestamp();
        assert_eq!(ts.len(), 15, "格式应为 YYYYMMDD-HHMMSS，实际：{ts}");
        assert_eq!(ts.chars().nth(8), Some('-'));
        assert!(ts.chars().all(|c| c.is_ascii_digit() || c == '-'));
    }

    fn test_state(dir: &std::path::Path) -> Arc<crate::AppState> {
        let cfg = crate::Config {
            bind: "127.0.0.1".into(),
            port: 0,
            https_port: 0,
            token: None,
            name: "test".into(),
            adb: std::path::PathBuf::from("adb"),
            data_dir: dir.to_path_buf(),
            server_jar: std::path::PathBuf::new(),
            discovery: false,
            media_dir: None,
        };
        Arc::new(crate::AppState {
            peers: crate::peers::PeerStore::load(dir.join("peers.json")),
            start_lock: tokio::sync::Mutex::new(()),
            jobs: std::sync::Mutex::new(std::collections::HashMap::new()),
            discovery: crate::discovery::Discovery::new(),
            wireless: crate::wireless::Wireless::load(dir.join("wireless.json")),
            uids: std::sync::Mutex::new(std::collections::HashMap::new()),
            media_dir_override: None,
            agent: crate::agent::AgentState::default(),
            cfg,
        })
    }

    #[tokio::test]
    async fn shot_list_delete_flow() {
        let dir = std::env::temp_dir().join(format!("droidbridge-media-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = test_state(&dir);

        // 保存截图
        let item = save_shot(&st, "Pixel 7", b"\x89PNG fake").await.unwrap();
        assert_eq!(item.kind, "shot");
        assert_eq!(item.device, "Pixel_7");
        assert!(item.name.ends_with(".png"));

        // 列表包含它
        let l1 = list(&st).await.unwrap();
        let items = l1["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["name"], item.name);

        // 文件名校验：拒绝穿越
        assert!(resolve_path(&st, "../evil.png").await.is_err());
        assert!(resolve_path(&st, "a/b.png").await.is_err());
        assert!(resolve_path(&st, "..\\evil.png").await.is_err());

        // 删除
        delete(&st, &item.name).await.unwrap();
        let l2 = list(&st).await.unwrap();
        assert_eq!(l2["items"].as_array().unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn rec_flow_and_seq_conflict() {
        let dir = std::env::temp_dir().join(format!("droidbridge-rec-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = test_state(&dir);

        let id = rec_start(&st, "Pixel 7", "webm").await.unwrap();
        assert!(id.ends_with(".webm"));
        // seq 0 和 1 连续上传
        rec_chunk(&st, &id, 0, b"chunk0").await.unwrap();
        rec_chunk(&st, &id, 1, b"chunk1").await.unwrap();
        // 不连续的 seq 返回错误
        assert!(rec_chunk(&st, &id, 3, b"bad").await.is_err());
        // 完成
        let item = rec_finish(&st, &id).await.unwrap();
        assert_eq!(item.kind, "rec");
        assert_eq!(item.name, id);
        // .part 已改名，无残留
        assert!(list(&st).await.unwrap()["items"].as_array().unwrap().iter().any(|i| i["name"] == id));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn recover_stale_part_renames() {
        let dir = std::env::temp_dir().join(format!("droidbridge-recover-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = test_state(&dir);

        let id = "Pixel_7_20260101-120000.webm";
        let mdir = dir.join("media");
        tokio::fs::create_dir_all(&mdir).await.unwrap();
        let part = mdir.join(format!("{id}.part"));
        let seq = mdir.join(format!("{id}.part.seq"));
        tokio::fs::write(&part, b"partial-video").await.unwrap();
        tokio::fs::write(&seq, b"3").await.unwrap();

        // 把 .part 的修改时间设到 10 分钟前，模拟中断
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        let f = std::fs::OpenOptions::new().write(true).open(&part).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(old)).unwrap();
        drop(f);

        recover_stale_parts(&st).await;

        // .part 被改名为正式文件，sidecar 被删除
        assert!(mdir.join(id).exists());
        assert!(!part.exists());
        assert!(!seq.exists());

        // 恢复后的文件出现在列表中
        assert!(list(&st).await.unwrap()["items"].as_array().unwrap().iter().any(|i| i["name"] == id));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn recover_deletes_empty_part() {
        let dir = std::env::temp_dir().join(format!("droidbridge-recover-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = test_state(&dir);

        let id = "Pixel_7_20260101-120000.webm";
        let mdir = dir.join("media");
        tokio::fs::create_dir_all(&mdir).await.unwrap();
        let part = mdir.join(format!("{id}.part"));
        let seq = mdir.join(format!("{id}.part.seq"));
        // 0 字节：开始录屏后一块数据都没传上来
        tokio::fs::write(&part, b"").await.unwrap();
        tokio::fs::write(&seq, b"0").await.unwrap();

        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        let f = std::fs::OpenOptions::new().write(true).open(&part).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(old)).unwrap();
        drop(f);

        recover_stale_parts(&st).await;

        // 0 字节 .part 被直接删除，不恢复成空文件
        assert!(!part.exists());
        assert!(!seq.exists());
        assert!(!mdir.join(id).exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unique_path_avoids_existing_part() {
        let dir = std::env::temp_dir().join(format!("droidbridge-unique-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let (name1, _) = unique_path(&dir, "Dev", "webm").await;
        // 同一秒内已经开始了一次同名（.part 存在）
        tokio::fs::write(dir.join(format!("{name1}.part")), b"x").await.unwrap();

        let (name2, _) = unique_path(&dir, "Dev", "webm").await;
        assert_ne!(name1, name2, "unique_path 不应返回已存在 .part 的名字");
        // 返回的名字对应的 .part 也不应存在
        assert!(tokio::fs::metadata(dir.join(format!("{name2}.part"))).await.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
