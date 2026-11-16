# DroidBridge 插件开发

DroidBridge 的插件用 **Lua 5.4** 编写：一个文件夹里放 `plugin.json` 和 `main.lua`，程序会把它们跑在一个独立的子进程里，插件通过 `db` 这些函数访问设备、读写文件，并向界面注册菜单、面板、状态栏等“贡献”。插件不能直接改界面 —— 界面由主程序绘制，插件只描述“要显示什么”。

面向实现者的协议细节见 [SPEC.md](./SPEC.md)。

---

## 1. 快速开始

### 方式一：在界面里新建

1. 打开 **设置 → 插件**
2. 点右上角的 **+**，填一个 id（小写字母、数字、`_`、`-`）和名称
3. 程序会在插件目录里生成 `<id>/plugin.json` 和 `<id>/main.lua` 并直接启用
4. 点文件夹图标打开插件目录，编辑 `main.lua`，保存后 1 秒左右自动重新加载

### 方式二：手动放置

把插件文件夹放进插件目录（设置 → 插件 里悬停文件夹图标能看到完整路径，通常是数据目录下的 `plugins/`），然后打开它的开关启用。

启用时会列出插件申请的权限，确认后才会运行。新放进来的插件默认是 **停用** 状态。

### 第一个插件

`plugins/hello/plugin.json`：

```json
{
  "id": "hello",
  "name": "Hello",
  "version": "1.0.0",
  "main": "main.lua"
}
```

`plugins/hello/main.lua`：

```lua
db.log("Hello 插件已加载，电脑名：" .. db.node)

-- 设备右键菜单：Hello ▸ 打招呼
db.menu("device", "Hello/打招呼", { icon = "bell" }, function(dev)
  db.notify("你好，" .. (dev.model or dev.serial), { kind = "ok" })
  return "已打招呼"
end)

-- 状态栏项
db.status("hello-state", "Hello", { kind = "info", tip = "来自 Hello 插件", onClick = function()
  db.notify("状态栏被点击了")
end })
```

启用后：设备行右键会出现 **插件 ▸ Hello ▸ 打招呼**，窗口底部的状态栏会出现一个 `Hello` 状态项。

在 **设置 → 插件** 里点 **+** 新建插件，会生成一份带菜单、面板和事件示例的 `main.lua`，可以从它改起。

---

## 2. `plugin.json`

```json
{
  "id": "mygame-dev",
  "name": "MyGame 开发工具",
  "version": "1.0.0",
  "description": "安装测试包、查看 FPS 和 Lua 报错",
  "author": "你",
  "main": "main.lua",
  "permissions": ["device.shell", "device.install", "device.apps"],
  "match": { "apps": ["com.my.game"], "models": ["Pixel*"], "serials": ["*"] },
  "settings": [
    { "name": "apk", "label": "测试包路径", "type": "file", "default": "" },
    { "name": "pkg", "label": "包名", "type": "text", "default": "com.my.game" },
    { "name": "accept", "label": "接受降级安装", "type": "bool", "default": false }
  ]
}
```

| 字段 | 必填 | 说明 |
|---|---|---|
| `id` | 是 | `[a-z0-9][a-z0-9_-]{0,39}`，全局唯一。与文件夹名无关 |
| `name` | 是 | 显示名，会出现在菜单、面板标题和设置里 |
| `version` | 否 | 随便写，只用于显示 |
| `description` | 否 | 一句话说明，显示在插件列表里 |
| `author` | 否 | 作者 |
| `main` | 否 | 入口文件，默认 `main.lua`，相对插件目录 |
| `permissions` | 否 | 权限列表，见下一节。不写表示只需要“基础能力” |
| `match` | 否 | 限定插件能看到的设备，见下 |
| `settings` | 否 | 设置项 schema，界面会据此生成表单 |

### `match`：限制插件能看到哪些设备

```json
{ "apps": ["com.my.game", "com.my.*"], "models": ["Pixel*"], "serials": ["*"] }
```

