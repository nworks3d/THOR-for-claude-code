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
    next_call: u64,
}

impl Mcp {
    fn start(db: &Path, cwd: &Path) -> Mcp {
        Mcp::start_as(db, cwd, None)
    }

    /// The server as Claude Code starts it for `session`: its session id in
    /// the environment (never inherited from the environment the tests run
    /// in), and a home of its own so no real transcript is ever read.
    fn start_as(db: &Path, cwd: &Path, session: Option<&str>) -> Mcp {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mcp"));
        cmd.arg("--db").arg(db).current_dir(cwd);
        cmd.env_remove("CLAUDE_CODE_SESSION_ID");
        if let Some(session) = session {
            cmd.env("CLAUDE_CODE_SESSION_ID", session).env("USERPROFILE", cwd).env("HOME", cwd);
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
        let mut mcp = Mcp { child, stdin, lines: rx, next_id: 1, next_call: 0 };
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
        self.call_params(serde_json::json!({"name": tool, "arguments": args}))
    }

    /// One tool call the way Claude Code makes it: its `_meta` names the
    /// calling tool_use. With no transcript on disk the server sees it
    /// unseen, and never refuses for that (step 12, K21) - the only way an
    /// evaluation report is filed at all (step 15).
    fn call_meta(&mut self, tool: &str, args: serde_json::Value) -> String {
        self.next_call += 1;
        let id = format!("toolu_leak_{}", self.next_call);
        self.call_params(serde_json::json!({"name": tool, "arguments": args, "_meta": {"claudecode/toolUseId": id}}))
    }

    fn call_params(&mut self, params: serde_json::Value) -> String {
        let v = self.request("tools/call", params);
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

/// The evaluation start, written by the very function the hook calls
/// (`serve::eval_session::start`): `session` starts evaluating `project` at
/// the log's current tip - the start record with the judgement list frozen
/// there, THOR's own pass over the project's old rules, then the batch
/// record (step 14). The server finds it by the session it runs for
/// (`Mcp::start_as`).
fn open_start(db: &Path, session: &str, project: &str) {
    with_store(db, |s| {
        let started = serve::eval_session::start(db, s, session, project, "skill").expect("the evaluation starts");
        assert!(started.fresh && started.start.frozen.decided(), "{started:?}");
    });
}

/// An evaluation start of `project` by `session` whose frozen batch hands
/// the agent every rule the live batch owes (`serve::attest::fixture::
/// live_batch_start`) - for the tests about what the FILING GATE accepts as
/// settled, which THOR's real machine pass would decide itself first.
fn open_start_with_the_live_batch(db: &Path, session: &str, project: &str) {
    with_store(db, |s| {
        serve::attest::fixture::live_batch_start(s, session, project);
    });
}

/// Record the newest verdict on `id` as attested for `session`, the way the
/// Stop hook's attester records it once the call's lines are in the
/// session's transcript (`serve::attest::record_write_verifications`).
fn attest_newest_verdict(db: &Path, id: &str, session: &str) {
    with_store(db, |s| {
        let events = s.get_events_by_entity(id).unwrap();
        let mark = events
            .iter()
            .rev()
            .find(|e| matches!(e.kind, EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise))
            .expect("a verdict to attest");
        let body = serde_json::json!({"event_seq": mark.seq, "attested": true, "session": session, "agent_id": null, "why": "fixture"});
        s.append_event(session, session, "attester", EventKind::GateStoodAside, id, None, &body.to_string()).unwrap();
    });
}

/// Register `root` as `key`'s working copy the way the installer writes it
/// (raw: a test holds no THOR actor token).
fn register(db: &Path, key: &str, root: &Path) {
    let record = report(&model::store::project_opened_id(key), key, &["project-record"]);
    let body = model::store::canonical_body(&record).unwrap();
    let body = format!(
        "{},\"root\":{},\"root_at\":\"2026-09-28T10:00:00Z\"}}",
        &body[..body.len() - 1],
        serde_json::to_string(&root.to_string_lossy()).unwrap()
    );
    with_store(db, |s| {
        s.append_event("install", "install", "installer", EventKind::FactCreated, &record.id, None, &body).unwrap();
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
    let fact = |text: &str| {
        serde_json::json!({
            "id": "probe-a8", "kind": "rule", "severity": "irreversible", "project": "p",
            "text": text,
            "falsifier": "repartitioning without a backup never loses data",
            "check_kind": "forbidden", "check_literals": ["zqxj-never-typed"],
            "targets": [{"kind": "command", "value": "diskpart"}]
        })
    };
    // Red (A4, step 8): a literal the rule's own words never name is refused.
    let reply = mcp.call("remember", fact("Probe: never repartition the data disk without a fresh backup."));
    assert!(reply.starts_with("REFUSED") && reply.contains("'zqxj-never-typed' is not named anywhere in the rule's own text"), "{reply}");
    // Green (K7): named, it is wired - never proven, whatever it is.
    let reply = mcp.call("remember", fact("Probe: never run diskpart with zqxj-never-typed without a fresh backup."));
    assert!(reply.starts_with("stored") && reply.contains("teeth: wired, not proven"), "{reply}");
    assert!(!reply.contains("teeth: proven guard"), "an invented literal must never count as a proven tooth: {reply}");
}
// ------------------------------------------------------------------ B2

/// Step 8: a reason tag is neither demanded nor judged any more - the rule
/// is TRIED, and with nothing to build a tooth from it is a note, whatever
/// reason it carries.
#[test]
fn a_teeth_reason_is_no_answer_the_rule_is_tried_instead() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[])]);
    let mut mcp = Mcp::start(&db, tmp.path());
    for (id, thin) in [("probe-thin-1", "no-literal:this is a judgement rule and not a fixed text"), ("probe-thin-2", "")] {
        let tags: Vec<&str> = if thin.is_empty() { vec![] } else { vec![thin] };
        let reply = mcp.call(
            "remember",
            serde_json::json!({
                "id": id, "kind": "rule", "severity": "irreversible", "project": "p",
                "text": if id == "probe-thin-1" {
                    "Probe: never wipe the agent-zone index without a backup.".to_string()
                } else {
                    "Probe: the cache folder stays out of every release archive.".to_string()
                },
                "falsifier": "wiping the index turns out to cost nothing",
                "tags": tags,
                "targets": [{"kind": "path", "value": "files/README.md"}]
            }),
        );
        assert!(reply.starts_with("stored") && reply.contains("teeth: note - "), "{id}: {reply}");
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
    open_start_with_the_live_batch(&db, "s", "p");
    let mut mcp = Mcp::start_as(&db, tmp.path(), Some("s"));
    let reply = mcp.call_meta("remember", eval_report_args("eval-new", "p"));
    assert!(reply.contains("REFUSED") && reply.contains("labelled-heavy"), "{reply}");
    assert!(!reply.contains("backfill_teeth"), "a bulk relabel is never the way out: {reply}");
}

/// Step 8: a reason that names the rule's own file settles NOTHING, before
/// or after a check was tried - the filing refusal names the trial as the
/// way on, and a revise naming only the id runs it; the note it lands as
/// settles the rule.
#[test]
fn a_reason_settles_nothing_and_the_trial_the_refusal_names_does() {
    let tmp = tempfile::tempdir().unwrap();
    let heavy = rule(
        "honest-heavy",
        Some("p"),
        Some(Severity::Costly),
        vec![path("files/README.md")],
        vec!["no-literal:wiping files/README.md leaves no text in any command".to_string()],
    );
    let db = store_with(tmp.path(), &[report("eval-old", "p", &["evaluation-report"]), heavy]);
    with_store(&db, |s| served(s, "honest-heavy", 1));
    open_start_with_the_live_batch(&db, "s", "p");
    let mut mcp = Mcp::start_as(&db, tmp.path(), Some("s"));
    let reply = mcp.call_meta("remember", eval_report_args("eval-new", "p"));
    assert!(reply.contains("REFUSED") && reply.contains("honest-heavy"), "a reason is no answer: {reply}");
    assert!(reply.contains("mcp__thor__revise with only the rule's id"), "{reply}");
    assert!(reply.contains("retest --id 'honest-heavy'"), "{reply}");
    // The rule is on the evaluation's frozen batch: a side door may not
    // touch it (step 15), THOR's own tool may.
    let reply = mcp.call("revise", serde_json::json!({"id": "honest-heavy"}));
    assert!(reply.starts_with("REFUSED: 'honest-heavy' is on the frozen lists of an open evaluation"), "{reply}");
    let reply = mcp.call_meta("revise", serde_json::json!({"id": "honest-heavy"}));
    assert!(reply.starts_with("revised") && reply.contains("teeth: note - "), "{reply}");
    // The Stop checks that call; then the note the trial reached settles it.
    with_store(&db, |s| {
        serve::attest::fixture::attest_every_write(s, "s");
    });
    let reply = mcp.call_meta("remember", eval_report_args("eval-new", "p"));
    assert!(reply.starts_with("stored"), "the note the trial reached settles it: {reply}");
}
// ------------------------------------------------------------------ C

#[test]
fn the_filing_refusal_names_the_items_that_owe_a_judgement() {
    let tmp = tempfile::tempdir().unwrap();
    let light = rule("owed-light", Some("p"), None, vec![command("fixture-cmd")], vec![]);
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[]), light]);
    with_store(&db, |s| served(s, "owed-light", 40));
    open_start(&db, "s", "p");
    let mut mcp = Mcp::start_as(&db, tmp.path(), Some("s"));
    let reply = mcp.call_meta("remember", eval_report_args("eval-new", "p"));
    assert!(reply.contains("REFUSED") && reply.contains("'owed-light'"), "the owed item is named: {reply}");
}

