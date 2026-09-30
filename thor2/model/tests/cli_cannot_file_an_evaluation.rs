//! The model CLI cannot file an evaluation report (step 15, R15.2).
//!
//! MEASURED before step 15: `model.exe --db <store> declare --id
//! eval-fxp-2026-09-28 --kind report --project fxp --tags evaluation-report
//! --text 'TLDR' --session-id S1` printed "stored eval-fxp-2026-09-28 (seq 68)"
//! with exit 0. A report written from a shell could never count - the Stop
//! hook only accepts one whose filing call is in the evaluating session's own
//! transcript - so the CLI refuses it, with the sentence the tool server
//! gives, and writes nothing. Spawns the real compiled `model` binary.

use std::path::Path;
use std::process::Command;
use thor_core::event_store::EventStore;

fn declare(db: &Path, id: &str, tags: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_model"))
        .arg("--db")
        .arg(db)
        .args(["declare", "--id", id, "--kind", "report", "--project", "fxp", "--tags", tags, "--text", "TLDR"])
        .args(["--session-id", "0de7dc52-1b97-4b66-948f-61e4438043b2"])
        .output()
        .expect("run model")
}

#[test]
fn declare_of_an_evaluation_report_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("thor.db");
    drop(EventStore::new(&db).unwrap());
    let out = declare(&db, "eval-fxp-2026-09-28", "evaluation-report");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.starts_with(
            "REFUSED: this evaluation report is not coming through a THOR tool call that both Claude Code and THOR's hook \
             saw (the model command line is not a THOR tool call), so it could never count"
        ),
        "{stderr}"
    );
    assert!(stderr.contains("Nothing was written. File it with the THOR tool itself"), "{stderr}");
    assert!(String::from_utf8_lossy(&out.stdout).is_empty(), "nothing stored: {}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(EventStore::open_existing(&db).unwrap().get_all_events().unwrap().len(), 0, "nothing written");
    // The control: an ordinary report still stores from the command line.
    let out = declare(&db, "notes-fxp", "notes");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(EventStore::open_existing(&db).unwrap().get_all_events().unwrap().len(), 1);
}
