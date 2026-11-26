-- 插件配置：模型服务、模型、助手行为；保存在插件数据目录
local T = require("i18n")
local M = {}

local KEY = "config"
local PROVIDER = "droidbridge"

local DEFAULT = {
  baseUrl = "",
  api = "openai-completions",
  token = "",
  vision = true,
  models = {},
  model = "",
  thinking = "",
  actions = "confirm",
}

--- 当前配置（缺的字段用默认值）
function M.get()
  local c = db.store.get(KEY) or {}
  local out = {}
  for k, v in pairs(DEFAULT) do
    if c[k] == nil then out[k] = v else out[k] = c[k] end
  end
  if type(out.models) ~= "table" then out.models = {} end
  return out
end

--- 合并保存部分字段
function M.set(patch)
  local c = M.get()
  for k, v in pairs(patch or {}) do
    if DEFAULT[k] ~= nil then c[k] = v end
  end
  local seen, list = {}, {}
  for _, m in ipairs(c.models) do
    m = tostring(m):gsub("^%s+", ""):gsub("%s+$", "")
    if m ~= "" and not seen[m] then seen[m] = true; list[#list + 1] = m end
  end
  c.models = list
  if c.model == "" or not seen[c.model] then c.model = list[1] or "" end
  db.store.set(KEY, c)
  return c
end

--- 是否配置了自己的模型服务
function M.custom(c)
  c = c or M.get()
  return c.baseUrl ~= "" and c.model ~= ""
end

M.PROVIDER = PROVIDER

--- 写给 Pi 的 models.json 内容
function M.modelsJson(c)
  local input = c.vision and { "text", "image" } or { "text" }
  local models = {}
  for _, id in ipairs(c.models) do models[#models + 1] = { id = id, input = input } end
  return db.json.encode({
    providers = {
      [PROVIDER] = { baseUrl = c.baseUrl, api = c.api, apiKey = "$DB_PI_API_KEY", models = models },
    },
  })
end

local function trim(u)
  return (u:gsub("/+$", ""))
end

--- 从接口拉取模型列表
function M.fetchModels(c)
  if c.baseUrl == "" then error(T("先填写接口地址", "Enter the API URL first")) end
  local base = trim(c.baseUrl)
  local headers, urls = {}, {}
  if c.api == "anthropic-messages" then
    headers["x-api-key"] = c.token
    headers["anthropic-version"] = "2023-06-01"
    urls = { base .. "/v1/models", base .. "/models" }
  elseif c.api == "google-generative-ai" then
    urls = { base .. "/models?key=" .. c.token }
  else
    if c.token ~= "" then headers["Authorization"] = "Bearer " .. c.token end
    urls = { base .. "/models", base .. "/v1/models" }
  end
  local lastErr
  for _, url in ipairs(urls) do
    local ok, r = pcall(db.http.request, { url = url, headers = headers, timeout = 20 })
    if not ok then
      lastErr = tostring(r)
    elseif r.status >= 200 and r.status < 300 then
      local ok2, j = pcall(db.json.decode, r.body or "")
      if ok2 and type(j) == "table" then
        local list = {}
        for _, m in ipairs(j.data or j.models or {}) do
          local id = m.id or m.name
          if type(id) == "string" then list[#list + 1] = (id:gsub("^models/", "")) end
        end
        table.sort(list)
        return list
      end
      lastErr = T("接口返回的不是模型列表", "The API did not return a model list")
    else
      lastErr = "HTTP " .. r.status .. " " .. tostring(r.body or ""):sub(1, 200)
    end
  end
  error(lastErr or T("拉取失败", "Failed to fetch"))
end

return M
