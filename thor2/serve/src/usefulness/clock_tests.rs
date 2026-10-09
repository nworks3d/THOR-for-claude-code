//! Step 17 (B8, cadence, D13): the project's work clock, the cadence switch
//! and the weekly floor.

use super::*;
use model::item::{Binding, Item, Kind, TargetKind};

const NOW: i64 = 1_800_014_400;
const MIN: i64 = 60;
const DAY: i64 = 86_400;

/// Claude Code sessions (UUID-shaped: only such a session counts as log
/// activity, and only such a session has a transcript).
const S1: &str = "5e1f0c3a-0000-4000-8000-000000000001";
const S2: &str = "5e1f0c3a-0000-4000-8000-000000000002";

fn sidecar(db: &Path, project: &str) -> ProjectEvalState {
    project_eval_state(db, Some(project))
}

fn rule(id: &str, project: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("fixture rule {id} of {project}"),
        bindings: vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-command-{id}") }],
        severity: None,
        project: Some(project.to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(format!("fixture rule {id} turns out to be wrong")),
        check: None,
    }
}

/// A valid evaluation of `project`, filed `days_ago` days before the real
/// now as far as its transcript tells, and validated as a Stop validates it.
fn valid_evaluation(store: &mut EventStore, root: &Path, project: &str, days_ago: i64) {
    let report = crate::eval_filing::fixture::report(&format!("eval-{project}-fixture"), project, "TLDR: fixture evaluation.");
    let at = crate::time::now_unix() - days_ago * DAY;
    crate::eval_filing::fixture::valid_filing_at(store, root, crate::attest::fixture::SESSION, &report, "toolu_fixture_eval", at);
}

// ------------------------------------------------------------ the clock

/// B8: two sessions working side by side in one project advance the
/// project's clock once - every gap is measured from the PROJECT's previous
/// event, whichever session made it - while each session keeps its own.
#[test]
fn project_clock_counts_parallel_sessions_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    for (at, session) in [(0, S1), (MIN, S2), (2 * MIN, S1), (3 * MIN, S2), (4 * MIN, S1), (5 * MIN, S2)] {
        record_hook_event(&db, Some("fxp"), session, false, NOW + at, HookEventKind::Other).unwrap();
    }
    let state = sidecar(&db, "fxp");
    assert_eq!(state.work.accrued_secs, 5 * MIN, "the project: five one-minute gaps, counted once");
    assert_eq!(state.sessions[S1].accrued_secs, 4 * MIN, "S1's own: two two-minute gaps");
    assert_eq!(state.sessions[S2].accrued_secs, 4 * MIN, "S2's own: two two-minute gaps");
    // Two sessions one after the other add up (R17.1 in miniature): 40
    // minutes of S1, then 25 of S2, is 65 minutes of project work.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    for i in 0..=20 {
        record_hook_event(&db, Some("fxp"), S1, false, NOW + i * 2 * MIN, HookEventKind::Other).unwrap();
    }
    for i in 0..=12 {
        record_hook_event(&db, Some("fxp"), S2, false, NOW + 42 * MIN + i * 2 * MIN, HookEventKind::Other).unwrap();
    }
    let state = sidecar(&db, "fxp");
    assert_eq!(state.work.accrued_secs / MIN, 40 + 2 + 24, "{:?}", state.work);
    assert!(eval_debt_owed(EvalCadence::Weekly, state.work.accrued_secs / MIN, None, NOW + 70 * MIN, false));
    assert!(!eval_debt_owed(EvalCadence::Weekly, state.sessions[S1].accrued_secs / MIN, None, NOW + 70 * MIN, false));
}

