//! DroidBridge 服务版：无界面运行，供浏览器访问或被其他电脑添加为远程节点。

use droidbridge::{tr, Config};

#[tokio::main]
async fn main() {
    // 作为 SSH_ASKPASS 被 ssh 调用时，只输出密码后退出
    if droidbridge::remote::askpass_hook() {
        return;
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
        println!("{}", tr!("  已启用访问令牌", "  Access token enabled"));
    }
    println!();

    let _ = tokio::signal::ctrl_c().await;
}