- 每个字段是 glob 列表（`*` 通配），字段内部 **OR**，字段之间 **AND**
- `apps`：设备上装了其中任意一个包才可见（`hasApp` 例外，这项过滤由主程序内部完成，不需要 `device.apps` 权限）
- `models`：设备型号，`serials`：序列号
- 不写就表示“所有设备”

插件只能看到、操作匹配到的设备；界面上的菜单和面板也会按同一规则过滤。

### `settings`：设置项

`type` 支持 `text` / `number` / `bool` / `select`（需 `options: [...]`）/ `file` / `dir` / `password`。

用户在插件列表里修改后，插件会收到 `settings.changed` 事件，`db.settings` 立即变成新值。

```lua
db.on("settings.changed", function(values)
  db.log("apk 改成了 " .. tostring(values.apk))
end)
```

---

## 3. 权限

不声明的插件只能做“基础”操作：列出设备、读设备属性 `prop`、注册界面贡献、通知、对话框、插件自己的存储与设置、插件目录内只读文件、自己的数据目录读写、定时器、日志。

| 权限 | 允许 |
|---|---|
| `device.shell` | 在设备上执行任意 shell 命令 |
| `device.files` | `dev:push` / `dev:pull` |
| `device.install` | `dev:install` / `dev:uninstall` |
| `device.apps` | `dev:apps` `hasApp` `launch` `stop` `clear` `foreground`，`app.foreground` 事件 |
| `device.input` | `dev:tap` `swipe` `key` `text` |
| `device.screen` | `dev:screenshot`、`db.overlay` |
| `device.forward` | `dev:forward` / `dev:reverse` / `dev:unforward` |
| `device.logs` | `dev:logcat`、`db.logcat.watch` |
| `device.perf` | `dev:perf` |
| `fs.read` | 读这台电脑上的任意路径 |
| `fs` | 读写这台电脑上的任意路径（含 `fs.read`） |
| `exec` | `db.exec` 运行本机程序、`db.open` 打开文件/网址 |
| `net` | `db.http.request` |
| `clipboard` | `db.clipboard.get` / `set` |
| `media` | `db.media.save` 存入相册 |
| `mcp` | `db.mcp.tool` 注册 AI 工具 |

清单里出现新权限时，插件会变成“需要权限确认”并停止运行，直到用户再次确认。启用时确认的权限会被记录下来，之后清单里只增加权限、不减少，也会再次要求确认。

---

## 4. Lua API

全局表 `db` 也可以 `require("droidbridge")` 得到。所有会等待的调用（shell、文件、网络、对话框……）都是同步写法，底层由调度器切换协程，**不要在面板渲染函数里调用**（见第 5 节）。

### 4.1 基本信息

```lua
db.plugin      -- {id, name, version, dir, dataDir}
db.lang        -- "zh" 或 "en"
db.node        -- 这台电脑的名字
db.settings    -- 只读表：当前设置值
```

### 4.2 日志

```lua
db.log("普通日志")            -- 等同于 print
db.info("同上")
db.debug("调试日志")
db.warn("警告，控制台显示为黄色")
db.error("错误，控制台显示为红色")
```

日志会出现在 **设置 → 插件 → 点进插件 → 控制台**，也通过 WebSocket 实时推送。`print` 等同于 `db.log`。Lua 报错（含调用栈）会自动以 `error` 级记录。

### 4.3 协程与定时器

```lua
db.spawn(function(a) db.log("在后台跑") end, 1)   -- 新建协程，立即返回
db.wait(0.5)                                     -- 等待 0.5 秒（只能在协程里）
local t = db.every(2, function() db.log("每 2 秒一次") end)
local t2 = db.after(5, function() db.log("5 秒后执行一次") end)
t:cancel()
```

定时器和 `db.spawn` 回调都跑在各自的协程里，可以正常调用会等待的 API。

### 4.4 设备

```lua
local devs = db.devices()          -- 列表，已按 match 过滤
local dev = db.device("SERIAL")    -- 或者 nil
```

`dev` 对象：

