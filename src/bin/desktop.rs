//! DroidBridge 桌面版：独立窗口，内置服务端，同时作为局域网节点。
//! Windows 使用 WebView2，macOS 使用 WKWebView，Linux 使用 WebKitGTK。

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use droidbridge::{tr, Config, StartError};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Icon, ResizeDirection, WindowBuilder};
use wry::{NewWindowResponse, WebContext, WebViewBuilder};

fn main() {
    // 插件宿主进程（--plugin-host）：同步运行，不创建任何窗口
    if droidbridge::plugin_host::hook() {
        return;
    }
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
        Ok(cfg) => match rt.block_on(launch(cfg, args)) {
            // 已有一个 DroidBridge 在运行，并且已经把它的窗口显示出来了
            Ok(None) => return,
            Ok(Some(url)) => Ok(url),
            Err(e) => Err(e),
        },
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

/// 启动内置服务；端口被另一个 DroidBridge 占用时：它是桌面版就让它显示窗口（返回 None），
/// 否则直接连接它；被其他程序占用时改用随机端口。
async fn launch(cfg: Config, args: Vec<String>) -> Result<Option<String>, String> {
    let port = cfg.port;
    let token = cfg.token.clone();
    match droidbridge::start(cfg).await {
        Ok(s) => {
            tracing::info!("server started: http://127.0.0.1:{}", s.port);
            Ok(Some(format!("http://127.0.0.1:{}/?app=desktop&lang={}", s.port, droidbridge::i18n::code())))
        }
        Err(StartError::AddrInUse(_)) => {
            let addr = format!("127.0.0.1:{port}");
            if droidbridge::probe(&addr).await {
                // 已在运行的桌面版（可能缩在托盘里）：让它显示窗口，本进程退出。
                // Windows 只允许前台进程把别的窗口切到前面：先把这个权限让给它
                #[cfg(windows)]
                unsafe {
                    use windows_sys::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY};
                    AllowSetForegroundWindow(ASFW_ANY);
                }
                if droidbridge::request_show(&addr, token.as_deref()).await {
                    tracing::info!("DroidBridge is already running on port {port}; showed its window");
                    return Ok(None);
                }
                tracing::info!("DroidBridge is already running on port {port}; connecting to it");
                return Ok(Some(format!("http://127.0.0.1:{port}/?app=desktop&lang={}", droidbridge::i18n::code())));
            }
            tracing::warn!("port {port} is used by another program; using a random port (other computers cannot add this one)");
            let mut cfg = Config::from_args(args).ok().flatten().ok_or_else(|| tr!("参数错误", "Invalid arguments"))?;
            cfg.port = 0;
            cfg.https_port = 0;
            let s = droidbridge::start(cfg).await.map_err(|e| e.to_string())?;
            Ok(Some(format!("http://127.0.0.1:{}/?app=desktop&lang={}", s.port, droidbridge::i18n::code())))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// 网页标题栏发来的窗口操作（window.ipc.postMessage）
#[derive(Debug)]
enum WinCmd {
    Drag,
    Minimize,
    ToggleMaximize,
    /// 标题栏的关闭按钮（按“关闭窗口时”的设置处理）
    Close,
    /// 关闭确认对话框的选择：最小化到托盘 / 退出，是否记住
    CloseChoice(ClosePref, bool),
    /// 设置里修改“关闭窗口时”
    SetClosePref(ClosePref),
    Resize(ResizeDirection),
    /// 显示窗口（托盘、再次启动程序）
    Show,
    Quit,
    Tray(tray_icon::TrayIconEvent),
    /// 插件请求打开网页：应用内窗口，按 name 复用（关闭只是隐藏，再次打开立即显示）
    OpenWin { url: String, name: String, title: String },
}

/// 关闭窗口时：最小化到托盘 / 退出 / 每次询问
#[derive(Debug, Clone, Copy, PartialEq)]
enum ClosePref {
    Tray,
    Quit,
    Ask,
}

impl ClosePref {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "tray" => Self::Tray,
            "quit" => Self::Quit,
            "ask" => Self::Ask,
            _ => return None,
        })
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Tray => "tray",
            Self::Quit => "quit",
            Self::Ask => "ask",
        }
    }
    /// 读取 data/desktop.json；没有时为“每次询问”（第一次关闭时弹出选择）
    fn load(data_dir: &std::path::Path) -> Self {
        std::fs::read(data_dir.join("desktop.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v.get("close").and_then(|c| c.as_str()).and_then(Self::parse))
            .unwrap_or(Self::Ask)
    }
    fn save(self, data_dir: &std::path::Path) {
        let v = serde_json::json!({ "close": self.as_str() });
        if let Err(e) = std::fs::write(data_dir.join("desktop.json"), v.to_string()) {
            tracing::warn!("save desktop.json: {e}");
        }
    }
}

impl WinCmd {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "drag" => Self::Drag,
            "min" => Self::Minimize,
            "max" => Self::ToggleMaximize,
            "close" => Self::Close,
            _ if s.starts_with("close:") => {
                let mut it = s["close:".len()..].split(':');
                let pref = ClosePref::parse(it.next()?)?;
                Self::CloseChoice(pref, it.next() == Some("save"))
            }
            _ if s.starts_with("closepref:") => Self::SetClosePref(ClosePref::parse(&s["closepref:".len()..])?),
            _ if s.starts_with("openwin:") => {
                let v: serde_json::Value = serde_json::from_str(&s["openwin:".len()..]).ok()?;
                let url = v.get("url")?.as_str()?.to_string();
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return None;
                }
                let name = v.get("name").and_then(|n| n.as_str()).filter(|n| !n.is_empty()).unwrap_or(&url).to_string();
                let title = v.get("title").and_then(|n| n.as_str()).unwrap_or("").to_string();
                Self::OpenWin { url, name, title }
            }
            _ => Self::Resize(match s.strip_prefix("resize:")? {
                "n" => ResizeDirection::North,
                "s" => ResizeDirection::South,
                "e" => ResizeDirection::East,
                "w" => ResizeDirection::West,
                "ne" => ResizeDirection::NorthEast,
                "nw" => ResizeDirection::NorthWest,
                "se" => ResizeDirection::SouthEast,
                "sw" => ResizeDirection::SouthWest,
                _ => return None,
            }),
        })
    }
}

