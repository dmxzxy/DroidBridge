// 插件源：设置 → 插件 列表下方。列出各插件源里的插件，安装 / 更新 / 卸载。说明见 docs/plugins/SOURCES.md
'use strict';

/** 最近一次的插件源数据；updates 为有新版本的插件 id */
let pluginSrcView = null;
const pluginSrcUpdates = new Set();
let pluginSrcLoadedAt = 0;

function pluginSrcSet(v) {
  pluginSrcView = { sources: Array.isArray(v && v.sources) ? v.sources : [], updates: Array.isArray(v && v.updates) ? v.updates : [] };
  pluginSrcUpdates.clear();
  for (const id of pluginSrcView.updates || []) pluginSrcUpdates.add(id);
}

async function pluginSrcCall(body) {
  const r = await api('/api/plugins/sources', { method: 'POST', body: JSON.stringify(body) });
  pluginSrcSet(r.view);
  return r;
}

/** 插件源的显示名：清单里的名字，否则主机名或文件夹名 */
function pluginSrcName(s) {
  if (s.name) return pluginTxt(s.name);
  const u = String(s.url || '');
  const m = /^https?:\/\/([^/]+)/i.exec(u);
  return m ? m[1] : u.replace(/[\\/]+$/, '').split(/[\\/]/).pop() || u;
}

function pluginSrcTime(sec) {
  if (!sec) return '';
  const d = new Date(sec * 1000);
  return d.toLocaleString();
}

/**
 * 设置页的插件源区块。ctx: {manage, reloadPlugins(), plugin(id), select(id), changed()}
 * 返回元素；数据异步加载后自动重画
 */
