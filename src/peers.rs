//! 远程电脑（其他 DroidBridge 节点）管理与中转。

use crate::util::{parse_query, url_encode};
use axum::extract::ws::{Message as AMsg, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message as TMsg;

/// 连接远程节点得到的 WebSocket（TLS 未启用，只有明文流）
pub type WsClient = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

pub const DEFAULT_PORT: u16 = 8730;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub id: String,
    pub name: String,
    /// host:port
    pub addr: String,
    #[serde(default)]
    pub token: Option<String>,
    /// 通过 SSH 安装时使用的登录方式（不含密码），用于一键更新
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<SshInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SshInfo {
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    /// agent / password / key
    #[serde(default)]
    pub auth: String,
    #[serde(default)]
    pub key_file: String,
    /// 保存的令牌是安装时用 --token 设置的固定令牌（更新时沿用）；false 为授权得到的专属令牌
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_token: Option<bool>,
}

#[derive(Serialize)]
pub struct PeerPublic {
    pub id: String,
    pub name: String,
    pub addr: String,
    pub has_token: bool,
    pub ssh: Option<SshInfo>,
}

pub struct PeerStore {
    path: PathBuf,
    peers: RwLock<Vec<Peer>>,
}

impl PeerStore {
    pub fn load(path: PathBuf) -> Self {
        let peers = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<Peer>>(&b).ok())
            .unwrap_or_default();
        Self { path, peers: RwLock::new(peers) }
    }

    async fn save(&self, peers: &[Peer]) -> Result<(), String> {
        let data = serde_json::to_vec_pretty(peers).map_err(|e| e.to_string())?;
        if let Some(dir) = self.path.parent() {
            let _ = tokio::fs::create_dir_all(dir).await;
        }
        tokio::fs::write(&self.path, data).await.map_err(|e| tr!("保存 peers.json 失败：{e}", "Failed to save peers.json: {e}"))
    }

    pub async fn list(&self) -> Vec<PeerPublic> {
        self.peers
            .read()
            .await
            .iter()
            .map(|p| PeerPublic {
                id: p.id.clone(),
                name: p.name.clone(),
                addr: p.addr.clone(),
                has_token: p.token.as_deref().is_some_and(|t| !t.is_empty()),
                ssh: p.ssh.clone(),
            })
            .collect()
    }

    pub async fn get(&self, id: &str) -> Option<Peer> {
        self.peers.read().await.iter().find(|p| p.id == id).cloned()
    }

    pub async fn upsert(&self, peer: Peer) -> Result<(), String> {
        let mut peers = self.peers.write().await;
        if let Some(p) = peers.iter_mut().find(|p| p.id == peer.id || p.addr == peer.addr) {
            let ssh = peer.ssh.clone().or_else(|| p.ssh.clone());
            *p = Peer { ssh, ..peer };
        } else {
            peers.push(peer);
        }
        self.save(&peers).await
    }

    pub async fn update(&self, peer: Peer) -> Result<(), String> {
        let mut peers = self.peers.write().await;
        if peers.iter().any(|p| p.id != peer.id && p.addr == peer.addr) {
            return Err(tr!("该地址已在列表中", "This address is already in the list"));
        }
        let Some(p) = peers.iter_mut().find(|p| p.id == peer.id) else {
            return Err(tr!("远程电脑不存在", "Remote computer not found"));
        };
        let ssh = peer.ssh.clone().or_else(|| p.ssh.clone());
        *p = Peer { ssh, ..peer };
        self.save(&peers).await
    }

    /// 记录 SSH 登录方式（按地址）
    pub async fn set_ssh(&self, addr: &str, ssh: SshInfo) {
        let mut peers = self.peers.write().await;
        if let Some(p) = peers.iter_mut().find(|p| p.addr == addr) {
            p.ssh = Some(ssh);
            let _ = self.save(&peers).await;
        }
    }

    pub async fn remove(&self, id: &str) -> Result<bool, String> {
        let mut peers = self.peers.write().await;
        let before = peers.len();
        peers.retain(|p| p.id != id);
        let removed = peers.len() != before;
        self.save(&peers).await?;
        Ok(removed)
    }
}

/// "http://192.168.1.20:8730/" -> "192.168.1.20:8730"；缺省端口补 8730
pub fn normalize_addr(s: &str) -> Result<String, String> {
    let mut a = s.trim();
    for prefix in ["http://", "https://", "ws://", "wss://"] {
        if let Some(rest) = a.strip_prefix(prefix) {
            a = rest;
        }
    }
    let a = a.split('/').next().unwrap_or("").trim();
    if a.is_empty() {
        return Err(tr!("地址不能为空", "Address is required"));
    }
    if a.contains(' ') {
        return Err(tr!("地址格式不正确", "Invalid address"));
    }
    let has_port = if a.starts_with('[') {
        a.contains("]:")
    } else {
        a.matches(':').count() == 1
    };
    Ok(if has_port { a.to_string() } else { format!("{a}:{DEFAULT_PORT}") })
}

