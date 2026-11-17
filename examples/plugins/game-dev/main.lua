-- 游戏开发工具（综合示例）
--
-- 功能：
--   1. 设置项：测试包路径、包名、装完是否启动、是否允许降级
--   2. 设备菜单「游戏/安装测试包并启动」：用设置里的路径，没有就弹文件选择，带进度条安装并启动
--   3. 设备面板：前台应用、Lua 报错统计（徽标提醒）、最近日志、常用操作按钮
--   4. 日志：统计 Lua error 数量，并把 "LUA ERROR" 行高亮
--   5. AI（MCP）工具：game-dev__restart_game
--   6. 网页面板：web/index.html，用 SDK 与插件通信
--
-- 把整个 game-dev 文件夹放进插件目录，启用后在插件设置里填包名和测试包路径。

local settings = db.settings or {}
local pkg = settings.pkg or 'com.example.game'
local luaTag = settings.luaTag or 'Unity'

local luaErrors = 0     -- 累计的 Lua 报错数
local lastError = ''    -- 最近一条报错
local lastFg = '—'      -- 最近看到的前台应用
local subs = {}         -- 每台设备的日志订阅：serial -> sub
local recent = {}       -- 每台设备最近几行 Lua 日志：serial -> {line}
local apkPath = settings.apk or ''

local function short(dev)
  if not dev then return '（无设备）' end
  if dev.model and dev.model ~= '' then return dev.model end
  return dev.serial
end

-- ---------------- 安装 + 启动 ----------------

--- 安装一个 APK 到设备，带进度条提示；成功返回 true
local function installTo(dev, path)
  if not path or path == '' then
    db.notify('没有测试包路径，请在插件设置里填，或者在菜单里选择文件', { kind = 'warn' })
    return false
  end
  local p = db.ui.progress('正在安装测试包到 ' .. short(dev))
  p:set(0.1, '推送到设备…')
  local ok, err = pcall(dev.install, dev, path, {
    replace = true,
    grant = true,
    downgrade = settings.downgrade ~= false,
  })
  if not ok then
    p:close()
    db.error('安装失败：' .. tostring(err))
    db.notify('安装失败：' .. tostring(err), { kind = 'error' })
    return false
  end
  p:set(0.9, '启动应用…')
  if settings.launch ~= false then
    dev:launch(pkg)
  end
  p:set(1, '完成')
  p:close()
  db.notify('已安装 ' .. pkg .. ' 到 ' .. short(dev), { kind = 'ok' })
  return true
end

--- 菜单：安装测试包并启动（设置里有路径就直接用，否则弹文件选择）
db.menu('device', '游戏/安装测试包并启动', { icon = 'package' }, function(dev)
  local path = apkPath
  if path == '' then
    path = db.ui.pickFile({ accept = '.apk' })
    if not path then return nil end   -- 用户取消
  end
  -- 安装可能要几十秒，放到后台协程里，菜单点击立刻返回
  db.spawn(function()
    installTo(dev, path)
    db.repaint('gamedev')
  end)
  return '正在安装…'
end)

db.menu('device', '游戏/重启应用', { icon = 'refresh' }, function(dev)
  db.spawn(function()
    dev:stop(pkg)
    db.wait(0.5)
    dev:launch(pkg)
    db.notify('已重启 ' .. pkg, { kind = 'ok' })
  end)
  return '正在重启'
end)

db.menu('device', '游戏/清空应用数据…', { icon = 'eraser' }, function(dev)
  if not db.ui.confirm('清空 ' .. pkg .. ' 的数据？', '账号、存档和设置都会被删除，无法恢复。') then
    return nil
  end
  db.spawn(function()
    dev:clear(pkg)
    dev:launch(pkg)
    db.notify('已清空并重新启动', { kind = 'ok' })
  end)
  return '已清空'
end)

-- 文件浏览器：对 APK 直接安装
db.menu('file', '游戏/安装这个 APK', { icon = 'package', filter = { ext = { 'apk' } } }, function(dev, path)
  db.spawn(function()
    installTo(dev, path)
  end)
  return '正在安装…'
end)

