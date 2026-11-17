//! 内置的 mDNS 查询（只用于发现手机的无线调试服务）。
//!
//! 为什么不只用 `adb mdns services`：adb 的 mDNS 在不少环境下拿不到结果——
//! adb.exe 没有防火墙放行（5353 入站组播被拦）、本机 5037 端口被其他工具的旧版 adb 占用、
//! 多网卡（VPN / 虚拟网卡）选错接口等。这里在每个 IPv4 网卡上用临时端口发出查询
//! （RFC 6762 §6.7 “传统单播”查询 + QU 位），手机以单播回复到该端口，
//! 属于出站请求的响应，不需要额外的防火墙规则，也不占用 5353。

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;
use tokio::net::UdpSocket;

pub const PAIRING: &str = "_adb-tls-pairing._tcp.local";
pub const CONNECT: &str = "_adb-tls-connect._tcp.local";

#[derive(Clone, Debug, PartialEq)]
pub struct Service {
    /// 实例名（第一段标签），例如 adb-SERIAL-xxxx 或二维码中的名称
    pub name: String,
    /// 服务类型，例如 _adb-tls-connect._tcp
    pub kind: String,
    /// ip:port
    pub addr: String,
}

const MDNS: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353);
const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;
const TYPE_SRV: u16 = 33;

fn encode_name(out: &mut Vec<u8>, name: &str) {
    for l in name.trim_end_matches('.').split('.') {
        let b = l.as_bytes();
        out.push(b.len().min(63) as u8);
        out.extend_from_slice(&b[..b.len().min(63)]);
    }
    out.push(0);
}

fn build_query(id: u16, qs: &[(&str, u16)]) -> Vec<u8> {
    let mut p = Vec::with_capacity(64);
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&[0, 0]); // flags
    p.extend_from_slice(&(qs.len() as u16).to_be_bytes());
    p.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    for (n, t) in qs {
        encode_name(&mut p, n);
        p.extend_from_slice(&t.to_be_bytes());
        p.extend_from_slice(&0x8001u16.to_be_bytes()); // QU | IN
    }
    p
}

/// 读取（可能带压缩指针的）域名，返回 (名称, 名称之后的位置)
fn read_name(b: &[u8], mut i: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut end = None;
    for _ in 0..64 {
        let len = *b.get(i)? as usize;
        if len == 0 {
            return Some((labels.join("."), end.unwrap_or(i + 1)));
        }
        if len & 0xc0 == 0xc0 {
            let ptr = ((len & 0x3f) << 8) | *b.get(i + 1)? as usize;
            end.get_or_insert(i + 2);
            i = ptr;
            continue;
        }
        let l = b.get(i + 1..i + 1 + len)?;
        labels.push(String::from_utf8_lossy(l).into_owned());
        i += 1 + len;
    }
    None
}

#[derive(Default)]
struct Records {
    /// 服务类型 -> 实例全名
    ptr: Vec<(String, String)>,
    /// 实例全名 -> (目标主机, 端口)
    srv: HashMap<String, (String, u16)>,
    /// 主机名 -> IPv4
    a: HashMap<String, Ipv4Addr>,
}

fn parse(b: &[u8], rec: &mut Records) -> Option<()> {
    if b.len() < 12 || b[2] & 0x80 == 0 {
        return None; // 不是响应
    }
    let qd = u16::from_be_bytes([b[4], b[5]]) as usize;
    let rr = [6, 8, 10].iter().map(|&o| u16::from_be_bytes([b[o], b[o + 1]]) as usize).sum::<usize>();
    let mut i = 12;
    for _ in 0..qd {
        i = read_name(b, i)?.1 + 4;
    }
    for _ in 0..rr {
        let (name, j) = read_name(b, i)?;
        let h = b.get(j..j + 10)?;
        let ty = u16::from_be_bytes([h[0], h[1]]);
        let len = u16::from_be_bytes([h[8], h[9]]) as usize;
        let d = j + 10;
        let data = b.get(d..d + len)?;
        let key = name.to_ascii_lowercase();
        match ty {
            TYPE_PTR => {
                if let Some((t, _)) = read_name(b, d) {
                    rec.ptr.push((key, t));
                }
            }
            TYPE_SRV if len >= 7 => {
                let port = u16::from_be_bytes([data[4], data[5]]);
                if let Some((t, _)) = read_name(b, d + 6) {
                    rec.srv.insert(key, (t.to_ascii_lowercase(), port));
                }
            }
            TYPE_A if len == 4 => {
                rec.a.insert(key, Ipv4Addr::new(data[0], data[1], data[2], data[3]));
            }
            _ => {}
        }
        i = d + len;
    }
    Some(())
}

fn sockets() -> Vec<UdpSocket> {
    let mut ips: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(a) if !a.ip.is_loopback() && !a.ip.is_link_local() => Some(a.ip),
            _ => None,
        })
        .collect();
    ips.sort();
    ips.dedup();
    ips.into_iter()
        .filter_map(|ip| {
            let s = std::net::UdpSocket::bind(SocketAddr::from((ip, 0))).ok()?;
            s.set_nonblocking(true).ok()?;
            let s2 = socket2::Socket::from(s);
            let _ = s2.set_multicast_if_v4(&ip);
            let _ = s2.set_multicast_ttl_v4(255);
            UdpSocket::from_std(s2.into()).ok()
        })
        .collect()
}

