//! DroidBridge 核心：HTTP/WebSocket 服务、adb/scrcpy 会话、远程节点中转。
//! 命令行版（src/main.rs）与桌面版（src/bin/desktop.rs）共用。

#[macro_use]
pub mod i18n;
pub mod adb;
pub mod control;
pub mod icon;
pub mod logcat;
pub mod peers;
pub mod remote;
pub mod scrcpy;
pub mod util;

use axum::body::Bytes;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

const SCRCPY_SERVER: &[u8] = include_bytes!("../third_party/scrcpy-server");
const SCRCPY_VERSION_RAW: &str = include_str!("../third_party/scrcpy-version.txt");
const INDEX_HTML: &str = include_str!("../web/index.html");

pub fn scrcpy_version() -> &'static str {
    SCRCPY_VERSION_RAW.trim()
}

pub struct Config {
    pub bind: String,
    pub port: u16,
    pub https_port: u16,
    pub token: Option<String>,
    pub name: String,
    pub adb: PathBuf,
    pub data_dir: PathBuf,
    pub server_jar: PathBuf,
}

const HELP_ZH: &str = "DroidBridge — 安卓设备投屏与远程操作

用法: droidbridge [选项]

  --port <端口>         HTTP 端口（默认 8730）
  --https-port <端口>   HTTPS 端口（默认 8731，0 表示关闭；手机/其他电脑浏览器访问需用 HTTPS）
  --bind <地址>         监听地址（默认 0.0.0.0）
  --token <令牌>        访问令牌；设置后所有接口都需要令牌
  --name <名称>         本机显示名称（默认计算机名）
  --adb <路径>          adb 可执行文件路径（默认自动查找）
  --data <目录>         数据目录
  --lang <zh|en>        界面与提示语言（默认跟随系统）
  -h, --help            显示帮助
";

const HELP_EN: &str = "DroidBridge — mirror and control Android devices

Usage: droidbridge [options]

  --port <port>         HTTP port (default 8730)
  --https-port <port>   HTTPS port (default 8731, 0 to disable; browsers on other devices need HTTPS)
  --bind <addr>         Listen address (default 0.0.0.0)
  --token <token>       Access token; required by all APIs when set
  --name <name>         Display name of this computer (default: host name)
  --adb <path>          Path to adb (found automatically by default)
  --data <dir>          Data directory
  --lang <zh|en>        UI and message language (default: system)
  -h, --help            Show help
";

/// 命令行帮助（按当前语言）
pub fn help() -> &'static str {
    if i18n::is_en() { HELP_EN } else { HELP_ZH }
}

impl Config {
    /// 从命令行参数解析（不含程序名）。返回 Ok(None) 表示用户请求了帮助。
    pub fn from_args(args: impl IntoIterator<Item = String>) -> Result<Option<Config>, String> {
        let mut port = peers::DEFAULT_PORT;
        let mut https_port = peers::DEFAULT_PORT + 1;
        let mut bind = "0.0.0.0".to_string();
        let mut token = std::env::var("DROIDBRIDGE_TOKEN").ok().filter(|t| !t.is_empty());
        let mut name = None;
        let mut adb = None;
        let mut data = None;

        let args: Vec<String> = args.into_iter().collect();
        i18n::init_from_args(&args);
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            let mut val = |n: &str| it.next().ok_or_else(|| tr!("{n} 需要一个参数值", "{n} requires a value"));
            match a.as_str() {
                "--port" => port = val("--port")?.parse().map_err(|_| tr!("--port 无效", "Invalid --port"))?,
                "--https-port" => https_port = val("--https-port")?.parse().map_err(|_| tr!("--https-port 无效", "Invalid --https-port"))?,
                "--bind" => bind = val("--bind")?,
                "--token" => token = Some(val("--token")?).filter(|t| !t.is_empty()),
                "--name" => name = Some(val("--name")?),
                "--adb" => adb = Some(PathBuf::from(val("--adb")?)),
                "--data" => data = Some(PathBuf::from(val("--data")?)),
                "--lang" => {
                    val("--lang")?;
                }
                "-h" | "--help" => return Ok(None),
                other => {
                    let help = help();
                    return Err(tr!("未知参数：{other}\n\n{help}", "Unknown option: {other}\n\n{help}"));
                }
            }
        }

