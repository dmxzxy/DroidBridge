# Third-party components / 第三方组件

| File | Source | License |
|---|---|---|
| `scrcpy-server` | [Genymobile/scrcpy](https://github.com/Genymobile/scrcpy) v5.0.1 official release `scrcpy-server-v5.0.1`, unmodified. SHA-256 in `scrcpy-server.sha256` | Apache License 2.0, see `scrcpy-LICENSE` |
| `platform-tools/` (not in the repository) | Google Android SDK Platform-Tools, downloaded from dl.google.com by `tools/fetch-deps.ps1` / `tools/package-*.sh` and bundled with release packages | [Android SDK License](https://developer.android.com/studio/terms) |

DroidBridge runs the unmodified scrcpy-server on the device and implements a client for scrcpy's public protocol in `src/control.rs` and `src/scrcpy.rs`.

DroidBridge 在设备端运行未修改的 scrcpy-server，并在 `src/control.rs`、`src/scrcpy.rs` 中按 scrcpy 的公开协议实现了客户端。
