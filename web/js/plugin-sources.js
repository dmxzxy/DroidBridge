// 插件源：合并后的插件目录（显示在插件列表里）和插件源管理页。说明见 docs/plugins/SOURCES.md
'use strict';

/** 最近一次的插件源数据：sources 插件源，catalog 合并后的插件（同一 id 只有一个），updates 有新版本的插件 id */
let pluginSrcView = null;
let pluginSrcLoadedAt = 0;
/** 正在操作的 key（插件 id 或插件源地址） */
let pluginSrcBusy = '';

function pluginSrcSet(v) {
  pluginSrcView = {
    sources: Array.isArray(v && v.sources) ? v.sources : [],
    catalog: Array.isArray(v && v.catalog) ? v.catalog : [],
    updates: Array.isArray(v && v.updates) ? v.updates : [],
  };
  pluginSrcDot();
}

/** 显示用的版本：去掉 '+' 后面的内容哈希 */
function pluginSrcVer(v) {
  return pluginTxt(v).split('+')[0];
}

/** 有更新时在设置按钮和设置页的“插件”页签上显示圆点 */
function pluginSrcDot() {
  const on = !!pluginSrcView && pluginSrcView.updates.length > 0;
  const els = [document.getElementById('gear'), document.getElementById('mGear'), document.getElementById('tbGear'), document.querySelector('dialog.settings .set-tab[data-k=plugins]')];
  for (const el of els) if (el && el.classList) el.classList.toggle('plug-upd', on);
}

/** 启动时读一次插件源，之后每 30 分钟读一次 */
function pluginSrcWatch() {
  const tick = () => pluginSrcLoad(false).catch(() => {});
  tick();
  setInterval(tick, 30 * 60 * 1000);
}

/** 插件源数据超过 5 分钟没读时重新读一次（插件变化后刷新圆点） */
function pluginSrcChanged() {
  if (pluginSrcLoadedAt && pluginSrcStale()) pluginSrcLoad(false).catch(() => {});
}

async function pluginSrcCall(body) {
  const r = await api('/api/plugins/sources', { method: 'POST', body: JSON.stringify(body) });
  pluginSrcSet(r.view);
  return r;
}

/** 读取插件源数据；refresh 时让后台先刷新较久没检查的清单。返回数据是否有变化 */
async function pluginSrcLoad(refresh) {
  const before = JSON.stringify(pluginSrcView);
  try {
    pluginSrcSet(await api('/api/plugins/sources' + (refresh ? '?refresh=1' : '')));
    pluginSrcLoadedAt = Date.now();
  } catch (e) {
    if (!pluginSrcView) pluginSrcSet(null);
  }
  return JSON.stringify(pluginSrcView) !== before;
}

function pluginSrcStale() { return Date.now() - pluginSrcLoadedAt > 5 * 60 * 1000; }

/** 插件目录里的一项 */
function pluginSrcEntry(id) {
  return pluginSrcView ? pluginSrcView.catalog.find((e) => e.id === id) || null : null;
}

/** 还没安装、可以从插件源安装的插件 */
function pluginSrcAvailable() {
  return pluginSrcView ? pluginSrcView.catalog.filter((e) => e.status === 'install' || e.status === 'incompatible') : [];
}

/** 有新版本、可以更新的插件 */
function pluginSrcUpdateList() {
  return pluginSrcView ? pluginSrcView.catalog.filter((e) => e.status === 'update') : [];
}

/** 插件来自哪个插件源（显示名） */
function pluginSrcEntryName(e) {
  if (!e) return '';
  if (e.source) return pluginTxt(e.source);
  const s = pluginSrcView && pluginSrcView.sources.find((x) => x.url === e.url);
  return s ? pluginSrcName(s) : '';
}

/** 插件源的显示名：清单里的名字，否则主机名或文件夹名 */
function pluginSrcName(s) {
  if (s.name) return pluginTxt(s.name);
  const u = String(s.url || '');
  const m = /^https?:\/\/([^/]+)/i.exec(u);
  return m ? m[1] : u.replace(/[\\/]+$/, '').split(/[\\/]/).pop() || u;
}

function pluginSrcTime(sec) {
  return sec ? new Date(sec * 1000).toLocaleString() : '';
}

