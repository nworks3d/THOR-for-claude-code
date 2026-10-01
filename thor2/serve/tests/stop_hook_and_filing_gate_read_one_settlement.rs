//! The evaluation's batch and the evaluation-filing gate read ONE answer
//! (`model::store::settlement`, step 9): the same rules are owed on both, a
//! settled rule is named by neither, and a rule held by a machine blocker is
//! named by neither until the blocker lifts - then by both. Since step 28 no
//! Stop hook asks about these rules at all (the owner's decision: that is the
//! evaluation's question), and this file proves the Stop names none of them. Both blockers (step 14): a file check of a project with no
//! registered working copy, and a contains literal an uncommitted edit took
//! out of the registered working tree while the default branch still has it.
//!
//! (The spec names this file `stop_hook_teeth_debt_reads_the_same_settlement_
//! as_the_filing_gate.rs`; that name makes the test binary's path pass the
//! 260-character limit of the Windows linker in a deeply nested build folder,
//! so it carries this shorter one.)
//!
//! Before step 9 the Stop hook computed its own answer (`serve::prove::
//! teeth_box`) while the filing gate read tags, open points and probe events,
//! so the two could disagree about the same rule in the same turn.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use thor_core::event_store::{EventKind, EventStore};

fn run_hook(db: &Path, payload: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    String::from_utf8(out.stdout).unwrap()
}

/// The Stop of `session` in project `p`, once it has thirty minutes of its
/// own work there and an earlier turn - the point where the teeth question
/// used to hold it. What the hook said, whatever it was.
fn stop_reason(db: &Path, session: &str, cwd: &Path) -> String {
    let mut all = serve::usefulness::read_eval_debt_state(db);
    let now = serve::time::now_unix();
    all.entry("p".to_string()).or_default().sessions.insert(
        session.to_string(),
        serve::usefulness::SessionWorkState {
            anchor_unix: Some(now - 30 * 60),
            accrued_secs: 30 * 60,
            last_event_unix: Some(now),
            stops: 1,
            ..Default::default()
        },
    );
    std::fs::write(serve::usefulness::eval_debt_state_path(db), serde_json::to_string(&all).unwrap()).unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "stop_hook_active": false,
        "last_assistant_message": "",
    })
    .to_string();
    run_hook(db, &payload)
}

fn legacy(store: &mut EventStore, item: &Item) {
    let body = serde_json::to_string(item).unwrap();
    store.append_event("legacy", &item.id, "migration", EventKind::FactCreated, &item.id, None, &body).unwrap();
}

fn rule(id: &str, text: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings: vec![Binding::Moment(intent::Action::Deploy)],
        severity: Some(Severity::Irreversible),
        project: Some("p".to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the mistake turns out to cost nothing".to_string()),
        check: None,
    }
}

const IDS: [&str; 5] = ["owed-1", "owed-2", "noted", "blocked-1", "blocked-2"];

fn named(text: &str) -> BTreeSet<String> {
    IDS.iter().filter(|id| text.contains(&format!("'{id}'"))).map(|id| id.to_string()).collect()
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|id| id.to_string()).collect()
}

fn eval_report(id: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Report,
        text: format!("fixture evaluation report {id}"),
        bindings: vec![],
        severity: None,
        project: Some("p".to_string()),
        tags: vec![model::store::EVALUATION_REPORT_TAG.to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    }
}

/// What the three readers name, as sets of fixture ids.
fn three_answers(db: &Path, session: &str, cwd: &Path) -> (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>) {
    let stop = named(&stop_reason(db, session, cwd));
    let mut store = EventStore::open_existing(db).unwrap();
    let batch: BTreeSet<String> = model::store::evaluation_teeth_batch(&store, Some("p"))
        .owed_now
        .into_iter()
        .map(|(id, _)| id)
        .filter(|id| IDS.contains(&id.as_str()))
        .collect();
    // The filing gate reads the lists an evaluation froze at its start
    // (step 14); this start hands it the live batch, and the report goes
    // through the shared attested fixture (step 15).
    serve::attest::fixture::live_batch_start(&mut store, session, "p");
    let transcripts = db.parent().unwrap().join("transcripts");
    let report = eval_report(&format!("eval-{session}"));
    let filing = match serve::attest::fixture::attested_filing(&mut store, &transcripts, session, &report, "toolu_filing") {
        Ok(_) => BTreeSet::new(),
        Err(e) => named(&e.to_string()),
    };
    (stop, batch, filing)
}

