//! `tessra`: init, the thirteen verbs, and `tessra mcp` for agents.

mod mcp;

use std::cell::RefCell;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::rc::Rc;

use clap::{Parser, Subcommand};
use std::time::Duration;

use serde_json::{json, Value};
use tessra_daemon::client;
use tessra_daemon::principals::Actor;
use tessra_daemon::server::{self, Endpoint, Request};
use tessra_daemon::{verbs, InitOptions, Repo};

#[derive(Parser)]
#[command(name = "tessra", version, about = "Version control for the era of AI")]
struct Cli {
    /// Repository path (any directory inside it). Defaults to the current directory.
    #[arg(long, global = true)]
    repo: Option<PathBuf>,
    /// Act as an agent session with this durable agent name instead of as the owner.
    #[arg(long, global = true)]
    agent: Option<String>,
    /// Model identifier for the session, recorded in provenance.
    #[arg(long, global = true)]
    model: Option<String>,
    /// Write scope for an agent session, as glob patterns. Default `**`.
    #[arg(long = "write", global = true)]
    write_paths: Vec<String>,
    /// Workspace to act in, by ID prefix.
    #[arg(long, global = true)]
    workspace: Option<String>,
    /// Act as an external principal the daemon holds a key for, such as a CI system granted with `tessra grant`.
    #[arg(long = "as", global = true)]
    as_principal: Option<String>,
    /// The credential for acting as the owner (policy verbs) or as the principal named by --as. Also read from TESSRA_CREDENTIAL, and asked for at a terminal when a call needs it.
    #[arg(long, global = true, env = "TESSRA_CREDENTIAL", hide_env_values = true)]
    credential: Option<String>,
    /// Render for humans instead of printing JSON.
    #[arg(long, global = true)]
    pretty: bool,
    /// Run in this process instead of through the repository's daemon.
    #[arg(long, global = true)]
    no_daemon: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a repository here, importing the git HEAD tree if present.
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value = "local")]
        name: String,
        #[arg(long)]
        no_git: bool,
        /// First-parent commits to import as trunk history.
        #[arg(long, default_value_t = 100)]
        history: usize,
    },
    /// Where am I: task, claims, standard status, what changed since a cursor.
    Status {
        #[arg(long)]
        since: Option<String>,
    },
    /// A context pack for a path, or for one unit (--unit name or path:name), within a token budget.
    Context {
        /// The unit to pack: its text, deps, dependents, covering tests, memories, claims, and blame.
        #[arg(long)]
        unit: Option<String>,
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value_t = 4000)]
        budget: u64,
    },
    /// The M1 query subset: memory, revision, since, object.
    Query {
        #[arg(long, default_value = "memory")]
        kind: String,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        kinds: Vec<String>,
        #[arg(long)]
        change: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        id: Option<String>,
        /// For blame, tests, and diff: the path to answer unit by unit.
        #[arg(long)]
        path: Option<String>,
        /// For bisect: the test unit, in your current revision, to find the first failing trunk revision for.
        #[arg(long)]
        test: Option<String>,
        /// For activity: how far back, e.g. 30m, 1h, 2d.
        #[arg(long)]
        window: Option<String>,
        /// For activity: summary, changes, or ops.
        #[arg(long)]
        altitude: Option<String>,
        /// For object: read a blob's text from this byte offset; the response names the next one.
        #[arg(long)]
        offset: Option<u64>,
        /// For object: the end of a blob within the budget, where a verifier's summary and errors are.
        #[arg(long)]
        tail: bool,
        #[arg(long, default_value_t = 4000)]
        budget: u64,
    },
    /// Create, list, or drop a workspace.
    Workspace {
        /// list, create, drop, or rewind (--to a revision: files and state paths back to that point).
        #[arg(long, default_value = "list")]
        action: String,
        #[arg(long, default_value = "trunk")]
        from: String,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        id: Option<String>,
        /// For create: restore the build state and other state paths the revision's snapshot carried.
        #[arg(long)]
        with_state: bool,
        /// For rewind: the revision to go back to.
        #[arg(long)]
        to: Option<String>,
    },
    /// Write a file, or replace text in one.
    Edit {
        /// The file to write. Optional with --rename, which then covers every tracked file.
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        content: Option<String>,
        #[arg(long)]
        old: Option<String>,
        #[arg(long)]
        new: Option<String>,
        #[arg(long)]
        all: bool,
        /// Semantic operation: rename this identifier everywhere (whole word), recorded on the change.
        #[arg(long)]
        rename: Option<String>,
        /// The new name for --rename.
        #[arg(long)]
        to: Option<String>,
        /// Delete the file instead of writing it.
        #[arg(long)]
        delete: bool,
        /// Mark a conflicted file resolved: removes its sidecar after writing.
        #[arg(long)]
        resolve: bool,
    },
    /// Record the workspace's state. Never blocks.
    Snapshot {
        #[arg(long)]
        title: Option<String>,
        /// After the snapshot: `verify` runs the verifiers and reports the standard clause by clause; `promote` proposes the change.
        #[arg(long)]
        then: Option<String>,
        /// Also record the toolchain, lockfile hashes, and the state paths (target, data, .venv, node_modules or config state_paths) as trees.
        #[arg(long)]
        with_state: bool,
    },
    /// Claim or release paths.
    Claim {
        #[arg(long, default_value = "claim")]
        action: String,
        #[arg(long)]
        paths: Vec<String>,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        exclusive: bool,
        #[arg(long)]
        note: Option<String>,
    },
    /// Record a memory.
    Remember {
        #[arg(long, default_value = "fact")]
        kind: String,
        #[arg(long)]
        body: String,
        #[arg(long, default_value = "repo")]
        scope_kind: String,
        #[arg(long, default_value = "")]
        scope_ref: String,
        #[arg(long, default_value_t = 0.8)]
        confidence: f64,
        #[arg(long, default_value = "shared")]
        visibility: String,
    },
    /// Run the verifiers the standard still needs and report it clause by clause.
    Verify {
        /// Only these attestation kinds.
        #[arg(long)]
        kinds: Vec<String>,
        /// Run the whole suite even when a cached or selected result would do.
        #[arg(long)]
        full: bool,
    },
    /// Show or edit the trunk standard (owner only). Clauses are `kind(name, key=value)`.
    Standard {
        /// Add a required clause, e.g. 'attest(tests.pass)' or 'attest(tests.fail_on_parent, for=new_tests)'.
        #[arg(long)]
        require: Vec<String>,
        /// Add a forbidden clause, e.g. 'structural(test.weakened)'.
        #[arg(long)]
        forbid: Vec<String>,
        /// Remove a clause by index or by its text.
        #[arg(long)]
        remove: Vec<String>,
        /// Put the added clauses under a risk level: they apply only when the change's risk is at least this (low, medium, high, critical).
        #[arg(long)]
        when: Option<String>,
        /// Edit a target's release standard instead of trunk's.
        #[arg(long)]
        target: Option<String>,
    },
    /// Show hooks, or define one (owner only): an event, an optional where clause, and actions.
    Hook {
        #[arg(long)]
        name: Option<String>,
        /// Event: snapshot, proposed, landed, conflict.opened, or a family like conflict.*.
        #[arg(long)]
        on: Option<String>,
        /// Predicate over the event, e.g. 'scope(src/**)'.
        #[arg(long = "where")]
        where_: Option<String>,
        /// Actions, e.g. 'webhook(http://ci.local/run)', 'run(./ci.sh)', 'verify()', 'attest(ci.notified)', 'task(look at this)'.
        #[arg(long = "do")]
        do_: Vec<String>,
        /// Disable the named hook.
        #[arg(long)]
        disable: bool,
        /// Re-enable the named hook.
        #[arg(long)]
        enable: bool,
    },
    /// Partition an intent across agents by blast radius: sub-intents, tasks, and scoped budgets per agent.
    Plan {
        #[arg(long)]
        intent: String,
        /// Glob patterns of the paths the intent covers. Default everything.
        #[arg(long, num_args = 1..)]
        paths: Vec<String>,
        /// How many agents to plan for.
        #[arg(long, default_value_t = 8)]
        agents: u64,
        /// Agent names to assign, in order. Default agent01..agentNN.
        #[arg(long, num_args = 1..)]
        names: Vec<String>,
        /// Op budget per agent session.
        #[arg(long, default_value_t = 2000)]
        ops: u64,
    },
    /// Show machine-local configuration, or set keys (owner only): --set key=value ...
    Config {
        #[arg(long, num_args = 1..)]
        set: Vec<String>,
    },
    /// Revoke an agent by name and unwind everything unlanded of theirs in one op (owner only).
    Revoke {
        #[arg(long)]
        name: String,
    },
    /// Grant (owner only): --external creates a CI-style principal that may only attest; --human a person who can approve; --to gives an agent's sessions a standing grant.
    Grant {
        #[arg(long)]
        external: Option<String>,
        #[arg(long)]
        human: Option<String>,
        /// Agent name to grant to.
        #[arg(long)]
        to: Option<String>,
        /// Extra verbs for the agent's sessions, e.g. plan.
        #[arg(long, num_args = 1..)]
        verbs: Vec<String>,
        /// Write scope for the agent's sessions.
        #[arg(long, num_args = 1..)]
        paths: Vec<String>,
        /// Let the agent delegate capabilities contained in its own.
        #[arg(long)]
        delegable: bool,
        /// Op budget per session.
        #[arg(long)]
        ops: Option<u64>,
    },
    /// Record an attestation under your own principal, as an external verifier would.
    Attest {
        #[arg(long)]
        kind: String,
        /// Object ID prefix; defaults to your current revision.
        #[arg(long)]
        subject: Option<String>,
        /// true, false, an integer, or text. Default true.
        #[arg(long)]
        result: Option<String>,
        #[arg(long)]
        evidence: Option<String>,
    },
    /// Speculate: several candidate edits, each verified as its own revision and ranked. --keep writes one into your workspace.
    Try {
        /// JSON array of candidates: {path, content} or {path, old, new}, each with an optional title.
        #[arg(long)]
        candidates: Option<String>,
        /// A file holding that JSON array.
        #[arg(long)]
        candidates_file: Option<PathBuf>,
        /// Write this candidate into the workspace.
        #[arg(long)]
        keep: Option<u64>,
    },
    /// Answer an approval request as a human principal (--as <name>).
    Approve {
        #[arg(long)]
        request: String,
        /// Decline instead of approving.
        #[arg(long)]
        no: bool,
        #[arg(long)]
        note: Option<String>,
    },
    /// Define a channel humans are reachable on (owner only), or list channels.
    Channel {
        #[arg(long)]
        name: Option<String>,
        /// inbox (a directory requests are written to) or webhook (a URL they are posted to).
        #[arg(long, default_value = "inbox")]
        kind: String,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        url: Option<String>,
        /// Human principals reachable on it.
        #[arg(long, num_args = 1..)]
        principals: Vec<String>,
    },
    /// Move the current revision to a stage, or deploy to a target (--to <target> --slice canary|all). --all with --to landed lands every proposed change that meets the standard (owner).
    Promote {
        /// Land every landable proposed change: the integration frontier.
        #[arg(long)]
        all: bool,
        /// For a target: canary or all.
        #[arg(long)]
        slice: Option<String>,
        #[arg(long, default_value = "proposed")]
        to: String,
    },
    /// Revert a landed change (--change) at unit granularity, or roll a target back to its previous deployment (--target).
    Revert {
        #[arg(long)]
        change: Option<String>,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// List targets and releases, or define a target (owner only).
    Target {
        #[arg(long)]
        name: Option<String>,
        /// dir(<path>) copies the snapshot into <path>/<slice>; run(<cmd>) runs a deployer with TESSRA_* in its environment.
        #[arg(long)]
        deployer: Option<String>,
        /// Observers: run(<cmd>) printing a JSON object of signals.
        #[arg(long, num_args = 1..)]
        observers: Vec<String>,
        /// Canary slice percentage.
        #[arg(long, default_value_t = 10)]
        canary: u64,
        /// Secrets for the deployer, e.g. vault:local:DEPLOY_TOKEN, read from config key vault.DEPLOY_TOKEN.
        #[arg(long, num_args = 1..)]
        secrets: Vec<String>,
    },
    /// Cut a release of the trunk head, or list releases.
    Release {
        #[arg(long)]
        name: Option<String>,
    },
    /// Run a target's observers, attest the signals, and roll back if its standard trips.
    Observe {
        #[arg(long)]
        target: String,
    },
    /// Take back your most recent op, or a given one.
    Undo {
        #[arg(long)]
        op: Option<String>,
    },
    /// Serve the thirteen verbs over MCP on stdio.
    Mcp,
    /// Render shared memory into a file for tools that do not speak Tessra, or (--format git) export trunk as a git branch.
    Export {
        /// For git: the branch to write. Default main.
        #[arg(long)]
        branch: Option<String>,
        /// For git: push the branch to this remote afterwards.
        #[arg(long)]
        push: Option<String>,
        #[arg(long, default_value = "agents-md")]
        format: String,
        #[arg(long)]
        path: Option<String>,
    },
    /// Import commits that appeared on a git branch since the last export or import, each landed on trunk (owner).
    Import {
        #[arg(long, default_value = "main")]
        branch: String,
    },
    /// Measure the M1 performance budgets and record them as an attestation.
    Bench,
    /// Serve this repository over loopback until idle. Other commands start it as needed.
    Daemon {
        #[arg(long, default_value_t = 30)]
        idle_minutes: u64,
    },
}

