//! `owner_reads_answer: true` on a Stop payload that comes from an agent
//! (it carries `agent_id`) brings back the rules marked `owner_reading_only`
//! for that one answer, and nothing else: the call stays an agent's call, so
//! no memory-upkeep debt of the owner's session is asked of it.
//!
//! WHY (2026-09-26). An agent's answer is normally read by the session that
//! spawned it, so the rules about how an answer reads to the owner (short,
//! a plain summary first, no reflexive disclaimers) step aside for it. A local
//! model's answer is different: the owner reads it himself, in his own chat,
//! with no session in between. Its conductor calls the Stop hook as an agent
//! so the call never counts toward a Claude session's upkeep or judgement
//! debt, and needs a way to say "the owner reads this one".

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const RULEBOOK: &str = r#"[
  {"id":"owner-style","tier":"block","owner_reading_only":true,
   "any_of":["an owner style breach"],"none_of":["any_of"],
   "reminder":"this reads badly for the owner"}
]"#;

const BREACH: &str = "Here is an owner style breach in the answer.";

fn run_hook(db: &Path, payload: &serde_json::Value) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    String::from_utf8(out.stdout).unwrap()
}

/// An empty store with the rulebook beside it, and a working directory that
/// resolves to no project, so no evaluation debt can speak first.
fn fixture(tmp: &Path) -> (PathBuf, PathBuf) {
    let store = tmp.join("store");
    std::fs::create_dir_all(&store).unwrap();
    let db = store.join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(store.join("guard-response-rulebook.json"), RULEBOOK).unwrap();
    let cwd = tmp.join("no-project");
    std::fs::create_dir_all(&cwd).unwrap();
    (db, cwd)
}

fn stop(cwd: &Path, message: &str, agent: bool, owner_reads: Option<bool>, retry: bool) -> serde_json::Value {
    let mut p = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "s1",
        "cwd": cwd.to_string_lossy(),
        "stop_hook_active": retry,
        "last_assistant_message": message,
    });
    if agent {
        p["agent_id"] = serde_json::json!("local-model-1");
        p["agent_type"] = serde_json::json!("local-llm");
    }
    if let Some(flag) = owner_reads {
        p["owner_reads_answer"] = serde_json::json!(flag);
    }
    p
}

fn decision(out: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(out.trim()).ok()?;
    v.get("decision").and_then(|d| d.as_str()).map(str::to_string)
}

/// THE DEFECT THIS PREVENTS: a local model's answer, read by the owner
/// himself, was waved through every rule about how an answer reads to him,
/// because the call had to carry `agent_id` to stay out of his session's debts.
#[test]
fn an_agent_answer_the_owner_reads_is_held_by_an_owner_reading_rule() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, cwd) = fixture(tmp.path());
    let out = run_hook(&db, &stop(&cwd, BREACH, true, Some(true), false));
    assert_eq!(decision(&out).as_deref(), Some("block"), "owner_reads_answer must bring the owner's rules back: {out}");
    assert!(out.contains("reads badly for the owner"), "{out}");
}

/// The retry pass of the same turn reads the answer the same way, as a warning.
#[test]
fn the_retry_pass_of_an_answer_the_owner_reads_still_warns() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, cwd) = fixture(tmp.path());
    let out = run_hook(&db, &stop(&cwd, BREACH, true, Some(true), true));
    assert!(out.contains("reads badly for the owner"), "the retry pass must still say it: {out}");
    assert_eq!(decision(&out), None, "a retry pass never blocks: {out}");
}

/// Unchanged without the field, or with it false: an agent's answer is read by
/// its own session, so the owner's reading rules step aside.
#[test]
fn an_agent_answer_without_the_field_or_with_it_false_is_not_held() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, cwd) = fixture(tmp.path());
    for owner_reads in [None, Some(false)] {
        let out = run_hook(&db, &stop(&cwd, BREACH, true, owner_reads, false));
        assert!(out.trim().is_empty(), "owner_reads_answer {owner_reads:?} must leave the agent exemption alone: {out}");
    }
}

/// The session separation stays: a debt the owner's own session owes (here a
/// decision his prompt flagged for capture) holds his session's Stop, and
/// never an agent's, even one whose answer he reads.
#[test]
fn an_agent_answer_the_owner_reads_is_never_held_for_his_sessions_debts() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, cwd) = fixture(tmp.path());
    let prompt = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "s1",
        "cwd": cwd.to_string_lossy(),
        "prompt": "from now on always run the full test suite before a push",
    });
    run_hook(&db, &prompt);

    let agent = run_hook(&db, &stop(&cwd, "The suite ran.", true, Some(true), false));
    assert!(agent.trim().is_empty(), "an agent must not be held for the owner's session debts: {agent}");

    let owner = run_hook(&db, &stop(&cwd, "The suite ran.", false, None, false));
    assert_eq!(decision(&owner).as_deref(), Some("block"), "fixture sanity: the owner's own session is held: {owner}");
}
