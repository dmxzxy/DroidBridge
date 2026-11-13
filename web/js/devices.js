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

/** 设备行副标题：连接方式 + 简短标识（无线设备的 mDNS 名称太长，只显示“无线”） */
function devSub(d) {
  if (!d.wireless) return 'USB · ' + d.serial;
  return d.serial.includes('._adb-tls') ? t('无线') : t('无线') + ' · ' + d.serial.replace(/:5555$/, '');
}

function devRows(hst, emptyEl) {
  if (!hst.loaded) return [h('div', { class: 'empty' }, t('正在读取…'))];
  // 详细错误放在标题行的提示里，这里只给出处理入口
  if (hst.error) return [h('div', { class: 'empty' },
    h('button', { class: 'linkbtn', type: 'button', onclick: async () => { await refreshHost(hst); renderSide(); if (hst.error) toast(hst.error, { kind: 'bad', ms: 5000 }); } }, t('重试')),
    hst.remote ? [' · ', h('button', { class: 'linkbtn', type: 'button', onclick: () => peerDialog(hst) }, t('编辑'))] : null)];
  if (!hst.devices.length) return [emptyEl];
  // 可用的设备排在前面，未授权 / 离线的放后面
  const list = [...hst.devices].sort((a, b) => (a.state !== 'device') - (b.state !== 'device'));
  return list.map(d => {
    const ready = d.state === 'device';
    const open = [...viewers.values()].some(v => v.host.id === hst.id && v.dev.serial === d.serial);
    return ctxMenu(h('div', {
      class: 'dev' + (open ? ' open' : '') + (ready ? '' : ' na'), title: d.serial,
      onclick: () => ready ? openViewer(hst, d) : toast(STATE_HINT[d.state] || (t('设备状态：') + d.state)),
    }, h('span', { class: 'dot ' + (ready ? 'ok' : 'warn') }),
      h('div', { class: 't' }, h('b', {}, d.model || d.serial), h('small', {}, ready ? devSub(d) : (STATE_HINT[d.state] || d.serial))),
      ready ? null : h('span', { class: 'st' }, STATE_TEXT[d.state] || d.state),
      ready ? ib('more', t('更多'), e => { e.stopPropagation(); deviceMenu(e.currentTarget, hst, d); }) : null), () => deviceItems(hst, d));
  });
}

/** 设备菜单项：不打开投屏也能管理应用和文件 */
function deviceItems(hst, dev) {
  const ready = dev.state === 'device';
  const open = findViewer(hst, dev);
  return [
    ready ? (open
      ? { icon: 'x', label: t('关闭投屏'), fn: () => open.close() }
      : { icon: 'phone', label: t('开始投屏'), fn: () => openViewer(hst, dev) }) : null,
    ready ? '-' : null,
    ready ? { icon: 'grid', label: t('应用管理…'), fn: () => appsDialog(hst, dev) } : null,
    ready ? { icon: 'folder', label: t('文件管理…'), fn: () => open ? open.toggleFiles(true) : filesDialog(hst, dev) } : null,
    ready ? '-' : null,
    { icon: 'copy', label: t('复制序列号'), fn: () => copyText(dev.serial) },
  ];
}
/** “⋯”按钮：只放管理类操作（点设备行本身就是开始投屏） */
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
  const remotes = [...hosts.values()].filter(x => x.remote);
  const out = [];
  if (local) out.push(hostGroup(local, h('div', { class: 'empty' }, t('未检测到设备 · '), h('a', { href: '#', onclick: e => { e.preventDefault(); helpDialog(); } }, t('如何连接')))));
  for (const r of remotes) out.push(hostGroup(r, h('div', { class: 'empty' }, t('没有设备'))));
  $('#lists').replaceChildren(...out);
  renderSideFoot();
}

/** 一台电脑一组：标题行（名称 + 设备数 / 状态，可折叠）+ 设备列表 */
function hostGroup(hst, emptyEl) {
  const local = !hst.remote;
  const key = 'fold_' + hst.id;
  const folded = pref.get(key, '0') === '1';
  const devs = hst.devices || [];
  const viewing = [...viewers.values()].filter(v => v.host.id === hst.id).length;
  const short = hst.error ? (/令牌|token/i.test(hst.error) ? t('令牌错误') : t('无法连接')) : null;
  const status = short || (!hst.loaded ? t('正在读取…') : !devs.length ? t('没有设备')
    : t('{0} 台设备', devs.length) + (viewing ? ' · ' + t('{0} 台投屏中', viewing) : ''));
  const upd = local ? null : hst.updating
    ? h('span', { class: 'upd busy' }, h('span', { class: 'spin sm' }), t('更新中'))
    : needsUpdate(hst) ? h('button', { class: 'upd', type: 'button', title: t('对方 {0}，本机 {1}，点击更新', hst.info.version, INFO.version), onclick: e => { e.stopPropagation(); updatePeer(hst); } }, t('可更新')) : null;
  const tip = local ? (INFO ? INFO.name : '') : hst.name + '\n' + hst.addr + (hst.info ? ' · v' + hst.info.version : '') + (hst.error ? '\n' + hst.error : '');
  const head = ctxMenu(h('div', {
    class: 'host' + (local ? ' local' : ''), title: tip,
    onclick: () => { pref.set(key, folded ? '0' : '1'); renderSide(); },
  },
    h('span', { class: 'chev', html: svg('chevDown') }),
    h('span', { class: 'hi' + (short ? ' err' : ''), html: svg('monitor') }),
    h('div', { class: 't' }, h('b', {}, local ? t('本机') : hst.name),
      h('div', { class: 'sub' }, h('small', { class: short ? 'err' : '' }, status), upd)),
    // 操作按钮悬停时浮在右侧，平时不占宽度，名称可以显示完整
    h('div', { class: 'acts' },
      hst.error ? null : ib('wifi', t('无线连接设备'), e => { e.stopPropagation(); wirelessDialog(hst); }),
      local ? null : ib('more', t('更多'), e => { e.stopPropagation(); hostMenu(e.currentTarget, hst); }))),
  () => local ? [
    { icon: 'wifi', label: t('无线连接设备…'), fn: () => wirelessDialog(hst) },
    { icon: 'refresh', label: t('刷新'), fn: async () => { await refreshHost(hst); renderSide(); } },
  ] : hostItems(hst));
  return h('div', { class: 'grp' + (folded ? ' folded' : '') }, head, folded ? null : devRows(hst, emptyEl));
}

/** 侧栏底部：添加远程电脑。局域网里发现的电脑放在添加窗口里，这里只提示数量 */
function renderSideFoot() {
  const foot = $('#sideFoot');
  if (!foot) return;
  const n = FOUND.length;
  foot.replaceChildren(h('button', { class: 'add-peer', type: 'button', onclick: () => peerDialog() },
    h('span', { html: svg('plus') }).firstChild, h('span', { class: 't' }, t('添加远程电脑')),
    n ? h('span', { class: 'found-n', title: t('在局域网中发现 {0} 台电脑', n) }, t('发现 {0} 台', n)) : null));
}
