// 设备文件管理：浏览共享存储、预览、上传、下载、新建文件夹、重命名、删除。
// FileBrowser 是一个组件，可以放进单独的对话框（filesDialog），也可以作为投屏窗口旁边的侧栏（和日志面板一样）。
'use strict';

const fmtSize = (n) => {
  if (n < 1024) return n + ' B';
  const u = ['KB', 'MB', 'GB', 'TB'];
  let i = -1;
  do { n /= 1024; i++; } while (n >= 1024 && i < u.length - 1);
  return (n >= 100 ? n.toFixed(0) : n.toFixed(1)) + ' ' + u[i];
};
const fmtTime = (t) => {
  if (!t) return '';
  const d = new Date(t * 1000), now = new Date();
  const two = (v) => String(v).padStart(2, '0');
  const sameYear = d.getFullYear() === now.getFullYear();
  return (sameYear ? '' : d.getFullYear() + '-') + two(d.getMonth() + 1) + '-' + two(d.getDate()) + ' ' + two(d.getHours()) + ':' + two(d.getMinutes());
};
const FILE_KIND = (name, dir) => {
  if (dir) return 'folder';
  const e = name.toLowerCase().split('.').pop();
  if (['jpg', 'jpeg', 'png', 'webp', 'gif', 'bmp', 'heic'].includes(e)) return 'image';
  if (['mp4', 'webm', 'mkv', 'mov', '3gp', 'm4v'].includes(e)) return 'video';
  if (['mp3', 'm4a', 'aac', 'wav', 'flac', 'ogg', 'opus', 'amr'].includes(e)) return 'audio';
  if (e === 'apk' || e === 'apks' || e === 'xapk') return 'apk';
  if (['txt', 'log', 'json', 'xml', 'md', 'csv', 'ini', 'cfg', 'conf', 'properties', 'yaml', 'yml', 'lua', 'js', 'html', 'css', 'sh'].includes(e)) return 'text';
  if (['zip', 'rar', '7z', 'tar', 'gz', 'obb'].includes(e)) return 'archive';
  return 'file';
};
const KIND_ICON = { folder: 'folder', image: 'images', video: 'record', audio: 'headphones', apk: 'package', text: 'logs', archive: 'package', file: 'file' };
const FILE_PLACES = [
  ['/sdcard', '存储根目录'], ['/sdcard/Download', '下载'], ['/sdcard/DCIM', '相机'], ['/sdcard/Pictures', '图片'],
  ['/sdcard/Movies', '视频'], ['/sdcard/Documents', '文档'], ['/sdcard/Android/data', '应用数据'],
];

/** 带进度的上传；失败时抛出服务端返回的错误信息 */
function xhrUpload(url, file, onProgress) {
  return new Promise((resolve, reject) => {
    const x = new XMLHttpRequest();
    x.open('POST', url);
    x.upload.onprogress = (e) => { if (e.lengthComputable && onProgress) onProgress(Math.round(e.loaded / e.total * 100)); };
    x.onload = () => {
      let j = {};
      try { j = JSON.parse(x.responseText); } catch {}
      if (x.status >= 200 && x.status < 300 && !j.error) resolve(j);
      else reject(new Error(j.error || ('HTTP ' + x.status)));
    };
    x.onerror = () => reject(new Error(t('网络错误')));
    x.send(file);
  });
}

class FileBrowser {
  /** opts.compact：侧栏模式（窄）；opts.onClose：侧栏的关闭按钮 */
  constructor(hst, dev, opts = {}) {
    this.hst = hst; this.dev = dev; this.opts = opts;
    this.path = pref.get('files_path_' + dev.serial, '/sdcard');
    this.parent = null; this.entries = []; this.space = null;
    this.cache = new Map();          // 目录内容缓存：返回已看过的目录时立即显示，后台再刷新
    this.history = [];               // 后退
    this.sort = pref.get('files_sort', 'name');  // name / time / size
    this.showHidden = pref.get('files_hidden', '0') === '1';
    this.view = pref.get('files_view', 'list');  // list / grid
    this.selected = new Set();
    this.loadSeq = 0;
    this.build();
    this.load(this.path);
  }