fn main() {
    // The clap-derived command builder and the verb dispatch have large
    // stack frames in a debug build, past the 8 MiB main-thread limit, so
    // run everything on a thread with room to spare.
    let child = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(run)
        .expect("spawn main thread");
    let code = child.join().unwrap_or(1);
    std::process::exit(code);
}

fn run() -> i32 {
    let cli = Cli::parse();
    let start = cli.repo.clone().unwrap_or_else(|| PathBuf::from("."));

    if let Cmd::Init {
        path,
        name,
        no_git,
        history,
    } = &cli.cmd
    {
        let out = match Repo::init(
            path,
            InitOptions {
                name: name.clone(),
                import_git: !no_git,
                history: *history,
                data_dir: None,
            },
        ) {
            Ok(repo) => json!({
                "ok": true,
                "result": {
                    "root": repo.root.display().to_string(),
                    "store": repo.store_dir.display().to_string(),
                    "keys": repo.keys_dir.display().to_string(),
                    "repo_id": repo.repo_id.to_letters(),
                    "daemon": repo.daemon.to_letters(),
                    "cloud_synced_repo": tessra_daemon::paths::is_cloud_synced(&repo.root),
                    "owner_credential": std::fs::read_to_string(repo.owner_credential_path()).ok().map(|s| s.trim().to_string()),
                    "owner_credential_note": format!(
                        "what makes you the owner beyond reaching the daemon: standard, hook, channel, target, grant, revoke, config --set, attest, revert, and undo ask for it (--credential, TESSRA_CREDENTIAL, or the prompt). It stays in {} until you move it somewhere agents cannot read; the daemon keeps only its hash",
                        repo.owner_credential_path().display()
                    ),
                }
            }),
            Err(e) => json!({ "ok": false, "code": e.code(), "message": e.to_string() }),
        };
        print(&out, cli.pretty);
        return 0;
    }

    let root = match client::find_root(&start) {
        Ok(r) => r,
        Err(e) => {
            print(
                &json!({ "ok": false, "code": e.code(), "message": e.to_string(), "fix": "tessra init" }),
                cli.pretty,
            );
            return 2;
        }
    };
    let tessra_dir = root.join(".tessra");

    if let Cmd::Daemon { idle_minutes } = &cli.cmd {
        if let Some(e) = Endpoint::read(&tessra_dir) {
            if client::alive(&e) {
                print(
                    &json!({ "ok": true, "result": { "already_running": true, "pid": e.pid, "port": e.port } }),
                    cli.pretty,
                );
                return 0;
            }
        }
        let repo = match Repo::open(&root) {
            Ok(r) => r,
            Err(e) => {
                print(
                    &json!({ "ok": false, "code": e.code(), "message": e.to_string() }),
                    cli.pretty,
                );
                return 2;
            }
        };
        if let Err(e) = server::serve(repo, Duration::from_secs(idle_minutes * 60)) {
            eprintln!("daemon: {e}");
            return 1;
        }
        return 0;
    }

    let write = if cli.write_paths.is_empty() {
        vec!["**".to_string()]
    } else {
        cli.write_paths.clone()
    };

    // Prefer the repository's daemon; fall back to running in this process.
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("tessra"));
    let endpoint = if cli.no_daemon {
        None
    } else {
        client::ensure(&tessra_dir, &exe, &root, true)
    };
    // The credential travels with every call; a terminal can supply it after
    // a refusal, so the cell is shared with the retry below.
    let credential: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(cli.credential.clone()));
    // One verb call, into the daemon or into this process.
    type Caller = Box<dyn FnMut(&str, &Value) -> Value>;
    let mut caller: Caller = match endpoint {
        Some(endpoint) => {
            let agent = cli.agent.clone();
            let model = cli.model.clone();
            let workspace = cli.workspace.clone();
            let principal = cli.as_principal.clone();
            let credential = Rc::clone(&credential);
            Box::new(move |verb: &str, args: &Value| {
                let req = Request {
                    token: endpoint.token.clone(),
                    agent: agent.clone(),
                    model: model.clone(),
                    write: write.clone(),
                    workspace: workspace.clone(),
                    principal: principal.clone(),
                    credential: credential.borrow().clone(),
                    verb: verb.to_string(),
                    args: args.clone(),
                };
                client::call(&endpoint, &req).unwrap_or_else(
                    |e| json!({ "ok": false, "code": "DAEMON", "message": e.to_string() }),
                )
            })
        }
        None => {
            let mut repo = match Repo::open(&root) {
                Ok(r) => r,
                Err(e) => {
                    print(
                        &json!({ "ok": false, "code": e.code(), "message": e.to_string() }),
                        cli.pretty,
                    );
                    return 2;
                }
            };
            let as_principal = cli.as_principal.clone();
            let agent = cli.agent.clone();
            let model = cli.model.clone();
            let workspace = cli.workspace.clone();
            let credential = Rc::clone(&credential);
            // The actor is opened on the first call and kept, so a workspace
            // it creates stays its own across the calls of one MCP session;
            // a credential supplied after a refusal is honoured on the next.
            let mut actor: Option<Actor> = None;
            Box::new(move |verb: &str, args: &Value| {
                let presented = credential.borrow().clone();
                if actor.is_none() {
                    let opened = match (&as_principal, &agent) {
                        (Some(name), _) => repo.open_external(name, presented.as_deref()),
                        (None, Some(name)) => {
                            repo.open_session(name, model.as_deref(), write.clone())
                        }
                        (None, None) => Ok(repo.daemon_actor()),
                    };
                    match opened {
                        Ok(mut a) => {
                            if let Some(prefix) = &workspace {
                                a.workspace = repo
                                    .workspaces
                                    .iter()
                                    .find(|w| w.id.matches_prefix(prefix))
                                    .map(|w| w.id);
                            }
                            actor = Some(a);
                        }
                        Err(e) => {
                            return json!({ "ok": false, "code": e.code(), "message": e.to_string() })
                        }
                    }
                }
                let a = actor.as_mut().expect("opened above");
                if a.kind == "daemon" && !a.credentialed {
                    a.credentialed = repo.owner_credential_ok(presented.as_deref());
                }
                verbs::call(&mut repo, a, verb, args)
            })
        }
    };

    if let Cmd::Mcp = cli.cmd {
        mcp::serve(&mut *caller);
        return 0;
    }

    let (verb, args) = to_call(&cli.cmd);
    let mut out = caller(verb, &args);
    // A person at a terminal is asked for the credential once; nothing else
    // reads it from anywhere.
    if out.get("code") == Some(&json!("CREDENTIAL_REQUIRED"))
        && credential.borrow().is_none()
        && std::io::stdin().is_terminal()
    {
        let who = cli
            .as_principal
            .clone()
            .unwrap_or_else(|| "the owner".into());
        eprint!("credential for {who}: ");
        if let Ok(secret) = rpassword::read_password() {
            let secret = secret.trim().to_string();
            if !secret.is_empty() {
                *credential.borrow_mut() = Some(secret);
                out = caller(verb, &args);
            }
        }
    }
    print(&out, cli.pretty);
    if out.get("ok") != Some(&Value::Bool(true)) {
        return 1;
    }
    0
}

