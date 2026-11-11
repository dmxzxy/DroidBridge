// 相册：截图与录屏
// 由 web/index.html 拆分而来；经典脚本，按 index.html 中的顺序加载，共享全局作用域。
'use strict';

function isLocalHost() {
  const h = location.hostname;
  return h === '127.0.0.1' || h === 'localhost' || h === '::1' || h === '[::1]';
}

// 截图 / 录屏完成后的提示：带“查看”按钮，打开相册并定位到该文件
function galleryHint(item) {
  const msg = item.kind === 'rec' ? t('录屏已保存') : t('已保存到相册');
  toast(msg, { kind: 'ok', sticky: true });
  const el = $('#toasts').lastElementChild;
  if (el) {
    el.append(h('button', { class: 'linkbtn', type: 'button', onclick: () => { el.remove(); openGallery(item.name); } }, t('查看')));
    setTimeout(() => el.remove(), 6000);
  }
}

async function openGallery(locate) {
  if (document.querySelector('dialog.gallery')) return;
  const dlg = h('dialog', { class: 'gallery' });
  let items = [];
  let dir = '';
  let filter = 'all';        // all | shot | rec
  let device = 'all';
  let previewIndex = -1;
  let prevLayer = null;

  const title = h('div', { class: 'dlg-h', style: 'display:flex;align-items:center;justify-content:space-between' },
    h('span', {}, t('相册')), ib('x', t('关闭'), () => dlg.close()));
  const bar = h('div', { class: 'gal-bar' });
  const body = h('div', { class: 'gal-body' });

  // 筛选：全部 / 截图 / 录屏
  const segBtns = {};
  const seg = h('div', { class: 'seg' },
    [['all', t('全部')], ['shot', t('截图')], ['rec', t('录屏')]].map(([k, l]) =>
      (segBtns[k] = h('button', { type: 'button', onclick: () => setFilter(k) }, l))));

  // 设备下拉
  const devSel = h('select', { class: 'select', onchange: () => { device = devSel.value; render(); } });

  // 媒体目录路径（点击复制）
  const dirEl = h('span', { class: 'gal-dir', title: t('点击复制') }, '');
  dirEl.onclick = () => { if (dir) copyText(dir); };

  // 打开文件夹（仅本机）
  const openBtn = h('button', { class: 'btn', type: 'button', hidden: !isLocalHost(), onclick: () => {
    api('/api/media/reveal', { method: 'POST', body: '{}' }).catch(e => toast(e.message, { kind: 'bad' }));
  } }, t('打开文件夹'));

  bar.append(seg, devSel, dirEl, openBtn);

  const form = h('form', { method: 'dialog' }, title, bar, body);
  dlg.append(form);
  dlg.addEventListener('close', () => { dlg.remove(); });
  // 预览层打开时，Esc 只关闭预览层而不是整个相册
  dlg.addEventListener('cancel', (e) => {
    if (previewIndex >= 0) { e.preventDefault(); closePreview(); }
  });
  // 键盘：← / → 切换上一张 / 下一张（预览打开时）
  dlg.addEventListener('keydown', (e) => {
    if (previewIndex < 0) return;
    const list = filtered();
    if (list.length <= 1) return;
    if (e.key === 'ArrowLeft') { e.preventDefault(); const ni = (previewIndex - 1 + list.length) % list.length; closePreview(); openPreview(ni); }
    else if (e.key === 'ArrowRight') { e.preventDefault(); const ni = (previewIndex + 1) % list.length; closePreview(); openPreview(ni); }
  });
  document.body.append(dlg);

  function setFilter(k) {
    filter = k;
    for (const key in segBtns) segBtns[key].classList.toggle('on', key === k);
    render();
  }

  function filtered() {
    return items.filter(it => (filter === 'all' || it.kind === filter) && (device === 'all' || it.device === device));
  }

  function fmtDate(ts) {
    const d = new Date(ts * 1000);
    const today = new Date(); today.setHours(0, 0, 0, 0);
    const yest = new Date(today); yest.setDate(yest.getDate() - 1);
    const day = new Date(d); day.setHours(0, 0, 0, 0);
    if (day.getTime() === today.getTime()) return t('今天');
    if (day.getTime() === yest.getTime()) return t('昨天');
    const mo = d.getMonth() + 1, dd = d.getDate();
    return d.getFullYear() + '-' + String(mo).padStart(2, '0') + '-' + String(dd).padStart(2, '0');
  }

  function fmtTime(ts) {
    const d = new Date(ts * 1000);
    return String(d.getHours()).padStart(2, '0') + ':' + String(d.getMinutes()).padStart(2, '0');
  }

  function fmtDur(secs) {
    secs = Math.max(0, Math.round(secs));
    const m = Math.floor(secs / 60), s = secs % 60;
    return String(m).padStart(2, '0') + ':' + String(s).padStart(2, '0');
  }

  function openPreview(i) {
    previewIndex = i;
    renderPreview();
  }

  function closePreview() {
    if (prevLayer) { prevLayer.remove(); prevLayer = null; }
    previewIndex = -1;
  }

  function renderPreview() {
    const list = filtered();
    if (previewIndex < 0 || previewIndex >= list.length) return;
    const it = list[previewIndex];
    const url = withToken(it.url) + (it.kind === 'rec' && !it.url.startsWith('data:') ? '#t=0.1' : '');
    const prev = h('div', { class: 'gal-prev' });
    prevLayer = prev;
    const closeBtn = ib('x', t('关闭'), () => closePreview());
    const media = it.kind === 'shot'
      ? h('img', { src: url })
      : h('video', { src: url, controls: true, autoplay: true });
    const nameEl = h('span', { class: 'pname' }, it.name);
    const dl = h('button', { class: 'btn', type: 'button', onclick: () => { const a = h('a', { href: url, download: it.name }); a.click(); } }, t('下载'));
    const revealBtn = h('button', { class: 'btn', type: 'button', hidden: !isLocalHost(), onclick: () => api('/api/media/reveal?name=' + encodeURIComponent(it.name), { method: 'POST', body: '{}' }).catch(e => toast(e.message, { kind: 'bad' })) }, t('打开所在文件夹'));
    const delBtn = h('button', { class: 'btn danger', type: 'button', onclick: async () => {
      if (!await confirmDialog(t('删除'), t('确定要删除这个文件吗？'), t('删除'), true)) return;
      try { await api('/api/media/' + encodeURIComponent(it.name), { method: 'DELETE' }); closePreview(); await load(); } catch (e) { toast(e.message, { kind: 'bad' }); }
    } }, t('删除'));
    prev.append(
      h('div', { class: 'phead' }, closeBtn, nameEl),
      h('div', { class: 'pbody' }, media),
      h('div', { class: 'pfoot' }, dl, revealBtn, delBtn, h('span', { class: 'pname' }, it.device + ' · ' + fmtDate(it.time) + ' ' + fmtTime(it.time))));
    if (list.length > 1) {
      const prevBtn = h('button', { class: 'gal-nav prev-btn', type: 'button', html: svg('chevLeft') });
      prevBtn.onclick = () => { const ni = (previewIndex - 1 + list.length) % list.length; closePreview(); openPreview(ni); };
      const nextBtn = h('button', { class: 'gal-nav next-btn', type: 'button', html: svg('chevLeft') });
      nextBtn.style.transform = 'translateY(-50%) rotate(180deg)';
      nextBtn.onclick = () => { const ni = (previewIndex + 1) % list.length; closePreview(); openPreview(ni); };
      prev.append(prevBtn, nextBtn);
    }
    dlg.append(prev);
  }

  function render() {
    // 更新设备下拉
    const devices = [...new Set(items.map(it => it.device))].sort();
    const cur = device;
    devSel.replaceChildren(h('option', { value: 'all' }, t('全部')), ...devices.map(d => h('option', { value: d }, d)));
    devSel.value = devices.includes(cur) ? cur : 'all';
    device = devSel.value;
    dirEl.textContent = dir;

    const list = filtered();
    if (!list.length) { body.replaceChildren(h('div', { class: 'gal-empty' }, t('还没有截图或录屏'))); return; }
    const groups = new Map();
    for (const it of list) {
      const key = fmtDate(it.time);
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(it);
    }
    const frag = document.createDocumentFragment();
    for (const [key, arr] of groups) {
      const grid = h('div', { class: 'gal-grid' });
      for (const it of arr) {
        const url = withToken(it.url) + (it.kind === 'rec' && !it.url.startsWith('data:') ? '#t=0.1' : '');
        let thumb;
        // 录屏用 <video> 显示首帧；演示里 mock 的 dataURL 是静态图，用 <img> 显示
        if (it.kind === 'shot' || it.url.startsWith('data:')) thumb = h('img', { class: 'thumb', src: url, loading: 'lazy' });
        else thumb = h('video', { class: 'thumb', src: url, preload: 'metadata', muted: true });
        const idx = list.indexOf(it);
        const card = h('div', { class: 'gal-card', onclick: () => openPreview(idx) }, thumb,
          h('div', { class: 'meta' }, h('span', { class: 'dev' }, it.device), h('div', { class: 'time' }, fmtTime(it.time))));
        // 录屏：在卡片右上角叠播放标记 + 时长（video 子元素不渲染，需放在卡片上）
        if (it.kind === 'rec') {
          const flag = h('span', { class: 'vid-flag' }, h('span', { html: svg('play') }).firstChild, h('span', { class: 'dur' }, ''));
          card.append(flag);
          thumb.addEventListener('loadedmetadata', () => {
            let d = thumb.duration;
            if (!isFinite(d)) {
              // 时长未知（某些分段流为 Infinity）：seek 到很大再读，或干脆不显示
              const old = thumb.currentTime;
              thumb.currentTime = 1e9;
              thumb.addEventListener('seeked', () => { flag.querySelector('.dur').textContent = fmtDur(thumb.duration); }, { once: true });
              thumb.currentTime = old;
            } else {
              flag.querySelector('.dur').textContent = fmtDur(d);
            }
          });
        }
        grid.append(card);
      }
      frag.append(h('div', { class: 'gal-group' }, h('div', { class: 'gal-group-t' }, key), grid));
    }
    body.replaceChildren(frag);

    // 定位到指定文件
    if (locate) {
      const i = list.findIndex(it => it.name === locate);
      if (i >= 0) { openPreview(i); locate = null; }
    }
  }

  async function load() {
    const r = await api('/api/media');
    items = r.items || [];
    dir = r.dir || '';
    previewIndex = -1;
    render();
  }

  segBtns.all.classList.add('on');
  dlg.showModal();
  try { await load(); } catch (e) { body.replaceChildren(h('div', { class: 'gal-empty' }, e.message)); }
}