/// 网页据此决定标题栏样式：mac 保留系统红绿灯按钮，其他平台由网页绘制最小化 / 最大化 / 关闭
const CHROME: &str = if cfg!(target_os = "macos") { "mac" } else if cfg!(windows) { "win" } else { "linux" };

fn run_window(target: Result<String, String>, data_dir: &std::path::Path) -> Result<(), String> {
    let event_loop = EventLoopBuilder::<WinCmd>::with_user_event().build();
    let mut close_pref = ClosePref::load(data_dir);
    let data_dir = data_dir.to_path_buf();
    // 托盘事件、再次启动程序时的“显示窗口”请求，都转到事件循环里处理
    {
        let proxy = event_loop.create_proxy();
        tray_icon::TrayIconEvent::set_event_handler(Some(move |e| {
            let _ = proxy.send_event(WinCmd::Tray(e));
        }));
        let proxy = event_loop.create_proxy();
        tray_icon::menu::MenuEvent::set_event_handler(Some(move |e: tray_icon::menu::MenuEvent| {
            let cmd = match e.id.0.as_str() {
                "show" => WinCmd::Show,
                "quit" => WinCmd::Quit,
                _ => return,
            };
            let _ = proxy.send_event(cmd);
        }));
        let proxy = std::sync::Mutex::new(event_loop.create_proxy());
        droidbridge::set_show_hook(move || {
            let _ = proxy.lock().unwrap().send_event(WinCmd::Show);
        });
    }
    #[cfg(target_os = "macos")]
    let _menu = mac_menu();

    // 正常启动时使用网页绘制的标题栏；启动失败的错误页保留系统标题栏，方便关闭窗口
    let custom = target.is_ok();
    let builder = WindowBuilder::new()
        .with_title("DroidBridge")
        .with_inner_size(LogicalSize::new(1280.0, 820.0))
        .with_min_inner_size(LogicalSize::new(640.0, 480.0))
        .with_window_icon(app_icon());
    #[cfg(target_os = "macos")]
    let builder = {
        use tao::platform::macos::WindowBuilderExtMacOS;
        builder
            .with_titlebar_transparent(custom)
            .with_title_hidden(custom)
            .with_fullsize_content_view(custom)
    };
    #[cfg(not(target_os = "macos"))]
    let builder = builder.with_decorations(!custom);
    #[cfg(windows)]
    let builder = {
        use tao::platform::windows::WindowBuilderExtWindows;
        builder.with_undecorated_shadow(true)
    };
    let window = builder.build(&event_loop).map_err(|e| e.to_string())?;

    let proxy = event_loop.create_proxy();
    let mut ctx = WebContext::new(Some(data_dir.join("webview")));
    // 托盘：Linux 缺少 AppIndicator 库、或创建失败时，关闭窗口就直接退出
    let tray_ok = tray_supported();
    let builder = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_background_color((15, 17, 21, 255))
        .with_new_window_req_handler(|url, _| {
            open_external(&url);
            NewWindowResponse::Deny
        });
    let builder = if custom {
        builder
            .with_initialization_script(format!(
                "window.__DB_CHROME={CHROME:?};window.__DB_TRAY={tray_ok};window.__DB_CLOSE={:?};",
                close_pref.as_str()
            ))
            .with_ipc_handler(move |req| {
                if let Some(cmd) = WinCmd::parse(req.body()) {
                    let _ = proxy.send_event(cmd);
                }
            })
    } else {
        builder
    };
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

    let mut last_state = (false, true);
    let mut tray: Option<tray_icon::TrayIcon> = None;
    let mut tray_ok = tray_ok;
    let main_id = window.id();
    // 插件打开的应用内窗口：name -> (窗口, WebView, 当前地址)
    let mut subs: std::collections::HashMap<String, (tao::window::Window, wry::WebView, String)> = std::collections::HashMap::new();
    event_loop.run(move |event, target, control_flow| {
        let _keep = &webview;
        *control_flow = ControlFlow::Wait;
        let show = |window: &tao::window::Window| {
            window.set_visible(true);
            window.set_minimized(false);
            window.set_focus();
        };
        match event {
            // 托盘要在事件循环启动后创建（macOS 的要求）
            Event::NewEvents(tao::event::StartCause::Init) if tray_ok => match build_tray() {
                Ok(t) => tray = Some(t),
                Err(e) => {
                    tracing::warn!("tray icon unavailable: {e}");
                    tray_ok = false;
                    let _ = webview.evaluate_script("window.__DB_TRAY=false");
                }
            },
            // 插件窗口：关闭只是隐藏，页面和连接保留
            Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } if window_id != main_id => {
                if let Some((w, _, _)) = subs.values().find(|(w, _, _)| w.id() == window_id) {
                    w.set_visible(false);
                }
            }
            Event::UserEvent(WinCmd::OpenWin { url, name, title }) => {
                if let Some((w, wv, cur)) = subs.get_mut(&name) {
                    if *cur != url {
                        let _ = wv.load_url(&url);
                        *cur = url;
                    }
                    if !title.is_empty() {
                        w.set_title(&title);
                    }
                    show(w);
                } else {
                    match open_sub_window(target, &mut ctx, &url, &title) {
                        Ok((w, wv)) => {
                            subs.insert(name, (w, wv, url));
                        }
                        Err(e) => {
                            tracing::warn!("open window failed: {e}");
                            open_external(&url);
                        }
                    }
                }
            }
            Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } if window_id == main_id => {
                main_close(&mut *control_flow, custom, tray_ok, close_pref, &window, &webview, &show);
            }
            Event::UserEvent(WinCmd::Close) => {
                main_close(&mut *control_flow, custom, tray_ok, close_pref, &window, &webview, &show);
            }
            Event::UserEvent(WinCmd::CloseChoice(pref, save)) => {
                if save {
                    close_pref = pref;
                    pref.save(&data_dir);
                }
                if pref == ClosePref::Quit {
                    *control_flow = ControlFlow::Exit;
                } else {
                    window.set_visible(false);
                }
            }
            Event::UserEvent(WinCmd::SetClosePref(pref)) => {
                close_pref = pref;
                pref.save(&data_dir);
            }
            Event::UserEvent(WinCmd::Show) => show(&window),
            Event::UserEvent(WinCmd::Quit) => *control_flow = ControlFlow::Exit,
            // 托盘：左键单击 / 双击显示窗口，右键弹出菜单（macOS 单击即弹出菜单）
            Event::UserEvent(WinCmd::Tray(e)) => {
                use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
                match e {
                    TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } if !cfg!(target_os = "macos") => show(&window),
                    TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => show(&window),
                    _ => {}
                }
            }
            // macOS：点 Dock 图标时重新显示窗口
            Event::Reopen { .. } => show(&window),
            Event::LoopDestroyed => {
                tray.take();
            }
            Event::UserEvent(WinCmd::Drag) => {
                let _ = window.drag_window();
            }
            Event::UserEvent(WinCmd::Minimize) => window.set_minimized(true),
            Event::UserEvent(WinCmd::ToggleMaximize) => window.set_maximized(!window.is_maximized()),
            Event::UserEvent(WinCmd::Resize(dir)) => {
                if !window.is_maximized() {
                    let _ = window.drag_resize_window(dir);
                }
            }
            // 把最大化 / 焦点状态告诉网页：切换“还原”图标、标题栏变暗
            Event::WindowEvent { window_id, event: WindowEvent::Resized(_) | WindowEvent::Focused(_), .. } if custom && window_id == main_id => {
                let state = (window.is_maximized(), window.is_focused());
                if state != last_state {
                    last_state = state;
                    let _ = webview.evaluate_script(&format!(
                        "window.__dbWinState&&window.__dbWinState({{max:{},focus:{}}})",
                        state.0, state.1
                    ));
                }
            }
            _ => {}
        }
    });
}

