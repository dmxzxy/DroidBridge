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

/// 将 HTTP 请求体流式写入本机临时文件（恒定内存开销，出错或 0 字节时自动清理临时文件）
pub async fn save_body_to_file(body: axum::body::Body, dest: &std::path::Path) -> Result<u64, String> {
    let mut stream = body.into_data_stream();
    let mut f = tokio::fs::File::create(dest)
        .await
        .map_err(|e| tr!("保存文件失败：{e}", "Failed to save the file: {e}"))?;
    let mut total = 0u64;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                if bytes.is_empty() {
                    continue;
                }
                if let Err(e) = f.write_all(&bytes).await {
                    drop(f);
                    let _ = tokio::fs::remove_file(dest).await;
                    return Err(tr!("写入临时文件失败：{e}", "Cannot write the temporary file: {e}"));
                }
                total += bytes.len() as u64;
            }
            Err(e) => {
                drop(f);
                let _ = tokio::fs::remove_file(dest).await;
                return Err(tr!("读取上传数据流中断：{e}", "Upload stream interrupted: {e}"));
            }
        }
    }
    if let Err(e) = f.flush().await {
        drop(f);
        let _ = tokio::fs::remove_file(dest).await;
        return Err(e.to_string());
    }
    drop(f);
    if total == 0 {
        let _ = tokio::fs::remove_file(dest).await;
    }
    Ok(total)
}

/// 从已建立连接并写完请求体的 TcpStream 读取对端 HTTP 响应头与小响应体
async fn read_http_reply(s: &mut TcpStream) -> Result<(u16, String, Vec<u8>), String> {
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.map_err(|e| e.to_string())?;
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

/// 将浏览器传入的 `axum::body::Body` 流式转发到远程节点（已知长度走 `Content-Length`，未知走 `Transfer-Encoding: chunked`，不落盘且不缓冲整包）
pub async fn post_body_stream(
    peer: &Peer,
    path_query: &str,
    headers: &[(&str, &str)],
    content_len: Option<u64>,
    body: axum::body::Body,
    total: Duration,
) -> Result<serde_json::Value, String> {
    let addr = &peer.addr;
    let path = with_token(path_query, &peer.token);
    let mut extra = String::new();
    for (k, v) in headers {
        extra.push_str(&format!("{k}: {v}\r\n"));
    }
    let fut = async {
        let mut s = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
            .await
            .map_err(|_| tr!("连接 {addr} 超时", "Connection to {addr} timed out"))?
            .map_err(|e| tr!("无法连接 {addr}：{e}", "Cannot connect to {addr}: {e}"))?;
        let framing = match content_len {
            Some(n) => format!("Content-Length: {n}\r\n"),
            None => "Transfer-Encoding: chunked\r\n".to_string(),
        };
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: {addr}\r\nAccept: application/json\r\nX-Lang: {}\r\n{extra}Content-Type: application/octet-stream\r\n{framing}Connection: close\r\n\r\n",
            crate::i18n::code(),
        );
        s.write_all(req.as_bytes()).await.map_err(|e| e.to_string())?;
        let mut stream = body.into_data_stream();
        let mut sent = 0u64;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| e.to_string())?;
            if bytes.is_empty() {
                continue;
            }
            sent += bytes.len() as u64;
            if content_len.is_some() {
                s.write_all(&bytes).await.map_err(|e| e.to_string())?;
            } else {
                let hdr = format!("{:x}\r\n", bytes.len());
                s.write_all(hdr.as_bytes()).await.map_err(|e| e.to_string())?;
                s.write_all(&bytes).await.map_err(|e| e.to_string())?;
                s.write_all(b"\r\n").await.map_err(|e| e.to_string())?;
            }
        }
        if content_len.is_none() {
            s.write_all(b"0\r\n\r\n").await.map_err(|e| e.to_string())?;
        }
        if sent == 0 && content_len == Some(0) {
            return Err(tr!("文件为空", "File is empty"));
        }
        read_http_reply(&mut s).await
    };
    let (code, _head, resp) = tokio::time::timeout(total, fut)
        .await
        .map_err(|_| tr!("请求 {addr} 超时", "Request to {addr} timed out"))??;
    parse_json_reply(code, &resp)
}