        let data_dir = data.unwrap_or_else(default_data_dir);
        let name = name
            .or_else(|| std::env::var("COMPUTERNAME").ok())
            .or_else(|| std::env::var("HOSTNAME").ok())
            .or_else(util::hostname)
            .unwrap_or_else(|| tr!("本机", "This computer"));
        Ok(Some(Config {
            bind,
            port,
            https_port,
            token,
            name,
            adb: adb::find_adb(adb),
            server_jar: data_dir.join("scrcpy-server.jar"),
            data_dir,
        }))
    }
}

/// 默认数据目录：Windows 为程序目录下 data（便携）；macOS 为 ~/Library/Application Support/DroidBridge；
/// 其他系统为 $XDG_DATA_HOME/droidbridge 或 ~/.local/share/droidbridge
pub fn default_data_dir() -> PathBuf {
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        if cfg!(target_os = "macos") {
            if let Some(h) = home {
                return h.join("Library/Application Support/DroidBridge");
            }
        } else if let Some(x) = std::env::var_os("XDG_DATA_HOME").filter(|x| !x.is_empty()) {
            return PathBuf::from(x).join("droidbridge");
        } else if let Some(h) = home {
            return h.join(".local/share/droidbridge");
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("data")))
        .unwrap_or_else(|| PathBuf::from("data"))
}

pub struct AppState {
    pub cfg: Config,
    pub peers: peers::PeerStore,
    pub start_lock: tokio::sync::Mutex<()>,
    pub jobs: std::sync::Mutex<HashMap<String, remote::Job>>,
}

type Shared = Arc<AppState>;

pub struct Started {
    pub port: u16,
    pub https_port: u16,
    pub state: Arc<AppState>,
}

#[derive(Debug)]
pub enum StartError {
    /// HTTP 端口被占用
    AddrInUse(SocketAddr),
    Other(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::AddrInUse(a) => f.write_str(&tr!("端口 {a} 已被占用（可用 --port 修改）", "Port {a} is already in use (change it with --port)")),
            StartError::Other(s) => f.write_str(s),
        }
    }
}

/// 准备数据目录与 scrcpy-server，绑定端口并在后台启动 HTTP/HTTPS 服务。
pub async fn start(cfg: Config) -> Result<Started, StartError> {
    std::fs::create_dir_all(&cfg.data_dir)
        .map_err(|e| { let d = cfg.data_dir.display(); StartError::Other(tr!("无法创建数据目录 {d}：{e}", "Cannot create data directory {d}: {e}")) })?;
    std::fs::write(&cfg.server_jar, SCRCPY_SERVER)
        .map_err(|e| StartError::Other(tr!("无法写出 scrcpy-server：{e}", "Cannot write scrcpy-server: {e}")))?;

    let http_addr: SocketAddr = format!("{}:{}", cfg.bind, cfg.port)
        .parse()
        .map_err(|e| StartError::Other(tr!("监听地址无效：{e}", "Invalid listen address: {e}")))?;
    let listener = tokio::net::TcpListener::bind(http_addr).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            StartError::AddrInUse(http_addr)
        } else {
            StartError::Other(tr!("无法监听 {http_addr}：{e}", "Cannot listen on {http_addr}: {e}"))
        }
    })?;
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(cfg.port);

    match adb::run(&cfg.adb, &["start-server"]).await {
        Ok(_) => tracing::info!("adb: {}", cfg.adb.display()),
        Err(e) => tracing::error!("adb unavailable: {e} (use --adb to set its path)"),
    }

    let state = Arc::new(AppState {
        peers: peers::PeerStore::load(cfg.data_dir.join("peers.json")),
        start_lock: tokio::sync::Mutex::new(()),
        jobs: std::sync::Mutex::new(HashMap::new()),
        cfg,
    });
    let app = router(state.clone());

    let mut https_port = state.cfg.https_port;
    if https_port != 0 {
        match format!("{}:{}", state.cfg.bind, https_port).parse::<SocketAddr>() {
            Ok(https_addr) => {
                let app2 = app.clone();
                let data_dir = state.cfg.data_dir.clone();
                let name = state.cfg.name.clone();
                tokio::spawn(async move {
                    match tls_config(&data_dir, &util::local_ips(), &name).await {
                        Ok(tls) => {
                            if let Err(e) = axum_server::bind_rustls(https_addr, tls).serve(app2.into_make_service()).await {
                                tracing::error!("HTTPS server stopped: {e}");
                            }
                        }
                        Err(e) => tracing::error!("HTTPS setup failed: {e}"),
                    }
                });
            }
            Err(e) => {
                tracing::error!("Invalid HTTPS address: {e}");
                https_port = 0;
            }
        }
    }

    use axum::serve::ListenerExt;
    let listener = listener.tap_io(|tcp| {
        let _ = tcp.set_nodelay(true);
    });
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("HTTP server stopped: {e}");
        }
    });

    Ok(Started { port, https_port, state })
}

