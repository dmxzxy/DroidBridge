# DroidBridge

[English](README.md)

在电脑上查看并操作安卓设备的屏幕——不只是插在本机上的，还包括插在局域网里其他电脑上的。

![DroidBridge](docs/screenshots/zh/1-desktop-dark.png)

- **投屏与操作**：基于 [scrcpy](https://github.com/Genymobile/scrcpy) 的设备端硬件编码，低延迟；支持点击、多指触控、滚轮、键盘、剪贴板、截图
- **多电脑统一管理**：每台插着手机的电脑运行一个 DroidBridge；同一局域网内的电脑会被自动发现，点一下即可添加
- **无线 adb**：扫码或配对码配对（Android 11+）、按 IP 连接，或把 USB 设备切换为无线
- **断线自动重连**：重新插拔、手机重启或 Wi-Fi 断开后自动恢复投屏；同一台手机另有 USB / 无线连接时会自动切换过去
- **声音转发**：在电脑上听到手机的声音（Android 11+）
- **SSH 一键部署**：添加 macOS / Linux 电脑时可通过 SSH 自动安装并注册为开机服务
- **设备日志**：在画面旁实时查看 logcat，可按级别、关键字、应用过滤
- **安装 APK / 传文件**：把文件拖到设备画面上即可
- **桌面版 + 浏览器访问**：Windows、macOS、Linux 桌面客户端；手机或其他电脑也可以直接用浏览器打开

| 设备日志 | 无线配对 | 手机浏览器 |
|---|---|---|
| ![](docs/screenshots/zh/8-logcat.png) | ![](docs/screenshots/zh/9-wireless.png) | ![](docs/screenshots/zh/7-mobile-view.png) |

> 截图使用的是模拟数据，手机画面为示意图（`tools/demo/shots.ps1` 生成）。

## 工作原理

```
 桌面版 / 浏览器
   │  HTTP + WebSocket（只连接当前打开的节点）
   ▼
 DroidBridge 节点 A ──USB/adb──▶ 安卓设备 1、2…
   │  中转设备列表、视频流、日志、文件
   ▼
 DroidBridge 节点 B（局域网另一台电脑） ──USB/adb──▶ 安卓设备 3、4…
```

- 节点通过 `adb` 把 scrcpy-server 推到设备上运行，设备用 MediaCodec 把屏幕编码成 H.264，经 `adb forward` 传回节点，再通过 WebSocket 转给界面，由 WebCodecs 硬件解码。
- 触摸、按键等操作按 scrcpy 控制协议发回设备注入。
- 界面只和自己打开的节点通信，远程电脑上的设备由这个节点中转，因此远程电脑只需开放 HTTP 端口。

## 下载

在 [Releases](https://github.com/dmxzxy/DroidBridge/releases) 下载最新版本，每个安装包都带有 Android platform-tools（adb）。

| 平台 | 桌面版 | 服务版 |
|---|---|---|
| Windows x64 | `DroidBridge-<版本>-windows-x64.zip` → `DroidBridge.exe`（需要 WebView2，Win10/11 一般已自带） | 同一压缩包 → `droidbridge-server.exe` |
| macOS（Apple 芯片与 Intel） | `DroidBridge-<版本>-macos-universal.zip` → `DroidBridge.app` | `droidbridge-server-<版本>-macos-universal.tar.gz` |
| Linux x86_64 | `droidbridge-desktop-<版本>-linux-x86_64.tar.gz`（需要 WebKitGTK 4.1，如 `libwebkit2gtk-4.1-0`） | `droidbridge-server-<版本>-linux-x86_64.tar.gz` |

桌面版同时也在运行服务，其他电脑可以添加它；只插手机、不看画面的电脑用服务版即可。

> **macOS**：应用未经公证。第一次打开时请右键点击选择“打开”，或运行 `xattr -cr DroidBridge.app`。
>
> **Linux**：普通用户使用 USB 设备需要安装发行版的 Android udev 规则（如 `android-sdk-platform-tools-common`）。

## 使用

1. 手机打开 **开发者选项 → USB 调试**，连接电脑后在手机上点“允许”。小米等机型还需打开 **USB 调试（安全设置）**，否则无法点击和输入。
2. 启动 DroidBridge，在左侧点击设备即可投屏。
3. 添加其他电脑：同一局域网内运行 DroidBridge 的电脑会出现在“在局域网中发现”下，点 **+** 即可添加；也可以点“远程电脑”旁的 **+** 手动填写 IP（默认端口 8730）。
   - 对方是 Windows：在对方电脑上运行 DroidBridge 即可。
   - 对方是 macOS / Linux 且未安装：打开“通过 SSH 安装”，会使用本机 `~/.ssh` 中的密钥登录，上传源码在对方编译（需要 Rust，没有时自动用 rustup 安装到当前用户），查找或用 Homebrew 安装 adb，并注册为登录启动服务（launchd / systemd --user）。
4. 如果防火墙弹窗，请允许“专用网络”，否则其他电脑无法连接本机。

### 操作方式

| 操作 | 效果 |
|---|---|
| 左键点击 / 拖动、手指触摸（多指） | 点击 / 滑动 |
| 右键、Esc | 返回 |
| 中键 | 主页 |
| 滚轮 | 滚动 |
| 聚焦画面后打字 | 输入字符；回车、退格、方向键可用 |
| Ctrl+V | 粘贴本机剪贴板到设备 |
| 右侧图标栏 | 电源、音量、旋转、截图、输入文字（中文等）、日志；“…”里是通知栏、熄屏投屏、安装 APK、重连 |
| 拖文件到画面 | APK 安装，其他文件传到 `/sdcard/Download` |

### 无线连接

点“本机”旁的 Wi-Fi 图标（远程电脑在其 “…” 菜单中选“无线连接设备…”）。手机需要与该电脑在同一网络。

- **扫码配对**（Android 11+）：手机打开 开发者选项 → 无线调试 → 使用二维码配对设备，扫码即可
- **配对码**（Android 11+）：填写手机上显示的配对地址、端口和配对码
- **IP 地址**：已配对过、或执行过 `adb tcpip` 的设备
- USB 设备可在 “…” 菜单选“切换为无线连接”，自动执行 `adb tcpip 5555` 并连接，之后可以拔掉数据线

无线地址会被记住并在后台自动重连；通过 mDNS 公布的已配对设备也会自动重连。

### 声音

点右侧图标栏的耳机图标，或在 **设置 → 声音** 中打开“默认转发声音”。需要 Android 11+（Android 11 开始投屏时需解锁屏幕）。声音使用 Opus 编码，浏览器不支持 Opus 解码时改用原始 PCM。“同时在手机上播放”需要 Android 13+。

### 设备日志

点图标栏的 **日志** 打开 logcat 面板。窗口够宽时在画面右侧，否则在下方。

- 级别过滤；关键字过滤（空格分隔为“且”，`-词` 表示排除）
- 填写包名或点 ⊙ 选择当前前台应用，只看该应用的日志（应用重启后仍会跟踪）
- 暂停、清空、保存为文本文件

### 用浏览器访问

浏览器的 WebCodecs 只在安全上下文（`https://` 或 `localhost`）中可用：

- 本机：`http://localhost:8730`
- 其他设备：`https://<电脑IP>:8731`（首次启动时生成的自签名证书，第一次访问需选择“继续访问”）

### 语言

界面跟随系统语言（中文或英文）。可在 **设置 → 语言** 中切换，或启动时加 `--lang zh` / `--lang en`。

## 安全须知

DroidBridge 能完全控制连接的设备，默认**不需要认证**，请只在可信的局域网中使用：

- 用 `--token <令牌>` 启用访问令牌（网页、远程节点访问都需要），或 `--bind 127.0.0.1` 只允许本机访问。
- 不要把端口暴露到公网。
- 局域网自动发现会在 UDP 8730 上广播本机名称和 HTTP 端口；可用 `--no-discovery` 关闭（`--bind 127.0.0.1` 时也会关闭）。
- 远程电脑的令牌以明文保存在数据目录的 `peers.json` 中。

## 命令行参数

```
--port <端口>         HTTP 端口（默认 8730）
--https-port <端口>   HTTPS 端口（默认 8731，0 表示关闭）
--bind <地址>         监听地址（默认 0.0.0.0）
--token <令牌>        访问令牌（也可用环境变量 DROIDBRIDGE_TOKEN）
--name <名称>         本机显示名称（默认计算机名）
--adb <路径>          adb 路径（默认依次查找：随附的 platform-tools、ANDROID_HOME、常见安装位置、PATH）
--data <目录>         数据目录（见下）
--lang <zh|en>        界面与提示语言（默认跟随系统，也可用 DROIDBRIDGE_LANG）
--no-discovery        不参与局域网自动发现
```

默认数据目录（远程电脑列表、无线设备地址、证书、日志）：Windows 为程序目录下的 `data\`，macOS 为 `~/Library/Application Support/DroidBridge`，Linux 为 `~/.local/share/droidbridge`。

## 从源码编译

需要 Rust 1.85+。Linux 编译桌面版还需要 `libwebkit2gtk-4.1-dev`、`libgtk-3-dev`。

```sh
cargo run                                               # 服务版
cargo run --features desktop --bin droidbridge-desktop  # 桌面版
cargo test
```

打包：

```sh
powershell -File build.ps1 -Version 0.2.0   # Windows（会下载 platform-tools）
sh tools/package-mac.sh 0.2.0               # macOS
sh tools/package-linux.sh 0.2.0             # Linux
```

发布由 GitHub Actions 完成：推送 `v*` 标签后自动构建各平台安装包，并创建 Release 草稿。

目录结构、接口说明见 [英文 README](README.md#layout)。

## 计划

- 录屏
- 一个 APK 装到多台设备、多设备同步操作
- 崩溃 / ANR 捕获、性能浮层
- iOS 设备（基于 WebDriverAgent）

## 致谢

- [scrcpy](https://github.com/Genymobile/scrcpy)（Apache-2.0）提供设备端的投屏与控制服务
- [tao](https://github.com/tauri-apps/tao) / [wry](https://github.com/tauri-apps/wry)、[axum](https://github.com/tokio-rs/axum)、[tokio](https://tokio.rs)

## 许可

DroidBridge 以 [MIT](LICENSE) 许可发布。随附的 scrcpy-server 遵循 Apache-2.0，见 [third_party/README.md](third_party/README.md)。