/// 主窗口关闭：启动失败的错误页、没有托盘时直接退出；否则按“关闭窗口时”的设置处理
fn main_close(
    control_flow: &mut ControlFlow,
    custom: bool,
    tray_ok: bool,
    close_pref: ClosePref,
    window: &tao::window::Window,
    webview: &wry::WebView,
    show: &dyn Fn(&tao::window::Window),
) {
    let pref = if !custom || !tray_ok { ClosePref::Quit } else { close_pref };
    match pref {
        ClosePref::Quit => *control_flow = ControlFlow::Exit,
        ClosePref::Tray => window.set_visible(false),
        ClosePref::Ask => {
            show(window);
            let _ = webview.evaluate_script("window.__dbAskClose&&window.__dbAskClose()");
        }
    }
}

/// 插件打开的应用内窗口：系统标题栏，和主窗口共用 WebView 存储（登录状态等），没有 IPC
fn open_sub_window(
    target: &tao::event_loop::EventLoopWindowTarget<WinCmd>,
    ctx: &mut WebContext,
    url: &str,
    title: &str,
) -> Result<(tao::window::Window, wry::WebView), String> {
    let window = WindowBuilder::new()
        .with_title(if title.is_empty() { "DroidBridge" } else { title })
        .with_inner_size(LogicalSize::new(1180.0, 800.0))
        .with_min_inner_size(LogicalSize::new(420.0, 320.0))
        .with_window_icon(app_icon())
        .build(target)
        .map_err(|e| e.to_string())?;
    let builder = WebViewBuilder::new_with_web_context(ctx)
        .with_background_color((15, 17, 21, 255))
        .with_url(url)
        .with_new_window_req_handler(|url, _| {
            open_external(&url);
            NewWindowResponse::Deny
        });
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().ok_or("no GTK container")?;
        builder.build_gtk(vbox)
    };
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window);
    let webview = webview.map_err(|e| e.to_string())?;
    Ok((window, webview))
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

