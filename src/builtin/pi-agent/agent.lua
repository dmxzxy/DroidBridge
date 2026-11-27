-- Pi 进程：每个对话一个 `pi --mode rpc`，工具调用经 extension_ui 通道回到插件
local T = require("i18n")
local tools = require("tools")
local config = require("config")

local M = {}

local CALL = "droidbridge:call"
local MAX_CONV = 6
local IDLE_SEC = 20 * 60
local WIN = db.plugin.dir:find("\\", 1, true) ~= nil

--- key -> 对话 {key, serial, p, busy, items, last, err, seq, pending}
local convs = {}

local function emit(c, kind, data)
  data = data or {}
  data.key, data.kind = c.key, kind
  db.web.emit("ev", data)
end

local function push(c, item)
  c.items[#c.items + 1] = item
  if #c.items > 400 then table.remove(c.items, 1) end
end

local function dirname(p)
  return (p:gsub("[\\/][^\\/]*$", ""))
end

local UNIX_FIND = [[
PATH="$PATH:$HOME/.npm-global/bin:$HOME/.local/bin:$HOME/.bun/bin:$HOME/.volta/bin:/opt/homebrew/bin:/usr/local/bin"
printf '\n__DB__%s__DB__%s__DB__\n' "$PATH" "$(command -v pi)"
]]

--- 用用户的登录 shell 取 PATH 并查找 pi，返回 pi 路径和 PATH
local function unixFind()
  local tries = {
    { "/bin/sh", { "-c", 'exec "${SHELL:-/bin/sh}" -ilc "$1" </dev/null 2>/dev/null', "sh", UNIX_FIND } },
    { "/bin/sh", { "-c", UNIX_FIND } },
  }
  for _, t in ipairs(tries) do
    local ok, r = pcall(db.exec, t[1], t[2], { timeout = 15 })
    local path, pi
    if ok then path, pi = (r.stdout or ""):match("__DB__(.-)__DB__(.-)__DB__") end
    if pi and pi:sub(1, 1) == "/" then return pi, path end
  end
end

local WIN_PATH = [[
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$p = $env:Path + ';' + [Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User') + ';' + $env:APPDATA + '\npm'
[Environment]::ExpandEnvironmentVariables($p)
]]

--- Windows 上取当前和注册表里最新的 PATH，去重后返回目录列表和 PATH 字符串
local function winPath()
  local ok, r = pcall(db.exec, "powershell", { "-NoProfile", "-NonInteractive", "-Command", WIN_PATH }, { timeout = 15 })
  local dirs, seen = {}, {}
  for d in ((ok and r.stdout) or ""):gmatch("[^;\r\n]+") do
    d = d:gsub('^%s*"?', ""):gsub('"?%s*$', ""):gsub("[\\/]+$", "")
    if d ~= "" and not seen[d:lower()] then
      seen[d:lower()] = true
      dirs[#dirs + 1] = d
    end
  end
  return dirs, #dirs > 0 and table.concat(dirs, ";") or nil
end

local function winFind(dirs, names)
  for _, d in ipairs(dirs) do
    for _, n in ipairs(names) do
      local f = d .. "\\" .. n
      if db.fs.exists(f) then return f end
    end
  end
end

--- 自动查找 Pi，返回程序、前置参数和启动时要用的环境变量
function M.command()
  if not WIN then
    local p, path = unixFind()
    if not p then error(T("没有找到 Pi", "Pi not found")) end
    return p, {}, { PATH = path }
  end
  local dirs, path = winPath()
  local env = { PATH = path }
  local p = winFind(dirs, { "pi.exe", "pi.cmd" })
  if not p then error(T("没有找到 Pi", "Pi not found")) end
  if p:lower():match("%.cmd$") then
    local txt = db.fs.read(p) or ""
    local rel = txt:match('"%%~?dp0%%?\\([^"]-%.js)"')
    if not rel then error(T("无法识别 Pi 的安装：", "Cannot recognize the Pi installation: ") .. p) end
    local dir = dirname(p)
    local node = winFind({ dir }, { "node.exe" }) or winFind(dirs, { "node.exe" })
    if not node then error(T("没有找到 Node.js，Pi 需要它才能运行", "Node.js not found, Pi needs it to run")) end
    return node, { dir .. "\\" .. rel }, env
  end
  return p, {}, env
end

--- 对话对应的 Pi 会话 id；renew 时换一个新的
local function sessionId(key, renew)
  local k = "session:" .. key
  local id = not renew and db.store.get(k)
  if not id then
    local hex = {}
    for i = 1, 16 do hex[i] = ("%02x"):format(math.random(0, 255)) end
    id = table.concat(hex, "", 1, 4) .. "-" .. table.concat(hex, "", 5, 6) .. "-" .. table.concat(hex, "", 7, 8) .. "-" .. table.concat(hex, "", 9, 10) .. "-" .. table.concat(hex, "", 11, 16)
    db.store.set(k, id)
  end
  return id
end

--- 根据设置生成启动参数和环境变量
local function launch(c)
  local s = config.get()
  local data = db.plugin.dataDir
  local prog, args, penv = M.command()
  local rel = "sessions/" .. (c.key == "" and "_" or c.key:gsub("[^%w%-_.]", "_"))
  db.fs.mkdir(rel)
  db.fs.mkdir("work")
  local sessions = db.fs.join(data, rel)
  tools.refresh()
  local manifest, guide = tools.manifest(), tools.guideText()
  c.sig = manifest .. guide
  db.fs.write("tools.json", manifest)
  db.fs.write("guides.md", guide)

  local list = {
    "--mode", "rpc", "-ne", "-e", db.fs.join(db.plugin.dir, "pi", "db.ts"),
    "--no-builtin-tools", "--tools", "db_*,*__*", "--no-mcp",
    "-ns", "-np", "-nc", "--no-themes", "--no-approve", "--offline",
    "--system-prompt", db.fs.join(db.plugin.dir, "pi", T("system.md", "system.en.md")),
    "--append-system-prompt", db.fs.join(db.plugin.dir, "pi", T("knowledge.md", "knowledge.en.md")),
    "--session-dir", sessions,
  }
  if guide ~= "" then
    list[#list + 1] = "--append-system-prompt"
    list[#list + 1] = db.fs.join(data, "guides.md")
  end
  list[#list + 1] = "--session-id"
  list[#list + 1] = sessionId(c.key)
  if c.serial ~= "" then
    local d = db.device(c.serial)
    list[#list + 1] = "--append-system-prompt"
    list[#list + 1] = T("当前设备：", "Current device: ") .. c.serial .. (d and d.model and (" (" .. d.model .. ")") or "") .. T("，工具不传 serial 时默认用它。", "; tools use it when serial is omitted.")
  end

  local env = {
    DB_PI_TOOLS = db.fs.join(data, "tools.json"),
    PI_SKIP_VERSION_CHECK = "1",
    PI_TELEMETRY = "0",
    PATH = penv.PATH,
  }
  if config.custom(s) then
    db.fs.mkdir("agent")
    db.fs.write("agent/models.json", config.modelsJson(s))
    env.PI_CODING_AGENT_DIR = db.fs.join(data, "agent")
    env.DB_PI_API_KEY = s.token ~= "" and s.token or "none"
    list[#list + 1] = "--model"
    list[#list + 1] = config.PROVIDER .. "/" .. s.model
  end
  if s.thinking ~= "" then
    list[#list + 1] = "--thinking"
    list[#list + 1] = s.thinking
  end
  for _, a in ipairs(list) do args[#args + 1] = a end
  return prog, args, env
end

local function send(c, obj)
  if c.p and c.p:alive() then c.p:send(db.json.encode(obj)) end
end

local function setBusy(c, busy)
  c.busy = busy
  emit(c, "busy", { busy = busy })
end

--- 执行一次工具调用并把结果交回 Pi
local function onCall(c, req)
  local ok, call = pcall(db.json.decode, req.placeholder or "")
  local r
  if not ok or type(call) ~= "table" then
    r = { error = T("无法解析工具调用", "Cannot parse the tool call") }
  else
    r = tools.run(call.tool, call.args, c.serial)
  end
  send(c, { type = "extension_ui_response", id = req.id, value = db.json.encode(r) })
end

--- 把 Pi 保存的历史消息放到对话记录前面
local function rebuild(c, messages)
  local now = c.items
  local calls = {}
  c.items = {}
  for _, m in ipairs(messages or {}) do
    if m.role == "user" then
      local text = type(m.content) == "string" and m.content or ""
      if type(m.content) == "table" then
        for _, b in ipairs(m.content) do if b.type == "text" then text = text .. b.text end end
      end
      push(c, { role = "user", text = text })
    elseif m.role == "assistant" then
      for _, b in ipairs(m.content or {}) do
        if b.type == "text" and b.text ~= "" then push(c, { role = "ai", text = b.text })
        elseif b.type == "toolCall" then
          local it = { role = "tool", id = b.id, name = b.name, args = b.arguments, done = true }
          calls[b.id or ""] = it
          push(c, it)
        end
      end
    elseif m.role == "toolResult" then
      local it = calls[m.toolCallId or ""]
      if it then
        it.error = m.isError or nil
        it.summary = tools.summary(it.name, m, m.isError)
      end
    end
  end
  for _, it in ipairs(now) do push(c, it) end
  emit(c, "reset", { items = c.items })
end

--- 处理 Pi 输出的一行
local function onLine(c, line)
  local ok, r = pcall(db.json.decode, line)
  if not ok or type(r) ~= "table" then return end
  local t = r.type
  if t == "message_update" then
    local e = r.assistantMessageEvent or {}
    if e.type == "text_delta" then
      local last = c.items[#c.items]
      if not (last and last.role == "ai" and last.open) then
        last = { role = "ai", text = "", open = true }
        push(c, last)
      end
      last.text = last.text .. e.delta
      emit(c, "delta", { text = e.delta })
    end
  elseif t == "message_end" then
    local last = c.items[#c.items]
    if last then last.open = nil end
    local m = r.message or {}
    if m.role == "assistant" then
      c.lastErr = m.stopReason == "error" and m.errorMessage or nil
    end
  elseif t == "tool_execution_start" then
    push(c, { role = "tool", id = r.toolCallId, name = r.toolName, args = r.args })
    emit(c, "tool", { id = r.toolCallId, name = r.toolName, args = r.args })
  elseif t == "tool_execution_end" then
    for i = #c.items, 1, -1 do
      local it = c.items[i]
      if it.role == "tool" and it.id == r.toolCallId then
        it.done, it.error = true, r.isError or nil
        it.summary = tools.summary(r.toolName, r.result, r.isError)
        break
      end
    end
    emit(c, "tool_end", { id = r.toolCallId, error = r.isError, summary = tools.summary(r.toolName, r.result, r.isError) })
  elseif t == "agent_start" then
    setBusy(c, true)
  elseif t == "agent_settled" then
    if c.lastErr and not r.aborted then
      push(c, { role = "error", text = c.lastErr })
      emit(c, "error", { text = c.lastErr })
    end
    c.lastErr = nil
    setBusy(c, false)
    c.last = os.time()
  elseif t == "extension_ui_request" then
    if r.method == "input" and r.title == CALL then
      onCall(c, r)
    elseif r.method == "notify" then
      emit(c, "notice", { text = r.message })
    elseif r.id and (r.method == "select" or r.method == "confirm" or r.method == "input" or r.method == "editor") then
      send(c, { type = "extension_ui_response", id = r.id, cancelled = true })
    end
  elseif t == "response" then
    if r.id == "history" and r.success then
      rebuild(c, r.data and r.data.messages)
    elseif (r.id == "model" or r.command == "set_model") and r.success then
      local m = r.data and (r.data.model or (r.command == "set_model" and r.data))
      if m then
        c.model = (m.provider and (m.provider .. "/") or "") .. (m.id or m.name or "")
        emit(c, "model", { model = c.model })
      end
    elseif r.command == "prompt" and r.success and r.data and r.data.disposition == "handled" then
      setBusy(c, false)
    elseif r.success == false then
      push(c, { role = "error", text = r.error })
      emit(c, "error", { text = r.error })
      if r.command == "prompt" then setBusy(c, false) end
    end
  end
end

--- 结束最久没用的对话进程，给新对话腾位置
local function evict()
  local n, oldest = 0, nil
  for _, c in pairs(convs) do
    if c.p then
      n = n + 1
      if not c.busy and (not oldest or c.last < oldest.last) then oldest = c end
    end
  end
  if n >= MAX_CONV and oldest then M.stop(oldest.key) end
end

local function start(c)
  evict()
  local prog, args, env = launch(c)
  c.err = {}
  local p
  p = db.proc.start(prog, args, {
    cwd = db.fs.join(db.plugin.dataDir, "work"),
    env = env,
    onLine = function(line) onLine(c, line) end,
    onStderr = function(line)
      c.err[#c.err + 1] = line
      if #c.err > 20 then table.remove(c.err, 1) end
    end,
    onExit = function(code)
      if c.p ~= p then return end -- 已经换成新进程
      c.p = nil
      if c.busy then setBusy(c, false) end
      if code ~= 0 and not c.stopping then
        local msg = T("Pi 已退出（", "Pi exited (") .. tostring(code) .. T("）", ")") .. (#c.err > 0 and (T("：", ": ") .. table.concat(c.err, "\n")) or "")
        push(c, { role = "error", text = msg })
        emit(c, "error", { text = msg })
      end
      c.stopping = nil
    end,
  })
  c.p = p
  c.stopping = nil
  c.last = os.time()
  send(c, { id = "model", type = "get_state" })
  if #c.items == 0 then send(c, { id = "history", type = "get_messages" }) end
end

--- 取得（必要时创建）一个对话
function M.get(key)
  local c = convs[key]
  if not c then
    c = { key = key, serial = key, items = {}, busy = false, last = os.time() }
    convs[key] = c
  end
  return c
end

--- 确保 Pi 进程在运行；空闲时如果可用工具变了（插件启停、设置改动），重启进程接着原会话
function M.ensure(key)
  local c = M.get(key)
  if c.p and c.p:alive() and not c.busy then
    tools.refresh()
    if tools.manifest() .. tools.guideText() ~= c.sig then
      c.p:close()
      c.p = nil
    end
  end
  if not (c.p and c.p:alive()) then start(c) end
  return c
end

--- 发一条消息给 Pi
function M.ask(key, text)
  if text:match("^/new%s*$") or text:match("^/clear%s*$") then return M.reset(key) end
  local c = M.ensure(key)
  push(c, { role = "user", text = text })
  emit(c, "user", { text = text })
  local msg = { type = "prompt", message = text }
  if c.busy then msg.streamingBehavior = "steer" end
  send(c, msg)
  c.last = os.time()
  if not c.busy then setBusy(c, true) end
end

function M.abort(key)
  local c = convs[key]
  if c then send(c, { type = "abort" }) end
end

--- 开始新对话
function M.reset(key)
  local c = M.get(key)
  c.items = {}
  sessionId(c.key, true)
  M.stop(key)
  emit(c, "reset", { items = c.items })
end

function M.stop(key)
  local c = convs[key]
  if c and c.p then
    c.stopping = true
    c.p:close()
  end
end

function M.stopAll()
  for k in pairs(convs) do M.stop(k) end
end

local checked

--- 检查 Pi 能否启动，返回 {ok, version, path} 或 {ok=false, error}
function M.check(force)
  if checked and not force then return checked end
  local ok, prog, args, env = pcall(M.command)
  local r
  if not ok then
    r = { ok = false, error = (tostring(prog):gsub("^.-:%d+: ", "")) }
  else
    local path = args[1] or prog
    args[#args + 1] = "--version"
    local ok2, res = pcall(db.exec, prog, args, { timeout = 30, env = env })
    if ok2 and res.code == 0 then
      r = { ok = true, version = (res.stdout or ""):match("[%d%.]+"), path = path }
    else
      r = { ok = false, error = ok2 and ((res.stderr ~= "" and res.stderr) or T("Pi 无法运行", "Pi cannot run")) or tostring(res) }
    end
  end
  checked = r.ok and r or nil
  return r
end

--- 运行中的对话切换模型
function M.setModel(id)
  for _, c in pairs(convs) do
    send(c, { type = "set_model", provider = config.PROVIDER, modelId = id })
  end
end

function M.state(key)
  local c = M.get(key)
  if #c.items == 0 and not c.p and db.store.get("session:" .. key) then
    pcall(M.ensure, key)
  end
  return { key = key, busy = c.busy, items = c.items, model = c.model }
end

db.every(60, function()
  local now = os.time()
  for k, c in pairs(convs) do
    if c.p and not c.busy and now - c.last > IDLE_SEC then M.stop(k) end
  end
end)

return M
