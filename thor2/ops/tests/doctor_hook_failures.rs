//! `doctor` says when Claude Code could not start a THOR hook, proven against
//! the REAL compiled binary: a unit test of the line alone could not catch the
//! call in `main` being dropped. The transcripts folder is a throwaway sandbox
//! (`USERPROFILE` and `HOME` point at it), never the real user's.

use std::process::Command;
use thor_core::event_store::EventStore;

const SESSION: &str = "3f2ac470-767e-4df0-9ef3-2b89ed671524";

fn doctor_output(home: &std::path::Path) -> String {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    EventStore::new(&db).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_doctor"))
        .arg("--db")
        .arg(&db)
        .env("USERPROFILE", home)
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn a_recent_hook_launch_failure_is_named_and_a_quiet_week_says_nothing() {
    let quiet_home = tempfile::tempdir().unwrap();
    let quiet = doctor_output(quiet_home.path());
    assert!(!quiet.contains("hook launch failures"), "{quiet}");

    let home = tempfile::tempdir().unwrap();
    let project = home.path().join(".claude").join("projects").join("C--Users-someone-dev-demo");
    std::fs::create_dir_all(&project).unwrap();
    let record = serde_json::json!({
        "attachment": {
            "type": "hook_non_blocking_error",
            "toolUseID": "toolu-blocked-1",
            "hookEvent": "SessionStart",
            "stderr": "serve.exe: Permission denied",
            "exitCode": 126,
            "command": "\"C:\\Users\\someone\\thor2\\bin\\serve.exe\" --db \"C:\\Users\\someone\\thor2\\thor.db\" hook"
        },
        "timestamp": serve::time::iso8601_from_unix(serve::time::now_unix() - 600),
        "sessionId": SESSION,
    });
    std::fs::write(project.join(format!("{SESSION}.jsonl")), record.to_string() + "\n").unwrap();
    let found = doctor_output(home.path());
    let line = found.lines().find(|l| l.starts_with("hook launch failures")).unwrap_or_else(|| panic!("no hook line: {found}"));
    assert!(line.contains("1 in the last 7 days") && line.contains("SessionStart, exit 126"), "{line}");
}