/// Only a new VALID evaluation, read from the log at a Stop, resets the
/// clock: a hand-set `last_evaluation_seen` resets nothing at the next hook
/// event (the step-16 leftover), a report on file that does not count resets
/// nothing, and the valid one resets the project and its sessions - keeping
/// each session's Stop count.
#[test]
fn project_clock_resets_only_on_a_valid_evaluation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let root = tempfile::tempdir().unwrap();
    let mut store = EventStore::new(&db).unwrap();
    // Real time, an hour back: the valid evaluation below is filed "now".
    let t0 = crate::time::now_unix() - 3600;
    for i in 0..=10 {
        record_hook_event(&db, Some("fxp"), S1, false, t0 + i * 2 * MIN, HookEventKind::Other).unwrap();
    }
    record_main_stop(&db, Some("fxp"), S1, t0 + 21 * MIN).unwrap();
    assert_eq!(sidecar(&db, "fxp").work.accrued_secs, 21 * MIN, "fixture sanity");

    // The sidecar edited by hand: a report "seen" now, and none behind it.
    let mut all = read_eval_debt_state(&db);
    let entry = all.get_mut("fxp").unwrap();
    entry.last_evaluation_seen = Some(t0 + 22 * MIN);
    entry.last_evaluation_report_id = Some("eval-fxp-invented".to_string());
    std::fs::write(eval_debt_state_path(&db), serde_json::to_string(&all).unwrap()).unwrap();
    let after = record_hook_event(&db, Some("fxp"), S1, false, t0 + 23 * MIN, HookEventKind::Other).unwrap();
    assert_eq!(after.accrued_secs, 23 * MIN, "a hook event never resets from the sidecar's own copy");
    assert_eq!(sidecar(&db, "fxp").work.accrued_secs, 23 * MIN);

    // A report on file that does not count: the Stop puts the sidecar back
    // to what the log says and resets nothing.
    let report = crate::eval_filing::fixture::report("eval-fxp-unshown", "fxp", "TLDR: filed without its start note.");
    crate::attest::fixture::filing_without_the_start_note(&mut store, root.path(), S2, &report, "toolu_unshown").unwrap();
    crate::attest::record_write_verifications_in(&mut store, S2, Some(root.path()));
    crate::eval_filing::validate_new_reports(&mut store, Some(root.path()), S2);
    let record = crate::eval_filing::current_validation(&store, &report.id).expect("the Stop recorded its check");
    assert!(!record.valid, "fixture sanity: a report that does not count: {record:?}");
    let state = update_eval_debt_state(&store, &db, Some("fxp"), t0 + 24 * MIN);
    assert_eq!((state.last_evaluation_seen, state.work.accrued_secs), (None, 23 * MIN));
    assert_eq!(state.sessions[S1].accrued_secs, 23 * MIN);

    // A valid evaluation: the project clock and every session reset.
    valid_evaluation(&mut store, root.path(), "fxp", 0);
    let state = update_eval_debt_state(&store, &db, Some("fxp"), crate::time::now_unix());
    assert!(state.last_evaluation_seen.is_some(), "the valid report is seen");
    assert_eq!(state.work.accrued_secs, 0, "the project clock restarts");
    assert_eq!(state.sessions[S1].accrued_secs, 0, "and the session's own");
    assert_eq!(state.sessions[S1].stops, 1, "the Stop count survives: it counts Stops, not work");
}

/// A sidecar written before the project clock existed starts it from its
/// largest session accrual - the upgrade never restarts a project at zero.
#[test]
fn an_old_sidecar_starts_the_project_clock_from_its_largest_session() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let text = format!(
        r#"{{"fxp":{{"sessions":{{"{S1}":{{"anchor_unix":{},"accrued_secs":3000,"last_event_unix":{}}},"{S2}":{{"anchor_unix":{},"accrued_secs":600,"last_event_unix":{}}}}}}}}}"#,
        NOW - 3600,
        NOW - 60,
        NOW - 900,
        NOW - 30
    );
    std::fs::write(eval_debt_state_path(&db), text).unwrap();
    record_hook_event(&db, Some("fxp"), S2, false, NOW, HookEventKind::Other).unwrap();
    assert_eq!(sidecar(&db, "fxp").work.accrued_secs, 3000 + 60, "S1's 50 minutes, plus the gap since its last event");
}

// ------------------------------------------------------------ the cadence

/// The rhythm follows the plan, and doctor's sentence says so. A small plan
/// whose valid evaluation is two days old is quiet; a week-old one is due.
#[test]
fn the_shipped_cadence_follows_the_plan() {
    assert_eq!(cadence_for(crate::plan::Plan::Small), EvalCadence::Weekly);
    assert_eq!(cadence_for(crate::plan::Plan::Max), EvalCadence::WeeklyUnlessRot);
    assert!(!eval_debt_owed(EvalCadence::Weekly, 61, Some(NOW - 2 * DAY), NOW, false));
    assert!(eval_debt_owed(EvalCadence::Weekly, 61, Some(NOW - 8 * DAY), NOW, false));
}

