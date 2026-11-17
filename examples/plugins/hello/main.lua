-- Hello 示例插件：设备菜单、带按钮 / 滑块 / 表格的设备面板、状态栏、通知。
-- 把整个 hello 文件夹放进插件目录，然后在 设置 → 插件 里启用。
--
-- 注意：面板的渲染函数会被反复执行，里面**不能**调用会等待的 API
-- （dev:* / db.notify / db.repaint / db.badge / db.devices() / db.fs.read / 对话框…）。
-- 要干活就用 db.spawn(function() ... end)，需要的数据提前缓存好。

local count = 0            -- 面板里点了多少次按钮
local volume = 5           -- 滑块的值
local abi = ''             -- 缓存下来的 CPU 架构（渲染函数里不能现查）
local devList = {}         -- 缓存的设备列表（db.devices() 会等待，不能在渲染函数里调用）
local lastShell = '（还没有执行过命令）'

-- 问候语：设置里可以改，设置变了立即生效
local function greeting()
  local v = db.settings and db.settings.greeting
  if type(v) == 'string' and v ~= '' then return v end
  return '你好'
end

local function devName(dev)
  if dev and dev.model and dev.model ~= '' then return dev.model end
  if dev then return dev.serial end
  return '世界'
end

-- 缓存设备列表：db.devices() 会等主进程，不能在渲染函数里调用
local function refreshDevices()
  devList = db.devices() or {}
  db.repaint('hello-main')
end

db.every(5, refreshDevices)

db.log('Hello 插件已加载，电脑名：' .. tostring(db.node))

-- ---------------- 菜单 ----------------

-- 设备卡片 / 设备行的右键菜单：插件 ▸ Hello ▸ 打招呼
db.menu('device', 'Hello/打招呼', { icon = 'bell' }, function(dev)
  count = count + 1
  db.notify(greeting() .. '，' .. devName(dev), { kind = 'ok' })
  db.repaint('hello-main')
  return '已打招呼'
end)

-- 应用管理的右键菜单：只在装了匹配包名的应用时出现
db.menu('app', 'Hello/这个应用的版本', { icon = 'package', filter = { packages = { 'com.*' } } }, function(dev, pkg)
  local out, code = dev:shell('dumpsys package ' .. pkg .. ' | grep versionName')
  if code ~= 0 or out == '' then
    db.notify('读不到 ' .. pkg .. ' 的版本', { kind = 'warn' })
    return nil
  end
  db.notify(out)
  return out
end)

-- 文件浏览器的右键菜单：只对 APK 出现
db.menu('file', 'Hello/查看 APK 大小', { icon = 'file', filter = { ext = { 'apk' } } }, function(dev, path)
  local out = dev:shell('ls -l "' .. path .. '"')
  lastShell = out
  db.notify(out, { kind = 'info' })
  return out
end)

-- “工具”菜单里的窗口面板入口
db.menu('tools', 'Hello/打开窗口示例', { icon = 'monitor' }, function()
  db.repaint('hello-window')
  db.notify('窗口面板在「工具」菜单里也可以打开')
end)

-- ---------------- 设备面板（dock = device，投屏窗口侧边页签） ----------------

db.panel('hello-main', { title = 'Hello', icon = 'activity', dock = 'device' }, function(ui, dev)
  ui.heading('Hello 示例')
  if not dev then
    ui.label('没有选择设备', { kind = 'warn' })
    return
  end

  ui.label('设备：' .. devName(dev))
  ui.label('状态：' .. tostring(dev.state) .. ' · 点击 ' .. count .. ' 次')

  if ui.button('打个招呼', { id = 'hi', kind = 'primary' }) then
    count = count + 1
    -- db.notify 会等主进程回复，渲染函数里不能直接调用
    local name = devName(dev)
    db.spawn(function() db.notify(greeting() .. '，' .. name, { kind = 'ok' }) end)
    -- 事件本身会触发一次重绘，这里不需要再 db.repaint
  end

  -- 滑块：事件轮返回新值，其他时候返回旧值；改动后放到协程里去设置
  local v = ui.slider('音量', volume, 0, 15, 1, { id = 'vol' })
  if v ~= volume then
    volume = v
    db.spawn(function()
      local _, code = dev:shell('media volume --stream 3 --set ' .. volume)
      if code ~= 0 then db.warn('设置音量失败') end
      db.repaint('hello-main')
    end)
  end

  -- 会等待的 API 都放进 db.spawn，渲染函数立刻返回
  if ui.button('读取 CPU 架构', { id = 'abi' }) then
    db.spawn(function()
      local out = dev:shell('getprop ro.product.cpu.abi')
      lastShell = out
      abi = out
      db.repaint('hello-main')
    end)
  end
  if db.settings and db.settings.showCpu ~= false and abi ~= '' then
    ui.label('CPU：' .. abi)
  end

  ui.separator()
  ui.label('最近一次 shell 输出')
  ui.code(lastShell)

  ui.separator()
  local rows = {}
  for _, d in ipairs(devList) do
    rows[#rows + 1] = { d.serial, (d.model ~= '' and d.model or '—'), d.state }
  end
  ui.table({ '序列号', '型号', '状态' }, rows)
end)

-- ---------------- 窗口面板（dock = window） ----------------

db.panel('hello-window', { title = 'Hello 窗口', icon = 'monitor', dock = 'window' }, function(ui)
  ui.heading('这是一个窗口面板')
  ui.text('从「工具」菜单打开，与具体设备无关。')
  ui.progress(nil, '不确定进度示例')
  ui.chart('示例曲线', { 3, 5, 4, 8, 6, 9 }, '')
  ui.link('插件开发文档', 'https://example.com/droidbridge/plugins')
end)

-- ---------------- 状态栏 ----------------

db.status('hello-status', 'Hello · ' .. greeting(), {
  kind = 'ok',
  tip = '来自 Hello 示例插件',
  onClick = function()
    db.notify('状态栏被点击了，目前点击 ' .. count .. ' 次')
  end,
})

-- 设备接入时补一次 CPU 架构，让面板一打开就有内容
db.on('device.connected', function(dev)
  db.spawn(function()
    refreshDevices()
    abi = dev:shell('getprop ro.product.cpu.abi')
    db.repaint('hello-main')
  end)
end)

db.on('device.disconnected', function()
  db.spawn(refreshDevices)
end)

-- 插件停止前收尾
db.on('plugin.unload', function()
  db.log('Hello 插件卸载')
end)
