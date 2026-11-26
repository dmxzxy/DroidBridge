-- 交给 Pi 的设备工具
local T = require("i18n")
local M = {}

local MAX_TEXT = 20000

local list = {}

--- 定义一个工具；action = true 表示会改变设备状态
local function tool(name, def)
  def.name = name
  list[#list + 1] = def
end

--- 截断过长的文字
local function clip(s)
  s = tostring(s or "")
  if #s <= MAX_TEXT then return s end
  return s:sub(1, MAX_TEXT) .. "\n… (truncated, " .. #s .. " bytes in total; narrow it down)"
end

local function sh(dev, cmd)
  local out = dev:shell(cmd, { timeout = 30 })
  return (out or ""):gsub("%s+$", "")
end

local SERIAL = { type = "string", description = "Device serial; defaults to the current device" }

tool("db_devices", {
  description = "List all available devices",
  params = {},
  device = false,
  fn = function()
    local out = {}
    for _, d in ipairs(db.devices()) do
      out[#out + 1] = { serial = d.serial, model = d.model, name = d.name, state = d.state, computer = d.computer }
    end
    return #out > 0 and db.json.encode(out) or "No devices"
  end,
})

tool("db_device_info", {
  description = "Device basics: model, Android version, resolution, foreground app, battery",
  params = {},
  fn = function(dev)
    local info = {
      serial = dev.serial,
      model = dev.model,
      brand = dev:prop("ro.product.brand"),
      android = dev:prop("ro.build.version.release"),
      sdk = dev:prop("ro.build.version.sdk"),
      abi = dev:prop("ro.product.cpu.abi"),
      size = sh(dev, "wm size"),
      density = sh(dev, "wm density"),
      foreground = dev:foreground(),
      battery = sh(dev, "dumpsys battery | grep -E 'level|status|temperature'"),
    }
    return db.json.encode(info)
  end,
})

tool("db_logcat", {
  description = "Read the device log (logcat)",
  params = {
    lines = { type = "number", description = "How many recent lines, default 300, at most 2000" },
    level = { type = "string", description = "Minimum level: V D I W E F" },
    grep = { type = "string", description = "Keep only lines containing this text" },
    package = { type = "string", description = "Only this app's log" },
  },
  fn = function(dev, a)
    local n = math.min(tonumber(a.lines) or 300, 2000)
    return clip(dev:logcat({ lines = n, level = a.level, grep = a.grep, package = a.package }))
  end,
})

tool("db_crash", {
  description = "Read recent crashes (logcat crash buffer) and ANR records",
  params = {},
  fn = function(dev)
    local crash = sh(dev, "logcat -d -b crash -t 300")
    local anr = sh(dev, "ls -t /data/anr 2>/dev/null | head -5")
    return clip("== crash ==\n" .. (crash ~= "" and crash or "(none)") .. "\n== /data/anr ==\n" .. (anr ~= "" and anr or "(none, or no permission)"))
  end,
})

tool("db_screenshot", {
  description = "Take a screenshot of the device",
  params = {},
  fn = function(dev)
    return { image = dev:screenshot() }
  end,
})

tool("db_ui", {
  description = "Read the current Android view tree: text, resource id, class, center (device pixels), clickable",
  params = {
    all = { type = "boolean", description = "Include views without text or id; default false" },
  },
  fn = function(dev, a)
    local t = dev:ui()
    local rows = { ("screen %dx%d"):format(t.width or 0, t.height or 0) }
    for _, n in ipairs(t.nodes or {}) do
      local label = n.text ~= "" and n.text or n.desc
      if a.all or (label and label ~= "") or (n.id and n.id ~= "") then
        rows[#rows + 1] = ("%s | %s | %s | (%d,%d)%s"):format(
          label or "", n.id or "", (n.class or ""):match("[^.]+$") or "",
          n.x or 0, n.y or 0, n.clickable and " click" or "")
      end
    end
    return clip(table.concat(rows, "\n"))
  end,
})

tool("db_apps", {
  description = "List installed apps",
  params = {
    system = { type = "boolean", description = "Include system apps; default false" },
    filter = { type = "string", description = "Keep only apps whose package or name contains this text" },
  },
  fn = function(dev, a)
    local out = {}
    for _, app in ipairs(dev:apps({ system = a.system == true })) do
      local f = a.filter and a.filter:lower()
      if not f or app.package:lower():find(f, 1, true) or (app.name or ""):lower():find(f, 1, true) then
        out[#out + 1] = ("%s  %s  %s"):format(app.package, app.name or "", app.version or "")
      end
    end
    return clip(table.concat(out, "\n"))
  end,
})

tool("db_app_info", {
  description = "App details: version, install and update time, target SDK, whether it is running",
  required = { "package" },
  params = { package = { type = "string", description = "Package name" } },
  fn = function(dev, a)
    local pkg = a.package:gsub("[^%w%._]", "")
    local info = sh(dev, "dumpsys package " .. pkg .. " | grep -E 'versionName|versionCode|firstInstallTime|lastUpdateTime|targetSdk' | head -10")
    local pid = sh(dev, "pidof " .. pkg)
    return info .. "\npid: " .. (pid ~= "" and pid or "not running")
  end,
})

tool("db_perf", {
  description = "Current performance: CPU, memory, frame rate, temperature, etc.",
  params = {},
  fn = function(dev)
    return db.json.encode(dev:perf())
  end,
})

tool("db_shell", {
  description = "Run an adb shell command on the device",
  action = true,
  required = { "cmd" },
  params = { cmd = { type = "string", description = "Shell command" } },
  fn = function(dev, a)
    local out, code = dev:shell(a.cmd, { timeout = 60 })
    return clip((out or "") .. "\n(exit " .. tostring(code) .. ")")
  end,
  describe = function(a) return T("执行命令：", "Run command: ") .. a.cmd end,
})

tool("db_tap", {
  description = "Tap the screen (device pixels)",
  action = true,
  required = { "x", "y" },
  params = { x = { type = "number" }, y = { type = "number" } },
  fn = function(dev, a) dev:tap(a.x, a.y); return "ok" end,
  describe = function(a) return (T("点击 (%d, %d)", "Tap (%d, %d)")):format(a.x, a.y) end,
})

tool("db_swipe", {
  description = "Swipe the screen (device pixels)",
  action = true,
  required = { "x1", "y1", "x2", "y2" },
  params = {
    x1 = { type = "number" }, y1 = { type = "number" }, x2 = { type = "number" }, y2 = { type = "number" },
    ms = { type = "number", description = "Duration in ms, default 300" },
  },
  fn = function(dev, a) dev:swipe(a.x1, a.y1, a.x2, a.y2, a.ms or 300); return "ok" end,
  describe = function(a) return (T("滑动 (%d, %d) → (%d, %d)", "Swipe (%d, %d) → (%d, %d)")):format(a.x1, a.y1, a.x2, a.y2) end,
})

tool("db_key", {
  description = "Press a key: back home recent power volup voldown enter del, etc.",
  action = true,
  required = { "key" },
  params = { key = { type = "string" } },
  fn = function(dev, a) dev:key(a.key); return "ok" end,
  describe = function(a) return T("按键 ", "Press ") .. a.key end,
})

tool("db_text", {
  description = "Type text into the focused field",
  action = true,
  required = { "text" },
  params = { text = { type = "string" } },
  fn = function(dev, a) dev:text(a.text); return "ok" end,
  describe = function(a) return T("输入文字：", "Type: ") .. a.text end,
})

tool("db_launch", {
  description = "Launch an app",
  action = true,
  required = { "package" },
  params = { package = { type = "string" } },
  fn = function(dev, a) dev:launch(a.package); return "ok" end,
  describe = function(a) return T("启动 ", "Launch ") .. a.package end,
})

tool("db_stop", {
  description = "Force-stop an app",
  action = true,
  required = { "package" },
  params = { package = { type = "string" } },
  fn = function(dev, a) dev:stop(a.package); return "ok" end,
  describe = function(a) return T("停止 ", "Stop ") .. a.package end,
})

local byName = {}
for _, t in ipairs(list) do byName[t.name] = t end

local function count(text, pat)
  local n = 0
  for _ in text:gmatch(pat) do n = n + 1 end
  return n
end

local LINE = "[^\n]+"

local SUMMARY = {
  db_devices = function(t)
    if t:find("No devices", 1, true) then return T("没有连接设备", "No devices connected") end
    return count(t, '"serial"') .. T(" 台设备", " devices")
  end,
  db_logcat = function(t)
    local lines, bad = 0, 0
    for line in t:gmatch(LINE) do
      lines = lines + 1
      if line:match("^%S+%s+%S+%s+%d+%s+%d+%s+[EF]%s") or line:match("^[EF]/") then bad = bad + 1 end
    end
    return lines .. T(" 行", " lines") .. (bad > 0 and (T("，", ", ") .. bad .. T(" 条错误", " errors")) or T("，没有错误", ", no errors"))
  end,
  db_crash = function(t)
    local n = count(t, "FATAL EXCEPTION") + count(t, "Fatal signal")
    return n > 0 and (n .. T(" 次崩溃", n == 1 and " crash" or " crashes")) or T("没有崩溃记录", "No crashes")
  end,
  db_screenshot = function() return T("已截图", "Captured") end,
  db_ui = function(t) return math.max(count(t, LINE) - 1, 0) .. T(" 个控件", " views") end,
  db_apps = function(t) return count(t, LINE) .. T(" 个应用", " apps") end,
  db_app_info = function(t) return t:match("versionName=(%S+)") and (T("版本 ", "version ") .. t:match("versionName=(%S+)")) or nil end,
  db_perf = function(t)
    local ok, p = pcall(db.json.decode, t)
    if not ok or type(p) ~= "table" then return nil end
    local parts = {}
    local fps = p.fps or (p.app and p.app.fps)
    if fps then parts[#parts + 1] = T("帧率 ", "FPS ") .. math.floor(fps + 0.5) end
    local cpu = p.cpu or (p.app and p.app.cpu)
    if type(cpu) == "number" then parts[#parts + 1] = "CPU " .. math.floor(cpu + 0.5) .. "%" end
    return #parts > 0 and table.concat(parts, "，") or nil
  end,
}

--- 工具结果的一行摘要（给界面显示）
function M.summary(name, result, isError)
  local text = ""
  for _, b in ipairs((result and result.content) or {}) do
    if b.type == "text" then text = text .. b.text end
  end
  if isError then
    return (text:match(LINE) or T("失败", "Failed")):sub(1, 120)
  end
  local f = SUMMARY[name]
  local ok, s = pcall(function() return f and f(text) end)
  if ok and s then return s end
  if name == "db_shell" then
    return text:match("%(exit (%-?%d+)%)$") and (T("退出码 ", "exit ") .. text:match("%(exit (%-?%d+)%)$")) or nil
  end
  return nil
end

-- ---------------------------------------------------------------- 其他插件的工具

--- 上次读到的其他插件工具：name -> {plugin, description, inputSchema, readOnly}
local external = {}
local guides = {}

--- 重新读取其他插件提供的 AI 工具和使用说明
function M.refresh()
  local ok, r = pcall(db.ai.tools)
  if not ok or type(r) ~= "table" then
    if not ok then db.log("cannot read plugin tools: " .. tostring(r)) end
    r = {}
  end
  external, guides = {}, r.guides or {}
  for _, t in ipairs(r.tools or {}) do external[t.name] = t end
end

--- 其他插件的使用说明，拼成一段系统提示词
function M.guideText()
  local parts = {}
  for _, g in ipairs(guides) do
    parts[#parts + 1] = "## " .. (g.name or g.plugin) .. "\n" .. g.text
  end
  if #parts == 0 then return "" end
  return T("# 插件提供的能力\n名字带 `__` 的工具来自下面这些插件。\n\n", "# Plugin capabilities\nTools whose names contain `__` come from these plugins.\n\n") .. table.concat(parts, "\n\n")
end

local function sorted(t)
  local keys = {}
  for k in pairs(t) do keys[#keys + 1] = k end
  table.sort(keys)
  return keys
end

--- 当前设置下可用的工具（含其他插件的）
function M.active()
  local mode = require("config").get().actions
  local out = {}
  for _, t in ipairs(list) do
    if not (t.action and mode == "off") then out[#out + 1] = t end
  end
  for _, name in ipairs(sorted(external)) do
    local e = external[name]
    if e.readOnly or mode ~= "off" then
      local schema = e.inputSchema or {}
      local req = schema.required
      out[#out + 1] = {
        name = name,
        description = e.description,
        params = schema.properties or {},
        required = (type(req) == "table" and #req > 0) and req or nil,
        action = not e.readOnly,
        plugin = e.plugin,
      }
    end
  end
  return out
end

--- 写出给 Pi 扩展读取的工具清单
function M.manifest()
  local out = {}
  for _, t in ipairs(M.active()) do
    local props = {}
    for k, v in pairs(t.params) do props[k] = v end
    if t.device ~= false then props.serial = SERIAL end
    out[#out + 1] = {
      name = t.name,
      description = t.description,
      parameters = { type = "object", properties = props, required = t.required },
    }
  end
  return db.json.encode(out)
end

--- 其他插件工具的返回值转成 {text}
local function fromPlugin(r)
  if type(r) == "string" then return { text = clip(r) } end
  if r == nil then return { text = "(empty)" } end
  return { text = clip(db.json.encode(r)) }
end

--- 执行一次工具调用，返回 {text} | {image} | {error}
function M.run(name, args, defaultSerial)
  local t = byName[name]
  local e = not t and external[name]
  if e then
    t = {
      name = name,
      action = not e.readOnly,
      fn = function(dev, a) return fromPlugin(db.ai.call(name, a, dev)) end,
      describe = function(a)
        local s = db.json.encode(a)
        return (e.plugin or "") .. " · " .. (e.tool or name) .. (s ~= "{}" and s ~= "[]" and ("：" .. s:sub(1, 200)) or "")
      end,
    }
  end
  if not t then return { error = "No such tool: " .. tostring(name) } end
  local mode = require("config").get().actions
  if t.action and mode == "off" then return { error = "Device actions are off (read-only mode)" } end
  args = args or {}

  local dev
  if t.device ~= false then
    local serial = args.serial or defaultSerial
    if not serial or serial == "" then
      local devs = db.devices()
      if #devs == 1 then serial = devs[1].serial end
    end
    if not serial or serial == "" then return { error = "No device given; call db_devices and pass serial" } end
    dev = db.device(serial)
    if not dev then return { error = "Device not found: " .. serial } end
  end

  if t.action and mode == "confirm" then
    local what = t.describe and t.describe(args) or name
    if not db.ui.confirm(T("AI 助手要操作设备", "The AI assistant wants to act on the device"), (dev and (dev.model or dev.serial) .. T("：", ": ") or "") .. what) then
      return { error = "The user declined this action" }
    end
  end

  if e then args.serial = nil end
  local ok, r = pcall(t.fn, dev, args)
  if not ok then return { error = (tostring(r):gsub("^.-:%d+: ", "")) } end
  if type(r) == "table" then return r end
  return { text = tostring(r) }
end

return M
