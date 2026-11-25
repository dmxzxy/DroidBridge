// 投屏查看器：视频解码、控制、录制、性能浮层、AI 提示
// 由 web/index.html 拆分而来；经典脚本，按 index.html 中的顺序加载，共享全局作用域。
'use strict';

function avcCodecString(data) {
  for (let i = 0; i + 4 < data.length; i++) {
    if (data[i] === 0 && data[i + 1] === 0 && data[i + 2] === 1) {
      if ((data[i + 3] & 0x1f) === 7 && i + 6 < data.length) {
        const x = b => b.toString(16).padStart(2, '0');
        return 'avc1.' + x(data[i + 4]) + x(data[i + 5]) + x(data[i + 6]);
      }
      i += 2;
    }
  }
  return null;
}
const KEYMAP = { Enter: 66, Backspace: 67, Delete: 112, Tab: 61, ArrowUp: 19, ArrowDown: 20, ArrowLeft: 21, ArrowRight: 22, Home: 122, End: 123, PageUp: 92, PageDown: 93, Insert: 124 };
const AKEY = { BACK: 4, HOME: 3, APP_SWITCH: 187, POWER: 26, VOLUME_UP: 24, VOLUME_DOWN: 25 };

// 多台设备的排布：fit 按高度自适应（可缩放）| rows 双排 | wall 矩阵墙
const STAGE = {
  mode: pref.get('stage_mode', 'fit'),
  scale: Math.max(40, Math.min(100, parseInt(pref.get('stage_scale', '100'), 10) || 100)),
  compact: pref.get('stage_compact', '0') === '1',
};
// 只有一台投屏时总是标准排布、不缩放
const stageMode = () => viewers.size >= 2 ? STAGE.mode : 'fit';
const stageScale = () => viewers.size >= 2 ? STAGE.scale : 100;
const stageCompact = () => viewers.size >= 2 && (STAGE.compact || STAGE.mode !== 'fit') && !isMobile();
function fitAllViewers() {
  $('#grid').classList.toggle('mode-wall', stageMode() === 'wall');
  for (const v of viewers.values()) v.fit();
}
function allReadyDevices() {
  const out = [];
  for (const hst of hosts.values()) {
    if (hst.error) continue;
    for (const d of (hst.devices || [])) if (d.state === 'device') out.push({ hst, d });
  }
  return out;
}
function setStage(k, v) {
  STAGE[k] = v;
  pref.set('stage_' + k, typeof v === 'boolean' ? (v ? '1' : '0') : String(v));
  renderStageBar();
  fitAllViewers();
}

let stageSig = '';
/** 舞台工具条：多台设备时出现，只用图标 */
function renderStageBar() {
  const bar = $('#stageBar');
  const ready = allReadyDevices();
  const all = [...viewers.values()];
  const unopened = ready.filter(({ hst, d }) => !findViewer(hst, d)).length;
  const multi = all.length >= 2;
  const canOpenAll = unopened > 0 && ready.length >= 2;
  const show = !isMobile() && (multi || canOpenAll);
  // 状态没变就不重建，避免拖动缩放滑块时被定时刷新打断
  const sig = [show, multi, canOpenAll, STAGE.mode, STAGE.compact, all.length, unopened, LANG].join();
  if (sig === stageSig) return;
  stageSig = sig;
  const wasHidden = bar.hidden;
  bar.hidden = !show;
  if (wasHidden !== bar.hidden) setTimeout(fitAllViewers);
  if (!show) { bar.replaceChildren(); return; }

  const mode = (m, icon, tip) => ib(icon, tip, () => setStage('mode', m), STAGE.mode === m ? 'on' : '');
  const range = h('input', {
    type: 'range', class: 'stage-range', min: '40', max: '100', step: '5', value: String(STAGE.scale),
    title: t('画面缩放') + ' ' + STAGE.scale + '%',
    oninput: () => {
      STAGE.scale = +range.value;
      range.title = t('画面缩放') + ' ' + STAGE.scale + '%';
      pref.set('stage_scale', String(STAGE.scale));
      fitAllViewers();
    },
  });
  const fit = STAGE.mode === 'fit';
  const openAll = canOpenAll ? h('button', {
    class: 'ib', type: 'button', title: t('全部投屏'), 'aria-label': t('全部投屏'),
    onclick: () => { for (const { hst, d } of allReadyDevices()) if (!findViewer(hst, d)) openViewer(hst, d); },
  }, h('span', { html: svg('playAll') }).firstChild, h('i', { class: 'badge' }, String(unopened))) : null;
  const closeAll = multi ? ib('closeAll', t('全部关闭'), () => { for (const v of [...viewers.values()]) v.close(); }) : null;

  bar.replaceChildren(...[
    h('div', { class: 'stage-grow' }),
    openAll, closeAll,
    ...(multi ? [
      h('span', { class: 'stage-sep' }),
      // 缩放和紧凑边框只在标准排布下有意义；双排和矩阵墙总是紧凑
      fit ? range : null,
      fit ? ib('frame', t('紧凑边框：悬停时显示工具栏'), () => setStage('compact', !STAGE.compact), STAGE.compact ? 'on' : '') : null,
      h('div', { class: 'stage-seg' }, mode('fit', 'columns', t('标准')), mode('rows', 'lrows', t('双排')), mode('wall', 'lwall', t('矩阵墙'))),
    ] : []),
  ].filter(Boolean));
}

/** 观看者 id：随机 12 位 base36，同一 Viewer 重连时保持不变 */
const viewerVid = () => Array.from({ length: 12 }, () => Math.floor(Math.random() * 36).toString(36)).join('');
/** 观看者显示名称：本机用电脑名，其他浏览器用设置里的名称或“XX 上的浏览器” */
function viewerWho() {
  if (isLocalHost()) return (INFO && INFO.name) || '';
  const n = (pref.get('client_name', '') || '').trim();
  if (n) return n;
  const ua = navigator.userAgent;
  const guess = /iPhone|iPad/.test(ua) ? 'iPhone / iPad' : /Android/.test(ua) ? t('安卓手机') : /Mac/.test(ua) ? 'Mac' : /Windows/.test(ua) ? 'Windows' : t('浏览器');
  return t('{0} 上的浏览器', guess);
}

class Viewer {
  constructor(host, dev) {
    this.host = host; this.dev = dev; this.key = host.id + '|' + dev.serial;
    this.audioOn = pref.get('audio', '0') === '1'; this.fails = 0;
    this.vid = viewerVid(); this.forceNext = false; this.busyAt = 0;
    this.vw = 0; this.vh = 0; this.bytes = 0; this.frames = 0; this.lastKeyReq = 0;
    this.pointers = new Map();
    this.recording = false; this.recorder = null; this.recStream = null;
    this.recId = null; this.recExt = 'webm'; this.recQueue = Promise.resolve(); this.recSeq = 0;
    this.recTimer = null; this.recStart = 0; this.recFinishing = false;
    this.build();
    this.connect();
    this.statTimer = setInterval(() => this.updateStat(), 1000);
  }

