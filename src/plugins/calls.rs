//! 宿主 → 主进程的 call 处理（SPEC 3.3）：权限检查、match 范围检查、审计与设备操作。

use super::*;
use crate::{adb, agent, apps, files, logcat, media, perf, scrcpy, util, wireless};
use futures_util::{SinkExt, StreamExt};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::tungstenite::Message as TMsg;

/// 限流后异步执行，结果通过 `ret` 回复宿主
pub(super) fn dispatch(app: Arc<AppState>, ph: Arc<Plugin>, id: u64, m: String, p: Value) {
    tokio::spawn(async move {
        ph.take_rate().await;
        let r = handle(&app, &ph, &m, &p).await;
        let msg = match r {
            Ok(v) => json!({"t": "ret", "id": id, "ok": v}),
            Err(e) => json!({"t": "ret", "id": id, "err": e}),
        };
        if let Some(h) = ph.host() {
            let _ = h.out.send(msg).await;
        }
    });
}

/// 需要设备参数的方法（先做权限与 match 检查，再占用并发名额）
fn is_device_method(m: &str) -> bool {
    matches!(
        m,
        "prop" | "shell"
            | "push"
            | "pull"
            | "install"
            | "uninstall"
            | "apps"
            | "hasApp"
            | "launch"
            | "stop"
            | "clear"
            | "foreground"
            | "input"
            | "screenshot"
            | "forward"
            | "unforward"
            | "logcat"
            | "logwatch"
            | "perf"
            | "overlay"
            | "badge"
            | "ui"
            | "files"
            | "mkdir"
            | "rename"
            | "rm"
            | "clipboard"
            | "cmd"
            | "tcpip"
            | "mirror"
    )
}

async fn handle(app: &Arc<AppState>, ph: &Arc<Plugin>, m: &str, p: &Value) -> Result<Value, String> {
    if is_device_method(m) {
        // 先查权限，再查 match（SPEC 7.3）
        if let Some(perm) = perm_for(m) {
            need(ph, perm)?;
        }
        let serial = device(app, ph, p).await?;
        // 每插件最多 4 个、每设备最多 2 个插件操作
        let _plugin_slot = ph.sem.acquire().await.expect("plugin semaphore");
        let dev_sem = app.plugins.device_sem(&serial);
        let _dev_slot = dev_sem.acquire_owned().await.expect("device semaphore");
        return device_op(app, ph, m, p, &serial).await;
    }
    match m {
        "devices" => {
            // 本机 + 各远程电脑上的设备（SPEC 7.3）
            let mut out = Vec::new();
            for d in app.plugins.all_devices(app).await {
                if !app.plugins.device_allowed(app, ph, &d.id).await {
                    continue;
                }
                out.push(d.json());
            }
            Ok(json!(out))
        }
        "register" => register(app, ph, p),
        "unregister" => {
            let kind = arg_str(p, "kind").unwrap_or("");
            let id = arg_str(p, "id").unwrap_or("");
            let mut c = ph.contributions.lock().unwrap();
            if kind == "event" {
                c.events.remove(id);
            } else if kind == "export" {
                c.exports.remove(id);
            } else if kind == "mcp" {
                // mcp 的标识是 name（SPEC 3.3）
                c.mcps.retain(|d| d.get("name").and_then(|v| v.as_str()) != Some(id));
            } else if let Some(list) = c.list(kind) {
                list.retain(|d| d.get("id").and_then(|v| v.as_str()) != Some(id));
            } else if kind == "panel" || kind == "webpanel" {
                c.panels.retain(|(_, _, d)| d.get("id").and_then(|v| v.as_str()) != Some(id));
            }
            drop(c);
            app.plugins.emit_changed();
            Ok(json!(true))
        }
        "notify" => {
            let text = arg_str(p, "text").unwrap_or("").to_string();
            let kind = arg_str(p, "kind").unwrap_or("info").to_string();
            app.plugins.toast(&ph.id, text, &kind);
            Ok(json!(true))
        }
        "dialog" => Ok(app.plugins.ask(ph, p, arg_str(p, "client")).await),
        "uiopen" => {
            let url = arg_str(p, "url").unwrap_or_default();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(tr!("只能打开 http(s) 地址", "Only http(s) URLs can be opened"));
            }
            app.plugins.emit(arg_str(p, "client"), json!({
                "type": "open",
                "plugin": ph.id,
                "url": url,
                "name": p.get("name").cloned().unwrap_or(Value::Null),
                "title": p.get("title").cloned().unwrap_or(Value::Null),
                "mode": p.get("mode").cloned().unwrap_or(Value::Null),
            }));
            Ok(json!(true))
        }
        "progress" => {
            app.plugins.emit(None, json!({
                "type": "progress",
                "plugin": ph.id,
                "id": p.get("id").cloned().unwrap_or(Value::Null),
                "title": p.get("title").cloned().unwrap_or(Value::Null),
                "frac": p.get("frac").cloned().unwrap_or(Value::Null),
                "text": p.get("text").cloned().unwrap_or(Value::Null),
                "done": p.get("done").cloned().unwrap_or(json!(false)),
            }));
            Ok(json!(true))
        }
        "repaint" => {
            app.plugins.emit(None, json!({"type": "repaint", "plugin": ph.id, "panel": p.get("panel").cloned().unwrap_or(Value::Null)}));
            Ok(json!(true))
        }
        "webemit" => {
            app.plugins.emit(None, json!({
                "type": "webmsg",
                "plugin": ph.id,
                "name": p.get("name").cloned().unwrap_or(Value::Null),
                "data": p.get("data").cloned().unwrap_or(Value::Null),
            }));
            Ok(json!(true))
        }
        "logunwatch" => {
            if let Some(sub) = p.get("sub").and_then(|v| v.as_u64()) {
                stop_watch(app, sub);
            }
            Ok(json!(true))
        }
        "media.save" => {
            need(ph, "media")?;
            let path = arg_str(p, "path").ok_or_else(|| tr!("缺少 path", "Missing path"))?;
            let path = &local_path(app, ph, path, false)?;
            let data = tokio::fs::read(path).await.map_err(|e| {
                let path = path.to_string();
                tr!("无法读取 {path}：{e}", "Cannot read {path}: {e}")
            })?;
            let device = arg_str(p, "serial").unwrap_or_default();
            let item = media::save_shot(app, device, &data).await?;
            app.plugins.audit(&ph.id, "media.save", json!({"path": path, "name": item.name.clone()}));
            Ok(json!(item.name))
        }
        "media.list" => {
            need(ph, "media")?;
            Ok(json!(media_items(app).await?))
        }
        "media.item" => {
            let name = arg_str(p, "name").unwrap_or_default();
            Ok(media_items(app).await?.into_iter().find(|it| it["name"] == name).unwrap_or(Value::Null))
        }
        "media.remove" => {
            need(ph, "media")?;
            let name = arg_str(p, "name").ok_or_else(|| tr!("缺少 name", "Missing name"))?;
            media::delete(app, name).await?;
            app.plugins.audit(&ph.id, "media.remove", json!({"name": name}));
            Ok(json!(true))
        }
        "mirrors" => Ok(scrcpy::mirrors_json(app)["list"].clone()),
        "computers" => {
            let list: Vec<Value> = app.peers.list().await.into_iter().map(|p| json!({"id": p.id, "name": p.name, "addr": p.addr})).collect();
            Ok(json!(list))
        }
        "plugins" => {
            let list: Vec<Value> = app
                .plugins
                .plugins()
                .iter()
                .map(|x| {
                    let man = x.manifest();
                    let exports: Vec<String> = x.contributions.lock().unwrap().exports.iter().cloned().collect();
                    json!({"id": x.id, "name": man.name, "version": man.version, "state": x.state().as_str(), "exports": exports})
                })
                .collect();
            Ok(json!(list))
        }
        "plugin.call" => {
            let target = arg_str(p, "plugin").unwrap_or_default();
            let name = arg_str(p, "name").ok_or_else(|| tr!("缺少 name", "Missing name"))?;
            let other = app.plugins.get(target).filter(|x| x.state() == PluginState::Running);
            let Some((other, h)) = other.and_then(|x| x.host().map(|h| (x, h))) else {
                return Err(tr!("插件 {target} 没有运行", "Plugin {target} is not running"));
            };
            if !other.contributions.lock().unwrap().exports.contains(name) {
                return Err(tr!("插件 {target} 没有提供 {name}", "Plugin {target} does not export {name}"));
            }
            let q = json!({"name": name, "args": p.get("args").cloned().unwrap_or(Value::Null), "from": ph.id});
            h.request("export", q, Duration::from_secs(120)).await
        }
        "adb.connect" | "adb.pair" | "adb.disconnect" | "adb.discover" => {
            need(ph, "device.connect")?;
            let addr = arg_str(p, "addr").unwrap_or_default().to_string();
            let r = match m {
                "adb.connect" => wireless::connect(app, wireless::ConnectReq { addr: addr.clone() }).await?["serial"].clone(),
                "adb.pair" => {
                    let code = arg_str(p, "code").unwrap_or_default().to_string();
                    let connect = arg_str(p, "connect").unwrap_or_default().to_string();
                    let v = wireless::pair(app, wireless::PairReq { addr: addr.clone(), code, connect }).await?;
                    json!({"serial": v.get("serial").cloned().unwrap_or(Value::Null), "host": v.get("host").cloned().unwrap_or(Value::Null)})
                }
                "adb.disconnect" => {
                    wireless::disconnect(app, wireless::SerialReq { serial: addr.clone() }).await?;
                    json!(true)
                }
                _ => {
                    let list: Vec<Value> = wireless::discover(&app.cfg.adb).await.into_iter().map(|s| json!({"name": s.name, "kind": s.kind, "addr": s.addr})).collect();
                    return Ok(json!(list));
                }
            };
            app.plugins.audit(&ph.id, m, json!({"addr": addr}));
            Ok(r)
        }
        other => Err(tr!("未知方法：{other}", "Unknown method: {other}")),
    }
}

