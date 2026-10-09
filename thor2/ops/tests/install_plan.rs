//! Proof that install settles the user's plan from Claude Code's own file,
//! records it with its source in the mode sidecar, and lets the user correct
//! it with `--set-plan`. All against a throwaway home folder.

use serve::plan::{read, Plan, PlanSource};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn install(home: &Path, args: &[&str]) -> Output {
    let repo = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_install"))
        .current_dir(repo.path())
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("LOCALAPPDATA", home.join("localappdata"))
        .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

fn db_of(home: &Path) -> PathBuf {
    home.join("localappdata").join("thor2").join("thor.db")
}

fn claude_json(home: &Path, body: &str) {
    std::fs::write(home.join(".claude.json"), body).unwrap();
}

#[test]
fn a_max_tier_in_claude_codes_file_is_recorded_as_detected_max() {
    let home = tempfile::tempdir().unwrap();
    claude_json(home.path(), r#"{"oauthAccount":{"organizationRateLimitTier":"default_claude_max_20x"}}"#);
    let out = install(home.path(), &["--no-mcp"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("plan: Claude Code says your plan is Max"), "{stdout}");
    assert_eq!(read(&db_of(home.path())), Some((Plan::Max, PlanSource::Detected)));
}

#[test]
fn another_tier_or_no_file_at_all_means_a_small_plan_and_says_so() {
    let home = tempfile::tempdir().unwrap();
    claude_json(home.path(), r#"{"oauthAccount":{"organizationRateLimitTier":"default_claude_pro"}}"#);
    let out = install(home.path(), &["--no-mcp"]);
    assert!(out.status.success());
    assert_eq!(read(&db_of(home.path())), Some((Plan::Small, PlanSource::Detected)));

    let bare = tempfile::tempdir().unwrap();
    let out = install(bare.path(), &["--no-mcp"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("do not say which plan you have"), "{stdout}");
    assert_eq!(read(&db_of(bare.path())), Some((Plan::Small, PlanSource::Detected)));
}

#[test]
fn set_plan_records_the_users_answer_and_a_second_install_keeps_it() {
    let home = tempfile::tempdir().unwrap();
    claude_json(home.path(), r#"{"oauthAccount":{"organizationRateLimitTier":"default_claude_max_20x"}}"#);
    install(home.path(), &["--no-mcp"]);
    let db = db_of(home.path());
    let out = install(home.path(), &["--set-plan", "small", "--db", &db.display().to_string()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(read(&db), Some((Plan::Small, PlanSource::Confirmed)), "the file had a tier, so this is a confirmation");
    install(home.path(), &["--no-mcp"]);
    assert_eq!(read(&db), Some((Plan::Small, PlanSource::Confirmed)), "install never overwrites the user's answer");
}

#[test]
fn set_plan_without_a_tier_in_the_file_is_recorded_as_asked_and_a_bad_word_is_refused() {
    let home = tempfile::tempdir().unwrap();
    install(home.path(), &["--no-mcp"]);
    let db = db_of(home.path());
    let out = install(home.path(), &["--set-plan", "max", "--db", &db.display().to_string()]);
    assert!(out.status.success());
    assert_eq!(read(&db), Some((Plan::Max, PlanSource::Asked)));
    let bad = install(home.path(), &["--set-plan", "gold", "--db", &db.display().to_string()]);
    assert_eq!(bad.status.code(), Some(2));
    assert_eq!(read(&db), Some((Plan::Max, PlanSource::Asked)), "a bad word changes nothing");
}

#[test]
fn set_plan_keeps_the_bug_mode() {
    let home = tempfile::tempdir().unwrap();
    install(home.path(), &["--no-mcp"]);
    let db = db_of(home.path());
    install(home.path(), &["--set-mode", "test", "--db", &db.display().to_string()]);
    install(home.path(), &["--set-plan", "max", "--db", &db.display().to_string()]);
    assert_eq!(serve::thor_bugs::read_mode(&db), Some(serve::bug_report::Mode::Test));
}
