//! The write marker (step 12), end to end through the real compiled `serve
//! hook` binary: a THOR write the PreToolUse hook lets through gets one more
//! line of context, "[THOR] write attested: <tool_use_id> <16 hex>", naming
//! the call and the digest of its arguments. Claude Code records that context
//! as a hook attachment of the call (after the tool finished), and the Stop
//! reads it back as proof the write passed the hook
//! (`serve::attest::verify_write`). Any other tool, and a call the hook
//! blocks, never gets it.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use thor_core::event_store::EventStore;

const SESSION: &str = "0de7dc52-1b97-4b66-948f-61e4438043b2";

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

fn pre_tool(cwd: &Path, tool: &str, input: serde_json::Value, tool_use_id: &str) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": SESSION,
        "cwd": cwd.to_string_lossy(),
        "tool_name": tool,
        "tool_input": input,
        "tool_use_id": tool_use_id,
    })
}

fn context(out: &str) -> String {
    if out.trim().is_empty() {
        return String::new();
    }
    let v: serde_json::Value = serde_json::from_str(out).unwrap_or_else(|e| panic!("hook output is JSON: {e}: {out}"));
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

fn marker_of(tool_use_id: &str, input: &serde_json::Value) -> String {
    let digest = model::store::args_digest(input);
    format!("[THOR] write attested: {tool_use_id} {}", &digest[..16])
}

/// A store holding one rule that REFUSES a THOR remember call whose
/// arguments never say `alpha-ok` (a requires check on the tool's own name,
/// read from the call's argument values), so the hook can be seen blocking a
/// THOR write.
fn store(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    let rule = Item {
        id: "remember-says-alpha-ok".to_string(),
        kind: Kind::Rule,
        text: "Every `mcp__thor__remember` call in this fixture says `alpha-ok`.".to_string(),
        bindings: vec![Binding::Target { kind: TargetKind::Command, value: "mcp__thor__remember".to_string() }],
        severity: Some(Severity::Costly),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("A remember call without alpha-ok turns out to be fine here.".to_string()),
        check: Some(Check::Requires { when: "mcp__thor__remember".to_string(), required: vec!["alpha-ok".to_string()] }),
    };
    model::store::declare(&mut store, "s", "l", "a", &rule).unwrap();
    db
}

#[test]
fn marker_only_for_thor_write_tools_and_never_on_a_block() {
    let dir = tempfile::tempdir().unwrap();
    let db = store(dir.path());
    let cwd = dir.path();

    // Every THOR write tool that is let through gets the marker, as the LAST
    // line of its context, with the digest of exactly the input it carries.
    let remember = serde_json::json!({"id": "x", "kind": "rule", "text": "alpha-ok", "falsifier": "f"});
    let writes = [
        ("mcp__thor__remember", remember.clone()),
        ("mcp__thor__revise", serde_json::json!({"id": "x", "text": "u"})),
        ("mcp__thor__retract", serde_json::json!({"id": "x", "reason": "wrong"})),
        ("mcp__thor__mark", serde_json::json!({"id": "x"})),
        ("mcp__thor__pin", serde_json::json!({"id": "x"})),
        ("mcp__thor__unpin", serde_json::json!({"id": "x"})),
        ("mcp__thor__resolve", serde_json::json!({"id": "x", "keep": "a", "discard": ["b"]})),
    ];
    for (i, (tool, input)) in writes.iter().enumerate() {
        let id = format!("toolu_W{i}");
        let ctx = context(&run_hook(&db, &pre_tool(cwd, tool, input.clone(), &id)));
        assert_eq!(ctx.lines().last(), Some(marker_of(&id, input).as_str()), "{tool}: {ctx}");
    }

    // Read tools, the library lane and every other tool never get it.
    for tool in ["mcp__thor__lookup", "mcp__thor__get", "mcp__thor__shelve", "Bash", "Read"] {
        let ctx = context(&run_hook(&db, &pre_tool(cwd, tool, serde_json::json!({"command": "ls", "id": "x"}), "toolu_R")));
        assert!(!ctx.contains("write attested"), "{tool}: {ctx}");
    }

    // A THOR write the hook BLOCKS never gets it: the requires rule refuses
    // a remember that never says alpha-ok.
    let blocked_input = serde_json::json!({"id": "y", "kind": "rule", "text": "t"});
    let out = run_hook(&db, &pre_tool(cwd, "mcp__thor__remember", blocked_input, "toolu_B"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert_eq!(v["decision"], "block", "fixture sanity - the rule must block this call: {out}");
    assert!(!out.contains("write attested"), "a blocked call carries no marker: {out}");

    // And a let-through call with no tool_use id gets none either.
    let mut no_id = pre_tool(cwd, "mcp__thor__remember", remember, "");
    no_id.as_object_mut().unwrap().remove("tool_use_id");
    assert!(!run_hook(&db, &no_id).contains("write attested"));
}
