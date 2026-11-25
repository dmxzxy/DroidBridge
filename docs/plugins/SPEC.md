# DroidBridge 插件系统 — 实现规格（内部）

> 这是实现用的规格，所有模块必须严格遵守这里的协议和命名。面向插件作者的文档另见 `docs/plugins/README.md`。

## 0. 总体

- 插件语言：Lua 5.4（`mlua`，`vendored`）。
- 插件位置：`<data_dir>/plugins/<目录>/plugin.json`。一个目录一个插件。
- **每个已启用的插件运行在独立子进程（插件宿主）里**。子进程 = 当前可执行文件加参数 `--plugin-host`（`droidbridge` 与 `droidbridge-desktop` 都要在 `main` 最前面处理）。主进程与宿主通过 stdin/stdout 交换 NDJSON（每行一个 JSON 对象，UTF-8）。宿主的 stderr 由主进程读取并作为插件控制台的 `error` 级日志。
- 插件不能直接改界面：只能“注册贡献”（菜单、面板等），界面由主程序的前端绘制。
- 插件只在**它所在的电脑**上运行，只能看到这台电脑上的设备。前端访问远程电脑的设备时，所有插件接口都通过 `?peer=<id>` 转发给那台电脑（复用 `local_or_peer` / `peers::proxy_ws`）。
- 安全模式：启动参数 `--no-plugins` 不加载任何插件（仍可在管理界面看到列表）。

## 1. 清单 `plugin.json`

```json
{
  "id": "mygame-dev",            // 必填，[a-z0-9][a-z0-9_-]{0,39}，全局唯一；与目录名无关
  "name": "MyGame 开发工具",      // 必填
  "version": "1.0.0",
  "description": "",
  "author": "",
  "main": "main.lua",            // 默认 main.lua
  "permissions": ["device.shell", "exec"],
  "match": { "apps": ["com.my.game"], "models": ["Pixel*"], "serials": ["*"] }, // 可选，限定插件能看到的设备；各项为 OR 内的 glob 列表，多项之间 AND
  "settings": [
    { "name": "luaDir", "label": "Lua 目录", "type": "dir", "default": "" }
    // type: text | number | bool | select(options:[..]) | file | dir | password
  ]
}
```

### 权限（字符串常量）

| 权限 | 允许 |
|---|---|
| （无需声明） | 列出设备、读设备属性 `prop`、注册界面贡献、通知、对话框、插件自己的存储/设置、插件目录内只读文件、插件数据目录读写、定时器、日志 |
| `device.shell` | `dev:shell` 任意 shell 命令 |
| `device.files` | `dev:push` / `dev:pull`、`dev:list` `mkdir` `rename` `remove`（只限共享存储） |
| `device.install` | `dev:install` / `dev:uninstall` |
| `device.apps` | `dev:apps` `hasApp` `launch` `stop` `clear` `foreground`、`app.foreground` 事件 |
| `device.input` | `dev:tap` `swipe` `longpress` `touch` `scroll` `send` `key` `text` `cmd`、`db.broadcast`、`dev:getClipboard` / `setClipboard` |
| `device.observe` | `device.action` / `device.input` 事件（含用户输入的文字与剪贴板内容） |
| `device.screen` | `dev:screenshot`、`dev:ui` `find` `waitFor`、`db.overlay` |
| `device.forward` | `dev:forward` / `dev:reverse` / `dev:unforward` |
| `device.logs` | `dev:logcat`、`db.logcat.watch` |
| `device.perf` | `dev:perf` |
| `device.connect` | `db.adb.connect` `pair` `disconnect` `discover`、`dev:tcpip` |
| `fs.read` | 读本机任意路径 |
| `fs` | 读写本机任意路径（含 `fs.read`） |
| `exec` | `db.exec` 运行本机程序、`db.open` 打开文件/网址 |
| `net` | `db.http.request` / `db.http.download` |
| `clipboard` | `db.clipboard.get/set` |
| `media` | `db.media.save` 存入图库、`db.media.list` / `remove` |
| `mcp` | `db.mcp.tool` 注册 AI 工具 |

`hasApp` 例外：`match.apps` 过滤由主进程内部完成，不需要权限。

## 2. 主进程状态文件 `<data_dir>/plugins/.state.json`

```json
{ "<plugin id>": { "enabled": true, "granted": ["device.shell"], "settings": { "luaDir": "D:/x" } } }
```
- 新出现的插件默认 `enabled:false`。
- 启用 = 用户确认权限；`granted` = 确认时的 `permissions` 副本。若清单里出现 `granted` 中没有的权限，状态变为 `needs_grant`，不启动，直到用户再次确认。
- 插件数据目录：`<data_dir>/plugins/.data/<id>/`（自动创建）。临时目录：`<data_dir>/plugins/.tmp/<id>/`（截图等按插件隔离）与 `<data_dir>/plugins/.tmp/uploads/`（仅单文件只读，禁止列目录）。审计日志：`<data_dir>/plugins/.audit.log`。

