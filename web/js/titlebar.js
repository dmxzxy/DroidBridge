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
    Object.assign(ib('gear', t('设置'), settingsDialog), { id: 'tbGear' }));
  const win = [];
  if (CHROME !== 'mac') {
    tbMax = h('button', { class: 'tb-w', type: 'button', title: t('最大化'), 'aria-label': t('最大化'), html: svg('wmax'), onclick: () => winCmd('max') });
    win.push(h('div', { class: 'tb-win' },
      h('button', { class: 'tb-w', type: 'button', title: t('最小化'), 'aria-label': t('最小化'), html: svg('wmin'), onclick: () => winCmd('min') }),
      tbMax,
      h('button', { class: 'tb-w close', type: 'button', title: t('关闭'), 'aria-label': t('关闭'), html: svg('wclose'), onclick: () => winCmd('close') })));
  }
  bar.replaceChildren(
    ib('sidebar', t('收起侧栏 (Ctrl+B)'), () => toggleSidebar(), 'side-tog'),
    h('div', { class: 'tb-brand' }, h('span', { class: 'logo app', html: LOGO_SVG }), 'DroidBridge'),
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

/** 关闭窗口时的选择（桌面版启动时注入）：tray 最小化到托盘 / quit 退出 / ask 询问 */
let CLOSE_PREF = window.__DB_CLOSE || 'ask';
/** Rust 端在用户第一次关闭窗口（还没选过）时调用 */
window.__dbAskClose = () => {
  if (document.querySelector('dialog.close-ask')) return;
  const remember = h('input', { type: 'checkbox' }); remember.checked = true;
  let choice = null;
  const dlg = dialog({
    title: t('关闭窗口'),
    desc: t('缩到托盘后仍在后台运行，其他电脑和 AI 不会断开。'),
    body: [h('label', { class: 'check' }, remember, h('span', {}, t('记住选择')))],
    cls: 'close-ask',
    actions: [h('button', { class: 'btn', type: 'button', onclick: () => { choice = 'quit'; dlg.close(); } }, t('退出')),
      h('button', { class: 'btn primary', type: 'submit' }, t('最小化到托盘'))],
    onsubmit: () => { choice = 'tray'; dlg.close(); },
  });
  dlg.addEventListener('close', () => {
    if (!choice) return;
    if (remember.checked) CLOSE_PREF = choice;
    winCmd('close:' + choice + (remember.checked ? ':save' : ''));
  });
};

/** 设置里的“关闭窗口时”（只在桌面版显示） */
function closePrefRow(row) {
  if (!CHROME || !window.__DB_TRAY) return null;
  const s = h('select', { class: 'select', 'aria-label': t('关闭窗口时'), onchange: () => { CLOSE_PREF = s.value; winCmd('closepref:' + s.value); } },
    h('option', { value: 'tray' }, t('最小化到托盘')), h('option', { value: 'quit' }, t('退出程序')), h('option', { value: 'ask' }, t('每次询问')));
  s.value = CLOSE_PREF;
  return row(t('关闭窗口时'), s);
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