fn to_call(cmd: &Cmd) -> (&'static str, Value) {
    match cmd {
        Cmd::Init { .. } | Cmd::Mcp | Cmd::Daemon { .. } => ("status", json!({})),
        Cmd::Status { since } => ("status", json!({ "since": since })),
        Cmd::Context { unit, path, budget } => (
            "context",
            json!({ "unit": unit, "path": path, "budget": budget }),
        ),
        Cmd::Query {
            kind,
            scope,
            kinds,
            change,
            since,
            id,
            path,
            test,
            window,
            altitude,
            offset,
            tail,
            budget,
        } => (
            "query",
            json!({ "kind": kind, "scope": scope.as_ref().map(|s| json!({ "ref": s })), "kinds": kinds, "change": change, "since": since, "id": id, "path": path, "test": test, "window": window, "altitude": altitude, "offset": offset, "tail": tail, "budget": budget }),
        ),
        Cmd::Workspace {
            action,
            from,
            path,
            id,
            with_state,
            to,
        } => (
            "workspace",
            json!({ "action": action, "from": from, "path": path, "id": id, "with_state": with_state, "to": to }),
        ),
        Cmd::Edit {
            path,
            content,
            old,
            new,
            all,
            rename,
            to,
            delete,
            resolve,
        } => (
            "edit",
            json!({ "path": path, "content": content, "old": old, "new": new, "all": all, "rename": rename, "to": to, "delete": delete, "resolve": resolve }),
        ),
        Cmd::Export {
            branch,
            push,
            format,
            path,
        } => (
            "export",
            json!({ "format": format, "path": path, "branch": branch, "push": push }),
        ),
        Cmd::Import { branch } => ("import", json!({ "branch": branch })),
        Cmd::Bench => ("bench", json!({})),
        Cmd::Snapshot {
            title,
            then,
            with_state,
        } => (
            "snapshot",
            json!({ "title": title, "then": then, "with_state": with_state }),
        ),
        Cmd::Claim {
            action,
            paths,
            id,
            exclusive,
            note,
        } => (
            "claim",
            json!({ "action": action, "paths": paths, "id": id, "exclusive": exclusive, "note": note }),
        ),
        Cmd::Remember {
            kind,
            body,
            scope_kind,
            scope_ref,
            confidence,
            visibility,
        } => (
            "remember",
            json!({ "kind": kind, "body": body, "scope": { "kind": scope_kind, "ref": scope_ref }, "confidence": confidence, "visibility": visibility }),
        ),
        Cmd::Verify { kinds, full } => ("verify", json!({ "kinds": kinds, "full": full })),
        Cmd::Standard {
            require,
            forbid,
            remove,
            when,
            target,
        } => (
            "standard",
            json!({ "require": require, "forbid": forbid, "remove": remove, "when": when, "target": target }),
        ),
        Cmd::Hook {
            name,
            on,
            where_,
            do_,
            disable,
            enable,
        } => (
            "hook",
            json!({ "name": name, "on": on, "where": where_, "do": do_, "enable": if *disable { Some(false) } else if *enable { Some(true) } else { None } }),
        ),
        Cmd::Grant {
            external,
            human,
            to,
            verbs,
            paths,
            delegable,
            ops,
        } => (
            "grant",
            json!({ "external": external, "human": human, "to": to, "verbs": verbs, "paths": paths, "delegable": delegable, "ops": ops }),
        ),
        Cmd::Plan {
            intent,
            paths,
            agents,
            names,
            ops,
        } => (
            "plan",
            json!({ "intent": intent, "paths": paths, "agents": agents, "names": names, "ops": ops }),
        ),
        Cmd::Revoke { name } => ("revoke", json!({ "name": name })),
        Cmd::Config { set } => ("config", json!({ "set": set })),
        Cmd::Attest {
            kind,
            subject,
            result,
            evidence,
        } => (
            "attest",
            json!({ "kind": kind, "subject": subject, "result": result, "evidence": evidence }),
        ),
        Cmd::Try {
            candidates,
            candidates_file,
            keep,
        } => {
            let text = match (candidates, candidates_file) {
                (Some(t), _) => t.clone(),
                (None, Some(p)) => std::fs::read_to_string(p).unwrap_or_default(),
                _ => "[]".into(),
            };
            let cands: Value = serde_json::from_str(&text).unwrap_or(Value::Array(vec![]));
            ("try", json!({ "candidates": cands, "keep": keep }))
        }
        Cmd::Approve { request, no, note } => (
            "approve",
            json!({ "request": request, "no": no, "note": note }),
        ),
        Cmd::Channel {
            name,
            kind,
            path,
            url,
            principals,
        } => (
            "channel",
            json!({ "name": name, "kind": kind, "path": path, "url": url, "principals": principals }),
        ),
        Cmd::Promote { all, to, slice } => {
            ("promote", json!({ "all": all, "to": to, "slice": slice }))
        }
        Cmd::Revert {
            change,
            target,
            reason,
        } => (
            "revert",
            json!({ "change": change, "target": target, "reason": reason }),
        ),
        Cmd::Target {
            name,
            deployer,
            observers,
            canary,
            secrets,
        } => (
            "target",
            json!({ "name": name, "deployer": deployer, "observers": observers, "canary": canary, "secrets": secrets }),
        ),
        Cmd::Release { name } => ("release", json!({ "name": name })),
        Cmd::Observe { target } => ("observe", json!({ "target": target })),
        Cmd::Undo { op } => ("undo", json!({ "op": op })),
    }
}

fn print(v: &Value, pretty: bool) {
    if pretty {
        println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
    } else {
        println!("{}", serde_json::to_string(v).unwrap_or_default());
    }
}