/// WeeklyUnlessRot on a clean project: a two-day-old valid evaluation keeps
/// it quiet however long it is worked; an eight-day-old one does not; and
/// there is no repeat ask at all. A small plan is just as quiet.
#[test]
fn weekly_mode_is_quiet_for_a_clean_project() {
    let weekly = EvalCadence::WeeklyUnlessRot;
    assert!(!eval_debt_owed(weekly, 600, Some(NOW - 2 * DAY), NOW, false), "no repeat in weekly mode");
    assert!(eval_debt_owed(weekly, 61, Some(NOW - 8 * DAY), NOW, false));
    assert!(!eval_debt_owed(weekly, 59, Some(NOW - 8 * DAY), NOW, false), "still an hour of work first");
    assert!(eval_debt_owed(weekly, 61, None, NOW, false), "never evaluated: due after an hour");

    // End to end on a store: a valid evaluation two days old, nothing owed -
    // both plans are quiet.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let root = tempfile::tempdir().unwrap();
    let mut store = EventStore::new(&db).unwrap();
    valid_evaluation(&mut store, root.path(), "fxp", 2);
    let now = crate::time::now_unix();
    let state = update_eval_debt_state(&store, &db, Some("fxp"), now);
    assert_eq!(crate::eval_filing::project_rot(&store, "fxp"), None, "fixture sanity: a clean project");
    let work = SessionWorkState { accrued_secs: 90 * MIN, ..Default::default() };
    assert_eq!(project_due(&store, &state, "fxp", &work, now, weekly), None, "max, clean: quiet");
    assert_eq!(project_due(&store, &state, "fxp", &work, now, EvalCadence::Weekly), None, "small plan: quiet");
}

/// WeeklyUnlessRot goes daily on rot: the same two-day-old evaluation, with
/// one rule of the project left without a verdict, is due after an hour on
/// Max - and a small plan stays strictly weekly, whatever is found.
#[test]
fn weekly_mode_goes_daily_on_rot_on_max_only() {
    let weekly = EvalCadence::WeeklyUnlessRot;
    assert!(eval_debt_owed(weekly, 61, Some(NOW - 2 * DAY), NOW, true));
    assert!(!eval_debt_owed(weekly, 61, Some(NOW - 10 * 3600), NOW, true), "inside a day even rot waits");

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let root = tempfile::tempdir().unwrap();
    let mut store = EventStore::new(&db).unwrap();
    // The rot item is older than the evaluation, so nothing but the rule
    // itself makes the project rot.
    model::store::declare(&mut store, "fixture", "fixture", "fixture", &rule("fx-rot", "fxp")).unwrap();
    valid_evaluation(&mut store, root.path(), "fxp", 2);
    let now = crate::time::now_unix();
    let state = update_eval_debt_state(&store, &db, Some("fxp"), now);
    let why = crate::eval_filing::project_rot(&store, "fxp").expect("a rule without a verdict is rot");
    assert!(why.contains("fx-rot"), "{why}");
    let work = SessionWorkState { accrued_secs: 90 * MIN, ..Default::default() };
    assert_eq!(project_due(&store, &state, "fxp", &work, now, weekly), Some(false), "max with rot: due");
    assert_eq!(project_due(&store, &state, "fxp", &work, now, EvalCadence::Weekly), None, "small plan with rot: still weekly");
}

// -------------------------------------------------------- the weekly floor

