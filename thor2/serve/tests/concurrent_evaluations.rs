//! STEP 24, FIELD DEFECT 1 (2026-10-01): two sessions evaluated one project
//! at once. Their frozen lists overlapped; the first one's attested edits of
//! listed rules voided the other's evaluation, and neither report was ever
//! filed. These cases pin the two halves of the fix through the library the
//! hook and the tool server both use:
//! - an attested THOR call of ANOTHER session never voids an evaluation: the
//!   item it changed counts as settled by that session, credited to it in the
//!   machine section, and drops off this evaluation's obligations;
//! - a write stamped as another session's call that is not verified yet is
//!   neither: the evaluating session's own Stop checks it against that
//!   session's transcript, and one that cannot be checked is a side door.
//!
//! The one-evaluation-per-project half is pinned in `serve::eval_session`'s
//! own tests and, through the real hook, in `evaluation_debt_stop_hook.rs`.

use std::path::Path;

use model::item::{Binding, Item, Kind, TargetKind};
use thor_core::event_store::EventStore;

const S1: &str = "5e1f0c3a-0000-4000-8000-000000002401";
const S2: &str = "5e1f0c3a-0000-4000-8000-000000002402";
const PROJECT: &str = "fxp";

/// A decided rule of the fixture project (a note), so THOR's machine pass
/// leaves it alone and only the judgement list holds it.
fn noted_rule(store: &mut EventStore, id: &str) {
    let item = Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("Fixture rule {id}: run the demo linter before a release."),
        bindings: vec![Binding::Target { kind: TargetKind::Command, value: format!("demo-lint-{id}") }],
        severity: None,
        project: Some(PROJECT.to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the demo linter turns out to be optional".to_string()),
        check: None,
    };
    model::store::declare(store, "fixture", "fixture", "fixture", &item).unwrap();
    serve::teeth::fixture::decide(store, id);
}

/// `id` fired `n` times in `session` just now.
fn fire(store: &mut EventStore, session: &str, id: &str, n: usize) {
    let at = serve::time::iso8601_from_unix(serve::time::now_unix() - 60);
    for i in 0..n {
        serve::deliver::record_delivery_with_trigger(
            store,
            &serve::deliver::nth_session(session, i),
            session,
            serve::usefulness::ASSISTANT_DELIVERY_ACTOR,
            &at,
            &[id.to_string()],
            Some(&format!("demo-lint-{id} --all")),
        );
    }
}

/// S1's evaluation of the project, with `id` on its judgement list.
fn evaluating(db: &Path, store: &mut EventStore, id: &str) -> model::store::EvaluationStart {
    noted_rule(store, id);
    fire(store, S1, id, model::store::JUDGEMENT_DEBT_AFTER);
    let started = serve::eval_session::start(db, store, S1, PROJECT, "stop-ask").expect("S1 starts");
    assert_eq!(started.start.frozen.judgement, vec![id.to_string()], "fixture: the item is on S1's list");
    started.start
}

/// S2 rewords `id` through a THOR call - not verified yet.
fn reworded_by_s2(store: &mut EventStore, id: &str) -> i64 {
    let before = model::store::show(store, id).unwrap();
    let mut after = before.clone();
    after.text = format!("{} Reworded by the other session.", before.text);
    model::store::revise(store, S2, "mcp", "mcp", &before, &after).unwrap().seq
}

fn report(id: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Report,
        text: "S1 judged its list; one item was settled by the other session.".to_string(),
        bindings: vec![],
        severity: None,
        project: Some(PROJECT.to_string()),
        tags: vec![model::store::EVALUATION_REPORT_TAG.to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    }
}

/// The field case, as it ran: S2 rewords a rule that is on S1's frozen list
/// through an ATTESTED THOR call. That is no void any more - the item counts
/// as settled by S2, drops off S1's obligations, and S1's report files, with
/// the machine section crediting S2.
#[test]
fn another_sessions_attested_edit_is_credited_and_never_voids_this_evaluation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let root = dir.path().join("transcripts");
    let mut store = EventStore::new(&db).unwrap();
    let start = evaluating(&db, &mut store, "fx-shared");

    let seq = reworded_by_s2(&mut store, "fx-shared");
    serve::attest::fixture::attest_every_write(&mut store, S2);

    let written = model::store::frozen_writes_in_window(&store, &start, None);
    assert!(written.outside.is_empty(), "another session's attested call is no side door: {written:?}");
    assert_eq!(written.foreign, vec![("fx-shared".to_string(), seq, S2.to_string())]);
    let judgement = model::store::evaluation_judgement_owed(&store, &start, &[]);
    assert!(judgement.owed.is_empty(), "settled by S2, not S1's to judge: {judgement:?}");
    assert_eq!(judgement.credited, vec![("fx-shared".to_string(), S2.to_string())]);

    serve::attest::fixture::attested_filing(&mut store, &root, S1, &report("eval-fxp-1"), "toolu_eval_fxp_1")
        .expect("S1's report files: nothing was changed behind its back");
    let filed = model::store::show(&store, "eval-fxp-1").unwrap().text;
    assert!(
        filed.contains(&format!("- 'fx-shared': settled by session {S2} - an attested THOR call of that session changed it")),
        "{filed}"
    );
    assert!(filed.contains("1 settled by another session's attested call"), "{filed}");
}

/// A write stamped as another session's THOR call and not verified yet is
/// neither a void nor a credit: the filing names it and says THOR checks it
/// at this session's next Stop. That Stop checks it against the other
/// session's transcript - and when no transcript of it is on this machine,
/// it is a side door after all, and the evaluation is void as before.
#[test]
fn an_unchecked_write_of_another_session_is_checked_at_this_sessions_stop() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let root = dir.path().join("transcripts");
    std::fs::create_dir_all(&root).unwrap();
    let mut store = EventStore::new(&db).unwrap();
    let start = evaluating(&db, &mut store, "fx-pending");

    let before = model::store::show(&store, "fx-pending").unwrap();
    let mut after = before.clone();
    after.text = format!("{} Reworded through a call of the other session.", before.text);
    let att = model::store::Att {
        tool_use_id: Some("toolu_other_session".to_string()),
        args_sha256: "fixture".to_string(),
        session_hint: Some(S2.to_string()),
        inflight: Some(model::store::INFLIGHT_UNSEEN.to_string()),
        side_door: None,
    };
    let seq = model::store::revise_tried_scoped(&mut store, S2, "mcp", "mcp", &before, &after, &before, None, None, None, None, Some(&att))
        .unwrap()
        .seq;

    let written = model::store::frozen_writes_in_window(&store, &start, None);
    assert!(written.outside.is_empty(), "{written:?}");
    assert_eq!(written.foreign_pending, vec![("fx-pending".to_string(), seq, S2.to_string())]);
    let refused = serve::attest::fixture::attested_filing_unverified(&mut store, &root, S1, &report("eval-fxp-2"), "toolu_eval_fxp_2")
        .unwrap_err()
        .to_string();
    assert!(refused.contains("by another session on this evaluation's lists are not checked yet"), "{refused}");
    assert!(!refused.contains("outside an attested THOR call"), "not a void yet: {refused}");

    let recorded = serve::attest::record_foreign_write_verifications_in(&mut store, S1, Some(&root));
    assert_eq!(recorded, 1, "S1's Stop checked S2's write");
    let written = model::store::frozen_writes_in_window(&store, &start, None);
    assert_eq!(written.outside.len(), 1, "no transcript of S2 here: a side door after all: {written:?}");
    assert!(written.outside[0].door.contains(&format!("no transcript of session {S2}")), "{written:?}");
}
