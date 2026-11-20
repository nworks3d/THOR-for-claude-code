//! The last setup step is seeded into a NEW store only, and a plan carried out
//! by `install --tidy` leaves THOR's own record of it.

use model::store::{show, ORDER_NOTE_ID};
use ops::install::{ensure_and_seed_store, ensure_store, order_earlier_note, StoreOutcome};
use thor_core::event_store::EventStore;

#[test]
fn a_new_store_gets_the_pinned_note_and_an_existing_one_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let fresh = dir.path().join("fresh.db");
    let report = ensure_and_seed_store(&fresh).unwrap();
    assert_eq!(report.store, StoreOutcome::Created);
    let store = EventStore::open_existing(&fresh).unwrap();
    let note = show(&store, ORDER_NOTE_ID).expect("a new store carries the note");
    assert_eq!(note.kind, model::item::Kind::Orientation);
    assert_eq!(note.bindings, vec![model::item::Binding::Always], "pinned");
    assert!(note.falsifier.is_some());

    // An existing store (made before this note existed) is never given it.
    let old = dir.path().join("old.db");
    ensure_store(&old).unwrap();
    let again = ensure_and_seed_store(&old).unwrap();
    assert_eq!(again.store, StoreOutcome::AlreadyThere);
    assert!(again.seeded.is_empty());
    let store = EventStore::open_existing(&old).unwrap();
    assert!(show(&store, ORDER_NOTE_ID).is_err(), "an existing store never gets the note");
}

#[test]
fn the_note_is_short_plain_and_has_no_dashes() {
    let note = order_earlier_note();
    assert!(note.text.chars().count() < 300, "{}", note.text.chars().count());
    assert!(note.text.is_ascii() && !note.text.contains(" - "), "{}", note.text);
}

#[test]
fn a_tidy_run_leaves_thors_own_record_and_also_when_it_moved_nothing_but_not_for_an_empty_plan() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join("Desktop")).unwrap();
    let db = dir.path().join("thor.db");
    ensure_and_seed_store(&db).unwrap();
    let file = home.join("Desktop").join("cv.docx");
    std::fs::write(&file, "cv").unwrap();
    let folder = home.join("Documents").join("Cv en werk zoeken");
    let plan = serde_json::json!([{ "folder": folder.display().to_string(), "files": [file.display().to_string()] }]).to_string();

    let lines = ops::tidy::tidy(&db, &plan, &home).unwrap();
    assert!(lines.iter().any(|l| l.contains("moved 1 file(s)")), "{lines:?}");
    let store = EventStore::open_existing(&db).unwrap();
    let evidence = model::store::order_evidence(&store);
    assert_eq!(evidence.len(), 1, "{evidence:?}");
    assert_eq!(evidence[0].kind, "tidy");
    assert_eq!(evidence[0].body["moved"], 1);
    drop(store);

    // The same plan again moves nothing, but a plan was still carried out for a
    // valid folder, so it leaves a second record that says 0 files moved.
    ops::tidy::tidy(&db, &plan, &home).unwrap();
    let store = EventStore::open_existing(&db).unwrap();
    let again = model::store::order_evidence(&store);
    assert_eq!(again.len(), 2, "{again:?}");
    assert_eq!(again[1].body["moved"], 0);
    drop(store);

    // An empty plan (no folder at all) records nothing.
    ops::tidy::tidy(&db, "[]", &home).unwrap();
    let store = EventStore::open_existing(&db).unwrap();
    assert_eq!(model::store::order_evidence(&store).len(), 2);
}
