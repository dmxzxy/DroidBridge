# 第三方组件

| 文件 | 来源 | 许可 |
|---|---|---|
| `scrcpy-server` | [Genymobile/scrcpy](https://github.com/Genymobile/scrcpy) v5.0.1 官方发布的 `scrcpy-server-v5.0.1`，未修改。SHA-256 见 `scrcpy-server.sha256` | Apache License 2.0，见 `scrcpy-LICENSE` |
| `platform-tools/`（不在仓库中） | Google Android SDK Platform-Tools，由 `tools/fetch-deps.ps1` 从 dl.google.com 下载，仅随 Windows 安装包附带 | [Android SDK 许可协议](https://developer.android.com/studio/terms) |

DroidBridge 在设备端运行未修改的 scrcpy-server，并在 `src/control.rs`、`src/scrcpy.rs` 中按 scrcpy 的公开协议实现了客户端。