// ---------------------------------------- the scoped, frozen judgement
//
// MEASURED 2026-09-26: a fresh evaluation filed its report eight times and
// was refused eight times on a list that kept growing, then stamped sixty
// items useful, some it never saw fire. These drive the fix through the
// real server, with the hook's start record written by hand.

#[test]
fn a_report_is_never_held_for_items_that_fired_only_in_another_session() {
    let tmp = tempfile::tempdir().unwrap();
    let elsewhere = rule("fired-elsewhere", Some("p"), None, vec![command("fixture-cmd")], vec![]);
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[]), elsewhere]);
    // Decided by THOR's trial first (a note): since step 9 a rule with no
    // verdict holds the filing whatever its weight, and this test is about
    // the judgement list only.
    with_store(&db, |s| {
        serve::teeth::fixture::decide(s, "fired-elsewhere");
    });
    with_store(&db, |s| served_in(s, "a-parallel-session", "fired-elsewhere", 40));
    open_start(&db, "this-session", "p");
    let mut mcp = Mcp::start_as(&db, tmp.path(), Some("this-session"));
    let reply = mcp.call_meta("remember", eval_report_args("eval-new", "p"));
    assert!(reply.starts_with("stored"), "another session's firings are not this report's to settle: {reply}");
    assert!(reply.contains("1 item(s) stay owed store-wide"), "and the receipt says what stays owed: {reply}");
}

