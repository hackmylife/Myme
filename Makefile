# myme — top-level Makefile
#
# All Rust work is delegated to Cargo; this file is a convenience wrapper
# so that contributors and CI can use familiar short commands without
# remembering Cargo flags.
#
# Targets:
#   build    Build all workspace members in debug mode
#   test     Run the full workspace test suite
#   install  Build everything and install MymeIM.app to ~/Library/Input Methods/
#   dict     Compile raw dictionary sources into data/dict/myme.dict
#   cli      Build and run the myme-cli binary (useful during development)
#   clean    Remove Cargo build artefacts

.PHONY: build test install dict cli clean

# Default target
all: build

## Build every workspace member (debug profile).
build:
	cargo build --workspace

## Run every test in the workspace.
test:
	cargo test --workspace

## Build everything (Rust workspace + macOS app bundle) and install
## MymeIM.app to ~/Library/Input Methods/.
##
## After installation:
##   1. Open System Settings → Keyboard → Input Sources.
##   2. Click "+" and search for "Myme".
##   3. Add the input source and switch to it with the input menu.
install:
	@echo "==> Building Rust workspace (release) ..."
	cargo build --workspace --release
	@echo "==> Building macOS app bundle ..."
	bash macos/build.sh
	@echo "==> Installing MymeIM.app ..."
	@killall MymeIM 2>/dev/null || true
	@sleep 1
	mkdir -p "$(HOME)/Library/Input Methods"
	cp -R macos/build/MymeIM.app "$(HOME)/Library/Input Methods/"
	@echo "==> Registering input source ..."
	@swift -e '\
		import Carbon; \
		let url = CFURLCreateWithFileSystemPath(nil, "$(HOME)/Library/Input Methods/MymeIM.app" as CFString, .cfurlposixPathStyle, true)!; \
		TISRegisterInputSource(url); \
		let props = [kTISPropertyInputSourceID as String: "com.myme.inputmethod.Myme.Hiragana" as CFString] as CFDictionary; \
		if let sources = TISCreateInputSourceList(props, true)?.takeRetainedValue() as? [TISInputSource], let src = sources.first { \
			TISEnableInputSource(src); \
			print("Input source registered and enabled."); \
		} else { \
			print("WARNING: Could not find input source after registration."); \
		}'
	@echo ""
	@echo "Installation complete."
	@echo "Open System Settings → Keyboard → Input Sources to add 'Myme'."

## Compile raw dictionaries.  Requires source files in data/raw/.
dict:
	cargo run -p dict-builder --release

## Quick development loop: build and run the CLI harness.
cli:
	cargo run -p myme-cli

## Remove all Cargo-generated build artefacts.
clean:
	cargo clean