/** 执行一个插件源操作；ctx: {reloadPlugins(), render()} */
async function pluginSrcRun(ctx, key, body, done) {
  pluginSrcBusy = key;
  ctx.render();
  try {
    const r = await pluginSrcCall(body);
    if (done) await done(r);
  } catch (e) {
    toast(e.message, { kind: 'bad', ms: 6000 });
  }
  pluginSrcBusy = '';
  await ctx.reloadPlugins();
}

/** 安装或更新；ctx 另需 plugin(id) 取已安装的插件 */
function pluginSrcInstall(ctx, e) {
  const upd = e.status === 'update';
  pluginSrcRun(ctx, e.id, { action: 'install', url: e.url, id: e.id }, async () => {
    await ctx.reloadPlugins();
    const ph = ctx.plugin(e.id);
    if (ph && (upd ? ph.enabled && ph.state === 'needs_grant' : !ph.enabled)) await pluginGrant(ph);
    pluginInvalidate();
    toast(t(upd ? '已更新 {0}' : '已安装 {0}', pluginTxt(e.name || e.id)), { kind: 'ok' });
  });
}

/** 卸载插件；p 为已安装的插件（kind: source | local | link） */
async function pluginSrcUninstall(ctx, p) {
  if (!(await confirmDialog(t('卸载“{0}”？', pluginTxt(p.name || p.id)), t('插件的设置和数据会保留'), t('卸载'), true))) return false;
  await pluginSrcRun(ctx, p.id, { action: 'uninstall', id: p.id }, () => pluginInvalidate());
  return true;
}

/** 插件行右侧的安装 / 更新按钮；没有可做的操作时返回 null */
function pluginSrcAction(ctx, e, manage) {
  if (!e) return null;
  // 需要更高的 DroidBridge 版本：按钮的位置放一个禁用的小标记
  if (e.status === 'incompatible') {
    return h('span', { class: 'plug-incompat', title: t('需要 DroidBridge {0}', pluginTxt(e.minHost)), html: svg('alert') });
  }
  if (!['install', 'update'].includes(e.status)) return null;
  if (pluginSrcBusy === e.id) return h('span', { class: 'plugsrc-st busy', html: svg('refresh') });
  const upd = e.status === 'update';
  const ver = upd ? pluginSrcVer(e.installed) + ' → ' + pluginSrcVer(e.version) : pluginSrcVer(e.version);
  return h('button', { class: 'btn sm' + (upd ? ' primary' : ''), type: 'button', disabled: !!pluginSrcBusy || !manage,
    title: [ver, upd ? pluginTxt(e.notes) : ''].filter(Boolean).join('\n'),
    onclick: (ev) => { ev.stopPropagation(); pluginSrcInstall(ctx, e); } }, t(upd ? '更新' : '安装'));
}

