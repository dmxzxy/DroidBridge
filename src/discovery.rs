//! 局域网自动发现：各节点通过 UDP 广播（端口 8730）互相宣告。
//!
//! 报文（JSON）：{"app":"droidbridge","v":1,"id":实例,"name":..,"port":HTTP端口,"auth":bool,"version":..,"os":..,"q":是否请求回复}
//! 每 5 秒向各网卡的子网广播地址与 255.255.255.255 宣告一次；收到 q=true 时单播回复。
//! 超过 16 秒未收到宣告的节点视为离线。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

pub const PORT: u16 = 8730;
const INTERVAL: Duration = Duration::from_secs(5);
const EXPIRE: Duration = Duration::from_secs(16);

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Announce {
    app: String,
    #[serde(default)]
    v: u32,
    id: String,
    name: String,
    port: u16,
    #[serde(default)]
    auth: bool,
    #[serde(default)]
    version: String,
    #[serde(default)]
    os: String,
    #[serde(default)]
    q: bool,
}

#[derive(Serialize, Clone)]
pub struct Found {
    pub name: String,
    /// ip:port
    pub addr: String,
    pub auth: bool,
    pub version: String,
    pub os: String,
}

pub struct Discovery {
    pub id: String,
    found: Mutex<HashMap<String, (Found, Instant)>>,
}

impl Discovery {
    pub fn new() -> Self {
        Self { id: format!("{:08x}{:08x}", crate::util::random_scid(), crate::util::random_scid()), found: Mutex::new(HashMap::new()) }
    }

    pub fn list(&self) -> Vec<Found> {
        let mut m = self.found.lock().unwrap();
        m.retain(|_, (_, t)| t.elapsed() < EXPIRE);
        let mut v: Vec<Found> = m.values().map(|(f, _)| f.clone()).collect();
        v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.addr.cmp(&b.addr)));
        v
    }
}

impl Default for Discovery {
    fn default() -> Self {
        Self::new()
    }
}

/// 绑定可复用的 UDP 端口（同一台电脑上的多个实例都能收到广播）
fn bind() -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(all(unix, not(target_os = "solaris")))]
    let _ = s.set_reuse_port(true);
    s.set_broadcast(true)?;
    s.set_nonblocking(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    UdpSocket::from_std(s.into())
}

fn broadcast_targets() -> Vec<SocketAddr> {
    let mut v = vec![SocketAddr::from((Ipv4Addr::BROADCAST, PORT))];
    if let Ok(ifs) = if_addrs::get_if_addrs() {
        for i in ifs {
            if let if_addrs::IfAddr::V4(a) = i.addr {
                if a.ip.is_loopback() || a.ip.is_link_local() {
                    continue;
                }
                let b = a.broadcast.unwrap_or_else(|| {
                    let ip = u32::from(a.ip);
                    let mask = u32::from(a.netmask);
                    Ipv4Addr::from(ip | !mask)
                });
                let t = SocketAddr::from((b, PORT));
                if !v.contains(&t) {
                    v.push(t);
                }
            }
        }
    }
    v
}

/// 后台运行发现服务；http_port 为本节点实际的 HTTP 端口
pub async fn run(st: std::sync::Arc<crate::AppState>, http_port: u16) {
    let sock = match bind() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("LAN discovery disabled: cannot bind UDP {PORT}: {e}");
            return;
        }
    };
    let me = |q: bool| Announce {
        app: "droidbridge".into(),
        v: 1,
        id: st.discovery.id.clone(),
        name: st.cfg.name.clone(),
        port: http_port,
        auth: st.cfg.token.is_some(),
        version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        q,
    };
    let mut tick = tokio::time::interval(INTERVAL);
    let mut first = true;
    let mut buf = vec![0u8; 2048];
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let msg = serde_json::to_vec(&me(first)).unwrap_or_default();
                first = false;
                for t in broadcast_targets() {
                    let _ = sock.send_to(&msg, t).await;
                }
            }
            r = sock.recv_from(&mut buf) => {
                let Ok((n, from)) = r else { continue };
                let Ok(a) = serde_json::from_slice::<Announce>(&buf[..n]) else { continue };
                if a.app != "droidbridge" || a.id == st.discovery.id || a.port == 0 {
                    continue;
                }
                let IpAddr::V4(ip) = from.ip() else { continue };
                if a.q {
                    if let Ok(msg) = serde_json::to_vec(&me(false)) {
                        let _ = sock.send_to(&msg, from).await;
                    }
                }
                let addr = format!("{ip}:{}", a.port);
                let f = Found { name: a.name, addr: addr.clone(), auth: a.auth, version: a.version, os: a.os };
                st.discovery.found.lock().unwrap().insert(a.id, (f, Instant::now()));
            }
        }
    }
}
