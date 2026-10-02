//! A `requires` check looks for its required literals in the SAME command
//! segment as the call that triggers it, not anywhere in the line (step 29).
//!
//! Field 2026-10-02: `git log ... -- x.test.js && cd server && node --test
//! 2>&1 | tail -25` was not refused by the rule that wants `node --test` to
//! carry a reporter, a timeout or a `.test.js` file, because the `.test.js`
//! in the `git log` part satisfied it. A different command on the same line
//! is not an answer.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fixture(dir: &Path) -> PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    let item = model::item::Item {
        id: "node-test-needs-a-timeout".to_string(),
        kind: model::item::Kind::Rule,
        text: "node --test must carry --test-reporter, --test-timeout or a .test.js file".to_string(),
        bindings: vec![model::item::Binding::Target { kind: model::item::TargetKind::Command, value: "node --test".to_string() }],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("a bare node --test never hangs without output".to_string()),
        check: Some(model::item::Check::Requires {
            when: "node --test".to_string(),
            required: vec!["--test-reporter".to_string(), "--test-timeout".to_string(), ".test.js".to_string()],
        }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).expect("the write gate must accept the rule");
    db
}

fn run_bash(db: &Path, cwd: &Path, command: &str) -> String {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s1",
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    String::from_utf8(out.stdout).unwrap()
}

fn blocked(out: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(out.trim()).ok().and_then(|v| v.get("decision").cloned())
        == Some(serde_json::json!("block"))
}

#[test]
fn a_required_literal_in_another_command_of_the_line_does_not_answer() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    for command in [
        "git log --oneline -- server/test/a.test.js && cd server && node --test 2>&1 | tail -25",
        "echo --test-timeout=5 && node --test | tail",
        "node --test && node --test b.test.js",
    ] {
        let out = run_bash(&db, dir.path(), command);
        assert!(blocked(&out), "{command} must be refused: {out}");
        assert!(out.contains("node-test-needs-a-timeout"), "{out}");
    }
}

#[test]
fn a_required_literal_in_the_triggering_command_still_answers() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    for command in [
        "node --test --test-timeout=120000 2>&1 | tail -25",
        "cd server && node --test test/a.test.js",
        "git log -- x.test.js && node --test --test-reporter=spec",
        "node --test --test-timeout=1000 && node --test b.test.js",
    ] {
        let out = run_bash(&db, dir.path(), command);
        assert!(!blocked(&out), "{command} must pass: {out}");
    }
}
