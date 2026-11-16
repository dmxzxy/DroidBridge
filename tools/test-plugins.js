#!/usr/bin/env node
// 插件回归测试（docs/plugins/SPEC.md 第 7.4 节）：插件只运行在界面所连的本机，
// 但要作用于本机能看到的全部设备——远程电脑上的设备用 <电脑id>/<serial> 标识。
// 用 tools/demo/mock.js 模拟后端（本机 + 一台远程电脑 pB），再叠一层插件接口 mock。
// 用法：node tools/test-plugins.js（需要 Chrome / Edge / Chromium；BROWSER 环境变量可指定路径）
const fs = require('fs');
const path = require('path');
const os = require('os');
process.chdir(path.resolve(__dirname, '..'));
const { spawn } = require('child_process');
const sleep = ms => new Promise(r => setTimeout(r, ms));

// ---------------- 插件接口 mock ----------------
// 本机 /api/plugins/contrib：serial 为空时返回电脑级贡献（工具菜单、状态、日志高亮、覆盖层），
// 给了 serial 时返回该设备（本机 serial 或 <peer>/<serial>）的贡献：device 菜单 + dock=device 面板。
const PLUGIN_MOCK = `
(() => {
  const P = window.__PLUG = { reqs: [], urls: [], wsUrl: '', ws: null, wsCount: 0 };
  const json = (b, s = 200) => new Response(JSON.stringify(b), { status: s, headers: { 'content-type': 'application/json' } });
  const menu = (id, target, p, extra) => Object.assign({ plugin: 'devmenu', id, target, path: p, icon: 'package' }, extra || {});
  const contrib = (serial) => serial ? {
    menus: [menu('dev-menu', 'device', '远程测试包'), menu('tb-menu', 'toolbar', '工具栏测试'),
      menu('app-menu', 'app', '远程应用包', { filter: { packages: ['com.example.game'] } }),
      menu('file-menu', 'file', '远程文件包', { filter: { ext: ['apk'] } })],
    panels: [{ plugin: 'devmenu', id: 'devpanel', title: '设备面板', icon: 'monitor', dock: 'device', refresh: 0 }],
    status: [], metrics: [{ plugin: 'devmenu', id: 'm1', label: 'PLUGMETRIC', unit: '%' }], loghl: [],
    badges: { [serial]: [{ plugin: 'devmenu', text: 'PLUG', kind: 'ok', tip: 'badge tip' }] },
    overlays: {},
  } : {
    menus: [menu('tools-hello', 'tools', '电脑工具')],
    panels: [{ plugin: 'devmenu', id: 'win1', title: '窗口面板', icon: 'monitor', dock: 'window' }],
    status: [{ plugin: 'devmenu', id: 'st1', text: '插件状态', kind: 'ok', clickable: true }],
    metrics: [], loghl: [{ plugin: 'devmenu', id: 'lh', text: 'NullReferenceException', kind: 'bad', label: 'PLUG' }],
    badges: {}, overlays: {},
  };
  const list = () => ({ manage: true, dir: '/plugins', safeMode: false, list: [
    { id: 'devmenu', name: 'Dev Menu', version: '1.0.0', description: '', dir: '/plugins/devmenu', state: 'running', enabled: true,
      permissions: ['device.shell'], granted: ['device.shell'], settings: [], values: {} },
  ] });
  const prev = window.fetch;
  window.fetch = async (url, opts) => {
    const raw = String(url);
    const u = new URL(raw, 'http://x');
    if (!u.pathname.startsWith('/api/plugins')) return prev(url, opts);
    let body = null;
    try { body = typeof opts?.body === 'string' ? JSON.parse(opts.body) : null; } catch {}
    P.reqs.push({ path: u.pathname, query: u.search, serial: u.searchParams.get('serial'), peer: u.searchParams.get('peer'),
      id: u.searchParams.get('id'), body, url: raw });
    P.urls.push(raw);
    switch (u.pathname) {
      case '/api/plugins': return json(list());
      case '/api/plugins/contrib': return json(contrib(u.searchParams.get('serial') || ''));
      case '/api/plugins/invoke': return json({ ok: '已执行' });
      case '/api/plugins/panel': return json({ title: '设备面板', tree: [{ w: 'label', text: 'PANEL ' + (body && body.serial) }] });
      case '/api/plugins/metrics': return json([{ plugin: 'devmenu', id: 'm1', label: 'PLUGMETRIC', unit: '%', value: 42 }]);
      case '/api/plugins/console': return json([]);
      default: return json({ ok: true });
    }
  };
  // 只接管 /ws/plugins：其余（投屏、日志、性能）交给演示 mock
  const PrevWS = window.WebSocket;
  window.WebSocket = function (url) {
    const s = String(url);
    if (s.indexOf('/ws/plugins') < 0) return new PrevWS(url);
    P.wsUrl = s; P.wsCount++;
    const ws = { url: s, readyState: 0, sent: [], close() { this.readyState = 3; } };
    P.ws = ws;
    setTimeout(() => { ws.readyState = 1; if (ws.onopen) ws.onopen(); }, 30);
    return ws;
  };
})();
`;

