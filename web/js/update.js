// 桌面版自身更新：启动后检查新版本，提示后一键下载安装（程序会自行重启）
// 由 web/index.html 拆分而来；经典脚本，按 index.html 中的顺序加载，共享全局作用域。
'use strict';

let UPDATE = null;          // /api/update/check 的结果
let updateBadge = null;     // 标题栏上的“新版本”按钮

const updateSupported = () => UPDATE && UPDATE.supported;

/** 启动时静默检查一次（失败不打扰） */
async function checkUpdate(silent = true) {
  try {
    const r = await api('/api/update/check');
    UPDATE = r;
    if (r.newer) showUpdateBadge();
    return r;
  } catch (e) {
    if (!silent) toast(t('检查更新失败：') + e.message, { kind: 'bad', ms: 5000 });
    return null;
  }
}

function showUpdateBadge() {
  if (updateBadge || !UPDATE || !UPDATE.newer) return;
  updateBadge = h('button', { class: 'upd local', type: 'button', title: t('有新版本 {0}，点击查看', UPDATE.latest), onclick: () => updateDialog() }, t('新版本'));
  const brand = document.querySelector('.brand');
  const gear = $('#gear');
  if (brand && gear) brand.insertBefore(updateBadge, gear);
}

function updateDialog() {
  const r = UPDATE || {};
  const install = h('button', { class: 'btn primary', type: 'button' }, t('下载并安装'));
  const dlg = dialog({
    title: t('发现新版本 {0}', r.latest), cls: 'update',
    desc: t('当前版本 {0}', r.current),
    body: [
      h('div', { class: 'upd-what' },
        h('div', {}, t('更新会替换程序文件，设备列表和设置保持不变。')),
        h('div', {}, t('安装完成后程序会自动重新启动，正在投屏的画面会断开。'))),
      r.supported ? null : h('div', { class: 'upd-warn' }, t('当前安装位置不支持自动更新，请手动下载：'), h('a', { href: r.page, target: '_blank', rel: 'noreferrer' }, r.page)),
      h('div', { class: 'upd-link' }, h('a', { href: r.page, target: '_blank', rel: 'noreferrer' }, t('查看这个版本改了什么'))),
    ].filter(Boolean),
    actions: [h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, t('稍后')),
      r.supported ? install : null].filter(Boolean),
  });
  // 默认聚焦主按钮（避免焦点落在链接上出现框）
  (r.supported ? install : null)?.focus?.();
  install.onclick = async () => {
    install.disabled = true;
    install.textContent = t('正在下载…');
    const tt = toast(t('正在下载 {0}…', r.latest), { sticky: true });
    try {
      await api('/api/update/apply', { method: 'POST', body: '{}' });
      tt.set(t('正在安装，程序即将重新启动…'), 'ok');
      install.textContent = t('正在安装…');
      // 服务端会在约 1 秒后退出并重启；断开后每隔几秒尝试重新连接
      setTimeout(() => retryConnect(), 4000);
    } catch (e) {
      tt.set(e.message, 'bad');
      tt.close(8000);
      install.disabled = false;
      install.textContent = t('重试');
    }
  };
}

/** 更新后服务会短暂离线，恢复后自动刷新页面 */
async function retryConnect() {
  for (let i = 0; i < 60; i++) {
    try {
      const r = await fetch(withToken('/api/info'), { cache: 'no-store' });
      if (r.ok) { location.reload(); return; }
    } catch {}
    await new Promise(r => setTimeout(r, 2000));
  }
}

/** 设置页里的一项：版本号 + 检查更新 */
function updateSettingRow() {
  const ver = h('span', { class: 'v mono' }, 'v' + (INFO ? INFO.version : '?'));
  const btn = h('button', { class: 'btn', type: 'button' }, t('检查更新'));
  btn.onclick = async () => {
    btn.disabled = true;
    btn.textContent = t('正在检查…');
    const r = await checkUpdate(false);
    btn.disabled = false;
    if (!r) { btn.textContent = t('检查更新'); return; }
    if (r.newer) { btn.textContent = t('有新版本'); updateDialog(); }
    else { btn.textContent = t('已是最新'); setTimeout(() => { btn.textContent = t('检查更新'); }, 2500); }
  };
  return h('div', { class: 'set-r' }, h('span', { class: 'k' }, t('版本')), ver, btn);
}
