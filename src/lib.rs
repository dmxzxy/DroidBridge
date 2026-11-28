//! DroidBridge 核心：HTTP/WebSocket 服务、adb/scrcpy 会话、远程节点中转。
//! 命令行版（src/main.rs）与桌面版（src/bin/desktop.rs）共用。

#[macro_use]
pub mod i18n;
pub mod access;
pub mod adb;
pub mod agent;
pub mod apps;
pub mod control;
pub mod discovery;
pub mod files;
pub mod icon;
pub mod logcat;
pub mod mcp;
pub mod mdns;
pub mod media;
pub mod peers;
pub mod perf;
pub mod plugin_host;
pub mod plugins;
pub mod remote;
pub mod scrcpy;
pub mod update;
pub mod util;
pub mod wireless;

use axum::body::Bytes;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::connect_info::ConnectInfo;
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

/// 拆分出去的样式与脚本（路径, Content-Type, 内容）
const ASSETS: &[(&str, &str, &str)] = &[
    ("app.css", "text/css; charset=utf-8", include_str!("../web/app.css")),
    ("js/i18n.js", "text/javascript; charset=utf-8", include_str!("../web/js/i18n.js")),
    ("js/core.js", "text/javascript; charset=utf-8", include_str!("../web/js/core.js")),
    ("js/ui.js", "text/javascript; charset=utf-8", include_str!("../web/js/ui.js")),
    ("js/api.js", "text/javascript; charset=utf-8", include_str!("../web/js/api.js")),
    ("js/devices.js", "text/javascript; charset=utf-8", include_str!("../web/js/devices.js")),
    ("js/peers.js", "text/javascript; charset=utf-8", include_str!("../web/js/peers.js")),
    ("js/viewer.js", "text/javascript; charset=utf-8", include_str!("../web/js/viewer.js")),
    ("js/logcat.js", "text/javascript; charset=utf-8", include_str!("../web/js/logcat.js")),
    ("js/audio.js", "text/javascript; charset=utf-8", include_str!("../web/js/audio.js")),
    ("js/gallery.js", "text/javascript; charset=utf-8", include_str!("../web/js/gallery.js")),
    ("js/files.js", "text/javascript; charset=utf-8", include_str!("../web/js/files.js")),
    ("js/update.js", "text/javascript; charset=utf-8", include_str!("../web/js/update.js")),
    ("js/titlebar.js", "text/javascript; charset=utf-8", include_str!("../web/js/titlebar.js")),
    ("js/contextmenu.js", "text/javascript; charset=utf-8", include_str!("../web/js/contextmenu.js")),
    ("js/access.js", "text/javascript; charset=utf-8", include_str!("../web/js/access.js")),
    ("js/plugins.js", "text/javascript; charset=utf-8", include_str!("../web/js/plugins.js")),
    ("js/plugin-sources.js", "text/javascript; charset=utf-8", include_str!("../web/js/plugin-sources.js")),
];

async fn asset_root(Path(file): Path<String>) -> Response {
    asset(Path(file)).await
}

async fn asset_js(Path(file): Path<String>) -> Response {
    asset(Path(format!("js/{file}"))).await
}

/// 静态资源：内容随程序一起编译，每次都用 no-cache 返回最新版本
async fn asset(Path(path): Path<String>) -> Response {
    match ASSETS.iter().find(|(p, _, _)| *p == path) {
        Some((_, ct, body)) => ([(header::CONTENT_TYPE, *ct), (header::CACHE_CONTROL, "no-cache")], *body).into_response(),
        None => err(StatusCode::NOT_FOUND, "Not found"),
    }
}

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
    /// 局域网内任何人无需授权即可访问（旧版行为）
    pub open: bool,
    /// 额外允许的主机名（反向代理、自定义域名）
    pub allow_hosts: Vec<String>,
    /// 安全模式：不加载任何插件（仍可在管理界面看到列表）
    pub no_plugins: bool,
}

const HELP_ZH: &str = "DroidBridge — 安卓设备投屏与远程操作

用法: droidbridge [选项]

  --port <端口>         HTTP 端口（默认 8730）
  --https-port <端口>   HTTPS 端口（默认 8731，0 表示关闭；手机/其他电脑浏览器访问需用 HTTPS）
  --bind <地址>         监听地址（默认 0.0.0.0）
  --token <令牌>        固定访问令牌，可代替授权（供脚本、AI 客户端、旧版本使用）
  --open                局域网内任何人无需授权即可访问（不推荐）
  --allow-host <主机名> 允许通过该域名访问（反向代理时使用，可重复）
  --name <名称>         本机显示名称（默认计算机名）
  --adb <路径>          adb 可执行文件路径（默认自动查找）
  --data <目录>         数据目录
  --media-dir <目录>    截图与录屏保存目录（默认 <data>/media）
  --lang <zh|en>        界面与提示语言（默认跟随系统）
  --no-discovery        不参与局域网自动发现
  --no-plugins          安全模式：不加载任何插件
  -h, --help            显示帮助
";

const HELP_EN: &str = "DroidBridge — mirror and control Android devices

Usage: droidbridge [options]

  --port <port>         HTTP port (default 8730)
  --https-port <port>   HTTPS port (default 8731, 0 to disable; browsers on other devices need HTTPS)
  --bind <addr>         Listen address (default 0.0.0.0)
  --token <token>       Fixed access token, accepted instead of authorization (scripts, AI clients, older versions)
  --open                Anyone on the LAN may connect without authorization (not recommended)
  --allow-host <host>   Allow access through this host name (reverse proxies; repeatable)
  --name <name>         Display name of this computer (default: host name)
  --adb <path>          Path to adb (found automatically by default)
  --data <dir>          Data directory
  --media-dir <dir>     Where to save screenshots and recordings (default: <data>/media)
  --lang <zh|en>        UI and message language (default: system)
  --no-discovery        Do not take part in LAN discovery
  --no-plugins          Safe mode: load no plugins
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
        let mut open = false;
        let mut no_plugins = false;
        let mut allow_hosts = Vec::new();

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
                "--no-plugins" => no_plugins = true,
                "--open" => open = true,
                "--allow-host" => allow_hosts.push(val("--allow-host")?),
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
            open,
            allow_hosts,
            no_plugins,
        }))
    }
}

