#!/bin/sh
# 打包 Linux：桌面版与服务版，输出到 dist/
#   tools/package-linux.sh <版本号> [架构名，默认 x86_64]
set -eu
VERSION=${1:?version}
ARCH=${2:-x86_64}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
OUT=dist
TMP=$(mktemp -d)
mkdir -p "$OUT"

echo "Downloading platform-tools (Linux)…"
curl -fsSL -o "$TMP/pt.zip" https://dl.google.com/android/repository/platform-tools-latest-linux.zip
unzip -q "$TMP/pt.zip" -d "$TMP"
cargo run -q --release --example icons -- "$TMP/icons"

pack() {
  name=$1; shift
  dir="$TMP/$name"
  mkdir -p "$dir"
  for f in "$@"; do cp "$f" "$dir/"; done
  cp -R "$TMP/platform-tools" "$dir/platform-tools"
  cp README.md LICENSE "$dir/"
  cp third_party/scrcpy-LICENSE "$dir/scrcpy-LICENSE.txt"
  tar -C "$TMP" -czf "$OUT/$name.tar.gz" "$name"
}

pack "droidbridge-server-$VERSION-linux-$ARCH" target/release/droidbridge
if [ -f target/release/droidbridge-desktop ]; then
  cp "$TMP/icons/icon.png" "$TMP/droidbridge.png"
  cat > "$TMP/droidbridge.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=DroidBridge
Comment=Mirror and control Android devices
Exec=droidbridge-desktop
Icon=droidbridge
Categories=Development;Utility;
EOF
  pack "droidbridge-desktop-$VERSION-linux-$ARCH" target/release/droidbridge-desktop target/release/droidbridge \
    "$TMP/droidbridge.png" "$TMP/droidbridge.desktop"
fi

rm -rf "$TMP"
ls -la "$OUT"
