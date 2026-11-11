//! 构建脚本：Windows 下为程序嵌入图标与版本信息。

#[path = "src/icon.rs"]
#[allow(dead_code)]
mod icon;

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=web/index.html");
    println!("cargo:rerun-if-changed=web/app.css");
    for entry in std::fs::read_dir("web/js").into_iter().flatten().flatten() {
        println!("cargo:rerun-if-changed={}", entry.path().display());
    }
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("droidbridge.ico");
        std::fs::write(&out, icon::ico()).expect("write ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon(out.to_str().unwrap());
        res.set("ProductName", "DroidBridge");
        res.set("FileDescription", "DroidBridge");
        if let Err(e) = res.compile() {
            // 缺少资源编译器时不阻断构建，只是没有图标
            println!("cargo:warning=failed to embed Windows icon: {e}");
        }
    }
}
