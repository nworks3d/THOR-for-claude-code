//! The evaluation, end to end through the real compiled binaries: the `mcp`
//! tool server a session writes through, and the `serve` hook beside it that
//! Claude Code runs before each call and at each Stop. Every line of the
//! session's transcript is written here in the order Claude Code writes it
//! (step 1 of the build, answers 5 and 6): the tool_use line first, then the
//! call (carrying `_meta`), then the hook's attachments, then the result
//! (`common::Session`, the shared attested fixture).
//!
//! Step 12 - session identity for every write: each write carries the call's
//! stamp (`att`), each reply names its event seq, and the session's Stop
//! records, once per write, whether it was an attested THOR call.
//!
//! Step 15 - filing only through an attested THOR call, and THOR writes the
//! report's first part itself: a filing that is not a THOR tool call Claude
//! Code made is refused and writes nothing; one whose call line is not on
//! disk yet is stored and left to the Stop; a write to a frozen item from a
//! side door voids the evaluation; the stored report opens with the machine
//! section, which no revise can edit.
//!
//! Step 19 - the final gate: every EV scenario and every attack scenario of
//! the build is here (the model CLI, a report past the gate, an edited debt
//! sidecar, an answer-guard tag, a hand-fed refusal, a shell `serve mark`,
//! a missing or unreadable transcript, a machine pass cut off by a kill).

mod common;

use common::*;
use serde_json::{json, Value};

use model::item::{Binding, Item, Kind, Severity, TargetKind};
use thor_core::event_store::{EventKind, EventStore};

fn note(id: &str) -> Value {
    let mut args = rule(id, "Keep commit subjects under 60 characters.", Some("demo"));
    args["moments"] = json!(["commit"]);
    args
}

/// G12.1 in the real binaries: the stored event carries the call's tool_use
/// id, the session column is the session, the writer saw its line, and the
/// Stop records it attested - once.
#[test]
fn write_stamps_att_from_meta() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    let mut mcp = s.mcp();
    let reply = s.call(&mut mcp, "remember", note("subjects-short"), "toolu_e2e_remember");
    assert!(reply.starts_with("stored 'subjects-short'"), "{reply}");
    let event = last_of(&s.db, "subjects-short", EventKind::FactCreated);
    assert_eq!(event.session_id, WRITING_SESSION, "the session column is the server's session hint");
    let att = model::store::att_of(&event.body).expect("the write carries att");
    assert_eq!(att.tool_use_id.as_deref(), Some("toolu_e2e_remember"));
    assert_eq!(att.session_hint.as_deref(), Some(WRITING_SESSION));
    assert_eq!(att.inflight.as_deref(), Some(model::store::INFLIGHT_SEEN), "{att:?}");
    assert_eq!(att.args_sha256, model::store::args_digest(&note("subjects-short")));
    assert_eq!(named_seq(&reply), event.seq);

    assert!(s.verdicts("subjects-short").is_empty(), "nothing is verified before the Stop");
    s.stop();
    let verdict = s.verdicts("subjects-short").remove(&event.seq).expect("the Stop recorded it");
    assert!(verdict.attested && verdict.session == WRITING_SESSION && verdict.agent_id.is_none(), "{verdict:?}");
    s.stop();
    let by_attester = events_of(&s.db, "subjects-short").iter().filter(|e| e.actor == "attester").count();
    assert_eq!(by_attester, 1, "recorded once");
}

/// R12.1 in the real binaries: a second client sends no `_meta`. The write
/// still lands (nothing refuses it), stamped as a side door, and the Stop
/// records it unattested.
#[test]
fn write_without_meta_is_unattested() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Session::new(tmp.path());
    let mut mcp = s.mcp();
    let reply = mcp.call("remember", note("side-door-note"));
    assert!(reply.starts_with("stored 'side-door-note'"), "{reply}");
    let event = last_of(&s.db, "side-door-note", EventKind::FactCreated);
    let att = model::store::att_of(&event.body).expect("the write carries att");
    assert_eq!(att.tool_use_id, None);
    assert_eq!(att.inflight.as_deref(), Some(model::store::INFLIGHT_SIDE_DOOR));
    assert_eq!(att.side_door.as_deref(), Some("no toolUseId in the call's _meta"));
    s.stop();
    let verdict = s.verdicts("side-door-note").remove(&event.seq).expect("the Stop recorded it");
    assert!(!verdict.attested, "{verdict:?}");
    assert_eq!(verdict.why, "no toolUseId in the call's _meta");
}