fn register(app: &Arc<AppState>, ph: &Arc<Plugin>, p: &Value) -> Result<Value, String> {
    let kind = arg_str(p, "kind").unwrap_or("");
    let def = p.get("def").cloned().ok_or_else(|| tr!("缺少 def", "Missing def"))?;
    if kind == "mcp" {
        need(ph, "mcp")?;
    }
    if kind == "event" || kind == "export" {
        let name = def.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if name.is_empty() {
            return Err(tr!("{kind} 缺少 name", "{kind} requires a name"));
        }
        let mut c = ph.contributions.lock().unwrap();
        if kind == "event" {
            c.events.insert(name);
        } else {
            c.exports.insert(name);
        }
        drop(c);
        app.plugins.emit_changed();
        return Ok(json!(true));
    }
    let id = def.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut c = ph.contributions.lock().unwrap();
    match kind {
        // mcp 用 name 作为标识（SPEC 3.3：`{name, description, params}`），没有 id
        "mcp" => {
            let name = def.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() {
                return Err(tr!("mcp 缺少 name", "mcp requires a name"));
            }
            c.mcps.retain(|d| d.get("name").and_then(|v| v.as_str()) != Some(name));
            c.mcps.push(def);
        }
        "panel" | "webpanel" => {
            let external = kind == "webpanel" && def.get("url").and_then(|v| v.as_bool()) == Some(true);
            if external {
                // 嵌入外部网页：地址由插件按设备给出（web 处理函数 __url:<id>），前端再按清单的 embed 校验
                need(ph, "web.embed")?;
                if ph.manifest().embed.is_empty() {
                    return Err(tr!("嵌入外部网页需要在 plugin.json 的 embed 里列出网站", "List the sites in plugin.json \"embed\" to embed external pages"));
                }
            }
            let web = if kind == "webpanel" && !external {
                let entry = def.get("entry").and_then(|v| v.as_str()).unwrap_or("");
                if !manifest::safe_rel(entry) {
                    return Err(tr!("webpanel 的 entry 必须是插件目录内的相对路径", "webpanel entry must be a relative path inside the plugin"));
                }
                entry.to_string()
            } else {
                String::new()
            };
            if id.is_empty() {
                return Err(tr!("{kind} 缺少 id", "{kind} requires an id"));
            }
            c.panels.retain(|(_, _, d)| d.get("id").and_then(|v| v.as_str()) != Some(id.as_str()));
            c.panels.push((kind == "webpanel", web, def));
        }
        "menu" | "status" | "metric" | "loghl" => {
            if id.is_empty() {
                return Err(tr!("{kind} 缺少 id", "{kind} requires an id"));
            }
            let Some(list) = c.list(kind) else {
                return Err(tr!("未知的 kind：{kind}", "Unknown kind: {kind}"));
            };
            list.retain(|d| d.get("id").and_then(|v| v.as_str()) != Some(id.as_str()));
            list.push(def);
        }
        other => return Err(tr!("未知的 kind：{other}", "Unknown kind: {other}")),
    }
    drop(c);
    app.plugins.emit_changed();
    Ok(json!(true))
}

// ---------------------------------------------------------------- 设备操作

/// 设备操作的执行结果（本机执行与远程 devop 共用）
pub(crate) enum DevOut {
    Json(Value),
    Bytes(Vec<u8>),
}

/// 某个设备操作需要的权限（不需要的返回 None）
fn perm_for(m: &str) -> Option<&'static str> {
    Some(match m {
        "shell" => "device.shell",
        "push" | "pull" | "files" | "mkdir" | "rename" | "rm" => "device.files",
        "install" | "uninstall" => "device.install",
        "apps" | "launch" | "stop" | "clear" | "foreground" => "device.apps",
        "input" | "clipboard" | "cmd" => "device.input",
        "screenshot" | "overlay" | "ui" => "device.screen",
        "forward" | "unforward" => "device.forward",
        "logcat" | "logwatch" => "device.logs",
        "perf" => "device.perf",
        "tcpip" => "device.connect",
        _ => return None,
    })
}

