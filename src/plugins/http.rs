//! 插件 HTTP/WS 接口（SPEC 4.1）与 `/plugin-web` 静态文件服务。
//!
//! 带 `?peer=` 的请求转发给远程电脑；`manage` 与设置修改只允许本机请求。

use super::*;
use crate::{local_or_peer, peers, util, AppState};
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::net::SocketAddr;

type Shared = Arc<AppState>;

fn err(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({"error": msg.into()}))).into_response()
}

/// 转发请求时保留原始查询参数（去掉 peer / token / lang）
fn fwd_path(base: &str, q: &HashMap<String, String>) -> String {
    let mut parts: Vec<String> = q
        .iter()
        .filter(|(k, _)| k.as_str() != "peer" && k.as_str() != "token" && k.as_str() != "lang")
        .map(|(k, v)| format!("{}={}", util::url_encode(k), util::url_encode(v)))
        .collect();
    parts.sort();
    if parts.is_empty() { base.to_string() } else { format!("{base}?{}", parts.join("&")) }
}

fn peer_of(q: &HashMap<String, String>) -> Option<&str> {
    q.get("peer").map(String::as_str).filter(|p| !p.is_empty() && *p != "local")
}

// ---------------------------------------------------------------- 列表与管理

pub async fn plugins_get(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    let manage = crate::is_local_request(&st, &headers, Some(addr.ip()), &q);
    let safe = st.cfg.no_plugins;
    let st2 = st.clone();
    local_or_peer(&st, &q, "GET", "/api/plugins", b"", || async move { Ok(st2.plugins.list_json(manage, safe)) }).await
}

#[derive(Deserialize)]
struct ManageReq {
    #[serde(default)]
    id: Option<String>,
    action: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    values: Option<Map<String, Value>>,
}

/// 管理操作：启用、停用、重载、设置、打开目录、新建、重新扫描。**只允许本机请求。**
pub async fn plugins_manage(
    State(st): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !crate::is_local_request(&st, &headers, Some(addr.ip()), &q) || peer_of(&q).is_some() {
        return err(StatusCode::FORBIDDEN, tr!("只能在这台电脑上管理插件", "Plugins can only be managed on this computer"));
    }
    let Ok(req) = serde_json::from_slice::<ManageReq>(&body) else {
        return err(StatusCode::BAD_REQUEST, tr!("请求格式不正确", "Invalid request"));
    };
    let plugin = |id: &Option<String>| -> Result<Arc<Plugin>, Response> {
        let id = id.as_deref().unwrap_or("");
        st.plugins.get(id).ok_or_else(|| err(StatusCode::NOT_FOUND, tr!("插件不存在", "Plugin not found")))
    };
    match req.action.as_str() {
        "enable" => {
            let ph = match plugin(&req.id) {
                Ok(p) => p,
                Err(r) => return r,
            };
            match st.plugins.enable(&st, &ph) {
                Ok(()) => Json(json!({"ok": true})).into_response(),
                Err(e) => err(StatusCode::BAD_REQUEST, e),
            }
        }
        "disable" => {
            let ph = match plugin(&req.id) {
                Ok(p) => p,
                Err(r) => return r,
            };
            match st.plugins.disable(&ph) {
                Ok(()) => Json(json!({"ok": true})).into_response(),
                Err(e) => err(StatusCode::BAD_REQUEST, e),
            }
        }
        "reload" => {
            let ph = match plugin(&req.id) {
                Ok(p) => p,
                Err(r) => return r,
            };
            st.plugins.reload(&st, &ph);
            Json(json!({"ok": true})).into_response()
        }
        "settings" => {
            let ph = match plugin(&req.id) {
                Ok(p) => p,
                Err(r) => return r,
            };
            let values = req.values.unwrap_or_default();
            {
                let mut v = ph.values.lock().unwrap();
                for (k, val) in values {
                    // 只接受清单里声明过的设置项
                    if ph.manifest().settings.iter().any(|s| s.name == k) {
                        v.insert(k, val);
                    }
                }
            }
            st.plugins.save_state();
            let merged = ph.settings();
            if let Some(h) = ph.host() {
                let _ = h.request("settings", json!({"values": merged}), Duration::from_secs(10)).await;
            }
            Json(json!({"ok": true})).into_response()
        }
        "open_dir" => {
            let dir = match &req.id {
                Some(id) if !id.is_empty() => match st.plugins.get(id) {
                    Some(p) => p.dir.clone(),
                    None => st.plugins.root().to_path_buf(),
                },
                _ => st.plugins.root().to_path_buf(),
            };
            match open_dir(&dir) {
                Ok(()) => Json(json!({"ok": true})).into_response(),
                Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
            }
        }
        "create" => match create_plugin(&st, req.id.as_deref().unwrap_or(""), req.name.as_deref().unwrap_or("")) {
            Ok(id) => Json(json!({"ok": true, "id": id})).into_response(),
            Err(e) => err(StatusCode::BAD_REQUEST, e),
        },
        "rescan" => {
            st.plugins.rescan(&st);
            st.plugins.emit_changed();
            Json(json!({"ok": true})).into_response()
        }
        other => err(StatusCode::BAD_REQUEST, tr!("未知操作：{other}", "Unknown action: {other}")),
    }
}

