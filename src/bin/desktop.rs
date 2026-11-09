//! DroidBridge 桌面版：独立窗口（系统自带的 WebView2），内置服务端。
//! 同时作为局域网节点，其他电脑可以把本机添加为远程电脑。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use droidbridge::{Config, StartError};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::{Icon, WindowBuilder};
use wry::{NewWindowResponse, WebContext, WebViewBuilder};

#[cfg(windows)]
use wry::WebViewBuilderExtWindows;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = match Config::from_args(args.clone()) {
        Ok(Some(c)) => Ok(c),
        Ok(None) => Err(droidbridge::HELP.to_string()),
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
        tracing::error!("启动失败：{e}");
    }

    if let Err(e) = run_window(target, &data_dir) {
        tracing::error!("窗口创建失败：{e}");
        // WebView2 运行时缺失时退回到默认浏览器（服务仍在运行）
        eprintln!("{e}");
    }
    drop(rt);
}

/// 启动内置服务；端口被另一个 DroidBridge 占用时直接连接它，被其他程序占用时改用随机端口。
async fn launch(cfg: Config, args: Vec<String>) -> Result<String, String> {
    let port = cfg.port;
    match droidbridge::start(cfg).await {
        Ok(s) => {
            tracing::info!("服务已启动：http://127.0.0.1:{}", s.port);
            Ok(format!("http://127.0.0.1:{}/?app=desktop", s.port))
        }
        Err(StartError::AddrInUse(_)) => {
            if droidbridge::probe(&format!("127.0.0.1:{port}")).await {
                tracing::info!("端口 {port} 上已有 DroidBridge 在运行，直接连接");
                return Ok(format!("http://127.0.0.1:{port}/?app=desktop"));
            }
            tracing::warn!("端口 {port} 被其他程序占用，改用随机端口（其他电脑将无法添加本机）");
            let mut cfg = Config::from_args(args).ok().flatten().ok_or("参数错误")?;
            cfg.port = 0;
            cfg.https_port = 0;
            let s = droidbridge::start(cfg).await.map_err(|e| e.to_string())?;
            Ok(format!("http://127.0.0.1:{}/?app=desktop", s.port))
        }
        Err(e) => Err(e.to_string()),
    }
}

fn run_window(target: Result<String, String>, data_dir: &std::path::Path) -> Result<(), String> {
    let event_loop = EventLoop::new();
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
    let builder = builder.with_browser_accelerator_keys(false);
    let builder = match &target {
        Ok(url) => builder.with_url(url.as_str()),
        Err(e) => builder.with_html(error_page(e)),
    };
    let webview = builder.build(&window).map_err(|e| {
        format!("无法创建 WebView2 窗口：{e}。请安装 Microsoft Edge WebView2 运行时（Win10/11 一般已自带）")
    })?;

    event_loop.run(move |event, _, control_flow| {
        let _keep = (&webview, &ctx, &window);
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *control_flow = ControlFlow::Exit;
        }
    });
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
}

fn error_page(msg: &str) -> String {
    let esc = msg.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        r#"<!doctype html><meta charset="utf-8"><body style="margin:0;height:100vh;display:grid;place-items:center;background:#0f1115;color:#e6e8ec;font:14px 'Segoe UI','Microsoft YaHei',sans-serif">
<div style="max-width:520px;padding:24px"><div style="font-size:16px;font-weight:600;margin-bottom:8px">DroidBridge 无法启动</div>
<pre style="white-space:pre-wrap;color:#8b93a3;font:13px/1.6 Consolas,monospace">{esc}</pre></div></body>"#
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
    const N: u32 = 64;
    let mut rgba = vec![0u8; (N * N * 4) as usize];
    // 有符号距离：圆角矩形
    let sd = |x: f32, y: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32| {
        let qx = (x - cx).abs() - (hw - r);
        let qy = (y - cy).abs() - (hh - r);
        let ox = qx.max(0.0);
        let oy = qy.max(0.0);
        (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
    };
    for py in 0..N {
        for px in 0..N {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            let body = sd(x, y, 32.0, 32.0, 19.0, 29.0, 7.0);
            let screen = sd(x, y, 32.0, 29.0, 14.0, 21.0, 3.0);
            let i = ((py * N + px) * 4) as usize;
            let a_body = (0.5 - body).clamp(0.0, 1.0);
            let a_screen = (0.5 - screen).clamp(0.0, 1.0);
            // 机身强调色，屏幕深色
            let (r1, g1, b1) = (79.0, 140.0, 255.0);
            let (r2, g2, b2) = (20.0, 24.0, 32.0);
            let mix = |a: f32, b: f32| a * (1.0 - a_screen) + b * a_screen;
            let mut col = [mix(r1, r2), mix(g1, g2), mix(b1, b2)];
            // 底部小横条
            let bar = sd(x, y, 32.0, 55.0, 4.0, 1.2, 1.2);
            let a_bar = (0.5 - bar).clamp(0.0, 1.0);
            for c in col.iter_mut() {
                *c = *c * (1.0 - a_bar) + 255.0 * a_bar;
            }
            rgba[i] = col[0] as u8;
            rgba[i + 1] = col[1] as u8;
            rgba[i + 2] = col[2] as u8;
            rgba[i + 3] = (a_body * 255.0) as u8;
        }
    }
    Icon::from_rgba(rgba, N, N).ok()
}
