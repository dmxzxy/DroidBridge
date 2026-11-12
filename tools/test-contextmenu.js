#!/usr/bin/env node
// 右键菜单回归测试：空白处、设备画面（= 返回键）、输入框（剪切/复制/粘贴/全部选择）、选中的文字、对话框内。
// 用法：node tools/test-contextmenu.js（需要 Chrome / Edge / Chromium；BROWSER 环境变量可指定路径）
const fs = require('fs');
const path = require('path');
const os = require('os');
process.chdir(path.resolve(__dirname, '..'));
const { spawn } = require('child_process');
const sleep = ms => new Promise(r => setTimeout(r, ms));
let html = fs.readFileSync('web/index.html', 'utf8');
html = html.replace('<meta charset="utf-8">', () => `<meta charset="utf-8">\n<script>\n${fs.readFileSync('tools/demo/mock.js', 'utf8')}\n</script>`);
html = html.replace('<link rel="stylesheet" href="app.css">', () => `<style>\n${fs.readFileSync('web/app.css', 'utf8')}\n</style>`);
html = html.replace(/<script src="js\/([A-Za-z0-9_-]+)\.js"><\/script>/g, (m, n) => `<script>\n${fs.readFileSync('web/js/' + n + '.js', 'utf8')}\n</script>`);
const page = path.join(os.tmpdir(), 'db-ctx.html');
fs.writeFileSync(page, html);
const srv = require('http').createServer((q, r) => { r.setHeader('content-type', 'text/html; charset=utf-8'); r.end(fs.readFileSync(page)); }).listen(8892);
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'db-ctx-'));
const browser = process.env.BROWSER || [
  '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable', '/usr/bin/chromium', '/usr/bin/chromium-browser',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
].find(f => fs.existsSync(f));
if (!browser) { console.error('未找到 Chrome / Edge / Chromium，可用 BROWSER 环境变量指定'); process.exit(2); }
const p = spawn(browser, ['--headless=new', '--no-sandbox', '--disable-gpu', '--remote-debugging-port=9342', '--user-data-dir=' + profile, '--window-size=1440,900', 'about:blank']);
let fails = 0;
const check = (l, ok, d) => { console.log((ok ? '✓ ' : '✗ ') + l + (d ? '  ' + d : '')); if (!ok) fails++; };
(async () => {
  let tabs;
  for (let i = 0; i < 40; i++) { try { tabs = await (await fetch('http://127.0.0.1:9342/json')).json(); if (tabs.length) break; } catch {} await sleep(300); }
  const ws = new WebSocket(tabs.find(t => t.type === 'page').webSocketDebuggerUrl);
  await new Promise(r => ws.onopen = r);
  let id = 0; const pend = new Map(); const errs = [];
  ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); } if (m.method === 'Runtime.exceptionThrown') errs.push((m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text).slice(0, 200)); };
  const send = (method, params = {}) => new Promise(r => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
  const ev = async (x) => { const r = await send('Runtime.evaluate', { expression: x, awaitPromise: true, returnByValue: true }); return r.result.exceptionDetails ? 'EXC ' + (r.result.exceptionDetails.exception?.description || r.result.exceptionDetails.text) : r.result.result?.value; };
  const rclick = async (x, y) => {
    await send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'right', clickCount: 1 });
    await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'right', clickCount: 1 });
    await sleep(250);
  };
  const lclick = async (x, y) => {
    await send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', clickCount: 1 });
    await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'left', clickCount: 1 });
    await sleep(250);
  };
  const menuItems = () => ev(`(() => { const m = document.querySelector('.menu'); return m ? [...m.querySelectorAll('button')].map(b => b.textContent + (b.disabled ? '(禁用)' : '')).join('/') : ''; })()`);
  const center = async (sel) => JSON.parse(await ev(`JSON.stringify((() => { const r = document.querySelector(${JSON.stringify(sel)}).getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })())`));
  await send('Runtime.enable');
  await send('Browser.grantPermissions', { permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite'], origin: 'http://127.0.0.1:8892' }).catch(() => {});

  await send('Page.navigate', { url: 'http://127.0.0.1:8892/?s=mview&lang=zh' });
  await sleep(2600);
  // 1. 空白处：不出浏览器菜单，也不出我们的菜单
  const prevented = await ev(`(() => { let p = null; const h = e => { p = e.defaultPrevented; }; addEventListener('contextmenu', h); const ev = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 700, clientY: 850 }); document.querySelector('main').dispatchEvent(ev); removeEventListener('contextmenu', h); return ev.defaultPrevented; })()`);
  check('空白处右键：屏蔽浏览器菜单', prevented === true);
  await rclick(1300, 850);
  check('空白处右键：不弹菜单', (await menuItems()) === '', await menuItems());

  // 2. 设备画面：右键仍然是“返回”
  await ev(`[...viewers.values()][0].ws.sent = []; 1`);
  const c = await center('.screen canvas');
  await rclick(c.x, c.y);
  const sent = await ev(`JSON.stringify(([...viewers.values()][0].ws.sent || []).map(s => JSON.parse(s)).filter(m => m.t === 'back'))`);
  check('设备画面右键 = 返回键', JSON.parse(sent).length === 2, sent);
  check('设备画面右键不弹菜单', (await menuItems()) === '');

  // 3. 输入框：在设置窗口的“截图保存位置”之外找一个文本框——用“输入文字”框
  await ev(`[...viewers.values()][0].kbBtn.click(); 1`);
  await sleep(200);
  await ev(`(() => { const i = [...viewers.values()][0].textInput; i.value = 'hello world'; i.focus(); i.setSelectionRange(0, 5); return 1; })()`);
  const ti = await center('.typing input');
  await rclick(ti.x, ti.y);
  let items = await menuItems();
  check('输入框右键：剪切/复制/粘贴/全部选择', items === '剪切/复制/粘贴/全部选择', items);
  // 复制
  await ev(`[...document.querySelectorAll('.menu button')].find(b => b.textContent === '复制').click(); 1`);
  await sleep(200);
  const clip = await ev(`navigator.clipboard.readText().catch(e => 'ERR ' + e.message)`);
  check('复制选中文字', clip === 'hello', clip);
  // 剪切
  await ev(`(() => { const i = [...viewers.values()][0].textInput; i.focus(); i.setSelectionRange(6, 11); return 1; })()`);
  await rclick(ti.x, ti.y);
  await ev(`[...document.querySelectorAll('.menu button')].find(b => b.textContent === '剪切').click(); 1`);
  await sleep(300);
  check('剪切：文字被删掉', (await ev(`[...viewers.values()][0].textInput.value`)) === 'hello ', JSON.stringify(await ev(`[...viewers.values()][0].textInput.value`)));
  check('剪切：剪贴板里是被剪的文字', (await ev(`navigator.clipboard.readText().catch(e => 'ERR')`)) === 'world');
  // 粘贴（光标在末尾）
  await ev(`(() => { const i = [...viewers.values()][0].textInput; i.focus(); i.setSelectionRange(6, 6); return 1; })()`);
  await rclick(ti.x, ti.y);
  await ev(`[...document.querySelectorAll('.menu button')].find(b => b.textContent === '粘贴').click(); 1`);
  await sleep(300);
  check('粘贴到光标处', (await ev(`[...viewers.values()][0].textInput.value`)) === 'hello world', JSON.stringify(await ev(`[...viewers.values()][0].textInput.value`)));
  // 没选中时剪切/复制禁用
  await ev(`(() => { const i = [...viewers.values()][0].textInput; i.focus(); i.setSelectionRange(3, 3); return 1; })()`);
  await rclick(ti.x, ti.y);
  items = await menuItems();
  check('未选中时剪切/复制是禁用的', items === '剪切(禁用)/复制(禁用)/粘贴/全部选择', items);
  // 全部选择
  await ev(`[...document.querySelectorAll('.menu button')].find(b => b.textContent === '全部选择').click(); 1`);
  await sleep(200);
  check('全部选择', (await ev(`(() => { const i = [...viewers.values()][0].textInput; return i.selectionStart + '-' + i.selectionEnd; })()`)) === '0-11');
  // 菜单在鼠标位置
  await rclick(ti.x, ti.y);
  const pos = await ev(`(() => { const r = document.querySelector('.menu').getBoundingClientRect(); return Math.round(r.left) + ',' + Math.round(r.top); })()`);
  check('菜单出现在鼠标位置', Math.abs(+pos.split(',')[0] - Math.round(ti.x)) <= 2 || +pos.split(',')[0] < ti.x, `鼠标 ${Math.round(ti.x)},${Math.round(ti.y)} 菜单 ${pos}`);
  await send('Input.dispatchKeyEvent', { type: 'rawKeyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
  await send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
  await sleep(200);
  check('Esc 关闭菜单', (await menuItems()) === '');

  // 4. 页面上选中的文字（侧栏设备名）：只有“复制”
  await ev(`(() => { const b = document.querySelector('.vhead .name'); b.style.userSelect = 'text'; const r = document.createRange(); r.selectNodeContents(b); const s = getSelection(); s.removeAllRanges(); s.addRange(r); window.__selTarget = b; return b.textContent; })()`);
  const dv = JSON.parse(await ev(`JSON.stringify((() => { const r = window.__selTarget.getBoundingClientRect(); return { x: r.left + Math.min(20, r.width / 2), y: r.top + r.height / 2 }; })())`));
  await rclick(dv.x, dv.y);
  items = await menuItems();
  check('选中页面文字后右键：只有复制', items === '复制', items);
  await lclick(1300, 850);

  // 5. 对话框里的输入框（设置 → 修改保存位置 弹窗里的输入框）
  await ev(`promptDialog('测试', null, { value: 'abc' }); 1`);
  await sleep(300);
  const di = await center('dialog[open] input');
  await rclick(di.x, di.y);
  items = await menuItems();
  check('对话框里的输入框也有菜单', items.startsWith('剪切'), items);
  check('菜单显示在对话框上层', (await ev(`(() => { const m = document.querySelector('.menu'); const r = m.getBoundingClientRect(); return m.contains(document.elementFromPoint(r.left + 10, r.top + 10)); })()`)) === true);

  // 6. 对话框不出现横向滚动：设置页里很长的路径 / 地址
  await ev(`document.querySelectorAll('dialog[open]').forEach(d => d.close()); 1`);
  await ev(`(() => { const f = window.fetch; window.fetch = async (u, o) => String(u).includes('/api/settings') ? new Response(JSON.stringify({ media_dir: 'C:\\Users\\someone-with-a-long-name\\AppData\\Local\\Programs\\DroidBridge\\data\\media\\screenshots' }), { headers: { 'content-type': 'application/json' } }) : f(u, o); return 1; })()`);
  await ev(`settingsDialog(); 1`);
  await sleep(600);
  const ov = await ev(`(() => { const b = document.querySelector('dialog.settings .dlg-b'); return b.clientWidth + '/' + b.scrollWidth; })()`);
  const [cw, sw] = ov.split('/').map(Number);
  check('设置页没有横向滚动（长路径）', sw <= cw + 1, ov);
  await ev(`document.querySelector('dialog.settings').close(); 1`);

  console.log(errs.length ? 'page errors: ' + errs.join(' | ') : 'page errors: none');
  console.log(fails ? `\n${fails} 项失败` : '\n全部通过');
  ws.close(); p.kill(); srv.close(); process.exit(fails ? 1 : 0);
})().catch(e => { console.error(e); p.kill(); process.exit(1); });
