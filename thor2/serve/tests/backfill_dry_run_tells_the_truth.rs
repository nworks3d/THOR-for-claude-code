//! The teeth backfill, end to end through the real compiled binary: since
//! step 8 of the two tracks it is a loop over THOR's own trial
//! (`serve::teeth::retest`, then `apply` as the `retest` actor), against each
//! rule's REGISTERED working copy - never a folder named on the command line.
//!
//! MEASURED on a copy of a real store (2026-09-26): the old pass labelled
//! every file check "informing" without a working copy - read as "tried,
//! nothing there" - while nothing had been tried at all, and its dry run did
//! not predict what `--apply` would be refused. The trial now says per rule
//! what it decided, writes only a rule or a note verdict, and leaves a rule it
//! cannot try (blocked) untouched.

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use std::path::Path;
use std::process::Command;
use thor_core::event_store::{EventKind, EventStore};

fn legacy(store: &mut EventStore, item: &Item) {
    let body = serde_json::to_string(item).unwrap();
    store.append_event("legacy", &item.id, "migration", EventKind::FactCreated, &item.id, None, &body).unwrap();
}

fn heavy(id: &str, text: &str, bindings: Vec<Binding>, check: Option<Check>) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings,
        severity: Some(Severity::Costly),
        project: Some("p".to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the mistake turns out to cost nothing".to_string()),
        check,
    }
}

fn backfill(db: &Path, apply: bool) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_backfill_teeth"));
    cmd.arg("--db").arg(db);
    if apply {
        cmd.arg("--apply");
    }
    let out = cmd.output().expect("run backfill_teeth");
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

fn fact_writes(db: &Path, id: &str) -> usize {
    let store = EventStore::open_existing(db).unwrap();
    store
        .get_events_by_entity(id)
        .unwrap()
        .iter()
        .filter(|e| matches!(e.kind, EventKind::FactCreated | EventKind::FactRevised))
        .count()
}

/// A rule with nothing to build a tooth from becomes a note - and only
/// `--apply` writes it, once, as the `retest` actor with its record. A second
/// run finds it decided and writes nothing.
#[test]
fn the_dry_run_says_what_the_trial_decides_and_only_apply_writes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    legacy(&mut store, &heavy("unanswered", "probe rule about a costly deploy mistake", vec![Binding::Moment(intent::Action::Deploy)], None));
    drop(store);

    let (ok, out) = backfill(&db, false);
    assert!(ok, "{out}");
    assert!(out.contains("unanswered: note - "), "{out}");
    assert!(out.contains("note:               1"), "{out}");
    assert!(out.contains("nothing written"), "{out}");
    assert_eq!(fact_writes(&db, "unanswered"), 1, "a dry run writes nothing");

    let (ok, out) = backfill(&db, true);
    assert!(ok, "{out}");
    assert!(out.contains("written: 1"), "{out}");
    assert_eq!(fact_writes(&db, "unanswered"), 2);
    let store = EventStore::open_existing(&db).unwrap();
    assert_eq!(model::store::current_verdict(&store, "unanswered"), model::store::CurrentVerdict::Note);
    let last = store.get_events_by_entity("unanswered").unwrap().into_iter().rev().find(|e| e.kind == EventKind::FactRevised).unwrap();
    assert_eq!(last.actor, "retest", "THOR's own actor writes the verdict");
    drop(store);

    let (ok, out) = backfill(&db, true);
    assert!(ok, "{out}");
    assert!(out.contains("already decided:    1"), "{out}");
    assert!(out.contains("written: 0"), "{out}");
    assert_eq!(fact_writes(&db, "unanswered"), 2, "a second run over an unchanged store writes nothing");
}

/// A file check whose project has no registered working copy is BLOCKED:
/// never labelled, never written, whatever `--apply` says.
#[test]
fn without_a_registered_working_copy_a_file_check_is_blocked_never_written() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    legacy(
        &mut store,
        &heavy(
            "file-check",
            "Never write SECRET into README.md.",
            vec![Binding::Target { kind: TargetKind::Path, value: "README.md".to_string() }],
            Some(Check::Absent { path: "README.md".to_string(), literal: "SECRET".to_string() }),
        ),
    );
    drop(store);
    let (ok, out) = backfill(&db, true);
    assert!(ok, "{out}");
    assert!(out.contains("file-check: blocked - 'p' has no registered working copy"), "{out}");
    assert!(out.contains("blocked:            1"), "{out}");
    assert_eq!(fact_writes(&db, "file-check"), 1, "a blocked rule is never written");
}

/// `--root` is gone: a folder named on the command line could prove a check
/// the registered checkout does not hold. clap refuses it (exit 2), and
/// nothing is written.
#[test]
fn a_root_named_on_the_command_line_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    legacy(&mut store, &heavy("r", "probe rule about a costly deploy mistake", vec![Binding::Moment(intent::Action::Deploy)], None));
    drop(store);
    let out = Command::new(env!("CARGO_BIN_EXE_backfill_teeth"))
        .arg("--db")
        .arg(&db)
        .arg("--root")
        .arg(tmp.path())
        .arg("--apply")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unexpected argument '--root'"));
    assert_eq!(fact_writes(&db, "r"), 1);
}
