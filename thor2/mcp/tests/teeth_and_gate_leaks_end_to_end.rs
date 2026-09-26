//! The ways an agent could satisfy the teeth question and the evaluation
//! gate with a label instead of the work, driven end to end through the real
//! compiled `mcp` binary over its own stdio protocol - the same door an
//! assistant uses.
//!
//! MEASURED (2026-09-26, on a copy of a real store): each call below went
//! straight through. A literal no command will ever carry was stored as a
//! proven guard; a reason that fits any rule answered the teeth question of
//! a heavy one; a bulk "informing" label lifted the report gate; an old report
//! of another project, moved here, counted as this project's evaluation; a
//! heavy rule lost its severity, or was retracted, on no reason about itself;
//! a verdict was recorded on an item that never fired; and an open point
//! vanished by revising its tag away. The rule these tests hold every one of
//! those to: more real teeth, or a clear proof a rule cannot have one - never
//! a label that only satisfies a count or a gate.

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

    /// One tool call; the text of its reply.
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

/// A store in a temp folder that belongs to no git repository, with the
/// given items written straight into the log (the way a rule sits in an older
/// store, before any of today's write rules existed).
fn store_with(dir: &Path, items: &[Item]) -> PathBuf {
    let db = dir.join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    for item in items {
        let body = serde_json::to_string(item).unwrap();
        store.append_event("legacy", &item.id, "migration", EventKind::FactCreated, &item.id, None, &body).unwrap();
    }
    db
}

fn with_store(db: &Path, f: impl FnOnce(&mut EventStore)) {
    let mut store = EventStore::open_existing(db).unwrap();
    f(&mut store);
}

fn served(store: &mut EventStore, id: &str, n: usize) {
    served_in(store, "s", id, n);
}

/// Firings under a named session, the way the hook stamps them.
fn served_in(store: &mut EventStore, session: &str, id: &str, n: usize) {
    for _ in 0..n {
        store.append_event(session, "l", "hook", EventKind::ItemServed, id, None, "{}").unwrap();
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// The hook's evaluation handover, written the way the hook writes it
/// (`serve::eval_session`): `session` started evaluating `project` at the
/// log's current tip, and is filing `report_id`. The file is the contract
/// between the hook and this server, so it is written by hand here.
fn handover(db: &Path, session: &str, project: &str, report_id: &str) {
    let tip = EventStore::open_existing(db).unwrap().get_next_seq().unwrap() - 1;
    let now = now_unix();
    let json = serde_json::json!({
        "evaluations": [{"session_id": session, "project": project, "started_seq": tip, "started_unix": now, "started_by": "skill"}],
        "filings": [{"report_id": report_id, "session_id": session, "project": project, "at_unix": now}],
    });
    std::fs::write(db.parent().unwrap().join("evaluation-sessions.json"), json.to_string()).unwrap();
}

/// A check attached to `id` once and then given up, written straight into
/// the log: the history a no-literal reason needs before it counts
/// (2026-09-26, `model::gate::teeth_answer_settles`).
fn a_check_was_tried(db: &Path, item: &Item) {
    let mut tried = item.clone();
    tried.check = Some(model::item::Check::Forbidden { literals: vec![format!("{} --force", item.id)] });
    with_store(db, |s| {
        for body in [serde_json::to_string(&tried).unwrap(), serde_json::to_string(item).unwrap()] {
            s.append_mutate_checked("legacy", &item.id, "migration", EventKind::FactRevised, &item.id, None, &body).unwrap();
        }
    });
}

fn rule(id: &str, project: Option<&str>, severity: Option<Severity>, bindings: Vec<Binding>, tags: Vec<String>) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("probe rule {id} about a costly mistake"),
        bindings,
        severity,
        project: project.map(str::to_string),
        tags,
        expires: None,
        key: None,
        falsifier: Some("the mistake turns out to cost nothing".to_string()),
        check: None,
    }
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

fn path(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Path, value: value.to_string() }
}

fn command(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Command, value: value.to_string() }
}

fn eval_report_args(id: &str, project: &str) -> serde_json::Value {
    // Texts far enough apart that no two reports read as near-duplicates.
    let text = match id {
        "eval-1" => "Monday review: guards checked, verdicts given, nothing left hanging.",
        "eval-2" => "Tuesday pass over the store found two stale anchors and fixed both.",
        "eval-3" => "Wednesday audit of the open list, and a note on what still waits.",
        _ => "A new evaluation of this project, with everything settled today.",
    };
    serde_json::json!({"id": id, "kind": "report", "project": project, "tags": ["evaluation-report"], "text": text})
}

