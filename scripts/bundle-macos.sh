#!/usr/bin/env bash
# Собрать mutator.app: окно запускается двойным щелчком или `open`, без
# терминала (`agr:gui-detach`). Использование: scripts/bundle-macos.sh [каталог]
# (по умолчанию target/release). Нужен macOS и собранный release.
set -euo pipefail

cd "$(dirname "$0")/.."
out="${1:-target/release}"
app="$out/mutator.app"
version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)

cargo build --release
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/mutator "$app/Contents/MacOS/mutator"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>mutator</string>
  <key>CFBundleDisplayName</key><string>mutator</string>
  <key>CFBundleIdentifier</key><string>ru.mutator</string>
  <key>CFBundleExecutable</key><string>mutator</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

plutil -lint "$app/Contents/Info.plist" >/dev/null
# Подпись «ad hoc»: без неё Gatekeeper на этой же машине может не запустить.
command -v codesign >/dev/null && codesign --force --sign - "$app" >/dev/null 2>&1 || true
echo "$app"
