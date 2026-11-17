//! 无线 adb：IP 直连、配对码配对、二维码配对（Android 11+）、USB 设备切换为无线，
//! 以及后台自动重连（保存过的地址与 mDNS 发现的已配对设备）。

use crate::{adb, util, AppState};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct Wireless {
    path: PathBuf,
    saved: Mutex<Vec<String>>,
    tried: Mutex<HashMap<String, Instant>>,
    pub qr: Mutex<HashMap<String, QrJob>>,
}

#[derive(Clone, serde::Serialize)]
pub struct QrJob {
    /// waiting / pairing / connecting / done / failed
    pub state: String,
    pub msg: String,
    pub serial: Option<String>,
    /// 已配对但没找到连接端口时为手机 IP，前端据此切到「IP 地址」页
    #[serde(skip_serializing_if = "Option::is_none")]
    pub need_connect: Option<String>,
}

impl Wireless {
    pub fn load(path: PathBuf) -> Self {
        let saved = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { path, saved: Mutex::new(saved), tried: Mutex::new(HashMap::new()), qr: Mutex::new(HashMap::new()) }
    }
    fn save(&self, v: &[String]) {
        if let Ok(d) = serde_json::to_vec_pretty(v) {
            let _ = std::fs::write(&self.path, d);
        }
    }
    fn remember(&self, addr: &str) {
        let mut v = self.saved.lock().unwrap();
        if !v.iter().any(|a| a == addr) {
            v.push(addr.to_string());
            self.save(&v);
        }
    }
    fn forget(&self, addr: &str) {
        let mut v = self.saved.lock().unwrap();
        v.retain(|a| a != addr);
        self.save(&v);
    }
}

/// host:port 校验（防止参数注入 adb）
pub fn valid_endpoint(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty() && !s.starts_with('-') && s.len() < 200 && s.chars().all(|c| c.is_ascii_alphanumeric() || ".:-_[]%".contains(c))
}

fn normalize(addr: &str, default_port: u16) -> Result<String, String> {
    let a = addr.trim();
    if !valid_endpoint(a) {
        return Err(tr!("地址格式不正确", "Invalid address"));
    }
    let has_port = if a.starts_with('[') { a.contains("]:") } else { a.matches(':').count() == 1 };
    Ok(if has_port { a.to_string() } else { format!("{a}:{default_port}") })
}

/// adb 的 connect / pair 即使失败也可能返回 0，需要看输出
fn adb_ok(out: &str) -> bool {
    let o = out.to_ascii_lowercase();
    (o.contains("connected to") || o.contains("successfully paired")) && !o.contains("failed") && !o.contains("cannot")
}

async fn connect_addr(adb_path: &Path, addr: &str) -> Result<String, String> {
    connect_addr_timeout(adb_path, addr, Duration::from_secs(10)).await
}

async fn connect_addr_timeout(adb_path: &Path, addr: &str, timeout: Duration) -> Result<String, String> {
    let out = adb::run_timeout(adb_path, &["connect", addr], timeout).await.unwrap_or_else(|e| e);
    if adb_ok(&out) {
        Ok(addr.to_string())
    } else {
        let msg = out.trim().to_string();
        Err(tr!("连接 {addr} 失败：{msg}", "Failed to connect to {addr}: {msg}"))
    }
}

#[derive(Deserialize)]
pub struct ConnectReq {
    pub addr: String,
}

pub async fn connect(st: &AppState, r: ConnectReq) -> Result<serde_json::Value, String> {
    let addr = normalize(&r.addr, 5555)?;
    let serial = connect_addr(&st.cfg.adb, &addr).await?;
    st.wireless.remember(&addr);
    Ok(json!({"ok": true, "serial": serial, "msg": tr!("已连接 {serial}", "Connected {serial}")}))
}

#[derive(Deserialize)]
pub struct PairReq {
    pub addr: String,
    pub code: String,
    /// 配对后要连接的地址（无线调试页面上显示的 IP 地址和端口），可选
    #[serde(default)]
    pub connect: String,
}