/// 只能在本机执行的设备操作（端口转发指向设备所在电脑；切换无线连到本机 adb）
fn local_only(m: &str) -> bool {
    matches!(m, "forward" | "unforward" | "tcpip")
}

/// 路由前的检查：远程设备不支持端口转发与切换无线（SPEC 7.2）
fn check_route(m: &str, id: &str) -> Result<(), String> {
    if local_only(m) && split_dev(id).is_some() {
        return Err(if m == "tcpip" {
            tr!("只能切换这台电脑上的设备", "Only devices on this computer can be switched")
        } else {
            tr!("远程电脑上的设备暂不支持端口转发", "Port forwarding is not supported for devices on remote computers")
        });
    }
    Ok(())
}

/// 一次设备 call：权限 → match（用原始 serial）→ 路由（含 `/` 走远程 devop）→ 审计
async fn device_op(app: &Arc<AppState>, ph: &Arc<Plugin>, m: &str, p: &Value, id: &str) -> Result<Value, String> {
    if let Some(perm) = perm_for(m) {
        need(ph, perm)?;
    }
    check_route(m, id)?;
    // 只作用于界面（不执行设备操作）
    match m {
        "overlay" => {
            let shapes = p.get("shapes").cloned().unwrap_or(Value::Null);
            // size：形状坐标所在的空间（如设备分辨率），前端据此缩放到画面；不给则按画面像素
            let size = p.get("size").filter(|s| s.is_object()).cloned().unwrap_or(Value::Null);
            {
                let mut c = ph.contributions.lock().unwrap();
                if shapes.is_null() {
                    c.overlays.remove(id);
                } else {
                    c.overlays.insert(id.to_string(), json!({"shapes": shapes.clone(), "size": size.clone()}));
                }
            }
            app.plugins.emit(None, json!({"type": "overlay", "plugin": ph.id, "serial": id, "shapes": shapes, "size": size}));
            return Ok(json!(true));
        }
        "badge" => {
            let text = p.get("text").cloned().unwrap_or(Value::Null);
            {
                let mut c = ph.contributions.lock().unwrap();
                if text.is_null() {
                    c.badges.remove(id);
                } else {
                    c.badges.insert(
                        id.to_string(),
                        json!({"text": text, "kind": p.get("kind").cloned().unwrap_or(json!("info")), "tip": p.get("tip").cloned().unwrap_or(Value::Null)}),
                    );
                }
            }
            app.plugins.emit_changed();
            return Ok(json!(true));
        }
        "logwatch" => return start_watch(app, ph, id, p),
        "mirror" => {
            app.plugins.emit(arg_str(p, "client"), json!({"type": "mirror", "plugin": ph.id, "serial": id}));
            return Ok(json!(true));
        }
        _ => {}
    }

    // 本机路径在 A 上检查（远程设备也一样：文件在 A 与 B 之间传）
    let mut q = p.clone();
    match m {
        "push" => {
            let from = arg_str(p, "local").ok_or_else(|| tr!("缺少 local", "Missing local"))?;
            let to = arg_str(p, "remote").ok_or_else(|| tr!("缺少 remote", "Missing remote"))?;
            not_option(to)?;
            q["local"] = json!(local_path(app, ph, from, false)?);
        }
        "pull" => {
            let to = arg_str(p, "local").ok_or_else(|| tr!("缺少 local", "Missing local"))?;
            let from = arg_str(p, "remote").ok_or_else(|| tr!("缺少 remote", "Missing remote"))?;
            not_option(from)?;
            q["local"] = json!(local_path(app, ph, to, true)?);
        }
        "install" => {
            let path = arg_str(p, "path").ok_or_else(|| tr!("缺少 path", "Missing path"))?;
            q["path"] = json!(local_path(app, ph, path, false)?);
        }
        _ => {}
    }

    let out = match split_dev(id) {
        Some((peer_id, raw)) => {
            let peer = app.peers.get(peer_id).await.ok_or_else(|| tr!("远程电脑不存在", "Remote computer not found"))?;
            remote_op(&peer, raw, m, &q).await?
        }
        None => {
            let out = exec_local(app, m, &q, id).await?;
            match m {
                "forward" => {
                    if let DevOut::Json(Value::Number(n)) = &out {
                        let port = n.as_u64().unwrap_or(0);
                        let reverse = q.get("reverse").and_then(|v| v.as_bool()).unwrap_or(false);
                        ph.forwards.lock().unwrap().push(Forward { serial: id.to_string(), local: format!("tcp:{port}"), reverse });
                    }
                }
                "unforward" => {
                    let reverse = q.get("reverse").and_then(|v| v.as_bool()).unwrap_or(false);
                    let local = port_of(q.get("local"));
                    ph.forwards
                        .lock()
                        .unwrap()
                        .retain(|f| !(f.serial == id && (local == 0 || f.local == format!("tcp:{local}")) && f.reverse == reverse));
                }
                _ => {}
            }
            out
        }
    };

    if let Some(detail) = audit_json(m, id, &q) {
        app.plugins.audit(&ph.id, m, detail);
    }

    match m {
        "install" => {
            if let DevOut::Json(v) = &out {
                if let Some(pkg) = v.get("package").and_then(|v| v.as_str()).filter(|p| !p.is_empty()) {
                    // A 据此发 app.installed（本机与远程一致）
                    on_installed(app, id, pkg);
                }
            }
            Ok(json!(true))
        }
        "uninstall" => {
            on_uninstalled(app, id, &pkg_arg(p)?);
            Ok(json!(true))
        }
        "pull" => {
            let DevOut::Bytes(data) = out else { return Ok(json!(true)) };
            let local = arg_str(&q, "local").unwrap_or("").to_string();
            tokio::fs::write(&local, &data).await.map_err(|e| tr!("无法写入 {local}：{e}", "Cannot write {local}: {e}"))?;
            Ok(json!(true))
        }
        "screenshot" => {
            let DevOut::Bytes(png) = out else { return Err(tr!("截图失败", "Screenshot failed")) };
            let save = p.get("save").and_then(|v| v.as_bool()).unwrap_or(false);
            if save {
                let item = media::save_shot(app, id, &png).await?;
                let dir = media::media_dir(app).await;
                return Ok(json!(dir.join(item.name).to_string_lossy()));
            }
            let dir = app.plugins.root().join(".tmp");
            let _ = tokio::fs::create_dir_all(&dir).await;
            let path = dir.join(format!("shot-{:08x}.png", util::random_scid()));
            tokio::fs::write(&path, &png).await.map_err(|e| e.to_string())?;
            Ok(json!(path.to_string_lossy()))
        }
        _ => Ok(match out {
            DevOut::Json(v) => v,
            DevOut::Bytes(b) => json!(b.len()),
        }),
    }
}

