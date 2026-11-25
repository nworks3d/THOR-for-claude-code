//! Doctor's short summary lists EVERY pinned note taken down through a tool call in
//! the last 30 days, whatever its reason, saying how the tool server let it through
//! (read from the event's own stamp): five lines, then the count of the rest.

use model::item::{Binding, Item, Kind, TargetKind};
use model::store::{with_att, Att, UnpinStamp, UNPIN_NO_USE, UNPIN_OWNER_WORDS};
use thor_core::event_store::{EventKind, EventStore};

const TEXTS: [&str; 7] = [
    "Say which folder a command ran in, before and after it.",
    "Release notes list the user-visible changes, newest first.",
    "Backups of the photo library run over the wired network only.",
    "Printer firmware updates wait until the weekend, never mid-job.",
    "Tomato seedlings go outside only after the last frost.",
    "Quote the invoice number in every payment reminder you draft.",
    "The garden shed key hangs behind the kitchen door.",
];

fn pinned(id: &str, n: usize) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: TEXTS[n].to_string(),
        bindings: vec![Binding::Always, Binding::Target { kind: TargetKind::Path, value: format!("src/{id}.rs") }],
        severity: None,
        project: Some("demo".to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("a command ran with no folder named and nobody minded".to_string()),
        check: None,
    }
}

#[test]
fn doctor_lists_every_take_down_five_lines_and_the_count_of_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    {
        let mut store = EventStore::new(&db).unwrap();
        let seed = Item {
            id: "seed-demo".to_string(),
            kind: Kind::Report,
            text: "the owner named the collection 'demo'".to_string(),
            bindings: vec![],
            severity: None,
            project: Some("demo".to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        model::store::declare(&mut store, "seed", "seed", "owner", &seed).unwrap();
        // A serving dated now, so the take-downs below are dated within the last 30 days.
        let served = format!("{{\"served_at\":\"{}\"}}", serve::time::now_iso8601());
        store.append_event("s", "l", "hook", EventKind::ItemServed, "some-note", None, &served).unwrap();
        for n in 0..7 {
            let id = format!("pinned-{n}");
            let item = pinned(&id, n);
            model::store::declare(&mut store, "s", "l", "owner", &item).unwrap();
            let mut down = item.clone();
            down.bindings = vec![Binding::Target { kind: TargetKind::Path, value: format!("src/{id}.rs") }];
            let mut body = serde_json::to_value(&down).unwrap();
            body["because"] = serde_json::Value::String(format!("reason {n}: nothing of it is left and nothing replaces it"));
            let (kind, witnessed) = if n == 0 { (UNPIN_OWNER_WORDS, true) } else { (UNPIN_NO_USE, false) };
            let att = Att { unpin: Some(UnpinStamp { kind: kind.to_string(), witnessed, into: None }), ..Default::default() };
            let body = with_att(body.to_string(), Some(&att)).unwrap();
            store.append_mutate_checked("s", "l", "mcp", EventKind::FactRevised, &id, None, &body).unwrap();
        }
    }
    let lines = ops::health::plain_summary(&db, dir.path(), None, None);
    let notes: Vec<&String> = lines.iter().filter(|l| l.contains("taken down") || l.contains("unpinned")).collect();
    assert_eq!(notes.len(), 6, "five listed and one line for the rest: {lines:?}");
    assert!(notes[0].contains("unpinned on the owner's own words: pinned-0"), "{notes:?}");
    assert!(notes[1].contains("without the owner's own words, no check and no recent use: pinned-1"), "{notes:?}");
    assert!(notes[5].contains("2 more pinned note(s) were taken down in the last 30 days"), "{notes:?}");
}

/// THOR's own setup note is retracted by the very step the first-session text asks
/// for, so the summary never warns about it.
#[test]
fn the_setup_note_taken_down_as_asked_is_not_listed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    {
        let mut store = EventStore::new(&db).unwrap();
        let served = format!("{{\"served_at\":\"{}\"}}", serve::time::now_iso8601());
        store.append_event("s", "l", "hook", EventKind::ItemServed, "some-note", None, &served).unwrap();
        let mut item = pinned(model::store::SETUP_NOTE_ID, 0);
        item.project = None;
        item.bindings = vec![Binding::Always];
        model::store::declare(&mut store, "install", "install", "install-fixture", &item).unwrap();
        let att = Att { unpin: Some(UnpinStamp { kind: UNPIN_NO_USE.to_string(), witnessed: false, into: None }), ..Default::default() };
        let body = serde_json::json!({ "retracted": model::store::SETUP_NOTE_ID, "reason": "Setup finished: the answers are on record." }).to_string();
        let body = with_att(body, Some(&att)).unwrap();
        store.append_mutate_checked("s", "l", "mcp", EventKind::FactRetracted, model::store::SETUP_NOTE_ID, None, &body).unwrap();
        assert_eq!(model::store::tool_unpins(&store, 0, i64::MAX).len(), 1, "fixture: it is a take-down THOR knows");
    }
    let lines = ops::health::plain_summary(&db, dir.path(), None, None);
    assert!(lines.iter().all(|l| !l.contains(model::store::SETUP_NOTE_ID)), "{lines:?}");
}
