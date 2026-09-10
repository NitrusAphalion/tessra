//! Talking to a running daemon, and starting one when none is running.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::server::{Endpoint, Request};
use crate::{Error, Result};

/// Send one request and read one response line.
pub fn call(endpoint: &Endpoint, req: &Request) -> Result<Value> {
    let addr: SocketAddr = ([127, 0, 0, 1], endpoint.port).into();
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500))?;
    stream.set_read_timeout(Some(Duration::from_secs(600)))?;
    let line = serde_json::to_string(req)?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut out = String::new();
    reader.read_line(&mut out)?;
    if out.trim().is_empty() {
        return Err(Error::verb("DAEMON", "empty response from daemon"));
    }
    Ok(serde_json::from_str(out.trim())?)
}

/// Is the daemon at this endpoint alive?
pub fn alive(endpoint: &Endpoint) -> bool {
    let req = Request {
        token: endpoint.token.clone(),
        agent: None,
        model: None,
        write: vec![],
        workspace: None,
        principal: None,
        verb: "ping".into(),
        args: Value::Null,
    };
    matches!(call(endpoint, &req), Ok(v) if v["ok"] == Value::Bool(true))
}

/// Find a live daemon for the repository, starting one if allowed.
pub fn ensure(tessra_dir: &Path, exe: &Path, repo_root: &Path, may_start: bool) -> Option<Endpoint> {
    if let Some(e) = Endpoint::read(tessra_dir) {
        if alive(&e) {
            return Some(e);
        }
        let _ = std::fs::remove_file(tessra_dir.join("daemon"));
    }
    if !may_start {
        return None;
    }
    if spawn_detached(exe, repo_root).is_err() {
        return None;
    }
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(e) = Endpoint::read(tessra_dir) {
            if alive(&e) {
                return Some(e);
            }
        }
    }
    None
}

fn spawn_detached(exe: &Path, repo_root: &Path) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--repo")
        .arg(repo_root)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        // Our own stdio may be pipes a caller is waiting on. A child inherits
        // every inheritable handle, so mark them non-inheritable first or the
        // caller never sees end-of-file while the daemon lives.
        make_std_handles_private();
    }
    cmd.spawn()?;
    Ok(())
}

#[cfg(windows)]
fn make_std_handles_private() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(which: u32) -> isize;
        fn SetHandleInformation(handle: isize, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 1;
    for which in [-10i32 as u32, -11i32 as u32, -12i32 as u32] {
        // SAFETY: plain kernel32 calls on handles this process owns.
        unsafe {
            let h = GetStdHandle(which);
            if h != 0 && h != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// The repository root containing `start`, without opening the store.
pub fn find_root(start: &Path) -> Result<PathBuf> {
    let start = std::fs::canonicalize(start)?;
    let mut cur: Option<&Path> = Some(&start);
    while let Some(p) = cur {
        if p.join(".tessra").join("TESSRA").exists() {
            return Ok(p.to_path_buf());
        }
        cur = p.parent();
    }
    Err(Error::NotARepo(start))
}
