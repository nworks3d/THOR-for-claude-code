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

/// `serve` run with exactly these arguments and the payload on stdin.
fn run_args(args: &[String], payload: serde_json::Value) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.args(args);
    cmd.env_remove("CLAUDE_CODE_ENTRYPOINT").env_remove("CLAUDE_CODE_SESSION_ATTENDED");
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    child.wait_with_output().expect("wait serve");
}

/// The flags `serve::hook_forge` lets through, each before and after `hook`: the run ends before any
/// write (`--no-record`, `--help`, `-h`, `--version`, `-V`), or at least before any evidence
/// (`--diagnose`: no owner record, no evaluation start, the setting-file record where it was).
#[test]
fn each_probe_flag_ends_the_run_before_any_evidence_is_written() {
    for flag in ["--no-record", "--diagnose", "--help", "-h", "--version", "-V"] {
        for before in [false, true] {
            let (dir, db, work) = fixture();
            let store = dir.path().join("store");
            run_hook(&db, &[], start(&work));
            std::fs::write(store.join("stale-guard-mode.json"), r#"{"mode": "off"}"#).unwrap();
            let all_before = files(&store);
            let (db_s, flag_s) = (db.to_string_lossy().to_string(), flag.to_string());
            let args: Vec<String> = if before {
                vec!["--db".into(), db_s, flag_s, "hook".into()]
            } else {
                vec!["--db".into(), db_s, "hook".into(), flag_s]
            };
            let typed = serde_json::json!({ "hook_event_name": "UserPromptSubmit", "session_id": "probe-1", "cwd": work.to_string_lossy(), "prompt": "/thor-eval" });
            for payload in [prompt(&work), start(&work), typed] {
                run_args(&args, payload);
            }
            let after = files(&store);
            assert!(!after.contains_key("owner-prompts.jsonl"), "{flag} (before hook: {before}): no owner record");
            assert_eq!(after["mode-files-seen.json"], all_before["mode-files-seen.json"], "{flag} (before: {before}): the setting-file record did not move");
            if flag != "--diagnose" {
                assert_eq!(after, all_before, "{flag} (before hook: {before}): the store folder is byte-identical");
            }
        }
    }
}

/// `-v` is no flag of `serve`: it is not on the list, so a hand run with it is still refused.
#[test]
fn a_flag_serve_does_not_honour_is_not_a_probe() {
    for command in ["serve.exe --db x hook -v", "serve.exe --db x hook --verbose", "serve.exe --db x hook -H"] {
        let input = serde_json::json!({ "command": command });
        assert!(serve::hook_forge::refusal("Bash", Some(&input)).is_some(), "{command}");
    }
}
