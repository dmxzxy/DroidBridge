//! MCP 服务（Model Context Protocol，Streamable HTTP）：POST /mcp
//!
//! 让 Claude Code、Codex、Cursor 等 AI 客户端读取并操作所有电脑上的 Android 设备。
//! 设备 id：本机设备直接用 serial；远程电脑上的设备为 "<远程电脑 id>/<serial>"（list_devices 会列出）。
//! 工具在设备所在节点执行（远程设备转发到对方的 /api/agent/exec），投屏窗口会显示 AI 的每次操作。

use crate::{adb, agent, peers, AppState};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

const PROTOCOL: &str = "2025-06-18";

fn instructions() -> String {
    "DroidBridge controls real Android devices attached to this and other computers. \
Typical loop: list_devices → read_screen (UI elements with text and coordinates; cheap and precise) or screenshot (for games, images or when read_screen fails) → tap / swipe / type_text / press_key → read again to verify. \
Coordinates are pixel coordinates of the screenshot image (read_screen uses the same coordinate space). \
A human may be watching the device live and can pause you; if a tool says the device is paused, stop and tell the user. \
Prefer read-only tools when unsure, and ask the user before destructive actions."
        .into()
}

fn tools() -> Value {
    let dev = json!({"type": "string", "description": "Device id from list_devices. May be omitted when exactly one device is available."});
    let t = |name: &str, desc: &str, props: Value, required: &[&str], read_only: bool| {
        let mut p = props.as_object().cloned().unwrap_or_default();
        if name != "list_devices" {
            p.insert("device".into(), dev.clone());
        }
        json!({
            "name": name,
            "description": desc,
            "inputSchema": {"type": "object", "properties": p, "required": required},
            "annotations": {"readOnlyHint": read_only, "destructiveHint": !read_only && matches!(name, "stop_app")},
        })
    };
    json!([
        t("list_devices", "List Android devices on this computer and on connected remote computers, with their ids, models and state.", json!({}), &[], true),
        t("read_screen", "Read the current UI as a compact list of elements: [index] class \"text\" desc id @(center x,y) flags (click, scroll, edit, checked…). Coordinates match screenshot pixels. Also returns the foreground activity. Fails on games / animations / secure screens — use screenshot then.", json!({}), &[], true),
        t("screenshot", "Capture the screen as a JPEG (scaled so the long side ≤ 1568 px). Use its pixel coordinates for tap / swipe.", json!({}), &[], true),
        t("tap", "Tap (or long-press) an element from the last read_screen by index, or a point in screenshot coordinates.", json!({
            "element": {"type": "integer", "description": "Element index from the last read_screen"},
            "x": {"type": "number"}, "y": {"type": "number"},
            "long": {"type": "boolean", "description": "Long press (≈0.8 s)"}
        }), &[], false),
        t("swipe", "Swipe by direction (scrolls content: 'up' moves the finger up, revealing content below) or between two points in screenshot coordinates.", json!({
            "direction": {"type": "string", "enum": ["up", "down", "left", "right"]},
            "distance": {"type": "number", "description": "Fraction of the screen for direction swipes, 0.1–0.9 (default 0.5)"},
            "x1": {"type": "number"}, "y1": {"type": "number"}, "x2": {"type": "number"}, "y2": {"type": "number"},
            "duration_ms": {"type": "integer", "description": "Default 300"}
        }), &[], false),
        t("type_text", "Type text into the focused input field (any language, via clipboard paste). Tap the field first. Optionally press Enter afterwards.", json!({
            "text": {"type": "string"},
            "submit": {"type": "boolean", "description": "Press Enter after typing"}
        }), &["text"], false),
        t("press_key", "Press a key: back, home, recents, enter, delete, tab, escape, menu, search, power, wakeup, volume_up, volume_down, dpad_up/down/left/right/center, space, page_up, page_down, select_all, or an Android keycode number.", json!({
            "key": {"type": "string"}
        }), &["key"], false),
        t("list_apps", "List installed apps (name — package). User apps only unless include_system.", json!({
            "query": {"type": "string", "description": "Filter by name or package"},
            "include_system": {"type": "boolean"}
        }), &[], true),
        t("launch_app", "Launch an app by package name (opens its launcher activity).", json!({"package": {"type": "string"}}), &["package"], false),
        t("stop_app", "Force-stop an app by package name.", json!({"package": {"type": "string"}}), &["package"], false),
        t("logcat", "Read recent device logs (newest last).", json!({
            "lines": {"type": "integer", "description": "Default 200, max 2000"},
            "level": {"type": "string", "enum": ["V", "D", "I", "W", "E", "F"], "description": "Minimum level"},
            "package": {"type": "string", "description": "Only this running app"},
            "grep": {"type": "string", "description": "Case-insensitive text filter"}
        }), &[], true),
        t("perf", "Performance snapshot of the foreground app (≈1 s): FPS, app/device CPU, memory, CPU/battery temperature, battery level.", json!({}), &[], true),
        t("shell", "Run a read-only adb shell query (dumpsys, getprop, pm list, settings get, cat, ls, ps, top…, optionally piped to grep/head/tail). Commands that change the device are rejected.", json!({
            "command": {"type": "string"}
        }), &["command"], true),
        t("wait", "Wait for animations or loading to finish.", json!({"seconds": {"type": "number", "description": "0.1–30, default 1"}}), &[], true),
    ])
}

