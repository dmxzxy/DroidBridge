//! 浏览器 JSON 控制指令 -> scrcpy 二进制控制协议（对应 scrcpy v5 ControlMessageReader）。

use serde::Deserialize;

const TYPE_INJECT_KEYCODE: u8 = 0;
const TYPE_INJECT_TEXT: u8 = 1;
const TYPE_INJECT_TOUCH_EVENT: u8 = 2;
const TYPE_INJECT_SCROLL_EVENT: u8 = 3;
const TYPE_BACK_OR_SCREEN_ON: u8 = 4;
const TYPE_EXPAND_NOTIFICATION_PANEL: u8 = 5;
const TYPE_EXPAND_SETTINGS_PANEL: u8 = 6;
const TYPE_COLLAPSE_PANELS: u8 = 7;
const TYPE_SET_CLIPBOARD: u8 = 9;
const TYPE_SET_DISPLAY_POWER: u8 = 10;
const TYPE_ROTATE_DEVICE: u8 = 11;
const TYPE_OPEN_HARD_KEYBOARD_SETTINGS: u8 = 15;
const TYPE_START_APP: u8 = 16;
const TYPE_RESET_VIDEO: u8 = 17;

const INJECT_TEXT_MAX_BYTES: usize = 300;

fn default_pressure() -> f32 {
    1.0
}

#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Ctl {
    /// action: 0=down 1=up
    Key {
        action: u8,
        code: u32,
        #[serde(default)]
        repeat: u32,
        #[serde(default)]
        meta: u32,
    },
    Text {
        text: String,
    },
    /// action: 0=down 1=up 2=move 3=cancel；坐标基于当前视频尺寸 w×h
    Touch {
        action: u8,
        id: i64,
        x: i32,
        y: i32,
        w: u16,
        h: u16,
        #[serde(default = "default_pressure")]
        p: f32,
        #[serde(default)]
        buttons: u32,
    },
    /// hs/vs 范围 [-16, 16]，正值向左/向上
    Scroll {
        x: i32,
        y: i32,
        w: u16,
        h: u16,
        hs: f32,
        vs: f32,
    },
    /// 返回键（熄屏时点亮屏幕）
    Back {
        action: u8,
    },
    Clipboard {
        text: String,
        #[serde(default)]
        paste: bool,
        #[serde(default)]
        seq: u64,
    },
    /// 关闭/打开屏幕显示（投屏继续）
    Power {
        on: bool,
    },
    StartApp {
        name: String,
    },
    /// notif / settings / collapse / rotate / reset_video / keyboard_settings
    Cmd {
        name: String,
    },
}

fn u16_fixed(p: f32) -> u16 {
    let p = p.clamp(0.0, 1.0);
    if p >= 1.0 { 0xffff } else { (p * 65536.0) as u16 }
}

fn i16_fixed(v: f32) -> i16 {
    let v = v.clamp(-1.0, 1.0);
    if v >= 1.0 { 0x7fff } else { (v * 32768.0) as i16 }
}

fn put_position(buf: &mut Vec<u8>, x: i32, y: i32, w: u16, h: u16) {
    buf.extend_from_slice(&x.to_be_bytes());
    buf.extend_from_slice(&y.to_be_bytes());
    buf.extend_from_slice(&w.to_be_bytes());
    buf.extend_from_slice(&h.to_be_bytes());
}

/// 截断到不超过 max 字节的 UTF-8 边界并分片
fn split_utf8(s: &str, max: usize) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut rest = s;
    while !rest.is_empty() {
        let mut end = rest.len().min(max);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        parts.push(&rest[..end]);
        rest = &rest[end..];
    }
    parts
}

