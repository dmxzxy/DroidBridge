//! 界面与提示文字的语言（中文 / English）。
//! 优先级：命令行 `--lang zh|en` > 环境变量 `DROIDBRIDGE_LANG` > 系统区域设置。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

static EN: AtomicBool = AtomicBool::new(true);
static INIT: Once = Once::new();

/// 选择两种语言之一，用法同 `format!`：`tr!("已安装 {name}", "Installed {name}")`
#[macro_export]
macro_rules! tr {
    ($zh:literal, $en:literal $(,)?) => {
        if $crate::i18n::is_en() { format!($en) } else { format!($zh) }
    };
}

pub fn is_en() -> bool {
    if let Ok(v) = REQUEST_EN.try_with(|v| *v) {
        return v;
    }
    init_default();
    EN.load(Ordering::Relaxed)
}

tokio::task_local! {
    /// 当前 HTTP 请求的语言（来自界面传来的 X-Lang / Accept-Language），优先于进程默认语言
    pub static REQUEST_EN: bool;
}

/// 从请求头取语言；X-Lang 优先，其次 Accept-Language 的第一项
pub fn from_headers(h: &axum::http::HeaderMap) -> Option<bool> {
    let v = h
        .get("x-lang")
        .or_else(|| h.get(axum::http::header::ACCEPT_LANGUAGE))
        .and_then(|v| v.to_str().ok())?
        .trim()
        .to_ascii_lowercase();
    if v.is_empty() {
        return None;
    }
    Some(!v.starts_with("zh"))
}

/// 当前语言代码："zh" 或 "en"
pub fn code() -> &'static str {
    if is_en() { "en" } else { "zh" }
}

/// 设置语言；接受 zh、zh-CN、en、en-US 等，其他值视为英文
pub fn set(lang: &str) {
    INIT.call_once(|| {});
    EN.store(!lang.trim().to_ascii_lowercase().starts_with("zh"), Ordering::Relaxed);
}

/// 从命令行参数（`--lang x`）、环境变量或系统设置确定语言
pub fn init_from_args(args: &[String]) {
    if let Some(i) = args.iter().position(|a| a == "--lang") {
        if let Some(v) = args.get(i + 1) {
            set(v);
            return;
        }
    }
    if let Ok(v) = std::env::var("DROIDBRIDGE_LANG") {
        if !v.is_empty() {
            set(&v);
            return;
        }
    }
    set(&system());
}

fn init_default() {
    INIT.call_once(|| EN.store(!system().to_ascii_lowercase().starts_with("zh"), Ordering::Relaxed));
}

fn system() -> String {
    sys_locale::get_locale().unwrap_or_else(|| "en".into())
}
