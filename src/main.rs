//! DroidBridge 服务版：无界面运行，供浏览器访问或被其他电脑添加为远程节点。

use droidbridge::{tr, Config};

fn main() {
    // 插件宿主进程（--plugin-host）：同步运行，不初始化服务、不创建 tokio 运行时
    if droidbridge::plugin_host::hook() {
        return;
    }
    run();
}

#[tokio::main]
async fn run() {
    // 作为 SSH_ASKPASS 被 ssh 调用时，只输出密码后退出
    if droidbridge::remote::askpass_hook() {
        return;
    }
    // 通过 SSH 安装时由安装方调用：授权安装方访问本机，输出 TOKEN=<令牌> 后退出
    if let Some(code) = droidbridge::authorize_cli(std::env::args().skip(1).collect()) {
        std::process::exit(code);
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,hyper=warn".into()),
        )
        .init();

    let cfg = match Config::from_args(std::env::args().skip(1)) {
        Ok(Some(c)) => c,
        Ok(None) => {
            print!("{}", droidbridge::help());
            return;
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let has_token = cfg.token.is_some();
    let open = cfg.open;

    let started = match droidbridge::start(cfg).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    let (port, https) = (started.port, started.https_port);
    println!("\n  DroidBridge {}  (scrcpy-server {})", env!("CARGO_PKG_VERSION"), droidbridge::scrcpy_version());
    println!("{}", tr!("  本机打开:     http://localhost:{port}", "  Local:        http://localhost:{port}"));
    for ip in droidbridge::util::local_ips() {
        println!("{}", tr!("  局域网 HTTP:  http://{ip}:{port}   （供其他电脑添加为远程节点）", "  LAN HTTP:     http://{ip}:{port}   (add this address on other computers)"));
        if https != 0 {
            println!("{}", tr!("  局域网 HTTPS: https://{ip}:{https}  （手机/其他电脑浏览器直接访问用这个）", "  LAN HTTPS:    https://{ip}:{https}  (open this in browsers on other devices)"));
        }
    }
    if has_token {
        println!("{}", tr!("  已启用固定访问令牌", "  Fixed access token enabled"));
    }
    if open {
        println!("{}", tr!("  警告：--open 已开启，局域网内任何人都能控制本机的设备", "  Warning: --open is on; anyone on the LAN can control the devices on this computer"));
    } else {
        println!("{}", tr!("  其他电脑和浏览器首次连接时，需要在已授权的界面里点“允许”", "  Other computers and browsers must be allowed in an authorized window the first time they connect"));
    }
    println!();

    let _ = tokio::signal::ctrl_c().await;
}
