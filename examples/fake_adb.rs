//! 端到端测试用的假 adb（只用标准库，没有新依赖）：被当成 `adb` 可执行文件使用
//! （`Config.adb` / `AppState.cfg.adb`），配合 tests/shared_mirror.rs 里的假 scrcpy 设备，
//! 在没有真机的情况下跑通“多人共享投屏”的完整链路。
//!
//! 按参数的行为：
//!   - 每次调用都把完整参数写一行到 `$FAKE_ADB_LOG`（若设置）——测试用它统计
//!     `push` / `forward` / `forward --remove` / `app_process` 的调用次数与参数；
//!   - `forward tcp:0 …`：打印 `$FAKE_ADB_PORT`（假设备的监听端口）后退出 0；
//!   - `shell … app_process …`：一直不退出（模拟 scrcpy-server 常驻），被 kill 时结束；
//!   - 其他（`start-server`、`devices`、`push`、`forward --remove` …）：退出 0，没有输出。
//!
//! 构建：`cargo build --example fake_adb`。注意：`cargo test`（不加 target 过滤）会顺带构建 examples，
//! 但 `cargo test --test shared_mirror` 不会，所以只跑那一个测试前需要先 `cargo build --example fake_adb`。

use std::io::Write;
use std::time::Duration;

/// 把一行日志追加到 `$FAKE_ADB_LOG`。整行一次写出，避免和其他进程的追加交错。
fn log(args: &[String]) {
    let Ok(path) = std::env::var("FAKE_ADB_LOG") else { return };
    if path.is_empty() {
        return;
    }
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else { return };
    let line = format!("{}\n", args.join(" "));
    let _ = f.write_all(line.as_bytes());
    let _ = f.flush();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    log(&args);

    // `-s SERIAL forward tcp:0 localabstract:scrcpy_xxx`：返回假设备的端口
    if args.windows(2).any(|w| w[0] == "forward" && w[1] == "tcp:0") {
        let port = std::env::var("FAKE_ADB_PORT").unwrap_or_else(|_| "1".to_string());
        println!("{port}");
        let _ = std::io::stdout().flush();
        return;
    }

    // `-s SERIAL shell CLASSPATH=… app_process / com.genymobile.scrcpy.Server …`：
    // 模拟常驻的 scrcpy-server，直到被 kill（Forward 移除、会话停止、测试结束）
    if args.iter().any(|a| a == "app_process") {
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}
