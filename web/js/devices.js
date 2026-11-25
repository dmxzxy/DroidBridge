// 电脑与设备：刷新、匹配、侧栏
// 由 web/index.html 拆分而来；经典脚本，按 index.html 中的顺序加载，共享全局作用域。
'use strict';

// ---------------- 电脑与设备 ----------------
let INFO = null;
const hosts = new Map();
const viewers = new Map();
const STATE_TEXT = { unauthorized: t('未授权'), offline: t('离线'), recovery: 'Recovery', sideload: 'Sideload', bootloader: 'Bootloader' };
const STATE_HINT = { unauthorized: t('请在手机上点击“允许 USB 调试”'), offline: t('设备离线，请重新插拔数据线') };
let sideQuery = '';
const SIDE_W = 252;
/** 设备总数达到这个数量才显示侧栏搜索框 */
const SIDE_SEARCH_MIN = 8;

function devAlias(serial) {
  return (pref.get('alias_' + serial, '') || '').trim();
}
function setDevAlias(serial, val) {
  const v = (val || '').trim();
  if (v) pref.set('alias_' + serial, v);
  else localStorage.removeItem('db_alias_' + serial);
}
function devDisplayName(d) {
  return devAlias(d.serial) || d.model || d.serial;
}
async function editDevAlias(hst, dev) {
  const next = await promptDialog(t('设置备注名'), (dev.model ? dev.model + ' · ' : '') + dev.serial, {
    placeholder: dev.model || dev.serial, value: devAlias(dev.serial), okText: t('保存'),
  });
  if (next === null) return;
  setDevAlias(dev.serial, next);
  for (const v of viewers.values()) {
    if (v.dev.serial === dev.serial && v.nameEl) v.nameEl.textContent = devDisplayName(v.dev);
  }
  renderSide();
}

/** 收起 / 展开侧栏 */
function toggleSidebar(force) {
  if (isMobile()) return;
  const collapsed = force === undefined ? !document.body.classList.contains('side-collapsed') : !!force;
  document.body.classList.toggle('side-collapsed', collapsed);
  pref.set('side_collapsed', collapsed ? '1' : '0');
  const tip = collapsed ? t('展开侧栏 (Ctrl+B)') : t('收起侧栏 (Ctrl+B)');
  for (const el of document.querySelectorAll('.side-tog')) { el.title = tip; el.setAttribute('aria-label', tip); }
  setTimeout(fitAllViewers, 180);
}

function initSidebarLayout() {
  const savedW = parseInt(pref.get('side_w', String(SIDE_W)), 10);
  if (savedW >= 190 && savedW <= 440) document.documentElement.style.setProperty('--side-w', savedW + 'px');
  if (pref.get('side_collapsed', '0') === '1' && !isMobile()) document.body.classList.add('side-collapsed');

  // 品牌行的 logo 悬停时变成收起按钮；收起后左侧边缘留一个展开把手
  const logo = $('#logo');
  logo.append(h('span', { class: 'tog', html: svg('sidebar') }));
  logo.onclick = () => toggleSidebar();
  $('#sidePeek').innerHTML = svg('chevRight');
  $('#sidePeek').onclick = () => toggleSidebar(false);
  toggleSidebar(document.body.classList.contains('side-collapsed'));

  document.addEventListener('keydown', e => {
    if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'b') {
      if (document.querySelector('dialog[open]') || isMobile()) return;
      e.preventDefault();
      toggleSidebar();
    }
  });

  // 侧栏边缘拖拽调宽，双击恢复默认宽度
  const resizer = $('#sideResizer');
  resizer.title = t('拖拽调整侧栏宽度（双击重置）');
  const setW = (w) => { document.documentElement.style.setProperty('--side-w', w + 'px'); fitAllViewers(); };
  resizer.addEventListener('dblclick', () => { setW(SIDE_W); pref.set('side_w', String(SIDE_W)); });
  resizer.addEventListener('pointerdown', e => {
    if (e.button !== 0 || isMobile()) return;
    e.preventDefault();
    resizer.setPointerCapture(e.pointerId);
    document.body.classList.add('resizing-side');
    let w = SIDE_W;
    const onMove = ev => { w = Math.max(190, Math.min(440, Math.round(ev.clientX))); setW(w); };
    const onUp = () => {
      document.body.classList.remove('resizing-side');
      pref.set('side_w', String(w));
      resizer.removeEventListener('pointermove', onMove);
      resizer.removeEventListener('pointerup', onUp);
      resizer.removeEventListener('pointercancel', onUp);
    };
    resizer.addEventListener('pointermove', onMove);
    resizer.addEventListener('pointerup', onUp);
    resizer.addEventListener('pointercancel', onUp);
  });

  // 搜索框：设备多时才出现
  const box = $('#sideSearch');
  const inp = h('input', {
    class: 'input', type: 'search', placeholder: t('搜索'), spellcheck: 'false', title: t('型号、备注或序列号'),
    oninput: () => { sideQuery = inp.value.trim().toLowerCase(); renderSide(); },
    onkeydown: e => { if (e.key === 'Escape' && inp.value) { inp.value = ''; sideQuery = ''; renderSide(); } },
  });
  box.className = 'side-search';
  box.hidden = true;
  box.replaceChildren(h('span', { html: svg('search') }), inp);
}

