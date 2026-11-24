# 插件源

插件源让团队把插件集中发布，大家在 DroidBridge 里添加一次，就能看到、安装、更新其中的所有插件。

- **设置 → 插件** 顶栏的地球按钮打开「插件源」：添加、检查更新、删除插件源。
- 插件源里的插件直接出现在插件列表里，不分来源：没装的有「安装」，有新版本的有「更新」，卸载在插件详情页（任何插件都能卸载；本地开发的链接目录只删除链接）。
- 可以同时添加多个插件源。同一个插件 id 只显示一次：已安装的以安装它的插件源为准，没安装的取排在前面的插件源。安装它的插件源被删除后，由其他插件源接管更新。
- 安装后会像打开开关一样确认权限并启用；更新后如果申请了新权限，会再确认一次。
- 本地开发的插件（目录是链接，或者不是从插件源装的）不会被覆盖。
- 只能在运行插件的这台电脑上管理（和其他插件管理一样）。

## 插件源地址

插件源就是一个清单 `index.json` 的地址，有两种：

| 类型 | 地址示例 |
|---|---|
| http(s) | `https://gitlab.example.com/api/v4/projects/123/packages/generic/plugins/latest/index.json` |
| 本机或共享文件夹 | `D:/plugins`、`//server/share/plugins`（读其中的 `index.json`） |

清单和插件包放在哪里都可以：GitLab 通用软件包、GitHub Release 附件、内网文件服务器、对象存储……只要能用 GET 下载。

## 清单格式

```json
{
  "name": "团队插件",
  "version": "2026.11.23",
  "zip": "plugins.zip",
  "plugins": [
    { "id": "mytool", "name": "我的工具", "description": "……", "path": "mytool", "permissions": ["device.shell"],
      "version": "1.4.0+3fa2c1d9", "zip": "mytool.zip", "notes": "修好了安装失败的问题", "minHost": "0.9.4" }
  ]
}
```

| 字段 | 说明 |
|---|---|
| `name` | 插件源的名字（界面显示） |
| `version` | 发布标识（如标签名）。每个插件没写 `version` 时用它 |
| `zip` | 整个源的插件包地址，相对路径相对于清单所在位置。本机文件夹的插件源可以省略，直接从文件夹里复制 |
| `plugins[].id` | 插件 id，必须和插件目录里 `plugin.json` 的 `id` 一致 |
| `plugins[].path` | 插件在包里的目录（相对路径）。包外面多包了一层目录也能找到 |
| `plugins[].permissions` | 只用于显示，和判断自动更新时有没有新权限；实际以 `plugin.json` 为准 |

每个插件还可以有自己的一份（都可以省略）：

| 字段 | 说明 |
|---|---|
| `plugins[].version` | 这个插件自己的版本，约定是 `plugin.json` 的 `version` 加内容哈希（如 `1.4.0+3fa2c1d9`）：内容变了哈希就变，没改版本号也能更新。和已安装的不同就提示更新。界面上只显示 `+` 前面的部分；省略时用清单的 `version` |
| `plugins[].zip` | 这个插件单独的包：安装和更新只下载它。省略时用清单的 `zip` |
| `plugins[].notes` | 一句更新说明，显示在「更新」按钮的悬停提示里 |
| `plugins[].minHost` | 需要的最低 DroidBridge 版本。本机版本低于它时不能安装或更新，按钮的位置显示一个禁用的标记 |

DroidBridge 平时只取清单（打开插件设置页时、点「检查更新」时、后台每 6 小时），安装或更新时才下载插件包。

## 自动更新

插件源标题右边的下载按钮是「自动更新」（打开时变蓝）。打开后，后台每 6 小时检查一次时，会把其中「已安装、有新版本、清单里的 `permissions` 都在已授权的权限里、本机版本满足 `minHost`」的插件直接更新掉；需要新权限或本机版本不够的插件留给你手动更新。更新完界面会跟着刷新（插件列表、控制台）。

## 最低版本 minHost

`minHost`（清单里按插件写，或 `plugin.json` 里写）表示这个插件需要的最低 DroidBridge 版本，按 `.` 分段比数字（`0.10.0` 比 `0.9.9` 新）。

