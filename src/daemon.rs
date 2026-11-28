//! The long-running process: tracks windows over Wayland, keeps the dock's
//! slot order and focus history, and serves `switch`/`list` requests.

use std::error::Error;
use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};

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
    /// Desktop entry id, or the raw app id when no entry matches; None until
    /// the first `done`.
    group: Option<String>,
    activated: bool,
    minimized: bool,
    /// Value of `Daemon::focus_clock` when the window last gained focus.
    last_focus: u64,
    /// Whether COSMIC has sent the window's state (activated, minimized) yet.
    state_known: bool,
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
    entries_stamp: apps::ScanStamp,
    favorites: Vec<String>,
    /// Group key of each favorite, same order.
    pinned: Vec<String>,
    /// Running groups that are not pinned, in dock order.
    running: Vec<String>,
    focus_clock: u64,
    seat: WlSeat,
    manager: ZcosmicToplevelManagerV1,
    info: ZcosmicToplevelInfoV1,
}

impl Daemon {
    fn group_key(&mut self, app_id: &str) -> String {
        if app_id.is_empty() {
            // The dock gives such windows a placeholder entry with an empty
            // id, and later ones find that entry, so they share one group.
            return String::new();
        }
        if apps::find(&self.entries, app_id).is_none() {
            // The app may have been installed after we loaded the entries.
            self.refresh_entries();
        }
        match apps::find(&self.entries, app_id) {
            Some(i) => self.entries[i].id.clone(),
            None => app_id.to_string(),
        }
    }

    /// Rescans desktop entries if any scanned directory changed. Unmatched
    /// app ids would otherwise cost a full scan each (any app can pick one),
    /// so the check is a few stats. Pins are re-resolved afterwards, since a
    /// pin whose app was missing may now have an entry.
    fn refresh_entries(&mut self) {
        if !self.entries_stamp.changed() {
            return;
        }
        (self.entries, self.entries_stamp) = apps::load_entries();
        self.set_pins(self.favorites.clone());
    }

    fn has_windows(&self, key: &str) -> bool {
        self.windows.iter().any(|w| w.group.as_deref() == Some(key))
    }

    /// Re-reads the dock's pins and applies them if they changed.
    fn reload_favorites(&mut self) {
        let favorites = apps::load_favorites();
        if favorites == self.favorites {
            return;
        }
        if favorites
            .iter()
            .any(|f| apps::find(&self.entries, f).is_none())
        {
            self.refresh_entries();
        }
        self.set_pins(favorites);
    }

    /// Resolves pins to group keys and applies the dock's rules for pin
    /// changes: an unpinned running app moves to the end, a pinned one leaves
    /// the running list.
    fn set_pins(&mut self, favorites: Vec<String>) {
        let pinned: Vec<String> = favorites
            .iter()
            .map(|f| match apps::find(&self.entries, f) {
                Some(i) => self.entries[i].id.clone(),
                None => f.clone(),
            })
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
        if !self.has_windows(key) {
            self.running.retain(|k| k != key);
        }
    }

    fn assign_group(&mut self, idx: usize) {
        let app_id = self.windows[idx].app_id.clone();
        let key = self.group_key(&app_id);
        self.windows[idx].grouped_app_id = app_id;
        let old = self.windows[idx].group.replace(key.clone());
        if old.as_ref() == Some(&key) {
            return;
        }
        if let Some(old) = old {
            self.forget_group_if_empty(&old);
        }
        self.reload_favorites();
        if !self.pinned.contains(&key) && !self.running.contains(&key) {
            self.running.push(key);
        }
    }

    fn remove_window(&mut self, idx: usize) {
        let w = self.windows.remove(idx);
        w.cosmic.destroy();
        w.foreign.destroy();
        if let Some(group) = &w.group {
            self.forget_group_if_empty(group);
        }
    }

    fn slots(&self) -> impl Iterator<Item = &String> {
        self.pinned.iter().chain(&self.running)
    }

    /// Windows shown under the slot at `index` (0-based). When two pins
    /// resolve to the same entry, the dock gives the windows to the first.
    fn slot_windows(&self, index: usize, key: &str) -> Vec<usize> {
        if self.slots().position(|k| k == key) != Some(index) {
            return Vec::new();
        }
        (0..self.windows.len())
            .filter(|&i| self.windows[i].group.as_deref() == Some(key))
            .collect()
    }

    /// Activates a window and records the expected result right away: COSMIC
    /// reports the new state a little later, and a quick second keypress
    /// would otherwise act on the old focus. Its state events overwrite this.
    fn focus(&mut self, idx: usize) {
        let w = &self.windows[idx];
        if w.minimized {
            self.manager.unset_minimized(&w.cosmic);
        }
        self.manager.activate(&w.cosmic, &self.seat);
        for w in &mut self.windows {
            w.activated = false;
        }
        self.focus_clock += 1;
        let w = &mut self.windows[idx];
        (w.activated, w.minimized, w.last_focus) = (true, false, self.focus_clock);
    }

