//! 局域网自动发现：各节点通过 UDP 广播（端口 8730）互相宣告。
//!
//! 报文（JSON）：{"app":"droidbridge","v":2,"id":实例,"name":..,"port":HTTP端口,"auth":bool,"version":..,"os":..,"q":是否请求回复}
//! 每 2 秒向各网卡的子网广播地址与 255.255.255.255 宣告一次；收到 q=true 时单播回复。
//! 超过 7 秒未收到宣告的节点视为离线（v=1 的旧版本每 5 秒宣告一次，按 16 秒算）。
//! 打开“添加远程电脑”时会向局域网询问一遍谁在线（一秒内最多一次）；询问过后列表只留询问前后有回应的节点，
//! 所以突然关机的电脑不会停到超时才消失。
//! 关闭“允许被局域网发现”后只接收别人的宣告，自己不再广播、询问或回复。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

pub const PORT: u16 = 8730;
const INTERVAL: Duration = Duration::from_secs(2);
const EXPIRE: Duration = Duration::from_secs(7);
/// v=1 的旧版本每 5 秒才宣告一次，留给它更长的超时和更多的余地
const EXPIRE_V1: Duration = Duration::from_secs(16);
/// 两次主动询问之间的最短间隔
const QUERY_RATE: Duration = Duration::from_secs(1);
/// 询问发出后这么久之内还不拿它筛选：回应可能还在路上
const ANSWER_WAIT: Duration = Duration::from_secs(1);
/// 询问前这么久以内宣告过的节点也算有回应：它可能没收到询问（旧版本 5 秒才宣告一次，余地更大）
const ANSWER_GRACE: Duration = Duration::from_millis(1500);
const ANSWER_GRACE_V1: Duration = Duration::from_millis(4500);

fn ttl(v: u32) -> Duration {
    if v >= 2 {
        EXPIRE
    } else {
        EXPIRE_V1
    }
}

fn grace(v: u32) -> Duration {
    if v >= 2 {
        ANSWER_GRACE
    } else {
        ANSWER_GRACE_V1
    }
}

/// 节点是否还在线：最后听到它的时间没有超过超时；询问过局域网的话，还要求它在询问前后有过回应
fn online(t: Instant, v: u32, asked: Option<Instant>) -> bool {
    if t.elapsed() >= ttl(v) {
        return false;
    }
    asked.and_then(|q| q.checked_sub(grace(v))).map_or(true, |s| t >= s)
}

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
    /// 支持授权请求（0.9 起）
    #[serde(default)]
    pairing: bool,
}

#[derive(Serialize, Clone)]
pub struct Found {
    pub name: String,
    /// ip:port
    pub addr: String,
    pub auth: bool,
    pub version: String,
    pub os: String,
    pub pairing: bool,
}

