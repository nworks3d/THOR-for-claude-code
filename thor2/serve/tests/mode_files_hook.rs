//! A setting file the owner edits by hand cannot be changed silently, end to end through the real
//! compiled `serve hook`: the first session start records, a later edit is named at the next session
//! start (once), and a headless session neither prints it nor moves the record.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn start(db: &Path, cwd: &Path, session: &str, entrypoint: Option<&str>) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(db).arg("hook").current_dir(cwd).env_remove("CLAUDE_CODE_ENTRYPOINT").env_remove("CLAUDE_CODE_SESSION_ATTENDED");
    if let Some(e) = entrypoint {
        cmd.env("CLAUDE_CODE_ENTRYPOINT", e);
    }
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn serve");
    let payload = serde_json::json!({ "hook_event_name": "SessionStart", "source": "startup", "session_id": session, "cwd": cwd.to_string_lossy() });
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    let stdout = String::from_utf8(out.stdout).unwrap();
    if stdout.trim().is_empty() {
        return String::new();
    }
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("hook JSON ({e}): {stdout}"));
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("store")).unwrap();
    let db = dir.path().join("store").join("store.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    (dir, db, work)
}

#[test]
fn a_guard_switched_off_by_hand_is_named_at_the_next_session_start_once() {
    let (dir, db, work) = fixture();
    let store = dir.path().join("store");
    std::fs::write(store.join("stale-guard-mode.json"), r#"{"mode": "on"}"#).unwrap();

    // The first start only records.
    assert!(!start(&db, &work, "s1", None).contains("setting file"));
    // A quiet restart says nothing.
    assert!(!start(&db, &work, "s2", None).contains("setting file"));

    // One small edit, with no one watching.
    std::fs::write(store.join("stale-guard-mode.json"), r#"{"mode": "off"}"#).unwrap();
    std::fs::write(store.join("guard-response-rulebook.json"), "[]").unwrap();

    // A headless session sees it but leaves the record where it was.
    let headless = start(&db, &work, "s3", Some("sdk-cli"));
    assert!(headless.contains("stale-guard-mode.json changed"), "{headless}");
    // The next session the owner reads says what the file now says.
    let said = start(&db, &work, "s4", None);
    assert!(said.contains("The setting file stale-guard-mode.json changed") && said.contains(r#"{"mode": "off"}"#), "{said}");
    assert!(said.contains("guard-response-rulebook.json appeared"), "{said}");
    // And once.
    assert!(!start(&db, &work, "s5", None).contains("setting file"));
}

#[test]
fn a_guard_switched_off_and_on_again_inside_one_session_is_still_named() {
    let (dir, db, work) = fixture();
    let file = dir.path().join("store").join("stale-guard-mode.json");
    std::fs::write(&file, r#"{"mode": "on"}"#).unwrap();
    assert!(!start(&db, &work, "s1", None).contains("setting file"));
    // Off, a turn ends (a Stop looks), on again.
    std::fs::write(&file, r#"{"mode": "off"}"#).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(&db).arg("hook").current_dir(&work).env_remove("CLAUDE_CODE_ENTRYPOINT").env_remove("CLAUDE_CODE_SESSION_ATTENDED");
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn serve");
    let payload = serde_json::json!({ "hook_event_name": "Stop", "session_id": "s1", "cwd": work.to_string_lossy(), "stop_hook_active": false, "last_assistant_message": "" });
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    assert!(child.wait_with_output().expect("wait serve").status.success());
    std::fs::write(&file, r#"{"mode": "on"}"#).unwrap();
    let said = start(&db, &work, "s2", None);
    assert!(said.contains("stale-guard-mode.json was changed during the last session and put back"), "{said}");
    assert!(!start(&db, &work, "s3", None).contains("setting file"), "said once");
}

#[test]
fn an_attended_sdk_session_moves_the_record_like_any_session_a_person_reads() {
    let (dir, db, work) = fixture();
    let file = dir.path().join("store").join("thor-mode.json");
    std::fs::write(&file, r#"{"mode": "release"}"#).unwrap();
    start(&db, &work, "s1", None);
    std::fs::write(&file, r#"{"mode": "off"}"#).unwrap();
    // Marked attended by Claude Code: it says it and moves the record.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(&db).arg("hook").current_dir(&work).env("CLAUDE_CODE_ENTRYPOINT", "sdk-cli").env("CLAUDE_CODE_SESSION_ATTENDED", "1");
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn serve");
    let payload = serde_json::json!({ "hook_event_name": "SessionStart", "source": "startup", "session_id": "s2", "cwd": work.to_string_lossy() });
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = String::from_utf8(child.wait_with_output().expect("wait serve").stdout).unwrap();
    assert!(out.contains("thor-mode.json changed"), "{out}");
    assert!(!start(&db, &work, "s3", None).contains("setting file"), "the attended start moved the record");
}