  build() {
    const key = (code) => () => this.pressKey(code);
    this.canvas = h('canvas', { tabindex: '0', width: '360', height: '720' });
    this.ctx = this.canvas.getContext('2d');
    this.overlay = h('div', { class: 'overlay' });
    // 插件覆盖层：与画面同尺寸，按设备像素绘制；不接收鼠标事件
    this.pov = h('canvas', { class: 'pov', width: '1', height: '1' });
    this.dot = h('span', { class: 'dot warn' });
    this.nameEl = h('span', { class: 'name' }, typeof devDisplayName === 'function' ? devDisplayName(this.dev) : (this.dev.model || this.dev.serial));
    this.statEl = h('span', { class: 'fps' });
    // 观看者标记：眼睛 + 人数（只有自己时隐藏），悬停列出所有名称
    this.eyeNum = h('span', {}, '');
    this.eyeEl = h('span', { class: 'vwatch', hidden: true }, h('span', { class: 'ico', html: svg('eye') }), this.eyeNum);
    // 别人正在操作时，画面顶部的一条细提示
    this.ctlBar = h('div', { class: 'ctlbar', hidden: true });
    // 真正的键盘输入落在这个隐藏的 textarea 上（canvas 不能触发输入法）
    this.ime = h('textarea', { class: 'ime', autocomplete: 'off', autocorrect: 'off', autocapitalize: 'off', spellcheck: 'false', 'aria-label': t('键盘输入') });
    this.textInput = h('input', { class: 'input', placeholder: t('输入文字，回车发送'), enterkeyhint: 'send' });
    this.textInput.addEventListener('keydown', e => {
      if (e.key !== 'Enter' || e.isComposing) return;
      e.preventDefault();
      const v = this.textInput.value;
      if (v) { this.typeText(v); this.textInput.value = ''; } else this.pressKey(66);
    });
    this.typing = h('div', { class: 'typing' }, this.textInput);
    this.kbBtn = ib('keyboard', t('输入文字'), () => this.toggleTyping());
    this.fileInput = h('input', { type: 'file', multiple: true, hidden: true, onchange: () => { this.upload([...this.fileInput.files]); this.fileInput.value = ''; } });
    this.plugBtn = ib('puzzle', t('插件'), () => pluginDockMenu(this));
    this.plugBtn.hidden = true;
    this.plugPins = h('span', { class: 'plug-pins' });
    this.moreBtn = ib('more', t('更多'), () => this.moreMenu());
    this.logBtn = ib('logs', t('日志'), () => this.toggleLog());
    this.filesBtn = ib('folder', t('文件'), () => this.toggleFiles());
    this.audioBtn = ib('headphones', t('转发声音'), () => this.toggleAudio());
    this.audioBtn.classList.toggle('on', this.audioOn);
    this.recBtn = ib('record', t('录制屏幕'), () => this.toggleRecord());
    this.recEl = h('span', { class: 'rec', hidden: true });
    this.perfBtn = ib('activity', t('性能数据'), () => this.togglePerf());
    this.perfEl = h('div', { class: 'perf', hidden: true });
    this.aiLabel = h('span', { class: 'ai-l' });
    this.aiBtn = h('button', { type: 'button', onclick: () => this.setAgentPaused(!this.aiPaused) }, t('暂停'));
    this.aiBar = h('div', { class: 'aibar', hidden: true }, h('span', { class: 'ai-dot' }), h('b', {}, 'AI'), this.aiLabel, this.aiBtn);
    // 右侧面板：日志 / 文件 / 插件可同时开着，切换时保留各自状态；宽度可拖动
    this.dockW = parseInt(pref.get('dock_w', '0'), 10) || 0;
    this.activeDock = null;
    this.dockHidden = false;
    this.logWrap = h('div', { class: 'vdock-pane', hidden: true });
    this.filesWrap = h('div', { class: 'vdock-pane', hidden: true });
    this.plugWrap = h('div', { class: 'vdock-pane', hidden: true });
    this.dockBody = h('div', { class: 'vdock-body' }, this.logWrap, this.filesWrap, this.plugWrap);
    this.dockResizer = h('div', { class: 'vlog-resizer', title: t('拖拽调整侧栏宽度（双击重置）') });
    this.vlog = h('div', { class: 'vlog' }, this.dockResizer, this.dockBody);
    this.bindDockResizer();
    this.root = h('div', { class: 'viewer' },
      ctxMenu(h('div', { class: 'vhead' }, this.dot, this.nameEl, this.recEl, h('span', { class: 'sub' }, this.host.remote ? this.host.name : ''), this.statEl, this.eyeEl,
        this.plugPins, ib('max', t('全屏'), () => this.fullscreen()), ib('x', t('关闭'), () => this.close())), () => this.moreItems()),
      h('div', { class: 'vmain' },
      h('div', { class: 'vbody' },
        h('div', { class: 'vcol' },
          h('div', { class: 'screen' }, this.canvas, this.pov, this.ime, this.overlay, this.perfEl, this.aiBar, this.ctlBar, h('div', { class: 'drop' }, t('松开以安装 APK 或传到手机'))),
          h('div', { class: 'nav' }, ib('back', t('返回 (右键 / Esc)'), key(AKEY.BACK)), ib('home', t('主页 (中键)'), key(AKEY.HOME)), ib('recent', t('最近任务'), key(AKEY.APP_SWITCH))),
          this.typing),
        h('div', { class: 'vtools' },
          ib('power', t('电源键'), key(AKEY.POWER)), ib('volUp', t('音量 +'), key(AKEY.VOLUME_UP)), ib('volDown', t('音量 −'), key(AKEY.VOLUME_DOWN)),
          ib('rotate', t('旋转屏幕'), () => this.send({ t: 'cmd', name: 'rotate' })),
          h('div', { class: 'sep' }),
          ib('camera', t('截取屏幕'), () => this.screenshot()), this.recBtn, this.kbBtn, this.audioBtn, this.logBtn, this.filesBtn, this.plugBtn, this.perfBtn, this.moreBtn)),
      this.vlog),
      this.fileInput);
    this.bindInput();
    this.bindDrop();
  }

  moreMenu() { menu(this.moreBtn, this.moreItems()); }
  moreItems() {
    return [
      { icon: 'grid', label: t('应用管理…'), fn: () => appsDialog(this.host, this.dev) },
      { icon: 'folder', label: t('文件管理…'), fn: () => this.toggleFiles(true) },
      { icon: 'max', label: t('在新窗口中管理文件'), fn: () => filesDialog(this.host, this.dev) },
      '-',
      { icon: 'bell', label: t('下拉通知栏'), fn: () => this.send({ t: 'cmd', name: 'notif' }) },
      { icon: 'sliders', label: t('快捷设置'), fn: () => this.send({ t: 'cmd', name: 'settings' }) },
      '-',
      { icon: 'moon', label: t('关闭手机屏幕（投屏继续）'), fn: () => this.send({ t: 'power', on: false }) },
      { icon: 'sun', label: t('打开手机屏幕'), fn: () => this.send({ t: 'power', on: true }) },
      '-',
      { icon: 'paste', label: t('粘贴电脑剪贴板 (Ctrl+V)'), fn: () => this.paste() },
      { icon: 'upload', label: t('安装 APK / 传文件…'), fn: () => this.fileInput.click() },
      '-',
      this.dev.wireless
        ? { icon: 'wifiOff', label: t('断开无线连接'), fn: () => wirelessAction(this, '/api/adb/disconnect', t('正在断开…')) }
        : { icon: 'wifi', label: t('切换为无线连接'), fn: () => wirelessAction(this, '/api/adb/tcpip', t('正在切换为无线连接…')) },
      { icon: 'refresh', label: t('重新连接'), fn: () => this.reconnect() },
      { icon: 'x', label: t('关闭投屏'), fn: () => this.close() },
    ];
  }

