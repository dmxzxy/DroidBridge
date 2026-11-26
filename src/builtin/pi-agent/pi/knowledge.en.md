# DroidBridge features (answer usage questions only from this)

- Screen and control: click a device in the left list to show its screen. Left click to tap or drag, right click or Esc for Back, middle click for Home, wheel to scroll; with the screen focused you can type, and Ctrl+V pastes the computer clipboard.
- Right-hand icon bar: power, volume, rotate, screenshot (camera), screen recording, type text, Logs, Performance (pulse icon), folder, headphones (audio); "…" holds notifications, screen-off mirroring, install APK, reconnect, Apps, Files and "Switch to wireless".
- Connecting: on the phone turn on Developer options → USB debugging, plug it in and tap "Allow" on the phone; Xiaomi and some others also need "USB debugging (Security settings)" for taps and typing. "How to connect" under "This computer" in the sidebar explains it.
- Wireless: click the Wi-Fi icon next to "This computer" (for a remote computer, choose "Connect device wirelessly…" in its "…" menu) to pair by QR code, by pairing code (Android 11+) or by IP; a USB device can use "Switch to wireless" in "…". Phone and computer must be on the same network; the address is remembered and reconnected.
- Remote computers: other computers running DroidBridge on the LAN appear under "Found on your network"; click + to add one and the other side clicks "Allow". You can also enter an IP (default port 8730). macOS/Linux can be installed over SSH.
- Logs: "Logs" in the icon bar opens logcat; filter by level, keyword (space means AND, -word excludes) and app, or ⊙ for only the foreground app; pause, clear and save. Right-click a log line and choose "Ask AI" to have the AI assistant explain it.
- Performance overlay: the pulse icon shows the foreground app's frame rate, CPU, memory, temperature and battery, refreshed every second.
- Apps: "…" → Apps can launch, force stop, clear data and uninstall; right-click an app for "AI Diagnose". On OPPO/OnePlus/realme, clearing data needs "Disable permission monitoring" in Developer options.
- Files: the folder icon or "…" → Files, limited to /sdcard; upload (drag in), download, rename, delete, install APKs.
- Installing APKs / sending files: drag a file onto the screen; APKs install, other files go to /sdcard/Download. To install on several devices, use the package icon at the top of the sidebar.
- Screenshots, recordings and Gallery: screenshots are PNG, recordings WebM/MP4; browse, copy and download them in the Gallery.
- Sync input: with two or more screens open, click the link icon in a device's title bar to join; input on one device is repeated on the others.
- Audio: the headphones icon or Settings → Audio, Android 11+.
- Shared viewing: several people watching one device share one stream; only one can control it at a time, released after 3 seconds idle.
- Plugins: Settings → Plugins to enable, configure, install and update from plugin sources.
- Browser access: http://localhost:8730 on this computer, https://<computer IP>:8731 from other devices.
- Updates: click "Update" in the title bar when it appears, or Settings → Version → Check for updates.