## 3. 宿主进程（`src/plugin_host.rs`）

启动：`<exe> --plugin-host`。第一条 stdin 消息必为 `init`。宿主读到 stdin EOF 立刻退出。

### 3.1 消息格式（每行一个 JSON）

主 → 宿主：
```
{"t":"init","plugin":{"id","name","version","dir","dataDir","main"},"granted":[...],"settings":{...},"lang":"zh|en","node":"电脑名","adb":"adb路径"}
{"t":"req","id":<u64>,"m":"<请求类型>","p":{...}}    // 主进程发起，宿主必须用 res 回复
{"t":"ret","id":<u64>,"ok":<任意>}                    // 回复宿主的 call
{"t":"ret","id":<u64>,"err":"消息"}
```
宿主 → 主：
```
{"t":"ready"}                                        // main.lua 顶层执行完毕（无论成功与否）后发送一次
{"t":"call","id":<u64>,"m":"<方法>","p":{...}}       // 宿主发起，主进程必须用 ret 回复
{"t":"res","id":<u64>,"ok":<任意>}                    // 回复主进程的 req
{"t":"res","id":<u64>,"err":"消息"}
{"t":"log","level":"debug|info|warn|error","msg":"...","src":"main.lua:12"}   // src 可省略
```
`id` 两个方向各自独立编号。

### 3.2 主 → 宿主请求类型 `req.m`

| m | p | 宿主行为 / 返回 `ok` |
|---|---|---|
| `ping` | `{}` | `true` |
| `menu` | `{id, ctx}` | 调用该菜单项的处理函数 `fn(dev_or_nil, ctx)`，返回 `fn` 的返回值（或 null） |
| `status` | `{id}` | 调用状态栏项的 `onClick()` |
| `panel` | `{id, serial, event}` | 即时模式渲染（见 3.5），返回 `{"title":..., "tree":[...]}` |
| `metric` | `{id, serial}` | 调用指标函数 `fn(dev)`，返回数字或 null |
| `web` | `{name, args, serial}` | 调用 `db.web.handle(name, fn)` 注册的 `fn(dev_or_nil, args)`，返回值 |
| `export` | `{name, args, from}` | 调用 `db.export(name, fn)` 注册的 `fn(args, from)`（from 为调用方插件 id），返回值 |
| `mcp` | `{name, args, serial}` | 调用 `db.mcp.tool` 注册的 `fn(dev, args)`；返回字符串或表（主进程转成文本） |
| `event` | `{name, args:[...]}` | 依次调用 `db.on(name, fn)` 的所有监听者（各自独立协程，不等待完成），立即返回 `true`。`args` 中的设备以 `{"serial":..}` 形式给出，宿主转成 dev 对象 |
| `settings` | `{values}` | 更新 `db.settings`，然后触发 `settings.changed` 事件，返回 `true` |
| `logline` | `{sub, line}` | 调用 `db.logcat.watch` 的回调 `fn(line)`；line = `{time, pid, tid, level, tag, msg, raw}` |

处理函数抛错 → 回复 `err`（带 traceback 第一行），同时 `log` error（完整 traceback）。

### 3.3 宿主 → 主调用方法 `call.m`

所有方法返回值放在 `ret.ok`，失败用 `ret.err`。设备参数统一 `serial`。

界面贡献：
- `register` `{kind, def}`；`unregister` `{kind, id}`。kind 与 def：
  - `menu`: `{id, target, path, icon?, filter?, pin?, on?}`（`pin`：toolbar 项直接显示为投屏卡片工具列的图标按钮；`on`：按钮开启状态的设备 serial 列表）；target ∈ `device | app | file | tools | toolbar | media | logcat`；path 用 `/` 分级，如 `"MyGame/安装测试包"`；filter：`{hasApp?, models?, serials?, packages?, ext?, dir?}`（packages/ext/dir 由前端判断；hasApp/models/serials 由主进程按设备判断）
  - `panel`: `{id, title, icon?, dock, filter?, refresh?}`；dock ∈ `device | window | settings`；refresh 秒数（前端自动重绘间隔，可省略）
  - `webpanel`: `{id, title, icon?, entry, dock, filter?}`；entry 为插件目录下相对路径，如 `web/index.html`
  - `status`: `{id, text, kind?, tip?, clickable}`；kind ∈ `info | ok | warn | error`
  - `metric`: `{id, label, unit?, filter?}`
  - `loghl`: `{id, text, tag?, kind?, label?}`（不区分大小写的子串匹配）
  - `mcp`: `{name, description, params}`；params = JSON Schema `properties`，`required` 单独字段 `{..., "required":[..]}`
  - `event`: `{name}`（订阅事件，主进程据此决定是否轮询前台应用等）
  - `export`: `{name}`（提供给其他插件调用的函数）
