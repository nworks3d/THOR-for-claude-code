//! The switches a person's no goes through: `install --set-connections`,
//! `--set-git-hooks` and `--set-reply-rules`, and the rule that one computer
//! account has one THOR (`--move-to-this-store`).
//!
//! Every run has HOME, USERPROFILE, LOCALAPPDATA and the global git config
//! pointed at a throwaway folder, so no real file of the machine is touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    home: tempfile::TempDir,
    /// A folder of programs the installer is pointed at instead of the build folder.
    bin: Option<tempfile::TempDir>,
}

impl Sandbox {
    fn new() -> Self {
        Self { home: tempfile::tempdir().unwrap(), bin: None }
    }
    /// A program folder that holds the real `serve` and a stand-in for the code reader, so the
    /// git helpers can be wired whether or not the code reader was built before this test ran.
    fn with_programs(mut self) -> Self {
        let install = PathBuf::from(env!("CARGO_BIN_EXE_install"));
        let ext = if cfg!(windows) { ".exe" } else { "" };
        let bin = tempfile::tempdir().unwrap();
        std::fs::copy(install.with_file_name(format!("serve{ext}")), bin.path().join(format!("serve{ext}"))).unwrap();
        std::fs::copy(&install, bin.path().join(format!("codeindex{ext}"))).unwrap();
        self.bin = Some(bin);
        self
    }
    fn db_named(&self, name: &str) -> PathBuf {
        self.home.path().join("localappdata").join(name).join("thor.db")
    }
    fn db(&self) -> PathBuf {
        self.db_named("thor2")
    }
    fn settings(&self) -> PathBuf {
        self.home.path().join(".claude").join("settings.json")
    }
    fn gitconfig(&self) -> PathBuf {
        self.home.path().join(".gitconfig")
    }
    fn run(&self, args: &[&str], db: &Path) -> Output {
        let cwd = tempfile::tempdir().unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_install"));
        cmd.current_dir(cwd.path()).args(args).arg("--db").arg(db);
        // A switch takes only what it needs; a plain install is kept away from
        // the real Claude Code files.
        let is_switch = args.first().is_some_and(|a| a.starts_with("--set-"));
        if !is_switch || args.first() == Some(&"--set-connections") {
            cmd.arg("--settings").arg(self.settings());
        }
        if !is_switch {
            cmd.arg("--no-mcp");
        }
        if let Some(bin) = &self.bin {
            let ext = if cfg!(windows) { ".exe" } else { "" };
            cmd.arg("--serve-exe").arg(bin.path().join(format!("serve{ext}")));
        }
        cmd.env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("LOCALAPPDATA", self.home.path().join("localappdata"))
            .env("GIT_CONFIG_GLOBAL", self.gitconfig())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    }
    fn install(&self, args: &[&str]) -> Output {
        self.run(args, &self.db())
    }
    fn settings_text(&self) -> String {
        std::fs::read_to_string(self.settings()).unwrap_or_default()
    }
}

