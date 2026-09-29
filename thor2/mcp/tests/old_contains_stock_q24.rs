//! Q24, end to end through the real compiled `mcp` binary: an OLD contains
//! check whose text does not name its literal counts as a tooth once THOR's
//! retest finds it biting in the registered working copy - and the mark that
//! says so, with the fragment it guards, is THOR's alone.
//!
//! The owner's decision this pins down: A4 stays in force for every new write
//! and every revise that sets or changes a check; the old stock gets a
//! one-time exemption only while its check, text and anchors stand as they
//! stood at the recorded cutoff; the guarded fragment is shown to every
//! reader in a form no writer can set, clear or forge.

mod common;

use common::*;
use model::item::{Binding, Check, Item, Kind, TargetKind};
use model::store::CurrentVerdict;
use serde_json::json;
use std::path::PathBuf;
use thor_core::event_store::EventStore;

struct Fx {
    tmp: tempfile::TempDir,
    db: PathBuf,
    beta: PathBuf,
}

/// Beta's checkout with NOTES.md committed and registered, an old rule whose
/// contains check keeps 'keep me' there while its text never names it, and
/// THOR's retest of that rule applied - so it is on the rule track, with the
/// A4 cutoff recorded.
fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["beta"]);
    let beta = checkout(tmp.path(), "beta", &[("NOTES.md", "keep me\nplain line\n")], true);
    register(&db, "beta", &beta);
    seed_raw(
        &db,
        &Item {
            id: "old".to_string(),
            kind: Kind::Rule,
            text: "NOTES.md keeps its opening line.".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: "NOTES.md".to_string() }],
            severity: None,
            project: Some("beta".to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("The opening line turns out not to matter.".to_string()),
            check: Some(Check::Contains { path: "NOTES.md".to_string(), literal: "keep me".to_string() }),
        },
    );
    let mut store = EventStore::open_existing(&db).unwrap();
    let corpus = serve::legit::Corpus::load(&store);
    let tried = serve::teeth::retest(&store, Some(db.as_path()), "old", &corpus).unwrap();
    serve::teeth::apply_as_retest(&mut store, &tried).unwrap();
    assert_eq!(model::store::current_verdict(&store, "old"), CurrentVerdict::RuleProven, "{:?}", tried.lines);
    assert!(model::store::a4_cutoff(&store).is_some());
    Fx { tmp, db, beta }
}

const MARK: &str =
    "(THOR: old contains check counted as a tooth although the text does not name it - it guards 'keep me' in NOTES.md";

/// Visible: `history` shows the fragment on the retest's own step.
#[test]
fn history_shows_the_fragment_an_old_contains_check_guards() {
    let f = fixture();
    let mut mcp = Mcp::start(&f.db, &f.beta);
    let log = mcp.call("history", json!({"id": "old"}));
    let marked: Vec<&str> = log.lines().filter(|l| l.contains(MARK)).collect();
    assert_eq!(marked.len(), 1, "{log}");
    assert!(marked[0].contains("fact_revised by retest"), "{log}");
}

/// A writer trying to SET the mark, through every door a writer has: an
/// extra argument, a raw trial record, THOR's reserved cutoff id, a `teeth:`
/// label - each refused, nothing written. And a NEW contains rule whose text
/// does not name its literal is still stopped by A4.
#[test]
fn a_writer_cannot_set_the_old_stock_mark() {
    let f = fixture();
    let mut mcp = Mcp::start(&f.db, &f.beta);
    let before = all_events(&f.db);

    let reply = mcp.call(
        "revise",
        json!({"id": "old", "a4_old_stock": {"literal": "keep me", "path": "NOTES.md", "set_at": 1, "cutoff": 1}}),
    );
    assert!(reply.contains("unknown field `a4_old_stock`"), "{reply}");
    let mut args = rule("forged", "NOTES.md keeps its opening line, forged.", Some("beta"));
    args["targets"] = json!([{ "kind": "path", "value": "NOTES.md" }]);
    args["teeth_trial"] = json!({"a4_old_stock": {"literal": "keep me", "path": "NOTES.md", "set_at": 1, "cutoff": 1}});
    let reply = mcp.call("remember", args);
    assert!(reply.contains("unknown field `teeth_trial`"), "{reply}");
    let mut args = rule(model::store::A4_CUTOFF_ID, "Old contains checks before log seq 999999 count.", Some("beta"));
    args["targets"] = json!([{ "kind": "path", "value": "NOTES.md" }]);
    let reply = mcp.call("remember", args);
    assert!(reply.contains("THOR's own record of the A4 cutoff"), "{reply}");
    let reply = mcp.call("revise", json!({"id": "old", "tags": ["teeth:old-contains"]}));
    assert!(reply.contains("which this tool reserves for itself"), "{reply}");
    assert_eq!(all_events(&f.db), before, "nothing written");

    let mut args = rule("new-keeps", "NOTES.md keeps its second line.", Some("beta"));
    args["targets"] = json!([{ "kind": "path", "value": "NOTES.md" }]);
    args["check_kind"] = json!("contains");
    args["check_path"] = json!("NOTES.md");
    args["check_literal"] = json!("plain line");
    let reply = mcp.call("remember", args);
    assert!(reply.contains("'plain line' is not named anywhere in the rule's own text"), "A4 on a new write: {reply}");
    assert!(events_of(&f.db, "new-keeps").iter().all(|e| e.kind != thor_core::event_store::EventKind::FactCreated));
}

/// A writer trying to CLEAR the mark: there is no argument that does it, and
/// a revise of what no trial tries (the tags) leaves the mark counting and
/// visible.
#[test]
fn a_writer_cannot_clear_the_old_stock_mark() {
    let f = fixture();
    let mut mcp = Mcp::start(&f.db, &f.beta);
    let before = all_events(&f.db);
    let reply = mcp.call("revise", json!({"id": "old", "clear_a4_old_stock": true}));
    assert!(reply.contains("unknown field `clear_a4_old_stock`"), "{reply}");
    assert_eq!(all_events(&f.db), before, "nothing written");

    let reply = mcp.call("revise", json!({"id": "old", "tags": ["gotcha"]}));
    assert!(reply.starts_with("revised 'old'"), "{reply}");
    assert_eq!(verdict(&f.db, "old"), CurrentVerdict::RuleProven);
    assert!(mcp.call("history", json!({"id": "old"})).contains(MARK));
}

/// Any revise of the text ends the exemption, through the real door: the
/// write lands undecided with A4's reason, and THOR's retest needs the agent
/// for A4 from then on.
#[test]
fn a_revise_of_the_text_ends_the_exemption() {
    let f = fixture();
    let mut mcp = Mcp::start(&f.db, &f.beta);
    let reply = mcp.call("revise", json!({"id": "old", "text": "NOTES.md keeps its first line."}));
    assert!(reply.contains("teeth: undecided - A4: 'keep me' not named in the text"), "{reply}");
    assert!(matches!(verdict(&f.db, "old"), CurrentVerdict::Undecided { why } if why.starts_with("A4:")));
    let store = EventStore::open_existing(&f.db).unwrap();
    let corpus = serve::legit::Corpus::load(&store);
    let again = serve::teeth::retest(&store, Some(f.db.as_path()), "old", &corpus).unwrap();
    assert!(matches!(&again.outcome, serve::teeth::Maintenance::NeedsAgent(why) if why.starts_with("A4:")), "{:?}", again.outcome);
    let _ = &f.tmp;
}