pub struct Discovery {
    pub id: String,
    /// id -> (节点, 最后一次听到它的时间, 它的宣告版本)
    found: Mutex<HashMap<String, (Found, Instant, u32)>>,
    /// 最近两次主动询问的时间
    queries: Mutex<Vec<Instant>>,
    /// 请求后台询问一次；通道里最多排一个
    probe_tx: mpsc::Sender<()>,
    probe_rx: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Discovery {
    pub fn new() -> Self {
        let (probe_tx, probe_rx) = mpsc::channel(1);
        Self {
            id: format!("{:08x}{:08x}", crate::util::random_scid(), crate::util::random_scid()),
            found: Mutex::new(HashMap::new()),
            queries: Mutex::new(Vec::new()),
            probe_tx,
            probe_rx: Mutex::new(Some(probe_rx)),
        }
    }

    /// 请后台向局域网询问一遍谁在线（见 [`Discovery::list`]）
    pub fn probe(&self) {
        let _ = self.probe_tx.try_send(());
    }

    /// 发现的节点。向局域网询问过后只列出询问前后有回应的，所以突然关机的电脑不会停到超时才消失
    pub fn list(&self) -> Vec<Found> {
        // 用最近一次已经等过回应的询问来筛选
        let asked = self.queries.lock().unwrap().iter().rev().find(|t| t.elapsed() >= ANSWER_WAIT).copied();
        let mut m = self.found.lock().unwrap();
        m.retain(|_, (_, t, v)| online(*t, *v, asked));
        let mut v: Vec<Found> = m.values().map(|(f, _, _)| f.clone()).collect();
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

/// 向各广播地址发送一条报文
async fn send_all(sock: &UdpSocket, msg: &[u8]) {
    for t in broadcast_targets() {
        let _ = sock.send_to(msg, t).await;
    }
}

/// 后台运行发现服务；http_port 为本节点实际的 HTTP 端口
pub async fn run(st: std::sync::Arc<crate::AppState>, http_port: u16) {
    let Some(mut probes) = st.discovery.probe_rx.lock().unwrap().take() else { return };
    let sock = match bind() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("LAN discovery disabled: cannot bind UDP {PORT}: {e}");
            return;
        }
    };
    let me = |q: bool| Announce {
        app: "droidbridge".into(),
        v: 2,
        id: st.discovery.id.clone(),
        name: st.cfg.name.clone(),
        port: http_port,
        auth: !crate::is_open(&st) || st.cfg.token.is_some(),
        version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        q,
        pairing: true,
    };
    let mut tick = tokio::time::interval(INTERVAL);
    let mut first = true;
    let mut last_query: Option<Instant> = None;
    let mut buf = vec![0u8; 2048];
    loop {
        tokio::select! {
            _ = tick.tick() => {
                // 不允许被发现：只听别人的宣告，自己不发
                if !st.access.is_discoverable() {
                    first = true;
                    continue;
                }
                let msg = serde_json::to_vec(&me(first)).unwrap_or_default();
                first = false;
                send_all(&sock, &msg).await;
            }
            // 有人读取发现列表：问一遍谁在线，在线的电脑立即回应。
            // 询问本身就是一次宣告，所以不允许被发现的电脑只听不问
            _ = probes.recv() => {
                if !last_query.map_or(true, |t| t.elapsed() >= QUERY_RATE) {
                    continue;
                }
                last_query = Some(Instant::now());
                if !st.access.is_discoverable() {
                    continue;
                }
                let msg = serde_json::to_vec(&me(true)).unwrap_or_default();
                {
                    let mut q = st.discovery.queries.lock().unwrap();
                    q.push(Instant::now());
                    if q.len() > 2 {
                        q.remove(0);
                    }
                }
                send_all(&sock, &msg).await;
            }
            r = sock.recv_from(&mut buf) => {
                let Ok((n, from)) = r else { continue };
                let Ok(a) = serde_json::from_slice::<Announce>(&buf[..n]) else { continue };
                if a.app != "droidbridge" || a.id == st.discovery.id || a.port == 0 {
                    continue;
                }
                let IpAddr::V4(ip) = from.ip() else { continue };
                if a.q && st.access.is_discoverable() {
                    if let Ok(msg) = serde_json::to_vec(&me(false)) {
                        let _ = sock.send_to(&msg, from).await;
                    }
                }
                let addr = format!("{ip}:{}", a.port);
                let f = Found { name: a.name, addr: addr.clone(), auth: a.auth, version: a.version, os: a.os, pairing: a.pairing };
                st.discovery.found.lock().unwrap().insert(a.id, (f, Instant::now(), a.v));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一台 ago 之前最后宣告过的电脑；v 是它的宣告版本
    fn put(d: &Discovery, id: &str, name: &str, ago: Duration, v: u32) {
        let f = Found { name: name.into(), addr: "192.168.1.20:8730".into(), auth: false, version: String::new(), os: String::new(), pairing: false };
        d.found.lock().unwrap().insert(id.into(), (f, Instant::now() - ago, v));
    }
    fn names(d: &Discovery) -> Vec<String> {
        d.list().into_iter().map(|f| f.name).collect()
    }
    /// ago 之前向局域网询问过一次
    fn asked(d: &Discovery, ago: Duration) {
        d.queries.lock().unwrap().push(Instant::now() - ago);
    }

    #[test]
    fn nodes_that_stop_announcing_expire() {
        let d = Discovery::new();
        put(&d, "a", "new", Duration::from_secs(5), 2);
        put(&d, "b", "gone", Duration::from_secs(9), 2);
        // 旧版本宣告得慢，超时也更长
        put(&d, "c", "old", Duration::from_secs(12), 1);
        put(&d, "d", "old and gone", Duration::from_secs(20), 1);
        assert_eq!(names(&d), ["new", "old"]);
    }

    #[test]
    fn after_a_query_only_nodes_that_answered_stay() {
        let d = Discovery::new();
        put(&d, "a", "off", Duration::from_secs(4), 2);
        put(&d, "b", "on", Duration::from_secs(1), 2);
        // 还没询问过：按超时算，已经关机的也还在
        assert_eq!(names(&d), ["off", "on"]);
        asked(&d, Duration::from_secs(2));
        assert_eq!(names(&d), ["on"]);
    }

    #[test]
    fn announcing_just_before_the_query_counts_as_answering() {
        let d = Discovery::new();
        asked(&d, Duration::from_secs(2));
        // 询问前 2.5 秒内还宣告过的（可能没收到询问）留下，更早的去掉
        put(&d, "a", "kept", Duration::from_millis(2500), 2);
        put(&d, "b", "dropped", Duration::from_secs(5), 2);
        assert_eq!(names(&d), ["kept"]);
    }

    #[test]
    fn old_peers_get_a_longer_grace() {
        let d = Discovery::new();
        asked(&d, Duration::from_secs(2));
        // 旧版本 5 秒才宣告一次：询问前 4 秒还宣告过的也算在线，更早的去掉
        put(&d, "a", "old", Duration::from_secs(4), 1);
        put(&d, "b", "old and gone", Duration::from_secs(8), 1);
        assert_eq!(names(&d), ["old"]);
    }

    #[test]
    fn a_query_younger_than_the_answer_wait_does_not_filter() {
        let d = Discovery::new();
        put(&d, "a", "off", Duration::from_secs(4), 2);
        asked(&d, Duration::from_millis(300));
        assert_eq!(names(&d), ["off"]);
    }
}
