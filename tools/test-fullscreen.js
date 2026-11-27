#!/usr/bin/env node
// 全屏排版与导航胶囊回归测试：全屏时导航条贴底、画面用满剩下的高度（窗口很高、画面是横屏时也一样）；
// 触屏和窄窗口上导航键是画面下沿的小胶囊：碰到画面时展开、停手 3 秒后收回、收起时点它只展开，按键照常发给设备。
// 无头浏览器进不了真正的全屏，所以把样式里的 :fullscreen 规则照搬到一个测试类上，再按浏览器默认样式把查看器钉满窗口。
// 用法：node tools/test-fullscreen.js（需要 Chrome / Edge / Chromium；BROWSER 环境变量可指定路径）
const fs = require('fs');
const path = require('path');
const os = require('os');
process.chdir(path.resolve(__dirname, '..'));
const { spawn } = require('child_process');
const sleep = ms => new Promise(r => setTimeout(r, ms));
let html = fs.readFileSync('web/index.html', 'utf8');
// 模拟数据要放在 <meta charset> 之后、页面脚本之前（放在 <head> 开头会让浏览器按错误编码解析中文）
html = html.replace('<meta charset="utf-8">', () => `<meta charset="utf-8">\n<script>\n${fs.readFileSync('tools/demo/mock.js', 'utf8')}\n</script>`);
// 地址带 mouse=1 时把“能不能悬停”一律当成能：没有鼠标的电脑（比如无头 Linux）上界面按触屏排布，桌面布局就测不到
html = html.replace('<meta charset="utf-8">', () => `<meta charset="utf-8">\n<script>
(function () {
  if (!/[?&]mouse=1/.test(location.search)) return;
  const orig = window.matchMedia;
  window.matchMedia = (q) => /hover:\\s*none/.test(q) ? { matches: false, media: q } : orig(q);
})();
</script>`);
html = html.replace('<link rel="stylesheet" href="app.css">', () => `<style>\n${fs.readFileSync('web/app.css', 'utf8')}\n</style>`);
html = html.replace(/<script src="js\/([A-Za-z0-9_-]+)\.js"><\/script>/g, (m, n) => `<script>\n${fs.readFileSync('web/js/' + n + '.js', 'utf8')}\n</script>`);
const page = path.join(os.tmpdir(), 'db-fullscreen-test.html');
fs.writeFileSync(page, html);
const srv = require('http').createServer((q, r) => { r.setHeader('content-type', 'text/html; charset=utf-8'); r.end(fs.readFileSync(page)); }).listen(8896);
// 浏览器：环境变量 BROWSER，或常见的 Chrome / Edge / Chromium 位置
const browser = process.env.BROWSER || [
  '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable', '/usr/bin/chromium', '/usr/bin/chromium-browser',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
].find(f => fs.existsSync(f));
if (!browser) { console.error('未找到 Chrome / Edge / Chromium，可用 BROWSER 环境变量指定'); process.exit(2); }
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'db-fullscreen-'));
const p = spawn(browser, ['--headless=new', '--disable-gpu', '--no-sandbox', '--remote-debugging-port=9346', '--user-data-dir=' + profile, '--window-size=1440,900', 'about:blank']);
let fails = 0;
const check = (label, ok, detail) => { console.log((ok ? '✓ ' : '✗ ') + label + (detail ? '  ' + detail : '')); if (!ok) fails++; };
(async () => {
  let tabs;
  // 浏览器冷启动可能较慢（CI 上可能超过 10 秒），最多等 60 秒
  for (let i = 0; i < 240; i++) { try { tabs = await (await fetch('http://127.0.0.1:9346/json')).json(); if (tabs && tabs.some(t => t.type === 'page')) break; } catch {} await sleep(250); }
  if (!tabs || !tabs.some(t => t.type === 'page')) { console.error('浏览器没有启动（60 秒内未打开调试端口）'); p.kill(); process.exit(3); }
  const ws = new WebSocket(tabs.find(t => t.type === 'page').webSocketDebuggerUrl);
  await new Promise(r => ws.onopen = r);
  let id = 0; const pend = new Map(); const errs = [];
  ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); } if (m.method === 'Runtime.exceptionThrown') errs.push((m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text).slice(0, 200)); };
  const send = (method, params = {}) => new Promise(r => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
  const ev = async (x) => { const r = await send('Runtime.evaluate', { expression: x, awaitPromise: true, returnByValue: true }); return r.result.exceptionDetails ? 'EXC ' + (r.result.exceptionDetails.exception?.description || r.result.exceptionDetails.text) : r.result.result?.value; };
  const V = '[...viewers.values()][0]';
  /** 按指定窗口大小重新打开页面；touch 为真时模拟触屏（手机、平板），否则模拟有鼠标的电脑 */
  const load = async (w, h, touch) => {
    await send('Emulation.setDeviceMetricsOverride', { width: w, height: h, deviceScaleFactor: 1, mobile: touch });
    await send('Emulation.setTouchEmulationEnabled', { enabled: touch, maxTouchPoints: 5 });
    await send('Page.navigate', { url: `http://127.0.0.1:8896/fullscreen.html?s=mview&lang=zh${touch ? '' : '&mouse=1'}` });
    await sleep(500);
    for (let i = 0; i < 40; i++) { if ((await ev(`typeof viewers === 'object' && viewers.size > 0 && ${V}.gotFrame`)) === true) break; await sleep(250); }
    await sleep(400);
  };
  /** 把画面改成指定尺寸（拉伸演示画面） */
  const frame = (w, h) => ev(`(() => { const v = ${V}; v.canvas.width = ${w}; v.canvas.height = ${h}; v.vw = ${w}; v.vh = ${h}; v.fit(); return 1; })()`);
  // 照搬出来的全屏样式：:fullscreen 换成测试类，外加浏览器默认给全屏元素的定位
  const mirror = '.viewer.test-fs{position:fixed;left:0;top:0;width:100%;height:100%;margin:0;z-index:9999}\n' + fs.readFileSync('web/app.css', 'utf8').replace(/\.viewer:fullscreen/g, '.viewer.test-fs');
  const full = (on) => on
    ? ev(`(() => { const st = document.createElement('style'); st.id = 'test-fs'; st.textContent = ${JSON.stringify(mirror)}; document.head.append(st); ${V}.root.classList.add('test-fs'); return 1; })()`)
    : ev(`(() => { document.getElementById('test-fs').remove(); ${V}.root.classList.remove('test-fs'); return 1; })()`);
  /** [left, top, width, height] */
  const rect = async (sel) => JSON.parse(await ev(`(() => { const v = ${V}; const b = (v.root.matches('${sel}') ? v.root : v.root.querySelector('${sel}')).getBoundingClientRect(); return JSON.stringify([Math.round(b.left), Math.round(b.top), Math.round(b.width), Math.round(b.height)]); })()`));
  const isOpen = () => ev(`${V}.nav.classList.contains('open')`);
  const keys = async () => { const s = await ev(`JSON.stringify((${V}.ws.sent || []).map(s => JSON.parse(s)).filter(m => m.t === 'key').map(m => m.action + ':' + m.code))`); return JSON.parse(s).join(','); };
  const reset = () => ev(`${V}.ws.sent = []; 1`);
  const tap = async (x, y) => {
    await send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] });
    await send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  };
  const center = (r) => ({ x: r[0] + r[2] / 2, y: r[1] + r[3] / 2 });

  await send('Runtime.enable');

  // ---- 桌面：导航键是画面下方的一行 ----
  await load(1440, 900, false);
  // 样式表出错时（比如花括号不配对）全局 box-sizing 会丢掉，页面凭空多出滚动条
  const box = await ev(`(() => { const d = document.documentElement; return getComputedStyle(document.body).boxSizing + ' ' + (d.scrollHeight <= innerHeight && d.scrollWidth <= innerWidth) + ' ' + d.scrollWidth + 'x' + d.scrollHeight; })()`);
  check('页面没有多余的滚动条（全局 box-sizing 生效）', box.startsWith('border-box true'), box);
  check('桌面：导航键是画面下方的一行', (await ev(`${V}.root.classList.contains('nav-pill')`)) === false && (await ev(`${V}.nav.previousElementSibling.classList.contains('screen')`)) === true);
  await frame(720, 1600);
  await full(true);
  let nav = await rect('.nav'), cv = await rect('canvas');
  check('全屏（竖屏画面）：导航条贴底，画面高度用满', nav[1] + nav[3] === 900 && cv[3] === 860 && cv[1] === 0, `nav ${nav} canvas ${cv}`);
  await frame(1600, 720);
  nav = await rect('.nav'); cv = await rect('canvas');
  check('全屏（横屏画面）：画面在导航条上方居中', nav[1] + nav[3] === 900 && cv[1] + cv[3] <= nav[1] && Math.abs(cv[1] - (nav[1] - cv[3]) / 2) <= 1, `nav ${nav} canvas ${cv}`);
  await full(false);
  await reset();
  const bk = center(await rect('.nav .ib:first-child'));
  await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: bk.x, y: bk.y, button: 'left', clickCount: 1 });
  await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: bk.x, y: bk.y, button: 'left', clickCount: 1 });
  await sleep(150);
  check('桌面：点「返回」发出返回键', (await keys()) === '0:4,1:4', await keys());

  // ---- 很高的窗口（竖屏）+ 接近方形的画面：以前整块内容被垂直居中，导航条浮在屏幕中间 ----
  await load(1088, 1894, false);
  await frame(1088, 1291);
  await full(true);
  nav = await rect('.nav'); cv = await rect('canvas');
  check('全屏（竖屏窗口）：导航条贴底，画面在它上方居中', nav[1] + nav[3] === 1894 && cv[1] + cv[3] <= nav[1] && Math.abs(cv[1] - (nav[1] - cv[3]) / 2) <= 1, `nav ${nav} canvas ${cv}`);
  await full(false);

  // ---- 触屏：导航键是画面下沿的小胶囊 ----
  await load(390, 844, true);
  check('触屏：导航键是画面下方留白里的小胶囊，默认收起', (await ev(`${V}.root.classList.contains('nav-pill') && ${V}.nav.previousElementSibling.classList.contains('screen')`)) === true && (await isOpen()) === false);
  cv = await rect('canvas'); nav = await rect('.nav');
  const scr = await rect('.screen');
  check('触屏：胶囊在画面下方的留白里，不遮住画面', nav[1] === scr[1] + scr[3] && cv[1] + cv[3] <= nav[1] && nav[3] === 40, `canvas ${cv} nav ${nav} screen ${scr}`);
  await frame(1600, 720);
  await full(true);
  cv = await rect('canvas'); nav = await rect('.nav');
  check('触屏全屏（横屏画面）：画面居中，胶囊贴底', nav[1] + nav[3] === 844 && Math.abs(cv[1] - (nav[1] - cv[3]) / 2) <= 1, `canvas ${cv} nav ${nav}`);
  await full(false);
  await frame(720, 1600);

  await reset();
  const c = center(await rect('canvas'));
  await tap(c.x, c.y);
  await sleep(200);
  check('触屏：碰到画面时展开', (await isOpen()) === true);
  await sleep(3300);
  check('触屏：停手 3 秒后收回', (await isOpen()) === false);
  await reset();
  await tap(...Object.values(center(await rect('.nav'))));
  await sleep(250);
  check('触屏：点收起的胶囊只展开，不发按键', (await isOpen()) === true && (await keys()) === '', await keys());
  await tap(...Object.values(center(await rect('.nav .ib:first-child'))));
  await sleep(200);
  check('触屏：展开后点「返回」发出返回键', (await keys()) === '0:4,1:4', await keys());

  // ---- 窄窗口（没有触屏）也用胶囊；横过来的手机、平板窗口很宽，靠的是触屏 ----
  await load(600, 900, false);
  check('窄窗口：同样用胶囊', (await ev(`${V}.root.classList.contains('nav-pill')`)) === true);
  await load(915, 390, true);
  check('横过来的手机、平板（宽屏触屏）：同样用胶囊', (await ev(`${V}.root.classList.contains('nav-pill')`)) === true);

  console.log(errs.length ? 'page errors: ' + errs.join(' | ') : 'page errors: none');
  console.log(fails ? `\n${fails} 项失败` : '\n全部通过');
  ws.close(); p.kill(); srv.close(); process.exit(fails ? 1 : 0);
})().catch(e => { console.error(e); p.kill(); process.exit(1); });
