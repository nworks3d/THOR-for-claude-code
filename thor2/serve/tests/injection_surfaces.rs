//! The three promises to the owner (CONTRACT.md / the workspace design
//! note), each proven against the real library, combining all three
//! injection surfaces in one place so no single surface's own unit tests can
//! hide an overlap between them:
//!
//! - `a_report_never_appears_on_any_injection_surface` - a Report is archive;
//!   it must never reach session start, the moment surface, or the prompt
//!   surface, no matter how closely its text resembles what is happening.
//! - `adding_five_hundred_archive_items_changes_no_injection_block_at_all` -
//!   "your store may grow without getting louder": the exact rendered output
//!   of all three injection surfaces must be byte-identical before and after
//!   500 archive items are declared.
//! - `a_prompt_that_resolves_to_nothing_gets_an_empty_block` - an ordinary
//!   prompt naming no action and no declared target renders to zero bytes,
//!   never a "closest N" fallback.
//! - `a_heavy_note_ranks_exactly_as_the_same_rule_did` - a rule THOR's trial
//!   puts on the note track (step 8) keeps its severity and ranks exactly
//!   where the same rule ranked before its trial.

use intent::Action;
use model::item::{Binding, Item, Kind, Severity, TargetKind};
use model::store;
use serve::input::ServeInput;
use serve::{live, prompt, rank, render, session_start};
use thor_core::event_store::EventStore;

fn rule_always(id: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("standing rule {id}"),
        bindings: vec![Binding::Always],
        severity: Some(Severity::Irreversible),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(format!("standing rule {id} turns out to be wrong")),
        check: None,
    }
}

fn rule_moment(id: &str, action: Action) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("never {} without checking first", action.as_str()),
        bindings: vec![Binding::Moment(action)],
        severity: Some(Severity::Irreversible),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(format!("{id} turns out to be safe without checking first")),
        check: None,
    }
}

/// A report worded to be as tempting a false-positive as possible for every
/// surface: it repeats "always", the moment action's own name, and the
/// prompt text's own words - if any surface fell back to closeness-style
/// ranking instead of a real binding match, this is the item that would leak.
fn tempting_report(id: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Report,
        text: "always remember: before you push, force, publish or deploy anything, read this report first"
            .to_string(),
        bindings: vec![],
        severity: None,
        project: Some("test-project".to_string()),
        tags: vec!["push".to_string(), "always".to_string()],
        expires: Some("2027-01-01".to_string()),
        key: None,
        falsifier: None,
        check: None,
    }
}

/// Capture the exact rendered output of the three injection surfaces for a
/// fixed set of inputs, so growth of the store can be compared byte for
/// byte. Empty string stands for "nothing rendered" (0 bytes), same as the
/// render functions' own `None` - kept as `String` so `assert_eq!` compares
/// plain bytes, not an `Option` wrapper.
fn capture(db: &EventStore) -> (String, String, String) {
    let candidates = live::live_items(db);

    let start_items = session_start::select(&candidates, Some("thor2"));
    let start_block = session_start::render(&start_items).unwrap_or_default();

    let mut moment_input = ServeInput::default();
    moment_input.add_moment(Action::Push);
    let moment_all = rank::select(&candidates, &moment_input);
    let moment_selection = render::cap(moment_all);
    let moment_block = render::render_text(&moment_selection, &moment_input).unwrap_or_default();

    let prompt_text = "please run git push --force origin main right now";
    let prompt_input = prompt::resolve(prompt_text, &candidates);
    let prompt_all = rank::select(&candidates, &prompt_input);
    let prompt_selection = render::cap(prompt_all);
    let prompt_block = render::render_text(&prompt_selection, &prompt_input).unwrap_or_default();

    (start_block, moment_block, prompt_block)
}

#[test]
fn a_report_never_appears_on_any_injection_surface() {
    let mut db = EventStore::in_memory().unwrap();
    store::declare(&mut db, "s", "l", "a", &rule_always("always-1")).unwrap();
    store::declare(&mut db, "s", "l", "a", &rule_moment("moment-1", Action::Push)).unwrap();
    store::declare(&mut db, "s", "l", "a", &tempting_report("report-1")).unwrap();

    let candidates = live::live_items(&db);

    // Surface 1: session start.
    let start_items = session_start::select(&candidates, Some("thor2"));
    assert!(start_items.iter().all(|r| r.id != "report-1"), "a Report must never reach session start");

    // Surface 2: moment of action.
    let mut moment_input = ServeInput::default();
    moment_input.add_moment(Action::Push);
    let moment_hits = rank::select(&candidates, &moment_input);
    assert!(moment_hits.iter().all(|r| r.id != "report-1"), "a Report must never reach the moment surface");

    // Surface 3: per prompt.
    let prompt_input = prompt::resolve("please run git push --force origin main right now", &candidates);
    let prompt_hits = rank::select(&candidates, &prompt_input);
    assert!(prompt_hits.iter().all(|r| r.id != "report-1"), "a Report must never reach the prompt surface");
}

