//! The changelog flow at the commit moment, end to end through the real compiled
//! `serve hook` binary: with no answer on record a git project is asked once per
//! session; with "off" the flow notes are not served in that project (and every
//! other note is); with "on", and in another project, they are served. A folder
//! that is not a code project is never asked.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const FLOW_NOTE: &str = "changelog-with-every-user-visible-change";
const OTHER_NOTE: &str = "an-ordinary-commit-note";

fn run_hook(db: &Path, payload: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    String::from_utf8(out.stdout).unwrap()
}

fn commit_payload(session_id: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Bash",
        "tool_input": { "command": "git commit -m x" },
    })
    .to_string()
}

fn context_of(out: &str) -> String {
    if out.trim().is_empty() {
        return String::new();
    }
    let v: serde_json::Value = serde_json::from_str(out).unwrap_or_else(|e| panic!("not JSON: {e}: {out}"));
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

fn note(id: &str, tags: Vec<String>) -> model::item::Item {
    model::item::Item {
        id: id.to_string(),
        kind: model::item::Kind::Rule,
        text: format!("note {id} says a thing about commits"),
        bindings: vec![model::item::Binding::Moment(intent::Action::Commit)],
        severity: None,
        project: None,
        tags,
        expires: None,
        key: None,
        falsifier: Some("a commit made without this note turns out as good".to_string()),
        check: None,
    }
}

fn store_with_the_two_notes(dir: &Path) -> PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    model::store::declare(&mut store, "s", "l", "a", &note(FLOW_NOTE, vec![serve::changelog_flow::TAG.to_string()])).unwrap();
    model::store::declare(&mut store, "s", "l", "a", &note(OTHER_NOTE, vec![])).unwrap();
    db
}

fn git_project(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new("git").arg("-C").arg(&dir).args(["init", "--quiet"]).output().expect("git must be on PATH");
    assert!(out.status.success());
    dir
}

#[test]
fn with_no_answer_the_commit_moment_carries_the_question_once_per_session() {
    let dir = tempfile::tempdir().unwrap();
    let db = store_with_the_two_notes(dir.path());
    let repo = git_project(dir.path(), "demo-shop");

    let first = context_of(&run_hook(&db, &commit_payload("s1", &repo)));
    assert!(first.contains("Shall I keep a changelog and version numbers for this project?"), "{first}");
    assert!(first.contains("--set-changelog-flow on") && first.contains("--set-changelog-flow off"), "{first}");
    let second = context_of(&run_hook(&db, &commit_payload("s1", &repo)));
    assert!(!second.contains("Shall I keep a changelog"), "asked a second time in one session: {second}");
    let next_session = context_of(&run_hook(&db, &commit_payload("s2", &repo)));
    assert!(next_session.contains("Shall I keep a changelog"), "a new session asks again until answered: {next_session}");

    serve::changelog_flow::set(&db, "demo-shop", true).unwrap();
    let answered = context_of(&run_hook(&db, &commit_payload("s3", &repo)));
    assert!(!answered.contains("Shall I keep a changelog"), "an answer on record ends the question: {answered}");
}

#[test]
fn a_folder_that_is_not_a_code_project_is_never_asked() {
    let dir = tempfile::tempdir().unwrap();
    let db = store_with_the_two_notes(dir.path());
    let plain = dir.path().join("kitchen-renovation");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join(".thor-project"), "kitchen-renovation\n").unwrap();
    let ctx = context_of(&run_hook(&db, &commit_payload("s1", &plain)));
    assert!(!ctx.contains("Shall I keep a changelog"), "{ctx}");
}

#[test]
fn off_hides_the_flow_notes_in_that_project_only() {
    let dir = tempfile::tempdir().unwrap();
    let db = store_with_the_two_notes(dir.path());
    let shop = git_project(dir.path(), "demo-shop");
    let blog = git_project(dir.path(), "demo-blog");
    serve::changelog_flow::set(&db, "demo-shop", false).unwrap();
    serve::changelog_flow::set(&db, "demo-blog", true).unwrap();

    let in_shop = context_of(&run_hook(&db, &commit_payload("s1", &shop)));
    assert!(!in_shop.contains(FLOW_NOTE), "the flow note must not be served where the answer is no: {in_shop}");
    assert!(in_shop.contains(OTHER_NOTE), "no other note is affected: {in_shop}");
    assert!(!in_shop.contains("Shall I keep a changelog"), "a no is never raised again: {in_shop}");

    let in_blog = context_of(&run_hook(&db, &commit_payload("s2", &blog)));
    assert!(in_blog.contains(FLOW_NOTE) && in_blog.contains(OTHER_NOTE), "{in_blog}");

    // Switching to yes brings the notes back in the first project.
    serve::changelog_flow::set(&db, "demo-shop", true).unwrap();
    let again = context_of(&run_hook(&db, &commit_payload("s3", &shop)));
    assert!(again.contains(FLOW_NOTE), "{again}");
}
