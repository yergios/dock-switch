# Per-user install: the daemon, its autostart entry and the COSMIC shortcuts
# all belong to one user's session, so the default needs no root.
PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
XDG_CONFIG_HOME ?= $(HOME)/.config
AUTOSTART_DIR ?= $(XDG_CONFIG_HOME)/autostart

BIN := target/release/dock-switch
# Matches this user's processes named exactly dock-switch, whatever path
# they run from.
STOP_DAEMON := pkill -u "$$(id -u)" -x dock-switch

.PHONY: build test install shortcuts uninstall

build:
	cargo build --release --locked

test:
	cargo test --locked

install: build
	install -Dm755 $(BIN) "$(BINDIR)/dock-switch"
	install -d "$(AUTOSTART_DIR)"
	sed 's|@BINDIR@|$(BINDIR)|' dist/dock-switch.desktop.in > "$(AUTOSTART_DIR)/dock-switch.desktop"
	@# Stop a running daemon so the new binary takes over. The next keypress
	@# starts a fresh one; starting it from here would hand it this shell's
	@# environment and directory.
	-$(STOP_DAEMON)

# Adds the Super+Number bindings to COSMIC, pointing at the installed binary.
shortcuts: install
	"$(BINDIR)/dock-switch" shortcuts

uninstall:
	-"$(BINDIR)/dock-switch" shortcuts remove
	-$(STOP_DAEMON)
	rm -f "$(BINDIR)/dock-switch" "$(AUTOSTART_DIR)/dock-switch.desktop"
