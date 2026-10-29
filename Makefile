# Dart/Flutter IDE — common tasks.
#
#   make run       foreground run (logs in this terminal)
#   make restart   rebuild, stop the old window, relaunch in the background
#   make test      whole workspace
#   make check     clippy

CRATE := app
BIN  := dart-ide
LOG  := /tmp/dart-ide.log

.PHONY: build run restart stop test check fmt clean

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

test:
	cargo test --workspace

check:
	cargo clippy --workspace --all-targets

fmt:
	cargo fmt --all

clean:
	cargo clean
