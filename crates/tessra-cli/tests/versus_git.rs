//! Tessra against git, with many agents in one file.
//!
//! Each scenario runs twice from the same base file. In git, every agent is
//! a branch off the base commit and an unattended integrator merges the
//! branches into `main` oldest first, giving up on a merge that conflicts.
//! In Tessra, every agent is a session with a workspace of its own, all of
//! them working at once against the daemon the test starts, and the owner's
//! `promote --to landed --all` is the frontier. The test counts what landed
//! without a person, what needs one, and whether the result compiles, then
//! prints the comparison as a table:
//!
//! ```text
//! cargo test -p tessra-cli --test versus_git -- --nocapture
//! ```
//!
//! The assertions hold each system to what it can do. Git is expected to
//! merge edits to different functions; it is expected to stop on the
//! scenarios where every agent touches the same lines, and to merge a rename
//! against a new caller into a file that does not compile.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const TESSRA: &str = env!("CARGO_BIN_EXE_tessra");
const FILE: &str = "lib.rs";

/// Agents in the scenarios where every agent does the same kind of thing.
const AGENTS: usize = 20;

/// One distinct import per agent.
const MODULES: [&str; AGENTS] = [
    "io", "env", "mem", "fs", "cmp", "ops", "iter", "str", "num", "hash", "any", "cell", "rc",
    "sync", "time", "thread", "path", "ptr", "slice", "vec",
];

/// The base file: one function per agent, each with a doc line, so any two
/// edits to different functions are separated by unchanged lines and git's
/// line merge has a fair chance.
fn base() -> String {
    let mut s = String::from("use std::fmt;\n\n");
    for i in 0..AGENTS {
        s.push_str(&format!(
            "/// Function {i}.\npub fn f{i:02}(x: i32) -> i32 {{\n    x + {i}\n}}\n\n"
        ));
    }
    s
}

fn body(i: usize) -> String {
    format!("    x + {i}\n")
}

#[derive(Clone)]
enum Action {
    /// Write the whole file, as any editor or agent tool does.
    Write(String),
    /// Rename an identifier: `edit --rename` in Tessra, which records the
    /// operation on the change; a textual rename in git, which has nothing
    /// else to offer.
    Rename {
        from: &'static str,
        to: &'static str,
    },
}

#[derive(Clone)]
struct Agent {
    name: String,
    title: String,
    action: Action,
}

struct Scenario {
    name: &'static str,
    agents: Vec<Agent>,
}