/// `--authorize <名称> [--data <目录>]`：在数据目录里授权一个客户端并输出 `TOKEN=<令牌>`。
/// 通过 SSH 安装时由安装方调用（能 SSH 登录即证明是这台电脑的主人）。返回 None 表示不是该命令。
pub fn authorize_cli(args: Vec<String>) -> Option<i32> {
    let i = args.iter().position(|a| a == "--authorize")?;
    let Some(name) = args.get(i + 1).filter(|n| !n.starts_with("--")) else {
        eprintln!("--authorize <name>");
        return Some(2);
    };
    let data = args.iter().position(|a| a == "--data").and_then(|j| args.get(j + 1)).map(PathBuf::from).unwrap_or_else(default_data_dir);
    if let Err(e) = std::fs::create_dir_all(&data) {
        eprintln!("{e}");
        return Some(1);
    }
    let token = access::Access::load(&data, vec![], &[]).issue(name, "SSH");
    println!("TOKEN={token}");
    Some(0)
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
    /// AI Agent：设备状态与操作事件
    pub agent: agent::AgentState,
    /// 已授权的电脑与配对请求
    pub access: access::Access,
    /// 插件管理器
    pub plugins: plugins::Plugins,
    /// 共享投屏会话（同一台设备只有一个 scrcpy-server）
    pub mirrors: scrcpy::Mirrors,
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
    let plugins = plugins::Plugins::new(&cfg.data_dir);
    let state = Arc::new(AppState {
        peers: peers::PeerStore::load(cfg.data_dir.join("peers.json")),
        start_lock: tokio::sync::Mutex::new(()),
        jobs: std::sync::Mutex::new(HashMap::new()),
        discovery: discovery::Discovery::new(),
        wireless: wireless::Wireless::load(cfg.data_dir.join("wireless.json")),
        uids: std::sync::Mutex::new(HashMap::new()),
        media_dir_override,
        agent: agent::AgentState::default(),
        access: access::Access::load(&cfg.data_dir, cfg.allow_hosts.clone(), std::slice::from_ref(&cfg.name)),
        plugins,
        mirrors: scrcpy::Mirrors::default(),
        cfg,
    });
    let app = router(state.clone());
    if state.cfg.discovery {
        tokio::spawn(discovery::run(state.clone(), port));
    }
    tokio::spawn(wireless::keeper(state.clone()));
    // 插件管理器：扫描、启动、热重载与事件轮询
    tokio::spawn(plugins::run(state.clone()));
    tokio::spawn(plugins::sources_run(state.clone()));
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

/// 桌面版注册的“显示窗口”回调：再次启动 DroidBridge 时，由新进程请求已在运行的那个显示窗口
static SHOW_HOOK: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> = std::sync::OnceLock::new();

/// 桌面版调用：注册显示窗口的回调（只能注册一次）
pub fn set_show_hook(f: impl Fn() + Send + Sync + 'static) {
    let _ = SHOW_HOOK.set(Box::new(f));
}

/// 请求本机已在运行的 DroidBridge 桌面版显示窗口；对方是服务版或旧版本时返回 false
pub async fn request_show(addr: &str, token: Option<&str>) -> bool {
    let path = match token {
        Some(t) => format!("/api/app/show?token={}", util::url_encode(t)),
        None => "/api/app/show".to_string(),
    };
    match peers::http_request(addr, "POST", &path, b"{}", std::time::Duration::from_secs(3)).await {
        Ok((200, body)) => serde_json::from_slice::<serde_json::Value>(&body).ok().and_then(|v| v.get("shown").and_then(|s| s.as_bool())).unwrap_or(false),
        _ => false,
    }
}

/// 显示桌面版窗口（只接受本机请求）
async fn app_show(State(st): State<Shared>, ConnectInfo(addr): ConnectInfo<SocketAddr>) -> Response {
    if !st.access.is_local_ip(addr.ip()) {
        return err(StatusCode::FORBIDDEN, tr!("仅本机可执行此操作", "Only local requests can do this"));
    }
    match SHOW_HOOK.get() {
        Some(f) => {
            f();
            Json(json!({"shown": true})).into_response()
        }
        None => Json(json!({"shown": false})).into_response(),
    }
}

fn router(state: Shared) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/{file}", get(asset_root))
        .route("/js/{file}", get(asset_js))
        .route("/api/info", get(info))
        .route("/api/devices", get(devices))
        .route("/api/mirrors", get(mirrors_list))
        .route("/api/presence", get(presence))
        .route("/api/app/show", post(app_show))
        .route("/api/peers", get(list_peers).post(add_peer))
        .route("/api/peers/{id}", put(edit_peer).delete(remove_peer))
        .route("/api/peers/pair", post(peer_pair_start))
        .route("/api/peers/pair/{id}", get(peer_pair_status).delete(peer_pair_cancel))
        .route("/api/pair/request", post(pair_request))
        .route("/api/pair/request/{id}", get(pair_poll).delete(pair_cancel))
        .route("/api/pair/pending", get(pair_pending))
        .route("/api/pair/pending/{id}", post(pair_decide))
        .route("/api/clients", get(clients_list).post(clients_issue))
        .route("/api/clients/{id}", axum::routing::delete(clients_revoke))
        .route("/api/access", get(access_get).put(access_put))
        .route("/api/install", post(install_start))
        .route("/api/peers/{id}/update", post(peer_update))
        .route("/api/node", get(node_info))
        .route("/api/install/{id}", get(install_status))
        .route("/api/ssh/keys", get(ssh_keys))
        .route("/api/discovered", get(discovered))
        .route("/api/adb/connect", post(adb_connect))
        .route("/api/adb/pair", post(adb_pair))
        .route("/api/adb/disconnect", post(adb_disconnect))
        .route("/api/adb/tcpip", post(adb_tcpip))
        .route("/api/adb/qr", post(adb_qr_start))
        .route("/api/adb/qr/{id}", get(adb_qr_status).delete(adb_qr_cancel))
        .route("/api/push", post(push_file).layer(DefaultBodyLimit::max(peers::PUSH_MAX as usize)))
        .route("/api/media", get(media::media_list_handler))
        .route("/api/media/shot", post(media::media_shot_handler).layer(DefaultBodyLimit::max(64usize << 20)))
        .route("/api/media/file/{name}", get(media::media_file_handler))
        .route("/api/media/rec/start", post(media::media_rec_start_handler))
        .route("/api/media/rec/{id}/finish", post(media::media_rec_finish_handler))
        .route("/api/media/rec/{id}", axum::routing::post(media::media_rec_chunk_handler).delete(media::media_rec_abort_handler).layer(DefaultBodyLimit::max(64usize << 20)))
        .route("/api/media/{name}", axum::routing::delete(media::media_delete_handler))
        .route("/api/media/reveal", post(media::media_reveal_handler))
        .route("/api/media/copy", post(media::media_copy_handler))
        .route("/api/update/check", get(update_check))
        .route("/api/update/apply", post(update_apply))
        .route("/api/settings", axum::routing::get(media::settings_get_handler).put(media::settings_put_handler))
        .route("/ws/stream", get(ws_stream))
        .route("/ws/logcat", get(ws_logcat))
        .route("/ws/perf", get(ws_perf))
        .route("/mcp", post(mcp::handle).get(mcp::handle_get).delete(mcp::handle_get))
        .route("/api/agent/exec", post(agent_exec))
        .route("/api/files/list", post(files_list))
        .route("/api/files/mkdir", post(files_mkdir))
        .route("/api/files/rename", post(files_rename))
        .route("/api/files/delete", post(files_delete))
        .route("/api/files/upload", post(files_upload).layer(DefaultBodyLimit::max(peers::UPLOAD_MAX as usize)))
        .route("/api/files/download", get(files_download))
        .route("/api/files/preview", get(files_preview))
        .route("/api/files/install", post(files_install))
        .route("/api/agent/pause", post(agent_pause))
        .route("/api/apps", post(apps_list))
        .route("/api/apps/action", post(apps_action))
        .route("/api/plugins", get(plugins::plugins_get))
        .route("/api/plugins/manage", post(plugins::plugins_manage))
        .route("/api/plugins/sources", get(plugins::sources_get).post(plugins::sources_post))
        .route("/api/plugins/contrib", get(plugins::plugins_contrib))
        .route("/api/plugins/invoke", post(plugins::plugins_invoke))
        .route("/api/plugins/panel", post(plugins::plugins_panel))
        .route("/api/plugins/metrics", get(plugins::plugins_metrics))
        .route("/api/plugins/web", post(plugins::plugins_web))
        .route("/api/plugins/dialog", post(plugins::plugins_dialog))
        .route("/api/plugins/upload", post(plugins::plugins_upload).layer(DefaultBodyLimit::max(peers::UPLOAD_MAX as usize)))
        .route("/api/plugins/devop", post(plugins::plugins_devop).layer(DefaultBodyLimit::max(peers::UPLOAD_MAX as usize)))
        .route("/api/plugins/console", get(plugins::plugins_console))
        .route("/api/plugins/mcp", get(plugins::plugins_mcp_list))
        .route("/ws/plugins", get(plugins::plugins_ws))
        .route("/plugin-web/sdk.js", get(plugins::plugin_web_sdk))
        .route("/plugin-web/{id}/{*path}", get(plugins::plugin_web_file))
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

/// 把远程电脑的回复转给浏览器。对方的 401 表示“对方未授权本机 / 令牌失效”，
/// 不能原样转发：浏览器会当成本机要令牌而反复弹出口令框。
pub(crate) fn peer_reply(code: u16, content_type: String, body: Vec<u8>) -> Response {
    if code == 401 {
        return err(StatusCode::BAD_GATEWAY, tr!("对方电脑尚未授权本机（或令牌错误）", "Not authorized by the remote computer (or wrong token)"));
    }
    (StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY), [(header::CONTENT_TYPE, content_type)], body).into_response()
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

/// 不需要授权的接口：页面与静态资源、基本信息、发起配对
fn is_public(path: &str) -> bool {
    !(path.starts_with("/api/") || path.starts_with("/ws/") || path == "/mcp") || path == "/api/info" || path.starts_with("/api/pair/request")
}

async fn auth_inner(st: Shared, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let is_api = path.starts_with("/api/") || path.starts_with("/ws/") || path == "/mcp";
    let headers = req.headers();
    // 1. 浏览器里其他网站发来的请求一律拒绝（跨站请求、WebSocket 劫持）
    if is_api && !st.access.origin_ok(headers) {
        tracing::warn!("blocked cross-site request: {path} (Origin: {:?})", headers.get(header::ORIGIN));
        return err(StatusCode::FORBIDDEN, tr!("已拦截来自其他网站的请求", "Blocked a request from another website"));
    }
    if !is_api || is_public(&path) {
        return next.run(req).await;
    }
    // 2. 带有效令牌：启动参数 --token，或配对得到的专属令牌
    let q = util::parse_query(req.uri().query());
    let peer_ip = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
    let ip_s = peer_ip.map(|ip| ip.to_canonical().to_string()).unwrap_or_default();
    if let Some(given) = access::token_from(headers, &q) {
        let global = st.cfg.token.as_deref().is_some_and(|t| access::token_matches(&given, t));
        let client = if global { None } else { st.access.verify(&given) };
        if global || client.is_some() {
            let ident = match client {
                Some(id) => access::Ident {
                    name: st.access.client_name(&id).unwrap_or_else(|| ip_s.clone()),
                    key: format!("c:{id}"),
                    addr: ip_s.clone(),
                },
                None if is_local_request(&st, req.headers(), peer_ip, &q) => local_ident(&st),
                None => access::Ident { key: format!("ip:{ip_s}"), name: ip_s.clone(), addr: ip_s.clone() },
            };
            return run_as(&st, ident, req, next).await;
        }
    }
    // 3. 本机访问（经反向代理转发的除外；设置了 --token 时本机也需要令牌），或 --open：
    //    不需要令牌，但 Host 必须可信（防 DNS 重绑定）
    let local = is_local_request(&st, headers, peer_ip, &q);
    if local || is_open(&st) {
        if !st.access.local_host_ok(headers) {
            let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
            tracing::warn!("blocked request with untrusted Host: {host} {path}");
            return err(StatusCode::FORBIDDEN, tr!("不允许通过 {host} 访问（可用 --allow-host 允许）", "Access through {host} is not allowed (allow it with --allow-host)"));
        }
        let ident = if local { local_ident(&st) } else { access::Ident { key: format!("ip:{ip_s}"), name: ip_s.clone(), addr: ip_s } };
        return run_as(&st, ident, req, next).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": tr!("需要对方授权才能访问", "Authorization required"), "pair": true})),
    )
        .into_response()
}

fn local_ident(st: &AppState) -> access::Ident {
    access::Ident { key: "local".into(), name: st.cfg.name.clone(), addr: String::new() }
}

/// 记下这次访问是谁发起的（“谁连着这台电脑”），并把身份交给后面的处理函数
async fn run_as(st: &AppState, ident: access::Ident, mut req: Request, next: Next) -> Response {
    st.access.touch(&ident);
    req.extensions_mut().insert(ident);
    next.run(req).await
}

/// 谁连着这台电脑：最近 15 秒内访问过的电脑 / 浏览器，以及正在投屏的人。
/// `you` 标出发起这次查询的连接者自己。
pub fn presence_json(st: &AppState, me: Option<&str>) -> serde_json::Value {
    use std::collections::BTreeMap;
    let mut by: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let entry = |by: &mut BTreeMap<String, serde_json::Value>, key: &str, name: &str, addr: &str, ago: u64| {
        by.entry(key.to_string()).or_insert_with(|| json!({"key": key, "name": name, "addr": addr, "ago": ago, "you": Some(key) == me, "views": []}));
    };
    for (i, ago) in st.access.recent(std::time::Duration::from_secs(15)) {
        entry(&mut by, &i.key, &i.name, &i.addr, ago);
    }
    for m in st.mirrors.list() {
        for v in m.views() {
            let key = v["ident"].as_str().unwrap_or("").to_string();
            if key.is_empty() {
                continue;
            }
            entry(&mut by, &key, v["ident_name"].as_str().unwrap_or(""), v["addr"].as_str().unwrap_or(""), 0);
            if let Some(e) = by.get_mut(&key) {
                e["ago"] = json!(0);
                if let Some(a) = e["views"].as_array_mut() {
                    a.push(json!({"serial": m.serial, "who": v["who"], "controlling": v["controlling"]}));
                }
            }
        }
    }
    let mut list: Vec<serde_json::Value> = by.into_values().collect();
    list.sort_by_key(|e| (e["you"].as_bool() != Some(true), e["name"].as_str().unwrap_or("").to_lowercase()));
    json!({"list": list})
}

async fn presence(State(st): State<Shared>, ident: Option<axum::Extension<access::Ident>>, Query(q): Query<HashMap<String, String>>) -> Response {
    let me = ident.map(|e| e.0.key);
    let st2 = st.clone();
    local_or_peer(&st, &q, "GET", "/api/presence", b"", || async move { Ok(presence_json(&st2, me.as_deref())) }).await
}

/// 是不是“本机请求”（与 auth 中间件里的判断一致）：来源 IP 是本机、没有经过反向代理
/// （无 X-Forwarded-For / Forwarded）；设置了 `--token` 时，本机还要带上全局令牌。
/// 插件管理这类操作只允许本机请求。
pub fn is_local_request(st: &AppState, headers: &HeaderMap, ip: Option<std::net::IpAddr>, q: &HashMap<String, String>) -> bool {
    let forwarded = headers.contains_key("x-forwarded-for") || headers.contains_key(header::FORWARDED);
    if forwarded || !ip.is_some_and(|ip| st.access.is_local_ip(ip)) {
        return false;
    }
    match st.cfg.token.as_deref() {
        None => true,
        Some(expected) => access::token_from(headers, q).is_some_and(|t| access::token_matches(&t, expected)),
    }
}

async fn index() -> impl IntoResponse {
    let mut h = HeaderMap::new();
    h.insert(header::CACHE_CONTROL, "no-cache".parse().unwrap());
    (h, Html(INDEX_HTML))
}

async fn info(State(st): State<Shared>, req: Request) -> Json<serde_json::Value> {
    let q = util::parse_query(req.uri().query());
    info_json(&st, &req, &q)
}

fn info_json(st: &AppState, req: &Request, q: &HashMap<String, String>) -> Json<serde_json::Value> {
    let headers = req.headers();
    let peer_ip = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
    let has_valid_token = access::token_from(headers, q).is_some_and(|given| {
        st.cfg.token.as_deref().is_some_and(|t| access::token_matches(&given, t)) || st.access.verify(&given).is_some()
    });
    let show_ips = is_local_request(st, headers, peer_ip, q) || is_open(st) || has_valid_token;
    Json(json!({
        "app": "droidbridge",
        "name": st.cfg.name,
        "version": env!("CARGO_PKG_VERSION"),
        "build": remote::build_id(),
        "official": remote::OFFICIAL,
        "os": std::env::consts::OS,
        "lang": i18n::code(),
        "scrcpy": scrcpy_version(),
        "auth": !is_open(st) || st.cfg.token.is_some(),
        "pairing": true,
        "ips": if show_ips { util::local_ips() } else { Vec::<String>::new() },
        "port": st.cfg.port,
        "https_port": st.cfg.https_port,
    }))
}

fn remote_id(q: &HashMap<String, String>) -> Option<&str> {
    q.get("peer").map(String::as_str).filter(|p| !p.is_empty() && *p != "local")
}

async fn devices(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    match remote_id(&q) {
        None => match local_devices(&st).await {
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

/// 本机（或 ?peer= 指定的远程电脑）当前所有共享投屏会话的概况，供侧栏显示
async fn mirrors_list(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let st2 = st.clone();
    local_or_peer(&st, &q, "GET", "/api/mirrors", b"", || async move { Ok(scrcpy::mirrors_json(&st2)) }).await
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
            peers::Peer { id: id.clone(), name: name.trim().to_string(), addr, token: token.or(old.token), ssh: old.ssh }
        }
        None => {
            // 同一地址已在列表中（如重新安装、更新）：沿用原来的 id、名称和令牌，已打开的投屏不受影响
            let old_id = st.peers.list().await.into_iter().find(|p| p.addr == addr).map(|p| p.id);
            let old = match old_id { Some(i) => st.peers.get(&i).await, None => None };
            peers::Peer {
                id: old.as_ref().map(|p| p.id.clone()).unwrap_or_else(|| format!("p{:08x}", util::random_scid())),
                name: Some(name.trim()).filter(|n| !n.is_empty()).or(old.as_ref().map(|p| p.name.as_str())).unwrap_or("").to_string(),
                addr,
                token: token.or_else(|| old.as_ref().and_then(|p| p.token.clone())),
                ssh: None,
            }
        }
    };
    let info = peers::get_json(&peer, "/api/info")
        .await
        .map_err(|e| { let a = &peer.addr; tr!("无法访问 {a}：{e}", "Cannot reach {a}: {e}") })?;
    if info.get("app").and_then(|v| v.as_str()) != Some("droidbridge") {
        return Err(tr!("对端不是 DroidBridge", "The other side is not DroidBridge"));
    }
    let (code, _) = peers::forward(&peer, "GET", "/api/devices", b"", std::time::Duration::from_secs(10)).await?;
    // 只确认对方接受本机的令牌；对方 adb 暂时不可用等问题不影响添加
    if code == 401 {
        return Err(if info.get("pairing").and_then(|v| v.as_bool()) == Some(true) {
            need_pair_msg()
        } else {
            tr!("对方电脑尚未授权本机（或令牌错误）", "Not authorized by the remote computer (or wrong token)")
        });
    }
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

/// 对方需要授权（save_peer 返回此错误时，界面改为发起授权请求）
pub fn need_pair_msg() -> String {
    tr!("需要对方电脑授权本机", "This computer needs to be authorized by the remote computer")
}

fn save_peer_reply(r: Result<serde_json::Value, String>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) if e == need_pair_msg() => (StatusCode::FORBIDDEN, Json(json!({"error": e, "need_pair": true}))).into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, e),
    }
}

