//! DroidBridge 桌面版：独立窗口，内置服务端，同时作为局域网节点。
//! Windows 使用 WebView2，macOS 使用 WKWebView，Linux 使用 WebKitGTK。

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use droidbridge::{tr, Config, StartError};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::{Icon, WindowBuilder};
use wry::{NewWindowResponse, WebContext, WebViewBuilder};

fn main() {
    // 作为 SSH_ASKPASS 被 ssh 调用时，只输出密码后退出
    if droidbridge::remote::askpass_hook() {
        return;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = match Config::from_args(args.clone()) {
        Ok(Some(c)) => Ok(c),
        Ok(None) => Err(droidbridge::help().to_string()),
        Err(e) => Err(e),
    };
    let data_dir = cfg.as_ref().map(|c| c.data_dir.clone()).unwrap_or_else(|_| droidbridge::default_data_dir());
    let _ = std::fs::create_dir_all(&data_dir);
    init_log(&data_dir);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let target = match cfg {
        Ok(cfg) => rt.block_on(launch(cfg, args)),
        Err(e) => Err(e),
    };
    if let Err(e) = &target {
        tracing::error!("startup failed: {e}");
    }

    if let Err(e) = run_window(target, &data_dir) {
        tracing::error!("cannot create window: {e}");
        eprintln!("{e}");
    }
    drop(rt);
}

/// 启动内置服务；端口被另一个 DroidBridge 占用时直接连接它，被其他程序占用时改用随机端口。
async fn launch(cfg: Config, args: Vec<String>) -> Result<String, String> {
    let port = cfg.port;
    match droidbridge::start(cfg).await {
        Ok(s) => {
            tracing::info!("server started: http://127.0.0.1:{}", s.port);
            Ok(format!("http://127.0.0.1:{}/?app=desktop&lang={}", s.port, droidbridge::i18n::code()))
        }
        Err(StartError::AddrInUse(_)) => {
            if droidbridge::probe(&format!("127.0.0.1:{port}")).await {
                tracing::info!("DroidBridge is already running on port {port}; connecting to it");
                return Ok(format!("http://127.0.0.1:{port}/?app=desktop&lang={}", droidbridge::i18n::code()));
            }
            tracing::warn!("port {port} is used by another program; using a random port (other computers cannot add this one)");
            let mut cfg = Config::from_args(args).ok().flatten().ok_or_else(|| tr!("参数错误", "Invalid arguments"))?;
            cfg.port = 0;
            cfg.https_port = 0;
            let s = droidbridge::start(cfg).await.map_err(|e| e.to_string())?;
            Ok(format!("http://127.0.0.1:{}/?app=desktop&lang={}", s.port, droidbridge::i18n::code()))
        }
        Err(e) => Err(e.to_string()),
    }
}

fn run_window(target: Result<String, String>, data_dir: &std::path::Path) -> Result<(), String> {
    let event_loop = EventLoop::new();
    #[cfg(target_os = "macos")]
    let _menu = mac_menu();

    let window = WindowBuilder::new()
        .with_title("DroidBridge")
        .with_inner_size(LogicalSize::new(1280.0, 820.0))
        .with_min_inner_size(LogicalSize::new(640.0, 480.0))
        .with_window_icon(app_icon())
        .build(&event_loop)
        .map_err(|e| e.to_string())?;

    let mut ctx = WebContext::new(Some(data_dir.join("webview")));
    let builder = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_background_color((15, 17, 21, 255))
        .with_new_window_req_handler(|url, _| {
            open_external(&url);
            NewWindowResponse::Deny
        });
    #[cfg(windows)]
    let builder = {
        use wry::WebViewBuilderExtWindows;
        builder.with_browser_accelerator_keys(false)
    };
    let builder = match &target {
        Ok(url) => builder.with_url(url.as_str()),
        Err(e) => builder.with_html(error_page(e)),
    };

    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().ok_or("no GTK container")?;
        builder.build_gtk(vbox)
    };
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window);
    let webview = webview.map_err(|e| {
        if cfg!(windows) {
            tr!(
                "无法创建 WebView2 窗口：{e}。请安装 Microsoft Edge WebView2 运行时（Win10/11 一般已自带）",
                "Cannot create the WebView2 window: {e}. Please install the Microsoft Edge WebView2 Runtime"
            )
        } else if cfg!(target_os = "linux") {
            tr!(
                "无法创建窗口：{e}。请安装 WebKitGTK（如 libwebkit2gtk-4.1-0）",
                "Cannot create the window: {e}. Please install WebKitGTK (e.g. libwebkit2gtk-4.1-0)"
            )
        } else {
            tr!("无法创建窗口：{e}", "Cannot create the window: {e}")
        }
    })?;

    event_loop.run(move |event, _, control_flow| {
        let _keep = (&webview, &ctx, &window);
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *control_flow = ControlFlow::Exit;
        }
    });
}

/// macOS 菜单栏：没有“编辑”菜单时，WebView 中的复制 / 粘贴快捷键不可用
#[cfg(target_os = "macos")]
fn mac_menu() -> Option<muda::Menu> {
    use muda::{Menu, PredefinedMenuItem as P, Submenu};
    let app = Submenu::with_items(
        "DroidBridge",
        true,
        &[&P::about(None, None), &P::separator(), &P::services(None), &P::separator(), &P::hide(None), &P::hide_others(None), &P::separator(), &P::quit(None)],
    )
    .ok()?;
    let edit = Submenu::with_items(
        tr!("编辑", "Edit"),
        true,
        &[&P::undo(None), &P::redo(None), &P::separator(), &P::cut(None), &P::copy(None), &P::paste(None), &P::select_all(None)],
    )
    .ok()?;
    let win = Submenu::with_items(
        tr!("窗口", "Window"),
        true,
        &[&P::minimize(None), &P::maximize(None), &P::fullscreen(None), &P::separator(), &P::close_window(None)],
    )
    .ok()?;
    let menu = Menu::with_items(&[&app, &edit, &win]).ok()?;
    menu.init_for_nsapp();
    Some(menu)
}

fn open_external(url: &str) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .creation_flags(0x0800_0000)
            .spawn();
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

fn error_page(msg: &str) -> String {
    let esc = msg.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let title = tr!("DroidBridge 无法启动", "DroidBridge failed to start");
    format!(
        r#"<!doctype html><meta charset="utf-8"><body style="margin:0;height:100vh;display:grid;place-items:center;background:#0f1115;color:#e6e8ec;font:14px -apple-system,'Segoe UI','Microsoft YaHei',sans-serif">
<div style="max-width:520px;padding:24px"><div style="font-size:16px;font-weight:600;margin-bottom:8px">{title}</div>
<pre style="white-space:pre-wrap;color:#8b93a3;font:13px/1.6 Consolas,Menlo,monospace">{esc}</pre></div></body>"#
    )
}

fn init_log(data_dir: &std::path::Path) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,hyper=warn".into());
    match std::fs::File::create(data_dir.join("desktop.log")) {
        Ok(f) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(f))
            .init(),
        Err(_) => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}

/// 程序化绘制窗口图标：圆角手机轮廓
fn app_icon() -> Option<Icon> {
    let (rgba, n) = droidbridge::icon::rgba(64);
    Icon::from_rgba(rgba, n, n).ok()
}
