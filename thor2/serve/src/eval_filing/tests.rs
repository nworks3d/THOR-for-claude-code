//! Units of the Stop-time validator of an evaluation report (step 16 of the
//! watertight teeth build): what makes a filing count, what makes it not
//! count, format drift, and the carry of THOR's own open points counted from
//! the validator's records.

#![cfg(test)]

use super::fixture::{report, valid_filing};
use super::*;
use crate::attest::fixture::{attested_filing, attested_filing_unverified, filing_without_the_start_note, main_transcript};
use model::item::{Binding, Check, Kind, Severity, TargetKind};
use std::time::Duration;

const S1: &str = "0de7dc52-1b97-4b66-948f-61e4438043b2";
const S2: &str = "373e8610-8130-490d-ac29-56aa98709fd1";
const S3: &str = "5e1f0c3a-0000-4000-8000-000000000003";

/// A store on disk (the machine pass needs a folder beside it) and the
/// transcripts folder the validator reads.
struct Fx {
    dir: tempfile::TempDir,
    db: PathBuf,
    store: EventStore,
}

impl Fx {
    fn new() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let store = EventStore::new(&db).unwrap();
        Fx { dir, db, store }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("transcripts")
    }

    /// Every validator record written now, as a Stop writes them.
    fn stop(&mut self) -> Vec<EvaluationValidation> {
        let root = self.root();
        crate::attest::record_write_verifications_in(&mut self.store, S1, Some(&root));
        validate_new_reports(&mut self.store, Some(&root), S1)
    }

    /// A raw write, the way an older store or a program past the gate
    /// leaves one.
    fn raw(&mut self, session: &str, kind: EventKind, item: &Item, parent: Option<&str>) -> Event {
        let body = model::store::canonical_body(item).unwrap();
        self.store.append_event(session, session, "cli", kind, &item.id, parent, &body).unwrap()
    }

    /// One evaluation start of `project` by `session`, both writes (THOR's
    /// real machine pass, with a box nothing here runs out of).
    fn start(&mut self, session: &str, project: &str) -> EvaluationStart {
        let began = crate::eval_session::begin(&self.db, &mut self.store, session, project, "command").unwrap();
        crate::eval_session::decide(&mut self.store, began.start, Duration::from_secs(120))
    }
}

fn rule(id: &str, text: &str, bindings: Vec<Binding>, check: Option<Check>, project: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings,
        severity: Some(Severity::Irreversible),
        project: Some(project.to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("It turns out to be harmless.".to_string()),
        check,
    }
}

/// A file-check rule of `project`, written raw (an old rule): with no working
/// copy of the project registered, THOR's machine pass holds it by its own
/// open point `open-no-working-copy-<project>`.
fn unregistered_file_rule(fx: &mut Fx, id: &str, project: &str) {
    let item = rule(
        id,
        "README.md keeps `alpha line`.",
        vec![Binding::Target { kind: TargetKind::Path, value: "README.md".to_string() }],
        Some(Check::Contains { path: "README.md".to_string(), literal: "alpha line".to_string() }),
        project,
    );
    let body = model::store::canonical_body(&item).unwrap();
    fx.store.append_event("legacy", id, "migration", EventKind::FactCreated, id, None, &body).unwrap();
}

/// A report stored past the tool server's filing gate, as if a program
/// wrote it: raw, but otherwise exactly what an attested `remember` leaves -
/// the stamp of the call, and the call's three lines in `session`'s
/// transcript - with `stored_text` as the text on the log.
fn filed_past_the_gate(fx: &mut Fx, session: &str, sent: &Item, stored_text: &str, tool_use_id: &str) -> Event {
    let root = fx.root();
    let args = crate::attest::fixture::filing_args(sent);
    let mut transcript = main_transcript(&root, session);
    transcript.call(tool_use_id, "mcp__thor__remember", &args).write();
    let att = crate::attest::att_for_call(&crate::attest::fixture::meta(tool_use_id), Some(session), &args, "remember", Some(&root));
    let mut stored = sent.clone();
    stored.text = stored_text.to_string();
    let body = model::store::with_att(model::store::canonical_body(&stored).unwrap(), Some(&att)).unwrap();
    let event = fx.store.append_event(session, session, "mcp", EventKind::FactCreated, &sent.id, None, &body).unwrap();
    let marker = crate::attest::write_marker("mcp__thor__remember", Some(tool_use_id), Some(&args)).unwrap();
    transcript
        .hook_context(tool_use_id, "mcp__thor__remember", &marker)
        .result(tool_use_id, &format!("stored '{}' (Report, event seq {})", sent.id, event.seq))
        .write();
    event
}

