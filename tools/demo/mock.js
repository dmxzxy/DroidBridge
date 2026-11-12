// 演示用：模拟后端接口、视频流与日志流，只用于生成 README 截图，不随程序发布。所有名称、地址、序列号均为虚构。
(() => {
  const S = new URLSearchParams(location.search).get('s') || 'desk';
  const INFO = { app: 'droidbridge', name: 'Studio-PC', version: '0.1.0', scrcpy: '5.0.1', auth: false, ips: ['192.168.1.10'], port: 8730, https_port: 8731 };
  const PEERS = [
    { id: 'pB', name: 'Lab-Mac', addr: '192.168.1.20:8730', has_token: true },
    { id: 'pC', name: 'Build-Linux', addr: '192.168.1.30:8730', has_token: false },
  ];
  const d = (serial, state, model) => ({ serial, state, model, product: '', device: '', transport_id: '1', wireless: serial.includes(':'), uid: serial });
  const DEV = {
    local: [d('R5CT20ABCDE', 'device', 'SM S9180'), d('7HX5T19B28', 'unauthorized', '')],
    pB: [d('2A281FDH3000XY', 'device', 'Pixel 7'), d('192.168.1.31:5555', 'device', '23013RK75C'), d('emulator-5554', 'offline', '')],
  };
  window.__DEV = DEV;

  // 相册模拟：用 canvas 生成几张假缩略图
  function thumb(seed, label) {
    const c = document.createElement('canvas'); c.width = 320; c.height = 320;
    const g = c.getContext('2d');
    const colors = [['#1d3b7a', '#a8426b'], ['#0b6e4f', '#1b4332'], ['#5a2d82', '#2a1b3d'], ['#b05a2c', '#7a3b1d']];
    const [c1, c2] = colors[seed % colors.length];
    const gr = g.createLinearGradient(0, 0, 320, 320); gr.addColorStop(0, c1); gr.addColorStop(1, c2);
    g.fillStyle = gr; g.fillRect(0, 0, 320, 320);
    g.fillStyle = 'rgba(255,255,255,.85)'; g.font = '600 20px sans-serif'; g.textAlign = 'center';
    g.fillText(label, 160, 160);
    g.fillStyle = 'rgba(255,255,255,.5)'; g.font = '14px sans-serif';
    g.fillText((LANG === 'en' ? 'Screenshot' : '截图') + ' · ' + seed, 160, 190);
    return c.toDataURL('image/png');
  }
  const SHOTS = [
    { name: 'Pixel_7_20261107-102400.png', kind: 'shot', device: 'Pixel 7', size: 182000, time: Math.floor(Date.now() / 1000) - 3600, url: null },
    { name: 'SM-S9180_20261107-101500.png', kind: 'shot', device: 'SM-S9180', size: 201000, time: Math.floor(Date.now() / 1000) - 86400, url: null },
    { name: 'Pixel_7_20261106-093000.png', kind: 'shot', device: 'Pixel 7', size: 175000, time: Math.floor(Date.now() / 1000) - 2 * 86400, url: null },
    { name: 'SM-S9180_20261107-104500.webm', kind: 'rec', device: 'SM-S9180', size: 4200000, time: Math.floor(Date.now() / 1000) - 1800, url: null },
  ];
  // 缩略图懒生成：不能在顶层调用 thumb()，因为本脚本在 <head> 先于页面脚本执行，
  // 此时页面里的 LANG（let）还不存在，会 ReferenceError 导致整个 mock 失效。
  const json = (b, status = 200) => new Response(JSON.stringify(b), { status, headers: { 'content-type': 'application/json' } });
  window.fetch = async (url, opts) => {
    const u = new URL(url, 'http://x');
    const post = typeof opts?.body === 'string' ? JSON.parse(opts.body) : {};
    if (u.pathname === '/api/info') return json(INFO);
    if (u.pathname === '/api/peers') return json(PEERS);
    if (u.pathname === '/api/node') return json(u.searchParams.get('peer') === 'pB' ? { version: '0.0.9', build: 'old', os: 'macos' } : INFO);
    if (u.pathname === '/api/discovered') return json({ enabled: true, list: [{ name: 'QA-Bench-04', addr: '192.168.1.44:8730', auth: false, version: '0.2.0', os: 'windows' }] });
    if (u.pathname === '/api/adb/qr') return json({ job: 'q1', name: 'DroidBridge-demo', svg: fakeQr() });
    if (u.pathname.startsWith('/api/adb/qr/')) return json({ state: 'waiting', msg: LANG === 'en' ? 'Waiting for the phone to scan…' : '等待手机扫码…' });
    if (u.pathname === '/api/devices') {
      const peer = u.searchParams.get('peer') || 'local';
      if (peer === 'pC') return json({ error: (LANG === "en" ? "Cannot reach 192.168.1.30:8730: connection refused (os error 111)" : "无法访问 192.168.1.30:8730：由于目标计算机积极拒绝，无法连接。 (os error 10061)") }, 502);
      return json(DEV[peer] || []);
    }
    if (u.pathname === '/api/media') {
      SHOTS.forEach((s, i) => { if (!s.url) s.url = thumb(i, s.device); });
      return json({ dir: 'C:\\DroidBridge\\data\\media', items: SHOTS });
    }
    if (u.pathname === '/api/apps') return json({ named: true, apps: APPS() });
    if (u.pathname === '/api/apps/action') return json({ ok: true, msg: LANG === 'en' ? 'Done' : '完成' });
    if (u.pathname === '/api/files/preview') {
      // 缩略图：用 canvas 画一张渐变图
      const c = document.createElement('canvas'); c.width = c.height = 160; const g = c.getContext('2d');
      const n = (u.searchParams.get('path') || '').length;
      const gr = g.createLinearGradient(0, 0, 160, 160); gr.addColorStop(0, ['#1d3b7a', '#0b6e4f', '#5a2d82', '#b05a2c'][n % 4]); gr.addColorStop(1, ['#a8426b', '#1b4332', '#2a1b3d', '#7a3b1d'][n % 4]);
      g.fillStyle = gr; g.fillRect(0, 0, 160, 160);
      g.fillStyle = 'rgba(255,255,255,.5)'; g.beginPath(); g.arc(110, 50, 18, 0, 7); g.fill();
      g.fillStyle = 'rgba(0,0,0,.25)'; g.beginPath(); g.moveTo(0, 160); g.lineTo(60, 80); g.lineTo(120, 160); g.fill();
      const blob = await new Promise(r => c.toBlob(r, 'image/jpeg'));
      return new Response(blob, { headers: { 'content-type': 'image/jpeg' } });
    }
    if (u.pathname === '/api/files/list') {
      const p = post.path || '/sdcard';
      return json(FILES(p));
    }
    if (u.pathname === '/api/update/check') return json({ current: '0.7.0', latest: '0.8.0', tag: 'v0.8.0', newer: true, supported: true,
      asset: 'DroidBridge-0.8.0-windows-x64.zip', page: 'https://github.com/dmxzxy/DroidBridge/releases/tag/v0.8.0',
      download: 'https://github.com/dmxzxy/DroidBridge/releases/download/v0.8.0/DroidBridge-0.8.0-windows-x64.zip' });
    if (u.pathname === '/api/settings') return json({ media_dir: 'C:\\DroidBridge\\data\\media' });
    return json({ error: 'not found' }, 404);
  };
  if (!window.VideoDecoder) window.VideoDecoder = function () {};
  function fakeQr() {
    const n = 29, c = 8; let r = '', seed = 7;
    const rnd = () => (seed = (seed * 9301 + 49297) % 233280) / 233280;
    const finder = (x, y) => `<rect x="${x*c}" y="${y*c}" width="${7*c}" height="${7*c}"/><rect x="${(x+1)*c}" y="${(y+1)*c}" width="${5*c}" height="${5*c}" fill="#fff"/><rect x="${(x+2)*c}" y="${(y+2)*c}" width="${3*c}" height="${3*c}"/>`;
    for (let y = 0; y < n; y++) for (let x = 0; x < n; x++) { const inF = (x < 8 && y < 8) || (x > n - 9 && y < 8) || (x < 8 && y > n - 9); if (!inF && rnd() < 0.5) r += `<rect x="${x*c}" y="${y*c}" width="${c}" height="${c}"/>`; }
    return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="-16 -16 ${n*c+32} ${n*c+32}"><rect x="-16" y="-16" width="${n*c+32}" height="${n*c+32}" fill="#fff"/>${r}${finder(0,0)}${finder(n-7,0)}${finder(0,n-7)}</svg>`;
  }

  const APPS = () => {
    const zh = LANG !== 'en';
    const u = [['Example Game', 'com.example.game', 1], [zh ? '记账本' : 'Ledger', 'com.example.ledger', 0], [zh ? '地图导航' : 'Maps Go', 'com.example.maps', 0], ['Chrome', 'com.android.chrome', 0],
      [zh ? '测试工具' : 'Test Kit', 'com.example.testkit', 1], [zh ? '视频剪辑' : 'Video Editor', 'com.example.video', 0], [zh ? '音乐' : 'Music', 'com.example.music', 0], ['WeChat', 'com.tencent.mm', 1],
      [zh ? '阅读' : 'Reader', 'com.example.reader', 0], [zh ? '天气' : 'Weather', 'com.example.weather', 0]];
    const sys = [[zh ? '设置' : 'Settings', 'com.android.settings', 1], [zh ? '相机' : 'Camera', 'com.android.camera', 0], [zh ? '电话' : 'Phone', 'com.android.dialer', 0]];
    return [...u.map(([name, pkg, r]) => ({ name, pkg, system: false, running: !!r })), ...sys.map(([name, pkg, r]) => ({ name, pkg, system: true, running: !!r }))];
  };
  // 文件管理演示数据
  const FILES = (path) => {
    const zh = LANG !== 'en';
    const t = Math.floor(Date.now() / 1000);
    const dir = (name, mtime) => ({ name, dir: true, size: 4096, mtime });
    const file = (name, size, mtime) => ({ name, dir: false, size, mtime });
    const tree = {
      '/sdcard': [dir('Android', t - 86400 * 20), dir('DCIM', t - 3600), dir('Download', t - 7200), dir('Movies', t - 86400 * 3), dir('Music', t - 86400 * 9), dir('Pictures', t - 86400), dir('Documents', t - 86400 * 2),
        file('notes.txt', 2048, t - 600), file(zh ? '录音.m4a' : 'recording.m4a', 3_400_000, t - 900)],
      '/sdcard/DCIM': [dir('Camera', t - 3600), dir('Screenshots', t - 1800)],
      '/sdcard/DCIM/Camera': [file('IMG_20261107_102400.jpg', 3_800_000, t - 300), file('IMG_20261107_101500.jpg', 3_600_000, t - 900), file(zh ? '视频 001.mp4' : 'VID_001.mp4', 48_200_000, t - 3600)],
      '/sdcard/DCIM/Screenshots': [file('Screenshot_20261107-101500.png', 880_000, t - 1800)],
      '/sdcard/Download': [file('example-game-2.4.1.apk', 23 * 1048576, t - 7200), file(zh ? '测试报告.pdf' : 'report.pdf', 1_240_000, t - 172800), file('DroidBridge-0.6.0-windows-x64.zip', 23_500_000, t - 43200)],
      '/sdcard/Movies': [file('screen-2026-11-07.webm', 12_400_000, t - 86400 * 3)],
      '/sdcard/Music': [file('track-01.mp3', 6_200_000, t - 86400 * 9)],
      '/sdcard/Pictures': [file('wallpaper.png', 420_000, t - 86400)],
      '/sdcard/Documents': [file(zh ? '清单.md' : 'todo.md', 1200, t - 86400 * 2)],
      '/sdcard/Android': [dir('data', t - 86400 * 20), dir('obb', t - 86400 * 20)],
    };
    const items = (tree[path] || []).slice().sort((a, b) => (b.dir - a.dir) || a.name.localeCompare(b.name));
    const parent = path === '/sdcard' ? null : path.split('/').slice(0, -1).join('/') || '/sdcard';
    return { path, parent, items, space: { total: 126_000_000_000, free: 47_800_000_000 } };
  };
  let perfTick = 0;
  const perfSample = () => {
    perfTick++;
    const fps = 57 + Math.round(Math.sin(perfTick / 2) * 2 + (perfTick % 7 === 3 ? -14 : 0));
    return { type: 'perf', pkg: 'com.example.game', fps, cpu: 41.3, app_cpu: 18.6, mem_total: 11742, mem_used: 7210, app_mem: 1184, cpu_temp: 46.2, batt_temp: 33.5, level: 78, charging: false };
  };
  const KIND = { 'R5CT20ABCDE': 'home', '2A281FDH3000XY': 'settings', '192.168.1.31:5555': 'error' };
  const NAME = { 'R5CT20ABCDE': 'SM-S9180', '2A281FDH3000XY': 'Pixel 7' };
  class FakeWS {
    constructor(url) { this.readyState = 0; this.isLog = url.includes('/ws/logcat'); this.isPerf = url.includes('/ws/perf'); if (this.isPerf) { this.readyState = 1; const hist = () => { for (let i = 0; i < 30; i++) this.emit(perfSample()); }; setTimeout(hist, 50); this.timer = setInterval(() => this.emit(perfSample()), 1000); return; } this.serial = new URLSearchParams(url.split('?')[1] || '').get('serial'); setTimeout(() => this.isLog ? this.startLog() : this.start(), 50); }
    startLog() {
      this.readyState = 1; this.onopen && this.onopen();
      const T = (i) => '11-07 10:24:' + String(10 + (i >> 2)).padStart(2, '0') + '.' + String(100 + i * 37 % 900);
      const rows = [['I','ActivityManager','Start proc 5123:com.example.game/u0a231 for top-activity {com.example.game/com.example.game.MainActivity}'],['D','Unity','Initialize engine version: 2022.3.20f1'],['I','Unity','SystemInfo CPU = ARM64 FP ARMv8-A, Cores = 8, Memory = 7612mb'],['D','Unity','GfxDevice: creating device client; threaded=1; jobified=1'],['I','Unity','Loading scene: Assets/Scenes/Main.unity'],['W','Unity','The referenced script on this Behaviour (Game Object "HUD") is missing!'],['D','AudioTrack','start(1234): prior state:STATE_STOPPED'],['I','Unity','[Net] Connected to 192.168.1.50:9000 in 42 ms'],['D','OpenGLRenderer','Davey! duration=712ms; Flags=1, IntendedVsync=8412'],['W','Unity','Shader warning: unrecognized pragma in Hidden/Blit'],['E','Unity','NullReferenceException: Object reference not set to an instance of an object'],['E','Unity','  at PlayerController.Update () [0x0001c] in PlayerController.cs:57'],['I','Unity','[Shop] Fetched 24 items'],['D','InputDispatcher','Delivering touch to com.example.game: action: DOWN'],['I','Unity','Frame time avg 16.4 ms (61 fps)']];
      const lines = rows.map((r, i) => T(i) + '  5123  5140 ' + r[0] + ' ' + r[1].padEnd(8) + ': ' + r[2]);
      setTimeout(() => this.emit({ type: 'lines', l: lines }), 50);
    }
    emit(o) { this.onmessage && this.onmessage({ data: JSON.stringify(o) }); }
    start() {
      this.readyState = 1;
      const kind = KIND[this.serial] || 'home';
      if (kind === 'error') {
        this.emit({ type: 'status', msg: (LANG === "en" ? "Pushing scrcpy-server…" : "推送 scrcpy-server…") });
        setTimeout(() => this.emit({ type: 'error', msg: (LANG === "en" ? "scrcpy-server exited (exit code: 1); make sure USB debugging is authorized on the device" : "scrcpy-server 启动失败并退出（exit code: 1），请检查设备是否已授权 USB 调试") }), 100);
        setTimeout(() => { this.readyState = 3; this.onclose && this.onclose(); }, 150);
        return;
      }
      this.emit({ type: 'meta', device_name: NAME[this.serial] || this.serial, codec: 'h264', serial: this.serial });
      this.emit({ type: 'session', width: 720, height: 1600 });
      setTimeout(() => { for (const v of viewers.values()) if (v.ws === this) paint(v, kind); }, 100);
    }
    send(d) { (this.sent || (this.sent = [])).push(d); }
    close() { this.readyState = 3; clearInterval(this.timer); }
  }
  window.WebSocket = FakeWS;

  function paint(v, kind) {
    const W = 720, H = 1600, c = v.canvas;
    c.width = W; c.height = H; v.vw = W; v.vh = H;
    const g = c.getContext('2d');
    kind === 'home' ? drawHome(g, W, H) : drawSettings(g, W, H);
    g.font = '22px sans-serif'; g.textAlign = 'center'; g.fillStyle = kind === 'home' ? 'rgba(255,255,255,.55)' : 'rgba(0,0,0,.35)';
    g.fillText((LANG === "en" ? "Mock screen · not a real device" : "示意画面 · 非真实设备"), W / 2, H - 40);
    v.gotFrame = true; v.overlay.classList.add('hide'); v.setDot('ok'); v.fit();
    v.updateStat = () => {}; v.statEl.textContent = '60 fps';
  }
  const rr = (g, x, y, w, h, r) => { g.beginPath(); g.roundRect(x, y, w, h, r); g.fill(); };
  function statusBar(g, W, color) {
    g.fillStyle = color; g.font = '600 28px sans-serif'; g.textAlign = 'left'; g.fillText('10:24', 36, 52);
    g.textAlign = 'right'; g.fillText('5G  87%', W - 36, 52);
  }
  function drawHome(g, W, H) {
    const gr = g.createLinearGradient(0, 0, W, H); gr.addColorStop(0, '#1d3b7a'); gr.addColorStop(.55, '#4b2c83'); gr.addColorStop(1, '#a8426b');
    g.fillStyle = gr; g.fillRect(0, 0, W, H);
    statusBar(g, W, '#fff');
    g.fillStyle = '#fff'; g.textAlign = 'center'; g.font = '200 150px sans-serif'; g.fillText('10:24', W / 2, 300);
    g.font = '30px sans-serif'; g.fillText((LANG === "en" ? "Sat, Nov 7  ☀ 18°C" : "11月7日 星期六  ☀ 18°C"), W / 2, 360);
    const EN_APPS = {"电话":"Phone","信息":"Messages","相机":"Camera","相册":"Photos","设置":"Settings","浏览器":"Browser","日历":"Calendar","时钟":"Clock","天气":"Weather","音乐":"Music","视频":"Videos","文件":"Files","应用商店":"Store","地图":"Maps","计算器":"Calculator","备忘录":"Notes"};
    const apps = [['电话', '#34c759'], ['信息', '#30b0ff'], ['相机', '#8e8e93'], ['相册', '#ff9f0a'], ['设置', '#636366'], ['浏览器', '#0a84ff'], ['日历', '#ff453a'], ['时钟', '#1c1c1e'],
      ['天气', '#64d2ff'], ['音乐', '#ff375f'], ['视频', '#bf5af2'], ['文件', '#ffd60a'], ['应用商店', '#0a84ff'], ['地图', '#30d158'], ['计算器', '#ff9f0a'], ['备忘录', '#ffcc00']];
    const cols = 4, size = 112, gapX = (W - cols * size) / (cols + 1);
    apps.forEach(([n, col], i) => {
      const x = gapX + (i % cols) * (size + gapX), y = 520 + Math.floor(i / cols) * 200;
      g.fillStyle = col; rr(g, x, y, size, size, 30);
      g.fillStyle = 'rgba(255,255,255,.9)'; g.beginPath(); g.arc(x + size / 2, y + size / 2, 24, 0, 7); g.fill();
      g.fillStyle = '#fff'; g.font = '26px sans-serif'; g.textAlign = 'center'; g.fillText(LANG === "en" ? EN_APPS[n] : n, x + size / 2, y + size + 38);
    });
    g.fillStyle = 'rgba(255,255,255,.22)'; rr(g, 40, 1340, W - 80, 90, 45);
    g.fillStyle = 'rgba(255,255,255,.85)'; g.font = '30px sans-serif'; g.textAlign = 'left'; g.fillText((LANG === "en" ? "🔍  Search" : "🔍  搜索"), 90, 1396);
    g.fillStyle = 'rgba(255,255,255,.8)'; rr(g, W / 2 - 90, H - 24, 180, 8, 4);
  }
  function drawSettings(g, W, H) {
    g.fillStyle = '#f2f2f7'; g.fillRect(0, 0, W, H);
    statusBar(g, W, '#111');
    g.fillStyle = '#111'; g.font = '600 64px sans-serif'; g.textAlign = 'left'; g.fillText((LANG === "en" ? "Settings" : "设置"), 40, 180);
    g.fillStyle = '#e3e3e8'; rr(g, 40, 220, W - 80, 80, 40);
    g.fillStyle = '#888'; g.font = '30px sans-serif'; g.fillText((LANG === "en" ? "🔍  Search settings" : "🔍  搜索设置"), 80, 272);
    const items = [['WLAN', '已连接 Office-5G', '#0a84ff'], ['蓝牙', '已开启', '#0a84ff'], ['移动网络', '中国移动 5G', '#34c759'], ['已连接的设备', 'USB 调试已连接', '#5e5ce6'],
      ['应用', '最近打开：DroidBridge', '#ff9f0a'], ['通知', '3 个应用已关闭通知', '#ff453a'], ['电池', '87% · 约剩 1 天', '#30d158'], ['存储', '已使用 64%', '#bf5af2'],
      ['显示', '深色模式 · 自动亮度', '#ffcc00'], ['开发者选项', 'USB 调试已开启', '#636366']];
    if (LANG === "en") items.splice(0, items.length, ["Wi-Fi","Office-5G","#0a84ff"],["Bluetooth","On","#0a84ff"],["Mobile network","5G","#34c759"],["Connected devices","USB debugging connected","#5e5ce6"],["Apps","Recently opened: DroidBridge","#ff9f0a"],["Notifications","3 apps muted","#ff453a"],["Battery","87% · about 1 day left","#30d158"],["Storage","64% used","#bf5af2"],["Display","Dark mode · auto brightness","#ffcc00"],["Developer options","USB debugging on","#636366"]);
    let y = 350;
    g.fillStyle = '#fff'; rr(g, 30, y - 10, W - 60, items.length * 112 + 20, 28);
    items.forEach(([t, s, col], i) => {
      const cy = y + i * 112 + 56;
      g.fillStyle = col; g.beginPath(); g.arc(90, cy, 30, 0, 7); g.fill();
      g.fillStyle = '#111'; g.font = '32px sans-serif'; g.textAlign = 'left'; g.fillText(t, 145, cy - 6);
      g.fillStyle = '#888'; g.font = '24px sans-serif'; g.fillText(s, 145, cy + 30);
      g.fillStyle = '#c7c7cc'; g.font = '36px sans-serif'; g.textAlign = 'right'; g.fillText('›', W - 60, cy + 10);
      if (i) { g.fillStyle = '#e5e5ea'; g.fillRect(145, cy - 56, W - 175, 2); }
    });
    g.fillStyle = 'rgba(0,0,0,.6)'; rr(g, W / 2 - 90, H - 24, 180, 8, 4);
  }

  window.addEventListener('load', () => setTimeout(() => {
    const L = hosts.get('local'), B = hosts.get('pB');
    if (S === 'desk') { openViewer(L, DEV.local[0]); openViewer(B, DEV.pB[0]); openViewer(B, DEV.pB[1]); }
    if (S === 'mview' || S === 'menu' || S === 'settings' || S === 'log') openViewer(L, DEV.local[0]);
    if (S === 'log') setTimeout(() => [...viewers.values()][0].toggleLog(true), 300);
    if (S === 'add') peerDialog();
    if (S === 'perf') { openViewer(L, DEV.local[0]); setTimeout(() => [...viewers.values()][0].togglePerf(), 300); }
    if (S === 'update') { UPDATE = { current: '0.7.0', latest: '0.8.0', page: 'https://github.com/dmxzxy/DroidBridge/releases/tag/v0.8.0', supported: true }; updateDialog(); }
    if (S === 'files') { setTimeout(() => { filesDialog(L, DEV.local[0]); setTimeout(() => { const fb = document.querySelector('dialog.files .fb'); [...fb.querySelectorAll('.fb-row.dir')].find(r => r.innerText.includes('DCIM')).click(); setTimeout(() => [...fb.querySelectorAll('.fb-row.dir')].find(r => r.innerText.includes('Camera')).click(), 200); }, 300); }, 300); }
    if (S === 'files-side') { openViewer(L, DEV.local[0]); setTimeout(() => { const v = [...viewers.values()][0]; v.toggleFiles(true); }, 500); }
    if (S === 'apps') { openViewer(L, DEV.local[0]); setTimeout(() => appsDialog(L, DEV.local[0]), 300); }
    if (S === 'batch') setTimeout(() => {
      batchInstallDialog(new File([new Uint8Array(23 * 1048576)], 'example-game-2.4.1.apk'));
      const d = [...document.querySelectorAll('.bi-d .r')];
      const st = [['ok', LANG === 'en' ? 'Installed' : '已安装'], ['ok', LANG === 'en' ? 'Installed' : '已安装'], ['', LANG === 'en' ? 'Confirm on the device…' : '等待设备上确认…']];
      d.forEach((r, i) => { if (!st[i]) return; r.className = 'r ' + st[i][0]; r.replaceChildren(st[i][0] ? h('span', { html: svg('check') }).firstChild : h('span', { class: 'spin sm' }), st[i][1]); });
      document.querySelectorAll('.bi-d input').forEach(c => c.disabled = true);
      const b = document.querySelector('dialog.wide .btn.primary'); b.textContent = LANG === 'en' ? 'Installing…' : '正在安装…'; b.disabled = true;
    }, 300);
    if (S === 'sync') { openViewer(B, DEV.pB[0]); openViewer(L, DEV.local[0]); setTimeout(() => { for (const v of viewers.values()) v.toggleSync(true); }, 400); }
    if (S === 'wireless') wirelessDialog(L);
    if (S === 'gallery') setTimeout(() => openGallery(), 300);
    if (S === 'settings') setTimeout(settingsDialog, 300);
    if (S === 'menu') { removeEventListener('scroll', closeMenu, true); window.closeMenu = () => {}; setTimeout(() => [...viewers.values()][0].moreMenu(), 200); }
    if (S === 'mview' || S === 'desk' || S === 'sync') window.scrollTo(0, 0);
  }, 800));
})();