```lua
dev.serial, dev.model, dev.name, dev.state   -- state 一般是 "device"

dev:prop("ro.product.cpu.abi")               -- 读系统属性（无需权限）
local out, code = dev:shell("ls /sdcard", { timeout = 10 })   -- device.shell
dev:push("D:/a.txt", "/sdcard/a.txt")        -- device.files
dev:pull("/sdcard/a.txt", "D:/a.txt")
dev:install("D:/game.apk", { replace = true, grant = false, downgrade = false })  -- device.install
dev:uninstall("com.my.game")
local apps = dev:apps({ system = false })    -- device.apps：{{package, name, version}, ...}
dev:hasApp("com.my.game")                    -- 布尔
dev:launch("com.my.game", "com.my.game.MainActivity")   -- activity 可省略
dev:stop("com.my.game")
dev:clear("com.my.game")
dev:foreground()                             -- 前台包名或 nil
dev:tap(540, 1200)                           -- device.input
dev:swipe(100, 800, 100, 200, 300)           -- x1, y1, x2, y2, 毫秒
dev:key("BACK")                              -- HOME / BACK / APP_SWITCH / POWER / VOLUME_UP / VOLUME_DOWN / ENTER ...
dev:text("你好")                              -- 输入文字（非 ASCII 走剪贴板）
local png = dev:screenshot({ save = true })  -- device.screen：返回本机 PNG 路径
local port = dev:forward(0, "tcp:1234")      -- device.forward：0 = 自动分配，返回实际端口
dev:reverse("tcp:1234", 0)
dev:unforward(port)
local log = dev:logcat({ lines = 200, level = "E", grep = "Unity", package = "com.my.game" })  -- device.logs
local perf = dev:perf()                      -- device.perf：性能快照（fps/cpu/内存/温度…）
```

插件停止时，它建立的所有端口转发会被主进程自动清理。

### 4.5 日志监听与高亮

```lua
local sub = db.logcat.watch(dev, { level = "E", tag = "Unity", grep = "Lua" }, function(line)
  -- line = {time, pid, tid, level, tag, msg, raw}
  db.log(line.tag .. ": " .. line.msg)
end)
sub:stop()

db.logcat.highlight("LUA ERROR", { tag = "Unity", kind = "error", label = "LUA" })
```

`db.logcat.watch` 需要 `device.logs`。日志面板里匹配到 `highlight` 的行会高亮并带上标签；匹配是不区分大小写的子串。

### 4.6 界面贡献

菜单：

```lua
-- target: device | app | file | tools | toolbar
local item = db.menu("device", "MyGame/安装测试包", {
  icon = "package",
  filter = { hasApp = "com.my.game", ext = { "apk" }, dir = false },
}, function(dev) db.notify("点了菜单") end)
item:remove()
```

- `path` 用 `/` 分级，界面会做成子菜单：`MyGame ▸ 安装测试包`
- `icon` 只能用程序内置的图标名（见下）
- `filter`：`hasApp`（布尔）、`models`、`serials`（glob 列表）由主程序按设备判断；`packages`（glob 列表，匹配包名）、`ext`（glob 列表，匹配扩展名）、`dir`（布尔）由界面判断
- 处理函数参数：`device`/`toolbar` → `fn(dev)`；`app` → `fn(dev, pkg)`；`file` → `fn(dev, path, isDir)`；`tools` → `fn()`
- 返回值：字符串会作为提示显示，其他值只表示成功

面板：

```lua
db.panel("main", { title = "MyGame", icon = "activity", dock = "device", refresh = 1 }, function(ui, dev)
  ui.heading("MyGame")
  if ui.button("重启游戏", { id = "restart", kind = "primary" }) then
    db.spawn(function() dev:shell("am force-stop com.my.game") end)
  end
end)
```

- `dock`：`device`（投屏窗口侧边页签）、`window`（从“工具”菜单打开的窗口）、`settings`（插件设置页里）
- `refresh`：自动重绘间隔（秒），可省略
- 处理函数每次渲染都会被调用，参数 `ui` 用来描述界面，见第 5 节

网页面板：

```lua
db.webpanel("web", { title = "我的网页", dock = "window", entry = "web/index.html" })
db.web.handle("hello", function(dev, args)
  return { ok = true, serial = dev and dev.serial }
end)
db.web.emit("tick", { n = 1 })
```