/// The machine block THOR writes for `start` filed at the log's tip now.
fn machine_block_now(fx: &Fx, start: &EvaluationStart) -> (String, i64) {
    let filed = fx.store.get_next_seq().unwrap() - 1;
    let block = format!(
        "{}\n\n{}\n",
        model::store::render_machine_section(&fx.store, start, filed),
        model::gate::AGENT_SECTION_BEGIN
    );
    (block, filed)
}

fn invalid_why(validation: Validation) -> String {
    match validation {
        Validation::Invalid(why) => why,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

// ------------------------------------------------------------- (1) attestation

/// FIELD DEFECT 3 (2026-10-01): a report filed through the THOR tool itself,
/// before the build that checks filings was installed, was described as
/// "written by another program (the command line, a repair tool or an older
/// build)". The rule stands - it does not count - but the words say what
/// happened: filed before THOR 2.5 checked filings. A report written past the
/// tools AFTER checked filings began keeps the other wording.
#[test]
fn a_report_filed_before_checks_began_says_so_plainly() {
    let mut fx = Fx::new();
    crate::attest::fixture::quiet_start(&mut fx.store, S1, "fxp");
    let early = report("eval-fxp-2026-09-30", "fxp", "TLDR: filed through the tool, before filings were checked.");
    fx.raw(S1, EventKind::FactCreated, &early, None);
    // Checked filings begin: the first write THOR's attester verifies.
    let checked = rule("checked-rule", "Run the demo linter before `demo-release`.", vec![Binding::Target { kind: TargetKind::Command, value: "demo-release".to_string() }], None, "fxp");
    fx.raw(S2, EventKind::FactCreated, &checked, None);
    crate::attest::fixture::attest_every_write(&mut fx.store, S2);
    let why = invalid_why(validate(&fx.store, &early.id, Some(&fx.root())));
    assert!(why.contains("filed before THOR 2.5 checked filings; does not count after the upgrade"), "{why}");
    assert!(!why.contains("written by another program"), "{why}");

    let late = report("eval-fxp-2026-10-01", "fxp", "TLDR: written past the tools after checks began.");
    fx.raw(S1, EventKind::FactCreated, &late, None);
    let why = invalid_why(validate(&fx.store, &late.id, Some(&fx.root())));
    assert!(why.contains("written by another program") && !why.contains("filed before THOR 2.5"), "{why}");

    // A record written by the build before this one keeps its words on the
    // log; every reader shows today's.
    let mut old_record = EvaluationValidation {
        valid: false,
        why: format!("{NOT_ATTESTED} (it carries no stamp of THOR's tool server - it was written by another program (the command line, a repair tool or an older build))"),
        drift: false,
        report: early.id.clone(),
        report_seq: 1,
        project: Some("fxp".to_string()),
        session: S1.to_string(),
        agent: None,
        started_seq: None,
        filed_seq: None,
        filed_at: None,
        machine_sha256: None,
        verdicts: VerdictShares::default(),
        carry: Vec::new(),
    };
    assert!(why_in_words(&fx.store, &old_record).contains("filed before THOR 2.5 checked filings"));
    old_record.report_seq = i64::MAX;
    assert!(why_in_words(&fx.store, &old_record).contains("written by another program"), "after checks began: unchanged");
}

/// R16.1 / R16.3 at the unit level: a report that reached the log without a
/// THOR tool call of its session - an older binary, the model CLI, a repair
/// tool - never counts, and the record says why.
#[test]
fn report_without_attestation_is_invalid() {
    let mut fx = Fx::new();
    crate::attest::fixture::quiet_start(&mut fx.store, S1, "fxp");
    let old = report("eval-fxp-2026-09-28", "fxp", "TLDR: filed by an older binary.");
    fx.raw(S1, EventKind::FactCreated, &old, None);
    let why = invalid_why(validate(&fx.store, &old.id, Some(&fx.root())));
    assert!(why.starts_with(NOT_ATTESTED) && why.contains("no stamp of THOR's tool server"), "{why}");

    let written = fx.stop();
    assert_eq!(written.len(), 1, "recorded once");
    assert!(!written[0].valid && !written[0].drift && written[0].why == why, "{:?}", written[0]);
    assert_eq!(newest_valid_evaluation(&fx.store, Some("fxp")), None, "a report on file that does not count silences nothing");
    assert_eq!(newest_invalid_report(&fx.store, Some("fxp")), Some((old.id.clone(), why)));
}

// ------------------------------------------------------------- (4) the machine block

/// B1 at the reader: a machine block that is not the one THOR derives from
/// the log for its N and F - written past the gate - never counts.
#[test]
fn machine_block_differing_from_the_log_is_invalid() {
    let mut fx = Fx::new();
    let start = crate::attest::fixture::quiet_start(&mut fx.store, S1, "fxp");
    main_transcript(&fx.root(), S1).prompt_context(&crate::eval_session::start_note(&start)).write();
    let (block, _) = machine_block_now(&fx, &start);
    let forged = block.replacen("JUDGEMENT: 0 item(s)", "JUDGEMENT: 0 item(s), all judged with care", 1);
    assert_ne!(forged, block, "fixture sanity: the block was changed");
    let sent = report("eval-fxp-2026-09-28", "fxp", "TLDR: nothing to add.");
    fx.filed_past(&sent, &format!("{forged}{}", sent.text));
    let why = invalid_why(validate(&fx.store, &sent.id, Some(&fx.root())));
    assert!(why.contains("machine section is not the one THOR derives from the log"), "{why}");

    // The same filing with the block THOR derives passes this check.
    let mut fx = Fx::new();
    let start = crate::attest::fixture::quiet_start(&mut fx.store, S1, "fxp");
    main_transcript(&fx.root(), S1).prompt_context(&crate::eval_session::start_note(&start)).write();
    let (block, _) = machine_block_now(&fx, &start);
    fx.filed_past(&sent, &format!("{block}{}", sent.text));
    assert!(matches!(validate(&fx.store, &sent.id, Some(&fx.root())), Validation::Valid(_)));
}

/// 2026-10-06: a report filed by a tool server older than the hook's checker
/// did not count, and the reason blamed the agent ("edited, or written past
/// the filing gate"). A block whose header names another renderer revision -
/// or none, as before the header carried one - is version skew: the reason
/// says so, tells what to do, and the validator's record is what the next
/// machine section counts.
#[test]
fn a_block_from_another_renderer_revision_is_named_version_skew_not_an_edit() {
    for header in [
        "; renderer 1".to_string(),                           // an older revision
        String::new(),                                        // written before the header carried one
        format!("; renderer {}", model::gate::MACHINE_RENDERER_REV + 1), // a newer tool server than the checker
    ] {
        let mut fx = Fx::new();
        let start = crate::attest::fixture::quiet_start(&mut fx.store, S1, "fxp");
        main_transcript(&fx.root(), S1).prompt_context(&crate::eval_session::start_note(&start)).write();
        let (block, _) = machine_block_now(&fx, &start);
        let current = format!("; renderer {}", model::gate::MACHINE_RENDERER_REV);
        let skewed = block.replacen(&current, &header, 1).replacen("JUDGEMENT: 0 item(s)", "JUDGEMENT: no items", 1);
        assert_ne!(skewed, block, "fixture sanity");
        let sent = report("eval-fxp-2026-09-28", "fxp", "TLDR: nothing to add.");
        fx.filed_past(&sent, &format!("{skewed}{}", sent.text));
        let why = invalid_why(validate(&fx.store, &sent.id, Some(&fx.root())));
        assert!(why.starts_with(model::store::SKEW_WHY), "{header:?}: {why}");
        assert!(why.contains("restart Claude") && why.contains("nothing was edited"), "{why}");
        assert!(!why.contains("edited, or written past"), "{why}");
    }
}

/// The same revision with other bytes IS an edit (or a write past the gate),
/// and the reason points at the first line that differs.
#[test]
fn a_block_of_the_same_revision_with_other_bytes_names_the_first_differing_line() {
    let mut fx = Fx::new();
    let start = crate::attest::fixture::quiet_start(&mut fx.store, S1, "fxp");
    main_transcript(&fx.root(), S1).prompt_context(&crate::eval_session::start_note(&start)).write();
    let (block, _) = machine_block_now(&fx, &start);
    let forged = block.replacen("JUDGEMENT: 0 item(s)", "JUDGEMENT: 0 item(s), all judged with care", 1);
    let line = block.lines().zip(forged.lines()).position(|(a, b)| a != b).expect("a differing line") + 1;
    let sent = report("eval-fxp-2026-09-28", "fxp", "TLDR: nothing to add.");
    fx.filed_past(&sent, &format!("{forged}{}", sent.text));
    let why = invalid_why(validate(&fx.store, &sent.id, Some(&fx.root())));
    assert!(why.contains("edited, or written past the filing gate"), "{why}");
    assert!(why.contains(&format!("first differs at line {line} ")), "{why}");
    assert!(!why.starts_with(model::store::SKEW_WHY), "{why}");
}

impl Fx {
    fn filed_past(&mut self, sent: &Item, stored_text: &str) -> Event {
        filed_past_the_gate(self, S1, sent, stored_text, "toolu_past")
    }
}

// ------------------------------------------------------------- (2) the start note

/// A filing whose session was never shown the start note - no Stop ask,
/// typed command or skill call of it carries the lists - does not count.
#[test]
fn start_note_missing_from_the_transcript_is_invalid() {
    let mut fx = Fx::new();
    let root = fx.root();
    filing_without_the_start_note(&mut fx.store, &root, S1, &report("eval-fxp-a", "fxp", "TLDR."), "toolu_f1").unwrap();
    let why = invalid_why(validate(&fx.store, "eval-fxp-a", Some(&root)));
    assert!(why.contains("the start note of 'evaluation-start-fxp-") && why.contains("is not in session"), "{why}");

    // Shown as the Stop ask's block reason, it counts.
    let start = model::store::evaluation_starts_of_session(&fx.store, S1).pop().unwrap();
    let mut fx2 = Fx::new();
    let root2 = fx2.root();
    let start2 = crate::attest::fixture::quiet_start(&mut fx2.store, S1, "fxp");
    main_transcript(&root2, S1).stop_block(&format!("[THOR] asked.\n\n{}", crate::eval_session::start_note(&start2))).write();
    filing_without_the_start_note(&mut fx2.store, &root2, S1, &report("eval-fxp-a", "fxp", "TLDR."), "toolu_f1").unwrap();
    assert!(matches!(validate(&fx2.store, "eval-fxp-a", Some(&root2)), Validation::Valid(_)), "{start:?}");
}

// ------------------------------------------------------------- recorded once

/// G16.1 at the unit level: a valid filing is recorded ONCE, read back by
/// every later reader, and keeps counting after a typo fix below its machine
/// block - but not after its block is changed.
#[test]
fn valid_report_is_recorded_once_and_read_back() {
    let mut fx = Fx::new();
    let root = fx.root();
    let filed = report("eval-fxp-2026-09-28", "fxp", "TLDR: evaluaton of fxp.");
    let event = attested_filing(&mut fx.store, &root, S1, &filed, "toolu_f1").unwrap();
    let written = fx.stop();
    assert_eq!(written.len(), 1);
    let record = &written[0];
    assert!(record.valid && record.why.is_empty() && !record.drift, "{record:?}");
    assert_eq!((record.report.as_str(), record.report_seq, record.session.as_str()), ("eval-fxp-2026-09-28", event.seq, S1));
    assert_eq!(record.agent, None);
    let stored = model::store::show(&fx.store, &filed.id).unwrap();
    let block = model::gate::machine_block(&stored.text).unwrap();
    assert_eq!(record.machine_sha256.as_deref(), Some(model::store::machine_block_digest(block).as_str()));
    let (n, f) = model::gate::machine_section_range(&stored.text).unwrap();
    assert_eq!((record.started_seq, record.filed_seq), (Some(n), Some(f)));
    assert!(record.filed_at.as_deref().and_then(unix_of_timestamp).is_some(), "{record:?}");
    let validator_events: Vec<Event> = fx
        .store
        .get_events_by_entity(&filed.id)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EventKind::GateStoodAside && e.actor == model::store::EVAL_VALIDATOR_ACTOR)
        .collect();
    assert_eq!(validator_events.len(), 1);
    assert_eq!(validator_events[0].session_id, S1);

    assert!(fx.stop().is_empty(), "a later Stop reads the record and writes nothing");
    assert_eq!(current_validation(&fx.store, &filed.id).as_ref(), Some(record));
    assert_eq!(newest_valid_evaluation(&fx.store, Some("fxp")).as_ref(), Some(record));
    assert_eq!(newest_invalid_report(&fx.store, Some("fxp")), None);

    // A typo fix below the machine block: still this project's evaluation.
    let mut fixed = stored.clone();
    fixed.text = stored.text.replace("evaluaton", "evaluation");
    let head = fx.store.get_events_by_entity(&filed.id).unwrap().into_iter().filter(|e| e.kind == EventKind::FactCreated).last().unwrap();
    let typo = fx.raw(S1, EventKind::FactRevised, &fixed, Some(&head.this_hash));
    assert!(fx.stop().is_empty(), "a revise that files nothing new is not a new filing");
    assert_eq!(newest_valid_evaluation(&fx.store, Some("fxp")).as_ref(), Some(record), "the block's digest is unchanged");

    // A changed block: it no longer counts.
    let mut edited = fixed.clone();
    edited.text = fixed.text.replacen("JUDGEMENT:", "JUDGEMENT (all read):", 1);
    fx.raw(S1, EventKind::FactRevised, &edited, Some(&typo.this_hash));
    assert_eq!(newest_valid_evaluation(&fx.store, Some("fxp")), None, "the machine block no longer hashes to the record");
}

// ------------------------------------------------------------- D14

/// R16.7: the session's transcript deleted after an attested filing - the
/// report is recorded as invalid, never as drift.
#[test]
fn a_missing_transcript_is_invalid_never_drift() {
    let mut fx = Fx::new();
    let root = fx.root();
    attested_filing_unverified(&mut fx.store, &root, S1, &report("eval-fxp-a", "fxp", "TLDR."), "toolu_f1").unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    let written = fx.stop();
    assert_eq!(written.len(), 1);
    assert!(!written[0].valid && !written[0].drift, "{:?}", written[0]);
    assert!(written[0].why.contains("the session's transcript is gone"), "{}", written[0].why);
    assert!(!drift_recorded(&fx.store));
    assert_eq!(newest_valid_evaluation(&fx.store, Some("fxp")), None);
}

/// R16.9: a transcript of more than 64 KB whose lines use a shape THOR does
/// not know - no report counts, the record says drift, and the Stop's own
/// question for the session says so too.
#[test]
fn an_unreadable_transcript_shape_is_drift_and_nothing_counts() {
    let mut fx = Fx::new();
    let root = fx.root();
    attested_filing_unverified(&mut fx.store, &root, S1, &report("eval-fxp-a", "fxp", "TLDR."), "toolu_f1").unwrap();
    let file = root.join(crate::attest::fixture::PROJECT_FOLDER).join(format!("{S1}.jsonl"));
    let unknown = r#"{"kind":"turn","parts":[{"call":{"ref":"x1","tool":"remember","args":{"id":"eval-fxp-a"}}}],"pad":"PAD"}"#
        .replace("PAD", &"z".repeat(900));
    let lines: Vec<String> = std::iter::repeat(unknown).take(110).collect();
    std::fs::write(&file, lines.join("\n") + "\n").unwrap();
    assert!(std::fs::metadata(&file).unwrap().len() > DRIFT_MIN_BYTES);
    assert!(session_drifted(&fx.db, Some(&root), S1));
    assert!(matches!(validate(&fx.store, "eval-fxp-a", Some(&root)), Validation::Drift(_)));

    let written = fx.stop();
    assert_eq!(written.len(), 1);
    assert!(written[0].drift && !written[0].valid, "{:?}", written[0]);
    assert!(drift_recorded(&fx.store));
    assert_eq!(newest_valid_evaluation(&fx.store, Some("fxp")), None, "nothing counts during drift");

    // A small transcript of the same shape is no drift: just no call.
    let mut fx = Fx::new();
    let root = fx.root();
    attested_filing_unverified(&mut fx.store, &root, S1, &report("eval-fxp-a", "fxp", "TLDR."), "toolu_f1").unwrap();
    let file = root.join(crate::attest::fixture::PROJECT_FOLDER).join(format!("{S1}.jsonl"));
    std::fs::write(&file, "{\"kind\":\"turn\"}\n").unwrap();
    assert!(!session_drifted(&fx.db, Some(&root), S1));
    assert!(matches!(validate(&fx.store, "eval-fxp-a", Some(&root)), Validation::Invalid(_)));
}

/// C3b probe B0: a chat-only first turn (no tool call at all) whose transcript
/// is over 64 KB only because of the injected context is readable, not drift -
/// even when that context mentions the word tool_use in plain text.
#[test]
fn a_large_transcript_with_no_tool_call_is_no_drift() {
    let fx = Fx::new();
    let root = fx.root();
    std::fs::create_dir_all(root.join(crate::attest::fixture::PROJECT_FOLDER)).unwrap();
    let file = root.join(crate::attest::fixture::PROJECT_FOLDER).join(format!("{S1}.jsonl"));
    let context = r#"{"type":"attachment","attachment":{"type":"hook_additional_context","content":["PAD a tool_use block is what Claude Code writes for a call"]}}"#
        .replace("PAD", &"z".repeat(900));
    let chat = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello."}]}}"#.to_string();
    let mut lines: Vec<String> = std::iter::repeat(context).take(110).collect();
    lines.push(chat);
    std::fs::write(&file, lines.join("
") + "
").unwrap();
    assert!(std::fs::metadata(&file).unwrap().len() > DRIFT_MIN_BYTES);
    assert!(!session_drifted(&fx.db, Some(&root), S1), "no tool call to read is not a shape THOR cannot read");
}

/// A transcript that does hold tool calls but only in a shape THOR does not
/// know (a tool_use block without its id) is drift; one known call among
/// them is not.
#[test]
fn tool_calls_that_cannot_be_read_are_drift_and_one_readable_call_is_not() {
    let fx = Fx::new();
    let root = fx.root();
    std::fs::create_dir_all(root.join(crate::attest::fixture::PROJECT_FOLDER)).unwrap();
    let file = root.join(crate::attest::fixture::PROJECT_FOLDER).join(format!("{S1}.jsonl"));
    let broken = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","call":"c1","pad":"PAD"}]}}"#
        .replace("PAD", &"z".repeat(900));
    let mut lines: Vec<String> = std::iter::repeat(broken).take(110).collect();
    std::fs::write(&file, lines.join("
") + "
").unwrap();
    assert!(session_drifted(&fx.db, Some(&root), S1));
    let known = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{}}]}}"#;
    lines.push(known.to_string());
    std::fs::write(&file, lines.join("
") + "
").unwrap();
    assert!(!session_drifted(&fx.db, Some(&root), S1));
}

// ------------------------------------------------------------- D10

/// R16.8: an item on the frozen judgement list changed in the window by a
/// program (no THOR tool call), and a report stored past the tool server's
/// gate - the reader alone finds it: invalid, naming the item, the seq and
/// the door.
#[test]
fn a_side_door_write_to_a_frozen_item_makes_the_report_invalid() {
    let mut fx = Fx::new();
    let listed = Item {
        id: "fx-listed".to_string(),
        kind: Kind::Rule,
        text: "Run the spelling pass over docs before a release.".to_string(),
        bindings: vec![Binding::Target { kind: TargetKind::Command, value: "fx-spell".to_string() }],
        severity: None,
        project: Some("fxp".to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("Skipping the spelling pass never cost anything.".to_string()),
        check: None,
    };
    model::store::declare(&mut fx.store, "t", "t", "t", &listed).unwrap();
    for i in 0..model::store::JUDGEMENT_DEBT_AFTER {
        crate::deliver::record_delivery_with_trigger(&mut fx.store, &crate::deliver::nth_session(S1, i), S1, "hook", "2026-09-28T00:00:00Z", &[listed.id.clone()], Some("fx-spell"));
    }
    let start = crate::attest::fixture::quiet_start(&mut fx.store, S1, "fxp");
    assert_eq!(start.frozen.judgement, vec!["fx-listed".to_string()], "fixture sanity: the item is on the frozen list");
    main_transcript(&fx.root(), S1).prompt_context(&crate::eval_session::start_note(&start)).write();
    let archived = model::store::archive(&mut fx.store, "cli", "cli", "cli", "fx-listed", "the spelling pass is gone", None).unwrap();
    let (block, _) = machine_block_now(&fx, &start);
    let sent = report("eval-fxp-a", "fxp", "TLDR.");
    fx.filed_past(&sent, &format!("{block}{}", sent.text));
    let why = invalid_why(validate(&fx.store, &sent.id, Some(&fx.root())));
    assert!(
        why.contains(&format!("'fx-listed' was changed at log seq {} outside an attested THOR call of this evaluation", archived.seq))
            && why.contains("written by 'cli' with no stamp"),
        "{why}"
    );
}

// ------------------------------------------------------------- carry (B5)

/// Three evaluations of a project with no registered working copy, filed by
/// S1, S2 and S3: the carry of the subject is counted from the validator's
/// records - 0 before the first, 1 before the second - per subject, and a
/// subject another point holds is counted apart.
#[test]
fn carry_is_counted_from_validator_events_per_subject() {
    let mut fx = Fx::new();
    let root = fx.root();
    unregistered_file_rule(&mut fx, "fx-file", "fxp");
    unregistered_file_rule(&mut fx, "beta-file", "beta");
    let first = fx.start(S1, "fxp");
    assert_eq!(first.frozen.machine.as_ref().unwrap().points.len(), 1, "{:?}", first.frozen.machine);
    let one = valid_filing(&mut fx.store, &root, S1, &report("eval-fxp-1", "fxp", "TLDR one."), "toolu_f1");
    assert_eq!(one.carry.iter().map(|c| (c.subject.as_str(), c.carried, c.session.as_str())).collect::<Vec<_>>(), vec![("fxp", 0, S1)]);
    assert_eq!(model::store::carried_by_valid_evaluations(&fx.store, "fxp", "fxp", i64::MAX), 1);
    assert_eq!(model::store::carried_by_valid_evaluations(&fx.store, "fxp", "beta", i64::MAX), 0, "per subject");
    assert_eq!(model::store::carried_by_valid_evaluations(&fx.store, "beta", "fxp", i64::MAX), 0, "per project");

    fx.start(S2, "fxp");
    let two = valid_filing(&mut fx.store, &root, S2, &report("eval-fxp-2", "fxp", "TLDR two."), "toolu_f2");
    assert_eq!(two.carry.iter().map(|c| (c.subject.as_str(), c.carried, c.session.as_str())).collect::<Vec<_>>(), vec![("fxp", 1, S2)]);
    assert_eq!(model::store::carried_by_valid_evaluations(&fx.store, "fxp", "fxp", i64::MAX), 2);

    // An invalid filing carries nothing.
    let third = fx.start(S3, "fxp");
    filing_without_the_start_note(&mut fx.store, &root, S3, &report("eval-fxp-3", "fxp", "TLDR three."), "toolu_f3").unwrap();
    crate::attest::record_write_verifications_in(&mut fx.store, S3, Some(&root));
    validate_new_reports(&mut fx.store, Some(&root), S3);
    assert!(!current_validation(&fx.store, "eval-fxp-3").unwrap().valid);
    assert_eq!(model::store::carried_by_valid_evaluations(&fx.store, "fxp", "fxp", i64::MAX), 2, "{third:?}");
}

/// R16.4 (spec intent): THOR's own point closed and written again for the
/// same subject - the next start re-creates it - keeps its carry, because
/// the carry is counted per subject from the validator's records. A retract
/// of the point from outside THOR is refused at the store's fence (the only
/// actor that may write it is `eval-machine`).
#[test]
fn retract_and_refile_does_not_reset_the_carry() {
    let mut fx = Fx::new();
    let root = fx.root();
    unregistered_file_rule(&mut fx, "fx-file", "fxp");
    fx.start(S1, "fxp");
    valid_filing(&mut fx.store, &root, S1, &report("eval-fxp-1", "fxp", "TLDR one."), "toolu_f1");

    let side_door = model::store::retract(&mut fx.store, "cli", "cli", "cli", "open-no-working-copy-fxp", "done", None);
    assert!(matches!(side_door, Err(model::store::WriteError::Refused(_))), "{side_door:?}");

    super::fixture::close_point(&mut fx.store, S1, "open-no-working-copy-fxp");
    assert!(model::store::open_points(&fx.store, Some("fxp")).is_empty(), "closed");
    let next = fx.start(S2, "fxp");
    let point = &next.frozen.machine.as_ref().unwrap().points[0];
    assert_eq!(point.id, "open-no-working-copy-fxp", "the next start re-creates the point for the same subject");
    assert_eq!(point.carried_by.len(), 1, "carried 1, not 0: {point:?}");
    assert_eq!(model::store::carried_by_valid_evaluations(&fx.store, "fxp", "fxp", i64::MAX), 1);
}

/// B5: the third start of a project whose subject two valid evaluations
/// carried puts the point in the OWNER QUEUE - its text and the start note
/// say the owner decides and name every carrying session with its date -
/// and the third filing is not refused for it.
#[test]
fn owner_queue_names_every_carrying_session() {
    let mut fx = Fx::new();
    let root = fx.root();
    unregistered_file_rule(&mut fx, "fx-file", "fxp");
    fx.start(S1, "fxp");
    let one = valid_filing(&mut fx.store, &root, S1, &report("eval-fxp-1", "fxp", "TLDR one."), "toolu_f1");
    fx.start(S2, "fxp");
    let two = valid_filing(&mut fx.store, &root, S2, &report("eval-fxp-2", "fxp", "TLDR two."), "toolu_f2");
    let third = fx.start(S3, "fxp");
    let date = |v: &EvaluationValidation| v.filed_at.clone().unwrap()[..10].to_string();
    let named = format!("owner decides - carried by session {S1} on {}, {S2} on {}", date(&one), date(&two));
    let note = crate::eval_session::start_note(&third);
    assert!(note.contains(&named), "the start note: {note}");
    let point = model::store::open_points(&fx.store, Some("fxp")).pop().unwrap();
    assert!(point.text.contains("the owner decides - carried by session") && point.text.contains(S2), "{}", point.text);
    let queue = model::store::owner_queue(&fx.store, "fxp");
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].1.iter().map(|c| c.session.as_str()).collect::<Vec<_>>(), vec![S1, S2]);

    let filed = valid_filing(&mut fx.store, &root, S3, &report("eval-fxp-3", "fxp", "TLDR three."), "toolu_f3");
    let text = model::store::show(&fx.store, "eval-fxp-3").unwrap().text;
    assert!(text.contains(&format!("in the owner queue: the owner decides - carried by session {S1} on")), "{text}");
    assert_eq!(filed.carry[0].carried, 2);
}

/// A filing that is a THOR call is validated at its OWN session's Stop only:
/// another session's Stop may run before the call's lines are all on disk.
/// A report that is no THOR call at all is decided by any main-session Stop.
#[test]
fn another_sessions_stop_leaves_the_filing_to_its_own() {
    let mut fx = Fx::new();
    let root = fx.root();
    attested_filing_unverified(&mut fx.store, &root, S1, &report("eval-fxp-a", "fxp", "TLDR."), "toolu_f1").unwrap();
    assert!(validate_new_reports(&mut fx.store, Some(&root), S2).is_empty(), "not S2's to decide");
    let written = validate_new_reports(&mut fx.store, Some(&root), S1);
    assert_eq!(written.len(), 1);
    assert!(written[0].valid, "{:?}", written[0]);

    let old = report("eval-beta-old", "beta", "TLDR: filed by an older binary.");
    fx.raw("mcp", EventKind::FactCreated, &old, None);
    let written = validate_new_reports(&mut fx.store, Some(&root), S2);
    assert_eq!(written.iter().map(|r| r.report.as_str()).collect::<Vec<_>>(), vec!["eval-beta-old"]);
}

// ------------------------------------------------------------- step 21

/// An old rule with NO project whose check reads a file, written raw: no
/// working copy anywhere can try it, so THOR's machine pass holds it
/// (`Blocker::global_rules`).
fn global_file_rule(fx: &mut Fx, id: &str) {
    let item = Item {
        project: None,
        ..rule(
            id,
            "Every repo keeps `eol=lf` in .gitattributes.",
            vec![Binding::Target { kind: TargetKind::Command, value: "git add".to_string() }],
            Some(Check::Contains { path: ".gitattributes".to_string(), literal: "eol=lf".to_string() }),
            "unused",
        )
    };
    let body = model::store::canonical_body(&item).unwrap();
    fx.store.append_event("legacy", id, "migration", EventKind::FactCreated, id, None, &body).unwrap();
}

/// Decision 5 of step 21: rules with no project held for lack of a working
/// copy get ONE store-wide point - not one per evaluated project - that no
/// project's owner budget counts, and whose carry is counted once across
/// every project's valid evaluations. Every project's evaluation still holds
/// those rules as blocked, never on its batch.
#[test]
fn global_rules_hold_one_store_wide_point_counted_once() {
    let mut fx = Fx::new();
    let root = fx.root();
    global_file_rule(&mut fx, "glob-file");
    let point_id = "open-no-working-copy--global-rules";

    let first = fx.start(S1, "fxp");
    let machine = first.frozen.machine.clone().unwrap();
    assert_eq!((machine.blocked, first.frozen.batch().len()), (1, 0), "blocked, never the agent's batch: {machine:?}");
    assert_eq!(machine.points.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec![point_id], "{machine:?}");
    valid_filing(&mut fx.store, &root, S1, &report("eval-fxp-1", "fxp", "TLDR one."), "toolu_f1");

    let second = fx.start(S2, "beta");
    let machine = second.frozen.machine.clone().unwrap();
    assert_eq!((machine.blocked, second.frozen.batch().len()), (1, 0), "{machine:?}");
    assert_eq!(machine.points.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec![point_id], "the same one point: {machine:?}");
    valid_filing(&mut fx.store, &root, S2, &report("eval-beta-1", "beta", "TLDR two."), "toolu_f2");

    let store_wide: Vec<_> = model::store::open_points(&fx.store, None).into_iter().filter(|p| p.machine).collect();
    assert_eq!(store_wide.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec![point_id], "one point in the whole store");
    for project in ["fxp", "beta"] {
        assert!(model::store::open_points(&fx.store, Some(project)).is_empty(), "no point of {project}'s own");
        assert!(model::store::owner_queue(&fx.store, project).is_empty(), "{project}'s budget of 8 does not count it");
    }
    let carriers = model::store::carriers_of(&fx.store, "fxp", model::store::GLOBAL_RULES_SUBJECT, i64::MAX);
    assert_eq!(carriers.iter().map(|c| c.session.as_str()).collect::<Vec<_>>(), vec![S1, S2], "carried by both projects' evaluations");
    assert_eq!(model::store::GLOBAL_RULES_POINT_ID, point_id, "the id stays stable");
    let queue = model::store::store_wide_owner_queue(&fx.store);
    assert_eq!(queue.iter().map(|(p, c)| (p.id.as_str(), c.len())).collect::<Vec<_>>(), vec![(point_id, 2)], "counted once in the owner queue");

    let third = fx.start(S3, "fxp");
    assert!(third.frozen.batch().is_empty());
    let point = model::store::open_points(&fx.store, None).into_iter().find(|p| p.id == point_id).unwrap();
    assert!(point.text.starts_with("IN THE OWNER QUEUE") && point.text.contains(S2), "{}", point.text);
}

/// The owner-queue opening of THOR's point reads as one sentence: no run of
/// spaces left by a lost line break.
#[test]
fn the_owner_queue_point_text_has_no_lost_line_break() {
    let mut fx = Fx::new();
    let root = fx.root();
    unregistered_file_rule(&mut fx, "fx-file", "fxp");
    fx.start(S1, "fxp");
    valid_filing(&mut fx.store, &root, S1, &report("eval-fxp-1", "fxp", "TLDR one."), "toolu_f1");
    fx.start(S2, "fxp");
    let point = model::store::open_points(&fx.store, Some("fxp")).pop().unwrap();
    assert!(point.text.contains("valid evaluation(s) were filed while this still held, and no agent can lift it."), "{}", point.text);
    assert!(!point.text.contains("  "), "{}", point.text);
}
