//! 远程设备执行接口 `POST /api/plugins/devop`（SPEC 7.2）。
//!
//! 设备所在的电脑 B 提供这个接口（权限已在发起方 A 检查，B 只记录审计）；
//! A 用自己保存的令牌访问 B，参数放在 `X-Devop`（url 编码的 JSON）里，
//! 文件内容（push / install 的 APK）是请求体。B 上的实现与插件在本机的操作
//! 共用 `calls::exec_local`，不复制逻辑。
//!
//! 返回 `{ok: ...}` 或 `{error}`；`pull`、`screenshot` 返回 `application/octet-stream`。

use super::calls::{exec_local, DevOut};
use super::*;
use crate::{peers, util};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::net::SocketAddr;

/// 允许远程执行的设备操作（SPEC 7.2）
const OPS: &[&str] = &[
    "prop", "shell", "push", "pull", "install", "uninstall", "apps", "hasApp", "launch", "stop", "clear", "foreground", "input", "logcat", "perf", "screenshot",
    "ui", "files", "mkdir", "rename", "rm", "clipboard", "cmd",
];

fn err(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({"error": msg.into()}))).into_response()
}

/// B 侧：执行一个远程请求的设备操作
pub async fn plugins_devop(
    State(st): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    let op = q.get("op").map(String::as_str).unwrap_or("").to_string();
    let serial = q.get("serial").map(String::as_str).unwrap_or("").trim().to_string();
    if !OPS.contains(&op.as_str()) {
        return err(StatusCode::BAD_REQUEST, tr!("未知操作：{op}", "Unknown operation: {op}"));
    }
    if serial.is_empty() || serial.contains('/') {
        return err(StatusCode::BAD_REQUEST, tr!("serial 参数不正确", "Invalid serial parameter"));
    }
    let params = match headers.get("x-devop").and_then(|v| v.to_str().ok()).filter(|v| !v.trim().is_empty()) {
        Some(raw) => match serde_json::from_str::<Value>(&util::url_decode(raw)) {
            Ok(v) if v.is_object() => v,
            Ok(_) => return err(StatusCode::BAD_REQUEST, tr!("X-Devop 必须是 JSON 对象", "X-Devop must be a JSON object")),
            Err(e) => {
                let e = e.to_string();
                return err(StatusCode::BAD_REQUEST, tr!("X-Devop 参数格式不正确：{e}", "Invalid X-Devop parameters: {e}"));
            }
        },
        None => json!({}),
    };
    let mut params = params;

    // push / install 的文件内容流式写到 `.tmp/<devop-id>/`，用完删除整个目录（避免大 APK 整包缓冲进内存）。
    // 文件名用 A 上的原名：adb install 要求以 .apk / .apex 结尾，push 到目录时它就是设备上的文件名
    let mut tmp: Option<PathBuf> = None;
    if matches!(op.as_str(), "push" | "install") {
        let id = format!("devop-{:08x}", util::random_scid());
        let dir = st.plugins.root().join(".tmp").join(&id);
        let _ = tokio::fs::create_dir_all(&dir).await;
        let name = params.get("name").and_then(Value::as_str).and_then(safe_name);
        let name = match (name, op.as_str()) {
            (Some(n), "install") => with_apk_ext(&n),
            (Some(n), _) => n,
            (None, "install") => format!("{id}.apk"),
            (None, _) => id,
        };
        let path = dir.join(name);
        let saved = peers::save_body_to_file(body, &path, peers::UPLOAD_MAX).await;
        let failed = match saved {
            Ok(0) => Some((StatusCode::BAD_REQUEST, tr!("缺少文件内容", "Missing file content"))),
            Ok(_) => None,
            Err(e) => Some((StatusCode::INTERNAL_SERVER_ERROR, e)),
        };
        if let Some((code, e)) = failed {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            return err(code, e);
        }
        let file = path.to_string_lossy().to_string();
        params[if op == "push" { "local" } else { "path" }] = json!(file);
        tmp = Some(dir);
    }

    let r = exec_local(&st, &op, &params, &serial).await;
    if let Some(p) = &tmp {
        let _ = tokio::fs::remove_dir_all(p).await;
    }
    let is_touch_move = op == "input"
        && params.get("kind").and_then(Value::as_str) == Some("touch")
        && params.get("action").and_then(Value::as_u64) == Some(2);
    if !is_touch_move {
        st.plugins.audit(
            "devop",
            &op,
            json!({
                "serial": serial,
                "from": addr.ip().to_canonical().to_string(),
                "ok": r.is_ok(),
                "detail": audit_detail(&op, &params),
            }),
        );
    }
    match r {
        Ok(DevOut::Json(v)) => Json(json!({"ok": v})).into_response(),
        Ok(DevOut::Bytes(b)) => ([(header::CONTENT_TYPE, "application/octet-stream")], b).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

/// A 上的文件名能直接在本机（可能是另一种系统）当文件名用时才返回
fn safe_name(s: &str) -> Option<String> {
    let n = s.rsplit(['/', '\\']).next()?;
    let stem = n.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|p| stem.strip_prefix(p).is_some_and(|d| d.len() == 1 && d.starts_with(|c: char| c.is_ascii_digit())));
    (!n.is_empty()
        && n.len() <= 200
        && !n.starts_with(' ')
        && !n.ends_with([' ', '.'])
        && !n.chars().any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        && !reserved)
    .then(|| n.to_string())
}

/// adb install 只接受以 .apk / .apex 结尾的文件名
fn with_apk_ext(n: &str) -> String {
    let lower = n.to_ascii_lowercase();
    if lower.ends_with(".apk") || lower.ends_with(".apex") {
        n.to_string()
    } else {
        format!("{n}.apk")
    }
}

/// 审计里只记有用的参数（不记文件内容与临时路径）
fn audit_detail(op: &str, p: &Value) -> Value {
    let mut d = Map::new();
    let keys: &[&str] = match op {
        "shell" => &["cmd", "timeout"],
        "push" | "pull" => &["remote"],
        "install" => &["replace", "grant", "downgrade"],
        "uninstall" | "launch" | "stop" | "clear" | "hasApp" => &["package"],
        "input" => &["kind"],
        "prop" => &["name"],
        "cmd" => &["name"],
        "mkdir" | "files" => &["path"],
        "rename" => &["path", "name"],
        "rm" => &["path", "paths"],
        _ => &[],
    };
    for k in keys {
        if let Some(v) = p.get(*k) {
            d.insert((*k).into(), v.clone());
        }
    }
    Value::Object(d)
}

/// 远程电脑上的版本太旧（没有 devop 接口）
pub(crate) fn too_old(peer: &peers::Peer) -> String {
    let n = &peer.name;
    tr!(
        "远程电脑 {n} 的 DroidBridge 版本太旧，请先更新",
        "The DroidBridge on remote computer {n} is too old; update it first"
    )
}

/// 向远程电脑发起一次 devop 请求，返回 (状态码, Content-Type, body)
pub(crate) async fn call(
    peer: &peers::Peer,
    raw: &str,
    op: &str,
    p: &Value,
    body: &[u8],
    timeout: Duration,
) -> Result<(u16, String, Vec<u8>), String> {
    let params = serde_json::to_string(p).map_err(|e| e.to_string())?;
    let hdr = util::url_encode(&params);
    // 参数走请求头（SPEC 7.2），太长时明确报错而不是被对方拒绝
    if hdr.len() > 6000 {
        return Err(tr!("参数过长，无法转发给远程电脑", "The parameters are too long to forward to the remote computer"));
    }
    let path = format!("/api/plugins/devop?op={}&serial={}", util::url_encode(op), util::url_encode(raw));
    peers::forward_ex(peer, "POST", &path, &[("X-Devop", hdr.as_str())], body, timeout).await
}

fn parse_reply(peer: &peers::Peer, code: u16, body: &[u8]) -> Result<Value, String> {
    if code == 404 {
        return Err(too_old(peer));
    }
    let n = &peer.name;
    let v: Value = serde_json::from_slice(body)
        .map_err(|_| tr!("远程电脑 {n} 返回了无法解析的响应（HTTP {code}）", "Remote computer {n} returned an invalid reply (HTTP {code})"))?;
    if code == 401 {
        return Err(tr!("对方电脑尚未授权本机（或令牌错误）", "Not authorized by the remote computer (or wrong token)"));
    }
    if code >= 400 {
        return Err(v
            .get("error")
            .and_then(|e| e.as_str())
            .map(String::from)
            .unwrap_or_else(|| tr!("远程电脑 {n} 返回错误（HTTP {code}）", "Remote computer {n} returned an error (HTTP {code})")));
    }
    Ok(v)
}

/// 发起 devop 请求并取 `ok` 的值
pub(crate) async fn json_call(
    peer: &peers::Peer,
    raw: &str,
    op: &str,
    p: &Value,
    body: &[u8],
    timeout: Duration,
) -> Result<Value, String> {
    let (code, _ct, reply) = call(peer, raw, op, p, body, timeout).await?;
    Ok(parse_reply(peer, code, &reply)?.get("ok").cloned().unwrap_or(Value::Null))
}

/// 发起带本机文件体的流式 devop 请求并取 `ok` 的值（不把文件读进内存）
pub(crate) async fn json_call_file(
    peer: &peers::Peer,
    raw: &str,
    op: &str,
    p: &Value,
    file_path: &std::path::Path,
    timeout: Duration,
) -> Result<Value, String> {
    let params = serde_json::to_string(p).map_err(|e| e.to_string())?;
    let hdr = util::url_encode(&params);
    if hdr.len() > 6000 {
        return Err(tr!("参数过长，无法转发给远程电脑", "The parameters are too long to forward to the remote computer"));
    }
    let path = format!("/api/plugins/devop?op={}&serial={}", util::url_encode(op), util::url_encode(raw));
    let (code, _ct, reply) = peers::send_file_ex(peer, "POST", &path, &[("X-Devop", hdr.as_str())], file_path, timeout).await?;
    Ok(parse_reply(peer, code, &reply)?.get("ok").cloned().unwrap_or(Value::Null))
}

/// 发起 devop 请求并返回二进制内容（pull / screenshot）
pub(crate) async fn bytes_call(peer: &peers::Peer, raw: &str, op: &str, p: &Value, timeout: Duration) -> Result<Vec<u8>, String> {
    let (code, _ct, reply) = call(peer, raw, op, p, b"", timeout).await?;
    if code >= 400 {
        parse_reply(peer, code, &reply)?;
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer() -> peers::Peer {
        peers::Peer { id: "p1".into(), name: "PCB".into(), addr: "127.0.0.1:1".into(), token: None, ssh: None }
    }

    #[test]
    fn ops_list() {
        assert!(OPS.contains(&"shell") && OPS.contains(&"pull") && OPS.contains(&"screenshot"));
        // 端口转发只能在本机做（SPEC 7.2）
        assert!(!OPS.contains(&"forward") && !OPS.contains(&"unforward") && !OPS.contains(&"tcpip"));
        assert!(!OPS.contains(&"badge") && !OPS.contains(&"logwatch"));
    }

    #[test]
    fn replies() {
        let p = peer();
        // 远程版本太旧
        let e = parse_reply(&p, 404, b"").unwrap_err();
        assert!(e.contains("PCB"), "{e}");
        assert!(e.contains("太旧") || e.contains("too old"), "{e}");
        let e = parse_reply(&p, 400, br#"{"error":"boom"}"#).unwrap_err();
        assert_eq!(e, "boom");
        assert_eq!(parse_reply(&p, 200, br#"{"ok":{"out":"x"}}"#).unwrap(), json!({"ok": {"out": "x"}}));
    }

    #[test]
    fn temp_file_names() {
        assert_eq!(safe_name("C:\\dir\\app.apk").as_deref(), Some("app.apk"));
        assert_eq!(safe_name("/tmp/微信 1.APK").as_deref(), Some("微信 1.APK"));
        assert_eq!(safe_name("/tmp/a:b.apk"), None, "Windows 不允许的字符");
        assert_eq!(safe_name("/tmp/con.apk"), None, "Windows 保留名");
        assert_eq!(safe_name("/tmp/a\nb.apk"), None);
        assert_eq!(safe_name("/tmp/.."), None);
        assert_eq!(safe_name(""), None);
        assert_eq!(with_apk_ext("app.apk"), "app.apk");
        assert_eq!(with_apk_ext("APP.APK"), "APP.APK");
        assert_eq!(with_apk_ext("a.apex"), "a.apex");
        assert_eq!(with_apk_ext("app"), "app.apk");
        assert_eq!(with_apk_ext("app.zip"), "app.zip.apk");
    }

    #[test]
    fn audit_has_no_secrets() {
        let p = json!({"cmd": "getprop x", "timeout": 5, "local": "C:/tmp/a.apk", "remote": "/sdcard/a.apk", "path": "C:/tmp/b.apk"});
        let d = audit_detail("push", &p);
        assert_eq!(d["remote"], json!("/sdcard/a.apk"));
        assert!(d.get("local").is_none(), "本机路径不写审计");
        let d = audit_detail("shell", &p);
        assert_eq!(d["cmd"], json!("getprop x"));
        assert!(audit_detail("screenshot", &p).as_object().unwrap().is_empty());
    }
}