/// 新建插件：写入 `plugin.json` 与 main.lua 模板，然后直接启用
fn create_plugin(app: &Arc<AppState>, id: &str, name: &str) -> Result<String, String> {
    let id = id.trim().to_ascii_lowercase();
    if !manifest::valid_id(&id) {
        return Err(tr!("插件 id 需要 [a-z0-9][a-z0-9_-]{{0,39}}", "Plugin id must match [a-z0-9][a-z0-9_-]{{0,39}}"));
    }
    if app.plugins.get(&id).is_some() {
        return Err(tr!("插件 {id} 已存在", "Plugin {id} already exists"));
    }
    let dir = app.plugins.root().join(&id);
    if dir.exists() {
        return Err(tr!("目录已存在：{id}", "The folder already exists: {id}"));
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = if name.trim().is_empty() { id.clone() } else { name.trim().to_string() };
    let man = json!({
        "id": id,
        "name": name,
        "version": "1.0.0",
        "description": "",
        "author": "",
        "main": "main.lua",
        "permissions": [],
        "settings": [],
    });
    let data = serde_json::to_vec_pretty(&man).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("plugin.json"), data).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("main.lua"), NEW_PLUGIN_TEMPLATE).map_err(|e| e.to_string())?;
    app.plugins.rescan(app);
    if let Some(ph) = app.plugins.get(&id) {
        app.plugins.enable(app, &ph)?;
    }
    Ok(id)
}

const NEW_PLUGIN_TEMPLATE: &str = r#"-- DroidBridge 插件模板
-- 修改保存后会自动重新加载；日志显示在“设置 → 插件 → 控制台”。
-- 需要更多权限时，在 plugin.json 的 permissions 里添加后重新启用。

local count = 0

-- 设备卡片右键菜单：MyGame / 打个招呼
db.menu("device", "MyGame/打个招呼", { icon = "👋" }, function(dev)
    db.notify("你好，" .. dev.model)
    db.log("菜单被点击：", dev.serial)
end)

-- 投屏窗口侧边的面板：一个按钮 + 一个计数器
db.panel("demo", { title = "示例面板", dock = "device" }, function(ui, dev)
    ui.label(dev and ("设备：" .. dev.serial) or "没有选择设备")
    if ui.button("点我 +1") then
        count = count + 1
    end
    ui.label("点击次数：" .. count)
end)

-- 设备连接时记一条日志
db.on("device.connected", function(dev)
    db.log("设备已连接：" .. dev.serial .. "（" .. dev.model .. "）")
end)
"#;

// ---------------------------------------------------------------- 贡献与调用

pub async fn plugins_contrib(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let serial = q.get("serial").cloned().filter(|s| !s.is_empty());
    let path = fwd_path("/api/plugins/contrib", &q);
    let st2 = st.clone();
    local_or_peer(&st, &q, "GET", &path, b"", || async move { Ok(st2.plugins.contrib_json(&st2, serial.as_deref()).await) }).await
}

