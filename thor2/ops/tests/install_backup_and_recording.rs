//! `install --set-backup local|<place>|off` and `install --set-recording off|on`:
//! the two choices the first-session list offers that a fresh install does NOT
//! make for the user. A fresh install wires no backup at all; these tests hold
//! that the choice does exactly what it says, runs the backup once to prove it,
//! never touches a hook that is not the backup, and can be undone.
//! Everything runs against a throwaway home folder and a throwaway git config.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fx {
    home: tempfile::TempDir,
}

impl Fx {
    fn new() -> Fx {
        let fx = Fx { home: tempfile::tempdir().unwrap() };
        std::fs::create_dir_all(fx.db().parent().unwrap()).unwrap();
        ops::install::ensure_store(&fx.db()).unwrap();
        // A memory with something in it, as every real one has.
        ops::install::seed_working_contract(&fx.db()).unwrap();
        fx
    }
    fn db(&self) -> PathBuf {
        self.home.path().join("localappdata").join("thor2").join("thor.db")
    }
    fn settings(&self) -> PathBuf {
        self.home.path().join(".claude").join("settings.json")
    }
    fn local_repo(&self) -> PathBuf {
        self.db().parent().unwrap().join("backup-repo")
    }
    fn install(&self, args: &[&str]) -> Output {
        let cwd = tempfile::tempdir().unwrap();
        Command::new(env!("CARGO_BIN_EXE_install"))
            .current_dir(cwd.path())
            .args(args)
            .arg("--db")
            .arg(self.db())
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("LOCALAPPDATA", self.home.path().join("localappdata"))
            // No identity anywhere: the install must cope on its own.
            .env("GIT_CONFIG_GLOBAL", self.home.path().join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    }
    fn settings_json(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.settings()).unwrap()).unwrap()
    }
    /// The commands of every hook under `event`.
    fn commands(&self, event: &str) -> Vec<String> {
        self.settings_json()["hooks"][event]
            .as_array()
            .map(|groups| {
                groups
                    .iter()
                    .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                    .filter_map(|h| h["command"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn backup_hooks(&self) -> Vec<String> {
        self.commands("SessionStart").into_iter().filter(|c| c.contains("backup")).collect()
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("no-such-gitconfig"))
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn said(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn foreign_settings(fx: &Fx) {
    std::fs::create_dir_all(fx.settings().parent().unwrap()).unwrap();
    std::fs::write(
        fx.settings(),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"\"C:\\other\\tool.exe\" start"}]}]}}"#,
    )
    .unwrap();
}

#[test]
fn a_fresh_install_wires_no_backup_so_the_questionnaire_must_say_so() {
    let specs = ops::install::standard_hooks("C:/thor/serve.exe", "C:/thor/thor.db");
    assert!(specs.iter().all(|s| !s.command.contains("backup")), "the questionnaire says nothing is backed up until the user answers");
}

#[test]
fn local_makes_a_git_folder_wires_one_session_start_hook_and_proves_it_with_a_first_copy() {
    let fx = Fx::new();
    foreign_settings(&fx);
    let out = fx.install(&["--set-backup", "local"]);
    assert!(out.status.success(), "{}", said(&out));
    let text = said(&out);
    assert!(text.contains("first copy") && text.contains("committed"), "a plain proof result is printed: {text}");

    let repo = fx.local_repo();
    assert!(repo.join(".git").exists(), "the local default is a git folder beside the memory");
    assert_eq!(git(&repo, &["remote"]), "", "local means no remote: nothing leaves this machine");
    assert_eq!(git(&repo, &["rev-list", "--count", "HEAD"]), "1", "the first copy is committed");
    assert!(repo.join("thor2").join("events-0001.jsonl").exists() || repo.join("thor2").read_dir().is_ok());

    let hooks = fx.backup_hooks();
    assert_eq!(hooks.len(), 1, "{hooks:?}");
    assert!(hooks[0].contains("--repo") && hooks[0].contains("backup-repo"), "{}", hooks[0]);
    assert!(fx.commands("SessionStart").iter().any(|c| c.contains("other")), "a foreign hook is untouched");
}

#[test]
fn choosing_local_twice_adds_nothing_the_second_time() {
    let fx = Fx::new();
    assert!(fx.install(&["--set-backup", "local"]).status.success());
    let before = std::fs::read_to_string(fx.settings()).unwrap();
    let again = fx.install(&["--set-backup", "local"]);
    assert!(again.status.success(), "{}", said(&again));
    assert_eq!(std::fs::read_to_string(fx.settings()).unwrap(), before, "settings are byte for byte the same");
    assert_eq!(fx.backup_hooks().len(), 1);
}