#[test]
fn the_judgement_list_is_fixed_when_the_evaluation_starts() {
    let tmp = tempfile::tempdir().unwrap();
    let before = rule("owed-before-start", Some("p"), None, vec![command("fixture-cmd-a")], vec![]);
    let during = rule("remember-bound-during", Some("p"), None, vec![command("fixture-cmd-b")], vec![]);
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[]), before, during]);
    // Decided by THOR's trial first (a note): since step 9 a rule with no
    // verdict holds the filing whatever its weight, and this test is about
    // the judgement list only.
    with_store(&db, |s| {
        serve::teeth::fixture::decide(s, "owed-before-start");
        serve::teeth::fixture::decide(s, "remember-bound-during");
    });
    with_store(&db, |s| {
        served(s, "owed-before-start", 40);
        served(s, "remember-bound-during", 39);
    });
    open_start(&db, "s", "p");
    // The evaluation's own writes fire it past the threshold, after the start.
    with_store(&db, |s| served(s, "remember-bound-during", 5));
    let mut mcp = Mcp::start_as(&db, tmp.path(), Some("s"));
    let reply = mcp.call_meta("remember", eval_report_args("eval-new", "p"));
    assert!(reply.contains("REFUSED") && reply.contains("'owed-before-start'"), "{reply}");
    assert!(!reply.contains("remember-bound-during"), "nothing after the start joins the list: {reply}");

    let marked = mcp.call("mark", serde_json::json!({"id": "owed-before-start"}));
    assert!(!marked.contains("REFUSED"), "{marked}");
    // Step 13: a verdict settles the list only once its recorded attestation
    // says it came through a THOR call of the evaluating session. This one
    // carries no tool_use id at all, so until a Stop has recorded it as
    // attested it settles nothing.
    let reply = mcp.call_meta("remember", eval_report_args("eval-new", "p"));
    assert!(reply.contains("REFUSED") && reply.contains("'owed-before-start'"), "an unverified verdict settles nothing: {reply}");
    attest_newest_verdict(&db, "owed-before-start", "s");
    let reply = mcp.call_meta("remember", eval_report_args("eval-new", "p"));
    assert!(reply.starts_with("stored"), "the fixed list is settled: {reply}");
}