/// Every write tool's success reply names the seq of the event it wrote -
/// the one thing that tells a replayed real tool call id apart at the Stop.
#[test]
fn every_write_reply_names_its_event_seq() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    let mut mcp = s.mcp();
    let id = "subjects-short";

    let reply = s.call(&mut mcp, "remember", note(id), "toolu_q1");
    assert_eq!(named_seq(&reply), last_of(&s.db, id, EventKind::FactCreated).seq, "remember: {reply}");
    let reply = s.call(&mut mcp, "revise", json!({"id": id, "text": "Keep commit subjects under 50 characters."}), "toolu_q2");
    assert_eq!(named_seq(&reply), last_of(&s.db, id, EventKind::FactRevised).seq, "revise: {reply}");
    let reply = s.call(&mut mcp, "pin", json!({"id": id}), "toolu_q3");
    assert_eq!(named_seq(&reply), last_of(&s.db, id, EventKind::FactRevised).seq, "pin: {reply}");
    let reply = s.call(&mut mcp, "unpin", json!({"id": id}), "toolu_q4");
    assert_eq!(named_seq(&reply), last_of(&s.db, id, EventKind::FactRevised).seq, "unpin: {reply}");
    {
        let mut store = EventStore::open_existing(&s.db).unwrap();
        serve::deliver::record_delivery(&mut store, WRITING_SESSION, WRITING_SESSION, "hook", "2026-09-28T00:00:00Z", &[id.to_string()]);
    }
    let reply = s.call(&mut mcp, "mark", json!({"id": id}), "toolu_q5");
    assert!(reply.starts_with("marked useful: "), "{reply}");
    assert_eq!(named_seq(&reply), last_of(&s.db, id, EventKind::ItemMarkedUseful).seq, "mark: {reply}");

    // resolve: two heads written apart, the way two machines revise one fact.
    let div = "diverged-note";
    s.call(&mut mcp, "remember", note(div), "toolu_q6");
    let created = last_of(&s.db, div, EventKind::FactCreated);
    {
        let mut store = EventStore::open_existing(&s.db).unwrap();
        for text in ["Keep commit subjects under 50 characters.", "Keep commit subjects under 70 characters."] {
            let mut item: Item = serde_json::from_str(&created.body).unwrap();
            assert_eq!(item.kind, Kind::Rule);
            item.text = text.to_string();
            let body = model::store::canonical_body(&item).unwrap();
            store.append_event("elsewhere", "elsewhere", "machine-b", EventKind::FactRevised, div, Some(&created.this_hash), &body).unwrap();
        }
    }
    let heads: Vec<String> = events_of(&s.db, div).iter().filter(|e| e.kind == EventKind::FactRevised).map(|e| e.this_hash.clone()).collect();
    assert_eq!(heads.len(), 2);
    let reply = s.call(&mut mcp, "resolve", json!({"id": div, "keep": heads[0], "discard": [heads[1]]}), "toolu_q7");
    assert!(reply.starts_with("resolved "), "{reply}");
    assert_eq!(named_seq(&reply), events_of(&s.db, div).last().unwrap().seq, "resolve: {reply}");

    let reply = s.call(&mut mcp, "retract", json!({"id": id, "reason": "Folded into the commit message template."}), "toolu_q8");
    assert!(reply.starts_with("retracted "), "{reply}");
    assert_eq!(named_seq(&reply), last_of(&s.db, id, EventKind::FactRetracted).seq, "retract: {reply}");

    // And the Stop attests every one of them, the resolve included.
    s.stop();
    let verdicts = s.verdicts(id);
    assert_eq!(verdicts.len(), 6, "remember, revise, pin, unpin, mark, retract: {verdicts:?}");
    assert!(verdicts.values().all(|v| v.attested), "{verdicts:?}");
    let resolved = s.verdicts(div);
    assert_eq!(resolved.len(), 2, "remember and resolve (the two heads were written by another machine): {resolved:?}");
    assert!(resolved.values().all(|v| v.attested), "{resolved:?}");
}

// ---------------------------------------------------------------- step 15

const REPORT: &str = "eval-demo-2026-09-28";
const TLDR: &str = "TLDR: evaluation of demo. Nothing to add.";

/// A light rule of demo, decided by THOR's trial (a note), that fired
/// `times` times in `session` - owed a verdict at the next start once it
/// fired 40 times there.
fn fired_rule(db: &std::path::Path, id: &str, session: &str, times: usize) {
    let item = Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("Run the fixture check {id} before a commit."),
        bindings: vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-cmd-{id}") }],
        severity: None,
        project: Some("demo".to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the fixture check turns out to be useless".to_string()),
        check: None,
    };
    seed_raw(db, &item);
    let mut store = EventStore::open_existing(db).unwrap();
    serve::teeth::fixture::decide(&mut store, id);
    for _ in 0..times {
        serve::deliver::record_delivery(&mut store, session, session, "hook", "2026-09-28T00:00:00Z", &[id.to_string()]);
    }
}

/// The owner types the evaluation command: an evaluation of demo starts.
fn start_evaluation(s: &mut Session) -> model::store::EvaluationStart {
    let note = s.prompt("/t-eval");
    assert!(note.contains("The evaluation of 'demo' started at log position"), "{note}");
    s.open_start("demo").expect("the start record is on the log")
}

/// R15.1 (EV Z1 a): a second JSON-RPC client - no Claude Code, no hook, no
/// transcript - files the report. Measured before step 15: "stored
/// 'eval-fxp-2026-09-28' (Report, event seq 68) judgement: no hook record
/// says which session filed this report, so no verdict was required here".
/// Now: refused with the one sentence, nothing written, whether the client
/// sends no `_meta` or runs with no session at all.
#[test]
fn side_door_client_filing_is_refused_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    start_evaluation(&mut s);
    let before = all_events(&s.db);

    let mut mcp = s.mcp();
    let reply = mcp.call("remember", report_args(REPORT, "demo", TLDR));
    assert!(
        reply.starts_with(
            "REFUSED: this evaluation report is not coming through a THOR tool call that both Claude Code and THOR's hook \
             saw (no toolUseId in the call's _meta), so it could never count - the Stop hook only accepts a report whose \
             filing call is in this session's own transcript. Nothing was written. File it with the THOR tool itself"
        ),
        "{reply}"
    );
    assert!(reply.contains("query \"select:mcp__thor__remember,mcp__thor__revise,mcp__thor__mark\""), "{reply}");

    let mut stranger = Mcp::start(&s.db, &s.cwd);
    let reply = stranger.call_meta("remember", report_args(REPORT, "demo", TLDR), "toolu_stranger");
    assert!(reply.contains("(no CLAUDE_CODE_SESSION_ID in the server's environment)"), "{reply}");

    assert_eq!(all_events(&s.db), before, "nothing written, by either door");
    assert!(mcp.call("get", json!({"id": REPORT})).contains("no item"), "get finds nothing");
}

/// G15.1 (K21, F2): the filing call's own tool_use line reaches the
/// transcript only after the call. The writer sees it unseen, never refuses
/// for that and never waits: the report is stored, naming the session, and
/// the Stop - the reader - finds every line on disk and records it attested.
#[test]
fn an_unseen_filing_call_is_stored_and_left_to_the_reader() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    start_evaluation(&mut s);
    let mut mcp = s.mcp();
    let reply = s.call_unseen(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_fileb1");
    assert!(reply.starts_with(&format!("stored '{REPORT}' (Report, event seq ")), "{reply}");
    let event = last_of(&s.db, REPORT, EventKind::FactCreated);
    assert_eq!(named_seq(&reply), event.seq);
    assert_eq!(event.session_id, WRITING_SESSION, "filed in the evaluating session");
    let att = model::store::att_of(&event.body).unwrap();
    assert_eq!(att.inflight.as_deref(), Some(model::store::INFLIGHT_UNSEEN), "{att:?}");
    assert!(s.verdicts(REPORT).is_empty(), "not decided by the writer");
    s.stop();
    let verdict = s.verdicts(REPORT).remove(&event.seq).expect("the Stop decides it");
    assert!(verdict.attested && verdict.session == WRITING_SESSION, "{verdict:?}");
}

