// 远程电脑：菜单、添加/编辑、无线连接、设置、帮助
// 由 web/index.html 拆分而来；经典脚本，按 index.html 中的顺序加载，共享全局作用域。
'use strict';

async function addFound(f) {
  if (f.auth) return peerDialog({ addr: f.addr, name: f.name, prefill: true, auth: true });
  try {
    const res = await api('/api/peers', { method: 'POST', body: JSON.stringify({ addr: f.addr, name: '' }) });
    toast(t('已添加 ') + res.name, { kind: 'ok' });
    refreshAll();
  } catch (e) { toast(e.message, { kind: 'bad', ms: 4000 }); }
}

function hostMenu(anchor, r) {
  menu(anchor, [
    ...(needsUpdate(r) ? [{ icon: 'download', label: t('更新到 {0}', INFO.version), fn: () => updatePeer(r) }, '-'] : []),
    { icon: 'wifi', label: t('无线连接设备…'), fn: () => wirelessDialog(r) },
    { icon: 'refresh', label: t('刷新'), fn: async () => { await refreshHost(r); renderSide(); if (r.error) toast(r.error, { kind: 'bad', ms: 4000 }); } },
    { icon: 'edit', label: t('编辑'), fn: () => peerDialog(r) },
    { icon: 'download', label: t('通过 SSH 重新安装…'), fn: () => peerDialog(r, { install: true }) },
    '-',
    { icon: 'trash', label: t('移除'), danger: true, fn: () => removePeer(r) },
  ]);
}

