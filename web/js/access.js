// 授权：其他电脑 / 浏览器请求连接、批准请求、管理已授权的电脑
// 经典脚本，按 index.html 中的顺序加载，共享全局作用域。
'use strict';

const sleep = (ms) => new Promise(r => setTimeout(r, ms));
/** 6 位验证码显示为 123 456 */
const fmtCode = (c) => String(c || '').replace(/^(\d{3})(\d{3})$/, '$1 $2');

/**
 * 等待对方批准的对话框。poll() 返回 {state, ...}：pending 继续等；其他状态结束。
 * 返回最终状态对象；用户取消时返回 null（并调用 cancel）。
 */
function pairWaitDialog({ title, target, code, poll, cancel }) {
  return new Promise(resolve => {
    let done = false;
    const msg = h('div', { class: 'pair-msg' }, h('span', { class: 'spin sm' }),
      h('span', {}, t('请在「{0}」上点击“允许”。没有界面的电脑，可在任何一台已获它授权的电脑上允许。', target)));
    const err = h('div', { class: 'dlg-err' });
    const dlg = dialog({
      title,
      body: [h('div', { class: 'pair-code-l' }, t('验证码')), h('div', { class: 'pair-code' }, fmtCode(code)),
        h('div', { class: 'hint' }, t('请确认对方显示的验证码与这里一致')), msg, err],
      actions: [h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, t('取消'))],
    });
    dlg.addEventListener('close', () => { if (!done) { done = true; if (cancel) cancel(); resolve(null); } });
    (async () => {
      while (!done) {
        await sleep(1500);
        if (done) return;
        let r;
        try { r = await poll(); } catch { continue; }
        if (!r || r.state === 'pending') continue;
        done = true;
        if (r.state === 'approved' || r.state === 'done') { dlg.close(); resolve(r); return; }
        msg.remove();
        err.textContent = r.state === 'denied' ? t('对方拒绝了连接请求')
          : r.state === 'failed' ? (r.error || t('连接失败')) : t('请求已过期，请重新发起');
        resolve(r);
        return;
      }
    })();
  });
}

/** 浏览器直接访问另一台电脑（如手机打开 https://IP:8731）：请求授权，返回令牌；取消返回 null */
async function browserPairDialog() {
  const ua = navigator.userAgent;
  const guess = /iPhone|iPad/.test(ua) ? 'iPhone / iPad' : /Android/.test(ua) ? t('安卓手机') : /Mac/.test(ua) ? 'Mac' : /Windows/.test(ua) ? 'Windows' : t('浏览器');
  const name = h('input', { class: 'input', value: pref.get('client_name', t('{0} 上的浏览器', guess)), autocomplete: 'off', required: true });
  const err = h('div', { class: 'dlg-err' });
  let mode = null; // 'pair' | 'token' | null
  const tokenLink = h('button', { class: 'linkbtn', type: 'button', onclick: () => { mode = 'token'; dlg.close(); } }, t('已有访问令牌？'));
  const dlg = dialog({
    title: t('需要授权'),
    desc: t('这台电脑上的设备只有获得授权后才能查看和操作。发送请求后，请在这台电脑上点击“允许”。'),
    body: [field(t('本设备名称'), name, t('对方会在请求里看到这个名称')), h('div', {}, tokenLink), err],
    actions: [h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, t('取消')), h('button', { class: 'btn primary', type: 'submit' }, t('请求连接'))],
    onsubmit: () => { mode = 'pair'; dlg.close(); },
  });
  await new Promise(r => dlg.addEventListener('close', r));
  if (mode === 'token') {
    const tk = await promptDialog(t('需要访问令牌'), t('填写这台电脑上生成的访问令牌。'), { type: 'password', value: pref.get('token', '') });
    return tk === null ? null : tk.trim();
  }
  if (mode !== 'pair') return null;
  pref.set('client_name', name.value.trim());
  let req;
  try {
    const r = await fetch('/api/pair/request', { method: 'POST', headers: { 'content-type': 'application/json', 'x-lang': LANG }, body: JSON.stringify({ name: name.value.trim() }) });
    req = await r.json();
    if (!r.ok) throw new Error(req.error || ('HTTP ' + r.status));
  } catch (e) { toast(e.message, { kind: 'bad', ms: 5000 }); return null; }
  const res = await pairWaitDialog({
    title: t('等待授权'), target: req.name || location.hostname, code: req.code,
    poll: () => fetch('/api/pair/request/' + encodeURIComponent(req.id)).then(r => r.json()),
    cancel: () => fetch('/api/pair/request/' + encodeURIComponent(req.id), { method: 'DELETE' }).catch(() => {}),
  });
  if (res && res.state === 'approved' && res.token) { toast(t('已获得授权'), { kind: 'ok' }); return res.token; }
  return null;
}