- `badge` `{serial, text|null, kind?, tip?}`
- `notify` `{text, kind?}`
- `dialog` `{kind, title, message?, fields?, default?, accept?, client?}` → 用户答案：confirm→bool；prompt→string|null；form→object|null；file→本机路径|null（kind=file 时浏览器上传文件到这台电脑的临时目录）
- `uiopen` `{url, name?, title?, mode?, client?}`：在发起操作的界面打开网页（WS 事件 `open`）；只接受 http(s)。桌面版为应用内窗口（按 name 复用，关闭只隐藏），浏览器为同名标签页；`mode="browser"` 时桌面版交给系统浏览器。宿主像 dialog 一样自动附上 `client`
- `progress` `{id, title?, frac?, text?, done?}`（frac 0–1 或 null 表示不确定）
- `repaint` `{panel?}`
- `overlay` `{serial, shapes|null, size?:{w,h}}`；shape：`{type:"rect",x,y,w,h,color?,text?,fill?,dash?}` | `{type:"text",x,y,text,color?}` | `{type:"point",x,y,color?,text?}`。`size` 为形状坐标所在的空间（如设备分辨率），前端按画面实际尺寸缩放；不给时坐标为画面像素。rect 的 `fill` 为填充不透明度（0–1，默认 0.2，0 不填充），`dash` 为虚线描边
- `webemit` `{name, data}`

设备：
- `devices` `{}` → `[{serial, model, name, state}]`（已按 match 过滤，仅 state=device 之外的也列出）
- `prop` `{serial, name}` → string
- `shell` `{serial, cmd, timeout?}` → `{out, code}`（timeout 秒，默认 30，最大 600）
- `push` `{serial, local, remote}` / `pull` `{serial, remote, local}` → `true`
- `install` `{serial, path, replace?, grant?, downgrade?}` → `true`；`uninstall` `{serial, package}`
- `apps` `{serial, system?}` → `[{package, name, version}]`
- `hasApp` `{serial, package}` → bool
- `launch` `{serial, package, activity?}`；`stop` `{serial, package}`；`clear` `{serial, package}`
- `foreground` `{serial}` → package 字符串或 null
- `input` `{serial, kind:"tap"|"longpress"|"swipe"|"touch"|"scroll"|"key"|"back"|"text"|"power"|"cmd", x?,y?,x2?,y2?,w?,h?,duration?,action?,id?,p?,hs?,vs?,key?,code?,meta?,text?,on?,name?}`（目标设备已开启共享投屏时直通 scrcpy 控制通道并自动按会话尺寸换算 `(w,h)`；未开启投屏时退回 `adb shell input` 并按 `wm size` 换算 `(w,h)`；`touch` 未投屏时返回错误；`touch` move 帧单独限流每秒 240 帧，超出直接返回 `false`）
- `broadcast` `{serials:[...], ev:{...}}` → `{ok, failed}`（检查 `device.input` 权限与各设备的 `match` 范围，并发向多台本机/远程设备派发 `input`）
- `screenshot` `{serial, save?}` → 本机 PNG 路径（save=true 时同时存入图库）
- `forward` `{serial, local, remote, reverse?}` → 实际本机端口（local=0 自动分配）；`unforward` `{serial, local?, remote?, reverse?}`。插件停止时主进程自动移除它建立的所有转发
- `logcat` `{serial, lines?, level?, grep?, package?}` → string
- `logwatch` `{serial, level?, tag?, grep?, package?}` → sub id（数字）；`logunwatch` `{sub}`
- `perf` `{serial}` → perf 快照对象
- `ui` `{serial}` → `{width, height, nodes:[{text, desc, id, class, package, bounds:[x1,y1,x2,y2], x, y, clickable, scrollable, checked, selected, focused, enabled}]}`（设备像素；与 AI 工具共用设备锁）
- `files` `{serial, path?}` → `{path, parent, items:[{name, dir, size, mtime}], space}`；`mkdir` `{serial, path}` → 新路径；`rename` `{serial, path, name}` → 新路径；`rm` `{serial, path | paths}`。只允许共享存储（`/sdcard`）
- `clipboard` `{serial, text?}`：给了 text 时写入设备剪贴板，否则返回当前内容
- `cmd` `{serial, name}`；name ∈ `notif | settings | collapse | rotate`
- `tcpip` `{serial}` → 无线连接后的设备标识（只支持本机设备）
- `mirror` `{serial, client?}`：在界面里打开投屏窗口（WS 事件 `mirror`）；宿主像 dialog 一样自动附上 `client`
- `media.save` `{path, serial?}` → 图库文件名；`media.list` `{}` → `[{name, kind, device, size, time, path}]`；`media.remove` `{name}`；`media.item` `{name}` → 单个条目或 null（不需要权限，media 菜单用）

这台电脑：
- `adb.connect` `{addr}` → 设备标识；`adb.pair` `{addr, code, connect?}` → `{serial|null, host}`；`adb.disconnect` `{addr}`；`adb.discover` `{}` → `[{name, kind, addr}]`
- `mirrors` `{}` → `[{serial, viewers, controller}]`；`computers` `{}` → `[{id, name, addr}]`
- `plugins` `{}` → `[{id, name, version, state, exports}]`；`plugin.call` `{plugin, name, args}` → 目标插件 `export` 请求的返回值（目标未运行或未 export 该名字时报错，120 秒超时）

