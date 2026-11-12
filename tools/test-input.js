#!/usr/bin/env node
// 键盘与输入法回归测试：在无头浏览器里加载界面（模拟数据），用 CDP 的 Input 域发真实的鼠标点击、
// 按键和输入法组词，检查焦点和发给设备的控制消息。
// 用法：node tools/test-input.js（需要 Chrome / Edge / Chromium；BROWSER 环境变量可指定路径）
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
const page = path.join(os.tmpdir(), 'db-input-test.html');
fs.writeFileSync(page, html);
const srv = require('http').createServer((q, r) => { r.setHeader('content-type', 'text/html; charset=utf-8'); r.end(fs.readFileSync(page)); }).listen(8894);
const edge = 'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe';
const p = spawn(edge, ['--headless=new', '--disable-gpu', '--remote-debugging-port=9339', '--user-data-dir=' + process.cwd() + '\\tmp\\demo\\cdp5', '--window-size=1440,900', 'about:blank']);
let fails = 0;
const check = (label, ok, detail) => { console.log((ok ? '✓ ' : '✗ ') + label + (detail ? '  ' + detail : '')); if (!ok) fails++; };
(async () => {
  let tabs;
  for (let i = 0; i < 40; i++) { try { tabs = await (await fetch('http://127.0.0.1:9339/json')).json(); if (tabs.length) break; } catch {} await sleep(300); }
  const ws = new WebSocket(tabs.find(t => t.type === 'page').webSocketDebuggerUrl);
  await new Promise(r => ws.onopen = r);
  let id = 0; const pend = new Map(); const errs = [];
  ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); } if (m.method === 'Runtime.exceptionThrown') errs.push((m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text).slice(0, 200)); };
  const send = (method, params = {}) => new Promise(r => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
  const ev = async (x) => { const r = await send('Runtime.evaluate', { expression: x, awaitPromise: true, returnByValue: true }); return r.result.exceptionDetails ? 'EXC ' + (r.result.exceptionDetails.exception?.description || r.result.exceptionDetails.text) : r.result.result?.value; };
  const typed = (j) => JSON.parse(j).filter(m => m.t === 'text' || (m.t === 'clipboard' && m.paste)).map(m => m.text).join('');
  const sent = () => ev(`JSON.stringify(([...viewers.values()][0].ws.sent || []).map(s => JSON.parse(s)).filter(m => m.t !== 'touch'))`);
  const reset = () => ev(`[...viewers.values()][0].ws.sent = []; 1`);
  const key = async (k, code, text, mods = 0) => {
    await send('Input.dispatchKeyEvent', { type: text ? 'keyDown' : 'rawKeyDown', key: k, code, windowsVirtualKeyCode: code === 'Enter' ? 13 : code === 'Backspace' ? 8 : code === 'ArrowLeft' ? 37 : k.toUpperCase().charCodeAt(0), text, modifiers: mods });
    await send('Input.dispatchKeyEvent', { type: 'keyUp', key: k, code, windowsVirtualKeyCode: code === 'Enter' ? 13 : code === 'Backspace' ? 8 : code === 'ArrowLeft' ? 37 : k.toUpperCase().charCodeAt(0), modifiers: mods });
  };

  await send('Runtime.enable');
  await send('Page.navigate', { url: 'http://127.0.0.1:8894/input.html?s=mview&lang=zh' });
  await sleep(2600);
  const rect = JSON.parse(await ev(`JSON.stringify((() => { const r = [...viewers.values()][0].canvas.getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })())`));

  // 1. 真实鼠标点击画面
  await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: rect.x, y: rect.y, button: 'left', clickCount: 1 });
  await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: rect.x, y: rect.y, button: 'left', clickCount: 1 });
  await sleep(200);
  const focus = await ev(`document.activeElement.tagName + '.' + document.activeElement.className + ' inDOM=' + document.contains(document.activeElement)`);
  check('点击画面后焦点在输入框上', focus.startsWith('TEXTAREA.ime') && focus.includes('inDOM=true'), focus);

  // 2. 真实英文按键
  await reset();
  for (const c of 'hello') await key(c, 'Key' + c.toUpperCase(), c);
  await sleep(100);
  let s = await sent();
  check('英文按键送到设备', typed(s) === 'hello' && JSON.parse(s).every(m => m.t === 'text'), s);

  // 3. 回车 / 退格 / 方向键
  await reset();
  await key('Enter', 'Enter', '\r'); await key('Backspace', 'Backspace'); await key('ArrowLeft', 'ArrowLeft');
  await sleep(100);
  s = await sent();
  const codes = JSON.parse(s).filter(m => m.t === 'key' && m.action === 0).map(m => m.code);
  check('回车/退格/方向键是按键事件', JSON.stringify(codes) === '[66,67,21]', s);
  check('回车没有被当成文字发送', typed(s) === '', s);

  // 4. 输入法组词（Input.imeSetComposition → insertText）
  await reset();
  await send('Input.imeSetComposition', { text: 'n', selectionStart: 1, selectionEnd: 1 });
  await send('Input.imeSetComposition', { text: 'ni', selectionStart: 2, selectionEnd: 2 });
  await send('Input.imeSetComposition', { text: 'ni hao', selectionStart: 6, selectionEnd: 6 });
  await sleep(100);
  const during = await sent();
  check('组词过程中不发送', JSON.parse(during).length === 0, during);
  await send('Input.insertText', { text: '你好' });
  await sleep(150);
  s = await sent();
  check('组词完成后发送一次「你好」', typed(s) === '你好' && JSON.parse(s).length === 1, s);
  check('中文走剪贴板粘贴（scrcpy 无法直接注入非 ASCII 字符）', JSON.parse(s)[0] && JSON.parse(s)[0].t === 'clipboard' && JSON.parse(s)[0].paste === true, s);
  check('输入框内容已清空', (await ev(`[...viewers.values()][0].ime.value`)) === '');

  // 5. 连续输入第二段中文 + 标点
  await reset();
  await send('Input.imeSetComposition', { text: 'shi jie', selectionStart: 7, selectionEnd: 7 });
  await send('Input.insertText', { text: '世界' });
  await send('Input.insertText', { text: '！' });
  await sleep(150);
  s = await sent();
  check('第二段中文和标点', typed(s) === '世界！', s);

  // 6. Ctrl+V 走粘贴，不发送字母 v
  await reset();
  await ev(`(() => { const v = [...viewers.values()][0]; v.paste = () => { window.__pasted = true; }; return 1; })()`);
  await key('v', 'KeyV', '', 2);
  await sleep(100);
  s = await sent();
  check('Ctrl+V 触发粘贴', (await ev('!!window.__pasted')) === true && typed(s) === '', s);

  // 7. 点击工具栏的「输入文字」后，文字框仍然可以输入
  await ev(`[...viewers.values()][0].kbBtn.click(); 1`);
  await sleep(200);
  const f2 = await ev(`document.activeElement === [...viewers.values()][0].textInput`);
  check('「输入文字」框能获得焦点', f2 === true);
  await reset();
  for (const c of 'ab') await key(c, 'Key' + c.toUpperCase(), c);
  await key('Enter', 'Enter', '\r');
  await sleep(100);
  s = await sent();
  check('「输入文字」框回车发送', JSON.parse(s).some(m => m.t === 'text' && m.text === 'ab'), s);

  // 8. 设置页的输入框不会被抢走焦点（之前 focusin 的副作用）
  await ev(`settingsDialog(); 1`);
  await sleep(300);
  check('打开对话框不会被画面抢焦点', (await ev(`!document.activeElement.classList.contains('ime')`)) === true, await ev(`document.activeElement.tagName + '.' + document.activeElement.className`));

  console.log(errs.length ? 'page errors: ' + errs.join(' | ') : 'page errors: none');
  console.log(fails ? `\n${fails} 项失败` : '\n全部通过');
  ws.close(); p.kill(); srv.close(); process.exit(fails ? 1 : 0);
})().catch(e => { console.error(e); p.kill(); process.exit(1); });
