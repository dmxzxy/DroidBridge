// 设备文件管理：浏览共享存储、上传、下载、新建文件夹、重命名、删除
// 由 web/index.html 拆分而来；经典脚本，按 index.html 中的顺序加载，共享全局作用域。
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

function filesDialog(hst, dev) {
  const peer = hst.id, serial = dev.serial;
  let path = '/sdcard', parentPath = null, entries = [], space = null, busy = false;
  const titleEl = h('span', { class: 'fc-path', title: path });
  const list = h('div', { class: 'fc-list' });
  const upBtn = ib('chevLeft', t('上一级'), () => { if (parentPath) load(parentPath); });
  const freeEl = h('span', { class: 'fc-free' });
  const picker = h('input', { type: 'file', multiple: true, hidden: true, onchange: () => { const f = [...picker.files]; picker.value = ''; if (f.length) upload(f); } });
  const count = h('span', { class: 'apps-n' });

  const call = (name, body) => api('/api/files/' + name + '?peer=' + encodeURIComponent(peer), { method: 'POST', body: JSON.stringify({ serial, ...body }) });

  function renderPath() {
    // 路径过长时保留末尾几段（不用 direction:rtl，否则开头的 / 会被显示到结尾）
    const short = (p) => {
      if (p.length <= 42) return p;
      const parts = p.split('/').filter(Boolean);
      let out = '…';
      for (let i = parts.length - 1; i >= 0 && out.length + parts[i].length < 40; i--) out = out + '/' + parts[i];
      return out;
    };
    titleEl.textContent = short(path);
    titleEl.title = path;
    upBtn.disabled = !parentPath;
  }

  function row(e) {
    const full = (path.replace(/\/$/, '') + '/' + e.name);
    const click = () => { if (e.dir) load(full); };
    const items = () => [
      e.dir ? { icon: 'folder', label: t('打开'), fn: () => load(full) } : { icon: 'download', label: t('下载到电脑'), fn: () => download(full, e) },
      '-',
      { icon: 'edit', label: t('重命名…'), fn: () => rename(full, e) },
      { icon: 'copy', label: t('复制路径'), fn: () => copyText(full) },
      '-',
      { icon: 'trash', label: t('删除…'), danger: true, fn: () => remove(full, e) },
    ];
    const acts = h('div', { class: 'fc-acts' },
      e.dir ? null : ib('download', t('下载到电脑'), (ev) => { ev.stopPropagation(); download(full, e); }),
      ib('more', t('更多'), (ev) => {
        ev.stopPropagation();
        menu(ev.currentTarget, [
          { icon: 'edit', label: t('重命名…'), fn: () => rename(full, e) },
          { icon: 'trash', label: t('删除…'), danger: true, fn: () => remove(full, e) },
        ]);
      }));
    return ctxMenu(h('div', { class: 'fc-row' + (e.dir ? ' dir' : ''), onclick: click, title: full },
      h('span', { class: 'fc-ico', html: svg(e.dir ? 'folder' : 'file') }),
      h('div', { class: 'fc-t' }, h('b', {}, e.name), h('small', {}, fmtTime(e.mtime) + (e.dir ? '' : ' · ' + fmtSize(e.size)))),
      acts), items);
  }

  function render() {
    renderPath();
    count.textContent = entries.length ? t('{0} 项', entries.length) : '';
    freeEl.textContent = space ? t('可用 {0} / {1}', fmtSize(space.free), fmtSize(space.total)) : '';
    if (!entries.length) {
      list.replaceChildren(h('div', { class: 'apps-empty' }, h('span', {}, t('这个文件夹是空的'))));
      return;
    }
    list.replaceChildren(...entries.map(row));
  }

  async function load(p) {
    if (busy) return;
    busy = true;
    list.replaceChildren(h('div', { class: 'apps-empty' }, h('span', { class: 'spin' }), t('正在读取…')));
    try {
      const r = await call('list', { path: p });
      path = r.path; parentPath = r.parent; entries = r.items; space = r.space;
      render();
    } catch (e) {
      list.replaceChildren(h('div', { class: 'apps-empty' }, h('span', {}, e.message), h('button', { class: 'btn', type: 'button', onclick: () => load(p) }, t('重试'))));
    } finally { busy = false; }
  }

  async function upload(files) {
    const dir = path;
    const tt = toast(t('正在上传 0/{0}…', files.length), { sticky: true });
    let ok = 0;
    for (const [i, f] of files.entries()) {
      tt.set(t('正在上传 {0}（{1}/{2}）', f.name, i + 1, files.length));
      try {
        const qs = new URLSearchParams({ serial, peer, path: dir, name: f.name, lang: LANG });
        const r = await fetch(withToken('/api/files/upload?' + qs), { method: 'POST', body: f });
        const j = await r.json().catch(() => ({}));
        if (!r.ok) throw new Error(j.error || ('HTTP ' + r.status));
        ok++;
      } catch (e) {
        tt.set(t('上传 {0} 失败：', f.name) + e.message, 'bad');
        tt.close(6000);
        if (path === dir) load(dir);
        return;
      }
    }
    tt.set(t('已上传 {0} 个文件', ok), 'ok');
    tt.close(2500);
    if (path === dir) load(dir);
  }

  async function download(full, e) {
    const tt = toast(t('正在下载 {0}…', e.name), { sticky: true });
    try {
      const qs = new URLSearchParams({ serial, peer, path: full });
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

  async function rename(full, e) {
    const to = await promptDialog(t('重命名'), e.name, { value: e.name });
    if (to === null || to.trim() === e.name) return;
    try {
      await call('rename', { path: full, to });
      toast(t('已重命名'), { kind: 'ok' });
      load(path);
    } catch (err) { toast(err.message, { kind: 'bad', ms: 5000 }); }
  }

  async function remove(full, e) {
    if (!await confirmDialog(t('删除“{0}”？', e.name), e.dir ? t('文件夹及其中的内容都会被删除，无法恢复。') : t('文件会被删除，无法恢复。'), t('删除'), true)) return;
    try {
      await call('delete', { paths: [full] });
      toast(t('已删除'), { kind: 'ok' });
      load(path);
    } catch (err) { toast(err.message, { kind: 'bad', ms: 5000 }); }
  }

  const newFolder = async () => {
    const name = await promptDialog(t('新建文件夹'), null, { placeholder: t('文件夹名称') });
    if (name === null || !name.trim()) return;
    try {
      await call('mkdir', { path, to: name.trim() });
      toast(t('已创建'), { kind: 'ok' });
      load(path);
    } catch (err) { toast(err.message, { kind: 'bad', ms: 5000 }); }
  };

  ctxMenu(list, () => [
    { icon: 'upload', label: t('上传文件…'), fn: () => picker.click() },
    { icon: 'folder', label: t('新建文件夹…'), fn: () => newFolder() },
    { icon: 'refresh', label: t('刷新'), fn: () => load(path) },
  ]);
  const body = [
    h('div', { class: 'fc-bar' }, upBtn, titleEl, ib('refresh', t('刷新'), () => load(path))),
    list,
    picker,
  ];
  const dlg = dialog({
    title: t('文件管理') + ' · ' + (dev.model || serial), cls: 'files',
    body,
    actions: [
      h('span', { class: 'fc-meta' }, count, freeEl),
      ib('upload', t('上传文件'), () => picker.click()),
      h('button', { class: 'btn', type: 'button', onclick: newFolder }, t('新建文件夹')),
      h('button', { class: 'btn primary', type: 'button', onclick: () => dlg.close() }, t('关闭')),
    ],
  });
  load(path);
}