/// 远程设备：经 B 的 `/api/plugins/devop` 执行（SPEC 7.2）
async fn remote_op(peer: &crate::peers::Peer, raw: &str, m: &str, q: &Value) -> Result<DevOut, String> {
    let timeout = match m {
        // shell 按参数（留出网络与传输余量）
        "shell" => Duration::from_secs(q.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30).clamp(1, 600) + 10),
        "push" | "install" | "pull" => Duration::from_secs(1800),
        _ => Duration::from_secs(60),
    };
    let mut params = q.clone();
    if let Some(o) = params.as_object_mut() {
        o.remove("serial");
        o.remove("local");
        o.remove("path");
    }
    match m {
        "push" | "install" => {
            let file = if m == "push" { arg_str(q, "local") } else { arg_str(q, "path") }.unwrap_or("");
            let data = tokio::fs::read(file).await.map_err(|e| tr!("无法读取 {file}：{e}", "Cannot read {file}: {e}"))?;
            Ok(DevOut::Json(devop::json_call(peer, raw, m, &params, &data, timeout).await?))
        }
        "pull" | "screenshot" => Ok(DevOut::Bytes(devop::bytes_call(peer, raw, m, &params, timeout).await?)),
        _ => Ok(DevOut::Json(devop::json_call(peer, raw, m, &params, &[], timeout).await?)),
    }
}

/// 审计内容（会改变设备或运行程序的操作）
fn audit_json(m: &str, id: &str, q: &Value) -> Option<Value> {
    let s = |k: &str| json!(arg_str(q, k));
    let mut d = Map::new();
    d.insert("serial".into(), json!(id));
    match m {
        "shell" => {
            d.insert("cmd".into(), s("cmd"));
        }
        "push" => {
            d.insert("from".into(), s("local"));
            d.insert("to".into(), s("remote"));
        }
        "pull" => {
            d.insert("from".into(), s("remote"));
            d.insert("to".into(), s("local"));
        }
        "install" => {
            d.insert("path".into(), s("path"));
        }
        "uninstall" | "launch" | "stop" | "clear" => {
            d.insert("package".into(), s("package"));
        }
        "input" | "cmd" => {
            d.insert("kind".into(), s("kind"));
            d.insert("name".into(), s("name"));
        }
        "clipboard" => {
            d.insert("set".into(), json!(q.get("text").is_some_and(|t| t.is_string())));
        }
        "mkdir" | "rm" => {
            d.insert("path".into(), q.get("path").or(q.get("paths")).cloned().unwrap_or(Value::Null));
        }
        "rename" => {
            d.insert("path".into(), s("path"));
            d.insert("name".into(), s("name"));
        }
        "tcpip" => {}
        "forward" => {
            d.insert("local".into(), json!(port_of(q.get("local"))));
            d.insert("remote".into(), s("remote"));
            d.insert("reverse".into(), json!(q.get("reverse").and_then(|v| v.as_bool()).unwrap_or(false)));
        }
        "unforward" => {
            d.insert("local".into(), json!(port_of(q.get("local"))));
            d.insert("reverse".into(), json!(q.get("reverse").and_then(|v| v.as_bool()).unwrap_or(false)));
        }
        _ => return None,
    }
    Some(Value::Object(d))
}

