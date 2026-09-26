//! `serve hook --no-record` leaves the store's directory exactly as it found
//! it, end to end through the real compiled binary: every file beside the
//! store is hashed before and after a run of all four hook events in a
//! project, with a rule that fires, a rule that refuses, and a prompt the
//! capture guard flags.
//!
//! MEASURED (2026-09-26, on a copy of a real store): a `--no-record` hook call
//! still wrote a session line into `eval-debt-state.json` beside the store.
//! The flag skipped the delivery events and nothing else, so a measurement run
//! bent the very state (evaluation accrual, watermarks, guard markers) that
//! maintenance reads.

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn run_hook(db: &Path, payload: &serde_json::Value, no_record: bool) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(db).arg("hook");
    if no_record {
        cmd.arg("--no-record");
    }
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    String::from_utf8(out.stdout).unwrap()
}

/// Every file under `dir`, recursively, by relative path, with its full bytes.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn item(id: &str, bindings: Vec<Binding>, severity: Option<Severity>, check: Option<Check>) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("probe rule {id} for the no-record hook run"),
        bindings,
        severity,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("a no-record run turns out to change the store directory".to_string()),
        check,
    }
}

fn command(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Command, value: value.to_string() }
}

/// A store in its own directory (nothing else lives there), and a checkout
/// elsewhere that resolves to a project through its marker file.
fn fixture(tmp: &Path) -> (PathBuf, PathBuf) {
    let store_dir = tmp.join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let db = store_dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    for it in [
        item("always-shown", vec![Binding::Always], None, None),
        item("status-note", vec![command("git status")], None, None),
        item(
            "never-force-push",
            vec![command("git push")],
            Some(Severity::Costly),
            Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        ),
    ] {
        model::store::declare(&mut store, "seed", "seed", "seed", &it).unwrap_or_else(|e| panic!("{}: {e}", it.id));
    }
    drop(store);
    std::fs::write(
        store_dir.join("guard-response-rulebook.json"),
        r#"[{"id":"probe","any_of":["probe-trigger-phrase"],"reminder":"a probe reminder"}]"#,
    )
    .unwrap();
    let work = tmp.join("work").join("probe-project");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join(".thor-project"), "probe-project\n").unwrap();
    let transcript = tmp.join("work").join("transcript.jsonl");
    std::fs::write(&transcript, r#"{"type":"user","message":{"role":"user","content":"run the probe"}}"#).unwrap();
    (db, work)
}

/// The four hook events of one short session, in order.
fn payloads(work: &Path) -> Vec<serde_json::Value> {
    let cwd = work.to_string_lossy().to_string();
    let transcript = work.parent().unwrap().join("transcript.jsonl").to_string_lossy().to_string();
    vec![
        serde_json::json!({"hook_event_name": "SessionStart", "session_id": "s1", "cwd": cwd, "source": "startup"}),
        serde_json::json!({"hook_event_name": "UserPromptSubmit", "session_id": "s1", "cwd": cwd,
            "prompt": "from now on always run the probe before a push"}),
        serde_json::json!({"hook_event_name": "PreToolUse", "session_id": "s1", "cwd": cwd, "tool_name": "Bash",
            "tool_input": {"command": "git status"}}),
        serde_json::json!({"hook_event_name": "PreToolUse", "session_id": "s1", "cwd": cwd, "tool_name": "Bash",
            "tool_input": {"command": "git push --force origin main"}}),
        serde_json::json!({"hook_event_name": "Stop", "session_id": "s1", "cwd": cwd, "stop_hook_active": false,
            "last_assistant_message": "The probe ran.", "transcript_path": transcript}),
    ]
}

/// The whole session against one store, one output per event.
fn session(db: &Path, work: &Path, no_record: bool) -> Vec<String> {
    payloads(work).iter().map(|p| run_hook(db, p, no_record)).collect()
}

#[test]
fn a_no_record_session_leaves_every_file_beside_the_store_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, work) = fixture(tmp.path());
    let store_dir = db.parent().unwrap().to_path_buf();
    let before = snapshot(&store_dir);

    let outs = session(&db, &work, true);
    assert!(outs[0].contains("always-shown"), "fixture sanity: session start shows the pinned rule: {}", outs[0]);
    assert!(outs[2].contains("status-note"), "fixture sanity: the rule on git status fires: {}", outs[2]);
    assert!(outs[3].contains("\"block\""), "fixture sanity: the force push is refused: {}", outs[3]);

    let after = snapshot(&store_dir);
    let changed: Vec<&String> = before.keys().chain(after.keys()).filter(|k| before.get(*k) != after.get(*k)).collect();
    assert!(changed.is_empty(), "--no-record must leave the store directory byte-identical, changed: {changed:?}");
}

/// The control: the same session WITHOUT the flag does change the directory,
/// so the test above could see a change if one happened.
#[test]
fn the_same_session_without_the_flag_does_change_the_store_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, work) = fixture(tmp.path());
    let store_dir = db.parent().unwrap().to_path_buf();
    let before = snapshot(&store_dir);
    session(&db, &work, false);
    assert_ne!(snapshot(&store_dir), before, "a recorded session must leave its trace");
}

/// Same payload, same store contents: the answer is the same either way. Per
/// call on a fresh store, because a later call of a recorded session reads
/// what the earlier ones recorded (a flagged prompt, an accrued hour), and a
/// no-record session by definition recorded nothing for it to read.
#[test]
fn each_no_record_call_answers_exactly_like_a_recorded_one() {
    let normalize = |out: String, root: &Path| -> String {
        out.replace(&root.to_string_lossy().replace('\\', "\\\\"), "<tmp>")
    };
    for i in 0..payloads(Path::new("probe")).len() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let (db_a, work_a) = fixture(a.path());
        let (db_b, work_b) = fixture(b.path());
        let recorded = normalize(run_hook(&db_a, &payloads(&work_a)[i], false), a.path());
        let unrecorded = normalize(run_hook(&db_b, &payloads(&work_b)[i], true), b.path());
        assert_eq!(recorded, unrecorded, "event {i}");
    }
}
