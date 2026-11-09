//! DroidBridge 核心：HTTP/WebSocket 服务、adb/scrcpy 会话、远程节点中转。
//! 命令行版（src/main.rs）与桌面版（src/bin/desktop.rs）共用。

pub mod adb;
pub mod control;
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

pub const HELP: &str = "DroidBridge — 安卓设备投屏与远程操作

用法: droidbridge [选项]

  --port <端口>         HTTP 端口（默认 8730）
  --https-port <端口>   HTTPS 端口（默认 8731，0 表示关闭；手机/其他电脑浏览器访问需用 HTTPS）
  --bind <地址>         监听地址（默认 0.0.0.0）
  --token <令牌>        访问令牌；设置后所有接口都需要令牌
  --name <名称>         本机显示名称（默认计算机名）
  --adb <路径>          adb 可执行文件路径（默认自动查找）
  --data <目录>         数据目录（默认 exe 同目录下 data）
  -h, --help            显示帮助
";

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

        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            let mut val = |n: &str| it.next().ok_or(format!("{n} 需要一个参数值"));
            match a.as_str() {
                "--port" => port = val("--port")?.parse().map_err(|_| "--port 无效")?,
                "--https-port" => https_port = val("--https-port")?.parse().map_err(|_| "--https-port 无效")?,
                "--bind" => bind = val("--bind")?,
                "--token" => token = Some(val("--token")?).filter(|t| !t.is_empty()),
                "--name" => name = Some(val("--name")?),
                "--adb" => adb = Some(PathBuf::from(val("--adb")?)),
                "--data" => data = Some(PathBuf::from(val("--data")?)),
                "-h" | "--help" => return Ok(None),
                other => return Err(format!("未知参数：{other}\n\n{HELP}")),
            }
        }

        let data_dir = data.unwrap_or_else(default_data_dir);
        let name = name
            .or_else(|| std::env::var("COMPUTERNAME").ok())
            .or_else(|| std::env::var("HOSTNAME").ok())
            .unwrap_or_else(|| "本机".into());
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

pub fn default_data_dir() -> PathBuf {
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
            StartError::AddrInUse(a) => write!(f, "端口 {a} 已被占用（可用 --port 修改）"),
            StartError::Other(s) => f.write_str(s),
        }
    }
}

/// 准备数据目录与 scrcpy-server，绑定端口并在后台启动 HTTP/HTTPS 服务。
pub async fn start(cfg: Config) -> Result<Started, StartError> {
    std::fs::create_dir_all(&cfg.data_dir)
        .map_err(|e| StartError::Other(format!("无法创建数据目录 {}：{e}", cfg.data_dir.display())))?;
    std::fs::write(&cfg.server_jar, SCRCPY_SERVER)
        .map_err(|e| StartError::Other(format!("无法写出 scrcpy-server：{e}")))?;

    let http_addr: SocketAddr = format!("{}:{}", cfg.bind, cfg.port)
        .parse()
        .map_err(|e| StartError::Other(format!("监听地址无效：{e}")))?;
    let listener = tokio::net::TcpListener::bind(http_addr).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            StartError::AddrInUse(http_addr)
        } else {
            StartError::Other(format!("无法监听 {http_addr}：{e}"))
        }
    })?;
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(cfg.port);

    match adb::run(&cfg.adb, &["start-server"]).await {
        Ok(_) => tracing::info!("adb: {}", cfg.adb.display()),
        Err(e) => tracing::error!("adb 不可用：{e}（可用 --adb 指定路径）"),
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
                                tracing::error!("HTTPS 服务退出：{e}");
                            }
                        }
                        Err(e) => tracing::error!("HTTPS 初始化失败：{e}"),
                    }
                });
            }
            Err(e) => {
                tracing::error!("HTTPS 地址无效：{e}");
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
            tracing::error!("HTTP 服务退出：{e}");
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

async fn auth(State(st): State<Shared>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let protected = (path.starts_with("/api/") && path != "/api/info") || path.starts_with("/ws/");
    if let (true, Some(expected)) = (protected, st.cfg.token.as_deref()) {
        let q = util::parse_query(req.uri().query());
        let given = q
            .get("token")
            .cloned()
            .or_else(|| req.headers().get("x-token").and_then(|v| v.to_str().ok()).map(String::from));
        if given.as_deref() != Some(expected) {
            return err(StatusCode::UNAUTHORIZED, "需要访问令牌");
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
                return err(StatusCode::NOT_FOUND, "远程电脑不存在");
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
            let old = st.peers.get(id).await.ok_or("远程电脑不存在")?;
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
        .map_err(|e| format!("无法访问 {}：{e}", peer.addr))?;
    if info.get("app").and_then(|v| v.as_str()) != Some("droidbridge") {
        return Err("对端不是 DroidBridge".into());
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
        None => err(StatusCode::NOT_FOUND, "任务不存在"),
    }
}

async fn remove_peer(State(st): State<Shared>, Path(id): Path<String>) -> Response {
    match st.peers.remove(&id).await {
        Ok(true) => Json(json!({"ok": true})).into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, "远程电脑不存在"),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// 上传文件到设备：.apk 安装，其它文件推送到 /sdcard/Download/
async fn push_file(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>, body: Bytes) -> Response {
    let Some(serial) = q.get("serial").filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, "缺少 serial 参数");
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
        return err(StatusCode::BAD_REQUEST, "文件为空");
    }

    if let Some(id) = remote_id(&q) {
        let Some(peer) = st.peers.get(id).await else {
            return err(StatusCode::NOT_FOUND, "远程电脑不存在");
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
        return err(StatusCode::INTERNAL_SERVER_ERROR, format!("保存文件失败：{e}"));
    }
    let tmp_s = tmp.to_string_lossy().into_owned();
    let result = if is_apk {
        adb::run(&st.cfg.adb, &["-s", serial, "install", "-r", "-t", &tmp_s])
            .await
            .map(|_| format!("已安装 {name}"))
    } else {
        let dest = format!("/sdcard/Download/{name}");
        adb::run(&st.cfg.adb, &["-s", serial, "push", &tmp_s, &dest])
            .await
            .map(|_| format!("已传到手机 Download/{name}"))
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
        return err(StatusCode::BAD_REQUEST, "缺少 serial 参数");
    };
    if let Some(id) = remote_id(&q) {
        let Some(peer) = st.peers.get(id).await else {
            return err(StatusCode::NOT_FOUND, "远程电脑不存在");
        };
        return ws.on_upgrade(move |socket| peers::proxy_ws(socket, peer, "/ws/logcat", raw_query));
    }
    ws.on_upgrade(move |socket| logcat::run(st, serial, socket))
}

async fn ws_stream(State(st): State<Shared>, ws: WebSocketUpgrade, req_uri: axum::http::Uri) -> Response {
    let raw_query = req_uri.query().map(String::from);
    let q = util::parse_query(raw_query.as_deref());
    let Some(serial) = q.get("serial").cloned().filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, "缺少 serial 参数");
    };
    let ws = ws.max_message_size(64 << 20).max_frame_size(64 << 20);

    if let Some(id) = remote_id(&q) {
        let Some(peer) = st.peers.get(id).await else {
            return err(StatusCode::NOT_FOUND, "远程电脑不存在");
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
    ws.on_upgrade(move |socket| scrcpy::run_session(st, serial, opts, socket))
}