主进程对每个 call：检查权限（表 1），检查设备在插件 match 范围内，记录审计（会改变设备或运行程序的操作），执行。

### 3.4 宿主本地实现（不经过主进程）

`db.fs.*`、`db.exec`、`db.open`、`db.http.request`、`db.clipboard.*`、`db.store.*`、定时器、`require`、日志。权限检查在宿主内完成（init 里的 `granted`）。阻塞操作（exec、http、download、walk、tar、clipboard）放到后台线程执行，结果通过内部通道恢复协程。

### 3.5 调度模型（必须）

- 宿主单线程跑 Lua；主循环从一个 `mpsc` 通道取消息（stdin 读线程、后台任务线程都往里写），用 `recv_timeout` 处理定时器。
- 所有 Lua 代码（main.lua 顶层、每次 req、每个事件监听者、每个 `db.spawn`、每个定时器回调）都运行在**独立协程**里。
- API 调用在 Lua 层表现为同步函数，内部 `coroutine.yield(SCHED_TOKEN, op)` 交给调度器；调度器发出 `call` 或启动后台任务，收到结果后 `resume`。等待期间其他协程照常运行。
- 必须重写暴露给插件的 `coroutine.resume/wrap`：若内层协程 yield 了调度器请求（第一个返回值是 SCHED_TOKEN），把它继续向外 yield，拿到结果后再 resume 内层，使插件自己用协程时 API 仍可用。
- 面板渲染函数禁止 yield：渲染期间调用会 yield 的 API → 抛错 `"panel render must not call blocking APIs; use db.spawn"`。
- CPU 限制：每次 resume 连续执行超过 **3 秒**（用 `set_hook` 每 10000 条指令检查一次）→ 在该协程内抛错 `"script timeout"`。
- 内存限制：`lua.set_memory_limit(256 MB)`。
- 任何 Lua 错误都被捕获，写 `log` error（含 traceback），不让宿主退出。Rust 侧 panic 用 `catch_unwind` 包住每次 resume。

即时模式面板（`panel` 请求）：
1. 若 `event` 非空：执行一次渲染（“事件轮”），id 匹配 `event.id` 的控件返回事件值（button 返回 true；toggle/slider/input/select 返回 `event.value`），丢弃输出。
2. 再执行一次渲染（“绘制轮”，无事件），输出控件树返回。
- 控件自动 id：`<父容器id>/<控件类型>:<label 或序号>`；同名重复时追加 `#n`。用户可传 `{id="x"}` 覆盖。
- 控件树节点：`{"w":"<类型>","id":"..", ...属性, "children":[...]}`。类型与属性：
  - `label {text, kind?}`、`heading {text}`、`text {text}`（多行）、`code {text}`、`separator {}`、`space {}`
  - `button {label, kind?:"primary"|"danger", disabled?}`
  - `toggle {label, value}`、`slider {label, value, min, max, step}`、`input {label, value, placeholder?, multiline?, password?}`、`select {label, value, options:[{value,label}]}`
  - `progress {value, text?}`（value 0–1 或 null）
  - `table {columns:[..], rows:[[..]]}`
  - `chart {label, values:[number], unit?}`（折线）
  - `image {src}`（`data:` URI 或 http(s) URL）
  - `link {text, url}`
  - `row {children}`、`group {title, children, collapsed?}`

## 4. 主进程管理器（`src/plugins.rs`）

