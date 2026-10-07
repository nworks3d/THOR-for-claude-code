//! Every program printed `2.5.0`, so two builds of one release could not be
//! told apart: a tool server older than the hook's checker looked identical to
//! the current one. `--version` now carries a build id, a hash of the sources
//! the build came from, and doctor compares the ids of the installed programs.

use std::process::Command;

fn is_build_id(id: &str) -> bool {
    id.len() == 12 && id.chars().all(|c| c.is_ascii_hexdigit())
}

#[test]
fn serve_version_prints_the_version_and_the_build_id() {
    let out = Command::new(env!("CARGO_BIN_EXE_serve")).arg("--version").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.trim(), serve::version_line("serve"));
    assert!(stdout.starts_with(&format!("serve {} (build ", env!("CARGO_PKG_VERSION"))), "{stdout}");
    assert!(is_build_id(serve::BUILD_ID), "{}", serve::BUILD_ID);
}

#[test]
fn the_build_id_is_read_back_from_a_version_line() {
    assert_eq!(serve::build_id_in("serve 2.5.0 (build 0123456789ab)\n"), Some("0123456789ab"));
    assert_eq!(serve::build_id_in("mcp 2.5.0\n"), None, "an older program prints no build id");
    assert_eq!(serve::build_id_in("serve 2.5.0 (build )"), None);
    assert_eq!(serve::build_id_in(""), None);
}