fn router(state: Shared) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/info", get(info))
        .route("/api/devices", get(devices))
        .route("/api/peers", get(list_peers).post(add_peer))
        .route("/api/peers/{id}", put(edit_peer).delete(remove_peer))
        .route("/api/install", post(install_start))
        .route("/api/install/{id}", get(install_status))
        .route("/api/push", post(push_file).layer(DefaultBodyLimit::max(4usize << 30)))
        .route("/ws/stream", get(ws_stream))
        .route("/ws/logcat", get(ws_logcat))
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state)
}

/// 检查某地址上是否已经运行着 DroidBridge
pub async fn probe(addr: &str) -> bool {
    match peers::http_get(addr, "/api/info").await {
        Ok((_, body)) => serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("app").and_then(|a| a.as_str()).map(|a| a == "droidbridge"))
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// 自签名证书：首次启动生成并保存到 data 目录
async fn tls_config(
    data_dir: &std::path::Path,
    ips: &[String],
    name: &str,
) -> Result<axum_server::tls_rustls::RustlsConfig, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert_p = data_dir.join("cert.pem");
    let key_p = data_dir.join("key.pem");
    if !cert_p.exists() || !key_p.exists() {
        let mut sans = vec!["localhost".to_string(), "127.0.0.1".to_string(), name.to_string()];
        sans.extend(ips.iter().cloned());
        let ck = rcgen::generate_simple_self_signed(sans).map_err(|e| e.to_string())?;
        std::fs::write(&cert_p, ck.cert.pem()).map_err(|e| e.to_string())?;
        std::fs::write(&key_p, ck.signing_key.serialize_pem()).map_err(|e| e.to_string())?;
    }
    axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert_p, &key_p)
        .await
        .map_err(|e| e.to_string())
}

fn err(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({"error": msg.into()}))).into_response()
}

/// 中间件：按请求语言（?lang=、X-Lang、Accept-Language）输出提示，并校验访问令牌
async fn auth(State(st): State<Shared>, req: Request, next: Next) -> Response {
    let q = util::parse_query(req.uri().query());
    let lang_en = q
        .get("lang")
        .filter(|l| !l.is_empty())
        .map(|l| !l.to_ascii_lowercase().starts_with("zh"))
        .or_else(|| i18n::from_headers(req.headers()))
        .unwrap_or_else(i18n::is_en);
    i18n::REQUEST_EN.scope(lang_en, auth_inner(st, req, next)).await
}

async fn auth_inner(st: Shared, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let protected = (path.starts_with("/api/") && path != "/api/info") || path.starts_with("/ws/");
    if let (true, Some(expected)) = (protected, st.cfg.token.as_deref()) {
        let q = util::parse_query(req.uri().query());
        let given = q
            .get("token")
            .cloned()
            .or_else(|| req.headers().get("x-token").and_then(|v| v.to_str().ok()).map(String::from));
        if given.as_deref() != Some(expected) {
            return err(StatusCode::UNAUTHORIZED, tr!("需要访问令牌", "Access token required"));
        }
    }
    next.run(req).await
}

async fn index() -> impl IntoResponse {
    let mut h = HeaderMap::new();
    h.insert(header::CACHE_CONTROL, "no-cache".parse().unwrap());
    (h, Html(INDEX_HTML))
}

async fn info(State(st): State<Shared>) -> Json<serde_json::Value> {
    Json(json!({
        "app": "droidbridge",
        "name": st.cfg.name,
        "version": env!("CARGO_PKG_VERSION"),
        "build": remote::build_id(),
        "os": std::env::consts::OS,
        "lang": i18n::code(),
        "scrcpy": scrcpy_version(),
        "auth": st.cfg.token.is_some(),
        "ips": util::local_ips(),
        "port": st.cfg.port,
        "https_port": st.cfg.https_port,
    }))
}

