//! 生成应用图标文件：cargo run --example icons -- <输出目录>
//! 输出 icon.png（512）、icon.ico（Windows）、AppIcon.icns（macOS）

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&dir).expect("create output dir");
    std::fs::write(dir.join("icon.png"), droidbridge::icon::png(512)).expect("write png");
    std::fs::write(dir.join("icon.ico"), droidbridge::icon::ico()).expect("write ico");
    std::fs::write(dir.join("AppIcon.icns"), droidbridge::icon::icns()).expect("write icns");
    println!("icons written to {}", dir.display());
}
