---DroidBridge 插件 API 类型声明（lua-language-server）
---在插件的 .luarc.json 里把本文件所在目录加进 workspace.library 即可获得补全：
---  { "runtime.version": "Lua 5.4", "workspace.library": ["D:/DroidBridge/docs/plugins"], "diagnostics.globals": ["db"] }
---@meta

---@class DbDevice 一台（已按 match 过滤的）设备
---@field serial string 设备标识（远程设备形如 `<peerId>/<serial>`）
---@field model string 型号
---@field name string 名称
---@field state string 状态，通常是 "device"
---@field computer string 所在电脑名称
---@field remote boolean 是否位于远程电脑
local DbDevice = {}

---@param name string 系统属性名，如 "ro.product.cpu.abi"
---@return string 属性值（不存在时为空字符串）
function DbDevice:prop(name) end

---@param cmd string shell 命令
---@param opts? { timeout?: number } 超时秒数，默认 30，最大 600
---@return string out 输出
---@return number code 退出码
function DbDevice:shell(cmd, opts) end

---@param localPath string 本机路径
---@param remotePath string 设备路径
function DbDevice:push(localPath, remotePath) end

---@param remotePath string 设备路径
---@param localPath string 本机路径
function DbDevice:pull(remotePath, localPath) end

---@param apkPath string 本机 APK 路径
---@param opts? { replace?: boolean, grant?: boolean, downgrade?: boolean }
function DbDevice:install(apkPath, opts) end

---@param pkg string 包名
function DbDevice:uninstall(pkg) end

---@param opts? { system?: boolean }
---@return { package: string, name: string, version: string }[]
function DbDevice:apps(opts) end

---@param pkg string
---@return boolean
function DbDevice:hasApp(pkg) end

---@param pkg string
---@param activity? string
function DbDevice:launch(pkg, activity) end

---@param pkg string
function DbDevice:stop(pkg) end

---@param pkg string
function DbDevice:clear(pkg) end

---@return string|nil 前台应用包名
function DbDevice:foreground() end

---@param x number
---@param y number
---@param opts? { w?: number, h?: number } 源坐标画布宽高（提供后自动按目标设备分辨率换算）
function DbDevice:tap(x, y, opts) end

---@param x1 number
---@param y1 number
---@param x2 number
---@param y2 number
---@param ms number 滑动时长（毫秒）
---@param opts? { w?: number, h?: number } 源坐标画布宽高
function DbDevice:swipe(x1, y1, x2, y2, ms, opts) end

---单步触控帧注入（已开投屏时直通 scrcpy 控制通道，毫秒级延迟）
---@overload fun(self: DbDevice, action: "down"|"up"|"move"|"cancel"|0|1|2|3, id: integer, x: number, y: number, w?: number, h?: number, p?: number): boolean
---@param action "down"|"up"|"move"|"cancel"|0|1|2|3
---@param x number
---@param y number
---@param opts? { id?: integer, w?: number, h?: number, p?: number }
function DbDevice:touch(action, x, y, opts) end

---滚轮滚动（device.input）
---@param x number
---@param y number
---@param hs number 水平滚动 [-16, 16]
---@param vs number 垂直滚动 [-16, 16]
---@param opts? { w?: number, h?: number }
function DbDevice:scroll(x, y, hs, vs, opts) end

---转发一条 `device.action` 或 `device.input` 事件表到该设备（device.input）
---@param ev table
---@return boolean
function DbDevice:send(ev) end

---@param name string "HOME"|"BACK"|"APP_SWITCH"|"POWER"|"VOLUME_UP"|"VOLUME_DOWN"|"ENTER"...
function DbDevice:key(name) end

---@param s string 要输入的文字（非 ASCII 会走剪贴板粘贴）
function DbDevice:text(s) end

---@param opts? { save?: boolean } save=true 同时存入相册
---@return string 本机 PNG 路径
function DbDevice:screenshot(opts) end

---@param localPort number 0 表示自动分配
---@param remote string 例如 "tcp:1234"
---@return number 实际本机端口
function DbDevice:forward(localPort, remote) end