// 添加 / 编辑远程电脑。opts.install=true：直接进入“通过 SSH 安装”
function peerDialog(p, opts = {}) {
  const editing = !!p && !opts.install && !p.prefill;
  const saved = p && p.ssh ? { auth: p.ssh.auth, user: p.ssh.user || '', key: p.ssh.key_file || '' } : {};
  const sshPref = (k, d) => saved[k] !== undefined && saved[k] !== '' ? saved[k] : pref.get('ssh_' + k, d);
  let mode = opts.install ? 'ssh' : 'run';
  let authMode = sshPref('auth', 'agent');

  // ---- 输入框 ----
  const addr = h('input', { class: 'input', value: p ? p.addr : '', autofocus: true, required: true, autocomplete: 'off', spellcheck: 'false' });
  const name = h('input', { class: 'input', value: p && p.name ? p.name : '', placeholder: t('默认使用对方的计算机名'), autocomplete: 'off' });
  const token = h('input', { class: 'input', type: 'password', autocomplete: 'new-password' });
  const user = h('input', { class: 'input', value: sshPref('user', ''), autocomplete: 'off', spellcheck: 'false' });
  const password = h('input', { class: 'input', type: 'password', autocomplete: 'new-password' });
  const remember = h('input', { type: 'checkbox' }); remember.checked = sshPref('remember', '1') === '1';
  const keyList = h('datalist', { id: 'sshkeys' });
  const keyFile = h('input', { class: 'input', list: 'sshkeys', value: sshPref('key', ''), placeholder: '~/.ssh/id_ed25519', autocomplete: 'off', spellcheck: 'false' });
  const passphrase = h('input', { class: 'input', type: 'password', placeholder: t('密钥没有口令可不填'), autocomplete: 'new-password' });
  const sshPort = h('input', { class: 'input', placeholder: '22', inputmode: 'numeric', autocomplete: 'off', value: p && p.ssh && p.ssh.port ? String(p.ssh.port) : '' });
  api('/api/ssh/keys').then(r => {
    keyList.replaceChildren(...(r.keys || []).map(k => h('option', { value: k })));
    if (!keyFile.value && r.keys && r.keys.length) keyFile.value = r.keys[0];
  }).catch(() => {});

  const err = h('div', { class: 'dlg-err' }, editing && p.error ? p.error : '');
  const ok = h('button', { class: 'btn primary', type: 'submit' });
  const cancel = h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, t('取消'));
  const intro = h('div', { class: 'intro' });

  // ---- “已运行”模式：地址、名称；访问令牌默认收起 ----
  const addrHint = h('div', { class: 'hint' });
  const addrField = h('div', { class: 'field' }, h('label', {}, t('地址')), addr, addrHint);
  const nameSlot = h('div'), tokenSlot = h('div');
  const nameField = field(t('名称'), nameSlot);
  const tokenField = field(t('访问令牌'), tokenSlot, t('对方启动时用 --token 设置的令牌'));
  const tokenLink = h('button', { class: 'linkbtn', type: 'button', onclick: () => { tokenLink.hidden = true; tokenField.hidden = false; token.focus(); } }, t('对方设置了访问令牌？'));
  const showToken = editing ? p.has_token : !!(p && p.auth);
  tokenField.hidden = !(editing || showToken);
  tokenLink.hidden = editing || showToken;
  const runBox = h('div', { class: 'grid-b' }, nameField, h('div', {}, tokenLink), tokenField);

  // ---- “SSH 安装”模式 ----
  const AUTHS = [['agent', t('本机密钥')], ['password', t('密码')], ['key', t('密钥文件')]];
  const authBtns = {};
  const authSeg = h('div', { class: 'seg sm' }, AUTHS.map(([k, l]) => (authBtns[k] = h('button', { type: 'button', onclick: () => setAuth(k) }, l))));
  const authHint = h('div', { class: 'hint' });
  const pwField = field(t('密码'), password, t('只用于这次安装，不会保存'));
  const rememberRow = h('label', { class: 'check' }, remember, h('span', {}, t('记住登录：把本机公钥加到对方，以后不用再输密码')));
  const keyField = field(t('密钥文件'), h('div', {}, keyFile, keyList), t('这台电脑上的私钥路径'));
  const ppField = field(t('密钥口令'), passphrase);
  const advName = h('div'), advToken = h('div');
  const adv = h('details', { class: 'adv' }, h('summary', {}, t('高级选项')),
    h('div', { class: 'grid-b' }, field(t('SSH 端口'), sshPort), field(t('显示名称'), advName),
      field(t('为对方设置访问令牌（可选）'), advToken, t('设置后，其他电脑和浏览器访问它都需要这个令牌'))));
  const sshBox = h('div', { class: 'grid-b' },
    h('div', { class: 'field' }, h('label', {}, t('登录方式')), authSeg, authHint),
    field(t('用户名'), user), pwField, rememberRow, keyField, ppField, adv);

  const tabs = {};
  const seg = h('div', { class: 'seg' }, [['run', t('已运行 DroidBridge')], ['ssh', t('通过 SSH 安装')]]
    .map(([k, l]) => (tabs[k] = h('button', { type: 'button', onclick: () => setMode(k) }, l))));

  const dlg = dialog({
    title: editing ? t('编辑远程电脑') : opts.install ? t('通过 SSH 重新安装') : t('添加远程电脑'),
    body: editing ? [addrField, runBox, err] : [seg, intro, addrField, runBox, sshBox, err],
    actions: [cancel, ok], onsubmit: submit,
  });
  dlg.classList.add('wide');

  function setMode(m) {
    mode = m; err.textContent = '';
    for (const k in tabs) tabs[k].classList.toggle('on', k === m);
    const ssh = m === 'ssh';
    runBox.hidden = ssh; sshBox.hidden = !ssh;
    intro.textContent = ssh
      ? t('用 SSH 登录一台 macOS 或 Linux 电脑，自动安装 DroidBridge 并设为开机启动，然后添加到列表。Windows 电脑请在对方直接运行安装包。')
      : t('对方电脑已经运行着 DroidBridge（桌面版或服务版），并与本机在同一局域网。');
    addr.placeholder = ssh ? t('IP 或主机名，如 192.168.1.20') : '192.168.1.20';
    addrHint.textContent = ssh ? '' : t('端口默认 8730，可写成 IP:端口');
    // 名称、令牌输入框在两种模式间共用
    (ssh ? advName : nameSlot).append(name);
    (ssh ? advToken : tokenSlot).append(token);
    token.placeholder = ssh ? t('留空表示不设置') : '';
    ok.textContent = ssh ? t('安装并添加') : t('添加');
    if (ssh) setAuth(authMode);
  }
  function setAuth(a) {
    authMode = a;
    for (const k in authBtns) authBtns[k].classList.toggle('on', k === a);
    pwField.hidden = a !== 'password'; rememberRow.hidden = a !== 'password';
    keyField.hidden = a !== 'key'; ppField.hidden = a !== 'key';
    user.placeholder = a === 'agent' ? t('留空则使用 ~/.ssh/config 或当前用户名') : t('对方电脑的登录用户名');
    authHint.textContent = {
      agent: t('使用这台电脑 ~/.ssh 里的密钥、ssh-agent 和 ~/.ssh/config，和在命令行执行 ssh 一样。'),
      password: t('用对方电脑的账号密码登录。macOS 需在“系统设置 → 通用 → 共享”中打开“远程登录”。'),
      key: t('使用指定的私钥文件登录。'),
    }[a];
  }

  async function submit() {
    err.textContent = '';
    if (mode === 'ssh' && !editing) return runInstall();
    ok.disabled = true; const label = ok.textContent; ok.textContent = t('正在连接…');
    try {
      const res = await api(editing ? '/api/peers/' + encodeURIComponent(p.id) : '/api/peers', {
        method: editing ? 'PUT' : 'POST', body: JSON.stringify({ addr: addr.value, name: name.value, token: token.value }),
      });
      dlg.close();
      toast((editing ? t('已保存 ') : t('已添加 ')) + res.name, { kind: 'ok' });
      refreshAll();
    } catch (e) {
      ok.disabled = false; ok.textContent = label;
      err.replaceChildren(e.message);
      if (/令牌|token/i.test(e.message)) { tokenLink.hidden = true; tokenField.hidden = false; token.focus(); }
      else if (!editing && /无法连接|无法访问|拒绝|refused|超时|timed out|cannot (connect|reach)/i.test(e.message)) {
        err.replaceChildren(t('连不上这台电脑上的 DroidBridge，它可能还没安装或没有运行。'), ' ',
          h('button', { class: 'linkbtn', type: 'button', onclick: () => setMode('ssh') }, t('改为通过 SSH 安装')));
      }
    }
  }

  if (editing) { ok.textContent = t('保存'); nameSlot.append(name); tokenSlot.append(token); token.placeholder = p.has_token ? t('已设置，留空保持不变') : ''; addrHint.textContent = t('端口默认 8730，可写成 IP:端口'); }
  else setMode(mode);

  // ---- SSH 安装：表单替换为步骤进度 ----
  async function runInstall() {
    const port = sshPort.value.trim();
    if (port && !/^\d{1,5}$/.test(port)) { err.textContent = t('SSH 端口不正确'); return; }
    if (authMode === 'password' && !password.value) { err.textContent = t('请填写 SSH 密码'); password.focus(); return; }
    pref.set('ssh_auth', authMode); pref.set('ssh_user', user.value.trim()); pref.set('ssh_remember', remember.checked ? '1' : '0');
    if (authMode === 'key') pref.set('ssh_key', keyFile.value.trim());
    ok.disabled = true;
    let job;
    try {
      job = (await api('/api/install', { method: 'POST', body: JSON.stringify({
        addr: addr.value, name: name.value, token: token.value, user: user.value.trim(), ssh_port: port ? +port : null,
        auth: authMode, password: authMode === 'password' ? password.value : authMode === 'key' ? passphrase.value : '',
        key_file: authMode === 'key' ? keyFile.value.trim() : '', remember: authMode === 'password' && remember.checked,
      }) })).job;
    } catch (e) { err.textContent = e.message; ok.disabled = false; return; }
    password.value = ''; passphrase.value = '';
    const target = addr.value.trim();
    const stepsEl = h('div', { class: 'steps2' });
    const lastEl = h('div', { class: 'logline' });
    const logEl = h('pre', { class: 'logbox', hidden: true });
    const logBtn = h('button', { class: 'linkbtn', type: 'button', onclick: () => { logEl.hidden = !logEl.hidden; logBtn.textContent = logEl.hidden ? t('查看日志') : t('收起日志'); logEl.scrollTop = logEl.scrollHeight; } }, t('查看日志'));
    const body = $('.dlg-b', dlg);
    body.replaceChildren(stepsEl, lastEl, h('div', {}, logBtn), logEl, err);
    $('.dlg-h', dlg).textContent = t('正在安装到 ') + target;
    const d = $('.dlg-d', dlg); if (d) d.textContent = t('可以关闭窗口，安装会在后台继续。');
    cancel.textContent = t('后台运行');
    ok.hidden = true;
    let since = 0, lines = [];
    const tt = toast(t('正在安装到 ') + target + '…', { sticky: true });
    while (true) {
      let s;
      try { s = await api(`/api/install/${job}?since=${since}`); } catch (e) { await new Promise(r => setTimeout(r, 1500)); continue; }
      since = s.next;
      if (s.log.length) { lines.push(...s.log); if (lines.length > 500) lines = lines.slice(-500); logEl.textContent = lines.join('\n'); logEl.scrollTop = logEl.scrollHeight; lastEl.textContent = lines[lines.length - 1]; }
      stepsEl.replaceChildren(...s.steps.map((name, i) => {
        const last = i === s.steps.length - 1;
        const icon = !last || s.state === 'done' ? h('span', { class: 'st-ok', html: svg('check') })
          : s.state === 'failed' ? h('span', { class: 'st-bad', html: svg('x') }) : h('span', { class: 'spin sm' });
        return h('div', { class: 'step' + (last && s.state === 'running' ? ' cur' : '') }, icon, h('span', {}, name));
      }));
      if (s.state === 'running') { tt.set(t('正在安装到 ') + target + '：' + (s.steps[s.steps.length - 1] || '…')); await new Promise(r => setTimeout(r, 800)); continue; }
      if (s.state === 'done') {
        tt.set(t('已添加 ') + (s.peer && s.peer.name || target), 'ok'); tt.close(3000);
        refreshAll();
        if (dlg.open) dlg.close();
      } else {
        tt.set(t('安装失败：') + s.error, 'bad'); tt.close(6000);
        lastEl.textContent = '';
        cancel.textContent = t('关闭');
        err.replaceChildren(s.error, ' ', h('button', { class: 'linkbtn', type: 'button', onclick: () => { dlg.close(); peerDialog({ addr: target, name: name.value, prefill: true }, { install: true }); } }, t('修改后重试')));
      }
      return;
    }
  }
}