- `pub struct Plugins`，放在 `AppState.plugins`。`start` 之后 `tokio::spawn(plugins::run(state))`：扫描、启动已启用插件、每秒检查文件变化（插件目录下所有文件的 mtime/大小指纹，忽略 `.` 开头的目录），变化 → 重启该插件并广播 `toast`「已重新加载」。
- 进程管理：`tokio::process::Command`，`kill_on_drop(true)`，Windows 加 `CREATE_NO_WINDOW`。意外退出 → 状态 `crashed`，按 1s/2s/4s 重启；60 秒内崩溃 3 次 → 状态 `failed`，不再自动重启，广播 `toast` kind=error。每 3 秒 `ping`，15 秒没有任何输出（启动完成前为 60 秒）→ 杀掉重启。
- 状态：`stopped | starting | running | crashed | failed | needs_grant | disabled | invalid`（清单错误）。
- 限流：重型设备 I/O（`install`/`uninstall`/`push`/`pull`/`ui`/`screenshot`）每个插件同时最多 4 个，其余轻量设备操作最多 64 个；每台设备同时最多 4 个慢速插件操作（本机投屏直通 `input`/`cmd`/`setClipboard` 免占信号量）；每个插件每秒最多 500 次 call，超出则排队等待。
- 控制台：每个插件保留最近 1000 条 `{time, level, msg, src}`；新条目通过 ws 广播 `console`。
- 事件：维护设备列表（每 2 秒 `adb devices`），变化时向订阅了 `device.connected` / `device.disconnected` 的插件发 `event`。有插件订阅 `app.foreground` 时，每 2 秒查一次这些设备的前台应用，变化才发。有拥有 `device.observe` 权限的插件订阅 `device.action` 或 `device.input` 时，旁路捕获人工在投屏窗口（含本机 `Mirror::handle_text` 与远程 `peers::proxy_ws_with_ctl`）发送的控制帧，合成为语义动作 `device.action` 与 60Hz 节流控制流 `device.input`（插件注入的控制帧不经过此入口，杜绝回环）。DroidBridge 自身完成安装（文件管理器、拖放、插件 install）后发 `app.installed` `{dev, package}`（能拿到包名时）；应用管理或插件卸载后发 `app.uninstalled` `{dev, package}`；截图、录屏存入图库后发 `media.saved` `{item}`。插件启动完成（ready）后对每台在线设备补发一次 `device.connected`。
- MCP：`mcp.rs` 的工具列表追加本机所有运行中插件注册的工具，名字 `<pluginid>__<name>`（非法字符替换为 `_`，截断到 64）。调用时路由到对应插件 `mcp` 请求（120 秒超时）。远程电脑：`list_tools` 同时向各远程电脑 `GET /api/plugins/mcp` 合并（按名字去重）；调用远程设备时经 `/api/agent/exec` 转发，`agent::exec` 遇到含 `__` 的工具名交给 `plugins::mcp_call`。

### 4.1 HTTP 接口（全部经过现有 auth 中间件；带 `?peer=` 时转发给远程电脑）

| 方法 路径 | 请求 | 返回 |
|---|---|---|
| GET `/api/plugins` | | `{manage: bool, dir: "插件目录", safeMode: bool, list:[{id,name,version,description,author,dir,state,error?,enabled,permissions,granted,settings:[schema],values:{}}]}` |
| POST `/api/plugins/manage` | `{id?, action, ...}` action ∈ `enable`(同时授予当前 permissions) `disable` `reload` `settings`(values) `open_dir`(id 可省略，打开插件根目录或该插件目录) `create`(id,name：用模板新建并直接启用) `rescan` | `{ok:true}`。**仅本机请求可用**（与 auth 中“本机”判断一致：本机 IP 且非反向代理；或桌面窗口），否则 403 |
| GET `/api/plugins/contrib?serial=` | | `{menus:[{plugin,id,target,path,icon,filter}], panels:[{plugin,id,title,icon,dock,refresh,web?:"/plugin-web/<id>/<entry>"}], status:[{plugin,id,text,kind,tip,clickable}], metrics:[{plugin,id,label,unit}], loghl:[{plugin,id,text,tag,kind,label}], badges:{serial:[{plugin,text,kind,tip}]}, overlays:{serial:[{plugin,shapes,size}]}}`。给了 serial 时，菜单/面板/指标按 hasApp/models/serials 和插件 match 过滤 |
| POST `/api/plugins/invoke` | `{plugin, kind:"menu"\|"status", id, ctx:{serial?,package?,path?,isDir?,name?,line?}, client?}` | 处理函数返回值 `{ok:..}` 或 `{error}`（等待完成，最长 10 分钟） |
| POST `/api/plugins/panel` | `{plugin, panel, serial?, event?, client?}` | `{title, tree}`（5 秒超时） |
| GET `/api/plugins/metrics?serial=` | | `[{plugin,id,label,unit,value}]`（每个 3 秒超时，失败 value=null） |
| POST `/api/plugins/web` | `{plugin, name, args, serial?, client?}` | `{ok:..}` |
| POST `/api/plugins/dialog` | `{id, value}`（取消为 null） | `{ok:true}` |
| POST `/api/plugins/upload?name=` | body 为文件 | `{path}`（存到 `.tmp`，24 小时后清理） |
| GET `/api/plugins/console?id=` | | `[{time,level,msg,src}]` |
| GET `/api/plugins/mcp` | | `[{name, description, inputSchema}]` |
| WS `/ws/plugins?client=<cid>` | | 主 → 前端事件（见下） |
| GET `/plugin-web/sdk.js` | | 插件网页 SDK（`web/plugin-sdk.js`，编译进程序） |
| GET `/plugin-web/{id}/{*path}` | | 插件目录 `web/` 及 entry 所在目录下的静态文件；拒绝 `..`；`Content-Security-Policy: sandbox allow-scripts allow-forms` |
| GET `/plugin-web/@{peer}/{id}/{*path}` | | 转发到远程电脑的 `/plugin-web/{id}/{path}` |