/// D13: the floor reads the log only - no sidecar is even consulted. Served
/// items of the project from a Claude Code session in the last seven days,
/// and no valid evaluation in seven days, make it due; activity only older
/// than that, activity only from a session that is not Claude Code's, or a
/// valid evaluation three days old, do not.
#[test]
fn the_weekly_floor_reads_the_log_only() {
    let now = crate::time::now_unix();
    let served = |store: &mut EventStore, session: &str, days_ago: i64| {
        let at = crate::time::iso8601_from_unix(now - days_ago * DAY);
        crate::deliver::record_delivery(store, session, session, ASSISTANT_DELIVERY_ACTOR, &at, &["fx-rule".to_string()]);
    };
    let fresh = || {
        let mut store = EventStore::in_memory().unwrap();
        model::store::declare(&mut store, "fixture", "fixture", "fixture", &rule("fx-rule", "fxp")).unwrap();
        store
    };

    let mut store = fresh();
    assert!(!weekly_floor_due(&store, "fxp", now), "no activity at all");
    for days_ago in [1, 3, 5] {
        served(&mut store, S1, days_ago);
    }
    assert!(weekly_floor_due(&store, "fxp", now), "activity on three of the last seven days, never evaluated");
    assert!(!weekly_floor_due(&store, "gamma", now), "another project's activity is not this one's");

    let mut store = fresh();
    served(&mut store, S1, 9);
    assert!(!weekly_floor_due(&store, "fxp", now), "activity only nine days ago");

    let mut store = fresh();
    served(&mut store, "fixture-session", 1);
    assert!(!weekly_floor_due(&store, "fxp", now), "a session that is not Claude Code's is not activity");

    let dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut store = EventStore::new(&dir.path().join("thor.db")).unwrap();
    model::store::declare(&mut store, "fixture", "fixture", "fixture", &rule("fx-rule", "fxp")).unwrap();
    valid_evaluation(&mut store, root.path(), "fxp", 3);
    served(&mut store, S1, 1);
    assert!(!weekly_floor_due(&store, "fxp", now), "a valid evaluation three days old");
    let mut store = EventStore::new(&tempfile::tempdir().unwrap().path().join("thor.db")).unwrap();
    model::store::declare(&mut store, "fixture", "fixture", "fixture", &rule("fx-rule", "fxp")).unwrap();
    valid_evaluation(&mut store, root.path(), "fxp", 8);
    served(&mut store, S2, 1);
    assert!(weekly_floor_due(&store, "fxp", now), "a valid evaluation eight days old, activity yesterday");
}

/// A write carries no time of its own: it is dated by the newest dated event
/// before it - a lower bound, so an undated write never reads as recent.
#[test]
fn a_write_is_dated_by_the_served_item_before_it() {
    let now = crate::time::now_unix();
    let mut store = EventStore::in_memory().unwrap();
    let at = |days: i64| crate::time::iso8601_from_unix(now - days * DAY);
    crate::deliver::record_delivery(&mut store, "fixture", "fixture", ASSISTANT_DELIVERY_ACTOR, &at(10), &["other".to_string()]);
    model::store::declare(&mut store, S1, "mcp", "mcp", &rule("fx-written", "fxp")).unwrap();
    assert!(!weekly_floor_due(&store, "fxp", now), "written after an event ten days old: not provably recent");
    crate::deliver::record_delivery(&mut store, "fixture", "fixture", ASSISTANT_DELIVERY_ACTOR, &at(1), &["other".to_string()]);
    let mut second = rule("fx-written-2", "fxp");
    second.text = "a second fixture rule, about the backup rotation of fxp".to_string();
    model::store::declare(&mut store, S1, "mcp", "mcp", &second).unwrap();
    let (any, sessions) = project_activity(&store, "fxp", now - EVAL_FLOOR_DAYS * DAY, false);
    assert!(any && sessions.contains(S1), "written after an event of yesterday: {sessions:?}");
}

/// Part G, the rot signals THOR keeps beside the store: an open THOR bug
/// candidate is rot on any project (and ends once the candidate is reported),
/// and a sweep that never ran flags nothing.
#[test]
fn an_open_thor_bug_candidate_is_rot_until_it_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let root = tempfile::tempdir().unwrap();
    let mut store = EventStore::new(&db).unwrap();
    valid_evaluation(&mut store, root.path(), "fxp", 2);
    assert_eq!(crate::eval_filing::project_rot(&store, "fxp"), None, "fixture sanity: a clean project");
    assert!(crate::sweep::flagged_rule_ids(&db, &store).is_empty(), "no sweep ever ran");

    crate::thor_bugs::add(&db, crate::thor_bugs::hook_launch_finding("Stop", 1));
    let why = crate::eval_filing::project_rot(&store, "fxp").expect("an open bug candidate is rot");
    assert!(why.contains("THOR bug candidate"), "{why}");

    // Once the candidate is reported, nothing is open: back to weekly.
    let mut book = crate::thor_bugs::load(&db);
    for candidate in &mut book.candidates {
        candidate.status = crate::thor_bugs::Status::Reported;
    }
    std::fs::write(db.parent().unwrap().join(crate::thor_bugs::FILE), serde_json::to_string(&book).unwrap()).unwrap();
    assert_eq!(crate::eval_filing::project_rot(&store, "fxp"), None, "reported: the rot is gone");
}
