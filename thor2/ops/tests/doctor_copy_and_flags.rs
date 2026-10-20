//! `doctor` on a read-only COPY of the memory does not report what a copy
//! cannot fix as a problem, and `doctor --settings` / `--mcp-json` point it
//! at Claude Code's files when they are not in the standard place.

use std::path::Path;
use std::process::Command;

use model::item::{Binding, Item, Kind, TargetKind};
use thor_core::event_store::{EventKind, EventStore};

fn put(store: &mut EventStore, item: &Item) {
    let body = serde_json::to_string(item).unwrap();
    store.append_event("s", "l", "migration", EventKind::FactCreated, &item.id, None, &body).unwrap();
}

fn rule(id: &str, project: Option<&str>, bindings: Vec<Binding>) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("fixture rule {id}"),
        bindings,
        severity: None,
        project: project.map(str::to_string),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("this fixture rule turns out to be wrong".to_string()),
        check: None,
    }
}

/// A store with a project that owns a rule but has no registered working
/// copy, and a rule whose only command anchor no call can ever reach.
fn fixture(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    put(&mut store, &rule("alpha-rule", Some("alpha"), vec![Binding::Always]));
    let dead = Binding::Target { kind: TargetKind::Command, value: "remember".to_string() };
    put(&mut store, &rule("dead-anchor", None, vec![dead]));
    db
}

fn doctor(db: &Path, home: &Path, extra: &[&Path]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_doctor"));
    cmd.arg("--db").arg(db).arg("--full").env("USERPROFILE", home).env("HOME", home).env("THOR_SCHTASKS_XML", home.join("tasks.xml"));
    for pair in extra.chunks(2) {
        cmd.arg(pair[0]).arg(pair[1]);
    }
    String::from_utf8(cmd.output().unwrap().stdout).unwrap()
}

#[test]
fn a_normal_memory_still_reports_both_as_things_to_fix() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    let out = doctor(&db, dir.path(), &[]);
    assert!(out.contains("have no registered working copy"), "{out}");
    assert!(out.contains("command anchor never fired: dead-anchor"), "{out}");
}

#[test]
fn a_copy_softens_the_working_copy_line_and_does_not_list_the_never_fired_anchors() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    thor_core::copy_mode::write_marker(&db, &Default::default()).unwrap();
    let out = doctor(&db, dir.path(), &[]);
    let working = out.lines().find(|l| l.starts_with("working copies:")).unwrap_or_else(|| panic!("{out}"));
    assert!(working.contains("NOTE (optional, nothing is wrong)") && working.contains("alpha"), "{working}");
    assert!(!working.contains("own rules but have no registered"), "{working}");
    let anchors = out.lines().find(|l| l.starts_with("command anchors never fired:")).unwrap_or_else(|| panic!("{out}"));
    assert!(anchors.contains("NOTE (nothing is wrong here): 1 of 1 live item(s)"), "{anchors}");
    assert!(!out.contains("command anchor never fired: dead-anchor"), "the list is not printed on a copy:\n{out}");
}

#[test]
fn settings_and_mcp_json_flags_point_doctor_at_files_in_other_places() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    let other = dir.path().join("elsewhere");
    std::fs::create_dir_all(&other).unwrap();
    let settings = other.join("my-settings.json");
    let command = format!("\"serve.exe\" --db \"{}\" hook", db.display());
    let hooks = serde_json::json!({"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": command}]}]}});
    std::fs::write(&settings, hooks.to_string()).unwrap();
    let mcp = other.join("my-claude.json");
    let servers = serde_json::json!({"mcpServers": {"thor": {"command": "mcp.exe", "args": ["--db", db.display().to_string()]}}});
    std::fs::write(&mcp, servers.to_string()).unwrap();

    let without = doctor(&db, dir.path(), &[]);
    assert!(without.contains("PROBLEM: Claude Code does not know about THOR"), "{without}");

    let s = Path::new("--settings");
    let m = Path::new("--mcp-json");
    let with = doctor(&db, dir.path(), &[s, settings.as_path(), m, mcp.as_path()]);
    assert!(with.contains("OK: THOR is connected to Claude Code."), "{with}");
    assert!(with.contains("OK: your assistant can save notes."), "{with}");
    assert!(!with.contains("does not know about THOR"), "{with}");
}

#[test]
fn sync_pull_help_says_a_copy_needs_no_arguments_but_the_copy() {
    let out = Command::new(env!("CARGO_BIN_EXE_sync")).args(["pull", "--help"]).output().unwrap();
    let help = String::from_utf8(out.stdout).unwrap().split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(help.contains("with nothing else: the copy remembers"), "{help}");
    assert!(help.contains("its backup and its selection"), "{help}");
}