/// 将本机磁盘上的文件流式发送到远程节点（供插件远程 `push` / `install` 大 APK 使用，零内存缓冲）
pub async fn send_file_ex(
    peer: &Peer,
    method: &str,
    path_query: &str,
    headers: &[(&str, &str)],
    file_path: &std::path::Path,
    total: Duration,
) -> Result<(u16, String, Vec<u8>), String> {
    let addr = &peer.addr;
    let path = with_token(path_query, &peer.token);
    let mut extra = String::new();
    for (k, v) in headers {
        extra.push_str(&format!("{k}: {v}\r\n"));
    }
    let mut file = tokio::fs::File::open(file_path).await.map_err(|e| {
        let p = file_path.display();
        tr!("无法读取 {p}：{e}", "Cannot read {p}: {e}")
    })?;
    let len = file.metadata().await.map_err(|e| e.to_string())?.len();
    let fut = async {
        let mut s = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
            .await
            .map_err(|_| tr!("连接 {addr} 超时", "Connection to {addr} timed out"))?
            .map_err(|e| tr!("无法连接 {addr}：{e}", "Cannot connect to {addr}: {e}"))?;
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nAccept: application/json\r\nX-Lang: {}\r\n{extra}Content-Type: application/octet-stream\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
            crate::i18n::code(),
        );
        s.write_all(req.as_bytes()).await.map_err(|e| e.to_string())?;
        tokio::io::copy(&mut file, &mut s).await.map_err(|e| e.to_string())?;
        read_http_reply(&mut s).await
    };
    let (code, head, resp) = tokio::time::timeout(total, fut)
        .await
        .map_err(|_| tr!("请求 {addr} 超时", "Request to {addr} timed out"))??;
    Ok((code, content_type_of(&head), resp))
}

/// 流式代理远程节点的文件下载响应（保留 `Content-Disposition` / `Content-Length` 头，按 64 KB 分块转发，大文件不占内存）
pub async fn forward_download_stream(peer: &Peer, path_query: &str, timeout: Duration) -> Result<axum::response::Response, String> {
    use axum::response::IntoResponse;
    let addr = &peer.addr;
    let path = with_token(path_query, &peer.token);
    let (mut s, raw_head, prefix) = tokio::time::timeout(timeout, async {
        let mut s = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
            .await
            .map_err(|_| tr!("连接 {addr} 超时", "Connection to {addr} timed out"))?
            .map_err(|e| tr!("无法连接 {addr}：{e}", "Cannot connect to {addr}: {e}"))?;
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nX-Lang: {}\r\nConnection: close\r\n\r\n",
            crate::i18n::code(),
        );
        s.write_all(req.as_bytes()).await.map_err(|e| e.to_string())?;
        let mut hdr_buf = Vec::with_capacity(2048);
        let mut chunk = [0u8; 2048];
        loop {
            let n = s.read(&mut chunk).await.map_err(|e| e.to_string())?;
            if n == 0 {
                return Err(tr!("对端响应格式错误", "Malformed response"));
            }
            hdr_buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = hdr_buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&hdr_buf[..pos]).to_string();
                let rem = hdr_buf[pos + 4..].to_vec();
                return Ok((s, head, rem));
            }
            if hdr_buf.len() > 32 * 1024 {
                return Err(tr!("对端响应头过长", "Response header too large"));
            }
        }
    })
    .await
    .map_err(|_| tr!("请求 {addr} 超时", "Request to {addr} timed out"))??;

    let lower = raw_head.to_ascii_lowercase();
    let status: u16 = lower
        .split_whitespace()
        .nth(1)
        .and_then(|x| x.parse().ok())
        .ok_or_else(|| tr!("对端响应格式错误", "Malformed response"))?;

    if status >= 400 || lower.contains("transfer-encoding: chunked") {
        let mut body = prefix;
        s.read_to_end(&mut body).await.map_err(|e| e.to_string())?;
        if lower.contains("transfer-encoding: chunked") {
            body = dechunk(&body);
        }
        return Ok(crate::peer_reply(status, content_type_of(&lower), body));
    }

    let mut resp_headers = axum::http::HeaderMap::new();
    for line in raw_head.lines().skip(1) {
        let Some((k, v)) = line.split_once(':') else { continue };
        let kl = k.trim().to_ascii_lowercase();
        if matches!(kl.as_str(), "content-type" | "content-length" | "content-disposition") {
            if let (Ok(hn), Ok(hv)) = (axum::http::HeaderName::from_bytes(kl.as_bytes()), axum::http::HeaderValue::from_str(v.trim())) {
                resp_headers.insert(hn, hv);
            }
        }
    }
    let stream = futures_util::stream::unfold((Some(prefix), s), |(mut first, mut sock)| async move {
        if let Some(p) = first.take() {
            if !p.is_empty() {
                return Some((Ok::<Vec<u8>, std::io::Error>(p), (None, sock)));
            }
        }
        let mut buf = vec![0u8; 64 * 1024];
        match sock.read(&mut buf).await {
            Ok(0) | Err(_) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(buf), (None, sock)))
            }
        }
    });
    let code = axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::OK);
    Ok((code, resp_headers, axum::body::Body::from_stream(stream)).into_response())
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
    proxy_ws_with_ctl(socket, peer, path, raw_query, None).await;
}