async fn add_peer(State(st): State<Shared>, Json(np): Json<NewPeer>) -> Response {
    save_peer_reply(save_peer(&st, None, &np.addr, &np.name, np.token).await)
}

async fn edit_peer(State(st): State<Shared>, Path(id): Path<String>, Json(np): Json<NewPeer>) -> Response {
    save_peer_reply(save_peer(&st, Some(id), &np.addr, &np.name, np.token).await)
}

// ---------------------------------------------------------------- 授权（配对）

fn client_ip(addr: SocketAddr) -> String {
    addr.ip().to_canonical().to_string()
}

#[derive(Deserialize)]
struct PairReq {
    #[serde(default)]
    name: String,
}

/// 对方（其他电脑或浏览器）请求授权：不需要令牌
async fn pair_request(State(st): State<Shared>, ConnectInfo(addr): ConnectInfo<SocketAddr>, Json(r): Json<PairReq>) -> Response {
    let ip = client_ip(addr);
    match st.access.pair_request(&r.name, &ip) {
        Ok((id, code)) => {
            tracing::info!("authorization request from {} ({ip})", r.name);
            Json(json!({"id": id, "code": code, "name": st.cfg.name})).into_response()
        }
        Err(e) => err(StatusCode::TOO_MANY_REQUESTS, e),
    }
}