状态栏与徽标：

```lua
db.status("mygame", "MyGame · 就绪", { kind = "ok", tip = "提示", onClick = function() db.notify("点了") end })
db.status("mygame", nil)   -- 移除

db.badge(dev, "3 错误", { kind = "warn", tip = "最近的 Lua 报错数" })
db.badge(dev, nil)         -- 移除
```

状态显示在窗口最底部的状态栏里（所有插件共用）：`kind = "error"` / `"warn"` 的排在最前；不会自动收起，太多时可以横向滚动。用户右键状态栏可以选择显示哪些插件的状态。文字要短（建议 10 个字以内），只在有信息量时显示，没事就 `db.status(id, nil)` 移除。点击时：给了 `onClick` 就执行它；没给就打开这个插件的设置页（详情和控制台）。

性能浮层指标：

```lua
db.perf.metric("luaerr", { label = "LUA", unit = "" }, function(dev)
  return 3          -- 数字；nil 表示这次没有数据
end)
```

通知、覆盖层、重绘：

```lua
db.notify("完成了", { kind = "ok" })    -- kind: info | ok | warn | error
db.overlay(dev, {
  { type = "rect", x = 100, y = 200, w = 300, h = 120, color = "#ff4d4f", text = "目标" },
  { type = "text", x = 100, y = 180, text = "这里", color = "#ffffff" },
  { type = "point", x = 250, y = 260, color = "#ffd666", text = "点这里" },
})
db.overlay(dev, nil)     -- 清除
db.repaint("main")       -- 让某个面板立刻重绘（省略参数 = 全部）
```

覆盖层坐标是 **设备像素**，会随画面缩放和旋转自动跟随，并且不拦截鼠标（点击仍然作用到设备）。

内置图标名（`icon` 只能用这些，其他会显示默认图标）：
`phone plus more gear x monitor chevDown chevRight max power volUp volDown rotate camera record stop images keyboard paste bell sliders moon sun refresh upload back home recent chevLeft file folder activity grid link package eraser scissors arrowLeft arrowUp bookmark folderPlus sort blank copy edit trash check lock wifi wifiOff headphones download logs pause play target alert`

### 4.7 对话框与进度

```lua
if db.ui.confirm("清空数据？", "应用的全部数据都会被删除，无法恢复。") then ... end
local name = db.ui.prompt("新名称", nil, "默认值")        -- 取消返回 nil
local r = db.ui.form("新建配置", {
  { name = "pkg", label = "包名", type = "text", default = "com.my.game" },
  { name = "reset", label = "先清数据", type = "bool", default = false },
  { name = "mode", label = "模式", type = "select", options = { "debug", "release" } },
})                                                       -- 取消返回 nil
local path = db.ui.pickFile({ accept = ".apk" })          -- 取消返回 nil

local p = db.ui.progress("正在安装测试包")
p:set(0.3, "拷贝中…")
p:set(nil, "等待设备确认…")   -- 不确定进度
p:close()
```

`pickFile` 选中的文件会被上传到这台电脑的临时目录，返回的是 **本机路径**，可以直接 `dev:install(path)`。

### 4.8 本机文件、程序、网络

```lua
db.fs.read(path), db.fs.write(path, data), db.fs.append(path, data)
db.fs.list(dir)      -- {{name, size, mtime, dir}, ...}
db.fs.exists(path)   -- 布尔
db.fs.stat(path)     -- {size, mtime, dir}
db.fs.mkdir(path), db.fs.remove(path), db.fs.copy(a, b)
db.fs.join("a", "b", "c")            -- 拼路径

db.exec("adb", { "devices" }, { cwd = "D:/", timeout = 60, env = { A = "1" }, input = "y\n" })
-- -> { code = 0, stdout = "...", stderr = "" }
db.open("https://example.com")        -- 打开文件或网址
db.http.request({ url = "https://x", method = "GET", headers = {}, body = nil, timeout = 30 })
-- -> { status = 200, headers = {}, body = "..." }
db.clipboard.get(), db.clipboard.set("文字")
db.store.get("key"), db.store.set("key", { any = "值" })   -- JSON 持久化到 dataDir/store.json
db.json.encode(v), db.json.decode(s)
db.media.save("D:/shot.png")          -- 存入相册，返回文件名
```