#[derive(Deserialize)]
struct InvokeReq {
    plugin: String,
    kind: String,
    id: String,
    #[serde(default)]
    ctx: Value,
    #[serde(default)]
    client: Option<String>,
}

pub async fn plugins_invoke(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>, body: Bytes) -> Response {
    let path = fwd_path("/api/plugins/invoke", &q);
    let st2 = st.clone();
    let b = body.clone();
    local_or_peer(&st, &q, "POST", &path, &body, || async move {
        let req: InvokeReq = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
        let ph = st2.plugins.get(&req.plugin).ok_or_else(|| tr!("插件不存在", "Plugin not found"))?;
        let Some(h) = ph.host() else {
            return Ok(json!({"error": tr!("插件没有运行", "The plugin is not running")}));
        };
        let method = match req.kind.as_str() {
            "menu" => "menu",
            "status" => "status",
            other => return Err(tr!("未知的调用类型：{other}", "Unknown invoke kind: {other}")),
        };
        let p = json!({"id": req.id, "ctx": req.ctx, "client": req.client});
        match h.request(method, p, Duration::from_secs(600)).await {
            Ok(v) => Ok(json!({"ok": v})),
            Err(e) => Ok(json!({"error": e})),
        }
    })
    .await
}

#[derive(Deserialize)]
struct PanelReq {
    plugin: String,
    panel: String,
    #[serde(default)]
    serial: Option<String>,
    #[serde(default)]
    event: Value,
    #[serde(default)]
    client: Option<String>,
}

pub async fn plugins_panel(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>, body: Bytes) -> Response {
    let path = fwd_path("/api/plugins/panel", &q);
    let st2 = st.clone();
    let b = body.clone();
    local_or_peer(&st, &q, "POST", &path, &body, || async move {
        let req: PanelReq = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
        let ph = st2.plugins.get(&req.plugin).ok_or_else(|| tr!("插件不存在", "Plugin not found"))?;
        let h = ph.host().ok_or_else(|| tr!("插件没有运行", "The plugin is not running"))?;
        let p = json!({"id": req.panel, "serial": req.serial, "event": req.event, "client": req.client});
        h.request("panel", p, Duration::from_secs(5)).await
    })
    .await
}

pub async fn plugins_metrics(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let serial = q.get("serial").cloned().filter(|s| !s.is_empty());
    let path = fwd_path("/api/plugins/metrics", &q);
    let st2 = st.clone();
    local_or_peer(&st, &q, "GET", &path, b"", || async move { metrics_local(&st2, serial.as_deref()).await }).await
}

/// 每个指标单独 3 秒超时，失败时 value = null
async fn metrics_local(app: &Arc<AppState>, serial: Option<&str>) -> Result<Value, String> {
    let mut tasks = Vec::new();
    for ph in app.plugins.plugins() {
        if ph.state() != PluginState::Running {
            continue;
        }
        let defs: Vec<Value> = ph.contributions.lock().unwrap().metrics.clone();
        for d in defs {
            if let Some(serial) = serial {
                if !app.plugins.device_allowed(app, &ph, serial).await {
                    continue;
                }
                if !app.plugins.filter_ok(app, serial, d.get("filter")).await {
                    continue;
                }
            }
            let Some(h) = ph.host() else { continue };
            let (plugin, serial) = (ph.id.clone(), serial.map(String::from));
            tasks.push(tokio::spawn(async move {
                let id = d.get("id").cloned().unwrap_or(Value::Null);
                let v = h.request("metric", json!({"id": id, "serial": serial}), Duration::from_secs(3)).await.unwrap_or(Value::Null);
                json!({"plugin": plugin, "id": id, "label": d.get("label").cloned().unwrap_or(Value::Null), "unit": d.get("unit").cloned().unwrap_or(Value::Null), "value": v})
            }));
        }
    }
    let mut out = Vec::new();
    for t in tasks {
        if let Ok(v) = t.await {
            out.push(v);
        }
    }
    Ok(json!(out))
}

