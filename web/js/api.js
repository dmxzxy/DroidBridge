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
    // 从其他设备的浏览器访问：请求这台电脑授权（旧版本或固定令牌：直接输入令牌）
    tokenAsk = tokenAsk || (body && body.pair ? browserPairDialog()
      : promptDialog(t('需要访问令牌'), t('这台 DroidBridge 启用了访问令牌。'), { type: 'password', value: pref.get('token', '') }))
      .finally(() => { tokenAsk = null; });
    const tk = await tokenAsk;
    if (tk) { pref.set('token', tk.trim()); return api(path, opts); }
  }
  if (!r.ok) {
    const e = new Error((body && body.error) || ('HTTP ' + r.status));
    e.status = r.status; e.body = body;
    throw e;
  }
  return body;
}