/// 编码为一个或多个控制消息（文本可能被拆成多条）。
pub fn encode(ctl: &Ctl) -> Result<Vec<Vec<u8>>, String> {
    let mut msgs = Vec::new();
    match ctl {
        Ctl::Key { action, code, repeat, meta } => {
            let mut b = Vec::with_capacity(14);
            b.push(TYPE_INJECT_KEYCODE);
            b.push(*action);
            b.extend_from_slice(&code.to_be_bytes());
            b.extend_from_slice(&repeat.to_be_bytes());
            b.extend_from_slice(&meta.to_be_bytes());
            msgs.push(b);
        }
        Ctl::Text { text } => {
            for part in split_utf8(text, INJECT_TEXT_MAX_BYTES) {
                let mut b = Vec::with_capacity(5 + part.len());
                b.push(TYPE_INJECT_TEXT);
                b.extend_from_slice(&(part.len() as u32).to_be_bytes());
                b.extend_from_slice(part.as_bytes());
                msgs.push(b);
            }
        }
        Ctl::Touch { action, id, x, y, w, h, p, buttons } => {
            let mut b = Vec::with_capacity(32);
            b.push(TYPE_INJECT_TOUCH_EVENT);
            b.push(*action);
            b.extend_from_slice(&id.to_be_bytes());
            put_position(&mut b, *x, *y, *w, *h);
            let pressure = if *action == 1 { 0 } else { u16_fixed(*p) };
            b.extend_from_slice(&pressure.to_be_bytes());
            b.extend_from_slice(&0u32.to_be_bytes()); // action_button
            b.extend_from_slice(&buttons.to_be_bytes());
            msgs.push(b);
        }
        Ctl::Scroll { x, y, w, h, hs, vs } => {
            let mut b = Vec::with_capacity(21);
            b.push(TYPE_INJECT_SCROLL_EVENT);
            put_position(&mut b, *x, *y, *w, *h);
            b.extend_from_slice(&i16_fixed(hs / 16.0).to_be_bytes());
            b.extend_from_slice(&i16_fixed(vs / 16.0).to_be_bytes());
            b.extend_from_slice(&0u32.to_be_bytes());
            msgs.push(b);
        }
        Ctl::Back { action } => msgs.push(vec![TYPE_BACK_OR_SCREEN_ON, *action]),
        Ctl::Clipboard { text, paste, seq } => {
            let bytes = text.as_bytes();
            if bytes.len() > (1 << 18) - 14 {
                return Err("剪贴板文本过长".into());
            }
            let mut b = Vec::with_capacity(14 + bytes.len());
            b.push(TYPE_SET_CLIPBOARD);
            b.extend_from_slice(&seq.to_be_bytes());
            b.push(u8::from(*paste));
            b.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            b.extend_from_slice(bytes);
            msgs.push(b);
        }
        Ctl::Power { on } => msgs.push(vec![TYPE_SET_DISPLAY_POWER, u8::from(*on)]),
        Ctl::StartApp { name } => {
            let name = split_utf8(name, 255).into_iter().next().unwrap_or("");
            let mut b = vec![TYPE_START_APP, name.len() as u8];
            b.extend_from_slice(name.as_bytes());
            msgs.push(b);
        }
        Ctl::Cmd { name } => {
            let t = match name.as_str() {
                "notif" => TYPE_EXPAND_NOTIFICATION_PANEL,
                "settings" => TYPE_EXPAND_SETTINGS_PANEL,
                "collapse" => TYPE_COLLAPSE_PANELS,
                "rotate" => TYPE_ROTATE_DEVICE,
                "reset_video" => TYPE_RESET_VIDEO,
                "keyboard_settings" => TYPE_OPEN_HARD_KEYBOARD_SETTINGS,
                other => return Err(format!("未知命令：{other}")),
            };
            msgs.push(vec![t]);
        }
    }
    Ok(msgs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        let touch: Ctl = serde_json::from_str(r#"{"t":"touch","action":0,"id":-2,"x":10,"y":20,"w":1080,"h":2340}"#).unwrap();
        assert_eq!(encode(&touch).unwrap()[0].len(), 32);
        let key: Ctl = serde_json::from_str(r#"{"t":"key","action":0,"code":4}"#).unwrap();
        assert_eq!(encode(&key).unwrap()[0].len(), 14);
        let sc: Ctl = serde_json::from_str(r#"{"t":"scroll","x":1,"y":1,"w":2,"h":2,"hs":0,"vs":1}"#).unwrap();
        assert_eq!(encode(&sc).unwrap()[0].len(), 21);
        let txt = Ctl::Text { text: "中".repeat(200) };
        let parts = encode(&txt).unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| p.len() <= 5 + 300));
    }
}
