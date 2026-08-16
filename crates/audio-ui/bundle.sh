#!/usr/bin/env bash
# Wraps the wsctl-audio binary in an .app. The bundle is what lets the app hold
# a microphone permission of its own: a bare binary run from a terminal is
# attributed to the terminal instead, and a denied microphone reads as silence
# rather than as an error.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
OUT="${1:-$ROOT/target}"
PROFILE="${PROFILE:-release}"

APP="$OUT/wsctl-audio.app"
BUNDLE_ID=dev.pj.workstation.audio-ui
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"

cargo build --manifest-path "$ROOT/Cargo.toml" -p audio-ui --profile "$PROFILE"

case "$PROFILE" in
  dev|test) BIN="$ROOT/target/debug/wsctl-audio" ;;
  *)        BIN="$ROOT/target/$PROFILE/wsctl-audio" ;;
esac

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/wsctl-audio"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>$BUNDLE_ID</string>
	<key>CFBundleName</key>
	<string>wsctl audio</string>
	<key>CFBundleDisplayName</key>
	<string>wsctl audio</string>
	<key>CFBundleExecutable</key>
	<string>wsctl-audio</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>LSMinimumSystemVersion</key>
	<string>13.0</string>
	<key>LSUIElement</key>
	<true/>
	<key>NSMicrophoneUsageDescription</key>
	<string>wsctl reads your microphone so the audio bridge can cancel the echo of your speakers before other apps hear you.</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
PLIST

# Sign last. The signature seals Info.plist, so anything written afterwards
# invalidates it, and an invalid signature is treated far more harshly than an
# absent one. Re-signing ad hoc also changes the code identity, which is what
# TCC keys the microphone grant on, so expect to be asked again after a rebuild.
codesign --force --sign - "$APP"
codesign --verify --strict "$APP"

echo "built $APP"