async function loadPeers() {
  const peers = await api('/api/peers');
  const ids = new Set(['local', ...peers.map(p => p.id)]);
  for (const id of [...hosts.keys()]) if (!ids.has(id)) hosts.delete(id);
  if (!hosts.has('local')) hosts.set('local', { id: 'local', name: (INFO && INFO.name) || t('本机'), devices: [] });
  for (const p of peers) hosts.set(p.id, Object.assign(hosts.get(p.id) || { devices: [] }, p, { remote: true }));
}
async function refreshHost(hst) {
  // 共享会话概况（谁在看、谁在操作）：旧版本或没有这个接口时忽略，不影响设备列表
  const mirrors = api('/api/mirrors?peer=' + encodeURIComponent(hst.id)).then(r => {
    const m = new Map();
    for (const it of (r && r.list) || []) if (it && it.serial) m.set(it.serial, { viewers: it.viewers || [], controller: it.controller || null });
    hst.mirrors = m;
  }, () => {});
  // 谁连着这台电脑：旧版本没有这个接口时为 null（不显示）
  const presence = api('/api/presence?peer=' + encodeURIComponent(hst.id)).then(r => { hst.presence = (r && r.list) || []; }, () => { hst.presence = null; });
  try { hst.devices = await api('/api/devices?peer=' + encodeURIComponent(hst.id)); hst.error = null; }
  catch (e) { hst.error = e.message; hst.devices = []; }
  hst.loaded = true;
  // 远程电脑的版本信息：每 60 秒取一次，用于显示“可更新”
  if (hst.remote && !hst.error && (!hst.infoAt || Date.now() - hst.infoAt > 60000)) {
    try { hst.info = await api('/api/node?peer=' + encodeURIComponent(hst.id)); hst.infoAt = Date.now(); } catch {}
  }
  await mirrors;
  await presence;
  if (!hst.remote) notifyPresence(hst);
}

