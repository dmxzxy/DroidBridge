// 桌面版自定义标题栏：拖动窗口、最小化 / 最大化 / 关闭，以及版本号、检查更新、新版本提示
// 只在桌面客户端中启用（窗口启动时注入 window.__DB_CHROME，并提供 window.ipc）；浏览器中不显示。
'use strict';

const CHROME = window.ipc && window.__DB_CHROME ? window.__DB_CHROME : null;   // 'win' | 'mac' | 'linux' | null
const hasTitlebar = () => !!CHROME;
const winCmd = (c) => { try { window.ipc.postMessage(c); } catch {} };

let tbVer = null, tbUpd = null, tbMax = null;

function setupTitlebar() {
  if (!CHROME) return;
  document.body.classList.add('has-tb', 'tb-' + CHROME);
  const bar = $('#titlebar');
  bar.hidden = false;

  tbVer = h('button', { class: 'tb-ver', type: 'button', title: t('检查更新'), onclick: () => titlebarCheck() }, '');
  tbUpd = h('button', { class: 'tb-upd', type: 'button', hidden: true, onclick: () => updateDialog() });
  const tools = h('div', { class: 'tb-tools' },
    ib('package', t('批量安装 APK'), () => batchInstallDialog()),
    ib('images', t('相册'), () => openGallery()),
    ib('gear', t('设置'), settingsDialog));
  const win = [];
  if (CHROME !== 'mac') {
    tbMax = h('button', { class: 'tb-w', type: 'button', title: t('最大化'), 'aria-label': t('最大化'), html: svg('wmax'), onclick: () => winCmd('max') });
    win.push(h('div', { class: 'tb-win' },
      h('button', { class: 'tb-w', type: 'button', title: t('最小化'), 'aria-label': t('最小化'), html: svg('wmin'), onclick: () => winCmd('min') }),
      tbMax,
      h('button', { class: 'tb-w close', type: 'button', title: t('关闭'), 'aria-label': t('关闭'), html: svg('wclose'), onclick: () => winCmd('close') })));
  }
  bar.replaceChildren(
    h('div', { class: 'tb-brand' }, h('span', { class: 'logo', html: svg('phone') }), 'DroidBridge'),
    tbVer, tbUpd,
    h('div', { class: 'tb-fill' }),
    tools, ...win);
  setTitlebarVersion();

  // 空白处按下拖动窗口，双击最大化 / 还原
  bar.addEventListener('mousedown', e => {
    if (e.button !== 0 || e.target.closest('button,a,input,select')) return;
    e.preventDefault();
    winCmd(e.detail === 2 ? 'max' : 'drag');
  });

  // 无边框窗口的缩放边缘：Windows 左右下由系统处理，只补上边；Linux 四周都需要
  const edges = CHROME === 'win' ? ['n', 'nw', 'ne'] : CHROME === 'linux' ? ['n', 's', 'e', 'w', 'nw', 'ne', 'sw', 'se'] : [];
  for (const d of edges) {
    document.body.append(h('div', { class: 'tb-edge ' + d, onmousedown: e => { if (e.button === 0) { e.preventDefault(); winCmd('resize:' + d); } } }));
  }
}

/** Rust 端在窗口最大化 / 失去焦点时调用 */
window.__dbWinState = ({ max, focus }) => {
  document.body.classList.toggle('win-max', !!max);
  document.body.classList.toggle('win-blur', !focus);
  if (tbMax) {
    tbMax.innerHTML = svg(max ? 'wrestore' : 'wmax');
    tbMax.title = max ? t('还原') : t('最大化');
    tbMax.setAttribute('aria-label', tbMax.title);
  }
};

function setTitlebarVersion() {
  if (!tbVer) return;
  tbVer.disabled = false;
  tbVer.textContent = INFO ? 'v' + INFO.version : '';
  tbVer.hidden = !INFO;
}

/** 点版本号：检查更新 */
async function titlebarCheck() {
  if (UPDATE && UPDATE.newer) return updateDialog();
  tbVer.disabled = true;
  tbVer.replaceChildren(h('span', { class: 'spin sm' }), t('正在检查…'));
  const r = await checkUpdate(false);
  if (r && !r.newer) {
    tbVer.textContent = t('已是最新');
    setTimeout(setTitlebarVersion, 2500);
  } else {
    setTitlebarVersion();
    if (r && r.newer) updateDialog();
  }
}

/** 标题栏上的新版本提示 */
function showTitlebarUpdate() {
  if (!tbUpd || !UPDATE || !UPDATE.newer) return false;
  tbUpd.replaceChildren(h('span', { class: 'dot' }), t('新版本 {0}', UPDATE.latest));
  tbUpd.title = t('有新版本 {0}，点击查看', UPDATE.latest);
  tbUpd.hidden = false;
  return true;
}
