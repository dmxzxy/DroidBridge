---DroidBridge 插件 API 类型声明（lua-language-server）
---在插件的 .luarc.json 里把本文件所在目录加进 workspace.library 即可获得补全：
---  { "runtime.version": "Lua 5.4", "workspace.library": ["D:/DroidBridge/docs/plugins"], "diagnostics.globals": ["db"] }
---@meta

---@class DbDevice 一台（已按 match 过滤的）设备
---@field serial string 序列号
---@field model string 型号
---@field name string 名称
---@field state string 状态，通常是 "device"
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
function DbDevice:tap(x, y) end

---@param x1 number
---@param y1 number
---@param x2 number
---@param y2 number
---@param ms number 滑动时长（毫秒）
function DbDevice:swipe(x1, y1, x2, y2, ms) end

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
---@param opts? { id?: string, multiline?: boolean, placeholder?: string }
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

---@param title string
---@return DbProgress
function DbUiNs.progress(title) end

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
---@field wait fun(sec: number) 等待（只能在协程里）
---@field every fun(sec: number, fn: fun()): DbTimer
---@field after fun(sec: number, fn: fun()): DbTimer
---@field on fun(event: string, fn: fun(...)) 事件：device.connected / device.disconnected / app.foreground / app.installed / settings.changed / plugin.unload
---@field menu fun(target: string, path: string, opts: table|nil, fn: fun(...)): DbMenuItem
---@field panel fun(id: string, opts: { title: string, icon?: string, dock?: string, filter?: table, refresh?: number }, render: fun(ui: DbUi, dev: DbDevice|nil)): any
---@field webpanel fun(id: string, opts: { title: string, icon?: string, entry: string, dock?: string, filter?: table }): any
---@field status fun(id: string, text: string|nil, opts?: { kind?: string, tip?: string, onClick?: fun() }): any
---@field badge fun(dev: DbDevice, text: string|nil, opts?: { kind?: string, tip?: string }): any
---@field notify fun(text: string, opts?: { kind?: string }): any
---@field overlay fun(dev: DbDevice, shapes: table[]|nil): any
---@field repaint fun(panelId?: string): any
---@field exec fun(program: string, args?: string[], opts?: { cwd?: string, timeout?: number, env?: table<string, string>, input?: string }): { code: number, stdout: string, stderr: string }
---@field open fun(pathOrUrl: string): any
---@field media { save: fun(path: string, serial?: string): string }
---@field shell fun(serial: string, cmd: string, opts?: { timeout?: number }): string, number
db = {}
