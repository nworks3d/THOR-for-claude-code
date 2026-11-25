//! A git folder THOR has no memory for yet says so at session start, through the real compiled
//! `serve hook`, with the one command that registers it. A registered repository, a plain subject
//! folder and a folder that is no repository say nothing of the kind.

use model::item::{Item, Kind};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use thor_core::event_store::{EventKind, EventStore};

fn start(db: &Path, cwd: &Path, session: &str) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(db).arg("hook").current_dir(cwd).env_remove("CLAUDE_CODE_ENTRYPOINT");
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn serve");
    let payload = serde_json::json!({ "hook_event_name": "SessionStart", "source": "startup", "session_id": session, "cwd": cwd.to_string_lossy() });
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    let stdout = String::from_utf8(out.stdout).unwrap();
    if stdout.trim().is_empty() {
        return String::new();
    }
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("hook JSON ({e}): {stdout}"));
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

fn register(db: &Path, key: &str, root: &Path) {
    let mut store = EventStore::new(db).unwrap();
    let record = Item {
        id: model::store::project_opened_id(key),
        kind: Kind::Report,
        text: format!("Project {key}: a fixture record."),
        bindings: Vec::new(),
        severity: None,
        project: Some(key.to_string()),
        tags: vec!["project-record".to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    let body = model::store::canonical_body(&record).unwrap();
    let body = format!(
        "{},\"root\":{},\"root_at\":\"2026-09-29T00:00:00Z\"}}",
        &body[..body.len() - 1],
        serde_json::to_string(root.to_str().unwrap()).unwrap()
    );
    store.append_event("install", "install", model::store::INSTALLER_ACTOR, EventKind::FactCreated, &record.id, None, &body).unwrap();
}

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("store")).unwrap();
    let db = dir.path().join("store").join("store.db");
    EventStore::new(&db).unwrap();
    (dir, db)
}

fn repo(dir: &Path, name: &str) -> PathBuf {
    let root = dir.join(name);
    std::fs::create_dir_all(root.join(".git")).unwrap();
    root
}

#[test]
fn an_unregistered_git_folder_is_told_how_to_get_a_memory_and_nothing_else_is() {
    let (dir, db) = fixture();
    let fresh = repo(dir.path(), "calculator");
    let said = start(&db, &fresh, "s1");
    assert!(said.contains("is a git project that THOR has no memory for yet"), "{said}");
    assert!(said.contains("install") && said.contains("--db"), "it names the one command: {said}");
    assert!(said.contains("yes or no"), "a beginner is only asked yes or no: {said}");

    // Registered: the line is gone.
    let registered = repo(dir.path(), "shop");
    register(&db, "shop", &registered);
    assert!(!start(&db, &registered, "s2").contains("no memory for yet"));

    // A plain subject folder with its own marker, and a folder that is no repository.
    let topic = dir.path().join("garden");
    std::fs::create_dir_all(&topic).unwrap();
    std::fs::write(topic.join(".thor-project"), "garden\n").unwrap();
    assert!(!start(&db, &topic, "s3").contains("no memory for yet"));
    let plain = dir.path().join("loose");
    std::fs::create_dir_all(&plain).unwrap();
    assert!(!start(&db, &plain, "s4").contains("no memory for yet"));
}
