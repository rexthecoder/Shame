#!/usr/bin/env bash
# Build Shame for macOS: an optimized .app bundle and a distributable .dmg.
#
#   scripts/build-mac.sh                       # host architecture
#   scripts/build-mac.sh --arch arm64          # Apple Silicon
#   scripts/build-mac.sh --arch x86_64         # Intel
#   scripts/build-mac.sh --arch universal      # one binary for both (needs both targets)
#   scripts/build-mac.sh --no-dmg              # only produce dist/Shame.app
#
# Output (in dist/):
#   Shame.app
#   Shame-<version>-macos-<arch>.dmg
#   Shame-<version>-macos-<arch>.dmg.sha256
#
# Signing: ad-hoc by default, which runs locally but triggers Gatekeeper warnings
# on other machines. For a public release, sign with a Developer ID certificate:
#   CODESIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" scripts/build-mac.sh
# Notarization is not handled here; run `xcrun notarytool` on the resulting .dmg.
#
# The app name, bundle id, binary name and version come from the Makefile so the
# two build paths cannot drift apart.

set -euo pipefail

cd "$(dirname "$0")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: macOS builds can only be produced on macOS" >&2
  exit 1
fi

make_var() {
  awk -v k="$1" '$1 == k && $2 == ":=" { print $3; exit }' Makefile
}

APP_NAME="$(make_var APP_NAME)"
BUNDLE_ID="$(make_var BUNDLE_ID)"
BIN="$(make_var BIN)"
VERSION="$(make_var VERSION)"
SHORT_VERSION="${VERSION%%+*}"
BUILD_NUMBER="${VERSION#*+}"
if [[ "$BUILD_NUMBER" == "$VERSION" ]]; then BUILD_NUMBER=1; fi
CRATE="$(make_var CRATE)"
DIST_DIR="dist"

for v in APP_NAME BUNDLE_ID BIN VERSION CRATE; do
  if [[ -z "${!v}" ]]; then
    echo "error: could not read $v from the Makefile" >&2
    exit 1
  fi
done

HOST_ARCH="$(uname -m)"
ARCH="$HOST_ARCH"
MAKE_DMG=1
IDENTITY="${CODESIGN_IDENTITY:--}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --arch)
      ARCH="${2:?--arch needs a value: arm64, x86_64 or universal}"
      shift 2
      ;;
    --no-dmg)
      MAKE_DMG=0
      shift
      ;;
    -h | --help)
      sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1 (try --help)" >&2
      exit 1
      ;;
  esac
done

triple_for() {
  case "$1" in
    arm64) echo "aarch64-apple-darwin" ;;
    x86_64) echo "x86_64-apple-darwin" ;;
    *)
      echo "error: unsupported arch '$1' (use arm64, x86_64 or universal)" >&2
      exit 1
      ;;
  esac
}

case "$ARCH" in
  universal) ARCHS=(arm64 x86_64) ;;
  arm64 | x86_64) ARCHS=("$ARCH") ;;
  *)
    echo "error: unsupported arch '$ARCH' (use arm64, x86_64 or universal)" >&2
    exit 1
    ;;
esac

# Build one architecture; binaries land in target/<triple>/release.
build_arch() {
  local triple
  triple="$(triple_for "$1")"
  if ! rustup target list --installed 2>/dev/null | grep -qx "$triple"; then
    echo "==> Installing Rust target $triple"
    rustup target add "$triple"
  fi
  echo "==> Building $CRATE for $triple (release)"
  cargo build --release -p "$CRATE" --target "$triple"
}

# The iOS simulator helper is compiled by the app's build script; pick the newest.
helper_for() {
  # shellcheck disable=SC2012
  ls -t "target/$(triple_for "$1")/release/build/"*/out/shame-sim 2>/dev/null | head -1 || true
}

for a in "${ARCHS[@]}"; do
  build_arch "$a"
done

APP_DIR="$DIST_DIR/$APP_NAME.app"
echo "==> Assembling $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"

if [[ ${#ARCHS[@]} -eq 1 ]]; then
  a="${ARCHS[0]}"
  cp "target/$(triple_for "$a")/release/$BIN" "$APP_DIR/Contents/MacOS/$BIN"
  helper="$(helper_for "$a")"
  if [[ -n "$helper" ]]; then cp "$helper" "$APP_DIR/Contents/MacOS/shame-sim"; fi
else
  lipo -create \
    "target/$(triple_for arm64)/release/$BIN" \
    "target/$(triple_for x86_64)/release/$BIN" \
    -output "$APP_DIR/Contents/MacOS/$BIN"
  arm_helper="$(helper_for arm64)"
  x86_helper="$(helper_for x86_64)"
  if [[ -n "$arm_helper" && -n "$x86_helper" ]]; then
    lipo -create "$arm_helper" "$x86_helper" -output "$APP_DIR/Contents/MacOS/shame-sim"
  fi
fi

cp assets/icon.icns "$APP_DIR/Contents/Resources/icon.icns"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>$APP_NAME</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleVersion</key><string>$BUILD_NUMBER</string>
  <key>CFBundleShortVersionString</key><string>$SHORT_VERSION</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleExecutable</key><string>$BIN</string>
  <key>CFBundleIconFile</key><string>icon.icns</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

if [[ "$IDENTITY" == "-" ]]; then
  echo "==> Signing ad-hoc (set CODESIGN_IDENTITY for a distributable build)"
  codesign --force --deep --sign - "$APP_DIR"
else
  echo "==> Signing with: $IDENTITY"
  codesign --force --deep --options runtime --timestamp --sign "$IDENTITY" "$APP_DIR"
fi
codesign --verify --deep --strict "$APP_DIR"
echo "✓ Packaged $APP_DIR ($(lipo -archs "$APP_DIR/Contents/MacOS/$BIN"))"

if [[ "$MAKE_DMG" -eq 1 ]]; then
  DMG_PATH="$DIST_DIR/$APP_NAME-$VERSION-macos-$ARCH.dmg"
  echo "==> Creating $DMG_PATH"
  rm -f "$DMG_PATH" "$DMG_PATH.sha256"
  STAGING="$(mktemp -d)"
  trap 'rm -rf "$STAGING"' EXIT
  cp -R "$APP_DIR" "$STAGING/"
  ln -s /Applications "$STAGING/Applications"
  hdiutil create -volname "$APP_NAME $VERSION" -srcfolder "$STAGING" -ov -format UDZO "$DMG_PATH" > /dev/null
  if [[ "$IDENTITY" != "-" ]]; then
    codesign --force --timestamp --sign "$IDENTITY" "$DMG_PATH"
  fi
  (cd "$DIST_DIR" && shasum -a 256 "$(basename "$DMG_PATH")" > "$(basename "$DMG_PATH").sha256")
  echo "✓ Created $DMG_PATH"
  ls -lh "$DMG_PATH"
fi
