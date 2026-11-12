//! AI Agent 的设备操作（MCP 工具在设备所在节点上的实现）。
//!
//! 坐标约定：与 Agent 交换的坐标都是“截图坐标”——screenshot 返回的图片的像素坐标。
//! 截图按固定规则缩小（长边 ≤ 1568、总像素 ≤ 115 万），read_screen 的坐标同样换算到这个尺寸，
//! tap / swipe 收到的坐标再换算回设备像素。
//!
//! 每次操作会通过 `events` 广播给正在投屏这台设备的窗口（显示“AI 操作中”和点击位置）；
//! 用户可以暂停某台设备，暂停后只允许读取类操作。

use crate::{adb, apps, control, logcat::valid_pkg, perf, util, AppState};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Cursor;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;

const AGENT_JAR: &str = "/data/local/tmp/droidbridge-agent.jar";
const UI_XML: &str = "/data/local/tmp/droidbridge-ui.xml";
const MAX_SIDE: f64 = 1568.0;
const MAX_PIXELS: f64 = 1_150_000.0;

#[derive(Default, Clone)]
struct DevState {
    /// 当前方向下的设备屏幕尺寸（像素）
    screen: Option<(u32, u32)>,
    /// 上次 read_screen 的元素中心点（设备像素）和简述，tap {element} 用
    elements: Vec<(i32, i32, String)>,
    paused: bool,
}

pub struct AgentState {
    /// (serial, 事件 JSON)
    pub events: broadcast::Sender<(String, Value)>,
    devs: Mutex<HashMap<String, DevState>>,
    /// 每台设备一把锁：AI 客户端常常并行发多个工具调用，而 uiautomator 同一时间只能有一个
    /// （并发时设备端报 "UiAutomationService already registered" 并崩溃），输入事件也需要按顺序执行
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl Default for AgentState {
    fn default() -> Self {
        Self { events: broadcast::channel(64).0, devs: Mutex::new(HashMap::new()), locks: Mutex::new(HashMap::new()) }
    }
}

impl AgentState {
    fn lock_for(&self, serial: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks.lock().unwrap().entry(serial.to_string()).or_default().clone()
    }
}

impl AgentState {
    fn get(&self, serial: &str) -> DevState {
        self.devs.lock().unwrap().get(serial).cloned().unwrap_or_default()
    }
    fn update(&self, serial: &str, f: impl FnOnce(&mut DevState)) {
        f(self.devs.lock().unwrap().entry(serial.to_string()).or_default());
    }
    pub fn paused(&self, serial: &str) -> bool {
        self.get(serial).paused
    }
    pub fn set_paused(&self, serial: &str, paused: bool) {
        self.update(serial, |d| d.paused = paused);
        self.emit(serial, json!({"action": if paused { "paused" } else { "resumed" }}));
    }
    fn emit(&self, serial: &str, mut v: Value) {
        v["type"] = json!("agent");
        let _ = self.events.send((serial.to_string(), v));
    }
}

/// 截图坐标与设备像素的比例（≤ 1）
fn scale_for(w: u32, h: u32) -> f64 {
    let (w, h) = (w as f64, h as f64);
    (MAX_SIDE / w.max(h)).min((MAX_PIXELS / (w * h)).sqrt()).min(1.0)
}

fn text(s: impl Into<String>) -> Value {
    json!({"type": "text", "text": s.into()})
}

fn arg_str<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())
}
fn arg_f64(a: &Value, k: &str) -> Option<f64> {
    a.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
}

/// 读取类工具（暂停时仍允许）
fn read_only(tool: &str) -> bool {
    matches!(tool, "screenshot" | "read_screen" | "list_apps" | "logcat" | "perf" | "shell" | "wait")
}