  wsUrl() {
    let maxSize = +q('size'), bitRate = +q('rate'), maxFps = +q('fps');
    // 矩阵缩略墙或高密度小尺寸同屏时自动降档流参数，减轻多路解码负载
    if (!this.forceNext && (stageMode() === 'wall' || (viewers.size >= 4 && stageScale() <= 60))) {
      maxSize = maxSize > 0 ? Math.min(maxSize, 640) : 640;
      bitRate = Math.min(bitRate || 8000000, 1500000);
      maxFps = Math.min(maxFps || 60, 20);
    }
    const p = new URLSearchParams({ serial: this.dev.serial, peer: this.host.id, lang: LANG, max_size: String(maxSize), bit_rate: String(bitRate), max_fps: String(maxFps), stay_awake: pref.get('awake', '1'),
      audio: this.audioOn ? '1' : '0', audio_codec: AUDIO_CODEC, audio_dup: pref.get('audiodup', '0'),
      vid: this.vid, who: viewerWho(), force: this.forceNext ? '1' : '0' });
    this.forceNext = false;   // 只用于这一次连接
    const tk = pref.get('token', ''); if (tk) p.set('token', tk);
    return (location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/ws/stream?' + p;
  }

  connect() {
    this.closed = false; this.gotFrame = false; this.pendingConfig = null; this.needKey = true; this.lastError = '';
    this.showOverlay(t('正在连接…'), { spin: true });
    this.setDot('warn');
    const ws = this.ws = new WebSocket(this.wsUrl());
    ws.binaryType = 'arraybuffer';
    ws.onmessage = e => typeof e.data === 'string' ? this.onText(JSON.parse(e.data)) : this.onBinary(e.data);
    ws.onclose = () => {
      if (this.ws !== ws || this.closed) return;
      this.ws = null;
      this.onLost();
    };
  }
  /** 手动重连：清零失败计数；force 表示按我的画质设置重启共享会话 */
  reconnect(force) { if (force) this.forceNext = true; this.recovering = false; this.fails = 0; this.restart(); }
  restart() { this.teardown(); this.connect(); if (this.log) this.log.connect(); if (this.perfOn) { this.closePerf(); this.openPerf(); } }

  /** 连接断开后自动恢复：设备在线就重连（失败多次后停止），设备不在就等它回来 */
  async onLost() {
    if (this.recovering) return;
    if (this.recording) this.stopRecord();
    if (!this.gotFrame) this.fails++; else this.fails = 0;
    this.teardown();
    this.recovering = true;
    this.setDot('warn');
    const reason = this.lastError || t('连接已断开');
    const stop = () => { this.recovering = false; this.setDot('bad'); this.showOverlay(reason, { retry: true }); };
    const wait = (text) => this.showOverlay(text, { spin: true, extra: h('button', { class: 'btn', type: 'button', onclick: stop }, t('停止自动重连')) });
    let delay = Math.min(1000 * 2 ** Math.max(0, this.fails - 1), 8000);
    while (this.recovering && !this.closed) {
      wait(reason + '\n' + t('正在自动重连…'));
      await new Promise(r => setTimeout(r, delay));
      if (!this.recovering || this.closed) return;
      await refreshHost(this.host);
      renderSide();
      const d = matchDevice(this.host, this.dev);
      if (d && d.state === 'device') {
        if (this.fails >= 4) return stop();
        this.dev = Object.assign({}, this.dev, d);
        this.recovering = false;
        return this.restart();
      }
      wait(this.host.error ? t('「{0}」暂时无法连接，等待恢复…', this.host.name) : t('设备已断开，等待重新连接…'));
      delay = 2000;
    }
  }
  teardown() {
    if (this.ws) { const w = this.ws; this.ws = null; try { w.close(); } catch {} }
    if (this.decoder && this.decoder.state !== 'closed') try { this.decoder.close(); } catch {}
    this.decoder = null; this.decoderReady = false;
    if (this.pendingFrame) { this.pendingFrame.close(); this.pendingFrame = null; }
    if (this.audioOut) { this.audioOut.close(); this.audioOut = null; }
    // 连接断开后会话信息就过期了，等新会话的 viewers 消息再显示
    this.ctlBar.hidden = true;
    this.eyeEl.hidden = true;
  }

  toggleAudio() {
    this.audioOn = !this.audioOn;
    pref.set('audio', this.audioOn ? '1' : '0');
    this.audioBtn.classList.toggle('on', this.audioOn);
    if (this.audioOn) audioCtx();   // 在点击中创建，满足浏览器自动播放限制
    // 打开声音要让后端给共享会话开声音（重启一次）；关闭声音不用重启
    this.reconnect(this.audioOn);
  }
  close() {
    this.closed = true; this.recovering = false;
    this.perfOn = false; this.closePerf();
    if (this.recording) this.stopRecord();
    if (this.log) this.log.destroy();
    if (this.files) this.files.destroy();
    pluginViewerDestroy(this);
    this.teardown();
    clearInterval(this.statTimer);
    if (document.fullscreenElement === this.root) document.exitFullscreen();
    this.root.remove();
    viewers.delete(this.key);
    afterViewersChange();
  }

  setDot(c) { this.dot.className = 'dot ' + c; }
  showOverlay(text, { spin, retry, extra } = {}) {
    this.overlay.replaceChildren(...[spin ? h('div', { class: 'spin' }) : null, h('div', { class: 'ovtext' }, text),
      retry ? h('button', { class: 'btn', type: 'button', onclick: () => this.reconnect() }, t('重新连接')) : null, extra].filter(Boolean));
    this.overlay.classList.remove('hide');
  }

  bindDockResizer() {
    const rz = this.dockResizer;
    rz.addEventListener('dblclick', () => { this.dockW = 0; localStorage.removeItem('db_dock_w'); this.fit(); });
    rz.addEventListener('pointerdown', e => {
      if (e.button !== 0 || isMobile()) return;
      e.preventDefault();
      rz.setPointerCapture(e.pointerId);
      this.root.classList.add('resizing-dock');
      const startX = e.clientX;
      const startW = this.vlog.getBoundingClientRect().width || 420;
      const onMove = ev => {
        const w = Math.max(280, Math.min(1000, Math.round(startW + (startX - ev.clientX))));
        this.dockW = w;
        pref.set('dock_w', String(w));
        this.fit();
      };
      const onUp = () => {
        this.root.classList.remove('resizing-dock');
        rz.removeEventListener('pointermove', onMove);
        rz.removeEventListener('pointerup', onUp);
        rz.removeEventListener('pointercancel', onUp);
      };
      rz.addEventListener('pointermove', onMove);
      rz.addEventListener('pointerup', onUp);
      rz.addEventListener('pointercancel', onUp);
    });
  }

  hasOpenDock() {
    return !this.dockHidden && !!this.activeDock && !!(
      (this.activeDock === 'log' && this.log) ||
      (this.activeDock === 'files' && this.files) ||
      (this.activeDock === 'plug' && this.plugDock)
    );
  }

  syncDockUI() {
    const open = this.hasOpenDock();
    this.root.classList.toggle('with-log', open);
    this.logWrap.hidden = !(open && this.activeDock === 'log');
    this.filesWrap.hidden = !(open && this.activeDock === 'files');
    this.plugWrap.hidden = !(open && this.activeDock === 'plug');
    // 工具列的按钮就是页签：当前面板高亮，后台开着的面板带圆点
    for (const [btn, k, alive] of [[this.logBtn, 'log', this.log], [this.filesBtn, 'files', this.files], [this.plugBtn, 'plug', this.plugDock]]) {
      btn.classList.toggle('on', open && this.activeDock === k);
      btn.classList.toggle('bg', !!alive && !(open && this.activeDock === k));
    }
  }

  activateDock(kind) {
    this.activeDock = kind;
    this.dockHidden = false;
    this.syncDockUI();
    this.fit();
  }

  closeDockTab(kind) {
    if (kind === 'log' && this.log) { this.log.destroy(); this.log = null; this.logWrap.replaceChildren(); }
    if (kind === 'files' && this.files) { this.files.destroy(); this.files = null; this.filesWrap.replaceChildren(); }
    if (kind === 'plug' && this.plugDock) { pluginDockClose(this); return; }
    if (this.activeDock === kind) {
      this.activeDock = this.log ? 'log' : this.files ? 'files' : this.plugDock ? 'plug' : null;
    }
    this.syncDockUI();
    this.fit();
  }

  onPlugDockClosed() {
    this.plugWrap.replaceChildren();
    if (this.activeDock === 'plug') {
      this.activeDock = this.log ? 'log' : this.files ? 'files' : null;
    }
    this.syncDockUI();
    this.fit();
  }

  toggleLog(force) {
    if (force === false) { this.closeDockTab('log'); return; }
    if (force === true || !this.log) {
      if (!this.log) { this.log = new LogPanel(this); this.logWrap.replaceChildren(this.log.el); }
      this.activateDock('log');
      return;
    }
    if (this.activeDock === 'log' && !this.dockHidden) {
      this.dockHidden = true; this.syncDockUI(); this.fit();
    } else {
      this.activateDock('log');
    }
  }
  toggleFiles(force) {
    if (force === false) { this.closeDockTab('files'); return; }
    if (force === true || !this.files) {
      if (!this.files) {
        this.files = new FileBrowser(this.host, this.dev, { compact: true, onClose: () => this.closeDockTab('files') });
        this.filesWrap.replaceChildren(this.files.el);
      }
      this.activateDock('files');
      return;
    }
    if (this.activeDock === 'files' && !this.dockHidden) {
      this.dockHidden = true; this.syncDockUI(); this.fit();
    } else {
      this.activateDock('files');
    }
  }

  // 桌面端布局：支持标准比例 / 双排紧凑 / 多机矩阵墙 + 自由缩放 + 侧栏自定义拖拽宽度
  fit() {
    const r = this.root;
    const compact = stageCompact();
    r.classList.toggle('compact-chrome', compact);
    const chromeW = compact ? 2 : 46;
    if (this.pov) pluginOverlayFit(this);
    if (!this.vw || !this.vh || isMobile()) {
      r.style.width = ''; this.canvas.style.maxHeight = ''; this.vlog.style.width = '';
      r.classList.remove('log-bottom'); return;
    }
    const avail = Math.max(240, $('main').clientWidth - 32);
    const sb = document.body.classList.contains('has-sb') ? 24 : 0;
    const tb = hasTitlebar() ? 36 : 0;
    const bar = $('#stageBar');
    const barH = bar.hidden ? 0 : bar.offsetHeight + 8;
    const fullH = Math.max(180, innerHeight - 150 - sb - tb - barH);

    let maxH = fullH;
    let cw;
    const mode = stageMode();
    if (mode === 'wall') {
      const cols = avail >= 1560 ? 6 : avail >= 1240 ? 5 : avail >= 920 ? 4 : 3;
      const cellW = Math.floor((avail - (cols - 1) * 10) / cols) - chromeW;
      cw = Math.max(140, cellW);
      maxH = Math.round(cw * this.vh / this.vw);
    } else if (mode === 'rows') {
      maxH = Math.max(220, Math.floor((innerHeight - 130 - sb - tb - barH) / 2) - 48);
      cw = Math.min(this.vw, maxH * this.vw / this.vh);
    } else {
      maxH = Math.round(fullH * (stageScale() / 100));
      cw = Math.min(this.vw, maxH * this.vw / this.vh);
    }
    this.canvas.style.maxHeight = maxH + 'px';

    if (!this.hasOpenDock()) {
      r.classList.remove('log-bottom');
      const minCardW = compact ? 120 : 240;
      r.style.width = Math.round(Math.max(minCardW, Math.min(cw + chromeW, avail))) + 'px';
      return;
    }
    const side = avail - (cw + chromeW);
    const portrait = this.vh >= this.vw;
    if (side >= 320 || portrait || avail - chromeW - 360 >= cw * 0.7) {
      const autoW = Math.round(Math.max(side >= 320 ? Math.min(side, 900) : 380, 320));
      const lw = this.dockW ? Math.min(Math.max(280, this.dockW), Math.max(280, avail - 200)) : autoW;
      r.classList.remove('log-bottom');
      this.vlog.style.width = lw + 'px';
      r.style.width = Math.round(Math.min(avail, cw + chromeW + lw)) + 'px';
    } else {
      r.classList.add('log-bottom');
      this.vlog.style.width = '';
      r.style.width = Math.round(avail) + 'px';
    }
  }

  onText(m) {
    switch (m.type) {
      case 'status': if (!this.gotFrame) this.showOverlay(m.msg, { spin: true }); break;
      case 'viewers': this.onViewers(m); break;
      case 'busy': this.onBusy(m); break;
      case 'meta': if (m.device_name && !(typeof devAlias === 'function' && devAlias(this.dev.serial))) this.nameEl.textContent = m.device_name; this.showOverlay(t('等待画面…'), { spin: true }); break;
      case 'session':
        this.vw = m.width; this.vh = m.height;
        if (this.canvas.width !== m.width || this.canvas.height !== m.height) { this.canvas.width = m.width; this.canvas.height = m.height; }
        this.fit();
        break;
      case 'error': this.lastError = m.msg; break;
      case 'audio':
        if (m.state === 'on') { if (this.audioOut) this.audioOut.close(); this.audioOut = new AudioOut(m.codec === 'raw' ? 'raw' : 'opus'); this.audioBtn.title = t('转发声音'); }
        else { toast(m.msg, { ms: 5000 }); this.audioBtn.title = m.msg; this.audioBtn.classList.remove('on'); }
        break;
      case 'log': console.log('[' + this.dev.serial + ']', m.msg); break;
      case 'agent': this.onAgent(m); break;
      case 'clipboard':
        if (navigator.clipboard && isSecureContext) navigator.clipboard.writeText(m.text).then(() => toast(t('已同步手机剪贴板')), () => {});
        break;
    }
  }

  /** 谁在看、谁在操作 */
  onViewers(m) {
    if (m.you) this.vid = m.you;
    const list = Array.isArray(m.list) ? m.list : [];
    const name = x => (x && x.name) || '?';
    this.eyeNum.textContent = String(list.length);
    this.eyeEl.hidden = list.length < 2;
    const ctl = m.controller && m.controller !== 'ai' && m.controller !== this.vid ? (m.controller_name || '?') : null;
    this.eyeEl.title = list.filter(x => x && x.id !== this.vid).map(name).join('\n');
    // AI 控制时不显示这条（已经有 AI 提示条了）
    this.ctlBar.hidden = !ctl;
    if (ctl) this.ctlBar.textContent = t('{0} 正在操作', ctl);
  }
  /** 别人正在操作，这条输入被丢弃：同一 Viewer 3 秒内最多提示一次 */
  onBusy(m) {
    const now = Date.now();
    if (now - this.busyAt < 3000) return;
    this.busyAt = now;
    toast(t('{0} 正在操作', (m && m.by) || '?'), { ms: 2000 });
  }

  ensureDecoder() {
    if (this.decoder && this.decoder.state !== 'closed') return;
    this.decoder = new VideoDecoder({
      output: f => this.onFrame(f),
      error: () => { this.decoderReady = false; this.needKey = true; this.requestKeyFrame(); },
    });
  }
  requestKeyFrame() {
    const now = performance.now();
    if (now - this.lastKeyReq < 1000) return;
    this.lastKeyReq = now;
    this.send({ t: 'cmd', name: 'reset_video' });
  }
  onBinary(buf) {
    if (!window.VideoDecoder) return;
    this.bytes += buf.byteLength;
    const dv = new DataView(buf);
    const flags = dv.getUint8(0);
    const pts = Number(dv.getBigUint64(1));
    let data = new Uint8Array(buf, 9);
    if (flags & 4) { if (this.audioOut) this.audioOut.packet(!!(flags & 1), pts, data); return; }
    if (flags & 1) {
      this.pendingConfig = data.slice();
      this.ensureDecoder();
      try { this.decoder.configure({ codec: avcCodecString(data) || 'avc1.42E01F', optimizeForLatency: true }); this.decoderReady = true; }
      catch (e) { this.showOverlay(t('解码器不支持此视频：') + e.message, { retry: true }); }
      return;
    }
    let isKey = !!(flags & 2);
    if (this.pendingConfig) {
      const merged = new Uint8Array(this.pendingConfig.length + data.length);
      merged.set(this.pendingConfig); merged.set(data, this.pendingConfig.length);
      data = merged; isKey = true; this.pendingConfig = null;
    }
    if (!this.decoderReady) { this.requestKeyFrame(); return; }
    if (this.needKey && !isKey) { this.requestKeyFrame(); return; }
    if (!isKey && this.decoder.decodeQueueSize > 8) { this.needKey = true; this.requestKeyFrame(); return; }
    this.needKey = false;
    try { this.decoder.decode(new EncodedVideoChunk({ type: isKey ? 'key' : 'delta', timestamp: pts, data })); }
    catch { this.needKey = true; this.requestKeyFrame(); }
  }
  onFrame(frame) {
    if (this.pendingFrame) this.pendingFrame.close();
    this.pendingFrame = frame;
    if (!this.raf) this.raf = requestAnimationFrame(() => this.draw());
  }
  draw() {
    this.raf = 0;
    const f = this.pendingFrame; if (!f) return;
    this.pendingFrame = null;
    const w = f.displayWidth, ht = f.displayHeight;
    if (this.canvas.width !== w || this.canvas.height !== ht) { this.canvas.width = w; this.canvas.height = ht; this.vw = w; this.vh = ht; this.fit(); }
    if (!this.vw) { this.vw = w; this.vh = ht; }
    this.ctx.drawImage(f, 0, 0, w, ht);
    if (this.pov) pluginOverlayPaint(this);
    f.close();
    this.frames++;
    if (!this.gotFrame) { this.gotFrame = true; this.fails = 0; this.overlay.classList.add('hide'); this.setDot('ok'); }
  }
  updateStat() {
    if (!this.gotFrame) { this.statEl.textContent = ''; return; }
    this.statEl.textContent = this.frames + ' fps';
    this.statEl.title = `${this.vw}×${this.vh} · ${(this.bytes * 8 / 1e6).toFixed(1)} Mbps`;
    this.frames = 0; this.bytes = 0;
  }

  /** 把文字输入到设备：ASCII 直接注入按键，其他文字（中文、emoji…）通过剪贴板粘贴 */
  typeText(text) {
    if (!text) return;
    if (/^[ -~]*$/.test(text)) { this.send({ t: 'text', text }); return; }
    this.send({ t: 'clipboard', text, paste: true });
  }
  sendRaw(obj) { if (this.ws && this.ws.readyState === 1) this.ws.send(JSON.stringify(obj)); }
  send(obj) { this.sendRaw(obj); }
  /** 群控同步到这台设备时，在落点显示扩散圈（fx/fy 为画面比例坐标） */
  syncMark(fx, fy) {
    const sr = this.canvas.parentElement.getBoundingClientRect(), cr = this.canvas.getBoundingClientRect();
    if (!cr.width || !(fx >= 0 && fx <= 1 && fy >= 0 && fy <= 1)) return;
    const el = h('div', { class: 'aitap synctap' });
    el.style.left = (cr.left - sr.left + fx * cr.width) + 'px';
    el.style.top = (cr.top - sr.top + fy * cr.height) + 'px';
    this.canvas.parentElement.append(el);
    setTimeout(() => el.remove(), 600);
  }

  // ---- AI Agent 操作提示 ----
  onAgent(m) {
    if (m.action === 'paused' || m.action === 'resumed') { this.aiPaused = m.action === 'paused'; this.showAgent(); return; }
    if (m.action === 'blocked') { this.aiPaused = true; this.showAgent(t('AI 想继续操作，已被暂停')); return; }
    if (m.action === 'tap' && m.x != null) this.aiMark(m.x, m.y);
    if (m.action === 'swipe' && m.x != null) this.aiMark(m.x, m.y, m.x2, m.y2);
    this.showAgent(m.label || '');
  }
  showAgent(label) {
    if (label !== undefined) this.aiLabel.textContent = label;
    this.aiBar.hidden = false;
    this.aiBar.classList.toggle('paused', !!this.aiPaused);
    this.aiBtn.textContent = this.aiPaused ? t('继续') : t('暂停');
    if (this.aiPaused && !label) this.aiLabel.textContent = t('已暂停，AI 只能读取画面');
    this.root.classList.toggle('ai-active', !this.aiPaused);
    clearTimeout(this.aiTimer);
    // 暂停时一直显示；否则最后一次操作 15 秒后隐藏
    if (!this.aiPaused) this.aiTimer = setTimeout(() => { this.aiBar.hidden = true; this.root.classList.remove('ai-active'); }, 15000);
  }
  async setAgentPaused(p) {
    try {
      await api('/api/agent/pause?peer=' + encodeURIComponent(this.host.id), { method: 'POST', body: JSON.stringify({ serial: this.dev.serial, paused: p }) });
      this.aiPaused = p; this.showAgent(p ? undefined : t('已继续'));
    } catch (e) { toast(e.message, { kind: 'bad' }); }
  }
  /** 在画面上标出 AI 的点击（或滑动）位置；x、y 为相对画面的比例 */
  aiMark(x, y, x2, y2) {
    const sr = this.canvas.parentElement.getBoundingClientRect(), cr = this.canvas.getBoundingClientRect();
    if (!cr.width) return;
    const at = (fx, fy) => [cr.left - sr.left + fx * cr.width, cr.top - sr.top + fy * cr.height];
    const [l, tp] = at(x, y);
    const el = h('div', { class: 'aitap' + (x2 != null ? ' move' : '') });
    el.style.left = l + 'px'; el.style.top = tp + 'px';
    this.canvas.parentElement.append(el);
    if (x2 != null) {
      const [l2, t2] = at(x2, y2);
      requestAnimationFrame(() => requestAnimationFrame(() => { el.style.left = l2 + 'px'; el.style.top = t2 + 'px'; el.style.opacity = '0'; }));
    }
    setTimeout(() => el.remove(), 1200);
  }

  togglePerf() {
    this.perfOn = !this.perfOn;
    this.perfBtn.classList.toggle('on', this.perfOn);
    this.perfEl.hidden = !this.perfOn;
    this.closePerf();
    if (this.perfOn) { this.perfHist = []; pluginPerfStart(this); this.perfEl.replaceChildren(h('div', { class: 'pk' }, t('正在读取性能数据…'))); this.openPerf(); }
    else pluginPerfStop(this);
  }
  closePerf() { if (this.perfWs) { const w = this.perfWs; this.perfWs = null; try { w.close(); } catch {} } clearTimeout(this.perfRetry); }
  openPerf() {
    const p = new URLSearchParams({ serial: this.dev.serial, peer: this.host.id, lang: LANG });
    const tk = pref.get('token', ''); if (tk) p.set('token', tk);
    const ws = this.perfWs = new WebSocket((location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/ws/perf?' + p);
    ws.onmessage = e => {
      if (this.perfWs !== ws) return;
      const m = JSON.parse(e.data);
      if (m.type === 'perf') this.renderPerf(m);
      else if (m.msg) this.perfEl.replaceChildren(h('div', { class: 'pk err' }, m.msg));
    };
    // 断开后（设备暂时断线等）每 3 秒重试，直到关闭浮层
    ws.onclose = () => {
      if (this.perfWs !== ws) return;
      this.perfWs = null;
      if (this.perfOn && !this.closed) this.perfRetry = setTimeout(() => { if (this.perfOn && !this.perfWs && !this.closed) this.openPerf(); }, 3000);
    };
  }
  renderPerf(m) {
    const hist = this.perfHist;
    if (m.fps != null) { hist.push(m.fps); if (hist.length > 40) hist.shift(); }
    const n = v => v == null ? '–' : (v < 10 && v % 1 ? v.toFixed(1) : Math.round(v));
    const row = (k, v, cls) => h('div', { class: 'pr' + (cls ? ' ' + cls : '') }, h('span', { class: 'pl' }, k), h('span', {}, v));
    const pair = (a, b) => [a, b].filter(Boolean).join(' · ') || '–';
    let spark = null;
    if (hist.length > 1) {
      const max = Math.max(60, ...hist), w = 60, ht = 14, step = w / 39;
      const pts = hist.map((v, i) => (w - (hist.length - 1 - i) * step).toFixed(1) + ',' + (ht - 1 - v / max * (ht - 2)).toFixed(1)).join(' ');
      spark = h('span', { html: `<svg viewBox="0 0 ${w} ${ht}" fill="none" stroke="#7aa7ff" stroke-width="1.2" stroke-linejoin="round"><polyline points="${pts}"/></svg>` }).firstChild;
    }
    const temp = m.cpu_temp ?? m.batt_temp;
    this.perfEl.replaceChildren(...[
      h('div', { class: 'pk' }, m.pkg || '—'),
      h('div', { class: 'pr' }, h('span', { class: 'pl' }, 'FPS'), h('b', {}, n(m.fps)), spark),
      row('CPU', pair(m.app_cpu != null && t('应用') + ' ' + n(m.app_cpu) + '%', m.cpu != null && t('整机') + ' ' + n(m.cpu) + '%')),
      row(t('内存'), pair(m.app_mem != null && t('应用') + ' ' + m.app_mem + ' MB', m.mem_total && t('整机') + ' ' + Math.round(m.mem_used / m.mem_total * 100) + '%')),
      row(t('温度'), pair(m.cpu_temp != null && 'CPU ' + Math.round(m.cpu_temp) + '°C', m.batt_temp != null && t('电池') + ' ' + m.batt_temp.toFixed(1) + '°C'), temp >= 55 ? 'bad' : temp >= 45 ? 'warn' : ''),
      row(t('电量'), m.level == null ? '–' : m.level + '%' + (m.charging ? ' · ' + t('充电中') : ''), m.level != null && m.level <= 15 && !m.charging ? 'warn' : ''),
      // 插件指标（每 2 秒拉一次 /api/plugins/metrics）
      this.plugPerfBox || null,
    ].filter(Boolean));
  }
  pressKey(code, meta = 0) { this.send({ t: 'key', action: 0, code, meta }); this.send({ t: 'key', action: 1, code, meta }); }
  toggleTyping() {
    const show = !this.typing.classList.contains('show');
    this.typing.classList.toggle('show', show);
    this.kbBtn.classList.toggle('on', show);
    if (show) this.textInput.focus(); else this.canvas.focus({ preventScroll: true });
  }
  async paste() {
    let text = null;
    try { if (navigator.clipboard && isSecureContext) text = await navigator.clipboard.readText(); } catch {}
    if (text === null) text = await promptDialog(t('粘贴到手机'), t('浏览器无法读取剪贴板，请在这里粘贴内容。'), { okText: t('发送') });
    if (text) this.send({ t: 'clipboard', text, paste: true });
  }
  async screenshot() {
    if (!this.gotFrame) return;
    this.canvas.toBlob(async b => {
      if (!b) return;
      try {
        const device = this.dev.model || this.dev.serial;
        const item = await api('/api/media/shot?device=' + encodeURIComponent(device), { method: 'POST', headers: { 'content-type': 'image/png' }, body: b });
        // 只保留一条带“查看”按钮的提示
        galleryHint(item, this.canvas, b);
      } catch (e) { toast(t('截图失败：') + e.message, { kind: 'bad', ms: 4000 }); }
    }, 'image/png');
  }
  fullscreen() {
    if (document.fullscreenElement) document.exitFullscreen();
    else if (this.root.requestFullscreen) this.root.requestFullscreen().catch(() => {});
  }

  // ---------------- 录屏 ----------------
  toggleRecord() {
    if (this.recording) this.stopRecord();
    else this.startRecord();
  }

  async startRecord() {
    if (!this.gotFrame || this.recording || this.recFinishing) return;
    if (!window.MediaRecorder) { toast(t('当前浏览器不支持录屏')); return; }
    let mime = null, ext = 'webm';
    const cands = [
      ['video/mp4;codecs=avc1', 'mp4'], ['video/mp4', 'mp4'],
      ['video/webm;codecs=vp9,opus', 'webm'], ['video/webm', 'webm'],
    ];
    for (const [m, e] of cands) { if (MediaRecorder.isTypeSupported(m)) { mime = m; ext = e; break; } }
    if (!mime) { toast(t('当前浏览器不支持录屏')); return; }

    let recId;
    try {
      const device = this.dev.model || this.dev.serial;
      const r = await api('/api/media/rec/start?device=' + encodeURIComponent(device) + '&ext=' + ext, { method: 'POST', body: '{}' });
      recId = r.id;
    } catch (e) { toast(e.message, { kind: 'bad' }); return; }

    const stream = this.canvas.captureStream(30);
    if (this.audioOut && this.audioOut.capture) {
      this.audioOut.capture.stream.getAudioTracks().forEach(tr => stream.addTrack(tr));
    }
    this.recStream = stream;
    this.recId = recId; this.recExt = ext;
    this.recQueue = Promise.resolve(); this.recSeq = 0;
    this.recFinishing = false;
    this.recFailed = false;

    this.recorder = new MediaRecorder(stream, { mimeType: mime });
    this.recorder.ondataavailable = e => {
      if (e.data && e.data.size) {
        const seq = this.recSeq++;
        this.recQueue = this.recQueue.then(() => this.uploadChunk(seq, e.data)).catch(err => {
          // 任一分块失败就停止，保存已上传的部分
          if (!this.recFailed) {
            this.recFailed = true;
            this.stopRecord();
          }
        });
      }
    };
    this.recorder.onerror = () => { this.abortRecord(); };
    this.recorder.start(1000);

    this.recording = true;
    this.recBtn.classList.add('rec');
    this.recBtn.innerHTML = svg('stop');
    this.recBtn.title = t('停止录屏');
    this.recStart = Date.now();
    this.updateRecTimer();
    this.recTimer = setInterval(() => this.updateRecTimer(), 1000);
    toast(t('开始录屏'));
  }

  uploadChunk(seq, blob) {
    return api('/api/media/rec/' + encodeURIComponent(this.recId) + '?seq=' + seq, {
      method: 'POST', headers: { 'content-type': 'application/octet-stream' }, body: blob,
    });
  }

  updateRecTimer() {
    if (!this.recording) return;
    const s = Math.floor((Date.now() - this.recStart) / 1000);
    const mm = String(Math.floor(s / 60)).padStart(2, '0');
    const ss = String(s % 60).padStart(2, '0');
    this.recEl.textContent = mm + ':' + ss;
    this.recEl.hidden = false;
  }

  async stopRecord() {
    if (!this.recording && !this.recFinishing) return;
    this.recording = false;
    clearInterval(this.recTimer); this.recTimer = null;
    this.recBtn.classList.remove('rec');
    this.recBtn.innerHTML = svg('record');
    this.recBtn.title = t('录制屏幕');
    this.recEl.hidden = true;
    const rec = this.recorder;
    if (!rec) { this.recFinishing = false; return; }
    this.recFinishing = true;
    await new Promise(res => { rec.onstop = res; try { rec.stop(); } catch { res(); } });
    // 等所有分块上传完
    await this.recQueue;
    const id = this.recId;
    this.recorder = null; this.recId = null;
    if (this.recStream) { this.recStream.getTracks().forEach(t => t.stop()); this.recStream = null; }
    const failed = this.recFailed;
    this.recFailed = false;
    try {
      const item = await api('/api/media/rec/' + encodeURIComponent(id) + '/finish', { method: 'POST', body: '{}' });
      if (failed) toast(t('录屏上传失败，已保存已上传的部分'), { kind: 'bad', ms: 5000 });
      else galleryHint(item);
    } catch (e) { toast(t('录屏保存失败：') + e.message, { kind: 'bad', ms: 4000 }); }
    this.recFinishing = false;
  }

  abortRecord() {
    if (!this.recId) return;
    const id = this.recId; this.recId = null;
    this.recording = false; this.recFinishing = false;
    clearInterval(this.recTimer); this.recTimer = null;
    this.recBtn.classList.remove('rec');
    this.recBtn.innerHTML = svg('record');
    this.recBtn.title = t('录制屏幕');
    this.recEl.hidden = true;
    if (this.recorder) { try { this.recorder.stop(); } catch {} this.recorder = null; }
    if (this.recStream) { this.recStream.getTracks().forEach(t => t.stop()); this.recStream = null; }
    api('/api/media/rec/' + encodeURIComponent(id), { method: 'DELETE' }).catch(() => {});
  }

  async upload(files) {
    for (const f of files) {
      const apk = /\.apk$/i.test(f.name);
      const tt = toast((apk ? t('正在安装 ') : t('正在传输 ')) + f.name + '…', { sticky: true });
      try {
        const qs = new URLSearchParams({ serial: this.dev.serial, peer: this.host.id, name: f.name, lang: LANG });
        const r = await fetch(withToken('/api/push?' + qs), { method: 'POST', body: f });
        const j = await r.json().catch(() => ({}));
        if (!r.ok) throw new Error(j.error || ('HTTP ' + r.status));
        tt.set(j.msg || t('完成'), 'ok');
      } catch (e) { tt.set((apk ? t('安装失败：') : t('传输失败：')) + e.message, 'bad'); }
      tt.close(4500);
    }
  }
  bindDrop() {
    // 只有拖到画面区域才是“安装 APK / 传到手机”；拖到文件面板等其他区域由它们自己处理
    const zone = this.root.querySelector('.screen');
    const r = this.root;
    let depth = 0;
    const isFiles = e => e.dataTransfer && [...e.dataTransfer.types].includes('Files');
    zone.addEventListener('dragenter', e => { if (isFiles(e)) { depth++; r.classList.add('dropping'); } });
    zone.addEventListener('dragleave', () => { if (--depth <= 0) { depth = 0; r.classList.remove('dropping'); } });
    zone.addEventListener('dragover', e => { if (isFiles(e)) { e.preventDefault(); e.dataTransfer.dropEffect = 'copy'; } });
    // 拖放被取消（Esc、拖出窗口）时浏览器不一定发 dragleave：任何地方 drop / dragend 后都清掉提示
    const reset = () => { depth = 0; r.classList.remove('dropping'); };
    addEventListener('drop', reset, true);
    addEventListener('dragend', reset, true);
    document.addEventListener('dragleave', e => { if (!e.relatedTarget) reset(); });
    zone.addEventListener('drop', e => {
      if (!isFiles(e)) return;
      e.preventDefault(); e.stopPropagation(); depth = 0; r.classList.remove('dropping');
      const files = [...(e.dataTransfer.files || [])];
      if (files.length) this.upload(files);
    });
  }

  pos(e) {
    const r = this.canvas.getBoundingClientRect();
    const x = Math.round((e.clientX - r.left) / r.width * this.vw);
    const y = Math.round((e.clientY - r.top) / r.height * this.vh);
    return { x: Math.max(0, Math.min(this.vw - 1, x)), y: Math.max(0, Math.min(this.vh - 1, y)), w: this.vw, h: this.vh };
  }
  bindInput() {
    const c = this.canvas;
    const pid = e => e.pointerType === 'mouse' ? -2 : (e.pointerId % 0x7fffffff);
    const touch = (e, action) => {
      if (!this.vw) return;
      this.send({ t: 'touch', action, id: pid(e), ...this.pos(e), p: action === 1 ? 0 : (e.pressure || 1) });
    };
    c.addEventListener('pointerdown', e => {
      if (e.pointerType === 'mouse' && e.button !== 0) return;
      e.preventDefault();
      if (!isMobile()) this.ime.focus({ preventScroll: true }); else c.focus({ preventScroll: true });
      c.setPointerCapture(e.pointerId);
      this.pointers.set(e.pointerId, true);
      touch(e, 0);
    });
    c.addEventListener('pointermove', e => {
      if (!this.pointers.has(e.pointerId)) return;
      e.preventDefault();
      const evs = e.getCoalescedEvents ? e.getCoalescedEvents() : [];
      for (const ev of (evs.length ? evs : [e])) touch(ev, 2);
    });
    const up = e => { if (!this.pointers.has(e.pointerId)) return; this.pointers.delete(e.pointerId); touch(e, 1); };
    c.addEventListener('pointerup', up);
    c.addEventListener('pointercancel', up);
    c.addEventListener('contextmenu', e => { e.preventDefault(); this.send({ t: 'back', action: 0 }); this.send({ t: 'back', action: 1 }); });
    c.addEventListener('auxclick', e => { if (e.button === 1) { e.preventDefault(); this.pressKey(AKEY.HOME); } });
    c.addEventListener('wheel', e => {
      if (!this.vw) return;
      e.preventDefault();
      const k = e.deltaMode === 1 ? 1 / 3 : 1 / 100, cl = v => Math.max(-16, Math.min(16, v));
      this.send({ t: 'scroll', ...this.pos(e), hs: cl(-e.deltaX * k), vs: cl(-e.deltaY * k) });
    }, { passive: false });
    const down = (e) => {
      if (e.isComposing || e.keyCode === 229) return;   // 输入法正在组词
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'v') { e.preventDefault(); this.paste(); return; }
      if (e.key === 'Escape' && !document.fullscreenElement) { e.preventDefault(); this.pressKey(AKEY.BACK); return; }
      if (e.ctrlKey && /^[a-z]$/i.test(e.key)) { e.preventDefault(); this.pressKey(29 + e.key.toLowerCase().charCodeAt(0) - 97, 0x3000); return; }
      const code = KEYMAP[e.key];
      if (code) {
        e.preventDefault();
        this.send({ t: 'key', action: 0, code, repeat: e.repeat ? 1 : 0, meta: e.shiftKey ? 1 : 0 });
        return true;
      }
      return false;
    };
    const keyUp = (e) => {
      const code = KEYMAP[e.key];
      if (code) { e.preventDefault(); this.send({ t: 'key', action: 1, code, meta: e.shiftKey ? 1 : 0 }); }
    };
    // 设备端输入框里输入文字：把已确定的文字发过去并清空，避免越积越多
    const flush = () => {
      const v = this.ime.value;
      if (!v) return;
      this.ime.value = '';
      this.typeText(v);
    };
    c.addEventListener('keydown', e => { if (down(e) && !isMobile()) return; });
    c.addEventListener('keyup', keyUp);
    const ime = this.ime;
    ime.addEventListener('keydown', down);
    ime.addEventListener('keyup', keyUp);
    ime.addEventListener('input', (e) => { if (!e.isComposing) flush(); });
    // 输入法组词结束后补一次（部分输入法只发 compositionend）
    ime.addEventListener('compositionend', () => setTimeout(flush, 0));
    // 没有输入法时（例如直接敲键盘）也要把焦点留在 ime 上
    // 焦点落到画面本身时交给 ime（键盘、输入法都从这里进入）
    c.addEventListener('focus', () => { if (!isMobile()) ime.focus({ preventScroll: true }); });
  }
}

function openViewer(hst, dev) {
  const key = hst.id + '|' + dev.serial;
  if (isMobile()) document.body.classList.add('viewing');
  const existing = findViewer(hst, dev);
  if (existing) { existing.root.scrollIntoView({ behavior: 'smooth', block: 'nearest' }); return; }
  if (!window.VideoDecoder) { showNotice(); toast(t('当前浏览器无法解码视频')); document.body.classList.remove('viewing'); return; }
  const v = new Viewer(hst, dev);
  viewers.set(key, v);
  pluginViewerUpdate(v);
  $('#grid').append(v.root);
  afterViewersChange();
  history.replaceState(null, '', location.search + '#open=' + encodeURIComponent(hst.id) + '|' + encodeURIComponent(dev.serial));
  if (isMobile()) scrollTo(0, 0);
}

function afterViewersChange() {
  $('#placeholder').style.display = viewers.size ? 'none' : '';
  if (!viewers.size) { document.body.classList.remove('viewing'); history.replaceState(null, '', location.pathname + location.search); }
  renderSide();
  for (const v of viewers.values()) pluginViewerUpdate(v);
  fitAllViewers();
}

function showNotice() {
  const text = !isSecureContext
    ? [t('当前通过 HTTP 访问，浏览器不允许解码视频。请改用 '), (() => {
        const port = (INFO && INFO.https_port) || 8731;
        const host = location.hostname.includes(':') ? '[' + location.hostname + ']' : location.hostname;
        const url = `https://${host}:${port}/`;
        return h('a', { href: url }, url);
      })(), t('（首次需在浏览器中选择继续访问）。')]
    : [t('当前浏览器不支持视频解码，请使用新版 Chrome、Edge 或 Safari。')];
  $('#notice').replaceChildren(h('div', { class: 'notice' }, h('span', { html: svg('alert') }).firstChild, h('div', {}, text)));
  if (isMobile()) $('#noticeSide').replaceChildren(h('div', { class: 'notice' }, h('span', { html: svg('alert') }).firstChild, h('div', {}, text.map(x => typeof x === 'string' ? x : x.cloneNode(true)))));
}
