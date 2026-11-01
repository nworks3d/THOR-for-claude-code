//! A folder is resolved to the project the memory already has, whatever its
//! own spelling: 'Acme tool tracking' and 'Acme-tool-tracking' are one name
//! (case, spaces and hyphens), so a folder of one spelling is served the notes
//! filed under the other, and a plain folder with no repository and no marker
//! belongs to the project of its own name. Without this a folder opened a second
//! scope, or "belonged to no project" (field reports of 2026-10-22 and
//! 2026-10-28). Runs the real `serve` hook.

use model::item::{Binding, Item, Kind, Severity};
use model::store;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use thor_core::event_store::EventStore;

fn note(id: &str, text: &str, project: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Orientation,
        text: text.to_string(),
        bindings: vec![Binding::Always],
        severity: Some(Severity::Costly),
        project: Some(project.to_string()),
        tags: Vec::new(),
        expires: None,
        key: None,
        falsifier: Some(format!("{id} turns out to be wrong")),
        check: None,
    }
}

fn session_start(db: &Path, cwd: &Path) -> String {
    let payload = serde_json::json!({
        "session_id": "5e1f0c3a-0000-4000-8000-0000000000aa",
        "hook_event_name": "SessionStart",
        "source": "startup",
        "cwd": cwd.to_string_lossy(),
    });
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(db).arg("hook").current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.env_remove("CLAUDE_CODE_ENTRYPOINT");
    let mut child = cmd.spawn().unwrap();
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    if stdout.trim().is_empty() {
        return String::new();
    }
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("hook JSON ({e}): {stdout}"));
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

#[test]
fn a_folder_is_served_the_notes_of_the_existing_spelling_of_its_project() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    {
        let mut s = EventStore::new(&db).unwrap();
        store::declare(&mut s, "s", "l", "a", &note("acme-note", "ACMETEXT labels print per tool.", "Acme-tool-tracking")).unwrap();
        store::declare(&mut s, "s", "l", "a", &note("water-note", "BOARDTEXT the blocks are cast in two parts.", "sensor board")).unwrap();
        store::declare(&mut s, "s", "l", "a", &note("other-note", "OTHERTEXT belongs to another project.", "Investments")).unwrap();
    }

    // A git checkout whose folder is spelled with spaces: the project that exists is spelled with hyphens.
    let repo = dir.path().join("Acme tool tracking");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let served = session_start(&db, &repo);
    assert!(served.contains("ACMETEXT"), "the folder is the project that exists, spelled the other way: {served}");
    assert!(!served.contains("OTHERTEXT") && !served.contains("BOARDTEXT"), "and no other project's notes: {served}");

    // A plain folder (no repository, no marker) named like an existing project belongs to it.
    let plain = dir.path().join("Sensor-Board");
    std::fs::create_dir_all(&plain).unwrap();
    let served = session_start(&db, &plain);
    assert!(served.contains("BOARDTEXT"), "a plain folder named like a project belongs to it: {served}");
    assert!(!served.contains("ACMETEXT"), "{served}");

    // A plain folder named like nothing stays no project: only what applies everywhere.
    let stranger = dir.path().join("Holiday photos");
    std::fs::create_dir_all(&stranger).unwrap();
    let served = session_start(&db, &stranger);
    assert!(!served.contains("ACMETEXT") && !served.contains("BOARDTEXT") && !served.contains("OTHERTEXT"), "{served}");

    // The scopes themselves are untouched: still the same three, spelled as filed.
    let held = EventStore::open_existing(&db).unwrap().project_scope_counts();
    let names: Vec<&str> = held.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Acme-tool-tracking", "Investments", "sensor board"]);
}
