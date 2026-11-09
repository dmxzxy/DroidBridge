// 演示用：模拟后端接口、视频流与日志流，只用于生成 README 截图，不随程序发布。所有名称、地址、序列号均为虚构。
(() => {
  const S = new URLSearchParams(location.search).get('s') || 'desk';
  const INFO = { app: 'droidbridge', name: 'Studio-PC', version: '0.1.0', scrcpy: '5.0.1', auth: false, ips: ['192.168.1.10'], port: 8730, https_port: 8731 };
  const PEERS = [
    { id: 'pB', name: 'Lab-Mac', addr: '192.168.1.20:8730', has_token: true },
    { id: 'pC', name: 'Build-Linux', addr: '192.168.1.30:8730', has_token: false },
  ];
  const d = (serial, state, model) => ({ serial, state, model, product: '', device: '', transport_id: '1' });
  const DEV = {
    local: [d('R5CT20ABCDE', 'device', 'SM S9180'), d('7HX5T19B28', 'unauthorized', '')],
    pB: [d('2A281FDH3000XY', 'device', 'Pixel 7'), d('192.168.1.31:5555', 'device', '23013RK75C'), d('emulator-5554', 'offline', '')],
  };
  window.__DEV = DEV;
  const json = (b, status = 200) => new Response(JSON.stringify(b), { status, headers: { 'content-type': 'application/json' } });
  window.fetch = async (url) => {
    const u = new URL(url, 'http://x');
    if (u.pathname === '/api/info') return json(INFO);
    if (u.pathname === '/api/peers') return json(PEERS);
    if (u.pathname === '/api/devices') {
      const peer = u.searchParams.get('peer') || 'local';
      if (peer === 'pC') return json({ error: '无法访问 192.168.1.30:8730：由于目标计算机积极拒绝，无法连接。 (os error 10061)' }, 502);
      return json(DEV[peer] || []);
    }
    return json({ error: 'not found' }, 404);
  };
  if (!window.VideoDecoder) window.VideoDecoder = function () {};

  const KIND = { 'R5CT20ABCDE': 'home', '2A281FDH3000XY': 'settings', '192.168.1.31:5555': 'error' };
  const NAME = { 'R5CT20ABCDE': 'SM-S9180', '2A281FDH3000XY': 'Pixel 7' };
  class FakeWS {
    constructor(url) { this.readyState = 0; this.isLog = url.includes('/ws/logcat'); this.serial = new URLSearchParams(url.split('?')[1] || '').get('serial'); setTimeout(() => this.isLog ? this.startLog() : this.start(), 50); }
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
        this.emit({ type: 'status', msg: '推送 scrcpy-server…' });
        setTimeout(() => this.emit({ type: 'error', msg: 'scrcpy-server 启动失败并退出（exit code: 1），请检查设备是否已授权 USB 调试' }), 100);
        return;
      }
      this.emit({ type: 'meta', device_name: NAME[this.serial] || this.serial, codec: 'h264', serial: this.serial });
      this.emit({ type: 'session', width: 720, height: 1600 });
      setTimeout(() => { for (const v of viewers.values()) if (v.ws === this) paint(v, kind); }, 100);
    }
    send() {}
    close() { this.readyState = 3; }
  }
  window.WebSocket = FakeWS;

  function paint(v, kind) {
    const W = 720, H = 1600, c = v.canvas;
    c.width = W; c.height = H; v.vw = W; v.vh = H;
    const g = c.getContext('2d');
    kind === 'home' ? drawHome(g, W, H) : drawSettings(g, W, H);
    g.font = '22px sans-serif'; g.textAlign = 'center'; g.fillStyle = kind === 'home' ? 'rgba(255,255,255,.55)' : 'rgba(0,0,0,.35)';
    g.fillText('示意画面 · 非真实设备', W / 2, H - 40);
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
    g.font = '30px sans-serif'; g.fillText('11月7日 星期六  ☀ 18°C', W / 2, 360);
    const apps = [['电话', '#34c759'], ['信息', '#30b0ff'], ['相机', '#8e8e93'], ['相册', '#ff9f0a'], ['设置', '#636366'], ['浏览器', '#0a84ff'], ['日历', '#ff453a'], ['时钟', '#1c1c1e'],
      ['天气', '#64d2ff'], ['音乐', '#ff375f'], ['视频', '#bf5af2'], ['文件', '#ffd60a'], ['应用商店', '#0a84ff'], ['地图', '#30d158'], ['计算器', '#ff9f0a'], ['备忘录', '#ffcc00']];
    const cols = 4, size = 112, gapX = (W - cols * size) / (cols + 1);
    apps.forEach(([n, col], i) => {
      const x = gapX + (i % cols) * (size + gapX), y = 520 + Math.floor(i / cols) * 200;
      g.fillStyle = col; rr(g, x, y, size, size, 30);
      g.fillStyle = 'rgba(255,255,255,.9)'; g.beginPath(); g.arc(x + size / 2, y + size / 2, 24, 0, 7); g.fill();
      g.fillStyle = '#fff'; g.font = '26px sans-serif'; g.textAlign = 'center'; g.fillText(n, x + size / 2, y + size + 38);
    });
    g.fillStyle = 'rgba(255,255,255,.22)'; rr(g, 40, 1340, W - 80, 90, 45);
    g.fillStyle = 'rgba(255,255,255,.85)'; g.font = '30px sans-serif'; g.textAlign = 'left'; g.fillText('🔍  搜索', 90, 1396);
    g.fillStyle = 'rgba(255,255,255,.8)'; rr(g, W / 2 - 90, H - 24, 180, 8, 4);
  }
  function drawSettings(g, W, H) {
    g.fillStyle = '#f2f2f7'; g.fillRect(0, 0, W, H);
    statusBar(g, W, '#111');
    g.fillStyle = '#111'; g.font = '600 64px sans-serif'; g.textAlign = 'left'; g.fillText('设置', 40, 180);
    g.fillStyle = '#e3e3e8'; rr(g, 40, 220, W - 80, 80, 40);
    g.fillStyle = '#888'; g.font = '30px sans-serif'; g.fillText('🔍  搜索设置', 80, 272);
    const items = [['WLAN', '已连接 Office-5G', '#0a84ff'], ['蓝牙', '已开启', '#0a84ff'], ['移动网络', '中国移动 5G', '#34c759'], ['已连接的设备', 'USB 调试已连接', '#5e5ce6'],
      ['应用', '最近打开：DroidBridge', '#ff9f0a'], ['通知', '3 个应用已关闭通知', '#ff453a'], ['电池', '87% · 约剩 1 天', '#30d158'], ['存储', '已使用 64%', '#bf5af2'],
      ['显示', '深色模式 · 自动亮度', '#ffcc00'], ['开发者选项', 'USB 调试已开启', '#636366']];
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
    if (S === 'settings') setTimeout(settingsDialog, 300);
    if (S === 'menu') { removeEventListener('scroll', closeMenu, true); window.closeMenu = () => {}; setTimeout(() => [...viewers.values()][0].moreMenu(), 200); }
    if (S === 'mview' || S === 'desk') window.scrollTo(0, 0);
  }, 800));
})();