  call(name, body) {
    return api('/api/files/' + name + '?peer=' + encodeURIComponent(this.hst.id), { method: 'POST', body: JSON.stringify({ serial: this.dev.serial, ...body }) });
  }
  previewUrl(full, thumb) {
    const qs = new URLSearchParams({ serial: this.dev.serial, peer: this.hst.id, path: full });
    if (thumb) qs.set('thumb', String(thumb));
    return withToken('/api/files/preview?' + qs);
  }

  build() {
    const c = this.opts.compact;
    this.backBtn = ib('arrowLeft', t('后退'), () => this.back());
    this.upBtn = ib('arrowUp', t('上一级'), () => { if (this.parent) this.go(this.parent); });
    this.crumbs = h('div', { class: 'fb-crumbs' });
    this.filter = h('input', { class: 'input fb-filter', type: 'search', placeholder: t('筛选'), spellcheck: 'false', oninput: () => this.render() });
    this.picker = h('input', { type: 'file', multiple: true, hidden: true, onchange: () => { const f = [...this.picker.files]; this.picker.value = ''; if (f.length) this.upload(f); } });
    this.list = h('div', { class: 'fb-list', tabindex: '0' });
    this.status = h('div', { class: 'fb-status' });
    this.viewBtn = ib(this.view === 'grid' ? 'logs' : 'grid', this.view === 'grid' ? t('列表视图') : t('缩略图视图'), () => this.toggleView());
    const sortMenu = (e) => menu(e.currentTarget, [
      ...[['name', t('按名称')], ['time', t('按修改时间')], ['size', t('按大小')]].map(([k, l]) => ({ icon: this.sort === k ? 'check' : 'blank', label: l, fn: () => { this.sort = k; pref.set('files_sort', k); this.render(); } })),
      '-',
      { icon: this.showHidden ? 'check' : 'blank', label: t('显示隐藏文件'), fn: () => { this.showHidden = !this.showHidden; pref.set('files_hidden', this.showHidden ? '1' : '0'); this.render(); } },
    ]);
    const placesMenu = (e) => menu(e.currentTarget, FILE_PLACES.map(([p, l]) => ({ icon: 'folder', label: t(l), fn: () => this.go(p) })));
    this.head = h('div', { class: 'fb-head' },
      c ? h('span', { class: 'ltitle' }, t('文件')) : null,
      this.backBtn, this.upBtn, ib('bookmark', t('常用位置'), placesMenu), this.crumbs,
      ib('refresh', t('刷新'), () => this.load(this.path, { force: true })),
      c ? ib('x', t('关闭文件面板'), () => this.opts.onClose && this.opts.onClose()) : null);
    this.bar = h('div', { class: 'fb-bar' }, this.filter,
      ib('sort', t('排序与显示'), sortMenu), this.viewBtn,
      ib('upload', t('上传文件'), () => this.picker.click()),
      ib('folderPlus', t('新建文件夹'), () => this.newFolder()));
    this.dropHint = h('div', { class: 'fb-drop' });
    this.el = h('div', { class: 'fb' + (c ? ' compact' : '') }, this.head, this.bar, this.list, this.status, this.picker, this.dropHint);

    // 键盘：回车打开、Backspace 上一级、Delete 删除、F2 重命名、↑↓ 移动选择
    this.list.addEventListener('keydown', (e) => this.onKey(e));
    // 拖放上传
    let depth = 0;
    const isFiles = (e) => [...(e.dataTransfer?.types || [])].includes('Files');
    this.el.addEventListener('dragenter', (e) => {
      if (!isFiles(e)) return;
      e.preventDefault(); e.stopPropagation();
      depth++;
      this.dropHint.textContent = t('松开上传到 {0}', this.path);
      this.el.classList.add('dropping');
    });
    this.el.addEventListener('dragover', (e) => { if (isFiles(e)) { e.preventDefault(); e.stopPropagation(); e.dataTransfer.dropEffect = 'copy'; } });
    this.el.addEventListener('dragleave', (e) => { e.stopPropagation(); if (--depth <= 0) { depth = 0; this.el.classList.remove('dropping'); } });
    const reset = () => { depth = 0; this.el.classList.remove('dropping'); };
    addEventListener('drop', reset, true);
    addEventListener('dragend', reset, true);
    document.addEventListener('dragleave', (e) => { if (!e.relatedTarget) reset(); });
    this.el.addEventListener('drop', (e) => {
      const files = [...(e.dataTransfer?.files || [])];
      if (!files.length) return;
      e.preventDefault(); e.stopPropagation(); depth = 0; this.el.classList.remove('dropping');
      this.upload(files);
    });
    ctxMenu(this.list, () => [
      { icon: 'upload', label: t('上传文件…'), fn: () => this.picker.click() },
      { icon: 'folderPlus', label: t('新建文件夹…'), fn: () => this.newFolder() },
      { icon: 'refresh', label: t('刷新'), fn: () => this.load(this.path, { force: true }) },
      '-',
      { icon: 'copy', label: t('复制当前路径'), fn: () => copyText(this.path) },
    ]);
  }

