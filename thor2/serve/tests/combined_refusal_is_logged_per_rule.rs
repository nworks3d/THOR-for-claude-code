//! A refusal that answers several rules in one message is still logged, once
//! per rule (step 31).
//!
//! Field 2026-10-02: an Agent call tripped two `requires` rules and was refused
//! with one combined "answer ALL of them" message. The message opens with a
//! line about the count, not with `[THOR] rule <id>`, so the hook read no rule
//! id out of it and wrote no `gate_refused` event at all: doctor never counted
//! the refusal and the transcript check could never verify it.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn declare_rule(store: &mut thor_core::event_store::EventStore, id: &str, required: &str) {
    let item = model::item::Item {
        id: id.to_string(),
        kind: model::item::Kind::Rule,
        text: format!("An agent brief must say {required}."),
        bindings: vec![model::item::Binding::Target { kind: model::item::TargetKind::Command, value: "Agent".to_string() }],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("a brief without it works fine".to_string()),
        check: Some(model::item::Check::Requires { when: "Agent".to_string(), required: vec![required.to_string()] }),
    };
    model::store::declare(store, "s", "l", "a", &item).expect("the write gate must accept the rule");
}

fn fixture(dir: &Path) -> PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    declare_rule(&mut store, "alpha-clause-rule", "alpha-clause");
    declare_rule(&mut store, "beta-clause-rule", "beta-clause");
    db
}

fn run_agent(db: &Path, cwd: &Path, prompt: &str) -> String {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s1",
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Agent",
        "tool_use_id": "toolu_demo_1",
        "tool_input": {"description": "demo", "prompt": prompt},
    });
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

fn refused_rules(db: &Path) -> Vec<(String, String)> {
    let store = thor_core::event_store::EventStore::new(db).unwrap();
    store
        .events_of_kinds(&[thor_core::event_store::EventKind::GateRefused], 0)
        .unwrap()
        .into_iter()
        .map(|e| (e.entity_id, e.body))
        .collect()
}

#[test]
fn a_combined_refusal_writes_one_gate_refused_per_rule() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    let out = run_agent(&db, dir.path(), "do the thing");
    assert!(out.contains("answer ALL"), "both rules must answer in one message: {out}");
    let refused = refused_rules(&db);
    let mut ids: Vec<&str> = refused.iter().map(|(id, _)| id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["alpha-clause-rule", "beta-clause-rule"], "one event per rule: {refused:?}");
    for (_, body) in &refused {
        assert!(body.contains("toolu_demo_1"), "each event carries the call id: {body}");
    }
}

#[test]
fn a_single_rule_refusal_still_writes_exactly_one() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    let out = run_agent(&db, dir.path(), "do the thing, alpha-clause");
    assert!(out.contains("beta-clause-rule"), "{out}");
    let refused = refused_rules(&db);
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].0, "beta-clause-rule");
}

#[test]
fn rule_ids_are_read_out_of_a_combined_message() {
    let one = "[THOR] rule one-rule applies here: this call trips x.";
    assert_eq!(serve::absent_guard::rule_ids_of(one), ["one-rule"]);
    let two = "[THOR] this call trips 2 rules at once - answer ALL of them in one go, then run it again.\n\n\
               [THOR] rule a-rule applies here: x.\n\n[THOR] rule b-rule applies here: y.";
    assert_eq!(serve::absent_guard::rule_ids_of(two), ["a-rule", "b-rule"]);
    assert!(serve::absent_guard::rule_ids_of("something else").is_empty());
}
