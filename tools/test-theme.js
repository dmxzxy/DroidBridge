#!/usr/bin/env node
// 外观（主题）回归测试：跟随系统时系统的深浅色一变界面立刻跟着变；设了深色 / 浅色就不再跟系统；
// 选择记在浏览器里，重新打开还在；插件网页面板也拿到当前外观。
// 用法：node tools/test-theme.js（需要 Chrome / Edge / Chromium；BROWSER 环境变量可指定路径）
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
const page = path.join(os.tmpdir(), 'db-theme-test.html');
fs.writeFileSync(page, html);
const srv = require('http').createServer((q, r) => { r.setHeader('content-type', 'text/html; charset=utf-8'); r.end(fs.readFileSync(page)); }).listen(8897);
// 浏览器：环境变量 BROWSER，或常见的 Chrome / Edge / Chromium 位置
const browser = process.env.BROWSER || [
  '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable', '/usr/bin/chromium', '/usr/bin/chromium-browser',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
].find(f => fs.existsSync(f));
if (!browser) { console.error('未找到 Chrome / Edge / Chromium，可用 BROWSER 环境变量指定'); process.exit(2); }
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'db-theme-'));
const p = spawn(browser, ['--headless=new', '--disable-gpu', '--no-sandbox', '--remote-debugging-port=9347', '--user-data-dir=' + profile, '--window-size=1440,900', 'about:blank']);
let fails = 0;
const check = (label, ok, detail) => { console.log((ok ? '✓ ' : '✗ ') + label + (detail ? '  ' + detail : '')); if (!ok) fails++; };
(async () => {
  let tabs;
  // 浏览器冷启动可能较慢（CI 上可能超过 10 秒），最多等 60 秒
  for (let i = 0; i < 240; i++) { try { tabs = await (await fetch('http://127.0.0.1:9347/json')).json(); if (tabs && tabs.some(t => t.type === 'page')) break; } catch {} await sleep(250); }
  if (!tabs || !tabs.some(t => t.type === 'page')) { console.error('浏览器没有启动（60 秒内未打开调试端口）'); p.kill(); process.exit(3); }
  const ws = new WebSocket(tabs.find(t => t.type === 'page').webSocketDebuggerUrl);
  await new Promise(r => ws.onopen = r);
  let id = 0; const pend = new Map(); const errs = [];
  ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); } if (m.method === 'Runtime.exceptionThrown') errs.push((m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text).slice(0, 200)); };
  const send = (method, params = {}) => new Promise(r => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
  const ev = async (x) => { const r = await send('Runtime.evaluate', { expression: x, awaitPromise: true, returnByValue: true }); return r.result.exceptionDetails ? 'EXC ' + (r.result.exceptionDetails.exception?.description || r.result.exceptionDetails.text) : r.result.result?.value; };
  await send('Runtime.enable');
  await send('Emulation.setDeviceMetricsOverride', { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
  /** 系统的深浅色 */
  const system = (scheme) => send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-color-scheme', value: scheme }] });
  const load = async () => {
    await send('Page.navigate', { url: 'http://127.0.0.1:8897/theme.html?s=mlist&lang=zh' });
    await sleep(500);
    for (let i = 0; i < 40; i++) { if ((await ev('typeof setTheme === "function" && typeof pluginTheme === "function"')) === true) break; await sleep(250); }
    await sleep(500);
  };
  /** 当前外观：[data-theme, 背景 --bg] */
  const look = async () => JSON.parse(await ev(`JSON.stringify([document.documentElement.dataset.theme, getComputedStyle(document.documentElement).getPropertyValue('--bg').trim()])`));
  const DARK = ['dark', '#0e1013'], LIGHT = ['light', '#f6f7f9'];
  /** 在设置里选外观 */
  const choose = async (v) => {
    await ev(`(() => { if (!document.querySelector('dialog.settings')) settingsDialog(); const s = document.querySelector('dialog.settings select[aria-label="外观"]'); s.value = '${v}'; s.dispatchEvent(new Event('change')); document.querySelector('dialog.settings').close(); return 1; })()`);
    await sleep(150);
  };

  // 1. 没设置过：跟随系统，系统一变界面立刻跟着变
  await system('dark'); await load();
  check('默认跟随系统（系统是深色）', JSON.stringify(await look()) === JSON.stringify(DARK), JSON.stringify(await look()));
  await system('light'); await sleep(200);
  check('系统切到浅色，界面立刻变浅', JSON.stringify(await look()) === JSON.stringify(LIGHT), JSON.stringify(await look()));
  await system('dark'); await sleep(200);
  check('系统切回深色，界面立刻变深', JSON.stringify(await look()) === JSON.stringify(DARK), JSON.stringify(await look()));

  // 2. 设了深色 / 浅色就不再跟系统
  await choose('light');
  check('设置里选“浅色”立即生效', JSON.stringify(await look()) === JSON.stringify(LIGHT) && (await ev(`localStorage.getItem('db_theme')`)) === 'light', JSON.stringify(await look()));
  await system('light'); await system('dark'); await sleep(200);
  check('选了浅色后，系统变深也不跟着变', JSON.stringify(await look()) === JSON.stringify(LIGHT), JSON.stringify(await look()));
  await choose('dark'); await system('light'); await sleep(200);
  check('选了深色后，系统变浅也不跟着变', JSON.stringify(await look()) === JSON.stringify(DARK), JSON.stringify(await look()));
  await choose('auto'); await sleep(200);
  check('改回“跟随系统”立即生效（系统是浅色）', JSON.stringify(await look()) === JSON.stringify(LIGHT), JSON.stringify(await look()));
  await system('dark'); await sleep(200);
  check('改回“跟随系统”后，系统再变就跟着变', JSON.stringify(await look()) === JSON.stringify(DARK), JSON.stringify(await look()));

  // 3. 选择记在浏览器里：重新打开还在
  await choose('light'); await system('dark'); await load();
  check('选了浅色后重新打开页面还是浅色', JSON.stringify(await look()) === JSON.stringify(LIGHT), JSON.stringify(await look()));
  await ev(`localStorage.removeItem('db_theme'); 1`);

  // 4. 插件网页面板：地址里带着外观，变化时收到通知，SDK 把它写到 <html data-theme>
  check('面板地址带当前外观', (await ev(`pluginWebSrc({ web: '/plugin-web/demo/index.html' })`)) === '/plugin-web/demo/index.html?theme=light'
    && (await ev(`pluginWebSrc({ web: '/plugin-web/demo/index.html?a=1' })`)) === '/plugin-web/demo/index.html?a=1&theme=light'
    && (await ev(`pluginWebSrc({ web: 'https://x/' })`)) === '');
  await ev(`(() => { window.__posted = []; pluginWebCtx.add({ frame: { contentWindow: { postMessage: m => window.__posted.push(m) } } }); setTheme('dark'); return 1; })()`);
  const posted = JSON.parse(await ev('JSON.stringify(window.__posted)'));
  check('外观变化时通知插件面板', posted.length === 1 && posted[0].t === 'theme' && posted[0].theme === 'dark', JSON.stringify(posted));
  // SDK 装进一个同源的 iframe 里（和插件面板一样是独立的页面）
  const sdkSrc = fs.readFileSync('web/plugin-sdk.js', 'utf8').replace(/<\/script/gi, '<\\/script');
  const made = await ev(`(() => { const f = document.createElement('iframe'); f.id = 'sdktest'; f.srcdoc = ${JSON.stringify('<!doctype html><html><head><script>' + sdkSrc + '</script></head><body></body></html>')}; document.body.append(f); return 1; })()`);
  await sleep(400);
  await ev(`document.getElementById('sdktest').contentWindow.postMessage({ __droidbridge: 1, t: 'init', device: { serial: '' }, lang: 'zh', theme: 'light' }, '*'); 1`);
  await sleep(150);
  let sdk = JSON.parse(await ev(`JSON.stringify((() => { const w = document.getElementById('sdktest').contentWindow; return [w.droidbridge.theme, w.document.documentElement.dataset.theme, w.droidbridge.lang]; })())`));
  check('SDK：init 里的外观写到 <html data-theme>', made === 1 && JSON.stringify(sdk) === JSON.stringify(['light', 'light', 'zh']), made + ' ' + JSON.stringify(sdk));
  await ev(`document.getElementById('sdktest').contentWindow.postMessage({ __droidbridge: 1, t: 'theme', theme: 'dark' }, '*'); 1`);
  await sleep(150);
  sdk = JSON.parse(await ev(`JSON.stringify((() => { const w = document.getElementById('sdktest').contentWindow; return [w.droidbridge.theme, w.document.documentElement.dataset.theme]; })())`));
  check('SDK：外观变化的消息同样生效', JSON.stringify(sdk) === JSON.stringify(['dark', 'dark']), JSON.stringify(sdk));

  console.log(errs.length ? 'page errors: ' + errs.join(' | ') : 'page errors: none');
  console.log(fails ? `\n${fails} 项失败` : '\n全部通过');
  ws.close(); p.kill(); srv.close(); process.exit(fails ? 1 : 0);
})().catch(e => { console.error(e); p.kill(); process.exit(1); });