#[test]
fn adding_five_hundred_archive_items_changes_no_injection_block_at_all() {
    let mut db = EventStore::in_memory().unwrap();
    store::declare(&mut db, "s", "l", "a", &rule_always("always-1")).unwrap();
    store::declare(&mut db, "s", "l", "a", &rule_moment("moment-1", Action::Push)).unwrap();

    let before = capture(&db);
    assert!(
        !before.0.is_empty() && !before.1.is_empty() && !before.2.is_empty(),
        "the fixture must actually produce output on all three surfaces before growth, or this test proves nothing: {before:?}"
    );

    for i in 0..500 {
        let item = Item {
            id: format!("archive-{i}"),
            kind: if i % 2 == 0 { Kind::Report } else { Kind::Chunk },
            // Three tokens that each embed `i` as part of the word itself
            // (`entry{i}`, `marker{i}`, `moment{i}`), not as a separately
            // repeated bare number - model::store's near-duplicate check
            // compares normalised WORD SETS, so the same number written
            // twice in one string (the original shape here) collapses to a
            // single shared token and every Report (or every Chunk) ends up
            // a Jaccard near-duplicate of the last one declared. Three
            // distinct varying tokens against a fixed common template of 9
            // words keeps every pairwise Jaccard at 9/15 = 0.6, safely under
            // the 0.8 refusal threshold, while every text is still genuinely
            // distinct - this loop is about proving bulk, not repeating one
            // string 500 times.
            text: format!(
                "archive filler entry{i} marker{i}: always push force publish deploy standing rule moment{i}"
            ),
            bindings: vec![],
            severity: None,
            project: Some("test-project".to_string()),
            tags: vec!["push".to_string(), "always".to_string(), format!("tag-{i}")],
            expires: if i % 2 == 0 { Some("2027-01-01".to_string()) } else { None },
            key: None,
            falsifier: None,
            check: None,
        };
        store::declare(&mut db, "s", "l", "a", &item).unwrap();
    }

    let after = capture(&db);
    assert_eq!(
        before, after,
        "adding 500 archive items must not change any injection surface's output at all - the store may grow without getting louder"
    );
}

#[test]
fn a_prompt_that_resolves_to_nothing_gets_an_empty_block() {
    let mut db = EventStore::in_memory().unwrap();
    store::declare(&mut db, "s", "l", "a", &rule_always("always-1")).unwrap();
    store::declare(&mut db, "s", "l", "a", &rule_moment("moment-1", Action::Push)).unwrap();
    store::declare(
        &mut db,
        "s",
        "l",
        "a",
        &Item {
            id: "target-1".to_string(),
            kind: Kind::Orientation,
            text: "the release checklist lives in RELEASE.md".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: "RELEASE.md".to_string() }],
            severity: Some(Severity::HouseStyle),
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("the release checklist moves out of RELEASE.md".to_string()),
            check: None,
        },
    )
    .unwrap();

    let candidates = live::live_items(&db);
    let input = prompt::resolve(
        "What do you think the weather will be like this weekend, in general?",
        &candidates,
    );
    assert!(input.is_empty(), "an ordinary prompt must resolve to no moment and no target");

    let all = rank::select(&candidates, &input);
    let selection = render::cap(all);
    let block = render::render_text(&selection, &input);
    assert!(block.is_none(), "an ordinary prompt must render to nothing, never a ranked fallback");
    assert_eq!(block.map(|s| s.len()).unwrap_or(0), 0, "zero bytes, not almost zero");
}

/// Step 8: a heavy rule whose trial finds no working tooth becomes a note -
/// stored without a check, labelled `teeth:note`, its severity kept. A note
/// still arrives at its moment, ranked by its severity: it must rank exactly
/// where the same rule ranked before its trial, among heavier and lighter
/// neighbours.
#[test]
fn a_heavy_note_ranks_exactly_as_the_same_rule_did() {
    let neighbours = |db: &mut EventStore| {
        let mut costly = rule_moment("moment-costly", Action::Push);
        costly.severity = Some(Severity::Costly);
        costly.text = "never push a branch nobody reviewed".to_string();
        let mut house = rule_moment("moment-house", Action::Push);
        house.severity = Some(Severity::HouseStyle);
        house.text = "push small commits, one change each".to_string();
        let mut other = rule_moment("moment-other", Action::Push);
        other.text = "never push while a migration is half applied".to_string();
        for item in [costly, house, other] {
            store::declare(db, "s", "l", "a", &item).unwrap();
        }
    };
    let ranked = |db: &EventStore| -> Vec<(String, Option<Severity>)> {
        let mut input = ServeInput::default();
        input.add_moment(Action::Push);
        rank::select(&live::live_items(db), &input).into_iter().map(|r| (r.id.clone(), r.item.severity)).collect()
    };

    let mut before = EventStore::in_memory().unwrap();
    store::declare(&mut before, "s", "l", "a", &rule_moment("moment-1", Action::Push)).unwrap();
    neighbours(&mut before);

    let mut after = EventStore::in_memory().unwrap();
    store::declare(&mut after, "s", "l", "a", &rule_moment("moment-1", Action::Push)).unwrap();
    neighbours(&mut after);
    let corpus = serve::legit::Corpus::load(&after);
    let retest = serve::teeth::retest(&after, None, "moment-1", &corpus).unwrap();
    serve::teeth::apply_as_retest(&mut after, &retest).unwrap();

    let note = live::live_items(&after).into_iter().find(|li| li.id == "moment-1").unwrap();
    assert_eq!(note.item.tags, vec!["teeth:note".to_string()], "the trial put it on the note track");
    assert!(note.item.check.is_none(), "a note carries no check");
    assert_eq!(note.item.severity, Some(Severity::Irreversible), "a note keeps its severity");

    let (was, is) = (ranked(&before), ranked(&after));
    assert_eq!(was.len(), 4, "{was:?}");
    assert_eq!(is, was, "the note ranks exactly where the rule did");
}
