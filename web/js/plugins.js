// 插件前端：贡献（菜单 / 面板 / 状态栏 / 徽标 / 覆盖层 / 指标 / 日志高亮）、
// 插件管理界面、即时模式面板绘制、网页面板桥接、插件发起的对话框。
// 规格见 docs/plugins/SPEC.md 第 5 节与 7.4 节：插件只运行在界面所连的这台电脑上，
// 但作用于这台电脑能看到的所有设备；远程电脑上的设备用 <电脑id>/<serial> 标识。
// 经典脚本，按 index.html 中的顺序加载。
'use strict';

// ---------------- 常量与状态 ----------------
// 每个页面一个客户端 id：插件发起的对话框只发给发起操作的那个界面
const PLUGIN_CLIENT = 'c' + Math.random().toString(36).slice(2, 10) + Date.now().toString(36);
const PLUGIN_DOT = { info: '', ok: 'ok', warn: 'warn', error: 'bad', bad: 'bad' };
// 权限的中文说明（启用时逐个列出让用户确认）
const PLUGIN_PERMS = {
  'device.shell': '在设备上执行任意 shell 命令',
  'device.files': '管理设备共享存储里的文件，推送和下载文件',
  'device.install': '安装和卸载应用',
  'device.apps': '查看应用列表、启动/停止/清除应用、读取前台应用',
  'device.input': '点击、滑动、按键、输入文字，读写设备剪贴板',
  'device.observe': '看到你在投屏窗口里的操作，包括输入的文字和粘贴的内容',
  'device.screen': '截取屏幕、读取界面结构、在画面上绘制覆盖层',
  'device.forward': '建立端口转发',
  'device.logs': '读取设备日志',
  'device.perf': '读取设备性能数据',
  'device.connect': '连接、配对和断开无线设备',
  'fs.read': '读取这台电脑上的任意文件',
  'fs': '读写这台电脑上的任意文件',
  'exec': '运行这台电脑上的程序和打开文件/网址',
  'net': '访问网络',
  'clipboard': '读写这台电脑的剪贴板',
  'media': '把图片、录屏保存到相册',
  'mcp': '向 AI（MCP）注册工具',
  'web.embed': '在面板里嵌入插件声明的外部网站',
};

const pluginContribMaps = new Map();  // 设备标识 -> { at, data }；'' 表示整台电脑的贡献
const pluginFetches = new Map();      // 设备标识 -> Promise
const pluginConsoles = new Map();     // plugin -> [entry]
const pluginOpenDialogs = new Map();  // dialog id -> close()
const pluginProgress = new Map();     // 'plugin|id' -> toast
const pluginOverlayMaps = new Map();  // 设备标识 -> [{plugin, shapes}]
const pluginWebCtx = new Set();       // 托管中的网页面板 ctx（按 contentWindow 比较）
const pluginViews = new Set();        // 打开着的面板视图
const pluginMetricsMap = new Map();   // 设备标识 -> [{plugin,id,label,unit,value}]
let pluginList = null;                // 本机 /api/plugins 返回
let pluginListLoading = null;
let pluginManager = null;             // 打开着的插件管理对话框
let pluginStarted = false;
let pluginWs = null;                  // 本机 /ws/plugins 连接
let pluginWsTimer = null;             // 重连定时器
let pluginWsRetry = 0;

const pluginTxt = (v) => (v === null || v === undefined ? '' : String(v));
// 插件只运行在界面所连的这台电脑上，但作用于这台电脑能看到的所有设备（含远程电脑上的设备）
const pluginHosts = () => [...hosts.values()];
/** 插件接口里的设备标识：本机设备就是 serial，远程电脑上的设备是 <电脑id>/<serial> */
const pluginDevId = (hst, serial) => (!serial || !hst || hst.id === 'local' ? pluginTxt(serial) : hst.id + '/' + serial);
/** 反过来：从插件设备标识得到所在电脑的 id 与原始 serial */
function pluginDevParse(devId) {
  const s = pluginTxt(devId);
  const i = s.indexOf('/');
  return i < 0 ? { hostId: 'local', serial: s } : { hostId: s.slice(0, i), serial: s.slice(i + 1) };
}
/** 所有电脑上在线的设备 */
function pluginOnlineDevs() {
  const out = [];
  for (const hst of pluginHosts()) for (const d of (hst.devices || [])) if (d.state === 'device') out.push({ hst, dev: d });
  return out;
}
/** 插件设备标识对应的投屏窗口 */
function pluginViewersOf(devId) {
  const { hostId, serial } = pluginDevParse(devId);
  return [...viewers.values()].filter((v) => v.host.id === hostId && v.dev.serial === serial);
}
const pluginDot = (kind) => PLUGIN_DOT[kind] === undefined ? '' : PLUGIN_DOT[kind];
const pluginGlob = (pat, s) => {
  const re = '^' + String(pat).replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*').replace(/\?/g, '.') + '$';
  try { return new RegExp(re, 'i').test(String(s || '')); } catch { return false; }
};
/** 图标只允许用程序里已有的名字 */
const pluginIcon = (name, fallback = 'puzzle') => (name && Object.prototype.hasOwnProperty.call(P, name) ? name : fallback);

// ---------------- 连接 ----------------
function pluginsStart() {
  if (pluginStarted) return;
  pluginStarted = true;
  pluginTick();
  setInterval(pluginTick, 3000);
}

function pluginTick() {
  pluginConnect();
  // 拉取贡献；每次最多几个请求，避免设备多时刷屏
  let budget = 5;
  if (pluginStale('')) { budget--; pluginContrib('').catch(() => {}); }
  for (const { hst, dev } of pluginOnlineDevs()) {
    if (budget <= 0) break;
    const devId = pluginDevId(hst, dev.serial);
    if (pluginStale(devId)) { budget--; pluginContrib(devId).catch(() => {}); }
  }
}

function pluginStale(devId) {
  const c = pluginContribMaps.get(devId || '');
  return !c || Date.now() - c.at > 20000;
}

