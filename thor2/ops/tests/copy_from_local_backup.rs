//! A second machine can copy from the backup folder that `install --set-backup
//! local` makes (a git folder with no remote). It used to fail with raw git
//! text ("git branch --set-upstream-to=..."); now the folder is read as it is,
//! and a backup that cannot be reached is a plain sentence with the next step.
//!
//! Every run has HOME, USERPROFILE, LOCALAPPDATA, APPDATA and the global git
//! config pointed at a throwaway folder.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Machine {
    home: PathBuf,
}

impl Machine {
    fn new(root: &Path, name: &str) -> Self {
        let home = root.join(name);
        std::fs::create_dir_all(&home).unwrap();
        Self { home }
    }
    fn db(&self) -> PathBuf {
        self.home.join("lad").join("thor2").join("thor.db")
    }
    fn install(&self, args: &[&str]) -> Output {
        let cwd = tempfile::tempdir().unwrap();
        Command::new(env!("CARGO_BIN_EXE_install"))
            .current_dir(cwd.path())
            .args(args)
            .arg("--settings")
            .arg(self.home.join("settings.json"))
            .arg("--db")
            .arg(self.db())
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("LOCALAPPDATA", self.home.join("lad"))
            .env("APPDATA", self.home.join("ad"))
            .env("GIT_CONFIG_GLOBAL", self.home.join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    }
}

fn said(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

#[test]
fn a_local_backup_folder_is_a_valid_source_for_a_copy() {
    let root = tempfile::tempdir().unwrap();
    let main = Machine::new(root.path(), "main");
    let copy = Machine::new(root.path(), "copy");
    let first = main.install(&["--no-mcp"]);
    assert!(first.status.success(), "{}", said(&first));
    let backup = main.install(&["--set-backup", "local"]);
    assert!(backup.status.success(), "{}", said(&backup));
    let folder = main.db().parent().unwrap().join("backup-repo");
    assert!(folder.join(".git").exists(), "{}", said(&backup));

    let folder_text = folder.display().to_string();
    let made = copy.install(&["--copy-from", &folder_text, "--whole-memory"]);
    let text = said(&made);
    assert!(made.status.success(), "{text}");
    assert!(text.contains("made the copy from the backup"), "{text}");
    assert!(!text.contains("set-upstream-to") && !text.contains("git pull failed"), "{text}");
}

#[test]
fn a_backup_that_cannot_be_reached_is_a_plain_sentence_with_the_next_step() {
    let root = tempfile::tempdir().unwrap();
    let copy = Machine::new(root.path(), "copy");
    let nowhere = root.path().join("no-such-backup");
    let made = copy.install(&["--copy-from", &nowhere.display().to_string(), "--whole-memory"]);
    let text = said(&made);
    assert!(!made.status.success(), "{text}");
    assert!(text.contains("could not be reached") && text.contains("Next step"), "{text}");
    assert!(!text.contains("fatal:") && !text.contains("git ls-remote"), "raw git text leaked: {text}");
}