/// 执行设备操作（纯逻辑：只调用 adb / apps / perf / agent，不含权限检查与审计）。
/// 本机的插件 call 与 B 上的 devop 都调用它。
pub(crate) async fn exec_local(app: &AppState, m: &str, p: &Value, s: &str) -> Result<DevOut, String> {
    let adb_path = &app.cfg.adb;
    let out = match m {
        "prop" => {
            let name = arg_str(p, "name").ok_or_else(|| tr!("缺少 name", "Missing name"))?;
            if !name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
                return Err(tr!("属性名不合法", "Invalid property name"));
            }
            let cmd = format!("getprop {name}");
            let o = adb::run_timeout(adb_path, &["-s", s, "shell", &cmd], Duration::from_secs(20)).await?;
            DevOut::Json(json!(o.trim_end()))
        }
        "shell" => {
            let cmd = arg_str(p, "cmd").ok_or_else(|| tr!("缺少 cmd", "Missing cmd"))?.to_string();
            let secs = p.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30).clamp(1, 600);
            let (out, code) = shell_out(app, s, &cmd, Duration::from_secs(secs)).await?;
            DevOut::Json(json!({"out": out, "code": code}))
        }
        "push" => {
            let from = arg_str(p, "local").ok_or_else(|| tr!("缺少 local", "Missing local"))?.to_string();
            let to = arg_str(p, "remote").ok_or_else(|| tr!("缺少 remote", "Missing remote"))?.to_string();
            not_option(&to)?;
            adb::run_timeout(adb_path, &["-s", s, "push", &from, &to], Duration::from_secs(1800)).await?;
            DevOut::Json(json!(true))
        }
        "pull" => {
            let from = arg_str(p, "remote").ok_or_else(|| tr!("缺少 remote", "Missing remote"))?.to_string();
            not_option(&from)?;
            match arg_str(p, "local") {
                // 本机路径已经检查过，直接拉到目的地
                Some(to) => {
                    adb::run_timeout(adb_path, &["-s", s, "pull", &from, to], Duration::from_secs(1800)).await?;
                    DevOut::Json(json!(true))
                }
                // 没有本机路径：拉到临时文件后读成字节（devop 用）
                None => {
                    let dir = app.plugins.root().join(".tmp");
                    let _ = tokio::fs::create_dir_all(&dir).await;
                    let tmp = dir.join(format!("pull-{:08x}", util::random_scid()));
                    let tmp_s = tmp.to_string_lossy().to_string();
                    let r = adb::run_timeout(adb_path, &["-s", s, "pull", &from, &tmp_s], Duration::from_secs(1800)).await;
                    let data = tokio::fs::read(&tmp).await;
                    let _ = tokio::fs::remove_file(&tmp).await;
                    r?;
                    DevOut::Bytes(data.map_err(|e| tr!("无法读取下载的文件：{e}", "Cannot read the downloaded file: {e}"))?)
                }
            }
        }
        "install" => {
            let path = arg_str(p, "path").ok_or_else(|| tr!("缺少 path", "Missing path"))?;
            let pkg = install_apk(app, path, s, p).await?;
            DevOut::Json(json!({"ok": true, "package": pkg}))
        }
        "uninstall" => {
            let pkg = pkg_arg(p)?;
            adb::run_timeout(adb_path, &["-s", s, "uninstall", &pkg], Duration::from_secs(120)).await.map_err(|e| {
                let pkg = pkg.clone();
                tr!("卸载 {pkg} 失败：{e}", "Failed to uninstall {pkg}: {e}")
            })?;
            DevOut::Json(json!(true))
        }
        "apps" => {
            let system = p.get("system").and_then(|v| v.as_bool()).unwrap_or(false);
            let r = apps::list(app, apps::ListReq { serial: s.to_string() }).await?;
            let list: Vec<Value> = r["apps"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|a| system || !a["system"].as_bool().unwrap_or(false))
                .map(|a| json!({"package": a["pkg"], "name": a["name"], "version": ""}))
                .collect();
            DevOut::Json(json!(list))
        }
        "hasApp" => {
            let pkg = pkg_arg(p)?;
            DevOut::Json(json!(app.plugins.has_package(app, s, &pkg).await))
        }
        "launch" | "stop" | "clear" => {
            let pkg = pkg_arg(p)?;
            let r = apps::action(app, apps::ActionReq { serial: s.to_string(), pkg, action: m.to_string() }).await?;
            DevOut::Json(json!(r["msg"].as_str().unwrap_or("OK")))
        }
        "foreground" => {
            let fg = agent::foreground(app, s).await;
            // agent 给的是 包名/Activity，插件只要包名
            let pkg = fg.split('/').next().unwrap_or("");
            DevOut::Json(if pkg.is_empty() { Value::Null } else { json!(pkg) })
        }
        "input" => {
            let kind = arg_str(p, "kind").unwrap_or("");
            let num = |k: &str| p.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|x| x.trim().parse().ok()))).unwrap_or(0.0);
            match kind {
                "tap" => {
                    let cmd = format!("input tap {} {}", num("x") as i64, num("y") as i64);
                    shell_out(app, s, &cmd, Duration::from_secs(20)).await?;
                }
                "swipe" => {
                    let ms = p.get("duration").and_then(|v| v.as_u64()).unwrap_or(300).clamp(20, 10_000);
                    let cmd = format!("input swipe {} {} {} {} {ms}", num("x") as i64, num("y") as i64, num("x2") as i64, num("y2") as i64);
                    shell_out(app, s, &cmd, Duration::from_secs(30)).await?;
                }
                "key" => {
                    let key = arg_str(p, "key").ok_or_else(|| tr!("缺少 key", "Missing key"))?.to_ascii_lowercase();
                    if key == "select_all" {
                        shell_out(app, s, "input keycombination 113 29 2>/dev/null || input keyevent --longpress 29", Duration::from_secs(20)).await?;
                    } else if let Some(code) = agent::key_code(&key) {
                        shell_out(app, s, &format!("input keyevent {code}"), Duration::from_secs(20)).await?;
                    } else {
                        return Err(tr!("未知按键 {key}", "Unknown key {key}"));
                    }
                }
                "text" => {
                    let text = p.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    if text.is_empty() {
                        return Err(tr!("缺少 text", "Missing text"));
                    }
                    agent::paste_text(app, s, &text).await?;
                }
                other => return Err(tr!("未知输入类型：{other}", "Unknown input kind: {other}")),
            }
            DevOut::Json(json!(true))
        }
        "screenshot" => DevOut::Bytes(adb::run_bytes(adb_path, &["-s", s, "exec-out", "screencap", "-p"], Duration::from_secs(30)).await?),
        "forward" => {
            let remote = arg_str(p, "remote").ok_or_else(|| tr!("缺少 remote", "Missing remote"))?.to_string();
            let reverse = p.get("reverse").and_then(|v| v.as_bool()).unwrap_or(false);
            let local = port_of(p.get("local"));
            let local = if local == 0 && reverse { free_port() } else { local };
            let port = if reverse {
                adb::run_timeout(adb_path, &["-s", s, "reverse", &remote, &format!("tcp:{local}")], Duration::from_secs(30)).await?;
                local
            } else {
                let out = adb::run_timeout(adb_path, &["-s", s, "forward", &format!("tcp:{local}"), &remote], Duration::from_secs(30)).await?;
                out.trim().parse::<u16>().unwrap_or(local)
            };
            DevOut::Json(json!(port))
        }
        "unforward" => {
            let reverse = p.get("reverse").and_then(|v| v.as_bool()).unwrap_or(false);
            let local = port_of(p.get("local"));
            let args: Vec<String> = if local == 0 {
                vec!["-s".into(), s.into(), if reverse { "reverse" } else { "forward" }.into(), "--remove-all".into()]
            } else {
                vec!["-s".into(), s.into(), if reverse { "reverse" } else { "forward" }.into(), "--remove".into(), format!("tcp:{local}")]
            };
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            adb::run_timeout(adb_path, &refs, Duration::from_secs(30)).await?;
            DevOut::Json(json!(true))
        }
        "logcat" => {
            let content = agent::logcat(app, s, p).await?;
            let text: Vec<&str> = content.iter().filter_map(|c| c.get("text").and_then(|t| t.as_str())).collect();
            DevOut::Json(json!(text.join("\n")))
        }
        "perf" => DevOut::Json(perf::snapshot(app, s).await?),
        "ui" => DevOut::Json(agent::ui_tree(app, s).await?),
        "files" => {
            let path = arg_str(p, "path").unwrap_or("").to_string();
            DevOut::Json(files::list(app, files::ListReq { serial: s.to_string(), path }).await?)
        }
        "mkdir" => {
            let path = arg_str(p, "path").ok_or_else(|| tr!("缺少 path", "Missing path"))?.trim_end_matches('/');
            let (dir, name) = path.rsplit_once('/').ok_or_else(|| tr!("路径必须是以 / 开头的绝对路径", "Path must be absolute (start with /)"))?;
            let r = files::mkdir(app, files::TwoReq { serial: s.to_string(), path: dir.to_string(), to: name.to_string() }).await?;
            DevOut::Json(r["path"].clone())
        }
        "rename" => {
            let path = arg_str(p, "path").ok_or_else(|| tr!("缺少 path", "Missing path"))?.to_string();
            let to = arg_str(p, "name").ok_or_else(|| tr!("缺少 name", "Missing name"))?.to_string();
            let r = files::rename(app, files::TwoReq { serial: s.to_string(), path, to }).await?;
            DevOut::Json(r["path"].clone())
        }
        "rm" => {
            let paths: Vec<String> = match p.get("paths") {
                Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str()).map(String::from).collect(),
                _ => arg_str(p, "path").map(|x| vec![x.to_string()]).unwrap_or_default(),
            };
            files::delete(app, files::DeleteReq { serial: s.to_string(), paths }).await?;
            DevOut::Json(json!(true))
        }
        "clipboard" => match p.get("text").and_then(|v| v.as_str()) {
            Some(text) => {
                agent::set_clipboard(app, s, text, false).await?;
                DevOut::Json(json!(true))
            }
            None => DevOut::Json(json!(agent::get_clipboard(app, s).await?)),
        },
        "cmd" => {
            agent::device_cmd(app, s, arg_str(p, "name").unwrap_or("")).await?;
            DevOut::Json(json!(true))
        }
        "tcpip" => {
            let r = wireless::tcpip(app, wireless::SerialReq { serial: s.to_string() }).await?;
            DevOut::Json(r["serial"].clone())
        }
        other => return Err(tr!("未知方法：{other}", "Unknown method: {other}")),
    };
    Ok(out)
}

