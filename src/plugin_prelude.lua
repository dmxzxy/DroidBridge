-- DroidBridge 插件宿主中的 Lua 侧封装（由 src/plugin_host/api.rs 以沙箱环境加载）。
-- 这里定义插件可见的 db API、设备对象、即时模式面板控件、协程调度桥接与 require。

local H = __host
local G = _G

-- 宿主函数抛出的错误改成一行纯文字（去掉 "runtime error: " 前缀和 Rust 侧的 traceback），
-- 插件里 pcall 拿到的就是可读的信息，错误位置指向插件代码
local function host_err(e)
  local s = tostring(e):gsub("^runtime error: ", "")
  return (s:match("^[^\n]*"))
end
for k, f in pairs(H) do
  if type(f) == "function" and k ~= "tb" and k ~= "new_thread" then
    H[k] = function(...)
      local r = table.pack(pcall(f, ...))
      if not r[1] then error(host_err(r[2]), 3) end
      return table.unpack(r, 2, r.n)
    end
  end
end

-- 捕获真实实现（后面会替换全局 coroutine / print / load）
local raw_co = coroutine
local raw_resume = raw_co.resume
local raw_yield = raw_co.yield
local raw_status = raw_co.status
local raw_running = raw_co.running
local raw_isyieldable = raw_co.isyieldable
local raw_close = raw_co.close
local raw_load = load
local raw_xpcall = xpcall

local pack = table.pack
local unpack = table.unpack
local concat = table.concat

local TOKEN = H.token
local NULL = H.null

local function prot(fn, ...)
  local r = pack(raw_xpcall(fn, H.tb, ...))
  if not r[1] then error(r[2], 0) end
  return unpack(r, 2, r.n)
end

-- ---------------------------------------------------------------- 调度桥

-- 渲染面板时禁止阻塞（yield）
local no_yield = false

local function sched(req)
  if no_yield then
    error("panel render must not call blocking APIs; use db.spawn", 0)
  end
  local r = pack(raw_yield(TOKEN, req))
  if not r[1] then error(r[2], 0) end
  return r[2]
end

-- 调用主进程
local function call(m, p)
  return sched({ k = "call", m = m, p = p or {} })
end

-- ---------------------------------------------------------------- 协程

-- 内层协程若 yield 了调度请求，转发出去，拿到结果再继续
local function resume_inner(th, ...)
  local r = pack(raw_resume(th, ...))
  while r[1] and r[2] == TOKEN do
    local out = pack(raw_yield(TOKEN, r[3]))
    r = pack(raw_resume(th, unpack(out, 1, out.n)))
  end
  return unpack(r, 1, r.n)
end

local function co_create(fn)
  if type(fn) ~= "function" then error("coroutine.create: function expected", 2) end
  return H.new_thread(fn)
end

local function co_resume(th, ...)
  return resume_inner(th, ...)
end

local function co_wrap(fn)
  local th = co_create(fn)
  return function(...)
    local r = pack(resume_inner(th, ...))
    if not r[1] then error(r[2], 2) end
    return unpack(r, 2, r.n)
  end
end

G.coroutine = {
  create = co_create,
  resume = co_resume,
  status = raw_status,
  running = raw_running,
  isyieldable = raw_isyieldable,
  yield = raw_yield,
  wrap = co_wrap,
  close = raw_close,
}

-- 宿主用：把函数包装成带 traceback 的协程体（args 是数组表）
function H.task_from(fn, args)
  args = args or {}
  return function()
    local r = pack(raw_xpcall(fn, H.tb, unpack(args, 1, args.n)))
    if not r[1] then error(r[2], 0) end
    return unpack(r, 2, r.n)
  end
end

function H.task(fn, ...)
  local args = pack(...)
  return H.task_from(fn, args)
end

-- ---------------------------------------------------------------- 基础

local function fmt(...)
  local n = select("#", ...)
  local t = {}
  for i = 1, n do
    local v = select(i, ...)
    t[i] = type(v) == "string" and v or tostring(v)
  end
  return concat(t, " ")
end

local db = {}
G.db = db
G.droidbridge = db

local function log_at(level, ...)
  H.log(level, fmt(...), H.where())
end

function db.log(...) log_at("info", ...) end
function db.info(...) log_at("info", ...) end
function db.debug(...) log_at("debug", ...) end
function db.warn(...) log_at("warn", ...) end
function db.error(...) log_at("error", ...) end
G.print = function(...) log_at("info", ...) end

db.plugin = H.plugin()
db.lang = H.lang
db.node = H.node

