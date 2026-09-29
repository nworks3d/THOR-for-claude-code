//! Every Stop of a session records, once, whether each of its refusals was a
//! real refusal of a real tool call (`serve::attest`): one `attester` event
//! per new refusal of THAT session, none on the next Stop, none for another
//! session's refusals, and the same when Claude Code sets
//! `stop_hook_active`. The transcripts are found under the home folder the
//! hook runs with (`USERPROFILE`, else `HOME`), by the session id alone.

use serde_json::json;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use thor_core::event_store::{EventKind, EventStore};

const S1: &str = "0de7dc52-1b97-4b66-948f-61e4438043b2";
const S2: &str = "373e8610-8130-490d-ac29-56aa98709fd1";

fn run_stop(db: &Path, home: &Path, session: &str, active: bool) {
    let payload = json!({
        "hook_event_name": "Stop",
        "session_id": session,
        "stop_hook_active": active,
        "last_assistant_message": "",
        "cwd": home.to_string_lossy(),
    })
    .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .env("USERPROFILE", home)
        .env("HOME", home)
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the serve binary must start");
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "the Stop hook never fails: {}", String::from_utf8_lossy(&out.stderr));
}

fn verdicts(db: &Path, rule: &str) -> Vec<(i64, bool)> {
    let store = EventStore::open_read_only(db).unwrap();
    let events = store.get_events_by_entity(rule).unwrap();
    let mut out: Vec<(i64, bool)> =
        model::store::recorded_refusal_verdicts(&events).into_iter().map(|(seq, (field, _))| (seq, field)).collect();
    out.sort();
    out
}

fn attester_events(db: &Path) -> usize {
    let store = EventStore::open_read_only(db).unwrap();
    store
        .events_of_kinds(&[EventKind::GateStoodAside], 0)
        .unwrap()
        .iter()
        .filter(|e| e.actor == model::store::ATTESTER_ACTOR)
        .count()
}

fn refuse(db: &Path, session: &str, target: &str, tool_use_id: &str) -> i64 {
    let mut store = EventStore::open_existing(db).unwrap();
    serve::deliver::record_delivery_with_trigger(&mut store, session, session, "hook", "2026-09-28T00:00:00Z", &["no-force".to_string()], Some(target));
    serve::deliver::record_gate_outcome_for_call(&mut store, session, session, "hook", "2026-09-28T00:00:01Z", true, "no-force", target, Some(tool_use_id));
    store.get_events_by_entity("no-force").unwrap().last().unwrap().seq
}

/// The hook files the payload's own tool call id with every refusal it
/// records, so the call can be found in the session's transcript later.
#[test]
fn a_hook_refusal_carries_the_payloads_tool_use_id() {
    let home = tempfile::tempdir().unwrap();
    let db_dir = tempfile::tempdir().unwrap();
    let db = db_dir.path().join("thor.db");
    {
        let mut store = EventStore::new(&db).unwrap();
        let item = model::item::Item {
            id: "no-force".to_string(),
            kind: model::item::Kind::Rule,
            text: "Never run `git push --force` on main.".to_string(),
            bindings: vec![model::item::Binding::Target { kind: model::item::TargetKind::Command, value: "git push".to_string() }],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("A force push to main turns out harmless.".to_string()),
            check: Some(model::item::Check::Forbidden { literals: vec!["--force".to_string()] }),
        };
        model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    }
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "session_id": S1,
        "tool_use_id": "toolu_FROM_PAYLOAD",
        "tool_name": "Bash",
        "tool_input": {"command": "git push --force origin main"},
        "cwd": home.path().to_string_lossy(),
    })
    .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(&db)
        .arg("hook")
        .env("USERPROFILE", home.path())
        .env("HOME", home.path())
        .current_dir(home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"block\""), "fixture sanity: the hook refuses");
    let store = EventStore::open_read_only(&db).unwrap();
    let refused = store.events_of_kinds(&[EventKind::GateRefused], 0).unwrap();
    assert_eq!(refused.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&refused[0].body).unwrap();
    assert_eq!(body["tool_use_id"], "toolu_FROM_PAYLOAD");
    assert_eq!(body["target"], "git push --force origin main");
}

#[test]
fn a_stop_records_one_verdict_per_new_refusal_of_its_session_and_none_on_the_next() {
    let home = tempfile::tempdir().unwrap();
    let db_dir = tempfile::tempdir().unwrap();
    let db = db_dir.path().join("thor.db");
    let projects = home.path().join(".claude").join("projects");
    {
        let mut store = EventStore::new(&db).unwrap();
        let item = model::item::Item {
            id: "no-force".to_string(),
            kind: model::item::Kind::Rule,
            text: "Never run `git push --force` on main.".to_string(),
            bindings: vec![model::item::Binding::Target { kind: model::item::TargetKind::Command, value: "git push".to_string() }],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("A force push to main turns out harmless.".to_string()),
            check: Some(model::item::Check::Forbidden { literals: vec!["--force".to_string()] }),
        };
        model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    }
    let reason = serve::attest::fixture::block_reason("no-force", "git push --force origin main");
    let input = json!({"command": "git push --force origin main"});
    serve::attest::fixture::main_transcript(&projects, S1).block("toolu_ONE", "Bash", &input, &reason).write();
    let first = refuse(&db, S1, "git push --force origin main", "toolu_ONE");
    let other = refuse(&db, S2, "git push --force origin main", "toolu_TWO");

    run_stop(&db, home.path(), S1, false);
    assert_eq!(verdicts(&db, "no-force"), vec![(first, true)], "one verdict, for this session's refusal only");
    assert_eq!(attester_events(&db), 1);

    run_stop(&db, home.path(), S1, false);
    assert_eq!(attester_events(&db), 1, "none on the second Stop");

    // A new refusal, and a Stop Claude Code marks as already held: recorded
    // all the same - recording never holds a turn.
    serve::attest::fixture::main_transcript(&projects, S1).block("toolu_THREE", "Bash", &input, &reason).write();
    let third = refuse(&db, S1, "git push --force origin main", "toolu_THREE");
    run_stop(&db, home.path(), S1, true);
    assert_eq!(verdicts(&db, "no-force"), vec![(first, true), (third, true)]);

    // The other session's refusal is decided at its own Stop: no transcript
    // of it exists, so it stays unrecorded - never proof.
    run_stop(&db, home.path(), S2, false);
    assert!(!verdicts(&db, "no-force").iter().any(|(seq, _)| *seq == other));
}