/// 收集各 socket 上的回复（socket 数量很少，轮询即可）
async fn collect(socks: &[UdpSocket], rec: &mut Records, src: &mut HashMap<String, Ipv4Addr>, dur: Duration) {
    let mut buf = vec![0u8; 9000];
    let deadline = tokio::time::Instant::now() + dur;
    while tokio::time::Instant::now() < deadline {
        let mut got = false;
        for s in socks {
            while let Ok((n, from)) = s.try_recv_from(&mut buf) {
                got = true;
                let mut r = Records::default();
                if parse(&buf[..n], &mut r).is_none() {
                    continue;
                }
                if let SocketAddr::V4(f) = from {
                    for (_, inst) in &r.ptr {
                        src.entry(inst.to_ascii_lowercase()).or_insert(*f.ip());
                    }
                    for k in r.srv.keys() {
                        src.entry(k.clone()).or_insert(*f.ip());
                    }
                }
                rec.ptr.extend(r.ptr);
                rec.srv.extend(r.srv);
                rec.a.extend(r.a);
            }
        }
        if !got {
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }
}

/// 在所有 IPv4 网卡上查询指定服务类型，等待 `wait` 收集回复。
pub async fn browse(kinds: &[&str], wait: Duration) -> Vec<Service> {
    let socks = sockets();
    if socks.is_empty() {
        return Vec::new();
    }
    let id = (crate::util::random_scid() & 0xffff) as u16 | 1;
    let q = build_query(id, &kinds.iter().map(|k| (*k, TYPE_PTR)).collect::<Vec<_>>());
    let mut rec = Records::default();
    // 回复的源地址：SRV 目标主机没有附带 A 记录时用它作为设备 IP
    let mut src: HashMap<String, Ipv4Addr> = HashMap::new();
    for round in 0..2 {
        for s in &socks {
            let _ = s.send_to(&q, MDNS).await;
        }
        collect(&socks, &mut rec, &mut src, wait / 2).await;
        // 第二轮：补查缺少 SRV 的实例
        let missing: Vec<String> = rec.ptr.iter().map(|(_, i)| i.to_ascii_lowercase()).filter(|i| !rec.srv.contains_key(i)).collect();
        if round == 0 && !missing.is_empty() {
            let q2 = build_query(id, &missing.iter().map(|m| (m.as_str(), TYPE_SRV)).collect::<Vec<_>>());
            for s in &socks {
                let _ = s.send_to(&q2, MDNS).await;
            }
        }
    }
    let mut out: Vec<Service> = Vec::new();
    for (kind, inst) in &rec.ptr {
        if !kinds.iter().any(|k| k.eq_ignore_ascii_case(kind)) {
            continue;
        }
        let key = inst.to_ascii_lowercase();
        let Some((host, port)) = rec.srv.get(&key) else { continue };
        let Some(ip) = rec.a.get(host).or_else(|| src.get(&key)) else { continue };
        let name = inst.split('.').next().unwrap_or_default().to_string();
        let kind = kind.trim_end_matches(".local").to_string();
        let s = Service { name, kind, addr: format!("{ip}:{port}") };
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rr(p: &mut Vec<u8>, name: &str, ty: u16, data: &[u8]) {
        encode_name(p, name);
        p.extend_from_slice(&ty.to_be_bytes());
        p.extend_from_slice(&1u16.to_be_bytes());
        p.extend_from_slice(&120u32.to_be_bytes());
        p.extend_from_slice(&(data.len() as u16).to_be_bytes());
        p.extend_from_slice(data);
    }

    #[test]
    fn parse_response() {
        let mut p = vec![0, 1, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 2];
        let mut inst = Vec::new();
        encode_name(&mut inst, "adb-ABC-xyz._adb-tls-connect._tcp.local");
        rr(&mut p, CONNECT, TYPE_PTR, &inst);
        let mut srv = vec![0, 0, 0, 0, 0x9c, 0x40];
        encode_name(&mut srv, "Android.local");
        rr(&mut p, "adb-ABC-xyz._adb-tls-connect._tcp.local", TYPE_SRV, &srv);
        rr(&mut p, "Android.local", TYPE_A, &[192, 168, 1, 7]);
        let mut r = Records::default();
        parse(&p, &mut r).unwrap();
        assert_eq!(r.ptr[0].1, "adb-ABC-xyz._adb-tls-connect._tcp.local");
        assert_eq!(r.srv["adb-abc-xyz._adb-tls-connect._tcp.local"], ("android.local".to_string(), 40000));
        assert_eq!(r.a["android.local"], Ipv4Addr::new(192, 168, 1, 7));
        // 压缩指针
        let b = [3, b'f', b'o', b'o', 0, 0xc0, 0];
        assert_eq!(read_name(&b, 5).unwrap(), ("foo".to_string(), 7));
    }
}