---@param remote string
---@param localPort number
function DbDevice:reverse(remote, localPort) end

---@param localPort number
function DbDevice:unforward(localPort) end

---@param opts? { lines?: number, level?: string, grep?: string, package?: string }
---@return string 日志文本
function DbDevice:logcat(opts) end

---@return table 性能快照（fps / cpu / mem / temp / level …）
function DbDevice:perf() end

---长按（device.input）
---@param x number
---@param y number
---@param ms? number 按住时长（毫秒），默认 800
---@param opts? { w?: number, h?: number } 源坐标画布宽高
function DbDevice:longpress(x, y, ms, opts) end

---@return string 设备剪贴板内容（device.input）
function DbDevice:getClipboard() end

---写入设备剪贴板，不粘贴（device.input）
---@param text string
function DbDevice:setClipboard(text) end

---系统操作（device.input）
---@param name "notif"|"settings"|"collapse"|"rotate" 展开通知栏 / 展开快捷设置 / 收起 / 旋转屏幕
function DbDevice:cmd(name) end

---@class DbUiNode 界面元素（坐标为设备像素）
---@field text string
---@field desc string content-desc
---@field id string 资源 id，如 "com.x:id/login"
---@field class string
---@field package string
---@field bounds integer[] {x1, y1, x2, y2}
---@field x integer 中心
---@field y integer 中心
---@field clickable boolean
---@field scrollable boolean
---@field checked boolean
---@field selected boolean
---@field focused boolean
---@field enabled boolean

---读取界面结构（device.screen，uiautomator）
---@return { width: integer, height: integer, nodes: DbUiNode[] }
function DbDevice:ui() end

---找第一个匹配的界面元素
---@param q string|table|fun(n: DbUiNode): boolean 字符串：等于 text / desc / 资源 id（可写短名）；表：各字段相等；函数：返回 true 的节点
---@return DbUiNode|nil
function DbDevice:find(q) end

---等到界面上出现匹配的元素
---@param q string|table|fun(n: DbUiNode): boolean
---@param timeout? number 秒，默认 10
---@return DbUiNode|nil 超时返回 nil
function DbDevice:waitFor(q, timeout) end

---列出设备共享存储里的文件夹（device.files）
---@param path? string 默认 /sdcard
---@return { path: string, parent: string|nil, items: { name: string, dir: boolean, size: integer, mtime: integer }[], space: { total: integer, free: integer }|nil }
function DbDevice:list(path) end

---@param path string 要创建的文件夹（共享存储内）
---@return string 新路径
function DbDevice:mkdir(path) end

---@param path string
---@param name string 新名字（不含 /）
---@return string 新路径
function DbDevice:rename(path, name) end

---@param paths string|string[]
function DbDevice:remove(paths) end

---USB 设备切换为无线（device.connect，只支持本机设备）
---@return string 新的设备标识（ip:5555）
function DbDevice:tcpip() end

---在发起操作的界面里打开投屏窗口
function DbDevice:mirror() end

---@class DbTimer
local DbTimer = {}

function DbTimer:cancel() end

---@class DbLogSub
local DbLogSub = {}

function DbLogSub:stop() end

---@class DbLogLine
---@field time string
---@field pid number
---@field tid number
---@field level string
---@field tag string
---@field msg string
---@field raw string

---@class DbProgress
local DbProgress = {}

---@param frac number|nil 0–1；nil 表示不确定进度
---@param text? string
function DbProgress:set(frac, text) end

function DbProgress:close() end

---@class DbMenuItem
local DbMenuItem = {}

function DbMenuItem:remove() end

--- 面板控件构建器（每次渲染传入）。控件返回“本帧的事件值”。
---@class DbUi
local DbUi = {}

---@param text string
function DbUi.heading(text) end

---@param text string
---@param opts? { kind?: string, id?: string } kind: "info"|"ok"|"warn"|"error"
function DbUi.label(text, opts) end

---@param text string
function DbUi.text(text) end

---@param text string
function DbUi.code(text) end

function DbUi.separator() end

function DbUi.space() end

