// 电脑与设备：刷新、匹配、侧栏
// 由 web/index.html 拆分而来；经典脚本，按 index.html 中的顺序加载，共享全局作用域。
'use strict';

// ---------------- 电脑与设备 ----------------
let INFO = null;
const hosts = new Map();
const viewers = new Map();
const STATE_TEXT = { unauthorized: t('未授权'), offline: t('离线'), recovery: 'Recovery', sideload: 'Sideload', bootloader: 'Bootloader' };
const STATE_HINT = { unauthorized: t('请在手机上点击“允许 USB 调试”'), offline: t('设备离线，请重新插拔数据线') };

async function loadPeers() {
  const peers = await api('/api/peers');
  const ids = new Set(['local', ...peers.map(p => p.id)]);
  for (const id of [...hosts.keys()]) if (!ids.has(id)) hosts.delete(id);
  if (!hosts.has('local')) hosts.set('local', { id: 'local', name: (INFO && INFO.name) || t('本机'), devices: [] });
  for (const p of peers) hosts.set(p.id, Object.assign(hosts.get(p.id) || { devices: [] }, p, { remote: true }));
}
async function refreshHost(hst) {
  try { hst.devices = await api('/api/devices?peer=' + encodeURIComponent(hst.id)); hst.error = null; }
  catch (e) { hst.error = e.message; hst.devices = []; }
  hst.loaded = true;
  // 远程电脑的版本信息：每 60 秒取一次，用于显示“可更新”
  if (hst.remote && !hst.error && (!hst.infoAt || Date.now() - hst.infoAt > 60000)) {
    try { hst.info = await api('/api/node?peer=' + encodeURIComponent(hst.id)); hst.infoAt = Date.now(); } catch {}
  }
}

/** 比较版本号 a 与 b：-1 / 0 / 1 */
function cmpVer(a, b) {
  const x = String(a || '0').split('.').map(n => parseInt(n, 10) || 0), y = String(b || '0').split('.').map(n => parseInt(n, 10) || 0);
  for (let i = 0; i < Math.max(x.length, y.length); i++) { const d = (x[i] || 0) - (y[i] || 0); if (d) return d < 0 ? -1 : 1; }
  return 0;
}
/** 远程电脑是否比本机旧（正式版按版本号比较；开发版版本号相同时再比较构建） */
function needsUpdate(r) {
  if (!r.info || !INFO || r.error) return false;
  const c = cmpVer(r.info.version, INFO.version);
  if (c) return c < 0;
  return !INFO.official && !!r.info.build && r.info.build !== INFO.build;
}

/** 一键更新：用安装时保存的 SSH 登录方式，把本机版本装到远程电脑 */
async function updatePeer(r) {
  if (r.updating) return;
  if (r.info && r.info.os === 'windows') {
    toast(t('「{0}」是 Windows 电脑，请在对方电脑上下载新版本安装包并运行', r.name), { ms: 6000 });
    return;
  }
  if (!r.ssh || r.ssh.auth === 'password') return peerDialog(r, { install: true });
  r.updating = true; renderSide();
  const tt = toast(t('正在更新「{0}」…', r.name), { sticky: true });
  try {
    const job = (await api('/api/peers/' + encodeURIComponent(r.id) + '/update', { method: 'POST', body: '{}' })).job;
    while (true) {
      await new Promise(res => setTimeout(res, 1000));
      let st; try { st = await api('/api/install/' + job); } catch { continue; }
      if (st.state === 'running') { tt.set(t('正在更新「{0}」：', r.name) + (st.steps[st.steps.length - 1] || '…')); continue; }
      if (st.state === 'done') tt.set(t('「{0}」已更新到 {1}', r.name, INFO.version), 'ok');
      else throw new Error(st.error);
      break;
    }
  } catch (e) {
    tt.set(t('更新失败：') + e.message, 'bad');
  }
  tt.close(6000);
  r.updating = false; r.infoAt = 0;
  refreshAll();
}
let FOUND = [];
async function refreshAll() {
  try { await loadPeers(); } catch {}
  await Promise.all([...hosts.values()].map(refreshHost).concat(api('/api/discovered').then(r => { FOUND = r.list || []; }, () => {})));
  renderSide();
}