function pluginConnect() {
  if (pluginWs && (pluginWs.readyState === 0 || pluginWs.readyState === 1)) return;
  const p = new URLSearchParams({ client: PLUGIN_CLIENT });
  const tk = pref.get('token', ''); if (tk) p.set('token', tk);
  const url = (location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/ws/plugins?' + p.toString();
  let ws;
  try { ws = new WebSocket(url); } catch { return; }
  pluginWs = ws;
  ws.onopen = () => {
    pluginWsRetry = 0;
    pluginInvalidate();
    setTimeout(() => { if (pluginStarted) pluginContrib('').catch(() => {}); }, 200);
  };
  ws.onmessage = (e) => {
    let m; try { m = JSON.parse(e.data); } catch { return; }
    if (m && typeof m === 'object') pluginOnEvent(m);
  };
  ws.onclose = () => {
    if (pluginWs !== ws) return;
    pluginWs = null;
    if (!pluginStarted) return;
    // 断线自动重连（1s/2s/4s…最多 15s）
    clearTimeout(pluginWsTimer);
    pluginWsTimer = setTimeout(() => { pluginWsTimer = null; pluginConnect(); }, Math.min(1000 * 2 ** Math.min(pluginWsRetry++, 4), 15000));
  };
  ws.onerror = () => {};
}

// 贡献变化：缓存只标记过期（重新拉取前仍用旧数据，已打开的插件面板不会因一时拿不到数据被关掉）
let pluginContribGen = 0;
function pluginInvalidate() {
  for (const c of pluginContribMaps.values()) c.at = 0;
  pluginFetches.clear();
  pluginContribGen++;
  pluginList = null;
}

// ---------------- 贡献数据 ----------------
/** 只读缓存的贡献（渲染路径用，避免每次重画都发请求） */
function pluginContribCached(devId) {
  const c = pluginContribMaps.get(devId || '');
  return c ? c.data : null;
}

/** 同步取缓存的贡献；过期时后台刷新 */
function pluginContribFor(devId) {
  const key = devId || '';
  const c = pluginContribMaps.get(key);
  if (c) {
    if (Date.now() - c.at > 15000) pluginContrib(key).catch(() => {});
    return c.data || {};
  }
  pluginContrib(key).catch(() => {});
  return null;
}

function pluginContrib(devId, force) {
  const key = devId || '';
  const hit = pluginContribMaps.get(key);
  if (!force && hit && Date.now() - hit.at < 15000) return Promise.resolve(hit.data);
  if (pluginFetches.has(key)) return pluginFetches.get(key);
  // 一律问本机：设备用 <电脑id>/<serial> 标识，带 ?peer= 的转发已不再需要
  const url = '/api/plugins/contrib' + (key ? '?serial=' + encodeURIComponent(key) : '');
  const gen = pluginContribGen;
  const p = api(url).then((d) => {
    if (gen !== pluginContribGen) return pluginContrib(key);
    pluginFetches.delete(key);
    const data = d || {};
    pluginContribMaps.set(key, { at: Date.now(), data });
    if (!key) pluginApplyHostContrib(data);
    else pluginOnSerialContrib(key);
    return data;
  }, (e) => { if (gen === pluginContribGen) pluginFetches.delete(key); throw e; });
  pluginFetches.set(key, p);
  return p;
}

/** 电脑级贡献到达：覆盖层、投屏工具栏按钮、侧栏 */
function pluginApplyHostContrib(data) {
  const ov = data.overlays || {};
  for (const devId of Object.keys(ov)) pluginOverlayMaps.set(devId, ov[devId] || []);
  for (const v of viewers.values()) pluginViewerUpdate(v);
  pluginRefreshFoot();
}

/** 某台设备的贡献到达：更新它打开的投屏窗口（插件按钮、页签） */
function pluginOnSerialContrib(devId) {
  for (const v of pluginViewersOf(devId)) pluginViewerUpdate(v);
}

/** 只重画侧栏底部（状态栏项 / 工具入口）：不打断已打开的设备菜单 */
function pluginRefreshFoot() {
  if (openMenu) return;
  if ($('#sideFoot') && typeof renderSideFoot === 'function') renderSideFoot();
}

/** 贡献变化：整块重画侧栏（徽标、状态栏项都可能变了） */
function pluginSideChanged() {
  if (openMenu || document.querySelector('.ctx-on')) return;
  if (typeof renderSide === 'function') renderSide();
}

// ---------------- 菜单贡献 ----------------
function pluginFilterOk(f, ctx) {
  if (!f || typeof f !== 'object') return true;
  if (Array.isArray(f.packages) && f.packages.length) {
    if (!ctx.package || !f.packages.some((g) => pluginGlob(g, ctx.package))) return false;
  }
  if (Array.isArray(f.ext) && f.ext.length) {
    if (ctx.isDir) return false;
    const name = pluginTxt(ctx.path);
    const m = /\.([^./\\]+)$/.exec(name);
    const ext = m ? m[1].toLowerCase() : '';
    if (!f.ext.some((g) => pluginGlob(g, ext) || pluginGlob(g, '.' + ext))) return false;
  }
  if (f.dir !== undefined && f.dir !== null) {
    if (!!ctx.isDir !== !!f.dir) return false;
  }
  return true;
}

/** 某台设备的插件菜单项（原始贡献）；devId 为插件设备标识，'' 为电脑级 */
function pluginMenuEntries(devId, target, ctx = {}, allowFetch = true) {
  const d = (allowFetch ? pluginContribFor(devId) : pluginContribCached(devId)) || {};
  return (d.menus || []).filter((m) => m && m.target === target && pluginFilterOk(m.filter, ctx));
}

/** 把同一层的 path 组织成子菜单 */
function pluginMenuTree(entries, ctx = {}) {
  const root = { children: new Map() };
  for (const e of entries) {
    const parts = String(e.path || e.id || '')
      .split('/').map((s) => s.trim()).filter(Boolean);
    if (!parts.length) continue;
    let node = root;
    for (const p of parts) {
      if (!node.children.has(p)) node.children.set(p, { children: new Map() });
      node = node.children.get(p);
    }
    node.entry = e;
  }
  const build = (node) => {
    const out = [];
    for (const [name, ch] of node.children) {
      if (ch.children.size) {
        out.push({ icon: pluginIcon(ch.entry && ch.entry.icon, 'folder'), label: name, sub: build(ch) });
      } else if (ch.entry) {
        const e = ch.entry;
        out.push({
          icon: pluginIcon(e.icon),
          label: name,
          fn: () => pluginInvoke({ plugin: e.plugin, kind: 'menu', id: e.id, ctx }),
        });
      }
    }
    return out;
  };
  return build(root);
}


/** 接入点用：返回可以直接拼进 menu()/ctxMenu() 的菜单项 */
function pluginMenuItems(hst, target, ctx = {}) {
  if (!hst) return [];
  // 远程电脑上的设备也用本机插件的贡献：serial 换成插件设备标识
  const serial = pluginDevId(hst, ctx.serial);
  const c = serial ? { ...ctx, serial } : { ...ctx };
  const entries = pluginMenuEntries(serial, target, c);
  if (!entries.length) return [];
  return [{ icon: 'puzzle', label: t('插件'), sub: pluginMenuTree(entries, c) }];
}

/** 相册条目的插件菜单（图库在插件所在的这台电脑上） */
function pluginMediaItems(item) {
  const ctx = { name: item.name };
  const entries = pluginMenuEntries('', 'media', ctx, false);
  return entries.length ? [{ icon: 'puzzle', label: t('插件'), sub: pluginMenuTree(entries, ctx) }] : [];
}

/** 执行插件的菜单/状态项。不打扰：只在执行超过 0.5 秒时显示进度；
 *  成功时只显示插件返回的文字（插件自己 notify 的不重复提示），失败才提示错误。 */
function pluginInvoke({ plugin, kind, id, ctx }) {
  let tt = null;
  const timer = setTimeout(() => { tt = toast(t('正在执行插件操作…'), { sticky: true }); }, 500);
  const done = (text, kind2, ms) => {
    clearTimeout(timer);
    if (!text) { if (tt) tt.close(); return; }
    (tt || toast(text, { sticky: true })).set(text, kind2).close(ms);
  };
  return api('/api/plugins/invoke', {
    method: 'POST',
    body: JSON.stringify({ plugin, kind, id, ctx: ctx || {}, client: PLUGIN_CLIENT }),
  }).then((r) => {
    if (r && r.error) return done(pluginTxt(r.error), 'bad', 6000);
    const v = r && r.ok;
    done(typeof v === 'string' && v ? pluginTxt(v) : '', 'ok', 2500);
  }, (e) => done(e.message, 'bad', 6000));
}

// ---------------- 状态栏、工具入口、徽标 ----------------
/** 电脑级的状态项（所有插件） */
function pluginStatusItems() {
  const d = pluginContribCached('');
  return d ? (d.status || []) : [];
}

/** 电脑级的工具菜单项（顶栏拼图图标） */
function pluginToolsMenus() {
  return pluginMenuEntries('', 'tools', {}, false);
}

/** 以独立窗口打开的面板 */
function pluginWindowPanels() {
  const d = pluginContribCached('') || {};
  return (d.panels || []).filter((p) => p && p.dock === 'window');
}

/** 顶栏拼图图标里的菜单：插件的工具菜单 + 窗口面板 */
function pluginToolsItems() {
  const items = [];
  const entries = pluginToolsMenus();
  if (entries.length) items.push(...pluginMenuTree(entries, {}));
  const wins = pluginWindowPanels();
  if (wins.length) {
    if (items.length) items.push('-');
    for (const p of wins) items.push({ icon: pluginIcon(p.icon, 'monitor'), label: pluginTxt(p.title || p.id), fn: () => pluginOpenWindow(p, null) });
  }
  return items;
}

/** 由 devices.js 的 renderSideFoot 调用：更新底部状态栏和顶栏工具入口 */
function pluginRenderSideFoot() {
  pluginRenderStatusBar();
  pluginSyncToolsBtn();
}

const pluginHiddenStatus = () => new Set(JSON.parse(pref.get('plugHide', '[]') || '[]'));
function pluginSetStatusHidden(plugin, hide) {
  const s = pluginHiddenStatus();
  if (hide) s.add(plugin); else s.delete(plugin);
  pref.set('plugHide', JSON.stringify([...s]));
  pluginRenderStatusBar();
}

function pluginRenderStatusBar() {
  let bar = document.getElementById('statusbar');
  const all = pluginStatusItems();
  const hidden = pluginHiddenStatus();
  const rank = { error: 0, bad: 0, warn: 1 };
  const items = all.filter((s) => !hidden.has(s.plugin)).sort((x, y) => (rank[x.kind] ?? 2) - (rank[y.kind] ?? 2));
  const show = all.length > 0;
  document.body.classList.toggle('has-sb', show);
  if (!show) { if (bar) bar.remove(); return; }
  if (!bar) {
    bar = h('footer', { id: 'statusbar', class: 'statusbar', role: 'status' });
    // 竖向滚轮横向滚动
    bar.addEventListener('wheel', (e) => { if (bar.scrollWidth > bar.clientWidth && Math.abs(e.deltaY) > Math.abs(e.deltaX)) { bar.scrollLeft += e.deltaY; e.preventDefault(); } }, { passive: false });
    ctxMenu(bar, () => pluginStatusMenu());
    document.body.append(bar);
  }
  if (!pluginList && !pluginListLoading) { pluginListLoading = api('/api/plugins').then((i) => { pluginList = i; pluginRenderStatusBar(); }).catch(() => {}).finally(() => { pluginListLoading = null; }); }
  const nameOf = (id) => { const p = ((pluginList && pluginList.list) || []).find((x) => x.id === id); return p ? pluginTxt(p.name || p.id) : pluginTxt(id); };
  const left = bar.scrollLeft;
  bar.replaceChildren(...items.map((s) => h('button', {
    class: 'sb-it' + (s.kind ? ' k-' + s.kind : ''), type: 'button',
    title: [nameOf(s.plugin), pluginTxt(s.tip || '')].filter(Boolean).join(' · '),
    onclick: () => (s.clickable ? pluginInvoke({ plugin: s.plugin, kind: 'status', id: s.id, ctx: {} }) : pluginOpenSettings(s.plugin)),
  }, h('span', { class: 'dot ' + pluginDot(s.kind) }), h('span', { class: 't' }, pluginTxt(s.text)))));
  if (!items.length) bar.append(h('span', { class: 'sb-empty', title: t('右键选择要显示的状态') }, h('span', { class: 'si', html: svg('puzzle') })));
  bar.scrollLeft = left;
}

/** 右键状态栏：勾选显示哪些插件的状态 */
function pluginStatusMenu() {
  const hidden = pluginHiddenStatus();
  const byPlugin = new Map();
  for (const s of pluginStatusItems()) if (!byPlugin.has(s.plugin)) byPlugin.set(s.plugin, s);
  const nameOf = (id) => { const p = ((pluginList && pluginList.list) || []).find((x) => x.id === id); return p ? pluginTxt(p.name || p.id) : pluginTxt(id); };
  return [...byPlugin.keys()].map((id) => ({
    icon: hidden.has(id) ? 'blank' : 'check', label: nameOf(id),
    fn: () => pluginSetStatusHidden(id, !hidden.has(id)),
  }));
}

/** 打开设置 → 插件，直接进入某个插件的详情 */
let pluginPendingSelect = null;
function pluginOpenSettings(id) {
  if (pluginManager && pluginManager.select) {
    pluginManager.select(id);
    const tab = document.querySelector('dialog.settings[open] .set-tab[data-k=plugins]');
    if (tab) tab.click();
    return;
  }
  pluginPendingSelect = id;
  settingsDialog('plugins');
}

/** 工具菜单入口：标题栏里的拼图图标（桌面侧栏顶部 + 手机顶栏），有工具菜单时才出现 */
function pluginSyncToolsBtn() {
  const has = pluginToolsItems().length > 0;
  for (const anchor of [document.getElementById('gear'), document.getElementById('mGear'), document.getElementById('tbGear')]) {
    if (!anchor || !anchor.parentNode) continue;
    let btn = anchor.parentNode.querySelector(':scope > .plug-toolsbtn');
    if (!has) { if (btn) btn.remove(); continue; }
    if (!btn) {
      const b = ib('puzzle', t('插件工具'), () => { const items = pluginToolsItems(); if (items.length) menu(b, items); }, 'plug-toolsbtn');
      anchor.parentNode.insertBefore(b, anchor);
    }
  }
}

/** 由 devices.js 的设备行调用：返回徽标元素 */
function pluginBadges(hst, dev) {
  const devId = pluginDevId(hst, dev.serial);
  const d = pluginContribCached(devId);
  const list = d && d.badges ? d.badges[devId] : null;
  if (!list || !list.length) return [];
  return [h('span', { class: 'plug-badges' }, list.map((b) => h('span', {
    class: 'plug-badge' + (b.kind ? ' k-' + b.kind : ''), title: b.tip || '',
  }, pluginTxt(b.text))))];
}

// ---------------- 面板：即时模式 ----------------
function pluginPanelsFor(devId, dock) {
  const d = pluginContribFor(devId) || {};
  return (d.panels || []).filter((p) => p && (!dock || p.dock === dock));
}

/** 面板内容：按控件树绘制，交互后带 event 重新请求并重绘 */
class PluginPanelView {
  constructor(def, devId) {
    this.def = def; this.serial = devId || '';
    this.plugin = def.plugin; this.id = def.id;
    this.title = def.title || def.id;
    this.collapsed = new Map();
    this.busy = false; this.nextEv = undefined;
    this.liveEv = null; this.liveT = null; this.destroyed = false;
    this.body = h('div', { class: 'plug-body' });
    this.el = h('div', { class: 'plug-panel' }, this.body);
    this.timer = null;
    pluginViews.add(this);
    this.refresh();
    this.arm();
  }
  arm() {
    clearInterval(this.timer);
    const s = Number(this.def.refresh);
    if (isFinite(s) && s > 0) this.timer = setInterval(() => { if (!document.hidden) this.refresh(); }, Math.max(500, s * 1000));
  }
  destroy() {
    this.destroyed = true;
    clearInterval(this.timer); clearTimeout(this.liveT);
    pluginViews.delete(this);
    this.el.remove();
  }
  event(id, value, live) {
    if (!id) return;
    if (live) {
      this.liveEv = { id, value };
      if (!this.liveT) this.liveT = setTimeout(() => { const ev = this.liveEv; this.liveEv = null; this.liveT = null; this.refresh(ev); }, 120);
      return;
    }
    this.refresh({ id, value });
  }
  refresh(ev) {
    if (this.busy) { this.nextEv = ev; return; }
    this.busy = true;
    const body = {
      plugin: this.plugin, panel: this.id, serial: this.serial,
      event: ev || null, client: PLUGIN_CLIENT,
    };
    api('/api/plugins/panel', { method: 'POST', body: JSON.stringify(body) })
      .then((r) => {
        if (this.destroyed) return;
        if (r && r.error) { this.showError(r.error); return; }
        if (r && r.title) this.title = r.title;
        this.draw(r && r.tree ? r.tree : []);
      })
      .catch((e) => { if (!this.destroyed) this.showError(e.message); })
      .then(() => {
        this.busy = false;
        if (this.nextEv !== undefined) { const next = this.nextEv; this.nextEv = undefined; this.refresh(next); }
      });
  }
  showError(msg) {
    this.body.replaceChildren(h('div', { class: 'plug-err' }, pluginTxt(msg)));
  }
  draw(tree) {
    const box = this.body;
    const focus = pluginFocusInfo(box);
    const scroll = box.scrollTop;
    const nodes = (Array.isArray(tree) ? tree : []).map((n) => pluginNode(this, n)).filter(Boolean);
    box.replaceChildren(...nodes);
    box.scrollTop = scroll;
    pluginRestoreFocus(box, focus);
  }
}

function pluginFocusInfo(box) {
  const a = document.activeElement;
  if (!a || !box.contains(a)) return null;
  const pid = a.dataset ? a.dataset.pid : '';
  if (pid === undefined) return null;
  const info = { id: pid, start: null, end: null };
  try { if (typeof a.selectionStart === 'number') { info.start = a.selectionStart; info.end = a.selectionEnd; } } catch {}
  return info;
}

function pluginRestoreFocus(box, info) {
  if (!info) return;
  const el = [...box.querySelectorAll('[data-pid]')].find((x) => x.dataset.pid === info.id);
  if (!el || el === document.activeElement) return;
  try { el.focus({ preventScroll: true }); } catch {}
  if (info.start !== null && typeof el.setSelectionRange === 'function') {
    try { el.setSelectionRange(info.start, info.end); } catch {}
  }
}

function pluginNode(view, n) {
  if (!n || typeof n !== 'object') return null;
  const id = typeof n.id === 'string' ? n.id : '';
  const pid = id ? { 'data-pid': id } : {};
  switch (n.w) {
    case 'label':
      return h('div', { class: 'pl-label' + (n.kind ? ' k-' + pluginTxt(n.kind) : '') }, pluginTxt(n.text));
    case 'heading':
      return h('div', { class: 'pl-heading' }, pluginTxt(n.text));
    case 'text':
      return h('div', { class: 'pl-text' }, pluginTxt(n.text));
    case 'code':
      return h('pre', { class: 'pl-code' }, pluginTxt(n.text));
    case 'separator':
      return h('div', { class: 'pl-sep' });
    case 'space':
      return h('div', { class: 'pl-space' });
    case 'button':
      return h('button', {
        class: 'btn' + (n.kind === 'primary' ? ' primary' : n.kind === 'danger' ? ' danger' : ''), type: 'button', disabled: !!n.disabled, ...pid,
        onclick: () => view.event(id, true),
      }, pluginTxt(n.label));
    case 'toggle': {
      const box = h('input', { type: 'checkbox', ...pid });
      box.checked = !!n.value;
      box.onchange = () => view.event(id, box.checked);
      return h('label', { class: 'pl-toggle' }, box, h('span', {}, pluginTxt(n.label)));
    }
    case 'slider': {
      const min = Number.isFinite(Number(n.min)) ? Number(n.min) : 0;
      const max = Number.isFinite(Number(n.max)) ? Number(n.max) : 100;
      const step = Number.isFinite(Number(n.step)) && Number(n.step) > 0 ? Number(n.step) : 1;
      const val = h('span', { class: 'pl-val' }, pluginTxt(n.value));
      const rng = h('input', { type: 'range', min: String(min), max: String(max), step: String(step), ...pid });
      rng.value = String(Number(n.value) || 0);
      rng.oninput = () => { val.textContent = rng.value; view.event(id, Number(rng.value), true); };
      rng.onchange = () => view.event(id, Number(rng.value));
      return h('div', { class: 'pl-slider' }, h('div', { class: 'pl-slider-h' }, h('span', {}, pluginTxt(n.label)), val), rng);
    }
    case 'input': {
      const multi = !!n.multiline;
      const el = multi
        ? h('textarea', { class: 'input pl-input', rows: '3', ...pid })
        : h('input', { class: 'input pl-input', type: n.password ? 'password' : 'text', autocomplete: 'off', ...pid });
      el.value = pluginTxt(n.value);
      if (n.placeholder) el.placeholder = pluginTxt(n.placeholder);
      const send = () => view.event(id, el.value);
      el.onchange = send;
      el.onkeydown = (e) => { if (e.key === 'Enter' && !multi && !e.isComposing) { e.preventDefault(); send(); } };
      return h('div', { class: 'pl-field' }, n.label ? h('label', {}, pluginTxt(n.label)) : null, el);
    }
    case 'select': {
      const sel = h('select', { class: 'select pl-select', ...pid });
      const opts = Array.isArray(n.options) ? n.options : [];
      for (const o of opts) {
        const ov = o && typeof o === 'object' ? o.value : o;
        const ol = o && typeof o === 'object' ? (o.label === undefined ? o.value : o.label) : o;
        sel.append(h('option', { value: pluginTxt(ov) }, pluginTxt(ol)));
      }
      sel.value = pluginTxt(n.value);
      sel.onchange = () => view.event(id, sel.value);
      return h('div', { class: 'pl-field' }, n.label ? h('label', {}, pluginTxt(n.label)) : null, sel);
    }
    case 'progress': {
      const frac = n.value === null || n.value === undefined ? null : Math.max(0, Math.min(1, Number(n.value) || 0));
      return h('div', { class: 'pl-progress' + (frac === null ? ' indet' : '') },
        h('div', { class: 'pl-bar' }, h('i', { style: 'width:' + (frac === null ? 30 : frac * 100) + '%' })),
        n.text ? h('div', { class: 'pl-progress-t' }, pluginTxt(n.text)) : null);
    }
    case 'table': {
      const cols = Array.isArray(n.columns) ? n.columns : [];
      const rows = Array.isArray(n.rows) ? n.rows : [];
      const tbl = h('table', { class: 'pl-table' },
        cols.length ? h('thead', {}, h('tr', {}, cols.map((c) => h('th', {}, pluginTxt(c))))) : null,
        h('tbody', {}, rows.map((r) => h('tr', {}, (Array.isArray(r) ? r : [r]).map((c) => h('td', {}, pluginTxt(c)))))));
      return h('div', { class: 'pl-tablebox' }, tbl);
    }
    case 'chart':
      return pluginChart(n);
    case 'image': {
      const src = pluginTxt(n.src);
      if (!/^(data:image\/|https?:\/\/)/i.test(src)) return h('div', { class: 'pl-err' }, t('不支持的图片地址'));
      return h('img', { class: 'pl-img', src, alt: '', loading: 'lazy' });
    }
    case 'link': {
      const url = pluginTxt(n.url);
      if (!/^https?:\/\//i.test(url)) return h('div', { class: 'pl-text' }, pluginTxt(n.text || url));
      return h('a', { class: 'pl-link', href: url, target: '_blank', rel: 'noopener noreferrer' }, pluginTxt(n.text || url));
    }
    case 'row': {
      const kids = (Array.isArray(n.children) ? n.children : []).map((c) => pluginNode(view, c)).filter(Boolean);
      return h('div', { class: 'pl-row' }, kids);
    }
    case 'group': {
      const key = id || 'g:' + pluginTxt(n.title);
      const det = h('details', { class: 'pl-group' });
      // 折叠状态由前端记住（渲染函数每次都会重跑）
      det.open = !view.collapsed.get(key);
      det.addEventListener('toggle', () => view.collapsed.set(key, !det.open));
      det.append(h('summary', {}, pluginTxt(n.title)));
      const inner = h('div', { class: 'pl-group-b' });
      for (const c of (Array.isArray(n.children) ? n.children : [])) { const el = pluginNode(view, c); if (el) inner.append(el); }
      det.append(inner);
      if (n.collapsed === true) { det.open = false; view.collapsed.set(key, true); }
      return det;
    }
    default:
      return null;
  }
}

function pluginChart(n) {
  const NS = 'http://www.w3.org/2000/svg';
  const w = 220, ht = 46;
  const vals = (Array.isArray(n.values) ? n.values : []).map((v) => Number(v)).filter((v) => isFinite(v));
  const box = h('div', { class: 'pl-chartbox' });
  const head = h('div', { class: 'pl-chart-h' }, h('span', {}, pluginTxt(n.label)), h('b', {}, vals.length ? pluginTxt(vals[vals.length - 1]) + (n.unit ? ' ' + pluginTxt(n.unit) : '') : '–'));
  box.append(head);
  const el = document.createElementNS(NS, 'svg');
  el.setAttribute('class', 'pl-chart');
  el.setAttribute('viewBox', '0 0 ' + w + ' ' + ht);
  if (vals.length > 1) {
    const max = Math.max(...vals), min = Math.min(0, ...vals);
    const span = max - min || 1;
    const step = w / (vals.length - 1);
    const poly = document.createElementNS(NS, 'polyline');
    poly.setAttribute('points', vals.map((v, i) => (i * step).toFixed(1) + ',' + (ht - 2 - ((v - min) / span) * (ht - 4)).toFixed(1)).join(' '));
    poly.setAttribute('fill', 'none');
    poly.setAttribute('stroke', 'currentColor');
    poly.setAttribute('stroke-width', '1.5');
    poly.setAttribute('stroke-linejoin', 'round');
    el.append(poly);
  }
  box.append(el);
  return box;
}

/** 面板工具栏（标题 + 重新加载） */
function pluginPanelHeader(view, onClose) {
  return h('div', { class: 'plug-head' },
    h('span', { class: 'pl-title' }, view.title),
    ib('refresh', t('刷新'), () => view.refresh()),
    onClose ? ib('x', t('关闭'), onClose) : null);
}

function pluginOpenWindow(def, devId) {
  const view = pluginPanelView(def, devId);
  const dlg = dialog({
    title: pluginTxt(def.title || def.id), cls: 'plug-win',
    body: [view.el],
    actions: [h('button', { class: 'btn primary', type: 'button', onclick: () => dlg.close() }, t('关闭'))],
  });
  dlg.addEventListener('close', () => view.destroy());
}

function pluginPanelView(def, devId) {
  if (def && Array.isArray(def.embed)) return new PluginExtView(def, devId);
  if (def && def.web) return new PluginWebView(def, devId);
  return new PluginPanelView(def, devId);
}

// ---------------- 打开网页（db.ui.openUrl / 插件网页 SDK 的 droidbridge.open） ----------------
// 桌面版：应用内窗口，按 name 复用（关闭只是隐藏，再次打开立即出现）；浏览器：同名标签页
function pluginOpenUrl(plugin, url, opts) {
  url = String(url || '');
  if (!/^https?:\/\//i.test(url)) return false;
  opts = opts || {};
  const name = String(opts.name || url).slice(0, 200);
  const key = String(plugin || '') + ':' + name;
  if (opts.mode !== 'browser' && window.ipc && window.__DB_CHROME) {
    try {
      window.ipc.postMessage('openwin:' + JSON.stringify({ url, name: key, title: String(opts.title || '') }));
      return true;
    } catch {}
  }
  // 浏览器里自己记住打开过的窗口引用：跨站点跳转后浏览器会清掉 window.name，按名字找不回来
  const prev = pluginOpenWins.get(key);
  if (prev && !prev.w.closed) {
    // 同一地址：只切到前面，不重新加载（页面和连接保留）
    if (prev.url !== url) { try { prev.w.location.href = url; prev.url = url; } catch {} }
    prev.w.focus();
    return true;
  }
  // 桌面版里 window.open 会交给系统浏览器
  const w = window.open(url, 'dbp_' + key.replace(/[^\w-]/g, '_'));
  if (w) pluginOpenWins.set(key, { w, url });
  else if (!window.__DB_CHROME) toast(t('浏览器拦截了弹出窗口'), { kind: 'warn' });
  return true;
}
const pluginOpenWins = new Map();

// ---------------- 网页面板（sandbox iframe + postMessage） ----------------
function pluginWebUrl(def) {
  const u = pluginTxt(def.web);
  // 只允许加载插件自己的页面（后端 /plugin-web 只提供插件目录内的文件）；插件网页一律由本机提供
  return u.indexOf('/plugin-web/') === 0 ? u : '';
}

class PluginWebView {
  constructor(def, devId) {
    this.def = def; this.serial = devId || '';
    this.plugin = def.plugin; this.id = def.id;
    this.title = def.title || def.id;
    this.el = h('div', { class: 'plug-panel plug-webpanel' });
    const src = pluginWebUrl(def);
    this.frame = h('iframe', { class: 'plug-frame', sandbox: 'allow-scripts allow-forms', src, title: this.title });
    this.ctx = { plugin: this.plugin, serial: this.serial, frame: this.frame, view: this };
    this.el.append(this.frame);
    this.frame.addEventListener('load', () => {
      try { this.frame.contentWindow.postMessage({ __droidbridge: 1, t: 'init', device: { serial: this.serial || '' }, lang: LANG }, '*'); } catch {}
    });
    pluginWebCtx.add(this.ctx);
    pluginViews.add(this);
  }
  refresh() { try { this.frame.contentWindow.location.reload(); } catch {} }
  destroy() {
    pluginWebCtx.delete(this.ctx);
    pluginViews.delete(this);
    this.el.remove();
  }
}

// ---------------- 嵌入的外部网页（plugin.json 的 embed 列出的网站） ----------------
// 地址由插件按设备给出（__url:<id>）；还没有地址时显示占位并每 3 秒再问一次。
// 和插件自己的网页不同：外部网站有自己的登录和存储，所以带 allow-same-origin（它的 origin 是外部网站，不是 DroidBridge）
function pluginOriginOf(url) {
  try { const u = new URL(url); return /^https?:$/.test(u.protocol) ? u.protocol + '//' + u.host : ''; } catch { return ''; }
}

// 桌面版：嵌入网站自己弹出的窗口（window.open）留在应用内打开，和面板共用登录状态
const pluginInAppOrigins = new Set();
function pluginInAppSync(origins) {
  let changed = false;
  for (const o of origins) if (!pluginInAppOrigins.has(o)) { pluginInAppOrigins.add(o); changed = true; }
  if (changed && window.ipc && window.__DB_CHROME) {
    try { window.ipc.postMessage('inapp:' + JSON.stringify([...pluginInAppOrigins])); } catch {}
  }
}

class PluginExtView {
  constructor(def, devId) {
    this.def = def; this.serial = devId || '';
    this.plugin = def.plugin; this.id = def.id;
    this.title = def.title || def.id;
    this.allowed = new Set((def.embed || []).map(pluginOriginOf).filter(Boolean));
    pluginInAppSync(this.allowed);
    this.wait = h('div', { class: 'plug-ext-wait', html: svg('globe') });
    this.el = h('div', { class: 'plug-panel plug-webpanel' }, this.wait);
    this.frame = null; this.url = ''; this.timer = 0; this.dead = false;
    pluginViews.add(this);
    this.resolve();
  }
  async resolve() {
    clearTimeout(this.timer);
    if (this.dead) return;
    let url = '';
    try {
      const r = await api('/api/plugins/web', {
        method: 'POST',
        body: JSON.stringify({ plugin: this.plugin, name: '__url:' + this.id, args: {}, serial: this.serial, client: PLUGIN_CLIENT }),
      });
      url = r && typeof r.ok === 'string' ? r.ok : '';
    } catch {}
    if (this.dead) return;
    const origin = pluginOriginOf(url);
    // https 页面里不能嵌 http（浏览器拦截混合内容）：交给“打开网页”
    const mixed = location.protocol === 'https:' && origin.startsWith('http:');
    if (!url || !this.allowed.has(origin) || mixed) {
      this.wait.title = mixed ? t('当前页面是 https，无法嵌入 http 网页') : url ? t('这个网址不在插件允许嵌入的网站里') : '';
      if (this.frame) { this.frame.remove(); this.frame = null; this.url = ''; this.wait.hidden = false; }
      this.timer = setTimeout(() => this.resolve(), 3000);
      return;
    }
    if (url === this.url && this.frame) return;
    this.url = url;
    if (!this.frame) {
      this.frame = h('iframe', {
        class: 'plug-frame', title: this.title, allow: 'clipboard-read; clipboard-write',
        sandbox: 'allow-scripts allow-forms allow-same-origin allow-popups allow-popups-to-escape-sandbox allow-modals allow-downloads',
      });
      this.el.append(this.frame);
    }
    this.frame.src = url;
    this.wait.hidden = true;
  }
  /** 刷新：重新向插件要地址（设备换了客户端时会换页面），同一地址则重新加载 */
  refresh() {
    if (this.frame) { const u = this.url; this.url = ''; this.frame.src = 'about:blank'; this.frame.src = u; this.url = u; }
    this.resolve();
  }
  destroy() {
    this.dead = true;
    clearTimeout(this.timer);
    pluginViews.delete(this);
    this.el.remove();
  }
}

function pluginWebPost(ctx, msg) {
  try { ctx.frame.contentWindow.postMessage(msg, '*'); } catch {}
}

/** 找到消息来源对应的网页面板（只接受自己托管的 iframe） */
function pluginWebCtxOf(source) {
  for (const ctx of pluginWebCtx) {
    try { if (ctx.frame.contentWindow === source) return ctx; } catch {}
  }
  return null;
}

// 嵌入的外部网页发来的消息：来源窗口和 origin 都对得上才转给插件（db.webpanel 的 onMessage）
addEventListener('message', (e) => {
  for (const view of pluginViews) {
    if (!(view instanceof PluginExtView) || !view.frame) continue;
    let mine = false;
    try { mine = view.frame.contentWindow === e.source; } catch {}
    if (!mine) continue;
    if (!view.url || e.origin !== pluginOriginOf(view.url) || !view.def.onMessage) return;
    let body;
    try { body = JSON.stringify(e.data); } catch { return; }
    if (!body || body.length > 65536) return;
    api('/api/plugins/web', {
      method: 'POST',
      body: JSON.stringify({ plugin: view.plugin, name: '__msg:' + view.id, args: JSON.parse(body), serial: view.serial, client: PLUGIN_CLIENT }),
    }).catch(() => {});
    return;
  }
});

addEventListener('message', (e) => {
  const ctx = pluginWebCtxOf(e.source);
  if (!ctx) return;
  const d = e.data;
  if (!d || typeof d !== 'object' || d.__droidbridge !== 1) return;
  if (d.t === 'open') { pluginOpenUrl(ctx.plugin, d.url, { name: d.name, title: d.title, mode: d.mode }); return; }
  if (d.t === 'notify') {
    const kind = d.kind === 'error' ? 'bad' : d.kind === 'warn' ? 'warn' : d.kind === 'ok' ? 'ok' : undefined;
    toast(pluginTxt(d.text), { kind, ms: 5000 });
    return;
  }
  if (d.t !== 'call') return;
  const rid = d.id;
  const reply = (ok, err) => pluginWebPost(ctx, { __droidbridge: 1, t: 'result', id: rid, ok, err });
  api('/api/plugins/web', {
    method: 'POST',
    body: JSON.stringify({ plugin: ctx.plugin, name: d.name, args: d.args, serial: ctx.serial, client: PLUGIN_CLIENT }),
  }).then((r) => {
    if (r && r.error) reply(null, pluginTxt(r.error));
    else reply(r ? r.ok : null);
  }, (err) => reply(null, err.message));
});

// ---------------- 插件管理（设置 → 插件 页签） ----------------
// 设计原则：先用形状和图标表达（状态圆点、开关、权限图标），文字放在悬停提示和详情里。

const PLUGIN_PERM_ICON = {
  'device.shell': 'terminal', 'device.files': 'folder', 'device.install': 'package', 'device.apps': 'grid',
  'device.input': 'target', 'device.observe': 'eye', 'device.screen': 'camera', 'device.forward': 'swap', 'device.logs': 'logs',
  'device.perf': 'activity', 'device.connect': 'wifi', 'fs.read': 'file', 'fs': 'drive', 'exec': 'play', 'net': 'globe', 'web.embed': 'monitor',
  'clipboard': 'paste', 'media': 'images', 'mcp': 'bot',
};

function pluginStateText(st) {
  return t({
    stopped: '已停止', starting: '正在启动', running: '运行中', crashed: '已崩溃',
    failed: '启动失败', needs_grant: '需要权限确认', disabled: '已停用', invalid: '清单有错误',
  }[st] || st || '未知');
}
const pluginStateDot = (st) => (st === 'running' ? 'ok' : st === 'starting' ? 'run' : ['failed', 'invalid', 'crashed'].includes(st) ? 'bad' : st === 'needs_grant' ? 'warn' : 'off');
const svgEl = (n) => h('span', { class: 'si', html: svg(n) });

/** 插件的图形标识：取名字首字，按 id 选一个色相 */
function pluginTile(p, size) {
  const name = pluginTxt(p.name || p.id) || '?';
  let hue = 0;
  for (const ch of pluginTxt(p.id)) hue = (hue * 31 + ch.charCodeAt(0)) % 360;
  return h('span', { class: 'plug-tile' + (size ? ' ' + size : ''), style: `--h:${hue}`, 'aria-hidden': 'true' }, [...name][0].toUpperCase());
}

/** 权限：图标一排，说明放在悬停提示里 */
function pluginPermIcons(perms, granted) {
  return h('div', { class: 'plug-picons' }, perms.map((k) => {
    const desc = t(PLUGIN_PERMS[k] || k);
    const miss = granted && !granted.includes(k);
    return h('span', { class: 'plug-pi' + (miss ? ' miss' : ''), title: desc + '\n' + k + (miss ? '\n' + t('尚未授权') : ''), html: svg(PLUGIN_PERM_ICON[k] || 'shield') });
  }));
}

function pluginSwitch(on, disabled, onchange) {
  const i = h('input', { type: 'checkbox', disabled, onchange: () => onchange(i.checked, i) });
  i.checked = !!on;
  return h('label', { class: 'switch', title: on ? t('停用') : t('启用'), onclick: (e) => e.stopPropagation() }, i, h('i'));
}

function pluginSettingsPaneClosed() { if (pluginManager) pluginManager.destroy(); }

/** 兼容旧入口：打开设置的“插件”页 */
function pluginsDialog() {
  if (pluginManager) { pluginManager.reload(); return; }
  settingsDialog('plugins');
}

function pluginSettingsPane() {
  if (pluginManager) pluginManager.destroy();
  const root = h('div', { class: 'plugpane' });
  let info = pluginList;   // 本机 /api/plugins 返回（插件只运行在界面所连的这台电脑上）
  let loadErr = '';
  let selected = pluginPendingSelect;    // 插件 id，null 为列表
  pluginPendingSelect = null;
  let showSources = false;
  let views = [];         // 详情里的设置面板（dock=settings）
  let con = null, conKey = '', conTools = null;
  const srcCtx = {
    reloadPlugins: () => reload(),
    render: () => render(),
    plugin: (pid) => ((info && info.list) || []).find((x) => x.id === pid),
    back: () => { showSources = false; render(); },
    get manage() { return !!(info && info.manage); },
  };

  async function reload() {
    try { info = await api('/api/plugins'); pluginList = info; loadErr = ''; }
    catch (e) { loadErr = e.message; }
    if (info && info.manage && (!pluginSrcView || pluginSrcStale())) {
      pluginSrcLoad(pluginSrcStale()).then((changed) => { if (changed && !selected) render(); });
    }
    // 详情页里正在输入设置时不重画，避免打断输入
    if (selected && root.contains(document.activeElement) && document.activeElement.matches('input, select')) return;
    render();
  }

  function render() {
    for (const v of views) v.destroy();
    views = []; con = null; conKey = ''; conTools = null;
    root.replaceChildren(selected ? detail() : showSources ? pluginSourcesPane(srcCtx) : list());
  }

  async function manage(payload) {
    try {
      await api('/api/plugins/manage', { method: 'POST', body: JSON.stringify(payload) });
      pluginInvalidate();
    } catch (e) { toast(e.message, { kind: 'bad', ms: 5000 }); }
    await reload();
  }

  function toggle(p) {
    return pluginSwitch(p.enabled, false, async (on, input) => {
      if (on && !(await pluginGrantDialog(p))) { input.checked = false; return; }
      manage({ id: p.id, action: on ? 'enable' : 'disable' });
    });
  }

  // ---------- 列表 ----------
  function list() {
    const canManage = !!(info && info.manage);
    const ups = canManage ? pluginSrcUpdateList() : [];
    const out = [h('div', { class: 'plugpane-bar' },
      h('span', { class: 'plugpane-n' }, countText()),
      ups.length > 1 ? ib('download', t('全部更新'), updateAll) : null,
      canManage ? ib('plus', t('新建插件'), newPluginDialog) : null,
      canManage ? ib('globe', t('插件源'), () => { showSources = true; render(); }) : null,
      canManage ? ib('folder', t('打开插件目录') + (info.dir ? '\n' + info.dir : ''), () => manage({ action: 'open_dir' })) : null,
      ib('refresh', t('重新扫描'), () => (canManage ? manage({ action: 'rescan' }) : reload())))];
    if (info && info.safeMode) out.push(h('div', { class: 'plug-note warn' }, svgEl('alert'), h('span', {}, t('安全模式：插件未加载'))));
    const items = [];
    if (loadErr) items.push(h('div', { class: 'plug-row muted', title: pluginTxt(loadErr) }, svgEl('wifiOff'), h('span', { class: 'plug-row-e' }, pluginTxt(loadErr))));
    const installed = new Set(((info && info.list) || []).map((p) => p.id));
    for (const p of ((info && info.list) || [])) items.push(row(p));
    if (canManage) for (const e of pluginSrcAvailable()) if (!installed.has(e.id)) items.push(availRow(e));
    if (!items.length) {
      out.push(h('div', { class: 'plug-empty' }, h('span', { class: 'plug-empty-i', html: svg('puzzle') }),
        h('div', {}, t('还没有插件')),
        canManage ? h('button', { class: 'btn sm', type: 'button', onclick: newPluginDialog }, t('新建插件')) : null));
    } else {
      out.push(h('div', { class: 'set-c' }, items));
    }
    if (info && !info.manage) out.push(h('div', { class: 'plug-ro' }, svgEl('lock'), h('span', {}, t('只能在这台电脑上管理插件'))));
    return h('div', { class: 'plugpane-list' }, out);
  }

  /** 插件源里还没安装的插件：点开看详情，右侧是安装按钮 */
  function availRow(e) {
    const tip = [pluginTxt(e.name || e.id) + (e.version ? '  ' + pluginSrcVer(e.version) : ''), pluginTxt(e.description || ''), e.id].filter(Boolean).join('\n');
    const open = () => { selected = e.id; render(); };
    return h('div', { class: 'plug-row static off', role: 'button', tabindex: '0', title: tip, onclick: open,
        onkeydown: (ev) => { if (ev.key === 'Enter' && ev.target === ev.currentTarget) open(); } },
      pluginTile(e),
      h('div', { class: 'plug-row-m' }, h('div', { class: 'plug-row-n' }, h('b', {}, pluginTxt(e.name || e.id)))),
      pluginSrcAction(srcCtx, e, true),
      h('span', { class: 'plug-row-c', html: svg('chevRight') }));
  }

  /** 依次更新所有有新版的插件；需要新权限的照常确认 */
  async function updateAll() {
    const ups = pluginSrcUpdateList();
    let n = 0;
    for (const e of ups) {
      pluginSrcBusy = e.id; render();
      try { await pluginSrcCall({ action: 'install', url: e.url, id: e.id }); n++; }
      catch (err) { toast(err.message, { kind: 'bad', ms: 6000 }); }
    }
    pluginSrcBusy = '';
    pluginInvalidate();
    await reload();
    for (const e of ups) {
      const ph = ((info && info.list) || []).find((x) => x.id === e.id);
      if (ph && ph.enabled && ph.state === 'needs_grant') await pluginGrant(ph);
    }
    if (n) toast(t('已更新 {0} 个插件', n), { kind: 'ok' });
  }

  function row(p) {
    const st = p.state || (p.enabled ? 'stopped' : 'disabled');
    const tip = [pluginTxt(p.name || p.id) + (p.version ? '  v' + pluginTxt(p.version) : ''), pluginStateText(st), pluginTxt(p.description || '')].filter(Boolean).join('\n');
    const err = st === 'needs_grant' ? '' : ['failed', 'invalid', 'crashed'].includes(st) ? pluginTxt(p.error || pluginStateText(st)) : '';
    // 已启用、升级后申请了新权限：行内直接给“授权”按钮
    const grantBtn = st === 'needs_grant' && p.enabled && info.manage
      ? h('button', { class: 'btn sm primary plug-grant-btn', type: 'button', title: t('需要权限确认'),
          onclick: (e) => { e.stopPropagation(); pluginGrant(p); } }, t('授权'))
      : null;
    const open = () => { selected = p.id; render(); };
    return h('div', { class: 'plug-row' + (p.enabled ? '' : ' off'), role: 'button', tabindex: '0', title: tip,
      onclick: open, onkeydown: (e) => { if (e.key === 'Enter' && e.target === e.currentTarget) open(); } },
      pluginTile(p),
      h('div', { class: 'plug-row-m' },
        h('div', { class: 'plug-row-n' }, h('span', { class: 'dot ' + pluginStateDot(st) }), h('b', {}, pluginTxt(p.name || p.id)),
          p.kind === 'builtin' ? h('span', { class: 'plug-lock', title: t('内置插件：可以停用，不能删除'), html: svg('lock') }) : null),
        err ? h('div', { class: 'plug-row-e' }, err) : null),
      grantBtn,
      info.manage ? pluginSrcAction(srcCtx, pluginSrcEntry(p.id), true) : null,
      info.manage ? toggle(p) : null,
      h('span', { class: 'plug-row-c', html: svg('chevRight') }));
  }

  function countText() {
    let n = 0, run = 0;
    for (const p of ((info && info.list) || [])) { n++; if (p.state === 'running') run++; }
    return n ? t('{0} 个 · {1} 个运行中', n, run) : '';
  }

  // ---------- 详情 ----------
  /** 还没安装的插件：标题、版本、来源、说明、权限，头部是安装按钮 */
  function srcDetail(e) {
    return h('div', { class: 'plugd' },
      h('div', { class: 'plugd-h' },
        ib('chevLeft', t('返回'), () => { selected = null; render(); }),
        pluginTile(e, 'lg'),
        h('div', { class: 'plugd-t' },
          h('div', { class: 'plugd-n' }, h('b', {}, pluginTxt(e.name || e.id)), e.version ? h('span', { class: 'ver' }, pluginSrcVer(e.version)) : null),
          h('div', { class: 'plugd-s' }, h('span', {}, t('来自 {0}', pluginSrcEntryName(e))))),
        pluginSrcAction(srcCtx, e, !!(info && info.manage))),
      e.description ? h('div', { class: 'plugd-d' }, pluginTxt(e.description)) : null,
      (e.permissions || []).length ? pluginPermIcons(e.permissions, null) : null);
  }

  function detail() {
    const id = selected;
    const p = info && (info.list || []).find((x) => x.id === id);
    const entry = pluginSrcEntry(id);
    // 本地插件、链接目录没有来源
    const e = entry && entry.status !== 'local' ? entry : null;
    if (!p) { if (!entry) { selected = null; return list(); } return srcDetail(entry); }
    const can = !!info.manage;
    const st = p.state || (p.enabled ? 'stopped' : 'disabled');
    const newer = !!(e && e.status === 'update');
    const ver = newer ? pluginSrcVer(e.installed || p.version) + ' → ' + pluginSrcVer(e.version) : p.version ? pluginTxt(p.version) : '';
    const nodes = [];
    nodes.push(h('div', { class: 'plugd-h' },
      ib('chevLeft', t('返回'), () => { selected = null; render(); }),
      pluginTile(p, 'lg'),
      h('div', { class: 'plugd-t' },
        h('div', { class: 'plugd-n' }, h('b', {}, pluginTxt(p.name || p.id)), ver ? h('span', { class: 'ver' }, ver) : null,
          e ? h('span', { class: 'ver' }, t('来自 {0}', pluginSrcEntryName(e))) : null),
        h('div', { class: 'plugd-s' }, h('span', { class: 'dot ' + pluginStateDot(st) }), h('span', {}, pluginStateText(st)))),
      can ? pluginSrcAction(srcCtx, e, true) : null,
      can ? ib('refresh', t('重新加载'), () => manage({ id: p.id, action: 'reload' })) : null,
      can ? ib('folder', t('打开目录') + (p.dir ? '\n' + p.dir : ''), () => manage({ id: p.id, action: 'open_dir' })) : null,
      can ? uninstallBtn(p) : null,
      can ? toggle(p) : null));
    if (p.description) nodes.push(h('div', { class: 'plugd-d' }, pluginTxt(p.description)));
    if (st === 'needs_grant' && p.enabled && can) {
      nodes.push(h('div', { class: 'plug-note warn' }, svgEl('shield'), h('span', { class: 'grow' }, t('需要权限确认')),
        h('button', { class: 'btn sm primary', type: 'button', onclick: () => pluginGrant(p) }, t('授权'))));
    }
    if (p.error) nodes.push(h('div', { class: 'plug-note bad' }, svgEl('alert'), h('span', {}, pluginTxt(p.error))));
    if (p.permissions && p.permissions.length) nodes.push(pluginPermIcons(p.permissions, p.enabled ? p.granted || [] : null));
    if (p.enabled) nodes.push(pluginEntryList(p));

    // 设置：和其他设置一样，改了立即保存
    const schema = Array.isArray(p.settings) ? p.settings : [];
    if (schema.length) {
      const values = { ...(p.values || {}) };
      let timer = 0;
      const save = () => {
        clearTimeout(timer);
        timer = setTimeout(() => api('/api/plugins/manage', { method: 'POST', body: JSON.stringify({ id: p.id, action: 'settings', values }) })
          .then(() => { p.values = { ...values }; }).catch((e) => toast(e.message, { kind: 'bad' })), 400);
      };
      nodes.push(h('div', { class: 'set-c' }, schema.map((f) => pluginSettingRow(f, values, can, save))));
    }

    // 插件自己放在设置页的面板（整台电脑级，不分设备）
    for (const def of pluginPanelsFor('', 'settings').filter((d) => d.plugin === p.id)) {
      const view = pluginPanelView(def, '');
      views.push(view);
      nodes.push(h('div', { class: 'plug-setpanel' }, view.el));
    }

    // 控制台：没有日志时只有一行标题
    const hasLog = (pluginConsoles.get(p.id) || []).length > 0;
    con = h('div', { class: 'plug-console', hidden: !hasLog });
    conKey = p.id;
    conTools = h('span', { class: 'plugd-con-t', hidden: !hasLog },
      ib('copy', t('复制日志'), () => copyText(pluginConsoleLines(p.id).join('\n'))),
      ib('eraser', t('清空'), () => { pluginConsoles.set(conKey, []); paint(); }));
    nodes.push(h('div', { class: 'plugd-con' },
      h('div', { class: 'plugd-con-h' }, h('span', { class: 'si', title: t('控制台'), html: svg('terminal') }), conTools),
      con));
    loadConsole(p.id);
    return h('div', { class: 'plugd' }, nodes);
  }

  /** 从插件源安装的插件可以卸载 */
  function uninstallBtn(p) {
    if (p.kind !== 'source') return null;
    return ib('trash', t('卸载'), async () => { if (await pluginSrcUninstall(srcCtx, p)) { selected = null; render(); } });
  }

  async function loadConsole(pluginId) {
    try {
      const l = await api('/api/plugins/console?id=' + encodeURIComponent(pluginId));
      pluginConsoles.set(pluginId, (l || []).slice(-1000));
    } catch {}
    paint();
  }

  function paint() {
    if (!con) return;
    const l = pluginConsoles.get(conKey) || [];
    const has = l.length > 0;
    con.hidden = !has;
    if (conTools) conTools.hidden = !has;
    if (!has) { con.replaceChildren(); return; }
    const stick = con.scrollHeight - con.scrollTop - con.clientHeight < 24;
    con.replaceChildren(...l.slice(-500).map((e) => h('div', { class: 'plug-cl' + (e.level === 'error' ? ' err' : e.level === 'warn' ? ' warn' : '') },
      h('span', { class: 't' }, pluginTime(e.time)), ' ',
      e.src ? h('span', { class: 's' }, pluginTxt(e.src) + ' ') : null,
      h('span', { class: 'm' }, pluginTxt(e.msg)))));
    if (stick) con.scrollTop = con.scrollHeight;
  }

  function newPluginDialog() {
    const id = h('input', { class: 'input mono', placeholder: 'my-plugin', spellcheck: 'false', autocomplete: 'off' });
    const name = h('input', { class: 'input', placeholder: t('我的插件'), autocomplete: 'off' });
    const err = h('div', { class: 'dlg-err' });
    const d = dialog({
      title: t('新建插件'),
      body: [field('ID', id, t('小写字母、数字、- 或 _')), field(t('名称'), name), err],
      actions: [h('button', { class: 'btn', type: 'button', onclick: () => d.close() }, t('取消')),
        h('button', { class: 'btn primary', type: 'submit' }, t('创建'))],
      onsubmit: async () => {
        const pid = id.value.trim();
        if (!/^[a-z0-9][a-z0-9_-]{0,39}$/.test(pid)) { err.textContent = t('ID 只能用小写字母、数字、- 或 _，最长 40 个字符'); return; }
        try {
          await api('/api/plugins/manage', { method: 'POST', body: JSON.stringify({ action: 'create', id: pid, name: name.value.trim() || pid }) });
          d.close();
          pluginInvalidate('local');
          selected = 'local|' + pid;
          await reload();
        } catch (e) { err.textContent = e.message; }
      },
    });
  }

  const self = {
    root,
    reload,
    onConsole(pluginId) { if (con && conKey === pluginId) paint(); },
    select(id) { selected = id; render(); },
    destroy() { for (const v of views) v.destroy(); views = []; if (pluginManager === self) pluginManager = null; },
  };
  pluginManager = self;
  root.append(h('div', { class: 'plug-console-e' }, '…'));
  reload();
  return root;
}

/** 插件放在了哪些地方：位置用图标表示（悬停看文字）；能直接用的可以点 */
const PLUGIN_WHERE = {
  device: ['phone', '设备右键菜单'], app: ['grid', '应用管理右键菜单'], file: ['folder', '文件管理右键菜单'],
  tools: ['puzzle', '顶栏的插件工具菜单'], toolbar: ['puzzle', '投屏窗口工具栏的插件按钮'],
  media: ['camera', '相册右键菜单'], logcat: ['logs', '设备日志右键菜单'],
  'panel:device': ['phone', '投屏窗口侧栏（插件按钮）'], 'panel:window': ['monitor', '独立窗口（插件工具菜单）'],
  'panel:settings': ['gear', '这个设置页'], status: ['logs', '侧栏底部的状态'], metric: ['activity', '性能浮层'], loghl: ['logs', '设备日志高亮'],
};
function pluginEntryList(p) {
  const box = h('div', { class: 'set-c plug-entries' }, h('div', { class: 'plug-console-e', style: 'padding:10px 12px' }, '…'));
  // 在线设备包括远程电脑上的：插件就作用在这台电脑能看到的所有设备上
  const online = pluginOnlineDevs();
  pluginContrib('', true).then((d) => {
    const rows = [];
    const add = (where, title, icon, run, needDev) => {
      const [wi, wt] = PLUGIN_WHERE[where] || ['puzzle', where];
      const dis = needDev && !online.length;
      const tip = t(wt) + (dis ? '\n' + t('连接设备后可用') : '');
      rows.push(h(run && !dis ? 'button' : 'div', { class: 'plug-entry' + (dis ? ' dis' : '') + (run && !dis ? ' run' : ''), type: run ? 'button' : null, title: tip, onclick: run && !dis ? run : null },
        h('span', { class: 'plug-pi', html: svg(wi) }),
        h('span', { class: 'si', html: svg(pluginIcon(icon, 'blank')) }),
        h('span', { class: 'n' }, pluginTxt(title)),
        run && !dis ? h('span', { class: 'si go', html: svg('play') }) : null));
    };
    const mine = (x) => x.plugin === p.id;
    for (const m of (d.menus || []).filter(mine)) {
      const target = m.target;
      const run = target === 'tools' ? () => pluginInvoke({ plugin: p.id, kind: 'menu', id: m.id, ctx: {} }) : null;
      add(target, String(m.path || '').split('/').pop(), m.icon, run, target !== 'tools' && target !== 'media');
    }
    for (const x of (d.panels || []).filter(mine)) {
      // 侧栏面板：优先用已经打开投屏的设备，否则第一台在线设备
      const run = x.dock === 'window' ? () => pluginOpenWindow(x, '')
        : x.dock === 'device' && online.length ? () => {
          const pick = online.find(({ hst, dev }) => pluginViewersOf(pluginDevId(hst, dev.serial)).length) || online[0];
          openViewer(pick.hst, pick.dev);
          setTimeout(() => {
            const v = pluginViewersOf(pluginDevId(pick.hst, pick.dev.serial))[0];
            if (v) pluginDockOpen(v, x);
          }, 600);
          const dlg = document.querySelector('dialog.settings[open]'); if (dlg) dlg.close();
        } : null;
      add('panel:' + x.dock, x.title || x.id, x.icon, run, x.dock === 'device');
    }
    for (const x of (d.status || []).filter(mine)) add('status', x.text, null, null, false);
    for (const x of (d.metrics || []).filter(mine)) add('metric', x.label, null, null, true);
    for (const x of (d.loghl || []).filter(mine)) add('loghl', x.label || x.text, null, null, true);
    box.replaceChildren(...(rows.length ? rows : [h('div', { class: 'plug-console-e', style: 'padding:10px 12px' }, '—')]));
  }).catch(() => box.replaceChildren());
  return box;
}

/** 设置项：一行一个，标签在左、控件在右（与其他设置一致） */
function pluginSettingRow(f, values, can, save) {
  const name = pluginTxt(f.name);
  const label = pluginTxt(f.label || f.name);
  const type = pluginTxt(f.type || 'text');
  const cur = values[name] === undefined ? f.default : values[name];
  const r = (ctrl) => h('div', { class: 'set-r' }, h('span', { class: 'k' }, label), ctrl);
  if (type === 'bool') {
    return r(pluginSwitch(cur === true || cur === 'true', !can, (on) => { values[name] = on; save(); }));
  }
  if (type === 'select') {
    const sel = h('select', { class: 'select', disabled: !can, onchange: () => { values[name] = sel.value; save(); } },
      (f.options || []).map((o) => (o && typeof o === 'object' ? h('option', { value: pluginTxt(o.value) }, pluginTxt(o.label || o.value)) : h('option', { value: pluginTxt(o) }, pluginTxt(o)))));
    sel.value = pluginTxt(cur === undefined ? '' : cur);
    return r(sel);
  }
  const inp = h('input', {
    class: 'input plug-in' + (type === 'file' || type === 'dir' ? ' mono' : ''), disabled: !can,
    type: type === 'password' ? 'password' : type === 'number' ? 'number' : 'text',
    value: pluginTxt(cur === undefined ? '' : cur), autocomplete: 'off', spellcheck: 'false',
    placeholder: type === 'dir' ? t('文件夹路径') : type === 'file' ? t('文件路径') : '',
    oninput: () => { values[name] = type === 'number' ? Number(inp.value) : inp.value; save(); },
  });
  return r(inp);
}

/** 启用前的权限确认（这里需要文字：用户要知道自己授权了什么） */
function pluginGrantDialog(p) {
  const perms = Array.isArray(p.permissions) ? p.permissions : [];
  if (!perms.length) return Promise.resolve(true);
  // 已启用的插件升级后申请了新权限：只列出新增的，按钮是“授权”
  const upgrade = !!p.enabled && p.state === 'needs_grant';
  const granted = Array.isArray(p.granted) ? p.granted : [];
  const has = (k) => granted.includes(k) || (k === 'fs.read' && granted.includes('fs'));
  const shown = upgrade ? perms.filter((k) => !has(k)) : perms;
  return new Promise((res) => {
    let ok = false;
    const dlg = dialog({
      title: upgrade ? t('“{0}”需要新的权限', pluginTxt(p.name || p.id)) : t('启用“{0}”？', pluginTxt(p.name || p.id)),
      body: [h('div', { class: 'set-c plug-grant' }, (shown.length ? shown : perms).map((k) => h('div', { class: 'set-r' },
        h('span', { class: 'plug-pi', html: svg(PLUGIN_PERM_ICON[k] || 'shield') }), h('span', { class: 'k' }, t(PLUGIN_PERMS[k] || k)))))],
      actions: [h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, t('取消')),
        h('button', { class: 'btn primary', type: 'submit' }, upgrade ? t('授权') : t('启用'))],
      onsubmit: () => { ok = true; dlg.close(); },
    });
    dlg.addEventListener('close', () => res(ok));
  });
}

/** 对已启用、等待权限确认的插件：弹出新增权限，确认后直接授权并启动（不用先关再开） */
async function pluginGrant(p) {
  if (!(await pluginGrantDialog(p))) return false;
  try {
    await api('/api/plugins/manage', { method: 'POST', body: JSON.stringify({ id: p.id, action: 'enable' }) });
    return true;
  } catch (e) {
    toast(e.message, { kind: 'bad' });
    return false;
  }
}

function pluginTime(v) {
  if (v === null || v === undefined || v === '') return '';
  const str = String(v);
  if (typeof v === 'number' || /^d{10,}$/.test(str)) {
    const d = new Date(Number(v));
    if (!isNaN(d.getTime())) return d.toLocaleTimeString(LANG === 'zh' ? 'zh-CN' : 'en', { hour12: false });
  }
  return str.length > 11 ? str.slice(11, 19) : str;
}

function pluginConsoleLines(pluginId) {
  return (pluginConsoles.get(pluginId) || []).map((e) => `[${e.level || 'info'}] ${e.src ? e.src + ' ' : ''}${e.msg || ''}`);
}

function pluginPushConsole(pluginId, entry) {
  const list = pluginConsoles.get(pluginId) || [];
  list.push(entry);
  if (list.length > 1000) list.splice(0, list.length - 1000);
  pluginConsoles.set(pluginId, list);
}

// ---------------- 插件发起的对话框 ----------------
const pluginDirWaits = new Map();
let pluginDirSeq = 0;
/** 桌面版且连的是本机（插件就跑在这台电脑上）才用系统文件夹对话框 */
function pluginNativeDir() {
  return !!(window.ipc && window.__DB_CHROME) && ['127.0.0.1', 'localhost', '[::1]'].includes(location.hostname);
}
/** 结果：路径 / null（取消）/ undefined（不支持） */
function pluginPickDirNative(start, title) {
  return new Promise((res) => {
    const id = ++pluginDirSeq;
    pluginDirWaits.set(id, res);
    try { window.ipc.postMessage('pickdir:' + JSON.stringify({ id, start, title })); } catch { pluginDirWaits.delete(id); res(undefined); }
  });
}
window.__dbPickedDir = (id, path, ok) => {
  const res = pluginDirWaits.get(id);
  if (!res) return;
  pluginDirWaits.delete(id);
  res(ok ? (path || null) : undefined);
};
function pluginAnswerDialog(id, value) {
  api('/api/plugins/dialog', { method: 'POST', body: JSON.stringify({ id, value }) }).catch(() => {});
}

function pluginShowDialog(m) {
  if (!m || !m.id) return;
  // 选文件夹：本机桌面版用系统对话框，其他情况（浏览器、平台不支持）退回输入路径
  if (pluginTxt(m.kind) === 'dir') {
    const fallback = () => pluginShowDialog(Object.assign({}, m, { kind: 'prompt', message: m.message || t('文件夹路径') }));
    if (!pluginNativeDir()) return fallback();
    pluginOpenDialogs.set(String(m.id), () => pluginOpenDialogs.delete(String(m.id)));
    pluginPickDirNative(pluginTxt(m.default), pluginTxt(m.title)).then((v) => {
      if (!pluginOpenDialogs.has(String(m.id))) return;
      if (v === undefined) return fallback();
      pluginOpenDialogs.delete(String(m.id));
      pluginAnswerDialog(m.id, v);
    });
    return;
  }
  const kind = pluginTxt(m.kind);
  const title = pluginTxt(m.title || (m.plugin || ''));
  let closed = false;
  let dlg = null;
  const cleanup = () => { pluginOpenDialogs.delete(String(m.id)); };
  const done = (value) => {
    if (closed) return;
    closed = true;
    cleanup();
    pluginAnswerDialog(m.id, value === undefined ? null : value);
  };
  // dialog.close：后端让这个对话框失效（例如别的界面已经回答）
  pluginOpenDialogs.set(String(m.id), () => { closed = true; cleanup(); if (dlg && dlg.open) dlg.close(); });

  if (kind === 'file') {
    pluginFileDialog(title, pluginTxt(m.message), pluginTxt(m.accept), (d) => { dlg = d; })
      .then((v) => done(v === undefined ? null : v));
    return;
  }
  if (kind === 'form') {
    pluginFormDialog(title, pluginTxt(m.message), Array.isArray(m.fields) ? m.fields : [], (d) => { dlg = d; })
      .then((v) => done(v));
    return;
  }
  // confirm / prompt：自己搭对话框，方便 dialog.close 关掉它
  const body = [];
  let result = null;
  let getValue = null;
  if (kind === 'prompt') {
    const inp = h('input', { class: 'input', value: pluginTxt(m.default), autocomplete: 'off', autofocus: true });
    getValue = () => inp.value;
    body.push(m.message ? h('div', { class: 'hint' }, pluginTxt(m.message)) : null, inp);
  } else if (m.message) {
    body.push(h('div', { class: 'hint' }, pluginTxt(m.message)));
  }
  const okText = pluginTxt(m.accept) || t('确定');
  dlg = dialog({
    title, body,
    actions: [h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, t('取消')),
      h('button', { class: 'btn primary', type: 'submit' }, okText)],
    onsubmit: () => { result = kind === 'prompt' ? getValue() : true; dlg.close(); },
  });
  dlg.addEventListener('close', () => {
    if (closed) return;
    if (result === null) done(kind === 'prompt' ? null : false);
    else done(result);
  });
}
function pluginFormDialog(title, message, fields, setup) {
  return new Promise((res) => {
    const inputs = [];
    const err = h('div', { class: 'dlg-err' });
    const body = [];
    if (message) body.push(h('div', { class: 'hint' }, message));
    for (const f of fields) {
      const name = pluginTxt(f && f.name);
      const label = pluginTxt((f && (f.label || f.name)) || '');
      const type = pluginTxt(f && f.type || 'text');
      const def = f && f.default !== undefined ? f.default : '';
      let get;
      if (type === 'bool') {
        const cb = h('input', { type: 'checkbox' });
        cb.checked = def === true || def === 'true';
        get = () => cb.checked;
        body.push(h('label', { class: 'check' }, cb, h('span', {}, label)));
      } else if (type === 'select') {
        const sel = h('select', { class: 'select' }, (f.options || []).map((o) => {
          const v = o && typeof o === 'object' ? o.value : o;
          const l = o && typeof o === 'object' ? (o.label === undefined ? o.value : o.label) : o;
          return h('option', { value: pluginTxt(v) }, pluginTxt(l));
        }));
        sel.value = pluginTxt(def);
        get = () => sel.value;
        body.push(field(label, sel));
      } else {
        const inp = h('input', { class: 'input', type: type === 'number' ? 'number' : type === 'password' ? 'password' : 'text', value: pluginTxt(def), autocomplete: 'off' });
        get = () => (type === 'number' ? Number(inp.value) : inp.value);
        body.push(field(label, inp));
      }
      inputs.push([name, get]);
    }
    let result = null;
    const dlg = dialog({
      title, body: [...body, err],
      actions: [h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, t('取消')),
        h('button', { class: 'btn primary', type: 'submit' }, t('确定'))],
      onsubmit: () => {
        const obj = {};
        for (const [name, get] of inputs) obj[name] = get();
        result = obj;
        dlg.close();
      },
    });
    if (setup) setup(dlg);
    dlg.addEventListener('close', () => res(result));
  });
}