/// R15.8 (K21, F2): a side-door client sends the filing with a `_meta`
/// carrying the id of a REAL Bash call of the session. The transcript holds
/// that id under another tool: positive evidence of a side door. Refused,
/// naming it, and nothing written.
#[test]
fn a_bash_tool_use_id_cannot_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    start_evaluation(&mut s);
    s.bash_call("toolu_bash1", "git status", "nothing to commit, working tree clean");
    let before = all_events(&s.db);
    let mut mcp = s.mcp();
    let reply = mcp.call_meta("remember", report_args(REPORT, "demo", TLDR), "toolu_bash1");
    assert!(reply.starts_with("REFUSED: this evaluation report is not coming through a THOR tool call"), "{reply}");
    assert!(reply.contains("(toolu_bash1 is a Bash call, not mcp__thor__remember)"), "{reply}");
    assert_eq!(all_events(&s.db), before, "nothing written");
}

/// R15.7 (D10): after the start, an item on the frozen list is changed
/// behind the evaluation. From a side-door client it is refused on the spot
/// and nothing is written; written past the tool server (a shell writing the
/// store directly), the next attested filing is refused naming the item, the
/// seq and the door, the start record is closed as void, and the next start
/// writes a new start with a new list.
#[test]
fn a_side_door_write_to_a_frozen_item_voids_the_evaluation() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    fired_rule(&s.db, "listed-a", WRITING_SESSION, 40);
    fired_rule(&s.db, "listed-b", WRITING_SESSION, 40);
    let start = start_evaluation(&mut s);
    assert_eq!(start.frozen.judgement, vec!["listed-a".to_string(), "listed-b".to_string()]);

    // A side-door client: refused on the spot, nothing written.
    let mut mcp = s.mcp();
    let before = events_of(&s.db, "listed-a").len();
    let reply = mcp.call("retract", json!({"id": "listed-a", "reason": "fixture-cmd-listed-a is gone"}));
    assert!(
        reply.starts_with(&format!(
            "REFUSED: 'listed-a' is on the frozen lists of an open evaluation of 'demo' ('{}', session {WRITING_SESSION}), and \
             this write is coming from outside an attested THOR call (no toolUseId in the call's _meta)",
            start.id
        )),
        "{reply}"
    );
    assert!(reply.contains("Make this change with the THOR tool itself"), "{reply}");
    assert_eq!(events_of(&s.db, "listed-a").len(), before, "nothing written");
    assert!(s.open_start("demo").is_some(), "the evaluation stands");

    // A shell writing the store past the tool server.
    let shell = {
        let mut store = EventStore::open_existing(&s.db).unwrap();
        model::store::retract(&mut store, "shell", "shell", "cli", "listed-a", "fixture-cmd-listed-a is gone", None).unwrap()
    };
    let reply = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file1");
    assert!(
        reply.starts_with(&format!(
            "REFUSED: 'listed-a' was changed at log seq {} outside an attested THOR call of this evaluation (written by 'cli' \
             with no stamp of THOR's tool server",
            shell.seq
        )),
        "{reply}"
    );
    assert!(
        reply.contains("An evaluation whose list was changed behind it cannot count. The next Stop starts a fresh evaluation with a fresh list."),
        "{reply}"
    );
    assert!(mcp.call("get", json!({"id": REPORT})).contains("no item"), "nothing filed");
    let record = events_of(&s.db, &start.id);
    assert_eq!(record.len(), 3, "the start, its batch record, the void mark");
    assert_eq!((record[2].kind, record[2].actor.as_str()), (EventKind::FactRevised, "eval-machine"));
    let void = model::store::frozen_of(&record[2].body).and_then(|f| f.void).expect("marked void");
    assert_eq!((void.id.as_str(), void.seq), ("listed-a", shell.seq));
    assert_eq!(s.open_start("demo"), None, "no evaluation is open any more");
    let again = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file2");
    assert!(again.contains("no evaluation of 'demo' is open in this session") && again.contains("is void: 'listed-a'"), "{again}");

    // The next start: a new record, a new list.
    let fresh = start_evaluation(&mut s);
    assert_ne!(fresh.id, start.id);
    assert_eq!(fresh.frozen.judgement, vec!["listed-b".to_string()], "the retracted item is gone from the list");
}

/// G15.4 (D10): THOR's own writes in the window never void the evaluation.
/// Two old rules with no verdict fired here 40 times: they are on the frozen
/// judgement list, and THOR's machine pass decides both right after the
/// start record - two writes by `teeth-eval` inside the window. The
/// session's verdicts on them are recorded by the `attester` at its Stop.
/// None of it is a side door: the report files.
#[test]
fn the_machine_s_own_writes_in_the_window_never_void_the_evaluation() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    for id in ["undecided-a", "undecided-b"] {
        let item = Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("Run the fixture check {id} before a commit."),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-cmd-{id}") }],
            severity: None,
            project: Some("demo".to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("the fixture check turns out to be useless".to_string()),
            check: None,
        };
        seed_raw(&s.db, &item);
        let mut store = EventStore::open_existing(&s.db).unwrap();
        for _ in 0..40 {
            serve::deliver::record_delivery(&mut store, WRITING_SESSION, WRITING_SESSION, "hook", "2026-09-28T00:00:00Z", &[id.to_string()]);
        }
    }
    let start = start_evaluation(&mut s);
    assert_eq!(start.frozen.judgement, vec!["undecided-a".to_string(), "undecided-b".to_string()]);
    let by_machine: Vec<(String, i64)> = ["undecided-a", "undecided-b"]
        .iter()
        .flat_map(|id| events_of(&s.db, id))
        .filter(|e| e.seq > start.seq && e.actor == "teeth-eval")
        .map(|e| (e.entity_id, e.seq))
        .collect();
    assert_eq!(by_machine.len(), 2, "the machine pass wrote both inside the window: {by_machine:?}");

    let mut mcp = s.mcp();
    assert!(s.call(&mut mcp, "mark", json!({"id": "undecided-a"}), "toolu_mark_a").starts_with("marked useful"));
    assert!(s.call(&mut mcp, "mark", json!({"id": "undecided-b", "noise": true}), "toolu_mark_b").starts_with("marked noise"));
    s.stop();
    let store = EventStore::open_existing(&s.db).unwrap();
    assert_eq!(model::store::frozen_writes_in_window(&store, &start, None), model::store::FrozenWrites::default());
    drop(store);
    let reply = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file_g154");
    assert!(reply.starts_with(&format!("stored '{REPORT}'")), "THOR's own writes void nothing: {reply}");
    assert!(reply.contains("held 2 item(s): 1 useful, 1 noise, 0 skipped"), "{reply}");
}