// ------------------------------------------------------------------ B1

#[test]
fn an_invented_literal_is_never_stored_as_a_proven_guard() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[])]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let reply = mcp.call(
        "remember",
        serde_json::json!({
            "id": "probe-a8", "kind": "rule", "severity": "irreversible", "project": "p",
            "text": "Probe: never repartition the data disk without a fresh backup.",
            "falsifier": "repartitioning without a backup never loses data",
            "check_kind": "forbidden", "check_literals": ["zqxj-never-typed"],
            "targets": [{"kind": "command", "value": "diskpart"}]
        }),
    );
    assert!(reply.starts_with("stored"), "{reply}");
    assert!(!reply.contains("teeth: proven guard"), "an invented literal must never count as a proven tooth: {reply}");
}

// ------------------------------------------------------------------ B2

#[test]
fn a_heavy_rules_teeth_reason_that_fits_any_rule_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[])]);
    let mut mcp = Mcp::start(&db, tmp.path());
    for thin in ["no-literal:this is a judgement rule and not a fixed text", "no-literal:it is an omission so there is nothing to type"] {
        let reply = mcp.call(
            "remember",
            serde_json::json!({
                "id": "probe-thin", "kind": "rule", "severity": "irreversible", "project": "p",
                "text": "Probe: never wipe the agent-zone index without a backup.",
                "falsifier": "wiping the index turns out to cost nothing",
                "tags": [thin],
                "targets": [{"kind": "path", "value": "files/README.md"}]
            }),
        );
        assert!(reply.contains("REFUSED"), "a reason that fits any rule is no answer: {reply}");
    }
}

/// The bulk-label loophole: a heavy rule that fired carries only the label a
/// maintenance pass writes ("informing") and an old thin reason. That is the
/// question, not an answer, so the report is refused - and the refusal sends
/// the writer to the real work, never to relabelling (C).
#[test]
fn an_informing_label_alone_never_lets_an_evaluation_through_and_the_refusal_names_real_work() {
    let tmp = tempfile::tempdir().unwrap();
    let heavy = rule(
        "labelled-heavy",
        Some("p"),
        Some(Severity::Costly),
        vec![path("files/README.md")],
        vec!["no-literal:this is a judgement rule and not a fixed text".to_string(), "teeth:informing".to_string()],
    );
    let db = store_with(tmp.path(), &[report("eval-old", "p", &["evaluation-report"]), heavy]);
    with_store(&db, |s| served(s, "labelled-heavy", 1));
    let mut mcp = Mcp::start(&db, tmp.path());
    let reply = mcp.call("remember", eval_report_args("eval-new", "p"));
    assert!(reply.contains("REFUSED") && reply.contains("labelled-heavy"), "{reply}");
    assert!(!reply.contains("backfill_teeth"), "a bulk relabel is never the way out: {reply}");
}

/// The other half of B2: a reason a reader can check against the rule - it
/// names the rule's own file - IS the clear proof that it cannot have a
/// tooth, and settles it.
#[test]
fn a_reason_that_names_the_trigger_settles_a_fired_heavy_rule() {
    let tmp = tempfile::tempdir().unwrap();
    let heavy = rule(
        "honest-heavy",
        Some("p"),
        Some(Severity::Costly),
        vec![path("files/README.md")],
        vec!["no-literal:wiping files/README.md leaves no text in any command".to_string()],
    );
    let db = store_with(tmp.path(), &[report("eval-old", "p", &["evaluation-report"]), heavy.clone()]);
    with_store(&db, |s| served(s, "honest-heavy", 1));
    let mut mcp = Mcp::start(&db, tmp.path());
    let reply = mcp.call("remember", eval_report_args("eval-new", "p"));
    assert!(
        reply.contains("REFUSED") && reply.contains("honest-heavy"),
        "a reason on a rule that never carried a check is not the answer yet: {reply}"
    );
    a_check_was_tried(&db, &heavy);
    let reply = mcp.call("remember", eval_report_args("eval-new", "p"));
    assert!(reply.starts_with("stored"), "a real reason after a real try settles it: {reply}");
}

// ------------------------------------------------------------------ C

#[test]
fn the_filing_refusal_names_the_items_that_owe_a_judgement() {
    let tmp = tempfile::tempdir().unwrap();
    let light = rule("owed-light", Some("p"), None, vec![command("fixture-cmd")], vec![]);
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[]), light]);
    with_store(&db, |s| served(s, "owed-light", 40));
    handover(&db, "s", "p", "eval-new");
    let mut mcp = Mcp::start(&db, tmp.path());
    let reply = mcp.call("remember", eval_report_args("eval-new", "p"));
    assert!(reply.contains("REFUSED") && reply.contains("'owed-light'"), "the owed item is named: {reply}");
}