fn scenarios() -> Vec<Scenario> {
    let base = base();
    let each = |title: &dyn Fn(usize) -> String, action: &dyn Fn(usize) -> String| -> Vec<Agent> {
        (0..AGENTS)
            .map(|i| Agent {
                name: format!("agent{i:02}"),
                title: title(i),
                action: Action::Write(action(i)),
            })
            .collect()
    };
    let write = |name: &str, title: &str, text: String| Agent {
        name: name.into(),
        title: title.into(),
        action: Action::Write(text),
    };
    let reformatted = (0..AGENTS).fold(base.clone(), |t, i| {
        t.replace(
            &format!("pub fn f{i:02}(x: i32) -> i32 {{\n    x + {i}\n}}"),
            &format!("pub fn f{i:02}(x: i32) -> i32 {{ x + {i} }}"),
        )
    });
    vec![
        Scenario {
            name: "each agent edits its own function",
            agents: each(&|i| format!("f{i:02}: add a thousand"), &|i| {
                base.replace(&body(i), &format!("    x + {i} + 1000\n"))
            }),
        },
        Scenario {
            name: "each agent adds a function",
            agents: each(&|i| format!("add g{i:02}"), &|i| {
                format!(
                    "{base}/// Added by agent {i}.\npub fn g{i:02}(x: i32) -> i32 {{\n    x * {i}\n}}\n\n"
                )
            }),
        },
        Scenario {
            name: "each agent adds an import",
            agents: each(&|i| format!("f{i:02}: import {}", MODULES[i]), &|i| {
                base.replacen(
                    "use std::fmt;\n",
                    &format!("use std::fmt;\nuse std::{};\n", MODULES[i]),
                    1,
                )
                .replace(&body(i), &format!("    x + {i} + 1000\n"))
            }),
        },
        Scenario {
            name: "one renames, another calls the old name",
            agents: vec![
                Agent {
                    name: "renamer".into(),
                    title: "rename f01 to one_more".into(),
                    action: Action::Rename {
                        from: "f01",
                        to: "one_more",
                    },
                },
                write(
                    "caller",
                    "uses calls f01",
                    format!("{base}/// Uses f01.\npub fn uses(x: i32) -> i32 {{\n    f01(x) * 2\n}}\n\n"),
                ),
            ],
        },
        Scenario {
            name: "one reformats the file, another edits a function",
            agents: vec![
                write("formatter", "one line per function", reformatted),
                write(
                    "editor",
                    "f10: add a thousand",
                    base.replace(&body(10), "    x + 10 + 1000\n"),
                ),
            ],
        },
        Scenario {
            name: "two edit the same function, a third edits another",
            agents: vec![
                write("first", "f05: add a hundred", base.replace(&body(5), "    x + 5 + 100\n")),
                write(
                    "second",
                    "f05: add two hundred",
                    base.replace(&body(5), "    x + 5 + 200\n"),
                ),
                write(
                    "bystander",
                    "f15: add three hundred",
                    base.replace(&body(15), "    x + 15 + 300\n"),
                ),
            ],
        },
    ]
}

/// What one system made of a scenario.
struct Outcome {
    /// Changes on trunk (git: `main`) without anyone stepping in.
    landed: usize,
    /// Git: merges the integrator could not finish. Tessra: conflict tasks
    /// the frontier opened, each carrying both sides.
    stuck: usize,
    /// Tessra: the semantic resolutions the landings reported.
    semantic: Vec<String>,
    /// The file on trunk afterwards.
    text: String,
    /// Whether that file compiles, when `rustc` is available.
    compiles: Option<bool>,
    elapsed: Duration,
    notes: Vec<String>,
}

// ---------------------------------------------------------------- git --

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t.local",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git")
}

