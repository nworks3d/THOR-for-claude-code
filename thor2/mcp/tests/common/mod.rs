//! Shared harness for the mcp integration tests that drive the real compiled
//! `mcp` binary over stdio - the same door an assistant uses - plus the
//! fixtures THOR's trial reads: registered working copies and real calls in
//! the log.
//!
//! Every fixture name is neutral (alpha, beta, gamma, demo); every git
//! repository runs with its own empty hooks path (builder rule 11), so the
//! machine's shared THOR git hooks never act on it.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use model::item::{Item, Kind};
use thor_core::event_store::{EventKind, EventStore};

/// Two UUID-shaped session ids: the corpus of real calls reads only those.
pub const OTHER_SESSION: &str = "373e8610-8130-490d-ac29-56aa98709fd1";
pub const WRITING_SESSION: &str = "0de7dc52-1b97-4b66-948f-61e4438043b2";

pub struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

pub struct Start<'a> {
    pub db: &'a Path,
    pub cwd: &'a Path,
    /// The Claude Code session id the server writes for, if any. Never
    /// inherited from the environment the tests run in.
    pub session: Option<&'a str>,
    /// Extra arguments (a replica's `--http`, `--capture-inbox`).
    pub args: Vec<String>,
}

impl Mcp {
    pub fn start(db: &Path, cwd: &Path) -> Mcp {
        Mcp::start_with(Start { db, cwd, session: None, args: vec![] })
    }

    pub fn start_with(start: Start<'_>) -> Mcp {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mcp"));
        cmd.arg("--db").arg(start.db).args(&start.args).current_dir(start.cwd);
        cmd.env_remove("CLAUDE_CODE_SESSION_ID");
        if let Some(session) = start.session {
            cmd.env("CLAUDE_CODE_SESSION_ID", session);
        }
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().expect("spawn mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut mcp = Mcp { child, stdin, lines: rx, next_id: 1 };
        mcp.request(
            "initialize",
            serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}),
        );
        mcp.send(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        mcp
    }

    fn send(&mut self, v: serde_json::Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    pub fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let line = self.lines.recv_timeout(Duration::from_secs(120)).expect("the server answers within two minutes");
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                return v;
            }
        }
    }

    /// A tool call's text - its result, or (for a call the transport itself
    /// refused, such as an unknown argument) the error message.
    pub fn call(&mut self, tool: &str, args: serde_json::Value) -> String {
        let v = self.request("tools/call", serde_json::json!({"name": tool, "arguments": args}));
        if let Some(parts) = v["result"]["content"].as_array() {
            return parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n");
        }
        if let Some(message) = v["error"]["message"].as_str() {
            return message.to_string();
        }
        v.to_string()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A fresh store at `dir/thor.db`, with a collection named for each of
/// `scopes` (a report filed under it), so writes into them are not refused
/// as a new collection.
pub fn store(dir: &Path, scopes: &[&str]) -> PathBuf {
    let db = dir.join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    for scope in scopes {
        let seed = Item {
            id: format!("seed-{scope}"),
            kind: Kind::Report,
            text: format!("the owner named the collection '{scope}'"),
            bindings: vec![],
            severity: None,
            project: Some(scope.to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        model::store::declare(&mut store, "seed", "seed", "owner", &seed).unwrap();
    }
    db
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A git checkout of project `key` at `parent/key`, holding `files`, its
/// marker naming `key`. Committed when `commit`.
pub fn checkout(parent: &Path, key: &str, files: &[(&str, &str)], commit: bool) -> PathBuf {
    let root = parent.join(key);
    std::fs::create_dir_all(&root).unwrap();
    let hooks = parent.join(format!("{key}-no-hooks"));
    std::fs::create_dir_all(&hooks).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
    std::fs::write(root.join(".thor-project"), format!("{key}\n")).unwrap();
    for (rel, content) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    if commit {
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "fixture"]);
    }
    serve::project::checkout_top(&root)
}

pub fn commit_all(root: &Path) {
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "fixture change"]);
}

