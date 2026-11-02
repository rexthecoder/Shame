# Dart/Flutter IDE — development & release tasks.
#
#   make run             foreground debug run (logs in this terminal)
#   make restart         rebuild debug, stop old window, relaunch in background
#   make test            run workspace test suite
#   make check           clippy across workspace
#   make icon            generate assets/icon.png + assets/icon.icns from assets/icon.svg
#   make release-build   optimized LTO release binary (target/release/dart-ide)
#   make bundle          package macOS app bundle (dist/Dart IDE.app) with icon.icns
#   make dmg             create distributable macOS disk image (dist/Dart-IDE-0.1.0-macos.dmg)
#   make release         full v0.1.0 release pipeline (test + release-build + bundle + dmg)
#   make install         install dist/Dart IDE.app into /Applications
#   make dev-app         debug build wrapped as target/debug/Shame Dev.app and
#                        opened on this repo (own bundle id, so it can be
#                        screenshotted and driven like any app);
#                        DEV_COMMAND="go-to-file, type:main" runs keymap
#                        commands at startup to reach a state to look at

CRATE     := app
BIN       := dart-ide
APP_NAME  := Shame
BUNDLE_ID := dev.shame.ide
VERSION   := 0.1.0
DIST_DIR  := dist
APP_DIR   := $(DIST_DIR)/$(APP_NAME).app
DMG_PATH  := $(DIST_DIR)/$(APP_NAME)-$(VERSION)-macos.dmg
LOG       := /tmp/shame.log

.PHONY: build run restart stop test check fmt clean icon release-build bundle dmg release install dev-app

build:
	cargo build -p $(CRATE)

# Foreground: Ctrl-C ends the app and shows its output here.
run:
	cargo run -p $(CRATE)

# Background: build, replace the running instance, bring up a fresh window.
restart:
	cargo build -p $(CRATE)
	-pkill -f "target/debug/$(BIN)"
	nohup cargo run -p $(CRATE) > $(LOG) 2>&1 &
	@sleep 1
	@pgrep -fl "target/debug/$(BIN)" || echo "not running — see $(LOG)"

stop:
	-pkill -f "target/debug/$(BIN)"

DEV_APP := target/debug/$(APP_NAME) Dev.app
DEV_DIR ?= $(CURDIR)