pub async fn pair(st: &AppState, r: PairReq) -> Result<serde_json::Value, String> {
    let addr = normalize(&r.addr, 0)?;
    let code = r.code.trim();
    if code.is_empty() || code.len() > 16 || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(tr!("配对码格式不正确", "Invalid pairing code"));
    }
    let out = adb::run_timeout(&st.cfg.adb, &["pair", &addr, code], Duration::from_secs(20)).await.unwrap_or_else(|e| e);
    if !adb_ok(&out) {
        let msg = out.trim().to_string();
        return Err(tr!("配对失败：{msg}", "Pairing failed: {msg}"));
    }
    if !r.connect.trim().is_empty() {
        let c = normalize(&r.connect, 5555)?;
        connect_addr(&st.cfg.adb, &c).await?;
        st.wireless.remember(&c);
        return Ok(json!({"ok": true, "serial": c, "msg": tr!("配对成功并已连接 {c}", "Paired and connected {c}")}));
    }
    let host = addr.rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or_default();
    let pair_port = addr.rsplit_once(':').and_then(|(_, p)| p.parse().ok()).unwrap_or(0);
    match find_and_connect(st, &host, pair_guid(&out).as_deref(), pair_port).await {
        Some(c) => Ok(json!({"ok": true, "serial": c, "msg": tr!("配对成功并已连接 {c}", "Paired and connected {c}")})),
        // 配对已成功但没找到连接端口：告诉前端让用户填写无线调试页面上的「IP 地址和端口」，不再当作成功关掉对话框
        None => Ok(json!({
            "ok": true,
            "need_connect": true,
            "host": host,
            "msg": tr!(
                "配对已成功，但没有自动找到连接端口。请把手机「无线调试」页面顶部显示的「IP 地址和端口」填到下面进行连接（无需再次配对）",
                "Paired, but the connection port was not found automatically. Enter the \"IP address & Port\" shown at the top of the phone's Wireless debugging page below (no need to pair again)"
            )
        })),
    }
}

/// `adb pair` 成功输出中的 guid：Successfully paired to 1.2.3.4:37000 [guid=adb-SERIAL-xxxx]
/// 它等于手机 _adb-tls-connect 服务的实例名
fn pair_guid(out: &str) -> Option<String> {
    let s = out.split("guid=").nth(1)?;
    let g: String = s.chars().take_while(|c| !matches!(c, ']' | ' ' | '\r' | '\n')).collect();
    (!g.is_empty()).then_some(g)
}

