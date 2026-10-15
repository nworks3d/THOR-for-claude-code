//! The example hourly ship script records a failed attempt, a missing program
//! included, so `doctor` can tell "never ran" from "ran and failed" from
//! "program missing" (2026-10-11: a quarantined sync.exe made the script die
//! silently and doctor blamed a sleeping machine for two days).

#![cfg(windows)]

use std::os::windows::process::CommandExt;
use std::path::PathBuf;

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("deploy").join("ship-hourly.example.ps1")
}

fn run(sync: &std::path::Path, db: &std::path::Path) -> std::process::ExitStatus {
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script())
        .arg("-SyncExe")
        .arg(sync)
        .arg("-Store")
        .arg(db)
        .args(["-To", "http://127.0.0.1:9"])
        .creation_flags(0x0800_0000)
        .status()
        .unwrap()
}

#[test]
fn a_missing_program_is_recorded_as_a_failed_attempt_and_doctor_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sync = dir.path().join("gone").join("sync.exe");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    ops::ship_state::record_success_at(&db, 5, now - 3 * 3600).unwrap();

    let status = run(&sync, &db);
    assert!(!status.success(), "the task must still fail");

    let state = ops::ship_state::read(&db).expect("the failure must be recorded");
    let error = state.last_attempt_error.expect("the error must be recorded");
    assert!(error.contains("program not found") && error.contains("quarantined"), "{error}");
    assert_eq!(state.receiver_seq, 5, "the last success stays on file");
    assert_eq!(state.completed_unix, now - 3 * 3600);

    let line = ops::health::ship_line(&db).unwrap();
    assert!(line.contains("FAILED") && line.contains("program not found") && line.contains("last succeeded 3h ago"), "{line}");
}

#[test]
fn a_first_ever_failure_is_recorded_with_no_success_on_file() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    run(&dir.path().join("sync.exe"), &db);
    let state = ops::ship_state::read(&db).expect("the failure must be recorded");
    assert_eq!(state.completed_unix, 0);
    assert!(state.last_attempt_error.unwrap().contains("program not found"));
    assert!(ops::health::ship_line(&db).unwrap().contains("no successful ship yet"));
}
