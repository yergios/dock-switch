# dock-switch

Windows-style `Super+Number` app switching for the COSMIC desktop (Wayland).

## Behavior

`Super+N` targets the app in dock slot N (1-9), and `Super+0` targets the
last slot:

| App state | Result |
|---|---|
| Not running | Launch it from its desktop entry |
| Running, not focused | Focus its most recently used window (restoring it if minimized) |
| Focused, one window | Minimize it |
| Focused, several windows | Focus its next window, wrapping around |

`Super+Shift+N` (and `Super+Shift+0` for the last slot) always launches the
slot's app again. Whether that opens a new window is up to the app:
single-instance apps may only raise their existing window.

`Super+,` minimizes the focused window. This uses COSMIC's built-in `Minimize`
action, so it needs no code here.

## Slot numbering

Slots must match what the dock shows, so the order follows the dock applet
(`cosmic-app-list`):

1. Pinned apps, in the order of
   `~/.config/cosmic/com.system76.CosmicAppList/v1/favorites`
   (falling back to `/usr/share/cosmic/...` when the user file is missing).
   A pin takes a slot even when its desktop entry is missing.
2. Then running apps that are not pinned, in the order their group first
   appeared. A group leaves the list when its last window closes and goes
   to the end if it comes back.
3. Unpinning a running app moves it to the end of the running list; pinning
   a running app removes it from that list.

Windows are grouped by desktop entry. A window's app ID is matched to a
desktop entry the same way the dock does it, comparing ASCII case-insensitively
and taking the first hit in this order:

1. `StartupWMClass` equals the app ID
2. The desktop id (file stem) equals the app ID, or equals the app ID's
   last dot-separated part
3. `Name` equals the app ID
4. `Exec` equals the app ID, then `Exec`'s first word equals it

Pin strings are matched with the same rule. Windows with no matching entry
group by their raw app ID. Desktop entries come from `$XDG_DATA_HOME` then
`$XDG_DATA_DIRS`, each under `applications/`, earlier directories winning.

## Design

One binary, `dock-switch`, with these commands:

- `dock-switch daemon`: long-running process holding the window state
- `dock-switch switch N`: asks the daemon to act on slot N (`last` for the
  last slot)
- `dock-switch new N`: launches the app in slot N even if it is running
- `dock-switch list`: prints the slots and their windows, for debugging
- `dock-switch shortcuts [remove]`: adds or removes the COSMIC bindings

**Why a daemon:** a one-shot command only sees the windows open right now.
It cannot know the dock's history-dependent order of unpinned apps, or which
window of an app was used last, since Wayland exposes no focus history.

**Wayland protocols** (client side only):

- `ext_foreign_toplevel_list_v1`: window list, app ID, title
- `zcosmic_toplevel_info_v1` (v2+): per-window state (activated, minimized),
  obtained with `get_cosmic_toplevel`
- `zcosmic_toplevel_manager_v1`: `activate`, `set_minimized`, `unset_minimized`
- `wl_seat`: required by `activate`

The COSMIC protocol XML files are vendored unchanged from
`pop-os/cosmic-protocols` and keep their own permissive notices. Bindings are
generated with `wayland-scanner`. The `cosmic-protocols` crate is not used
because it is GPL-3.0-only.

**IPC:** a Unix socket at `$XDG_RUNTIME_DIR/dock-switch.sock` (that directory
is private to the user). The client sends one line (`switch 3`, `new 3`,
`list`); the daemon replies `ok` (optionally followed by output lines),
`launch <desktop file>` or `err <message>`, then closes the connection.

**Event loop:** single-threaded. It polls the Wayland fd and the listening
socket together, so there are no locks.

**Focus history:** each time a window gains the activated state it gets the
next value of a counter. "Most recently used" is the highest value.

**Launching:** the client runs `gio launch <desktop file>`, which handles
`Exec` field codes and `DBusActivatable` entries. The daemon only picks the
file: COSMIC starts the client fresh for each keypress with the session's
environment, whereas a daemon started from a terminal carries that terminal's
environment and directory, which launched apps would inherit. For the same
reason the daemon changes to `$HOME` on start, and `make install` only stops
the old daemon instead of starting the new one.

**Robustness:**

- If the client cannot reach the daemon, it starts one and retries every 2 ms
  for up to 1 s, so a crashed or missing daemon costs one slower keypress
  (about 160 ms, see the next point).
- On start the daemon waits, up to 500 ms, for COSMIC's deferred batch of
  window states before serving requests; until then it cannot tell which
  window is focused.
- A request must arrive whole within 200 ms and 64 bytes, and at most 16
  connections are served per loop pass, so a stalling or flooding client
  cannot hold up window tracking. `list` replaces control characters in
  app-supplied text (titles, app ids) before printing.
- One daemon per user: it holds an exclusive `flock` on
  `$XDG_RUNTIME_DIR/dock-switch.lock`, and a second one exits. Only the lock
  holder replaces a stale socket file.
- The favorites file is re-read on each request and when a window gets a new
  app ID, so pin changes apply without a restart.
- Desktop entries are reloaded when a lookup misses, so newly installed apps
  are found.

## Files

`make install` places the first two (override the location with `PREFIX=`).
`make shortcuts` (`dock-switch shortcuts`) adds the bindings below to the
third, replacing only bindings on the same keys and keeping the rest verbatim;
the file is renamed into place so COSMIC never reads it half written.
`make uninstall` runs `dock-switch shortcuts remove` first.

| Path | Purpose |
|---|---|
| `~/.local/bin/dock-switch` | Installed binary |
| `~/.config/autostart/dock-switch.desktop` | Starts the daemon at login |
| `~/.config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom` | Shortcuts (existing file, extended) |

Shortcut changes:

| Binding | Action | Was |
|---|---|---|
| `Super+1..9` | `Spawn("~/.local/bin/dock-switch switch N")` | `Workspace(N)` |
| `Super+Ctrl+1..9` | `Workspace(N)` | unbound |
| `Super+0` | `Spawn("~/.local/bin/dock-switch switch last")` | `LastWorkspace` |
| `Super+Ctrl+0` | `LastWorkspace` | unbound |
| `Super+Shift+1..9` | `Spawn("~/.local/bin/dock-switch new N")` | `MoveToWorkspace(N)` |
| `Super+Shift+0` | `Spawn("~/.local/bin/dock-switch new last")` | `MoveToLastWorkspace` |
| `Super+Ctrl+Shift+1..9` | `MoveToWorkspace(N)` | unbound |
| `Super+Ctrl+Shift+0` | `MoveToLastWorkspace` | unbound |
| `Super+,` | `Minimize` | `ZoomOut` (still on `Super+-`) |

## Dependencies

`wayland-client`, `wayland-protocols` (staging), `wayland-scanner`, and
`rustix` (already pulled in by `wayland-client`; used directly for `poll`).

## Out of scope

- Matching rules for snap apps and Proton/Wine games
- Matching the dock's order after the daemon restarts mid-session: it rebuilds
  the unpinned order from the compositor's current window order, which can
  differ if pins changed earlier in the session
- Multi-seat setups

## Testing

1. `dock-switch list` shows the same order as the dock, with each pinned
   app's windows grouped under it.
2. With the shortcuts installed, check each row of the behavior table, plus:
   minimized target, target on another workspace, an unpinned app, and
   pinning or unpinning while running.
3. Kill the daemon, press a shortcut: it restarts and acts.