fn said(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

/// THOR's hook entries in a settings text. Windows writes the program and an
/// `args` list (`"hook"` is its own element); other systems write one shell
/// command string that ends in ` hook`.
fn hook_entries(text: &str) -> usize {
    text.matches("\"hook\"").count() + text.matches(" hook\"").count()
}

#[test]
fn a_no_to_the_connections_is_removed_recorded_and_kept_by_the_next_plain_install() {
    let sb = Sandbox::new();
    let first = sb.install(&[]);
    assert!(first.status.success(), "{}", said(&first));
    assert_eq!(hook_entries(&sb.settings_text()), 4, "{}", sb.settings_text());

    let off = sb.install(&["--set-connections", "off"]);
    assert!(off.status.success(), "{}", said(&off));
    assert_eq!(hook_entries(&sb.settings_text()), 0, "{}", sb.settings_text());

    let again = sb.install(&[]);
    assert!(again.status.success(), "{}", said(&again));
    assert!(said(&again).contains("you chose no for the connection between THOR and Claude Code"), "{}", said(&again));
    assert_eq!(hook_entries(&sb.settings_text()), 0, "a plain install wired them again:\n{}", sb.settings_text());
    assert!(!said(&again).contains("= THOR was already connected"), "{}", said(&again));

    let on = sb.install(&["--set-connections", "on"]);
    assert!(on.status.success(), "{}", said(&on));
    assert_eq!(hook_entries(&sb.settings_text()), 4, "{}", sb.settings_text());
    let last = sb.install(&[]);
    assert!(said(&last).contains("= THOR was already connected to Claude Code"), "{}", said(&last));
}

#[test]
fn doctor_calls_a_no_to_the_connections_a_choice_not_a_problem() {
    let sb = Sandbox::new();
    assert!(sb.install(&[]).status.success());
    assert!(sb.install(&["--set-connections", "off"]).status.success());
    let cwd = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_doctor"))
        .current_dir(cwd.path())
        .arg("--db")
        .arg(sb.db())
        .env("HOME", sb.home.path())
        .env("USERPROFILE", sb.home.path())
        .env("LOCALAPPDATA", sb.home.path().join("localappdata"))
        .output()
        .unwrap();
    let text = said(&out);
    assert!(text.contains("OK: you chose not to connect THOR to Claude Code"), "{text}");
    assert!(!text.contains("PROBLEM: Claude Code does not know about THOR"), "{text}");
}

#[test]
fn a_no_to_the_reply_rules_sets_the_file_aside_and_a_plain_install_leaves_it_off() {
    let sb = Sandbox::new();
    assert!(sb.install(&[]).status.success());
    let rulebook = sb.db().parent().unwrap().join("guard-response-rulebook.json");
    assert!(rulebook.exists());
    std::fs::write(&rulebook, std::fs::read_to_string(&rulebook).unwrap() + "\n").unwrap();
    let changed = std::fs::read_to_string(&rulebook).unwrap();

    let off = sb.install(&["--set-reply-rules", "off"]);
    assert!(off.status.success(), "{}", said(&off));
    assert!(!rulebook.exists());
    let again = sb.install(&[]);
    assert!(said(&again).contains("you chose no for the reply rules"), "{}", said(&again));
    assert!(!rulebook.exists(), "a plain install wrote the reply rules again");

    let on = sb.install(&["--set-reply-rules", "on"]);
    assert!(on.status.success(), "{}", said(&on));
    assert_eq!(std::fs::read_to_string(&rulebook).unwrap(), changed, "the person's own changes come back");
}

#[test]
fn a_no_to_the_git_hooks_is_recorded_and_a_plain_install_wires_nothing() {
    let sb = Sandbox::new().with_programs();
    assert!(sb.install(&[]).status.success());
    let config = std::fs::read_to_string(sb.gitconfig()).unwrap_or_default();
    assert!(config.contains("hooksPath"), "the install did not wire the git helpers: {config}");

    let off = sb.install(&["--set-git-hooks", "off"]);
    assert!(off.status.success(), "{}", said(&off));
    let config = std::fs::read_to_string(sb.gitconfig()).unwrap_or_default();
    assert!(!config.contains("hooksPath"), "{config}");
    let again = sb.install(&[]);
    assert!(said(&again).contains("you chose no for THOR's helpers in git"), "{}", said(&again));
    assert!(!std::fs::read_to_string(sb.gitconfig()).unwrap_or_default().contains("hooksPath"));

    let on = sb.install(&["--set-git-hooks", "on"]);
    assert!(on.status.success(), "{}", said(&on));
    assert!(std::fs::read_to_string(sb.gitconfig()).unwrap_or_default().contains("hooksPath"));
}

#[test]
fn a_switch_refuses_a_word_it_does_not_know() {
    let sb = Sandbox::new();
    assert!(sb.install(&[]).status.success());
    for flag in ["--set-connections", "--set-git-hooks", "--set-reply-rules"] {
        let out = sb.install(&[flag, "maybe"]);
        assert_eq!(out.status.code(), Some(2), "{flag}: {}", said(&out));
        assert!(said(&out).contains("'on' or 'off'"), "{}", said(&out));
    }
}

#[test]
fn a_second_memory_is_refused_while_the_first_is_connected_and_can_take_over_on_request() {
    let sb = Sandbox::new();
    let first = sb.db_named("thor-a");
    let second = sb.db_named("thor-b");
    assert!(sb.run(&[], &first).status.success());
    let before = sb.settings_text();
    assert_eq!(hook_entries(&before), 4);

    let refused = sb.run(&[], &second);
    assert!(!refused.status.success(), "{}", said(&refused));
    let text = said(&refused);
    assert!(text.contains("already connected to another THOR memory") && text.contains("--move-to-this-store"), "{text}");
    assert!(text.contains("Nothing was changed"), "{text}");
    assert!(!second.exists(), "the refused run created a memory");
    assert_eq!(sb.settings_text(), before, "the refused run changed the settings");

    let moved = sb.run(&["--move-to-this-store"], &second);
    assert!(moved.status.success(), "{}", said(&moved));
    let after = sb.settings_text();
    assert_eq!(hook_entries(&after), 4, "one set of four, not two:\n{after}");
    assert!(after.contains("thor-b") && !after.contains("thor-a"), "{after}");
}

/// A reinstall from another program folder (a new download unpacked elsewhere)
/// must REPLACE THOR's connections, not add a second set beside the first.
#[test]
fn a_reinstall_from_another_program_folder_replaces_the_connections_instead_of_adding_a_second_set() {
    let mut sb = Sandbox::new().with_programs();
    let first = sb.bin.as_ref().unwrap().path().to_path_buf();
    assert!(sb.install(&[]).status.success());
    assert_eq!(hook_entries(&sb.settings_text()), 4);

    let install = PathBuf::from(env!("CARGO_BIN_EXE_install"));
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let second = tempfile::tempdir().unwrap();
    std::fs::copy(install.with_file_name(format!("serve{ext}")), second.path().join(format!("serve{ext}"))).unwrap();
    std::fs::copy(&install, second.path().join(format!("codeindex{ext}"))).unwrap();
    let second_path = second.path().to_path_buf();
    sb.bin = Some(second);

    let out = sb.install(&[]);
    assert!(out.status.success(), "{}", said(&out));
    let text = sb.settings_text();
    assert_eq!(hook_entries(&text), 4, "one set of four, not eight:\n{text}");
    let flat = text.replace("\\\\", "/").replace('\\', "/").to_lowercase();
    assert!(flat.contains(&second_path.display().to_string().replace('\\', "/").to_lowercase()), "points at the new folder:\n{text}");
    assert!(!flat.contains(&first.display().to_string().replace('\\', "/").to_lowercase()), "no longer points at the old folder:\n{text}");
    assert!(said(&out).contains("replaced an older connection"), "{}", said(&out));
}

/// The refusal names the other memory as it was written, not in lower case.
#[test]
fn the_other_memory_is_named_as_written_in_the_refusal() {
    let sb = Sandbox::new();
    let first = sb.db_named("Thor-Mixed-Case");
    let second = sb.db_named("thor-b");
    assert!(sb.run(&[], &first).status.success());
    let text = said(&sb.run(&[], &second));
    assert!(text.contains(&first.display().to_string()), "the path as written ({}) is in: {text}", first.display());
}

/// A git helper folder THOR made for one memory is cleared by an uninstall that
/// names another; one that is not THOR's never is.
#[test]
fn uninstall_for_another_memory_still_recognises_the_git_helper_folder_thor_made() {
    let sb = Sandbox::new().with_programs();
    assert!(sb.run(&[], &sb.db_named("thor-a")).status.success());
    let theirs = sb.db_named("thor-a").parent().unwrap().join("githooks");
    assert!(theirs.is_dir(), "the install made the helper folder");
    let other = sb.db_named("thor-b");
    assert!(ops::uninstall::hooks_path_is_thors(Some(&theirs.display().to_string()), &other));
    let foreign = sb.home.path().join("their-hooks");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join("pre-commit"), "#!/bin/sh\n").unwrap();
    assert!(!ops::uninstall::hooks_path_is_thors(Some(&foreign.display().to_string()), &other));
}