/// 安装 APK，返回能识别到的新包名（不做权限检查，也不写审计）
async fn install_apk(app: &AppState, path: &str, serial: &str, p: &Value) -> Result<Option<String>, String> {
    let replace = p.get("replace").and_then(|v| v.as_bool()).unwrap_or(true);
    let grant = p.get("grant").and_then(|v| v.as_bool()).unwrap_or(false);
    let downgrade = p.get("downgrade").and_then(|v| v.as_bool()).unwrap_or(false);
    let before = shell_out(app, serial, "pm list packages -3", Duration::from_secs(60)).await.map(|(o, _)| o).unwrap_or_default();
    let mut args: Vec<&str> = vec!["-s", serial, "install"];
    if replace {
        args.push("-r");
    }
    if grant {
        args.push("-g");
    }
    if downgrade {
        args.push("-d");
    }
    args.push(path);
    adb::run_timeout(&app.cfg.adb, &args, Duration::from_secs(900)).await.map_err(|e| {
        let path = path.to_string();
        tr!("安装 {path} 失败：{e}", "Failed to install {path}: {e}")
    })?;
    let after = shell_out(app, serial, "pm list packages -3", Duration::from_secs(60)).await.map(|(o, _)| o).unwrap_or_default();
    Ok(new_packages(&before, &after).first().cloned())
}
// ---------------------------------------------------------------- logwatch

struct Bucket {
    tokens: f64,
    last: Instant,
    rate: f64,
}

impl Bucket {
    fn new(rate: f64) -> Self {
        Self { tokens: rate, last: Instant::now(), rate }
    }

    fn allow(&mut self) -> bool {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * self.rate).min(self.rate);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// logcat 的一行 → `{time, pid, tid, level, tag, msg, raw}`
fn parse_logline(line: &str) -> Option<Value> {
    // 08-01 12:00:00.123  1234  5678 I Tag     : message
    let mut it = line.split_whitespace();
    let date = it.next()?;
    let time = it.next()?;
    let pid = it.next()?;
    let tid = it.next()?;
    let level = it.next()?;
    let rest = line.splitn(2, level).nth(1).unwrap_or("");
    let rest = rest.trim_start();
    let (tag, msg) = rest.split_once(':').unwrap_or((rest, ""));
    Some(json!({
        "time": format!("{date} {time}"),
        "pid": pid.parse::<u32>().unwrap_or(0),
        "tid": tid.parse::<u32>().unwrap_or(0),
        "level": level.chars().next().unwrap_or('I'),
        "tag": tag.trim(),
        "msg": msg.trim_start(),
        "raw": line,
    }))
}

fn level_rank(c: char) -> u8 {
    match c.to_ascii_uppercase() {
        'V' => 0,
        'D' => 1,
        'I' => 2,
        'W' => 3,
        'E' => 4,
        'F' | 'A' => 5,
        _ => 2,
    }
}

/// 发起一次 logwatch（本机 adb logcat，或作为客户端连远程电脑的 `/ws/logcat`）
fn start_watch(app: &Arc<AppState>, ph: &Arc<Plugin>, id: &str, p: &Value) -> Result<Value, String> {
    let sub = app.plugins.next_sub();
    let stop = app.plugins.add_watch(sub, &ph.id);
    let (app2, ph2, p2, id2) = (app.clone(), ph.clone(), p.clone(), id.to_string());
    tokio::spawn(async move { watch_loop(app2, ph2, sub, id2, p2, stop).await });
    Ok(json!(sub))
}

/// 日志行的过滤规则（level / tag / grep / 包名对应的进程号）
struct LineFilter {
    min: u8,
    tag: Option<String>,
    grep: Option<String>,
    pids: Vec<String>,
}

impl LineFilter {
    fn from(p: &Value, pids: Vec<String>) -> Self {
        Self {
            min: arg_str(p, "level").and_then(|l| l.chars().next()).map(level_rank).unwrap_or(0),
            tag: arg_str(p, "tag").map(|t| t.to_lowercase()),
            grep: arg_str(p, "grep").map(|g| g.to_lowercase()),
            pids,
        }
    }

    /// 符合规则时返回 `{time, pid, tid, level, tag, msg, raw}`
    fn check(&self, line: &str) -> Option<Value> {
        let v = parse_logline(line)?;
        if v["level"].as_str().and_then(|s| s.chars().next()).map(level_rank).unwrap_or(2) < self.min {
            return None;
        }
        if let Some(t) = &self.tag {
            if !v["tag"].as_str().unwrap_or("").to_lowercase().contains(t.as_str()) {
                return None;
            }
        }
        if let Some(g) = &self.grep {
            if !line.to_lowercase().contains(g.as_str()) {
                return None;
            }
        }
        if !self.pids.is_empty() && !self.pids.contains(&v["pid"].as_u64().unwrap_or(0).to_string()) {
            return None;
        }
        Some(v)
    }
}

async fn watch_loop(app: Arc<AppState>, ph: Arc<Plugin>, sub: u64, id: String, p: Value, stop: tokio::sync::watch::Receiver<bool>) {
    match split_dev(&id) {
        Some((peer_id, raw)) => remote_watch(app.clone(), ph.clone(), sub, peer_id.to_string(), raw.to_string(), &p, stop).await,
        None => local_watch(app.clone(), ph.clone(), sub, id, &p, stop).await,
    }
    app.plugins.remove_watch(sub);
}

/// 本机设备：直接跑 adb logcat
async fn local_watch(app: Arc<AppState>, ph: Arc<Plugin>, sub: u64, serial: String, p: &Value, mut stop: tokio::sync::watch::Receiver<bool>) {
    let package = arg_str(p, "package").filter(|pkg| logcat::valid_pkg(pkg) && !pkg.contains(':')).map(String::from);
    let pids: Vec<String> = match &package {
        Some(pkg) => adb::run(&app.cfg.adb, &["-s", &serial, "shell", &format!("pidof {pkg}")])
            .await
            .unwrap_or_default()
            .split_whitespace()
            .map(String::from)
            .collect(),
        None => Vec::new(),
    };
    let filter = LineFilter::from(p, pids);
    let mut child = match adb::command(&app.cfg.adb)
        .args(["-s", &serial, "logcat", "-v", "threadtime"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            app.plugins.console(&ph, "error", &tr!("无法启动 adb logcat：{e}", "Cannot start adb logcat: {e}"), None);
            return;
        }
    };
    let Some(stdout) = child.stdout.take() else { return };
    let mut rd = BufReader::new(stdout);
    let mut bucket = Bucket::new(200.0);
    let mut buf: Vec<u8> = Vec::new();
    let mut next_id = 1u64 << 62;
    loop {
        tokio::select! {
            r = rd.read_until(b'\n', &mut buf) => match r {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                    buf.clear();
                    if line.is_empty() || !bucket.allow() {
                        continue;
                    }
                    let Some(v) = filter.check(&line) else { continue };
                    if !send_line(&ph, sub, &mut next_id, v).await {
                        break;
                    }
                }
            },
            _ = stop.changed() => break,
        }
    }
    let _ = child.kill().await;
}

