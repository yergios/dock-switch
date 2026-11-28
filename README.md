# dock-switch

[![CI](https://github.com/yergios/dock-switch/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/yergios/dock-switch/actions/workflows/ci.yml)

Windows-style `Super+Number` app switching for the COSMIC desktop (Wayland).
`Super+N` launches, focuses, cycles or minimizes the app in dock slot N,
`Super+0` targets the last slot, and `Super+Shift+N` opens a new window.
See [SPEC.md](SPEC.md) for the full behavior and design.

## Install

Needs Rust (cargo), `libwayland-dev` and `gio` (from GLib, preinstalled on Pop!_OS).

```sh
make install                      # to ~/.local/bin, plus a login autostart entry
make install PREFIX=/some/prefix  # elsewhere
make uninstall                    # also removes the shortcuts
```

`make install` also restarts a running daemon so the new build takes over.

## Shortcuts

```sh
make shortcuts
```

This adds the bindings to COSMIC's custom shortcuts
(`~/.config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom`), keeping
your other bindings. The first time it changes the file, it saves your
original as `custom.before-dock-switch` next to it. Running it again is
harmless, and `make uninstall` removes the bindings.

| Keys | Action |
|---|---|
| `Super+1..9`, `Super+0` | Dock app N, or the last one: launch, focus, cycle or minimize |
| `Super+Shift+1..9`, `Super+Shift+0` | Open a new window of that app |
| `Super+Ctrl+1..9`, `Super+Ctrl+0` | Switch to workspace N, or the last one (moved from `Super`) |
| `Super+Ctrl+Shift+1..9`, `Super+Ctrl+Shift+0` | Move the window to workspace N, or the last one (moved from `Super+Shift`) |
| `Super+,` | Minimize the focused window (was a second Zoom out key) |

## Debugging

`dock-switch list` prints each slot with its windows, marking the focused and
minimized ones. Compare it with the dock.