async function removePeer(r) {
  if (!await confirmDialog(t('移除远程电脑'), t('将从列表中移除“{0}”，对方电脑不受影响。', r.name), t('移除'), true)) return;
  try { await api('/api/peers/' + encodeURIComponent(r.id), { method: 'DELETE' }); } catch (e) { toast(e.message, { kind: 'bad' }); }
  for (const v of [...viewers.values()]) if (v.host.id === r.id) v.close();
  refreshAll();
}

function helpDialog() {
  const dlg = dialog({
    title: t('连接安卓设备'),
    body: [h('ol', { class: 'steps' },
      h('li', {}, t('在手机上打开 '), h('b', {}, t('设置 → 开发者选项 → USB 调试')), t('。没有开发者选项时，在“关于手机”里连续点击版本号 7 次。')),
      h('li', {}, t('用数据线连接电脑，在手机弹窗中点击 '), h('b', {}, t('允许')), '。'),
      h('li', {}, t('小米、红米等机型还需开启 '), h('b', {}, t('USB 调试（安全设置）')), t('，否则无法点击和输入。')),
      h('li', {}, t('设备出现在左侧列表后，点击即可投屏。')))],
    actions: [h('button', { class: 'btn primary', type: 'button', onclick: () => dlg.close() }, t('知道了'))],
  });
}

