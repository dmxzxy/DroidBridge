// 右键菜单：屏蔽浏览器 / WebView 自带的菜单（后退、刷新、另存为、检查…），
// 输入框里换成应用自己的“剪切 / 复制 / 粘贴 / 全选”，选中文字时提供“复制”。
// 设备画面上的右键仍然是“返回”（在 viewer.js 中处理）。
'use strict';

/** 当前选中的文字（输入框优先） */
function selectedText(el) {
  if (el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') && typeof el.selectionStart === 'number') {
    return el.value.slice(el.selectionStart, el.selectionEnd);
  }
  return String(window.getSelection() || '');
}

/** 在输入框的光标处插入文字（替换选中部分），并触发 input 事件 */
function insertAtCursor(el, text) {
  el.focus();
  const start = el.selectionStart ?? el.value.length, end = el.selectionEnd ?? el.value.length;
  if (typeof el.setRangeText === 'function') el.setRangeText(text, start, end, 'end');
  else el.value = el.value.slice(0, start) + text + el.value.slice(end);
  el.dispatchEvent(new Event('input', { bubbles: true }));
}

async function readClipboard() {
  try { if (navigator.clipboard && isSecureContext) return await navigator.clipboard.readText(); } catch {}
  return null;
}

/** 把菜单放在鼠标位置 */
function menuAt(x, y, items) {
  const anchor = h('span', { style: `position:fixed;left:${x}px;top:${y}px;width:1px;height:1px;pointer-events:none` });
  (document.querySelector('dialog[open]') || document.body).append(anchor);
  menu(anchor, items);
  // 菜单关闭后移除临时锚点
  const tick = setInterval(() => { if (!openMenu || openMenu.anchor !== anchor) { clearInterval(tick); anchor.remove(); } }, 300);
}

// 右键按下时 Chromium 会先移动光标、清掉选区，所以在 mousedown 时记下选区，菜单里用它
let ctxSel = null;
document.addEventListener('mousedown', (e) => {
  if (e.button !== 2) return;
  const el = e.target.closest ? e.target.closest('input, textarea') : null;
  ctxSel = el && typeof el.selectionStart === 'number'
    ? { el, start: el.selectionStart, end: el.selectionEnd, focused: document.activeElement === el }
    : { el: null, text: String(window.getSelection() || '') };
}, true);

document.addEventListener('contextmenu', (e) => {
  // 设备画面自己处理（右键 = 返回）
  if (e.target.closest && e.target.closest('.screen canvas')) return;
  e.preventDefault();
  const el = e.target.closest ? e.target.closest('input, textarea') : null;
  const editable = el && !el.readOnly && !el.disabled && !['checkbox', 'radio', 'range', 'file', 'button', 'submit', 'color'].includes(el.type)
    && !el.classList.contains('ime');
  // 用 mousedown 时记下的选区；右键点在已获得焦点的输入框里时，把选区恢复回去
  let sel = '';
  if (editable && ctxSel && ctxSel.el === el && ctxSel.focused && ctxSel.end > ctxSel.start) {
    el.setSelectionRange(ctxSel.start, ctxSel.end);
    sel = el.value.slice(ctxSel.start, ctxSel.end);
  } else if (!editable && ctxSel && ctxSel.text) {
    sel = ctxSel.text;
  } else {
    sel = selectedText(editable ? el : null);
  }
  const items = [];
  if (editable) {
    const secret = el.type === 'password';
    const hasSel = !!sel;
    items.push(
      { icon: 'scissors', label: t('剪切'), disabled: !hasSel || secret, fn: async () => {
        try { await navigator.clipboard.writeText(sel); } catch { document.execCommand('copy'); }
        insertAtCursor(el, '');
      } },
      { icon: 'copy', label: t('复制'), disabled: !hasSel || secret, fn: () => copyText(sel) },
      { icon: 'paste', label: t('粘贴'), fn: async () => {
        const text = await readClipboard();
        if (text === null) { el.focus(); if (!document.execCommand('paste')) toast(t('无法读取剪贴板，请用 Ctrl+V 粘贴')); return; }
        insertAtCursor(el, text);
      } },
      '-',
      { icon: 'check', label: t('全部选择'), disabled: !el.value, fn: () => { el.focus(); el.select(); } },
    );
  } else if (sel.trim()) {
    items.push({ icon: 'copy', label: t('复制'), fn: () => copyText(sel) });
  }
  if (items.length) menuAt(e.clientX, e.clientY, items);
}, true);
