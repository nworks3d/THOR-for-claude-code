//! A reason that nothing can catch a heavy rule's mistake counts only after a
//! real try - and that try runs through THOR's own write-time proof, never as
//! a check hung live on the real store.
//!
//! MEASURED (2026-09-27, a fresh evaluation on the real store): to be allowed
//! a reason, a session hung a check it already knew was too broad - a
//! forbidden fragment on every write - and for about a minute that check
//! refused every store write that mentioned the file it named. `revise` with
//! `trial_check` now tries the check and records the try in the rule's own
//! history, and the tried check is never stored. Driven end to end through the
//! real compiled `mcp` binary, the same door an assistant uses.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use model::item::{Binding, Item, Kind, Severity, TargetKind};
use thor_core::event_store::{EventKind, EventStore};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Mcp {
    fn start(db: &Path, cwd: &Path) -> Mcp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mcp"))
            .arg("--db")
            .arg(db)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mcp");
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

    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let line = self.lines.recv_timeout(Duration::from_secs(60)).expect("the server answers within a minute");
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                return v;
            }
        }
    }

    fn call(&mut self, tool: &str, args: serde_json::Value) -> String {
        let v = self.request("tools/call", serde_json::json!({"name": tool, "arguments": args}));
        v["result"]["content"]
            .as_array()
            .map(|parts| parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"))
            .unwrap_or_else(|| v.to_string())
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn store_with(dir: &Path, items: &[Item]) -> PathBuf {
    let db = dir.join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    for item in items {
        let body = serde_json::to_string(item).unwrap();
        store.append_event("legacy", &item.id, "migration", EventKind::FactCreated, &item.id, None, &body).unwrap();
    }
    db
}

fn served(db: &Path, id: &str) {
    let mut store = EventStore::open_existing(db).unwrap();
    store.append_event("s", "l", "hook", EventKind::ItemServed, id, None, "{}").unwrap();
}

fn report(id: &str, project: &str, tags: &[&str]) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Report,
        text: format!("probe report {id}"),
        bindings: vec![],
        severity: None,
        project: Some(project.to_string()),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    }
}

/// A heavy rule on a command, with a reason that quotes the legitimate call -
/// and no check ever tried, so the reason does not count yet.
fn heavy_with_an_untried_reason() -> Item {
    Item {
        id: "deploy-needs-a-look".to_string(),
        kind: Kind::Rule,
        text: "probe rule: after fixture-deploy look at the running service, not at the version it reports".to_string(),
        bindings: vec![Binding::Target { kind: TargetKind::Command, value: "fixture-deploy".to_string() }],
        severity: Some(Severity::Costly),
        project: Some("p".to_string()),
        tags: vec!["no-catch-on-command:same text as 'fixture-deploy --prod' in the legitimate call".to_string()],
        expires: None,
        key: None,
        falsifier: Some("the reported version only changes once the service really restarted".to_string()),
        check: None,
    }
}

fn eval_report_args(id: &str) -> serde_json::Value {
    serde_json::json!({"id": id, "kind": "report", "project": "p", "tags": ["evaluation-report"],
        "text": "A new evaluation of this project, with everything settled today."})
}

#[test]
fn a_tried_check_that_fails_is_recorded_as_the_try_and_never_stored() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(tmp.path(), &[report("eval-old", "p", &["evaluation-report"]), heavy_with_an_untried_reason()]);
    served(&db, "deploy-needs-a-look");
    let mut mcp = Mcp::start(&db, tmp.path());

    let reply = mcp.call("remember", eval_report_args("eval-new"));
    assert!(reply.contains("REFUSED") && reply.contains("deploy-needs-a-look"), "a reason with no try is not the answer yet: {reply}");

    // The fragment sits inside the anchor itself, so the check could only
    // ever refuse every call on it - a failed try, and an honest one.
    let reply = mcp.call(
        "revise",
        serde_json::json!({"id": "deploy-needs-a-look", "trial_check": true, "check_kind": "forbidden",
            "check_literals": ["fixture-deploy"]}),
    );
    assert!(reply.starts_with("revised"), "{reply}");
    assert!(reply.contains("trial:"), "the receipt names the try: {reply}");

    let got = mcp.call("get", serde_json::json!({"id": "deploy-needs-a-look"}));
    assert!(!got.contains("\"check\""), "the tried check never hangs on the rule: {got}");
    let history = mcp.call("history", serde_json::json!({"id": "deploy-needs-a-look"}));
    assert!(history.contains("fact_revised"), "{history}");

    let reply = mcp.call("remember", eval_report_args("eval-new"));
    assert!(reply.starts_with("stored"), "the reason after a recorded try settles it: {reply}");
}

#[test]
fn a_tried_check_that_works_is_refused_as_a_trial_and_must_be_hung() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[]), heavy_with_an_untried_reason()]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let reply = mcp.call(
        "revise",
        serde_json::json!({"id": "deploy-needs-a-look", "trial_check": true, "check_kind": "forbidden",
            "check_literals": ["--wipe-volumes"]}),
    );
    assert!(reply.starts_with("REFUSED") && reply.contains("hang it for real"), "{reply}");
    let got = mcp.call("get", serde_json::json!({"id": "deploy-needs-a-look"}));
    assert!(!got.contains("\"check\""), "{got}");
}

#[test]
fn a_trial_needs_a_check_that_can_refuse() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[]), heavy_with_an_untried_reason()]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let reply = mcp.call("revise", serde_json::json!({"id": "deploy-needs-a-look", "trial_check": true}));
    assert!(reply.starts_with("REFUSED") && reply.contains("check_kind"), "{reply}");
    let reply = mcp.call(
        "revise",
        serde_json::json!({"id": "deploy-needs-a-look", "trial_check": true, "check_kind": "contains",
            "check_path": "README.md", "check_literal": "deploy"}),
    );
    assert!(reply.starts_with("REFUSED") && reply.contains("only a check that can refuse"), "{reply}");
}
