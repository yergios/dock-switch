# dock-switch

Windows-style `Super+Number` app switching for the COSMIC desktop (Wayland).
`Super+N` launches, focuses, cycles or minimizes the app in dock slot N,
`Super+0` targets the last slot, and `Super+Shift+N` opens a new window.
See [SPEC.md](SPEC.md) for the full behavior and design.

## Install

Needs Rust (cargo), `libwayland-dev` and `gio` (from GLib, preinstalled on Pop!_OS).

```sh
make install                      # to ~/.local/bin, plus a login autostart entry
make install PREFIX=/some/prefix  # elsewhere
make uninstall
```

`make install` also restarts a running daemon so the new build takes over.

## Shortcuts

Installing does not touch your shortcuts. Bind them in
`~/.config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom`, for example:

```ron
(
    modifiers: [Super],
    key: "1",
    description: Some("Switch to dock app 1"),
): Spawn("/home/<user>/.local/bin/dock-switch switch 1"),
```

Use `switch last` for `Super+0` and `new N` for `Super+Shift+N`. SPEC.md lists
every binding, including where the displaced workspace shortcuts moved.

## Debugging

`dock-switch list` prints each slot with its windows, marking the focused and
minimized ones. Compare it with the dock.
