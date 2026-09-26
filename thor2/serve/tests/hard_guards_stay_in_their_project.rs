//! The three hard guards that read only the TEXT of a call - the requires
//! guard, the command guard and the always-forbidden content guard - refuse
//! only for rules that apply to the session's own project, end to end
//! through the real compiled `serve hook` binary.
//!
//! MEASURED (2026-09-26, on a copy of a real store): a rule written for one
//! repository ("a pip install of the fork must carry --no-deps", scoped to
//! that repository) refused a plain `pip install` in a completely different
//! repository. Nothing in a command's text says which checkout it belongs
//! to, and the guard read every live item in the store. A false block is the
//! most expensive outcome this system has, so these tests pin both halves:
//! another project's rule stays silent, and the rule still refuses inside
//! its own project - and a global rule refuses everywhere.

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

fn blocked(out: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(out.trim()).ok().and_then(|v| v.get("decision").cloned())
        == Some(serde_json::json!("block"))
}

/// A working directory that resolves to `project` through its own marker file.
fn checkout(root: &Path, project: &str) -> PathBuf {
    let dir = root.join(project);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".thor-project"), format!("{project}\n")).unwrap();
    dir
}

fn bash(session: &str, cwd: &Path, command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Bash",
        "tool_input": {"command": command},
    })
    .to_string()
}

fn write_file(session: &str, cwd: &Path, file: &str, content: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Write",
        "tool_input": {"file_path": cwd.join(file).to_string_lossy(), "content": content},
    })
    .to_string()
}

fn rule(id: &str, project: Option<&str>, bindings: Vec<Binding>, check: Check) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("probe rule {id} for the project scope of the hard guards"),
        bindings,
        severity: Some(Severity::Costly),
        project: project.map(str::to_string),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the guard turns out to refuse nothing in its own project".to_string()),
        check: Some(check),
    }
}

fn command(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Command, value: value.to_string() }
}

fn store_with(dir: &Path, items: &[Item]) -> PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    for item in items {
        model::store::declare(&mut store, "s", "l", "a", item).unwrap_or_else(|e| panic!("{}: {e}", item.id));
    }
    db
}

fn gate_refusals(db: &Path, id: &str) -> usize {
    let store = thor_core::event_store::EventStore::open_existing(db).unwrap();
    store
        .get_events_by_entity(id)
        .unwrap()
        .iter()
        .filter(|e| e.kind == thor_core::event_store::EventKind::GateRefused)
        .count()
}

#[test]
fn a_requires_rule_from_another_project_never_refuses_an_ordinary_call_here() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(
        tmp.path(),
        &[rule(
            "fork-install-needs-no-deps",
            Some("printer-project"),
            vec![command("pip")],
            Check::Requires { when: "pip".to_string(), required: vec!["--no-deps".to_string()] },
        )],
    );
    let elsewhere = checkout(tmp.path(), "memory-project");
    let out = run_hook(&db, &bash("s1", &elsewhere, "pip install trimesh"));
    assert!(!blocked(&out), "another project's requires rule must not refuse a plain pip install here: {out}");
    assert_eq!(gate_refusals(&db, "fork-install-needs-no-deps"), 0, "nothing was refused, so nothing is recorded");
}

#[test]
fn a_requires_rule_still_refuses_inside_its_own_project_and_the_refusal_is_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(
        tmp.path(),
        &[rule(
            "fork-install-needs-no-deps",
            Some("printer-project"),
            vec![command("pip")],
            Check::Requires { when: "pip".to_string(), required: vec!["--no-deps".to_string()] },
        )],
    );
    let home = checkout(tmp.path(), "printer-project");
    let out = run_hook(&db, &bash("s1", &home, "pip install ./fork"));
    assert!(blocked(&out), "the rule must still refuse inside its own project: {out}");
    assert_eq!(gate_refusals(&db, "fork-install-needs-no-deps"), 1, "a requires refusal must leave a record in the log");
    let out = run_hook(&db, &bash("s1", &home, "pip install ./fork --no-deps"));
    assert!(!blocked(&out), "the satisfied form must pass: {out}");
}

#[test]
fn a_forbidden_command_rule_from_another_project_never_refuses_here() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(
        tmp.path(),
        &[rule(
            "fork-never-force-pushed",
            Some("printer-project"),
            vec![command("git push")],
            Check::Forbidden { literals: vec!["--force".to_string()] },
        )],
    );
    let elsewhere = checkout(tmp.path(), "memory-project");
    let out = run_hook(&db, &bash("s1", &elsewhere, "git push --force origin topic"));
    assert!(!blocked(&out), "another project's forbidden rule must not refuse here: {out}");
    let home = checkout(tmp.path(), "printer-project");
    let out = run_hook(&db, &bash("s1", &home, "git push --force origin main"));
    assert!(blocked(&out), "the rule must still refuse inside its own project: {out}");
}

#[test]
fn a_global_forbidden_command_rule_still_refuses_in_every_project() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(
        tmp.path(),
        &[rule(
            "never-make-a-repo-public",
            None,
            vec![command("gh repo edit")],
            Check::Forbidden { literals: vec!["--visibility public".to_string()] },
        )],
    );
    for project in ["memory-project", "printer-project"] {
        let cwd = checkout(tmp.path(), project);
        let out = run_hook(&db, &bash("s1", &cwd, "gh repo edit acme/x --visibility public"));
        assert!(blocked(&out), "a global rule must refuse in {project}: {out}");
    }
}

#[test]
fn an_always_forbidden_text_from_another_project_never_refuses_a_write_here() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(
        tmp.path(),
        &[rule(
            "no-customer-code-in-files",
            Some("shop-project"),
            vec![Binding::Always],
            Check::Forbidden { literals: vec!["CUST-SECRET-42".to_string()] },
        )],
    );
    let elsewhere = checkout(tmp.path(), "memory-project");
    let out = run_hook(&db, &write_file("s1", &elsewhere, "notes.md", "a test fixture: CUST-SECRET-42\n"));
    assert!(!blocked(&out), "another project's forbidden text must not refuse a write here: {out}");
    let home = checkout(tmp.path(), "shop-project");
    let out = run_hook(&db, &write_file("s1", &home, "notes.md", "a note: CUST-SECRET-42\n"));
    assert!(blocked(&out), "the rule must still refuse inside its own project: {out}");
}