/** 在电脑的设备列表中找到同一台设备（无线重连后 serial 可能变化，按 uid 匹配） */
function matchDevice(hst, dev) {
  const list = hst.devices || [];
  return list.find(d => d.serial === dev.serial && d.state === 'device')
    || (dev.uid && list.find(d => d.uid === dev.uid && d.state === 'device'))
    || list.find(d => d.serial === dev.serial);
}
function findViewer(hst, dev) {
  for (const v of viewers.values()) if (v.host.id === hst.id && (v.dev.serial === dev.serial || (dev.uid && v.dev.uid === dev.uid))) return v;
  return null;
}

function devRows(hst, emptyEl) {
  if (!hst.loaded) return [h('div', { class: 'empty' }, t('正在读取…'))];
  if (hst.error) return [h('div', { class: 'empty err' }, hst.error)];
  if (!hst.devices.length) return [emptyEl];
  return hst.devices.map(d => {
    const ready = d.state === 'device';
    return h('div', {
      class: 'dev' + ([...viewers.values()].some(v => v.host.id === hst.id && v.dev.serial === d.serial) ? ' open' : '') + (ready ? '' : ' na'), title: d.serial,
      onclick: () => ready ? openViewer(hst, d) : toast(STATE_HINT[d.state] || (t('设备状态：') + d.state)),
    }, h('span', { class: 'dot ' + (ready ? 'ok' : 'warn') }),
      h('div', { class: 't' }, h('b', {}, d.model || d.serial), d.model ? h('small', {}, d.serial) : null),
      d.wireless ? h('span', { class: 'wl', title: t('无线连接'), html: svg('wifi') }) : null,
      ready ? null : h('span', { class: 'st' }, STATE_TEXT[d.state] || d.state),
      ready ? ib('more', t('更多'), e => { e.stopPropagation(); deviceMenu(e.currentTarget, hst, d); }) : null);
  });
}

/** 设备菜单：不打开投屏也能管理应用和文件 */
function deviceMenu(anchor, hst, dev) {
  menu(anchor, [
    { icon: 'grid', label: t('应用管理…'), fn: () => appsDialog(hst, dev) },
    { icon: 'folder', label: t('文件管理…'), fn: () => filesDialog(hst, dev) },
  ]);
}

function renderSide() {
  // 侧栏里有展开的菜单时先不刷新，关闭菜单后再补上
  if (openMenu && $('#lists').contains(openMenu.anchor)) { renderPending = true; return; }
  renderPending = false;
  const local = hosts.get('local');
  const out = [h('div', { class: 'sec' }, t('本机'), local ? ib('wifi', t('无线连接设备'), () => wirelessDialog(local)) : null)];
  if (local) out.push(...devRows(local, h('div', { class: 'empty' }, t('未检测到设备 · '), h('a', { href: '#', onclick: e => { e.preventDefault(); helpDialog(); } }, t('如何连接')))));
  out.push(h('div', { class: 'sec' }, t('远程电脑'), ib('plus', t('添加远程电脑'), () => peerDialog())));
  const remotes = [...hosts.values()].filter(x => x.remote);
  if (!remotes.length && !FOUND.length) out.push(h('div', { class: 'empty' }, t('添加局域网内运行 DroidBridge 的电脑')));
  for (const r of remotes) {
    const short = r.error ? (/令牌|token/i.test(r.error) ? t('令牌错误') : t('无法连接')) : null;
    const upd = r.updating
      ? h('span', { class: 'upd busy' }, h('span', { class: 'spin sm' }), t('更新中'))
      : needsUpdate(r) ? h('button', { class: 'upd', type: 'button', title: t('对方 {0}，本机 {1}，点击更新', r.info.version, INFO.version), onclick: () => updatePeer(r) }, t('可更新')) : null;
    out.push(h('div', { class: 'host', title: r.addr + (r.info ? ' · v' + r.info.version : '') + (r.error ? '\n' + r.error : '') },
      h('span', { class: 'hn' }, r.name),
      h('span', { class: 'meta' + (short ? ' err' : '') }, short || r.addr),
      upd,
      ib('more', t('更多'), e => hostMenu(e.currentTarget, r))));
    if (!r.error) out.push(...devRows(r, h('div', { class: 'empty' }, t('没有设备'))));
  }
  if (FOUND.length) {
    out.push(h('div', { class: 'found-t' }, t('在局域网中发现')));
    for (const f of FOUND) out.push(h('div', { class: 'found', title: f.addr, onclick: () => addFound(f) },
      h('span', { class: 'hn' }, f.name), h('span', { class: 'meta' }, f.addr),
      ib('plus', t('添加'), e => { e.stopPropagation(); addFound(f); })));
  }
  $('#lists').replaceChildren(...out);
}
