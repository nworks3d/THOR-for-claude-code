//! Proof of the mode the bug nudge runs in (`serve::thor_bugs`): `release` by
//! default on EVERY build (a source build included), `test` only through an
//! explicit `install --set-mode test`, never flipped by a second install.
//!
//! The CLI runs with HOME, USERPROFILE and LOCALAPPDATA pointed at a
//! throwaway folder, like the other install tests.

use ops::install::{settle_mode, ModeHow};
use serve::bug_report::Mode;
use serve::thor_bugs::read_mode;
use std::path::Path;
use std::process::{Command, Output};

#[test]
fn the_default_is_release_on_every_build_and_a_release_marker_changes_nothing() {
    let store = tempfile::tempdir().unwrap();
    let db = store.path().join("thor.db");
    assert_eq!(settle_mode(&db, None).unwrap(), (Mode::Release, ModeHow::Default));
    assert_eq!(read_mode(&db), Some(Mode::Release));
}

#[test]
fn a_second_install_keeps_the_mode_and_only_the_owner_changes_it() {
    let store = tempfile::tempdir().unwrap();
    let db = store.path().join("thor.db");
    settle_mode(&db, Some(Mode::Test)).unwrap();
    assert_eq!(settle_mode(&db, None).unwrap(), (Mode::Test, ModeHow::Kept));
    assert_eq!(settle_mode(&db, Some(Mode::Release)).unwrap(), (Mode::Release, ModeHow::Told));
    assert_eq!(read_mode(&db), Some(Mode::Release));
}

fn set_mode(args: &[&str]) -> Output {
    let home = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_install"))
        .args(args)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("LOCALAPPDATA", home.path().join("localappdata"))
        .output()
        .unwrap()
}

fn fake_store(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    std::fs::write(&db, b"").unwrap();
    db
}

#[test]
fn set_mode_writes_the_mode_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let db = fake_store(dir.path());
    let out = set_mode(&["--set-mode", "test", "--db", &db.display().to_string()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("bug nudge mode set to 'test'"));
    assert_eq!(read_mode(&db), Some(Mode::Test));
    let mut names: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, vec!["thor-mode.json".to_string(), "thor.db".to_string()], "no hook, no setting, no other file");
}

#[test]
fn set_mode_refuses_a_bad_word_a_missing_db_and_a_missing_memory() {
    let dir = tempfile::tempdir().unwrap();
    let db = fake_store(dir.path());
    let bad = set_mode(&["--set-mode", "production", "--db", &db.display().to_string()]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("'release', 'test' or 'off'"));
    let no_db = set_mode(&["--set-mode", "test"]);
    assert_eq!(no_db.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&no_db.stderr).contains("--db"));
    let gone = dir.path().join("nothing.db");
    let missing = set_mode(&["--set-mode", "test", "--db", &gone.display().to_string()]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("REFUSED"));
    assert_eq!(read_mode(&db), None);
}

#[test]
fn a_full_install_from_a_build_in_a_checkout_writes_release_mode() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_install"))
        .current_dir(repo.path())
        .arg("--no-mcp")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("LOCALAPPDATA", home.path().join("localappdata"))
        .env("GIT_CONFIG_GLOBAL", home.path().join(".gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("bug nudge mode 'release'") && stdout.contains("nothing is sent without your yes"), "{stdout}");
    let db = home.path().join("localappdata").join("thor2").join("thor.db");
    assert_eq!(read_mode(&db), Some(Mode::Release));
}