#[derive(Deserialize)]
struct WebReq {
    plugin: String,
    name: String,
    #[serde(default)]
    args: Value,
    #[serde(default)]
    serial: Option<String>,
    #[serde(default)]
    client: Option<String>,
}

pub async fn plugins_web(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>, body: Bytes) -> Response {
    let path = fwd_path("/api/plugins/web", &q);
    let st2 = st.clone();
    let b = body.clone();
    local_or_peer(&st, &q, "POST", &path, &body, || async move {
        let req: WebReq = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
        let ph = st2.plugins.get(&req.plugin).ok_or_else(|| tr!("插件不存在", "Plugin not found"))?;
        let h = ph.host().ok_or_else(|| tr!("插件没有运行", "The plugin is not running"))?;
        let p = json!({"name": req.name, "args": req.args, "serial": req.serial, "client": req.client});
        let v = h.request("web", p, Duration::from_secs(120)).await?;
        Ok(json!({"ok": v}))
    })
    .await
}

#[derive(Deserialize)]
struct DialogAnswer {
    id: String,
    #[serde(default)]
    value: Value,
}

pub async fn plugins_dialog(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>, body: Bytes) -> Response {
    let path = fwd_path("/api/plugins/dialog", &q);
    let st2 = st.clone();
    let b = body.clone();
    local_or_peer(&st, &q, "POST", &path, &body, || async move {
        let a: DialogAnswer = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
        st2.plugins.answer(&a.id, a.value);
        Ok(json!({"ok": true}))
    })
    .await
}

