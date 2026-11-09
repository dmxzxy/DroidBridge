//! DroidBridge 核心：HTTP/WebSocket 服务、adb/scrcpy 会话、远程节点中转。
//! 命令行版（src/main.rs）与桌面版（src/bin/desktop.rs）共用。

#[macro_use]
pub mod i18n;
pub mod adb;
pub mod control;
pub mod discovery;
pub mod icon;
pub mod logcat;
pub mod media;
pub mod peers;
pub mod remote;
pub mod scrcpy;
pub mod util;
pub mod wireless;

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
    /// 局域网自动发现
    pub discovery: bool,
    /// 命令行指定的媒体目录（优先于 settings.json 与默认值）
    pub media_dir: Option<PathBuf>,
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
  --media-dir <目录>    截图与录屏保存目录（默认 <data>/media）
  --lang <zh|en>        界面与提示语言（默认跟随系统）
  --no-discovery        不参与局域网自动发现
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
  --media-dir <dir>     Where to save screenshots and recordings (default: <data>/media)
  --lang <zh|en>        UI and message language (default: system)
  --no-discovery        Do not take part in LAN discovery
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
        let mut media_dir = None;
        let mut discovery = true;

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
                "--media-dir" => media_dir = Some(PathBuf::from(val("--media-dir")?)),
                "--no-discovery" => discovery = false,
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
        let discovery = discovery && bind != "127.0.0.1" && bind != "localhost";
        Ok(Some(Config {
            discovery,
            bind,
            port,
            https_port,
            token,
            name,
            adb: adb::find_adb(adb),
            server_jar: data_dir.join("scrcpy-server.jar"),
            data_dir,
            media_dir,
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
    pub discovery: discovery::Discovery,
    pub wireless: wireless::Wireless,
    /// 无线设备 serial → ro.serialno
    pub uids: std::sync::Mutex<HashMap<String, String>>,
    /// 命令行 `--media-dir` 指定的媒体目录（优先）
    pub media_dir_override: Option<PathBuf>,
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

    let media_dir_override = cfg.media_dir.clone();
    let state = Arc::new(AppState {
        peers: peers::PeerStore::load(cfg.data_dir.join("peers.json")),
        start_lock: tokio::sync::Mutex::new(()),
        jobs: std::sync::Mutex::new(HashMap::new()),
        discovery: discovery::Discovery::new(),
        wireless: wireless::Wireless::load(cfg.data_dir.join("wireless.json")),
        uids: std::sync::Mutex::new(HashMap::new()),
        media_dir_override,
        cfg,
    });
    let app = router(state.clone());
    if state.cfg.discovery {
        tokio::spawn(discovery::run(state.clone(), port));
    }
    tokio::spawn(wireless::keeper(state.clone()));
    let cleanup_state = state.clone();
    tokio::spawn(async move { media::recover_parts_loop(cleanup_state).await; });

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
                            if let Err(e) = axum_server::bind_rustls(https_addr, tls).serve(app2.into_make_service_with_connect_info::<SocketAddr>()).await {
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
        if let Err(e) = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await {
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
        .route("/api/ssh/keys", get(ssh_keys))
        .route("/api/discovered", get(discovered))
        .route("/api/adb/connect", post(adb_connect))
        .route("/api/adb/pair", post(adb_pair))
        .route("/api/adb/disconnect", post(adb_disconnect))
        .route("/api/adb/tcpip", post(adb_tcpip))
        .route("/api/adb/qr", post(adb_qr_start))
        .route("/api/adb/qr/{id}", get(adb_qr_status).delete(adb_qr_cancel))
        .route("/api/push", post(push_file).layer(DefaultBodyLimit::max(4usize << 30)))
        .route("/api/media", get(media::media_list_handler))
        .route("/api/media/shot", post(media::media_shot_handler).layer(DefaultBodyLimit::max(64usize << 20)))
        .route("/api/media/file/{name}", get(media::media_file_handler))
        .route("/api/media/rec/start", post(media::media_rec_start_handler))
        .route("/api/media/rec/{id}/finish", post(media::media_rec_finish_handler))
        .route("/api/media/rec/{id}", axum::routing::post(media::media_rec_chunk_handler).delete(media::media_rec_abort_handler).layer(DefaultBodyLimit::max(64usize << 20)))
        .route("/api/media/{name}", axum::routing::delete(media::media_delete_handler))
        .route("/api/media/reveal", post(media::media_reveal_handler))
        .route("/api/settings", axum::routing::get(media::settings_get_handler).put(media::settings_put_handler))
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
            Ok(mut list) => {
                fill_uids(&st, &mut list).await;
                Json(list).into_response()
            }
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
        audio: q.get("audio").is_some_and(|v| v == "1" || v == "true"),
        audio_codec: q.get("audio_codec").cloned().unwrap_or_else(|| "opus".into()),
        audio_dup: q.get("audio_dup").is_some_and(|v| v == "1" || v == "true"),
    };
    let en = i18n::is_en();
    ws.on_upgrade(move |socket| i18n::REQUEST_EN.scope(en, scrcpy::run_session(st, serial, opts, socket)))
}

// ---------------------------------------------------------------- 发现与无线 adb

/// 读取无线设备的 ro.serialno（结果缓存），用于识别“同一台设备换了连接方式”
async fn fill_uids(st: &AppState, list: &mut [adb::Device]) {
    for d in list.iter_mut().filter(|d| d.wireless && d.state == "device") {
        let cached = st.uids.lock().unwrap().get(&d.serial).cloned();
        let uid = match cached {
            Some(u) => u,
            None => {
                let u = adb::run_timeout(&st.cfg.adb, &["-s", &d.serial, "shell", "getprop ro.serialno"], std::time::Duration::from_secs(4))
                    .await
                    .map(|o| o.trim().to_string())
                    .unwrap_or_default();
                if !u.is_empty() {
                    st.uids.lock().unwrap().insert(d.serial.clone(), u.clone());
                }
                u
            }
        };
        d.uid = uid;
    }
}

/// 局域网中发现的其他节点（已添加的电脑和本机除外）
async fn discovered(State(st): State<Shared>) -> Json<serde_json::Value> {
    let known: Vec<String> = st.peers.list().await.into_iter().map(|p| p.addr).collect();
    let mine: Vec<String> = util::local_ips().into_iter().map(|ip| format!("{ip}:{}", st.cfg.port)).collect();
    let list: Vec<_> = st
        .discovery
        .list()
        .into_iter()
        .filter(|f| !known.contains(&f.addr) && !mine.contains(&f.addr))
        .collect();
    Json(json!({"enabled": st.cfg.discovery, "list": list}))
}

/// 带 ?peer= 时把请求转发给远程电脑，否则在本机执行
async fn local_or_peer<F, Fut>(st: &AppState, q: &HashMap<String, String>, method: &str, path: &str, body: &[u8], local: F) -> Response
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<serde_json::Value, String>>,
{
    if let Some(id) = remote_id(q) {
        let Some(peer) = st.peers.get(id).await else {
            return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
        };
        return match peers::forward(&peer, method, path, body, std::time::Duration::from_secs(60)).await {
            Ok((code, b)) => (
                StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY),
                [(header::CONTENT_TYPE, "application/json")],
                b,
            )
                .into_response(),
            Err(e) => err(StatusCode::BAD_GATEWAY, e),
        };
    }
    match local().await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

macro_rules! adb_post {
    ($name:ident, $req:ty, $path:literal, $f:path) => {
        async fn $name(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>, body: Bytes) -> Response {
            let st2 = st.clone();
            let b = body.clone();
            local_or_peer(&st, &q, "POST", $path, &body, || async move {
                let r: $req = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
                $f(&st2, r).await
            })
            .await
        }
    };
}
adb_post!(adb_connect, wireless::ConnectReq, "/api/adb/connect", wireless::connect);
adb_post!(adb_pair, wireless::PairReq, "/api/adb/pair", wireless::pair);
adb_post!(adb_disconnect, wireless::SerialReq, "/api/adb/disconnect", wireless::disconnect);
adb_post!(adb_tcpip, wireless::SerialReq, "/api/adb/tcpip", wireless::tcpip);

async fn adb_qr_start(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let st2 = st.clone();
    local_or_peer(&st, &q, "POST", "/api/adb/qr", b"", || async move { wireless::qr_start(st2) }).await
}

async fn adb_qr_status(State(st): State<Shared>, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    let path = format!("/api/adb/qr/{}", util::url_encode(&id));
    let st2 = st.clone();
    local_or_peer(&st, &q, "GET", &path, b"", || async move {
        wireless::qr_status(&st2, &id)
            .map(|j| serde_json::to_value(j).unwrap_or_default())
            .ok_or_else(|| tr!("任务不存在", "Job not found"))
    })
    .await
}

async fn adb_qr_cancel(State(st): State<Shared>, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    let path = format!("/api/adb/qr/{}", util::url_encode(&id));
    let st2 = st.clone();
    local_or_peer(&st, &q, "DELETE", &path, b"", || async move {
        wireless::qr_cancel(&st2, &id);
        Ok(json!({"ok": true}))
    })
    .await
}

/// 本机 ~/.ssh 下可用的私钥文件
async fn ssh_keys() -> Json<serde_json::Value> {
    Json(json!({ "keys": remote::list_keys() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> Shared {
        let cfg = Config {
            bind: "127.0.0.1".into(),
            port: 0,
            https_port: 0,
            token: None,
            name: "test".into(),
            adb: PathBuf::from("adb"),
            data_dir: std::env::temp_dir().join("droidbridge-test-media"),
            server_jar: PathBuf::new(),
            discovery: false,
            media_dir: None,
        };
        Arc::new(AppState {
            peers: peers::PeerStore::load(cfg.data_dir.join("peers.json")),
            start_lock: tokio::sync::Mutex::new(()),
            jobs: std::sync::Mutex::new(HashMap::new()),
            discovery: discovery::Discovery::new(),
            wireless: wireless::Wireless::load(cfg.data_dir.join("wireless.json")),
            uids: std::sync::Mutex::new(HashMap::new()),
            media_dir_override: None,
            cfg,
        })
    }

    /// 构建 router 不应 panic（重复 path 会在 .route 时 panic）
    #[test]
    fn media_routes_register_without_panic() {
        let st = test_state();
        let _app = router(st);
    }
}