  // ---------------- 导航 ----------------
  go(p) {
    if (p === this.path) return;
    this.history.push(this.path);
    if (this.history.length > 50) this.history.shift();
    this.load(p);
  }
  back() {
    const p = this.history.pop();
    if (p) this.load(p);
  }

  async load(p, { force = false, keepSel = false } = {}) {
    const seq = ++this.loadSeq;
    const cached = this.cache.get(p);
    if (cached && !force) this.show(p, cached, keepSel);    // 立即显示缓存
    else if (!cached) this.list.replaceChildren(h('div', { class: 'fb-empty' }, h('span', { class: 'spin' }), t('正在读取…')));
    this.el.classList.add('loading');
    try {
      const r = await this.call('list', { path: p });
      if (seq !== this.loadSeq) return;               // 期间又打开了别的目录
      this.cache.set(r.path, r);
      if (this.cache.size > 60) this.cache.delete(this.cache.keys().next().value);
      this.show(r.path, r, keepSel || !!cached);
    } catch (e) {
      if (seq !== this.loadSeq) return;
      if (cached) { toast(e.message, { kind: 'bad', ms: 4000 }); return; }
      this.list.replaceChildren(h('div', { class: 'fb-empty' }, h('span', {}, e.message), h('button', { class: 'btn', type: 'button', onclick: () => this.load(p, { force: true }) }, t('重试'))));
    } finally { if (seq === this.loadSeq) this.el.classList.remove('loading'); }
  }
  show(p, r, keepSel) {
    const changed = p !== this.path;
    this.path = r.path || p; this.parent = r.parent; this.entries = r.items || []; this.space = r.space;
    pref.set('files_path_' + this.dev.serial, this.path);
    if (changed || !keepSel) { this.selected.clear(); this.filter.value = ''; }
    this.render();
    if (changed) this.list.scrollTop = 0;
  }

