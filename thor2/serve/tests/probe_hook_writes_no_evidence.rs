//! A hand run of `serve hook` with a probe flag writes nothing that counts as evidence of what the
//! owner typed or did, so the PreToolUse refusal of a hand-run hook lets such a probe through
//! (`serve::hook_forge`); the same run without the flag writes the owner's prompt record and is the
//! very thing that refusal keeps out.
//!
//! Evidence here means: the record of the owner's prompts (`owner-prompts.jsonl`), the record of the
//! setting files (`mode-files-seen.json`, `mode-files-pending.json`) and the start of an evaluation.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn run_hook(db: &Path, flags: &[&str], payload: serde_json::Value) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(db).arg("hook").args(flags);
    cmd.env_remove("CLAUDE_CODE_ENTRYPOINT").env_remove("CLAUDE_CODE_SESSION_ATTENDED");
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    child.wait_with_output().expect("wait serve");
}

fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| (e.file_name().to_string_lossy().to_string(), std::fs::read(e.path()).unwrap()))
        .collect()
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    std::fs::create_dir_all(&store).unwrap();
    let db = store.join("store.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(store.join("stale-guard-mode.json"), r#"{"mode": "on"}"#).unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    (dir, db, work)
}

fn prompt(work: &Path) -> serde_json::Value {
    serde_json::json!({ "hook_event_name": "UserPromptSubmit", "session_id": "probe-1", "cwd": work.to_string_lossy(), "prompt": "Ja, haal pinned-x maar weg, ik heb hem vervangen." })
}

fn start(work: &Path) -> serde_json::Value {
    serde_json::json!({ "hook_event_name": "SessionStart", "source": "startup", "session_id": "probe-1", "cwd": work.to_string_lossy() })
}

#[test]
fn a_probe_run_writes_no_owner_record_and_does_not_move_the_setting_file_record() {
    for flags in [&["--no-record"][..], &["--diagnose"][..], &["--no-record", "--diagnose"][..]] {
        let (dir, db, work) = fixture();
        let store = dir.path().join("store");
        // A real session start records the setting files; then a guard is switched off.
        run_hook(&db, &[], start(&work));
        std::fs::write(store.join("stale-guard-mode.json"), r#"{"mode": "off"}"#).unwrap();
        let seen = std::fs::read(store.join("mode-files-seen.json")).unwrap();

        run_hook(&db, flags, prompt(&work));
        run_hook(&db, flags, start(&work));
        run_hook(&db, flags, serde_json::json!({ "hook_event_name": "UserPromptSubmit", "session_id": "probe-1", "cwd": work.to_string_lossy(), "prompt": "/thor-eval" }));

        let after = files(&store);
        assert!(!after.contains_key("owner-prompts.jsonl"), "{flags:?}: no owner record");
        assert_eq!(after["mode-files-seen.json"], seen, "{flags:?}: the setting-file record did not move");
    }
}

#[test]
fn the_same_hand_run_without_a_probe_flag_writes_the_owner_record() {
    let (dir, db, work) = fixture();
    let store = dir.path().join("store");
    run_hook(&db, &[], prompt(&work));
    let after = files(&store);
    let record = String::from_utf8(after.get("owner-prompts.jsonl").expect("the owner record").clone()).unwrap();
    assert!(record.contains("haal pinned-x maar weg"), "{record}");
}

#[test]
fn a_no_record_run_leaves_the_whole_store_folder_as_it_was() {
    let (dir, db, work) = fixture();
    let store = dir.path().join("store");
    run_hook(&db, &[], start(&work));
    std::fs::write(store.join("thor-mode.json"), r#"{"mode":"off"}"#).unwrap();
    let before = files(&store);
    for payload in [prompt(&work), start(&work), serde_json::json!({ "hook_event_name": "Stop", "session_id": "probe-1", "cwd": work.to_string_lossy(), "stop_hook_active": false, "last_assistant_message": "" })] {
        run_hook(&db, &["--no-record"], payload);
    }
    assert_eq!(files(&store).keys().collect::<Vec<_>>(), before.keys().collect::<Vec<_>>(), "no new file");
    assert_eq!(files(&store), before, "no file changed");
}