function pluginFileDialog(title, message, accept, setup) {
  return new Promise((res) => {
    const picker = h('input', { type: 'file', hidden: true, accept: accept || '' });
    const nameEl = h('div', { class: 'hint' });
    let result = null, busy = false;
    const dlg = dialog({
      title, desc: message || t('选择一个文件上传给插件'),
      body: [h('button', { class: 'btn', type: 'button', onclick: () => picker.click() }, t('选择文件…')), nameEl, picker],
      actions: [h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, t('取消'))],
    });
    if (setup) setup(dlg);
    picker.onchange = async () => {
      const f = picker.files && picker.files[0];
      if (!f || busy) return;
      busy = true;
      nameEl.textContent = t('正在上传…') + ' ' + f.name;
      try {
        const qs = new URLSearchParams({ name: f.name });
        const r = await fetch(withToken('/api/plugins/upload?' + qs.toString()), {
          method: 'POST', headers: { 'x-lang': LANG }, body: f,
        });
        const j = await r.json().catch(() => ({}));
        if (!r.ok) throw new Error(j.error || ('HTTP ' + r.status));
        result = pluginTxt(j.path);
        nameEl.textContent = f.name;
        dlg.close();
      } catch (e) {
        nameEl.textContent = e.message;
        busy = false;
        picker.value = '';
      }
    };
    dlg.addEventListener('close', () => res(result));
  });
}

