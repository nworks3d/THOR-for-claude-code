//! A `requires` check whose trigger is a literal INSIDE the bound command,
//! end to end: stored through the real write gate, enforced by the real
//! compiled `serve hook` binary.
//!
//! THE CATCH THIS MAKES POSSIBLE (2026-09-26): "a dev docker compose over ssh
//! must carry -f docker-compose.dev.yml". The call that runs is `ssh`, and the
//! only thing that marks it as the dev stack is a literal deep inside it
//! (`-p acme-dev`). While a trigger had to name one of the rule's own
//! bindings, the rule could either watch every ssh call (and refuse the
//! ordinary ones) or none.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fixture(dir: &Path) -> PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    let item = model::item::Item {
        id: "dev-compose-names-its-file".to_string(),
        kind: model::item::Kind::Rule,
        text: "a dev docker compose over ssh must carry -f docker-compose.dev.yml".to_string(),
        bindings: vec![model::item::Binding::Target { kind: model::item::TargetKind::Command, value: "ssh".to_string() }],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the dev stack starts correctly without naming its compose file".to_string()),
        check: Some(model::item::Check::Requires {
            when: "-p acme-dev".to_string(),
            required: vec!["-f docker-compose.dev.yml".to_string()],
        }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).expect("the write gate must accept an inner trigger");
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
fn the_dev_compose_run_without_its_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    let out = run_bash(&db, dir.path(), "ssh admin@host \"cd /srv/app && sudo docker compose -p acme-dev up -d\"");
    assert!(blocked(&out), "{out}");
    assert!(out.contains("dev-compose-names-its-file"), "{out}");
}

#[test]
fn the_dev_compose_run_with_its_file_passes() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    let out = run_bash(
        &db,
        dir.path(),
        "ssh admin@host \"cd /srv/app && sudo docker compose -f docker-compose.dev.yml -p acme-dev up -d\"",
    );
    assert!(!blocked(&out), "{out}");
}

#[test]
fn every_other_ssh_call_passes() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    for command in [
        "ssh admin@host \"sudo docker ps\"",
        "ssh admin@host \"cd /srv/app && sudo docker compose -p acme-prod up -d\"",
        "ssh admin@host \"sudo docker compose -p acme-dev-old logs\"",
    ] {
        let out = run_bash(&db, dir.path(), command);
        assert!(!blocked(&out), "{command} must pass: {out}");
    }
}
