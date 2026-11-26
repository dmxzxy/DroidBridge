-- 插件设置页（显示在 DroidBridge 的插件详情里）
local agent = require("agent")
local config = require("config")
local T = require("i18n")

local M = {}

local API = {
  { value = "openai-completions", label = T("OpenAI 兼容（Chat Completions）", "OpenAI compatible (Chat Completions)") },
  { value = "openai-responses", label = "OpenAI Responses" },
  { value = "anthropic-messages", label = "Anthropic" },
  { value = "google-generative-ai", label = "Google Gemini" },
}
local THINKING = {
  { value = "", label = T("默认", "Default") }, { value = "off", label = T("关闭", "Off") },
  { value = "low", label = T("低", "Low") }, { value = "medium", label = T("中", "Medium") }, { value = "high", label = T("高", "High") },
}
local ACTIONS = {
  { value = "confirm", label = T("操作前确认", "Ask before acting") }, { value = "auto", label = T("直接操作", "Act directly") }, { value = "off", label = T("只读，不操作设备", "Read only, never change the device") },
}
local INSTALL = "npm i -g @earendil-works/pi-coding-agent"

local pi = nil              -- Pi 检查结果
local fetching = false
local fetched = nil         -- 从接口拉到的模型
local fetchMsg, fetchKind = nil, nil
local draftModel = ""

local function repaint() db.repaint("settings") end

--- 配置变化后让对话用新配置重启
function M.changed()
  agent.stopAll()
  db.web.emit("settings", {})
end

local function save(patch)
  db.spawn(function()
    config.set(patch)
    M.changed()
    repaint()
  end)
end

local function recheck()
  pi = nil
  db.spawn(function()
    pi = agent.check(true)
    repaint()
  end)
end

local function fetch()
  if fetching then return end
  fetching, fetchMsg = true, nil
  db.spawn(function()
    local ok, list = pcall(config.fetchModels, config.get())
    fetching = false
    if ok then
      fetched = list
      fetchMsg, fetchKind = #list > 0 and T("找到 " .. #list .. " 个模型，选择要用的", "Found " .. #list .. " models; pick the ones to use") or T("接口没有返回模型，请手动填写", "The API returned no models; enter one by hand"), #list > 0 and "ok" or "warn"
    else
      fetched = nil
      fetchMsg, fetchKind = T("获取失败：", "Failed: ") .. tostring(list):gsub("^.-:%d+: ", ""), "error"
    end
    repaint()
  end)
end

local function addModel(id)
  id = tostring(id or ""):gsub("^%s+", ""):gsub("%s+$", "")
  if id == "" then return end
  local c = config.get()
  local list = c.models
  list[#list + 1] = id
  save({ models = list })
end

local function removeModel(id)
  local list = {}
  for _, m in ipairs(config.get().models) do if m ~= id then list[#list + 1] = m end end
  save({ models = list })
end

local function piSection(ui)
  ui.heading("Pi")
  if not pi then
    ui.label(T("正在检测…", "Checking…"))
  elseif pi.ok then
    ui.row(function()
      ui.label(T("已找到 Pi ", "Found Pi ") .. (pi.version or ""), { kind = "ok" })
      if ui.button(T("重新检测", "Check again"), { id = "recheck" }) then recheck() end
    end)
  else
    ui.label(T("没有找到 Pi，安装后点重新检测", "Pi not found; install it, then check again"), { kind = "error" })
    ui.code(INSTALL)
    ui.row(function()
      if ui.button(T("重新检测", "Check again"), { id = "recheck", kind = "primary" }) then recheck() end
    end)
  end
end

local function modelSection(ui, c)
  ui.heading(T("模型服务", "Model service"))
  ui.label(T("填写模型服务的接口地址和 Token；不填时使用 Pi 自己登录的账号和默认模型", "Enter the API URL and token of a model service; leave empty to use the account and default model Pi is signed in with"))

  local api = ui.select(T("接口协议", "API protocol"), c.api, API, { id = "api" })
  if api ~= c.api then save({ api = api }) end

  local url = ui.input(T("接口地址", "API URL"), c.baseUrl, { id = "baseUrl", placeholder = "https://api.example.com/v1" })
  if url ~= c.baseUrl then save({ baseUrl = (url:gsub("^%s+", ""):gsub("%s+$", "")) }) end

  local tok = ui.input("Token", "", {
    id = "token", password = true,
    placeholder = c.token ~= "" and T("已保存，输入新的可替换", "Saved; type a new one to replace it") or T("没有可不填", "Optional"),
  })
  if tok ~= "" then save({ token = tok }) end

  ui.row(function()
    if ui.button(fetching and T("获取中…", "Fetching…") or T("获取模型列表", "Fetch models"), { id = "fetch", disabled = fetching or c.baseUrl == "" }) then fetch() end
    if fetchMsg then ui.label(fetchMsg, { kind = fetchKind }) end
  end)

  if fetched and #fetched > 0 then
    local have = {}
    for _, m in ipairs(c.models) do have[m] = true end
    local opts = { { value = "", label = T("选择要添加的模型…", "Pick a model to add…") } }
    for _, m in ipairs(fetched) do
      if not have[m] then opts[#opts + 1] = { value = m, label = m } end
    end
    local pick = ui.select(T("添加模型", "Add model"), "", opts, { id = "pick" })
    if pick ~= "" then addModel(pick) end
  end

  ui.row(function()
    local v = ui.input("", draftModel, { id = "draft", placeholder = T("或手动填写模型名，回车添加", "Or type a model name and press Enter") })
    if v ~= draftModel then
      draftModel = ""
      addModel(v)
    end
  end)

  if #c.models > 0 then
    ui.label(T("已配置的模型", "Models"))
    for _, m in ipairs(c.models) do
      ui.row(function()
        if m == c.model then
          ui.label(m .. T("（默认）", " (default)"), { kind = "ok" })
        else
          ui.label(m)
          if ui.button(T("设为默认", "Make default"), { id = "use:" .. m }) then save({ model = m }) end
        end
        if ui.button(T("移除", "Remove"), { id = "rm:" .. m }) then removeModel(m) end
      end)
    end
  elseif c.baseUrl ~= "" then
    ui.label(T("还没有模型，获取列表或手动填写后才会使用这个服务", "No models yet; this service is used once you fetch or add one"), { kind = "warn" })
  end

  local vision = ui.toggle(T("模型支持图片（AI 能看截图）", "The model accepts images (AI can see screenshots)"), c.vision, { id = "vision" })
  if vision ~= c.vision then save({ vision = vision }) end
end

local function assistantSection(ui, c)
  ui.heading(T("助手", "Assistant"))
  local th = ui.select(T("思考强度", "Thinking"), c.thinking, THINKING, { id = "thinking" })
  if th ~= c.thinking then save({ thinking = th }) end
  local ac = ui.select(T("设备操作（点击、输入、shell 命令等）", "Device actions (taps, typing, shell commands…)"), c.actions, ACTIONS, { id = "actions" })
  if ac ~= c.actions then save({ actions = ac }) end
end

db.panel("settings", { title = T("设置", "Settings"), dock = "settings" }, function(ui)
  local c = config.get()
  piSection(ui)
  ui.separator()
  modelSection(ui, c)
  ui.separator()
  assistantSection(ui, c)
end)

db.spawn(function()
  pi = agent.check()
  repaint()
end)

return M
