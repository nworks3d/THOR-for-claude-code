//! A `requires` trigger ending in one `*` is a prefix, end to end: stored
//! through the real write gate, enforced by the real compiled `serve hook`,
//! and tried at write time with an attempt the live guard really refuses.
//!
//! THE MISS THIS CLOSES (Z6, measured): a rule meant to catch a folder made
//! outside the scratch area, trigger "/thor", refused `mkdir /thor` and let
//! `mkdir /thor-probe-eval` through. The diagnosis was wrong for a while: the
//! trigger IS found inside an argument at a path separator; it is the
//! character AFTER it that hides it, a deliberate token boundary. The star is
//! the one way to ask for a name that continues: "/thor*".

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn star_rule(when: &str) -> Item {
    Item {
        id: "no-thor-folder-outside-scratch".to_string(),
        kind: Kind::Rule,
        text: "a new /thor* folder is made only under thor2 or the scratchpad".to_string(),
        bindings: vec![Binding::Target { kind: TargetKind::Command, value: "mkdir".to_string() }],
        severity: Some(Severity::Costly),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("a thor folder outside both turns out harmless".to_string()),
        check: Some(Check::Requires {
            when: when.to_string(),
            required: vec!["thor2".to_string(), "scratchpad".to_string()],
        }),
    }
}

fn store(dir: &Path, item: &Item) -> PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    model::store::declare(&mut store, "star", "star", "star-fixture", item)
        .unwrap_or_else(|e| panic!("the write gate must accept a star trigger: {e:?}"));
    db
}

fn hook(db: &Path, cwd: &Path, command: &str) -> String {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "star-session",
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    let file = db.with_file_name("payload.json");
    std::fs::write(&file, payload.to_string()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .arg("--no-record")
        .stdin(Stdio::from(std::fs::File::open(&file).unwrap()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn serve");
    assert!(out.status.success(), "the hook must always exit 0");
    String::from_utf8(out.stdout).unwrap()
}

fn blocked(out: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(out.trim()).ok().and_then(|v| v.get("decision").cloned())
        == Some(serde_json::json!("block"))
}

#[test]
fn a_star_trigger_refuses_every_name_that_continues_after_the_stem() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &star_rule("/thor*"));
    for command in [
        "mkdir /c/tmp/thor-probe-eval",
        "mkdir Temp/thor-probe-eval",
        "mkdir foo /thor-x",
        "mkdir -p C:/x/thor3",
        "cd /tmp && mkdir -p /tmp/thor_new",
        "ssh nas 'mkdir /srv/thor-copy'",
    ] {
        let out = hook(&db, tmp.path(), command);
        assert!(blocked(&out), "must be refused: {command}\n{out}");
        assert!(out.contains("no-thor-folder-outside-scratch"), "{out}");
        assert!(out.contains("carrying \\\"/thor*\\\""), "the refusal names the trigger that tripped: {out}");
    }
}

#[test]
fn a_star_trigger_lets_the_answer_and_every_other_name_through() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &star_rule("/thor*"));
    for command in [
        "mkdir C:/x/thor2/cache",
        "mkdir -p /c/tmp/claude/x/scratchpad/thor-x",
        "mkdir /c/tmp/authority",
        "mkdir thor-notes",
        "echo mkdir /thor-x",
    ] {
        let out = hook(&db, tmp.path(), command);
        assert!(!blocked(&out), "must pass: {command}\n{out}");
    }
}

/// The stated price of a prefix, pinned rather than hidden.
#[test]
fn the_stated_price_a_longer_word_on_the_same_stem_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &star_rule("/thor*"));
    assert!(blocked(&hook(&db, tmp.path(), "mkdir build/thorough")));
}

/// Without the star nothing changes: the token boundary keeps a name that
/// continues after the trigger apart from it.
#[test]
fn without_the_star_the_boundary_keeps_the_miss() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &star_rule("/thor"));
    assert!(blocked(&hook(&db, tmp.path(), "mkdir /thor")));
    assert!(!blocked(&hook(&db, tmp.path(), "mkdir /thor-probe-eval")));
}

/// The write-time trial tries the shape the star exists to catch, and the
/// receipt says so: `mkdir /thor-probe`, refused.
#[test]
fn the_write_receipt_shows_the_star_attempt_refused() {
    let item = star_rule("/thor*");
    let reality = serve::prove::Reality::default();
    match serve::prove::teeth_box(&item.id, &item, None, &reality) {
        serve::prove::TeethBox::Wired { forbidden_tried, clean_tried, note } => {
            assert_eq!(forbidden_tried, "mkdir /thor-probe");
            assert!(note.contains("tried forbidden: mkdir /thor-probe; refused"), "{note}");
            assert!(clean_tried.contains("mkdir /thor-probe"), "{clean_tried}");
        }
        other => panic!("a star rule must come out wired, with its attempt refused: {other:?}"),
    }
}

#[test]
fn the_write_gate_refuses_every_other_shape_of_star() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = thor_core::event_store::EventStore::new(&tmp.path().join("thor.db")).unwrap();
    for when in ["thor*", "/th*or", "**", "mkdir*"] {
        let err = model::store::declare(&mut store, "star", "star", "star-fixture", &star_rule(when))
            .expect_err(&format!("{when} must be refused"));
        assert!(format!("{err:?}").contains(when), "the refusal names the trigger {when}: {err:?}");
    }
}