fn remote_id(q: &HashMap<String, String>) -> Option<&str> {
    q.get("peer").map(String::as_str).filter(|p| !p.is_empty() && *p != "local")
}

async fn devices(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    match remote_id(&q) {
        None => match adb::list_devices(&st.cfg.adb).await {
            Ok(list) => Json(list).into_response(),
            Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
        },
        Some(id) => {
            let Some(peer) = st.peers.get(id).await else {
                return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
            };
            match peers::get_json(&peer, "/api/devices").await {
                Ok(v) => Json(v).into_response(),
                Err(e) => err(StatusCode::BAD_GATEWAY, e),
            }
        }
    }
}

async fn list_peers(State(st): State<Shared>) -> Json<Vec<peers::PeerPublic>> {
    Json(st.peers.list().await)
}

#[derive(Deserialize)]
struct NewPeer {
    #[serde(default)]
    name: String,
    addr: String,
    #[serde(default)]
    token: Option<String>,
}

/// 校验对端（确认是 DroidBridge、令牌正确）并保存。id 为 None 时新增（同地址则覆盖），否则编辑；
/// 编辑时令牌留空表示保持原令牌。名称为空时使用对端的计算机名。
pub async fn save_peer(
    st: &AppState,
    id: Option<String>,
    addr: &str,
    name: &str,
    token: Option<String>,
) -> Result<serde_json::Value, String> {
    let addr = peers::normalize_addr(addr)?;
    let token = token.filter(|t| !t.is_empty());
    let mut peer = match &id {
        Some(id) => {
            let old = st.peers.get(id).await.ok_or_else(|| tr!("远程电脑不存在", "Remote computer not found"))?;
            peers::Peer { id: id.clone(), name: name.trim().to_string(), addr, token: token.or(old.token) }
        }
        None => peers::Peer {
            id: format!("p{:08x}", util::random_scid()),
            name: name.trim().to_string(),
            addr,
            token,
        },
    };
    let info = peers::get_json(&peer, "/api/info")
        .await
        .map_err(|e| { let a = &peer.addr; tr!("无法访问 {a}：{e}", "Cannot reach {a}: {e}") })?;
    if info.get("app").and_then(|v| v.as_str()) != Some("droidbridge") {
        return Err(tr!("对端不是 DroidBridge", "The other side is not DroidBridge"));
    }
    peers::get_json(&peer, "/api/devices").await?;
    if peer.name.is_empty() {
        peer.name = info.get("name").and_then(|v| v.as_str()).unwrap_or(&peer.addr).to_string();
    }
    if id.is_some() {
        st.peers.update(peer.clone()).await?;
    } else {
        st.peers.upsert(peer.clone()).await?;
    }
    Ok(json!({"id": peer.id, "name": peer.name, "addr": peer.addr}))
}

async fn add_peer(State(st): State<Shared>, Json(np): Json<NewPeer>) -> Response {
    match save_peer(&st, None, &np.addr, &np.name, np.token).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, e),
    }
}

async fn edit_peer(State(st): State<Shared>, Path(id): Path<String>, Json(np): Json<NewPeer>) -> Response {
    match save_peer(&st, Some(id), &np.addr, &np.name, np.token).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, e),
    }
}