/// 托盘图标与菜单：打开 / 退出
fn build_tray() -> Result<tray_icon::TrayIcon, String> {
    use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
    let menu = Menu::new();
    menu.append_items(&[
        &MenuItem::with_id("show", tr!("打开 DroidBridge", "Open DroidBridge"), true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id("quit", tr!("退出", "Quit"), true, None),
    ])
    .map_err(|e| e.to_string())?;
    // macOS 菜单栏用只有线条的模板图标（系统按明暗模式着色）；其他平台用应用图标，
    // 深色任务栏 / 面板上用反色版本，免得黑色方块和任务栏混在一起
    let (rgba, n) = if cfg!(target_os = "macos") {
        droidbridge::icon::tray_rgba(44, false)
    } else if taskbar_is_light() {
        droidbridge::icon::rgba(32)
    } else {
        droidbridge::icon::rgba_inverted(32)
    };
    let icon = tray_icon::Icon::from_rgba(rgba, n, n).map_err(|e| e.to_string())?;
    let builder = tray_icon::TrayIconBuilder::new().with_id("droidbridge");
    #[cfg(target_os = "macos")]
    let builder = builder.with_icon_templated(icon);
    #[cfg(not(target_os = "macos"))]
    let builder = builder.with_icon(icon);
    builder
        .with_tooltip("DroidBridge")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(cfg!(target_os = "macos"))
        .build()
        .map_err(|e| e.to_string())
}

/// 任务栏是否为浅色（Windows 读取系统主题设置；Linux 面板多为深色）
fn taskbar_is_light() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
        let key: Vec<u16> = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize".encode_utf16().chain([0]).collect();
        let name: Vec<u16> = "SystemUsesLightTheme".encode_utf16().chain([0]).collect();
        let mut data: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let r = unsafe {
            RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&mut data as *mut u32).cast(), &mut size)
        };
        r == 0 && data == 1
    }
    #[cfg(not(windows))]
    false
}

