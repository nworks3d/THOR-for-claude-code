//! The teeth backfill's dry run, end to end through the real compiled
//! binary: it predicts what the write gate will refuse, and it never labels a
//! file check it could not try.
//!
//! MEASURED on a copy of a real store (2026-09-26): the dry run reported only
//! what it would label, and `--apply` was then refused on hundreds of those
//! writes for reasons the dry run never looked at; and without `--root`
//! every file check was labelled "informing" - read as "tried, nothing
//! there" - while nothing had been tried at all.

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use std::path::Path;
use std::process::Command;
use thor_core::event_store::{EventKind, EventStore};

fn legacy(store: &mut EventStore, item: &Item) {
    let body = serde_json::to_string(item).unwrap();
    store.append_event("legacy", &item.id, "migration", EventKind::FactCreated, &item.id, None, &body).unwrap();
}

fn heavy(id: &str, bindings: Vec<Binding>, check: Option<Check>) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("probe rule {id} about a costly mistake"),
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

fn dry_run(db: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_backfill_teeth")).arg("--db").arg(db).output().expect("run backfill_teeth");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn the_dry_run_names_what_the_write_gate_would_refuse() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    // Heavy, no check, never answered the teeth question: labelling it is a
    // revise, and the gate refuses that revise until the question is
    // answered.
    legacy(&mut store, &heavy("unanswered", vec![Binding::Moment(intent::Action::Deploy)], None));
    drop(store);
    let out = dry_run(&db);
    assert!(out.contains("the write gate would refuse: 1"), "{out}");
    assert!(out.contains("would be refused: unanswered"), "{out}");
}

#[test]
fn without_a_root_a_file_check_is_skipped_never_labelled() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    legacy(
        &mut store,
        &heavy(
            "file-check",
            vec![Binding::Target { kind: TargetKind::Path, value: "README.md".to_string() }],
            Some(Check::Absent { path: "README.md".to_string(), literal: "SECRET".to_string() }),
        ),
    );
    drop(store);
    let out = dry_run(&db);
    assert!(out.contains("1 need --root"), "{out}");
    assert!(!out.contains("informing only           1"), "a file check nobody tried must not be labelled: {out}");
}