`client` 用于把对话框发给发起操作的那个界面；宿主的 `dialog` call 若带 `client` 就只发给该客户端，否则广播给所有连接中的客户端，第一个回答生效并广播 `dialog.close`。调用链里的 `client` 由主进程记录（invoke/panel/web 请求传入的 client 会作为该次处理期间宿主 dialog 的默认 client：主进程在 req 的 p 里带上 `client`，宿主把它保存在当前协程上下文里，dialog 调用时自动附上）。

WS 事件（JSON 文本帧）：
```
{"type":"changed"}                                  // 贡献/插件列表变化，前端重新拉取 contrib 与列表
{"type":"toast","plugin","text","kind"}
{"type":"dialog","id","plugin","kind","title","message","fields","default","accept"}
{"type":"dialog.close","id"}
{"type":"progress","plugin","id","title","frac","text","done"}
{"type":"open","plugin","url","name","title","mode"}
{"type":"mirror","plugin","serial"}               // 打开该设备的投屏窗口
{"type":"repaint","plugin","panel"}
{"type":"overlay","plugin","serial","shapes","size"}
{"type":"console","plugin","entry":{time,level,msg,src}}
{"type":"webmsg","plugin","name","data"}
{"type":"state","plugin","state","error"}
```

## 5. 前端（`web/js/plugins.js`，`web/plugin-sdk.js`）

- 每个页面生成随机 `client` id；对本机和每个已添加的远程电脑各开一个 `/ws/plugins`（远程用 `?peer=`），断线自动重连。
- 设置里新增“插件”页：列表（状态、版本、权限）、启用（弹窗列出权限中文说明并确认）/停用/重新加载/打开目录/新建插件、设置表单（按 settings schema 生成）、控制台（实时日志，error 红色，可清空、可复制）。非本机（manage=false）时只读并提示“只能在这台电脑上管理插件”。远程电脑的插件在列表里按电脑分组。
- 菜单贡献：设备卡片右键菜单、应用管理右键菜单、文件浏览器右键菜单、相册右键菜单（media）、日志行右键菜单（logcat）、侧边栏“工具”菜单（有 tools 菜单时才出现按钮）、投屏工具栏“插件”按钮（toolbar 项）。path 多级 → 子菜单（不支持子菜单时用 “MyGame ▸ 安装测试包” 文本）。点击 → invoke，执行中 toast 显示进度，失败 toast error。
- 面板：dock=device 的面板作为投屏窗口侧边的页签（与日志、文件同级）；dock=window 从工具菜单打开为对话框；dock=settings 显示在插件设置页。根据 tree 绘制控件；交互 → 发送 event 并用返回的 tree 重绘；收到 repaint 或 refresh 到期时重绘。
- webpanel：sandbox iframe（`sandbox="allow-scripts allow-forms"`，不加 allow-same-origin），postMessage 桥接：只接受 `e.source === iframe.contentWindow` 的消息；iframe 只能调用自己插件的 `web` 处理函数；`webmsg` 转发给该插件的 iframe。iframe 加载后收到 `{t:"init", device:{serial}, lang}`。iframe 发来的 `{t:"open", url, name, title, mode}`（SDK 的 `droidbridge.open`）按 `uiopen` 的规则直接在本界面打开，name 加上插件 id 前缀，避免不同插件互相复用窗口；`{t:"notify", text, kind}`（`droidbridge.notify`）显示为 toast。
- 徽标显示在设备卡片；状态栏项显示在侧边栏底部；指标在性能浮层追加行（每 2 秒拉 metrics）；日志高亮在 logcat 行上加样式与标签；覆盖层在投屏画面上绘制（canvas 叠加层，pointer-events:none，坐标按设备像素缩放）。
- 对话框：confirm / prompt / form / file（选择文件后上传到 `/api/plugins/upload`，把返回 path 作为答案）；progress 显示为常驻 toast。
- 所有插件提供的文字都用 textContent 插入，绝不用 innerHTML。

## 6. Lua API（插件可见，全局 `db`，也可 `require("droidbridge")`）