/// 在设备所在节点执行一个工具，返回 MCP content 数组
pub async fn exec(st: &AppState, tool: &str, args: &Value) -> Result<Vec<Value>, String> {
    let serial = arg_str(args, "serial").ok_or_else(|| tr!("缺少设备", "Missing device"))?.to_string();
    let s = serial.as_str();
    if !read_only(tool) && st.agent.paused(s) {
        st.agent.emit(s, json!({"action": "blocked"}));
        return Err(tr!(
            "用户已暂停 AI 对这台设备的操作。请告知用户，等待用户在 DroidBridge 中点“继续”后再操作；读取类工具（screenshot、read_screen 等）仍可使用。",
            "The user has paused AI control of this device. Tell the user and wait until they click Resume in DroidBridge; read-only tools (screenshot, read_screen, …) still work."
        ));
    }
    if tool == "wait" {
        let secs = arg_f64(args, "seconds").unwrap_or(1.0).clamp(0.1, 30.0);
        tokio::time::sleep(Duration::from_secs_f64(secs)).await;
        return Ok(vec![text(tr!("已等待 {secs} 秒", "Waited {secs} s"))]);
    }
    let lock = st.agent.lock_for(s);
    let _guard = match tokio::time::timeout(Duration::from_secs(90), lock.lock()).await {
        Ok(g) => g,
        Err(_) => return Err(tr!("这台设备上还有别的操作在执行，请稍后重试", "Another operation is still running on this device; try again shortly")),
    };
    match tokio::time::timeout(Duration::from_secs(100), run_tool(st, s, &serial, tool, args)).await {
        Ok(r) => r,
        Err(_) => Err(tr!("操作超时（设备没有响应），请检查设备连接后重试", "The operation timed out (the device did not respond); check the connection and retry")),
    }
}

async fn run_tool(st: &AppState, s: &str, serial: &str, tool: &str, args: &Value) -> Result<Vec<Value>, String> {
    let serial = serial.to_string();
    match tool {
        "screenshot" => screenshot(st, s).await,
        "read_screen" => read_screen(st, s).await,
        "tap" => tap(st, s, args).await,
        "swipe" => swipe(st, s, args).await,
        "type_text" => type_text(st, s, args).await,
        "press_key" => press_key(st, s, args).await,
        "launch_app" | "stop_app" => {
            let pkg = arg_str(args, "package").ok_or_else(|| tr!("缺少 package", "Missing package"))?.to_string();
            let action = if tool == "launch_app" { "launch" } else { "stop" };
            let label = if tool == "launch_app" { tr!("启动 {pkg}", "Launch {pkg}") } else { tr!("停止 {pkg}", "Stop {pkg}") };
            st.agent.emit(s, json!({"action": "app", "label": label}));
            let r = apps::action(st, apps::ActionReq { serial: serial.clone(), pkg, action: action.into() }).await?;
            Ok(vec![text(r["msg"].as_str().unwrap_or("OK"))])
        }
        "list_apps" => list_apps(st, s, args).await,
        "logcat" => logcat(st, s, args).await,
        "perf" => {
            st.agent.emit(s, json!({"action": "look", "label": tr!("读取性能数据", "Reading performance")}));
            Ok(vec![text(format_perf(&perf::snapshot(st, s).await?))])
        }
        "shell" => shell(st, s, args).await,
        other => Err(tr!("未知工具：{other}", "Unknown tool: {other}")),
    }
}

// ---------------------------------------------------------------- 读取

async fn screencap(st: &AppState, s: &str) -> Result<image::DynamicImage, String> {
    let png = adb::run_bytes(&st.cfg.adb, &["-s", s, "exec-out", "screencap", "-p"], Duration::from_secs(20)).await?;
    image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .map_err(|e| tr!("截图失败（设备可能锁屏或处于安全界面）：{e}", "Screenshot failed (the device may be locked or showing a secure screen): {e}"))
}

async fn screenshot(st: &AppState, s: &str) -> Result<Vec<Value>, String> {
    st.agent.emit(s, json!({"action": "look", "label": tr!("查看屏幕", "Looking at the screen")}));
    let img = screencap(st, s).await?;
    let (w, h) = (img.width(), img.height());
    st.agent.update(s, |d| d.screen = Some((w, h)));
    let k = scale_for(w, h);
    let (sw, sh) = (((w as f64) * k).round() as u32, ((h as f64) * k).round() as u32);
    let small = if k < 1.0 { img.resize_exact(sw, sh, image::imageops::FilterType::Triangle) } else { img };
    let mut jpg = Vec::new();
    small
        .to_rgb8()
        .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut Cursor::new(&mut jpg), 80))
        .map_err(|e| e.to_string())?;
    Ok(vec![
        text(tr!(
            "截图 {sw}×{sh}（设备 {w}×{h}）。tap / swipe 的坐标请用这张截图的像素坐标。",
            "Screenshot {sw}×{sh} (device {w}×{h}). Use pixel coordinates of this screenshot for tap / swipe."
        )),
        json!({"type": "image", "data": util::base64(&jpg), "mimeType": "image/jpeg"}),
    ])
}

