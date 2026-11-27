# Per-user install: the daemon, its autostart entry and the COSMIC shortcuts
# all belong to one user's session, so the default needs no root.
PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
XDG_CONFIG_HOME ?= $(HOME)/.config
AUTOSTART_DIR ?= $(XDG_CONFIG_HOME)/autostart

BIN := target/release/dock-switch
# Matches any dock-switch daemon of this user, whichever binary it runs from.
STOP_DAEMON := pkill -u "$$(id -u)" -xf '.*/dock-switch daemon'

.PHONY: build test install shortcuts uninstall

build:
	cargo build --release --locked

test:
	cargo test --locked

install: build
	install -Dm755 $(BIN) "$(BINDIR)/dock-switch"
	install -d "$(AUTOSTART_DIR)"
	sed 's|@BINDIR@|$(BINDIR)|' dist/dock-switch.desktop.in > "$(AUTOSTART_DIR)/dock-switch.desktop"
	@# Replace a running daemon so the new binary takes over; the client
	@# starts a fresh one on its first request.
	-$(STOP_DAEMON)
	"$(BINDIR)/dock-switch" list > /dev/null

# Adds the Super+Number bindings to COSMIC, pointing at the installed binary.
shortcuts: install
	"$(BINDIR)/dock-switch" shortcuts

uninstall:
	-"$(BINDIR)/dock-switch" shortcuts remove
	-$(STOP_DAEMON)
	rm -f "$(BINDIR)/dock-switch" "$(AUTOSTART_DIR)/dock-switch.desktop"