  // ---------------- 显示 ----------------
  visible() {
    const q = this.filter.value.trim().toLowerCase();
    let arr = this.entries.filter(e => (this.showHidden || !e.name.startsWith('.')) && (!q || e.name.toLowerCase().includes(q)));
    const by = this.sort;
    arr = arr.slice().sort((a, b) => (b.dir - a.dir)
      || (by === 'time' ? b.mtime - a.mtime : by === 'size' ? b.size - a.size : 0)
      || a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' }));
    return arr;
  }
  renderCrumbs() {
    const parts = this.path.split('/').filter(Boolean);
    const out = [];
    let acc = '';
    parts.forEach((p, i) => {
      acc += '/' + p;
      const target = acc;
      // /sdcard 显示成“存储”，/storage/emulated/0 同理
      const label = (i === 0 && p === 'sdcard') ? t('存储') : (acc === '/storage/emulated/0' ? t('存储') : p);
      if (acc === '/storage' || acc === '/storage/emulated') return;
      if (out.length) out.push(h('span', { class: 'fb-sep' }, '›'));
      out.push(h('button', { class: 'fb-crumb' + (i === parts.length - 1 ? ' cur' : ''), type: 'button', title: target, onclick: () => this.go(target) }, label));
    });
    this.crumbs.replaceChildren(...out);
    // 太长时滚动到最后一段
    requestAnimationFrame(() => { this.crumbs.scrollLeft = this.crumbs.scrollWidth; });
  }
  render() {
    this.renderCrumbs();
    this.upBtn.disabled = !this.parent;
    this.backBtn.disabled = !this.history.length;
    const arr = this.visible();
    this.shown = arr;
    this.list.classList.toggle('grid', this.view === 'grid');
    const hidden = this.entries.length - this.entries.filter(e => this.showHidden || !e.name.startsWith('.')).length;
    const sel = [...this.selected].filter(n => arr.some(e => e.name === n));
    const selSize = arr.filter(e => sel.includes(e.name) && !e.dir).reduce((a, e) => a + e.size, 0);
    this.status.replaceChildren(...[
      h('span', {}, sel.length ? t('已选 {0} 项', sel.length) + (selSize ? ' · ' + fmtSize(selSize) : '') : t('{0} 项', arr.length) + (hidden ? ' · ' + t('{0} 个隐藏', hidden) : '')),
      sel.length ? h('button', { class: 'linkbtn', type: 'button', onclick: () => this.removeSelected() }, t('删除所选')) : null,
      sel.length ? h('button', { class: 'linkbtn', type: 'button', onclick: () => this.downloadSelected() }, t('下载所选')) : null,
      h('span', { class: 'fb-space' }, this.space ? t('可用 {0} / {1}', fmtSize(this.space.free), fmtSize(this.space.total)) : '')].filter(Boolean));
    if (!arr.length) {
      this.list.replaceChildren(h('div', { class: 'fb-empty' }, this.filter.value ? t('没有匹配的文件') : t('这个文件夹是空的')));
      return;
    }
    // 大目录分批渲染，先出前 200 项，滚动到底再补
    this.rendered = 0;
    this.list.replaceChildren();
    this.renderMore();
    if (!this.onScroll) {
      this.onScroll = () => { if (this.list.scrollTop + this.list.clientHeight > this.list.scrollHeight - 300) this.renderMore(); };
      this.list.addEventListener('scroll', this.onScroll, { passive: true });
    }
    this.observeThumbs();
  }
  renderMore() {
    const arr = this.shown || [];
    if (this.rendered >= arr.length) return;
    const end = Math.min(arr.length, this.rendered + 200);
    const frag = document.createDocumentFragment();
    for (let i = this.rendered; i < end; i++) frag.append(this.row(arr[i]));
    this.list.append(frag);
    this.rendered = end;
    this.observeThumbs();
  }
  full(e) { return this.path.replace(/\/$/, '') + '/' + e.name; }
  row(e) {
    const full = this.full(e), kind = FILE_KIND(e.name, e.dir);
    const icon = h('span', { class: 'fb-ico k-' + kind, html: svg(KIND_ICON[kind]) });
    if (kind === 'image') icon.dataset.thumb = full;
    const el = h('div', {
      class: 'fb-row' + (e.dir ? ' dir' : '') + (this.selected.has(e.name) ? ' sel' : ''), title: full, 'data-name': e.name,
      onclick: (ev) => this.onClick(ev, e),
      ondblclick: () => this.open(e),
    }, icon,
      h('div', { class: 'fb-t' }, h('b', {}, e.name), h('small', {}, (e.dir ? '' : fmtSize(e.size) + ' · ') + fmtTime(e.mtime))),
      h('div', { class: 'fb-acts' },
        e.dir ? null : ib('download', t('下载到电脑'), (ev) => { ev.stopPropagation(); this.download(e); }),
        ib('more', t('更多'), (ev) => { ev.stopPropagation(); this.select(e.name, false); menu(ev.currentTarget, this.items(e)); })));
    return ctxMenu(el, () => { if (!this.selected.has(e.name)) this.select(e.name, false); return this.items(e); });
  }
  items(e) {
    const full = this.full(e), kind = FILE_KIND(e.name, e.dir);
    const multi = this.selected.size > 1 && this.selected.has(e.name);
    if (multi) return [
      { icon: 'download', label: t('下载所选（{0} 项）', this.selected.size), fn: () => this.downloadSelected() },
      '-',
      { icon: 'trash', label: t('删除所选（{0} 项）', this.selected.size), danger: true, fn: () => this.removeSelected() },
    ];
    return [
      e.dir ? { icon: 'folder', label: t('打开'), fn: () => this.go(full) }
        : ['image', 'video', 'audio', 'text'].includes(kind) ? { icon: 'max', label: t('预览'), fn: () => this.open(e) } : null,
      kind === 'apk' ? { icon: 'package', label: t('安装到设备'), fn: () => this.installApk(full, e) } : null,
      e.dir ? null : { icon: 'download', label: t('下载到电脑'), fn: () => this.download(e) },
      '-',
      { icon: 'edit', label: t('重命名…'), fn: () => this.rename(e) },
      { icon: 'copy', label: t('复制路径'), fn: () => copyText(full) },
      '-',
      { icon: 'trash', label: t('删除…'), danger: true, fn: () => this.remove([e]) },
    ];
  }

  // 图片缩略图：进入可视区域才加载，同时最多 3 个请求
  observeThumbs() {
    if (!this.io) {
      this.thumbQueue = []; this.thumbActive = 0;
      this.io = new IntersectionObserver((ents) => {
        for (const en of ents) if (en.isIntersecting) { this.io.unobserve(en.target); this.thumbQueue.push(en.target); }
        this.pumpThumbs();
      }, { root: this.list, rootMargin: '200px' });
    }
    this.list.querySelectorAll('.fb-ico[data-thumb]:not([data-obs])').forEach(el => { el.dataset.obs = '1'; this.io.observe(el); });
  }
  pumpThumbs() {
    while (this.thumbActive < 3 && this.thumbQueue.length) {
      const el = this.thumbQueue.shift();
      if (!el.isConnected) continue;
      this.thumbActive++;
      const img = new Image();
      const done = () => { this.thumbActive--; this.pumpThumbs(); };
      img.onload = () => { el.replaceChildren(img); el.classList.add('has-thumb'); done(); };
      img.onerror = done;
      img.src = this.previewUrl(el.dataset.thumb, this.view === 'grid' ? 240 : 96);
    }
  }

  // ---------------- 选择 ----------------
  onClick(ev, e) {
    if (ev.ctrlKey || ev.metaKey) { this.select(e.name, true); return; }
    if (ev.shiftKey && this.anchor) {
      const names = this.shown.map(x => x.name);
      const [a, b] = [names.indexOf(this.anchor), names.indexOf(e.name)].sort((x, y) => x - y);
      this.selected = new Set(names.slice(a, b + 1));
      this.render();
      return;
    }
    // 单击：文件夹直接打开；文件选中（再次单击或双击打开）
    if (e.dir) { this.go(this.full(e)); return; }
    if (this.selected.size === 1 && this.selected.has(e.name)) { this.open(e); return; }
    this.select(e.name, false);
  }
  select(name, toggle) {
    if (toggle) { this.selected.has(name) ? this.selected.delete(name) : this.selected.add(name); }
    else { this.selected = new Set([name]); }
    this.anchor = name;
    this.list.querySelectorAll('.fb-row').forEach(r => r.classList.toggle('sel', this.selected.has(r.dataset.name)));
    this.renderStatusOnly();
  }
  renderStatusOnly() {
    // 只更新状态栏（不重建列表）
    const arr = this.shown || [];
    const sel = arr.filter(e => this.selected.has(e.name));
    const selSize = sel.filter(e => !e.dir).reduce((a, e) => a + e.size, 0);
    const first = this.status.firstChild;
    if (first) first.textContent = sel.length ? t('已选 {0} 项', sel.length) + (selSize ? ' · ' + fmtSize(selSize) : '') : t('{0} 项', arr.length);
    this.status.querySelectorAll('.linkbtn').forEach(b => b.remove());
    if (sel.length) {
      const space = this.status.querySelector('.fb-space');
      this.status.insertBefore(h('button', { class: 'linkbtn', type: 'button', onclick: () => this.removeSelected() }, t('删除所选')), space);
      this.status.insertBefore(h('button', { class: 'linkbtn', type: 'button', onclick: () => this.downloadSelected() }, t('下载所选')), space);
    }
  }
  onKey(ev) {
    const arr = this.shown || [];
    if (!arr.length) return;
    const cur = arr.findIndex(e => e.name === this.anchor);
    const pick = (i) => {
      i = Math.max(0, Math.min(arr.length - 1, i));
      while (this.rendered <= i) this.renderMore();
      this.select(arr[i].name, false);
      const row = this.list.querySelector(`.fb-row[data-name="${CSS.escape(arr[i].name)}"]`);
      if (row) row.scrollIntoView({ block: 'nearest' });
    };
    const e = arr[cur];
    const step = this.view === 'grid' ? Math.max(1, Math.floor(this.list.clientWidth / 128)) : 1;
    switch (ev.key) {
      case 'ArrowDown': ev.preventDefault(); pick(cur < 0 ? 0 : cur + step); break;
      case 'ArrowUp': ev.preventDefault(); pick(cur < 0 ? 0 : cur - step); break;
      case 'ArrowRight': if (this.view === 'grid') { ev.preventDefault(); pick(cur + 1); } break;
      case 'ArrowLeft': if (this.view === 'grid') { ev.preventDefault(); pick(cur - 1); } break;
      case 'Enter': if (e) { ev.preventDefault(); this.open(e); } break;
      case 'Backspace': ev.preventDefault(); if (this.parent) this.go(this.parent); break;
      case 'Delete': if (this.selected.size) { ev.preventDefault(); this.removeSelected(); } break;
      case 'F2': if (e) { ev.preventDefault(); this.rename(e); } break;
      case 'a': if (ev.ctrlKey || ev.metaKey) { ev.preventDefault(); this.selected = new Set(arr.map(x => x.name)); this.render(); } break;
    }
  }
  toggleView() {
    this.view = this.view === 'grid' ? 'list' : 'grid';
    pref.set('files_view', this.view);
    this.viewBtn.innerHTML = svg(this.view === 'grid' ? 'logs' : 'grid');
    this.viewBtn.title = this.view === 'grid' ? t('列表视图') : t('缩略图视图');
    this.list.querySelectorAll('.fb-ico[data-obs]').forEach(el => { delete el.dataset.obs; el.classList.remove('has-thumb'); });
    this.render();
  }

  // ---------------- 打开 / 预览 ----------------
  open(e) {
    if (e.dir) { this.go(this.full(e)); return; }
    const kind = FILE_KIND(e.name, e.dir);
    if (['image', 'video', 'audio', 'text'].includes(kind)) this.preview(e, kind);
    else if (kind === 'apk') this.installApk(this.full(e), e);
    else this.download(e);
  }
  preview(e, kind) {
    const full = this.full(e);
    const files = (this.shown || []).filter(x => FILE_KIND(x.name, x.dir) === 'image');
    let idx = files.findIndex(x => x.name === e.name);
    const box = h('div', { class: 'fb-pv-body' });
    const title = h('span', { class: 'fb-pv-name' });
    const show = async (item, k) => {
      const f = this.full(item);
      title.textContent = item.name + ' · ' + fmtSize(item.size);
      box.replaceChildren(h('span', { class: 'spin' }));
      if (k === 'image') {
        const img = h('img', { class: 'fb-pv-img', alt: item.name });
        img.onload = () => box.replaceChildren(img);
        img.onerror = () => box.replaceChildren(h('div', { class: 'fb-empty' }, t('无法显示这张图片')));
        img.src = this.previewUrl(f, item.size > 4 * 1048576 ? 2000 : 0);
      } else if (k === 'video' || k === 'audio') {
        if (item.size > 200 * 1048576) { box.replaceChildren(h('div', { class: 'fb-empty' }, t('文件较大，请下载后查看'))); return; }
        const m = h(k === 'video' ? 'video' : 'audio', { controls: true, autoplay: true, class: 'fb-pv-media', src: this.previewUrl(f) });
        box.replaceChildren(m);
      } else {
        try {
          const r = await fetch(this.previewUrl(f));
          const txt = await r.text();
          box.replaceChildren(h('pre', { class: 'fb-pv-text' }, txt + (item.size > 512 * 1024 ? '\n\n' + t('…（只显示前 512 KB）') : '')));
        } catch (err) { box.replaceChildren(h('div', { class: 'fb-empty' }, err.message)); }
      }
    };
    const nav = (d) => { if (kind !== 'image' || files.length < 2) return; idx = (idx + d + files.length) % files.length; show(files[idx], 'image'); };
    const dlg = dialog({
      title: '', cls: 'fb-preview',
      body: [h('div', { class: 'fb-pv-bar' }, title,
        kind === 'image' && files.length > 1 ? ib('chevLeft', t('上一张'), () => nav(-1)) : null,
        kind === 'image' && files.length > 1 ? h('span', { class: 'fb-pv-next' }, ib('chevLeft', t('下一张'), () => nav(1))) : null,
        ib('download', t('下载到电脑'), () => this.download(kind === 'image' ? files[idx] : e)),
        ib('x', t('关闭'), () => dlg.close())), box],
      actions: [],
    });
    dlg.addEventListener('keydown', (ev) => { if (ev.key === 'ArrowLeft') nav(-1); if (ev.key === 'ArrowRight') nav(1); });
    show(e, kind);
  }

  // ---------------- 操作 ----------------
  async upload(files) {
    const dir = this.path;
    const tt = toast(t('正在上传 0/{0}…', files.length), { sticky: true });
    let ok = 0;
    for (const [i, f] of files.entries()) {
      tt.set(t('正在上传 {0}（{1}/{2}）', f.name, i + 1, files.length));
      try {
        const qs = new URLSearchParams({ serial: this.dev.serial, peer: this.hst.id, path: dir, name: f.name, lang: LANG });
        await xhrUpload(withToken('/api/files/upload?' + qs), f, (pct) => tt.set(t('正在上传 {0}（{1}/{2}）', f.name, i + 1, files.length) + ' ' + pct + '%'));
        ok++;
      } catch (e) {
        tt.set(t('上传 {0} 失败：', f.name) + e.message, 'bad');
        tt.close(6000);
        this.cache.delete(dir);
        if (this.path === dir) this.load(dir, { force: true });
        return;
      }
    }
    tt.set(t('已上传 {0} 个文件', ok), 'ok');
    tt.close(2500);
    this.cache.delete(dir);
    if (this.path === dir) this.load(dir, { force: true });
  }
  async download(e) {
    const full = this.full(e);
    const tt = toast(t('正在下载 {0}…', e.name), { sticky: true });
    try {
      const qs = new URLSearchParams({ serial: this.dev.serial, peer: this.hst.id, path: full });
      const r = await fetch(withToken('/api/files/download?' + qs));
      if (!r.ok) {
        const j = await r.json().catch(() => ({}));
        throw new Error(j.error || ('HTTP ' + r.status));
      }
      const blob = await r.blob();
      const a = h('a', { href: URL.createObjectURL(blob), download: e.name });
      document.body.append(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(a.href), 30000);
      tt.set(t('已下载 {0}', e.name), 'ok');
      tt.close(3000);
    } catch (err) { tt.set(t('下载失败：') + err.message, 'bad'); tt.close(6000); }
  }
  async downloadSelected() {
    const arr = (this.shown || []).filter(e => this.selected.has(e.name) && !e.dir);
    if (!arr.length) { toast(t('文件夹不能直接下载，请进入后选择文件')); return; }
    for (const e of arr) await this.download(e);
  }
  async rename(e) {
    const to = await promptDialog(t('重命名'), e.name, { value: e.name });
    if (to === null || to.trim() === e.name || !to.trim()) return;
    try {
      await this.call('rename', { path: this.full(e), to: to.trim() });
      toast(t('已重命名'), { kind: 'ok' });
      this.selected = new Set([to.trim()]);
      this.load(this.path, { force: true, keepSel: true });
    } catch (err) { toast(err.message, { kind: 'bad', ms: 5000 }); }
  }
  async remove(arr) {
    const one = arr.length === 1 ? arr[0] : null;
    const title = one ? t('删除“{0}”？', one.name) : t('删除 {0} 项？', arr.length);
    const desc = arr.some(e => e.dir) ? t('文件夹及其中的内容都会被删除，无法恢复。') : t('文件会被删除，无法恢复。');
    if (!await confirmDialog(title, desc, t('删除'), true)) return;
    try {
      await this.call('delete', { paths: arr.map(e => this.full(e)) });
      toast(t('已删除'), { kind: 'ok' });
      this.selected.clear();
      this.load(this.path, { force: true });
    } catch (err) { toast(err.message, { kind: 'bad', ms: 5000 }); this.load(this.path, { force: true }); }
  }
  removeSelected() { const arr = (this.shown || []).filter(e => this.selected.has(e.name)); if (arr.length) this.remove(arr); }
  async newFolder() {
    const name = await promptDialog(t('新建文件夹'), null, { placeholder: t('文件夹名称') });
    if (name === null || !name.trim()) return;
    try {
      await this.call('mkdir', { path: this.path, to: name.trim() });
      toast(t('已创建'), { kind: 'ok' });
      this.selected = new Set([name.trim()]);
      this.load(this.path, { force: true, keepSel: true });
    } catch (err) { toast(err.message, { kind: 'bad', ms: 5000 }); }
  }
  async installApk(full, e) {
    if (!await confirmDialog(t('安装“{0}”？', e.name), t('部分手机会在设备上弹出安装确认。'), t('安装'))) return;
    const tt = toast(t('正在安装 {0}…', e.name), { sticky: true });
    try {
      const r = await this.call('install', { path: full });
      tt.set(r.msg || t('已安装'), 'ok'); tt.close(3000);
    } catch (err) { tt.set(err.message, 'bad'); tt.close(7000); }
  }
  destroy() { if (this.io) this.io.disconnect(); }
}

/** 单独的文件管理窗口 */
function filesDialog(hst, dev) {
  const fb = new FileBrowser(hst, dev);
  const dlg = dialog({ title: t('文件管理') + ' · ' + (dev.model || dev.serial), cls: 'files', body: [fb.el], actions: [] });
  dlg.addEventListener('close', () => fb.destroy());
  // 对话框里也放一个关闭按钮（标题栏右侧）
  const head = dlg.querySelector('.dlg-h');
  if (head) head.append(ib('x', t('关闭'), () => dlg.close()));
  setTimeout(() => fb.list.focus({ preventScroll: true }), 50);
}