/// 配对成功后找到手机的连接端口并连接：
/// 1. mDNS（adb 自带 + 内置查询）按 guid 或 IP 匹配 _adb-tls-connect 服务；
/// 2. adb 可能已自动连上；
/// 3. 都没有时扫描手机的动态端口范围，对开放的端口尝试 adb connect（mDNS 被路由器/防火墙拦截时仍可用）。
async fn find_and_connect(st: &AppState, host: &str, guid: Option<&str>, pair_port: u16) -> Option<String> {
    let a = &st.cfg.adb;
    let host_prefix = format!("{host}:");
    for _ in 0..4 {
        for s in discover(a).await {
            let by_guid = guid.is_some_and(|g| s.name.eq_ignore_ascii_case(g));
            if s.kind.contains("adb-tls-connect") && (by_guid || s.addr.starts_with(&host_prefix)) && connect_addr(a, &s.addr).await.is_ok() {
                st.wireless.remember(&s.addr);
                return Some(s.addr);
            }
        }
        if let Ok(list) = adb::list_devices(a).await {
            if let Some(d) = list.iter().find(|d| {
                d.state == "device" && (d.serial.starts_with(&host_prefix) || guid.is_some_and(|g| d.serial.to_ascii_lowercase().starts_with(&g.to_ascii_lowercase())))
            }) {
                return Some(d.serial.clone());
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let ip: std::net::Ipv4Addr = host.parse().ok()?;
    tracing::info!("wireless: mDNS did not find the connect port of {host}; scanning ports");
    for port in scan_ports(ip, pair_port).await.into_iter().take(8) {
        let c = format!("{ip}:{port}");
        if connect_addr_timeout(a, &c, Duration::from_secs(6)).await.is_ok() {
            st.wireless.remember(&c);
            return Some(c);
        }
    }
    None
}

/// 扫描手机上开放的 TCP 端口（Android 无线调试的连接端口是系统动态端口 32768–65535）
async fn scan_ports(ip: std::net::Ipv4Addr, skip: u16) -> Vec<u16> {
    use futures_util::StreamExt;
    let mut open: Vec<u16> = futures_util::stream::iter((32768u16..=65535).filter(|p| *p != skip))
        .map(|port| async move {
            let addr = std::net::SocketAddr::from((ip, port));
            matches!(tokio::time::timeout(Duration::from_millis(800), tokio::net::TcpStream::connect(addr)).await, Ok(Ok(_))).then_some(port)
        })
        .buffer_unordered(2048)
        .filter_map(|x| async move { x })
        .collect()
        .await;
    open.sort();
    open
}

#[derive(Deserialize)]
pub struct SerialReq {
    pub serial: String,
}

pub async fn disconnect(st: &AppState, r: SerialReq) -> Result<serde_json::Value, String> {
    if !valid_endpoint(&r.serial) {
        return Err(tr!("设备序列号不正确", "Invalid serial"));
    }
    st.wireless.forget(&r.serial);
    adb::run_timeout(&st.cfg.adb, &["disconnect", &r.serial], Duration::from_secs(8)).await?;
    Ok(json!({"ok": true, "msg": tr!("已断开 {}", "Disconnected {}", r.serial)}))
}

/// USB 设备切换为无线：读取 Wi-Fi IP，adb tcpip 5555，然后连接
pub async fn tcpip(st: &AppState, r: SerialReq) -> Result<serde_json::Value, String> {
    let serial = r.serial.trim();
    if !valid_endpoint(serial) {
        return Err(tr!("设备序列号不正确", "Invalid serial"));
    }
    let a = &st.cfg.adb;
    let ip = device_ip(a, serial).await.ok_or_else(|| tr!("没有获取到设备的 Wi-Fi IP，请确认手机已连接 Wi-Fi", "Cannot get the device Wi-Fi IP; make sure it is on Wi-Fi"))?;
    adb::run_timeout(a, &["-s", serial, "tcpip", "5555"], Duration::from_secs(10)).await?;
    let addr = format!("{ip}:5555");
    let mut last = String::new();
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        match connect_addr(a, &addr).await {
            Ok(_) => {
                st.wireless.remember(&addr);
                return Ok(json!({"ok": true, "serial": addr, "msg": tr!("已切换为无线连接 {addr}，可以拔掉数据线了", "Switched to wireless {addr}; you can unplug the cable")}));
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

async fn device_ip(a: &Path, serial: &str) -> Option<String> {
    let parse_inet = |o: &str| {
        o.split_whitespace()
            .skip_while(|w| *w != "inet")
            .nth(1)
            .and_then(|w| w.split('/').next())
            .filter(|ip| ip.parse::<std::net::Ipv4Addr>().is_ok())
            .map(String::from)
    };
    for iface in ["wlan0", "wlan1", "eth0"] {
        if let Ok(o) = adb::run_timeout(a, &["-s", serial, "shell", &format!("ip -f inet addr show {iface}")], Duration::from_secs(5)).await {
            if let Some(ip) = parse_inet(&o) {
                return Some(ip);
            }
        }
    }
    let o = adb::run_timeout(a, &["-s", serial, "shell", "ip route"], Duration::from_secs(5)).await.ok()?;
    o.lines().find(|l| l.contains("wlan")).and_then(|l| {
        let w: Vec<&str> = l.split_whitespace().collect();
        w.iter().position(|x| *x == "src").and_then(|i| w.get(i + 1)).map(|s| s.to_string())
    })
}

// ---------------------------------------------------------------- mDNS / 二维码

pub struct MdnsService {
    pub name: String,
    pub kind: String,
    pub addr: String,
}

pub async fn mdns(a: &Path) -> Vec<MdnsService> {
    let Ok(o) = adb::run_timeout(a, &["mdns", "services"], Duration::from_secs(5)).await else { return Vec::new() };
    o.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 3 && f[1].contains("._tcp")).then(|| MdnsService { name: f[0].into(), kind: f[1].trim_end_matches('.').into(), addr: f[f.len() - 1].into() })
        })
        .collect()
}

/// 发现手机的无线调试服务：adb 自带的 mDNS 与内置 mDNS 查询同时进行、合并结果。
/// adb 的 mDNS 常因防火墙未放行 adb.exe、旧版 adb 占用 5037 端口、多网卡等原因拿不到结果。
pub async fn discover(a: &Path) -> Vec<MdnsService> {
    let (mut v, own) = tokio::join!(mdns(a), crate::mdns::browse(&[crate::mdns::PAIRING, crate::mdns::CONNECT], Duration::from_millis(1500)));
    for s in own {
        if !v.iter().any(|x| x.name.eq_ignore_ascii_case(&s.name) && x.kind == s.kind && x.addr == s.addr) {
            v.push(MdnsService { name: s.name, kind: s.kind, addr: s.addr });
        }
    }
    v
}

/// 开始二维码配对：返回任务 id 与二维码 SVG。手机在“无线调试 → 使用二维码配对设备”中扫码后，
/// 会以 mDNS 服务（名称 = 二维码中的 S）公布配对端口，这里轮询发现后自动配对并连接。
pub fn qr_start(st: Arc<AppState>) -> Result<serde_json::Value, String> {
    let name = format!("DroidBridge-{:06x}", util::random_scid() & 0xff_ffff);
    let password = format!("{:06}", util::random_scid() % 1_000_000);
    let text = format!("WIFI:T:ADB;S:{name};P:{password};;");
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(|e| e.to_string())?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(220, 220)
        .dark_color(qrcode::render::svg::Color("#000000"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build();
    let id = format!("q{:08x}", util::random_scid());
    st.wireless.qr.lock().unwrap().insert(
        id.clone(),
        QrJob { state: "waiting".into(), msg: tr!("等待手机扫码…", "Waiting for the phone to scan…"), serial: None, need_connect: None },
    );
    let shown = name.clone();
    let en = crate::i18n::is_en();
    let job = id.clone();
    tokio::spawn(crate::i18n::REQUEST_EN.scope(en, async move {
        let set = |state: &str, msg: String, serial: Option<String>| {
            if let Some(j) = st.wireless.qr.lock().unwrap().get_mut(&job) {
                j.state = state.into();
                j.msg = msg;
                j.serial = serial;
            }
        };
        let a = st.cfg.adb.clone();
        let started = Instant::now();
        let mut hinted = false;
        let pair_addr = loop {
            if started.elapsed() > Duration::from_secs(150) {
                set("failed", tr!("超时：没有发现扫码的手机。请确认手机与这台电脑在同一网络（同一网段、未开启访客/AP 隔离），或改用配对码", "Timed out waiting for the phone. Make sure it is on the same network and subnet (no guest/AP isolation), or use a pairing code"), None);
                return;
            }
            if !st.wireless.qr.lock().unwrap().contains_key(&job) {
                return;
            }
            if !hinted && started.elapsed() > Duration::from_secs(30) {
                hinted = true;
                set("waiting", tr!(
                    "等待手机扫码… 如果手机已扫码但一直没有反应，通常是手机与电脑不在同一网段（或路由器拦截了组播），请改用「配对码」",
                    "Waiting for the phone… If it has scanned but nothing happens, the phone is probably on a different subnet (or the router blocks multicast); use a pairing code instead"
                ), None);
            }
            if let Some(s) = discover(&a).await.into_iter().find(|s| s.name == name && s.kind.contains("adb-tls-pairing")) {
                break s.addr;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        set("pairing", tr!("正在配对 {pair_addr}…", "Pairing {pair_addr}…"), None);
        let out = match adb::run_timeout(&a, &["pair", &pair_addr, &password], Duration::from_secs(20)).await {
            Ok(o) if adb_ok(&o) => o,
            Ok(o) | Err(o) => {
                let o = o.trim().to_string();
                set("failed", tr!("配对失败：{o}", "Pairing failed: {o}"), None);
                return;
            }
        };
        set("connecting", tr!("配对成功，正在连接…", "Paired; connecting…"), None);
        let host = pair_addr.rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or_default();
        let pair_port = pair_addr.rsplit_once(':').and_then(|(_, p)| p.parse().ok()).unwrap_or(0);
        match find_and_connect(&st, &host, pair_guid(&out).as_deref(), pair_port).await {
            Some(c) => set("done", tr!("已连接 {c}", "Connected {c}"), Some(c)),
            None => {
                set(
                    "failed",
                    tr!(
                        "配对已成功，但没有找到连接端口。请填写手机「无线调试」页面顶部显示的「IP 地址和端口」进行连接（无需再次配对）",
                        "Paired, but the connection port was not found. Enter the \"IP address & Port\" shown at the top of the phone's Wireless debugging page (no need to pair again)"
                    ),
                    None,
                );
                if let Some(j) = st.wireless.qr.lock().unwrap().get_mut(&job) {
                    j.need_connect = Some(host);
                }
            }
        }
    }));
    Ok(json!({"job": id, "svg": svg, "name": shown}))
}

pub fn qr_status(st: &AppState, id: &str) -> Option<QrJob> {
    st.wireless.qr.lock().unwrap().get(id).cloned()
}

pub fn qr_cancel(st: &AppState, id: &str) {
    st.wireless.qr.lock().unwrap().remove(id);
}

// ---------------------------------------------------------------- 自动重连

/// 后台：每 10 秒检查保存过的无线地址与 mDNS 公布的已配对设备，不在线就尝试连接
pub async fn keeper(st: Arc<AppState>) {
    let a = st.cfg.adb.clone();
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    loop {
        tick.tick().await;
        let Ok(list) = adb::list_devices(&a).await else { continue };
        let online = |addr: &str| list.iter().any(|d| d.serial == addr && d.state == "device");
        let mut targets: Vec<(String, Duration)> =
            st.wireless.saved.lock().unwrap().iter().filter(|x| !online(x)).map(|x| (x.clone(), Duration::from_secs(20))).collect();
        for s in discover(&a).await {
            if s.kind.contains("adb-tls-connect") && !online(&s.addr) && !list.iter().any(|d| d.serial.contains(&s.name) && d.state == "device") {
                targets.push((s.addr, Duration::from_secs(60)));
            }
        }
        for (addr, every) in targets {
            {
                let mut tried = st.wireless.tried.lock().unwrap();
                if tried.get(&addr).is_some_and(|t| t.elapsed() < every) {
                    continue;
                }
                tried.insert(addr.clone(), Instant::now());
            }
            if connect_addr(&a, &addr).await.is_ok() {
                tracing::info!("wireless device reconnected: {addr}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoints() {
        assert_eq!(normalize("192.168.1.5", 5555).unwrap(), "192.168.1.5:5555");
        assert_eq!(normalize("192.168.1.5:37000", 5555).unwrap(), "192.168.1.5:37000");
        assert!(normalize("-s x", 5555).is_err());
        assert!(adb_ok("connected to 1.2.3.4:5555"));
        assert!(adb_ok("already connected to 1.2.3.4:5555"));
        assert!(!adb_ok("failed to connect to 1.2.3.4:5555"));
        assert!(adb_ok("Successfully paired to 1.2.3.4:37000 [guid=adb-x]"));
        assert_eq!(pair_guid("Successfully paired to 1.2.3.4:37000 [guid=adb-R5CX-Ab12]\r\n").as_deref(), Some("adb-R5CX-Ab12"));
        assert_eq!(pair_guid("Successfully paired to 1.2.3.4:37000"), None);
    }
}