/// 远程设备：连接 B 的 `/ws/logcat?serial=<原始 serial>`，把 `{"type":"lines"}` 里的日志行按原规则过滤后转给插件
async fn remote_watch(
    app: Arc<AppState>,
    ph: Arc<Plugin>,
    sub: u64,
    peer_id: String,
    raw: String,
    p: &Value,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let package = arg_str(p, "package").filter(|pkg| logcat::valid_pkg(pkg) && !pkg.contains(':')).map(String::from);
    let filter = LineFilter::from(p, Vec::new());
    let Some(peer) = app.peers.get(&peer_id).await else {
        let e = tr!("远程电脑不存在", "Remote computer not found");
        app.plugins.console(&ph, "error", &e, None);
        return;
    };
    let path = format!("/ws/logcat?serial={}", util::url_encode(&raw));
    let mut ws = match crate::peers::ws_connect(&peer, &path).await {
        Ok(w) => w,
        Err(e) => {
            app.plugins.console(&ph, "error", &e, None);
            return;
        }
    };
    // 远程电脑上的日志流也支持按包名过滤（它自己会跟踪进程号）
    if let Some(pkg) = &package {
        let _ = ws.send(TMsg::Text(json!({"t": "pkg", "name": pkg}).to_string().into())).await;
    }
    let mut bucket = Bucket::new(200.0);
    let mut next_id = 1u64 << 62;
    loop {
        tokio::select! {
            m = ws.next() => match m {
                Some(Ok(TMsg::Text(t))) => {
                    let text = t.as_str();
                    let Ok(v) = serde_json::from_str::<Value>(text) else { continue };
                    if v.get("type").and_then(|x| x.as_str()) != Some("lines") {
                        continue;
                    }
                    for line in v.get("l").and_then(|x| x.as_array()).into_iter().flatten().filter_map(|l| l.as_str()) {
                        if line.is_empty() || !bucket.allow() {
                            continue;
                        }
                        let Some(lv) = filter.check(line) else { continue };
                        if !send_line(&ph, sub, &mut next_id, lv).await {
                            return;
                        }
                    }
                }
                Some(Ok(TMsg::Ping(d))) => {
                    let _ = ws.send(TMsg::Pong(d)).await;
                }
                Some(Ok(TMsg::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => continue,
            },
            _ = stop.changed() => break,
        }
    }
    let _ = ws.close(None).await;
}

/// 把一条日志行发给插件宿主
async fn send_line(ph: &Arc<Plugin>, sub: u64, next_id: &mut u64, line: Value) -> bool {
    let Some(h) = ph.host() else { return false };
    let id = *next_id;
    *next_id += 1;
    h.out.send(json!({"t": "req", "id": id, "m": "logline", "p": {"sub": sub, "line": line}})).await.is_ok()
}

pub(super) fn stop_watch(app: &AppState, sub: u64) {
    if let Some(w) = app.plugins.watches.lock().unwrap().remove(&sub) {
        let _ = w.stop.send(true);
    }
}

/// 插件停止时清理它建立的所有 logwatch
pub(super) fn stop_watches(app: &AppState, ph: &Plugin) {
    let ids: Vec<u64> = app.plugins.watches.lock().unwrap().iter().filter(|(_, w)| w.plugin == ph.id).map(|(id, _)| *id).collect();
    for id in ids {
        stop_watch(app, id);
    }
}

/// 移除一个 adb 转发（插件停止时用）
pub(super) async fn remove_forward(adb_path: &std::path::Path, plugin: &str, f: &Forward) {
    let args = ["-s", f.serial.as_str(), if f.reverse { "reverse" } else { "forward" }, "--remove", f.local.as_str()];
    if let Err(e) = adb::run_timeout(adb_path, &args, Duration::from_secs(15)).await {
        tracing::warn!("plugin {plugin}: cannot remove forward {}: {e}", f.local);
    }
}

// ---------------------------------------------------------------- 小工具

/// 图库里的文件（带本机路径）：`[{name, kind, device, size, time, path}]`
pub(super) async fn media_items(app: &AppState) -> Result<Vec<Value>, String> {
    let v = media::list(app).await?;
    let dir = std::path::PathBuf::from(v["dir"].as_str().unwrap_or(""));
    Ok(v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|it| {
            let name = it["name"].as_str().unwrap_or("");
            json!({"name": name, "kind": it["kind"], "device": it["device"], "size": it["size"], "time": it["time"], "path": dir.join(name).to_string_lossy()})
        })
        .collect())
}

fn arg_str<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())
}

/// 主进程侧再检查一遍插件给的本机路径（宿主已检查过，这里防止绕过）：
/// 必须是绝对路径、不含 `..`；插件数据目录可读写，插件目录与 `.tmp` 只读，其余需要 fs / fs.read 权限
fn local_path(app: &AppState, ph: &Plugin, path: &str, write: bool) -> Result<String, String> {
    let p = std::path::Path::new(path);
    if !p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(tr!("本机路径必须是绝对路径且不能包含 ..", "Local paths must be absolute and must not contain .."));
    }
    let root = app.plugins.root();
    let own_rw = p.starts_with(root.join(".data").join(&ph.id));
    let own_ro = p.starts_with(&ph.dir) || p.starts_with(root.join(".tmp"));
    let ok = if write { own_rw || ph.has_perm("fs") } else { own_rw || own_ro || ph.has_perm("fs.read") };
    if !ok {
        let need = if write { "fs" } else { "fs.read" };
        return Err(tr!("访问 {path} 需要 {need} 权限", "Accessing {path} requires the {need} permission"));
    }
    Ok(path.to_string())
}

/// 设备上的路径 / 参数不能以 - 开头（否则会被 adb 当成选项）
fn not_option(v: &str) -> Result<(), String> {
    if v.starts_with('-') {
        return Err(tr!("参数不能以 - 开头：{v}", "Argument must not start with -: {v}"));
    }
    Ok(())
}

fn need(ph: &Plugin, perm: &str) -> Result<(), String> {
    if ph.has_perm(perm) {
        Ok(())
    } else {
        Err(tr!("插件没有 {perm} 权限", "The plugin lacks the {perm} permission"))
    }
}