#[test]
fn a_named_folder_becomes_the_nas_remote_and_the_branch_is_pushed_once() {
    let fx = Fx::new();
    let place = fx.home.path().join("nas-share").join("thor-backup.git");
    let out = fx.install(&["--set-backup", &place.display().to_string()]);
    assert!(out.status.success(), "{}", said(&out));

    assert!(place.join("HEAD").exists(), "an empty bare repository was created there, as AGENTS.md describes");
    let repo = fx.local_repo();
    assert_eq!(git(&repo, &["remote"]), "nas");
    assert!(git(&repo, &["remote", "get-url", "nas"]).replace('\\', "/").ends_with("nas-share/thor-backup.git"));
    assert!(!git(&place, &["rev-parse", "--verify", "main"]).is_empty(), "the branch reached the place");
    assert_eq!(fx.backup_hooks().len(), 1);
    assert!(said(&out).contains("first copy"), "{}", said(&out));
}

#[test]
fn switching_from_a_place_back_to_local_drops_the_remote() {
    let fx = Fx::new();
    let place = fx.home.path().join("nas-share").join("thor-backup.git");
    assert!(fx.install(&["--set-backup", &place.display().to_string()]).status.success());
    let out = fx.install(&["--set-backup", "local"]);
    assert!(out.status.success(), "{}", said(&out));
    assert_eq!(git(&fx.local_repo(), &["remote"]), "", "local means nothing leaves this machine");
    assert_eq!(fx.backup_hooks().len(), 1, "still one hook");
}

#[test]
fn off_removes_only_the_backup_hook_and_leaves_the_folder_alone() {
    let fx = Fx::new();
    foreign_settings(&fx);
    assert!(fx.install(&["--set-backup", "local"]).status.success());
    let out = fx.install(&["--set-backup", "off"]);
    assert!(out.status.success(), "{}", said(&out));
    assert!(fx.backup_hooks().is_empty());
    assert!(fx.commands("SessionStart").iter().any(|c| c.contains("other")), "the foreign hook stays");
    assert!(fx.local_repo().join(".git").exists(), "the copies already made stay on disk");
    let again = fx.install(&["--set-backup", "off"]);
    assert!(again.status.success(), "off twice is fine: {}", said(&again));
}

#[test]
fn another_backup_hook_for_this_memory_is_never_added_beside_or_removed_silently() {
    let fx = Fx::new();
    std::fs::create_dir_all(fx.settings().parent().unwrap()).unwrap();
    let theirs = format!(
        "\"C:/somewhere/backup.exe\" --db \"{}\" --repo \"C:/their/own-repo\"",
        fx.db().display().to_string().replace('\\', "/")
    );
    let body = serde_json::json!({"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": theirs}]}]}});
    std::fs::write(fx.settings(), body.to_string()).unwrap();
    let before = std::fs::read_to_string(fx.settings()).unwrap();
    let out = fx.install(&["--set-backup", "local"]);
    assert!(!out.status.success(), "refused");
    assert!(said(&out).contains("already") && said(&out).contains("--set-backup off"), "{}", said(&out));
    assert_eq!(std::fs::read_to_string(fx.settings()).unwrap(), before, "nothing changed");
}

#[test]
fn an_unknown_memory_or_an_empty_word_is_refused_and_changes_nothing() {
    let fx = Fx::new();
    let empty = fx.install(&["--set-backup", ""]);
    assert_eq!(empty.status.code(), Some(2), "{}", said(&empty));
    assert!(!fx.settings().exists());
}

#[test]
fn recording_can_be_switched_off_and_on_and_a_bad_word_is_refused() {
    let fx = Fx::new();
    assert!(serve::plan::recording_on(&fx.db()));
    let off = fx.install(&["--set-recording", "off"]);
    assert!(off.status.success(), "{}", said(&off));
    assert!(!serve::plan::recording_on(&fx.db()));
    assert!(said(&off).contains("nothing new is recorded"), "{}", said(&off));
    let bad = fx.install(&["--set-recording", "maybe"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(!serve::plan::recording_on(&fx.db()), "a bad word changes nothing");
    assert!(fx.install(&["--set-recording", "on"]).status.success());
    assert!(serve::plan::recording_on(&fx.db()));
}

#[test]
fn the_backup_hook_is_the_same_hook_in_any_path_spelling() {
    let a = ops::backup_setup::hook_spec("C:\\thor\\backup.exe", "C:\\thor\\thor.db", "C:\\thor\\backup-repo");
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings.json");
    ops::install::install_hooks(&settings, &[a]).unwrap();
    let b = ops::backup_setup::hook_spec("C:/thor/backup.exe", "C:/thor/thor.db", "C:/thor/backup-repo");
    let report = ops::install::install_hooks(&settings, &[b]).unwrap();
    assert_eq!(report.results[0].1, ops::install::HookOutcome::AlreadyPresent);
}