/** 插件源管理页。ctx: {manage, back(), reloadPlugins(), render()} */
function pluginSourcesPane(ctx) {
  const v = pluginSrcView;

  async function remove(s) {
    if (!(await confirmDialog(t('删除插件源“{0}”？', pluginSrcName(s)), t('已安装的插件不受影响'), t('删除'), true))) return;
    pluginSrcRun(ctx, s.url, { action: 'remove', url: s.url });
  }

  function row(s) {
    const local = !/^https?:/i.test(s.url);
    const tip = [s.url, s.checked ? t('上次检查') + '  ' + pluginSrcTime(s.checked) : ''].filter(Boolean).join('\n');
    const names = (s.plugins || []).join('\n');
    return h('div', { class: 'plugsrc-h', title: tip },
      h('span', { class: 'si', html: svg(local ? 'folder' : 'globe') }),
      h('b', {}, pluginSrcName(s)),
      s.version ? h('span', { class: 'ver' }, pluginSrcVer(s.version)) : null,
      h('span', { class: 'ver', title: names || undefined }, h('span', { class: 'si', html: svg('puzzle') }), pluginTxt(s.count || 0)),
      s.error ? h('span', { class: 'plugsrc-err', title: pluginTxt(s.error), html: svg('alert') }) : null,
      h('span', { class: 'grow' }),
      pluginSrcBusy === s.url ? h('span', { class: 'plugsrc-st busy', html: svg('refresh') })
        : ib('refresh', t('检查更新'), () => pluginSrcRun(ctx, s.url, { action: 'refresh', url: s.url })),
      ctx.manage ? ib('download', t('自动更新'), () => pluginSrcRun(ctx, s.url, { action: 'edit', url: s.url, auto: !s.auto }), s.auto ? 'on' : '') : null,
      ctx.manage ? ib(s.hasToken ? 'lock' : 'edit', t('访问令牌'), () => tokenDialog(s)) : null,
      ctx.manage ? ib('x', t('删除插件源'), () => remove(s)) : null);
  }

  function addDialog() {
    const url = h('input', { class: 'input mono', placeholder: 'https://…/index.json', spellcheck: 'false', autocomplete: 'off' });
    const token = h('input', { class: 'input mono', type: 'password', placeholder: t('可选'), autocomplete: 'off' });
    const header = h('input', { class: 'input mono', placeholder: 'Authorization', spellcheck: 'false', autocomplete: 'off' });
    const err = h('div', { class: 'dlg-err' });
    const more = h('details', { class: 'plugsrc-more' }, h('summary', {}, t('访问令牌')), field(t('令牌'), token), field(t('请求头'), header, 'PRIVATE-TOKEN / Deploy-Token'));
    const d = dialog({
      title: t('添加插件源'),
      body: [field(t('地址'), url, t('清单地址，或本机文件夹')), more, err],
      actions: [h('button', { class: 'btn', type: 'button', onclick: () => d.close() }, t('取消')),
        h('button', { class: 'btn primary', type: 'submit' }, t('添加'))],
      onsubmit: async () => {
        if (!url.value.trim()) { err.textContent = t('请输入插件源地址'); return; }
        err.textContent = '';
        try {
          await pluginSrcCall({ action: 'add', url: url.value, token: token.value, header: header.value });
          d.close();
          ctx.render();
        } catch (e) { err.textContent = e.message; }
      },
    });
    setTimeout(() => url.focus(), 50);
  }

  function tokenDialog(s) {
    const token = h('input', { class: 'input mono', type: 'password', placeholder: s.hasToken ? t('已设置，留空保持不变') : '', autocomplete: 'off' });
    const header = h('input', { class: 'input mono', placeholder: 'Authorization', spellcheck: 'false', autocomplete: 'off', value: s.header || '' });
    const clear = h('input', { type: 'checkbox' });
    const err = h('div', { class: 'dlg-err' });
    const d = dialog({
      title: pluginSrcName(s),
      body: [field(t('令牌'), token), field(t('请求头'), header, 'PRIVATE-TOKEN / Deploy-Token'),
        s.hasToken ? h('label', { class: 'plugsrc-clear' }, clear, t('清除令牌')) : null, err],
      actions: [h('button', { class: 'btn', type: 'button', onclick: () => d.close() }, t('取消')),
        h('button', { class: 'btn primary', type: 'submit' }, t('保存'))],
      onsubmit: async () => {
        const body = { action: 'edit', url: s.url, header: header.value };
        if (clear.checked) body.token = '';
        else if (token.value) body.token = token.value;
        try { await pluginSrcCall(body); d.close(); ctx.render(); } catch (e) { err.textContent = e.message; }
      },
    });
  }

  const sources = v ? v.sources : [];
  const head = h('div', { class: 'plugpane-bar' },
    ib('chevLeft', t('返回'), ctx.back),
    h('span', { class: 'plugpane-n grow' }, t('插件源')),
    ctx.manage ? ib('plus', t('添加插件源'), addDialog) : null,
    sources.length ? ib('refresh', t('检查更新'), () => pluginSrcRun(ctx, '*', { action: 'refresh' })) : null);
  const body = !v ? h('div', { class: 'plug-console-e' }, '…')
    : sources.length ? h('div', { class: 'set-c plugsrc-c' }, sources.map(row))
    : ctx.manage ? h('button', { class: 'plugsrc-add', type: 'button', onclick: addDialog },
      h('span', { class: 'si', html: svg('plus') }), h('span', {}, t('添加插件源')))
    : h('div', { class: 'plug-console-e' }, '—');
  return h('div', { class: 'plugpane-list plugsrc' }, head, body);
}