// ---------------------------------------- the scoped, frozen judgement
//
// MEASURED 2026-09-26: a fresh evaluation filed its report eight times and
// was refused eight times on a list that kept growing, then stamped sixty
// items useful, some it never saw fire. These drive the fix through the
// real server, with the hook's handover written by hand.

#[test]
fn a_report_is_never_held_for_items_that_fired_only_in_another_session() {
    let tmp = tempfile::tempdir().unwrap();
    let elsewhere = rule("fired-elsewhere", Some("p"), None, vec![command("fixture-cmd")], vec![]);
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[]), elsewhere]);
    with_store(&db, |s| served_in(s, "a-parallel-session", "fired-elsewhere", 40));
    handover(&db, "this-session", "p", "eval-new");
    let mut mcp = Mcp::start(&db, tmp.path());
    let reply = mcp.call("remember", eval_report_args("eval-new", "p"));
    assert!(reply.starts_with("stored"), "another session's firings are not this report's to settle: {reply}");
    assert!(reply.contains("1 item(s) stay owed store-wide"), "and the receipt says what stays owed: {reply}");
}

#[test]
fn the_judgement_list_is_fixed_when_the_evaluation_starts() {
    let tmp = tempfile::tempdir().unwrap();
    let before = rule("owed-before-start", Some("p"), None, vec![command("fixture-cmd-a")], vec![]);
    let during = rule("remember-bound-during", Some("p"), None, vec![command("fixture-cmd-b")], vec![]);
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[]), before, during]);
    with_store(&db, |s| {
        served(s, "owed-before-start", 40);
        served(s, "remember-bound-during", 39);
    });
    handover(&db, "s", "p", "eval-new");
    // The evaluation's own writes fire it past the threshold, after the start.
    with_store(&db, |s| served(s, "remember-bound-during", 5));
    let mut mcp = Mcp::start(&db, tmp.path());
    let reply = mcp.call("remember", eval_report_args("eval-new", "p"));
    assert!(reply.contains("REFUSED") && reply.contains("'owed-before-start'"), "{reply}");
    assert!(!reply.contains("remember-bound-during"), "nothing after the start joins the list: {reply}");

    let marked = mcp.call("mark", serde_json::json!({"id": "owed-before-start"}));
    assert!(!marked.contains("REFUSED"), "{marked}");
    let reply = mcp.call("remember", eval_report_args("eval-new", "p"));
    assert!(reply.starts_with("stored"), "the fixed list is settled: {reply}");
}

#[test]
fn an_honest_skip_lets_the_report_file_and_the_item_stays_owed() {
    let tmp = tempfile::tempdir().unwrap();
    let unsure = rule("unsure-item", Some("p"), None, vec![command("fixture-cmd")], vec![]);
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[]), unsure]);
    with_store(&db, |s| served(s, "unsure-item", 40));
    handover(&db, "s", "p", "eval-new");
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = eval_report_args("eval-new", "p");
    args["tags"] = serde_json::json!(["evaluation-report", "not-judgeable:unsure-item:it fired on a log grep I never opened"]);
    let reply = mcp.call("remember", args);
    assert!(reply.starts_with("stored"), "an explicit skip with a reason settles it for this filing: {reply}");
    assert!(reply.contains("1 skipped"), "{reply}");
    assert!(reply.contains("1 item(s) stay owed store-wide"), "{reply}");
}

// ------------------------------------------------------------ worktrees

/// MEASURED 2026-09-26: a session in a worktree attached a `contains` check
/// to code that existed only there; stored, it blocked every commit on main
/// for eleven minutes. The server, started in the worktree, judges checks
/// against the main checkout and refuses one that holds only in the worktree.
#[test]
fn a_check_that_holds_only_in_the_worktree_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("Main-Repo");
    let worktree = tmp.path().join("wt-slug");
    let gitdir = main.join(".git").join("worktrees").join("wt-slug");
    std::fs::create_dir_all(&gitdir).unwrap();
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(worktree.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
    std::fs::write(main.join("panel.js"), "showSuccess();\n").unwrap();
    std::fs::write(worktree.join("panel.js"), "if (!res.ok) showError(); else showSuccess();\n").unwrap();

    let db = store_with(tmp.path(), &[report("seed-p", "Main-Repo", &[])]);
    let mut mcp = Mcp::start(&db, &worktree);
    let fact = |literal: &str| {
        serde_json::json!({
            "id": "panel-checks-res-ok", "kind": "rule", "project": "Main-Repo",
            "text": "The device panel checks res.ok before it reports success.",
            "falsifier": "the panel reports success on a failed reply",
            "check_kind": "contains", "check_path": "panel.js", "check_literal": literal,
            "targets": [{"kind": "path", "value": "panel.js"}]
        })
    };
    let reply = mcp.call("remember", fact("res.ok"));
    assert!(reply.contains("REFUSED") && reply.contains("only in your worktree"), "{reply}");
    let reply = mcp.call("remember", fact("showSuccess();"));
    assert!(reply.starts_with("stored"), "a check that holds on main too goes through: {reply}");
}

