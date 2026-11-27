//! dock-switch: Windows-style Super+Number app switching for COSMIC.
//! See SPEC.md for the behavior and design.

mod apps;
mod daemon;
mod protocols;

use std::error::Error;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

const USAGE: &str = "usage: dock-switch daemon | switch <slot|last> | new <slot|last> | list\n";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["daemon"] => daemon::run().map(|()| 0),
        [cmd @ ("switch" | "new"), slot] => request(&format!("{cmd} {slot}")),
        ["list"] => request("list"),
        _ => {
            eprint!("{USAGE}");
            Ok(2)
        }
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("dock-switch: {e}");
            std::process::exit(1);
        }
    }
}

pub fn socket_path() -> Result<PathBuf, String> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(|dir| PathBuf::from(dir).join("dock-switch.sock"))
        .ok_or_else(|| "XDG_RUNTIME_DIR is not set".to_string())
}

/// Sends one request to the daemon, starting the daemon first if needed.
fn request(line: &str) -> Result<i32, Box<dyn Error>> {
    let path = socket_path()?;
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(_) => {
            start_daemon()?;
            connect_with_retry(&path)?
        }
    };
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(format!("{line}\n").as_bytes())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;

    if let Some(out) = reply.strip_prefix("ok\n") {
        print!("{out}");
        Ok(0)
    } else {
        eprint!("dock-switch: {}", reply.strip_prefix("err ").unwrap_or(&reply));
        Ok(1)
    }
}

fn start_daemon() -> Result<(), Box<dyn Error>> {
    Command::new(std::env::current_exe()?)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Own process group, so it outlives whatever spawned this client.
        .process_group(0)
        .spawn()?;
    Ok(())
}

fn connect_with_retry(path: &PathBuf) -> Result<UnixStream, Box<dyn Error>> {
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(20));
        if let Ok(s) = UnixStream::connect(path) {
            return Ok(s);
        }
    }
    Err("daemon did not start".into())
}