#[test]
fn uninstall_removes_every_thor_connection_whatever_memory_it_names() {
    let sb = Sandbox::new();
    assert!(sb.run(&[], &sb.db_named("thor-a")).status.success());
    // A second set, as an older release would have added beside the first.
    let text = sb.settings_text().replace("thor-a", "thor-b");
    let mut root: serde_json::Value = serde_json::from_str(&sb.settings_text()).unwrap();
    let other: serde_json::Value = serde_json::from_str(&text).unwrap();
    for (event, groups) in other["hooks"].as_object().unwrap() {
        root["hooks"][event].as_array_mut().unwrap().extend(groups.as_array().unwrap().iter().cloned());
    }
    std::fs::write(sb.settings(), serde_json::to_string_pretty(&root).unwrap()).unwrap();
    assert_eq!(hook_entries(&sb.settings_text()), 8);

    let removed = ops::uninstall::remove_thor_hooks(&sb.settings()).unwrap();
    assert_eq!(removed, 8);
    assert_eq!(ops::uninstall::thor_hooks_in(&sb.settings()).unwrap(), 0);
}

#[test]
fn a_no_to_the_connections_keeps_the_daily_safety_copy_and_leaves_no_empty_lists() {
    let sb = Sandbox::new();
    assert!(sb.install(&[]).status.success());
    let backup = sb.install(&["--set-backup", "local"]);
    assert!(backup.status.success(), "{}", said(&backup));
    let with_backup = sb.settings_text();
    assert!(with_backup.contains("backup"), "{with_backup}");

    let off = sb.install(&["--set-connections", "off"]);
    assert!(off.status.success(), "{}", said(&off));
    let after = sb.settings_text();
    assert_eq!(hook_entries(&after), 0, "{after}");
    assert!(after.contains("backup"), "the safety copy's entry went with the connections:\n{after}");
    for event in ["SessionStart", "PreToolUse", "UserPromptSubmit"] {
        assert!(!after.contains(&format!("\"{event}\": []")), "an empty list for {event} was left:\n{after}");
    }
    let on = sb.install(&["--set-connections", "on"]);
    assert!(on.status.success(), "{}", said(&on));
    assert!(sb.settings_text().contains("backup"));
    let doctor = Command::new(env!("CARGO_BIN_EXE_doctor"))
        .arg("--db")
        .arg(sb.db())
        .env("HOME", sb.home.path())
        .env("USERPROFILE", sb.home.path())
        .env("LOCALAPPDATA", sb.home.path().join("localappdata"))
        .output()
        .unwrap();
    assert!(said(&doctor).contains("backup:"), "{}", said(&doctor));
}