async fn pair_poll(State(st): State<Shared>, Path(id): Path<String>) -> Json<serde_json::Value> {
    Json(st.access.pair_poll(&id))
}

async fn pair_cancel(State(st): State<Shared>, Path(id): Path<String>) -> Json<serde_json::Value> {
    st.access.pair_cancel(&id);
    Json(json!({"ok": true}))
}

/// 等待批准的请求（本机或 ?peer= 指定的远程电脑）
async fn pair_pending(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let st2 = st.clone();
    local_or_peer(&st, &q, "GET", "/api/pair/pending", b"", || async move { Ok(json!({"list": st2.access.pending_list()})) }).await
}

#[derive(Deserialize)]
struct Decide {
    allow: bool,
}

async fn pair_decide(State(st): State<Shared>, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>, body: Bytes) -> Response {
    let st2 = st.clone();
    let b = body.clone();
    let path = format!("/api/pair/pending/{}", util::url_encode(&id));
    local_or_peer(&st, &q, "POST", &path, &body, || async move {
        let d: Decide = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
        st2.access.pair_decide(&id, d.allow)?;
        Ok(json!({"ok": true}))
    })
    .await
}

async fn clients_list(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let st2 = st.clone();
    local_or_peer(&st, &q, "GET", "/api/clients", b"", || async move { Ok(json!({"list": st2.access.list()})) }).await
}

/// 手动生成访问令牌（给旧版本、脚本、AI 客户端用）；令牌只返回这一次。只能在本机直连（不含反向代理）生成
async fn clients_issue(State(st): State<Shared>, ConnectInfo(addr): ConnectInfo<SocketAddr>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>, Json(r): Json<PairReq>) -> Response {
    if !is_local_request(&st, &headers, Some(addr.ip()), &q) {
        return err(StatusCode::FORBIDDEN, tr!("只能在本机生成访问令牌", "Access tokens can only be created on the computer itself"));
    }
    let name = if r.name.trim().is_empty() { tr!("访问令牌", "Access token") } else { r.name };
    Json(json!({"token": st.access.issue(&name, "")})).into_response()
}

async fn clients_revoke(State(st): State<Shared>, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    let st2 = st.clone();
    let path = format!("/api/clients/{}", util::url_encode(&id));
    local_or_peer(&st, &q, "DELETE", &path, b"", || async move {
        if st2.access.revoke(&id) { Ok(json!({"ok": true})) } else { Err(tr!("授权不存在", "Authorization not found")) }
    })
    .await
}

/// 局域网内无需授权即可访问：启动参数 --open，或界面里打开了这项设置
pub(crate) fn is_open(st: &AppState) -> bool {
    st.cfg.open || st.access.is_open()
}

fn access_json(st: &AppState) -> serde_json::Value {
    json!({
        "discoverable": st.access.is_discoverable(),
        "discovery": st.cfg.discovery,
        "open": is_open(st),
        "open_forced": st.cfg.open,
        "fixed_token": st.cfg.token.is_some(),
    })
}

async fn access_get(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let st2 = st.clone();
    local_or_peer(&st, &q, "GET", "/api/access", b"", || async move { Ok(access_json(&st2)) }).await
}

#[derive(Deserialize)]
struct AccessPut {
    #[serde(default)]
    discoverable: Option<bool>,
    #[serde(default)]
    open: Option<bool>,
}

