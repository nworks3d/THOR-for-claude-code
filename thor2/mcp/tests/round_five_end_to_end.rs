//! Three changes of the fifth round were only tested on their own, never through the real
//! binaries. Here each runs the way a person's assistant meets it: the tool server (`mcp`) writes
//! the rule, the real `serve hook` refuses a real call, a Stop records the refusal, and the
//! reader answers.
//!
//! - A rule that really refused something is shown as proven by `get`, whatever label it was
//!   stored with.
//! - A reply names at most three real calls that do not count, then how many more there are.
//! - The scan of one session's writes (`self_proof`) runs once per session, however many
//!   refusals of that session are asked about, also when two sessions' refusals alternate.

mod common;

use common::*;
use serde_json::{json, Value};

use thor_core::event_store::EventStore;

/// A rule `no-force` for `git push --force`, written by another session than the one that will
/// be refused (a refusal in the session that wrote the check is never proof).
fn write_the_rule(s: &Session) -> String {
    served(&s.db, OTHER_SESSION, &["git push origin main", "git push"]);
    let mut writer = Mcp::start_with(Start { db: &s.db, cwd: &s.cwd, session: Some(OTHER_SESSION), args: vec![], home: Some(&s.home) });
    let mut args = rule("no-force", "Never run `git push --force` on main.", Some("demo"));
    args["targets"] = json!([{ "kind": "command", "value": "git push" }]);
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["--force"]);
    writer.call("remember", args)
}

/// The session runs `command` through the hook: the call's line in the transcript, the hook's
/// answer, and - when the hook refused - the error result Claude Code records for it.
fn run_forbidden_call(s: &mut Session, id: &str, command: &str) -> Value {
    let input = json!({"command": command});
    s.line(json!({"type": "assistant", "message": {"role": "assistant", "content": [
        {"type": "tool_use", "id": id, "name": "Bash", "input": input}]}}));
    let out = s.hook(json!({
        "hook_event_name": "PreToolUse", "session_id": s.id, "cwd": s.cwd,
        "transcript_path": s.transcript, "tool_name": "Bash", "tool_input": input, "tool_use_id": id
    }));
    let parsed: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("the hook answers JSON: {e}: {out}"));
    let reason = parsed["reason"].as_str().unwrap_or_default().to_string();
    assert_eq!(parsed["decision"], "block", "the rule must refuse {command}: {out}");
    s.line(json!({"type": "user", "message": {"role": "user", "content": [
        {"tool_use_id": id, "type": "tool_result", "is_error": true,
         "content": format!("PreToolUse:Bash hook error: {reason}")}]}}));
    parsed
}

#[test]
fn a_rule_the_real_hook_refused_is_shown_proven_by_get() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    let stored = write_the_rule(&s);
    assert!(stored.starts_with("stored 'no-force'") && stored.contains("teeth: wired, not proven"), "{stored}");
    assert!(get(&mut s.mcp(), "no-force").tags.contains(&"teeth:wired-not-proven".to_string()));

    // The session has other activity (a refusal from a session with none is never counted), the
    // hook refuses a real forbidden call, and the Stop records the refusal against the transcript.
    served(&s.db, &s.id, &["git status"]);
    run_forbidden_call(&mut s, "toolu_force1", "git push --force origin main");
    s.stop();

    let shown = get(&mut s.mcp(), "no-force");
    assert!(shown.tags.contains(&"teeth:proven-guard".to_string()), "proven after a real refusal: {:?}", shown.tags);
    assert!(!shown.tags.contains(&"teeth:wired-not-proven".to_string()), "{:?}", shown.tags);
}

#[test]
fn a_reply_names_three_calls_that_do_not_count_and_how_many_more() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Session::new(tmp.path());
    // The writing session itself ran five different forbidden calls: they are real calls the
    // rule refuses, but made by the writer, so none of them counts as proof against forcing.
    let calls: Vec<String> = (1..=5).map(|i| format!("git push --force origin topic{i}")).collect();
    let triggers: Vec<&str> = calls.iter().map(String::as_str).collect();
    served(&s.db, &s.id, &triggers);
    served(&s.db, OTHER_SESSION, &["git push origin main", "git push"]);
    let mut writer = s.mcp();
    let mut args = rule("no-force", "Never run `git push --force` on main.", Some("demo"));
    args["targets"] = json!([{ "kind": "command", "value": "git push" }]);
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["--force"]);
    // The reply is a refusal here (the check would refuse real calls of other sessions too); what
    // it says about the calls that do not count is the same on a write that lands.
    let reply = writer.call("remember", args);
    let named = reply.matches("was made by the writing session and does not count").count();
    assert_eq!(named, 3, "three calls are named: {reply}");
    assert!(reply.contains("and 2 more real call(s) that do not count either"), "{reply}");
}

#[test]
fn the_scan_of_a_session_runs_once_however_many_of_its_refusals_are_asked() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = Session::new(tmp.path());
    let stored = write_the_rule(&a);
    assert!(stored.starts_with("stored 'no-force'"), "{stored}");
    // Two sessions, each refused three times, their refusals alternating in the log.
    let dir_b = tempfile::tempdir().unwrap();
    let mut b = Session::with_store(dir_b.path(), a.db.clone(), "5c1d3a9e-4b7f-4e2a-9d10-7a6b8c9d0e1f");
    served(&a.db, &a.id, &["git status"]);
    served(&a.db, &b.id, &["git status"]);
    for i in 1..=3 {
        run_forbidden_call(&mut a, &format!("toolu_a{i}"), &format!("git push --force origin a{i}"));
        run_forbidden_call(&mut b, &format!("toolu_b{i}"), &format!("git push --force origin b{i}"));
    }
    a.stop();
    b.stop();

    let store = EventStore::open_existing(&a.db).unwrap();
    let item = model::store::show(&store, "no-force").unwrap();
    let before = model::store::SESSION_EVALUATION_INDEX_BUILDS.load(std::sync::atomic::Ordering::Relaxed);
    let field = serve::attest::field_refusals(&store, "no-force", &item);
    let built = model::store::SESSION_EVALUATION_INDEX_BUILDS.load(std::sync::atomic::Ordering::Relaxed) - before;
    assert_eq!(field.len(), 6, "all six refusals count: {field:?}");
    assert_eq!(built, 2, "one scan for each of the two sessions, not one for every refusal");
}