相对路径相对于 `dataDir`（插件的数据目录）；`plugin://x` 表示插件目录内的文件（只读）。

### 4.9 AI（MCP）工具

```lua
db.mcp.tool("restart_game", {
  description = "重启游戏并回到主界面",
  params = { package = { type = "string", description = "包名，默认 com.my.game" } },
  required = { "package" },
}, function(dev, args)
  dev:shell("am force-stop " .. (args.package or "com.my.game"))
  dev:launch(args.package or "com.my.game")
  return "已重启"
end)
```

需要 `mcp` 权限。工具名在 AI 那边会变成 `<插件id>__<名字>`，例如 `game-dev__restart_game`。参数是 JSON Schema 的 `properties` 部分，`required` 单独给出。

### 4.10 事件

```lua
db.on("device.connected", function(dev) db.log("接入 " .. dev.serial) end)
db.on("device.disconnected", function(dev) end)
db.on("app.foreground", function(dev, pkg) end)   -- 需要 device.apps
db.on("app.installed", function(dev, pkg) end)
db.on("settings.changed", function(values) end)
db.on("plugin.unload", function() end)            -- 停止前收尾
```

- 监听者各自在独立协程里执行，互不阻塞；单个监听出错只记录日志
- 只要没有插件订阅 `app.foreground`，主程序就不会去轮询前台应用
- 插件启动完成后，会为每台在线设备补发一次 `device.connected`

### 4.11 `require`

```lua
local util = require("util")       -- 插件目录 util.lua
local x = require("lib.math")      -- 插件目录 lib/math.lua
```

只在插件目录里查找，结果会缓存。没有 `io`、`debug`、`package`、`os.execute`；`os` 只有 `time` / `clock` / `date`。

---

## 5. 即时模式面板

面板的渲染函数会被反复调用（每次事件、每次 `db.repaint`、每个 `refresh` 周期），写法类似 Unity 的 `OnGUI`：**你描述整棵树，程序负责画**。

```lua
local volume = 5

db.panel("main", { title = "音量", dock = "device" }, function(ui, dev)
  ui.heading("音量")
  ui.label("当前：" .. volume)

  -- 控件返回“本帧的事件值”：按钮是 true/false，滑块/开关/输入框是新的值
  local v = ui.slider("音量", volume, 0, 15, 1, { id = "vol" })
  if v ~= volume then volume = v end

  if ui.button("静音", { id = "mute", kind = "danger" }) then
    volume = 0
    db.repaint("main")     -- 也可以不写，事件本身就会触发一次重绘
  end
end)
```

规则：

1. 有事件时程序先跑一次“事件轮”：`id` 匹配 `event.id` 的控件返回事件值，其他控件返回旧值，这一轮的输出被丢弃
2. 再跑一次“绘制轮”：所有控件返回正常值，输出控件树
3. **渲染函数里不能调用会等待的 API**，否则报错 `panel render must not call blocking APIs; use db.spawn`
   - 禁止：`dev:*`（`dev:shell`、`dev:launch`…）、`db.devices()`、`db.fs.read/list/write`、`db.exec`、`db.open`、`db.http`、`db.clipboard`、`db.notify`、`db.repaint`、`db.badge`、`db.status`、`db.overlay`、`db.ui.*`（对话框/进度条）、`db.web.emit`
   - 可以用：`db.log/info/warn/error`、`db.json.*`、`db.store.get/set`、`db.fs.join`、`db.settings`、`db.plugin`、`db.lang`、`db.node`，以及渲染函数第二个参数 `dev`（直接用 `dev.model`、`dev.serial`，不要再查）
   - 要干活就把整个操作放进 `db.spawn(function() ... end)`；需要的数据提前缓存（例如 `db.devices()` 放到 `db.every` 里）