/// 修改“允许被局域网发现”“无需授权”。本机、已授权的电脑 / 浏览器（它们本来就能批准新的连接）
/// 或持有 --token 的人可以修改；因 --open 而未经授权进来的请求不行。
/// 带 ?peer= 时转发给远程电脑（例如用 SSH 安装、没有界面的电脑）。
async fn access_put(
    State(st): State<Shared>,
    ident: Option<axum::Extension<access::Ident>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    body: axum::body::Bytes,
) -> Response {
    if remote_id(&q).is_some() {
        return local_or_peer(&st, &q, "PUT", "/api/access", &body, || async { Ok(json!({})) }).await;
    }
    let r: AccessPut = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_REQUEST, e.to_string()),
    };
    let key = ident.map(|e| e.0.key).unwrap_or_default();
    let global = st.cfg.token.as_deref().is_some_and(|t| access::token_from(&headers, &q).is_some_and(|g| access::token_matches(&g, t)));
    if !(key == "local" || key.starts_with("c:") || global) {
        return err(StatusCode::FORBIDDEN, tr!("需要先获得这台电脑的授权", "Authorization from this computer is required"));
    }
    if r.open == Some(false) && st.cfg.open {
        return err(StatusCode::BAD_REQUEST, tr!("这台电脑以 --open 启动，需去掉该参数后重启", "This computer was started with --open; remove it and restart"));
    }
    let res = r.discoverable.map_or(Ok(()), |d| st.access.set_discoverable(d)).and_then(|_| r.open.map_or(Ok(()), |o| st.access.set_open(o)));
    match res {
        Ok(()) => Json(access_json(&st)).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct PairStart {
    addr: String,
    #[serde(default)]
    name: String,
    /// 已在列表中的远程电脑（重新授权）
    #[serde(default)]
    id: Option<String>,
}

/// 添加远程电脑时向对方请求授权
async fn peer_pair_start(State(st): State<Shared>, Json(r): Json<PairStart>) -> Response {
    let addr = match peers::normalize_addr(&r.addr) {
        Ok(a) => a,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    let body = json!({"name": st.cfg.name}).to_string();
    let (code, b) = match peers::http_request(&addr, "POST", "/api/pair/request", body.as_bytes(), std::time::Duration::from_secs(8)).await {
        Ok(x) => x,
        Err(e) => return err(StatusCode::BAD_GATEWAY, tr!("无法访问 {addr}：{e}", "Cannot reach {addr}: {e}")),
    };
    let v: serde_json::Value = serde_json::from_slice(&b).unwrap_or_default();
    if code == 404 || code == 405 || code == 401 {
        return err(StatusCode::BAD_GATEWAY, tr!("对方的 DroidBridge 版本较旧，不支持授权请求。请更新对方，或填写对方的访问令牌", "The remote DroidBridge is too old for authorization requests. Update it, or enter its access token"));
    }
    if code >= 400 {
        return err(StatusCode::BAD_GATEWAY, v.get("error").and_then(|e| e.as_str()).unwrap_or("error").to_string());
    }
    let (Some(remote_id), Some(pcode)) = (v.get("id").and_then(|x| x.as_str()), v.get("code").and_then(|x| x.as_str())) else {
        return err(StatusCode::BAD_GATEWAY, tr!("对端返回的内容无法识别", "Unexpected response from the remote computer"));
    };
    let job = format!("{:08x}{:08x}", util::random_scid(), util::random_scid());
    let mut out = st.access.outgoing.lock().unwrap();
    out.retain(|_, o| o.created.elapsed() < std::time::Duration::from_secs(600));
    out.insert(
        job.clone(),
        access::Outgoing { addr, name: r.name.trim().to_string(), peer_id: r.id, remote_id: remote_id.to_string(), created: std::time::Instant::now() },
    );
    Json(json!({"job": job, "code": pcode, "name": v.get("name").cloned().unwrap_or_default()})).into_response()
}

async fn peer_pair_status(State(st): State<Shared>, Path(job): Path<String>) -> Response {
    let Some(o) = st.access.outgoing.lock().unwrap().get(&job).cloned() else {
        return Json(json!({"state": "expired"})).into_response();
    };
    let path = format!("/api/pair/request/{}", util::url_encode(&o.remote_id));
    let v = match peers::http_request(&o.addr, "GET", &path, b"", std::time::Duration::from_secs(8)).await {
        Ok((_, b)) => serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default(),
        // 暂时连不上：继续等
        Err(_) => return Json(json!({"state": "pending"})).into_response(),
    };
    let state = v.get("state").and_then(|s| s.as_str()).unwrap_or("expired").to_string();
    if state == "pending" {
        return Json(json!({"state": "pending"})).into_response();
    }
    st.access.outgoing.lock().unwrap().remove(&job);
    if state != "approved" {
        return Json(json!({"state": state})).into_response();
    }
    let token = v.get("token").and_then(|t| t.as_str()).map(String::from);
    match save_peer(&st, o.peer_id.clone(), &o.addr, &o.name, token).await {
        Ok(p) => Json(json!({"state": "done", "peer": p})).into_response(),
        Err(e) => Json(json!({"state": "failed", "error": e})).into_response(),
    }
}

async fn peer_pair_cancel(State(st): State<Shared>, Path(job): Path<String>) -> Json<serde_json::Value> {
    let o = st.access.outgoing.lock().unwrap().remove(&job);
    if let Some(o) = o {
        let path = format!("/api/pair/request/{}", util::url_encode(&o.remote_id));
        let _ = peers::http_request(&o.addr, "DELETE", &path, b"", std::time::Duration::from_secs(5)).await;
    }
    Json(json!({"ok": true}))
}

async fn install_start(State(st): State<Shared>, Json(req): Json<remote::InstallReq>) -> Response {
    match remote::start(st, req) {
        Ok(id) => Json(json!({"job": id})).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

/// 一键更新：用保存的 SSH 登录方式把本机版本装到远程电脑
async fn peer_update(State(st): State<Shared>, Path(id): Path<String>) -> Response {
    let Some(peer) = st.peers.get(&id).await else {
        return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
    };
    let req = match remote::InstallReq::for_update(&peer) {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    match remote::start(st, req) {
        Ok(job) => Json(json!({"job": job})).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

/// 节点信息（版本、构建、系统）：本机或 ?peer= 指定的远程电脑
async fn node_info(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>, req: Request) -> Response {
    match remote_id(&q) {
        None => info_json(&st, &req, &q).into_response(),
        Some(id) => {
            let Some(peer) = st.peers.get(id).await else {
                return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
            };
            match peers::get_json(&peer, "/api/info").await {
                Ok(v) => Json(v).into_response(),
                Err(e) => err(StatusCode::BAD_GATEWAY, e),
            }
        }
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

/// 上传文件到设备：.apk 安装，其它文件推送到 /sdcard/Download/（流式接收与级联转发，大文件不占内存）
async fn push_file(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>, body: axum::body::Body) -> Response {
    let Some(serial) = q.get("serial").filter(|s| !s.is_empty()).cloned() else {
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

    if let Some(id) = remote_id(&q) {
        let Some(peer) = st.peers.get(id).await else {
            return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
        };
        let path = format!("/api/push?serial={}&name={}", util::url_encode(&serial), util::url_encode(&name));
        let clen = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|s| s.parse::<u64>().ok());
        return match peers::post_body_stream(&peer, &path, &[], clen, body, std::time::Duration::from_secs(600)).await {
            Ok(v) => Json(v).into_response(),
            Err(e) => err(StatusCode::BAD_GATEWAY, e),
        };
    }

    let is_apk = name.to_ascii_lowercase().ends_with(".apk");
    let dir = st.cfg.data_dir.join("uploads");
    let _ = tokio::fs::create_dir_all(&dir).await;
    let tmp = dir.join(format!("{:08x}{}", util::random_scid(), if is_apk { ".apk" } else { ".bin" }));
    match peers::save_body_to_file(body, &tmp, peers::PUSH_MAX).await {
        Ok(0) => return err(StatusCode::BAD_REQUEST, tr!("文件为空", "File is empty")),
        Ok(_) => {}
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
    let tmp_s = tmp.to_string_lossy().into_owned();
    let result = if is_apk {
        // 安装前后对比包名，拿到包名时通知插件 app.installed
        let before = adb::run(&st.cfg.adb, &["-s", &serial, "shell", "pm list packages -3"]).await.unwrap_or_default();
        let r = adb::run(&st.cfg.adb, &["-s", &serial, "install", "-r", "-t", &tmp_s]).await;
        if r.is_ok() {
            let after = adb::run(&st.cfg.adb, &["-s", &serial, "shell", "pm list packages -3"]).await.unwrap_or_default();
            for pkg in plugins::new_packages(&before, &after).iter().take(1) {
                plugins::on_installed(&st, &serial, pkg);
            }
        }
        r.map(|_| tr!("已安装 {name}", "Installed {name}"))
    } else {
        let dest = format!("/sdcard/Download/{name}");
        adb::run(&st.cfg.adb, &["-s", &serial, "push", &tmp_s, &dest])
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

async fn ws_perf(State(st): State<Shared>, ws: WebSocketUpgrade, req_uri: axum::http::Uri) -> Response {
    let raw_query = req_uri.query().map(String::from);
    let q = util::parse_query(raw_query.as_deref());
    let Some(serial) = q.get("serial").cloned().filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, tr!("缺少 serial 参数", "Missing serial parameter"));
    };
    if let Some(id) = remote_id(&q) {
        let Some(peer) = st.peers.get(id).await else {
            return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
        };
        return ws.on_upgrade(move |socket| peers::proxy_ws(socket, peer, "/ws/perf", raw_query));
    }
    let en = i18n::is_en();
    ws.on_upgrade(move |socket| i18n::REQUEST_EN.scope(en, perf::run(st, serial, socket)))
}

async fn ws_stream(State(st): State<Shared>, ident: Option<axum::Extension<access::Ident>>, ws: WebSocketUpgrade, req_uri: axum::http::Uri) -> Response {
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
        let dev_id = format!("{id}/{serial}");
        let tap = Some((st.clone(), dev_id));
        return ws.on_upgrade(move |socket| peers::proxy_ws_with_ctl(socket, peer, "/ws/stream", raw_query, tap));
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
    // 观看者身份（多人共享投屏）：id 与名称由前端提供，非法时后端生成 / 清洗
    let guest = scrcpy::Guest {
        id: scrcpy::sanitize_vid(q.get("vid").map(String::as_str)),
        name: scrcpy::sanitize_who(q.get("who").map(String::as_str)),
        force: q.get("force").is_some_and(|v| v == "1" || v == "true"),
        ident: ident.map(|e| e.0),
    };
    let en = i18n::is_en();
    ws.on_upgrade(move |socket| i18n::REQUEST_EN.scope(en, scrcpy::run_session(st, serial, opts, guest, socket)))
}

// ---------------------------------------------------------------- 发现与无线 adb

/// 读取无线设备的 ro.serialno（结果缓存），用于识别“同一台设备换了连接方式”
async fn fill_uids(st: &AppState, list: &mut [adb::Device]) {
    for d in list.iter_mut().filter(|d| d.wireless) {
        let cached = st.uids.lock().unwrap().get(&d.serial).cloned();
        let uid = match cached {
            Some(u) => u,
            None if d.state != "device" => String::new(),
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

/// 本机设备列表：补上无线设备的 uid，并把同一台设备的 USB 和无线连接合并成一条
pub(crate) async fn local_devices(st: &AppState) -> Result<Vec<adb::Device>, String> {
    let mut list = adb::list_devices(&st.cfg.adb).await?;
    fill_uids(st, &mut list).await;
    Ok(adb::merge_same(list))
}

/// 局域网中发现的其他节点（已添加的电脑和本机除外）。
/// 带 ?fresh=1 时先向局域网询问一遍谁在线：关机或关掉被发现的电脑不会在列表里停到超时
async fn discovered(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Json<serde_json::Value> {
    if q.contains_key("fresh") {
        st.discovery.probe();
    }
    let known: Vec<String> = st.peers.list().await.into_iter().map(|p| p.addr).collect();
    let mine: Vec<String> = util::local_ips().into_iter().map(|ip| format!("{ip}:{}", st.cfg.port)).collect();
    let list: Vec<_> = st
        .discovery
        .list()
        .into_iter()
        .filter(|f| !known.contains(&f.addr) && !mine.contains(&f.addr))
        .collect();
    Json(json!({"enabled": st.cfg.discovery, "discoverable": st.access.is_discoverable(), "list": list}))
}

/// 带 ?peer= 时把请求转发给远程电脑，否则在本机执行
pub(crate) async fn local_or_peer<F, Fut>(st: &AppState, q: &HashMap<String, String>, method: &str, path: &str, body: &[u8], local: F) -> Response
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<serde_json::Value, String>>,
{
    if let Some(id) = remote_id(q) {
        let Some(peer) = st.peers.get(id).await else {
            return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
        };
        return match peers::forward(&peer, method, path, body, std::time::Duration::from_secs(60)).await {
            Ok((code, b)) => peer_reply(code, "application/json".into(), b),
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
adb_post!(apps_list, apps::ListReq, "/api/apps", apps::list);

adb_post!(files_list, files::ListReq, "/api/files/list", files::list);
adb_post!(files_mkdir, files::TwoReq, "/api/files/mkdir", files::mkdir);
adb_post!(files_rename, files::TwoReq, "/api/files/rename", files::rename);
adb_post!(files_delete, files::DeleteReq, "/api/files/delete", files::delete);
adb_post!(files_install, files::TwoReq, "/api/files/install", files::install);

/// 上传文件到设备：?path=目录&name=文件名，body 为文件内容（流式接收与转发）
async fn files_upload(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>, body: axum::body::Body) -> Response {
    let Some(serial) = q.get("serial").filter(|s| !s.is_empty()).cloned() else {
        return err(StatusCode::BAD_REQUEST, tr!("缺少 serial 参数", "Missing serial parameter"));
    };
    let name = q.get("name").map(String::as_str).unwrap_or("file").to_string();
    if let Some(id) = remote_id(&q) {
        let Some(peer) = st.peers.get(id).await else {
            return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
        };
        let path = format!(
            "/api/files/upload?serial={}&path={}&name={}",
            util::url_encode(&serial),
            util::url_encode(q.get("path").map(String::as_str).unwrap_or("")),
            util::url_encode(&name)
        );
        let clen = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|s| s.parse::<u64>().ok());
        return match peers::post_body_stream(&peer, &path, &[], clen, body, std::time::Duration::from_secs(600)).await {
            Ok(v) => Json(v).into_response(),
            Err(e) => err(StatusCode::BAD_GATEWAY, e),
        };
    }
    let dir = q.get("path").map(String::as_str).unwrap_or("/sdcard").to_string();
    match files::upload_body(&st, &serial, &dir, &name, body).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

/// 从设备下载文件：?path=完整路径。远程电脑流式代理下载，本机设备直接 pull 流式返回
async fn files_download(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let Some(serial) = q.get("serial").filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, tr!("缺少 serial 参数", "Missing serial parameter"));
    };
    let Some(path) = q.get("path").filter(|p| !p.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, tr!("缺少 path 参数", "Missing path parameter"));
    };
    if let Some(id) = remote_id(&q) {
        let Some(peer) = st.peers.get(id).await else {
            return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
        };
        let target = format!("/api/files/download?serial={}&path={}", util::url_encode(serial), util::url_encode(path));
        return match peers::forward_download_stream(&peer, &target, std::time::Duration::from_secs(900)).await {
            Ok(resp) => resp,
            Err(e) => err(StatusCode::BAD_GATEWAY, e),
        };
    }
    match files::download(&st, serial, path).await {
        Ok(r) => r,
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

/// 预览 / 缩略图：?serial=&path=&thumb=像素（可选）；带 ?peer= 时转发给远程电脑
async fn files_preview(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>, uri: axum::http::Uri) -> Response {
    let Some(serial) = q.get("serial").filter(|s| !s.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, tr!("缺少 serial 参数", "Missing serial parameter"));
    };
    let Some(path) = q.get("path").filter(|p| !p.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, tr!("缺少 path 参数", "Missing path parameter"));
    };
    if let Some(id) = remote_id(&q) {
        let Some(peer) = st.peers.get(id).await else {
            return err(StatusCode::NOT_FOUND, tr!("远程电脑不存在", "Remote computer not found"));
        };
        let query: Vec<String> = util::parse_query(uri.query()).into_iter().filter(|(k, _)| k != "peer" && k != "token").map(|(k, v)| format!("{}={}", util::url_encode(&k), util::url_encode(&v))).collect();
        return match peers::forward_raw(&peer, &format!("/api/files/preview?{}", query.join("&")), std::time::Duration::from_secs(90)).await {
            Ok((code, ct, body)) => peer_reply(code, ct, body),
            Err(e) => err(StatusCode::BAD_GATEWAY, e),
        };
    }
    let thumb = q.get("thumb").and_then(|v| v.parse::<u32>().ok()).map(|v| v.clamp(32, 1024));
    match files::preview(&st, serial, path, thumb).await {
        Ok(r) => r,
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

/// 在本机执行 AI 工具（由收到 MCP 请求的节点转发过来）：{"tool":"tap","args":{"serial":..,..}}
async fn agent_exec(State(st): State<Shared>, Json(body): Json<serde_json::Value>) -> Response {
    let tool = body.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let args = body.get("args").cloned().unwrap_or(json!({}));
    match agent::exec(&st, tool, &args).await {
        Ok(content) => Json(json!({"content": content})).into_response(),
        Err(e) => Json(json!({"content": [{"type":"text","text": e}], "isError": true})).into_response(),
    }
}

#[derive(serde::Deserialize)]
pub struct PauseReq {
    serial: String,
    paused: bool,
}
async fn agent_pause(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>, body: Bytes) -> Response {
    let st2 = st.clone();
    let b = body.clone();
    local_or_peer(&st, &q, "POST", "/api/agent/pause", &body, || async move {
        let r: PauseReq = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
        st2.agent.set_paused(&r.serial, r.paused);
        Ok(json!({"ok": true, "paused": r.paused}))
    })
    .await
}
adb_post!(apps_action, apps::ActionReq, "/api/apps/action", apps::action);

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

/// 检查更新（只读；带 ?peer= 时检查远程电脑）
async fn update_check(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let st2 = st.clone();
    local_or_peer(&st, &q, "GET", "/api/update/check", b"", || async move { update::check(&st2).await }).await
}

/// 下载并安装更新：只在本机执行（远程电脑用 SSH 更新）
async fn update_apply(State(st): State<Shared>, ConnectInfo(addr): ConnectInfo<SocketAddr>) -> Response {
    if !addr.ip().is_loopback() {
        return err(StatusCode::FORBIDDEN, tr!("只允许在本机执行更新", "Updates can only be started on the computer itself"));
    }
    match update::apply(&st).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

/// 本机 ~/.ssh 下可用的私钥文件
async fn ssh_keys() -> Json<serde_json::Value> {
    Json(json!({ "keys": remote::list_keys() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> Shared {
        test_state_in(std::env::temp_dir().join("droidbridge-test-media"), "test")
    }

    fn test_state_in(data_dir: PathBuf, name: &str) -> Shared {
        test_state_with_adb(data_dir, name, PathBuf::from("adb-not-installed"))
    }

    fn test_state_with_adb(data_dir: PathBuf, name: &str, adb: PathBuf) -> Shared {
        test_state_full(data_dir, name, adb, None)
    }

    fn test_state_full(data_dir: PathBuf, name: &str, adb: PathBuf, token: Option<String>) -> Shared {
        let _ = std::fs::create_dir_all(&data_dir);
        let cfg = Config {
            bind: "127.0.0.1".into(),
            port: 0,
            https_port: 0,
            token,
            name: name.into(),
            adb,
            data_dir,
            server_jar: PathBuf::new(),
            discovery: false,
            media_dir: None,
            open: false,
            allow_hosts: vec![],
            no_plugins: false,
        };
        Arc::new(AppState {
            peers: peers::PeerStore::load(cfg.data_dir.join("peers.json")),
            start_lock: tokio::sync::Mutex::new(()),
            jobs: std::sync::Mutex::new(HashMap::new()),
            discovery: discovery::Discovery::new(),
            wireless: wireless::Wireless::load(cfg.data_dir.join("wireless.json")),
            uids: std::sync::Mutex::new(HashMap::new()),
            media_dir_override: None,
            agent: agent::AgentState::default(),
            access: access::Access::load(&cfg.data_dir, vec![], &[]),
            plugins: plugins::Plugins::new(&cfg.data_dir),
            mirrors: scrcpy::Mirrors::default(),
            cfg,
        })
    }

    /// 构建 router 不应 panic（重复 path 会在 .route 时 panic）
    #[test]
    fn media_routes_register_without_panic() {
        let st = test_state();
        let _app = router(st);
    }

    async fn call(app: &Router, from: &str, uri: &str, headers: &[(&str, &str)]) -> u16 {
        use tower::Service;
        let mut b = axum::http::Request::builder().uri(uri);
        if !headers.iter().any(|(k, _)| *k == "host") {
            b = b.header("host", "127.0.0.1:8730");
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let mut req = b.body(axum::body::Body::empty()).unwrap();
        req.extensions_mut().insert(ConnectInfo(from.parse::<SocketAddr>().unwrap()));
        let mut svc = app.clone();
        svc.call(req).await.unwrap().status().as_u16()
    }

    /// 访问控制：本机免授权、其他电脑需要授权、其他网站的请求被拦截
    #[tokio::test]
    async fn access_rules() {
        let st = test_state();
        let app = router(st.clone());
        let lan = "192.0.2.10:50000";
        // 本机
        assert_eq!(call(&app, "127.0.0.1:5000", "/api/access", &[]).await, 200);
        assert_eq!(call(&app, "127.0.0.1:5000", "/api/access", &[("origin", "http://127.0.0.1:8730")]).await, 200);
        // 本机浏览器里的其他网站（跨站请求 / WebSocket 劫持）
        assert_eq!(call(&app, "127.0.0.1:5000", "/api/access", &[("origin", "https://evil.example")]).await, 403);
        assert_eq!(call(&app, "127.0.0.1:5000", "/ws/stream?serial=x", &[("origin", "https://evil.example")]).await, 403);
        // DNS 重绑定
        assert_eq!(call(&app, "127.0.0.1:5000", "/api/access", &[("host", "evil.example:8730")]).await, 403);
        // 经本机反向代理转发的请求不算本机
        assert_eq!(call(&app, "127.0.0.1:5000", "/api/access", &[("x-forwarded-for", "192.0.2.10")]).await, 401);
        // 其他电脑：未授权
        assert_eq!(call(&app, lan, "/api/access", &[]).await, 401);
        assert_eq!(call(&app, lan, "/api/devices", &[("authorization", "Bearer wrong")]).await, 401);
        // 公开接口与页面
        assert_eq!(call(&app, lan, "/api/info", &[]).await, 200);
        assert_eq!(call(&app, lan, "/", &[("sec-fetch-site", "cross-site")]).await, 200);
        assert_eq!(call(&app, lan, "/api/pair/request/nope", &[]).await, 200);
        // 授权后可访问；撤销后不能
        let token = st.access.issue("PC", "192.0.2.10");
        assert_eq!(call(&app, lan, &format!("/api/access?token={token}"), &[]).await, 200);
        assert_eq!(call(&app, lan, "/api/access", &[("authorization", &format!("Bearer {token}"))]).await, 200);
        let id = st.access.list()[0].id.clone();
        st.access.revoke(&id);
        assert_eq!(call(&app, lan, &format!("/api/access?token={token}"), &[]).await, 401);
    }

    /// “谁连着这台电脑”：授权的电脑访问过之后出现在列表里，查询者自己标为 you
    #[tokio::test]
    async fn presence_lists_connected_clients() {
        let st = test_state_in(std::env::temp_dir().join(format!("droidbridge-presence-{:08x}", util::random_scid())), "Host");
        let app = router(st.clone());
        let token = st.access.issue("Office Mac", "192.0.2.30");
        assert_eq!(call(&app, "192.0.2.30:5000", &format!("/api/access?token={token}"), &[]).await, 200);
        let (code, v) = call_json(&app, "GET", "/api/presence", json!({})).await;
        assert_eq!(code, 200, "{v}");
        let list = v["list"].as_array().unwrap();
        let mac = list.iter().find(|e| e["name"] == "Office Mac").expect("authorized client listed");
        assert_eq!(mac["you"], false);
        assert_eq!(mac["addr"], "192.0.2.30");
        let me = list.iter().find(|e| e["key"] == "local").expect("local listed");
        assert_eq!(me["you"], true);
        assert_eq!(list[0]["you"], true, "the requester comes first");
        // 以授权电脑的身份查询：它自己是 you
        let mut req = axum::http::Request::builder().uri(format!("/api/presence?token={token}")).header("host", "127.0.0.1:8730").body(axum::body::Body::empty()).unwrap();
        req.extensions_mut().insert(ConnectInfo("192.0.2.30:5000".parse::<SocketAddr>().unwrap()));
        let res = tower::Service::call(&mut app.clone(), req).await.unwrap();
        let b = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
        let mac = v["list"].as_array().unwrap().iter().find(|e| e["name"] == "Office Mac").unwrap().clone();
        assert_eq!(mac["you"], true);
    }

    async fn call_json(app: &Router, method: &str, uri: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
        use tower::Service;
        let mut req = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:8730")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        req.extensions_mut().insert(ConnectInfo("127.0.0.1:5000".parse::<SocketAddr>().unwrap()));
        let mut svc = app.clone();
        let res = svc.call(req).await.unwrap();
        let code = res.status().as_u16();
        let b = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        (code, serde_json::from_slice(&b).unwrap_or_default())
    }

    /// 完整流程：A 添加 B → B 要求授权 → A 发起请求 → B 允许 → A 保存专属令牌并能访问 B；B 撤销后 A 不能再访问
    #[tokio::test]
    async fn pairing_end_to_end() {
        let base = std::env::temp_dir().join(format!("droidbridge-pair-{:08x}", util::random_scid()));
        let a = test_state_in(base.join("a"), "A");
        let b = test_state_in(base.join("b"), "B");
        // B 把所有连接都当作来自局域网的其他电脑
        let b_app = router(b.clone()).layer(axum::middleware::map_request(|mut req: Request| async move {
            req.extensions_mut().insert(ConnectInfo("192.0.2.20:40000".parse::<SocketAddr>().unwrap()));
            req
        }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let b_addr = l.local_addr().unwrap().to_string();
        tokio::spawn(async move { axum::serve(l, b_app.into_make_service()).await.unwrap() });
        let a_app = router(a.clone());

        let (code, v) = call_json(&a_app, "POST", "/api/peers", json!({"addr": b_addr})).await;
        assert_eq!(code, 403, "{v}");
        assert_eq!(v["need_pair"], true);

        let (code, v) = call_json(&a_app, "POST", "/api/peers/pair", json!({"addr": b_addr})).await;
        assert_eq!(code, 200, "{v}");
        let job = v["job"].as_str().unwrap().to_string();
        assert_eq!(v["name"], "B");
        let pending = b.access.pending_list();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["name"], "A");
        assert_eq!(pending[0]["code"], v["code"]);

        let (_, v) = call_json(&a_app, "GET", &format!("/api/peers/pair/{job}"), json!({})).await;
        assert_eq!(v["state"], "pending");
        b.access.pair_decide(pending[0]["id"].as_str().unwrap(), true).unwrap();
        let (_, v) = call_json(&a_app, "GET", &format!("/api/peers/pair/{job}"), json!({})).await;
        assert_eq!(v["state"], "done", "{v}");
        let pid = v["peer"]["id"].as_str().unwrap().to_string();
        assert_eq!(b.access.list().len(), 1);

        // 通过 A 访问 B 的接口：已授权
        let (code, v) = call_json(&a_app, "GET", &format!("/api/clients?peer={pid}"), json!({})).await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(v["list"][0]["name"], "A");

        // 在 A 的界面里修改 B 的访问设置（B 可能没有界面）
        let (code, v) = call_json(&a_app, "PUT", &format!("/api/access?peer={pid}"), json!({"discoverable": false, "open": true})).await;
        assert_eq!(code, 200, "{v}");
        assert!(!b.access.is_discoverable() && b.access.is_open());
        let (_, v) = call_json(&a_app, "GET", &format!("/api/access?peer={pid}"), json!({})).await;
        assert_eq!(v["open"], true, "{v}");
        b.access.set_open(false).unwrap();

        // B 撤销后 A 不能再访问；对方的 401 不能原样转给浏览器（否则浏览器会反复要本机令牌）
        b.access.revoke(&b.access.list()[0].id);
        let (code, v) = call_json(&a_app, "GET", &format!("/api/clients?peer={pid}"), json!({})).await;
        assert_eq!(code, 502, "{v}");
        assert!(v.get("pair").is_none(), "{v}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 1.2.4 验证：未授权外部请求访问 /api/info 不泄露网卡 IP 列表，且经反向代理或非回环 IP 禁止签发访问令牌
    #[tokio::test]
    async fn info_redacts_ips_for_unauthenticated_callers_and_guards_clients_issue() {
        use tower::Service;
        let dir = std::env::temp_dir().join(format!("droidbridge-infosec-{:08x}", util::random_scid()));
        let st = test_state_in(dir.clone(), "HostSec");
        let app = router(st.clone());

        // 1) 局域网未授权请求 /api/info：能拿到基本信息供配对使用，但 ips 为空数组
        let mut req = axum::http::Request::builder().uri("/api/info").header("host", "192.0.2.1:8730").body(axum::body::Body::empty()).unwrap();
        req.extensions_mut().insert(ConnectInfo("192.0.2.55:50000".parse::<SocketAddr>().unwrap()));
        let res = app.clone().call(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(v["ips"], json!([]));

        // 2) 授权令牌访问 /api/info：允许读取 ips
        let tok = st.access.issue("Trusted", "192.0.2.55");
        let mut req2 = axum::http::Request::builder()
            .uri(format!("/api/info?token={tok}"))
            .header("host", "192.0.2.1:8730")
            .body(axum::body::Body::empty())
            .unwrap();
        req2.extensions_mut().insert(ConnectInfo("192.0.2.55:50000".parse::<SocketAddr>().unwrap()));
        let res2 = app.clone().call(req2).await.unwrap();
        let v2: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(res2.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(v2["ips"], json!(util::local_ips()));

        // 3) 即使源 IP 是 127.0.0.1，只要带 X-Forwarded-For 反向代理头（即使附带客户端 token），也绝不允许调用 POST /api/clients 签发新令牌
        let mut req3 = axum::http::Request::builder()
            .method("POST")
            .uri("/api/clients")
            .header("host", "127.0.0.1:8730")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {tok}"))
            .header("x-forwarded-for", "192.0.2.99")
            .body(axum::body::Body::from(r#"{"name":"evil"}"#))
            .unwrap();
        req3.extensions_mut().insert(ConnectInfo("127.0.0.1:5000".parse::<SocketAddr>().unwrap()));
        let res3 = app.clone().call(req3).await.unwrap();
        assert_eq!(res3.status(), StatusCode::FORBIDDEN);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 启用 --token 时，本机界面用 ?token= 也能生成访问令牌
    #[tokio::test]
    async fn clients_issue_accepts_query_token() {
        use tower::Service;
        let dir = std::env::temp_dir().join(format!("droidbridge-issue-{:08x}", util::random_scid()));
        let st = test_state_full(dir.clone(), "HostTok", PathBuf::from("adb-not-installed"), Some("s3cret".into()));
        let app = router(st);
        let call = |uri: &str| {
            let mut req = axum::http::Request::builder()
                .method("POST")
                .uri(uri)
                .header("host", "127.0.0.1:8730")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(r#"{"name":"cli"}"#))
                .unwrap();
            req.extensions_mut().insert(ConnectInfo("127.0.0.1:5000".parse::<SocketAddr>().unwrap()));
            app.clone().call(req)
        };
        assert_eq!(call("/api/clients?token=s3cret").await.unwrap().status(), StatusCode::OK);
        assert_ne!(call("/api/clients?token=wrong").await.unwrap().status(), StatusCode::OK);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 同步操作：连在一组的设备，一台上的按下 / 移动 / 抬起按顺序同步到正在投屏的远程设备，并在它的画面上显示扩散圈
    #[tokio::test]
    async fn linked_devices_relay_full_gestures_to_remote_mirror() {
        let dir = std::env::temp_dir().join(format!("droidbridge-link-{:08x}", util::random_scid()));
        let st = test_state_in(dir.clone(), "HostLink");
        st.plugins.load();
        let ph = st.plugins.get("group-control").unwrap();
        st.plugins.set_state(&ph, plugins::PluginState::Running, None);
        *ph.link.lock().unwrap() = vec!["S1".into(), "pc/S2".into()];
        let (mut rx, tx) = st.plugins.remote_attach("pc/S2");
        let mut events = st.plugins.subscribe();

        let touch = |action: u8, x: i32| control::Ctl::Touch { action, id: -1, x, y: 500, w: 1080, h: 2400, p: 1.0, buttons: 0 };
        for (a, x) in [(0, 100), (2, 200), (2, 300), (1, 400)] {
            st.plugins.on_user_ctl(&st, "S1", &touch(a, x), None);
        }
        st.plugins.on_user_ctl(&st, "S1", &control::Ctl::Text { text: "hi".into() }, None);
        let mut got = Vec::new();
        while let Ok(c) = rx.try_recv() {
            got.push(match c {
                control::Ctl::Touch { action, x, .. } => format!("t{action}:{x}"),
                control::Ctl::Text { text } => format!("text:{text}"),
                other => format!("{other:?}"),
            });
        }
        assert_eq!(got, vec!["t0:100", "t2:200", "t2:300", "t1:400", "text:hi"]);
        let ev = events.try_recv().expect("应发出扩散圈事件");
        let v: serde_json::Value = serde_json::from_str(&ev.text).unwrap();
        assert_eq!((v["type"].as_str(), v["serial"].as_str()), (Some("ripple"), Some("pc/S2")));

        // 不在组里的设备、组被清空后都不再同步
        st.plugins.on_user_ctl(&st, "S3", &touch(0, 1), None);
        *ph.link.lock().unwrap() = vec![];
        st.plugins.on_user_ctl(&st, "S1", &touch(0, 1), None);
        assert!(rx.try_recv().is_err());
        st.plugins.remote_detach("pc/S2", &tx);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 流式保存请求体超过上限时中止并删除文件
    #[tokio::test]
    async fn save_body_enforces_limit() {
        let dir = std::env::temp_dir().join(format!("droidbridge-limit-{:08x}", util::random_scid()));
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("x.bin");
        let ok = peers::save_body_to_file(axum::body::Body::from(vec![1u8; 100]), &f, 100).await;
        assert_eq!(ok, Ok(100));
        let big = peers::save_body_to_file(axum::body::Body::from(vec![1u8; 101]), &f, 100).await;
        assert!(big.is_err());
        assert!(!f.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 1.2.1 & 1.2.3 端到端测试：流式分块上传转发（A → B）与远程节点设备文件流式下载（含 Content-Disposition 保留）
    #[tokio::test]
    async fn streaming_upload_and_remote_files_download_end_to_end() {
        use tower::Service;
        let base = std::env::temp_dir().join(format!("droidbridge-stream-{:08x}", util::random_scid()));
        let _ = std::fs::create_dir_all(&base);

        // 在 B 上放置一个模拟 adb 脚本：pull 时生成 192 KB 测试文件，满足 files::download 真实执行路径
        let sample_src = base.join("sample_device_file.bin");
        let expected_dl: Vec<u8> = (0..(192 * 1024)).map(|i| ((i * 31 + 7) & 0xff) as u8).collect();
        std::fs::write(&sample_src, &expected_dl).unwrap();

        let fake_adb = if cfg!(windows) {
            let p = base.join("fake-adb.cmd");
            // 参数: -s S1 pull /sdcard/Download/demo.mp4 <local_dest>
            let script = format!("@echo off\r\nif \"%~3\"==\"pull\" (\r\n  copy /y \"{}\" \"%~5\" >nul\r\n  exit /b 0\r\n)\r\nexit /b 0\r\n", sample_src.display());
            std::fs::write(&p, script).unwrap();
            p
        } else {
            let p = base.join("fake-adb.sh");
            let script = format!("#!/bin/sh\nif [ \"$3\" = \"pull\" ]; then cp \"{}\" \"$5\"; exit 0; fi\nexit 0\n", sample_src.display());
            std::fs::write(&p, script).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
            }
            p
        };

        let a = test_state_in(base.join("a"), "NodeA");
        let b = test_state_with_adb(base.join("b"), "NodeB", fake_adb);

        let b_token = b.access.issue("NodeA", "127.0.0.1");
        let b_app = router(b.clone()).layer(axum::middleware::map_request(|mut req: Request| async move {
            req.extensions_mut().insert(ConnectInfo("192.0.2.88:41000".parse::<SocketAddr>().unwrap()));
            req
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let b_addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move { axum::serve(listener, b_app.into_make_service()).await.unwrap() });

        let peer_res = save_peer(&a, None, &b_addr, "NodeB", Some(b_token)).await.expect("save_peer");
        let pid = peer_res["id"].as_str().unwrap().to_string();
        let a_app = router(a.clone());

        // 1) 测试未知 Content-Length 的多分块流式上传经 A 转发到 B（Transfer-Encoding: chunked → B 流式落盘）
        let upload_chunks: Vec<Vec<u8>> = (0..8).map(|c| vec![b'A' + c; 32 * 1024]).collect();
        let expected_up: Vec<u8> = upload_chunks.iter().flatten().copied().collect();
        let body_stream = futures_util::stream::iter(upload_chunks.into_iter().map(Ok::<Vec<u8>, std::io::Error>));
        let mut up_req = axum::http::Request::builder()
            .method("POST")
            .uri(format!("/api/plugins/upload?peer={pid}&name=test_bundle.apk"))
            .header("host", "127.0.0.1:8730")
            .body(axum::body::Body::from_stream(body_stream))
            .unwrap();
        up_req.extensions_mut().insert(ConnectInfo("127.0.0.1:5000".parse::<SocketAddr>().unwrap()));
        let up_res = a_app.clone().call(up_req).await.unwrap();
        assert_eq!(up_res.status(), StatusCode::OK);
        let up_json: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(up_res.into_body(), 65536).await.unwrap()).unwrap();
        let saved_on_b = PathBuf::from(up_json["path"].as_str().expect("returned path on B"));
        assert_eq!(std::fs::read(&saved_on_b).unwrap(), expected_up, "流式级联上传后的文件内容应字节级一致");

        // 2) 测试远程节点设备文件流式下载 `/api/files/download?peer=<pid>&serial=S1&path=/sdcard/Download/demo.mp4`
        let mut dl_req = axum::http::Request::builder()
            .method("GET")
            .uri(format!("/api/files/download?peer={pid}&serial=S1&path=/sdcard/Download/demo.mp4"))
            .header("host", "127.0.0.1:8730")
            .body(axum::body::Body::empty())
            .unwrap();
        dl_req.extensions_mut().insert(ConnectInfo("127.0.0.1:5000".parse::<SocketAddr>().unwrap()));
        let dl_res = a_app.clone().call(dl_req).await.unwrap();
        assert_eq!(dl_res.status(), StatusCode::OK);
        let disp = dl_res
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(disp.contains("demo.mp4"), "应保留远程文件下载的 Content-Disposition 文件名: {disp}");
        let dl_bytes = axum::body::to_bytes(dl_res.into_body(), 1 << 20).await.unwrap();
        assert_eq!(dl_bytes.as_ref(), expected_dl.as_slice(), "流式代理下载的文件内容应完整无损");

        let _ = std::fs::remove_dir_all(&base);
    }
}
