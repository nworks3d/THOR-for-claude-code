//! Field 2026-10-01 (step 29): the session-start block was 15.6 KB for 53
//! standing rules. Claude Code keeps a hook output over 10,000 characters in a
//! file and shows the agent only its first 2 KB, while THOR counted all 53
//! rules as shown. The real `serve hook`, run on such a store, must now put at
//! most 9,500 bytes in the block, say how many rules did not fit, and record as
//! served exactly the rules that are in the block.

use model::item::{Binding, Item, Kind, Severity};
use model::store;
use std::io::Write;
use std::process::{Command, Stdio};
use thor_core::event_store::{EventKind, EventStore};

fn standing_rule(i: usize) -> Item {
    Item {
        id: format!("standing-rule-{i:03}"),
        kind: Kind::Rule,
        text: format!("{i:03} {}", "standing words ".repeat(18)),
        bindings: vec![Binding::Always],
        severity: Some(Severity::Costly),
        project: None,
        tags: vec![format!("{}a fixture standing rule without a literal", model::store::NO_LITERAL_REASON_PREFIX)],
        expires: None,
        key: None,
        falsifier: Some("the fixture rule turns out to be wrong".to_string()),
        check: None,
    }
}

fn run_session_start(db: &std::path::Path, cwd: &std::path::Path, session: &str) -> serde_json::Value {
    let payload = serde_json::json!({ "session_id": session, "hook_event_name": "SessionStart", "cwd": cwd.to_string_lossy() });
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("the hook prints one JSON value")
}

#[test]
fn a_big_standing_block_stays_under_the_limit_and_only_what_is_in_it_counts_as_served() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    {
        let mut s = EventStore::new(&db).unwrap();
        for i in 0..53 {
            store::declare(&mut s, "s", "l", "a", &standing_rule(i)).unwrap();
        }
    }
    let cwd = dir.path().join("workdir");
    std::fs::create_dir_all(&cwd).unwrap();

    let json = run_session_start(&db, &cwd, "session-one");
    let block = json["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string();
    assert!(block.len() <= 9_500, "the block is {} bytes: Claude Code would keep it in a file", block.len());
    let in_block: Vec<String> = block
        .lines()
        .filter_map(|l| l.strip_prefix("- ["))
        .filter_map(|l| l.split_once(']').map(|(id, _)| id.to_string()))
        .collect();
    assert!(in_block.len() < 53 && !in_block.is_empty(), "{} rules in the block", in_block.len());
    let hidden = 53 - in_block.len();
    assert!(block.contains(&format!("{hidden} more standing rule(s) did not fit")), "{block}");

    let store = EventStore::open_existing(&db).unwrap();
    let mut served: Vec<String> = store
        .get_all_events()
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EventKind::ItemServed && e.session_id == "session-one")
        .map(|e| e.entity_id)
        .collect();
    served.sort();
    let mut expected = in_block.clone();
    expected.sort();
    assert_eq!(served, expected, "served events must be exactly the rules that are in the block");

    let log = std::fs::read_to_string(dir.path().join("volume-log.jsonl")).expect("the start is in the volume record");
    let line = log.lines().find(|l| l.contains("\"k\":\"start\"")).expect("a start line");
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(v["n"].as_u64().unwrap() as usize, hidden);
    assert_eq!(v["b"].as_u64().unwrap() as usize, block.len());
    assert_eq!(v["i"].as_array().unwrap().len(), in_block.len());
}

#[test]
fn a_small_standing_block_is_unchanged_and_all_of_it_counts_as_served() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    {
        let mut s = EventStore::new(&db).unwrap();
        for i in 0..4 {
            store::declare(&mut s, "s", "l", "a", &standing_rule(i)).unwrap();
        }
    }
    let cwd = dir.path().join("workdir");
    std::fs::create_dir_all(&cwd).unwrap();
    let json = run_session_start(&db, &cwd, "session-two");
    let block = json["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(!block.contains("did not fit"), "{block}");
    assert_eq!(block.lines().filter(|l| l.starts_with("- [")).count(), 4);
    let served = EventStore::open_existing(&db)
        .unwrap()
        .get_all_events()
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EventKind::ItemServed && e.session_id == "session-two")
        .count();
    assert_eq!(served, 4);
}
