#!/bin/sh
# 打包 macOS：DroidBridge.app（桌面版）与服务版，输出到 dist/
#   tools/package-mac.sh <版本号> [二进制目录]
# 二进制目录中应有 droidbridge 与 droidbridge-desktop（默认 target/release）；
# CI 中为 lipo 合并后的通用二进制（Apple Silicon + Intel）。
set -eu
VERSION=${1:?version}
BIN=${2:-target/release}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
OUT=dist
TMP=$(mktemp -d)
mkdir -p "$OUT"

echo "Downloading platform-tools (macOS)…"
curl -fsSL -o "$TMP/pt.zip" https://dl.google.com/android/repository/platform-tools-latest-darwin.zip
unzip -q "$TMP/pt.zip" -d "$TMP"

cargo run -q --release --example icons -- "$TMP/icons"

# ---- DroidBridge.app ----
APP="$OUT/DroidBridge.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN/droidbridge-desktop" "$APP/Contents/MacOS/DroidBridge"
cp "$TMP/icons/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
cp -R "$TMP/platform-tools" "$APP/Contents/Resources/platform-tools"
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>DroidBridge</string>
  <key>CFBundleDisplayName</key><string>DroidBridge</string>
  <key>CFBundleIdentifier</key><string>io.github.dmxzxy.droidbridge</string>
  <key>CFBundleExecutable</key><string>DroidBridge</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSLocalNetworkUsageDescription</key><string>DroidBridge connects to other computers on your local network to show their Android devices.</string>
</dict>
</plist>
EOF
# 未签名：使用临时签名，便于在 Apple Silicon 上运行
codesign --force --deep --sign - "$APP" 2>/dev/null || true
ditto -c -k --keepParent "$APP" "$OUT/DroidBridge-$VERSION-macos-universal.zip"

# ---- 服务版 ----
SRV="$TMP/droidbridge-server-$VERSION-macos-universal"
mkdir -p "$SRV"
cp "$BIN/droidbridge" "$SRV/droidbridge"
cp -R "$TMP/platform-tools" "$SRV/platform-tools"
cp README.md LICENSE "$SRV/"
cp third_party/scrcpy-LICENSE "$SRV/scrcpy-LICENSE.txt"
tar -C "$TMP" -czf "$OUT/droidbridge-server-$VERSION-macos-universal.tar.gz" "$(basename "$SRV")"

rm -rf "$TMP"
ls -la "$OUT"