// ---------------- WS 事件 ----------------
function pluginOnEvent(m) {
  switch (m.type) {
    case 'changed':
      pluginInvalidate();
      pluginContrib('').catch(() => {});
      if (pluginManager) pluginManager.reload();
      pluginSideChanged();
      if (typeof pluginSrcChanged === 'function') pluginSrcChanged();
      for (const v of viewers.values()) pluginViewerUpdate(v);
      break;
    case 'open': {
      pluginOpenUrl(pluginTxt(m.plugin), m.url, { name: m.name, title: m.title, mode: m.mode });
      break;
    }
    case 'mirror': {
      const { hostId, serial } = pluginDevParse(m.serial);
      const hit = pluginOnlineDevs().find(({ hst, dev }) => hst.id === hostId && dev.serial === serial);
      if (hit) openViewer(hit.hst, hit.dev);
      break;
    }
    case 'toast': {
      const kind = m.kind === 'error' ? 'bad' : m.kind === 'warn' ? 'warn' : m.kind === 'ok' ? 'ok' : undefined;
      toast(pluginTxt(m.text), { kind, ms: 5000 });
      break;
    }
    case 'state': {
      if (pluginList && pluginList.list) {
        const p = pluginList.list.find((x) => x.id === m.plugin);
        // 已启用的插件升级后申请了新权限：提示一次，点提示直接去授权
        if (p && p.enabled && m.state === 'needs_grant' && p.state !== 'needs_grant' && pluginList.manage) {
          toast(t('“{0}”需要新的权限', pluginTxt(p.name || p.id)), { kind: 'warn', ms: 8000, onclick: () => pluginOpenSettings(p.id) });
        }
        if (p) { p.state = m.state; p.error = m.error || ''; }
        if (pluginManager) pluginManager.reload();
      }
      break;
    }
    case 'dialog':
      pluginShowDialog(m);
      break;
    case 'dialog.close': {
      const close = pluginOpenDialogs.get(String(m.id));
      if (close) close();
      break;
    }
    case 'progress': {
      const key = pluginTxt(m.plugin) + '|' + pluginTxt(m.id);
      let tt = pluginProgress.get(key);
      const text = [pluginTxt(m.title), pluginTxt(m.text)].filter(Boolean).join(' · ');
      const pct = m.frac === null || m.frac === undefined ? '' : ' ' + Math.round(Number(m.frac) * 100) + '%';
      if (m.done) {
        if (tt) { tt.set(text + ' ' + t('完成'), 'ok'); tt.close(3000); pluginProgress.delete(key); }
        break;
      }
      if (!tt) { tt = toast(text + pct, { sticky: true }); pluginProgress.set(key, tt); }
      else tt.set(text + pct);
      break;
    }
    case 'repaint': {
      for (const view of pluginViews) {
        if (!m.plugin || view.plugin !== m.plugin) continue;
        if (m.panel && view.id !== m.panel) continue;
        view.refresh();
      }
      break;
    }
    case 'overlay': {
      const devId = pluginTxt(m.serial);
      const list = (pluginOverlayMaps.get(devId) || []).filter((x) => x.plugin !== m.plugin);
      if (m.shapes && m.shapes.length) list.push({ plugin: m.plugin, shapes: m.shapes, size: m.size });
      pluginOverlayMaps.set(devId, list);
      pluginRepaintOverlay(devId);
      break;
    }
    case 'console': {
      if (!m.entry) break;
      pluginPushConsole(pluginTxt(m.plugin), m.entry);
      if (pluginManager) pluginManager.onConsole(pluginTxt(m.plugin));
      break;
    }
    case 'webmsg': {
      for (const ctx of pluginWebCtx) {
        if (ctx.plugin !== m.plugin) continue;
        pluginWebPost(ctx, { __droidbridge: 1, t: 'event', name: pluginTxt(m.name), data: m.data });
      }
      break;
    }
  }
}