// 无线连接：二维码配对 / 配对码 / IP 地址。设备需与该电脑在同一网络
function wirelessDialog(hst) {
  const peerQ = 'peer=' + encodeURIComponent(hst.id);
  let job = null, polling = false;
  const tabs = [['qr', t('扫码配对')], ['code', t('配对码')], ['ip', t('IP 地址')]];
  const panes = {}, tabBtns = {};
  // 二维码
  const qrBox = h('div', { class: 'qrbox' }, h('div', { class: 'spin sm' }));
  const qrState = h('div', { class: 'qrstate' });
  panes.qr = h('div', { class: 'pane' }, qrBox, qrState,
    h('div', { class: 'hint' }, t('手机打开 开发者选项 → 无线调试 → 使用二维码配对设备，扫描上方二维码。手机需要与「{0}」在同一网络（Android 11+）。', hst.name)));
  // 配对码
  const pAddr = h('input', { class: 'input', placeholder: '192.168.1.31:37099', autocomplete: 'off' });
  const pCode = h('input', { class: 'input', placeholder: '123456', inputmode: 'numeric', autocomplete: 'off' });
  const pConn = h('input', { class: 'input', placeholder: '192.168.1.31:41235', autocomplete: 'off' });
  panes.code = h('div', { class: 'pane' },
    h('div', { class: 'hint' }, t('手机打开 开发者选项 → 无线调试 → 使用配对码配对设备，填写其中的 IP 地址、端口和配对码。')),
    field(t('配对地址'), pAddr), field(t('配对码'), pCode), field(t('连接地址（可选）'), pConn, t('无线调试页面上方显示的 IP 地址和端口；留空会自动查找')));
  // IP
  const iAddr = h('input', { class: 'input', placeholder: '192.168.1.31:5555', autocomplete: 'off' });
  panes.ip = h('div', { class: 'pane' }, field(t('地址'), iAddr, t('已配对设备在无线调试页面显示的地址，或执行过 adb tcpip 的设备（默认端口 5555）')));
  const err = h('div', { class: 'dlg-err' });
  const ok = h('button', { class: 'btn primary', type: 'submit' }, t('连接'));
  const seg = h('div', { class: 'seg' }, tabs.map(([k, label]) => (tabBtns[k] = h('button', { type: 'button', onclick: () => show(k) }, label))));
  const dlg = dialog({
    title: t('无线连接设备'),
    desc: hst.remote ? t('设备将连接到「{0}」', hst.name) : null,
    body: [seg, panes.qr, panes.code, panes.ip, err],
    actions: [h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, t('关闭')), ok],
    onsubmit: submit,
  });
  dlg.classList.add('wide');
  dlg.addEventListener('close', () => { polling = false; if (job) api(`/api/adb/qr/${job}?${peerQ}`, { method: 'DELETE' }).catch(() => {}); });
  function show(k) {
    for (const [key] of tabs) { panes[key].hidden = key !== k; tabBtns[key].classList.toggle('on', key === k); }
    ok.hidden = k === 'qr'; ok.dataset.tab = k; err.textContent = '';
    if (k === 'qr' && !job) startQr();
    (k === 'code' ? pAddr : k === 'ip' ? iAddr : ok).focus();
  }
  async function startQr() {
    qrBox.replaceChildren(h('div', { class: 'spin sm' })); qrState.textContent = '';
    try {
      const r = await api('/api/adb/qr?' + peerQ, { method: 'POST', body: '{}' });
      job = r.job; qrBox.innerHTML = r.svg; polling = true; poll();
    } catch (e) { qrBox.replaceChildren(); qrState.textContent = e.message; qrState.className = 'qrstate err'; }
  }
  async function poll() {
    while (polling && job) {
      let s; try { s = await api(`/api/adb/qr/${job}?${peerQ}`); } catch { s = null; }
      if (s) {
        qrState.textContent = s.msg; qrState.className = 'qrstate' + (s.state === 'failed' ? ' err' : s.state === 'done' ? ' ok' : '');
        if (s.state === 'done') { toast(s.msg, { kind: 'ok' }); refreshAll(); job = null; setTimeout(() => dlg.open && dlg.close(), 1200); return; }
        if (s.state === 'failed') {
          job = null;
          qrState.append(' ', h('button', { class: 'linkbtn', type: 'button', onclick: startQr }, t('重试')));
          return;
        }
      }
      await new Promise(r => setTimeout(r, 1000));
    }
  }
  async function submit() {
    err.textContent = '';
    const tab = ok.dataset.tab;
    const [path, body] = tab === 'code'
      ? ['/api/adb/pair', { addr: pAddr.value, code: pCode.value, connect: pConn.value }]
      : ['/api/adb/connect', { addr: iAddr.value }];
    ok.disabled = true; const label = ok.textContent; ok.textContent = t('正在连接…');
    try {
      const r = await api(path + '?' + peerQ, { method: 'POST', body: JSON.stringify(body) });
      toast(r.msg, { kind: 'ok' }); refreshAll(); dlg.close();
    } catch (e) { err.textContent = e.message; }
    ok.disabled = false; ok.textContent = label;
  }
  show('qr');
}