pub fn with_token(path: &str, token: &Option<String>) -> String {
    match token.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => {
            let sep = if path.contains('?') { '&' } else { '?' };
            format!("{path}{sep}token={}", url_encode(t))
        }
        None => path.to_string(),
    }
}

/// 极简 HTTP/1.1 GET（对端是 DroidBridge，返回 JSON），返回 (状态码, body)
pub async fn http_get(addr: &str, path: &str) -> Result<(u16, Vec<u8>), String> {
    http_request(addr, "GET", path, &[], Duration::from_secs(8)).await
}

pub async fn http_request(
    addr: &str,
    method: &str,
    path: &str,
    body: &[u8],
    total: Duration,
) -> Result<(u16, Vec<u8>), String> {
    let (code, _head, body) = send_request(addr, method, path, "", body, total).await?;
    Ok((code, body))
}

/// 发一个 HTTP/1.1 请求，返回 (状态码, 小写化的响应头, body)。
/// `extra` 是额外的请求头（每行以 `\r\n` 结尾，可以带一个末尾换行）。
async fn send_request(addr: &str, method: &str, path: &str, extra: &str, body: &[u8], total: Duration) -> Result<(u16, String, Vec<u8>), String> {
    let fut = async {
        let mut s = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(addr))
            .await
            .map_err(|_| tr!("连接 {addr} 超时", "Connection to {addr} timed out"))?
            .map_err(|e| tr!("无法连接 {addr}：{e}", "Cannot connect to {addr}: {e}"))?;
        let extra = extra.trim_end_matches(['\r', '\n']);
        let extra = if extra.is_empty() { String::new() } else { format!("{extra}\r\n") };
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nAccept: application/json\r\nX-Lang: {}\r\n{extra}Content-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            crate::i18n::code(),
            if body.first() == Some(&b'{') { "application/json" } else { "application/octet-stream" },
            body.len()
        );
        s.write_all(req.as_bytes()).await.map_err(|e| e.to_string())?;
        if !body.is_empty() {
            s.write_all(body).await.map_err(|e| e.to_string())?;
        }
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.map_err(|e| e.to_string())?;
        Ok::<_, String>(buf)
    };
    let buf = tokio::time::timeout(total, fut)
        .await
        .map_err(|_| tr!("请求 {addr} 超时", "Request to {addr} timed out"))??;
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| tr!("对端响应格式错误", "Malformed response"))?;
    let head = String::from_utf8_lossy(&buf[..split]).to_ascii_lowercase();
    let mut body = buf[split + 4..].to_vec();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| tr!("对端响应格式错误", "Malformed response"))?;
    if head.contains("transfer-encoding: chunked") {
        body = dechunk(&body);
    }
    Ok((status, head, body))
}

fn content_type_of(head: &str) -> String {
    head.lines()
        .find_map(|l| l.strip_prefix("content-type:"))
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|| "application/octet-stream".into())
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(pos) = data.windows(2).position(|w| w == b"\r\n") else { break };
        let size_s = String::from_utf8_lossy(&data[..pos]);
        let Ok(size) = usize::from_str_radix(size_s.split(';').next().unwrap_or("").trim(), 16) else { break };
        if size == 0 {
            break;
        }
        let start = pos + 2;
        if data.len() < start + size {
            out.extend_from_slice(&data[start..]);
            break;
        }
        out.extend_from_slice(&data[start..start + size]);
        data = &data[(start + size + 2).min(data.len())..];
    }
    out
}

pub async fn get_json(peer: &Peer, path: &str) -> Result<serde_json::Value, String> {
    let (code, body) = http_get(&peer.addr, &with_token(path, &peer.token)).await?;
    parse_json_reply(code, &body)
}

/// 把文件转发到远程节点：endpoint 为 "/api/push"（安装 APK / 传到 Download）或 "/api/files/upload"（传到指定目录）
pub async fn post_file(peer: &Peer, endpoint: &str, query: &str, body: &[u8]) -> Result<serde_json::Value, String> {
    let path = with_token(&format!("{endpoint}?{query}"), &peer.token);
    let (code, resp) = http_request(&peer.addr, "POST", &path, body, Duration::from_secs(600)).await?;
    parse_json_reply(code, &resp)
}

/// GET 转发并保留 Content-Type（图片等二进制内容）
pub async fn forward_raw(peer: &Peer, path_query: &str, timeout: Duration) -> Result<(u16, String, Vec<u8>), String> {
    forward_ex(peer, "GET", path_query, &[], b"", timeout).await
}