/// 解析 uiautomator dump 的 XML：返回 (属性表) 列表
fn parse_nodes(xml: &str) -> Vec<HashMap<String, String>> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<node ") {
        rest = &rest[i + 6..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..end];
        let mut attrs = HashMap::new();
        let mut t = tag;
        while let Some(eq) = t.find("=\"") {
            let key = t[..eq].trim().to_string();
            let after = &t[eq + 2..];
            let Some(close) = after.find('"') else { break };
            attrs.insert(key, unescape(&after[..close]));
            t = &after[close + 1..];
        }
        out.push(attrs);
        rest = &rest[end..];
    }
    out
}

/// 解码 XML 实体：命名实体与数字实体（&#128512; / &#x1F600;，emoji 等）；换行显示为空格
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|e| *e <= 12) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let ch = match ent {
            "quot" => Some('"'),
            "apos" => Some('\''),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some('\n') => out.push(' '),
            Some('\r') => {}
            Some(c) => out.push(c),
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// "[0,0][1080,2400]" → (x1, y1, x2, y2)
fn bounds(s: &str) -> Option<(i32, i32, i32, i32)> {
    let v: Vec<i32> = s.split(|c: char| !c.is_ascii_digit() && c != '-').filter(|x| !x.is_empty()).filter_map(|x| x.parse().ok()).collect();
    (v.len() == 4).then(|| (v[0], v[1], v[2], v[3]))
}

/// 从 uiautomator 的节点中挑出有意义的元素，生成紧凑的文字描述
fn describe(nodes: &[HashMap<String, String>], k: f64) -> (Vec<String>, Vec<(i32, i32, String)>) {
    let mut lines = Vec::new();
    let mut elems = Vec::new();
    for n in nodes {
        let g = |key: &str| n.get(key).map(String::as_str).unwrap_or("");
        let Some((x1, y1, x2, y2)) = bounds(g("bounds")) else { continue };
        if x2 <= x1 || y2 <= y1 {
            continue;
        }
        let class = g("class").rsplit('.').next().unwrap_or("");
        let (txt, desc) = (g("text").trim(), g("content-desc").trim());
        let editable = class.contains("EditText");
        let click = g("clickable") == "true" || g("long-clickable") == "true";
        let scroll = g("scrollable") == "true";
        if txt.is_empty() && desc.is_empty() && !click && !scroll && !editable {
            continue;
        }
        let (cx, cy) = ((x1 + x2) / 2, (y1 + y2) / 2);
        let i = elems.len();
        let mut line = format!("[{i}] {class}");
        let short = |s: &str| -> String { if s.chars().count() > 80 { s.chars().take(80).collect::<String>() + "…" } else { s.to_string() } };
        if !txt.is_empty() {
            line += &format!(" \"{}\"", short(txt));
        }
        if !desc.is_empty() && desc != txt {
            line += &format!(" desc=\"{}\"", short(desc));
        }
        if let Some(id) = g("resource-id").split(":id/").nth(1).filter(|s| !s.is_empty()) {
            line += &format!(" id={id}");
        }
        line += &format!(" @({},{})", ((cx as f64) * k).round(), ((cy as f64) * k).round());
        for (on, flag) in [(click, "click"), (scroll, "scroll"), (editable, "edit"), (g("checked") == "true", "checked"), (g("selected") == "true", "selected"), (g("focused") == "true", "focused"), (g("enabled") == "false", "disabled")] {
            if on {
                line += " ";
                line += flag;
            }
        }
        let label = if !txt.is_empty() { txt } else if !desc.is_empty() { desc } else { class };
        elems.push((cx, cy, short(label)));
        lines.push(line);
        if lines.len() >= 400 {
            lines.push(tr!("…（元素过多，已截断）", "… (truncated)"));
            break;
        }
    }
    (lines, elems)
}

async fn foreground(st: &AppState, s: &str) -> String {
    let out = adb::run(&st.cfg.adb, &["-s", s, "shell", "dumpsys activity activities | grep -m1 -E 'topResumedActivity|mResumedActivity'"]).await.unwrap_or_default();
    out.split_whitespace().find(|t| t.contains('/') && t.contains('.')).map(|t| t.trim_matches(|c| c == '{' || c == '}').to_string()).unwrap_or_default()
}

async fn read_screen(st: &AppState, s: &str) -> Result<Vec<Value>, String> {
    st.agent.emit(s, json!({"action": "look", "label": tr!("读取界面", "Reading the screen")}));
    let file = format!("{UI_XML}.{:08x}", util::random_scid());
    let cmd = format!("uiautomator dump --compressed {file} >/dev/null 2>&1; cat {file} 2>/dev/null; rm -f {file}");
    let mut nodes = Vec::new();
    for attempt in 0..2 {
        let xml = adb::run_timeout(&st.cfg.adb, &["-s", s, "shell", &cmd], Duration::from_secs(25)).await.unwrap_or_default();
        nodes = parse_nodes(&xml);
        if !nodes.is_empty() || attempt == 1 {
            break;
        }
        // 上一个 uiautomator 刚退出时偶尔会失败，稍等再试一次
        tokio::time::sleep(Duration::from_millis(600)).await;
    }
    if nodes.is_empty() {
        return Err(tr!(
            "无法读取界面结构（画面一直在变化、游戏或安全界面时常见）。请改用 screenshot。",
            "Cannot read the UI structure (common with animations, games or secure screens). Use screenshot instead."
        ));
    }
    // 根节点的范围就是当前方向下的屏幕尺寸
    let (w, h) = nodes.iter().filter_map(|n| n.get("bounds").and_then(|b| bounds(b))).map(|(_, _, x2, y2)| (x2.max(1) as u32, y2.max(1) as u32)).fold((1, 1), |a, b| (a.0.max(b.0), a.1.max(b.1)));
    let k = scale_for(w, h);
    let (lines, elems) = describe(&nodes, k);
    st.agent.update(s, |d| {
        d.screen = Some((w, h));
        d.elements = elems;
    });
    let fg = foreground(st, s).await;
    let (sw, sh) = (((w as f64) * k).round(), ((h as f64) * k).round());
    let head = tr!(
        "前台：{fg}\n屏幕：{sw}×{sh}（截图坐标）\n元素（[编号] 类型 \"文字\" desc=说明 id=资源ID @(中心坐标) 状态）：",
        "Foreground: {fg}\nScreen: {sw}×{sh} (screenshot coordinates)\nElements ([index] class \"text\" desc=description id=resource-id @(center) flags):"
    );
    Ok(vec![text(format!("{head}\n{}", lines.join("\n")))])
}

// ---------------------------------------------------------------- 操作

/// 当前屏幕尺寸；还不知道时截一次图
async fn screen(st: &AppState, s: &str) -> Result<(u32, u32), String> {
    if let Some(v) = st.agent.get(s).screen {
        return Ok(v);
    }
    let img = screencap(st, s).await?;
    let v = (img.width(), img.height());
    st.agent.update(s, |d| d.screen = Some(v));
    Ok(v)
}

/// 截图坐标 → 设备像素
fn to_device(x: f64, y: f64, (w, h): (u32, u32)) -> (i32, i32) {
    let k = scale_for(w, h);
    (((x / k).round() as i32).clamp(0, w as i32 - 1), ((y / k).round() as i32).clamp(0, h as i32 - 1))
}

async fn input(st: &AppState, s: &str, cmd: &str) -> Result<(), String> {
    adb::run_timeout(&st.cfg.adb, &["-s", s, "shell", cmd], Duration::from_secs(20)).await.map(|_| ())
}

async fn tap(st: &AppState, s: &str, a: &Value) -> Result<Vec<Value>, String> {
    let scr = screen(st, s).await?;
    let (x, y, what) = if let Some(i) = arg_f64(a, "element") {
        let el = st.agent.get(s).elements;
        let (x, y, label) = el.get(i as usize).cloned().ok_or_else(|| tr!("元素编号 {i} 不存在，请先调用 read_screen", "Element {i} not found; call read_screen first"))?;
        (x, y, format!("“{label}”"))
    } else {
        let (x, y) = (arg_f64(a, "x"), arg_f64(a, "y"));
        let (Some(x), Some(y)) = (x, y) else { return Err(tr!("需要 element，或 x 和 y", "Provide element, or x and y")) };
        let (dx, dy) = to_device(x, y, scr);
        (dx, dy, format!("({x}, {y})"))
    };
    let long = a.get("long").and_then(|v| v.as_bool()).unwrap_or(false);
    let label = if long { tr!("长按 {what}", "Long press {what}") } else { tr!("点击 {what}", "Tap {what}") };
    st.agent.emit(s, json!({"action": "tap", "x": x as f64 / scr.0 as f64, "y": y as f64 / scr.1 as f64, "label": label}));
    if long {
        input(st, s, &format!("input swipe {x} {y} {x} {y} 800")).await?;
    } else {
        input(st, s, &format!("input tap {x} {y}")).await?;
    }
    Ok(vec![text(tr!("已{label}", "Done: {label}"))])
}

async fn swipe(st: &AppState, s: &str, a: &Value) -> Result<Vec<Value>, String> {
    let scr = screen(st, s).await?;
    let (w, h) = (scr.0 as f64, scr.1 as f64);
    let (x1, y1, x2, y2) = if let Some(dir) = arg_str(a, "direction") {
        let d = arg_f64(a, "distance").unwrap_or(0.5).clamp(0.1, 0.9);
        let (cx, cy) = (w / 2.0, h / 2.0);
        match dir {
            "up" => (cx, cy + h * d / 2.0, cx, cy - h * d / 2.0),
            "down" => (cx, cy - h * d / 2.0, cx, cy + h * d / 2.0),
            "left" => (cx + w * d / 2.0, cy, cx - w * d / 2.0, cy),
            "right" => (cx - w * d / 2.0, cy, cx + w * d / 2.0, cy),
            _ => return Err(tr!("direction 应为 up / down / left / right", "direction must be up / down / left / right")),
        }
    } else {
        let v: Vec<f64> = ["x1", "y1", "x2", "y2"].iter().filter_map(|k| arg_f64(a, k)).collect();
        if v.len() != 4 {
            return Err(tr!("需要 direction，或 x1、y1、x2、y2", "Provide direction, or x1, y1, x2, y2"));
        }
        let (a1, b1) = to_device(v[0], v[1], scr);
        let (a2, b2) = to_device(v[2], v[3], scr);
        (a1 as f64, b1 as f64, a2 as f64, b2 as f64)
    };
    let ms = arg_f64(a, "duration_ms").unwrap_or(300.0).clamp(50.0, 5000.0) as u32;
    let label = tr!("滑动", "Swipe");
    st.agent.emit(s, json!({"action": "swipe", "x": x1 / w, "y": y1 / h, "x2": x2 / w, "y2": y2 / h, "label": label}));
    input(st, s, &format!("input swipe {} {} {} {} {ms}", x1 as i32, y1 as i32, x2 as i32, y2 as i32)).await?;
    Ok(vec![text(tr!("已滑动", "Swiped"))])
}

const KEYS: &[(&str, u32)] = &[
    ("back", 4), ("home", 3), ("recents", 187), ("enter", 66), ("delete", 67), ("backspace", 67), ("forward_delete", 112), ("tab", 61), ("escape", 111),
    ("menu", 82), ("search", 84), ("power", 26), ("wakeup", 224), ("sleep", 223), ("volume_up", 24), ("volume_down", 25), ("volume_mute", 164),
    ("dpad_up", 19), ("dpad_down", 20), ("dpad_left", 21), ("dpad_right", 22), ("dpad_center", 23), ("space", 62), ("page_up", 92), ("page_down", 93),
    ("move_home", 122), ("move_end", 123), ("camera", 27), ("media_play_pause", 85), ("select_all", 0),
];

async fn press_key(st: &AppState, s: &str, a: &Value) -> Result<Vec<Value>, String> {
    let key = arg_str(a, "key").ok_or_else(|| tr!("缺少 key", "Missing key"))?.to_ascii_lowercase();
    let label = tr!("按键 {key}", "Key {key}");
    st.agent.emit(s, json!({"action": "key", "label": label}));
    if key == "select_all" {
        input(st, s, "input keycombination 113 29 2>/dev/null || input keyevent --longpress 29").await?;
    } else {
        let code = KEYS.iter().find(|(n, _)| *n == key).map(|(_, c)| *c).or_else(|| key.parse().ok()).ok_or_else(|| {
            let names: Vec<&str> = KEYS.iter().map(|(n, _)| *n).collect();
            let names = names.join(", ");
            tr!("未知按键 {key}，可用：{names}，或直接写 Android keycode 数字", "Unknown key {key}; use one of {names}, or an Android keycode number")
        })?;
        input(st, s, &format!("input keyevent {code}")).await?;
    }
    Ok(vec![text(tr!("已按 {key}", "Pressed {key}"))])
}

async fn type_text(st: &AppState, s: &str, a: &Value) -> Result<Vec<Value>, String> {
    let t = a.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if t.is_empty() {
        return Err(tr!("缺少 text", "Missing text"));
    }
    let preview: String = t.chars().take(30).collect();
    let label = tr!("输入 “{preview}”", "Type “{preview}”");
    st.agent.emit(s, json!({"action": "type", "label": label}));
    paste_text(st, s, t).await?;
    if a.get("submit").and_then(|v| v.as_bool()).unwrap_or(false) {
        input(st, s, "input keyevent 66").await?;
    }
    Ok(vec![text(tr!("已输入（粘贴到当前输入框）", "Typed (pasted into the focused field)"))])
}

/// 通过一个只有控制通道的 scrcpy-server 会话，设置剪贴板并粘贴（支持中文等任意文字）
async fn paste_text(st: &AppState, s: &str, t: &str) -> Result<(), String> {
    let adb_path = &st.cfg.adb;
    let jar = st.cfg.server_jar.to_string_lossy().into_owned();
    adb::run_timeout(adb_path, &["-s", s, "push", &jar, AGENT_JAR], Duration::from_secs(30)).await?;
    let scid = util::random_scid();
    let local = format!("localabstract:scrcpy_{scid:08x}");
    let port: u16 = adb::run(adb_path, &["-s", s, "forward", "tcp:0", &local])
        .await?
        .trim()
        .parse()
        .map_err(|_| tr!("adb forward 未返回端口", "adb forward returned no port"))?;
    let result = async {
        let mut child = adb::command(adb_path)
            .args(["-s", s, "shell", &format!("CLASSPATH={AGENT_JAR}"), "app_process", "/", "com.genymobile.scrcpy.Server", crate::scrcpy_version()])
            .args([
                format!("scid={scid:08x}"),
                "log_level=warn".into(),
                "tunnel_forward=true".into(),
                "video=false".into(),
                "audio=false".into(),
                "control=true".into(),
                "send_device_meta=false".into(),
                "send_dummy_byte=true".into(),
                "cleanup=false".into(),
                "clipboard_autosync=false".into(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut ctl = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
            let conn = tokio::time::timeout(Duration::from_secs(1), TcpStream::connect(("127.0.0.1", port))).await;
            if let Ok(Ok(mut c)) = conn {
                let mut b = [0u8; 1];
                if let Ok(Ok(_)) = tokio::time::timeout(Duration::from_secs(2), c.read_exact(&mut b)).await {
                    ctl = Some(c);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let mut ctl = ctl.ok_or_else(|| tr!("无法启动输入通道（scrcpy-server）", "Cannot start the input channel (scrcpy-server)"))?;
        for m in control::encode(&control::Ctl::Clipboard { text: t.to_string(), paste: true, seq: 1 })? {
            ctl.write_all(&m).await.map_err(|e| e.to_string())?;
        }
        // 等设备确认（ACK_CLIPBOARD），确认后粘贴已完成
        let ack = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match ctl.read_u8().await {
                    Ok(1) => return ctl.read_u64().await.is_ok(),
                    Ok(0) => {
                        let Ok(n) = ctl.read_u32().await else { return false };
                        let mut buf = vec![0u8; n as usize];
                        if ctl.read_exact(&mut buf).await.is_err() {
                            return false;
                        }
                    }
                    _ => return false,
                }
            }
        })
        .await;
        let _ = child.kill().await;
        match ack {
            Ok(true) => Ok(()),
            _ => Err(tr!("设备没有确认输入", "The device did not confirm the input")),
        }
    }
    .await;
    let _ = adb::run(adb_path, &["-s", s, "forward", "--remove", &format!("tcp:{port}")]).await;
    result
}

// ---------------------------------------------------------------- 其他

async fn list_apps(st: &AppState, s: &str, a: &Value) -> Result<Vec<Value>, String> {
    let r = apps::list(st, apps::ListReq { serial: s.to_string() }).await?;
    let q = arg_str(a, "query").map(str::to_lowercase);
    let sys = a.get("include_system").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut lines: Vec<String> = r["apps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|x| sys || !x["system"].as_bool().unwrap_or(false))
        .filter(|x| {
            q.as_ref().is_none_or(|q| {
                x["pkg"].as_str().unwrap_or("").to_lowercase().contains(q) || x["name"].as_str().unwrap_or("").to_lowercase().contains(q)
            })
        })
        .map(|x| {
            let (n, p) = (x["name"].as_str().unwrap_or(""), x["pkg"].as_str().unwrap_or(""));
            let run = if x["running"].as_bool().unwrap_or(false) { tr!("（运行中）", " (running)") } else { String::new() };
            if n.is_empty() { format!("{p}{run}") } else { format!("{n} — {p}{run}") }
        })
        .collect();
    lines.sort();
    let n = lines.len();
    Ok(vec![text(format!("{}\n{}", tr!("{n} 个应用：", "{n} apps:"), lines.join("\n")))])
}

async fn logcat(st: &AppState, s: &str, a: &Value) -> Result<Vec<Value>, String> {
    let lines = arg_f64(a, "lines").unwrap_or(200.0).clamp(10.0, 2000.0) as u32;
    let level = arg_str(a, "level").map(|l| l.chars().next().unwrap_or('V').to_ascii_uppercase()).filter(|c| "VDIWEF".contains(*c)).unwrap_or('V');
    let mut cmd = format!("logcat -d -v threadtime -t {} *:{level}", if arg_str(a, "grep").is_some() { lines * 5 } else { lines });
    if let Some(pkg) = arg_str(a, "package") {
        if !valid_pkg(pkg) || pkg.contains(':') {
            return Err(tr!("包名格式不正确", "Invalid package name"));
        }
        let pid = adb::run(&st.cfg.adb, &["-s", s, "shell", &format!("pidof {pkg}")]).await.unwrap_or_default();
        let Some(pid) = pid.split_whitespace().next() else { return Ok(vec![text(tr!("{pkg} 没有在运行", "{pkg} is not running"))]) };
        cmd += &format!(" --pid={pid}");
    }
    let out = adb::run_timeout(&st.cfg.adb, &["-s", s, "shell", &cmd], Duration::from_secs(20)).await?;
    let grep = arg_str(a, "grep").map(str::to_lowercase);
    let mut v: Vec<&str> = out.lines().filter(|l| grep.as_ref().is_none_or(|g| l.to_lowercase().contains(g))).collect();
    if v.len() > lines as usize {
        v.drain(..v.len() - lines as usize);
    }
    let mut body = v.join("\n");
    if body.len() > 60_000 {
        let cut = body.len() - 60_000;
        let cut = (cut..body.len()).find(|i| body.is_char_boundary(*i)).unwrap_or(cut);
        body = body[cut..].to_string();
    }
    Ok(vec![text(if body.is_empty() { tr!("（没有匹配的日志）", "(no matching log lines)") } else { body })])
}

fn format_perf(m: &Value) -> String {
    let f = |k: &str| m[k].as_f64().map(|v| format!("{v:.1}")).unwrap_or_else(|| "-".into());
    let i = |k: &str| m[k].as_u64().map(|v| v.to_string()).unwrap_or_else(|| "-".into());
    tr!(
        "前台应用：{}\n帧率：{} fps\nCPU：应用 {}%，整机 {}%\n内存：应用 {} MB，整机已用 {} / {} MB\n温度：CPU {}°C，电池 {}°C\n电量：{}%{}",
        "Foreground app: {}\nFPS: {}\nCPU: app {}%, device {}%\nMemory: app {} MB, device {} / {} MB used\nTemperature: CPU {}°C, battery {}°C\nBattery: {}%{}",
        m["pkg"].as_str().unwrap_or("-"), f("fps"), f("app_cpu"), f("cpu"), i("app_mem"), i("mem_used"), i("mem_total"), f("cpu_temp"), f("batt_temp"), i("level"),
        if m["charging"].as_bool().unwrap_or(false) { tr!("（充电中）", " (charging)") } else { String::new() }
    )
}

/// 只读 shell：只允许查询类命令，不允许管道以外的组合与重定向
fn shell_allowed(cmd: &str) -> bool {
    if cmd.contains([';', '&', '>', '<', '`', '\n', '\r']) || cmd.contains("$(") {
        return false;
    }
    cmd.split('|').all(|part| {
        let w: Vec<&str> = part.split_whitespace().collect();
        match w.as_slice() {
            [] => false,
            ["dumpsys", ..] | ["getprop", ..] | ["ls", ..] | ["cat", ..] | ["ps", ..] | ["top", ..] | ["df", ..] | ["uptime"] | ["date", ..] | ["id"] | ["whoami"]
            | ["ip", ..] | ["ifconfig", ..] | ["pidof", ..] | ["grep", ..] | ["head", ..] | ["tail", ..] | ["wc", ..] | ["sort", ..] | ["uniq", ..] | ["cut", ..] | ["awk", ..] | ["sed", "-n", ..]
            | ["logcat", "-d", ..] | ["uname", ..] | ["free", ..] | ["stat", ..] | ["find", ..] | ["du", ..] | ["nproc"] => {
                !part.contains(" -delete") && !part.contains(" -exec")
            }
            ["pm", sub, ..] => matches!(*sub, "list" | "path" | "dump"),
            ["cmd", "package", sub, ..] => matches!(*sub, "list" | "resolve-activity" | "query-activities"),
            ["wm", sub] => matches!(*sub, "size" | "density"),
            ["settings", "get", ..] | ["settings", "list", ..] => true,
            ["am", "stack", "list"] | ["am", "get-current-user"] => true,
            _ => false,
        }
    })
}

async fn shell(st: &AppState, s: &str, a: &Value) -> Result<Vec<Value>, String> {
    let cmd = arg_str(a, "command").ok_or_else(|| tr!("缺少 command", "Missing command"))?;
    if !shell_allowed(cmd) {
        return Err(tr!(
            "只允许只读的查询命令（dumpsys、getprop、pm list、settings get、cat、ls、ps、top 等，可用 | 接 grep/head/tail）。修改设备请用其他工具。",
            "Only read-only commands are allowed (dumpsys, getprop, pm list, settings get, cat, ls, ps, top, …, optionally piped to grep/head/tail). Use the other tools to change the device."
        ));
    }
    let out = adb::run_timeout(&st.cfg.adb, &["-s", s, "shell", cmd], Duration::from_secs(30)).await.unwrap_or_else(|e| e);
    let mut out = out.trim_end().to_string();
    if out.len() > 60_000 {
        let end = (0..=60_000).rev().find(|i| out.is_char_boundary(*i)).unwrap_or(0);
        out.truncate(end);
        out += "\n…";
    }
    Ok(vec![text(if out.is_empty() { tr!("（无输出）", "(no output)") } else { out })])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_and_coords() {
        assert_eq!(scale_for(720, 1600), 0.98); // 1568 / 1600
        let k = scale_for(2800, 2000);
        assert!((k - 0.4532).abs() < 0.001, "{k}");
        // 截图坐标 → 设备像素：往返一致
        let (x, y) = to_device(634.0, 453.0, (2800, 2000));
        assert!((x - 1399).abs() <= 1 && (y - 1000).abs() <= 1, "{x},{y}");
        assert_eq!(to_device(99999.0, -5.0, (1080, 2400)), (1079, 0));
    }

    #[test]
    fn ui_xml() {
        let xml = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0"><node index="0" text="" resource-id="" class="android.widget.FrameLayout" package="com.x" content-desc="" clickable="false" enabled="true" scrollable="false" bounds="[0,0][1080,2400]"><node index="0" text="登录 &amp; 注册" resource-id="com.x:id/login" class="android.widget.Button" package="com.x" content-desc="" clickable="true" enabled="true" bounds="[100,1500][980,1640]" /><node index="1" text="" resource-id="com.x:id/user" class="android.widget.EditText" content-desc="用户名" clickable="true" focused="true" bounds="[100,1200][980,1300]" /><node index="2" text="" class="android.view.View" clickable="false" bounds="[0,0][10,10]" /></node></hierarchy>"#;
        let nodes = parse_nodes(xml);
        assert_eq!(nodes.len(), 4);
        let (lines, elems) = describe(&nodes, scale_for(1080, 2400));
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("[0] Button \"登录 & 注册\" id=login @("), "{}", lines[0]);
        assert!(lines[0].ends_with("click"));
        assert!(lines[1].contains("desc=\"用户名\"") && lines[1].contains("edit") && lines[1].contains("focused"), "{}", lines[1]);
        assert_eq!(elems[0], (540, 1570, "登录 & 注册".to_string()));
    }

    #[tokio::test]
    async fn per_device_lock_serializes() {
        let st = AgentState::default();
        let (a, b) = (st.lock_for("dev1"), st.lock_for("dev1"));
        assert!(Arc::ptr_eq(&a, &b), "same device → same lock");
        assert!(!Arc::ptr_eq(&a, &st.lock_for("dev2")), "different devices run independently");
        let g = a.lock().await;
        assert!(b.try_lock().is_err(), "second call on the same device waits");
        drop(g);
        assert!(b.try_lock().is_ok());
    }

    #[test]
    fn entities() {
        assert_eq!(unescape("hi 你好&#128512;"), "hi 你好😀");
        assert_eq!(unescape("&#x1F600;&lt;a&gt; &amp;&quot;x&apos;"), "😀<a> &\"x'");
        assert_eq!(unescape("line&#10;two&#13;"), "line two");
        assert_eq!(unescape("AT&T & co"), "AT&T & co");
    }

    #[test]
    fn shell_rules() {
        assert!(shell_allowed("dumpsys battery"));
        assert!(shell_allowed("getprop ro.build.version.release"));
        assert!(shell_allowed("dumpsys package com.x | grep version"));
        assert!(shell_allowed("pm list packages -3"));
        assert!(!shell_allowed("pm uninstall com.x"));
        assert!(!shell_allowed("rm -rf /sdcard"));
        assert!(!shell_allowed("cat /a > /b"));
        assert!(!shell_allowed("ls; reboot"));
        assert!(!shell_allowed("find /sdcard -delete"));
        assert!(!shell_allowed("settings put global x 1"));
        assert!(!shell_allowed("echo $(reboot)"));
    }
}
