//! The full install asks for a Claude Code restart only when something a
//! restart loads changed (its hooks, the note-saving program entry, a code
//! index built for the first time) or on the very first install; a repeat run
//! that changed none of those says no restart is needed.
//!
//! Every spawned program runs with HOME, USERPROFILE and LOCALAPPDATA pointed
//! at a throwaway sandbox, and every install passes `--no-mcp` plus an
//! explicit `--settings`, so no real file of the machine is ever opened.

use std::path::Path;
use std::process::Command;

fn said(sandbox: &tempfile::TempDir, folder: &Path) -> String {
    let settings = sandbox.path().join(".claude").join("settings.json");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_install"));
    cmd.current_dir(folder).arg("--no-mcp").arg("--settings").arg(settings);
    cmd.env("HOME", sandbox.path());
    cmd.env("USERPROFILE", sandbox.path());
    cmd.env("LOCALAPPDATA", sandbox.path().join("localappdata"));
    cmd.env("GIT_CONFIG_GLOBAL", sandbox.path().join(".gitconfig"));
    cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn a_repeat_install_that_changed_nothing_a_restart_loads_needs_no_restart() {
    let sandbox = tempfile::tempdir().unwrap();
    let folder = sandbox.path().join("Documents").join("Kitchen renovation");
    std::fs::create_dir_all(&folder).unwrap();

    let first = said(&sandbox, &folder);
    assert!(first.contains("ONE THING LEFT FOR YOU: close Claude Code completely"), "{first}");
    assert!(first.contains("this is the first install"), "{first}");

    let again = said(&sandbox, &folder);
    assert!(!again.contains("ONE THING LEFT"), "{again}");
    assert!(again.contains("you do not need to restart it"), "{again}");
}
