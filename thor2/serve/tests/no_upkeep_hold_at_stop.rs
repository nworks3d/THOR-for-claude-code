//! NO JUDGEMENT ASK AND NO RULE-UPKEEP ASK AT A TURN'S END (step 28), end to end
//! through the real compiled `serve hook` binary.
//!
//! The owner's decision, 2026-10-02: "1x per sessie is meer dan voldoende",
//! "wat ik wil vermijden is dat een sessie nu elk kwartier iets krijgt om te
//! beoordelen", "liever 1 blok dan random continu lopen zeuren - daar dient de
//! eval voor". So no Stop holds a turn for the items that fired repeatedly
//! since they were last judged, nor for the old rules THOR's trial has not
//! decided: both are asked inside the project's evaluation only (its frozen
//! judgement list, its batch and its A4 list), at most once per 16 hours per
//! project. A fact also counts once per SESSION toward the 40 that make a
//! verdict owed.
//!
//! Every test runs the hook with `HOME`/`USERPROFILE` pointed at a throwaway
//! sandbox. The payloads that must prove the judgement behaviour alone name a
//! no-project `cwd` (the evaluation ask is never made outside a project).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use model::item::{Binding, Item, Kind, TargetKind};
use thor_core::event_store::EventStore;

const AFTER: usize = serve::usefulness::JUDGEMENT_DEBT_AFTER;

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self { home: tempfile::tempdir().unwrap() }
    }

    /// A real, empty directory with no marker and no `.git` above it, so the
    /// folder belongs to no project.
    fn global_cwd(&self) -> PathBuf {
        let dir = self.home.path().join("no-project-cwd");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

fn run_hook(db: &Path, payload: &str, sandbox: &Sandbox) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(db).arg("hook").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.env("HOME", sandbox.home.path()).env("USERPROFILE", sandbox.home.path());
    let mut child = cmd.spawn().expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    assert!(out.stderr.is_empty(), "the hook must never write to stderr: {:?}", out.stderr);
    String::from_utf8(out.stdout).unwrap()
}

fn stop_payload(session_id: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": false,
        "last_assistant_message": "",
        "cwd": cwd.to_string_lossy(),
    })
    .to_string()
}

fn read_payload(session: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Read",
        "tool_input": {"file_path": cwd.join("notes.md").to_string_lossy()},
    })
    .to_string()
}

fn is_a_hold(out: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(out.trim()).ok().and_then(|v| v.get("decision").cloned())
        == Some(serde_json::json!("block"))
}

/// `n` Always-bound (pinned) GLOBAL rules, each served in `AFTER` sessions
/// (`session_id` among them) - enough on their own to be owed a verdict were
/// they not pinned.
fn declare_pinned_items(store: &mut EventStore, n: usize, session_id: &str) {
    for i in 0..n {
        let id = format!("pinned-{i:02}");
        let item = Item {
            id: id.clone(),
            kind: Kind::Rule,
            text: format!("fixture pinned item number {i}"),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some(format!("fixture pinned item number {i} turns out to be wrong")),
            check: None,
        };
        model::store::declare(store, "fixture", "fixture", "fixture", &item).unwrap();
        serve::teeth::fixture::decide(store, &id);
        serve_in_sessions(store, &id, session_id, AFTER);
    }
}

/// `n` trigger-bound rules, each on its own `Command` anchor, served in
/// `AFTER` sessions (`session_id` among them).
fn declare_trigger_items(store: &mut EventStore, n: usize, session_id: &str) {
    for i in 0..n {
        let id = format!("trigger-{i:02}");
        let item = Item {
            id: id.clone(),
            kind: Kind::Rule,
            text: format!("fixture trigger-bound item number {i}"),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-trigger-command-{i:02}") }],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some(format!("fixture trigger-bound item number {i} turns out to be wrong")),
            check: None,
        };
        model::store::declare(store, "fixture", "fixture", "fixture", &item).unwrap();
        serve::teeth::fixture::decide(store, &id);
        serve_in_sessions(store, &id, session_id, AFTER);
    }
}