/// Register `root` as `key`'s working copy the way the installer writes it
/// (raw: a test holds no THOR actor token).
pub fn register(db: &Path, key: &str, root: &Path) {
    let record = Item {
        id: model::store::project_opened_id(key),
        kind: Kind::Report,
        text: format!("Project {key}: a fixture record."),
        bindings: vec![],
        severity: None,
        project: Some(key.to_string()),
        tags: vec!["project-record".to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    let body = model::store::canonical_body(&record).unwrap();
    let body = format!(
        "{},\"root\":{},\"root_at\":\"2026-09-28T10:00:00Z\"}}",
        &body[..body.len() - 1],
        serde_json::to_string(&root.to_string_lossy()).unwrap()
    );
    let mut store = EventStore::open_existing(db).unwrap();
    store.append_event("install", "install", "installer", EventKind::FactCreated, &record.id, None, &body).unwrap();
    assert!(serve::project::registered_checkout(&store, Some(key)).is_ok(), "fixture: {key} registered at {}", root.display());
}

/// Real calls in the log: each trigger served once in `session` (a UUID).
pub fn served(db: &Path, session: &str, triggers: &[&str]) {
    let mut store = EventStore::open_existing(db).unwrap();
    for trigger in triggers {
        serve::deliver::record_delivery_with_trigger(
            &mut store,
            session,
            session,
            "hook",
            "2026-09-28T00:00:00Z",
            &["some-rule".to_string()],
            Some(trigger),
        );
    }
}

/// An item written straight into the log, the way an older build wrote it.
pub fn seed_raw(db: &Path, item: &Item) {
    let body = serde_json::to_string(item).unwrap();
    let mut store = EventStore::open_existing(db).unwrap();
    store.append_event("legacy", &item.id, "migration", EventKind::FactCreated, &item.id, None, &body).unwrap();
}

pub fn events_of(db: &Path, id: &str) -> Vec<thor_core::event_store::Event> {
    EventStore::open_existing(db).unwrap().get_events_by_entity(id).unwrap()
}

pub fn all_events(db: &Path) -> usize {
    EventStore::open_existing(db).unwrap().get_all_events().unwrap().len()
}

pub fn verdict(db: &Path, id: &str) -> model::store::CurrentVerdict {
    model::store::current_verdict(&EventStore::open_existing(db).unwrap(), id)
}

pub fn get(mcp: &mut Mcp, id: &str) -> Item {
    let text = mcp.call("get", serde_json::json!({ "id": id }));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("get {id}: {e}: {text}"))
}

/// A rule's remember arguments, with the fields every test sets.
pub fn rule(id: &str, text: &str, project: Option<&str>) -> serde_json::Value {
    let mut v = serde_json::json!({
        "id": id,
        "kind": "rule",
        "text": text,
        "falsifier": "the rule turns out to be wrong in practice",
    });
    if let Some(p) = project {
        v["project"] = serde_json::json!(p);
    }
    v
}

// ------------------------------------------------------------ the replica door

/// A replica connector (`mcp --http 127.0.0.1:0 --capture-inbox <file>`),
/// driven over its real HTTP door with a minimal client: plain HTTP/1.1 on a
/// TCP socket, the session id the transport hands back, and the one
/// `data:` line of the event stream each answer arrives in. Only for tests
/// (no HTTP client dependency is added for it).
pub struct Replica {
    child: Child,
    addr: String,
    session: Option<String>,
    next_id: u64,
}

impl Replica {
    pub fn start(db: &Path, inbox: &Path) -> Replica {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mcp"))
            .arg("--db")
            .arg(db)
            .args(["--http", "127.0.0.1:0", "--capture-inbox"])
            .arg(inbox)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the replica");
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        let addr = line
            .split("http://")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or_else(|| panic!("the replica names its address: {line}"))
            .to_string();
        std::thread::spawn(move || for _ in out.lines().map_while(Result::ok) {});
        let mut replica = Replica { child, addr, session: None, next_id: 1 };
        replica.post(serde_json::json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params":
            {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}}));
        replica.post(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        replica
    }

    /// POST one JSON-RPC message; the `data:` line of the answer, if any.
    fn post(&mut self, body: serde_json::Value) -> Option<serde_json::Value> {
        use std::io::Read;
        let body = body.to_string();
        let mut stream = std::net::TcpStream::connect(&self.addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        let session = self.session.as_ref().map(|s| format!("Mcp-Session-Id: {s}\r\n")).unwrap_or_default();
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n{session}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.addr,
            body.len()
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut raw = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw);
                    if text.contains("\"jsonrpc\"") && text.contains("}\n") && text.split("data: ").nth(1).is_some_and(|d| d.contains('\n')) {
                        break;
                    }
                    if text.starts_with("HTTP/1.1 202") && text.contains("\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let text = String::from_utf8_lossy(&raw).to_string();
        if let Some(id) = text
            .lines()
            .find_map(|l| l.to_ascii_lowercase().starts_with("mcp-session-id:").then(|| l.split_once(':').unwrap().1.trim().to_string()))
        {
            self.session = Some(id);
        }
        text.split("data: ").skip(1).find_map(|chunk| serde_json::from_str::<serde_json::Value>(chunk.lines().next().unwrap_or("")).ok())
    }

    /// A tool call's text, as the agent on the replica reads it.
    pub fn call(&mut self, tool: &str, args: serde_json::Value) -> String {
        let id = self.next_id;
        self.next_id += 1;
        let v = self
            .post(serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}}))
            .expect("the replica answers");
        if let Some(parts) = v["result"]["content"].as_array() {
            return parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n");
        }
        v["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| v.to_string())
    }
}

impl Drop for Replica {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The calls queued in a replica's capture inbox.
pub fn queued(inbox: &Path) -> Vec<thor_core::inbox::InboxOp> {
    if !inbox.exists() {
        return Vec::new();
    }
    thor_core::inbox::read_all(inbox).unwrap()
}