4. 控件 id 可以省略：程序按 `<父容器>/<类型>:<label>` 自动生成；同名重复会加 `#2`、`#3`
5. 重绘时输入框的焦点和光标位置会被保留

`ui` 的可用控件（与 SPEC 3.5 的控件树一一对应）：

| 调用 | 说明 |
|---|---|
| `ui.heading(text)` / `ui.label(text, opts)` / `ui.text(text)` / `ui.code(text)` | 标题、普通文字（`kind` 可为 `ok/warn/error`）、多行文字、等宽代码块 |
| `ui.separator()` / `ui.space()` | 分隔线 / 空白 |
| `ui.button(label, opts) -> bool` | 按钮，`kind` 可为 `primary` / `danger`，`disabled` 可禁用 |
| `ui.toggle(label, value, opts) -> bool` | 开关 |
| `ui.slider(label, value, min, max, step, opts) -> number` | 滑块 |
| `ui.input(label, value, opts) -> string` | 输入框，`multiline = true` 为多行，`placeholder` 提示文字 |
| `ui.select(label, value, options, opts) -> string` | 下拉框，`options` 可以是 `{ "a", "b" }` 或 `{{value="a", label="A"}}` |
| `ui.progress(value, text)` | 进度条，`value` 为 0–1 或 `nil`（不确定进度） |
| `ui.table(columns, rows)` | 表格 |
| `ui.chart(label, values, unit)` | 折线图 |
| `ui.image(src)` | 图片，`data:` URI 或 http(s) 地址 |
| `ui.link(text, url)` | 链接，在浏览器里打开 |
| `ui.row(function() ... end)` | 一行里并排放多个控件 |
| `ui.group(title, function() ... end, opts)` | 可折叠分组，`collapsed = true` 默认收起 |

所有控件的 `opts` 都支持 `id`（覆盖自动 id）。

---

## 6. 网页面板与 SDK

想画复杂界面（图表、拖拽、Canvas……）就用网页面板：插件目录里放 `web/index.html`，然后

```lua
db.webpanel("web", { title = "MyGame", dock = "window", entry = "web/index.html" })
```

页面里引入 SDK：

```html
<script src="/plugin-web/sdk.js"></script>
<script>
  // 调用插件里的 db.web.handle
  const r = await droidbridge.call("stats", { since: 60 });
  // 接收 db.web.emit 发来的消息
  droidbridge.on("tick", (data) => { document.title = data.n; });
  // 面板是从哪台设备打开的（可能为空字符串）
  droidbridge.device.serial;
</script>
```

```lua
db.web.handle("stats", function(dev, args)
  return { errors = 3, uptime = 120 }
end)
db.web.emit("tick", { n = 42 })     -- 广播给所有打开的网页面板
```

安全模型：

- 页面跑在 `sandbox="allow-scripts allow-forms"` 的 iframe 里（没有 `allow-same-origin`），拿不到宿主的 cookie、localStorage 和 DOM
- 只能调用 **自己这个插件** 的 `web.handle`，不能调用别的插件或任意接口
- 页面只能加载插件 `web/` 目录下的静态文件（拒绝 `..`），响应带 `Content-Security-Policy: sandbox`

---

## 7. 调试

- **控制台**：设置 → 插件 → 点进插件 → 控制台。实时显示 `db.log/warn/error` 和 Lua 报错的完整调用栈，可以清空、复制
- **热重载**：插件目录里的文件（`.mtime`/大小）一变，1 秒内自动重启该插件，并弹出“已重新加载”的提示。改完 `main.lua` 存盘即可，不用手动操作
- **手动重新加载**：插件列表里每个插件都有 ⟳ 按钮
- **安全模式**：启动时加 `--no-plugins`，所有插件都不加载（列表里仍然能看到并管理）
- **审计日志**：会改变设备或运行程序的操作记录在插件目录下的 `.audit.log`
- **状态**：`running` / `starting` / `stopped` / `crashed`（自动重启中）/ `failed`（60 秒内崩 3 次，不再重启）/ `needs_grant` / `disabled` / `invalid`（清单有错）。崩溃按 1s、2s、4s 重试