// ---------------- 覆盖层 ----------------
function pluginOverlayFit(v) {
  const cv = v.pov;
  if (!cv) return;
  const r = v.canvas.getBoundingClientRect();
  const sr = v.canvas.parentElement.getBoundingClientRect();
  if (!r.width) return;
  cv.style.left = (r.left - sr.left) + 'px';
  cv.style.top = (r.top - sr.top) + 'px';
  cv.style.width = r.width + 'px';
  cv.style.height = r.height + 'px';
  if (v.vw && v.vh && (cv.width !== v.vw || cv.height !== v.vh)) { cv.width = v.vw; cv.height = v.vh; }
}

function pluginOverlayPaint(v) {
  const cv = v.pov;
  if (!cv) return;
  pluginOverlayFit(v);
  const key = pluginDevId(v.host, v.dev.serial);
  const list = pluginOverlayMaps.get(key) || [];
  const ctx = cv.getContext('2d');
  ctx.clearRect(0, 0, cv.width, cv.height);
  for (const item of list) {
    // size：插件声明的坐标空间（如设备分辨率），画面可能被缩小过（max_size）
    const sz = item.size;
    const sx = sz && sz.w > 0 ? cv.width / sz.w : 1;
    const sy = sz && sz.h > 0 ? cv.height / sz.h : 1;
    for (const s of item.shapes || []) {
      if (!s || typeof s !== 'object') continue;
      const color = typeof s.color === 'string' && s.color.length < 40 ? s.color : '#ff4d4f';
      ctx.strokeStyle = color;
      ctx.fillStyle = color;
      ctx.lineWidth = Math.max(1, Math.round(cv.width / 400));
      ctx.font = Math.round(Math.max(10, cv.width / 60)) + 'px sans-serif';
      const x = (Number(s.x) || 0) * sx, y = (Number(s.y) || 0) * sy;
      if (s.type === 'rect') {
        const w = (Number(s.w) || 0) * sx, ht = (Number(s.h) || 0) * sy;
        const fill = typeof s.fill === 'number' ? Math.max(0, Math.min(1, s.fill)) : 0.2;
        ctx.save();
        if (fill > 0) {
          ctx.globalAlpha = fill;
          ctx.fillRect(x, y, w, ht);
          ctx.globalAlpha = 1;
        }
        if (s.dash) ctx.setLineDash([ctx.lineWidth * 4, ctx.lineWidth * 3]);
        ctx.strokeRect(x, y, w, ht);
        ctx.restore();
      } else if (s.type === 'text') {
        ctx.fillText(pluginTxt(s.text), x, y);
      } else if (s.type === 'point') {
        ctx.beginPath();
        ctx.arc(x, y, Math.max(3, cv.width / 120), 0, Math.PI * 2);
        ctx.fill();
      }
      if (s.text && s.type !== 'text') ctx.fillText(pluginTxt(s.text), x, Math.max(12, y - 4));
    }
  }
}