fn git(dir: &Path, hooks: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn the_stop_hook_the_batch_and_the_filing_gate_name_the_same_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let checkout: PathBuf = tmp.path().join("p");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join("NOTES.md"), "keep me\nplain line\n").unwrap();
    std::fs::write(checkout.join("OTHER.md"), "stay put\nplain line\n").unwrap();
    let db = tmp.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    serve::attest::fixture::attested_filing(&mut store, &tmp.path().join("transcripts"), "s", &eval_report("eval-old"), "toolu_old").unwrap();
    legacy(&mut store, &rule("owed-1", "probe rule owed-1 about a costly mistake"));
    legacy(&mut store, &rule("owed-2", "probe rule owed-2 about a costly mistake"));
    legacy(&mut store, &rule("noted", "probe rule noted about a costly mistake"));
    let mut blocked = rule("blocked-1", "NOTES.md must keep the line `keep me`.");
    blocked.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "NOTES.md".to_string() }];
    blocked.check = Some(Check::Contains { path: "NOTES.md".to_string(), literal: "keep me".to_string() });
    legacy(&mut store, &blocked);
    let mut differs = rule("blocked-2", "OTHER.md must keep the line `stay put`.");
    differs.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "OTHER.md".to_string() }];
    differs.check = Some(Check::Contains { path: "OTHER.md".to_string(), literal: "stay put".to_string() });
    legacy(&mut store, &differs);
    // The trial settles `noted` as a note.
    let corpus = serve::legit::Corpus::load(&store);
    let retest = serve::teeth::retest(&store, Some(db.as_path()), "noted", &corpus).unwrap();
    assert!(matches!(retest.outcome, serve::teeth::Maintenance::Note { .. }), "{:?}", retest.outcome);
    serve::teeth::apply_as_retest(&mut store, &retest).unwrap();
    drop(store);

    // p is not registered: blocked-1 and blocked-2 are held by that blocker
    // (no working copy).
    std::fs::write(checkout.join(".thor-project"), "p\n").unwrap();
    let (stop, batch, filing) = three_answers(&db, "session-one", &checkout);
    assert_eq!(stop, set(&[]), "no Stop names a rule of the upkeep any more");
    assert_eq!(batch, set(&["owed-1", "owed-2"]), "the batch");
    assert_eq!(filing, batch, "and the filing gate reads the same answer");

    // Registered: that blocker lifts, and all three name blocked-1. For
    // blocked-2 the other blocker holds now: both files are committed, and an
    // uncommitted edit then takes its literal out of the working tree while
    // the default branch still has it (the working tree differs).
    std::fs::remove_file(checkout.join(".thor-project")).unwrap();
    let hooks = tmp.path().join("no-hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    git(&checkout, &hooks, &["init", "-q", "-b", "main"]);
    git(&checkout, &hooks, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
    git(&checkout, &hooks, &["config", "user.email", "fixture@example.invalid"]);
    git(&checkout, &hooks, &["config", "user.name", "fixture"]);
    git(&checkout, &hooks, &["add", "NOTES.md", "OTHER.md"]);
    git(&checkout, &hooks, &["commit", "-q", "-m", "fixture"]);
    std::fs::write(checkout.join("OTHER.md"), "mid-edit\nplain line\n").unwrap();
    let mut store = EventStore::open_existing(&db).unwrap();
    let record = Item {
        id: model::store::project_opened_id("p"),
        kind: Kind::Report,
        text: "Project p: a fixture record.".to_string(),
        bindings: Vec::new(),
        severity: None,
        project: Some("p".to_string()),
        tags: vec!["project-record".to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    let body = model::store::canonical_body(&record).unwrap();
    let body = format!(
        "{},\"root\":{},\"root_at\":\"2026-09-29T00:00:00Z\"}}",
        &body[..body.len() - 1],
        serde_json::to_string(checkout.to_str().unwrap()).unwrap()
    );
    store
        .append_event("install", "install", model::store::INSTALLER_ACTOR, EventKind::FactCreated, &record.id, None, &body)
        .unwrap();
    // ONE HOLDER PER PROJECT (step 26): session-one, still active, holds p's
    // debt through its open evaluation and its lease. Its round is over here:
    // the evaluation is closed as superseded and the lease released, so
    // session-two is the one asked.
    serve::attest::fixture::supersede_others(&mut store, "session-two", "p");
    drop(store);
    serve::holder::release(&db, "session-one");
    let (stop, batch, filing) = three_answers(&db, "session-two", &checkout);
    assert_eq!(stop, set(&[]), "the Stop hook");
    assert_eq!(batch, set(&["owed-1", "owed-2", "blocked-1"]), "the batch");
    assert_eq!(filing, batch, "the filing gate");
    {
        let store = EventStore::open_existing(&db).unwrap();
        assert_eq!(
            model::store::settlement(&store, "blocked-2", None),
            model::store::Settlement::Settled(model::store::Settled::Blocked(model::store::Blocker::WorkingTreeDiffers {
                path: "OTHER.md".to_string()
            })),
            "held by the working tree, not by the agent"
        );
    }

    // The edit is dropped: that blocker lifts too, and all three name it.
    std::fs::write(checkout.join("OTHER.md"), "stay put\nplain line\n").unwrap();
    // Session-two's round is over the same way (step 26).
    let mut store = EventStore::open_existing(&db).unwrap();
    serve::attest::fixture::supersede_others(&mut store, "session-three", "p");
    drop(store);
    serve::holder::release(&db, "session-two");
    let (stop, batch, filing) = three_answers(&db, "session-three", &checkout);
    assert_eq!(stop, set(&[]), "the Stop hook");
    assert_eq!(batch, set(&["owed-1", "owed-2", "blocked-1", "blocked-2"]), "the batch");
    assert_eq!(filing, batch, "the filing gate");
}
