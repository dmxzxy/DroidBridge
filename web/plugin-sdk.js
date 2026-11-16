// 插件网页 SDK：在插件自己的网页（/plugin-web/<id>/<entry>）里使用。
// 页面里引入 <script src="/plugin-web/sdk.js"></script> 后：
//   await droidbridge.call('hello', { a: 1 })   // 调用 db.web.handle 注册的处理函数
//   droidbridge.on('tick', (data) => {})        // 接收 db.web.emit 发出的消息
//   droidbridge.device.serial                  // 打开这个面板的设备（可能为空）
// 与父页面通过 postMessage 通信：带请求 id，超时 5 分钟。
(function () {
  'use strict';
  var TIMEOUT = 5 * 60 * 1000;
  var pending = new Map();
  var listeners = new Map();
  var seq = 0;
  var sdk = {
    device: { serial: '' },
    /** 调用插件自己的 web 处理函数，返回 Promise */
    call: function (name, args) {
      return new Promise(function (resolve, reject) {
        var id = ++seq;
        var timer = setTimeout(function () {
          pending.delete(id);
          reject(new Error('调用超时：' + name));
        }, TIMEOUT);
        pending.set(id, { resolve: resolve, reject: reject, timer: timer });
        post({ __droidbridge: 1, t: 'call', id: id, name: String(name), args: args === undefined ? null : args });
      });
    },
    /** 监听 db.web.emit 发来的消息 */
    on: function (name, fn) {
      if (typeof fn !== 'function') return function () {};
      var key = String(name);
      if (!listeners.has(key)) listeners.set(key, []);
      listeners.get(key).push(fn);
      return function () { sdk.off(key, fn); };
    },
    off: function (name, fn) {
      var list = listeners.get(String(name));
      if (!list) return;
      var i = list.indexOf(fn);
      if (i >= 0) list.splice(i, 1);
    },
  };

  function post(msg) {
    try { window.parent.postMessage(msg, '*'); } catch (e) {}
  }

  window.addEventListener('message', function (e) {
    if (e.source !== window.parent) return;      // 只接受来自宿主页面的消息
    var d = e.data;
    if (!d || typeof d !== 'object' || d.__droidbridge !== 1) return;
    if (d.t === 'result') {
      var p = pending.get(d.id);
      if (!p) return;
      pending.delete(d.id);
      clearTimeout(p.timer);
      if (d.err !== null && d.err !== undefined) p.reject(new Error(String(d.err)));
      else p.resolve(d.ok);
    } else if (d.t === 'event') {
      var list = listeners.get(String(d.name));
      if (!list) return;
      for (var i = 0; i < list.slice().length; i++) {
        try { list[i](d.data); } catch (err) { console.error(err); }
      }
    } else if (d.t === 'init') {
      if (d.device && typeof d.device === 'object') sdk.device.serial = String(d.device.serial || '');
      var ready = listeners.get('ready');
      if (ready) for (var j = 0; j < ready.slice().length; j++) { try { ready[j](sdk.device); } catch (err) { console.error(err); } }
    }
  });

  window.droidbridge = sdk;
})();
