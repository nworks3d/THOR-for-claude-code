//! The shipped `checked-claim-needs-evidence` rule, end to end through the
//! real compiled `serve hook` binary: a `path:line` citation counts as
//! evidence only when that file exists under the session's working directory
//! and has that line.
//!
//! THE ESCAPE THIS CLOSES (2026-09-26): the rule accepted any text shaped like
//! `name.ext:NN`, so an answer could claim to have verified something, type a
//! file name and a line number nobody opened, and pass.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SHIPPED_RULEBOOK: &str = include_str!("../../guard-response-rulebook.example.json");

fn run_hook(db: &Path, payload: &serde_json::Value) -> String {
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

/// A store with the shipped rulebook beside it, and a working directory (no
/// project) holding `src/lib.rs` with five lines, next to a file outside it.
fn fixture(tmp: &Path) -> (PathBuf, PathBuf) {
    let store = tmp.join("store");
    std::fs::create_dir_all(&store).unwrap();
    let db = store.join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(store.join("guard-response-rulebook.json"), SHIPPED_RULEBOOK).unwrap();
    let work = tmp.join("work");
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("src").join("lib.rs"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
    std::fs::write(tmp.join("outside.rs"), "one\ntwo\n").unwrap();
    (db, work)
}

fn stop(cwd: Option<&Path>, message: &str) -> serde_json::Value {
    let mut p = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "s1",
        "stop_hook_active": false,
        "last_assistant_message": message,
    });
    if let Some(cwd) = cwd {
        p["cwd"] = serde_json::json!(cwd.to_string_lossy());
    }
    p
}

fn blocked(out: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(out.trim()).ok().and_then(|v| v.get("decision").cloned())
        == Some(serde_json::json!("block"))
}

#[test]
fn a_citation_of_a_real_line_under_the_working_directory_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, work) = fixture(tmp.path());
    let relative = run_hook(&db, &stop(Some(&work), "Verified: src/lib.rs:3 has the fix."));
    assert!(!blocked(&relative), "a real line must count: {relative}");
    let absolute = format!("Verified: {}:5 has the fix.", work.join("src").join("lib.rs").display());
    let out = run_hook(&db, &stop(Some(&work), &absolute));
    assert!(!blocked(&out), "an absolute path under the working directory must count: {out}");
}

#[test]
fn a_citation_nobody_could_have_opened_is_held_and_named() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, work) = fixture(tmp.path());
    for (message, cited) in [
        ("Verified: src/lib.rs:9 has the fix.", "src/lib.rs:9"),
        ("Verified: src/missing.rs:1 has the fix.", "src/missing.rs:1"),
        ("Verified: ../outside.rs:1 has the fix.", "../outside.rs:1"),
        ("Verified: src/lib.rs:0 has the fix.", "src/lib.rs:0"),
    ] {
        let out = run_hook(&db, &stop(Some(&work), message));
        assert!(blocked(&out), "{message} must be held: {out}");
        assert!(out.contains(cited), "the reason must name the citation that did not count: {out}");
    }
}

/// Without a working directory in the payload nothing can be looked up, so a
/// citation is judged by its shape, as before.
#[test]
fn without_a_working_directory_a_citation_counts_by_its_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _work) = fixture(tmp.path());
    let out = run_hook(&db, &stop(None, "Verified: src/lib.rs:9 has the fix."));
    assert!(!blocked(&out), "{out}");
}