let html = fs.readFileSync('web/index.html', 'utf8');
html = html.replace('<meta charset="utf-8">', () => `<meta charset="utf-8">\n<script>\n${fs.readFileSync('tools/demo/mock.js', 'utf8')}\n</script>\n<script>\n${PLUGIN_MOCK}\n</script>`);
html = html.replace('<link rel="stylesheet" href="app.css">', () => `<style>\n${fs.readFileSync('web/app.css', 'utf8')}\n</style>`);
html = html.replace(/<script src="js\/([A-Za-z0-9_-]+)\.js"><\/script>/g, (m, n) => `<script>\n${fs.readFileSync('web/js/' + n + '.js', 'utf8')}\n</script>`);
const page = path.join(os.tmpdir(), 'db-plugins.html');
fs.writeFileSync(page, html);

const PORT = 8893;      // 静态服务器（不要用 8730，那是真实后端）
const CDP = 9343;       // 调试端口
const srv = require('http').createServer((q, r) => { r.setHeader('content-type', 'text/html; charset=utf-8'); r.end(fs.readFileSync(page)); }).listen(PORT);
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'db-plug-'));
const browser = process.env.BROWSER || [
  '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable', '/usr/bin/chromium', '/usr/bin/chromium-browser',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
].find(f => fs.existsSync(f));
if (!browser) { console.error('未找到 Chrome / Edge / Chromium，可用 BROWSER 环境变量指定'); srv.close(); process.exit(2); }
const p = spawn(browser, ['--headless=new', '--no-sandbox', '--disable-gpu', '--remote-debugging-port=' + CDP, '--user-data-dir=' + profile, '--window-size=1440,900', 'about:blank']);
let fails = 0;
const check = (l, ok, d) => { console.log((ok ? '✓ ' : '✗ ') + l + (d ? '  ' + d : '')); if (!ok) fails++; };
const REMOTE_DEV = '2A281FDH3000XY';            // 在远程电脑 pB 上
const REMOTE_ID = 'pB/' + REMOTE_DEV;           // 插件接口里的标识