---@param label string
---@param opts? { id?: string, kind?: string, disabled?: boolean } kind: "primary"|"danger"
---@return boolean 本帧被点击时为 true
function DbUi.button(label, opts) end

---@param label string
---@param value boolean
---@param opts? { id?: string }
---@return boolean 新值
function DbUi.toggle(label, value, opts) end

---@param label string
---@param value number
---@param min number
---@param max number
---@param step number
---@param opts? { id?: string }
---@return number 新值
function DbUi.slider(label, value, min, max, step, opts) end

---@param label string
---@param value string
---@param opts? { id?: string, multiline?: boolean, placeholder?: string, password?: boolean }
---@return string 新值
function DbUi.input(label, value, opts) end

---@param label string
---@param value string
---@param options (string|{ value: string, label: string })[]
---@param opts? { id?: string }
---@return string 新值
function DbUi.select(label, value, options, opts) end

---@param value number|nil 0–1，nil 表示不确定进度
---@param text? string
function DbUi.progress(value, text) end

---@param columns string[]
---@param rows table[][]
function DbUi.table(columns, rows) end

---@param label string
---@param values number[]
---@param unit? string
function DbUi.chart(label, values, unit) end

---@param src string data: URI 或 http(s) 地址
function DbUi.image(src) end

---@param text string
---@param url string
function DbUi.link(text, url) end

---@param fn fun() 在行内绘制子控件
function DbUi.row(fn) end

---@param title string
---@param fn fun() 在分组内绘制子控件
---@param opts? { collapsed?: boolean, id?: string }
function DbUi.group(title, fn, opts) end

---@class DbPerf
local DbPerf = {}

---注册性能浮层里的指标（每 2 秒取一次）
---@param id string
---@param opts { label: string, unit?: string, filter?: table }
---@param fn fun(dev: DbDevice|nil): number|nil
function DbPerf.metric(id, opts, fn) end

---@class DbMcp
local DbMcp = {}

---注册一个 AI（MCP）工具，名字会变成 <插件id>__<name>
---@param name string
---@param opts { description?: string, params?: table<string, table>, required?: string[] }
---@param fn fun(dev: DbDevice|nil, args: table): string|table
function DbMcp.tool(name, opts, fn) end

---@class DbLogcat
local DbLogcat = {}

---@param dev DbDevice
---@param opts { level?: string, tag?: string, grep?: string, package?: string }
---@param fn fun(line: DbLogLine)
---@return DbLogSub
function DbLogcat.watch(dev, opts, fn) end

---@param text string 不区分大小写的子串
---@param opts? { tag?: string, kind?: string, label?: string } kind: "info"|"ok"|"warn"|"error"
function DbLogcat.highlight(text, opts) end

---@class DbFs
local DbFs = {}

---@param path string
---@return string
function DbFs.read(path) end

---@param path string
---@param data string
function DbFs.write(path, data) end

---@param path string
---@param data string
function DbFs.append(path, data) end

---@param dir string
---@return { name: string, size: number, mtime: number, dir: boolean }[]
function DbFs.list(dir) end

---@param path string
---@return boolean
function DbFs.exists(path) end

---@param path string
---@return { size: number, mtime: number, dir: boolean }
function DbFs.stat(path) end

---@param path string
function DbFs.mkdir(path) end

---@param path string
function DbFs.remove(path) end

---@param a string
---@param b string
function DbFs.copy(a, b) end

--- 递归列出目录下的文件（path 为相对路径，/ 分隔；mtime 为带小数的秒）
---@param dir string
---@param opts? { ext?: string[], hidden?: boolean } ext 只列这些扩展名；hidden 为假（默认）时跳过 . 开头的文件和目录
---@return { path: string, size: number, mtime: number }[]
function DbFs.walk(dir, opts) end

--- 把 dir 下的文件打成 tar（ustar，含上级目录项）
---@param dir string
---@param out string 输出文件
---@param files? string[] 相对 dir 的路径；省略时打包全部（跳过 . 开头的）
---@param opts? { gzip?: boolean } gzip 为真时输出 .tar.gz
---@return { count: number, size: number }
function DbFs.tar(dir, out, files, opts) end