```lua
db.plugin            -- {id, name, version, dir, dataDir}
db.lang, db.node     -- "zh"/"en"，电脑名
db.settings          -- 只读表，当前设置值
db.log(...), db.warn(...), db.error(...)   -- print 等同 db.log
db.spawn(fn, ...)    -- 新协程，立即返回
db.parallel(items, fn) -> results, errs   -- 为每项启动协程并发执行 fn(item, idx)，等待全部完成
db.broadcast(devs, ev) -> {ok, failed}    -- 并发广播输入事件到多台设备
db.wait(sec)         -- 等待（仅协程内）
db.every(sec, fn) -> timer   -- timer:cancel()
db.after(sec, fn) -> timer
db.on(event, fn)     -- device.connected(dev) device.disconnected(dev) app.foreground(dev, pkg) device.action(dev, act) device.input(dev, raw) app.installed(dev, pkg) app.uninstalled(dev, pkg) media.saved(item) settings.changed(values) plugin.unload()

db.devices() -> {dev...}
db.device(serial) -> dev|nil
dev.serial, dev.model, dev.name, dev.state, dev.computer, dev.remote
dev:prop(name) dev:shell(cmd, {timeout}) -> out, code
dev:push(localPath, remotePath) dev:pull(remotePath, localPath)
dev:install(apkPath, {replace=true, grant=false, downgrade=false}) dev:uninstall(pkg)
dev:apps({system=false}) dev:hasApp(pkg) dev:launch(pkg, activity) dev:stop(pkg) dev:clear(pkg) dev:foreground()
dev:tap(x, y, {w,h}) dev:swipe(x1, y1, x2, y2, ms, {w,h}) dev:longpress(x, y, ms=800, {w,h}) dev:touch(action, x, y, {id,w,h,p}) dev:scroll(x, y, hs, vs, {w,h}) dev:send(ev) dev:key(name) dev:text(s)
dev:getClipboard() -> string ; dev:setClipboard(text) ; dev:cmd("notif"|"settings"|"collapse"|"rotate")
dev:ui() -> {width, height, nodes} ; dev:find(q) -> node|nil ; dev:waitFor(q, timeout=10) -> node|nil
-- q：字符串（等于 text / desc / 资源 id，id 可写短名）、表（各字段相等）或函数 fn(node) -> bool
dev:list(path) dev:mkdir(path) dev:rename(path, name) dev:remove(pathOrList)   -- 设备共享存储
dev:tcpip() -> serial ; dev:mirror()
dev:screenshot({save=false}) -> localPath
dev:forward(localPort, "tcp:1234") -> localPort ; dev:reverse("tcp:1234", localPort) ; dev:unforward(localPort)
dev:logcat({lines, level, grep, package}) -> string
dev:perf() -> table

db.logcat.watch(dev, {level, tag, grep, package}, fn(line)) -> sub ; sub:stop()
db.logcat.highlight(text, {tag, kind, label})

db.menu(target, path, [opts], fn)   -- opts: {icon, filter, id, pin, on}；返回 item，item:remove()
db.panel(id, {title, icon, dock="device", filter, refresh}, fn(ui, dev))
db.webpanel(id, {title, icon, entry, dock, filter})
db.web.handle(name, fn(dev, args)) ; db.web.emit(name, data)
db.status(id, text, {kind, tip, onClick}) ; db.status(id, nil) 移除
db.badge(dev, text, {kind, tip}) ; db.badge(dev, nil)
db.notify(text, {kind})
db.overlay(dev, shapes, size) ; db.overlay(dev, nil)
db.repaint(panelId)
db.perf.metric(id, {label, unit, filter}, fn(dev) -> number)
db.mcp.tool(name, {description, params={x={type,description}}, required={..}}, fn(dev, args))

db.ui.confirm(title, message) -> bool
db.ui.prompt(title, message, default) -> string|nil
db.ui.form(title, fields) -> table|nil
db.ui.pickFile({accept=".apk"}) -> path|nil
db.ui.pickDir({title, default}) -> path|nil   -- dialog kind="dir"：本机桌面版弹系统文件夹对话框（IPC pickdir:），否则退回输入路径
db.ui.progress(title) -> p ; p:set(frac, text) ; p:close()
db.ui.openUrl(url, {name, title, mode = "window" | "browser"})

db.fs.read(path) db.fs.write(path, data) db.fs.append(path, data) db.fs.list(dir) db.fs.exists(path)
db.fs.stat(path) -> {size, mtime, dir} db.fs.mkdir(path) db.fs.remove(path) db.fs.copy(a, b) db.fs.join(...)
db.fs.walk(dir, {ext, hidden=false}) -> {{path, size, mtime}}   -- 递归，path 相对 dir（/ 分隔），默认跳过 . 开头的项；后台线程
db.fs.tar(dir, out, files?, {gzip=false}) -> {count, size}      -- ustar（长路径用 prefix，含目录项）；后台线程
db.fs.hash(dir, files) -> {[path]=hex}                           -- FNV-1a 64 内容哈希；后台线程
db.exec(program, args, {cwd, timeout=60, env, input}) -> {code, stdout, stderr}
db.proc.start(program, args, {cwd, env, onLine, onStderr, onExit}) -> p ; p:send(line) p:write(s) p:close() p:kill() p:alive()
-- 常驻子进程（exec 权限，宿主本地实现）：stdout/stderr 按行投递，每行一个新协程；最多 8 个；插件停止时先关 stdin 等 0.3 秒，再强杀
db.open(pathOrUrl)
db.http.request({url, method="GET", headers, body, timeout=30}) -> {status, headers, body}
db.http.download(url, path, {headers, timeout=1800}) -> {status, headers, size}   -- 流式写文件（先写 .part 再改名），非 2xx 不留文件
db.clipboard.get() db.clipboard.set(text)
db.store.get(key) db.store.set(key, value)   -- JSON 持久化到 dataDir/store.json
db.json.encode(v) db.json.decode(s)
db.media.save(path) ; db.media.list() ; db.media.remove(name)
db.adb.connect(addr) db.adb.pair(addr, code, connectAddr) db.adb.disconnect(addr) db.adb.discover()
db.mirrors() db.computers()
db.export(name, fn(args, from)) ; db.plugins.list() ; db.plugins.call(id, name, args)
```
相对路径：`db.fs` 中相对路径相对于 `dataDir`；`plugin://x` 表示插件目录内的文件（只读）。
菜单处理函数参数：device/toolbar → `fn(dev)`；app → `fn(dev, pkg)`；file → `fn(dev, path, isDir)`；tools → `fn()`；media → `fn(item)`；logcat → `fn(dev, line)`。