/** 向远程电脑请求授权，批准后添加（或更新）到列表。p: {addr, name, id?} */
async function requestPeerAuth(p) {
  let start;
  try {
    start = await api('/api/peers/pair', { method: 'POST', body: JSON.stringify({ addr: p.addr, name: p.name || '', id: p.id || null }) });
  } catch (e) { toast(e.message, { kind: 'bad', ms: 6000 }); return false; }
  const target = p.name || start.name || p.addr;
  const res = await pairWaitDialog({
    title: t('请求「{0}」授权', target), target, code: start.code,
    poll: () => api('/api/peers/pair/' + start.job),
    cancel: () => api('/api/peers/pair/' + start.job, { method: 'DELETE' }).catch(() => {}),
  });
  if (res && res.state === 'done') {
    toast(t('「{0}」已授权本机', (res.peer && res.peer.name) || target), { kind: 'ok' });
    refreshAll();
    return true;
  }
  return false;
}

// ---------------- 批准请求 ----------------
// 本机和各远程电脑收到的请求都在这里弹出；一次只显示一个
const pairQueue = [];
let pairShown = null;
let pairPolling = false;

async function pollPairRequests() {
  if (pairPolling) return;
  pairPolling = true;
  try {
    const live = new Set();
    const targets = [...hosts.values()].filter(x => !x.error && !x.noPair);
    await Promise.all(targets.map(async hst => {
      let r;
      try { r = await api('/api/pair/pending?peer=' + encodeURIComponent(hst.id)); }
      catch (e) { if (hst.remote && /404|405|non-JSON|非 JSON/.test(e.message)) hst.noPair = true; return; }
      for (const q of r.list || []) {
        const key = hst.id + '|' + q.id;
        live.add(key);
        if ((!pairShown || pairShown.key !== key) && !pairQueue.some(x => x.key === key)) pairQueue.push({ key, hst, q });
      }
    }));
    // 已在别处处理或过期的请求
    for (let i = pairQueue.length - 1; i >= 0; i--) if (!live.has(pairQueue[i].key)) pairQueue.splice(i, 1);
    if (pairShown && !live.has(pairShown.key)) pairShown.dlg.close();
    showNextPair();
  } finally { pairPolling = false; }
}

function showNextPair() {
  if (pairShown || !pairQueue.length) return;
  const item = pairQueue.shift();
  const { hst, q } = item;
  const local = !hst.remote;
  const target = local ? t('本机') : t('「{0}」', hst.name);
  const decide = async (allow) => {
    try {
      await api('/api/pair/pending/' + encodeURIComponent(q.id) + '?peer=' + encodeURIComponent(hst.id), { method: 'POST', body: JSON.stringify({ allow }) });
      if (allow) toast(t('已允许「{0}」连接{1}', q.name, target), { kind: 'ok' });
    } catch (e) { toast(e.message, { kind: 'bad', ms: 5000 }); }
    dlg.close();
  };
  const dlg = dialog({
    title: t('连接请求'),
    desc: t('「{0}」（{1}）请求连接{2}。允许后，它可以查看和操作{2}上的所有设备。', q.name, q.ip, target),
    body: [h('div', { class: 'pair-code-l' }, t('验证码')), h('div', { class: 'pair-code' }, fmtCode(q.code)),
      h('div', { class: 'hint' }, t('请确认与对方屏幕上显示的验证码一致。不认识这个请求就点“拒绝”。'))],
    actions: [h('button', { class: 'btn', type: 'button', onclick: () => decide(false) }, t('拒绝')),
      h('button', { class: 'btn primary', type: 'button', onclick: () => decide(true) }, t('允许'))],
  });
  // Esc 关闭只是暂不处理，请求仍在，下次刷新会再弹出
  pairShown = { key: item.key, dlg };
  dlg.addEventListener('close', () => { pairShown = null; setTimeout(showNextPair, 300); });
}

// ---------------- 已授权的电脑 ----------------
function fmtWhen(sec) {
  if (!sec) return '—';
  const d = new Date(sec * 1000);
  return d.toLocaleDateString(LANG === 'zh' ? 'zh-CN' : 'en') + ' ' + d.toLocaleTimeString(LANG === 'zh' ? 'zh-CN' : 'en', { hour: '2-digit', minute: '2-digit' });
}

