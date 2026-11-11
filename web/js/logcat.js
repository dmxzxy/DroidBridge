// 设备日志面板
// 由 web/index.html 拆分而来；经典脚本，按 index.html 中的顺序加载，共享全局作用域。
'use strict';

const LOG_RE = /^\d\d-\d\d\s+(\d\d:\d\d:\d\d\.\d+)\s+(\d+)\s+\d+\s+([VDIWEFA])\s+(.*)$/;
const LOG_LV = { V: 0, D: 1, I: 2, W: 3, E: 4, F: 5, A: 5 };
const LOG_MAX = 20000, LOG_DOM = 3000;

class LogPanel {
  constructor(v) {
    this.v = v; this.buf = []; this.pkg = ''; this.pids = new Set();
    this.paused = false; this.pending = 0; this.follow = true; this.queue = []; this.words = [];
    this.minLv = +pref.get('loglv', '0');
    const lv = h('select', { class: 'select', title: t('级别'), onchange: () => { this.minLv = +lv.value; pref.set('loglv', lv.value); this.rerender(); } },
      [['0', t('全部')], ['1', t('调试+')], ['2', t('信息+')], ['3', t('警告+')], ['4', t('错误')]].map(([val, l]) => h('option', { value: val }, l)));
    lv.value = String(this.minLv);
    this.qInput = h('input', { class: 'input q', placeholder: t('过滤：关键字或 Tag，-排除'), spellcheck: 'false' });
    let qt = 0;
    this.qInput.addEventListener('input', () => { clearTimeout(qt); qt = setTimeout(() => { this.setQuery(this.qInput.value); this.rerender(); }, 150); });
    this.appInput = h('input', { class: 'input', placeholder: t('全部应用'), title: t('只看某个应用：填写包名后回车'), spellcheck: 'false' });
    this.appInput.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); this.sendPkg(this.appInput.value.trim()); } });
    this.appInput.addEventListener('change', () => this.sendPkg(this.appInput.value.trim()));
    this.stateEl = h('span', { class: 'lstate' });
    this.pauseBtn = ib('pause', t('暂停'), () => this.togglePause());
    this.list = h('div', { class: 'lbody' });
    this.jump = h('button', { class: 'ljump', type: 'button', hidden: true, onclick: () => { this.follow = true; this.list.scrollTop = this.list.scrollHeight; this.jump.hidden = true; } }, t('跳到最新'));
    this.list.addEventListener('scroll', () => {
      this.follow = this.list.scrollTop + this.list.clientHeight >= this.list.scrollHeight - 24;
      this.jump.hidden = this.follow;
    });
    this.el = h('div', { class: 'lpanel' },
      h('div', { class: 'lhead' }, h('span', { class: 'ltitle' }, t('日志')), this.stateEl,
        this.pauseBtn, ib('trash', t('清空'), () => this.clear()), ib('download', t('保存为文件'), () => this.save()), ib('x', t('关闭日志'), () => v.toggleLog(false))),
      h('div', { class: 'lbar' }, lv, this.qInput,
        h('div', { class: 'lapp' }, this.appInput, ib('target', t('只看当前前台应用'), () => this.sendPkg('@top')))),
      this.list, this.jump);
    this.connect();
  }

  connect() {
    if (this.ws) { const w = this.ws; this.ws = null; try { w.close(); } catch {} }
    const p = new URLSearchParams({ serial: this.v.dev.serial, peer: this.v.host.id, lang: LANG });
    const tk = pref.get('token', ''); if (tk) p.set('token', tk);
    const ws = this.ws = new WebSocket((location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/ws/logcat?' + p);
    this.ended = false;
    this.setState(t('正在连接…'));
    ws.onopen = () => { this.setState(''); if (this.pkg) this.sendPkg(this.pkg); };
    ws.onmessage = e => {
      const m = JSON.parse(e.data);
      if (m.type === 'lines') this.add(m.l);
      else if (m.type === 'filter') this.onFilter(m);
      else if (m.type === 'notice') toast(m.msg);
      else if (m.type === 'end') { this.ended = true; this.setState(m.msg, true); }
    };
    ws.onclose = () => { if (this.ws === ws && !this.ended) this.setState(t('日志连接已断开'), true); };
  }
  destroy() { const w = this.ws; this.ws = null; if (w) try { w.close(); } catch {} }

  setState(text, retry) {
    this.stateEl.replaceChildren(text, retry ? h('button', { class: 'linkbtn', type: 'button', onclick: () => this.connect() }, t(' 重连')) : '');
    this.stateEl.title = text;
  }
  updateState() {
    const parts = [];
    if (this.pkg) parts.push(this.pids.size ? this.pkg : this.pkg + t('（未运行）'));
    if (this.paused) parts.push(t('已暂停{0}', this.pending ? t(' · 新增 {0} 行', this.pending) : ''));
    this.setState(parts.join(' · '));
  }

  sendPkg(name) {
    if (this.ws && this.ws.readyState === 1) this.ws.send(JSON.stringify({ t: 'pkg', name }));
  }
  onFilter(m) {
    if (m.pkg !== this.pkg) { this.pkg = m.pkg; this.pids = new Set(); }
    // 记住应用历次的进程号，重启后之前的日志仍然可见
    for (const p of m.pids) this.pids.add(p);
    if (document.activeElement !== this.appInput) this.appInput.value = m.pkg;
    this.updateState();
    this.rerender();
  }

  setQuery(s) {
    this.words = s.toLowerCase().split(/\s+/).filter(Boolean).map(w => w.startsWith('-') && w.length > 1 ? { neg: true, w: w.slice(1) } : { neg: false, w });
  }
  parse(raw) {
    const m = LOG_RE.exec(raw);
    return m ? { raw, t: m[1], pid: +m[2], lc: m[3], lv: LOG_LV[m[3]], msg: m[4] } : { raw, t: '', pid: 0, lc: '', lv: -1, msg: raw };
  }
  match(e) {
    if (e.lv >= 0 && e.lv < this.minLv) return false;
    if (e.lv < 0 && (this.minLv > 0 || this.pkg)) return false;
    if (this.pkg && !this.pids.has(e.pid) && !e.raw.includes(this.pkg)) return false;
    if (this.words.length) {
      const low = e.low || (e.low = e.raw.toLowerCase());
      for (const { neg, w } of this.words) if (low.includes(w) === neg) return false;
    }
    return true;
  }
  row(e) {
    if (e.lv < 0) return h('div', { class: 'll lm' }, e.msg);
    return h('div', { class: 'll l' + e.lv }, h('span', { class: 'lt' }, e.t), ' ', h('b', {}, e.lc), ' ', e.msg);
  }

  add(lines) {
    for (const raw of lines) {
      const e = this.parse(raw);
      this.buf.push(e);
      if (this.match(e)) { if (this.paused) this.pending++; else this.queue.push(e); }
    }
    if (this.buf.length > LOG_MAX) this.buf.splice(0, this.buf.length - LOG_MAX);
    if (this.paused) { this.updateState(); return; }
    if (!this.raf) this.raf = requestAnimationFrame(() => this.flush());
  }
  flush() {
    this.raf = 0;
    const q = this.queue.length > LOG_DOM ? this.queue.slice(-LOG_DOM) : this.queue;
    this.queue = [];
    if (!q.length) return;
    const frag = document.createDocumentFragment();
    for (const e of q) frag.append(this.row(e));
    this.list.append(frag);
    let extra = this.list.childElementCount - LOG_DOM;
    while (extra-- > 0) this.list.firstChild.remove();
    if (this.follow) this.list.scrollTop = this.list.scrollHeight;
  }
  rerender() {
    this.queue = [];
    const out = [];
    for (let i = this.buf.length - 1; i >= 0 && out.length < LOG_DOM; i--) if (this.match(this.buf[i])) out.push(this.buf[i]);
    const frag = document.createDocumentFragment();
    for (let i = out.length - 1; i >= 0; i--) frag.append(this.row(out[i]));
    this.list.replaceChildren(frag);
    this.follow = true; this.jump.hidden = true;
    this.list.scrollTop = this.list.scrollHeight;
  }
  togglePause() {
    this.paused = !this.paused;
    this.pauseBtn.innerHTML = svg(this.paused ? 'play' : 'pause');
    this.pauseBtn.title = this.paused ? t('继续') : t('暂停');
    this.pauseBtn.classList.toggle('on', this.paused);
    if (!this.paused) { this.pending = 0; this.rerender(); }
    this.updateState();
  }
  clear() { this.buf = []; this.queue = []; this.pending = 0; this.list.replaceChildren(); this.updateState(); }
  save() {
    const lines = this.buf.filter(e => this.match(e)).map(e => e.raw);
    if (!lines.length) { toast(t('没有可保存的日志')); return; }
    const name = (this.v.dev.model || this.v.dev.serial).replace(/[^\w.-]/g, '_');
    const a = h('a', { href: URL.createObjectURL(new Blob([lines.join('\n') + '\n'], { type: 'text/plain' })), download: `${name}-logcat-${new Date().toISOString().replace(/\D/g, '').slice(0, 14)}.txt` });
    a.click(); setTimeout(() => URL.revokeObjectURL(a.href), 5000);
    toast(t('已保存 {0} 行日志', lines.length));
  }
}