/// 所有设备：(设备 id, 所在电脑名, 远程电脑 id, serial, model, state)
async fn all_devices(st: &AppState) -> (Vec<(String, String, Option<String>, String, String, String)>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    match adb::list_devices(&st.cfg.adb).await {
        Ok(list) => {
            for d in list {
                out.push((d.serial.clone(), st.cfg.name.clone(), None, d.serial, d.model, d.state));
            }
        }
        Err(e) => errors.push(format!("{}: {e}", st.cfg.name)),
    }
    for p in st.peers.list().await {
        let Some(peer) = st.peers.get(&p.id).await else { continue };
        match peers::get_json(&peer, "/api/devices").await {
            Ok(v) => {
                for d in v.as_array().into_iter().flatten() {
                    let serial = d["serial"].as_str().unwrap_or("").to_string();
                    let model = d["model"].as_str().unwrap_or("").to_string();
                    let state = d["state"].as_str().unwrap_or("").to_string();
                    out.push((format!("{}/{serial}", p.id), p.name.clone(), Some(p.id.clone()), serial, model, state));
                }
            }
            Err(e) => errors.push(format!("{}: {e}", p.name)),
        }
    }
    (out, errors)
}

/// 设备 id → (远程电脑 id, serial)
async fn resolve(st: &AppState, device: Option<&str>) -> Result<(Option<String>, String), String> {
    let (list, _) = all_devices(st).await;
    let ready: Vec<_> = list.iter().filter(|d| d.5 == "device").collect();
    match device.map(str::trim).filter(|s| !s.is_empty()) {
        None => match ready.as_slice() {
            [d] => Ok((d.2.clone(), d.3.clone())),
            [] => Err("No device is available. Connect a device and call list_devices.".into()),
            _ => Err("Several devices are available; pass `device` (see list_devices).".into()),
        },
        Some(id) => {
            if let Some(d) = list.iter().find(|d| d.0 == id || (d.2.is_none() && d.3 == id)) {
                return if d.5 == "device" { Ok((d.2.clone(), d.3.clone())) } else { Err(format!("Device {id} is not ready (state: {}).", d.5)) };
            }
            // 只给了 serial：在所有电脑中唯一匹配也可以
            let m: Vec<_> = list.iter().filter(|d| d.3 == id).collect();
            match m.as_slice() {
                [d] if d.5 == "device" => Ok((d.2.clone(), d.3.clone())),
                [d] => Err(format!("Device {id} is not ready (state: {}).", d.5)),
                _ => Err(format!("Device {id} not found; call list_devices.")),
            }
        }
    }
}