async function wirelessAction(v, path, label) {
  const tt = toast(label, { sticky: true });
  try {
    const r = await api(path + '?peer=' + encodeURIComponent(v.host.id), { method: 'POST', body: JSON.stringify({ serial: v.dev.serial }) });
    tt.set(r.msg, 'ok');
    await refreshHost(v.host); renderSide();
  } catch (e) { tt.set(e.message, 'bad'); }
  tt.close(4500);
}

function settingsDialog() {
  let note = null;
  const changed = () => { if (viewers.size && note) note.hidden = false; };
  const row = (label, ...right) => h('div', { class: 'set-r' }, h('span', { class: 'k' }, label), ...right);
  const sw = (key, def, onChange) => {
    const i = h('input', { type: 'checkbox', onchange: () => { pref.set(key, i.checked ? '1' : '0'); if (onChange) onChange(); } });
    i.checked = pref.get(key, def) === '1';
    return h('label', { class: 'switch' }, i, h('i'));
  };
  const sel = (k) => {
    const s = h('select', { class: 'select', 'aria-label': QUALITY[k].label, onchange: () => { pref.set(k, s.value); changed(); } }, QUALITY[k].opts.map(([v, l]) => h('option', { value: v }, l)));
    s.value = q(k);
    return row(QUALITY[k].label, s);
  };
  const req = (text, v) => [text, h('small', {}, v)];
  const lang = h('select', { class: 'select', 'aria-label': 'Language', onchange: () => { localStorage.setItem('db_lang', lang.value); location.reload(); } },
    h('option', { value: 'zh' }, '简体中文'), h('option', { value: 'en' }, 'English'));
  lang.value = LANG;

  // 截图和录屏的保存位置：路径过长时保留末尾；本机才能修改或打开
  let mediaDir = '';
  const dirText = h('bdi', {}, '…');
  const dirEl = h('span', { class: 'v path' }, dirText);
  const setDir = (d) => { mediaDir = d || ''; dirText.textContent = mediaDir || '—'; dirEl.title = mediaDir; };
  api('/api/settings').then(r => setDir(r.media_dir)).catch(() => setDir(''));
  const dirTools = isLocalHost() ? [
    ib('folder', t('打开文件夹'), () => api('/api/media/reveal', { method: 'POST', body: '{}' }).catch(e => toast(e.message, { kind: 'bad' }))),
    ib('edit', t('更改保存位置'), async () => {
      const v = await promptDialog(t('更改保存位置'), null, { value: mediaDir, okText: t('保存') });
      if (v === null || !v.trim() || v.trim() === mediaDir) return;
      try {
        await api('/api/settings', { method: 'PUT', body: JSON.stringify({ media_dir: v.trim() }) });
        setDir(v.trim()); toast(t('已保存'), { kind: 'ok' });
      } catch (e) { toast(e.message, { kind: 'bad', ms: 4000 }); }
    }),
  ] : [];

  const ips = INFO ? (INFO.ips || []).map(ip => `${ip}:${INFO.port}`) : [];
  const mcpUrl = /^https?:$/.test(location.protocol) && !isLocalHost() ? `${location.protocol}//${location.host}/mcp` : `http://127.0.0.1:${INFO ? INFO.port : 8730}/mcp`;
  const group = (title, ...rows) => h('section', { class: 'set-g' }, h('div', { class: 'set-t' }, title), h('div', { class: 'set-c' }, rows));
  note = h('div', { class: 'set-n', hidden: true }, h('span', {}, t('已打开的投屏需重新连接才会生效')),
    h('button', { class: 'linkbtn', type: 'button', onclick: () => { for (const v of viewers.values()) v.reconnect(); note.hidden = true; } }, t('重新连接')));
  const body = [
    group(t('画面'), sel('size'), sel('rate'), sel('fps'), row(t('保持屏幕常亮'), sw('awake', '1', changed))),
    group(t('声音'),
      row(req(t('默认转发声音'), 'Android 11+'), sw('audio', '0')),
      row(req(t('同时在手机上播放'), 'Android 13+'), sw('audiodup', '0', changed))),
    group(t('通用'),
      row(LANG === 'zh' ? req('语言', 'Language') : req('Language', '语言'), lang),
      row(t('截图保存位置'), dirEl, ...dirTools)),
    group(t('AI 接入（MCP）'),
      row(t('地址'), h('span', { class: 'v mono' }, mcpUrl), ib('copy', t('复制'), () => copyText(mcpUrl)))),
    group(t('本机'),
      row(t('名称'), h('span', { class: 'v' }, INFO ? INFO.name : '—')),
      ...(ips.length ? ips : ['—']).map((a, i) => row(i ? '' : t('地址'), h('span', { class: 'v mono' }, a), a === '—' ? null : ib('copy', t('复制'), () => copyText(a))))),
    note,
  ];
  const ver = INFO ? h('span', { class: 'set-ver' }, `DroidBridge ${INFO.version} · scrcpy ${INFO.scrcpy}`) : null;
  const dlg = dialog({ title: t('设置'), body, cls: 'settings', actions: [ver, h('button', { class: 'btn primary', type: 'button', onclick: () => dlg.close() }, t('完成'))] });
  if (document.activeElement && dlg.contains(document.activeElement)) document.activeElement.blur();
}

