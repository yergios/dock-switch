//! The long-running process: tracks windows over Wayland, keeps the dock's
//! slot order and focus history, and serves `switch`/`list` requests.

use std::error::Error;
use std::fmt::Write as _;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, poll};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry::WlRegistry, wl_seat::WlSeat};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, backend::WaylandError, delegate_noop,
    event_created_child,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};

use crate::apps::{self, DesktopEntry};
use crate::protocols::toplevel_info::{
    zcosmic_toplevel_handle_v1::{self, ZcosmicToplevelHandleV1},
    zcosmic_toplevel_info_v1::{self, ZcosmicToplevelInfoV1},
};
use crate::protocols::toplevel_management::zcosmic_toplevel_manager_v1::ZcosmicToplevelManagerV1;

// Values of zcosmic_toplevel_handle_v1's `state` enum.
const STATE_MINIMIZED: u32 = 1;
const STATE_ACTIVATED: u32 = 2;

struct Window {
    foreign: ExtForeignToplevelHandleV1,
    cosmic: ZcosmicToplevelHandleV1,
    title: String,
    /// Latest app id sent; it only takes effect on the handle's `done` event.
    app_id: String,
    /// The app id `group` was computed from.
    grouped_app_id: String,
    /// Desktop entry id, or the raw app id when no entry matches. Empty until
    /// the first `done`.
    group: String,
    activated: bool,
    minimized: bool,
    /// Value of `Daemon::focus_clock` when the window last gained focus.
    last_focus: u64,
}

/// What a successful request asks of the client.
enum Reply {
    Done,
    Output(String),
    /// Run this desktop file.
    Launch(String),
}

struct Daemon {
    /// In creation order, which is also the order an app's windows cycle in.
    windows: Vec<Window>,
    entries: Vec<DesktopEntry>,
    favorites: Vec<String>,
    /// Group key of each favorite, same order.
    pinned: Vec<String>,
    /// Running groups that are not pinned, in dock order.
    running: Vec<String>,
    focus_clock: u64,
    unknown_apps: u32,
    seat: WlSeat,
    manager: ZcosmicToplevelManagerV1,
    info: ZcosmicToplevelInfoV1,
}

impl Daemon {
    fn group_key(&mut self, app_id: &str) -> String {
        if app_id.is_empty() {
            // The dock shows each of these as its own "Unknown Application".
            self.unknown_apps += 1;
            return format!("unknown-{}", self.unknown_apps);
        }
        if let Some(i) = apps::find(&self.entries, app_id) {
            return self.entries[i].id.clone();
        }
        // The app may have been installed after we loaded the entries.
        self.entries = apps::load_entries();
        match apps::find(&self.entries, app_id) {
            Some(i) => self.entries[i].id.clone(),
            None => app_id.to_string(),
        }
    }

    fn has_windows(&self, key: &str) -> bool {
        self.windows.iter().any(|w| w.group == key)
    }

    /// Re-reads the dock's pins and applies the dock's rules for pin changes:
    /// an unpinned running app moves to the end, a pinned one leaves the
    /// running list.
    fn reload_favorites(&mut self) {
        let favorites = apps::load_favorites();
        if favorites == self.favorites {
            return;
        }
        let resolve = |entries: &[DesktopEntry], fav: &str| {
            apps::find(entries, fav).map(|i| entries[i].id.clone())
        };
        if favorites
            .iter()
            .any(|f| resolve(&self.entries, f).is_none())
        {
            self.entries = apps::load_entries();
        }
        let pinned: Vec<String> = favorites
            .iter()
            .map(|f| resolve(&self.entries, f).unwrap_or_else(|| f.clone()))
            .collect();

        for key in &self.pinned {
            if !pinned.contains(key) && self.has_windows(key) && !self.running.contains(key) {
                self.running.push(key.clone());
            }
        }
        self.running.retain(|k| !pinned.contains(k));
        self.favorites = favorites;
        self.pinned = pinned;
    }

    fn forget_group_if_empty(&mut self, key: &str) {
        if !key.is_empty() && !self.has_windows(key) {
            self.running.retain(|k| k != key);
        }
    }

    fn assign_group(&mut self, idx: usize) {
        let app_id = self.windows[idx].app_id.clone();
        let key = self.group_key(&app_id);
        self.windows[idx].grouped_app_id = app_id;
        let old = std::mem::replace(&mut self.windows[idx].group, key.clone());
        if old == key {
            return;
        }
        self.forget_group_if_empty(&old);
        self.reload_favorites();
        if !self.pinned.contains(&key) && !self.running.contains(&key) {
            self.running.push(key);
        }
    }

    fn remove_window(&mut self, idx: usize) {
        let w = self.windows.remove(idx);
        w.cosmic.destroy();
        w.foreign.destroy();
        self.forget_group_if_empty(&w.group);
    }

