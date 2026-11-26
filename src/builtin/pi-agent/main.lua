local agent = require("agent")
local config = require("config")
local T = require("i18n")
require("settings")

local function key(dev)
  return dev and dev.serial or ""
end

local function call(fn)
  local ok, err = pcall(fn)
  if not ok then return { error = (tostring(err):gsub("^.-:%d+: ", "")) } end
  return { ok = true }
end

db.webpanel("chat", { title = "AI", icon = "target", dock = "device", entry = "web/index.html" })
db.webpanel("window", { title = T("AI 助手", "AI Assistant"), icon = "target", dock = "window", entry = "web/index.html" })

db.web.handle("state", function(dev)
  return agent.state(key(dev))
end)

db.web.handle("info", function(dev)
  local c = config.get()
  return {
    pi = agent.check(),
    custom = config.custom(c),
    model = config.custom(c) and c.model or "",
    models = config.custom(c) and c.models or {},
    device = dev and (dev.model or dev.name or dev.serial) or nil,
    actions = c.actions,
  }
end)

db.web.handle("model.use", function(_, a)
  local c = config.set({ model = tostring(a.id or "") })
  agent.setModel(c.model)
  db.web.emit("settings", {})
  db.repaint("settings")
  return { ok = true }
end)

db.web.handle("actions.set", function(_, a)
  local v = tostring(a.value or "")
  if v ~= "confirm" and v ~= "auto" and v ~= "off" then return { error = T("无效的设备操作设置", "Invalid device action setting") } end
  config.set({ actions = v })
  db.web.emit("settings", {})
  db.repaint("settings")
  return { ok = true }
end)

db.web.handle("send", function(dev, a)
  local text = tostring(a.text or ""):gsub("^%s+", ""):gsub("%s+$", "")
  if text == "" then return { ok = true } end
  return call(function() agent.ask(key(dev), text) end)
end)

db.web.handle("abort", function(dev)
  agent.abort(key(dev))
  return { ok = true }
end)

db.web.handle("reset", function(dev)
  return call(function() agent.reset(key(dev)) end)
end)

local SENT = T("已发给 AI 助手，在设备侧栏的「AI」里查看", "Sent to the AI assistant; see \"AI\" in the device sidebar")

db.menu("logcat", T("问 AI", "Ask AI"), { icon = "target" }, function(dev, line)
  agent.ask(key(dev), T("解释这条日志，必要时查看前后的日志再判断原因：", "Explain this log line; check the surrounding logs if needed before deciding the cause:") .. "\n```\n" .. line .. "\n```")
  return SENT
end)

db.menu("app", T("AI 诊断", "AI Diagnose"), { icon = "target" }, function(dev, pkg)
  agent.ask(key(dev), T("应用 " .. pkg .. " 有什么问题？查看它的崩溃记录和日志，分析原因并给出建议。",
    "What is wrong with the app " .. pkg .. "? Check its crash records and logs, find the cause and suggest what to do."))
  return SENT
end)

db.on("plugin.unload", function()
  agent.stopAll()
end)
