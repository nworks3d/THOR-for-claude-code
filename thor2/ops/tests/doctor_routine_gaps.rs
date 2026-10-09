//! THOR checks its own doctor against its own routine: every labelled line
//! `doctor --eval` prints must be named in the evaluation routine THOR ships
//! (`eval-command.example.md`), or an agent is left to guess what it means.
//! A gap becomes a THOR bug candidate (`serve::thor_bugs`).

use ops::health::{eval_report, note_routine_gaps_against};
use serve::thor_bugs::{load, routine_gaps, Signal};
use thor_core::event_store::EventStore;

const SHIPPED_ROUTINE: &str = include_str!("../../eval-command.example.md");

fn store_with_an_open_candidate() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    EventStore::new(&db).unwrap();
    serve::thor_bugs::write_mode(&db, serve::bug_report::Mode::Test).unwrap();
    serve::thor_bugs::add(&db, serve::thor_bugs::hook_launch_finding("Stop", 126));
    (dir, db)
}

#[test]
fn every_line_doctor_eval_prints_is_named_in_the_routine_thor_ships() {
    let (dir, db) = store_with_an_open_candidate();
    let lines = eval_report(&db, Some(dir.path()), false, None);
    assert!(lines.iter().any(|l| l.starts_with("THOR bug candidates:")), "the open candidate is a doctor line: {lines:?}");
    let gaps = routine_gaps(SHIPPED_ROUTINE, &lines);
    assert!(gaps.is_empty(), "doctor --eval prints lines the routine never names: {gaps:?}");
}

#[test]
fn a_routine_that_lacks_a_label_makes_a_bug_candidate_once() {
    let (dir, db) = store_with_an_open_candidate();
    let lines = eval_report(&db, Some(dir.path()), false, None);
    let stale_routine = SHIPPED_ROUTINE.replace("THOR bug candidates", "something else");
    assert_eq!(note_routine_gaps_against(&db, &stale_routine, &lines), vec!["thor bug candidates".to_string()]);
    note_routine_gaps_against(&db, &stale_routine, &lines);
    let book = load(&db);
    let routine_bugs: Vec<_> = book.candidates.iter().filter(|c| c.finding.signal == Signal::DoctorRoutine).collect();
    assert_eq!(routine_bugs.len(), 1, "the same gap is one bug");
    assert_eq!(routine_bugs[0].seen, 2);
}
