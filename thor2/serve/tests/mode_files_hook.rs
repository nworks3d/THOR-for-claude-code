//! A setting file the owner edits by hand cannot be changed silently, end to end through the real
//! compiled `serve hook`: the first session start records, a later edit is named at the next session
//! start (once), and a headless session neither prints it nor moves the record.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn start(db: &Path, cwd: &Path, session: &str, entrypoint: Option<&str>) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(db).arg("hook").current_dir(cwd).env_remove("CLAUDE_CODE_ENTRYPOINT");
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
