//! 访问控制：
//! - 拦截其他网站发起的请求（跨站请求、WebSocket 劫持、DNS 重绑定）
//! - 其他电脑与浏览器需经本机授权（配对）才能访问；本机访问不需要
//!
//! 配对流程：请求方 POST /api/pair/request → 本机（或任意已授权的电脑）界面弹出“允许 / 拒绝”，
//! 双方显示同一个 6 位验证码 → 允许后请求方取回专属令牌，以后免验证，可随时撤销。
//! 令牌只保存 SHA-256 摘要（clients.json）。

use axum::http::{header, HeaderMap};
use ring::rand::SecureRandom;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 配对请求等待批准的时间
const PAIR_TTL: Duration = Duration::from_secs(180);
/// 同时等待中的配对请求上限（防止被刷屏）
const PAIR_MAX: usize = 16;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Client {
    pub id: String,
    pub name: String,
    /// 授权时对方的 IP（仅用于显示）
    #[serde(default)]
    pub addr: String,
    /// 令牌的 SHA-256（十六进制）
    hash: String,
    /// 授权时间、最近使用时间（Unix 秒）
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub last_seen: i64,
}

#[derive(Serialize)]
pub struct ClientPublic {
    pub id: String,
    pub name: String,
    pub addr: String,
    pub created: i64,
    pub last_seen: i64,
}

#[derive(Clone, Debug, PartialEq)]
enum PairState {
    Waiting,
    Approved(String),
    Denied,
}

struct Pending {
    name: String,
    ip: String,
    code: String,
    created: Instant,
    state: PairState,
}

/// 本机向远程电脑发起的配对（添加远程电脑时）
#[derive(Clone)]
pub struct Outgoing {
    pub addr: String,
    pub name: String,
    pub peer_id: Option<String>,
    pub remote_id: String,
    pub created: Instant,
}

pub struct Access {
    path: PathBuf,
    clients: Mutex<Vec<Client>>,
    pending: Mutex<HashMap<String, Pending>>,
    pub outgoing: Mutex<HashMap<String, Outgoing>>,
    /// 允许的额外主机名（反向代理、自定义域名），小写
    allow_hosts: Vec<String>,
    /// 本机的计算机名（小写；局域网内常用 名称 / 名称.local 访问）
    names: Vec<String>,
    settings_path: PathBuf,
    /// 是否让局域网内其他电脑发现本机
    pub discoverable: AtomicBool,
    /// 局域网内无需授权（界面设置）
    open: AtomicBool,
    local_ips: Mutex<(Option<Instant>, Vec<IpAddr>)>,
    /// 最近访问过本机的连接者（身份 → 最近一次请求的时间）
    seen: Mutex<HashMap<String, (Ident, Instant)>>,
}

/// 发起请求的是谁（由 auth 中间件放进请求扩展里）
#[derive(Clone, Debug)]
pub struct Ident {
    /// local / c:<授权 id> / ip:<IP>
    pub key: String,
    /// 显示名称：本机为电脑名，授权的电脑为授权时的名称，其他为 IP
    pub name: String,
    /// 对方 IP（本机为空）
    pub addr: String,
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    ring::rand::SystemRandom::new().fill(&mut b).expect("system random");
    b
}

/// 新的随机令牌（192 位）
pub fn new_token() -> String {
    hex(&random_bytes::<24>())
}

fn new_id() -> String {
    hex(&random_bytes::<16>())
}

fn sha256_hex(s: &str) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, s.as_bytes()).as_ref())
}