/// `times` servings of `id`, each in a session of its own, `session_id` first.
fn serve_in_sessions(store: &mut EventStore, id: &str, session_id: &str, times: usize) {
    for i in 0..times {
        serve::deliver::record_delivery(
            store,
            &serve::deliver::nth_session(session_id, i),
            session_id,
            serve::usefulness::ASSISTANT_DELIVERY_ACTOR,
            "2026-09-08T00:00:00Z",
            &[id.to_string()],
        );
    }
}

fn judgement_list(db: &Path, session_id: &str) -> Vec<String> {
    let store = EventStore::new(db).unwrap();
    let tip = store.get_next_seq().unwrap();
    model::store::evaluation_judgement_list(&store, None, session_id, tip)
}

#[test]
fn a_store_of_only_pinned_items_holds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    declare_pinned_items(&mut store, 15, "s1");
    drop(store);
    let sandbox = Sandbox::new();
    let out = run_hook(&db, &stop_payload("s1", &sandbox.global_cwd()), &sandbox);
    assert!(out.trim().is_empty(), "{out}");
}

/// THE DECISION: items owed a verdict wait for the evaluation. Nothing holds
/// the turn, and the evaluation's frozen judgement list still names the
/// trigger-bound items this session saw and never a pinned one.
#[test]
fn items_owed_a_verdict_never_hold_a_stop_and_wait_for_the_evaluation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    declare_pinned_items(&mut store, 3, "s1");
    declare_trigger_items(&mut store, 3, "s1");
    drop(store);
    let sandbox = Sandbox::new();

    for _ in 0..3 {
        let out = run_hook(&db, &stop_payload("s1", &sandbox.global_cwd()), &sandbox);
        assert!(!is_a_hold(&out), "no Stop holds a turn for a verdict any more: {out}");
        assert!(!out.contains("fired repeatedly"), "{out}");
    }
    let list = judgement_list(&db, "s1");
    assert_eq!(list, vec!["trigger-00".to_string(), "trigger-01".to_string(), "trigger-02".to_string()]);
}

/// A fact counts once per SESSION: sixty showings in one session owe nothing,
/// 40 sessions that each saw it once do.
#[test]
fn a_fact_counts_once_per_session_toward_the_verdict_it_owes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    declare_trigger_items(&mut store, 1, "s1");
    // The same fact, sixty more showings, all in this one session.
    for _ in 0..60 {
        serve::deliver::record_delivery(
            &mut store,
            "s2",
            "s2",
            serve::usefulness::ASSISTANT_DELIVERY_ACTOR,
            "2026-09-08T00:00:00Z",
            &["trigger-00".to_string()],
        );
    }
    let counts = serve::usefulness::served_since_last_verdict(&store);
    assert_eq!(counts.get("trigger-00"), Some(&(AFTER + 1)), "AFTER sessions plus s2, however often each one was shown it");
    drop(store);
    assert_eq!(judgement_list(&db, "s2"), vec!["trigger-00".to_string()]);

    // And the other way round: sixty showings, all in ONE session, owe nothing.
    let id = "trigger-00".to_string();
    let third = tempfile::tempdir().unwrap();
    let db3 = third.path().join("thor.db");
    let mut store = EventStore::new(&db3).unwrap();
    let item = Item {
        id: id.clone(),
        kind: Kind::Rule,
        text: "fixture one-session item".to_string(),
        bindings: vec![Binding::Target { kind: TargetKind::Command, value: "fixture-one-session".to_string() }],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the fixture turns out to be wrong".to_string()),
        check: None,
    };
    model::store::declare(&mut store, "fixture", "fixture", "fixture", &item).unwrap();
    serve::teeth::fixture::decide(&mut store, &id);
    for _ in 0..60 {
        serve::deliver::record_delivery(
            &mut store,
            "only",
            "only",
            serve::usefulness::ASSISTANT_DELIVERY_ACTOR,
            "2026-09-08T00:00:00Z",
            &[id.clone()],
        );
    }
    assert_eq!(serve::usefulness::served_since_last_verdict(&store).get(&id), Some(&1));
    drop(store);
    assert!(judgement_list(&db3, "only").is_empty(), "sixty showings in one session are one session");
}

