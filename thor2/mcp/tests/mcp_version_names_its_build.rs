//! `mcp --version` prints the same build id `serve --version` does when both
//! come from one source tree, so doctor can tell a stale tool server from a
//! current one without starting either.

use std::process::Command;

#[test]
fn mcp_version_prints_the_version_and_the_build_id() {
    let out = Command::new(env!("CARGO_BIN_EXE_mcp")).arg("--version").output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.trim(), serve::version_line("mcp"));
    assert!(stdout.starts_with(&format!("mcp {} (build ", env!("CARGO_PKG_VERSION"))), "{stdout}");
}
