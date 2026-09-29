//! The CLI cannot set a rule's `teeth:` label (K16, R8.22).
//!
//! MEASURED before step 8 of the two tracks: `model --db <store> declare ...
//! --tags teeth:proven-guard` printed "stored f-rule (seq 1)" - a proven label
//! nothing had ever tried, on the one door that never ran the trial - and an
//! evaluation report filed right after it. The label is only a readable copy
//! of THOR's own trial record now, and the store refuses a new one no record
//! implies, for every writer. Spawns the real compiled `model` binary.

use std::path::Path;
use std::process::Command;
use thor_core::event_store::EventStore;

fn declare(db: &Path, id: &str, tag: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_model"))
        .arg("--db")
        .arg(db)
        .args(["declare", "--id", id, "--kind", "rule", "--text", "Never force push with --force", "--severity", "irreversible"])
        .args(["--project", "demo", "--target", "command:git push", "--falsifier", "Force push is fine."])
        .args(["--check-kind", "forbidden", "--check-literals=--force", "--tags", tag])
        .output()
        .expect("run model")
}

fn events(db: &Path) -> usize {
    EventStore::open_existing(db).unwrap().get_all_events().unwrap().len()
}

#[test]
fn a_teeth_label_given_on_the_command_line_is_refused_and_nothing_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("thor.db");
    drop(EventStore::new(&db).unwrap());
    for tag in ["teeth:hard", "teeth:proven-guard"] {
        let out = declare(&db, "f-rule", tag);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{tag}: {stderr}");
        assert!(stderr.contains(&format!("'{tag}' starts with 'teeth:', which only THOR's own trial writes")), "{tag}: {stderr}");
        assert_eq!(events(&db), 0, "{tag}: nothing written");
    }
    // The control: the same call with an ordinary tag stores.
    let out = declare(&db, "f-rule", "gotcha");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(events(&db), 1);
}