---

## 8. 限制

| 限制 | 说明 |
|---|---|
| 语言 | Lua 5.4，只有 `base`（去掉 `dofile`、`loadfile`）、`string`、`table`、`math`、`utf8`、`coroutine`、`os.time/clock/date` |
| CPU | 每次唤醒最多连续执行 **3 秒**，超出在该协程里抛 `script timeout` |
| 内存 | 每个插件最多 **256 MB** |
| 隔离 | 每个插件一个子进程，崩溃不影响 DroidBridge 主程序和其他插件 |
| 并发 | 每个插件同时最多 4 个设备操作；每台设备同时最多 2 个插件操作；每个插件每秒最多 50 次调用，超出排队 |
| 面板渲染 | 5 秒超时；渲染期间禁止等待类 API |
| 指标 | 每个指标函数 3 秒超时，失败显示 `–` |
| 对话框 | 只在发起操作的那个界面弹出；没有界面时广播给所有连接 |

---

## 9. 远程电脑

插件运行在 **你正在操作的这台电脑** 上，作用于这台电脑能看到的 **全部设备**：本机插着的设备，加上已添加的远程电脑上的设备。远程电脑上 **不需要** 安装插件，但需要是支持插件的 DroidBridge 版本（旧版本会提示“版本太旧，请先更新”）。

- 远程设备的 `dev.serial` 形如 `<电脑id>/<序列号>`，`dev.remote` 为 `true`，`dev.computer` 是那台电脑的名字；`match` 规则按原始序列号和型号匹配。
- `dev:shell / push / pull / install / logcat / perf / screenshot / tap …` 都可以用在远程设备上；文件会在两台电脑之间自动传输。
- `dev:forward / reverse` 暂不支持远程设备（端口只能开在设备所在的那台电脑上）。
- 设备连接/断开、前台应用切换等事件也包括远程设备。

---

## 10. 编辑器补全（VS Code）

本目录下的 [`droidbridge.d.lua`](./droidbridge.d.lua) 是 [lua-language-server](https://luals.github.io/) 的 `---@meta` 声明，包含全部 API 的类型和注释。

在插件目录里建 `.luarc.json`：

```json
{
  "runtime.version": "Lua 5.4",
  "workspace.library": ["D:/DroidBridge/docs/plugins"],
  "diagnostics.globals": ["db"]
}
```

（把 `workspace.library` 指向放 `droidbridge.d.lua` 的目录即可。）之后 `db.` 后面会自动列出所有函数和参数说明。

---

## 11. 常见问题

**Q：改完代码没反应？**
看插件列表的状态和控制台。常见原因：Lua 语法错误（`invalid`/`failed`）、清单里加了新权限（`needs_grant`）、被 `--no-plugins` 安全模式挡住。

**Q：面板是空白的？**
渲染函数出错会记录到控制台。另外注意渲染函数里不能调用会等待的 API，要用 `db.spawn`。

**Q：菜单没出现？**
检查 `target`（`device`/`app`/`file`/`tools`/`toolbar`）、`filter`（`hasApp`/`ext`/`dir`/`packages`）、`match` 是否把设备过滤掉了。设备右键菜单里的插件项在首次拉取贡献后出现，最多几秒。

**Q：为什么 `dev:shell` 报错“没有权限”？**
`plugin.json` 的 `permissions` 里要写 `device.shell`，改完需要在列表里重新启用并确认权限。

**Q：插件能读写我的文件吗？**
默认只能读写自己的数据目录和插件目录（只读）。要访问其他路径必须声明 `fs.read` 或 `fs`，启用时用户会看到这条权限。

**Q：怎么卸载插件？**
在插件列表里停用，然后点“打开插件目录”把文件夹删掉，再点“重新扫描”。插件数据在 `plugins/.data/<id>/`，可以一起删。

**Q：插件能拿到设备的截图吗？**
`dev:screenshot()` 需要 `device.screen`，返回本机 PNG 路径，接着可以用 `db.ui.pickFile` 之外的 `db.fs`/`db.http`/`db.media.save` 处理它。
