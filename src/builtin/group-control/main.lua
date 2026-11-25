-- 群控：在一台设备上的操作同步到其他群控设备（坐标由主程序按各设备分辨率换算）

local EN = db.lang == "en"
local function T(zh, en) return EN and en or zh end

local group = {}   -- 群控中的设备 serial -> true
local sizes = {}   -- 设备最近一次操作时的画面尺寸 serial -> {w, h}
local marks = {}   -- 从机落点标记的令牌，新标记出现时旧的不再清除
local skew = false

local function members()
  local out = {}
  for s in pairs(group) do out[#out + 1] = s end
  table.sort(out)
  return out
end

-- 群控设备的屏幕比例是否相差超过 8%
local function skewed()
  local lo, hi
  for s in pairs(group) do
    local z = sizes[s]
    if z and z.w > 0 and z.h > 0 then
      local r = math.min(z.w, z.h) / math.max(z.w, z.h)
      lo = lo and math.min(lo, r) or r
      hi = hi and math.max(hi, r) or r
    end
  end
  return lo ~= nil and hi / lo > 1.08
end

local refresh

-- 投屏卡片上的群控按钮与状态栏
refresh = function()
  local list = members()
  skew = skewed()
  db.menu("toolbar", T("群控", "Group control"), { id = "toggle", icon = "link", pin = true, on = list }, function(dev)
    local s = dev.serial
    group[s] = not group[s] or nil
    refresh()
    if group[s] and #members() < 2 then
      return T("再给至少一台设备打开群控", "Turn on group control for at least one more device")
    end
  end)
  if #list == 0 then
    db.status("sync", nil)
    return
  end
  db.status("sync", T("群控 ", "Sync ") .. #list, {
    kind = skew and "warn" or "info",
    tip = (skew and T("设备屏幕比例不一致，边缘点击可能偏移\n", "Screen aspect ratios differ, edge taps may land off target\n") or "")
      .. T("点击退出群控", "Click to stop group control"),
    onClick = function()
      group = {}
      refresh()
    end,
  })
end

-- 在从机画面上短暂显示同步落点
local function mark(s, raw)
  local tok = {}
  marks[s] = tok
  db.overlay({ serial = s }, { { type = "point", x = raw.x, y = raw.y, color = "#3fb27f" } }, { w = raw.w, h = raw.h })
  db.after(0.6, function()
    if marks[s] == tok then
      marks[s] = nil
      db.overlay({ serial = s }, nil)
    end
  end)
end

db.on("device.input", function(dev, raw)
  local src = dev.serial
  if tonumber(raw.w) and raw.w > 0 and tonumber(raw.h) and raw.h > 0 then
    sizes[src] = { w = raw.w, h = raw.h }
  end
  if not group[src] then return end
  local others = {}
  for s in pairs(group) do
    if s ~= src then others[#others + 1] = s end
  end
  if #others == 0 then return end
  if raw.t == "clipboard" and not raw.paste then
    for _, s in ipairs(others) do
      local d = db.device(s)
      if d then d:setClipboard(raw.text or "") end
    end
    return
  end
  db.broadcast(others, raw)
  if raw.t == "touch" and raw.action == 0 and sizes[src] then
    for _, s in ipairs(others) do mark(s, raw) end
    if skewed() ~= skew then refresh() end
  end
end)

db.on("device.disconnected", function(dev)
  if group[dev.serial] then
    group[dev.serial] = nil
    refresh()
  end
end)

refresh()