/// 固定时间比较
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Access {
    /// 从数据目录读取 clients.json（已授权的电脑）与 access.json（是否可被发现）
    pub fn load(data_dir: &std::path::Path, allow_hosts: Vec<String>, names: &[String]) -> Self {
        let path = data_dir.join("clients.json");
        let settings_path = data_dir.join("access.json");
        let clients = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Vec<Client>>(&b).ok()).unwrap_or_default();
        let settings: serde_json::Value = std::fs::read(&settings_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let discoverable = settings.get("discoverable").and_then(|d| d.as_bool()).unwrap_or(true);
        let open = settings.get("open").and_then(|d| d.as_bool()).unwrap_or(false);
        let mut nm = Vec::new();
        for n in names.iter().cloned().chain(crate::util::hostname()) {
            let n = n.trim().to_ascii_lowercase().replace(' ', "-");
            if !n.is_empty() && !nm.contains(&n) {
                nm.push(format!("{n}.local"));
                nm.push(n);
            }
        }
        Self {
            path,
            names: nm,
            settings_path,
            clients: Mutex::new(clients),
            pending: Mutex::new(HashMap::new()),
            outgoing: Mutex::new(HashMap::new()),
            allow_hosts: allow_hosts.into_iter().map(|h| h.trim().to_ascii_lowercase()).filter(|h| !h.is_empty()).collect(),
            discoverable: AtomicBool::new(discoverable),
            open: AtomicBool::new(open),
            local_ips: Mutex::new((None, Vec::new())),
            seen: Mutex::new(HashMap::new()),
        }
    }

    pub fn is_discoverable(&self) -> bool {
        self.discoverable.load(Ordering::Relaxed)
    }

    pub fn set_discoverable(&self, on: bool) -> Result<(), String> {
        self.discoverable.store(on, Ordering::Relaxed);
        self.save_settings()
    }

    /// 局域网内无需授权即可访问：与启动参数 --open 效果相同，但可在界面上开关，重启后保留
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }

    pub fn set_open(&self, on: bool) -> Result<(), String> {
        self.open.store(on, Ordering::Relaxed);
        self.save_settings()
    }

    fn save_settings(&self) -> Result<(), String> {
        let v = serde_json::json!({ "discoverable": self.is_discoverable(), "open": self.is_open() });
        let data = serde_json::to_vec_pretty(&v).map_err(|e| e.to_string())?;
        std::fs::write(&self.settings_path, data).map_err(|e| e.to_string())
    }

    fn save(&self, list: &[Client]) {
        if let Ok(data) = serde_json::to_vec_pretty(list) {
            if let Some(d) = self.path.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            if let Err(e) = std::fs::write(&self.path, data) {
                tracing::warn!("save clients.json: {e}");
            }
        }
    }

    // ------------------------------------------------------------ 已授权的电脑

    /// 校验令牌；有效时返回对应的授权记录 id，并更新最近使用时间
    pub fn verify(&self, token: &str) -> Option<String> {
        if token.is_empty() {
            return None;
        }
        let h = sha256_hex(token);
        let mut list = self.clients.lock().unwrap();
        let mut found = None;
        for c in list.iter() {
            if ct_eq(c.hash.as_bytes(), h.as_bytes()) {
                found = Some(c.id.clone());
            }
        }
        let id = found?;
        let now = now_secs();
        if let Some(c) = list.iter_mut().find(|c| c.id == id) {
            // 最近使用时间精确到 10 分钟即可，避免每个请求都写文件
            if now - c.last_seen > 600 {
                c.last_seen = now;
                let snapshot = list.clone();
                self.save(&snapshot);
            }
        }
        Some(id)
    }

    /// 授权记录的名称
    pub fn client_name(&self, id: &str) -> Option<String> {
        self.clients.lock().unwrap().iter().find(|c| c.id == id).map(|c| c.name.clone())
    }

    /// 记录一次访问
    pub fn touch(&self, ident: &Ident) {
        let mut m = self.seen.lock().unwrap();
        m.retain(|_, (_, t)| t.elapsed() < Duration::from_secs(600));
        m.insert(ident.key.clone(), (ident.clone(), Instant::now()));
    }

    /// 最近 within 内访问过的连接者，及距今秒数
    pub fn recent(&self, within: Duration) -> Vec<(Ident, u64)> {
        self.seen.lock().unwrap().values().filter(|(_, t)| t.elapsed() < within).map(|(i, t)| (i.clone(), t.elapsed().as_secs())).collect()
    }

    /// 授权一个新的客户端，返回令牌（只在这里出现一次）
    pub fn issue(&self, name: &str, addr: &str) -> String {
        let token = new_token();
        let now = now_secs();
        let c = Client {
            id: new_id()[..12].to_string(),
            name: name.trim().chars().take(64).collect::<String>(),
            addr: addr.to_string(),
            hash: sha256_hex(&token),
            created: now,
            last_seen: now,
        };
        let mut list = self.clients.lock().unwrap();
        // 通过 SSH 重新安装 / 更新时替换同一台电脑原来的授权，不重复累积
        if addr == "SSH" {
            list.retain(|x| !(x.addr == "SSH" && x.name == c.name));
        }
        list.push(c);
        let snapshot = list.clone();
        drop(list);
        self.save(&snapshot);
        token
    }

    pub fn list(&self) -> Vec<ClientPublic> {
        let mut v: Vec<ClientPublic> = self
            .clients
            .lock()
            .unwrap()
            .iter()
            .map(|c| ClientPublic { id: c.id.clone(), name: c.name.clone(), addr: c.addr.clone(), created: c.created, last_seen: c.last_seen })
            .collect();
        v.sort_by_key(|c| std::cmp::Reverse(c.last_seen));
        v
    }

    pub fn revoke(&self, id: &str) -> bool {
        let mut list = self.clients.lock().unwrap();
        let n = list.len();
        list.retain(|c| c.id != id);
        let changed = list.len() != n;
        if changed {
            let snapshot = list.clone();
            drop(list);
            self.save(&snapshot);
        }
        changed
    }

    // ------------------------------------------------------------ 配对请求（对方 → 本机）

    fn prune(m: &mut HashMap<String, Pending>) {
        m.retain(|_, p| p.created.elapsed() < PAIR_TTL);
    }

    /// 新的配对请求，返回 (id, 验证码)。同一 IP 的旧请求被替换。
    pub fn pair_request(&self, name: &str, ip: &str) -> Result<(String, String), String> {
        let mut m = self.pending.lock().unwrap();
        Self::prune(&mut m);
        m.retain(|_, p| !(p.ip == ip && p.state == PairState::Waiting));
        if m.len() >= PAIR_MAX {
            return Err(tr!("连接请求太多，请稍后再试", "Too many connection requests; try again later"));
        }
        let id = new_id();
        let r = random_bytes::<4>();
        let code = format!("{:06}", u32::from_le_bytes(r) % 1_000_000);
        let name: String = name.trim().chars().filter(|c| !c.is_control()).take(64).collect();
        let name = if name.is_empty() { ip.to_string() } else { name };
        m.insert(id.clone(), Pending { name, ip: ip.to_string(), code: code.clone(), created: Instant::now(), state: PairState::Waiting });
        Ok((id, code))
    }

    /// 请求方查询结果：pending / approved（附令牌，只返回一次）/ denied / expired
    pub fn pair_poll(&self, id: &str) -> serde_json::Value {
        let mut m = self.pending.lock().unwrap();
        Self::prune(&mut m);
        match m.get(id).map(|p| p.state.clone()) {
            None => serde_json::json!({"state": "expired"}),
            Some(PairState::Waiting) => serde_json::json!({"state": "pending"}),
            Some(PairState::Denied) => {
                m.remove(id);
                serde_json::json!({"state": "denied"})
            }
            Some(PairState::Approved(token)) => {
                m.remove(id);
                serde_json::json!({"state": "approved", "token": token})
            }
        }
    }

    /// 请求方取消
    pub fn pair_cancel(&self, id: &str) {
        self.pending.lock().unwrap().remove(id);
    }

    /// 等待批准的请求（给已授权的界面显示）
    pub fn pending_list(&self) -> Vec<serde_json::Value> {
        let mut m = self.pending.lock().unwrap();
        Self::prune(&mut m);
        let mut v: Vec<(&String, &Pending)> = m.iter().filter(|(_, p)| p.state == PairState::Waiting).collect();
        v.sort_by_key(|(_, p)| p.created);
        v.into_iter()
            .map(|(id, p)| serde_json::json!({"id": id, "name": p.name, "ip": p.ip, "code": p.code, "age": p.created.elapsed().as_secs()}))
            .collect()
    }

    /// 允许或拒绝
    pub fn pair_decide(&self, id: &str, allow: bool) -> Result<(), String> {
        let mut m = self.pending.lock().unwrap();
        Self::prune(&mut m);
        let Some(p) = m.get_mut(id).filter(|p| p.state == PairState::Waiting) else {
            return Err(tr!("请求已过期或已处理", "The request has expired or was already handled"));
        };
        if allow {
            let (name, ip) = (p.name.clone(), p.ip.clone());
            drop(m);
            let token = self.issue(&name, &ip);
            let mut m = self.pending.lock().unwrap();
            if let Some(p) = m.get_mut(id) {
                p.state = PairState::Approved(token);
                p.created = Instant::now();
            }
        } else {
            p.state = PairState::Denied;
        }
        Ok(())
    }

    // ------------------------------------------------------------ 请求检查

    /// 请求是否来自本机（回环或本机自己的网卡地址）
    pub fn is_local_ip(&self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
            v4 => v4,
        };
        if ip.is_loopback() {
            return true;
        }
        let mut c = self.local_ips.lock().unwrap();
        if c.0.is_none_or(|t| t.elapsed() > Duration::from_secs(10)) {
            let mut v = Vec::new();
            if let Ok(ifs) = if_addrs::get_if_addrs() {
                v.extend(ifs.into_iter().map(|i| i.ip()));
            }
            *c = (Some(Instant::now()), v);
        }
        c.1.contains(&ip)
    }

    fn host_allowed(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        if host == "localhost" || host.ends_with(".localhost") || host.parse::<IpAddr>().is_ok() {
            return true;
        }
        self.allow_hosts.iter().chain(&self.names).any(|h| *h == host)
    }

    /// 免令牌的本机访问还要求 Host 是本机地址或允许的主机名，防止 DNS 重绑定
    pub fn local_host_ok(&self, headers: &HeaderMap) -> bool {
        match headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
            Some(h) => self.host_allowed(&host_only(h)),
            None => true,
        }
    }

    /// 浏览器请求必须来自本服务自己的页面
    pub fn origin_ok(&self, headers: &HeaderMap) -> bool {
        let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).map(|h| h.to_ascii_lowercase());
        if let Some(origin) = headers.get(header::ORIGIN) {
            let Ok(origin) = origin.to_str() else { return false };
            let Some((scheme, authority)) = origin.to_ascii_lowercase().split_once("://").map(|(s, a)| (s.to_string(), a.trim_end_matches('/').to_string())) else {
                return false; // 包括 "null"
            };
            let norm = |a: &str| -> String {
                match scheme.as_str() {
                    "http" => a.strip_suffix(":80").unwrap_or(a).to_string(),
                    "https" => a.strip_suffix(":443").unwrap_or(a).to_string(),
                    _ => a.to_string(),
                }
            };
            if host.as_deref().is_some_and(|h| norm(h) == norm(&authority)) {
                return true;
            }
            // 通过反向代理访问：Origin 是允许的主机名
            return self.allow_hosts.iter().any(|h| *h == host_only(&authority));
        }
        // 没有 Origin：只看 Sec-Fetch-Site（浏览器的 <img>、<script> 等跨站 GET 会带上它）
        !matches!(headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()), Some("cross-site") | Some("same-site"))
    }
}