/// 能否显示托盘图标：Linux 需要系统里有 AppIndicator 库
fn tray_supported() -> bool {
    if !cfg!(target_os = "linux") {
        return true;
    }
    let libs = ["libayatana-appindicator3.so.1", "libappindicator3.so.1"];
    let dirs = ["/usr/lib", "/usr/lib64", "/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu", "/usr/local/lib"];
    libs.iter().any(|l| dirs.iter().any(|d| std::path::Path::new(d).join(l).exists()))
}

/// 程序化绘制窗口图标
fn app_icon() -> Option<Icon> {
    let (rgba, n) = droidbridge::icon::rgba(64);
    Icon::from_rgba(rgba, n, n).ok()
}

#[cfg(test)]
mod tests {
    use super::WinCmd;

    #[test]
    fn parses_open_window_requests() {
        match WinCmd::parse(r#"openwin:{"url":"http://a/b?c=1","name":"tool:x","title":"T"}"#) {
            Some(WinCmd::OpenWin { url, name, title }) => {
                assert_eq!(url, "http://a/b?c=1");
                assert_eq!(name, "tool:x");
                assert_eq!(title, "T");
            }
            other => panic!("{other:?}"),
        }
        // 没有 name 时用地址作为窗口名
        assert!(matches!(WinCmd::parse(r#"openwin:{"url":"https://x"}"#), Some(WinCmd::OpenWin { name, .. }) if name == "https://x"));
        // 只接受 http(s)
        assert!(WinCmd::parse(r#"openwin:{"url":"file:///c:/x"}"#).is_none());
        assert!(WinCmd::parse(r#"openwin:{"url":"javascript:alert(1)"}"#).is_none());
        assert!(WinCmd::parse("openwin:not json").is_none());
    }
}