// ---------------- 应用管理 ----------------
function appsDialog(hst, dev) {
  const peer = hst.id, serial = dev.serial;
  let apps = [], kind = 'user';
  const search = h('input', { class: 'input', type: 'search', placeholder: t('搜索名称或包名'), spellcheck: 'false', oninput: () => render() });
  const segBtn = (k, label) => h('button', { type: 'button', class: k === kind ? 'on' : '', onclick: e => { kind = k; [...seg.children].forEach(b => b.classList.toggle('on', b === e.currentTarget)); render(); } }, label);
  const seg = h('div', { class: 'seg sm' }, segBtn('user', t('用户应用')), segBtn('system', t('系统应用')));
  const list = h('div', { class: 'apps-list' });
  const count = h('span', { class: 'apps-n' });
  const call = async (a, action, label) => {
    const tt = toast(label + ' ' + (a.name || a.pkg) + '…', { sticky: true });
    try {
      const r = await api('/api/apps/action?peer=' + encodeURIComponent(peer), { method: 'POST', body: JSON.stringify({ serial, pkg: a.pkg, action }) });
      tt.set(r.msg + ' · ' + (a.name || a.pkg), 'ok'); tt.close(2500);
      if (action === 'uninstall') apps = apps.filter(x => x !== a);
      else a.running = action === 'launch' || (action === 'stop' || action === 'clear' ? false : a.running);
      render();
    } catch (e) { tt.set(e.message, 'bad'); tt.close(7000); }
  };
  const more = (btn, a) => menu(btn, [
    { icon: 'stop', label: t('强制停止'), fn: () => call(a, 'stop', t('正在停止')) },
    { icon: 'eraser', label: t('清除数据…'), fn: async () => { if (await confirmDialog(t('清除“{0}”的数据？', a.name || a.pkg), t('应用的全部数据、账号和设置都会被删除，无法恢复。'), t('清除数据'), true)) call(a, 'clear', t('正在清除')); } },
    a.system ? null : { icon: 'trash', label: t('卸载…'), danger: true, fn: async () => { if (await confirmDialog(t('卸载“{0}”？', a.name || a.pkg), t('应用及其数据会从设备上删除。'), t('卸载'), true)) call(a, 'uninstall', t('正在卸载')); } },
  ].filter(Boolean));
  function render() {
    const q = search.value.trim().toLowerCase();
    const shown = apps.filter(a => (kind === 'system') === a.system && (!q || a.pkg.toLowerCase().includes(q) || (a.name || '').toLowerCase().includes(q)));
    count.textContent = t('{0} 个应用', shown.length);
    if (!shown.length) { list.replaceChildren(h('div', { class: 'apps-empty' }, q ? t('没有匹配的应用') : t('没有应用'))); return; }
    list.replaceChildren(...shown.map(a => h('div', { class: 'app' },
      h('div', { class: 't' }, h('b', {}, h('span', {}, a.name || a.pkg), a.running ? h('span', { class: 'run', title: t('正在运行') }, '● ' + t('运行中')) : null), a.name ? h('small', {}, a.pkg) : null),
      h('div', { class: 'acts' }, ib('play', t('启动'), () => call(a, 'launch', t('正在启动'))), ib('more', t('更多'), e => more(e.currentTarget, a))))));
  }
  async function load() {
    list.replaceChildren(h('div', { class: 'apps-empty' }, h('span', { class: 'spin' }), t('正在读取应用列表…')));
    count.textContent = '';
    try {
      const r = await api('/api/apps?peer=' + encodeURIComponent(peer), { method: 'POST', body: JSON.stringify({ serial }) });
      const coll = new Intl.Collator(LANG === 'zh' ? 'zh-CN' : 'en');
      apps = r.apps.sort((a, b) => coll.compare(a.name || a.pkg, b.name || b.pkg));
      render();
    } catch (e) {
      list.replaceChildren(h('div', { class: 'apps-empty' }, h('span', {}, e.message), h('button', { class: 'btn', type: 'button', onclick: load }, t('重试'))));
    }
  }
  const dlg = dialog({
    title: t('应用管理') + ' · ' + (dev.model || serial), cls: 'apps',
    body: [h('div', { class: 'apps-bar' }, search, seg, ib('refresh', t('刷新'), load)), list],
    actions: [count, h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, t('关闭'))],
  });
  load();
}

