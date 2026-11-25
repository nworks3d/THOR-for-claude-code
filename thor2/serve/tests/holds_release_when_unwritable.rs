//! A memory file that cannot be written (read-only file or folder, full disk)
//! releases every hold that only a write can settle, with one plain line that
//! says why; the hold comes back when the file can be written again.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

fn run_hook(db: &Path, payload: &str, home: &Path) -> String {
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
    String::from_utf8(out.stdout).unwrap()
}

fn stop_payload(session_id: &str) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": false,
        "last_assistant_message": "",
    })
    .to_string()
}

fn declare_setup_note(store: &mut thor_core::event_store::EventStore) {
    let item = model::item::Item {
        id: model::store::SETUP_NOTE_ID.to_string(),
        kind: model::item::Kind::Rule,
        text: "walk the owner through setup once, then retract this note".to_string(),
        bindings: vec![model::item::Binding::Always],
        severity: None,
        project: None,
        tags: vec!["working-contract".to_string()],
        expires: None,
        key: None,
        falsifier: Some("this note is still served after the owner already answered".to_string()),
        check: None,
    };
    model::store::declare(store, "install", "install", "install-fixture", &item).unwrap();
}

fn set_read_only(path: &Path, on: bool) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(on);
    std::fs::set_permissions(path, perms).unwrap();
}

#[test]
fn a_store_that_cannot_be_written_releases_the_setup_hold_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let db = dir.path().join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    declare_setup_note(&mut store);
    drop(store);

    // Works: the hold is there.
    let held = run_hook(&db, &stop_payload("s1"), &home);
    assert!(held.contains("\"block\""), "fixture: the setup hold stands while the file works: {held}");

    set_read_only(&db, true);
    let released = run_hook(&db, &stop_payload("s2"), &home);
    set_read_only(&db, false);
    assert!(!released.contains("\"block\""), "nothing that needs a write may hold the turn: {released}");
    assert!(released.contains("cannot save anything right now") && released.contains("read-only"), "one plain line says why: {released}");

    // Writable again: the hold comes back.
    let back = run_hook(&db, &stop_payload("s3"), &home);
    assert!(back.contains("\"block\""), "the hold returns once the file can be written: {back}");
}

#[test]
fn the_release_line_follows_the_chosen_language() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let db = dir.path().join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    declare_setup_note(&mut store);
    drop(store);
    serve::plan::set_reply_language(&db, Some("nl")).unwrap();
    set_read_only(&db, true);
    let released = run_hook(&db, &stop_payload("s2"), &home);
    set_read_only(&db, false);
    assert!(released.contains("kan nu niets bewaren"), "{released}");
}