--- 文件内容哈希（FNV-1a 64，16 位十六进制）；不存在的文件不出现在结果里
---@param dir string
---@param files string[] 相对 dir 的路径
---@return table<string, string>
function DbFs.hash(dir, files) end

---@param ... string
---@return string
function DbFs.join(...) end

---@class DbUiNs
local DbUiNs = {}

---@param title string
---@param message? string
---@return boolean
function DbUiNs.confirm(title, message) end

---@param title string
---@param message? string
---@param default? string
---@return string|nil 取消返回 nil
function DbUiNs.prompt(title, message, default) end

---@param title string
---@param fields { name: string, label?: string, type?: string, default?: any, options?: any[] }[] type: text|number|bool|select|password
---@return table|nil 取消返回 nil
function DbUiNs.form(title, fields) end

---@param opts? { accept?: string }
---@return string|nil 本机路径，取消返回 nil
function DbUiNs.pickFile(opts) end

--- 选择这台电脑上的文件夹：本机桌面版为系统对话框，其他界面为输入路径
---@param opts? { title?: string, default?: string }
---@return string|nil 取消返回 nil
function DbUiNs.pickDir(opts) end

---@param title string
---@return DbProgress
function DbUiNs.progress(title) end

--- 在发起操作的界面里打开网页：桌面版为应用内窗口（按 name 复用，关闭只隐藏），浏览器为同名标签页；只接受 http(s)
---@param url string
---@param opts {name: string|nil, title: string|nil, mode: "window"|"browser"|nil}|nil
---@return boolean
function DbUiNs.openUrl(url, opts) end

---@class DbWeb
local DbWeb = {}

---网页面板 droidbridge.call(name, args) 的处理函数
---@param name string
---@param fn fun(dev: DbDevice|nil, args: table): any
function DbWeb.handle(name, fn) end

---发消息给网页面板（droidbridge.on(name, fn)）
---@param name string
---@param data any
function DbWeb.emit(name, data) end

---@class DbStore
local DbStore = {}

---@param key string
---@return any
function DbStore.get(key) end

---@param key string
---@param value any
function DbStore.set(key, value) end

---@class DbHttp
local DbHttp = {}

---@param req { url: string, method?: string, headers?: table<string, string>, body?: string, timeout?: number }
---@return { status: number, headers: table<string, string>, body: string }
function DbHttp.request(req) end

--- GET 并流式写入文件（不限大小）；非 2xx 时不留文件。需要 net 权限
---@param url string
---@param path string 本机路径（相对 dataDir）
---@param opts? { headers?: table<string, string>, timeout?: number } timeout 默认 1800 秒
---@return { status: number, headers: table<string, string>, size: number }
function DbHttp.download(url, path, opts) end

---@class DbClipboard
local DbClipboard = {}

---@return string
function DbClipboard.get() end

---@param text string
function DbClipboard.set(text) end

---@class DbJson
local DbJson = {}

---@param v any
---@return string
function DbJson.encode(v) end

---@param s string
---@return any
function DbJson.decode(s) end