#[test]
fn an_honest_skip_lets_the_report_file_and_the_item_stays_owed() {
    let tmp = tempfile::tempdir().unwrap();
    let unsure = rule("unsure-item", Some("p"), None, vec![command("fixture-cmd")], vec![]);
    let db = store_with(tmp.path(), &[report("seed-p", "p", &[]), unsure]);
    // Decided by THOR's trial first (a note): since step 9 a rule with no
    // verdict holds the filing whatever its weight, and this test is about
    // the judgement list only.
    with_store(&db, |s| {
        serve::teeth::fixture::decide(s, "unsure-item");
    });
    with_store(&db, |s| served(s, "unsure-item", 40));
    open_start(&db, "s", "p");
    let mut mcp = Mcp::start_as(&db, tmp.path(), Some("s"));
    let mut args = eval_report_args("eval-new", "p");
    args["tags"] = serde_json::json!(["evaluation-report", "not-judgeable:unsure-item:it fired on a log grep I never opened"]);
    let reply = mcp.call_meta("remember", args);
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
    // Since step 8 a file check is tried in its project's REGISTERED working
    // copy: the main checkout.
    register(&db, "Main-Repo", &main);
    let mut mcp = Mcp::start(&db, &worktree);
    let fact = |literal: &str| {
        serde_json::json!({
            "id": "panel-checks-res-ok", "kind": "rule", "project": "Main-Repo",
            "text": "The device panel checks res.ok before it calls showSuccess(); to report success.",
            "falsifier": "the panel reports success on a failed reply",
            "check_kind": "contains", "check_path": "panel.js", "check_literal": literal,
            "targets": [{"kind": "path", "value": "panel.js"}]
        })
    };
    let reply = mcp.call("remember", fact("res.ok"));
    assert!(reply.contains("REFUSED") && reply.contains("only in your worktree"), "{reply}");
    assert!(!reply.contains("teeth:"), "refused before any trial ran: {reply}");
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
    open_start(&db, "s", "p");
    let mut mcp = Mcp::start_as(&db, tmp.path(), Some("s"));
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

/// Step 14: an open point an agent filed before THOR wrote them itself (a
/// legacy point) holds no filing - carried or not, however many reports
/// later - and nobody files a new one.
#[test]
fn a_legacy_open_point_never_holds_a_filing() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(tmp.path(), &[report("open-2", "p", &["open-point"])]);
    let mut mcp = Mcp::start_as(&db, tmp.path(), Some("s"));
    // Three evaluations in a row, each filed through THOR's own tool.
    open_start(&db, "s", "p");
    let first = mcp.call_meta("remember", eval_report_args("eval-1", "p"));
    assert!(first.starts_with("stored"), "{first}");
    let carried = mcp.call("revise", serde_json::json!({"id": "open-2", "tags": ["open-point", "carried:1"]}));
    assert!(carried.starts_with("revised"), "a tag it already carries is not a new open point: {carried}");
    open_start(&db, "s", "p");
    let second = mcp.call_meta("remember", eval_report_args("eval-2", "p"));
    assert!(second.starts_with("stored"), "{second}");
    open_start(&db, "s", "p");
    let third = mcp.call_meta("remember", eval_report_args("eval-3", "p"));
    assert!(third.starts_with("stored"), "a legacy point holds no filing, ever: {third}");

    let fresh = mcp.call(
        "remember",
        serde_json::json!({"id": "open-3", "kind": "report", "project": "p", "tags": ["open-point"], "text": "The push rule still needs a real call, later."}),
    );
    assert!(fresh.starts_with("REFUSED: open points are written and closed by THOR itself"), "{fresh}");
    assert!(mcp.call("get", serde_json::json!({"id": "open-3"})).contains("no item"), "nothing stored");
}
