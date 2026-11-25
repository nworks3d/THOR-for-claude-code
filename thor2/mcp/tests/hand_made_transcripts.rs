//! A hand-made transcript is not a Claude Code transcript (the new-user run of 2026-11-25).
//!
//! The testers of that run played Claude Code with a script: it wrote each `tool_use` line and
//! each result, ran THOR's PreToolUse hook and threw its answer away. The evaluation hold then
//! came back six times after the note was read (`get`), marked and the report filed, and
//! `doctor` counted the marks as "outside an attested THOR call".
//!
//! That was no fault of THOR. A real session's transcript holds, between the call and its
//! result, the two `attachment` lines Claude Code writes for the hook it ran; the hook's
//! "write attested" marker is in the second one. Without them THOR cannot tell a call Claude
//! Code made from a call a script made, so it attests nothing, and says why. Real transcripts
//! (read on 2026-11-25, session 845e177b of the owner, 84 writes) attest every call of
//! `mcp__thor__...`; the same session's calls with the attachment lines left out do not.
//!
//! These tests keep both halves true: the script-shaped transcript is not attested and the hold
//! stays, the real shape is attested and the hold lifts, and a `get` that is not THOR's answer
//! is no read of a note.

mod common;

use common::*;
use serde_json::json;

use model::item::{Binding, Item, Kind};
use thor_core::event_store::EventStore;

const REPORT: &str = "eval-demo-2026-09-28";
const TLDR: &str = "TLDR: evaluation of demo. Nothing to add.";
const NOTE: &str = "pinned-by-script";

fn pinned_note(s: &Session) {
    let item = Item {
        id: NOTE.to_string(),
        kind: Kind::Rule,
        text: "Always say which folder a command ran in, before and after it.".to_string(),
        bindings: vec![Binding::Always],
        severity: None,
        project: Some("demo".to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("a command ran with no folder named and nobody minded".to_string()),
        check: None,
    };
    seed_raw(&s.db, &item);
    let mut store = EventStore::open_existing(&s.db).unwrap();
    serve::teeth::fixture::decide(&mut store, NOTE);
    serve::deliver::record_delivery(&mut store, "an-earlier-session", "an-earlier-session", "hook", "2026-09-28T00:00:00Z", &[NOTE.to_string()]);
}

/// A call the way the testers' script made it: the `tool_use` line, THOR's hook run and its
/// answer thrown away, the call, and a result whose content is one string. No attachment lines.
fn scripted_call(s: &mut Session, mcp: &mut Mcp, tool: &str, args: serde_json::Value, id: &str) -> String {
    let name = format!("mcp__thor__{tool}");
    s.line(json!({"type": "assistant", "message": {"role": "assistant", "content": [
        {"type": "tool_use", "id": id, "name": name, "input": args}]}}));
    let _ = s.hook(json!({
        "hook_event_name": "PreToolUse", "session_id": s.id, "cwd": s.cwd,
        "transcript_path": s.transcript, "tool_name": name, "tool_input": args, "tool_use_id": id
    }));
    let reply = mcp.call_meta(tool, args, id);
    s.line(json!({"type": "user", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": id, "content": json!({"content": [{"type": "text", "text": reply}]}).to_string()}]}}));
    reply
}

#[test]
fn a_script_made_transcript_attests_nothing_and_the_hold_stays_where_a_real_one_lifts() {
    // The scripted shape: get, mark, stop, file - the hold answers every time.
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    pinned_note(&s);
    let note = s.prompt("/t-eval");
    assert!(note.contains("1b. PINNED NOTES TO RE-READ") && note.contains(NOTE), "{note}");
    let mut mcp = s.mcp();
    scripted_call(&mut s, &mut mcp, "get", json!({"id": NOTE}), "toolu_get");
    let marked = scripted_call(&mut s, &mut mcp, "mark", json!({"id": NOTE}), "toolu_mark");
    assert!(marked.starts_with("marked useful"), "{marked}");
    for round in 0..3 {
        s.stop();
        let held = scripted_call(&mut s, &mut mcp, "remember", report_args(REPORT, "demo", TLDR), &format!("toolu_file{round}"));
        assert!(held.starts_with("REFUSED") && held.contains(&format!("'{NOTE}'")), "round {round}: {held}");
    }
    let marks: Vec<_> = events_of(&s.db, NOTE).into_iter().filter(|e| e.kind == thor_core::event_store::EventKind::ItemMarkedUseful).collect();
    assert_eq!(marks.len(), 1);
    let verdict = s.verdicts(NOTE).get(&marks[0].seq).cloned().expect("the Stop recorded a verdict for the mark");
    assert!(!verdict.attested, "no hook line in the transcript: nothing to attest");
    assert!(verdict.why.contains("no THOR hook marker"), "and it says why: {}", verdict.why);

    // The real shape, the same calls: the hook's attachment lines are there, the mark is
    // attested, the note was read, and the report files.
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    pinned_note(&s);
    assert!(s.prompt("/t-eval").contains(NOTE));
    let mut mcp = s.mcp();
    s.call(&mut mcp, "get", json!({"id": NOTE}), "toolu_get");
    s.call(&mut mcp, "mark", json!({"id": NOTE}), "toolu_mark");
    s.stop();
    let filed = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file");
    assert!(filed.starts_with(&format!("stored '{REPORT}'")), "{filed}");
    let marks: Vec<_> = events_of(&s.db, NOTE).into_iter().filter(|e| e.kind == thor_core::event_store::EventKind::ItemMarkedUseful).collect();
    assert!(s.verdicts(NOTE).get(&marks[0].seq).is_some_and(|v| v.attested));
}