// ------------------------------------------------------------------ D

#[test]
fn moving_another_projects_report_here_is_held_to_this_projects_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let light = rule("owed-here", Some("p"), None, vec![command("fixture-cmd")], vec![]);
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[]), report("eval-elsewhere", "q", &["evaluation-report"]), light]);
    with_store(&db, |s| served(s, "owed-here", 40));
    handover(&db, "s", "p", "eval-elsewhere");
    let mut mcp = Mcp::start(&db, tmp.path());
    let reply = mcp.call("revise", serde_json::json!({"id": "eval-elsewhere", "project": "p"}));
    assert!(reply.contains("REFUSED"), "moving a filed report is a filing for the project it lands in: {reply}");
}

// ------------------------------------------------------------------ E

#[test]
fn lowering_or_clearing_a_heavy_rules_severity_needs_a_because_even_with_no_check() {
    let tmp = tempfile::tempdir().unwrap();
    let tags = vec!["no-literal:an authorised 'fixture-cmd --go' is typed like a wrong one".to_string()];
    let a = rule("heavy-a", Some("p"), Some(Severity::Costly), vec![command("fixture-cmd")], tags.clone());
    let b = rule("heavy-b", Some("p"), Some(Severity::Costly), vec![command("fixture-cmd-b")], tags);
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[]), a, b]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let lowered = mcp.call("revise", serde_json::json!({"id": "heavy-a", "severity": "house_style"}));
    assert!(lowered.contains("REFUSED") && lowered.contains("because"), "{lowered}");
    let cleared = mcp.call("revise", serde_json::json!({"id": "heavy-b", "clear_severity": true}));
    assert!(cleared.contains("REFUSED") && cleared.contains("because"), "{cleared}");
}

#[test]
fn a_heavy_rule_is_not_retracted_on_a_reason_that_fits_any_rule() {
    let tmp = tempfile::tempdir().unwrap();
    let heavy = rule(
        "heavy-c",
        Some("p"),
        Some(Severity::Costly),
        vec![path("files/README.md")],
        vec!["no-literal:wiping files/README.md leaves no text in any command".to_string()],
    );
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[]), heavy]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let reply = mcp.call("retract", serde_json::json!({"id": "heavy-c", "reason": "no longer relevant for this project"}));
    assert!(!reply.starts_with("retracted"), "{reply}");
}

// ------------------------------------------------------------------ F

#[test]
fn a_verdict_on_an_item_that_never_fired_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let never = rule("never-fired", Some("p"), None, vec![command("fixture-cmd")], vec![]);
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[]), never]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let reply = mcp.call("mark", serde_json::json!({"id": "never-fired"}));
    assert!(!reply.contains("marked useful"), "{reply}");
}

// ------------------------------------------------------------------ G

#[test]
fn an_open_point_cannot_be_revised_out_of_existence() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(tmp.path(), &[report("open-1", "p", &["open-point", "from-report:eval-1"])]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let reply = mcp.call("revise", serde_json::json!({"id": "open-1", "tags": ["from-report:eval-1"]}));
    assert!(reply.contains("REFUSED"), "an open point is closed by retracting it, never by dropping its tag: {reply}");
}

#[test]
fn a_carried_open_point_blocks_the_filing_after_the_next_one() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(tmp.path(), &[report("open-2", "p", &["open-point"])]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let first = mcp.call("remember", eval_report_args("eval-1", "p"));
    assert!(first.starts_with("stored"), "{first}");
    let carried = mcp.call("revise", serde_json::json!({"id": "open-2", "tags": ["open-point", "carried:1"]}));
    assert!(carried.starts_with("revised"), "{carried}");
    let second = mcp.call("remember", eval_report_args("eval-2", "p"));
    assert!(second.starts_with("stored"), "the carry covers one filing: {second}");
    let third = mcp.call("remember", eval_report_args("eval-3", "p"));
    assert!(third.contains("REFUSED") && third.contains("open-2"), "the carry is used up: {third}");
}
