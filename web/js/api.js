// 后端接口调用
// 由 web/index.html 拆分而来；经典脚本，按 index.html 中的顺序加载，共享全局作用域。
'use strict';

// ---------------- API ----------------
const withToken = (url) => {
  const tk = pref.get('token', '');
  return tk ? url + (url.includes('?') ? '&' : '?') + 'token=' + encodeURIComponent(tk) : url;
};
let tokenAsk = null;
async function api(path, opts = {}) {
  const r = await fetch(withToken(path), { ...opts, headers: { 'content-type': 'application/json', 'x-lang': LANG, ...(opts.headers || {}) } });
  let body = null;
  try { body = await r.json(); } catch {}
  if (r.status === 401) {
    tokenAsk = tokenAsk || promptDialog(t('需要访问令牌'), t('这台 DroidBridge 启用了访问令牌。'), { type: 'password', value: pref.get('token', '') }).finally(() => { tokenAsk = null; });
    const tk = await tokenAsk;
    if (tk !== null) { pref.set('token', tk.trim()); return api(path, opts); }
  }
  if (!r.ok) throw new Error((body && body.error) || ('HTTP ' + r.status));
  return body;
}