-- 工具菜单：打开记录面板（窗口）
db.menu('tools', '游戏/打开日志统计', { icon = 'activity' }, function()
  db.repaint('gamedev-window')
end)

-- ---------------- 设备面板 ----------------

db.panel('gamedev', { title = '游戏开发', icon = 'activity', dock = 'device', refresh = 2 }, function(ui, dev)
  ui.heading('游戏开发工具')
  if not dev then
    ui.label('请选择一台设备', { kind = 'warn' })
    return
  end

  ui.label('设备：' .. short(dev))
  ui.label('包名：' .. pkg)
  ui.label('前台应用：' .. lastFg)

  -- 用一个协程定期刷新前台应用（渲染函数里不能调用会等待的 API）
  ui.label('Lua 报错：' .. luaErrors .. (luaErrors > 0 and '  ⚠' or ''), {
    kind = luaErrors > 0 and 'warn' or 'ok',
  })
  if lastError ~= '' then ui.code(lastError) end

  ui.separator()
  ui.row(function()
    if ui.button('安装并启动', { id = 'install', kind = 'primary' }) then
      db.spawn(function()
        local path = apkPath ~= '' and apkPath or db.ui.pickFile({ accept = '.apk' })
        if path then installTo(dev, path) end
        db.repaint('gamedev')
      end)
    end
    if ui.button('重启', { id = 'restart' }) then
      db.spawn(function()
        dev:stop(pkg)
        db.wait(0.3)
        dev:launch(pkg)
      end)
    end
    if ui.button('清日志计数', { id = 'clearerr' }) then
      luaErrors = 0
      lastError = ''
      -- db.badge / db.repaint 会等主进程，渲染函数里要放进 db.spawn
      db.spawn(function()
        db.badge(dev, nil)
        db.repaint('gamedev')
      end)
    end
  end)

  ui.separator()
  ui.label('常用入口')
  ui.row(function()
    if ui.button('主界面', { id = 'launch' }) then
      db.spawn(function() dev:launch(pkg) end)
    end
    if ui.button('强制停止', { id = 'stop', kind = 'danger' }) then
      db.spawn(function() dev:stop(pkg) end)
    end
  end)

  ui.separator()
  ui.label('最近的 Lua 相关日志')
  local shown = recent[dev.serial] or {}
  ui.code(#shown > 0 and table.concat(shown, '\n') or '（没有匹配的日志）')
end)

-- 窗口面板：与设备无关的说明
db.panel('gamedev-window', { title = '日志统计', icon = 'logs', dock = 'window', refresh = 2 }, function(ui)
  ui.heading('Lua 报错统计')
  ui.label('累计：' .. luaErrors .. ' 条')
  if lastError ~= '' then
    ui.code(lastError)
  else
    ui.text('还没有捕获到 Lua 报错。\n日志里出现 "lua error"（不区分大小写）时会计数并高亮。')
  end
  ui.separator()
  ui.label('提示：日志面板里匹配的行会被标红并加上 “LUA” 标签。')
end)

-- ---------------- 日志：统计 Lua error 数 + 高亮 ----------------

-- 日志高亮：所有设备通用
db.logcat.highlight('LUA ERROR', { tag = luaTag, kind = 'error', label = 'LUA ERROR' })
db.logcat.highlight('NullReferenceException', { tag = luaTag, kind = 'warn', label = 'NRE' })

--- 给一台设备订阅日志（重复调用不会重复订阅）
local function watchDevice(dev)
  if not dev or subs[dev.serial] then return end
  subs[dev.serial] = db.logcat.watch(dev, { tag = luaTag }, function(line)
    -- 缓存最近的行：面板渲染函数里不能再查日志
    local list = recent[dev.serial] or {}
    list[#list + 1] = line.raw
    if #list > 10 then table.remove(list, 1) end
    recent[dev.serial] = list

    local msg = (line.msg or ''):lower()
    if not msg:find('lua error', 1, true) then return end
    luaErrors = luaErrors + 1
    lastError = line.tag .. ': ' .. line.msg
    -- 徽标提醒：设备卡片上直接看到数量
    db.badge(dev, luaErrors .. ' Lua 错误', { kind = 'warn', tip = lastError })
    if luaErrors == 1 or luaErrors % 5 == 0 then
      db.notify('Lua 报错 ' .. luaErrors .. ' 条：' .. line.msg, { kind = 'error' })
    end
    db.repaint('gamedev')
  end)
end

for _, d in ipairs(db.devices()) do watchDevice(d) end

db.on('device.connected', function(dev)
  watchDevice(dev)
  db.log('设备接入：' .. tostring(dev and dev.serial))
end)

db.on('device.disconnected', function(dev)
  if dev and subs[dev.serial] then
    subs[dev.serial]:stop()
    subs[dev.serial] = nil
  end
end)

-- 前台应用：只在面板打开时才有意义，但这里为了演示一直在记录
db.on('app.foreground', function(dev, name)
  if not dev then return end
  if name == pkg then
    lastFg = name
  else
    lastFg = tostring(name) .. '（不在游戏里）'
  end
end)

-- ---------------- 性能浮层指标 ----------------

db.perf.metric('luerr', { label = 'LUA', unit = '' }, function()
  return luaErrors
end)

db.perf.metric('fb', { label = '前台', unit = '' }, function(dev)
  if not dev then return nil end
  local fg = dev:foreground()
  if fg == pkg then return 1 end
  return 0
end)

-- ---------------- AI（MCP）工具 ----------------

db.mcp.tool('restart_game', {
  description = '强制停止并重新启动游戏，返回一句话说明',
  params = {
    package = { type = 'string', description = '包名，默认用插件设置里的包名' },
    clear = { type = 'boolean', description = '重启前是否清空应用数据' },
  },
  required = {},
}, function(dev, args)
  local target = (args and args.package) or pkg
  if not dev then
    -- 没指定设备时，挑第一台在线设备
    local list = db.devices()
    for _, d in ipairs(list) do
      if d.state == 'device' then dev = d break end
    end
  end
  if not dev then return '没有可用的设备' end
  if args and args.clear then dev:clear(target) end
  dev:stop(target)
  db.wait(0.5)
  dev:launch(target)
  return '已重启 ' .. target .. '（' .. dev.serial .. '）'
end)

-- ---------------- 网页面板 ----------------
-- web/index.html 里用 <script src="/plugin-web/sdk.js"> 和 droidbridge.call('stats') 通信

db.webpanel('stats', { title = '游戏统计', icon = 'grid', dock = 'window', entry = 'web/index.html' })

db.web.handle('stats', function(dev, args)
  local fg = nil
  local fps = nil
  if dev then
    fg = dev:foreground()
    local perf = dev:perf()
    if type(perf) == 'table' then fps = perf.fps end
  end
  return {
    package = pkg,
    foreground = fg,
    fps = fps,
    errors = luaErrors,
    lastError = lastError,
    devices = #db.devices(),
  }
end)

-- 每 3 秒把统计推给打开着的网页面板
db.every(3, function()
  db.web.emit('stats', { errors = luaErrors, ts = os.time() })
end)

-- ---------------- 设置变化 ----------------
db.on('settings.changed', function(values)
  pkg = values.pkg or pkg
  apkPath = values.apk or ''
  luaTag = values.luaTag or luaTag
  db.log('设置已更新：pkg=' .. pkg .. ' apk=' .. apkPath)
end)

-- ---------------- 状态栏 ----------------
db.status('gamedev', '游戏开发 · ' .. (luaErrors > 0 and (luaErrors .. ' 错误') or '就绪'), {
  kind = luaErrors > 0 and 'warn' or 'ok',
  tip = '点击查看日志统计',
  onClick = function()
    db.repaint('gamedev-window')
    db.notify('前台应用：' .. lastFg)
  end,
})

db.log('游戏开发工具已加载，包名 ' .. pkg)