/// 取设备参数并检查它是否在插件的 match 范围内
async fn device(app: &AppState, ph: &Plugin, p: &Value) -> Result<String, String> {
    let serial = arg_str(p, "serial").ok_or_else(|| tr!("缺少 serial", "Missing serial"))?.to_string();
    if !app.plugins.device_allowed(app, ph, &serial).await {
        return Err(tr!("设备 {serial} 不在插件的 match 范围内", "Device {serial} is outside the plugin's match rules"));
    }
    Ok(serial)
}

fn pkg_arg(p: &Value) -> Result<String, String> {
    let pkg = arg_str(p, "package").ok_or_else(|| tr!("缺少 package", "Missing package"))?;
    if !logcat::valid_pkg(pkg) || pkg.contains(':') {
        return Err(tr!("包名格式不正确", "Invalid package name"));
    }
    Ok(pkg.to_string())
}

/// `0` / `"0"` / `"tcp:0"` → 0
fn port_of(v: Option<&Value>) -> u16 {
    match v {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0).min(65535) as u16,
        Some(Value::String(s)) => s.trim().trim_start_matches("tcp:").parse().unwrap_or(0),
        _ => 0,
    }
}

/// 找一个空闲的本机端口（adb reverse 不支持自动分配）
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).unwrap_or(0)
}

/// 执行设备端 shell，返回 (stdout+stderr, 退出码)
async fn shell_out(app: &AppState, serial: &str, cmd: &str, timeout: Duration) -> Result<(String, i32), String> {
    let fut = adb::command(&app.cfg.adb).args(["-s", serial, "shell", cmd]).kill_on_drop(true).output();
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(o)) => {
            let mut out = String::from_utf8_lossy(&o.stdout).into_owned();
            out.push_str(&String::from_utf8_lossy(&o.stderr));
            Ok((out, o.status.code().unwrap_or(-1)))
        }
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => {
            let cmd = cmd.chars().take(80).collect::<String>();
            Err(tr!("命令超时：{cmd}", "Command timed out: {cmd}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing() {
        // 不含 `/`：本机执行
        assert!(check_route("shell", "emulator-5554").is_ok());
        assert!(check_route("forward", "emulator-5554").is_ok());
        // 含 `/`：远程 devop；端口转发不支持
        assert!(check_route("shell", "p1/emulator-5554").is_ok());
        assert!(check_route("push", "p1/emulator-5554").is_ok());
        assert!(check_route("logwatch", "p1/emulator-5554").is_ok());
        assert!(check_route("forward", "p1/emulator-5554").is_err());
        assert!(check_route("unforward", "p1/emulator-5554").is_err());
        assert!(check_route("tcpip", "p1/emulator-5554").is_err());
        assert!(check_route("ui", "p1/emulator-5554").is_ok());
        assert!(local_only("forward") && local_only("unforward") && local_only("tcpip") && !local_only("shell"));
    }

    #[test]
    fn permissions() {
        assert_eq!(perm_for("prop"), None);
        assert_eq!(perm_for("hasApp"), None);
        assert_eq!(perm_for("badge"), None);
        assert_eq!(perm_for("shell"), Some("device.shell"));
        assert_eq!(perm_for("push"), Some("device.files"));
        assert_eq!(perm_for("pull"), Some("device.files"));
        assert_eq!(perm_for("install"), Some("device.install"));
        assert_eq!(perm_for("logwatch"), Some("device.logs"));
        assert_eq!(perm_for("screenshot"), Some("device.screen"));
        assert_eq!(perm_for("forward"), Some("device.forward"));
        assert_eq!(perm_for("ui"), Some("device.screen"));
        assert_eq!(perm_for("files"), Some("device.files"));
        assert_eq!(perm_for("rm"), Some("device.files"));
        assert_eq!(perm_for("clipboard"), Some("device.input"));
        assert_eq!(perm_for("cmd"), Some("device.input"));
        assert_eq!(perm_for("tcpip"), Some("device.connect"));
        assert_eq!(perm_for("mirror"), None);
        for m in ["ui", "files", "mkdir", "rename", "rm", "clipboard", "cmd", "tcpip", "mirror"] {
            assert!(is_device_method(m), "{m}");
        }
    }

    #[test]
    fn audit_fields() {
        let q = json!({"cmd": "echo hi", "timeout": 5, "serial": "x", "local": "/a", "remote": "/b"});
        let d = audit_json("shell", "p1/dev", &q).unwrap();
        assert_eq!(d["serial"], json!("p1/dev"));
        assert_eq!(d["cmd"], json!("echo hi"));
        let d = audit_json("push", "dev", &q).unwrap();
        assert_eq!(d["from"], json!("/a"));
        assert_eq!(d["to"], json!("/b"));
        let d = audit_json("pull", "dev", &q).unwrap();
        assert_eq!(d["from"], json!("/b"));
        assert_eq!(d["to"], json!("/a"));
        assert!(audit_json("prop", "dev", &q).is_none());
        assert!(audit_json("logcat", "dev", &q).is_none());
        assert!(audit_json("ui", "dev", &q).is_none());
        // 剪贴板只记是否写入，不记内容
        let d = audit_json("clipboard", "dev", &json!({"text": "secret"})).unwrap();
        assert_eq!(d["set"], json!(true));
        assert!(!d.to_string().contains("secret"));
        let d = audit_json("rm", "dev", &json!({"paths": ["/sdcard/a"]})).unwrap();
        assert_eq!(d["path"], json!(["/sdcard/a"]));
    }

    #[test]
    fn logline_parsing() {
        let v = parse_logline("08-01 12:00:00.123  1234  5678 I ActivityManager: Start proc com.x").unwrap();
        assert_eq!(v["time"], json!("08-01 12:00:00.123"));
        assert_eq!(v["pid"], json!(1234));
        assert_eq!(v["tid"], json!(5678));
        assert_eq!(v["level"], json!("I"));
        assert_eq!(v["tag"], json!("ActivityManager"));
        assert_eq!(v["msg"], json!("Start proc com.x"));
        assert!(v["raw"].as_str().unwrap().starts_with("08-01"));

        // 格式不对的行
        assert!(parse_logline("").is_none());
        assert!(parse_logline("garbage only").is_none());
    }

    #[test]
    fn levels() {
        assert!(level_rank('V') < level_rank('I'));
        assert!(level_rank('I') < level_rank('W'));
        assert!(level_rank('w') == level_rank('W'));
        assert_eq!(level_rank('E'), 4);
    }

    #[test]
    fn ports() {
        assert_eq!(port_of(Some(&json!(0))), 0);
        assert_eq!(port_of(Some(&json!("8080"))), 8080);
        assert_eq!(port_of(Some(&json!("tcp:1234"))), 1234);
        assert_eq!(port_of(None), 0);
        let p = free_port();
        assert!(p > 0);
    }

    #[test]
    fn rate_bucket() {
        let mut b = Bucket::new(200.0);
        let mut ok = 0;
        for _ in 0..300 {
            if b.allow() {
                ok += 1;
            }
        }
        assert!(ok >= 200 && ok < 300, "{ok}");
    }
}
