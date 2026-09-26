//! The Stop hook's teeth question, end to end through the real compiled
//! `serve hook` binary: it moves on to other rules from one session to the
//! next (H), and it keeps asking about the project a session worked in after
//! that session changes directory to a folder that belongs to no project (I).
//!
//! MEASURED on a copy of a real store (2026-09-26): the question named the
//! same five lowest ids at every session start in a project with nearly a
//! hundred waiting, and a session that did an hour of work in a project and
//! then moved to a scratch folder was asked nothing about that project at all.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

fn stop(session: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "stop_hook_active": false,
        "last_assistant_message": "",
    })
    .to_string()
}

fn read_file(session: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Read",
        "tool_input": {"file_path": cwd.join("notes.md").to_string_lossy()},
    })
    .to_string()
}

fn reason(out: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("expected a block: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    v["reason"].as_str().unwrap().to_string()
}

fn checkout(root: &Path, project: &str) -> PathBuf {
    let dir = root.join(project);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".thor-project"), format!("{project}\n")).unwrap();
    dir
}

/// Heavy, checkless and unanswered - written straight into the log the way
/// such a rule sits in an older store (the write gate refuses it fresh).
fn legacy_unanswered(store: &mut thor_core::event_store::EventStore, id: &str, project: &str) {
    let item = model::item::Item {
        id: id.to_string(),
        kind: model::item::Kind::Rule,
        text: format!("probe rule {id} about a costly mistake"),
        bindings: vec![model::item::Binding::Moment(intent::Action::Deploy)],
        severity: Some(model::item::Severity::Irreversible),
        project: Some(project.to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the mistake turns out to cost nothing".to_string()),
        check: None,
    };
    let body = serde_json::to_string(&item).unwrap();
    store
        .append_event("legacy", id, "migration", thor_core::event_store::EventKind::FactCreated, id, None, &body)
        .unwrap();
}

fn store_with_unanswered(dir: &Path, project: &str, n: usize) -> PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    for i in 0..n {
        legacy_unanswered(&mut store, &format!("unanswered-{i:02}"), project);
    }
    db
}

#[test]
fn the_next_session_is_asked_about_other_rules_first() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with_unanswered(tmp.path(), "p", 10);
    let cwd = checkout(tmp.path(), "p");
    let first = reason(&run_hook(&db, &stop("session-one", &cwd)));
    let second = reason(&run_hook(&db, &stop("session-two", &cwd)));
    let named = |text: &str| (0..10).filter(|i| text.contains(&format!("'unanswered-{i:02}'"))).collect::<Vec<_>>();
    let (a, b) = (named(&first), named(&second));
    assert_eq!(a.len(), 5, "{first}");
    assert_eq!(b.len(), 5, "{second}");
    assert!(a.iter().all(|i| !b.contains(i)), "the second session must be asked about others: {a:?} then {b:?}");
}

#[test]
fn a_session_that_moved_to_a_folder_with_no_project_is_still_asked_about_its_project() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with_unanswered(tmp.path(), "p", 3);
    let project_dir = checkout(tmp.path(), "p");
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    // Work inside the project first, then move to a folder that belongs to
    // no project at all.
    let _ = run_hook(&db, &read_file("wanderer", &project_dir));
    let out = reason(&run_hook(&db, &stop("wanderer", &scratch)));
    assert!(out.contains("unanswered-00"), "the project's debt must follow the session: {out}");
}

#[test]
fn a_session_that_never_worked_in_a_project_is_not_given_one() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with_unanswered(tmp.path(), "p", 3);
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let out = run_hook(&db, &stop("stranger", &scratch));
    assert!(!out.contains("unanswered-00"), "no project was ever worked in, so none may be assumed: {out}");
}