fn git_ok(dir: &Path, args: &[&str]) {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_git(scenario: &Scenario, base: &str) -> Outcome {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_ok(root, &["init", "-q"]);
    git_ok(root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    fs::write(root.join(FILE), base).unwrap();
    git_ok(root, &["add", FILE]);
    git_ok(root, &["commit", "-q", "-m", "base"]);

    let started = Instant::now();
    // Every agent: a branch off the base with one commit.
    for agent in &scenario.agents {
        git_ok(root, &["checkout", "-q", "-b", &agent.name, "main"]);
        let text = match &agent.action {
            Action::Write(text) => text.clone(),
            Action::Rename { from, to } => base.replace(from, to),
        };
        fs::write(root.join(FILE), text).unwrap();
        git_ok(root, &["commit", "-q", "-a", "-m", &agent.title]);
        git_ok(root, &["checkout", "-q", "main"]);
    }
    // The integrator: merge each branch, oldest first. A merge that
    // conflicts is abandoned; it needs a person.
    let mut landed = 0;
    let mut stuck = 0;
    for agent in &scenario.agents {
        let out = git(root, &["merge", "-q", "--no-edit", &agent.name]);
        if out.status.success() {
            landed += 1;
        } else {
            stuck += 1;
            git_ok(root, &["merge", "--abort"]);
        }
    }
    let elapsed = started.elapsed();
    let text = fs::read_to_string(root.join(FILE)).unwrap();
    Outcome {
        landed,
        stuck,
        semantic: vec![],
        compiles: compiles(&text),
        text,
        elapsed,
        notes: vec![],
    }
}

// ------------------------------------------------------------- tessra --

/// The repository's daemon, started by the test and stopped when dropped,
/// so agents run as separate processes against one endpoint and nothing
/// outlives the test.
struct Daemon {
    child: Child,
    port: u16,
    token: String,
}

impl Daemon {
    fn start(root: &Path, data: &Path) -> Daemon {
        let child = Command::new(TESSRA)
            .args(["daemon", "--idle-minutes", "5"])
            .current_dir(root)
            .env("TESSRA_DATA_DIR", data)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn tessra daemon");
        // The guard owns the child from here, so a daemon that never comes
        // up is stopped by the panic's unwinding like any other.
        let mut daemon = Daemon {
            child,
            port: 0,
            token: String::new(),
        };
        let file = root.join(".tessra").join("daemon");
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(60) {
            if let Ok(text) = fs::read_to_string(&file) {
                let mut lines = text.lines();
                let port = lines.next().and_then(|l| l.trim().parse::<u16>().ok());
                let token = lines.next().map(|l| l.trim().to_string());
                let pid = lines.next().and_then(|l| l.trim().parse::<u32>().ok());
                if let (Some(port), Some(token), Some(pid)) = (port, token, pid) {
                    if pid == daemon.child.id() && request(port, &token, "ping").is_some() {
                        daemon.port = port;
                        daemon.token = token;
                        return daemon;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("the daemon did not come up within a minute");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if self.port != 0 {
            let _ = request(self.port, &self.token, "shutdown");
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One request straight to the daemon, for `ping` and `shutdown`.
fn request(port: u16, token: &str, verb: &str) -> Option<Value> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    writeln!(
        stream,
        "{}",
        json!({ "token": token, "verb": verb, "args": {} })
    )
    .ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    let reply: Value = serde_json::from_str(&line).ok()?;
    (reply["ok"] == json!(true)).then_some(reply)
}

struct Cli<'a> {
    root: &'a Path,
    data: &'a Path,
}

impl Cli<'_> {
    /// Run one verb and return its response, which must be `ok`.
    fn run(&self, agent: Option<&str>, workspace: Option<&str>, args: &[&str]) -> Value {
        let mut cmd = Command::new(TESSRA);
        cmd.current_dir(self.root).env("TESSRA_DATA_DIR", self.data);
        if let Some(agent) = agent {
            cmd.args(["--agent", agent]);
        }
        if let Some(workspace) = workspace {
            cmd.args(["--workspace", workspace]);
        }
        cmd.args(args);
        let out = cmd.output().expect("run tessra");
        let reply: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "tessra {args:?} printed no JSON ({e}): stdout {:?}, stderr {:?}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        assert_eq!(
            reply["ok"],
            json!(true),
            "tessra {args:?} as {agent:?}: {reply}"
        );
        reply
    }

    fn workspace(&self, agent: &str) -> (String, PathBuf) {
        let reply = self.run(Some(agent), None, &["workspace", "--action", "create"]);
        let id = reply["result"]["workspace"]
            .as_str()
            .expect("workspace id")
            .to_string();
        let path = PathBuf::from(reply["result"]["path"].as_str().expect("workspace path"));
        (id, path)
    }
}

fn run_tessra(scenario: &Scenario, base: &str) -> Outcome {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let data = dir.path().join("data");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&data).unwrap();
    fs::write(root.join(FILE), base).unwrap();
    let cli = Cli {
        root: &root,
        data: &data,
    };
    cli.run(None, None, &["--no-daemon", "init", "--no-git"]);
    let daemon = Daemon::start(&root, &data);
    // The owner lands the base as the first trunk revision.
    cli.run(None, None, &["snapshot", "--title", "base"]);
    cli.run(None, None, &["promote", "--to", "landed"]);

    let started = Instant::now();
    // Every agent at once: a workspace of its own, its edit, and a snapshot
    // that proposes the change. Nobody waits for anybody.
    let workspaces: Vec<(String, String, PathBuf)> = std::thread::scope(|s| {
        let handles: Vec<_> = scenario
            .agents
            .iter()
            .map(|agent| {
                let cli = &cli;
                s.spawn(move || {
                    let (id, path) = cli.workspace(&agent.name);
                    match &agent.action {
                        Action::Write(text) => {
                            cli.run(
                                Some(&agent.name),
                                Some(&id),
                                &["edit", "--path", FILE, "--content", text],
                            );
                        }
                        Action::Rename { from, to } => {
                            let reply = cli.run(
                                Some(&agent.name),
                                Some(&id),
                                &["edit", "--rename", from, "--to", to],
                            );
                            assert_eq!(reply["result"]["recorded"], json!(true), "{reply}");
                        }
                    }
                    cli.run(
                        Some(&agent.name),
                        Some(&id),
                        &["snapshot", "--title", &agent.title, "--then", "promote"],
                    );
                    (agent.name.clone(), id, path)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("agent thread"))
            .collect()
    });

    // The frontier: land every proposed change that meets the standard.
    let frontier = cli.run(None, None, &["promote", "--to", "landed", "--all"]);
    let elapsed = started.elapsed();
    let results = frontier["result"]["results"]
        .as_array()
        .expect("frontier results");
    assert_eq!(results.len(), scenario.agents.len(), "{frontier}");
    let landed = results
        .iter()
        .filter(|r| r["landed"] == json!(true))
        .count();
    assert_eq!(frontier["result"]["landed"], json!(landed), "{frontier}");
    let conflicted: Vec<&Value> = results
        .iter()
        .filter(|r| r["conflicts"].as_array().is_some_and(|c| !c.is_empty()))
        .collect();
    let semantic: Vec<String> = results
        .iter()
        .filter_map(|r| r["semantic"].as_array())
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect();
    let mut notes = Vec::new();
    for r in results {
        if r["landed"] != json!(true) && r["conflicts"].is_null() {
            notes.push(format!("{} refused: {}", r["title"], r["why"]));
        }
    }
    // A conflicted change is not lost: its workspace holds both sides with
    // markers and a sidecar, and a task names the unit.
    for r in &conflicted {
        let title = r["title"].as_str().unwrap_or_default();
        let (_, _, path) = workspaces
            .iter()
            .find(|(name, ..)| {
                scenario
                    .agents
                    .iter()
                    .any(|a| a.name == *name && a.title == title)
            })
            .expect("the conflicted change's workspace");
        assert!(
            path.join(format!("{FILE}.tessra-conflict")).exists(),
            "{title}: no conflict sidecar in {}",
            path.display()
        );
        let marked = fs::read_to_string(path.join(FILE)).unwrap();
        assert!(
            marked.contains("<<<<<<<") && marked.contains("|||||||"),
            "{title}: {marked}"
        );
        notes.push(format!(
            "{title}: {}, both sides in its workspace",
            r["conflicts"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !conflicted.is_empty() {
        let memories = cli.run(None, None, &["query", "--kind", "memory"]);
        let tasks = memories["result"]["memories"]
            .as_array()
            .map(|m| m.iter().filter(|m| m["kind"] == json!("task")).count())
            .unwrap_or(0);
        assert_eq!(tasks, conflicted.len(), "one task per conflict: {memories}");
    }

    // Trunk, as the next agent to arrive would see it.
    let (_, reader) = cli.workspace("reader");
    let text = fs::read_to_string(reader.join(FILE)).unwrap();
    drop(daemon);
    Outcome {
        landed,
        stuck: conflicted.len(),
        semantic,
        compiles: compiles(&text),
        text,
        elapsed,
        notes,
    }
}

// ------------------------------------------------------------- report --

/// Whether the file is a Rust library that type-checks. `None` when no
/// `rustc` is reachable, which never fails the test.
fn compiles(text: &str) -> Option<bool> {
    let dir = tempfile::tempdir().ok()?;
    let src = dir.path().join(FILE);
    fs::write(&src, text).ok()?;
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let out = Command::new(rustc)
        .args([
            "--edition",
            "2021",
            "--crate-type",
            "lib",
            "--emit=metadata",
            "-o",
        ])
        .arg(dir.path().join("lib.rmeta"))
        .arg(&src)
        .output()
        .ok()?;
    Some(out.status.success())
}

fn verdict(o: &Outcome) -> &'static str {
    match o.compiles {
        Some(true) => "compiles",
        Some(false) => "does not compile",
        None => "not compiled",
    }
}

fn row(name: &str, agents: usize, git: &Outcome, tessra: &Outcome) -> String {
    format!(
        "| {name} | {agents} | {} / {} | {} / {} | {} | {} |",
        git.landed,
        git.stuck,
        tessra.landed,
        tessra.stuck,
        verdict(git),
        verdict(tessra),
    )
}

#[test]
fn many_agents_in_one_file_git_versus_tessra() {
    let base = base();
    let mut table = vec![
        "| scenario | agents | git: landed / needs a person | tessra: landed / conflict tasks | git result | tessra result |".to_string(),
        "|---|---|---|---|---|---|".to_string(),
    ];
    let mut timings = Vec::new();
    for scenario in scenarios() {
        let n = scenario.agents.len();
        let git = run_git(&scenario, &base);
        let tessra = run_tessra(&scenario, &base);
        let line = row(scenario.name, n, &git, &tessra);
        println!("{line}");
        for note in git
            .notes
            .iter()
            .chain(&tessra.notes)
            .chain(&tessra.semantic)
        {
            println!("|   | | | | | {note} |");
        }
        table.push(line);
        timings.push(format!(
            "{}: git {} ms, tessra {} ms",
            scenario.name,
            git.elapsed.as_millis(),
            tessra.elapsed.as_millis()
        ));

        let each = |o: &Outcome, f: &dyn Fn(usize) -> String| -> Vec<usize> {
            (0..AGENTS).filter(|&i| !o.text.contains(&f(i))).collect()
        };
        match scenario.name {
            "each agent edits its own function" => {
                // The control: unchanged lines separate the edits, and both
                // systems land everything.
                let missing = |o: &Outcome| each(o, &|i| format!("    x + {i} + 1000\n"));
                assert_eq!((git.landed, git.stuck), (n, 0), "{}", git.text);
                assert_eq!((tessra.landed, tessra.stuck), (n, 0), "{}", tessra.text);
                assert!(missing(&git).is_empty(), "git lost {:?}", missing(&git));
                assert!(
                    missing(&tessra).is_empty(),
                    "tessra lost {:?}",
                    missing(&tessra)
                );
                assert_ne!(git.compiles, Some(false), "{}", git.text);
                assert_ne!(tessra.compiles, Some(false), "{}", tessra.text);
            }
            "each agent adds a function" => {
                // Every addition is at the end of the file: the same lines
                // for git, a position after the same unit for Tessra.
                let missing = |o: &Outcome| {
                    each(o, &|i| {
                        format!("pub fn g{i:02}(x: i32) -> i32 {{\n    x * {i}\n}}\n")
                    })
                };
                assert_eq!((git.landed, git.stuck), (1, n - 1), "{}", git.text);
                assert_eq!(missing(&git).len(), n - 1, "{}", git.text);
                assert_eq!((tessra.landed, tessra.stuck), (n, 0), "{}", tessra.text);
                assert!(
                    missing(&tessra).is_empty(),
                    "tessra lost {:?}",
                    missing(&tessra)
                );
                assert_ne!(tessra.compiles, Some(false), "{}", tessra.text);
            }
            "each agent adds an import" => {
                // Every import goes after the same line. Tessra merges the
                // import block as a set.
                let missing = |o: &Outcome| each(o, &|i| format!("use std::{};\n", MODULES[i]));
                assert_eq!((git.landed, git.stuck), (1, n - 1), "{}", git.text);
                assert_eq!(missing(&git).len(), n - 1, "{}", git.text);
                assert_eq!((tessra.landed, tessra.stuck), (n, 0), "{}", tessra.text);
                assert!(
                    missing(&tessra).is_empty(),
                    "tessra lost {:?}",
                    missing(&tessra)
                );
                let edits = each(&tessra, &|i| format!("    x + {i} + 1000\n"));
                assert!(edits.is_empty(), "tessra lost the edits {edits:?}");
                assert_ne!(tessra.compiles, Some(false), "{}", tessra.text);
            }
            "one renames, another calls the old name" => {
                // Git merges both cleanly and the caller names a function
                // that no longer exists. Tessra carries the rename into the
                // caller because the rename was recorded as an operation.
                assert_eq!((git.landed, git.stuck), (2, 0), "{}", git.text);
                assert!(git.text.contains("f01(x) * 2"), "{}", git.text);
                assert!(!git.text.contains("pub fn f01("), "{}", git.text);
                assert_ne!(
                    git.compiles,
                    Some(true),
                    "git's merge should not compile:\n{}",
                    git.text
                );
                assert_eq!((tessra.landed, tessra.stuck), (2, 0), "{}", tessra.text);
                assert!(tessra.text.contains("one_more(x) * 2"), "{}", tessra.text);
                assert!(tessra.text.contains("pub fn one_more("), "{}", tessra.text);
                // The doc comment still says "Uses f01": a rename touches
                // identifiers, never comments or strings.
                assert!(!tessra.text.contains("f01("), "{}", tessra.text);
                assert!(tessra.text.contains("/// Uses f01."), "{}", tessra.text);
                assert!(
                    tessra
                        .semantic
                        .iter()
                        .any(|s| s.contains("f01 renamed to one_more")),
                    "{:?}",
                    tessra.semantic
                );
                assert_ne!(tessra.compiles, Some(false), "{}", tessra.text);
            }
            "one reformats the file, another edits a function" => {
                // A reformat touches every line, so git conflicts with any
                // edit. Unit identity is format-insensitive: the edit wins in
                // its unit and the reformat stands everywhere else.
                assert_eq!((git.landed, git.stuck), (1, 1), "{}", git.text);
                assert_eq!((tessra.landed, tessra.stuck), (2, 0), "{}", tessra.text);
                assert!(
                    tessra.text.contains("pub fn f00(x: i32) -> i32 { x + 0 }"),
                    "{}",
                    tessra.text
                );
                assert!(
                    tessra.text.contains("pub fn f19(x: i32) -> i32 { x + 19 }"),
                    "{}",
                    tessra.text
                );
                assert!(
                    tessra.text.contains("    x + 10 + 1000\n"),
                    "{}",
                    tessra.text
                );
                assert_ne!(tessra.compiles, Some(false), "{}", tessra.text);
            }
            "two edit the same function, a third edits another" => {
                // A real collision. Git leaves the integrator with a half
                // done merge; Tessra lands the first, opens a conflict task
                // for the second with both sides in its workspace, and lands
                // the bystander without waiting.
                assert_eq!((git.landed, git.stuck), (2, 1), "{}", git.text);
                assert_eq!((tessra.landed, tessra.stuck), (2, 1), "{}", tessra.text);
                assert!(
                    tessra.text.contains("    x + 15 + 300\n"),
                    "the bystander landed:\n{}",
                    tessra.text
                );
                let firsts = ["    x + 5 + 100\n", "    x + 5 + 200\n"];
                assert_eq!(
                    firsts.iter().filter(|s| tessra.text.contains(*s)).count(),
                    1,
                    "exactly one side of the collision is on trunk:\n{}",
                    tessra.text
                );
                assert!(
                    tessra.notes.iter().any(|n| n.contains("function f05")),
                    "{:?}",
                    tessra.notes
                );
                assert_ne!(tessra.compiles, Some(false), "{}", tessra.text);
            }
            other => panic!("no assertions for scenario {other:?}"),
        }
    }
    println!();
    for line in &table {
        println!("{line}");
    }
    println!();
    for line in &timings {
        println!("{line}");
    }
}