-- 只读设置表
local settings_vals = H.settings()
db.settings = setmetatable({}, {
  __index = settings_vals,
  __newindex = function() error("db.settings 是只读的", 2) end,
  __pairs = function() return next, settings_vals, nil end,
  __len = function()
    local n = 0
    for _ in pairs(settings_vals) do n = n + 1 end
    return n
  end,
  __metatable = false,
})

local function update_settings(values)
  for k, v in pairs(values or {}) do settings_vals[k] = v end
end

-- 沙箱里的 load：只允许文本块，默认环境是插件环境
local function safe_load(chunk, name, mode, env)
  if type(chunk) ~= "string" then error("load: only text chunks are supported", 2) end
  return raw_load(chunk, name or "=(load)", "t", env or G)
end
G.load = safe_load

-- ---------------------------------------------------------------- require

local module_cache = {}

local function require(name)
  if type(name) ~= "string" then error("require: string expected", 2) end
  local key = name:gsub("%.", "/")
  local cached = module_cache[key]
  if cached ~= nil then
    if cached == true then return true end
    return cached
  end
  local rel = key .. ".lua"
  if not H.fs_exists("plugin://" .. rel) then
    rel = "lua/" .. key .. ".lua"
    if not H.fs_exists("plugin://" .. rel) then error("模块不存在: " .. name, 2) end
  end
  local src = H.plugin_read(rel)
  local chunk, lerr = raw_load(src, "@" .. rel, "t", G)
  if not chunk then error(lerr, 2) end
  module_cache[key] = true -- 防止循环 require
  local r = chunk(name)
  if r == nil then r = true end
  module_cache[key] = r
  return r
end
G.require = require

-- ---------------------------------------------------------------- 设备

local dev_cache = {}
local DEV = {}
DEV.__index = DEV

-- dev.computer：设备所在电脑名（本机为本机名）；dev.remote：是否在远程电脑上（SPEC 7.1）
local function mkdev(t)
  local d = { serial = t.serial, model = t.model, name = t.name, state = t.state, computer = t.computer, remote = t.remote and true or false }
  return setmetatable(d, DEV)
end

-- 用主进程给的字段更新设备对象
local function fill(d, t)
  if t.model ~= nil then d.model = t.model end
  if t.name ~= nil then d.name = t.name end
  if t.state ~= nil then d.state = t.state end
  if t.computer ~= nil then d.computer = t.computer end
  if t.remote ~= nil then d.remote = t.remote and true or false end
end

local function dev_for(serial)
  if serial == nil then return nil end
  local d = dev_cache[serial]
  if d then return d end
  d = mkdev({ serial = serial })
  dev_cache[serial] = d
  return d
end

local function refresh_devices()
  local list = call("devices", {}) or {}
  for _, t in ipairs(list) do
    local d = dev_cache[t.serial]
    if d then
      fill(d, t)
    else
      dev_cache[t.serial] = mkdev(t)
    end
  end
  return list
end

-- 拿到带属性的设备对象（属性为空时刷新一次）
local function dev_arg(serial)
  if serial == nil then return nil end
  local d = dev_cache[serial]
  if d and d.model ~= nil then return d end
  refresh_devices()
  return dev_cache[serial] or dev_for(serial)
end

-- 事件参数里的设备表（含 model / computer / remote）→ dev 对象
local function dev_from(t)
  local d = dev_cache[t.serial]
  if not d then
    d = mkdev({ serial = t.serial })
    dev_cache[t.serial] = d
  end
  fill(d, t)
  return d
end