沙箱：只开放 `base`（去掉 `dofile` `loadfile`；`load` 仅允许文本块且环境默认为插件环境）、`string` `table` `math` `utf8` `coroutine`（重写过的）、`os.time` `os.clock` `os.date`。没有 `io` `debug` `package` `os.execute` 等。`require(name)` 只在插件目录内按 `name.lua`、`lua/name.lua` 查找（`.` 转 `/`），结果缓存。

## 7. 插件作用于“这台电脑能看到的所有设备”（含远程电脑上的设备）

**模型变更**：插件运行在用户正在操作的这台电脑（界面所连的节点 A）上，作用于 A 能看到的全部设备：A 自己的设备 + A 已添加的远程电脑上的设备。远程电脑上**不需要**安装插件，只需要是支持本节 `devop` 接口的版本。界面只显示 A 的插件（不再显示/合并远程电脑自己的插件）。

### 7.1 设备标识
- 本机设备：`serial` 不变。
- 远程设备：`<peerId>/<serial>`（与 MCP 的设备 id 相同，peerId 来自 `st.peers`）。含 `/` 即为远程设备。
- 插件看到的 dev 对象：`serial`（上面的标识）、`model`、`name`、`state`、`computer`（所在电脑名，本机为本机名）、`remote`（bool）。
- `match` 规则用原始 serial（不含 peerId）和 model 匹配。

### 7.2 远程执行接口（在设备所在的电脑 B 上）
`POST /api/plugins/devop?op=<op>&serial=<原始serial>[&<其他参数>]`，经现有 auth 中间件（A 用它保存的令牌访问 B，与其他转发一致）。参数用 JSON 放在 header `X-Devop: <url 编码的 JSON>`，请求体为可选的二进制文件内容。B 不做插件权限检查（权限在 A 检查），但记录审计（来源 = 请求方）。返回 JSON `{ok: ...}` 或 `{error}`；`pull`、`screenshot` 返回二进制（`application/octet-stream`）。

| op | 参数 | 说明 |
|---|---|---|
| prop shell uninstall apps hasApp launch stop clear foreground input logcat perf ui files mkdir rename rm clipboard cmd | 同 SPEC 3.3 | 直接在 B 上执行 |
| push | `remote`；body=文件内容 | B 存到临时目录后 `adb push`，完成后删除 |
| install | `replace grant downgrade`；body=APK | 同上；返回 `{ok:true, package?}`（能拿到新包名时，A 据此发 `app.installed`） |
| pull | `remote` | 返回文件字节，A 写到插件给的本机路径 |
| screenshot | | 返回 PNG 字节，A 存到自己的 `.tmp`（save=true 时存入 A 的图库） |

不支持远程的：`forward`/`unforward`（端口只能转发到设备所在电脑）→ A 直接返回错误“远程电脑上的设备暂不支持端口转发”；`tcpip`（连接建在 A 的 adb 上）同样只支持本机设备。
`logwatch` 远程：A 作为 WebSocket 客户端连接 B 的 `/ws/logcat?serial=<原始serial>`（复用 peers 里连接远程 WS 的代码与令牌），按原有过滤规则转发给插件。

### 7.3 主进程（A）
- `devices`：本机设备 + 每个远程电脑的 `/api/devices`（缓存 3 秒，单台超时 3 秒，失败的电脑跳过）。
- 设备事件 `device.connected/disconnected`、`app.foreground` 覆盖远程设备（轮询同上）。
- 所有设备 call：先查权限与 match，再按标识路由：本机 → 原实现；远程 → `devop`。本机与远程共用同一份“执行设备操作”的函数（B 上的 devop 也调用它），不要复制两份逻辑。
- `contrib?serial=`、`invoke`、`panel`、`metrics`、`overlay`、`badge` 的 serial 一律用 7.1 的标识。
- MCP：插件工具在 A 上执行，`device` 参数用 7.1 的标识（list_devices 已是这个格式）；不再把插件工具调用转发给远程电脑。

### 7.4 前端
- 只连本机的 `/ws/plugins`，只读本机的 `/api/plugins*`。
- 远程电脑上的设备：所有插件请求（contrib/invoke/panel/metrics）都发给本机（不带 `?peer=`），serial 用 `<hostId>/<serial>`；覆盖层、徽标、面板、日志高亮按这个标识匹配设备。
- 插件设置页只列本机插件（去掉按电脑分组）。
