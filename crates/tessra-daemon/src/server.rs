//! The long-lived daemon: one open repository served over loopback TCP with
//! newline-delimited JSON. Sessions live in memory here. The listener is
//! bound to 127.0.0.1 on an ephemeral port, and `.tessra/daemon` records the
//! port, a bearer token, and the pid. A watchdog exits after an idle period.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::principals::Actor;
use crate::{verbs, Repo};

/// A request from a client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub token: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub write: Vec<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    /// An external principal the daemon holds a key for, instead of an agent session.
    #[serde(default)]
    pub principal: Option<String>,
    /// The credential that goes with acting as the owner or as `principal`.
    /// The daemon token alone reaches the daemon; it makes nobody the owner.
    #[serde(default)]
    pub credential: Option<String>,
    pub verb: String,
    #[serde(default)]
    pub args: Value,
}

/// What `.tessra/daemon` holds.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

impl Endpoint {
    pub fn read(tessra_dir: &std::path::Path) -> Option<Endpoint> {
        let text = std::fs::read_to_string(tessra_dir.join("daemon")).ok()?;
        let mut lines = text.lines();
        let port = lines.next()?.trim().parse().ok()?;
        let token = lines.next()?.trim().to_string();
        let pid = lines.next()?.trim().parse().ok()?;
        Some(Endpoint { port, token, pid })
    }
}

struct Shared {
    repo: Mutex<Repo>,
    /// The repository's verifying flag, readable without its lock.
    verifying: Arc<AtomicBool>,
    sessions: Mutex<HashMap<String, Actor>>,
    last_activity: AtomicU64,
    stop: AtomicBool,
    started: Instant,
}

/// Serve until idle for `idle` or until a shutdown request. Blocks.
pub fn serve(repo: Repo, idle: Duration) -> std::io::Result<()> {
    let tessra_dir: PathBuf = repo.tessra_dir.clone();
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let token = hex::encode(tessra_core::EntityId::random().0);
    let pid = std::process::id();
    let endpoint_path = tessra_dir.join("daemon");
    // Written whole and renamed into place, so a client polling for the
    // file never reads a half-written port or token.
    let staging = tessra_dir.join("daemon.tmp");
    std::fs::write(&staging, format!("{port}\n{token}\n{pid}\n"))?;
    std::fs::rename(&staging, &endpoint_path)?;

    let verifying = Arc::clone(&repo.verifying);
    let shared = Arc::new(Shared {
        repo: Mutex::new(repo),
        verifying,
        sessions: Mutex::new(HashMap::new()),
        last_activity: AtomicU64::new(0),
        stop: AtomicBool::new(false),
        started: Instant::now(),
    });

    let result = accept_loop(&listener, &shared, &token, idle);
    let _ = std::fs::remove_file(&endpoint_path);
    // Connection threads may still be flushing replies.
    std::thread::sleep(Duration::from_millis(300));
    result
}

fn accept_loop(
    listener: &TcpListener,
    shared: &Arc<Shared>,
    token: &str,
    idle: Duration,
) -> std::io::Result<()> {
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let idle_for =
            shared.started.elapsed().as_secs() - shared.last_activity.load(Ordering::SeqCst);
        if idle_for >= idle.as_secs() {
            return Ok(());
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let shared = Arc::clone(shared);
                let token = token.to_string();
                std::thread::spawn(move || handle(stream, shared, token));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
}

fn handle(stream: TcpStream, shared: Arc<Shared>, token: String) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(600)));
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let _ = writeln!(
                    writer,
                    "{}",
                    json!({ "ok": false, "code": "BAD_REQUEST", "message": e.to_string() })
                );
                continue;
            }
        };
        if req.token != token {
            let _ = writeln!(
                writer,
                "{}",
                json!({ "ok": false, "code": "UNAUTHORIZED", "message": "bad daemon token" })
            );
            continue;
        }
        shared
            .last_activity
            .store(shared.started.elapsed().as_secs(), Ordering::SeqCst);
        let out = dispatch(&shared, &req);
        let _ = writeln!(writer, "{}", out);
        let _ = writer.flush();
        if req.verb == "shutdown" {
            // Let the reply reach the client before the process goes away.
            let _ = writer.shutdown(std::net::Shutdown::Write);
            std::thread::sleep(Duration::from_millis(200));
            shared.stop.store(true, Ordering::SeqCst);
            break;
        }
    }
}

