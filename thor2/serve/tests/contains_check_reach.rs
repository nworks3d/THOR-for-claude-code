//! Field test 5 (D6): what a contains check really reaches at the hook, so
//! the words THOR uses for it are true. The real compiled `serve hook`.
//!
//! A contains check guards writes to ITS OWN check path, and it is looked at
//! for a write only when the rule carries some file or folder anchor (a path
//! or dir binding). The anchor need not be the checked file: a rule anchored
//! to files/BACKUP.md whose check names files/watch.sh refuses a write to
//! files/watch.sh that loses the literal. A rule bound Always, or to a
//! command, is never looked at for a write.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};

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

fn write_payload(session_id: &str, cwd: &Path, file_path: &Path, content: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Write",
        "tool_input": { "file_path": file_path.to_string_lossy(), "content": content },
    })
    .to_string()
}

fn is_block(out: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(out).ok().is_some_and(|v| v["decision"] == "block")
}

/// A checkout holding files/BACKUP.md and files/watch.sh (which carries
/// `chattr`) and one rule with the given bindings and a contains check on
/// files/watch.sh.
fn checkout_with_rule(dir: &Path, bindings: Vec<Binding>) -> (std::path::PathBuf, std::path::PathBuf) {
    let real = dir.join("real");
    std::fs::create_dir_all(real.join(".git")).unwrap();
    std::fs::create_dir_all(real.join("files")).unwrap();
    std::fs::write(real.join(".thor-project"), "demo\n").unwrap();
    std::fs::write(real.join("files").join("BACKUP.md"), "backup notes\n").unwrap();
    std::fs::write(real.join("files").join("watch.sh"), "#!/bin/sh\nchattr +i /data\n").unwrap();
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    let item = Item {
        id: "keep-chattr".to_string(),
        kind: Kind::Rule,
        text: "The watcher script keeps its chattr line (names 'chattr').".to_string(),
        bindings,
        severity: Some(Severity::Irreversible),
        project: Some("demo".to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("The line may go.".to_string()),
        check: Some(Check::Contains { path: "files/watch.sh".to_string(), literal: "chattr".to_string() }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    (db, real)
}

#[test]
fn a_rule_anchored_to_a_sibling_file_still_guards_its_own_check_path() {
    let dir = tempfile::tempdir().unwrap();
    let (db, real) = checkout_with_rule(
        dir.path(),
        vec![Binding::Target { kind: TargetKind::Path, value: "files/BACKUP.md".to_string() }],
    );
    let watch = real.join("files").join("watch.sh");
    let out = run_hook(&db, &write_payload("s1", &real, &watch, "#!/bin/sh\necho no line here\n"));
    assert!(is_block(&out), "the anchor names BACKUP.md, the write is to watch.sh, and it is still refused: {out}");
    assert!(out.contains("keep-chattr"), "{out}");
}

#[test]
fn a_rule_bound_always_is_never_looked_at_for_a_write() {
    let dir = tempfile::tempdir().unwrap();
    let (db, real) = checkout_with_rule(dir.path(), vec![Binding::Always]);
    let watch = real.join("files").join("watch.sh");
    let out = run_hook(&db, &write_payload("s1", &real, &watch, "#!/bin/sh\necho no line here\n"));
    assert!(!is_block(&out), "a rule with no file or folder anchor reaches no write: {out}");
}