function pluginRepaintOverlay(devId) {
  for (const v of pluginViewersOf(devId)) pluginOverlayPaint(v);
}

// ---------------- 性能指标 ----------------
function pluginPerfStart(v) {
  if (v.plugPerfBox) return;
  v.plugPerfBox = h('div', { class: 'plug-perf' });
  const devId = pluginDevId(v.host, v.dev.serial);
  const tick = async () => {
    if (v.closed) return;
    try {
      const list = await api('/api/plugins/metrics?serial=' + encodeURIComponent(devId));
      pluginMetricsMap.set(devId, (list || []).filter((x) => x && x.label));
      pluginPerfPaint(v);
    } catch {}
    if (v.perfOn && !v.closed) v.plugPerfTimer = setTimeout(tick, 2000);
  };
  v.plugPerfTimer = setTimeout(tick, 300);
}

function pluginPerfStop(v) {
  clearTimeout(v.plugPerfTimer);
  v.plugPerfTimer = null;
  if (v.plugPerfBox) { v.plugPerfBox.remove(); v.plugPerfBox = null; }
}

function pluginPerfPaint(v) {
  const box = v.plugPerfBox;
  if (!box) return;
  const list = pluginMetricsMap.get(pluginDevId(v.host, v.dev.serial)) || [];
  if (!list.length) { box.replaceChildren(); return; }
  box.replaceChildren(...list.map((m) => h('div', { class: 'pr' },
    h('span', { class: 'pl' }, pluginTxt(m.label)),
    h('span', {}, m.value === null || m.value === undefined ? '–' : pluginTxt(m.value) + (m.unit ? ' ' + pluginTxt(m.unit) : '')))));
}