/// B1: the machine section is derived from the LOG alone. The filing's
/// section equals `render_machine_section` of its start at F byte for byte,
/// and deleting the session's transcripts afterwards - or appending lines to
/// them - changes nothing about it.
#[test]
fn the_machine_section_never_reads_a_live_transcript() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    fired_rule(&s.db, "judged-here", WRITING_SESSION, 40);
    let start = start_evaluation(&mut s);
    let mut mcp = s.mcp();
    let reply = s.call(&mut mcp, "mark", json!({"id": "judged-here"}), "toolu_mark1");
    assert!(reply.starts_with("marked useful"), "{reply}");
    s.stop();
    let reply = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file1");
    assert!(reply.starts_with("stored"), "{reply}");
    let stored = get(&mut mcp, REPORT).text;
    let (n, f) = model::gate::machine_section_range(&stored).expect("a machine section");
    assert_eq!(n, start.frozen.started_seq);
    let block = model::gate::machine_block(&stored).unwrap();
    let expected = |db: &std::path::Path| {
        let store = EventStore::open_existing(db).unwrap();
        format!("{}\n\n{}\n", model::store::render_machine_section(&store, &start, f), model::gate::AGENT_SECTION_BEGIN)
    };
    assert_eq!(expected(&s.db), block, "the filing's section is the log's rendering");
    assert!(block.contains("- 'judged-here': useful - verdict at log seq"), "{block}");

    s.line(json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "a later line"}]}}));
    assert_eq!(expected(&s.db), block, "a transcript line changes nothing");
    std::fs::remove_dir_all(&s.home).unwrap();
    assert_eq!(expected(&s.db), block, "no transcript at all changes nothing");
}

/// R15.5 and G15.2 (EV B1): the stored report opens with the machine
/// section; a revise of that section is refused; a revise that only fixes
/// the TLDR goes through, with the section kept.
#[test]
fn stored_report_starts_with_the_machine_section() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    let start = start_evaluation(&mut s);
    let mut mcp = s.mcp();
    let reply = s.call(&mut mcp, "remember", report_args(REPORT, "demo", "TLDR: evaluaton of demo. Nothing to add."), "toolu_file1");
    assert!(reply.starts_with(&format!("stored '{REPORT}' (Report, event seq ")), "{reply}");
    assert!(reply.contains("machine section: THOR wrote it above your text, from its log"), "{reply}");
    let seq = named_seq(&reply);
    let stored = get(&mut mcp, REPORT).text;
    assert!(
        stored.starts_with(&format!(
            "=== MACHINE SECTION: derived from the log, seq {} to {}; it cannot be edited ===\n",
            start.frozen.started_seq,
            seq - 1
        )),
        "{stored}"
    );
    assert!(stored.contains(&format!("EVALUATION: project 'demo'; session {WRITING_SESSION}")), "{stored}");
    assert!(stored.ends_with(&format!("\n\n{}\nTLDR: evaluaton of demo. Nothing to add.", model::gate::AGENT_SECTION_BEGIN)), "{stored}");
    s.stop();

    // The section edited: refused, and nothing changes.
    let edited = stored.replacen("machine section", "machine sectiom", 1);
    assert_ne!(edited, stored, "fixture sanity: the section carries the words");
    let reply = s.call(&mut mcp, "revise", json!({"id": REPORT, "text": edited}), "toolu_rev1");
    assert!(reply.starts_with("REFUSED: the machine section of an evaluation report cannot be edited"), "{reply}");
    assert_eq!(get(&mut mcp, REPORT).text, stored);

    // Only the TLDR: through, the section kept byte for byte - by `text`
    // alone, or by a replace below the header.
    let reply = s.call(&mut mcp, "revise", json!({"id": REPORT, "text": "TLDR: evaluation of demo. Nothing to add."}), "toolu_rev2");
    assert!(reply.starts_with(&format!("revised '{REPORT}'")), "{reply}");
    let fixed = get(&mut mcp, REPORT).text;
    assert_eq!(model::gate::machine_block(&fixed), model::gate::machine_block(&stored));
    assert!(fixed.ends_with("\nTLDR: evaluation of demo. Nothing to add."), "{fixed}");
    let reply = s.call(&mut mcp, "revise", json!({"id": REPORT, "replace_from": "Nothing to add.", "replace_to": "Nothing more to add."}), "toolu_rev3");
    assert!(reply.starts_with(&format!("revised '{REPORT}'")), "{reply}");
    assert_eq!(model::gate::machine_block(&get(&mut mcp, REPORT).text), model::gate::machine_block(&stored));
}