// ---------------- 谁连着这台电脑 ----------------
/** 除了自己以外，正连着这台电脑的人 */
function presenceOthers(hst) {
  return (hst.presence || []).filter(p => !p.you);
}
/** 设备序列号 → 备注或型号（显示用） */
function devLabel(hst, serial) {
  const d = (hst.devices || []).find(x => x.serial === serial);
  return devAlias(serial) || (d && d.model) || serial;
}
/** 悬停提示里的一行：名字 · 在看的设备 */
function presenceLine(hst, p) {
  const devs = (p.views || []).map(v => devLabel(hst, v.serial));
  return devs.length ? p.name + ' · ' + devs.join('、') : p.name;
}
/** 本机：有人新连上时提示一次（首次读取时不提示） */
function notifyPresence(hst) {
  if (!hst.presence) return;
  const keys = new Set(presenceOthers(hst).map(p => p.key));
  if (hst.seenKeys) for (const p of presenceOthers(hst)) if (!hst.seenKeys.has(p.key)) toast(t('{0} 已连接', p.name), { ms: 3000 });
  hst.seenKeys = keys;
}
/** 标题行：还有几个人连着这台电脑，悬停看名字 */
function presenceBadge(hst) {
  const others = presenceOthers(hst);
  if (!others.length) return null;
  return h('span', { class: 'ppl', title: others.map(p => presenceLine(hst, p)).join('\n') },
    h('span', { class: 'ico', html: svg('users') }), String(others.length));
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
  pollPairRequests();
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

/** 远程电脑的错误是否表示“未授权本机 / 令牌错误” */
const needsAuth = (msg) => /授权|令牌|token|authoriz/i.test(msg || '');

/** 设备行悬停提示：备注 + 型号 + 连接方式 + 序列号 */
function devTip(d) {
  const alias = devAlias(d.serial);
  const head = alias ? `${alias} (${d.model || d.serial})` : (d.model || d.serial);
  const lines = [head, (d.wireless ? t('无线') : 'USB') + ' · ' + d.serial];
  if (d.state !== 'device') lines.push(STATE_HINT[d.state] || (t('设备状态：') + d.state));
  return lines.join('\n');
}

function shortSerial(s) {
  if (!s) return '';
  if (s.includes(':')) return s.split(':')[0].split('.').slice(-2).join('.');
  return s.length > 6 ? '…' + s.slice(-5) : s;
}

function devMatches(hst, d) {
  if (!sideQuery) return true;
  return [devAlias(d.serial), d.model, d.serial].filter(Boolean).join(' ').toLowerCase().includes(sideQuery);
}

function devRows(hst, emptyEl) {
  if (!hst.loaded) return [h('div', { class: 'empty' }, t('正在读取…'))];
  // 详细错误放在标题行的提示里，这里只给出处理入口
  if (hst.error && hst.remote && needsAuth(hst.error)) return [h('div', { class: 'empty' },
    h('button', { class: 'linkbtn', type: 'button', onclick: () => requestPeerAuth({ id: hst.id, addr: hst.addr, name: hst.name }) }, t('请求授权')),
    ' · ', h('button', { class: 'linkbtn', type: 'button', onclick: () => peerDialog(hst) }, t('填写令牌')))];
  if (hst.error) return [h('div', { class: 'empty' },
    h('button', { class: 'linkbtn', type: 'button', onclick: async () => { await refreshHost(hst); renderSide(); if (hst.error) toast(hst.error, { kind: 'bad', ms: 5000 }); } }, t('重试')),
    hst.remote ? [' · ', h('button', { class: 'linkbtn', type: 'button', onclick: () => peerDialog(hst) }, t('编辑'))] : null)];
  if (!hst.devices.length) return [emptyEl];
  // 同型号多台时追加短序列号区分
  const modelCnt = new Map();
  for (const d of hst.devices) if (d.model) modelCnt.set(d.model, (modelCnt.get(d.model) || 0) + 1);
  // 可用的设备排在前面，未授权 / 离线的放后面
  const list = [...hst.devices]
    .filter(d => devMatches(hst, d))
    .sort((a, b) => (a.state !== 'device') - (b.state !== 'device'));
  return list.map(d => {
    const ready = d.state === 'device';
    const open = [...viewers.values()].some(v => v.host.id === hst.id && v.dev.serial === d.serial);
    const alias = devAlias(d.serial);
    const tagText = !alias && d.model && modelCnt.get(d.model) > 1 ? shortSerial(d.serial) : null;
    return ctxMenu(h('div', {
      class: 'dev' + (open ? ' open' : '') + (ready ? '' : ' na'), title: devTip(d), 'data-serial': d.serial,
      onclick: () => ready ? openViewer(hst, d) : toast(STATE_HINT[d.state] || (t('设备状态：') + d.state)),
    }, h('span', { class: 'dot ' + (ready ? 'ok' : 'warn') }),
      h('div', { class: 't' }, h('b', {}, alias || d.model || d.serial)),
      d.wireless ? h('span', { class: 'wl', html: svg('wifi') }) : null,
      tagText ? h('span', { class: 'meta-tag' }, tagText) : null,
      ...pluginBadges(hst, d),
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
    ...pluginMenuItems(hst, 'device', { serial: dev.serial }),
    { icon: 'edit', label: t('设置备注名…'), fn: () => editDevAlias(hst, dev) },
    { icon: 'copy', label: t('复制序列号'), fn: () => copyText(dev.serial) },
  ];
}
/** “⋯”按钮：只放管理类操作（点设备行本身就是开始投屏） */
function deviceMenu(anchor, hst, dev) {
  menu(anchor, [
    { icon: 'grid', label: t('应用管理…'), fn: () => appsDialog(hst, dev) },
    { icon: 'folder', label: t('文件管理…'), fn: () => filesDialog(hst, dev) },
    '-',
    { icon: 'edit', label: t('设置备注名…'), fn: () => editDevAlias(hst, dev) },
  ]);
}

function renderSide() {
  // 有展开的菜单时先不刷新（菜单可能挂在设备行上，刷新会让它指向旧元素），关闭菜单后再补上
  if (openMenu) { renderPending = true; return; }
  renderPending = false;
  const local = hosts.get('local');
  const remotes = [...hosts.values()].filter(x => x.remote);
  const out = [];
  if (local) out.push(hostGroup(local, h('div', { class: 'empty' }, t('未检测到设备 · '), h('a', { href: '#', onclick: e => { e.preventDefault(); helpDialog(); } }, t('如何连接')))));
  for (const r of remotes) out.push(hostGroup(r, h('div', { class: 'empty' }, t('没有设备'))));
  $('#lists').replaceChildren(...out);
  renderSideFoot();
  const total = [...hosts.values()].reduce((n, x) => n + (x.devices || []).length, 0);
  $('#sideSearch').hidden = total < SIDE_SEARCH_MIN && !sideQuery;
  renderStageBar();
}

/** 一台电脑一组：标题行（名称 + 设备数角标，可折叠）+ 设备列表 */
function hostGroup(hst, emptyEl) {
  const local = !hst.remote;
  const key = 'fold_' + hst.id;
  const folded = pref.get(key, '0') === '1';
  const devs = hst.devices || [];
  const viewing = [...viewers.values()].filter(v => v.host.id === hst.id).length;
  const short = hst.error ? (needsAuth(hst.error) ? t('需要授权') : t('无法连接')) : null;
  // 设备数只用角标显示，数字含义放在悬停提示里
  const count = !short && hst.loaded && devs.length
    ? h('span', { class: 'cnt' + (viewing ? ' on' : ''), title: t('{0} 台设备', devs.length) + (viewing ? ' · ' + t('{0} 台投屏中', viewing) : '') }, String(devs.length))
    : null;
  const upd = local ? null : hst.updating
    ? h('span', { class: 'upd busy' }, h('span', { class: 'spin sm' }), t('更新中'))
    : needsUpdate(hst) ? h('button', { class: 'upd', type: 'button', title: t('对方 {0}，本机 {1}，点击更新', hst.info.version, INFO.version), onclick: e => { e.stopPropagation(); updatePeer(hst); } }, t('可更新')) : null;
  const tip = local ? (INFO ? INFO.name : '') : hst.name + '\n' + hst.addr + (hst.info ? ' · v' + hst.info.version : '') + (hst.error ? '\n' + hst.error : '');
  const ppl = short ? null : presenceBadge(hst);
  const head = ctxMenu(h('div', {
    class: 'host' + (local ? ' local' : ''), title: tip,
    onclick: () => { pref.set(key, folded ? '0' : '1'); renderSide(); },
  },
    h('span', { class: 'chev', html: svg('chevDown') }),
    h('span', { class: 'hi' + (short ? ' err' : ''), html: svg('monitor') }),
    h('div', { class: 't' }, h('div', { class: 'nm' }, h('b', {}, local ? t('本机') : hst.name), count),
      // 第二行只在有事时出现：连不上 / 可更新 / 还有别人连着
      short || upd || ppl ? h('div', { class: 'sub' }, short ? h('small', { class: 'err' }, short) : null, upd, ppl) : null),
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
  // 插件：状态栏项 + “工具”菜单入口
  if (typeof pluginRenderSideFoot === 'function') pluginRenderSideFoot(foot);
}