/// 转发请求并返回 (状态码, Content-Type, body)：可以带自定义请求头与二进制 body
pub async fn forward_ex(
    peer: &Peer,
    method: &str,
    path_query: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    timeout: Duration,
) -> Result<(u16, String, Vec<u8>), String> {
    let mut extra = String::new();
    for (k, v) in headers {
        extra.push_str(&format!("{k}: {v}\r\n"));
    }
    let path = with_token(path_query, &peer.token);
    let (code, head, body) = send_request(&peer.addr, method, &path, &extra, body, timeout).await?;
    Ok((code, content_type_of(&head), body))
}

/// 把请求原样转发给远程节点（自动附加令牌），返回 (状态码, body)
pub async fn forward(peer: &Peer, method: &str, path_query: &str, body: &[u8], timeout: Duration) -> Result<(u16, Vec<u8>), String> {
    http_request(&peer.addr, method, &with_token(path_query, &peer.token), body, timeout).await
}

/// 作为客户端连接远程节点的 WebSocket（自动附加令牌；`path_query` 以 `/` 开头）
pub async fn ws_connect(peer: &Peer, path_query: &str) -> Result<WsClient, String> {
    let url = format!("ws://{}{}", peer.addr, with_token(path_query, &peer.token));
    let n = &peer.name;
    match tokio::time::timeout(Duration::from_secs(5), tokio_tungstenite::connect_async_with_config(url.as_str(), None, true)).await {
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(e)) => Err(tr!("连接远程电脑 {n} 失败：{e}", "Cannot connect to remote computer {n}: {e}")),
        Err(_) => Err(tr!("连接远程电脑 {n} 超时", "Connection to remote computer {n} timed out")),
    }
}

fn parse_json_reply(code: u16, body: &[u8]) -> Result<serde_json::Value, String> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| tr!("对端返回非 JSON（HTTP {code}），确认对方运行的是 DroidBridge", "Unexpected non-JSON response (HTTP {code}); is the other side running DroidBridge?"))?;
    if code == 401 {
        return Err(tr!("对方电脑尚未授权本机（或令牌错误）", "Not authorized by the remote computer (or wrong token)"));
    }
    if code >= 400 {
        return Err(v.get("error").and_then(|e| e.as_str()).map(String::from).unwrap_or_else(|| tr!("对端错误", "Remote error")));
    }
    Ok(v)
}

/// 把本地浏览器的 WebSocket 与远程节点的同名接口（/ws/stream、/ws/logcat）双向桥接
pub async fn proxy_ws(socket: WebSocket, peer: Peer, path: &'static str, raw_query: Option<String>) {
    let q = parse_query(raw_query.as_deref());
    let mut parts = Vec::new();
    for (k, v) in q.iter() {
        if k == "peer" || k == "token" {
            continue;
        }
        parts.push(format!("{}={}", url_encode(k), url_encode(v)));
    }
    let path = format!("{path}?{}", parts.join("&"));

    let (mut ctx, mut crx) = socket.split();
    let remote = match ws_connect(&peer, &path).await {
        Ok(ws) => ws,
        Err(e) => {
            let msg = serde_json::json!({"type":"error","msg":e});
            let _ = ctx.send(AMsg::Text(msg.to_string().into())).await;
            return;
        }
    };
    let (mut rtx, mut rrx) = remote.split();

    let up = async {
        while let Some(Ok(m)) = crx.next().await {
            let tm = match m {
                AMsg::Text(t) => TMsg::Text(t.as_str().into()),
                AMsg::Binary(b) => TMsg::Binary(b),
                AMsg::Close(_) => break,
                _ => continue,
            };
            if rtx.send(tm).await.is_err() {
                break;
            }
        }
        let _ = rtx.close().await;
    };
    let down = async {
        while let Some(Ok(m)) = rrx.next().await {
            let am = match m {
                TMsg::Text(t) => AMsg::Text(t.as_str().into()),
                TMsg::Binary(b) => AMsg::Binary(b),
                TMsg::Close(_) => break,
                _ => continue,
            };
            if ctx.send(am).await.is_err() {
                break;
            }
        }
        let _ = ctx.close().await;
    };
    tokio::select! {
        _ = up => {},
        _ = down => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addr() {
        assert_eq!(normalize_addr("192.168.1.2").unwrap(), "192.168.1.2:8730");
        assert_eq!(normalize_addr("http://pc-a:9000/").unwrap(), "pc-a:9000");
        assert_eq!(normalize_addr("[fe80::1]:8730").unwrap(), "[fe80::1]:8730");
        assert!(normalize_addr("  ").is_err());
    }

    #[test]
    fn chunked() {
        assert_eq!(dechunk(b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n"), b"abcde");
    }
}
