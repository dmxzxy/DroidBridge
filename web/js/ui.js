// 菜单与对话框
// 由 web/index.html 拆分而来；经典脚本，按 index.html 中的顺序加载，共享全局作用域。
'use strict';

// ---------------- 菜单 ----------------
let openMenu = null;
let renderPending = false;
function closeMenu() {
  if (!openMenu) return;
  openMenu.anchor.removeAttribute('aria-expanded');
  openMenu.el.remove();
  openMenu = null;
  if (renderPending) setTimeout(renderSide);
}
function menu(anchor, items) {
  const wasOpen = openMenu && openMenu.anchor === anchor;
  closeMenu();
  if (wasOpen) return;
  const el = h('div', { class: 'menu', role: 'menu' });
  for (const it of items) {
    if (it === '-') { el.append(h('div', { class: 'sep' })); continue; }
    el.append(h('button', { class: it.danger ? 'danger' : '', type: 'button', role: 'menuitem', disabled: !!it.disabled,
      // mousedown 时不让输入框失去焦点和选区（右键菜单里的剪切/复制/粘贴需要它们）
      onmousedown: e => e.preventDefault(),
      onclick: () => { closeMenu(); it.fn(); } },
      h('span', { html: svg(it.icon) }).firstChild, it.label));
  }
  (anchor.closest('dialog[open]') || document.body).append(el);
  const r = anchor.getBoundingClientRect(), mw = el.offsetWidth, mh = el.offsetHeight;
  const point = r.width <= 1 && r.height <= 1;   // 右键菜单：锚点就是鼠标位置
  let left = point ? Math.min(r.left, innerWidth - mw - 8) : Math.min(r.right - mw, innerWidth - mw - 8);
  if (left < 8) left = Math.min(r.left, innerWidth - mw - 8);
  let top = point ? r.top : r.bottom + 4;
  if (top + mh > innerHeight - 8) top = Math.max(8, (point ? r.top : r.top - 4) - mh);
  el.style.left = left + 'px'; el.style.top = top + 'px';
  anchor.setAttribute('aria-expanded', 'true');
  openMenu = { el, anchor, w: innerWidth, h: innerHeight };
}
document.addEventListener('pointerdown', e => { if (openMenu && !openMenu.el.contains(e.target) && !openMenu.anchor.contains(e.target)) closeMenu(); }, true);
document.addEventListener('keydown', e => { if (e.key === 'Escape') closeMenu(); });
addEventListener('resize', () => { if (openMenu && (openMenu.w !== innerWidth || openMenu.h !== innerHeight)) closeMenu(); });
// 只有菜单的锚点所在区域滚动时才关闭（日志面板自动滚动等不影响）
addEventListener('scroll', e => { if (openMenu && (e.target === document || (e.target.contains && e.target.contains(openMenu.anchor)))) closeMenu(); }, true);

// ---------------- 对话框 ----------------
function dialog({ title, desc, body = [], actions = [], onsubmit, cls }) {
  const foot = h('div', { class: 'dlg-f' }, actions);
  const form = h('form', { method: 'dialog', onsubmit: onsubmit ? (e) => { e.preventDefault(); onsubmit(); } : null },
    h('div', { class: 'dlg-h' }, title), desc ? h('div', { class: 'dlg-d' }, desc) : null,
    h('div', { class: 'dlg-b' }, body), foot);
  const dlg = h('dialog', { class: cls }, form);
  dlg.addEventListener('close', () => dlg.remove());
  document.body.append(dlg);
  dlg.showModal();
  return dlg;
}
const field = (label, input, hint) => h('div', { class: 'field' }, h('label', {}, label), input, hint ? h('div', { class: 'hint' }, hint) : null);

function confirmDialog(title, desc, okText, danger) {
  return new Promise(res => {
    let ok = false;
    const dlg = dialog({
      title, desc,
      actions: [h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, t('取消')),
        h('button', { class: 'btn ' + (danger ? 'danger' : 'primary'), type: 'submit' }, okText)],
      onsubmit: () => { ok = true; dlg.close(); },
    });
    dlg.addEventListener('close', () => res(ok));
  });
}

function promptDialog(title, desc, { placeholder = '', value = '', type = 'text', okText = t('确定') } = {}) {
  return new Promise(res => {
    let result = null;
    const input = h('input', { class: 'input', type, placeholder, value, autofocus: true });
    const dlg = dialog({
      title, desc, body: [input],
      actions: [h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, t('取消')), h('button', { class: 'btn primary', type: 'submit' }, okText)],
      onsubmit: () => { result = input.value; dlg.close(); },
    });
    dlg.addEventListener('close', () => res(result));
  });
}

function copyText(text) {
  const done = () => toast(t('已复制 ') + text);
  if (navigator.clipboard && isSecureContext) navigator.clipboard.writeText(text).then(done, () => toast(text));
  else { const i = h('textarea', {}, text); document.body.append(i); i.select(); try { document.execCommand('copy'); done(); } catch { toast(text); } i.remove(); }
}