fn dispatch(shared: &Shared, req: &Request) -> Value {
    if req.verb == "shutdown" {
        return json!({ "ok": true, "result": { "stopping": true } });
    }
    if req.verb == "ping" {
        return json!({ "ok": true, "result": { "pid": std::process::id(), "uptime_s": shared.started.elapsed().as_secs() } });
    }
    // While a verifier runs, the repository is busy and nothing the code
    // under test does can reach it. Other callers retry.
    if shared.verifying.load(Ordering::SeqCst) {
        return json!({
            "ok": false,
            "code": "BUSY",
            "message": "a verifier is running in this repository; code under test may not act, and other callers should retry in a moment",
        });
    }
    let mut repo = shared.repo.lock().unwrap();
    let mut actor = match (&req.principal, &req.agent) {
        (Some(name), _) => match repo.open_external(name, req.credential.as_deref()) {
            Ok(a) => a,
            Err(e) => return json!({ "ok": false, "code": e.code(), "message": e.to_string() }),
        },
        (None, None) => {
            let mut a = repo.daemon_actor();
            a.credentialed = repo.owner_credential_ok(req.credential.as_deref());
            a
        }
        (None, Some(name)) => {
            let write = if req.write.is_empty() {
                vec!["**".to_string()]
            } else {
                req.write.clone()
            };
            let key = format!("{name}\u{0}{}", write.join("\u{0}"));
            let mut sessions = shared.sessions.lock().unwrap();
            match sessions.get(&key) {
                Some(a) => Actor {
                    signer: tessra_oplog::build::Signer {
                        principal: a.signer.principal,
                        key: tessra_core::sig::SecretKey::from_bytes(&a.signer.key.to_bytes()),
                    },
                    cap: a.cap,
                    write_paths: a.write_paths.clone(),
                    workspace: a.workspace,
                    kind: a.kind.clone(),
                    credentialed: false,
                },
                None => match repo.open_session(name, req.model.as_deref(), write) {
                    Ok(a) => {
                        sessions.insert(
                            key,
                            Actor {
                                signer: tessra_oplog::build::Signer {
                                    principal: a.signer.principal,
                                    key: tessra_core::sig::SecretKey::from_bytes(
                                        &a.signer.key.to_bytes(),
                                    ),
                                },
                                cap: a.cap,
                                write_paths: a.write_paths.clone(),
                                workspace: a.workspace,
                                kind: a.kind.clone(),
                                credentialed: false,
                            },
                        );
                        a
                    }
                    Err(e) => {
                        return json!({ "ok": false, "code": e.code(), "message": e.to_string() })
                    }
                },
            }
        }
    };
    if actor.kind == "session" && repo.is_revoked(&actor.principal()) {
        if let Some(name) = &req.agent {
            let write = if req.write.is_empty() {
                vec!["**".to_string()]
            } else {
                req.write.clone()
            };
            let key = format!("{name}\u{0}{}", write.join("\u{0}"));
            shared.sessions.lock().unwrap().remove(&key);
        }
        return json!({ "ok": false, "code": "REVOKED", "message": "this session's principal is revoked" });
    }
    if let Some(prefix) = &req.workspace {
        actor.workspace = repo
            .workspaces
            .iter()
            .find(|w| w.id.matches_prefix(prefix))
            .map(|w| w.id);
    }
    let out = verbs::call(&mut repo, &mut actor, &req.verb, &req.args);
    // Remember a workspace the session created, so later calls without an
    // explicit workspace keep using it.
    if let (Some(name), Some(ws)) = (&req.agent, actor.workspace) {
        let write = if req.write.is_empty() {
            vec!["**".to_string()]
        } else {
            req.write.clone()
        };
        let key = format!("{name}\u{0}{}", write.join("\u{0}"));
        if let Some(a) = shared.sessions.lock().unwrap().get_mut(&key) {
            a.workspace = Some(ws);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client;
    use crate::InitOptions;

    #[test]
    fn serve_and_call_over_loopback() {
        let dir = crate::paths::ScratchDir::new();
        let repo = Repo::init(
            dir.path(),
            InitOptions {
                name: "t".into(),
                import_git: false,
                history: 1,
                data_dir: Some(dir.data_dir()),
            },
        )
        .unwrap();
        let tessra_dir = repo.tessra_dir.clone();
        let handle = std::thread::spawn(move || serve(repo, Duration::from_secs(60)));
        let mut endpoint = None;
        for _ in 0..100 {
            if let Some(e) = Endpoint::read(&tessra_dir) {
                endpoint = Some(e);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let endpoint = endpoint.expect("daemon endpoint");
        let req = Request {
            token: endpoint.token.clone(),
            agent: Some("tester".into()),
            model: None,
            write: vec![],
            workspace: None,
            principal: None,
            credential: None,
            verb: "remember".into(),
            args: json!({ "kind": "gotcha", "body": "served over tcp", "scope": { "kind": "path", "ref": "src" } }),
        };
        let out = client::call(&endpoint, &req).unwrap();
        assert_eq!(out["ok"], Value::Bool(true), "{out}");
        let req2 = Request {
            token: endpoint.token.clone(),
            agent: None,
            model: None,
            write: vec![],
            workspace: None,
            principal: None,
            credential: None,
            verb: "query".into(),
            args: json!({ "kind": "memory" }),
        };
        let out = client::call(&endpoint, &req2).unwrap();
        assert_eq!(
            out["result"]["memories"][0]["body"],
            json!("served over tcp")
        );
        let bad = Request {
            token: "nope".into(),
            ..req2.clone()
        };
        let out = client::call(&endpoint, &bad).unwrap();
        assert_eq!(out["code"], json!("UNAUTHORIZED"));
        let stop = Request {
            verb: "shutdown".into(),
            ..req2
        };
        let out = client::call(&endpoint, &stop).unwrap();
        assert_eq!(out["ok"], Value::Bool(true));
        handle.join().unwrap().unwrap();
        assert!(!tessra_dir.join("daemon").exists());
    }

    #[test]
    fn the_daemon_token_makes_nobody_the_owner_or_a_human() {
        let dir = crate::paths::ScratchDir::new();
        let mut repo = Repo::init(
            dir.path(),
            InitOptions {
                name: "t".into(),
                import_git: false,
                history: 1,
                data_dir: Some(dir.data_dir()),
            },
        )
        .unwrap();
        let owner_secret = std::fs::read_to_string(repo.owner_credential_path())
            .unwrap()
            .trim()
            .to_string();
        let (_, _, maria_secret) = repo.grant_human("maria").unwrap();
        let tessra_dir = repo.tessra_dir.clone();
        let handle = std::thread::spawn(move || serve(repo, Duration::from_secs(60)));
        let mut endpoint = None;
        for _ in 0..100 {
            if let Some(e) = Endpoint::read(&tessra_dir) {
                endpoint = Some(e);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let endpoint = endpoint.expect("daemon endpoint");
        let base = Request {
            token: endpoint.token.clone(),
            agent: None,
            model: None,
            write: vec![],
            workspace: None,
            principal: None,
            credential: None,
            verb: "standard".into(),
            args: json!({ "require": ["attest(tests.pass)"] }),
        };
        // The token alone reaches the daemon and reads, and sets no policy.
        let out = client::call(&endpoint, &base).unwrap();
        assert_eq!(out["code"], json!("CREDENTIAL_REQUIRED"), "{out}");
        let out = client::call(
            &endpoint,
            &Request {
                args: json!({}),
                ..base.clone()
            },
        )
        .unwrap();
        assert_eq!(out["ok"], Value::Bool(true), "{out}");
        let out = client::call(
            &endpoint,
            &Request {
                credential: Some("guess".into()),
                ..base.clone()
            },
        )
        .unwrap();
        assert_eq!(out["code"], json!("CREDENTIAL_REQUIRED"), "{out}");
        let out = client::call(
            &endpoint,
            &Request {
                credential: Some(owner_secret.clone()),
                ..base.clone()
            },
        )
        .unwrap();
        assert_eq!(out["ok"], Value::Bool(true), "{out}");
        // Nor does it make anyone a human.
        let as_maria = Request {
            principal: Some("maria".into()),
            verb: "status".into(),
            args: json!({}),
            ..base.clone()
        };
        let out = client::call(&endpoint, &as_maria).unwrap();
        assert_eq!(out["code"], json!("CREDENTIAL_REQUIRED"), "{out}");
        let out = client::call(
            &endpoint,
            &Request {
                credential: Some(owner_secret.clone()),
                ..as_maria.clone()
            },
        )
        .unwrap();
        assert_eq!(out["code"], json!("CREDENTIAL_BAD"), "{out}");
        let out = client::call(
            &endpoint,
            &Request {
                credential: Some(maria_secret.clone()),
                ..as_maria.clone()
            },
        )
        .unwrap();
        assert_eq!(out["ok"], Value::Bool(true), "{out}");
        assert_eq!(out["state"]["kind"], json!("human"), "{out}");
        let stop = Request {
            verb: "shutdown".into(),
            ..base
        };
        client::call(&endpoint, &stop).unwrap();
        handle.join().unwrap().unwrap();
    }
}
