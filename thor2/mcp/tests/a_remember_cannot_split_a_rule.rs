//! A `remember` under the id of a live note used to be stored as a SECOND head
//! of that note: the note diverged, a call the rule refused was let through,
//! and a `resolve` keeping the new head removed the rule, its tags, its check
//! and its pin without the owner's word (found by an attack test, 2026-11-25).
//! The door now refuses the second write, and a `resolve` that discards a
//! pinned note passes the gate an unpin passes.

mod common;

use common::*;
use model::item::{Binding, Item, Kind, Severity};
use serde_json::json;
use thor_core::event_store::{EventKind, EventStore};

fn pinned_note(id: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Orientation,
        text: "The staging server answers only over the VPN.".to_string(),
        bindings: vec![Binding::Always],
        severity: Some(Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("staging answers with the VPN off".to_string()),
        check: None,
    }
}

#[test]
fn a_remember_under_the_id_of_a_live_note_is_refused_and_the_note_stays_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    {
        // A pin is the owner's to add, so the first note is written the way the installer writes one.
        let mut raw = EventStore::new(&db).unwrap();
        let mut first = pinned_note("one-rule");
        first.text = "Never run the deploy script without a dry run first.".to_string();
        first.project = Some("demo".to_string());
        model::store::declare(&mut raw, "s", "l", "owner", &first).unwrap();
    }
    let mut mcp = Mcp::start(&db, tmp.path());

    let mut args = rule("one-rule", "A completely different statement about log rotation settings.", Some("demo"));
    args["always"] = json!(true);
    let second = mcp.call("remember", args);
    println!("{second}");
    assert!(second.starts_with("REFUSED:") && second.contains("already in use") && second.contains("revise"), "{second}");

    let got = mcp.call("get", json!({ "id": "one-rule" }));
    assert!(got.trim_start().starts_with('{') && !got.contains("diverged"), "{got}");
    assert!(got.contains("deploy script"), "{got}");
}

#[test]
fn a_resolve_that_drops_a_pinned_note_needs_the_owner_and_one_that_keeps_it_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let (pinned_head, bare_head);
    {
        let mut raw = EventStore::new(&db).unwrap();
        let first = model::store::declare(&mut raw, "s", "l", "owner", &pinned_note("vpn-note")).unwrap();
        let mut bare = pinned_note("vpn-other");
        bare.bindings = vec![];
        bare.tags = vec!["fixture".to_string()];
        bare.text = "The staging server is reachable from anywhere.".to_string();
        let body = model::store::canonical_body(&bare).unwrap();
        // The state an older build could reach: a second head under the same id.
        let second = raw.append_event("s", "l", "owner", EventKind::FactCreated, "vpn-note", None, &body).unwrap();
        pinned_head = first.this_hash;
        bare_head = second.this_hash;
    }
    let mut mcp = Mcp::start(&db, tmp.path());
    let refused = mcp.call("resolve", json!({ "id": "vpn-note", "keep": bare_head, "discard": [pinned_head] }));
    println!("{refused}");
    assert!(refused.starts_with("REFUSED:") && refused.contains("because"), "{refused}");
    assert!(!refused.starts_with("resolved"), "{refused}");

    let kept = mcp.call("resolve", json!({ "id": "vpn-note", "keep": pinned_head, "discard": [bare_head] }));
    assert!(kept.starts_with("resolved vpn-note"), "{kept}");
}
