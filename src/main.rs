//! DroidBridge 命令行版：作为服务运行，供浏览器访问或被其他电脑添加为远程节点。

use droidbridge::{Config, HELP};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,hyper=warn".into()),
        )
        .init();

    let cfg = match Config::from_args(std::env::args().skip(1)) {
        Ok(Some(c)) => c,
        Ok(None) => {
            print!("{HELP}");
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

    println!("\n  DroidBridge {}  （scrcpy-server {}）", env!("CARGO_PKG_VERSION"), droidbridge::scrcpy_version());
    println!("  本机打开:   http://localhost:{}", started.port);
    for ip in droidbridge::util::local_ips() {
        println!("  局域网 HTTP:  http://{ip}:{}   （供其他电脑添加为远程节点）", started.port);
        if started.https_port != 0 {
            println!("  局域网 HTTPS: https://{ip}:{}  （手机/其他电脑浏览器直接访问用这个）", started.https_port);
        }
    }
    if has_token {
        println!("  已启用访问令牌");
    }
    println!();

    let _ = tokio::signal::ctrl_c().await;
}