function pluginSourcesSection(ctx) {
  const box = h('div', { class: 'plugsrc' });
  let busy = '';      // 正在操作的 key（url 或 url|id）

  async function load(refresh) {
    const before = [...pluginSrcUpdates].join();
    try {
      pluginSrcSet(await api('/api/plugins/sources' + (refresh ? '?refresh=1' : '')));
      if ([...pluginSrcUpdates].join() !== before && ctx.changed) ctx.changed();
      pluginSrcLoadedAt = Date.now();
    } catch (e) {
      if (!pluginSrcView) pluginSrcSet(null);
    }
    paint();
  }

  async function run(key, body, done) {
    busy = key;
    paint();
    try {
      const r = await pluginSrcCall(body);
      if (done) await done(r);
    } catch (e) {
      toast(e.message, { kind: 'bad', ms: 6000 });
    }
    busy = '';
    paint();
    ctx.reloadPlugins();
  }

  // 安装：装好后像打开开关一样确认权限并启用
  function install(s, p) {
    run(s.url + '|' + p.id, { action: 'install', url: s.url, id: p.id }, async () => {
      await ctx.reloadPlugins();
      const ph = ctx.plugin(p.id);
      if (ph && !ph.enabled) await pluginGrant(ph);
      pluginInvalidate();
      toast(t('已安装 {0}', pluginTxt(p.name || p.id)), { kind: 'ok' });
    });
  }

  function update(s, p) {
    run(s.url + '|' + p.id, { action: 'install', url: s.url, id: p.id }, async () => {
      await ctx.reloadPlugins();
      const ph = ctx.plugin(p.id);
      if (ph && ph.enabled && ph.state === 'needs_grant') await pluginGrant(ph);
      pluginInvalidate();
      toast(t('已更新 {0}', pluginTxt(p.name || p.id)), { kind: 'ok' });
    });
  }

  async function uninstall(s, p) {
    if (!(await confirmDialog(t('卸载“{0}”？', pluginTxt(p.name || p.id)), t('插件的设置和数据会保留'), t('卸载'), true))) return;
    run(s.url + '|' + p.id, { action: 'uninstall', id: p.id }, () => pluginInvalidate());
  }

  async function remove(s) {
    if (!(await confirmDialog(t('删除插件源“{0}”？', pluginSrcName(s)), t('已安装的插件不受影响'), t('删除'), true))) return;
    run(s.url, { action: 'remove', url: s.url });
  }

  // ---------- 画 ----------
  function action(s, p) {
    const key = s.url + '|' + p.id;
    if (busy === key) return h('span', { class: 'plugsrc-st busy', html: svg('refresh') });
    const dis = !!busy || !ctx.manage;
    switch (p.status) {
      case 'install':
        return h('button', { class: 'btn sm', type: 'button', disabled: dis, onclick: () => install(s, p) }, t('安装'));
      case 'update':
        return h('button', { class: 'btn sm primary', type: 'button', disabled: dis, title: pluginTxt(p.installed) + ' → ' + pluginTxt(s.version),
          onclick: () => update(s, p) }, t('更新'));
      case 'installed':
        return h('span', { class: 'plugsrc-acts' },
          h('span', { class: 'plugsrc-st ok', title: t('已安装') + (p.installed ? '  ' + pluginTxt(p.installed) : ''), html: svg('check') }),
          ctx.manage ? ib('trash', t('卸载'), () => uninstall(s, p)) : null);
      case 'local':
        return h('span', { class: 'plugsrc-st', title: t('本地开发中的同名插件，不会被覆盖'), html: svg('terminal') });
      default:
        return h('span', { class: 'plugsrc-st', title: t('已从其他插件源安装') + '\n' + pluginTxt(p.from || ''), html: svg('link') });
    }
  }

  function pluginRow(s, p) {
    const tip = [pluginTxt(p.name || p.id), pluginTxt(p.description || ''), p.id].filter(Boolean).join('\n');
    const open = ctx.plugin(p.id) && p.status !== 'install' ? () => ctx.select(p.id) : null;
    return h('div', { class: 'plug-row plugsrc-row' + (open ? '' : ' static'), title: tip, onclick: open },
      pluginTile(p),
      h('div', { class: 'plug-row-m' },
        h('div', { class: 'plug-row-n' }, h('b', {}, pluginTxt(p.name || p.id)))),
      h('span', { onclick: (e) => e.stopPropagation() }, action(s, p)));
  }

  function sourceCard(s) {
    const local = !/^https?:/i.test(s.url);
    const head = h('div', { class: 'plugsrc-h' },
      h('span', { class: 'si', html: svg(local ? 'folder' : 'globe') }),
      h('b', { title: s.url }, pluginSrcName(s)),
      s.version ? h('span', { class: 'ver', title: t('上次检查') + '  ' + pluginSrcTime(s.checked) }, pluginTxt(s.version)) : null,
      s.error ? h('span', { class: 'plugsrc-err', title: pluginTxt(s.error), html: svg('alert') }) : null,
      h('span', { class: 'grow' }),
      busy === s.url ? h('span', { class: 'plugsrc-st busy', html: svg('refresh') }) : ib('refresh', t('检查更新'), () => run(s.url, { action: 'refresh', url: s.url })),
      ctx.manage ? ib(s.hasToken ? 'lock' : 'edit', t('访问令牌'), () => tokenDialog(s)) : null,
      ctx.manage ? ib('x', t('删除插件源'), () => remove(s)) : null);
    const rows = (s.plugins || []).map((p) => pluginRow(s, p));
    return h('div', { class: 'set-c plugsrc-c' }, head, rows.length ? rows : h('div', { class: 'plug-row muted static' }, '—'));
  }

  function paint() {
    const v = pluginSrcView;
    const bar = h('div', { class: 'plugpane-bar' },
      h('span', { class: 'plugpane-n', html: svg('globe'), title: t('插件源') }),
      h('span', { class: 'plugpane-n grow' }, t('插件源')),
      ctx.manage ? ib('plus', t('添加插件源'), addDialog) : null,
      v && v.sources.length ? ib('refresh', t('检查更新'), () => run('*', { action: 'refresh' })) : null);
    const cards = v ? v.sources.map(sourceCard) : [h('div', { class: 'plug-console-e' }, '…')];
    if (v && !v.sources.length && ctx.manage) {
      cards.push(h('button', { class: 'plugsrc-add', type: 'button', onclick: addDialog },
        h('span', { class: 'si', html: svg('plus') }), h('span', {}, t('添加插件源'))));
    }
    box.replaceChildren(bar, ...cards);
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
          paint();
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
        try { await pluginSrcCall(body); d.close(); paint(); } catch (e) { err.textContent = e.message; }
      },
    });
  }

  paint();
  load(Date.now() - pluginSrcLoadedAt > 5 * 60 * 1000);
  return box;
}
