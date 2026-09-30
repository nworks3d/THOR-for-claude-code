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
    assert!(session_drifted(Some(&root), S1));
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
    assert!(!session_drifted(Some(&root), S1));
    assert!(matches!(validate(&fx.store, "eval-fxp-a", Some(&root)), Validation::Invalid(_)));
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
    for _ in 0..model::store::JUDGEMENT_DEBT_AFTER {
        crate::deliver::record_delivery_with_trigger(&mut fx.store, S1, S1, "hook", "2026-09-28T00:00:00Z", &[listed.id.clone()], Some("fx-spell"));
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