async fn call(st: &AppState, name: &str, mut args: Value) -> Value {
    let err = |msg: String| json!({"content": [{"type": "text", "text": msg}], "isError": true});
    if name == "list_devices" {
        let (list, errors) = all_devices(st).await;
        let mut lines: Vec<String> = list
            .iter()
            .map(|(id, host, _, _, model, state)| {
                let m = if model.is_empty() { String::new() } else { format!(" {model}") };
                let s = if state == "device" { String::new() } else { format!(" ({state})") };
                format!("- {id}{m} — on {host}{s}")
            })
            .collect();
        if lines.is_empty() {
            lines.push("No devices.".into());
        }
        for e in errors {
            lines.push(format!("(unreachable: {e})"));
        }
        return json!({"content": [{"type": "text", "text": lines.join("\n")}]});
    }
    if !tools().as_array().unwrap().iter().any(|t| t["name"] == name) {
        return err(format!("Unknown tool: {name}"));
    }
    let device = args.get("device").and_then(|v| v.as_str()).map(String::from);
    let (peer, serial) = match resolve(st, device.as_deref()).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if !args.is_object() {
        args = json!({});
    }
    args["serial"] = json!(serial);
    match peer {
        None => match agent::exec(st, name, &args).await {
            Ok(content) => json!({"content": content}),
            Err(e) => err(e),
        },
        Some(id) => {
            let Some(p) = st.peers.get(&id).await else { return err(format!("Remote computer {id} not found")) };
            let body = json!({"tool": name, "args": args}).to_string();
            match peers::forward(&p, "POST", if crate::i18n::is_en() { "/api/agent/exec?lang=en" } else { "/api/agent/exec?lang=zh" }, body.as_bytes(), Duration::from_secs(120)).await {
                Ok((200, b)) => serde_json::from_slice(&b).unwrap_or_else(|_| err("Invalid reply from the remote computer".into())),
                Ok((404, _)) => err(format!("{} runs an older DroidBridge without AI tools; update it first.", p.name)),
                Ok((code, b)) => err(format!("{} returned HTTP {code}: {}", p.name, String::from_utf8_lossy(&b))),
                Err(e) => err(e),
            }
        }
    }
}

async fn rpc(st: &AppState, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    // 通知（没有 id）不需要回复
    let id = id?;
    let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id.clone(), "result": result});
    let fail = |code: i32, m: &str| json!({"jsonrpc": "2.0", "id": id.clone(), "error": {"code": code, "message": m}});
    Some(match method {
        "initialize" => {
            let asked = msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL);
            let ver = if ["2024-11-05", "2025-03-26", "2025-06-18"].contains(&asked) { asked } else { PROTOCOL };
            ok(json!({
                "protocolVersion": ver,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "droidbridge", "title": "DroidBridge", "version": env!("CARGO_PKG_VERSION")},
                "instructions": instructions(),
            }))
        }
        "ping" => ok(json!({})),
        "tools/list" => ok(json!({"tools": tools()})),
        "tools/call" => {
            let p = &msg["params"];
            let name = p["name"].as_str().unwrap_or("");
            ok(call(st, name, p.get("arguments").cloned().unwrap_or(json!({}))).await)
        }
        "resources/list" => ok(json!({"resources": []})),
        "prompts/list" => ok(json!({"prompts": []})),
        _ => fail(-32601, &format!("Method not found: {method}")),
    })
}

pub async fn handle(State(st): State<Arc<AppState>>, body: axum::body::Bytes) -> Response {
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return Json(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "Parse error"}})).into_response();
    };
    let reply = if let Some(batch) = v.as_array() {
        let mut out = Vec::new();
        for m in batch {
            if let Some(r) = rpc(&st, m).await {
                out.push(r);
            }
        }
        (!out.is_empty()).then_some(Value::Array(out))
    } else {
        rpc(&st, &v).await
    };
    match reply {
        Some(r) => ([(header::CONTENT_TYPE, "application/json")], r.to_string()).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// 不提供服务端推送（SSE）与会话删除
pub async fn handle_get() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_schemas() {
        let t = tools();
        let names: Vec<&str> = t.as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"read_screen") && names.contains(&"tap") && names.contains(&"type_text"));
        for x in t.as_array().unwrap() {
            assert_eq!(x["inputSchema"]["type"], "object");
            let has_dev = x["inputSchema"]["properties"].get("device").is_some();
            assert_eq!(has_dev, x["name"] != "list_devices");
        }
    }
}
