# 发布新版本

每次发布都按这里的步骤和格式来，保证版本号、提交、标签和 GitHub Release 页面风格一致。

## 1. 发布前

1. **确认要发的内容**：`git status` 看一遍工作区。不是这次要发的改动（别人的、做到一半的）先问清楚，不要顺手带进去。
2. **改版本号**：`Cargo.toml` 的 `version`，然后 `cargo build` 让 `Cargo.lock` 跟着更新。版本规则：
   - 修复和小改进：第三位加一（0.9.2 → 0.9.3）
   - 较大的新功能或行为变化：第二位加一（0.9.3 → 0.10.0）
3. **同步文档**：用户能看到的新功能写进 `README.md` 和 `README.zh-CN.md`（两份都要改）；插件接口的变化写进 `docs/plugins/README.md`、`SPEC.md`、`droidbridge.d.lua`。
4. **跑全部测试**，都要通过：

   ```bash
   cargo test --release --features desktop
   node tools/check-ui.js
   # 界面测试需要 Chrome / Edge，用 BROWSER 指定路径
   node tools/test-plugins.js
   node tools/test-viewers.js
   node tools/test-contextmenu.js
   node tools/test-files.js
   node tools/test-input.js
   node tools/test-fullscreen.js
   node tools/test-theme.js
   ```

   偶发失败的测试要修好（或确认原来的代码也同样失败并顺手修掉），不要带着红灯发布。
5. **检查公开仓库里没有内部信息**：这是公开仓库，提交前看一遍暂存的改动里有没有公司名称、内部域名、内网 IP、内部项目或插件名、个人账号：

   ```bash
   git diff --cached | grep -i -E "<公司/内部关键字>"
   ```

   示例和测试里用通用写法（`example.com`、`mytool`、`tool:x`）。

## 2. 提交和标签

一次发布一个提交，提交信息就是发布说明的英文版（第一行之后的内容会原样出现在 GitHub 的提交记录里）：

```
v0.9.3: <一句话概要，英文，逗号分隔几个要点>

<分类标题>
- <面向开发者的说明，可以写接口名>

<分类标题>
- ...
```

然后打标签并推送：

```bash
git tag v0.9.3
git push origin main
git push origin v0.9.3
```

推送 `v*` 标签会触发 `.github/workflows/release.yml`：构建各平台安装包，并创建 **Release 草稿**（7 个附件：Windows 安装包和 zip、macOS、Linux 桌面版、两个服务端包、`SHA256SUMS.txt`）。在 Actions 页面确认 **Release** 和 **CI** 都成功。

## 3. Release 页面的标题和说明

GitHub 自动生成的草稿只有一行 Full Changelog，**必须按下面的格式改写后再发布**。

**标题**：`DroidBridge 0.9.3`（不是 `v0.9.3`）

**说明**：

```markdown
v0.9.3: <一句话概要，和提交信息第一行相同>

<English section> / <中文分类>
- <English sentence written for users.>
  <对应的中文，一句话说清楚用户能得到什么。>
- ...

<English section> / <中文分类>
- ...

**Full Changelog**: https://github.com/dmxzxy/DroidBridge/compare/v<上一版>...v<这一版>
```

写法要求：

- **第一行**是版本号加一句概要，和提交信息第一行一致。
- **分类标题**用「英文 / 中文」，例如 `Plugins / 插件`、`Desktop / 桌面版`、`Fixes / 修复`、`Gallery / 相册`、`Other / 其他`。按这次实际有的内容分类，把最重要的放前面，`Fixes` 和 `Other` 放最后。
- **每一条先英文、后中文**：英文一行，下一行缩进两个空格写中文。两种语言说的是同一件事，不是逐词翻译。
- **写给使用者看**：说用户能做什么、看到什么变化。不写函数名、接口名、文件名、内部实现，例如写“插件可以把自己工具的网页嵌在投屏旁边”，不写 `db.webpanel({url})`。必要的界面文字用引号或「」标出，如「设置 → 插件」。
- **兼容和前提要写清楚**：比如“远程电脑需先更新到 0.9.x”“需要 HTTPS”“Linux 需要安装 xxx”。
- **测试、重构这类用户感知不到的改动**最多在 `Other / 其他` 里提一句，或者不写。
- **最后一行**是 Full Changelog 链接，比较上一版和这一版的标签。

可参考的范例：[0.9.1](https://github.com/dmxzxy/DroidBridge/releases/tag/v0.9.1)、[0.9.2](https://github.com/dmxzxy/DroidBridge/releases/tag/v0.9.2)、[0.9.3](https://github.com/dmxzxy/DroidBridge/releases/tag/v0.9.3)。

## 4. 改草稿并发布

可以在 GitHub 网页上编辑草稿，也可以用 API（需要有仓库写权限的令牌）：

```bash
# 改标题和说明（notes.md 是按第 3 节写好的说明）
curl -X PATCH -H "Authorization: Bearer $TOKEN" -H "Accept: application/vnd.github+json" \
  https://api.github.com/repos/dmxzxy/DroidBridge/releases/<release id> \
  -d "$(jq -n --rawfile body notes.md '{name: "DroidBridge 0.9.3", tag_name: "v0.9.3", body: $body}')"

# 确认无误后发布，并设为最新版本
curl -X PATCH ... -d '{"draft": false, "make_latest": "true", "tag_name": "v0.9.3"}'
```

> **注意**：通过 API 修改草稿时**每次都要带上 `tag_name`**。不带的话草稿会和标签脱钩，变成 `untagged-…`，发布出去就不对了。改完用 GET 再看一眼 `tag_name` 是不是 `v0.9.3`。

## 5. 发布后检查

1. `https://github.com/dmxzxy/DroidBridge/releases/latest` 跳转到新版本。
2. 标题、说明、7 个附件都正确。
3. 打开一个旧版本的桌面版，**设置 → 版本 → 检查更新**，能看到新版本（应用读的就是上面那个 latest 地址）。