// ------------------------------------------------------------ rule upkeep

/// Heavy, checkless and unanswered - written straight into the log the way
/// such a rule sits in an older store (the write gate refuses it fresh).
fn legacy_unanswered(store: &mut EventStore, id: &str, project: &str) {
    let item = Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("probe rule {id} about a costly mistake"),
        bindings: vec![Binding::Moment(intent::Action::Deploy)],
        severity: Some(model::item::Severity::Irreversible),
        project: Some(project.to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the mistake turns out to cost nothing".to_string()),
        check: None,
    };
    let body = serde_json::to_string(&item).unwrap();
    store.append_event("legacy", id, "migration", thor_core::event_store::EventKind::FactCreated, id, None, &body).unwrap();
}

fn checkout(root: &Path, project: &str) -> PathBuf {
    let dir = root.join(project);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".thor-project"), format!("{project}\n")).unwrap();
    dir
}

/// `session` has worked `minutes` of its own in `project` and ended a turn
/// there before: the gates the rule-upkeep question used to wait for.
fn seed_own_work(db: &Path, project: &str, session: &str, minutes: i64) {
    let mut all = serve::usefulness::read_eval_debt_state(db);
    let entry = all.entry(project.to_string()).or_default();
    let now = serve::time::now_unix();
    entry.sessions.insert(
        session.to_string(),
        serve::usefulness::SessionWorkState {
            anchor_unix: Some(now - minutes * 60),
            accrued_secs: minutes * 60,
            last_event_unix: Some(now),
            stops: 1,
            ..Default::default()
        },
    );
    std::fs::write(serve::usefulness::eval_debt_state_path(db), serde_json::to_string(&all).unwrap()).unwrap();
}

fn store_with_unanswered(dir: &Path, project: &str, n: usize) -> PathBuf {
    let db = dir.join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    for i in 0..n {
        legacy_unanswered(&mut store, &format!("unanswered-{i:02}"), project);
    }
    db
}

/// The old rules without a verdict from THOR's trial are asked inside the
/// evaluation (its batch and A4 list); no Stop holds a session for them, not
/// the first one, not one after hours of work, and not from another folder.
#[test]
fn old_rules_without_a_verdict_never_hold_a_stop() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with_unanswered(tmp.path(), "p", 10);
    let cwd = checkout(tmp.path(), "p");
    let sandbox = Sandbox::new();
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    // Recently covered by an evaluation, so only the old-rule question could hold.
    let _ = run_hook(&db, &read_payload("one", &cwd), &sandbox);
    seed_own_work(&db, "p", "one", 30);
    seed_own_work(&db, "p", "two", 90);
    for (session, folder) in [("one", &cwd), ("two", &cwd), ("one", &scratch)] {
        let out = run_hook(&db, &stop_payload(session, folder), &sandbox);
        assert!(!out.contains("unanswered-"), "{session} was held for an old rule: {out}");
        assert!(!out.contains("have no current verdict"), "{session}: {out}");
    }
}

// ----------------------------------------------------------- the volume

/// Every Stop is recorded with whether it held the turn, for doctor.
#[test]
fn every_stop_is_recorded_for_doctors_volume_line() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    EventStore::new(&db).unwrap();
    let sandbox = Sandbox::new();
    run_hook(&db, &stop_payload("s1", &sandbox.global_cwd()), &sandbox);
    run_hook(&db, &stop_payload("s2", &sandbox.global_cwd()), &sandbox);
    let text = std::fs::read_to_string(dir.path().join(serve::volume::FILE)).expect("the volume record exists");
    let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines.iter().all(|l| l["k"] == "stop" && l["h"] == 0), "{text}");
    let shown = serve::volume::lines(&db, serve::time::now_unix());
    assert!(
        shown[0].contains("0 Stop hold(s) in 2 session(s), 0.0 per session on average"),
        "{shown:?}"
    );
}
