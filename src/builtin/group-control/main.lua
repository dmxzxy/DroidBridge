-- 群控：在一台设备上的操作同步到其他群控设备
-- 同步与落点扩散圈由主程序完成（db.link），插件只管理分组、按钮和状态栏

local EN = db.lang == "en"
local function T(zh, en) return EN and en or zh end

local group = {}   -- 群控中的设备 serial -> true
local sizes = {}   -- 设备画面尺寸 serial -> {w, h}
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
    if z then
      local r = math.min(z.w, z.h) / math.max(z.w, z.h)
      lo = lo and math.min(lo, r) or r
      hi = hi and math.max(hi, r) or r
    end
  end
  return lo ~= nil and hi / lo > 1.08
end

local refresh

-- 同步分组、投屏卡片上的群控按钮与状态栏
refresh = function()
  local list = members()
  skew = skewed()
  db.link(list)
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

db.on("device.action", function(dev, act)
  local src = dev.serial
  if not group[src] then return end
  if tonumber(act.w) and act.w > 0 and tonumber(act.h) and act.h > 0 then
    sizes[src] = { w = act.w, h = act.h }
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
