//! The shipped `checked-claim-needs-evidence` rule, end to end through the
//! real compiled `serve hook` binary: a `path:line` citation counts as
//! evidence only when that file exists under the session's working directory
//! and has that line.
//!
//! THE ESCAPE THIS CLOSES (2026-09-26): the rule accepted any text shaped like
//! `name.ext:NN`, so an answer could claim to have verified something, type a
//! file name and a line number nobody opened, and pass.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SHIPPED_RULEBOOK: &str = include_str!("../../guard-response-rulebook.example.json");

fn run_hook(db: &Path, payload: &serde_json::Value) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    String::from_utf8(out.stdout).unwrap()
}

/// A store with the shipped rulebook beside it, and a working directory (no
/// project) holding `src/lib.rs` with five lines, next to a file outside it.
fn fixture(tmp: &Path) -> (PathBuf, PathBuf) {
    let store = tmp.join("store");
    std::fs::create_dir_all(&store).unwrap();
    let db = store.join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(store.join("guard-response-rulebook.json"), SHIPPED_RULEBOOK).unwrap();
    let work = tmp.join("work");
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("src").join("lib.rs"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
    std::fs::write(tmp.join("outside.rs"), "one\ntwo\n").unwrap();
    (db, work)
}

fn stop(cwd: Option<&Path>, message: &str) -> serde_json::Value {
    let mut p = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "s1",
        "stop_hook_active": false,
        "last_assistant_message": message,
    });
    if let Some(cwd) = cwd {
        p["cwd"] = serde_json::json!(cwd.to_string_lossy());
    }
    p
}

fn blocked(out: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(out.trim()).ok().and_then(|v| v.get("decision").cloned())
        == Some(serde_json::json!("block"))
}

#[test]
fn a_citation_of_a_real_line_under_the_working_directory_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, work) = fixture(tmp.path());
    let relative = run_hook(&db, &stop(Some(&work), "Verified: src/lib.rs:3 has the fix."));
    assert!(!blocked(&relative), "a real line must count: {relative}");
    let absolute = format!("Verified: {}:5 has the fix.", work.join("src").join("lib.rs").display());
    let out = run_hook(&db, &stop(Some(&work), &absolute));
    assert!(!blocked(&out), "an absolute path under the working directory must count: {out}");
}

#[test]
fn a_citation_nobody_could_have_opened_is_held_and_named() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, work) = fixture(tmp.path());
    for (message, cited) in [
        ("Verified: src/lib.rs:9 has the fix.", "src/lib.rs:9"),
        ("Verified: src/missing.rs:1 has the fix.", "src/missing.rs:1"),
        ("Verified: ../outside.rs:1 has the fix.", "../outside.rs:1"),
        ("Verified: src/lib.rs:0 has the fix.", "src/lib.rs:0"),
    ] {
        let out = run_hook(&db, &stop(Some(&work), message));
        assert!(blocked(&out), "{message} must be held: {out}");
        assert!(out.contains(cited), "the reason must name the citation that did not count: {out}");
    }
}

/// Without a working directory in the payload nothing can be looked up, so a
/// citation is judged by its shape, as before.
#[test]
fn without_a_working_directory_a_citation_counts_by_its_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _work) = fixture(tmp.path());
    let out = run_hook(&db, &stop(None, "Verified: src/lib.rs:9 has the fix."));
    assert!(!blocked(&out), "{out}");
}

// ------------------------------------------- commits and line numbers (2026-09-26)
//
// THE ESCAPES THIS CLOSES: after citations were looked up, the rule still let an
// unchecked claim through on the literal words "commit ", "line " and "regel "
// (anywhere: "pipeline " carries "line "), on any typed sha-shaped token, and on a
// bare "name.rs:" with no line at all.

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .expect("git must be on PATH");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// The fixture's working directory made a git repository with one commit.
fn fixture_in_git(tmp: &Path) -> (PathBuf, PathBuf, String) {
    let (db, work) = fixture(tmp);
    git(&work, &["init", "-q"]);
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "init"]);
    let sha = git(&work, &["rev-parse", "HEAD"]);
    (db, work, sha)
}

#[test]
fn a_commit_counts_only_when_it_exists_in_the_working_directorys_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, work, sha) = fixture_in_git(tmp.path());
    for message in [format!("Verified: commit {} has the fix.", &sha[..8]), format!("Verified: {sha} has the fix.")] {
        let out = run_hook(&db, &stop(Some(&work), &message));
        assert!(!blocked(&out), "a real commit must count: {message}: {out}");
    }
    for (message, named) in [
        ("Verified: commit 1a2b3c4d has the fix.", Some("1a2b3c4d")),
        ("Verified: see 9f8e7d6c5b.", Some("9f8e7d6c5b")),
        ("Verified: see the last commit on main.", None),
    ] {
        let out = run_hook(&db, &stop(Some(&work), message));
        assert!(blocked(&out), "{message} must be held: {out}");
        if let Some(named) = named {
            assert!(out.contains(named), "the reason must name the sha that did not count: {out}");
        }
    }
}

/// The fixture repository with empty commits added until HEAD's first eight
/// characters are all digits (about one commit in forty-three), so the case
/// that made the test above fail one run in fifty is tried on every run.
fn all_digit_head(work: &Path) -> String {
    for n in 0..2000 {
        let sha = git(work, &["rev-parse", "HEAD"]);
        if sha[..8].bytes().all(|b| b.is_ascii_digit()) {
            return sha;
        }
        git(work, &["commit", "-q", "--allow-empty", "-m", &format!("filler {n}")]);
    }
    panic!("no all-digit commit in 2000 tries");
}

#[test]
fn a_commit_whose_short_form_is_all_digits_still_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, work, _sha) = fixture_in_git(tmp.path());
    let sha = all_digit_head(&work);
    let message = format!("Verified: commit {} has the fix.", &sha[..8]);
    let out = run_hook(&db, &stop(Some(&work), &message));
    assert!(!blocked(&out), "a real commit must count even when its short form is all digits: {message}: {out}");
    let made_up = "Verified: commit 12345678 has the fix.";
    let out = run_hook(&db, &stop(Some(&work), made_up));
    assert!(blocked(&out) || sha.starts_with("12345678"), "a made-up all-digit sha must be held: {out}");
}

#[test]
fn a_line_number_counts_only_when_a_named_file_has_that_line() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, work) = fixture(tmp.path());
    for message in ["Verified: line 3 of src/lib.rs has the fix.", "Geverifieerd: regel 4 van src/lib.rs klopt."] {
        let out = run_hook(&db, &stop(Some(&work), message));
        assert!(!blocked(&out), "a real line must count: {message}: {out}");
    }
    for message in [
        "Verified: line 30 of src/lib.rs has the fix.",
        "Verified: regel 2 klopt.",
        "Verified: the pipeline is green.",
        "Verified: src/lib.rs: the fix is there.",
    ] {
        let out = run_hook(&db, &stop(Some(&work), message));
        assert!(blocked(&out), "{message} must be held: {out}");
    }
}

/// Where no git repository answers for the working directory, a sha cannot be
/// looked up and is judged by its shape, as before.
#[test]
fn without_a_git_repository_a_sha_is_judged_by_its_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, work) = fixture(tmp.path());
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(&db)
        .arg("hook")
        .env("GIT_CEILING_DIRECTORIES", tmp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let payload = stop(Some(&work), "Verified: see 1a2b3c4d.");
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    assert!(!blocked(&out), "{out}");
}