// ---------------- 日志高亮 ----------------
/** 日志高亮是电脑级贡献：所有电脑上的设备共用 */
function pluginLogHighlight(raw, tag) {
  const d = pluginContribCached('');
  const list = d && d.loghl;
  if (!list || !list.length) return null;
  const low = String(raw || '').toLowerCase();
  for (const h of list) {
    const text = pluginTxt(h.text).toLowerCase();
    if (!text || !low.includes(text)) continue;
    if (h.tag && tag && String(tag).toLowerCase() !== String(h.tag).toLowerCase()) continue;
    return h;
  }
  return null;
}

// ---------------- 投屏窗口：页签与工具栏 ----------------
/** 投屏卡片工具列的插件项：pin 的直接显示成按钮，其余放进拼图菜单 */
function pluginToolbarEntries(devId, pinned) {
  return pluginMenuEntries(devId, 'toolbar', { serial: devId }).filter((m) => !!m.pin === pinned);
}

function pluginViewerUpdate(v) {
  if (!v || !v.plugBtn) return;
  const devId = pluginDevId(v.host, v.dev.serial);
  const panels = pluginPanelsFor(devId, 'device');
  const menus = pluginToolbarEntries(devId, false);
  if (v.plugPins) {
    v.plugPins.replaceChildren(...pluginToolbarEntries(devId, true).map((e) => {
      const on = Array.isArray(e.on) && e.on.includes(devId);
      const label = String(e.path || e.id).split('/').pop();
      return ib(pluginIcon(e.icon), label, () => pluginInvoke({ plugin: e.plugin, kind: 'menu', id: e.id, ctx: { serial: devId } }), on ? 'on' : '');
    }));
  }
  v.plugBtn.hidden = !(panels.length || menus.length);
  if (v.plugBtn.hidden) pluginDockClose(v);
  else if (v.plugDock) pluginDockSync(v);
}