// ---------------- 批量安装 ----------------
function batchInstallDialog(initial) {
  let file = initial || null, running = false, done = false;
  const picker = h('input', { type: 'file', accept: '.apk', hidden: true, onchange: () => { if (picker.files[0]) { file = picker.files[0]; showFile(); } picker.value = ''; } });
  const fileBtn = h('button', { class: 'bi-file', type: 'button', onclick: () => !running && picker.click() });
  const showFile = () => {
    fileBtn.replaceChildren(h('span', { html: svg('package') }).firstChild, file
      ? h('span', { class: 't' }, h('b', {}, file.name), '  ' + (file.size < 1048576 ? Math.max(1, Math.round(file.size / 1024)) + ' KB' : (file.size / 1048576).toFixed(1) + ' MB'))
      : h('span', { class: 't' }, t('选择 APK 文件')));
    update();
  };
  const rows = [];
  const groups = [];
  for (const hst of hosts.values()) {
    if (hst.error) continue;
    const ready = (hst.devices || []).filter(d => d.state === 'device');
    if (!ready.length) continue;
    const mine = [];
    for (const d of ready) {
      const cb = h('input', { type: 'checkbox', checked: true, onchange: () => update() });
      const res = h('span', { class: 'r' });
      const row = { hst, d, cb, res };
      mine.push(row); rows.push(row);
    }
    const all = h('button', { class: 'linkbtn', type: 'button', onclick: () => { const on = !mine.every(r => r.cb.checked); mine.forEach(r => r.cb.checked = on); update(); } }, t('全选'));
    groups.push(h('div', {}, h('div', { class: 'bi-h' }, hst.remote ? hst.name : t('本机'), all),
      ...mine.map(r => h('label', { class: 'bi-d' }, r.cb, h('span', { class: 't' }, r.d.model || r.d.serial, r.d.model ? h('small', {}, r.d.serial) : null), r.res))));
  }
  const ok = h('button', { class: 'btn primary', type: 'submit' });
  const retry = h('button', { class: 'btn', type: 'button', hidden: true, onclick: () => start(rows.filter(r => r.failed)) }, t('重试失败的设备'));
  function update() {
    if (done) { ok.textContent = t('完成'); ok.disabled = false; return; }
    const n = rows.filter(r => r.cb.checked).length;
    ok.textContent = running ? t('正在安装…') : n ? t('安装到 {0} 台设备', n) : t('安装');
    ok.disabled = running || !file || !n;
  }
  const setRes = (r, kind, text, icon) => { r.res.className = 'r' + (kind ? ' ' + kind : ''); r.res.replaceChildren(icon || '', text); r.res.title = text; };
  async function installOne(r) {
    r.failed = false;
    setRes(r, '', t('安装中'), h('span', { class: 'spin sm' }));
    // 部分手机（OPPO、vivo、小米等）需要在设备上点“安装”确认
    const slow = setTimeout(() => setRes(r, '', t('等待设备上确认…'), h('span', { class: 'spin sm' })), 10000);
    try {
      const qs = new URLSearchParams({ serial: r.d.serial, peer: r.hst.id, name: file.name, lang: LANG });
      const resp = await fetch(withToken('/api/push?' + qs), { method: 'POST', body: file });
      const j = await resp.json().catch(() => ({}));
      if (!resp.ok) throw new Error(j.error || ('HTTP ' + resp.status));
      setRes(r, 'ok', t('已安装'), h('span', { html: svg('check') }).firstChild);
    } catch (e) {
      r.failed = true;
      setRes(r, 'bad', e.message.replace(/^adb .*? (失败|failed)[:：]\s*/, ''));
    } finally { clearTimeout(slow); }
  }
  async function start(list) {
    running = true; done = false; retry.hidden = true;
    rows.forEach(r => r.cb.disabled = true);
    update();
    // 同时最多装 4 台
    const queue = [...list];
    await Promise.all(Array.from({ length: Math.min(4, queue.length) }, async () => { while (queue.length) await installOne(queue.shift()); }));
    running = false; done = true;
    const failed = rows.filter(r => r.failed).length;
    retry.hidden = !failed;
    update();
    toast(failed ? t('{0} 台安装失败', failed) : t('已安装到 {0} 台设备', list.length), { kind: failed ? 'bad' : 'ok' });
  }
  const dlg = dialog({
    title: t('批量安装 APK'), cls: 'wide',
    body: [fileBtn, picker, groups.length ? h('div', { class: 'bi-list' }, groups) : h('div', { class: 'empty' }, t('没有可用的设备'))],
    actions: [retry, h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, t('关闭')), ok],
    onsubmit: () => { if (done) dlg.close(); else if (!running) start(rows.filter(r => r.cb.checked)); },
  });
  showFile();
  if (document.activeElement === fileBtn) fileBtn.blur();
}
// 把 APK 拖到窗口空白处（不在投屏画面上）：批量安装
document.addEventListener('dragover', e => { if (e.dataTransfer && [...e.dataTransfer.types].includes('Files')) e.preventDefault(); });
document.addEventListener('drop', e => {
  const f = [...((e.dataTransfer && e.dataTransfer.files) || [])].find(x => /\.apk$/i.test(x.name));
  e.preventDefault();
  if (f && !document.querySelector('dialog[open]')) batchInstallDialog(f);
});
