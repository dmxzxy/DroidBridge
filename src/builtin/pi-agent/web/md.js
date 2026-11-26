// 轻量 Markdown 渲染：标题、段落、有序/无序列表、代码块、表格、引用、行内代码/粗体/斜体/链接
(() => {
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

  function inline(s) {
    const codes = [];
    s = s.replace(/`([^`]+)`/g, (_, c) => `\u0000${codes.push(c) - 1}\u0000`);
    s = esc(s)
      .replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>")
      .replace(/(^|[^*])\*([^*\s][^*]*)\*/g, "$1<i>$2</i>")
      .replace(/\[([^\]]+)\]\((https?:[^)\s]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>');
    return s.replace(/\u0000(\d+)\u0000/g, (_, i) => `<code>${esc(codes[i])}</code>`);
  }

  const isRow = (l) => /^\s*\|.*\|\s*$/.test(l);
  const isSep = (l) => /^\s*\|[\s:|-]+\|\s*$/.test(l);
  const cells = (r) => r.trim().slice(1, -1).split("|").map((c) => c.trim());

  function render(src) {
    const lines = src.replace(/\r/g, "").split("\n");
    const out = [];
    let para = [], list = null;
    const flushPara = () => { if (para.length) { out.push(`<p>${para.map(inline).join("<br>")}</p>`); para = []; } };
    const flushList = () => {
      if (!list) return;
      const start = list.tag === "ol" && list.start !== 1 ? ` start="${list.start}"` : "";
      out.push(`<${list.tag}${start}>${list.items.map((x) => `<li>${x}</li>`).join("")}</${list.tag}>`);
      list = null;
    };
    for (let i = 0; i < lines.length; i++) {
      const l = lines[i];
      const fence = l.match(/^\s*(```+|~~~+)\s*([\w+-]*)/);
      if (fence) {
        flushPara();
        const code = [];
        const indent = l.match(/^\s*/)[0].length;
        while (++i < lines.length && !lines[i].trim().startsWith(fence[1])) code.push(lines[i].slice(Math.min(indent, lines[i].match(/^\s*/)[0].length)));
        const html = `<div class="code"><button type="button" class="copy" title="${window.droidbridge && droidbridge.lang === "en" ? "Copy" : "复制"}"></button><pre><code>${esc(code.join("\n"))}</code></pre></div>`;
        if (list && indent > 0) list.items[list.items.length - 1] += html;
        else { flushList(); out.push(html); }
        continue;
      }
      if (isRow(l) && i + 1 < lines.length && isSep(lines[i + 1])) {
        flushPara(); flushList();
        const head = cells(l);
        const rows = [];
        for (i += 2; i < lines.length && isRow(lines[i]); i++) rows.push(cells(lines[i]));
        i--;
        out.push(`<div class="tbl"><table><tr>${head.map((c) => `<th>${inline(c)}</th>`).join("")}</tr>${rows.map((r) => `<tr>${r.map((c) => `<td>${inline(c)}</td>`).join("")}</tr>`).join("")}</table></div>`);
        continue;
      }
      const h = l.match(/^(#{1,6})\s+(.*)/);
      if (h) { flushPara(); flushList(); out.push(`<h4>${inline(h[2])}</h4>`); continue; }
      if (/^\s*>/.test(l)) {
        flushPara(); flushList();
        const q = [];
        for (; i < lines.length && /^\s*>/.test(lines[i]); i++) q.push(lines[i].replace(/^\s*>\s?/, ""));
        i--;
        out.push(`<blockquote>${render(q.join("\n"))}</blockquote>`);
        continue;
      }
      if (/^\s*([-*_])\s*\1\s*\1[\s\1]*$/.test(l)) { flushPara(); flushList(); out.push("<hr>"); continue; }
      const li = l.match(/^(\s*)([-*+]|(\d+)[.)])\s+(.*)/);
      if (li) {
        flushPara();
        const tag = li[3] ? "ol" : "ul";
        if (!list || (list.tag !== tag && li[1].length === 0)) { flushList(); list = { tag, start: li[3] ? +li[3] : 1, items: [] }; }
        if (li[1].length >= 2 && list.items.length) list.items[list.items.length - 1] += `<br>· ${inline(li[4])}`;
        else list.items.push(inline(li[4]));
        continue;
      }
      if (!l.trim()) {
        flushPara();
        // 列表项之间的空行不打断列表
        if (list) {
          let j = i + 1;
          while (j < lines.length && !lines[j].trim()) j++;
          if (j < lines.length && (/^(\s*)([-*+]|\d+[.)])\s+/.test(lines[j]) || /^\s{2,}\S/.test(lines[j]))) continue;
        }
        flushList();
        continue;
      }
      if (list && /^\s{2,}\S/.test(l)) { list.items[list.items.length - 1] += `<br>${inline(l.trim())}`; continue; }
      flushList();
      para.push(l);
    }
    flushPara(); flushList();
    return out.join("");
  }

  window.md = { render, esc };
})();
