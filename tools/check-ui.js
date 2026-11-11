#!/usr/bin/env node
// 前端自检：所有脚本语法、i18n 键唯一、t() 用到的中文都有英文对照、index.html 引用的文件都存在。
// 用法：node tools/check-ui.js
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');

const root = path.resolve(__dirname, '..');
const web = path.join(root, 'web');
const read = (p) => fs.readFileSync(p, 'utf8');
let failed = 0;
const fail = (msg) => { console.error('✗ ' + msg); failed++; };

// 1. index.html 引用的资源都存在，且都被服务端嵌入（src/lib.rs 的 ASSETS）
const html = read(path.join(web, 'index.html'));
const refs = [...html.matchAll(/(?:src|href)="([^"]+)"/g)].map(m => m[1]);
const lib = read(path.join(root, 'src/lib.rs'));
for (const r of refs) {
  const file = path.join(web, r);
  if (!fs.existsSync(file)) fail(`index.html 引用了不存在的文件：${r}`);
  if (!lib.includes(`include_str!("../web/${r}")`)) fail(`src/lib.rs 没有嵌入 ${r}`);
  if (!read(path.join(root, 'src/remote.rs')).includes(`src!("web/${r}")`)) fail(`src/remote.rs 的源码清单缺少 web/${r}`);
}

// 2. 每个脚本都能通过 node --check
const scripts = fs.readdirSync(path.join(web, 'js')).filter(f => f.endsWith('.js'));
for (const f of scripts) {
  try {
    execFileSync(process.execPath, ['--check', path.join(web, 'js', f)], { stdio: 'pipe' });
  } catch (e) {
    fail(`语法错误：web/js/${f}\n${e.stderr}`);
  }
}

// 3. 脚本里定义过的全局名字（简单检查：顶层 function/class/const/let），用于发现重复定义
const defined = new Map();
for (const f of scripts) {
  const src = read(path.join(web, 'js', f));
  for (const m of src.matchAll(/^(?:async )?function ([A-Za-z_$][\w$]*)\(|^class ([A-Za-z_$][\w$]*)|^(?:const|let) ([A-Za-z_$][\w$]*)\s*=/gm)) {
    const name = m[1] || m[2] || m[3];
    if (defined.has(name)) fail(`重复定义 ${name}（web/js/${defined.get(name)}.js 与 web/js/${f}）`);
    else defined.set(name, f);
  }
}

// 4. i18n：键唯一；t() 用到的中文都有对照
const i18n = read(path.join(web, 'js/i18n.js'));
const start = i18n.indexOf('const EN = {');
// 结尾的 }; 必须在行首（翻译文本里可能出现 };
const endMatch = /\n\};/.exec(i18n.slice(start));
if (!endMatch) { console.error('找不到 EN 对象的结尾'); process.exit(1); }
const dict = i18n.slice(start, start + endMatch.index + 3);
const keys = [...dict.matchAll(/(?:^\s*|[{,])"((?:[^"\\]|\\.)*)":/gm)].map(m => m[1]);
const seen = new Map();
for (const k of keys) {
  if (seen.has(k)) fail(`i18n 键重复：${k}`);
  seen.set(k, true);
}
const used = new Set();
const scan = (src) => { for (const m of src.matchAll(/\bt\('((?:[^'\\]|\\.)*)'/g)) used.add(m[1]); };
scripts.forEach(f => scan(read(path.join(web, 'js', f))));
scan(html);
for (const k of used) if (/[\u4e00-\u9fff]/.test(k) && !seen.has(k)) fail(`缺少英文对照：${k}`);

if (failed) {
  console.error(`\n${failed} 个问题`);
  process.exit(1);
}
console.log(`✓ ${scripts.length} 个脚本、${refs.length} 个资源引用、${keys.length} 条翻译，检查通过`);
