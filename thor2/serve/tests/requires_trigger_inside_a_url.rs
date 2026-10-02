//! A `requires` trigger that sits inside a URL or a path, the way a real
//! command carries it - the live guard and the write-time proof must see it
//! the same way.
//!
//! THE PHANTOM TOOTH THIS PINS DOWN (2026-09-27). A rule bound to `curl -s
//! http://<host>:3002/health` with the trigger "/health" came back wired at
//! write time and could never refuse the one call it was written for: the
//! live guard matched the trigger only as a word of its own, and inside the
//! URL it is glued to the port, while the proof appended the trigger as a
//! separate word and so always found it. Both now use one matcher, and a path
//! separator counts as the boundary it is.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use model::item::{Binding, Check, Item, Kind, TargetKind};

fn health_rule(binding: &str) -> Item {
    Item {
        id: "health-needs-a-start-time".to_string(),
        kind: Kind::Rule,
        text: "a health check after a deploy proves nothing on its own: read the container start time too".to_string(),
        bindings: vec![Binding::Target { kind: TargetKind::Command, value: binding.to_string() }],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the health version only flips once the container really restarted".to_string()),
        check: Some(Check::Requires { when: "/health".to_string(), required: vec!["docker inspect".to_string()] }),
    }
}

fn fixture(dir: &Path, binding: &str) -> PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    model::store::declare(&mut store, "s", "l", "a", &named(&health_rule(binding))).expect("the write gate accepts the rule");
    db
}

fn run_bash(db: &Path, cwd: &Path, command: &str) -> String {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s1",
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Bash",
        "tool_input": {"command": command},
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

fn blocked(out: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(out.trim()).ok().and_then(|v| v.get("decision").cloned())
        == Some(serde_json::json!("block"))
}

#[test]
fn a_trigger_glued_to_the_port_inside_a_url_is_caught_by_the_live_guard() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path(), "curl");
    let out = run_bash(&db, dir.path(), "curl -s http://10.0.0.5:3002/health");
    assert!(blocked(&out), "the real call carries /health inside its URL: {out}");
    let out = run_bash(&db, dir.path(), "curl -s http://10.0.0.5:3002/health -H 'X-Proof: docker inspect app'");
    assert!(!blocked(&out), "the call that carries the start time itself passes: {out}");
    // Step 29: the answer must sit in the command that trips the rule, not in
    // another command of the same line.
    let out = run_bash(&db, dir.path(), "curl -s http://10.0.0.5:3002/health && docker inspect app");
    assert!(blocked(&out), "a different command on the same line is no answer: {out}");
}

#[test]
fn a_longer_endpoint_is_still_not_the_trigger() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path(), "curl");
    let out = run_bash(&db, dir.path(), "curl -s http://10.0.0.5:3002/healthz");
    assert!(!blocked(&out), "/healthz is another endpoint: {out}");
    let out = run_bash(&db, dir.path(), "curl -s http://10.0.0.5:3002/api/orders");
    assert!(!blocked(&out), "an ordinary call on the same binding passes: {out}");
}

/// The write-time proof and the live guard agree: when the proof says wired,
/// the real call the binding itself spells out is refused by the real hook.
#[test]
fn the_proof_says_wired_only_when_the_live_guard_refuses_the_real_call() {
    let binding = "curl -s http://10.0.0.5:3002/health";
    let item = health_rule(binding);
    let reality = serve::prove::Reality::default();
    let boxed = serve::prove::teeth_box(&item.id, &item, None, &reality);
    assert!(matches!(boxed, serve::prove::TeethBox::Wired { .. }), "{boxed:?}");

    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path(), binding);
    let out = run_bash(&db, dir.path(), binding);
    assert!(blocked(&out), "wired must mean the real call is refused: {out}");
}

/// The phantom itself: a binding whose URL ends in a longer endpoint carries
/// the trigger text but never the trigger, so no real call on it can ever be
/// refused - and the proof must say so instead of calling it wired.
#[test]
fn the_proof_never_calls_a_trigger_the_live_guard_cannot_see_wired() {
    let binding = "curl -s http://10.0.0.5:3002/healthz";
    let item = health_rule(binding);
    let reality = serve::prove::Reality::default();
    let boxed = serve::prove::teeth_box(&item.id, &item, None, &reality);
    assert!(!matches!(boxed, serve::prove::TeethBox::Wired { .. }), "no real call on this binding carries /health: {boxed:?}");

    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path(), binding);
    let out = run_bash(&db, dir.path(), binding);
    assert!(!blocked(&out), "{out}");
}

/// `item` with every check literal its text does not name appended to the
/// text (in `U+XXXX` form for a glyph): since step 8 of the two tracks the
/// write gate refuses a check whose literal the rule's own words never name
/// (A4), and these fixtures are about the hook, not about naming.
fn named(item: &model::item::Item) -> model::item::Item {
    let mut out = item.clone();
    let unnamed = model::gate::unnamed_literals(&out);
    if !unnamed.is_empty() {
        let shown: Vec<String> = unnamed.iter().map(|l| format!("'{}'", model::gate::shown_literal(l))).collect();
        out.text = format!("{} (names {})", out.text, shown.join(" "));
    }
    out
}