// 同一插件里 group 相同的面板合成一个页签：页签上用小图标切换，各页面一直保留（切换不重新加载）
const pluginGroupKey = (p) => (p.group ? p.plugin + '#' + pluginTxt(p.group) : p.plugin + '|' + p.id);

/** 按页签分组：[{key, title, panels:[...]}]，保持首次出现的顺序 */
function pluginDockGroups(panels) {
  const out = [];
  const byKey = new Map();
  for (const p of panels) {
    const k = pluginGroupKey(p);
    let g = byKey.get(k);
    if (!g) {
      g = { key: k, title: p.group ? pluginTxt(p.group) : pluginTxt(p.title || p.id), panels: [] };
      byKey.set(k, g);
      out.push(g);
    }
    g.panels.push(p);
  }
  return out;
}

function pluginDockMenu(v) {
  const devId = pluginDevId(v.host, v.dev.serial);
  const items = [];
  for (const g of pluginDockGroups(pluginPanelsFor(devId, 'device'))) {
    const p = (v.plugDock && v.plugDock.groupSel.get(g.key)) || g.panels[0];
    items.push({ icon: pluginIcon(g.panels[0].icon, 'monitor'), label: g.title, fn: () => pluginDockOpen(v, p) });
  }
  const menus = pluginToolbarEntries(devId, false);
  if (menus.length) {
    if (items.length) items.push('-');
    items.push(...pluginMenuTree(menus, { serial: devId }));
  }
  if (items.length) menu(v.plugBtn, items);
}

function pluginDockInit(v) {
  if (v.plugDock) return v.plugDock;
  const tabs = h('div', { class: 'plug-tabs' });
  const stack = h('div', { class: 'plug-stack' });
  v.plugDock = { el: h('div', { class: 'plug-dock' }, tabs, stack), tabs, stack, views: new Map(), active: null, defs: new Map(), groupSel: new Map() };
  return v.plugDock;
}

function pluginDockSync(v) {
  const dock = v.plugDock;
  if (!dock) return;
  const devId = pluginDevId(v.host, v.dev.serial);
  const panels = pluginPanelsFor(devId, 'device');
  for (const [key, view] of [...dock.views]) {
    if (!panels.some((p) => p.plugin + '|' + p.id === key)) { view.destroy(); dock.views.delete(key); dock.defs.delete(key); }
  }
  for (const p of panels) {
    const key = p.plugin + '|' + p.id;
    if (dock.views.has(key)) continue;
    const view = pluginPanelView(p, devId);
    dock.views.set(key, view);
    dock.defs.set(key, p);
    view.el.hidden = true;
    dock.stack.append(view.el);
  }
  pluginDockTabs(v);
  if (dock.active && !dock.views.has(dock.active)) { dock.active = null; }
  if (!dock.active) { const first = panels[0]; if (first) dock.active = first.plugin + '|' + first.id; }
  pluginDockShow(v, dock.active);
}

function pluginDockTabs(v) {
  const dock = v.plugDock;
  if (!dock) return;
  const panels = pluginPanelsFor(pluginDevId(v.host, v.dev.serial), 'device');
  const activeDef = dock.defs.get(dock.active);
  const activeGroup = activeDef ? pluginGroupKey(activeDef) : '';
  dock.tabs.replaceChildren(...pluginDockGroups(panels).map((g) => {
    const on = g.key === activeGroup;
    const first = dock.groupSel.get(g.key) || g.panels[0];
    const tab = h('button', {
      class: 'plug-tab' + (on ? ' on' : ''), type: 'button', title: g.title,
      onclick: () => pluginDockShow(v, first.plugin + '|' + first.id),
    }, h('span', { class: 'ico', html: svg(pluginIcon(g.panels[0].icon, 'monitor')) }), h('span', { class: 't' }, g.title));
    if (g.panels.length < 2) return tab;
    // 组内切换：只在当前页签上显示，图标 + 悬停标题
    const seg = h('span', { class: 'plug-seg' }, g.panels.map((p) => {
      const key = p.plugin + '|' + p.id;
      return h('button', {
        class: 'plug-segb' + (dock.active === key ? ' on' : ''), type: 'button', title: pluginTxt(p.title),
        html: svg(pluginIcon(p.icon, 'monitor')), onclick: (e) => { e.stopPropagation(); pluginDockShow(v, key); },
      });
    }));
    return h('span', { class: 'plug-tabg' + (on ? ' on' : '') }, tab, on ? seg : null);
  }), ib('x', t('关闭插件面板'), () => pluginDockClose(v)));
}

function pluginDockShow(v, key) {
  const dock = v.plugDock;
  if (!dock) return;
  dock.active = key;
  const def = dock.defs.get(key);
  if (def) dock.groupSel.set(pluginGroupKey(def), def);
  for (const [k, view] of dock.views) view.el.hidden = k !== key;
  pluginDockTabs(v);
  const view = dock.views.get(key);
  // 网页面板一直保留，切回来不重新加载；即时模式面板重绘一次
  if (view && !(view instanceof PluginWebView) && !(view instanceof PluginExtView)) view.refresh();
}

function pluginDockOpen(v, p) {
  pluginDockInit(v);
  pluginDockSync(v);
  pluginDockShow(v, p.plugin + '|' + p.id);
  if (v.plugWrap) {
    if (!v.plugWrap.contains(v.plugDock.el)) v.plugWrap.replaceChildren(v.plugDock.el);
    v.activateDock('plug');
  } else {
    v.vlog.append(v.plugDock.el);
    v.root.classList.add('with-log');
    v.plugBtn.classList.add('on');
    v.fit();
  }
}

function pluginDockClose(v) {
  if (!v.plugDock) return;
  for (const view of v.plugDock.views.values()) view.destroy();
  v.plugDock.el.remove();
  v.plugDock = null;
  if (typeof v.onPlugDockClosed === 'function') v.onPlugDockClosed();
  else {
    if (v.plugBtn) v.plugBtn.classList.remove('on');
    if (v.root) v.root.classList.toggle('with-log', !!(v.log || v.files));
    if (v.fit) v.fit();
  }
}

function pluginViewerDestroy(v) {
  pluginPerfStop(v);
  pluginDockClose(v);
}

// ---------------- 初始化 ----------------
function pluginsInit() {
  pluginsStart();
  // 插件源：启动时读一次，有新版时设置按钮上显示圆点
  if (typeof pluginSrcWatch === 'function') pluginSrcWatch();
  // 首次进入时把贡献拉一遍，保证右键菜单、徽标、状态栏很快出现
  setTimeout(() => { pluginTick(); }, 800);
}