/// R15.5, second half: an agent text carrying either section header is
/// refused at the filing - only THOR writes them - and nothing is stored.
#[test]
fn agent_text_with_the_header_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    start_evaluation(&mut s);
    let mut mcp = s.mcp();
    for (n, text) in [
        "=== MACHINE SECTION: derived from the log, seq 1 to 2; it cannot be edited ===\nEverything settled.",
        "TLDR.\n=== TLDR AND REMARKS: written by the evaluating session; they settle nothing ===\nMore.",
    ]
    .iter()
    .enumerate()
    {
        let reply = s.call(&mut mcp, "remember", report_args(REPORT, "demo", text), &format!("toolu_forged{n}"));
        assert!(reply.starts_with("REFUSED: the text of this evaluation report carries a section header only THOR writes"), "{reply}");
        assert!(reply.contains("send only your own TLDR and remarks"), "{reply}");
    }
    assert!(mcp.call("get", json!({"id": REPORT})).contains("no item"), "nothing stored");
    let reply = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file_ok");
    assert!(reply.starts_with("stored"), "without the header it files: {reply}");
}

/// R15.3 (EV B2 a, completed - the SCR scenario z7-handover-sidecar): a
/// planted handover sidecar pointing the evaluation at another session,
/// between the hook run and the tool call, changes nothing - the verdict
/// owed on the frozen list is still demanded - and only an attested verdict
/// of the session, checked at its Stop, lets the report file.
#[test]
fn a_planted_handover_sidecar_waives_no_verdict() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    fired_rule(&s.db, "fx-rule", WRITING_SESSION, 41);
    let start = start_evaluation(&mut s);
    assert_eq!(start.frozen.judgement, vec!["fx-rule".to_string()]);
    let mut mcp = s.mcp();
    let first = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file1");
    assert!(first.starts_with("REFUSED: this evaluation report cannot be filed yet: 1 item(s)") && first.contains("'fx-rule'"), "{first}");

    let sidecar = serve::eval_session::retired_handover_path(&s.db);
    let planted = json!({
        "evaluations": [{"session_id": "5e1f0c3a-0000-4000-8000-000000000002", "project": "demo", "started_seq": 1, "started_unix": serve::time::now_unix(), "started_by": "skill"}],
        "filings": [{"report_id": REPORT, "session_id": "5e1f0c3a-0000-4000-8000-000000000002", "project": "demo", "at_unix": serve::time::now_unix()}],
    });
    std::fs::write(&sidecar, planted.to_string()).unwrap();
    let second = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file2");
    assert!(second.starts_with("REFUSED: this evaluation report cannot be filed yet: 1 item(s)") && second.contains("'fx-rule'"), "{second}");

    let reply = s.call(&mut mcp, "mark", json!({"id": "fx-rule"}), "toolu_mark1");
    assert!(reply.starts_with("marked useful"), "{reply}");
    let waiting = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file3");
    assert!(waiting.contains(model::store::TURN_NOT_ENDED), "a verdict of this turn is checked at its Stop: {waiting}");
    s.stop();
    let filed = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file4");
    assert!(filed.starts_with("stored"), "{filed}");
    assert!(filed.contains("held 1 item(s): 1 useful, 0 noise, 0 skipped"), "{filed}");
}

/// R15.4 and G15.3 (EV B2 b, completed - the SCR scenario b2-frozen-batch):
/// ten old rules that need the agent, the batch frozen at the start holds
/// the first eight. A rule on it retracted mid-evaluation settles it once the
/// Stop has checked that call; every refusal names only frozen ids, never
/// the two left for later; and after the report files, the next start leads
/// its batch with those two.
#[test]
fn the_filing_is_held_to_the_batch_frozen_at_its_start() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    let ids: Vec<String> = (1..=10).map(|n| format!("fx-heavy-{n:02}")).collect();
    for (n, id) in ids.iter().enumerate() {
        let tool = format!("acme-tool-{:02}", n + 1);
        seed_raw(
            &s.db,
            &Item {
                id: id.clone(),
                kind: Kind::Rule,
                text: format!("Never run `{tool} --wipe` on ledger {}.", n + 1),
                bindings: vec![Binding::Target { kind: TargetKind::Command, value: tool.clone() }],
                severity: Some(Severity::Costly),
                project: Some("demo".to_string()),
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("wiping ledger {} turns out harmless", n + 1)),
                check: None,
            },
        );
    }
    let start = start_evaluation(&mut s);
    assert_eq!(start.frozen.batch(), &ids[..8], "{:?}", start.frozen.machine);
    assert_eq!(start.frozen.batch_later, 2);
    let names = |text: &str| -> Vec<String> { ids.iter().filter(|id| text.contains(&format!("'{id}'"))).cloned().collect() };

    let mut mcp = s.mcp();
    let reply = s.call(&mut mcp, "retract", json!({"id": "fx-heavy-01", "reason": "acme-tool-01 was replaced by a database table"}), "toolu_ret1");
    assert!(reply.starts_with("retracted fx-heavy-01"), "{reply}");
    let refused = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file1");
    assert!(refused.starts_with("REFUSED"), "{refused}");
    assert!(refused.contains(model::store::TURN_NOT_ENDED), "the retract of this turn waits for its Stop: {refused}");
    assert_eq!(names(&refused), ids[..8].to_vec(), "only frozen ids: {refused}");

    s.stop();
    let refused = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file2");
    assert_eq!(names(&refused), ids[1..8].to_vec(), "the attested retract settled it, and nobody new: {refused}");
    assert!(refused.contains("2 more rule(s) in the same state are not this report's to settle"), "{refused}");

    // The fix each one names: hang the check its derived tooth asks for.
    for (n, id) in ids[1..8].iter().enumerate() {
        let reply = s.call(&mut mcp, "revise", json!({"id": id, "check_kind": "forbidden", "check_literals": ["--wipe"]}), &format!("toolu_fix{n}"));
        assert!(reply.starts_with(&format!("revised '{id}'")), "{reply}");
    }
    s.stop();
    let filed = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file3");
    assert!(filed.starts_with("stored"), "{filed}");
    assert!(filed.contains("teeth: 8 rule(s) on the frozen batch and A4 list, all settled."), "{filed}");
    s.stop();

    // G15.3: the next evaluation starts with the two that waited.
    let next = start_evaluation(&mut s);
    assert_ne!(next.id, start.id);
    assert_eq!(next.frozen.batch(), &ids[8..], "{:?}", next.frozen.machine);
}