/// 双向桥接远程 WebSocket；当 `ctl_tap` 为 `Some((state, dev_id))` 时同步旁路捕获浏览器发往远程投屏的控制指令供本机插件监听
pub async fn proxy_ws_with_ctl(
    socket: WebSocket,
    peer: Peer,
    path: &'static str,
    raw_query: Option<String>,
    ctl_tap: Option<(std::sync::Arc<crate::AppState>, String)>,
) {
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
    let session_sz = std::sync::Arc::new(std::sync::Mutex::new(None::<(u32, u32)>));
    let session_up = session_sz.clone();
    let has_tap = ctl_tap.is_some();

    let up = async {
        while let Some(Ok(m)) = crx.next().await {
            let mut tap_ctl = None;
            let tm = match m {
                AMsg::Text(t) => {
                    if ctl_tap.is_some() {
                        if let Ok(ctl) = serde_json::from_str::<crate::control::Ctl>(t.as_str()) {
                            if !matches!(&ctl, crate::control::Ctl::Cmd { name } if name == "reset_video") {
                                tap_ctl = Some(ctl);
                            }
                        }
                    }
                    TMsg::Text(t.as_str().into())
                }
                AMsg::Binary(b) => TMsg::Binary(b),
                AMsg::Close(_) => break,
                _ => continue,
            };
            if rtx.send(tm).await.is_err() {
                break;
            }
            if let (Some((st, dev_id)), Some(ctl)) = (&ctl_tap, tap_ctl) {
                let sz = *session_up.lock().unwrap();
                st.plugins.on_user_ctl(st, dev_id, &ctl, sz);
            }
        }
        let _ = rtx.close().await;
    };
    let down = async {
        while let Some(Ok(m)) = rrx.next().await {
            let am = match m {
                TMsg::Text(t) => {
                    if has_tap && t.contains("\"session\"") {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(t.as_str()) {
                            if v.get("type").and_then(|x| x.as_str()) == Some("session") {
                                let w = v.get("width").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                                let h = v.get("height").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                                if w > 0 && h > 0 {
                                    *session_sz.lock().unwrap() = Some((w, h));
                                }
                            }
                        }
                    }
                    AMsg::Text(t.as_str().into())
                }
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
