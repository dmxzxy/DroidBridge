#!/usr/bin/env node
// 多人共享投屏回归测试：视频流上的 viewers / busy 消息（观看者标记、正在操作提示条、被拒绝时的提示）、
// wsUrl 里的 vid / who / force，以及侧栏设备行的“别人正在看”标记。
// 用法：node tools/test-viewers.js（需要 Chrome / Edge / Chromium；BROWSER 环境变量可指定路径）
const fs = require('fs');
const path = require('path');
const os = require('os');
process.chdir(path.resolve(__dirname, '..'));
const { spawn } = require('child_process');
const sleep = ms => new Promise(r => setTimeout(r, ms));
let html = fs.readFileSync('web/index.html', 'utf8');
// 模拟数据要放在 <meta charset> 之后、页面脚本之前（放在 <head> 开头会让浏览器按错误编码解析中文）
html = html.replace('<meta charset="utf-8">', () => `<meta charset="utf-8">\n<script>\n${fs.readFileSync('tools/demo/mock.js', 'utf8')}\n</script>`);
html = html.replace('<link rel="stylesheet" href="app.css">', () => `<style>\n${fs.readFileSync('web/app.css', 'utf8')}\n</style>`);
html = html.replace(/<script src="js\/([A-Za-z0-9_-]+)\.js"><\/script>/g, (m, n) => `<script>\n${fs.readFileSync('web/js/' + n + '.js', 'utf8')}\n</script>`);
const page = path.join(os.tmpdir(), 'db-viewers-test.html');
fs.writeFileSync(page, html);
const srv = require('http').createServer((q, r) => { r.setHeader('content-type', 'text/html; charset=utf-8'); r.end(fs.readFileSync(page)); }).listen(8895);
const browser = process.env.BROWSER || [
  '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable', '/usr/bin/chromium', '/usr/bin/chromium-browser',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
].find(f => fs.existsSync(f));
if (!browser) { console.error('未找到 Chrome / Edge / Chromium，可用 BROWSER 环境变量指定'); process.exit(2); }
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'db-viewers-'));
const p = spawn(browser, ['--headless=new', '--disable-gpu', '--no-sandbox', '--remote-debugging-port=9345', '--user-data-dir=' + profile, '--window-size=1440,900', 'about:blank']);
let fails = 0;
const check = (label, ok, detail) => { console.log((ok ? '✓ ' : '✗ ') + label + (detail ? '  ' + detail : '')); if (!ok) fails++; };
(async () => {
  let tabs;
  for (let i = 0; i < 240; i++) { try { tabs = await (await fetch('http://127.0.0.1:9345/json')).json(); if (tabs && tabs.some(t => t.type === 'page')) break; } catch {} await sleep(250); }
  if (!tabs || !tabs.some(t => t.type === 'page')) { console.error('浏览器没有启动（60 秒内未打开调试端口）'); p.kill(); process.exit(3); }
  const ws = new WebSocket(tabs.find(t => t.type === 'page').webSocketDebuggerUrl);
  await new Promise(r => ws.onopen = r);
  let id = 0; const pend = new Map(); const errs = [];
  ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); } if (m.method === 'Runtime.exceptionThrown') errs.push((m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text).slice(0, 200)); };
  const send = (method, params = {}) => new Promise(r => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
  const ev = async (x) => { const r = await send('Runtime.evaluate', { expression: x, awaitPromise: true, returnByValue: true }); return r.result.exceptionDetails ? 'EXC ' + (r.result.exceptionDetails.exception?.description || r.result.exceptionDetails.text) : r.result.result?.value; };
  /** 把一个后端消息交给第一个 Viewer，等同 WebSocket 收到一条文本消息 */
  const feed = (msg) => ev(`(() => { const v = [...viewers.values()][0]; v.onmessage = null; v.onText(${JSON.stringify(msg)}); return 1; })()`);
  const badge = () => ev(`(() => { const v = [...viewers.values()][0]; return JSON.stringify({ hidden: v.eyeEl.hidden, n: v.eyeNum.textContent, title: v.eyeEl.title }); })()`);
  const bar = () => ev(`(() => { const v = [...viewers.values()][0]; return JSON.stringify({ hidden: v.ctlBar.hidden, text: v.ctlBar.textContent }); })()`);
  const toasts = () => ev(`JSON.stringify([...document.querySelectorAll('#toasts .toast')].map(e => e.textContent))`);

  await send('Runtime.enable');
  await send('Page.navigate', { url: 'http://127.0.0.1:8895/?s=mview&lang=zh' });
  await sleep(500);
  for (let i = 0; i < 40; i++) { if ((await ev(`typeof viewers === 'object' && viewers.size > 0 && [...viewers.values()][0].gotFrame`)) === true) break; await sleep(250); }
  await sleep(400);

  // 1. wsUrl 带上 vid / who / force（force 只用于一次连接）
  const url1 = JSON.parse(await ev(`(() => { const v = [...viewers.values()][0]; v.vid = 'abc123def456'; v.forceNext = true; const u = new URL(v.wsUrl()); const again = new URL(v.wsUrl()); return JSON.stringify({ vid: u.searchParams.get('vid'), who: u.searchParams.get('who'), force: u.searchParams.get('force'), force2: again.searchParams.get('force'), vidLen: v.vid.length }); })()`));
  check('wsUrl 带 vid', url1.vid === 'abc123def456' && url1.vidLen === 12, JSON.stringify(url1));
  check('wsUrl 带 who（非空）', typeof url1.who === 'string' && url1.who.length > 0 && url1.who !== 'null', url1.who);
  check('forceNext 只用一次（force=1 然后 0）', url1.force === '1' && url1.force2 === '0', JSON.stringify([url1.force, url1.force2]));

  // 2. 演示数据里的 viewers 消息：眼睛 + 人数、正在操作提示条
  let b = JSON.parse(await badge());
  check('观看者标记显示人数', b.hidden === false && b.n === '2', JSON.stringify(b));
  check('悬停提示列出名称并标出“我”', b.title.includes('（我）') && b.title.includes('小王'), b.title);
  let c = JSON.parse(await bar());
  check('别人操作时显示提示条', c.hidden === false && c.text.includes('小王'), JSON.stringify(c));

  // 3. 控制者是自己 / AI 时不显示提示条
  await feed({ type: 'viewers', you: 'abc123def456', list: [{ id: 'abc123def456', name: '我' }, { id: 'v9', name: '小林' }], controller: 'abc123def456', controller_name: '我' });
  c = JSON.parse(await bar());
  check('自己操作时不显示提示条', c.hidden === true, JSON.stringify(c));
  await feed({ type: 'viewers', you: 'abc123def456', list: [{ id: 'abc123def456', name: '我' }, { id: 'v9', name: '小林' }], controller: 'ai', controller_name: 'AI' });
  c = JSON.parse(await bar());
  check('AI 操作时不显示提示条', c.hidden === true, JSON.stringify(c));
  b = JSON.parse(await badge());
  check('人数变成 2（小林加入）', b.hidden === false && b.n === '2' && b.title.includes('小林'), JSON.stringify(b));

  // 4. 只剩自己时隐藏标记
  await feed({ type: 'viewers', you: 'abc123def456', list: [{ id: 'abc123def456', name: '我' }], controller: null, controller_name: null });
  b = JSON.parse(await badge());
  c = JSON.parse(await bar());
  check('只有自己时隐藏观看者标记', b.hidden === true, JSON.stringify(b));
  check('没有控制者时隐藏提示条', c.hidden === true, JSON.stringify(c));

  // 5. busy：提示“正在操作，请稍候”，同一 Viewer 3 秒内只提示一次
  await ev(`document.querySelector('#toasts').replaceChildren(); [...viewers.values()][0].busyAt = 0; 1`);
  await feed({ type: 'busy', by: '小王' });
  await sleep(150);
  let ts = JSON.parse(await toasts());
  check('被拒绝时提示「小王」正在操作', ts.length === 1 && ts[0].includes('小王'), JSON.stringify(ts));
  await feed({ type: 'busy', by: '小王' });
  await sleep(150);
  ts = JSON.parse(await toasts());
  check('3 秒内不重复提示', ts.length === 1, JSON.stringify(ts));
  await ev(`[...viewers.values()][0].busyAt = 0; 1`);
  await feed({ type: 'busy', by: '小林' });
  await sleep(150);
  ts = JSON.parse(await toasts());
  check('间隔足够后再次提示', ts.length === 2 && ts.some(x => x.includes('小林')), JSON.stringify(ts));

  // 6. 侧栏设备行：别人也在看这个设备
  const watch = await ev(`(() => { const w = document.querySelector('.dev .watch'); return w ? JSON.stringify({ n: w.textContent.trim(), title: w.title }) : ''; })()`);
  const w = watch ? JSON.parse(watch) : null;
  check('设备行显示眼睛 + 别人的人数', w && w.n === '1', watch);
  check('设备行提示列出名称和操作者', w && w.title.includes('小王') && w.title.includes('正在操作'), watch);

  console.log(errs.length ? 'page errors: ' + errs.join(' | ') : 'page errors: none');
  console.log(fails ? `\n${fails} 项失败` : '\n全部通过');
  ws.close(); p.kill(); srv.close(); process.exit(fails ? 1 : 0);
})().catch(e => { console.error(e); p.kill(); process.exit(1); });
