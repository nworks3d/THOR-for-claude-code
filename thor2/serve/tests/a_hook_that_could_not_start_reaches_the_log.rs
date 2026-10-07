//! A hook Claude Code could not start (a security product blocking
//! `serve.exe`, a permission) leaves a `hook_non_blocking_error` record in the
//! session's transcript and nothing in THOR. The next Stop that does run reads
//! it and writes it to the log, once, through the real compiled `serve hook`.

use std::io::Write as _;
use std::process::{Command, Stdio};

use thor_core::event_store::EventStore;

const SESSION: &str = "3f2ac470-767e-4df0-9ef3-2b89ed671524";

fn run_stop(db: &std::path::Path, home: &std::path::Path) {
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": SESSION,
        "stop_hook_active": false,
        "last_assistant_message": "",
    })
    .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .env("USERPROFILE", home)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
}

#[test]
fn the_stop_hook_writes_a_launch_failure_of_its_session_to_the_log_once() {
    let home = tempfile::tempdir().unwrap();
    let project = home.path().join(".claude").join("projects").join("C--Users-someone-dev-demo");
    std::fs::create_dir_all(&project).unwrap();
    let failure = serde_json::json!({
        "attachment": {
            "type": "hook_non_blocking_error",
            "hookName": "SessionStart:startup",
            "toolUseID": "toolu-blocked-1",
            "hookEvent": "SessionStart",
            "stderr": "serve.exe: Permission denied",
            "stdout": "",
            "exitCode": 126,
            "command": "\"C:\\Users\\someone\\thor2\\bin\\serve.exe\" --db \"C:\\Users\\someone\\thor2\\thor.db\" hook",
            "durationMs": 1316
        },
        "type": "attachment",
        "timestamp": "2026-10-06T22:29:00.231Z",
        "sessionId": SESSION,
    });
    std::fs::write(project.join(format!("{SESSION}.jsonl")), failure.to_string() + "\n").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    drop(EventStore::new(&db).unwrap());

    run_stop(&db, home.path());
    run_stop(&db, home.path());

    let store = EventStore::open_existing(&db).unwrap();
    let events = store.get_events_by_entity(&model::store::hook_fault_entity(SESSION)).unwrap();
    let faults = model::store::hook_faults_in(&events);
    assert_eq!(faults.len(), 1, "recorded once, not once per Stop: {faults:?}");
    assert_eq!((faults[0].hook_event.as_str(), faults[0].exit_code), ("SessionStart", 126));
    assert_eq!(faults[0].tool_use_id, "toolu-blocked-1");
}