    /// Minimizes a window, recording the result right away like `focus`.
    fn minimize(&mut self, idx: usize) {
        self.manager.set_minimized(&self.windows[idx].cosmic);
        let w = &mut self.windows[idx];
        (w.activated, w.minimized) = (false, true);
    }

    /// 0-based index and group key of a 1-based slot; `None` means the last.
    fn slot_key(&mut self, slot: Option<usize>) -> Result<(usize, String), String> {
        self.reload_favorites();
        // Resolved after the reload, since a pin change can move the last slot.
        let slot = slot.unwrap_or_else(|| self.slots().count());
        let index = slot.wrapping_sub(1);
        let key_at = |d: &Self| d.slots().nth(index).cloned();
        let key = key_at(self).ok_or_else(|| format!("no app in slot {slot}"))?;
        // A pin with no entry and no windows may be an app installed since
        // the last scan; a rescan re-resolves the pins.
        if !self.has_windows(&key) && !self.entries.iter().any(|e| e.id == key) {
            self.refresh_entries();
            let key = key_at(self).ok_or_else(|| format!("no app in slot {slot}"))?;
            return Ok((index, key));
        }
        Ok((index, key))
    }

    fn switch(&mut self, slot: Option<usize>) -> Result<Reply, String> {
        let (index, key) = self.slot_key(slot)?;
        let group = self.slot_windows(index, &key);
        if group.is_empty() {
            return self.launch(&key);
        }

        let is_focused = |i: &usize| self.windows[*i].activated && !self.windows[*i].minimized;
        if let Some(pos) = group.iter().position(is_focused) {
            if group.len() == 1 {
                self.minimize(group[0]);
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
            let name = if key.is_empty() { "(no app id)" } else { key };
            let _ = writeln!(out, "{}  {}{pin}", n + 1, printable(name));
            for &i in &self.slot_windows(n, key) {
                let w = &self.windows[i];
                let mark = match (w.activated && !w.minimized, w.minimized) {
                    (true, _) => "focused  ",
                    (_, true) => "minimized",
                    _ => "         ",
                };
                let (app_id, title) = (printable(&w.app_id), printable(&w.title));
                let _ = writeln!(out, "     {mark}  {app_id}  {title}");
            }
        }
        out
    }

    fn serve(&mut self, stream: UnixStream) -> io::Result<()> {
        stream.set_write_timeout(Some(Duration::from_millis(200)))?;
        let line = read_request(&stream)?;

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
                    _ => self.slot_key(slot).and_then(|(_, key)| self.launch(&key)),
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
    // Held for the daemon's lifetime. Checking the socket instead would race:
    // two daemons starting together could both see none, and the second
    // would unlink the first one's socket, leaving it running unreachable.
    let lock = std::fs::File::create(path.with_extension("lock"))?;
    if rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_err() {
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

    let (entries, entries_stamp) = apps::load_entries();
    let mut daemon = Daemon {
        windows: Vec::new(),
        entries,
        entries_stamp,
        favorites: Vec::new(),
        pinned: Vec::new(),
        running: Vec::new(),
        focus_clock: 0,
        seat,
        manager,
        info,
    };
    // Pins first, so windows that already exist group against them.
    daemon.reload_favorites();
    queue.roundtrip(&mut daemon)?;

    // COSMIC sends each window's state in a deferred batch, a while after the
    // handle is requested. Serving a request before it arrives would see no
    // focused window, so a keypress that started the daemon would activate
    // instead of minimize. Clients wait in the listen backlog meanwhile.
    let deadline = Instant::now() + Duration::from_millis(500);
    while daemon.windows.iter().any(|w| !w.state_known) {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        flush(&conn)?;
        if let Some(guard) = queue.prepare_read() {
            let timeout = rustix::time::Timespec::try_from(left)?;
            let ready = {
                let fd = guard.connection_fd();
                match poll(&mut [PollFd::new(&fd, PollFlags::IN)], Some(&timeout)) {
                    Ok(n) => n > 0,
                    Err(rustix::io::Errno::INTR) => false,
                    Err(e) => return Err(e.into()),
                }
            };
            if ready {
                read_events(guard)?;
            }
        }
        queue.dispatch_pending(&mut daemon)?;
    }

    // Pins can change and change back between two requests; the dock applies
    // each change, so a watch keeps our order in step. Without one (no user
    // config directory yet), pins are still re-read on every request.
    let favorites_watch = watch_favorites();

    loop {
        queue.dispatch_pending(&mut daemon)?;
        flush(&conn)?;
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let (wayland_ready, client_ready, favorites_changed) = {
            let fd = guard.connection_fd();
            let mut fds = vec![
                PollFd::new(&fd, PollFlags::IN),
                PollFd::new(&listener, PollFlags::IN),
            ];
            if let Some(watch) = &favorites_watch {
                fds.push(PollFd::new(watch, PollFlags::IN));
            }
            match poll(&mut fds, None) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(e.into()),
            }
            let ready = |i: usize| fds.get(i).is_some_and(|f| !f.revents().is_empty());
            (ready(0), ready(1), ready(2))
        };
        if favorites_changed && let Some(watch) = &favorites_watch {
            // The events only say something changed; read them all, then the
            // file itself.
            let mut buf = [0u8; 4096];
            while rustix::io::read(watch, &mut buf).is_ok_and(|n| n > 0) {}
            daemon.reload_favorites();
        }
        if wayland_ready {
            read_events(guard)?;
            // Serve requests against the newest window state.
            queue.dispatch_pending(&mut daemon)?;
        } else {
            drop(guard);
        }
        if client_ready {
            // Bounded, so a flood of connections cannot starve window
            // tracking; the rest are served on the next pass.
            for _ in 0..16 {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                if let Err(e) = daemon.serve(stream) {
                    eprintln!("dock-switch: request failed: {e}");
                }
            }
        }
    }
}

/// Watches the dock's settings directory: cosmic-config may rewrite the
/// favorites file in place or replace it, so both are covered.
fn watch_favorites() -> Option<rustix::fd::OwnedFd> {
    use rustix::fs::inotify::{self, CreateFlags, WatchFlags};
    let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK).ok()?;
    let events =
        WatchFlags::CLOSE_WRITE | WatchFlags::MOVED_TO | WatchFlags::CREATE | WatchFlags::DELETE;
    inotify::add_watch(&fd, apps::favorites_dir(), events).ok()?;
    Some(fd)
}

/// Longest valid request is `switch <number>`; anything near this is garbage.
const MAX_REQUEST: usize = 64;

/// Reads one request line. The deadline covers the whole request, not each
/// read, so a client trickling bytes cannot hold up window tracking.
fn read_request(stream: &UnixStream) -> io::Result<String> {
    let deadline = Instant::now() + Duration::from_millis(200);
    let mut buf = Vec::new();
    let mut chunk = [0u8; MAX_REQUEST];
    while !buf.contains(&b'\n') {
        let left = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(io::ErrorKind::TimedOut)?;
        stream.set_read_timeout(Some(left))?;
        let n = (&*stream).read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_REQUEST {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request too long",
            ));
        }
    }
    String::from_utf8(buf).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"))
}

