//! `doctor` names a THOR program that a hook, the tool server or a scheduled
//! task calls and that is not on disk (2026-10-11: Windows Defender
//! quarantined sync.exe and nothing said so for two days), and the ship line
//! tells "never recorded" apart from "ran and failed".

use std::path::{Path, PathBuf};
use std::process::Command;
use thor_core::event_store::EventStore;

/// Run doctor with a home folder of its own, so it reads this fixture's
/// settings and tool-server files and never the real ones.
fn run_doctor(db: &Path, home: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_doctor"))
        .arg("--db")
        .arg(db)
        .env("USERPROFILE", home)
        .env("HOME", home)
        // This fixture's own scheduled tasks; the machine's are never read.
        .env("THOR_SCHTASKS_XML", home.join("tasks.xml"))
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

fn settings_with_hook(home: &Path, program: &Path, db: &Path) {
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let command = format!("\"{}\" --db \"{}\" hook", program.display(), db.display());
    let json = serde_json::json!({"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": command}]}]}});
    std::fs::write(home.join(".claude").join("settings.json"), json.to_string()).unwrap();
}

#[test]
fn a_hook_that_calls_a_missing_program_is_named_with_the_cause_and_the_steps() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    EventStore::new(&db).unwrap();
    let gone: PathBuf = dir.path().join("thor2").join("serve.exe");
    settings_with_hook(dir.path(), &gone, &db);

    let stdout = run_doctor(&db, dir.path());
    let line = stdout.lines().find(|l| l.starts_with("PROBLEM: THOR program missing")).unwrap_or_else(|| panic!("no missing-program line:\n{stdout}"));
    assert!(line.contains("serve.exe"), "{line}");
    assert!(line.contains("install again"), "{line}");
    if cfg!(windows) {
        assert!(line.contains("quarantined") && line.contains("Protection history"), "{line}");
        assert!(line.contains("OFFER, never do it unasked") && line.contains("saying no is fine"), "{line}");
    }
}

#[test]
fn a_hook_whose_program_is_there_says_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    EventStore::new(&db).unwrap();
    settings_with_hook(dir.path(), Path::new(env!("CARGO_BIN_EXE_doctor")), &db);
    let stdout = run_doctor(&db, dir.path());
    assert!(!stdout.contains("THOR program missing"), "{stdout}");
}

#[test]
fn a_scheduled_task_whose_script_calls_a_missing_program_is_named_and_the_never_recorded_ship_is_said() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    EventStore::new(&db).unwrap();
    let thor2 = dir.path().join("thor2");
    std::fs::create_dir_all(&thor2).unwrap();
    let script = thor2.join("ship-hourly.ps1");
    let sync = thor2.join("sync.exe");
    std::fs::write(&script, format!("$bin = \"{}\"\n& $bin ship\n", sync.display())).unwrap();
    let xml = format!(
        "<Task><Actions><Exec><Command>powershell.exe</Command><Arguments>-NoProfile -File \"{}\"</Arguments></Exec></Actions></Task>",
        script.display()
    );
    std::fs::write(dir.path().join("tasks.xml"), xml).unwrap();

    let stdout = run_doctor(&db, dir.path());
    let missing = stdout.lines().find(|l| l.starts_with("PROBLEM: THOR program missing")).unwrap_or_else(|| panic!("{stdout}"));
    assert!(missing.contains("sync.exe"), "{missing}");
    let ship = stdout.lines().find(|l| l.starts_with("ship:")).unwrap_or_else(|| panic!("{stdout}"));
    assert!(ship.contains("ever been recorded"), "{ship}");
}

#[test]
fn a_task_that_calls_the_ship_but_left_no_record_is_the_third_case() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    EventStore::new(&db).unwrap();
    let called = vec![PathBuf::from("C:\\Users\\a\\thor2\\ship-hourly.ps1")];
    let line = ops::health::ship_never_recorded_line(&db, &called).expect("a task with no record must be said");
    assert!(line.starts_with("ship:") && line.contains("ever been recorded") && line.contains("Last Run Result"), "{line}");
    // A recorded failure is the second case, not this one.
    ops::ship_state::record_failure_at(&db, "program not found: sync.exe", 1_000_000).unwrap();
    assert_eq!(ops::health::ship_never_recorded_line(&db, &called), None);
    // No ship task, nothing to say.
    let other = tempfile::tempdir().unwrap();
    assert_eq!(ops::health::ship_never_recorded_line(&other.path().join("t.db"), &[PathBuf::from("C:\\x\\notes.ps1")]), None);
}