--- 全局 `db`：DroidBridge 插件 API
---@class Db
---@field plugin { id: string, name: string, version: string, dir: string, dataDir: string }
---@field lang string "zh" 或 "en"
---@field node string 这台电脑的名字
---@field settings table<string, any> 只读：当前设置值
---@field logcat DbLogcat
---@field perf DbPerf
---@field mcp DbMcp
---@field fs DbFs
---@field ui DbUiNs
---@field web DbWeb
---@field store DbStore
---@field http DbHttp
---@field clipboard DbClipboard
---@field json DbJson
---@field devices fun(): DbDevice[]
---@field device fun(serial: string): DbDevice|nil
---@field log fun(...) 普通日志，等同于 print
---@field warn fun(...) 警告日志
---@field error fun(...) 错误日志
---@field spawn fun(fn: fun(...), ...): nil 新协程，立即返回
---@field parallel fun(items: any[], fn: fun(item: any, idx: integer): any): table, table|nil 并发在独立协程中执行每一项，等待全部完成后返回 results, errs
---@field broadcast fun(devs: (DbDevice|string)[], ev: table): { ok: integer, failed: table<string, string> } 并发向多台设备广播输入事件（device.input）
---@field wait fun(sec: number) 等待（只能在协程里）
---@field every fun(sec: number, fn: fun()): DbTimer
---@field after fun(sec: number, fn: fun()): DbTimer
---@field on fun(event: string, fn: fun(...)) 事件：device.connected / device.disconnected / app.foreground / device.action / device.input / app.installed / app.uninstalled / media.saved / settings.changed / plugin.unload
---@field menu fun(target: string, path: string, opts: table|nil, fn: fun(...)): DbMenuItem target: device | app | file | tools | toolbar | media | logcat；opts: {icon?, filter?, id?, pin?: boolean, on?: string[]}（pin 仅 toolbar：直接显示为投屏卡片按钮，on 为开启状态的设备 serial）
---@field panel fun(id: string, opts: { title: string, icon?: string, dock?: string, filter?: table, refresh?: number }, render: fun(ui: DbUi, dev: DbDevice|nil)): any
---@field webpanel fun(id: string, opts: { title: string, icon?: string, entry: string, dock?: string, filter?: table }): any
---@field status fun(id: string, text: string|nil, opts?: { kind?: string, tip?: string, onClick?: fun() }): any
---@field badge fun(dev: DbDevice, text: string|nil, opts?: { kind?: string, tip?: string }): any
---@field notify fun(text: string, opts?: { kind?: string }): any
---@field overlay fun(dev: DbDevice, shapes: table[]|nil, size: {w: number, h: number}|nil): any
---@field repaint fun(panelId?: string): any
---@field exec fun(program: string, args?: string[], opts?: { cwd?: string, timeout?: number, env?: table<string, string>, input?: string }): { code: number, stdout: string, stderr: string }
---@field open fun(pathOrUrl: string): any
---@field media { save: fun(path: string, serial?: string): string, list: fun(): DbMediaItem[], remove: fun(name: string): boolean } list / remove 需要 media 权限
---@field adb { connect: fun(addr: string): string, pair: fun(addr: string, code: string, connectAddr?: string): { serial: string|nil, host: string|nil }, disconnect: fun(addr: string): boolean, discover: fun(): { name: string, kind: string, addr: string }[] } 这台电脑的无线 adb（device.connect）
---@field mirrors fun(): { serial: string, viewers: string[], controller: string|nil }[] 正在投屏的设备
---@field computers fun(): { id: string, name: string, addr: string }[] 已添加的远程电脑
---@field export fun(name: string, fn: fun(args: any, from: string): any) 提供给其他插件调用的函数
---@field plugins { list: fun(): { id: string, name: string, version: string, state: string, exports: string[] }[], call: fun(id: string, name: string, args?: any): any } 调用其他插件 export 的函数
---@field shell fun(serial: string, cmd: string, opts?: { timeout?: number }): string, number
db = {}

---@class DbMediaItem 相册里的一个文件
---@field name string
---@field kind "shot"|"rec"
---@field device string
---@field size integer
---@field time integer
---@field path string 本机路径（读取内容需要 fs.read）

---@class DbProc
---@field id integer
local DbProc = {}
--- 写一行（自动加换行）
function DbProc:send(line) end
--- 原样写入 stdin
function DbProc:write(s) end
--- 关闭 stdin
function DbProc:close() end
--- 结束进程
function DbProc:kill() end
---@return boolean
function DbProc:alive() end

---@class DbProcNs
local DbProcNs = {}
--- 启动常驻子进程（exec 权限）。stdout/stderr 每一行在新协程里回调；最多 8 个；插件停止时自动结束
---@param program string
---@param args string[]|nil
---@param opts {cwd: string|nil, env: table<string,string>|nil, onLine: fun(line: string)|nil, onStderr: fun(line: string)|nil, onExit: fun(code: integer)|nil}|nil
---@return DbProc
function DbProcNs.start(program, args, opts) end
db.proc = DbProcNs
