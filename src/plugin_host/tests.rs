//! 插件宿主单元测试：调度、协程、CPU/内存限制、沙箱、即时模式面板、JSON。

use super::*;
use std::cell::RefCell;
use std::sync::mpsc::channel;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("droidbridge-ph-{}-{}-{}", std::process::id(), tag, nanos));
    std::fs::create_dir_all(&dir).ok();
    dir
}

struct Harness {
    driver: Driver,
    out: Rc<RefCell<Vec<J>>>,
    dir: PathBuf,
    /// 已经回复过的 call id
    answered: std::collections::HashSet<u64>,
    /// 后台线程（子进程等）投递给驱动的消息
    rx: std::sync::mpsc::Receiver<Msg>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Harness {
    fn new(tag: &str, main_lua: &str, cfg: HostConfig) -> Self {
        let dir = temp_dir(tag);
        let data = dir.join("data");
        std::fs::create_dir_all(&data).ok();
        std::fs::write(dir.join("main.lua"), main_lua).ok();
        let out = Rc::new(RefCell::new(Vec::new()));
        let sink: Sink = {
            let out = out.clone();
            Rc::new(move |line: &str| {
                if let Ok(v) = serde_json::from_str::<J>(line) {
                    out.borrow_mut().push(v);
                }
            })
        };
        let (tx, rx) = channel::<Msg>();
        let mut driver = Driver::new(cfg, tx, sink);
        let init = json!({
            "t": "init",
            "plugin": {
                "id": "test-plugin",
                "name": "Test",
                "version": "1.0.0",
                "dir": dir.to_string_lossy(),
                "dataDir": data.to_string_lossy(),
                "main": "main.lua",
            },
            "granted": ["exec", "net", "clipboard", "device.logs", "device.screen", "fs"],
            "settings": {"luaDir": "D:/x"},
            "lang": "zh",
            "node": "PC",
        });
        driver.on_line(&init.to_string());
        let mut h = Self { driver, out, dir, answered: Default::default(), rx };
        h.settle(&devices());
        h
    }

    /// 处理后台线程投递的消息，直到 cond 成立或超时
    fn wait_msgs(&mut self, secs: f64, cond: impl Fn(&[J]) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs_f64(secs);
        while Instant::now() < end {
            match self.rx.recv_timeout(Duration::from_millis(20)) {
                Ok(Msg::Proc { id, kind, data, code }) => self.driver.on_proc(id, kind, data, code),
                Ok(Msg::Bg { id, ok }) => self.driver.on_bg(id, ok),
                _ => {}
            }
            self.settle(&devices());
            if cond(&self.out.borrow()) {
                return true;
            }
        }
        false
    }

    fn pump(&mut self) {
        // 两次：第一次可能产生新协程，第二次运行它们
        self.driver.pump();
        self.driver.pump();
    }

    fn take(&self) -> Vec<J> {
        std::mem::take(&mut *self.out.borrow_mut())
    }

    /// 处理所有未回复的 call，直到没有等待
    fn settle(&mut self, devs: &J) {
        for _ in 0..64 {
            self.pump();
            let pending: Vec<J> = {
                let out = self.out.borrow();
                out.iter()
                    .filter(|v| v.get("t").and_then(|t| t.as_str()) == Some("call"))
                    .filter(|v| {
                        let id = v.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
                        !self.answered.contains(&id)
                    })
                    .cloned()
                    .collect()
            };
            if pending.is_empty() {
                return;
            }
            for c in pending {
                let id = c.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
                self.answered.insert(id);
                let m = c.get("m").and_then(|x| x.as_str()).unwrap_or("");
                let ok = match m {
                    "devices" => devs.clone(),
                    "prop" => J::String("prop-value".into()),
                    "shell" => json!({"out": "shell-out", "code": 0}),
                    "ui" => json!({"width": 1080, "height": 2400, "nodes": [
                        {"text": "", "desc": "", "id": "", "class": "FrameLayout", "x": 540, "y": 1200},
                        {"text": "登录", "desc": "", "id": "com.x:id/login", "class": "Button", "x": 540, "y": 1570, "clickable": true},
                        {"text": "", "desc": "用户名", "id": "com.x:id/user", "class": "EditText", "x": 540, "y": 1250},
                    ]}),
                    "media.item" => json!({"name": c["p"]["name"], "path": "D:/media/x.png", "kind": "shot"}),
                    "plugin.call" => json!({"echo": c["p"]}),
                    _ => J::Bool(true),
                };
                self.driver.on_line(&json!({"t": "ret", "id": id, "ok": ok}).to_string());
            }
        }
    }

    fn req(&mut self, m: &str, p: J) -> J {
        self.req_with_devs(m, p, &devices())
    }

    fn req_with_devs(&mut self, m: &str, p: J, devs: &J) -> J {
        self.take();
        let line = json!({"t": "req", "id": 7, "m": m, "p": p}).to_string();
        self.driver.on_line(&line);
        self.settle(devs);
        let lines = self.out.borrow().clone();
        let mut res = J::Null;
        for v in &lines {
            if v.get("t").and_then(|t| t.as_str()) == Some("res") && v.get("id").and_then(|i| i.as_u64()) == Some(7) {
                res = v.clone();
            }
        }
        res
    }

    fn logs(&self) -> Vec<(String, String, Option<String>)> {
        self.out
            .borrow()
            .iter()
            .filter(|v| v.get("t").and_then(|t| t.as_str()) == Some("log"))
            .map(|v| {
                (
                    v.get("level").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    v.get("msg").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    v.get("src").and_then(|x| x.as_str()).map(|s| s.to_string()),
                )
            })
            .collect()
    }

    fn log_text(&self) -> String {
        self.logs().iter().map(|(_, m, _)| m.clone()).collect::<Vec<_>>().join("\n")
    }

    fn has_line(&self, t: &str) -> bool {
        self.out.borrow().iter().any(|v| v.get("t").and_then(|x| x.as_str()) == Some(t))
    }
}

fn devices() -> J {
    json!([{"serial": "S1", "model": "Pixel", "name": "Phone", "state": "device"}])
}

#[test]
fn dispatch_call_and_menu() {
    let mut h = Harness::new(
        "menu",
        r#"
db.menu("tools", "Count", function()
  local ds = db.devices()
  print("count=" .. #ds)
  return ds[1] and ds[1].serial or "none"
end)
"#,
        HostConfig::default(),
    );
    assert!(h.has_line("ready"), "应发送 ready");
    // 注册贡献
    let all = h.take();
    assert!(all.iter().any(|v| v["t"] == "call" && v["m"] == "register"), "应调用 register");
    // 调用菜单
    let res = h.req("menu", json!({"id": "tools:Count", "ctx": {}}));
    assert_eq!(res["ok"], json!("S1"));
    let logs = h.log_text();
    assert!(logs.contains("count=1"), "日志: {logs}");
}

#[test]
fn coroutine_wrap_passes_scheduler() {
    let mut h = Harness::new(
        "wrap",
        r#"
db.menu("tools", "Wrap", function()
  local f = coroutine.wrap(function()
    local d = db.devices()[1]
    local s = db.device(d.serial)
    return s:prop("ro.product.model") .. ":" .. s.serial
  end)
  return f()
end)
"#,
        HostConfig::default(),
    );
    let res = h.req("menu", json!({"id": "tools:Wrap", "ctx": {}}));
    // prop 用默认回复 "prop-value"
    assert_eq!(res["ok"], json!("prop-value:S1"));
}

#[test]
fn cpu_timeout() {
    let cfg = HostConfig {
        cpu_limit: Duration::from_millis(80),
        mem_limit: 64 * 1024 * 1024,
        instr_step: 1000,
    };
    let h = Harness::new("cpu", "local x = 0\nwhile true do x = x + 1 end", cfg);
    let logs = h.log_text().to_lowercase();
    assert!(logs.contains("script timeout"), "日志: {logs}");
    assert!(h.has_line("ready"), "main.lua 失败也要发 ready");
}

#[test]
fn memory_limit() {
    let cfg = HostConfig {
        cpu_limit: Duration::from_secs(5),
        mem_limit: 4 * 1024 * 1024,
        instr_step: 10_000,
    };
    let h = Harness::new(
        "mem",
        r#"
local t = {}
for i = 1, 200000 do t[i] = string.rep("x", 64) end
"#,
        cfg,
    );
    let logs = h.log_text().to_lowercase();
    assert!(logs.contains("memory"), "应触发内存上限，日志: {logs}");
    assert!(h.has_line("ready"), "main.lua 失败也要发 ready");
}

#[test]
fn sandbox_has_no_io_os_execute_debug() {
    let h = Harness::new(
        "sandbox",
        r#"
if io ~= nil then error("io leaked") end
if debug ~= nil then error("debug leaked") end
if package ~= nil then error("package leaked") end
if os.execute ~= nil then error("os.execute leaked") end
if os.remove ~= nil then error("os.remove leaked") end
if loadfile ~= nil or dofile ~= nil then error("loadfile leaked") end
if type(require) ~= "function" then error("require missing") end
if __host ~= nil then error("__host leaked") end
if type(os.time) ~= "function" then error("os.time missing") end
local f = load("return 1 + 1")
if f() ~= 2 then error("load broken") end
print("sandbox ok")
"#,
        HostConfig::default(),
    );
    let logs = h.log_text();
    assert!(logs.contains("sandbox ok"), "日志: {logs}");
    assert!(!logs.to_lowercase().contains("leaked"), "日志: {logs}");
}

#[test]
fn immediate_mode_panel_event_round() {
    let mut h = Harness::new(
        "panel",
        r#"
db.panel("p1", { title = "测试面板", dock = "device" }, function(ui, dev)
  local on = ui.toggle("开关", false)
  if ui.button("点我") then print("clicked") end
  ui.label("on=" .. tostring(on))
end)
"#,
        HostConfig::default(),
    );
    h.take();
    let res = h.req(
        "panel",
        json!({"id": "p1", "serial": "S1", "event": {"id": "button:点我", "value": true}}),
    );
    let logs = h.log_text();
    assert!(logs.contains("clicked"), "事件轮应触发按钮，日志: {logs}");
    assert_eq!(res["ok"]["title"], json!("测试面板"));
    let tree = res["ok"]["tree"].as_array().expect("控件树");
    assert_eq!(tree.len(), 3);
    assert_eq!(tree[0]["w"], json!("toggle"));
    assert_eq!(tree[0]["value"], json!(false), "绘制轮 toggle 应回到 false");
    assert_eq!(tree[1]["w"], json!("button"));
    assert_eq!(tree[2]["w"], json!("label"));
    assert_eq!(tree[2]["text"], json!("on=false"));
    assert_eq!(tree[0]["id"], json!("toggle:开关"));
}

#[test]
fn panel_render_forbids_blocking() {
    let mut h = Harness::new(
        "panel-block",
        r#"
db.panel("p2", { title = "T" }, function(ui, dev)
  dev:prop("x")
  ui.label("never")
end)
"#,
        HostConfig::default(),
    );
    let res = h.req("panel", json!({"id": "p2", "serial": "S1"}));
    let text = res["err"].as_str().unwrap_or("");
    assert!(text.contains("panel render must not call blocking APIs"), "err: {text}");
}

#[test]
fn json_and_store_roundtrip() {
    let h = Harness::new(
        "json",
        r#"
assert(db.json.encode({}) == "{}")
assert(db.json.encode({1, 2, 3}) == "[1,2,3]")
local t = db.json.decode('{"a":[1,2],"b":"x","c":null}')
assert(t.a[1] == 1 and t.a[2] == 2 and t.b == "x" and t.c == nil)
assert(db.json.encode(db.json.decode('{"k":true}')) == '{"k":true}')
db.store.set("k", {n = 7})
local v = db.store.get("k")
assert(v.n == 7)
db.fs.write("note.txt", "hello")
assert(db.fs.read("note.txt") == "hello")
assert(db.fs.exists("note.txt"))
print("json ok")
"#,
        HostConfig::default(),
    );
    let logs = h.log_text();
    assert!(logs.contains("json ok"), "日志: {logs}");
    // store 文件落到 dataDir
    assert!(h.dir.join("data/store.json").exists());
    assert!(h.dir.join("data/note.txt").exists());
}

#[test]
fn settings_and_events() {
    let mut h = Harness::new(
        "settings",
        r#"
db.on("settings.changed", function(values)
  print("changed=" .. tostring(values.luaDir))
end)
assert(db.settings.luaDir == "D:/x")
"#,
        HostConfig::default(),
    );
    let res = h.req("settings", json!({"values": {"luaDir": "D:/y"}}));
    assert_eq!(res["ok"], json!(true));
    let logs = h.log_text();
    assert!(logs.contains("changed=D:/y"), "日志: {logs}");
}

#[test]
fn errors_are_logged_with_src_and_host_keeps_running() {
    let mut h = Harness::new(
        "error",
        r#"
db.menu("tools", "Boom", function()
  error("boom")
end)
"#,
        HostConfig::default(),
    );
    let res = h.req("menu", json!({"id": "tools:Boom", "ctx": {}}));
    let text = res["err"].as_str().unwrap_or("");
    assert!(text.contains("boom"), "err: {text}");
    let logs = h.logs();
    let err = logs
        .iter()
        .find(|(level, msg, _)| level == "error" && msg.contains("boom"))
        .expect("应有 error 日志");
    assert_eq!(err.2.as_deref(), Some("main.lua:3"), "src: {:?}", err.2);
    // 宿主仍在运行
    let ping = h.req("ping", json!({}));
    assert_eq!(ping["ok"], json!(true));
}

#[test]
fn event_listener_spawns_coroutine_with_api_calls() {
    let mut h = Harness::new(
        "event",
        r#"
db.on("device.connected", function(dev)
  local d = db.devices()[1]
  print("spawned=" .. tostring(dev.serial) .. "/" .. tostring(d.serial))
end)
"#,
        HostConfig::default(),
    );
    let res = h.req("event", json!({"name": "device.connected", "args": [{"serial": "S1"}]}));
    assert_eq!(res["ok"], json!(true));
    let logs = h.log_text();
    assert!(logs.contains("spawned=S1/S1"), "日志: {logs}");
}

#[test]
fn timers_fire_and_cancel() {
    let mut h = Harness::new(
        "timer",
        r#"
local t = db.every(0.02, function() print("tick") end)
db.after(0.01, function() print("after") end)
db.after(0.10, function()
  t:cancel()
  print("cancelled")
end)
"#,
        HostConfig::default(),
    );
    // 分几次推进，避免定时器在同一轮里互相影响
    std::thread::sleep(Duration::from_millis(25));
    h.pump();
    assert!(h.log_text().contains("after"), "日志: {}", h.log_text());
    std::thread::sleep(Duration::from_millis(60));
    h.pump();
    assert!(h.log_text().contains("tick"), "日志: {}", h.log_text());
    std::thread::sleep(Duration::from_millis(150));
    h.pump();
    assert!(h.log_text().contains("cancelled"), "日志: {}", h.log_text());
}

#[test]
fn overdue_timers_fire_in_due_order() {
    // 宿主被卡住一会儿后，多个已到期的定时器要按到期顺序触发
    let mut h = Harness::new(
        "timer-order",
        r#"
db.after(0.05, function() print("third") end)
db.after(0.01, function() print("first") end)
db.after(0.03, function() print("second") end)
"#,
        HostConfig::default(),
    );
    std::thread::sleep(Duration::from_millis(120));
    h.pump();
    let text = h.log_text();
    let pos = |s: &str| text.find(s).unwrap_or(usize::MAX);
    assert!(pos("first") < pos("second") && pos("second") < pos("third"), "日志: {text}");
}

#[test]
fn null_fields_are_encoded_as_json_null() {
    let mut h = Harness::new(
        "null",
        r#"
db.menu("tools", "Null", function()
  db.overlay(nil, nil)
  db.badge(nil, nil)
end)
"#,
        HostConfig::default(),
    );
    let res = h.req("menu", json!({"id": "tools:Null", "ctx": {}}));
    assert_eq!(res["ok"], J::Null);
    let out = h.out.borrow().clone();
    let overlay = out
        .iter()
        .find(|v| v["t"] == "call" && v["m"] == "overlay")
        .expect("应有 overlay call");
    assert_eq!(overlay["p"]["shapes"], J::Null);
    let badge = out
        .iter()
        .find(|v| v["t"] == "call" && v["m"] == "badge")
        .expect("应有 badge call");
    assert_eq!(badge["p"]["text"], J::Null);
}

#[test]
fn ui_find_helpers() {
    let mut h = Harness::new(
        "uifind",
        r#"
db.menu("device", "Find", function(dev)
  local a = dev:find("登录")
  local b = dev:find("user")
  local c = dev:find({ id = "com.x:id/login", clickable = true })
  local d = dev:find(function(n) return n.class == "EditText" end)
  local e = dev:find("不存在")
  local w = dev:waitFor("用户名", 1)
  return table.concat({ a.y, b.desc, c.text, d.id, tostring(e), w.y }, "|")
end)
"#,
        HostConfig::default(),
    );
    let res = h.req("menu", json!({"id": "device:Find", "ctx": {"serial": "S1"}}));
    assert_eq!(res["ok"], json!("1570|用户名|登录|com.x:id/user|nil|1250"), "{res:?}");
}

#[test]
fn device_helpers_send_calls() {
    let mut h = Harness::new(
        "devhelp",
        r#"
db.menu("device", "Do", function(dev)
  dev:longpress(10, 20)
  dev:remove({ "/sdcard/a", "/sdcard/b" })
  dev:remove("/sdcard/c")
  dev:setClipboard("hi")
  dev:getClipboard()
  dev:cmd("notif")
  dev:mirror()
end)
"#,
        HostConfig::default(),
    );
    h.req("menu", json!({"id": "device:Do", "ctx": {"serial": "S1"}, "client": "c1"}));
    let out = h.out.borrow().clone();
    let calls: Vec<&J> = out.iter().filter(|v| v["t"] == "call").collect();
    let find = |m: &str| calls.iter().filter(|v| v["m"] == m).cloned().collect::<Vec<_>>();
    let input = find("input");
    assert_eq!(input[0]["p"]["kind"], "swipe");
    assert_eq!((input[0]["p"]["x"].clone(), input[0]["p"]["x2"].clone(), input[0]["p"]["duration"].clone()), (json!(10), json!(10), json!(800)));
    let rm = find("rm");
    assert_eq!(rm[0]["p"]["paths"], json!(["/sdcard/a", "/sdcard/b"]));
    assert_eq!(rm[1]["p"]["path"], "/sdcard/c");
    let clip = find("clipboard");
    assert_eq!(clip[0]["p"]["text"], "hi");
    assert!(clip[1]["p"].get("text").is_none(), "读取时不带 text");
    assert_eq!(find("cmd")[0]["p"]["name"], "notif");
    assert_eq!(find("mirror")[0]["p"]["client"], "c1", "打开投屏发给发起操作的界面");
}

#[test]
fn media_and_logcat_menus() {
    let mut h = Harness::new(
        "menus2",
        r#"
db.menu("media", "Up", function(item) return item.name .. "@" .. item.path end)
db.menu("logcat", "Line", function(dev, line) return dev.serial .. ":" .. line end)
"#,
        HostConfig::default(),
    );
    let res = h.req("menu", json!({"id": "media:Up", "ctx": {"name": "a.png"}}));
    assert_eq!(res["ok"], json!("a.png@D:/media/x.png"));
    let res = h.req("menu", json!({"id": "logcat:Line", "ctx": {"serial": "S1", "line": "E boom"}}));
    assert_eq!(res["ok"], json!("S1:E boom"));
}

#[test]
fn exports_between_plugins() {
    let mut h = Harness::new(
        "export",
        r#"
db.export("sum", function(args, from) return args.a + args.b .. "/" .. from end)
db.menu("tools", "Call", function() return db.plugins.call("other", "x", { n = 1 }) end)
"#,
        HostConfig::default(),
    );
    let all = h.take();
    assert!(all.iter().any(|v| v["m"] == "register" && v["p"]["kind"] == "export" && v["p"]["def"]["name"] == "sum"), "{all:?}");
    let res = h.req("export", json!({"name": "sum", "args": {"a": 1, "b": 2}, "from": "caller"}));
    assert_eq!(res["ok"], json!("3/caller"));
    let res = h.req("export", json!({"name": "nope", "args": {}}));
    assert!(res["err"].is_string());
    let res = h.req("menu", json!({"id": "tools:Call", "ctx": {}}));
    assert_eq!(res["ok"]["echo"], json!({"plugin": "other", "name": "x", "args": {"n": 1}}));
}

#[test]
fn ui_open_url_carries_client() {
    let mut h = Harness::new(
        "uiopen",
        r#"
db.menu("tools", "Open", function()
  db.ui.openUrl("http://example.com/x", { name = "w1", title = "T", mode = "window" })
  return pcall(db.ui.openUrl, "file:///etc/passwd")
end)
"#,
        HostConfig::default(),
    );
    h.req("menu", json!({"id": "tools:Open", "ctx": {}, "client": "c1"}));
    let out = h.out.borrow().clone();
    let calls: Vec<_> = out.iter().filter(|v| v["t"] == "call" && v["m"] == "uiopen").collect();
    assert_eq!(calls.len(), 1, "非 http 地址应在宿主里就被拒绝: {out:?}");
    assert_eq!(calls[0]["p"]["url"], "http://example.com/x");
    assert_eq!(calls[0]["p"]["name"], "w1");
    assert_eq!(calls[0]["p"]["client"], "c1");
}

#[test]
fn overlay_size_is_passed_through() {
    let mut h = Harness::new(
        "ovsize",
        r#"
db.menu("device", "Ov", function(dev)
  db.overlay(dev, { { type = "rect", x = 1, y = 2, w = 3, h = 4 } }, { w = 2000, h = 2800 })
  db.overlay(dev, { { type = "rect", x = 1, y = 2, w = 3, h = 4 } }, { w = "bad" })
end)
"#,
        HostConfig::default(),
    );
    h.req("menu", json!({"id": "device:Ov", "ctx": {"serial": "S1"}}));
    let out = h.out.borrow().clone();
    let calls: Vec<_> = out.iter().filter(|v| v["t"] == "call" && v["m"] == "overlay").collect();
    assert_eq!(calls.len(), 2, "{out:?}");
    assert_eq!(calls[0]["p"]["size"], json!({"w": 2000, "h": 2800}));
    assert_eq!(calls[1]["p"]["size"], J::Null);
}

#[test]
fn spawned_coroutine_starts_without_new_messages() {
    let mut h = Harness::new(
        "spawn",
        r#"
db.menu("tools", "SpawnOne", function()
  db.spawn(function()
    local d = db.devices()[1]
    print("child=" .. d.serial)
  end)
  return "parent"
end)
"#,
        HostConfig::default(),
    );
    h.take();
    h.driver
        .on_line(&json!({"t":"req","id":9,"m":"menu","p":{"id":"tools:SpawnOne","ctx":{}}}).to_string());
    // 只驱动一轮：父协程运行并 db.spawn，子协程应在同一轮里跑起来
    h.driver.pump();
    let calls: Vec<J> = h
        .out
        .borrow()
        .iter()
        .filter(|v| v["t"] == "call" && v["m"] == "devices")
        .cloned()
        .collect();
    assert!(!calls.is_empty(), "子协程应在同一轮 pump 里开始执行");
}

#[test]
fn unload_event_fires_on_shutdown() {
    let mut h = Harness::new(
        "unload",
        r#"
db.on("plugin.unload", function() print("bye-now") end)
"#,
        HostConfig::default(),
    );
    h.take();
    h.driver.shutdown();
    let logs = h.log_text();
    assert!(logs.contains("bye-now"), "日志: {logs}");
}

#[test]
fn unload_event_fires_even_if_boot_is_blocked() {
    // boot 卡在等主进程回复 register 时 stdin 关闭
    let dir = temp_dir("unload2");
    let data = dir.join("data");
    std::fs::create_dir_all(&data).ok();
    std::fs::write(
        dir.join("main.lua"),
        "db.on(\"plugin.unload\", function() print(\"bye-blocked\") end)
",
    )
    .ok();
    let out = Rc::new(RefCell::new(Vec::new()));
    let sink: Sink = {
        let out = out.clone();
        Rc::new(move |line: &str| {
            if let Ok(v) = serde_json::from_str::<J>(line) {
                out.borrow_mut().push(v);
            }
        })
    };
    let (tx, _rx) = channel::<Msg>();
    let mut driver = Driver::new(HostConfig::default(), tx, sink);
    let init = json!({
        "t": "init",
        "plugin": {"id": "p", "name": "P", "version": "1", "dir": dir.to_string_lossy(), "dataDir": data.to_string_lossy(), "main": "main.lua"},
        "granted": [], "settings": {}, "lang": "zh",
    });
    driver.on_line(&init.to_string());
    driver.pump();
    {
        let h = driver.h.as_ref().expect("h");
        assert!(h.get::<Function>("dispatch").is_ok(), "dispatch 缺失");
        assert!(h.get::<Function>("task_from").is_ok(), "task_from 缺失");
    }
    driver.shutdown();
    assert!(
        out.borrow().iter().any(|v| v.get("msg").and_then(|m| m.as_str()) == Some("bye-blocked")),
        "输出: {:?}",
        out.borrow()
    );
    let _ = std::fs::remove_dir_all(&dir);
}


fn logs_of(out: &[J]) -> Vec<String> {
    out.iter().filter(|v| v["t"] == "log").filter_map(|v| v["msg"].as_str().map(String::from)).collect()
}

#[test]
fn proc_streams_lines_and_exit() {
    // 读 stdin 按行原样输出的程序
    let (prog, args) = if cfg!(windows) { ("findstr", r#"{ "^" }"#) } else { ("cat", "{}") };
    let main = format!(
        r#"
local p = db.proc.start("{prog}", {args}, {{
  onLine = function(line) db.log("line:" .. line) end,
  onExit = function(code) db.log("exit:" .. tostring(code)) end,
}})
p:send("hello")
p:send("中文 world")
db.after(0.3, function() p:close() end)
"#
    );
    let mut h = Harness::new("proc", &main, HostConfig::default());
    let ok = h.wait_msgs(10.0, |out| logs_of(out).iter().any(|l| l.starts_with("exit:")));
    let logs = logs_of(&h.out.borrow());
    assert!(ok, "等不到退出: {logs:?}");
    let lines: Vec<&String> = logs.iter().filter(|l| l.starts_with("line:") || l.starts_with("exit:")).collect();
    assert_eq!(lines, vec!["line:hello", "line:中文 world", "exit:0"], "{logs:?}");
}

#[test]
fn proc_reports_missing_program() {
    let main = r#"
local ok, e = pcall(db.proc.start, "definitely-not-a-program-xyz", {})
db.log("missing:" .. tostring(ok) .. ":" .. tostring(e and e:find("无法启动") ~= nil))
"#;
    let mut h = Harness::new("procmiss", main, HostConfig::default());
    h.settle(&devices());
    let logs = logs_of(&h.out.borrow());
    assert!(logs.iter().any(|l| l == "missing:false:true"), "{logs:?}");
}

#[test]
fn tmp_isolation_blocks_other_plugins_and_directory_listing() {
    let root = temp_dir("tmpiso");
    let pdir = root.join("plug-a");
    let ddir = root.join(".data").join("plug-a");
    let my_tmp = root.join(".tmp").join("plug-a");
    let other_tmp = root.join(".tmp").join("plug-b");
    let up_dir = root.join(".tmp").join("uploads");
    for d in [&pdir, &ddir, &my_tmp, &other_tmp, &up_dir] {
        std::fs::create_dir_all(d).unwrap();
    }
    let my_shot = my_tmp.join("shot-1.png");
    let other_shot = other_tmp.join("shot-2.png");
    let up_file = up_dir.join("up-0123456789abcdef01234567-test.apk");
    std::fs::write(&my_shot, b"mine").unwrap();
    std::fs::write(&other_shot, b"secret").unwrap();
    std::fs::write(&up_file, b"apk").unwrap();

    let (tx, _rx) = channel::<Msg>();
    let sink: Sink = Rc::new(|_| {});
    let ctl = Ctl {
        tx,
        sink,
        info: state::PluginInfo {
            id: "plug-a".into(),
            name: "A".into(),
            version: "1.0".into(),
            dir: pdir.clone(),
            data_dir: ddir.clone(),
            main: "main.lua".into(),
        },
        granted: Default::default(), // 零权限插件
        lang: "zh".into(),
        node: "PC".into(),
        settings: RefCell::new(json!({})),
        client: RefCell::new(None),
        spawn_queue: RefCell::new(Vec::new()),
        unpark_queue: RefCell::new(Vec::new()),
        bg_seq: std::cell::Cell::new(0),
        uid: std::cell::Cell::new(0),
        new_timers: RefCell::new(Vec::new()),
        timer_cancel: RefCell::new(Vec::new()),
        timer_seq: std::cell::Cell::new(0),
        store: RefCell::new(None),
        closed: std::cell::Cell::new(false),
        procs: RefCell::new(Default::default()),
    };

    // 允许读自己的 .tmp/<id>/ 文件与已知路径的上传单文件
    assert!(local::resolve(&ctl, &my_shot.to_string_lossy(), false).is_ok());
    assert!(local::resolve(&ctl, &up_file.to_string_lossy(), false).is_ok());
    // 禁止读其他插件的 .tmp/<other>/，禁止列举 .tmp 或 .tmp/uploads 目录，禁止写 .tmp
    assert!(local::resolve(&ctl, &other_shot.to_string_lossy(), false).is_err());
    assert!(local::resolve(&ctl, &up_dir.to_string_lossy(), false).is_err());
    assert!(local::resolve(&ctl, &root.join(".tmp").to_string_lossy(), false).is_err());
    assert!(local::resolve(&ctl, &my_shot.to_string_lossy(), true).is_err());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn group_control_events_broadcast_and_parallel() {
    let mut h = Harness::new(
        "groupctl",
        r#"
db.on("device.action", function(dev, act)
  if dev.serial == "S1" then
    db.broadcast({ "S2", "p1/S3" }, act)
    print("replicated:" .. act.kind .. ":" .. tostring(act.x) .. "x" .. tostring(act.y))
  end
end)

db.on("device.input", function(dev, raw)
  if dev.serial == "S1" and raw.t == "touch" then
    local d2 = db.device("S2") or { serial = "S2" }
    setmetatable(d2, getmetatable(dev))
    d2:send(raw)
  end
end)

db.menu("device", "Batch", function(dev)
  dev:tap(120, 340, { w = 1080, h = 2400 })
  dev:touch("down", 50, 60, { id = 1, w = 720, h = 1280, p = 0.8 })
  dev:touch(2, -2, 80, 90, 1080, 2400, 1.0)
  dev:scroll(200, 300, 0, -2, { w = 1080, h = 2400 })
  local res, errs = db.parallel({ "A", "B", "C" }, function(item, idx)
    if item == "B" then error("fail-B") end
    local v = dev:prop("ro.serialno")
    return idx .. ":" .. item .. ":" .. v
  end)
  return res[1] .. "|" .. tostring(res[2]) .. "|" .. res[3] .. "|err=" .. tostring(errs and errs[2])
end)
"#,
        HostConfig::default(),
    );

    // 1) 触发 device.action 事件并检查 broadcast 调用
    let r1 = h.req(
        "event",
        json!({"name": "device.action", "args": [{"serial": "S1", "model": "Pixel"}, {"kind": "tap", "x": 540, "y": 1200, "w": 1080, "h": 2400}]}),
    );
    assert_eq!(r1["ok"], json!(true));
    assert!(h.log_text().contains("replicated:tap:540x1200"), "日志: {}", h.log_text());
    {
        let out = h.out.borrow();
        let bc = out.iter().find(|v| v["t"] == "call" && v["m"] == "broadcast").expect("应发出 broadcast call");
        assert_eq!(bc["p"]["serials"], json!(["S2", "p1/S3"]));
        assert_eq!(bc["p"]["ev"]["kind"], json!("tap"));
        assert_eq!(bc["p"]["ev"]["w"], json!(1080));
    }

    // 2) 触发 device.input 实时流事件并检查 dev:send 归一化
    let r2 = h.req(
        "event",
        json!({"name": "device.input", "args": [{"serial": "S1"}, {"t": "touch", "action": 0, "id": -2, "x": 88, "y": 99, "w": 720, "h": 1600, "p": 1.0}]}),
    );
    assert_eq!(r2["ok"], json!(true));
    {
        let out = h.out.borrow();
        let inp = out.iter().find(|v| v["t"] == "call" && v["m"] == "input" && v["p"]["serial"] == "S2").expect("S2 应收到 touch input");
        assert_eq!(inp["p"]["kind"], json!("touch"));
        assert_eq!(inp["p"]["x"], json!(88));
        assert_eq!(inp["p"]["w"], json!(720));
    }

    // 3) 测试带坐标空间的 tap/touch(两种签名)/scroll 以及 db.parallel 并发与错误收集
    let r3 = h.req("menu", json!({"id": "device:Batch", "ctx": {"serial": "S1"}}));
    assert_eq!(r3["ok"], json!("1:A:prop-value|nil|3:C:prop-value|err=main.lua:23: fail-B"), "{r3:?}");
    let out = h.out.borrow().clone();
    let inputs: Vec<&J> = out.iter().filter(|v| v["t"] == "call" && v["m"] == "input").collect();
    assert_eq!(inputs.len(), 4, "{inputs:?}");
    assert_eq!((inputs[0]["p"]["kind"].clone(), inputs[0]["p"]["w"].clone()), (json!("tap"), json!(1080)));
    assert_eq!((inputs[1]["p"]["kind"].clone(), inputs[1]["p"]["action"].clone(), inputs[1]["p"]["id"].clone()), (json!("touch"), json!(0), json!(1)));
    assert_eq!((inputs[2]["p"]["kind"].clone(), inputs[2]["p"]["action"].clone(), inputs[2]["p"]["x"].clone(), inputs[2]["p"]["w"].clone()), (json!("touch"), json!(2), json!(80), json!(1080)));
    assert_eq!((inputs[3]["p"]["kind"].clone(), inputs[3]["p"]["vs"].clone()), (json!("scroll"), json!(-2)));
}

#[test]
fn builtin_group_control_plugin() {
    let mut h = Harness::new("builtin-gc", include_str!("../builtin/group-control/main.lua"), HostConfig::default());
    let last_toggle = |h: &Harness| -> J {
        let out = h.out.borrow();
        out.iter().rev().find(|v| v["m"] == "register" && v["p"]["def"]["id"] == "toggle").cloned().expect("应注册群控按钮")
    };
    assert_eq!(last_toggle(&h)["p"]["def"]["pin"], json!(true));

    // 只有一台时提示再加一台；两台后按钮在两台上都是开启状态，状态栏显示人数
    let r = h.req("menu", json!({"id": "toggle", "ctx": {"serial": "S1"}}));
    assert_eq!(r["ok"], json!("再给至少一台设备打开群控"));
    h.req("menu", json!({"id": "toggle", "ctx": {"serial": "p1/S2"}}));
    assert_eq!(last_toggle(&h)["p"]["def"]["on"], json!(["S1", "p1/S2"]));
    {
        let out = h.out.borrow();
        let st = out.iter().rev().find(|v| v["m"] == "register" && v["p"]["kind"] == "status").unwrap();
        assert_eq!(st["p"]["def"]["text"], json!("群控 2"));
    }

    // 分组交给主程序同步
    {
        let out = h.out.borrow();
        let link = out.iter().rev().find(|v| v["m"] == "link").expect("应调用 link");
        assert_eq!(link["p"]["serials"], json!(["S1", "p1/S2"]));
    }

    // 退出后分组只剩一台
    h.req("menu", json!({"id": "toggle", "ctx": {"serial": "S1"}}));
    let out = h.out.borrow();
    let link = out.iter().rev().find(|v| v["m"] == "link").unwrap();
    assert_eq!(link["p"]["serials"], json!(["p1/S2"]));
}

#[test]
fn fs_walk_and_tar() {
    let long = format!("{}/{}/deep.lua", "d".repeat(60), "e".repeat(60));
    let main = format!(
        r#"
db.fs.write("src/a.lua", "A")
db.fs.write("src/sys/b.lua", "BB")
db.fs.write("src/sys/skip.txt", "x")
db.fs.write("src/.svn/c.lua", "x")
db.fs.write("src/{long}", "C")
db.spawn(function()
  local all = db.fs.walk("src", {{ ext = {{ "lua" }} }})
  table.sort(all, function(x, y) return x.path < y.path end)
  local names = {{}}
  for _, f in ipairs(all) do names[#names + 1] = f.path .. ":" .. f.size end
  db.log("walk:" .. table.concat(names, ","))
  local r = db.fs.tar("src", "out.tar", {{ "a.lua", "sys/b.lua", "{long}" }})
  db.log("tar:" .. r.count .. ":" .. r.size)
  local g = db.fs.tar("src", "out.tgz", {{ "a.lua", "sys/b.lua" }}, {{ gzip = true }})
  local hs = db.fs.hash("src", {{ "a.lua", "missing.lua" }})
  db.log("gz:" .. g.count .. ":" .. tostring(hs["a.lua"]) .. ":" .. tostring(hs["missing.lua"]))
end)
"#
    );
    let mut h = Harness::new("walktar", &main, HostConfig::default());
    let ok = h.wait_msgs(10.0, |out| logs_of(out).iter().any(|l| l.starts_with("gz:")));
    let logs = logs_of(&h.out.borrow());
    assert!(ok, "{logs:?}");
    assert!(logs.contains(&format!("walk:a.lua:1,{long}:1,sys/b.lua:2")), "{logs:?}");
    assert!(logs.contains(&"tar:3:4".to_string()), "{logs:?}");
    assert!(logs.contains(&"gz:2:af63fc4c860222ec:nil".to_string()), "{logs:?}");
    let gz = std::fs::read(h.dir.join("data/out.tgz")).unwrap();
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&gz[..]), &mut raw).unwrap();
    assert_eq!(&raw[512..517], b"a.lua");

    let data = std::fs::read(h.dir.join("data/out.tar")).unwrap();
    let mut names = Vec::new();
    let mut off = 0;
    while off + 512 <= data.len() && data[off] != 0 {
        let hd = &data[off..off + 512];
        let sum: u32 = hd.iter().enumerate().map(|(i, &c)| if (148..156).contains(&i) { 32 } else { c as u32 }).sum();
        let stored = u32::from_str_radix(std::str::from_utf8(&hd[148..154]).unwrap(), 8).unwrap();
        assert_eq!(sum, stored);
        let cstr = |b: &[u8]| String::from_utf8_lossy(&b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]).to_string();
        let (prefix, name) = (cstr(&hd[345..500]), cstr(&hd[..100]));
        names.push(if prefix.is_empty() { name } else { format!("{prefix}/{name}") });
        let size = u64::from_str_radix(cstr(&hd[124..136]).trim(), 8).unwrap() as usize;
        off += 512 + size.div_ceil(512) * 512;
    }
    assert_eq!(names, vec!["d".repeat(60), format!("{}/{}", "d".repeat(60), "e".repeat(60)), "sys".into(), "a.lua".into(), long.clone(), "sys/b.lua".into()]);
}

#[test]
fn http_download_streams_to_file() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let body = vec![7u8; 3 * 1024 * 1024 + 5];
    let served = body.clone();
    std::thread::spawn(move || {
        for (i, s) in listener.incoming().take(2).enumerate() {
            let mut s = s.unwrap();
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
            if i == 0 {
                assert!(req.contains("private-token: abc"), "{req}");
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", served.len());
                let _ = s.write_all(&served);
            } else {
                let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\nConnection: close\r\n\r\nno");
            }
        }
    });
    let main = format!(
        r#"
db.spawn(function()
  local r = db.http.download("http://127.0.0.1:{port}/a.apk", "dl/a.apk", {{ headers = {{ ["PRIVATE-TOKEN"] = "abc" }} }})
  db.log("ok:" .. r.status .. ":" .. r.size)
  local r2 = db.http.download("http://127.0.0.1:{port}/b.apk", "dl/b.apk")
  db.log("miss:" .. r2.status .. ":" .. tostring(db.fs.exists("dl/b.apk")))
end)
"#
    );
    let mut h = Harness::new("download", &main, HostConfig::default());
    let ok = h.wait_msgs(15.0, |out| logs_of(out).iter().any(|l| l.starts_with("miss:")));
    let logs = logs_of(&h.out.borrow());
    assert!(ok, "{logs:?}");
    assert!(logs.contains(&format!("ok:200:{}", body.len())), "{logs:?}");
    assert!(logs.contains(&"miss:404:false".to_string()), "{logs:?}");
    assert_eq!(std::fs::read(h.dir.join("data/dl/a.apk")).unwrap(), body);
}
