// 文件浏览器测试：窗口与侧栏两种形态，导航、缓存、筛选、排序、多选、键盘、右键、视图切换
const fs = require('fs');
const path = require('path');
const os = require('os');
const { spawn } = require('child_process');
process.chdir(path.resolve(__dirname, '..'));
const sleep = ms => new Promise(r => setTimeout(r, ms));
let html = fs.readFileSync('web/index.html', 'utf8');
html = html.replace('<meta charset="utf-8">', () => `<meta charset="utf-8">\n<script>\n${fs.readFileSync('tools/demo/mock.js', 'utf8')}\n</script>`);
html = html.replace('<link rel="stylesheet" href="app.css">', () => `<style>\n${fs.readFileSync('web/app.css', 'utf8')}\n</style>`);
html = html.replace(/<script src="js\/([A-Za-z0-9_-]+)\.js"><\/script>/g, (m, n) => `<script>\n${fs.readFileSync('web/js/' + n + '.js', 'utf8')}\n</script>`);
const page = path.join(os.tmpdir(), 'db-files-test.html');
fs.writeFileSync(page, html);
// 预览接口：图片缩略图请求（<img src>）不经过页面里的 fetch，这里直接返回一张小 JPEG
const jpg = Buffer.from('/9j/4AAQSkZJRgABAQEASABIAAD/2wBDAP//////////////////////////////////////////////////////////////////////////////////////wgALCAABAAEBAREA/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPxA=', 'base64');
const srv = require('http').createServer((q, r) => {
  if (q.url.startsWith('/api/files/preview')) { r.setHeader('content-type', 'image/jpeg'); return r.end(jpg); }
  r.setHeader('content-type', 'text/html; charset=utf-8'); r.end(fs.readFileSync(page));
}).listen(8889);
const browser = process.env.BROWSER || [
  '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable', '/usr/bin/chromium', '/usr/bin/chromium-browser',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
].find(f => fs.existsSync(f));
if (!browser) { console.error('未找到 Chrome / Edge / Chromium'); process.exit(2); }
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'db-files-'));
const p = spawn(browser, ['--headless=new', '--no-sandbox', '--disable-gpu', '--remote-debugging-port=9344', '--user-data-dir=' + profile, '--window-size=1440,900', 'about:blank']);
let fails = 0;
const check = (l, ok, d) => { console.log((ok ? '✓ ' : '✗ ') + l + (d !== undefined ? '  ' + d : '')); if (!ok) fails++; };
(async () => {
  let tabs;
  // 浏览器冷启动可能较慢（CI 上可能超过 10 秒），最多等 60 秒
  for (let i = 0; i < 240; i++) { try { tabs = await (await fetch('http://127.0.0.1:9344/json')).json(); if (tabs && tabs.some(t => t.type === 'page')) break; } catch {} await sleep(250); }
  if (!tabs || !tabs.some(t => t.type === 'page')) { console.error('浏览器没有启动（60 秒内未打开调试端口）'); p.kill(); process.exit(3); }
  const ws = new WebSocket(tabs.find(t => t.type === 'page').webSocketDebuggerUrl);
  await new Promise(r => ws.onopen = r);
  let id = 0; const pend = new Map(); const errs = [];
  ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id && pend.has(m.id)) { pend.get(m.id)(m); pend.delete(m.id); } if (m.method === 'Runtime.exceptionThrown') errs.push((m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text).slice(0, 200)); };
  const send = (method, params = {}) => new Promise(r => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
  const ev = async (x) => { const r = await send('Runtime.evaluate', { expression: x, awaitPromise: true, returnByValue: true }); return r.result.exceptionDetails ? 'EXC ' + (r.result.exceptionDetails.exception?.description || r.result.exceptionDetails.text) : r.result.result?.value; };
  const key = async (k, code, vk, mods = 0) => {
    await send('Input.dispatchKeyEvent', { type: 'rawKeyDown', key: k, code, windowsVirtualKeyCode: vk, modifiers: mods });
    await send('Input.dispatchKeyEvent', { type: 'keyUp', key: k, code, windowsVirtualKeyCode: vk, modifiers: mods });
    await sleep(150);
  };
  const rows = (root) => ev(`[...document.querySelector(${JSON.stringify(root)}).querySelectorAll('.fb-row')].map(r => r.querySelector('b').textContent).join(',')`);
  await send('Runtime.enable');
  await send('Page.navigate', { url: 'http://127.0.0.1:8889/?s=mview&lang=zh' });
  for (let i = 0; i < 40; i++) { if ((await ev(`typeof viewers === 'object' && viewers.size > 0 && typeof FileBrowser === 'function'`)) === true) break; await sleep(250); }
  await sleep(800);
  await ev(`localStorage.clear(); 1`);

  // ---- 侧栏 ----
  await ev(`[...viewers.values()][0].filesBtn.click(); 1`);
  await sleep(600);
  check('点“文件”按钮打开侧栏', (await ev(`!!document.querySelector('.vlog .fb.compact') && [...viewers.values()][0].root.classList.contains('with-log')`)) === true);
  check('侧栏列出 /sdcard', (await rows('.vlog .fb')).startsWith('Android,DCIM,Documents,Download'), await rows('.vlog .fb'));
  check('面包屑显示“存储”', (await ev(`document.querySelector('.vlog .fb-crumbs').innerText.trim()`)) === '存储');
  // 进入 DCIM → Camera（单击文件夹）
  await ev(`[...document.querySelectorAll('.vlog .fb-row.dir')].find(r => r.innerText.includes('DCIM')).click(); 1`);
  await sleep(300);
  await ev(`[...document.querySelectorAll('.vlog .fb-row.dir')].find(r => r.innerText.includes('Camera')).click(); 1`);
  await sleep(300);
  check('单击文件夹进入', (await ev(`document.querySelector('.vlog .fb-crumbs').innerText.replace(/\\s+/g, '')`)) === '存储›DCIM›Camera');
  check('图片显示缩略图', (await ev(`new Promise(r => setTimeout(() => r(document.querySelectorAll('.vlog .fb-ico.has-thumb').length), 800))`)) > 0);
  // 后退（缓存：立即显示）
  await ev(`[...viewers.values()][0].files.back(); 1`);
  check('后退立即显示（缓存）', (await rows('.vlog .fb')).includes('Camera'), await rows('.vlog .fb'));
  // 打开日志会替换文件面板
  await ev(`[...viewers.values()][0].logBtn.click(); 1`);
  await sleep(300);
  check('打开日志后文件面板关闭（侧栏只放一个）', (await ev(`!document.querySelector('.vlog .fb') && !!document.querySelector('.vlog .lpanel') && !viewers.values().next().value.filesBtn.classList.contains('on')`)) === true);
  await ev(`[...viewers.values()][0].filesBtn.click(); 1`);
  await sleep(500);
  check('再打开文件面板记住上次的位置', (await ev(`document.querySelector('.vlog .fb-crumbs').innerText.replace(/\\s+/g, '')`)) === '存储›DCIM');
  await ev(`[...viewers.values()][0].toggleFiles(false); 1`);

  // ---- 窗口 ----
  await ev(`filesDialog(hosts.get('local'), __DEV.local[0]); 1`);
  await sleep(600);
  await ev(`document.querySelector('dialog.files .fb-crumb').click(); 1`);
  await sleep(400);
  check('窗口模式打开', (await rows('dialog.files')).startsWith('Android,DCIM'), await rows('dialog.files'));
  // 筛选
  await ev(`(() => { const f = document.querySelector('dialog.files .fb-filter'); f.value = 'mu'; f.dispatchEvent(new Event('input')); return 1; })()`);
  check('筛选', (await rows('dialog.files')) === 'Music', await rows('dialog.files'));
  await ev(`(() => { const f = document.querySelector('dialog.files .fb-filter'); f.value = ''; f.dispatchEvent(new Event('input')); return 1; })()`);
  // 排序：按大小（文件夹仍在前）
  await ev(`(() => { const fb = document.querySelector('dialog.files .fb'); return 1; })()`);
  await ev(`(() => { const b = [...document.querySelectorAll('dialog.files .fb-bar .ib')].find(x => x.title === '排序与显示'); b.click(); return 1; })()`);
  await sleep(200);
  check('排序菜单', (await ev(`[...document.querySelectorAll('.menu button')].map(b => b.textContent).join('/')`)) === '按名称/按修改时间/按大小/显示隐藏文件');
  await ev(`[...document.querySelectorAll('.menu button')].find(b => b.textContent === '按大小').click(); 1`);
  await sleep(200);
  const bySize = await rows('dialog.files');
  check('按大小排序（文件夹在前，大文件在前）', bySize.endsWith(',录音.m4a,notes.txt'), bySize);
  // 键盘：↓ 选择、Enter 打开
  await ev(`document.querySelector('dialog.files .fb-list').focus(); 1`);
  await key('ArrowDown', 'ArrowDown', 40);
  await key('ArrowDown', 'ArrowDown', 40);
  check('↓ 移动选择', (await ev(`[...document.querySelectorAll('dialog.files .fb-row.sel')].map(r => r.dataset.name).join(',')`)) === 'DCIM');
  await key('Enter', 'Enter', 13);
  await sleep(400);
  check('Enter 打开文件夹', (await ev(`document.querySelector('dialog.files .fb-crumbs').innerText.replace(/\\s+/g, '')`)) === '存储›DCIM');
  await key('Backspace', 'Backspace', 8);
  await sleep(400);
  check('Backspace 上一级', (await ev(`document.querySelector('dialog.files .fb-crumbs').innerText.replace(/\\s+/g, '')`)) === '存储');
  // 多选：Ctrl+点击
  await ev(`(() => { const rows = [...document.querySelectorAll('dialog.files .fb-row:not(.dir)')]; rows[0].dispatchEvent(new MouseEvent('click', { bubbles: true })); rows[1].dispatchEvent(new MouseEvent('click', { bubbles: true, ctrlKey: true })); return 1; })()`);
  await sleep(200);
  check('Ctrl+点击多选', (await ev(`document.querySelectorAll('dialog.files .fb-row.sel').length`)) === 2);
  check('状态栏显示已选', (await ev(`document.querySelector('dialog.files .fb-status').innerText`)).startsWith('已选 2 项') && !(await ev(`document.querySelector('dialog.files .fb-status').innerText`)).includes('null'), await ev(`document.querySelector('dialog.files .fb-status').innerText`));
  // 右键选中的多项
  await ev(`(() => { const r = document.querySelector('dialog.files .fb-row.sel').getBoundingClientRect(); window.__pt = { x: r.left + 60, y: r.top + r.height / 2 }; return 1; })()`);
  const pt = await ev(`JSON.stringify(window.__pt)`);
  const { x, y } = JSON.parse(pt);
  await send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'right', clickCount: 1 });
  await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'right', clickCount: 1 });
  await sleep(300);
  check('右键多选：批量下载 / 删除', (await ev(`[...document.querySelectorAll('.menu button')].map(b => b.textContent).join('/')`)) === '下载所选（2 项）/删除所选（2 项）');
  await ev(`closeMenu(); 1`);
  // 视图切换
  await ev(`[...document.querySelectorAll('dialog.files .fb-bar .ib')].find(x => x.title === '缩略图视图').click(); 1`);
  await sleep(300);
  check('切换缩略图视图', (await ev(`document.querySelector('dialog.files .fb-list').classList.contains('grid')`)) === true);
  check('视图偏好被记住', (await ev(`localStorage.getItem('db_files_view')`)) === 'grid');
  // 大目录分批渲染：伪造 1500 项
  await ev(`(() => { const fb = document.querySelector('dialog.files .fb'); return 1; })()`);
  const big = await ev(`(() => {
    const fb = new FileBrowser(hosts.get('local'), __DEV.local[0]);
    fb.cache.clear();
    const items = Array.from({ length: 1500 }, (_, i) => ({ name: 'file-' + String(i).padStart(4, '0') + '.txt', dir: false, size: i, mtime: 1700000000 + i }));
    fb.show('/sdcard/big', { path: '/sdcard/big', parent: '/sdcard', items, space: null }, false);
    const n1 = fb.list.querySelectorAll('.fb-row').length;
    fb.renderMore();
    return n1 + '/' + fb.list.querySelectorAll('.fb-row').length + '/' + fb.status.innerText;
  })()`);
  check('1500 项分批渲染（首批 200）', big.startsWith('200/400/1500 项'), big);
  // 预览
  await ev(`document.querySelectorAll('dialog[open]').forEach(d => d.close()); filesDialog(hosts.get('local'), __DEV.local[0]); 1`);
  await sleep(500);
  await ev(`(() => { const fb = document.querySelector('dialog.files'); return 1; })()`);
  await ev(`[...document.querySelectorAll('dialog.files .fb-row.dir')].find(r => r.innerText.includes('DCIM')).click(); 1`);
  await sleep(300);
  await ev(`[...document.querySelectorAll('dialog.files .fb-row.dir')].find(r => r.innerText.includes('Camera')).click(); 1`);
  await sleep(300);
  await ev(`(() => { const r = [...document.querySelectorAll('dialog.files .fb-row')].find(r => r.dataset.name.endsWith('.jpg')); r.dispatchEvent(new MouseEvent('dblclick', { bubbles: true })); return 1; })()`);
  await sleep(600);
  check('双击图片打开预览', (await ev(`!!document.querySelector('dialog.fb-preview img.fb-pv-img')`)) === true);
  check('预览可以左右切换', (await ev(`document.querySelectorAll('dialog.fb-preview .fb-pv-bar .ib').length`)) >= 3);

  // 拖放：拖到侧栏文件面板时，画面不应出现“安装 APK / 传到手机”的蓝框，文件面板显示上传目录
  await ev(`document.querySelectorAll('dialog[open]').forEach(d => d.close()); [...viewers.values()][0].toggleFiles(true); 1`);
  await sleep(600);
  const tmpFile = path.join(os.tmpdir(), 'db-drop.txt');
  fs.writeFileSync(tmpFile, 'drop test');
  const lp = JSON.parse(await ev(`JSON.stringify((() => { const r = document.querySelector('.vlog .fb-list').getBoundingClientRect(); return { x: Math.round(r.left + r.width / 2), y: Math.round(r.top + r.height / 2) }; })())`));
  const dragData = { items: [], files: [tmpFile], dragOperationsMask: 1 };
  await send('Input.dispatchDragEvent', { type: 'dragEnter', x: lp.x, y: lp.y, data: dragData });
  await send('Input.dispatchDragEvent', { type: 'dragOver', x: lp.x, y: lp.y, data: dragData });
  await sleep(200);
  check('拖到文件面板：画面没有蓝框', (await ev(`[...viewers.values()][0].root.classList.contains('dropping')`)) === false);
  check('拖到文件面板：显示上传目录', (await ev(`document.querySelector('.vlog .fb').classList.contains('dropping') && document.querySelector('.vlog .fb-drop').textContent`)).startsWith('松开上传到 /sdcard'));
  await ev(`window.__uploaded = []; const u = FileBrowser.prototype.upload; FileBrowser.prototype.upload = function (files) { window.__uploaded = files.map(f => f.name); }; 1`);
  await send('Input.dispatchDragEvent', { type: 'drop', x: lp.x, y: lp.y, data: dragData });
  await sleep(300);
  check('松开后上传到当前目录', (await ev(`window.__uploaded.join(',')`)) === 'db-drop.txt');
  check('松开后提示消失', (await ev(`document.querySelector('.vlog .fb').classList.contains('dropping')`)) === false);
  fs.unlinkSync(tmpFile);

  // 记住的目录已经不存在：自动退回上一级可用目录（模拟服务端返回“文件夹不存在”）
  await ev(`[...viewers.values()][0].toggleFiles(false); localStorage.setItem('db_files_path_R5CT20ABCDE', '/sdcard/Download/gone'); 1`);
  await ev(`(() => { const f = window.fetch; window.fetch = async (u, o) => { if (String(u).includes('/api/files/list') && JSON.parse(o.body).path === '/sdcard/Download/gone') return new Response(JSON.stringify({ error: '文件夹不存在：/sdcard/Download/gone' }), { status: 400, headers: { 'content-type': 'application/json' } }); return f(u, o); }; return 1; })()`);
  await ev(`[...viewers.values()][0].toggleFiles(true); 1`);
  for (let i = 0; i < 20; i++) { await sleep(200); if ((await ev(`[...viewers.values()][0].files.opened === true`)) === true) break; }
  check('记住的目录不存在时退回上一级', (await ev(`[...viewers.values()][0].files.path`)) === '/sdcard/Download', await ev(`[...viewers.values()][0].files.path`));

  console.log(errs.length ? 'page errors: ' + errs.join(' | ') : 'page errors: none');
  console.log(fails ? `\n${fails} 项失败` : '\n全部通过');
  ws.close(); p.kill(); srv.close(); process.exit(fails ? 1 : 0);
})().catch(e => { console.error(e); p.kill(); process.exit(1); });