/// 上传文件到插件临时目录（kind=file 的对话框答案，流式接收与转发，避免大文件占满内存）
pub async fn plugins_upload(State(st): State<Shared>, headers: axum::http::HeaderMap, Query(q): Query<HashMap<String, String>>, body: axum::body::Body) -> Response {
    let path = fwd_path("/api/plugins/upload", &q);
    if let Some(id) = peer_of(&q) {
        let Some(peer) = st.peers.get(id).await else {
            return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
        };
        let clen = headers.get(axum::http::header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|s| s.parse::<u64>().ok());
        return match peers::post_body_stream(&peer, &path, &[], clen, body, std::time::Duration::from_secs(600)).await {
            Ok(v) => Json(v).into_response(),
            Err(e) => err(StatusCode::BAD_GATEWAY, e),
        };
    }
    let raw = q.get("name").map(String::as_str).unwrap_or("file");
    let name: String = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("file")
        .chars()
        .map(|c| if c.is_control() || "\"'`$;&|<>*?/:".contains(c) { '_' } else { c })
        .collect();
    let name = if name.trim().is_empty() { "file".to_string() } else { name };
    let dir = st.plugins.root().join(".tmp").join("uploads");
    let _ = tokio::fs::create_dir_all(&dir).await;
    let tok = &crate::access::new_token()[..24];
    let path = dir.join(format!("up-{tok}-{name}"));
    match peers::save_body_to_file(body, &path, peers::UPLOAD_MAX).await {
        Ok(0) => err(StatusCode::BAD_REQUEST, tr!("文件为空", "File is empty")),
        Ok(_) => Json(json!({"path": path.to_string_lossy()})).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

pub async fn plugins_console(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let id = q.get("id").cloned().unwrap_or_default();
    let path = fwd_path("/api/plugins/console", &q);
    let st2 = st.clone();
    local_or_peer(&st, &q, "GET", &path, b"", || async move {
        let ph = st2.plugins.get(&id).ok_or_else(|| tr!("插件不存在", "Plugin not found"))?;
        Ok(json!(st2.plugins.console_list(&ph)))
    })
    .await
}

/// 本机插件的 MCP 工具（SPEC 7.3：插件工具只在本机执行，不再合并远程电脑的插件工具）
pub async fn plugins_mcp_list(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let st2 = st.clone();
    local_or_peer(&st, &q, "GET", "/api/plugins/mcp", b"", || async move { Ok(json!(mcp_tools(&st2))) }).await
}

// ---------------------------------------------------------------- WebSocket

pub async fn plugins_ws(
    State(st): State<Shared>,
    ws: WebSocketUpgrade,
    Query(q): Query<HashMap<String, String>>,
    uri: axum::http::Uri,
) -> Response {
    let client = q.get("client").cloned();
    let raw_query = uri.query().map(String::from);
    if let Some(id) = peer_of(&q) {
        let Some(peer) = st.peers.get(id).await else {
            return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
        };
        return ws.on_upgrade(move |socket| peers::proxy_ws(socket, peer, "/ws/plugins", raw_query));
    }
    ws.on_upgrade(move |socket| ws_run(st, client, socket))
}

async fn ws_run(st: Shared, client: Option<String>, socket: WebSocket) {
    let (mut tx, mut rx) = socket.split();
    let mut sub = st.plugins.subscribe();
    let _ = tx.send(Message::Text(json!({"type": "changed"}).to_string().into())).await;
    loop {
        tokio::select! {
            m = sub.recv() => match m {
                Ok(msg) => {
                    if let Some(c) = &msg.client {
                        if Some(c) != client.as_ref() {
                            continue;
                        }
                    }
                    if tx.send(Message::Text(msg.text.into())).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            },
            m = rx.next() => match m {
                Some(Ok(_)) => continue,
                _ => break,
            },
        }
    }
}

// ---------------------------------------------------------------- 插件网页静态文件

/// 插件网页 SDK（编译进程序）
pub async fn plugin_web_sdk() -> Response {
    static_headers("text/javascript; charset=utf-8", include_str!("../../web/plugin-sdk.js").as_bytes().to_vec())
}

/// `/plugin-web/{id}/{path}`：只提供插件目录 `web/` 与 webpanel entry 所在目录下的文件。
/// `/plugin-web/@{peer}/{id}/{path}` 转发给远程电脑。
pub async fn plugin_web_file(State(st): State<Shared>, Path((id, path)): Path<(String, String)>) -> Response {
    if let Some(peer_id) = id.strip_prefix('@') {
        let (plugin, rest) = match path.split_once('/') {
            Some((p, r)) => (p.to_string(), r.to_string()),
            None => (path.clone(), String::new()),
        };
        let Some(peer) = st.peers.get(peer_id).await else {
            return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
        };
        let target = format!("/plugin-web/{}/{}", util::url_encode(&plugin), rest);
        return match peers::forward_raw(&peer, &target, std::time::Duration::from_secs(60)).await {
            Ok((code, ct, body)) => crate::peer_reply(code, ct, body),
            Err(e) => err(StatusCode::BAD_GATEWAY, e),
        };
    }
    if !manifest::safe_rel(&path) {
        return err(StatusCode::FORBIDDEN, tr!("无效的路径", "Invalid path"));
    }
    let Some(ph) = st.plugins.get(&id) else {
        return err(StatusCode::NOT_FOUND, tr!("插件不存在", "Plugin not found"));
    };
    let Some(file) = resolve_static(&ph, &path) else {
        return err(StatusCode::NOT_FOUND, tr!("文件不存在", "File not found"));
    };
    match tokio::fs::read(&file).await {
        Ok(data) => static_headers(content_type(&path), data),
        Err(_) => err(StatusCode::NOT_FOUND, tr!("文件不存在", "File not found")),
    }
}

/// 解析 `/plugin-web/<id>/<path>` 对应的文件：必须落在插件目录的 `web/` 或某个 webpanel
/// entry 所在目录内（符号链接、`..`、绝对路径都不能逃逸）
fn resolve_static(ph: &Plugin, path: &str) -> Option<PathBuf> {
    if !manifest::safe_rel(path) {
        return None;
    }
    let roots = web_roots(ph);
    let canon = std::fs::canonicalize(ph.dir.join(path)).ok()?;
    if !canon.is_file() || !roots.iter().any(|r| canon.starts_with(r)) {
        return None;
    }
    Some(canon)
}

/// 允许访问的目录：插件目录下的 `web/`，以及每个 webpanel entry 所在的目录
fn web_roots(ph: &Plugin) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let add = |roots: &mut Vec<PathBuf>, p: &PathBuf| {
        if let Ok(c) = std::fs::canonicalize(p) {
            if c.is_dir() && !roots.contains(&c) {
                roots.push(c);
            }
        }
    };
    add(&mut roots, &ph.dir.join("web"));
    for (is_web, entry, _) in ph.contributions.lock().unwrap().panels.iter() {
        if !*is_web {
            continue;
        }
        let full = ph.dir.join(entry);
        if let Some(parent) = full.parent() {
            // entry 放在插件根目录时不开放整个目录（否则 main.lua、plugin.json 等都能被局域网读取）
            if parent == ph.dir.as_path() {
                continue;
            }
            add(&mut roots, &parent.to_path_buf());
        }
    }
    roots
}

fn static_headers(ct: &str, data: Vec<u8>) -> Response {
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, ct.parse().unwrap_or(header::HeaderValue::from_static("application/octet-stream")));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, header::HeaderValue::from_static("nosniff"));
    h.insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-cache"));
    h.insert(
        "content-security-policy",
        header::HeaderValue::from_static("sandbox allow-scripts allow-forms"),
    );
    (h, data).into_response()
}

fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "txt" | "md" | "lua" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin_at(dir: std::path::PathBuf) -> Arc<Plugin> {
        std::fs::create_dir_all(&dir).unwrap();
        let man = Arc::new(Manifest { id: "test".into(), name: "Test".into(), ..Default::default() });
        Arc::new(Plugin::new(man, dir, StateEntry::default()))
    }

    #[test]
    fn content_types() {
        assert!(content_type("a/b/index.html").starts_with("text/html"));
        assert_eq!(content_type("x.PNG"), "image/png");
        assert_eq!(content_type("x.unknownext"), "application/octet-stream");
    }

    #[test]
    fn web_roots_and_escape() {
        let base = std::env::temp_dir().join(format!("droidbridge-web-{:08x}", util::random_scid()));
        let dir = base.join("myplugin");
        let ph = plugin_at(dir.clone());
        std::fs::create_dir_all(dir.join("web/assets")).unwrap();
        std::fs::write(dir.join("web/assets/app.js"), "1;").unwrap();
        assert!(resolve_static(&ph, "web/assets/app.js").is_some());
        std::fs::write(dir.join("web/index.html"), "<h1>hi</h1>").unwrap();
        std::fs::write(dir.join("secret.txt"), "secret").unwrap();

        let roots = web_roots(&ph);
        assert_eq!(roots.len(), 1);
        assert!(roots[0].ends_with("web"));

        // web/ 下的文件可以访问
        assert!(resolve_static(&ph, "web/index.html").is_some());
        assert!(resolve_static(&ph, "web/assets/missing.js").is_none());
        // 插件目录里的其他文件不能访问
        assert!(resolve_static(&ph, "secret.txt").is_none());
        // `..` / 绝对路径被拒绝
        assert!(resolve_static(&ph, "../secret.txt").is_none());
        assert!(resolve_static(&ph, "web/../../secret.txt").is_none());
        assert!(!manifest::safe_rel("../../etc/passwd"));
        assert!(!manifest::safe_rel("/etc/passwd"));

        // 符号链接逃逸：web/link.txt → 目录外的 secret.txt
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("secret.txt"), dir.join("web/link.txt")).unwrap();
            assert!(resolve_static(&ph, "web/link.txt").is_none());
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn forward_paths() {
        let mut q = HashMap::new();
        q.insert("peer".to_string(), "p1".to_string());
        q.insert("serial".to_string(), "abc".to_string());
        assert_eq!(fwd_path("/api/plugins/contrib", &q), "/api/plugins/contrib?serial=abc");
        assert_eq!(fwd_path("/api/plugins", &q), "/api/plugins?serial=abc");
        assert_eq!(peer_of(&q), Some("p1"));
        q.insert("peer".to_string(), "local".to_string());
        assert_eq!(peer_of(&q), None);
    }
}