async fn install_start(State(st): State<Shared>, Json(req): Json<remote::InstallReq>) -> Response {
    match remote::start(st, req) {
        Ok(id) => Json(json!({"job": id})).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

async fn install_status(
    State(st): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let since = q.get("since").and_then(|s| s.parse().ok()).unwrap_or(0);
    match remote::get(&st, &id, since) {
        Some(v) => Json(v).into_response(),
        None => err(StatusCode::NOT_FOUND, tr!("任务不存在", "Job not found")),
    }
}

async fn remove_peer(State(st): State<Shared>, Path(id): Path<String>) -> Response {
    match st.peers.remove(&id).await {
        Ok(true) => Json(json!({"ok": true})).into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found")),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// 上传文件到设备：.apk 安装，其它文件推送到 /sdcard/Download/
async fn push_file(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>, body: Bytes) -> Response {
    let Some(serial) = q.get("serial").filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, tr!("缺少 serial 参数", "Missing serial parameter"));
    };
    let raw_name = q.get("name").map(String::as_str).unwrap_or("file");
    let name: String = raw_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("file")
        .chars()
        .map(|c| if c.is_control() || "\"'`$;&|<>*?".contains(c) { '_' } else { c })
        .collect();
    let name = if name.trim().is_empty() { "file".to_string() } else { name };
    if body.is_empty() {
        return err(StatusCode::BAD_REQUEST, tr!("文件为空", "File is empty"));
    }

    if let Some(id) = remote_id(&q) {
        let Some(peer) = st.peers.get(id).await else {
            return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
        };
        let query = format!("serial={}&name={}", util::url_encode(serial), util::url_encode(&name));
        return match peers::post_file(&peer, &query, &body).await {
            Ok(v) => Json(v).into_response(),
            Err(e) => err(StatusCode::BAD_GATEWAY, e),
        };
    }

    let is_apk = name.to_ascii_lowercase().ends_with(".apk");
    let dir = st.cfg.data_dir.join("uploads");
    let _ = tokio::fs::create_dir_all(&dir).await;
    let tmp = dir.join(format!("{:08x}{}", util::random_scid(), if is_apk { ".apk" } else { ".bin" }));
    if let Err(e) = tokio::fs::write(&tmp, &body).await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, tr!("保存文件失败：{e}", "Failed to save file: {e}"));
    }
    let tmp_s = tmp.to_string_lossy().into_owned();
    let result = if is_apk {
        adb::run(&st.cfg.adb, &["-s", serial, "install", "-r", "-t", &tmp_s])
            .await
            .map(|_| tr!("已安装 {name}", "Installed {name}"))
    } else {
        let dest = format!("/sdcard/Download/{name}");
        adb::run(&st.cfg.adb, &["-s", serial, "push", &tmp_s, &dest])
            .await
            .map(|_| tr!("已传到手机 Download/{name}", "Copied to Download/{name} on the device"))
    };
    let _ = tokio::fs::remove_file(&tmp).await;
    match result {
        Ok(msg) => Json(json!({"ok": true, "msg": msg})).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn ws_logcat(State(st): State<Shared>, ws: WebSocketUpgrade, req_uri: axum::http::Uri) -> Response {
    let raw_query = req_uri.query().map(String::from);
    let q = util::parse_query(raw_query.as_deref());
    let Some(serial) = q.get("serial").cloned().filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, tr!("缺少 serial 参数", "Missing serial parameter"));
    };
    if let Some(id) = remote_id(&q) {
        let Some(peer) = st.peers.get(id).await else {
            return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
        };
        return ws.on_upgrade(move |socket| peers::proxy_ws(socket, peer, "/ws/logcat", raw_query));
    }
    let en = i18n::is_en();
    ws.on_upgrade(move |socket| i18n::REQUEST_EN.scope(en, logcat::run(st, serial, socket)))
}

async fn ws_stream(State(st): State<Shared>, ws: WebSocketUpgrade, req_uri: axum::http::Uri) -> Response {
    let raw_query = req_uri.query().map(String::from);
    let q = util::parse_query(raw_query.as_deref());
    let Some(serial) = q.get("serial").cloned().filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, tr!("缺少 serial 参数", "Missing serial parameter"));
    };
    let ws = ws.max_message_size(64 << 20).max_frame_size(64 << 20);

    if let Some(id) = remote_id(&q) {
        let Some(peer) = st.peers.get(id).await else {
            return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
        };
        return ws.on_upgrade(move |socket| peers::proxy_ws(socket, peer, "/ws/stream", raw_query));
    }

    let num = |k: &str, d: u32| q.get(k).and_then(|v| v.parse::<u32>().ok()).unwrap_or(d);
    let opts = scrcpy::StreamOpts {
        max_size: num("max_size", 1280),
        bit_rate: num("bit_rate", 8_000_000),
        max_fps: num("max_fps", 60),
        stay_awake: q.get("stay_awake").map(|v| v == "1" || v == "true").unwrap_or(true),
    };
    let en = i18n::is_en();
    ws.on_upgrade(move |socket| i18n::REQUEST_EN.scope(en, scrcpy::run_session(st, serial, opts, socket)))
}