- `plugin.json` 里写了 `minHost`、而本机版本更低时，插件不会启动，状态是「清单有错误」，错误信息是「需要 DroidBridge 0.9.4」。
- 清单里写了 `minHost`、而本机版本更低时，插件仍然会出现在列表里，但安装 / 更新按钮的位置显示一个标记，点不了。

## 令牌

插件源需要登录才能访问时，可以给它设置一个令牌（插件源标题右边的铅笔按钮，设置后变成锁），以及令牌放在哪个请求头里：

- 默认：`Authorization: Bearer <令牌>`
- GitLab 部署令牌：请求头填 `Deploy-Token`
- GitLab 个人/项目访问令牌：请求头填 `PRIVATE-TOKEN`

令牌只保存在这台电脑上（`plugins/.sources.json`），只发给清单所在的主机；下载地址跳到其他主机时不带令牌。

## 在私有 GitLab 项目里发布

项目和组都可以保持私有，只把软件包开放下载：

1. 项目 → 设置 → 通用 → 可见性、项目功能、权限 → 软件包：打开 **Allow anyone to pull from package registry**。这样 DroidBridge 不需要令牌。
   不能打开时，建一个只读部署令牌（只勾 `read_package_registry`），在 DroidBridge 里给插件源填上，请求头填 `Deploy-Token`。
2. 每次发布（打标签）时，把插件打包并上传到通用软件包的固定版本 `latest`：
   `.../packages/generic/plugins/latest/index.json` 和 `.../plugins.zip`。同名文件重复上传时，下载拿到的是最新那份，所以地址不变。
3. 大家添加插件源时填 `https://<gitlab>/api/v4/projects/<项目 id>/packages/generic/plugins/latest/index.json`。

打包上传可以用 CI，也可以在本机跑脚本，示例：

```yaml
publish:
  image: python:3-alpine
  rules:
    - if: $CI_COMMIT_TAG
  script:
    - python tools/publish.py --version "$CI_COMMIT_TAG" --upload
```

`publish.py` 做的事：扫描每个带 `plugin.json` 的目录生成 `index.json`，把这些目录打成 `plugins.zip`，再用 `PUT $CI_API_V4_URL/projects/$CI_PROJECT_ID/packages/generic/plugins/latest/<文件>`（请求头 `JOB-TOKEN: $CI_JOB_TOKEN`）上传两个文件。

## 安装后的文件

- 插件复制到 `plugins/<id>/`，并写入 `plugins/<id>/.source.json`（来源地址、插件版本、包内路径）。
- 启用状态、权限、设置值在 `plugins/.state.json`，数据在 `plugins/.data/<id>/`，都不在插件目录里，更新和卸载都不会动它们。
- 更新会让插件目录和新版本一致（多出来的文件会删掉）；文件被插件进程占用时，先停止插件再更新，之后自动重新启动。

## 安全

- 解压时拒绝绝对路径、`..` 和链接；插件包不超过 200 MB，解压后不超过 500 MB。
- 从插件源安装的插件和手动放进来的插件权限规则相同：启用和新增权限都要用户确认。

## 接口

只接受本机请求：

| 请求 | 说明 |
|---|---|
| GET `/api/plugins/sources[?refresh=1]` | `{sources: [{url, name, version, checked, error, hasToken, header, auto, count, plugins: [名字]}], catalog: [{id, name, description, permissions, url, source, version, notes, minHost, status, installed}], updates: [id]}`；status 为 `install` / `installed` / `update` / `local` / `other` / `incompatible`（本机版本低于 `minHost`）。`refresh=1` 时先刷新 5 分钟内没检查过的源 |
| POST `/api/plugins/sources` | `{action: "add", url, token?, header?}`、`{action: "edit", url, token?, header?, auto?}`（token/header/auto 省略则不变，`""` 清除）、`{action: "remove", url}`、`{action: "refresh", url?}`、`{action: "install", url, id}`（安装或更新）、`{action: "uninstall", id}`；返回 `{ok, id?, view}`，view 同 GET |
