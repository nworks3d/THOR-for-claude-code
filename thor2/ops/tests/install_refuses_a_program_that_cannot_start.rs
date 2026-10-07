//! A hook calling a program the machine will not start fails open: Claude Code
//! records an error and carries on without THOR. Windows Smart App Control or an
//! antivirus can do exactly that to a freshly built `serve.exe`, and the file is
//! then present but dead. The installer launches the program once and refuses,
//! in plain words, rather than writing hooks that can never speak.

use std::process::Command;

#[test]
fn install_refuses_a_serve_program_that_cannot_be_started_and_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    // A file is there, but it is no program: starting it fails the way a
    // blocked program does.
    let dead = home.path().join(if cfg!(windows) { "serve.exe" } else { "serve" });
    std::fs::write(&dead, "this is not a program").unwrap();
    let settings = home.path().join("settings.json");
    let out = Command::new(env!("CARGO_BIN_EXE_install"))
        .current_dir(home.path())
        .arg("--no-mcp")
        .arg("--serve-exe")
        .arg(&dead)
        .arg("--settings")
        .arg(&settings)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("LOCALAPPDATA", home.path().join("localappdata"))
        .env("GIT_CONFIG_GLOBAL", home.path().join(".gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "install must refuse: {stderr}");
    assert!(stderr.contains("could not be started"), "{stderr}");
    assert!(stderr.contains("refusing to write hooks"), "{stderr}");
    assert!(!settings.exists(), "nothing is written when the program cannot start");
}

#[test]
fn a_program_that_starts_and_exits_cleanly_is_no_problem() {
    // doctor answers --version with exit 0, as serve does.
    assert_eq!(ops::install::launch_problem(std::path::Path::new(env!("CARGO_BIN_EXE_doctor"))), None);
}