(async () => {
  let tabs;
  for (let i = 0; i < 240; i++) { try { tabs = await (await fetch('http://127.0.0.1:' + CDP + '/json')).json(); if (tabs && tabs.some(t => t.type === 'page')) break; } catch {} await sleep(250); }
  if (!tabs || !tabs.some(t => t.type === 'page')) { console.error('浏览器没有启动（60 秒内未打开调试端口）'); p.kill(); srv.close(); process.exit(3); }
  const ws = new WebSocket(tabs.find(t => t.type === 'page').webSocketDebuggerUrl);
  await new Promise(r => ws.onopen = r);
  let id = 0; const pend = new Map(); const errs = [];
  ws.onmessage = e => {
    const m = JSON.parse(e.data);
    if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); }
    if (m.method === 'Runtime.exceptionThrown') errs.push((m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text).slice(0, 200));
  };
  const send = (method, params = {}) => new Promise(r => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
  const ev = async (x) => {
    const r = await send('Runtime.evaluate', { expression: x, awaitPromise: true, returnByValue: true });
    return r.result.exceptionDetails ? 'EXC ' + (r.result.exceptionDetails.exception?.description || r.result.exceptionDetails.text) : r.result.result?.value;
  };
  const waitFor = async (expr, n = 80, ms = 250) => { for (let i = 0; i < n; i++) { if ((await ev(expr)) === true) return true; await sleep(ms); } return false; };
  const rclick = async (x, y) => {
    await send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'right', clickCount: 1 });
    await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'right', clickCount: 1 });
    await sleep(250);
  };
  const center = async (sel) => JSON.parse(await ev(`JSON.stringify((() => { const el = document.querySelector(${JSON.stringify(sel)}); if (!el) return null; const r = el.getBoundingClientRect(); return { x: r.left + Math.min(40, r.width / 2), y: r.top + r.height / 2 }; })())`));
  const menuItems = () => ev(`(() => { const m = document.querySelector('.menu'); return m ? [...m.children].filter(x => x.tagName === 'BUTTON').map(b => b.textContent + (b.disabled ? '(禁用)' : '')).join('/') : ''; })()`);
  const clickMenu = (label) => ev(`(() => { const b = [...document.querySelectorAll('.menu button')].find(x => x.textContent === ${JSON.stringify(label)}); if (!b) return false; b.click(); return true; })()`);
  const reqs = async () => JSON.parse(await ev(`JSON.stringify(window.__PLUG.reqs)`));

  await send('Runtime.enable');
  await send('Page.navigate', { url: 'http://127.0.0.1:' + PORT + '/?s=desk&lang=zh' });
  // 等设备列表、3 个投屏窗口和插件贡献都就绪
  const ready = await waitFor(`typeof pluginContribMaps === 'object' && viewers.size >= 3 && !!pluginContribMaps.get(${JSON.stringify(REMOTE_ID)}) && !!pluginContribMaps.get('')`);
  check('页面就绪：远程设备与本机的插件贡献都拉到了', ready === true,
    await ev(`JSON.stringify({ viewers: viewers.size, keys: [...pluginContribMaps.keys()] })`));

  // 1. 只连本机的 /ws/plugins
  const wsInfo = JSON.parse(await ev(`JSON.stringify({ url: window.__PLUG.wsUrl, n: window.__PLUG.wsCount })`));
  check('/ws/plugins 只连本机（不带 ?peer=）', wsInfo.n === 1 && wsInfo.url.indexOf('/ws/plugins') > 0 && wsInfo.url.indexOf('peer=') < 0, wsInfo.url);
  // 2. 所有插件请求都发给本机
  const urls = JSON.parse(await ev(`JSON.stringify(window.__PLUG.urls)`));
  check('插件请求都没有 ?peer=', urls.length > 0 && urls.every(u => u.indexOf('peer=') < 0), urls.slice(0, 4).join(' '));
  check('本机设备用原始 serial 取贡献', urls.some(u => u.indexOf('/api/plugins/contrib?serial=R5CT20ABCDE') === 0), urls.filter(u => u.indexOf('/contrib') >= 0).join(' '));
  check('远程设备用 <电脑id>/<serial> 取贡献', urls.some(u => u.indexOf('/api/plugins/contrib?serial=' + encodeURIComponent(REMOTE_ID)) === 0));

  // 3. 顶栏插件工具菜单与状态栏项（只用本机数据）
  await waitFor(`!!document.querySelector('.plug-toolsbtn') && !!document.querySelector('#statusbar .sb-it')`);
  check('顶栏出现插件工具按钮', (await ev(`!!document.querySelector('.plug-toolsbtn')`)) === true);
  check('窗口底部状态栏出现插件状态项', (await ev(`(document.querySelector('#statusbar .sb-it .t') || {}).textContent`)) === '插件状态');

  // 4. 远程设备的右键菜单里有本机插件的菜单项
  const devPoint = await center('.dev[title="' + REMOTE_DEV + '"]');
  await rclick(devPoint.x, devPoint.y);
  let items = await menuItems();
  check('远程设备的右键菜单出现“插件”', items.split('/').includes('插件'), items);
  await clickMenu('插件');
  await sleep(200);
  const sub = await ev(`(() => { const s = document.querySelector('.menu .sub'); return s ? [...s.children].filter(x => x.tagName === 'BUTTON').map(b => b.textContent).join('/') : ''; })()`);
  check('子菜单列出该插件的菜单项', sub === '远程测试包', sub);
  const before = (await reqs()).length;
  await clickMenu('远程测试包');
  await waitFor(`window.__PLUG.reqs.some(r => r.path === '/api/plugins/invoke')`, 20, 150);
  const invoke = (await reqs()).slice(before).find(r => r.path === '/api/plugins/invoke');
  check('点击后请求发到本机（invoke 不带 ?peer=）', !!invoke && !invoke.peer && invoke.url.indexOf('peer=') < 0, invoke && invoke.url);
  check('invoke 的 ctx.serial 是 <电脑id>/<serial>', !!invoke && invoke.body && invoke.body.ctx && invoke.body.ctx.serial === REMOTE_ID, invoke && JSON.stringify(invoke.body && invoke.body.ctx));
  check('invoke 的 kind/id 正确', !!invoke && invoke.body.kind === 'menu' && invoke.body.id === 'dev-menu' && invoke.body.plugin === 'devmenu');

  // 5. 该设备的投屏工具栏插件按钮可见，面板按设备标识请求
  check('远程设备的投屏工具栏插件按钮可见', (await ev(`(() => { const v = [...viewers.values()].find(x => x.dev.serial === ${JSON.stringify(REMOTE_DEV)}); return !!v && v.plugBtn.hidden === false; })()`)) === true);
  await ev(`(() => { const v = [...viewers.values()].find(x => x.dev.serial === ${JSON.stringify(REMOTE_DEV)}); v.plugBtn.click(); return 1; })()`);
  await sleep(200);
  items = await menuItems();
  check('工具栏插件菜单有面板与工具栏菜单项', items.includes('设备面板') && items.includes('工具栏测试'), items);
  await clickMenu('设备面板');
  await waitFor(`window.__PLUG.reqs.some(r => r.path === '/api/plugins/panel')`, 20, 150);
  const panel = (await reqs()).find(r => r.path === '/api/plugins/panel');
  check('面板请求发到本机且 serial 为 <电脑id>/<serial>', !!panel && !panel.peer && panel.body.serial === REMOTE_ID, panel && panel.url);
  check('面板按返回的控件树画出来', await waitFor(`(() => { const v = [...viewers.values()].find(x => x.dev.serial === ${JSON.stringify(REMOTE_DEV)}); return !!v.plugDock && v.plugDock.el.textContent.includes('PANEL ${REMOTE_ID}'); })()`, 20, 150));

  // 6. 性能浮层里的插件指标按设备标识拉取
  await ev(`(() => { const v = [...viewers.values()].find(x => x.dev.serial === ${JSON.stringify(REMOTE_DEV)}); v.togglePerf(); return 1; })()`);
  await waitFor(`window.__PLUG.reqs.some(r => r.path === '/api/plugins/metrics')`, 30, 200);
  const mreq = (await reqs()).find(r => r.path === '/api/plugins/metrics');
  check('指标请求发到本机且 serial 为 <电脑id>/<serial>', !!mreq && !mreq.peer && mreq.serial === REMOTE_ID, mreq && mreq.url);
  check('性能浮层显示插件指标', await waitFor(`(() => { const v = [...viewers.values()].find(x => x.dev.serial === ${JSON.stringify(REMOTE_DEV)}); return !!v.plugPerfBox && v.plugPerfBox.textContent.includes('PLUGMETRIC'); })()`, 30, 200));

  // 7. 徽标按设备标识匹配（远程设备的卡片上出现徽标）
  await ev(`renderSide(); 1`);
  await sleep(300);
  check('远程设备卡片显示插件徽标', (await ev(`(document.querySelector('.dev[title="${REMOTE_DEV}"] .plug-badge') || {}).textContent`)) === 'PLUG');

  // 8. 覆盖层：WS 事件的 serial 是插件设备标识，画到对应投屏窗口上
  await ev(`window.__PLUG.ws.onmessage({ data: JSON.stringify({ type: 'overlay', plugin: 'devmenu', serial: ${JSON.stringify(REMOTE_ID)}, shapes: [{ type: 'rect', x: 10, y: 10, w: 200, h: 100 }] }) }); 1`);
  await sleep(400);
  check('覆盖层按 <电脑id>/<serial> 存起来', (await ev(`(pluginOverlayMaps.get(${JSON.stringify(REMOTE_ID)}) || []).length`)) === 1);
  const lit = await ev(`(() => { const v = [...viewers.values()].find(x => x.dev.serial === ${JSON.stringify(REMOTE_DEV)}); if (!v || !v.pov || !v.pov.width) return -1; const d = v.pov.getContext('2d').getImageData(0, 0, v.pov.width, v.pov.height).data; let n = 0; for (let i = 3; i < d.length; i += 4) if (d[i]) n++; return n; })()`);
  check('覆盖层画在该设备的投屏画面上', lit > 0, String(lit));

  // 9. 日志高亮 / 应用菜单 / 文件菜单：远程设备同样适用
  check('日志高亮用的是本机贡献', (await ev(`(pluginLogHighlight('java.lang.NullReferenceException: boom', 'Unity') || {}).label`)) === 'PLUG');
  check('应用菜单对远程设备也生效（按包名过滤）', (await ev(`pluginMenuItems(hosts.get('pB'), 'app', { serial: ${JSON.stringify(REMOTE_DEV)}, package: 'com.example.game' }).length`)) === 1
    && (await ev(`pluginMenuItems(hosts.get('pB'), 'app', { serial: ${JSON.stringify(REMOTE_DEV)}, package: 'com.example.other' }).length`)) === 0);
  check('文件菜单对远程设备也生效（按扩展名过滤）', (await ev(`pluginMenuItems(hosts.get('pB'), 'file', { serial: ${JSON.stringify(REMOTE_DEV)}, path: '/sdcard/a.apk', isDir: false }).length`)) === 1
    && (await ev(`pluginMenuItems(hosts.get('pB'), 'file', { serial: ${JSON.stringify(REMOTE_DEV)}, path: '/sdcard/a.txt', isDir: false }).length`)) === 0);

  // 10. 设置 → 插件页只列本机插件（不再按电脑分组）
  await ev(`settingsDialog('plugins'); 1`);
  await waitFor(`document.querySelectorAll('.plugpane .plug-row').length > 0`);
  check('插件设置页只列本机插件', (await ev(`document.querySelectorAll('.plugpane .plug-row').length`)) === 1);
  check('插件设置页没有按电脑分组', (await ev(`document.querySelectorAll('.plugpane-grp').length`)) === 0);
  const listReqs = (await reqs()).filter(r => r.path === '/api/plugins');
  check('/api/plugins 请求发到本机', listReqs.length > 0 && listReqs.every(r => !r.peer));
  await ev(`document.querySelectorAll('dialog[open]').forEach(d => d.close()); 1`);

  console.log(errs.length ? 'page errors: ' + errs.join(' | ') : 'page errors: none');
  console.log(fails ? `\n${fails} 项失败` : '\n全部通过');
  ws.close(); p.kill(); srv.close();
  process.exit(fails ? 1 : 0);
})().catch(e => { console.error(e); p.kill(); srv.close(); process.exit(1); });