function db.devices()
  local list = refresh_devices()
  local out = {}
  for _, t in ipairs(list) do
    out[#out + 1] = dev_cache[t.serial] or mkdev(t)
  end
  return out
end

function db.device(serial)
  if serial == nil then return nil end
  if dev_cache[serial] then return dev_cache[serial] end
  refresh_devices()
  return dev_cache[serial]
end

function DEV:prop(name)
  return call("prop", { serial = self.serial, name = name })
end

function DEV:shell(cmd, opts)
  local r = call("shell", { serial = self.serial, cmd = cmd, timeout = opts and opts.timeout }) or {}
  return r.out, r.code
end

-- 本机路径先在宿主里解析成绝对路径并检查权限（相对路径相对于 dataDir，plugin:// 为插件目录）
local function lpath(p, write)
  if type(p) ~= "string" or p == "" then error("缺少本机路径", 3) end
  local ok, r = pcall(H.resolve, p, write)
  if not ok then error((tostring(r):gsub("^runtime error: ", ""):match("^[^\n]*")), 3) end
  return r
end

function DEV:push(localPath, remotePath)
  call("push", { serial = self.serial, ["local"] = lpath(localPath, false), remote = remotePath })
  return true
end

function DEV:pull(remotePath, localPath)
  call("pull", { serial = self.serial, remote = remotePath, ["local"] = lpath(localPath, true) })
  return true
end

function DEV:install(apk, opts)
  opts = opts or {}
  call("install", {
    serial = self.serial,
    path = lpath(apk, false),
    replace = opts.replace,
    grant = opts.grant,
    downgrade = opts.downgrade,
  })
  return true
end

function DEV:uninstall(pkg)
  call("uninstall", { serial = self.serial, package = pkg })
  return true
end

function DEV:apps(opts)
  opts = opts or {}
  return call("apps", { serial = self.serial, system = opts.system }) or {}
end

function DEV:hasApp(pkg)
  return call("hasApp", { serial = self.serial, package = pkg }) and true or false
end

function DEV:launch(pkg, activity)
  call("launch", { serial = self.serial, package = pkg, activity = activity })
  return true
end

function DEV:stop(pkg)
  call("stop", { serial = self.serial, package = pkg })
  return true
end

function DEV:clear(pkg)
  call("clear", { serial = self.serial, package = pkg })
  return true
end

function DEV:foreground()
  return call("foreground", { serial = self.serial })
end

function DEV:tap(x, y)
  call("input", { serial = self.serial, kind = "tap", x = x, y = y })
  return true
end

function DEV:swipe(x1, y1, x2, y2, ms)
  call("input", { serial = self.serial, kind = "swipe", x = x1, y = y1, x2 = x2, y2 = y2, duration = ms })
  return true
end

function DEV:key(name)
  call("input", { serial = self.serial, kind = "key", key = name })
  return true
end

function DEV:text(s)
  call("input", { serial = self.serial, kind = "text", text = s })
  return true
end

function DEV:screenshot(opts)
  opts = opts or {}
  return call("screenshot", { serial = self.serial, save = opts.save })
end

function DEV:forward(localPort, remote)
  return call("forward", { serial = self.serial, ["local"] = localPort or 0, remote = remote })
end

function DEV:reverse(remote, localPort)
  return call("forward", { serial = self.serial, ["local"] = localPort or 0, remote = remote, reverse = true })
end

function DEV:unforward(localPort)
  return call("unforward", { serial = self.serial, ["local"] = localPort })
end

function DEV:logcat(opts)
  opts = opts or {}
  return call("logcat", {
    serial = self.serial,
    lines = opts.lines,
    level = opts.level,
    grep = opts.grep,
    package = opts.package,
  })
end

function DEV:perf()
  return call("perf", { serial = self.serial })
end

-- ---------------------------------------------------------------- 贡献注册

local menus = {}
local panels = {}
local webpanels = {}
local stats = {}
local metrics = {}
local mcps = {}
local highlights = {}
local listeners = {}
local web_handlers = {}
local watches = {}

local function reg(kind, def)
  call("register", { kind = kind, def = def })
end

local function unreg(kind, id)
  call("unregister", { kind = kind, id = id })
end

function db.menu(target, path, a, b)
  local opts, fn
  if type(a) == "function" then
    opts, fn = {}, a
  else
    opts, fn = a or {}, b
  end
  if type(fn) ~= "function" then error("db.menu: 处理函数不能为空", 2) end
  if type(target) ~= "string" or type(path) ~= "string" then error("db.menu: target/path 必须是字符串", 2) end
  local id = opts.id or (target .. ":" .. path)
  local def = { id = id, target = target, path = path, icon = opts.icon, filter = opts.filter }
  menus[id] = { fn = fn, def = def }
  reg("menu", def)
  return {
    id = id,
    remove = function()
      menus[id] = nil
      unreg("menu", id)
    end,
  }
end

function db.panel(id, opts, fn)
  if type(id) ~= "string" then error("db.panel: id 必须是字符串", 2) end
  if type(fn) ~= "function" then error("db.panel: 渲染函数不能为空", 2) end
  opts = opts or {}
  local def = {
    id = id,
    title = opts.title or id,
    icon = opts.icon,
    group = opts.group,
    dock = opts.dock or "device",
    filter = opts.filter,
    refresh = opts.refresh,
  }
  panels[id] = { fn = fn, def = def }
  reg("panel", def)
  return {
    id = id,
    remove = function()
      panels[id] = nil
      unreg("panel", id)
    end,
  }
end

function db.webpanel(id, opts)
  if type(id) ~= "string" then error("db.webpanel: id 必须是字符串", 2) end
  opts = opts or {}
  local external = opts.url ~= nil
  if external then
    -- 外部网页：url 为 function(dev) -> 地址|nil（nil 表示暂时没有，界面会稍后再问），或固定地址
    if not H.has_perm("web.embed") then error("db.webpanel: 嵌入外部网页需要 web.embed 权限", 2) end
    local u = opts.url
    if type(u) ~= "function" and type(u) ~= "string" then error("db.webpanel: url 必须是字符串或函数", 2) end
    db.web.handle("__url:" .. id, function(dev)
      if type(u) == "function" then return u(dev) end
      return u
    end)
    -- 页面用 postMessage 发来的消息（界面已核对来源窗口和 origin）
    if type(opts.onMessage) == "function" then
      local on = opts.onMessage
      db.web.handle("__msg:" .. id, function(dev, data) return on(dev, data) end)
    end
  elseif type(opts.entry) ~= "string" then
    error("db.webpanel: 需要 entry 或 url", 2)
  end
  local def = {
    id = id,
    title = opts.title or id,
    icon = opts.icon,
    entry = opts.entry,
    url = external or nil,
    onMessage = (external and type(opts.onMessage) == "function") or nil,
    group = opts.group,
    dock = opts.dock or "device",
    filter = opts.filter,
  }
  webpanels[id] = def
  reg("webpanel", def)
  return {
    id = id,
    remove = function()
      webpanels[id] = nil
      unreg("webpanel", id)
    end,
  }
end

function db.status(id, text, opts)
  if type(id) ~= "string" then error("db.status: id 必须是字符串", 2) end
  if text == nil then
    stats[id] = nil
    unreg("status", id)
    return
  end
  opts = opts or {}
  local def = {
    id = id,
    text = tostring(text),
    kind = opts.kind,
    tip = opts.tip,
    clickable = opts.onClick ~= nil,
  }
  stats[id] = { fn = opts.onClick, def = def }
  reg("status", def)
end

function db.badge(dev, text, opts)
  opts = opts or {}
  if dev == nil and text ~= nil then error("db.badge: 需要设备", 2) end
  call("badge", { serial = dev and dev.serial, text = text == nil and NULL or text, kind = opts.kind, tip = opts.tip })
end

function db.notify(text, opts)
  opts = opts or {}
  call("notify", { text = tostring(text), kind = opts.kind })
end

function db.overlay(dev, shapes, size)
  if dev == nil and shapes ~= nil then error("db.overlay: 需要设备", 2) end
  if not H.has_perm("device.screen") then error("db.overlay 需要 device.screen 权限", 2) end
  local sz = NULL
  if type(size) == "table" and tonumber(size.w) and tonumber(size.h) then
    sz = { w = tonumber(size.w), h = tonumber(size.h) }
  end
  call("overlay", { serial = dev and dev.serial, shapes = shapes == nil and NULL or shapes, size = sz })
end

function db.repaint(panel_id)
  call("repaint", { panel = panel_id })
end

db.perf = {}

function db.perf.metric(id, opts, fn)
  if type(id) ~= "string" then error("db.perf.metric: id 必须是字符串", 2) end
  if type(fn) ~= "function" then error("db.perf.metric: 需要函数", 2) end
  opts = opts or {}
  local def = { id = id, label = opts.label or id, unit = opts.unit, filter = opts.filter }
  metrics[id] = { fn = fn, def = def }
  reg("metric", def)
  return {
    id = id,
    remove = function()
      metrics[id] = nil
      unreg("metric", id)
    end,
  }
end

db.mcp = {}

function db.mcp.tool(name, opts, fn)
  if not H.has_perm("mcp") then error("db.mcp.tool 需要 mcp 权限", 2) end
  if type(name) ~= "string" then error("db.mcp.tool: name 必须是字符串", 2) end
  if type(fn) ~= "function" then error("db.mcp.tool: 需要函数", 2) end
  opts = opts or {}
  local def = {
    name = name,
    description = opts.description or "",
    params = opts.params or {},
    required = opts.required,
  }
  mcps[name] = { fn = fn, def = def }
  reg("mcp", def)
  return {
    name = name,
    remove = function()
      mcps[name] = nil
      unreg("mcp", name)
    end,
  }
end

-- ---------------------------------------------------------------- logcat

db.logcat = {}

local SUB = {}
SUB.__index = SUB

function SUB:stop()
  if self.sub == nil then return end
  watches[self.sub] = nil
  local sub = self.sub
  self.sub = nil
  call("logunwatch", { sub = sub })
end

function db.logcat.watch(dev, opts, fn)
  if not H.has_perm("device.logs") then error("db.logcat.watch 需要 device.logs 权限", 2) end
  if type(fn) ~= "function" then error("db.logcat.watch: 需要函数", 2) end
  opts = opts or {}
  local sub = call("logwatch", {
    serial = dev and dev.serial or nil,
    level = opts.level,
    tag = opts.tag,
    grep = opts.grep,
    package = opts.package,
  })
  watches[sub] = fn
  return setmetatable({ sub = sub }, SUB)
end

function db.logcat.highlight(text, opts)
  opts = opts or {}
  local id = opts.id or ("loghl:" .. H.uid())
  local def = { id = id, text = tostring(text), tag = opts.tag, kind = opts.kind, label = opts.label }
  highlights[id] = def
  reg("loghl", def)
  return {
    id = id,
    remove = function()
      highlights[id] = nil
      unreg("loghl", id)
    end,
  }
end

-- ---------------------------------------------------------------- web / 事件

db.web = {}

function db.web.handle(name, fn)
  if type(name) ~= "string" then error("db.web.handle: name 必须是字符串", 2) end
  if type(fn) ~= "function" then error("db.web.handle: 需要函数", 2) end
  web_handlers[name] = fn
end

function db.web.emit(name, data)
  call("webemit", { name = name, data = data })
end

local function add_listener(name, fn)
  local list = listeners[name]
  local first = list == nil
  if first then
    list = {}
    listeners[name] = list
  end
  -- 先把回调挂上再注册，避免 register 期间（协程被挂起）收不到事件
  list[#list + 1] = fn
  if first then
    reg("event", { name = name })
  end
end

function db.on(name, fn)
  if type(name) ~= "string" then error("db.on: 事件名必须是字符串", 2) end
  if type(fn) ~= "function" then error("db.on: 需要函数", 2) end
  add_listener(name, fn)
end

local function fire_event(name, ...)
  local list = listeners[name]
  if not list then return end
  local args = pack(...)
  for _, fn in ipairs(list) do
    -- 每个监听者独立协程，不等待
    H.spawn(H.task(fn, unpack(args, 1, args.n)))
  end
end

-- ---------------------------------------------------------------- 定时器

local timer_fns = {}
local timer_repeat = {}
local TIMER = {}
TIMER.__index = TIMER

function TIMER:cancel()
  if self.id == nil then return end
  local id = self.id
  self.id = nil
  timer_fns[id] = nil
  timer_repeat[id] = nil
  H.cancel_timer(id)
end

local function new_timer(sec, repeat_, fn)
  if type(sec) ~= "number" then error("定时器: 秒数必须是数字", 3) end
  if type(fn) ~= "function" then error("定时器: 需要函数", 3) end
  local id = H.new_timer(sec, repeat_)
  timer_fns[id] = fn
  timer_repeat[id] = repeat_
  return setmetatable({ id = id }, TIMER)
end

function db.every(sec, fn)
  return new_timer(sec, true, fn)
end

function db.after(sec, fn)
  return new_timer(sec, false, fn)
end

function db.wait(sec)
  if type(sec) ~= "number" then error("db.wait: 秒数必须是数字", 2) end
  sched({ k = "sleep", sec = sec })
end

function db.spawn(fn, ...)
  if type(fn) ~= "function" then error("db.spawn: 需要函数", 2) end
  local args = pack(...)
  H.spawn(H.task(fn, unpack(args, 1, args.n)))
end

-- ---------------------------------------------------------------- 常驻子进程

db.proc = {}
local procs = {}   -- id -> {p, onLine, onStderr, onExit}

--- 启动常驻子进程。opts: {cwd, env, onLine(line), onStderr(line), onExit(code)}
--- 每输出一行就在新协程里调用一次 onLine；返回的对象：p:write(s) p:send(line) p:close() p:kill() p:alive()
function db.proc.start(program, args, opts)
  if not H.has_perm("exec") then error("db.proc 需要 exec 权限", 2) end
  if type(program) ~= "string" or program == "" then error("db.proc.start: 需要程序路径", 2) end
  opts = opts or {}
  local ok, id = pcall(H.proc_start, program, args or {}, opts.cwd, opts.env)
  if not ok then error((tostring(id):gsub("^runtime error: ", "")), 2) end
  local p = { id = id }
  function p:write(s) H.proc_write(self.id, tostring(s)) end
  function p:send(line) H.proc_write(self.id, tostring(line) .. "\n") end
  function p:close() H.proc_close(self.id) end
  function p:kill() H.proc_kill(self.id) end
  function p:alive() return H.proc_alive(self.id) end
  procs[id] = { p = p, onLine = opts.onLine, onStderr = opts.onStderr, onExit = opts.onExit }
  return p
end

-- 宿主投递的子进程事件
function H.proc_event(id, kind, data, code)
  local e = procs[id]
  if not e then return end
  if kind == "exit" then
    procs[id] = nil
    if e.onExit then e.onExit(code) end
  elseif kind == "err" then
    if e.onStderr then e.onStderr(data) end
  elseif e.onLine then
    e.onLine(data)
  end
end

-- 宿主触发定时器回调
function H.fire_timer(id, repeat_)
  local fn = timer_fns[id]
  if not fn then return end
  if not repeat_ then
    timer_fns[id] = nil
    timer_repeat[id] = nil
  end
  fn()
end

-- ---------------------------------------------------------------- UI 对话框

db.ui = {}

function db.ui.confirm(title, message)
  return call("dialog", { kind = "confirm", title = title, message = message }) and true or false
end

function db.ui.prompt(title, message, default)
  local v = call("dialog", { kind = "prompt", title = title, message = message, default = default })
  if v == nil then return nil end
  return tostring(v)
end

function db.ui.form(title, fields)
  return call("dialog", { kind = "form", title = title, fields = fields })
end

function db.ui.pickFile(opts)
  opts = opts or {}
  return call("dialog", { kind = "file", title = opts.title, accept = opts.accept })
end

function db.ui.pickDir(opts)
  opts = opts or {}
  local v = call("dialog", { kind = "dir", title = opts.title, default = opts.default })
  if v == nil or v == "" then return nil end
  return tostring(v)
end

--- 在发起操作的那个界面里打开网页：桌面版为应用内窗口（按 name 复用），浏览器里为同名标签页
--- opts: {name, title, mode = "window" | "browser"}
function db.ui.openUrl(url, opts)
  if type(url) ~= "string" or not (url:match("^https?://")) then error("db.ui.openUrl: 只能打开 http(s) 地址", 2) end
  opts = opts or {}
  call("uiopen", { url = url, name = opts.name, title = opts.title, mode = opts.mode })
  return true
end

function db.ui.progress(title)
  local id = "progress:" .. H.uid()
  call("progress", { id = id, title = title, frac = NULL, done = false })
  local p = { id = id }
  function p:set(frac, text)
    call("progress", { id = self.id, frac = frac == nil and NULL or frac, text = text })
  end
  function p:close()
    call("progress", { id = self.id, done = true })
  end
  return p
end

-- ---------------------------------------------------------------- 即时模式面板

local function make_ui(event)
  local ui = {}
  local root = { id = "", children = {} }
  local stack = { root }
  local ids = {}

  local function alloc(w, label, override)
    local parent = stack[#stack]
    if label == nil then label = #parent.children + 1 end
    label = tostring(label)
    local base = (parent.id == "" and "" or parent.id .. "/") .. w .. ":" .. label
    local id = override
    if id == nil then
      id = base
      local n = 2
      while ids[id] do
        id = base .. "#" .. n
        n = n + 1
      end
    end
    ids[id] = true
    local node = { w = w, id = id }
    parent.children[#parent.children + 1] = node
    return node, (event ~= nil and event.id == id)
  end

  local function text_control(w, text, o)
    local n = alloc(w, text, o and o.id)
    n.text = tostring(text or "")
    return n
  end

  function ui.label(text, o)
    local n = text_control("label", text, o)
    if o and o.kind then n.kind = o.kind end
  end

  function ui.heading(text, o)
    text_control("heading", text, o)
  end

  function ui.text(text, o)
    text_control("text", text, o)
  end

  function ui.code(text, o)
    text_control("code", text, o)
  end

  function ui.separator(o)
    alloc("separator", nil, o and o.id)
  end

  function ui.space(o)
    alloc("space", nil, o and o.id)
  end

  function ui.button(label, o)
    local n, hit = alloc("button", label, o and o.id)
    n.label = tostring(label or "")
    if o then
      if o.kind then n.kind = o.kind end
      if o.disabled ~= nil then n.disabled = o.disabled and true or false end
    end
    return hit and true or false
  end

  function ui.toggle(label, value, o)
    local n, hit = alloc("toggle", label, o and o.id)
    n.label = tostring(label or "")
    local v = value and true or false
    n.value = v
    if hit then return event.value and true or false end
    return v
  end

  function ui.slider(label, value, min, max, step, o)
    local n, hit = alloc("slider", label, o and o.id)
    n.label = tostring(label or "")
    local v = tonumber(value) or tonumber(min) or 0
    n.value = v
    n.min = tonumber(min) or 0
    n.max = tonumber(max) or 100
    n.step = tonumber(step) or 1
    if hit then
      local ev = tonumber(event.value)
      return ev or v
    end
    return v
  end

  function ui.input(label, value, o)
    local n, hit = alloc("input", label, o and o.id)
    n.label = tostring(label or "")
    local v = value == nil and "" or tostring(value)
    n.value = v
    if o then
      if o.placeholder then n.placeholder = tostring(o.placeholder) end
      if o.multiline ~= nil then n.multiline = o.multiline and true or false end
    end
    if hit then return tostring(event.value or "") end
    return v
  end

  function ui.select(label, value, options, o)
    local n, hit = alloc("select", label, o and o.id)
    n.label = tostring(label or "")
    local opts = {}
    for _, it in ipairs(options or {}) do
      if type(it) == "table" then
        opts[#opts + 1] = { value = it.value, label = tostring(it.label or it.value or "") }
      else
        opts[#opts + 1] = { value = it, label = tostring(it) }
      end
    end
    n.options = opts
    n.value = value
    if hit then return event.value end
    return value
  end

  function ui.progress(value, text, o)
    local n = alloc("progress", o and o.id, o and o.id)
    if value ~= nil then n.value = tonumber(value) end
    if text ~= nil then n.text = tostring(text) end
  end

  function ui.table(columns, rows, o)
    local n = alloc("table", o and o.id, o and o.id)
    n.columns = columns or {}
    n.rows = rows or {}
  end

  function ui.chart(label, values, unit, o)
    local n = alloc("chart", label, o and o.id)
    n.label = tostring(label or "")
    n.values = values or {}
    if unit then n.unit = tostring(unit) end
  end

  function ui.image(src, o)
    local n = alloc("image", nil, o and o.id)
    n.src = tostring(src or "")
  end

  function ui.link(text, url, o)
    local n = alloc("link", text, o and o.id)
    n.text = tostring(text or "")
    n.url = tostring(url or "")
  end

  function ui.row(fn, o)
    local n = alloc("row", o and o.id, o and o.id)
    n.children = {}
    stack[#stack + 1] = { id = n.id, children = n.children }
    if fn then fn() end
    stack[#stack] = nil
  end

  function ui.group(title, fn, o)
    local n = alloc("group", title, o and o.id)
    n.title = tostring(title or "")
    if o and o.collapsed ~= nil then n.collapsed = o.collapsed and true or false end
    n.children = {}
    stack[#stack + 1] = { id = n.id, children = n.children }
    if fn then fn() end
    stack[#stack] = nil
  end

  function ui.finish()
    return root.children
  end

  return ui
end

-- 执行一轮面板渲染；event 非空时是事件轮
local function render_panel(hp, dev, event)
  local ui = make_ui(event)
  no_yield = true
  local r = pack(raw_xpcall(hp.fn, H.tb, ui, dev))
  no_yield = false
  if not r[1] then error(r[2], 0) end
  return ui.finish()
end

-- ---------------------------------------------------------------- 主进程请求

local function dispatch(m, p)
  p = p or {}
  if m == "ping" then
    return true
  elseif m == "menu" then
    local item = menus[p.id]
    if not item then error("菜单不存在: " .. tostring(p.id)) end
    local ctx = p.ctx or {}
    local target = item.def.target
    local dev = dev_arg(ctx.serial)
    if target == "app" then
      return item.fn(dev, ctx.package)
    elseif target == "file" then
      return item.fn(dev, ctx.path, ctx.isDir)
    elseif target == "tools" then
      return item.fn()
    end
    return item.fn(dev)
  elseif m == "status" then
    local item = stats[p.id]
    if not item then error("状态栏项不存在: " .. tostring(p.id)) end
    if item.fn then return item.fn() end
    return true
  elseif m == "panel" then
    local hp = panels[p.id]
    if not hp then error("面板不存在: " .. tostring(p.id)) end
    local dev = dev_arg(p.serial)
    if p.event ~= nil then
      render_panel(hp, dev, p.event) -- 事件轮，结果丢弃
    end
    local tree = render_panel(hp, dev, nil) -- 绘制轮
    return { title = hp.def.title, tree = tree }
  elseif m == "metric" then
    local item = metrics[p.id]
    if not item then error("指标不存在: " .. tostring(p.id)) end
    return item.fn(dev_arg(p.serial))
  elseif m == "web" then
    local fn = web_handlers[p.name]
    if not fn then error("web 处理函数不存在: " .. tostring(p.name)) end
    return fn(dev_arg(p.serial), p.args or {})
  elseif m == "mcp" then
    local item = mcps[p.name]
    if not item then error("MCP 工具不存在: " .. tostring(p.name)) end
    return item.fn(dev_arg(p.serial), p.args or {})
  elseif m == "event" then
    local name = p.name
    local args = {}
    for i, a in ipairs(p.args or {}) do
      if type(a) == "table" and a.serial ~= nil then
        args[i] = dev_from(a)
      else
        args[i] = a
      end
    end
    local list = listeners[name]
    if list then
      for _, fn in ipairs(list) do
        H.spawn(H.task(fn, unpack(args, 1, #args)))
      end
    end
    return true
  elseif m == "settings" then
    update_settings(p.values)
    local values = {}
    for k, v in pairs(settings_vals) do values[k] = v end
    fire_event("settings.changed", values)
    return true
  elseif m == "logline" then
    local fn = watches[p.sub]
    if not fn then return false end
    fn(p.line)
    return true
  end
  error("未知请求类型: " .. tostring(m))
end

function H.dispatch(m, p)
  return prot(dispatch, m, p)
end

-- ---------------------------------------------------------------- 本地操作

db.fs = {
  read = function(path)
    return H.fs_read(path)
  end,
  write = function(path, data)
    H.fs_write(path, data)
    return true
  end,
  append = function(path, data)
    H.fs_append(path, data)
    return true
  end,
  list = function(dir)
    return H.fs_list(dir)
  end,
  exists = function(path)
    return H.fs_exists(path)
  end,
  stat = function(path)
    return H.fs_stat(path)
  end,
  mkdir = function(path)
    H.fs_mkdir(path)
    return true
  end,
  remove = function(path)
    H.fs_remove(path)
    return true
  end,
  copy = function(from, to)
    H.fs_copy(from, to)
    return true
  end,
  walk = function(dir, opts)
    opts = opts or {}
    return sched({ k = "walk", dir = dir, ext = opts.ext, hidden = opts.hidden == true })
  end,
  tar = function(dir, out, files, opts)
    return sched({ k = "tar", dir = dir, out = out, files = files, gzip = (opts or {}).gzip == true })
  end,
  hash = function(dir, files)
    return sched({ k = "hash", dir = dir, files = files or {} })
  end,
  join = function(...)
    local parts = pack(...)
    local out = ""
    for i = 1, parts.n do
      local p = tostring(parts[i])
      if i == 1 then
        out = p
      else
        out = (out:gsub("[/\\]+$", "")) .. "/" .. (p:gsub("^[/\\]+", ""))
      end
    end
    return out
  end,
}

function db.exec(program, args, opts)
  if not H.has_perm("exec") then error("db.exec 需要 exec 权限", 2) end
  opts = opts or {}
  local r = sched({
    k = "exec",
    program = program,
    args = args or {},
    cwd = opts.cwd,
    timeout = opts.timeout,
    env = opts.env,
    input = opts.input,
  }) or {}
  return { code = r.code or -1, stdout = r.stdout or "", stderr = r.stderr or "" }
end

function db.open(target)
  if not H.has_perm("exec") then error("db.open 需要 exec 权限", 2) end
  sched({ k = "open", target = target })
  return true
end

db.http = {}

function db.http.request(opts)
  if not H.has_perm("net") then error("db.http.request 需要 net 权限", 2) end
  opts = opts or {}
  if type(opts.url) ~= "string" then error("db.http.request: 需要 url", 2) end
  return sched({
    k = "http",
    url = opts.url,
    method = opts.method,
    headers = opts.headers,
    body = opts.body,
    timeout = opts.timeout,
  })
end

function db.http.download(url, path, opts)
  if not H.has_perm("net") then error("db.http.download 需要 net 权限", 2) end
  if type(url) ~= "string" or type(path) ~= "string" then error("db.http.download: 需要 url 和 path", 2) end
  opts = opts or {}
  return sched({ k = "download", url = url, path = path, headers = opts.headers, timeout = opts.timeout })
end

db.clipboard = {}

function db.clipboard.get()
  if not H.has_perm("clipboard") then error("db.clipboard.get 需要 clipboard 权限", 2) end
  return sched({ k = "clip", op = "get" })
end

function db.clipboard.set(text)
  if not H.has_perm("clipboard") then error("db.clipboard.set 需要 clipboard 权限", 2) end
  sched({ k = "clip", op = "set", text = tostring(text or "") })
  return true
end

db.store = {
  get = function(key)
    return H.store_get(tostring(key))
  end,
  set = function(key, value)
    H.store_set(tostring(key), value)
    return true
  end,
}

db.json = {
  encode = function(v)
    return H.json_encode(v)
  end,
  decode = function(s)
    return H.json_decode(s)
  end,
}

db.media = {}

function db.media.save(path)
  if not H.has_perm("media") then error("db.media.save 需要 media 权限", 2) end
  return call("media.save", { path = lpath(path, false) })
end
