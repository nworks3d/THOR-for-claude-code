//! THE DEFECT THIS PINS DOWN. The CHANGELOG and the evaluation-filing refusal
//! both tell a user to run the teeth backfill, but the release left the
//! program out of the bundle (`SHIPPED_BINS` in the release workflow), and
//! the text called it `backfill-teeth` while Cargo builds `backfill_teeth`.
//! A user following the instruction found nothing to run, and would not have
//! found it under that name either.
//!
//! Reads the repository's own files, the same technique
//! `a_comment_never_claims_what_the_code_does_not_do.rs` uses, plus the
//! built binary itself, so the name is checked where a user meets it.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = "backfill_teeth";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("serve/ sits two levels below the repository root")
        .to_path_buf()
}

#[test]
fn the_release_bundle_ships_the_backfill() {
    let workflow = repo_root().join(".github/workflows/release.yml");
    let text = std::fs::read_to_string(&workflow).expect("the release workflow must be readable");
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("SHIPPED_BINS:"))
        .expect("the release workflow must define SHIPPED_BINS");
    let shipped: Vec<&str> = line["SHIPPED_BINS:".len()..].split_whitespace().collect();
    assert!(shipped.contains(&BIN), "{BIN} must be in the release bundle: {line}");
}

#[test]
fn the_binary_names_itself_the_way_it_is_built() {
    let out = Command::new(env!("CARGO_BIN_EXE_backfill_teeth"))
        .arg("--version")
        .output()
        .expect("the backfill binary must run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "--version must succeed: {stdout}");
    assert!(stdout.starts_with(&format!("{BIN} ")), "--version must print the real program name: {stdout}");
}

#[test]
fn the_changelog_names_the_program_a_user_can_actually_run() {
    let changelog = std::fs::read_to_string(repo_root().join("CHANGELOG.md")).expect("CHANGELOG.md must be readable");
    assert!(changelog.contains(&format!("`{BIN}")), "the changelog must name `{BIN}`");
    assert!(!changelog.contains("backfill-teeth"), "no program called backfill-teeth is ever built");
}
