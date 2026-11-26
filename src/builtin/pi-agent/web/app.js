(() => {
  const $ = (id) => document.getElementById(id);
  const { render: md, esc } = window.md;
  const log = $("log"), q = $("q"), btnSend = $("btnSend");

  const P = {
    phone: '<rect x="7" y="2.5" width="10" height="19" rx="2"/><path d="M11 18.5h2"/>',
    monitor: '<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
    send: '<path d="M12 19V5M6 11l6-6 6 6"/>',
    stop: '<rect x="7" y="7" width="10" height="10" rx="1.5" fill="currentColor" stroke="none"/>',
    spark: '<path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z"/><path d="M19 15l.8 2.2L22 18l-2.2.8L19 21l-.8-2.2L16 18l2.2-.8z"/>',
    check: '<path d="M5 12.5l4.5 4.5L19 7.5"/>',
    x: '<path d="M6 6l12 12M18 6L6 18"/>',
    chev: '<path d="M9 6l6 6-6 6"/>',
    alert: '<path d="M12 3l9.5 17h-19z"/><path d="M12 10v4M12 17.5v.01"/>',
    copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V5a1 1 0 00-1-1H5a1 1 0 00-1 1v10a1 1 0 001 1h3"/>',
    list: '<path d="M8 6h12M8 12h12M8 18h12M4 6h.01M4 12h.01M4 18h.01"/>',
    info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v5M12 8v.01"/>',
    doc: '<path d="M6 3h8l4 4v14H6z"/><path d="M9 12h6M9 16h6"/>',
    bug: '<rect x="8" y="7" width="8" height="12" rx="4"/><path d="M12 7V5M8 11H4M20 11h-4M8 16H5M19 16h-3M9 4l1.5 2M15 4l-1.5 2"/>',
    image: '<rect x="3" y="4" width="18" height="16" rx="2"/><circle cx="9" cy="10" r="1.8"/><path d="M21 16l-5-5-9 9"/>',
    layout: '<rect x="3" y="3" width="18" height="18" rx="2"/><path d="M3 9h18M9 9v12"/>',
    grid: '<rect x="4" y="4" width="6" height="6" rx="1"/><rect x="14" y="4" width="6" height="6" rx="1"/><rect x="4" y="14" width="6" height="6" rx="1"/><rect x="14" y="14" width="6" height="6" rx="1"/>',
    package: '<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/><path d="M4 7.5l8 4.5 8-4.5M12 12v9"/>',
    activity: '<path d="M3 12h4l3-8 4 16 3-8h4"/>',
    terminal: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M7 9l3 3-3 3M12 15h5"/>',
    pointer: '<path d="M6 3l12 7-5 1.5L10.5 17z"/>',
    move: '<path d="M5 12h14M15 8l4 4-4 4"/>',
    key: '<rect x="3" y="6" width="18" height="12" rx="2"/><path d="M7 10h.01M11 10h.01M15 10h.01M8 14h8"/>',
    type: '<path d="M5 6h14M12 6v13M9 19h6"/>',
    play: '<path d="M7 4.5v15l12-7.5z"/>',
    square: '<rect x="6" y="6" width="12" height="12" rx="1.5"/>',
  };
  const ic = (n, cls = "") => `<svg class="${cls}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">${P[n] || P.info}</svg>`;

  const T = (zh, en) => (window.droidbridge && droidbridge.lang === "en" ? en : zh);
  const parts = (...xs) => xs.filter(Boolean).join(" · ");
  /** 每个工具：名称、图标、正在进行时的说法、参数摘要 */
  const TOOLS = () => ({
    db_devices: { label: T("设备列表", "Devices"), icon: "list", doing: T("查看设备列表", "listing devices") },
    db_device_info: { label: T("设备信息", "Device info"), icon: "info", doing: T("查看设备信息", "reading device info") },
    db_logcat: { label: T("日志", "Logs"), icon: "doc", doing: T("读取日志", "reading logs"), detail: (a) => parts(a.lines && T(`最近 ${a.lines} 行`, `last ${a.lines} lines`), a.level && T(`${a.level} 及以上`, `${a.level} and above`), a.package, a.grep && T(`含 “${a.grep}”`, `containing “${a.grep}”`)) },
    db_crash: { label: T("崩溃记录", "Crashes"), icon: "bug", doing: T("查看崩溃记录", "checking crashes") },
    db_screenshot: { label: T("截图", "Screenshot"), icon: "image", doing: T("截图", "taking a screenshot") },
    db_ui: { label: T("界面控件", "Screen views"), icon: "layout", doing: T("读取界面", "reading the screen") },
    db_apps: { label: T("应用列表", "Apps"), icon: "grid", doing: T("查看应用列表", "listing apps"), detail: (a) => a.filter },
    db_app_info: { label: T("应用详情", "App details"), icon: "package", doing: T("查看应用详情", "reading app details"), detail: (a) => a.package },
    db_perf: { label: T("性能数据", "Performance"), icon: "activity", doing: T("读取性能数据", "reading performance") },
    db_shell: { label: T("执行命令", "Shell"), icon: "terminal", doing: T("执行命令", "running a command"), detail: (a) => a.cmd },
    db_tap: { label: T("点击", "Tap"), icon: "pointer", doing: T("点击屏幕", "tapping"), detail: (a) => `(${a.x}, ${a.y})` },
    db_swipe: { label: T("滑动", "Swipe"), icon: "move", doing: T("滑动屏幕", "swiping"), detail: (a) => `(${a.x1}, ${a.y1}) → (${a.x2}, ${a.y2})` },
    db_key: { label: T("按键", "Key"), icon: "key", doing: T("按键", "pressing a key"), detail: (a) => a.key },
    db_text: { label: T("输入文字", "Type"), icon: "type", doing: T("输入文字", "typing"), detail: (a) => a.text },
    db_launch: { label: T("启动应用", "Launch app"), icon: "play", doing: T("启动应用", "launching the app"), detail: (a) => a.package },
    db_stop: { label: T("停止应用", "Stop app"), icon: "square", doing: T("停止应用", "stopping the app"), detail: (a) => a.package },
  });
  // 其他插件的工具（<插件id>__<名字>）：显示工具名，详情取第一个文字参数
  const pluginMeta = (name) => {
    const tool = String(name).split("__").slice(1).join("__");
    return { label: tool, icon: "info", doing: T("调用 ", "calling ") + tool, detail: (a) => String(Object.values(a).find((v) => typeof v === "string") || "").slice(0, 80) };
  };
  const meta = (name) => TOOLS()[name] || (String(name).includes("__") ? pluginMeta(name) : { label: String(name).replace(/^db_/, ""), icon: "info", doing: T("调用 ", "calling ") + name });
  const detailOf = (t) => { try { return (meta(t.name).detail && meta(t.name).detail(t.args || {})) || ""; } catch { return ""; } };

  /** 把原始错误换成看得懂的说法 */
  function friendly(text) {
    const s = String(text || "");
    const rules = [
      [/没有找到 pi|无法启动|无法从 .* 找到 Pi|Pi 已退出/i, T("Pi 没能启动", "Pi could not start"), T("检查 Pi 是否已安装", "Check that Pi is installed")],
      [/no api key|no credentials|not authenticated|not logged in|login/i, T("没有可用的模型账号", "No model account available"), T("在插件设置里配置模型服务，或先在 Pi 里登录", "Set up a model service in the plugin settings, or sign in to Pi first")],
      [/401|403|unauthori|invalid.*(api.?key|token)|incorrect api key/i, T("Token 无效", "Invalid token"), T("在插件设置里检查模型服务的 Token", "Check the model service token in the plugin settings")],
      [/model.*not found|unknown model|no model|404/i, T("找不到这个模型", "Model not found"), T("在插件设置里检查模型名称", "Check the model name in the plugin settings")],
      [/429|rate.?limit|quota|insufficient/i, T("请求太频繁或额度不足", "Too many requests or out of quota"), T("稍后再试，或检查模型账号的额度", "Try again later, or check the model account's quota")],
      [/connection|econnrefused|enotfound|fetch failed|timed? ?out|network/i, T("连不上模型服务", "Cannot reach the model service"), T("检查网络，或插件设置里的接口地址", "Check the network, or the API URL in the plugin settings")],
    ];
    for (const [re, title, hint] of rules) if (re.test(s)) return { title, hint };
    return { title: T("出错了", "Something went wrong"), hint: "" };
  }

  let key = "", info = null, model = "", busy = false, items = [];

  /** 设备操作模式：会改变设备状态的工具怎么处理 */
  const MODES = () => [
    { value: "confirm", label: T("操作前确认", "Ask first"), desc: T("点击、输入、命令等先问你", "Asks you before taps, typing, commands, etc.") },
    { value: "auto", label: T("直接操作", "Act directly"), desc: T("不再询问，直接执行", "Acts without asking") },
    { value: "off", label: T("只读", "Read only"), desc: T("只查看，不改变设备", "Only looks, never changes the device") },
  ];

  // ---------------------------------------------------------------- 渲染

  function header() {
    const dev = info && info.device;
    $("ctxIcon").outerHTML = `<i id="ctxIcon">${ic(dev ? "phone" : "monitor")}</i>`;
    $("ctxName").textContent = dev || T("所有设备", "All devices");
    const m = model || (info && info.model) || "";
    const chip = $("model");
    chip.hidden = !info || !info.pi || !info.pi.ok;
    chip.textContent = m ? m.replace(/^[^/]+\//, "") : T("Pi 默认模型", "Pi default model");
    const many = !!(info && Array.isArray(info.models) && info.models.length > 1);
    chip.classList.toggle("pick", many);
    const mode = MODES().find((x) => x.value === (info && info.actions));
    chip.title = `${T("模型：", "Model: ")}${m || T("Pi 默认模型", "Pi default model")}${info && info.pi && info.pi.version ? `\nPi ${info.pi.version}` : ""}`;
    const mc = $("mode");
    mc.hidden = chip.hidden || !mode;
    if (mode) { mc.textContent = mode.label; mc.title = `${T("设备操作：", "Device actions: ")}${mode.desc}\n${T("点击切换", "Click to change")}`; mc.dataset.mode = mode.value; }
    q.placeholder = (dev ? T(`问 ${dev} 的问题`, `Ask about ${dev}`) : T("描述要解决的问题", "Describe the problem")) + T("，输入 / 查看命令", "; type / for commands");
    $("btnNew").title = T("新对话", "New chat");
    document.title = T("AI 助手", "AI Assistant");
  }

  function emptyView() {
    if (info && info.pi && !info.pi.ok) {
      return `<div class="empty"><div class="logo">${ic("alert")}</div><h3>${T("需要先安装 Pi", "Install Pi first")}</h3><p>${T("AI 助手由 Pi 驱动，装好后就能用", "The AI assistant runs on Pi; it works once Pi is installed")}</p>
        <div class="setup"><ol>
          <li>${T("安装 Node.js（18 或更新）", "Install Node.js (18 or later)")}</li>
          <li>${T("在终端运行：", "Run in a terminal:")}<div class="cmd"><span>npm i -g @earendil-works/pi-coding-agent</span><button type="button" class="ib" data-copy="npm i -g @earendil-works/pi-coding-agent" title="${T("复制", "Copy")}">${ic("copy")}</button></div></li>
          <li>${T("在插件设置里配置模型服务（或使用 Pi 自己登录的账号）", "Set up a model service in the plugin settings (or use the account Pi is signed in with)")}</li>
        </ol>
        <div class="raw">${esc(info.pi.error || "")}</div>
        <button type="button" class="btn" data-act="recheck">${T("重新检测", "Check again")}</button></div></div>`;
    }
    const dev = info && info.device;
    const sugs = dev
      ? [["bug", T("前台应用为什么闪退？", "Why does the foreground app crash?")], ["activity", T("设备为什么卡、发热？", "Why is the device slow or hot?")], ["doc", T("最近日志里有哪些报错？", "What errors are in the recent log?")], ["layout", T("当前界面有没有异常？", "Is anything wrong on the screen?")]]
      : [["bug", T("检查所有设备有没有崩溃", "Check all devices for crashes")], ["list", T("现在连了哪些设备？", "Which devices are connected?")], ["info", T("怎么无线连接设备？", "How do I connect a device wirelessly?")]];
    return `<div class="empty"><div class="logo">${ic("spark")}</div>
      <h3>${dev ? T("问问这台设备", "Ask about this device") : T("有什么可以帮你？", "How can I help?")}</h3>
      <p>${dev ? T("说出要解决的问题，我会查日志、崩溃、截图和界面找原因", "Describe the problem; I will check logs, crashes, screenshots and the screen to find the cause") : T("排查设备和应用的问题，或问 DroidBridge 怎么用", "Troubleshoot devices and apps, or ask how to use DroidBridge")}</p>
      <div class="sugs">${sugs.map(([i, t]) => `<button type="button" data-ask="${esc(t)}">${ic(i)}${esc(t)}</button>`).join("")}</div></div>`;
  }

  /** 工具调用：每步一行，显示做了什么和结果 */
  function stepsView(group, running) {
    const rows = group.map((t) => {
      const m = meta(t.name), d = detailOf(t);
      const live = !t.done && running;
      const mark = live ? '<span class="spin"></span>' : t.error ? ic("x", "bad") : t.done ? ic("check", "ok") : "";
      const res = live ? T(`正在${m.doing}…`, `${m.doing}…`) : t.summary || "";
      return `<li class="step${t.error ? " fail" : ""}" title="${esc(m.label + (d ? "：" + d : "") + (t.summary ? " · " + t.summary : ""))}"><span class="mark">${mark}</span>${ic(m.icon)}<span class="name">${esc(m.label)}</span>${d ? `<span class="detail">${esc(d)}</span>` : ""}<span class="res">${esc(res)}</span></li>`;
    }).join("");
    return `<ul class="steps">${rows}</ul>`;
  }

  function errorView(text) {
    const f = friendly(text);
    return `<div class="err">${ic("alert")}<div class="body"><b>${esc(f.title)}</b>${f.hint ? `<div>${esc(f.hint)}</div>` : ""}<div class="raw">${esc(text)}</div></div><button type="button" data-act="retry">${T("重试", "Retry")}</button></div>`;
  }

  function render() {
    const stick = log.scrollHeight - log.scrollTop - log.clientHeight < 60;
    const html = [];
    if (!items.length) {
      html.push(emptyView());
    } else {
      for (let i = 0; i < items.length; i++) {
        const it = items[i];
        if (it.role === "user") html.push(`<div class="turn-user"><div>${esc(it.text)}</div></div>`);
        else if (it.role === "ai") {
          // 后面还有工具调用的是过程说明，最后一段才是结果
          let mid = false;
          for (let k = i + 1; k < items.length && items[k].role !== "user"; k++) if (items[k].role === "tool") { mid = true; break; }
          html.push(mid ? `<div class="note">${md(it.text)}</div>` : `<div class="answer">${md(it.text)}</div>`);
        }
        else if (it.role === "error") html.push(errorView(it.text));
        else if (it.role === "tool") {
          const g = [];
          while (i < items.length && items[i].role === "tool") g.push(items[i++]);
          i--;
          html.push(stepsView(g, busy));
        }
      }
      const last = items[items.length - 1];
      const toolRunning = last && last.role === "tool" && !last.done;
      if (busy && !toolRunning && !(last && last.role === "ai" && last.open)) {
        html.push(`<div class="working"><span class="spin"></span>${T("思考中…", "Thinking…")}</div>`);
      }
    }
    log.innerHTML = html.join("");
    for (const c of log.querySelectorAll(".copy")) c.innerHTML = ic("copy");
    if (stick) log.scrollTop = log.scrollHeight;
    updateSend();
  }

  let pending = false;
  const schedule = () => { if (!pending) { pending = true; requestAnimationFrame(() => { pending = false; render(); }); } };

  function updateSend() {
    const empty = !q.value.trim();
    const stop = busy && empty;
    btnSend.classList.toggle("stop", stop);
    btnSend.innerHTML = stop ? `${ic("stop")}<span>${T("停止", "Stop")}</span>` : ic("send");
    btnSend.title = stop ? T("停止回答", "Stop") : busy ? T("补充要求", "Add to the request") : T("发送", "Send");
    btnSend.disabled = !stop && empty;
    $("input").classList.toggle("off", !!(info && info.pi && !info.pi.ok));
  }

  // ---------------------------------------------------------------- 事件

  droidbridge.on("ev", (e) => {
    if (e.key !== key) return;
    const last = items[items.length - 1];
    switch (e.kind) {
      case "user": items.push({ role: "user", text: e.text }); break;
      case "delta":
        if (last && last.role === "ai" && last.open) last.text += e.text;
        else items.push({ role: "ai", text: e.text, open: true });
        break;
      case "tool":
        if (last && last.open) last.open = false;
        items.push({ role: "tool", id: e.id, name: e.name, args: e.args });
        break;
      case "tool_end": {
        const t = items.find((x) => x.role === "tool" && x.id === e.id);
        if (t) { t.done = true; t.error = !!e.error; t.summary = e.summary; }
        break;
      }
      case "error": items.push({ role: "error", text: e.text }); break;
      case "notice": droidbridge.notify(e.text, { kind: "warn" }); return;
      case "busy":
        busy = e.busy;
        if (!busy) for (const x of items) { x.open = false; if (x.role === "tool") x.done = true; }
        break;
      case "reset": items = Array.isArray(e.items) ? e.items : []; break;
      case "model": model = e.model || ""; header(); return;
    }
    schedule();
  });

  droidbridge.on("settings", () => { model = ""; loadInfo(); });

  async function call(name, args) {
    try {
      const r = await droidbridge.call(name, args || {});
      if (r && r.error) items.push({ role: "error", text: r.error }), schedule();
      return r;
    } catch (err) {
      items.push({ role: "error", text: String(err.message || err) });
      schedule();
    }
  }

  async function ask(text) {
    text = String(text || "").trim();
    if (!text) return;
    await call("send", { text });
  }

  async function send() {
    if (busy && !q.value.trim()) return call("abort");
    if (q.value.trim().startsWith("/") && await runCommand(q.value)) return;
    const text = q.value;
    q.value = "";
    fit();
    updateSend();
    await ask(text);
  }

  function fit() {
    q.style.height = "auto";
    q.style.height = Math.min(q.scrollHeight, 160) + "px";
  }

  function copy(text) {
    const done = () => droidbridge.notify(T("已复制", "Copied"), { kind: "ok" });
    const fallback = () => {
      const t = document.createElement("textarea");
      t.value = text;
      document.body.appendChild(t);
      t.select();
      document.execCommand("copy");
      t.remove();
      done();
    };
    try { navigator.clipboard.writeText(text).then(done, fallback); } catch { fallback(); }
  }

  log.addEventListener("click", (e) => {
    const b = e.target.closest("button");
    if (b) {
      if (b.dataset.ask) return ask(b.dataset.ask);
      if (b.dataset.copy) return copy(b.dataset.copy);
      if (b.classList.contains("copy")) return copy(b.parentElement.querySelector("code").textContent);
      if (b.dataset.act === "retry") {
        const u = [...items].reverse().find((x) => x.role === "user");
        return u && ask(u.text);
      }
      if (b.dataset.act === "recheck") return loadInfo();
    }
  });

  q.addEventListener("input", () => { fit(); updateSend(); menuSel = 0; paintMenu(); });
  q.addEventListener("blur", () => { menu.hidden = true; });
  q.addEventListener("focus", paintMenu);
  q.addEventListener("keydown", (e) => {
    if (e.isComposing) return;
    const open = !menu.hidden && menuItems.length;
    if (open && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
      e.preventDefault();
      menuSel = (menuSel + (e.key === "ArrowDown" ? 1 : menuItems.length - 1)) % menuItems.length;
      paintMenu();
      return;
    }
    if (open && e.key === "Escape") { e.preventDefault(); menu.hidden = true; return; }
    if (open && (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey))) {
      e.preventDefault();
      return void pick(menuItems[menuSel]);
    }
    if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); if (q.value.trim()) send(); }
  });
  btnSend.addEventListener("click", send);
  $("btnNew").innerHTML = ic("plus");

  // ---------------------------------------------------------------- 斜杠命令

  const menu = $("cmds");
  let menuItems = [], menuSel = 0;

  const modelList = () => (info && Array.isArray(info.models) ? info.models : []);
  const curModel = () => (model || (info && info.model) || "").replace(/^[^/]+\//, "");

  const COMMANDS = () => [
    { name: "new", alias: ["clear"], desc: T("开始新对话", "Start a new chat"), run: () => call("reset") },
    { name: "model", desc: T("选择模型", "Choose a model"), arg: true },
    { name: "mode", desc: T("设备操作：操作前确认 / 直接操作 / 只读", "Device actions: ask first / act directly / read only"), arg: true },
    { name: "stop", desc: T("停止回答", "Stop the answer"), run: () => call("abort") },
    { name: "retry", desc: T("重新问上一个问题", "Ask the last question again"), run: () => {
      const u = [...items].reverse().find((x) => x.role === "user");
      if (u) return ask(u.text);
    } },
  ];

  const useModel = (m) => call("model.use", { id: m });
  const setMode = async (v) => {
    const r = await call("actions.set", { value: v });
    if (r && r.ok) droidbridge.notify(T("设备操作：", "Device actions: ") + MODES().find((x) => x.value === v).label, { kind: "ok" });
  };

  /** 根据输入框内容算出菜单项 */
  function menuFor(text) {
    const m = text.match(/^\/(\S*)(\s+(.*))?$/s);
    if (!m) return [];
    const word = m[1].toLowerCase();
    if (m[2] !== undefined) {
      if (word === "mode") {
        const cur = info && info.actions;
        return MODES().map((x) => ({ label: x.label, desc: x.value === cur ? T("当前", "current") : x.desc, icon: x.value === cur ? "check" : "", run: () => setMode(x.value) }));
      }
      if (word !== "model") return [];
      const f = (m[3] || "").trim().toLowerCase();
      const list = modelList();
      if (!list.length) return [{ label: curModel() || T("Pi 默认模型", "Pi default model"), desc: T("在插件设置里配置模型服务后可切换", "Set up a model service in the plugin settings to switch"), disabled: true }];
      return list.filter((x) => x.toLowerCase().includes(f))
        .map((x) => ({ label: x, desc: x === curModel() ? T("当前", "current") : "", icon: x === curModel() ? "check" : "", run: () => useModel(x) }));
    }
    return COMMANDS().filter((c) => [c.name, ...(c.alias || [])].some((n) => n.startsWith(word)))
      .map((c) => ({ label: "/" + c.name, desc: c.desc, cmd: c }));
  }

  function paintMenu() {
    menuItems = q.value.startsWith("/") ? menuFor(q.value) : [];
    if (menuSel >= menuItems.length) menuSel = 0;
    menu.hidden = !menuItems.length;
    menu.innerHTML = menuItems.map((it, k) => `<button type="button" data-k="${k}" class="${k === menuSel ? "sel" : ""}"${it.disabled ? " disabled" : ""}>
      <span class="ck">${it.icon ? ic(it.icon) : ""}</span><b>${esc(it.label)}</b><span class="desc">${esc(it.desc || "")}</span></button>`).join("");
  }

  /** 执行菜单项：命令带参数时补全到输入框，否则直接执行 */
  async function pick(it) {
    if (!it || it.disabled) return;
    if (it.cmd && it.cmd.arg) { q.value = "/" + it.cmd.name + " "; menuSel = 0; paintMenu(); q.focus(); return; }
    q.value = ""; fit(); paintMenu(); updateSend();
    if (it.cmd) await it.cmd.run(); else if (it.run) await it.run();
  }

  /** 回车时把整行当命令执行；不是已知命令返回 false */
  async function runCommand(text) {
    const m = text.trim().match(/^\/(\S+)\s*(.*)$/s);
    if (!m) return false;
    const c = COMMANDS().find((x) => x.name === m[1].toLowerCase() || (x.alias || []).includes(m[1].toLowerCase()));
    if (!c) { droidbridge.notify(T(`没有 /${m[1]} 这个命令`, `No command /${m[1]}`), { kind: "warn" }); return true; }
    q.value = ""; fit(); paintMenu(); updateSend();
    if (c.name === "mode") {
      const want = m[2].trim();
      const hit = MODES().find((x) => x.value === want || x.label === want);
      if (hit) await setMode(hit.value);
      else { q.value = "/mode "; paintMenu(); q.focus(); }
      return true;
    }
    if (c.name === "model") {
      const want = m[2].trim().toLowerCase();
      const hit = modelList().find((x) => x.toLowerCase() === want) || modelList().find((x) => x.toLowerCase().includes(want));
      if (want && hit) await useModel(hit);
      else { q.value = "/model "; paintMenu(); q.focus(); }
      return true;
    }
    await c.run();
    return true;
  }

  menu.addEventListener("mousedown", (e) => e.preventDefault());
  menu.addEventListener("click", (e) => {
    const b = e.target.closest("button[data-k]");
    if (b) pick(menuItems[+b.dataset.k]);
  });

  /** 点模型标签：打开模型菜单 */
  $("model").addEventListener("click", () => {
    if (!modelList().length) return;
    q.value = "/model ";
    menuSel = 0;
    paintMenu();
    q.focus();
  });
  /** 点设备操作标签：打开模式菜单 */
  $("mode").addEventListener("click", () => {
    q.value = "/mode ";
    menuSel = 0;
    paintMenu();
    q.focus();
  });
  $("btnNew").addEventListener("click", () => call("reset"));

  async function loadInfo() {
    try { info = await droidbridge.call("info", {}); } catch { info = null; }
    header();
    render();
  }

  let started = false;
  async function init() {
    if (started) return;
    started = true;
    key = droidbridge.device.serial || "";
    header();
    const s = await droidbridge.call("state", {}).catch(() => null);
    if (s) { items = Array.isArray(s.items) ? s.items : []; busy = !!s.busy; model = s.model || ""; }
    render();
    loadInfo();
  }
  droidbridge.on("ready", init);
  setTimeout(init, 1500);
})();