// ---------------------------------------------------------------- step 16

/// Give this session `minutes` of accrued work in demo, so the Stop asks for
/// the evaluation unless a valid report covers the project.
fn worked(s: &Session, minutes: i64) {
    let mut all = serve::usefulness::read_eval_debt_state(&s.db);
    let now = serve::time::now_unix();
    let entry = all.entry("demo".to_string()).or_default();
    entry.sessions.insert(
        s.id.clone(),
        serve::usefulness::SessionWorkState {
            anchor_unix: Some(now - minutes * 60),
            accrued_secs: minutes * 60,
            last_event_unix: Some(now),
            // It ended a turn here before: a first Stop is never held (Q10).
            stops: 1,
            ..Default::default()
        },
    );
    // The project's own clock (step 17) carries the same work.
    entry.work = serve::usefulness::SessionWorkState {
        anchor_unix: Some(now - minutes * 60),
        accrued_secs: minutes * 60,
        last_event_unix: Some(now),
        ..Default::default()
    };
    std::fs::write(serve::usefulness::eval_debt_state_path(&s.db), serde_json::to_string(&all).unwrap()).unwrap();
}

/// G16.1 / G16.5 (Z1 reader, K21): a report filed through THOR's own tool, in
/// the session its evaluation started in, is validated at the next Stop from
/// the log and the transcript - recorded ONCE as valid by `eval-validator` -
/// and that Stop, and the next, are silent. Before it, the same session's
/// Stop asks for the evaluation.
#[test]
fn attested_filing_then_stop_is_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    start_evaluation(&mut s);
    worked(&s, 90);
    let asked = s.stop();
    assert!(asked.contains("\"decision\":\"block\"") && asked.contains("Run the THOR evaluation"), "fixture sanity: {asked}");

    let mut mcp = s.mcp();
    let reply = s.call_unseen(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file1");
    assert!(reply.starts_with(&format!("stored '{REPORT}' (Report, event seq ")), "{reply}");
    let filed = last_of(&s.db, REPORT, EventKind::FactCreated);

    let after = s.stop();
    assert!(after.trim().is_empty(), "a valid report silences the Stop: {after}");
    let validator = |db: &std::path::Path| -> Vec<thor_core::event_store::Event> {
        events_of(db, REPORT)
            .into_iter()
            .filter(|e| e.kind == EventKind::GateStoodAside && e.actor == model::store::EVAL_VALIDATOR_ACTOR)
            .collect()
    };
    let records = validator(&s.db);
    assert_eq!(records.len(), 1, "validated once");
    let record: model::store::EvaluationValidation = serde_json::from_str(&records[0].body).unwrap();
    assert!(record.valid && record.report_seq == filed.seq && record.session == WRITING_SESSION, "{record:?}");
    assert_eq!(records[0].session_id, WRITING_SESSION);

    worked(&s, 90);
    assert!(s.stop().trim().is_empty(), "and the next turn too, while it covers");
    assert_eq!(validator(&s.db).len(), 1, "a later Stop reads the record and writes nothing");
}

// ---------------------------------------------------------------- step 19
//
// The final gate (step 19, item 1): this file holds every EV scenario and
// every attack scenario end to end. The ones above came with steps 12-16;
// the ones below complete the list, each named after the scenario it proves.

/// A binary built beside `mcp` in the same target folder. A plain message
/// when it is not there: no test here ever skips.
fn sibling_exe(name: &str) -> std::path::PathBuf {
    let exe = std::path::Path::new(env!("CARGO_BIN_EXE_mcp")).with_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(
        exe.is_file(),
        "the {name} binary is not built beside mcp ({}) - build every binary first with the same CARGO_TARGET_DIR: \
         cargo build --release --bins",
        exe.display()
    );
    exe
}

/// EV Z1 b (R15.2): the model command line files the evaluation report while
/// an evaluation of demo is open. Measured on the installed build: "stored
/// eval-fxp-2026-09-28 (seq 68)", exit 0. Now: exit 1, the one sentence on
/// stderr, nothing written.
#[test]
fn the_model_cli_cannot_file_an_evaluation() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    start_evaluation(&mut s);
    let before = all_events(&s.db);
    let out = std::process::Command::new(sibling_exe("model"))
        .arg("--db")
        .arg(&s.db)
        .args(["declare", "--id", REPORT, "--kind", "report", "--project", "demo", "--tags", "evaluation-report"])
        .args(["--text", TLDR, "--session-id", WRITING_SESSION])
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output()
        .expect("run model");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.starts_with(
            "REFUSED: this evaluation report is not coming through a THOR tool call that both Claude Code and THOR's hook \
             saw (the model command line is not a THOR tool call)"
        ),
        "{stderr}"
    );
    assert_eq!(all_events(&s.db), before, "nothing written");
}

/// EV Z11 / Z1 c (R16.1, R16.3): a report that reached the log past the
/// filing gate - written raw, the shape an older `mcp.exe` leaves - does not
/// count at the Stop: the turn is held, the report named, the reason given,
/// and the validator records it invalid. Measured on the installed build
/// (SCR z11-oldbin): the old binary's report silenced the evaluation ask.
#[test]
fn a_report_written_past_the_gate_does_not_count_at_the_stop() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Session::new(tmp.path());
    let report = serve::eval_filing::fixture::report(REPORT, "demo", TLDR);
    {
        let mut store = EventStore::open_existing(&s.db).unwrap();
        let body = model::store::canonical_body(&report).unwrap();
        store.append_event("mcp", "mcp", "mcp", EventKind::FactCreated, REPORT, None, &body).unwrap();
    }
    worked(&s, 90);
    let out = s.stop();
    let v: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a block: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(
        reason.starts_with(&format!(
            "[THOR] The report '{REPORT}' is on file but does not count as this project's evaluation: not filed through an \
             attested THOR tool call"
        )),
        "{reason}"
    );
    assert!(reason.contains("The evaluation is still owed."), "{reason}");
    let store = EventStore::open_existing(&s.db).unwrap();
    let record = serve::eval_filing::current_validation(&store, REPORT).expect("the Stop recorded its check");
    assert!(!record.valid && !record.drift, "{record:?}");
}