# A debug build as a real .app with its own bundle id: it shows up as an
# app of its own (not the installed Shame), so tools that work per
# application, like screenshots, can find it. Opens DEV_DIR (default: here).
dev-app: build
	-pkill -f "$(APP_NAME) Dev.app/Contents/MacOS/$(BIN)"
	@mkdir -p "$(DEV_APP)/Contents/MacOS"
	@cp "target/debug/$(BIN)" "$(DEV_APP)/Contents/MacOS/$(BIN)"
	@HELPER=$$(ls -t target/debug/build/*/out/shame-sim 2>/dev/null | head -1); \
		if [ -n "$$HELPER" ]; then cp "$$HELPER" "$(DEV_APP)/Contents/MacOS/shame-sim"; fi
	@printf '%s\n' \
		'<?xml version="1.0" encoding="UTF-8"?>' \
		'<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
		'<plist version="1.0">' \
		'<dict>' \
		'  <key>CFBundleName</key><string>$(APP_NAME) Dev</string>' \
		'  <key>CFBundleIdentifier</key><string>$(BUNDLE_ID).dev</string>' \
		'  <key>CFBundlePackageType</key><string>APPL</string>' \
		'  <key>CFBundleExecutable</key><string>$(BIN)</string>' \
		'  <key>NSHighResolutionCapable</key><true/>' \
		'</dict>' \
		'</plist>' > "$(DEV_APP)/Contents/Info.plist"
	@codesign --force --sign - "$(DEV_APP)" 2>/dev/null || true
	@open -n "$(DEV_APP)" $(if $(DEV_COMMAND),--env SHAME_DEV_COMMAND="$(DEV_COMMAND)") --args "$(DEV_DIR)"
	@echo "✓ Opened $(DEV_APP) on $(DEV_DIR)"

test:
	cargo test --workspace

check:
	cargo clippy --workspace --all-targets

fmt:
	cargo fmt --all

clean:
	cargo clean
	rm -rf "$(DIST_DIR)"

# Generate 1024x1024 PNG and multi-resolution macOS .icns from assets/icon.svg
icon:
	@swift -e 'import AppKit; let u = URL(fileURLWithPath: "assets/icon.svg"); let p = URL(fileURLWithPath: "assets/icon.png"); guard let img = NSImage(contentsOf: u) else { exit(1) }; let sz = NSSize(width: 1024, height: 1024); let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: 1024, pixelsHigh: 1024, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!; rep.size = sz; NSGraphicsContext.saveGraphicsState(); NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep); img.draw(in: NSRect(origin: .zero, size: sz), from: .zero, operation: .copy, fraction: 1.0); NSGraphicsContext.restoreGraphicsState(); try! rep.representation(using: .png, properties: [:])!.write(to: p)'
	@ICONSET_DIR=$$(mktemp -d)/AppIcon.iconset && mkdir -p "$$ICONSET_DIR" && \
	for sz in 16 32 128 256 512; do \
		sips -z $$sz $$sz assets/icon.png --out "$$ICONSET_DIR/icon_$${sz}x$${sz}.png" >/dev/null; \
		sz2=$$((sz * 2)); \
		sips -z $$sz2 $$sz2 assets/icon.png --out "$$ICONSET_DIR/icon_$${sz}x$${sz}@2x.png" >/dev/null; \
	done && \
	iconutil -c icns "$$ICONSET_DIR" -o assets/icon.icns && \
	rm -rf "$$(dirname "$$ICONSET_DIR")"
	@echo "✓ Generated assets/icon.png and assets/icon.icns"

# Build stripped, LTO-optimized release binary
release-build:
	cargo build --release -p $(CRATE)

# Assemble signed macOS .app bundle in dist/Dart IDE.app
bundle: icon release-build
	@rm -rf "$(APP_DIR)"
	@mkdir -p "$(APP_DIR)/Contents/MacOS" "$(APP_DIR)/Contents/Resources"
	@cp "target/release/$(BIN)" "$(APP_DIR)/Contents/MacOS/$(BIN)"
	@HELPER=$$(ls -t target/release/build/*/out/shame-sim 2>/dev/null | head -1); \
		if [ -n "$$HELPER" ]; then cp "$$HELPER" "$(APP_DIR)/Contents/MacOS/shame-sim"; fi
	@cp "assets/icon.icns" "$(APP_DIR)/Contents/Resources/icon.icns"
	@printf '%s\n' \
		'<?xml version="1.0" encoding="UTF-8"?>' \
		'<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
		'<plist version="1.0">' \
		'<dict>' \
		'  <key>CFBundleName</key><string>$(APP_NAME)</string>' \
		'  <key>CFBundleDisplayName</key><string>$(APP_NAME)</string>' \
		'  <key>CFBundleIdentifier</key><string>$(BUNDLE_ID)</string>' \
		'  <key>CFBundleVersion</key><string>$(VERSION)</string>' \
		'  <key>CFBundleShortVersionString</key><string>$(VERSION)</string>' \
		'  <key>CFBundlePackageType</key><string>APPL</string>' \
		'  <key>CFBundleExecutable</key><string>$(BIN)</string>' \
		'  <key>CFBundleIconFile</key><string>icon.icns</string>' \
		'  <key>LSMinimumSystemVersion</key><string>13.0</string>' \
		'  <key>NSHighResolutionCapable</key><true/>' \
		'  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>' \
		'</dict>' \
		'</plist>' > "$(APP_DIR)/Contents/Info.plist"
	@codesign --force --deep --sign - "$(APP_DIR)"
	@echo "✓ Packaged $(APP_DIR)"

# Create distributable .dmg archive
dmg: bundle
	@rm -f "$(DMG_PATH)"
	@STAGING=$$(mktemp -d) && \
	cp -R "$(APP_DIR)" "$$STAGING/" && \
	ln -s /Applications "$$STAGING/Applications" && \
	hdiutil create -volname "$(APP_NAME) $(VERSION)" -srcfolder "$$STAGING" -ov -format UDZO "$(DMG_PATH)" >/dev/null && \
	rm -rf "$$STAGING"
	@echo "✓ Created $(DMG_PATH)"

# Full first-release pipeline
release: test dmg
	@ls -lh "$(DMG_PATH)"

# Install into /Applications and launch Shame.app
install: bundle
	@rm -rf "/Applications/Dart IDE.app" "/Applications/$(APP_NAME).app"
	@cp -R "$(APP_DIR)" "/Applications/$(APP_NAME).app"
	@xattr -cr "/Applications/$(APP_NAME).app" 2>/dev/null || true
	@open -a "/Applications/$(APP_NAME).app"
	@echo "✓ Installed and launched /Applications/$(APP_NAME).app"
