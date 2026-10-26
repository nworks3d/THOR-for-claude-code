//! Proof, against the REAL compiled `serve` binary, of the bug nudge
//! (`serve::thor_bugs`): the first prompt the owner types in a session carries
//! at most one THOR bug candidate, a second prompt carries none, a subagent
//! never gets one, nothing is offered before an install wrote a mode, and
//! the test-environment wording appears only in test mode.

use model::store::{HookFault, ATTESTER_ACTOR};
use serve::bug_report::Mode;
use std::io::Write;
use std::process::{Command, Stdio};
use thor_core::event_store::EventStore;

fn run_hook(db_path: &std::path::Path, payload: serde_json::Value) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db_path)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start the serve binary");
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().expect("failed to wait on the serve binary");
    assert_eq!(out.status.code(), Some(0));
    String::from_utf8(out.stdout).unwrap()
}

fn prompt(session: &str, text: &str) -> serde_json::Value {
    serde_json::json!({ "session_id": session, "hook_event_name": "UserPromptSubmit", "prompt": text })
}

fn store_with_a_hook_failure() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    let fault = HookFault {
        tool_use_id: "toolu-1".to_string(),
        hook_event: "SessionStart".to_string(),
        exit_code: 126,
        at: "2026-10-06T22:29:00.231Z".to_string(),
        host_wide: false,
        quick_silent: false,
    };
    let body = serde_json::json!({ "hook_fault": fault }).to_string();
    let entity = model::store::hook_fault_entity("s-old");
    store.append_event("s-old", "s-old", ATTESTER_ACTOR, thor_core::event_store::EventKind::GateStoodAside, &entity, None, &body).unwrap();
    (dir, db)
}

#[test]
fn nothing_is_offered_before_an_install_wrote_a_mode() {
    let (_dir, db) = store_with_a_hook_failure();
    let out = run_hook(&db, prompt("s1", "please fix the build"));
    assert!(!out.contains("THOR may have found a bug"), "{out}");
}

#[test]
fn the_first_owner_prompt_of_a_session_carries_one_candidate_and_the_second_none() {
    let (_dir, db) = store_with_a_hook_failure();
    serve::thor_bugs::write_mode(&db, Mode::Test).unwrap();
    let first = run_hook(&db, prompt("s1", "please fix the build"));
    assert!(first.contains("THOR may have found a bug in ITSELF"), "{first}");
    assert!(first.contains("THIS IS A TEST ENVIRONMENT"), "{first}");
    assert!(first.contains("normally I would push this to the THOR repo"), "{first}");
    assert!(first.contains("hook-launch"), "{first}");
    let second = run_hook(&db, prompt("s1", "and now the tests"));
    assert!(!second.contains("THOR may have found a bug"), "{second}");
    let other_session = run_hook(&db, prompt("s2", "a new session"));
    assert!(!other_session.contains("THOR may have found a bug"), "the only candidate was presented a moment ago: {other_session}");
}

#[test]
fn a_subagent_never_gets_the_nudge_and_does_not_spend_it() {
    let (_dir, db) = store_with_a_hook_failure();
    serve::thor_bugs::write_mode(&db, Mode::Release).unwrap();
    let mut sub = prompt("s1", "do a small task");
    sub["agent_id"] = serde_json::json!("agent-1");
    let out = run_hook(&db, sub);
    assert!(!out.contains("THOR may have found a bug"), "{out}");
    let main = run_hook(&db, prompt("s1", "do the real task"));
    assert!(main.contains("THOR may have found a bug in ITSELF"), "{main}");
    assert!(!main.contains("THIS IS A TEST ENVIRONMENT"), "release mode: {main}");
    assert!(main.contains("gh issue create --repo"), "{main}");
}

#[test]
fn a_panic_in_a_hook_is_remembered_where_it_happened() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    serve::thor_bugs::record_panic(&db, "hook", "serve/src/live.rs:3");
    serve::thor_bugs::write_mode(&db, Mode::Test).unwrap();
    EventStore::new(&db).unwrap();
    let out = run_hook(&db, prompt("s1", "anything"));
    assert!(out.contains("hook-panic") && out.contains("serve/src/live.rs:3"), "{out}");
}