/// Window titles come from apps (a web page sets its tab's title), so
/// control characters are replaced before they reach a terminal.
fn printable(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// Sends buffered requests. A full socket buffer is not fatal: what is left
/// goes out on the next pass of the loop.
fn flush(conn: &Connection) -> Result<(), WaylandError> {
    match conn.flush() {
        Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
        other => other,
    }
}

/// Reads whatever the compositor has sent; a spurious wakeup is not an error.
fn read_events(guard: wayland_client::backend::ReadEventsGuard) -> Result<(), WaylandError> {
    match guard.read() {
        Ok(_) => Ok(()),
        Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
        Err(e) => Err(e),
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
                group: None,
                activated: false,
                minimized: false,
                last_focus: 0,
                state_known: false,
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
                // The dock ignores a change to an empty app id, keeping the
                // window where it was.
                let changed = !w.app_id.is_empty() && w.app_id != w.grouped_app_id;
                if w.group.is_none() || changed {
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
        state.windows[i].state_known = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_requests_within_limits() {
        let (mut client, server) = UnixStream::pair().unwrap();
        client.write_all(b"switch 3\n").unwrap();
        assert_eq!(read_request(&server).unwrap(), "switch 3\n");

        let (mut client, server) = UnixStream::pair().unwrap();
        client.write_all(&[b'x'; MAX_REQUEST + 1]).unwrap();
        assert_eq!(
            read_request(&server).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        // No newline and no hang-up: the whole-request deadline ends it.
        let (mut client, server) = UnixStream::pair().unwrap();
        client.write_all(b"swi").unwrap();
        let started = Instant::now();
        assert!(read_request(&server).is_err());
        assert!(started.elapsed() < Duration::from_millis(400));
    }

    #[test]
    fn strips_control_characters() {
        assert_eq!(printable("a\x1b]52;c;evil\x07b\u{9b}"), "a?]52;c;evil?b?");
        assert_eq!(printable("Zen — Ecosia"), "Zen — Ecosia");
    }
}