    fn slots(&self) -> impl Iterator<Item = &String> {
        self.pinned.iter().chain(&self.running)
    }

    fn group_windows(&self, key: &str) -> Vec<usize> {
        (0..self.windows.len())
            .filter(|&i| self.windows[i].group == key)
            .collect()
    }

    fn focus(&self, idx: usize) {
        let w = &self.windows[idx];
        if w.minimized {
            self.manager.unset_minimized(&w.cosmic);
        }
        self.manager.activate(&w.cosmic, &self.seat);
    }

    /// Group key of a 1-based slot; `None` means the last slot.
    fn slot_key(&mut self, slot: Option<usize>) -> Result<String, String> {
        self.reload_favorites();
        // Resolved after the reload, since a pin change can move the last slot.
        let slot = slot.unwrap_or_else(|| self.slots().count());
        self.slots()
            .nth(slot.wrapping_sub(1))
            .cloned()
            .ok_or_else(|| format!("no app in slot {slot}"))
    }

    fn switch(&mut self, slot: Option<usize>) -> Result<Reply, String> {
        let key = self.slot_key(slot)?;
        let group = self.group_windows(&key);
        if group.is_empty() {
            return self.launch(&key);
        }

        let is_focused = |i: &usize| self.windows[*i].activated && !self.windows[*i].minimized;
        if let Some(pos) = group.iter().position(is_focused) {
            if group.len() == 1 {
                self.manager.set_minimized(&self.windows[group[0]].cosmic);
                return Ok(Reply::Done);
            }
            self.focus(group[(pos + 1) % group.len()]);
            return Ok(Reply::Done);
        }

        // Without any focus history yet, the newest window wins the tie.
        let target = group
            .iter()
            .copied()
            .max_by_key(|&i| self.windows[i].last_focus);
        self.focus(target.expect("group is not empty"));
        Ok(Reply::Done)
    }

    /// Tells the client which desktop file to run. The client runs it, not
    /// the daemon: COSMIC starts the client fresh for each keypress with the
    /// session's environment, while the daemon may have inherited a
    /// terminal's (variables, secrets, working directory) when a command
    /// started it, and launched apps would inherit that in turn.
    fn launch(&self, key: &str) -> Result<Reply, String> {
        let entry = self
            .entries
            .iter()
            .find(|e| e.id == key)
            .ok_or_else(|| format!("no desktop entry for '{key}'"))?;
        // The reply is line-based, so the path must be one line.
        match entry.path.to_str() {
            Some(path) if !path.contains('\n') => Ok(Reply::Launch(path.to_string())),
            _ => Err(format!("unusable desktop file path for '{key}'")),
        }
    }

    fn list(&self) -> String {
        let mut out = String::new();
        for (n, key) in self.slots().enumerate() {
            let pin = if n < self.pinned.len() {
                "  (pinned)"
            } else {
                ""
            };
            let _ = writeln!(out, "{}  {key}{pin}", n + 1);
            for &i in &self.group_windows(key) {
                let w = &self.windows[i];
                let mark = match (w.activated && !w.minimized, w.minimized) {
                    (true, _) => "focused  ",
                    (_, true) => "minimized",
                    _ => "         ",
                };
                let _ = writeln!(out, "     {mark}  {}  {}", w.app_id, w.title);
            }
        }
        out
    }

    fn serve(&mut self, stream: UnixStream) -> io::Result<()> {
        // A client that stalls must not freeze window tracking.
        stream.set_read_timeout(Some(Duration::from_millis(200)))?;
        stream.set_write_timeout(Some(Duration::from_millis(200)))?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line)?;

        let words: Vec<&str> = line.split_whitespace().collect();
        let reply = match words.as_slice() {
            [cmd @ ("switch" | "new"), n] => {
                let slot = match *n {
                    "last" => Ok(None),
                    n => n.parse().map(Some).map_err(|_| format!("bad slot '{n}'")),
                };
                slot.and_then(|slot| match *cmd {
                    "switch" => self.switch(slot),
                    // Whether a second launch opens a new window is up to the
                    // app; single-instance apps may just raise their window.
                    _ => self.slot_key(slot).and_then(|key| self.launch(&key)),
                })
            }
            ["list"] => Ok(Reply::Output(self.list())),
            _ => Err(format!("unknown request '{}'", line.trim())),
        };
        let text = match reply {
            Ok(Reply::Done) => "ok\n".to_string(),
            Ok(Reply::Output(out)) => format!("ok\n{out}"),
            Ok(Reply::Launch(path)) => format!("launch {path}\n"),
            Err(msg) => format!("err {msg}\n"),
        };
        (&stream).write_all(text.as_bytes())
    }
}