/// EV Z10 (R16.2): setting `last_evaluation_seen` to now in the debt sidecar
/// - no report behind it - silenced the evaluation for 16 hours on the
/// installed build (SCR z10-debt-sidecar: the second Stop was empty). Both
/// Stops hold the turn now, and the Stop puts the sidecar back to what the
/// log says.
#[test]
fn an_edited_debt_sidecar_does_not_silence_the_stop() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Session::new(tmp.path());
    worked(&s, 90);
    let first = s.stop();
    assert!(first.contains("\"decision\":\"block\"") && first.contains("Run the THOR evaluation"), "fixture sanity: {first}");

    let mut all = serve::usefulness::read_eval_debt_state(&s.db);
    let now = serve::time::now_unix();
    let entry = all.get_mut("demo").expect("the Stop kept demo's entry");
    entry.last_evaluation_seen = Some(now);
    entry.last_evaluation_report_id = Some(REPORT.to_string());
    for session in entry.sessions.values_mut() {
        session.anchor_unix = Some(now + 1);
    }
    std::fs::write(serve::usefulness::eval_debt_state_path(&s.db), serde_json::to_string(&all).unwrap()).unwrap();

    let second = s.stop();
    assert!(second.contains("\"decision\":\"block\""), "an edited sidecar never stands in for a valid report: {second}");
    let state = serve::usefulness::project_eval_state(&s.db, Some("demo"));
    assert_eq!((state.last_evaluation_seen, state.last_evaluation_report_id), (None, None), "put back from the log");
}

/// EV Z8 / K14 (R14.1): an OLD heavy rule tagged `answer-guard:` with an
/// entry the rulebook does not hold. On the installed build the tag counted
/// as settled (SCR z8-answer-guard: doctor named no batch). Now it settles
/// nothing: the rule is owed until THOR's machine pass tries it at the
/// start, and that pass writes its verdict.
#[test]
fn an_answer_guard_tag_settles_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    let id = "says-a-summary";
    seed_raw(
        &s.db,
        &Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: "Every answer to the owner opens with a one-line summary.".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: "README.md".to_string() }],
            severity: Some(Severity::Costly),
            project: Some("demo".to_string()),
            tags: vec![format!("{}no-such-entry", model::store::ANSWER_GUARD_TAG_PREFIX)],
            expires: None,
            key: None,
            falsifier: Some("An answer without a summary line cost the owner nothing.".to_string()),
            check: None,
        },
    );
    let owed = model::store::settlement(&EventStore::open_existing(&s.db).unwrap(), id, None);
    assert!(owed.is_owed(), "the tag settles nothing");

    let start = start_evaluation(&mut s);
    let tried = start.frozen.machine.as_ref().and_then(|m| m.items.iter().find(|i| i.id == id)).cloned();
    let tried = tried.unwrap_or_else(|| panic!("THOR's pass tried it at the start: {:?}", start.frozen.machine));
    assert_ne!(tried.outcome, model::store::MachineOutcome::Unchanged, "{tried:?}");
    let by_machine = events_of(&s.db, id).iter().filter(|e| e.seq > start.seq && e.actor == "teeth-eval").count();
    assert_eq!(by_machine, 1, "the verdict is THOR's trial, written once: {tried:?}");
}

/// EV Z9 (R7a.1, R14.2): a hand-fed PreToolUse payload - a real session id,
/// a made-up tool call id - piped into the hook for a rule whose check
/// refuses it. The hook records a refusal; the session's Stop checks it
/// against the transcript, which holds no such call, and records it as no
/// field evidence, saying why. It never counts as a real refusal.
#[test]
fn a_hand_fed_refusal_is_not_field_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    let id = "no-ledger-wipe";
    let mut mcp = s.mcp();
    let mut args = rule(id, "Never run `acme-tool --wipe` on the ledger; it is the only copy.", Some("demo"));
    args["targets"] = json!([{"kind": "command", "value": "acme-tool"}]);
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["--wipe"]);
    let stored = s.call(&mut mcp, "remember", args, "toolu_rule1");
    assert!(stored.starts_with(&format!("stored '{id}'")), "{stored}");
    // The session is a real one: it has other activity in the log, and its
    // transcript exists and holds other calls - just not this one.
    served(&s.db, WRITING_SESSION, &["git status"]);
    s.bash_call("toolu_REAL", "git status", "nothing to commit, working tree clean");

    let hand_fed = s.hook(json!({
        "hook_event_name": "PreToolUse", "session_id": s.id, "cwd": s.cwd, "transcript_path": s.transcript,
        "tool_name": "Bash", "tool_input": {"command": "acme-tool --wipe"}, "tool_use_id": "toolu_HAND"
    }));
    assert!(hand_fed.contains(id), "fixture sanity - the check refuses the hand-fed call: {hand_fed}");
    let refusal = events_of(&s.db, id)
        .into_iter()
        .filter(|e| e.kind == EventKind::GateRefused)
        .last()
        .expect("the hook recorded the refusal");
    s.stop();

    let verdicts = model::store::recorded_refusal_verdicts(&events_of(&s.db, id));
    let (field, why) = verdicts.get(&refusal.seq).cloned().expect("the Stop recorded its check of the refusal");
    assert!(!field, "a hand-fed refusal is never field evidence: {why}");
    assert!(why.contains("holds no tool call toolu_HAND"), "{why}");
    let item = get(&mut mcp, id);
    let store = EventStore::open_existing(&s.db).unwrap();
    assert!(serve::attest::field_refusals(&store, id, &item).is_empty());
}

