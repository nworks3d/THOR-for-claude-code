//! The two folders an owner may name (a temp-until-live folder and a tools
//! folder): `install --set-temp-folder` and `--set-tools-folder` write them
//! beside the memory in plain words, refuse what cannot work before changing
//! anything, and `doctor` then says what it finds in them. All against a
//! throwaway home folder.

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

fn settings_file(home: &Path) -> PathBuf {
    db_of(home).parent().unwrap().join("work-folders.json")
}

/// A home folder with a memory in it (what a normal install leaves).
fn installed() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let out = install(home.path(), &["--no-mcp"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    home
}

fn folder(home: &Path, name: &str) -> PathBuf {
    let dir = home.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

#[test]
fn naming_the_folders_writes_them_beside_the_memory_and_says_so_in_plain_words() {
    let home = installed();
    let db = db_of(home.path()).display().to_string();
    let temp = folder(home.path(), "temp-until-live");
    let tools = folder(home.path(), "tools");
    assert!(!settings_file(home.path()).exists(), "nothing is configured until the owner names a folder");

    let out = install(home.path(), &["--set-temp-folder", &temp.display().to_string(), "--set-tools-folder", &tools.display().to_string(), "--db", &db]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("temp-until-live folder set to"), "{stdout}");
    assert!(stdout.contains("STATUS: live") && stdout.contains("never deletes"), "{stdout}");
    assert!(stdout.contains("tools folder set to") && stdout.contains("README.txt whose first line says in one sentence"), "{stdout}");
    assert!(stdout.contains("install --set-temp-folder off") && stdout.contains("install --set-tools-folder off"), "how to undo it is spelled out: {stdout}");

    let written = std::fs::read_to_string(settings_file(home.path())).unwrap();
    assert!(!written.contains('\r'), "LF line endings only");
    let json: serde_json::Value = serde_json::from_str(&written).unwrap();
    assert_eq!(json["temp_until_live"], temp.display().to_string());
    assert_eq!(json["tools"], tools.display().to_string());
    assert_eq!(json["temp_max_age_days"], 30, "the age limit is written where the owner can see it");

    // Off removes only what it names, and doing it twice is harmless.
    let off = install(home.path(), &["--set-temp-folder", "off", "--db", &db]);
    assert!(off.status.success(), "{}", text(&off.stderr));
    assert!(text(&off.stdout).contains("temp-until-live folder switched off"), "{}", text(&off.stdout));
    let again = install(home.path(), &["--set-temp-folder", "OFF", "--db", &db]);
    assert!(again.status.success());
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(settings_file(home.path())).unwrap()).unwrap();
    assert!(json.get("temp_until_live").is_none());
    assert_eq!(json["tools"], tools.display().to_string(), "the tools folder stays");
    let off = install(home.path(), &["--set-tools-folder", "off", "--db", &db]);
    assert!(text(&off.stdout).contains("tools folder switched off"));
}

#[test]
fn a_hand_made_age_limit_survives_naming_the_folder_again() {
    let home = installed();
    let db = db_of(home.path()).display().to_string();
    let temp = folder(home.path(), "temp");
    std::fs::write(settings_file(home.path()), r#"{"temp_max_age_days": 10}"#).unwrap();
    let out = install(home.path(), &["--set-temp-folder", &temp.display().to_string(), "--db", &db]);
    assert!(out.status.success());
    assert!(text(&out.stdout).contains("older than 10 days"), "{}", text(&out.stdout));
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(settings_file(home.path())).unwrap()).unwrap();
    assert_eq!(json["temp_max_age_days"], 10);
}

#[test]
fn a_folder_that_cannot_work_is_refused_and_nothing_is_changed() {
    let home = installed();
    let db = db_of(home.path()).display().to_string();
    let good = folder(home.path(), "good");

    let relative = install(home.path(), &["--set-tools-folder", "tools", "--db", &db]);
    assert_eq!(relative.status.code(), Some(1));
    let why = text(&relative.stderr);
    assert!(why.contains("REFUSED") && why.contains("not a full path") && why.contains("Nothing was changed"), "{why}");

    let missing = install(home.path(), &["--set-tools-folder", &home.path().join("nowhere").display().to_string(), "--db", &db]);
    assert_eq!(missing.status.code(), Some(1));
    let why = text(&missing.stderr);
    assert!(why.contains("there is no folder at") && why.contains("network folder"), "{why}");

    // One good value and one bad one: the good one is not written either.
    let mixed = install(home.path(), &["--set-temp-folder", &good.display().to_string(), "--set-tools-folder", "relative/path", "--db", &db]);
    assert_eq!(mixed.status.code(), Some(1));
    assert!(!settings_file(home.path()).exists(), "a refusal writes nothing at all");
}

#[test]
fn a_settings_file_somebody_edited_into_nonsense_is_not_overwritten() {
    let home = installed();
    let db = db_of(home.path()).display().to_string();
    let tools = folder(home.path(), "tools");
    std::fs::write(settings_file(home.path()), "{ \"tools\": \"C:\\\\mine\", oops").unwrap();
    let out = install(home.path(), &["--set-tools-folder", &tools.display().to_string(), "--db", &db]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("could not write"), "{}", text(&out.stderr));
    assert_eq!(std::fs::read_to_string(settings_file(home.path())).unwrap(), "{ \"tools\": \"C:\\\\mine\", oops", "what a person typed is kept");
}

#[test]
fn the_memory_must_be_named_and_must_exist() {
    let home = installed();
    let tools = folder(home.path(), "tools");
    let no_db = install(home.path(), &["--set-tools-folder", &tools.display().to_string()]);
    assert_eq!(no_db.status.code(), Some(2));
    assert!(text(&no_db.stderr).contains("--db"), "{}", text(&no_db.stderr));
    let wrong = install(home.path(), &["--set-tools-folder", &tools.display().to_string(), "--db", &home.path().join("none.db").display().to_string()]);
    assert_eq!(wrong.status.code(), Some(1));
    assert!(text(&wrong.stderr).contains("there is no memory at"), "{}", text(&wrong.stderr));
}

#[test]
fn the_flags_do_not_mix_with_another_set_option_and_the_help_names_them() {
    let home = installed();
    let db = db_of(home.path()).display().to_string();
    let tools = folder(home.path(), "tools");
    let mixed = install(home.path(), &["--set-tools-folder", &tools.display().to_string(), "--set-plan", "max", "--db", &db]);
    assert_eq!(mixed.status.code(), Some(2), "{}", text(&mixed.stderr));
    assert!(!settings_file(home.path()).exists());
    let help = Command::new(env!("CARGO_BIN_EXE_install")).arg("--help").output().unwrap();
    let help = text(&help.stdout);
    assert!(help.contains("--set-temp-folder") && help.contains("--set-tools-folder"), "{help}");
}

// ------------------------------------------------------------------ doctor

/// Run doctor with a home folder of its own, so it reads this fixture's files
/// and never the real ones.
fn doctor(home: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_doctor"))
        .arg("--db")
        .arg(db_of(home))
        .env("USERPROFILE", home)
        .env("HOME", home)
        .env("LOCALAPPDATA", home.join("localappdata"))
        .env("THOR_SCHTASKS_XML", home.join("tasks.xml"))
        .output()
        .unwrap();
    text(&out.stdout)
}

#[test]
fn doctor_says_nothing_about_the_folders_until_they_are_named() {
    let home = installed();
    let report = doctor(home.path());
    assert!(!report.contains("temp-until-live") && !report.contains("work folders:") && !report.contains("tools folder"), "{report}");
}

#[test]
fn doctor_names_what_needs_a_look_in_the_short_version_and_the_detailed_report() {
    let home = installed();
    let db = db_of(home.path()).display().to_string();
    let temp = folder(home.path(), "temp-until-live");
    let tools = folder(home.path(), "tools");
    let done = temp.join("2026-10-01_shop_done");
    std::fs::create_dir_all(&done).unwrap();
    std::fs::write(done.join("STATUS.txt"), "STATUS: live\n").unwrap();
    std::fs::create_dir_all(tools.join("pdf-merge")).unwrap();
    std::fs::write(tools.join("pdf-merge").join("README.txt"), "Merge PDF files into one.\n").unwrap();
    std::fs::create_dir_all(tools.join("no-readme")).unwrap();
    let out = install(home.path(), &["--set-temp-folder", &temp.display().to_string(), "--set-tools-folder", &tools.display().to_string(), "--db", &db]);
    assert!(out.status.success(), "{}", text(&out.stderr));

    let report = doctor(home.path());
    let notes: Vec<&str> = report.lines().filter(|l| l.starts_with("  NOTE: in your ")).collect();
    assert_eq!(notes.len(), 2, "{report}");
    assert!(notes[0].contains("2026-10-01_shop_done") && notes[0].contains("THOR deletes nothing"), "{notes:?}");
    assert!(notes[1].contains("no-readme") && notes[1].contains("Next step"), "{notes:?}");
    let short_end = report.lines().position(|l| l.starts_with("Everything below is the detailed report")).expect("the short version ends");
    let note_at = report.lines().position(|l| l.starts_with("  NOTE: in your ")).unwrap();
    assert!(note_at < short_end, "the notes belong to the short version: {report}");
    let detail: Vec<&str> = report.lines().filter(|l| l.starts_with("work folders:")).collect();
    assert_eq!(detail.len(), 2, "{report}");
    assert!(detail[0].contains("1 production folder(s)") && detail[1].contains("2 tool folder(s)"), "{detail:?}");
}

#[test]
fn doctor_says_when_a_named_folder_is_gone() {
    let home = installed();
    let db = db_of(home.path()).display().to_string();
    let tools = folder(home.path(), "tools");
    install(home.path(), &["--set-tools-folder", &tools.display().to_string(), "--db", &db]);
    std::fs::remove_dir_all(&tools).unwrap();
    let report = doctor(home.path());
    assert!(report.lines().any(|l| l.starts_with("  NOTE: your tools folder (") && l.contains("could not be read")), "{report}");
}
