//! Work that belongs elsewhere gets its own session: against the REAL compiled
//! `serve` binary, a prompt that names another project with its own folder is
//! answered with one plain line for the assistant, and a session that opens in
//! a folder of no project is told to propose a folder. Nothing is said for the
//! session's own project or for a prompt that names none.

use model::item::{Binding, Item, Kind, Severity};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use thor_core::event_store::{EventKind, EventStore};

fn rule_for(project: &str) -> Item {
    Item {
        id: format!("{project}-rule"),
        kind: Kind::Rule,
        text: format!("standing rule for {project}"),
        bindings: vec![Binding::Always],
        severity: Some(Severity::Irreversible),
        project: Some(project.to_string()),
        tags: vec![format!("{}a test fixture where 'the same call' is also the legitimate one", model::store::NO_LITERAL_REASON_PREFIX)],
        expires: None,
        key: None,
        falsifier: Some(format!("the standing rule for {project} turns out to be wrong")),
        check: None,
    }
}

/// The installer's registration of `root` for `key`, as a raw event.
fn register(store: &mut EventStore, key: &str, root: &Path) {
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

struct Fixture {
    _dir: tempfile::TempDir,
    db: PathBuf,
    kitchen: PathBuf,
    garden: PathBuf,
    nowhere: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let kitchen = dir.path().join("Kitchen-renovation");
    let garden = dir.path().join("Garden");
    let nowhere = dir.path().join("plain-folder");
    for repo in [&kitchen, &garden] {
        std::fs::create_dir_all(repo.join(".git")).unwrap();
    }
    std::fs::create_dir_all(&nowhere).unwrap();
    let db = dir.path().join("store.db");
    {
        let mut store = EventStore::new(&db).unwrap();
        for project in ["Kitchen-renovation", "Garden"] {
            model::store::declare(&mut store, "s", "l", "a", &rule_for(project)).unwrap();
        }
        register(&mut store, "Kitchen-renovation", &kitchen);
    }
    Fixture { _dir: dir, db, kitchen, garden, nowhere }
}

fn hook(f: &Fixture, cwd: &Path, payload: serde_json::Value) -> String {
    let mut payload = payload;
    payload["cwd"] = serde_json::json!(cwd.to_str().unwrap());
    payload["session_id"] = serde_json::json!("other-project-test");
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(&f.db)
        .arg("hook")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    serde_json::from_str::<serde_json::Value>(text.trim())
        .ok()
        .and_then(|v| v["hookSpecificOutput"]["additionalContext"].as_str().map(str::to_string))
        .unwrap_or_default()
}

fn prompt(f: &Fixture, cwd: &Path, text: &str) -> String {
    hook(f, cwd, serde_json::json!({ "hook_event_name": "UserPromptSubmit", "prompt": text }))
}

#[test]
fn a_prompt_naming_another_project_with_its_own_folder_gets_one_plain_line() {
    let f = fixture();
    let context = prompt(&f, &f.garden, "what did we decide about the kitchen renovation budget?");
    assert!(context.contains("This message is about Kitchen-renovation"), "{context}");
    assert!(context.contains(&f.kitchen.display().to_string()), "the folder is named: {context}");
    assert!(context.contains("only clicks"), "{context}");
}

#[test]
fn the_same_name_in_another_spelling_is_still_that_project() {
    let f = fixture();
    let context = prompt(&f, &f.garden, "Open KITCHEN-RENOVATION again");
    assert!(context.contains("This message is about Kitchen-renovation"), "{context}");
}

#[test]
fn nothing_is_said_inside_that_project_or_for_a_prompt_that_names_none() {
    let f = fixture();
    let inside = prompt(&f, &f.kitchen, "what did we decide about the kitchen renovation budget?");
    assert!(!inside.contains("This message is about"), "{inside}");
    let none = prompt(&f, &f.garden, "please water the tomatoes in the workshop");
    assert!(!none.contains("This message is about"), "{none}");
}

#[test]
fn a_session_in_a_folder_of_no_project_is_told_to_propose_a_folder() {
    let f = fixture();
    let start = |cwd: &Path| hook(&f, cwd, serde_json::json!({ "hook_event_name": "SessionStart", "source": "startup" }));
    let nowhere = start(&f.nowhere);
    assert!(nowhere.contains("belongs to no project") && nowhere.contains("only clicks"), "{nowhere}");
    let inside = start(&f.garden);
    assert!(!inside.contains("belongs to no project"), "a folder of a project gets no such line: {inside}");
}
