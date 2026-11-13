// 投屏查看器：视频解码、控制、录制、性能浮层、群控、AI 提示
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

class Viewer {
  constructor(host, dev) {
    this.host = host; this.dev = dev; this.key = host.id + '|' + dev.serial;
    this.audioOn = pref.get('audio', '0') === '1'; this.fails = 0;
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
    this.dot = h('span', { class: 'dot warn' });
    this.nameEl = h('span', { class: 'name' }, this.dev.model || this.dev.serial);
    this.statEl = h('span', { class: 'fps' });
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
    this.syncBtn = ib('link', t('群控：在这台设备上的操作同步到其他群控设备'), () => this.toggleSync());
    this.syncBtn.hidden = viewers.size < 1;
    this.vlog = h('div', { class: 'vlog' });
    this.root = h('div', { class: 'viewer' },
      ctxMenu(h('div', { class: 'vhead' }, this.dot, this.nameEl, this.recEl, h('span', { class: 'sub' }, this.host.remote ? this.host.name : ''), this.statEl,
        this.syncBtn, ib('max', t('全屏'), () => this.fullscreen()), ib('x', t('关闭'), () => this.close())), () => this.moreItems()),
      h('div', { class: 'vmain' },
      h('div', { class: 'vbody' },
        h('div', { class: 'vcol' },
          h('div', { class: 'screen' }, this.canvas, this.ime, this.overlay, this.perfEl, this.aiBar, h('div', { class: 'drop' }, t('松开以安装 APK 或传到手机'))),
          h('div', { class: 'nav' }, ib('back', t('返回'), key(AKEY.BACK)), ib('home', t('主页'), key(AKEY.HOME)), ib('recent', t('最近任务'), key(AKEY.APP_SWITCH))),
          this.typing),
        h('div', { class: 'vtools' },
          ib('power', t('电源键'), key(AKEY.POWER)), ib('volUp', t('音量 +'), key(AKEY.VOLUME_UP)), ib('volDown', t('音量 −'), key(AKEY.VOLUME_DOWN)),
          ib('rotate', t('旋转屏幕'), () => this.send({ t: 'cmd', name: 'rotate' })),
          h('div', { class: 'sep' }),
          ib('camera', t('截取屏幕'), () => this.screenshot()), this.recBtn, this.kbBtn, this.audioBtn, this.logBtn, this.filesBtn, this.perfBtn, this.moreBtn)),
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
      { icon: 'paste', label: t('粘贴电脑剪贴板'), fn: () => this.paste() },
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
    const p = new URLSearchParams({ serial: this.dev.serial, peer: this.host.id, lang: LANG, max_size: q('size'), bit_rate: q('rate'), max_fps: q('fps'), stay_awake: pref.get('awake', '1'),
      audio: this.audioOn ? '1' : '0', audio_codec: AUDIO_CODEC, audio_dup: pref.get('audiodup', '0') });
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
  /** 手动重连：清零失败计数 */
  reconnect() { this.recovering = false; this.fails = 0; this.restart(); }
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
  }

  toggleAudio() {
    this.audioOn = !this.audioOn;
    pref.set('audio', this.audioOn ? '1' : '0');
    this.audioBtn.classList.toggle('on', this.audioOn);
    if (this.audioOn) audioCtx();   // 在点击中创建，满足浏览器自动播放限制
    this.reconnect();
  }
  close() {
    this.closed = true; this.recovering = false;
    this.perfOn = false; this.closePerf();
    if (this.recording) this.stopRecord();
    if (this.log) this.log.destroy();
    if (this.files) this.files.destroy();
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

  toggleLog(force) {
    const open = force === undefined ? !this.log : force;
    if (open && this.files) this.toggleFiles(false);   // 侧栏同一时间只放一个
    if (open && !this.log) { this.log = new LogPanel(this); this.vlog.append(this.log.el); }
    if (!open && this.log) { this.log.destroy(); this.log = null; this.vlog.replaceChildren(); }
    this.root.classList.toggle('with-log', !!(this.log || this.files));
    this.logBtn.classList.toggle('on', !!this.log);
    this.fit();
  }
  toggleFiles(force) {
    const open = force === undefined ? !this.files : force;
    if (open && this.log) this.toggleLog(false);
    if (open && !this.files) {
      this.files = new FileBrowser(this.host, this.dev, { compact: true, onClose: () => this.toggleFiles(false) });
      this.vlog.append(this.files.el);
    }
    if (!open && this.files) { this.files.destroy(); this.files = null; this.vlog.replaceChildren(); }
    this.root.classList.toggle('with-log', !!(this.log || this.files));
    this.filesBtn.classList.toggle('on', !!this.files);
    this.fit();
  }

  // 桌面端布局：窗口宽度跟随画面比例；打开日志时，空间够就放右侧，横屏设备且空间不够时放到画面下方
  fit() {
    const r = this.root;
    if (!this.vw || !this.vh || isMobile()) { r.style.width = ''; this.vlog.style.width = ''; r.classList.remove('log-bottom'); return; }
    const avail = $('main').clientWidth - 32;
    const cw = Math.min(this.vw, (innerHeight - 150 - (hasTitlebar() ? 36 : 0)) * this.vw / this.vh);
    if (!this.log && !this.files) { r.classList.remove('log-bottom'); r.style.width = Math.round(Math.max(300, Math.min(cw + 46, avail))) + 'px'; return; }
    const side = avail - (cw + 46);
    const portrait = this.vh >= this.vw;
    if (side >= 360 || portrait || avail - 46 - 400 >= cw * 0.7) {
      const lw = Math.round(Math.max(side >= 360 ? Math.min(side, 900) : 400, 360));
      r.classList.remove('log-bottom');
      this.vlog.style.width = lw + 'px';
      r.style.width = Math.round(Math.min(avail, cw + 46 + lw)) + 'px';
    } else {
      r.classList.add('log-bottom');
      this.vlog.style.width = '';
      r.style.width = Math.round(avail) + 'px';
    }
  }

  onText(m) {
    switch (m.type) {
      case 'status': if (!this.gotFrame) this.showOverlay(m.msg, { spin: true }); break;
      case 'meta': if (m.device_name) this.nameEl.textContent = m.device_name; this.showOverlay(t('等待画面…'), { spin: true }); break;
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
  /** 发送控制消息；开启群控时同步给其他群控设备（坐标按各自分辨率等比换算） */
  send(obj) {
    this.sendRaw(obj);
    if (!this.sync || !SYNC_TYPES.has(obj.t) || (obj.t === 'cmd' && obj.name === 'reset_video')) return;
    for (const v of viewers.values()) {
      if (v === this || !v.sync) continue;
      if (obj.w) { if (v.vw) v.sendRaw({ ...obj, x: Math.round(obj.x / obj.w * v.vw), y: Math.round(obj.y / obj.h * v.vh), w: v.vw, h: v.vh }); }
      else v.sendRaw(obj);
    }
  }
  toggleSync(on = !this.sync) {
    this.sync = on;
    this.root.classList.toggle('synced', on);
    this.syncBtn.classList.toggle('on', on);
    if (!on) return;
    const n = [...viewers.values()].filter(v => v.sync).length;
    toast(n < 2 ? t('再给至少一台设备打开群控') : t('群控：{0} 台设备同步操作', n));
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
    if (this.perfOn) { this.perfHist = []; this.perfEl.replaceChildren(h('div', { class: 'pk' }, t('正在读取性能数据…'))); this.openPerf(); }
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
    this.perfEl.replaceChildren(
      h('div', { class: 'pk' }, m.pkg || '—'),
      h('div', { class: 'pr' }, h('span', { class: 'pl' }, 'FPS'), h('b', {}, n(m.fps)), spark),
      row('CPU', pair(m.app_cpu != null && t('应用') + ' ' + n(m.app_cpu) + '%', m.cpu != null && t('整机') + ' ' + n(m.cpu) + '%')),
      row(t('内存'), pair(m.app_mem != null && t('应用') + ' ' + m.app_mem + ' MB', m.mem_total && t('整机') + ' ' + Math.round(m.mem_used / m.mem_total * 100) + '%')),
      row(t('温度'), pair(m.cpu_temp != null && 'CPU ' + Math.round(m.cpu_temp) + '°C', m.batt_temp != null && t('电池') + ' ' + m.batt_temp.toFixed(1) + '°C'), temp >= 55 ? 'bad' : temp >= 45 ? 'warn' : ''),
      row(t('电量'), m.level == null ? '–' : m.level + '%' + (m.charging ? ' · ' + t('充电中') : ''), m.level != null && m.level <= 15 && !m.charging ? 'warn' : ''),
    );
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
        galleryHint(item);
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
  $('#grid').append(v.root);
  afterViewersChange();
  history.replaceState(null, '', location.search + '#open=' + encodeURIComponent(hst.id) + '|' + encodeURIComponent(dev.serial));
  if (isMobile()) scrollTo(0, 0);
}

function afterViewersChange() {
  // 群控至少需要两台设备
  for (const v of viewers.values()) { v.syncBtn.hidden = viewers.size < 2; if (viewers.size < 2 && v.sync) v.toggleSync(false); }
  $('#placeholder').style.display = viewers.size ? 'none' : '';
  if (!viewers.size) { document.body.classList.remove('viewing'); history.replaceState(null, '', location.pathname + location.search); }
  renderSide();
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