/// "host:port" / "[::1]:port" → host
fn host_only(h: &str) -> String {
    let h = h.trim();
    if let Some(rest) = h.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("").to_string();
    }
    match h.rsplit_once(':') {
        Some((a, p)) if !a.contains(':') && p.chars().all(|c| c.is_ascii_digit()) => a.to_string(),
        _ => h.to_string(),
    }
    .to_ascii_lowercase()
}

/// 请求中携带的令牌：?token=、X-Token、Authorization: Bearer
pub fn token_from(headers: &HeaderMap, q: &HashMap<String, String>) -> Option<String> {
    q.get("token")
        .cloned()
        .or_else(|| headers.get("x-token").and_then(|v| v.to_str().ok()).map(String::from))
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(|v| v.trim().to_string())
        })
        .filter(|t| !t.is_empty())
}

/// 与启动参数 --token 比较（固定时间）
pub fn token_matches(given: &str, expected: &str) -> bool {
    ct_eq(sha256_hex(given).as_bytes(), sha256_hex(expected).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acc(allow: &[&str]) -> Access {
        let d = std::env::temp_dir().join(format!("droidbridge-access-{}", new_id()));
        std::fs::create_dir_all(&d).unwrap();
        Access::load(&d, allow.iter().map(|s| s.to_string()).collect(), &["My PC".to_string()])
    }

    fn hm(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
        }
        h
    }

    #[test]
    fn origin_rules() {
        let a = acc(&["phone.example.com"]);
        // 同源
        assert!(a.origin_ok(&hm(&[("host", "127.0.0.1:8730"), ("origin", "http://127.0.0.1:8730")])));
        assert!(a.origin_ok(&hm(&[("host", "192.168.1.5:8731"), ("origin", "https://192.168.1.5:8731")])));
        assert!(a.origin_ok(&hm(&[("host", "example.org"), ("origin", "https://example.org:443")])));
        // 非浏览器（MCP 客户端、其他节点、curl）
        assert!(a.origin_ok(&hm(&[("host", "127.0.0.1:8730")])));
        assert!(a.origin_ok(&hm(&[("host", "127.0.0.1:8730"), ("sec-fetch-site", "same-origin")])));
        // 其他网站
        assert!(!a.origin_ok(&hm(&[("host", "127.0.0.1:8730"), ("origin", "https://evil.com")])));
        assert!(!a.origin_ok(&hm(&[("host", "127.0.0.1:8730"), ("origin", "http://127.0.0.1:3000")])));
        assert!(!a.origin_ok(&hm(&[("host", "127.0.0.1:8730"), ("origin", "null")])));
        assert!(!a.origin_ok(&hm(&[("host", "127.0.0.1:8730"), ("sec-fetch-site", "cross-site")])));
        assert!(!a.origin_ok(&hm(&[("host", "127.0.0.1:8730"), ("sec-fetch-site", "same-site")])));
        // 反向代理
        assert!(a.origin_ok(&hm(&[("host", "127.0.0.1:8730"), ("origin", "https://phone.example.com")])));
    }

    #[test]
    fn host_rules() {
        let a = acc(&["phone.example.com"]);
        assert!(a.local_host_ok(&hm(&[("host", "127.0.0.1:8730")])));
        assert!(a.local_host_ok(&hm(&[("host", "localhost:8730")])));
        assert!(a.local_host_ok(&hm(&[("host", "[::1]:8730")])));
        assert!(a.local_host_ok(&hm(&[("host", "192.168.1.5:8730")])));
        assert!(a.local_host_ok(&hm(&[("host", "phone.example.com")])));
        assert!(a.local_host_ok(&hm(&[("host", "my-pc.local:8730")])));
        // DNS 重绑定：攻击者的域名解析到本机
        assert!(!a.local_host_ok(&hm(&[("host", "evil.com:8730")])));
        assert!(!a.local_host_ok(&hm(&[("host", "127.0.0.1.evil.com")])));
    }

    #[test]
    fn issue_verify_revoke() {
        let a = acc(&[]);
        let t = a.issue("A", "192.168.1.7");
        let id = a.verify(&t).expect("valid");
        assert!(a.verify("wrong").is_none());
        assert!(a.verify("").is_none());
        // 重新加载后仍有效（只保存摘要）
        let b = Access::load(a.path.parent().unwrap(), vec![], &[]);
        assert_eq!(b.verify(&t).as_deref(), Some(id.as_str()));
        assert!(!std::fs::read_to_string(&a.path).unwrap().contains(&t));
        assert!(b.revoke(&id));
        assert!(b.verify(&t).is_none());
        let _ = std::fs::remove_dir_all(a.path.parent().unwrap());
    }

    #[test]
    fn pairing_flow() {
        let a = acc(&[]);
        let (id, code) = a.pair_request("Mac", "192.168.1.7").unwrap();
        assert_eq!(code.len(), 6);
        assert_eq!(a.pair_poll(&id)["state"], "pending");
        assert_eq!(a.pending_list().len(), 1);
        a.pair_decide(&id, true).unwrap();
        let r = a.pair_poll(&id);
        assert_eq!(r["state"], "approved");
        let token = r["token"].as_str().unwrap();
        assert!(a.verify(token).is_some());
        // 令牌只返回一次
        assert_eq!(a.pair_poll(&id)["state"], "expired");

        let (id2, _) = a.pair_request("PC", "192.168.1.8").unwrap();
        a.pair_decide(&id2, false).unwrap();
        assert_eq!(a.pair_poll(&id2)["state"], "denied");
        assert!(a.pair_decide(&id2, true).is_err());

        // 同一 IP 的新请求替换旧请求
        let (x, _) = a.pair_request("PC", "192.168.1.9").unwrap();
        let (y, _) = a.pair_request("PC", "192.168.1.9").unwrap();
        assert_eq!(a.pair_poll(&x)["state"], "expired");
        assert_eq!(a.pair_poll(&y)["state"], "pending");
        let _ = std::fs::remove_dir_all(a.path.parent().unwrap());
    }

    #[test]
    fn discoverable_persists() {
        let a = acc(&[]);
        assert!(a.is_discoverable());
        a.set_discoverable(false).unwrap();
        let b = Access::load(a.path.parent().unwrap(), vec![], &[]);
        assert!(!b.is_discoverable());
        assert!(!b.is_open());
        b.set_open(true).unwrap();
        let c = Access::load(a.path.parent().unwrap(), vec![], &[]);
        assert!(c.is_open() && !c.is_discoverable());
        let _ = std::fs::remove_dir_all(a.path.parent().unwrap());
    }

    #[test]
    fn global_token_compare() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abc", "abd"));
        assert!(!token_matches("", "abc"));
    }
}