/** 管理某台电脑（本机或远程电脑）授权过的电脑和浏览器 */
function clientsDialog(hst) {
  const local = !hst || !hst.remote;
  const peerQ = '?peer=' + encodeURIComponent(local ? 'local' : hst.id);
  const list = h('div', { class: 'cl-list' }, h('div', { class: 'empty' }, t('正在读取…')));
  async function load() {
    try {
      const r = await api('/api/clients' + peerQ);
      const items = r.list || [];
      list.replaceChildren(...(items.length ? items.map(c => h('div', { class: 'cl' },
        h('div', { class: 't' }, h('b', {}, c.name), h('small', {}, [c.addr, t('最近使用 {0}', fmtWhen(c.last_seen))].filter(Boolean).join(' · '))),
        h('button', { class: 'btn sm danger', type: 'button', onclick: () => revoke(c) }, t('撤销'))))
        : [h('div', { class: 'empty' }, t('还没有授权任何电脑或浏览器'))]));
    } catch (e) { list.replaceChildren(h('div', { class: 'empty' }, e.message)); }
  }
  async function revoke(c) {
    if (!await confirmDialog(t('撤销授权'), t('「{0}」将不能再访问{1}，需要重新请求授权。', c.name, local ? t('本机') : t('「{0}」', hst.name)), t('撤销'), true)) return;
    try { await api('/api/clients/' + encodeURIComponent(c.id) + peerQ, { method: 'DELETE' }); toast(t('已撤销'), { kind: 'ok' }); } catch (e) { toast(e.message, { kind: 'bad' }); }
    load();
  }
  const gen = local && isLocalHost() ? h('button', { class: 'btn', type: 'button', onclick: issueTokenDialog }, t('生成访问令牌…')) : null;
  const dlg = dialog({
    title: local ? t('已授权的电脑和浏览器') : t('「{0}」授权的电脑和浏览器', hst.name),
    desc: t('这些电脑和浏览器可以查看和操作{0}上的设备。撤销后需要重新请求授权。', local ? t('本机') : t('「{0}」', hst.name)),
    body: [list], cls: 'clients',
    actions: [gen, h('span', { class: 'grow' }), h('button', { class: 'btn primary', type: 'button', onclick: () => dlg.close() }, t('完成'))],
  });
  load();
}

/** 生成访问令牌：给旧版本 DroidBridge、脚本或其他电脑上的 AI 客户端使用 */
async function issueTokenDialog() {
  const name = await promptDialog(t('生成访问令牌'), t('给它起个名字，方便以后识别和撤销，例如“办公室 Mac”或“Claude Code”。'), { okText: t('生成') });
  if (name === null) return;
  let tk;
  try { tk = (await api('/api/clients', { method: 'POST', body: JSON.stringify({ name: name.trim() }) })).token; }
  catch (e) { toast(e.message, { kind: 'bad' }); return; }
  const box = h('input', { class: 'input mono', value: tk, readonly: true, onfocus: e => e.target.select() });
  const dlg = dialog({
    title: t('访问令牌'),
    desc: t('令牌只显示这一次，请妥善保存。可以填在旧版 DroidBridge 的“访问令牌”里，或作为 Authorization: Bearer 用于脚本和 AI 客户端。'),
    body: [box],
    actions: [h('button', { class: 'btn', type: 'button', onclick: () => copyText(tk) }, t('复制')), h('button', { class: 'btn primary', type: 'button', onclick: () => dlg.close() }, t('完成'))],
  });
}

/** 设置里的“安全”分组 */
function securityGroup(group, row) {
  const local = isLocalHost();
  const sw = h('input', { type: 'checkbox', disabled: !local });
  const swLabel = h('label', { class: 'switch' }, sw, h('i'));
  const sub = h('small', {});
  api('/api/access').then(a => {
    sw.checked = a.discoverable && a.discovery;
    if (!a.discovery) { sw.disabled = true; sub.textContent = t('已用 --no-discovery 关闭'); }
    else if (!local) sub.textContent = t('只能在这台电脑上修改');
    if (a.open) toast(t('这台电脑以 --open 启动，局域网内任何人都能控制它的设备'), { kind: 'bad', ms: 6000 });
  }).catch(() => {});
  sw.onchange = async () => {
    try { await api('/api/access', { method: 'PUT', body: JSON.stringify({ discoverable: sw.checked }) }); }
    catch (e) { sw.checked = !sw.checked; toast(e.message, { kind: 'bad' }); }
  };
  return group(t('安全'),
    row([t('允许局域网内的电脑发现本机'), sub], swLabel),
    row(t('已授权的电脑和浏览器'), h('button', { class: 'btn sm', type: 'button', onclick: () => clientsDialog(null) }, t('管理…'))));
}