/// D9: `serve mark` run from a shell - no THOR tool call - neither clears
/// the judgement debt nor takes the item off the list an evaluation then
/// freezes. Only an attested verdict counts for either.
#[test]
fn a_shell_serve_mark_neither_clears_debt_nor_shrinks_a_list() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    fired_rule(&s.db, "listed-a", WRITING_SESSION, 40);
    fired_rule(&s.db, "listed-b", WRITING_SESSION, 40);
    let owed = |db: &std::path::Path| model::store::judgement_debt_in_project(&EventStore::open_existing(db).unwrap(), Some("demo"));
    assert!(owed(&s.db).contains(&"listed-a".to_string()), "fixture sanity: {:?}", owed(&s.db));
    let asked = s.stop();
    assert!(asked.contains("\"decision\":\"block\"") && asked.contains("listed-a"), "fixture sanity: {asked}");

    let out = std::process::Command::new(serve_exe())
        .arg("--db")
        .arg(&s.db)
        .args(["mark", "listed-a"])
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output()
        .expect("run serve mark");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && stdout.starts_with("marked useful: listed-a"), "{stdout} {}", String::from_utf8_lossy(&out.stderr));
    last_of(&s.db, "listed-a", EventKind::ItemMarkedUseful);

    assert!(owed(&s.db).contains(&"listed-a".to_string()), "the debt is unchanged by a verdict from a shell");
    let asked = s.stop();
    assert!(asked.contains("\"decision\":\"block\"") && asked.contains("listed-a"), "the Stop still asks for it: {asked}");
    let start = start_evaluation(&mut s);
    assert_eq!(start.frozen.judgement, vec!["listed-a".to_string(), "listed-b".to_string()], "the list did not shrink");
}

/// D14 (R16.7, R16.9): after an attested filing the session's transcript is
/// deleted - the report is invalid, never drift; a transcript of a shape THOR
/// cannot read is drift - nothing counts, and the Stop says so without
/// holding the turn for what cannot be filed.
#[test]
fn a_missing_transcript_is_invalid_and_an_unreadable_one_is_drift() {
    // Missing.
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    start_evaluation(&mut s);
    let mut mcp = s.mcp();
    let reply = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file1");
    assert!(reply.starts_with("stored"), "{reply}");
    std::fs::remove_file(&s.transcript).unwrap();
    s.stop();
    let record = serve::eval_filing::current_validation(&EventStore::open_existing(&s.db).unwrap(), REPORT)
        .expect("the Stop recorded its check");
    assert!(!record.valid && !record.drift, "{record:?}");
    assert!(record.why.contains("the session's transcript is gone"), "{}", record.why);

    // Unreadable: more than 64 KB, and no line of a tool call shape THOR knows.
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    start_evaluation(&mut s);
    let mut mcp = s.mcp();
    let reply = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file1");
    assert!(reply.starts_with("stored"), "{reply}");
    let unknown = r#"{"kind":"turn","parts":[{"call":{"ref":"x1","tool":"remember","args":{"id":"eval-demo"}}}],"pad":"PAD"}"#
        .replace("PAD", &"z".repeat(900));
    std::fs::write(&s.transcript, vec![unknown; 110].join("\n") + "\n").unwrap();
    worked(&s, 90);
    let out = s.stop();
    let record = serve::eval_filing::current_validation(&EventStore::open_existing(&s.db).unwrap(), REPORT)
        .expect("the Stop recorded its check");
    assert!(record.drift && !record.valid, "{record:?}");
    assert!(out.contains("in a shape THOR cannot read"), "the Stop tells the owner: {out}");
    assert!(!out.contains("\"decision\":\"block\""), "nothing is demanded that cannot be filed: {out}");
}

/// F10 (R14.8): the machine pass writes its start record FIRST - every write
/// of the pass and the batch record come after it - and a hook killed after
/// that first write leaves a start with no batch: a filing then is refused
/// with the "still deciding" sentence, and the session's next Stop finishes
/// the pass, once.
#[test]
fn the_machine_pass_writes_its_start_first_and_resumes_after_a_kill() {
    let old_rules = |db: &std::path::Path| {
        for id in ["undecided-a", "undecided-b"] {
            seed_raw(
                db,
                &Item {
                    id: id.to_string(),
                    kind: Kind::Rule,
                    text: format!("Run the fixture check {id} before a commit."),
                    bindings: vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-cmd-{id}") }],
                    severity: None,
                    project: Some("demo".to_string()),
                    tags: vec![],
                    expires: None,
                    key: None,
                    falsifier: Some("the fixture check turns out to be useless".to_string()),
                    check: None,
                },
            );
        }
    };

    // A whole start: the record, the pass's writes, the batch record - in that order.
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    old_rules(&s.db);
    let start = start_evaluation(&mut s);
    let record = events_of(&s.db, &start.id);
    assert_eq!(record.len(), 2, "the start record and its batch record");
    let pass: Vec<i64> = ["undecided-a", "undecided-b"]
        .iter()
        .flat_map(|id| events_of(&s.db, id))
        .filter(|e| e.actor == "teeth-eval")
        .map(|e| e.seq)
        .collect();
    assert_eq!(pass.len(), 2, "{pass:?}");
    assert!(
        pass.iter().all(|seq| record[0].seq < *seq && *seq < record[1].seq),
        "start {} < pass {pass:?} < batch {}",
        record[0].seq,
        record[1].seq
    );

    // What a killed hook leaves: the first of the two writes only.
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    old_rules(&s.db);
    {
        let mut store = EventStore::open_existing(&s.db).unwrap();
        let began = serve::eval_session::begin(&s.db, &mut store, WRITING_SESSION, "demo", "command").expect("a start");
        assert!(began.fresh && !began.start.frozen.decided());
    }
    let mut mcp = s.mcp();
    let reply = s.call(&mut mcp, "remember", report_args(REPORT, "demo", TLDR), "toolu_file1");
    assert!(reply.starts_with("REFUSED") && reply.contains("THOR is still deciding the old rules - end this turn once"), "{reply}");
    assert!(mcp.call("get", json!({"id": REPORT})).contains("no item"), "nothing filed");
    let out = s.stop();
    assert!(out.contains("THOR finished deciding the old rules of 'demo'"), "{out}");
    let resumed = s.open_start("demo").expect("still open");
    assert!(resumed.frozen.decided(), "{resumed:?}");
    assert_eq!(events_of(&s.db, &resumed.id).len(), 2, "the batch record is written, once");
    assert!(!s.stop().contains("finished deciding"), "a later Stop resumes nothing");
}