pub fn run() -> Result<(), Box<dyn Error>> {
    // Do not keep the directory we were started from busy; it may be a mount
    // the user wants to unmount.
    if let Some(home) = std::env::var_os("HOME") {
        let _ = std::env::set_current_dir(home);
    }
    let path = crate::socket_path()?;
    if UnixStream::connect(&path).is_ok() {
        eprintln!("dock-switch: daemon already running");
        return Ok(());
    }
    let _ = std::fs::remove_file(&path); // stale socket from a previous run
    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;

    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<Daemon>(&conn)?;
    let qh = queue.handle();
    let seat: WlSeat = globals.bind(&qh, 1..=9, ())?;
    let info: ZcosmicToplevelInfoV1 = globals.bind(&qh, 2..=3, ())?;
    let manager: ZcosmicToplevelManagerV1 = globals.bind(&qh, 1..=4, ())?;
    let _list: ExtForeignToplevelListV1 = globals.bind(&qh, 1..=1, ())?;

    let mut daemon = Daemon {
        windows: Vec::new(),
        entries: apps::load_entries(),
        favorites: Vec::new(),
        pinned: Vec::new(),
        running: Vec::new(),
        focus_clock: 0,
        unknown_apps: 0,
        seat,
        manager,
        info,
    };
    // Pins first, so windows that already exist group against them.
    daemon.reload_favorites();
    queue.roundtrip(&mut daemon)?;

    loop {
        queue.dispatch_pending(&mut daemon)?;
        conn.flush()?;
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let (wayland_ready, client_ready) = {
            let fd = guard.connection_fd();
            let mut fds = [
                PollFd::new(&fd, PollFlags::IN),
                PollFd::new(&listener, PollFlags::IN),
            ];
            match poll(&mut fds, None) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(e.into()),
            }
            (!fds[0].revents().is_empty(), !fds[1].revents().is_empty())
        };
        if wayland_ready {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
            // Serve requests against the newest window state.
            queue.dispatch_pending(&mut daemon)?;
        } else {
            drop(guard);
        }
        if client_ready {
            while let Ok((stream, _)) = listener.accept() {
                if let Err(e) = daemon.serve(stream) {
                    eprintln!("dock-switch: request failed: {e}");
                }
            }
        }
    }
}

// ------------------------------------------------------------ Wayland events

delegate_noop!(Daemon: ignore WlSeat);
delegate_noop!(Daemon: ignore ZcosmicToplevelManagerV1);

impl Dispatch<WlRegistry, GlobalListContents> for Daemon {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for Daemon {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            let cosmic = state.info.get_cosmic_toplevel(&toplevel, qh, ());
            state.windows.push(Window {
                foreign: toplevel,
                cosmic,
                title: String::new(),
                app_id: String::new(),
                grouped_app_id: String::new(),
                group: String::new(),
                activated: false,
                minimized: false,
                last_focus: 0,
            });
        }
    }

    event_created_child!(Daemon, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for Daemon {
    fn event(
        state: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_foreign_toplevel_handle_v1::Event;
        let Some(i) = state.windows.iter().position(|w| w.foreign == *handle) else {
            return;
        };
        match event {
            Event::Title { title } => state.windows[i].title = title,
            Event::AppId { app_id } => state.windows[i].app_id = app_id,
            Event::Done => {
                let w = &state.windows[i];
                if w.group.is_empty() || w.app_id != w.grouped_app_id {
                    state.assign_group(i);
                }
            }
            Event::Closed => state.remove_window(i),
            _ => {}
        }
    }
}

impl Dispatch<ZcosmicToplevelInfoV1, ()> for Daemon {
    fn event(
        _: &mut Self,
        _: &ZcosmicToplevelInfoV1,
        _: zcosmic_toplevel_info_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    // Only the deprecated `toplevel` event creates objects, and COSMIC does
    // not send it from v2 on, but wayland-client panics on a new object
    // without this mapping.
    event_created_child!(Daemon, ZcosmicToplevelInfoV1, [
        zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (ZcosmicToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZcosmicToplevelHandleV1, ()> for Daemon {
    fn event(
        state: &mut Self,
        handle: &ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let zcosmic_toplevel_handle_v1::Event::State { state: bytes } = event else {
            return;
        };
        let Some(i) = state.windows.iter().position(|w| w.cosmic == *handle) else {
            return;
        };
        let flags: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_ne_bytes(*c))
            .collect();
        let activated = flags.contains(&STATE_ACTIVATED);
        if activated && !state.windows[i].activated {
            state.focus_clock += 1;
            state.windows[i].last_focus = state.focus_clock;
        }
        state.windows[i].activated = activated;
        state.windows[i].minimized = flags.contains(&STATE_MINIMIZED);
    }
}
