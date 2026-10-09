//! The Stop hook validates an evaluation report (step 16 of the watertight
//! teeth build: Z1 reader, Z10, Z11, D10, D14).
//!
//! WHY A READER. The tool server refuses a filing that is plainly a side
//! door (step 15), but while the filing call runs its own lines are not in
//! the transcript yet, so the writer can only refuse on positive evidence.
//! And a report can reach the log without passing the writer at all: an
//! older binary, a repair tool, a store written by hand. Measured on a store
//! copy: a report filed with an older `mcp.exe` was stored, and the next Stop
//! never asked for the evaluation again. So nothing counts as an evaluation
//! until THIS reader has checked it, from the log and Claude Code's own
//! transcript only:
//!
//! 1. the event that filed the report is an attested THOR call of its session
//!    S - the three lines `attest::verify_write` demands, now on disk;
//! 2. an evaluation start record of (S, project) began at the log position
//!    the report's machine section names, is not void, and its start note is
//!    in S's transcript (a Stop ask, the typed command, or the skill call);
//! 3. the filing gate, run again on the log as it stood at the filing
//!    (`EventStore::truncated_copy`), lets it through;
//! 4. the machine block is byte for byte the one THOR derives from that log;
//! 5. the filing call's result carries a transcript timestamp (`filed_at`);
//! 6. no item on the evaluation's frozen lists was written in its window
//!    from outside an attested call of S (D10), judged by every verification
//!    recorded since - this same Stop records them first.
//!
//! THE ANSWER IS RECORDED ONCE: one `gate_stood_aside` by `eval-validator`
//! per filing (`model::store::record_evaluation_validation`). Every later
//! reader - the evaluation debt, doctor, the carry of THOR's own open points
//! - reads that record, never a transcript again.
//!
//! A MISSING TRANSCRIPT IS INVALID, never a way out: deleting a file does not
//! waive anything. A transcript that exists, is larger than 64 KB and holds
//! not one tool call THOR can parse, although a filing call must be in it, or
//! tool calls it fails to read (a `tool_use` structure no known line matches),
//! is FORMAT DRIFT (D14): a Claude Code update changed its shape. A large
//! transcript with no tool call at all is just a chat-only turn. Then no report counts, nothing is asked that
//! cannot be filed, and every turn says so (`DRIFT_LINE`). Rewriting a
//! transcript to look drifted is a forged artifact (N9).

use crate::attest::{self, Found, WriteAttestation};
use model::item::Item;
use model::store::{
    CarryEntry, EvaluationStart, EvaluationValidation, FilingContext, SystemActor, VerdictShares, WriteDoor,
};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use thor_core::event_store::{Event, EventKind, EventStore};

/// A transcript this large that holds no tool call of the known shape although
/// a call must be there (or holds calls in another shape) is format drift.
pub const DRIFT_MIN_BYTES: u64 = 64 * 1024;

/// What every turn says while the transcript is in a shape THOR cannot read
/// - the Stop hook and doctor alike.
pub const DRIFT_LINE: &str = "[THOR] this Claude Code version writes its transcript in a shape THOR cannot read, so no \
                              evaluation can be checked or counted until THOR is updated - tell the owner.";

/// The start of every reason a report that is not an attested THOR call does
/// not count.
pub const NOT_ATTESTED: &str = "not filed through an attested THOR tool call";

/// Why a report filed before THOR checked filings does not count (step 24),
/// in the words a reader needs: the rule stands - such a report never counts
/// - but it was not written by another program. Field case 2026-10-01: a
/// report filed through the THOR tool itself at 06:36, before the checking
/// build was installed, was described as "written by another program (the
/// command line, a repair tool or an older build)".
pub const FILED_BEFORE_CHECKS: &str = "filed before THOR 2.5 checked filings; does not count after the upgrade";

/// The log seq of the first write THOR's attester ever verified as an
/// attested THOR call - where checked filings began on this store. `None`
/// while there is none.
pub fn first_checked_write_seq(store: &EventStore) -> Option<i64> {
    let events = store.events_of_kinds(&[EventKind::GateStoodAside], 0).ok()?;
    model::store::recorded_write_verdicts(&events).into_iter().filter(|(_, v)| v.attested).map(|(seq, _)| seq).min()
}

/// Whether `event`, a filing with no stamp of THOR's tool server, came before
/// checked filings began (`first_checked_write_seq`).
fn filed_before_checks(store: &EventStore, event: &Event) -> bool {
    model::store::att_of(&event.body).is_none() && first_checked_write_seq(store).is_none_or(|first| event.seq < first)
}

/// The reason a validator record gives, in today's words: a record written
/// before step 24 for a report filed before checked filings began says
/// `FILED_BEFORE_CHECKS` instead of "written by another program".
pub fn why_in_words(store: &EventStore, v: &EvaluationValidation) -> String {
    let unstamped = v.why.starts_with(NOT_ATTESTED) && v.why.contains("carries no stamp of THOR's tool server");
    if unstamped && !v.why.contains(FILED_BEFORE_CHECKS) && first_checked_write_seq(store).is_none_or(|first| v.report_seq < first) {
        return before_checks_words();
    }
    v.why.clone()
}

/// The full reason for a report filed before checked filings began.
fn before_checks_words() -> String {
    format!("{NOT_ATTESTED} (it carries no stamp of THOR's tool server: {FILED_BEFORE_CHECKS})")
}

/// A report that counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Valid {
    pub session: String,
    pub agent: Option<String>,
    pub started_seq: i64,
    pub filed_seq: i64,
    pub filed_at: String,
    pub machine_sha256: String,
}

/// The validator's answer for one filing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validation {
    Valid(Valid),
    Invalid(String),
    /// The session's transcript is in a shape THOR cannot read (D14).
    Drift(String),
    /// Not decidable yet - the filing call has no result on disk, or a write
    /// of its window is not verified yet. Nothing is recorded; a later Stop
    /// asks again.
    Pending(String),
}

/// Everything `validate` found, for the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub validation: Validation,
    pub record: Option<EvaluationValidation>,
}

/// The event that FILED `report_id` - the last write that made it an
/// evaluation report of its project (created it, or newly tagged it, or
/// moved it to another project) - with the item as that event wrote it.
pub fn filing_event(store: &EventStore, report_id: &str) -> Option<(Event, Item)> {
    model::store::filing_event(store, report_id)
}

/// Whether the transcript `files` hold tool calls THOR cannot read although one
/// must be there (D14): the transcript is large, no line is a tool call of the
/// known shape, and EITHER some line carries a tool call in another shape OR
/// the log proves the session made a THOR call (`made_a_thor_call`: a write
/// stamped by the tool server, whose call must be in the transcript). Size
/// plus "no tool call this turn" is NEVER drift: a chat-only turn of a real
/// session is large only because of the context injected into it, and holds
/// no tool call to read at all.
pub fn transcript_drifted(files: &[PathBuf], made_a_thor_call: impl FnOnce() -> bool) -> bool {
    large_enough_to_hold_calls(files)
        && !files.iter().any(|f| holds_a_known_tool_use(f))
        && (files.iter().any(|f| holds_an_unreadable_tool_use(f)) || made_a_thor_call())
}

/// Whether the transcript `files` hold no tool call THOR can read although the
/// log proves one was made (the filing call of an attested report): such a
/// transcript is large enough that it must hold it (D14).
fn filing_call_unreadable(files: &[PathBuf]) -> bool {
    large_enough_to_hold_calls(files) && !files.iter().any(|f| holds_a_known_tool_use(f))
}

fn large_enough_to_hold_calls(files: &[PathBuf]) -> bool {
    let bytes: u64 = files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
    bytes > DRIFT_MIN_BYTES
}

/// Whether session `session`'s own transcript under `root` holds tool calls in
/// a shape THOR cannot read - the Stop hook's per-turn question. A session with
/// no transcript at all is not drift.
pub fn session_drifted(db_path: &Path, root: Option<&Path>, session: &str) -> bool {
    let Some(root) = root else { return false };
    transcript_drifted(&attest::session_transcripts(root, session), || session_made_a_thor_call(db_path, session))
}

/// Whether the log holds a write of this session stamped by THOR's tool server
/// with a call id: a call the session's transcript must hold.
fn session_made_a_thor_call(db_path: &Path, session: &str) -> bool {
    let Ok(store) = crate::store_dir::open_existing(db_path) else { return false };
    store
        .events_of_kinds_in_session(model::store::ATTESTED_WRITE_KINDS, session)
        .is_ok_and(|writes| writes.iter().any(|w| attest::claim_of(w).is_ok()))
}

/// Whether `file` holds one line of the known shape: a `message.content`
/// array with a `tool_use` block carrying a string `id` and `name`.
fn holds_a_known_tool_use(file: &Path) -> bool {
    let Ok(handle) = std::fs::File::open(file) else { return false };
    BufReader::new(handle).lines().map_while(Result::ok).filter(|line| line.contains("tool_use")).any(|line| {
        let Ok(v) = serde_json::from_str::<Value>(&line) else { return false };
        v.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()).is_some_and(|blocks| {
            blocks.iter().any(|b| {
                b.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                    && b.get("id").and_then(|i| i.as_str()).is_some()
                    && b.get("name").and_then(|n| n.as_str()).is_some()
            })
        })
    })
}

/// Whether `file` holds a line that IS a tool call by its structure - a JSON
/// key named `tool_use`, or a `type` of `tool_use` - or one that mentions a tool
/// call and does not parse as JSON at all. The word inside free text (injected
/// context, a pasted note) is neither: only structure counts.
fn holds_an_unreadable_tool_use(file: &Path) -> bool {
    let Ok(handle) = std::fs::File::open(file) else { return false };
    BufReader::new(handle).lines().map_while(Result::ok).filter(|line| line.contains("tool_use")).any(|line| {
        match serde_json::from_str::<Value>(&line) {
            Ok(v) => has_tool_use_structure(&v),
            Err(_) => true,
        }
    })
}

fn has_tool_use_structure(v: &Value) -> bool {
    match v {
        Value::Object(map) => {
            map.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                || map.contains_key("tool_use")
                || map.values().any(has_tool_use_structure)
        }
        Value::Array(items) => items.iter().any(has_tool_use_structure),
        _ => false,
    }
}

/// The texts THOR's hook put in front of the session that can carry a start
/// note: a Stop block (`hook_blocking_error`), a UserPromptSubmit context (the
/// typed command) and the context of a Skill or SlashCommand call.
fn hook_texts(attachment: &Value) -> Vec<String> {
    let field = |key: &str| attachment.get(key).and_then(|v| v.as_str()).unwrap_or_default();
    let strings = |v: Option<&Value>| -> Vec<String> {
        match v {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(parts)) => parts.iter().filter_map(|p| p.as_str().map(str::to_string)).collect(),
            _ => Vec::new(),
        }
    };
    match (field("type"), field("hookEvent")) {
        ("hook_blocking_error", "Stop") => {
            let error = attachment.get("blockingError");
            match error.and_then(|e| e.get("blockingError")) {
                Some(inner) => strings(Some(inner)),
                None => strings(error),
            }
        }
        ("hook_additional_context", "UserPromptSubmit") => strings(attachment.get("content")),
        ("hook_additional_context", "PreToolUse")
            if matches!(field("hookName"), "PreToolUse:Skill" | "PreToolUse:SlashCommand") =>
        {
            strings(attachment.get("content"))
        }
        _ => Vec::new(),
    }
}

/// Whether the start note of `start` is in the transcript `files`: a hook
/// text carrying its opening sentence with THOR's record id.
pub fn start_note_shown(files: &[PathBuf], start: &EvaluationStart) -> bool {
    let opening =
        format!("[THOR] The evaluation of '{}' started at log position {} (", start.frozen.project, start.frozen.started_seq);
    let record = format!("THOR's record '{}'", start.id);
    files.iter().any(|file| {
        let Ok(handle) = std::fs::File::open(file) else { return false };
        BufReader::new(handle).lines().map_while(Result::ok).filter(|line| line.contains("THOR's record")).any(|line| {
            let Ok(v) = serde_json::from_str::<Value>(&line) else { return false };
            let Some(attachment) = v.get("attachment") else { return false };
            hook_texts(attachment).iter().any(|text| text.contains(&opening) && text.contains(&record))
        })
    })
}

/// The unix time of a transcript timestamp: `2026-09-28T10:00:00.123Z` or
/// the same without the fraction.
pub fn unix_of_timestamp(at: &str) -> Option<i64> {
    let whole = match at.split_once('.') {
        Some((head, _)) => format!("{head}Z"),
        None => at.to_string(),
    };
    crate::time::unix_from_iso8601(&whole)
}

/// THE VALIDATOR (see this module's doc comment): whether the filing of
/// evaluation report `report_id` counts, from the log and the transcripts
/// under `transcripts_root` only. Writes nothing.
pub fn validate(store: &EventStore, report_id: &str, transcripts_root: Option<&Path>) -> Validation {
    check(store, report_id, transcripts_root).validation
}

/// `validate`, with the record it would write.
pub fn check(store: &EventStore, report_id: &str, root: Option<&Path>) -> Checked {
    let Some((event, filed)) = filing_event(store, report_id) else {
        let why = format!("no event on the log files '{report_id}' as an evaluation report");
        return Checked { validation: Validation::Invalid(why), record: None };
    };
    let mut record = EvaluationValidation {
        valid: false,
        why: String::new(),
        drift: false,
        report: report_id.to_string(),
        report_seq: event.seq,
        project: filed.project.clone(),
        session: event.session_id.clone(),
        agent: None,
        started_seq: None,
        filed_seq: None,
        filed_at: None,
        machine_sha256: None,
        verdicts: VerdictShares::default(),
        carry: Vec::new(),
    };
    let validation = judge(store, &event, &filed, root, &mut record);
    match &validation {
        Validation::Valid(valid) => {
            record.valid = true;
            record.agent = valid.agent.clone();
        }
        Validation::Invalid(why) => record.why = why.clone(),
        Validation::Drift(why) => {
            record.why = why.clone();
            record.drift = true;
        }
        Validation::Pending(_) => return Checked { validation, record: None },
    }
    Checked { validation, record: Some(record) }
}

fn judge(store: &EventStore, event: &Event, filed: &Item, root: Option<&Path>, record: &mut EvaluationValidation) -> Validation {
    let invalid = |why: String| Validation::Invalid(why);
    // (1) a THOR call at all: a stamp, a tool call id, a Claude Code session.
    let claim = match attest::claim_of(event) {
        Ok(claim) => claim,
        Err(_) if filed_before_checks(store, event) => return invalid(before_checks_words()),
        Err(why) => return invalid(format!("{NOT_ATTESTED} ({why})")),
    };
    if !matches!(event.kind, EventKind::FactCreated | EventKind::FactRevised) {
        return invalid(format!("{NOT_ATTESTED} (log seq {} is no remember or revise)", event.seq));
    }
    let session = event.session_id.clone();
    let files = root.map(|root| attest::session_transcripts(root, &session)).unwrap_or_default();
    if files.is_empty() {
        return invalid(format!(
            "the session's transcript is gone - no transcript file of session {session} is on this machine, so its \
             filing call cannot be checked"
        ));
    }
    if filing_call_unreadable(&files) {
        return Validation::Drift(format!(
            "session {session}'s transcript is larger than 64 KB and holds no tool call in a shape THOR can read"
        ));
    }
    // (1) the three lines: the recorded verification when this Stop (or an
    // earlier one) wrote it, else read now.
    let recorded = store.get_events_by_entity(&event.entity_id).ok().and_then(|events| {
        model::store::recorded_write_verdicts(&events).remove(&event.seq)
    });
    let agent = match recorded {
        Some(verdict) if verdict.attested && verdict.session == session => verdict.agent_id,
        Some(verdict) if verdict.attested => {
            return invalid(format!("{NOT_ATTESTED} of its own session (verified for session {})", verdict.session))
        }
        Some(verdict) => return invalid(format!("{NOT_ATTESTED} ({})", verdict.why)),
        None => match attest::verify_write(event, root) {
            None => return Validation::Pending("the filing call has no result in the transcript yet".to_string()),
            Some(WriteAttestation::Unattested(why)) => return invalid(format!("{NOT_ATTESTED} ({why})")),
            Some(WriteAttestation::Attested { agent_id, .. }) => agent_id,
        },
    };
    record.agent = agent.clone();
    // (5) filed_at: the transcript timestamp of the filing call's result.
    let filed_at = match attest::find_call(&files, &claim.tool_use_id) {
        Found::Call(call) => {
            call.result_times.iter().find(|(text, _)| attest::names_seq(text, event.seq)).map(|(_, at)| at.clone())
        }
        _ => None,
    };
    let Some(filed_at) = filed_at.filter(|at| unix_of_timestamp(at).is_some()) else {
        return invalid(format!(
            "the result of its filing call ({}) carries no timestamp in session {session}'s transcript, so when it was \
             filed cannot be read",
            claim.tool_use_id
        ));
    };
    record.filed_at = Some(filed_at.clone());
    // (2) the machine block names N and F; the start record of (S, project)
    // at N, as the log stood at F.
    let Some(block) = model::gate::machine_block(&filed.text) else {
        return invalid(
            "its text carries no machine section written by THOR, so it never went through THOR's filing gate".to_string(),
        );
    };
    let digest = model::store::machine_block_digest(block);
    record.machine_sha256 = Some(digest.clone());
    let Some((n, f)) = model::gate::machine_section_range(&filed.text) else {
        return invalid("its machine section names no log positions".to_string());
    };
    record.started_seq = Some(n);
    record.filed_seq = Some(f);
    if !(n < f && f < event.seq) {
        return invalid(format!(
            "its machine section names log positions {n} to {f}, which do not both come before its own filing at log \
             seq {}",
            event.seq
        ));
    }
    let project = filed.project.clone().unwrap_or_default();
    let Ok(at_filing) = store.truncated_copy(f) else {
        return Validation::Pending("the log could not be read as it stood at the filing".to_string());
    };
    let start = model::store::evaluation_starts_of_session(&at_filing, &session)
        .into_iter()
        .filter(|s| s.frozen.project == project && s.frozen.started_seq == n)
        .max_by_key(|s| s.seq);
    let Some(start) = start else {
        return invalid(format!(
            "no evaluation of '{project}' by session {session} started at log position {n} - THOR's log holds no start \
             record for it"
        ));
    };
    let void_now = model::store::evaluation_starts_of_session(store, &session)
        .into_iter()
        .find(|s| s.id == start.id)
        .and_then(|s| s.frozen.void);
    if let Some(void) = start.frozen.void.clone().or(void_now) {
        return invalid(format!(
            "its evaluation '{}' is void: '{}' was changed at log seq {} outside an attested THOR call of it ({})",
            start.id, void.id, void.seq, void.door
        ));
    }
    if !start_note_shown(&files, &start) {
        return invalid(format!(
            "the start note of '{}' is not in session {session}'s transcript - no Stop ask, typed command or skill call \
             of this session showed it the lists the report is held to",
            start.id
        ));
    }
    // (6) D10, judged by every verification on the log now.
    let written = model::store::frozen_writes_judged(store, &start, Some(f), None);
    if let Some(outside) = written.outside.first() {
        return invalid(format!(
            "'{}' was changed at log seq {} outside an attested THOR call of this evaluation ({}) - an evaluation whose \
             list was changed behind it cannot count",
            outside.id, outside.seq, outside.door
        ));
    }
    if let Some((id, seq)) = written.pending.first() {
        return Validation::Pending(format!("the write to '{id}' at log seq {seq} is not verified yet"));
    }
    // (4) the machine block, byte for byte.
    let expected = format!(
        "{}\n\n{}\n",
        model::store::render_machine_section(&at_filing, &start, f),
        model::gate::AGENT_SECTION_BEGIN
    );
    if block != expected {
        // A section of another renderer revision differs in bytes for a reason
        // that is nobody's edit: the tool server that wrote it was built from
        // another THOR than this checker. Never turned into a pass.
        let theirs = model::gate::machine_renderer_of(&filed.text);
        let ours = model::gate::MACHINE_RENDERER_REV;
        if theirs != Some(ours) {
            let theirs = theirs.map_or("none (before the header carried one)".to_string(), |rev| rev.to_string());
            return invalid(format!(
                "{}: its machine section was rendered by format {theirs}, this checker reads format {ours} - restart \
                 Claude so the tool server is the current one, and file again; nothing was edited",
                model::store::SKEW_WHY
            ));
        }
        let line = block.lines().zip(expected.lines()).position(|(a, b)| a != b).unwrap_or_else(|| block.lines().count().min(expected.lines().count())) + 1;
        return invalid(format!(
            "its machine section is not the one THOR derives from the log for seq {n} to {f} - it was edited, or written \
             past the filing gate; it first differs at line {line} of its machine section"
        ));
    }
    // (3) the filing gate, run again on the log as it stood at F.
    let ctx = FilingContext::new(model::store::INFLIGHT_SEEN, start.clone());
    if let Some(refusal) = model::store::evaluation_filing_refusal(&at_filing, filed, ctx.as_ref()) {
        return invalid(format!(
            "the filing gate, run again on the log as it stood at seq {f}, refuses it: {}",
            refusal.problem
        ));
    }
    record.verdicts = shares(&at_filing, &start, f, &project);
    record.carry = carry(store, &at_filing, &start, &project, &session, agent.as_deref());
    Validation::Valid(Valid {
        session,
        agent,
        started_seq: n,
        filed_seq: f,
        filed_at,
        machine_sha256: digest,
    })
}

/// How the verdicts of the evaluation `start` split, on the log as it stood at
/// F (`log`): on the list (the machine section's own count), off the list,
/// and outside an attested THOR call (B7).
fn shares(log: &EventStore, start: &EvaluationStart, filed_seq: i64, project: &str) -> VerdictShares {
    let summary = model::store::machine_section_summary(log, start, filed_seq);
    let mut shares = VerdictShares {
        useful: summary.useful,
        noise: summary.noise,
        skipped: summary.skipped,
        ..VerdictShares::default()
    };
    let session = start.frozen.session.as_str();
    let window = log.events_since(start.frozen.started_seq).unwrap_or_default();
    for event in window.iter().filter(|e| matches!(e.kind, EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise)) {
        let events = log.get_events_by_entity(&event.entity_id).unwrap_or_default();
        let verdicts = model::store::recorded_write_verdicts(&events);
        match model::store::write_door(event, &verdicts, session) {
            WriteDoor::Attested { .. } => {
                if !start.frozen.judgement.contains(&event.entity_id) {
                    shares.off_list += 1;
                }
            }
            WriteDoor::Outside(_) => {
                let ours = event.session_id == session
                    || model::store::show(log, &event.entity_id)
                        .is_ok_and(|item| crate::project::applies_to(item.project.as_deref(), Some(project)));
                if ours {
                    shares.unattested += 1;
                }
            }
            // Another session's verdict (step 24): that session's own share,
            // never this evaluation's and never a side door.
            WriteDoor::Machine | WriteDoor::Pending | WriteDoor::OtherSession { .. } | WriteDoor::OtherSessionPending { .. } => {}
        }
    }
    shares
}

/// Every subject THOR's own open point held while this evaluation was filed
/// (B5): a point of the start's machine pass still live at F. `carried` is
/// how many valid evaluations carried it before this one.
fn carry(
    store: &EventStore,
    log: &EventStore,
    start: &EvaluationStart,
    project: &str,
    session: &str,
    agent: Option<&str>,
) -> Vec<CarryEntry> {
    let points = start.frozen.machine.as_ref().map(|m| m.points.clone()).unwrap_or_default();
    points
        .iter()
        .filter(|p| model::store::is_machine_point(log, &p.id))
        .map(|p| CarryEntry {
            subject: p.blocker.subject.clone(),
            carried: model::store::carried_by_valid_evaluations(store, project, &p.blocker.subject, i64::MAX),
            session: session.to_string(),
            agent: agent.map(str::to_string),
        })
        .collect()
}

fn validator() -> SystemActor {
    SystemActor::new(model::store::EVAL_VALIDATOR_ACTOR)
}

/// Record `record`, once per filing (`model::store::record_evaluation_validation`).
pub fn record_validation(store: &mut EventStore, record: &EvaluationValidation) -> Option<Event> {
    model::store::record_evaluation_validation(store, validator(), record).ok().flatten()
}

/// Every live evaluation report on the log, as (id, project).
fn live_evaluation_reports(store: &EventStore) -> Vec<(String, Option<String>)> {
    crate::live::live_items(store)
        .into_iter()
        .filter(|li| model::gate::is_evaluation_report(&li.item))
        .map(|li| (li.id, li.item.project))
        .collect()
}

/// THE STOP'S HALF (`record_validation` of every live evaluation report whose
/// current filing has no validator record yet): validated once, against the
/// transcripts under `root`, and recorded. A filing that cannot be decided
/// yet is left for a later Stop. Returns the records written.
///
/// WHOSE STOP. A filing that is a THOR tool call at all (a stamp, a tool call
/// id, a Claude Code session) is validated at a main-session Stop of its OWN
/// session `session`: that Stop has just recorded the verifications of the
/// session's writes, and every line of the filing call is on disk by then.
/// Another session's Stop could run while the call's tool_use line is not
/// written yet (Claude Code writes it late at times) and record a false
/// "not in the transcript" for good. A filing that is no THOR call at all is
/// decided by any main-session Stop: nothing on disk can change that answer.
pub fn validate_new_reports(store: &mut EventStore, root: Option<&Path>, session: &str) -> Vec<EvaluationValidation> {
    let mut written = Vec::new();
    for (id, _) in live_evaluation_reports(store) {
        let Some((event, _)) = filing_event(store, &id) else { continue };
        if model::store::evaluation_validation_of(store, &id, event.seq).is_some() {
            continue;
        }
        if event.session_id != session && attest::claim_of(&event).is_ok() {
            continue;
        }
        let Some(record) = check(store, &id, root).record else { continue };
        if record_validation(store, &record).is_some() {
            written.push(record);
        }
    }
    written
}

/// The validator's record of `report`'s CURRENT filing, when there is one.
pub fn current_validation(store: &EventStore, report: &str) -> Option<EvaluationValidation> {
    let (event, _) = filing_event(store, report)?;
    model::store::evaluation_validation_of(store, report, event.seq)
}

/// THE ONE READER OF "THIS PROJECT'S EVALUATION" (replaces the sidecar's own
/// sighting of a new report id): the newest VALID validator record of
/// `project` whose report is still live, is still an evaluation report of
/// that project, still files at the same event, and whose machine block
/// still has the digest the validator recorded - so a later typo fix below
/// the block keeps it, and anything that changes the block drops it.
pub fn newest_valid_evaluation(store: &EventStore, project: Option<&str>) -> Option<EvaluationValidation> {
    model::store::newest_valid_evaluation(store, project)
}

/// Every live evaluation report of `project` with its current filing's
/// validator record (`None`: not validated yet), newest filing first.
pub fn reports_of_project(store: &EventStore, project: &str) -> Vec<(String, i64, Option<EvaluationValidation>)> {
    let mut out: Vec<(String, i64, Option<EvaluationValidation>)> = live_evaluation_reports(store)
        .into_iter()
        .filter(|(_, p)| p.as_deref() == Some(project))
        .filter_map(|(id, _)| {
            let (event, _) = filing_event(store, &id)?;
            let validation = model::store::evaluation_validation_of(store, &id, event.seq);
            Some((id, event.seq, validation))
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// The newest report of `project` that is on file but does NOT count, when
/// it is newer than the newest one that does - what the evaluation ask names
/// first: (id, why).
pub fn newest_invalid_report(store: &EventStore, project: Option<&str>) -> Option<(String, String)> {
    let project = project?;
    let valid_seq = newest_valid_evaluation(store, Some(project)).map_or(0, |v| v.report_seq);
    reports_of_project(store, project)
        .into_iter()
        .filter(|(_, seq, _)| *seq > valid_seq)
        .find_map(|(id, _, v)| v.filter(|v| !v.valid).map(|v| (id, why_in_words(store, &v))))
}

/// What the evaluation ask says about the project's reports (field test 14,
/// C2): the id of the newest report that COUNTS (`None` when none does), and
/// every report on file after it that does not, newest first, each with the
/// validator's reason. `newest_invalid_report` names only the newest dead one,
/// so an agent took an older dead report for a counted one.
pub fn reports_not_counted(store: &EventStore, project: &str) -> (Option<String>, Vec<(String, String)>) {
    let valid = newest_valid_evaluation(store, Some(project));
    let valid_seq = valid.as_ref().map_or(0, |v| v.report_seq);
    let dead = reports_of_project(store, project)
        .into_iter()
        .filter(|(_, seq, _)| *seq > valid_seq)
        .filter_map(|(id, _, v)| v.filter(|v| !v.valid).map(|v| (id, why_in_words(store, &v))))
        .collect();
    (valid.map(|v| v.report), dead)
}

// ------------------------------------------------------- version skew, before and after

/// THE REFUSAL OF A FILING THAT CANNOT COUNT (field test 13, D1). A tool server
/// renders the machine section with its own build of the renderer; the
/// checker (`serve.exe`, run by the Stop hook) compares it byte for byte with
/// its own. When the two renderer revisions differ the report is on file but
/// dead, and the only cure is a restart of Claude - which no one knew until
/// the Stop refused it, a turn too late (four dead reports in two days).
/// This is the check the tool server runs BEFORE it writes.
///
/// WHY THE RENDERER REVISION AND NOT THE BUILD ID. The build id differs for
/// any change in core, model or serve, including ones that never touch the
/// machine section, and a tool server (mcp.exe) that is simply not part of a
/// partial install would then refuse every filing for good. The revision is
/// the one number the checker compares, so it is the one refused on. It is
/// read from the installed checker's own `--version` line (`serve::renderer_in`).
///
/// `checker_version`: the installed checker's `--version` output, `None` when
/// it could not be run or prints no revision (an older program) - then
/// nothing is refused: this is a courtesy ahead of the Stop, never a new way
/// to block a filing.
pub fn checker_skew_refusal(own_revision: u32, checker_version: Option<&str>) -> Option<String> {
    let theirs = crate::renderer_in(checker_version?)?;
    if theirs == own_revision {
        return None;
    }
    Some(format!(
        "REFUSED: this tool server renders the evaluation machine section in format {own_revision}, but the installed checker \
         (serve.exe) reads format {theirs}, so a report filed now would be on file and would not count. Nothing was written. \
         WHAT TO DO: tell the owner to restart Claude (the tool server loads only when Claude starts), and end your turn; the \
         next end-of-turn ask starts the evaluation again and you file the report then. DONE WHEN: the owner restarted Claude \
         and the ask came again. Do not retry the filing in this session: it would be refused the same way."
    ))
}

/// `serve --version` of the checker installed beside `bin_dir`'s tool server,
/// run without a window and given a few seconds. `None` when it is not there,
/// cannot start or takes too long (the caller then refuses nothing).
pub fn installed_checker_version(bin_dir: &Path) -> Option<String> {
    use std::process::{Command, Stdio};
    let exe = bin_dir.join(format!("serve{}", std::env::consts::EXE_SUFFIX));
    if !exe.is_file() {
        return None;
    }
    let mut command = Command::new(&exe);
    command.arg("--version").stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn().ok()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait().ok()? {
            Some(status) if status.success() => break,
            Some(_) => return None,
            None if started.elapsed() > std::time::Duration::from_secs(5) => {
                let _ = child.kill();
                return None;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out)
}

/// A report of `project` that THIS session filed was refused as version skew,
/// and the session has not been restarted since (field test 13, D2). The Stop
/// then must not start a fresh evaluation and hold the turn for it: the
/// session's tool server is still the old build, so the second report would be
/// dead the same way and the hold would leave no other way out. Returns the
/// one plain line to say instead; `None` when there is no such report, when
/// the session started again after it (a restart resumes the same session id,
/// and its transcript holds a `SessionStart:startup` or `SessionStart:resume`
/// record after the filing - then it is asked normally), or when its
/// transcript cannot be read (fail open: the ask is never lost to a missing
/// file).
pub fn skew_awaiting_restart(store: &EventStore, project: &str, session: &str, root: Option<&Path>) -> Option<String> {
    let valid_seq = newest_valid_evaluation(store, Some(project)).map_or(0, |v| v.report_seq);
    // THIS session's newest skew-refused report: another session's newer dead
    // report of the project must not hide it (field test 14, H1).
    let (id, skew) = reports_of_project(store, project)
        .into_iter()
        .filter(|(_, seq, _)| *seq > valid_seq)
        .find_map(|(id, _, v)| {
            v.filter(|v| !v.valid && v.session == session && v.why.starts_with(model::store::SKEW_WHY)).map(|v| (id, v))
        })?;
    let filed = skew.filed_at.as_deref().and_then(unix_of_timestamp)?;
    let files = attest::session_transcripts(root?, session);
    if files.is_empty() || restarted_after(&files, filed) {
        return None;
    }
    Some(format!(
        "[THOR] The report '{id}' is on file but does not count: this session's tool server is an older build than the \
         checker. Restart Claude; the evaluation is asked again after the restart, and you file the report then. This \
         Stop does not hold the turn."
    ))
}

/// THIS SESSION'S TOOL SERVER IS OLDER THAN THE INSTALLED ONE (field test 14,
/// H1): the session's last `SessionStart:startup` or `SessionStart:resume` in
/// its transcript is older than the modification time of the installed tool
/// server (`mcp_exe`). A tool server loads only when Claude starts, so a
/// report filed now would be built by the old program and would be dead; the
/// Stop must not ask for it nor hold the turn for it. Returns the one plain
/// line to say instead. `None` (fail open: the ask is never lost) when the
/// installed program or its time cannot be read, when the transcript is
/// missing or records no start, or when the session started after the install.
pub fn tool_server_stale(session: &str, root: Option<&Path>, mcp_exe: Option<&Path>) -> Option<String> {
    let installed = std::fs::metadata(mcp_exe?).ok()?.modified().ok()?;
    let installed = installed.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    let files = attest::session_transcripts(root?, session);
    let started = last_session_start(&files)?;
    if started >= installed {
        return None;
    }
    Some(format!(
        "[THOR] This session's tool server is older than the installed one (the session started at {}, the tool server \
         was replaced at {}). A report filed now would be on file and would not count. Restart Claude; the evaluation is \
         asked again after the restart, and you file the report then. This Stop does not hold the turn.",
        crate::time::iso8601_from_unix(started),
        crate::time::iso8601_from_unix(installed)
    ))
}

/// The newest `SessionStart:startup` or `SessionStart:resume` time (unix
/// seconds) recorded in `files`; a compaction restarts no tool server.
fn last_session_start(files: &[PathBuf]) -> Option<i64> {
    files
        .iter()
        .filter_map(|file| std::fs::File::open(file).ok())
        .flat_map(|handle| {
            BufReader::new(handle).lines().map_while(Result::ok).filter_map(|line| {
                if !line.contains("\"hookName\":\"SessionStart:") {
                    return None;
                }
                let record = serde_json::from_str::<Value>(&line).ok()?;
                let name = record.pointer("/attachment/hookName").and_then(Value::as_str)?;
                if !matches!(name, "SessionStart:startup" | "SessionStart:resume") {
                    return None;
                }
                record.get("timestamp").and_then(Value::as_str).and_then(unix_of_timestamp)
            })
        })
        .max()
}

/// Whether any of `files` records the session starting (or resuming) after
/// `after_unix`. A compaction restarts no tool server and does not count.
fn restarted_after(files: &[PathBuf], after_unix: i64) -> bool {
    files.iter().any(|file| {
        let Ok(handle) = std::fs::File::open(file) else { return false };
        BufReader::new(handle).lines().map_while(Result::ok).any(|line| {
            if !line.contains("\"hookName\":\"SessionStart:") {
                return false;
            }
            let Ok(record) = serde_json::from_str::<Value>(&line) else { return false };
            let name = record.pointer("/attachment/hookName").and_then(Value::as_str).unwrap_or_default();
            let at = record.get("timestamp").and_then(Value::as_str).and_then(unix_of_timestamp);
            matches!(name, "SessionStart:startup" | "SessionStart:resume") && at.is_some_and(|at| at > after_unix)
        })
    })
}

/// WHETHER `project` SHOWS ROT, for the weekly cadence (step 17), from the log
/// only: why it does, or `None` for a clean project. Rot is any of: no valid
/// evaluation at all; the newest valid evaluation's start had anything to do
/// (a judgement list, a teeth batch, an A4 list or THOR's own open points);
/// since that evaluation, a report that does not count, a write to an item
/// of the project outside an attested THOR call, a live rule of the project
/// that `settlement` calls owed, or judgement debt in the project.
pub fn project_rot(store: &EventStore, project: &str) -> Option<String> {
    let Some(valid) = newest_valid_evaluation(store, Some(project)) else {
        return Some("no evaluation of it counts".to_string());
    };
    let start = model::store::evaluation_starts_of_session(store, &valid.session)
        .into_iter()
        .find(|s| s.frozen.project == project && Some(s.frozen.started_seq) == valid.started_seq);
    if let Some(start) = start {
        let machine_points = start.frozen.machine.as_ref().map_or(0, |m| m.points.len());
        let to_do = start.frozen.judgement.len()
            + start.frozen.batch().len()
            + start.frozen.a4_list().len()
            + machine_points;
        if to_do > 0 {
            return Some(format!("its last evaluation ('{}') started with {to_do} thing(s) to do", valid.report));
        }
    }
    if let Some((id, _)) = newest_invalid_report(store, Some(project)) {
        return Some(format!("the report '{id}' does not count"));
    }
    let ids = crate::usefulness::items_of_project(store, project);
    let writes = store.events_of_kinds(model::store::ATTESTED_WRITE_KINDS, valid.report_seq).unwrap_or_default();
    for event in writes.iter().filter(|e| ids.contains(&e.entity_id)) {
        let events = store.get_events_by_entity(&event.entity_id).unwrap_or_default();
        let verdicts = model::store::recorded_write_verdicts(&events);
        if let WriteDoor::Outside(why) = model::store::write_door(event, &verdicts, &event.session_id) {
            return Some(format!("'{}' was written at log seq {} outside an attested THOR call ({why})", event.entity_id, event.seq));
        }
    }
    let rot = crate::live::live_items(store)
        .into_iter()
        .filter(|li| li.item.project.as_deref() == Some(project) && li.item.kind.can_fire())
        .find(|li| model::store::settlement(store, &li.id, None).is_owed());
    if let Some(li) = rot {
        return Some(format!("the rule '{}' has no verdict (rot)", li.id));
    }
    let (_, owed) = crate::usefulness::judgement_debt_counts(store, Some(project));
    (owed > 0).then(|| format!("{owed} item(s) owe a verdict"))
}

/// Whether the newest validator record on the log says the transcript was in
/// a shape THOR cannot read - doctor's cold read of D14.
pub fn drift_recorded(store: &EventStore) -> bool {
    model::store::evaluation_validations(store).last().is_some_and(|(_, v)| v.drift)
}

/// The one-way flag (B7): at least this many verdicts on an evaluation's
/// list, every one the same way.
pub const ONE_WAY_AT: usize = 10;

/// "eval-<id>: <u> useful, <n> noise, <s> skipped (<p>% useful)" for one
/// valid evaluation - `p` rounded, and "no verdict" when none was given.
pub fn share_line(v: &EvaluationValidation) -> String {
    let given = v.verdicts.useful + v.verdicts.noise;
    let share = match given {
        0 => "no verdict".to_string(),
        _ => format!("{}% useful", (v.verdicts.useful * 100 + given / 2) / given),
    };
    format!("{}: {} useful, {} noise, {} skipped ({share})", v.report, v.verdicts.useful, v.verdicts.noise, v.verdicts.skipped)
}

/// Whether the verdicts of one valid evaluation all went one way, with at
/// least `ONE_WAY_AT` of them.
pub fn went_one_way(v: &EvaluationValidation) -> bool {
    let given = v.verdicts.useful + v.verdicts.noise;
    given >= ONE_WAY_AT && (v.verdicts.useful == 0 || v.verdicts.noise == 0)
}

/// TEST SUPPORT, shared by every crate whose fixture needs an evaluation
/// that COUNTS: filed the way Claude Code files one, then validated the way
/// the next Stop validates it. Writes only under the folder it is given.
pub mod fixture {
    use super::*;

    /// `attest::fixture::attested_filing` of `report` by `session` (a
    /// UUID-shaped id - only such a session has a transcript), then this
    /// module's validation against the transcripts under `root`, recorded.
    /// Returns the validator's record of it; panics when the filing is
    /// refused or the record is not valid, naming why.
    pub fn valid_filing(store: &mut EventStore, root: &Path, session: &str, report: &Item, tool_use_id: &str) -> EvaluationValidation {
        attest::fixture::attested_filing(store, root, session, report, tool_use_id)
            .unwrap_or_else(|e| panic!("the fixture filing of '{}' was refused: {e}", report.id));
        validated(store, root, session, report)
    }

    /// `valid_filing`, filed at unix time `at` as far as the transcript
    /// tells - the time the report's coverage is measured from.
    pub fn valid_filing_at(
        store: &mut EventStore,
        root: &Path,
        session: &str,
        report: &Item,
        tool_use_id: &str,
        at: i64,
    ) -> EvaluationValidation {
        let stamp = crate::time::iso8601_from_unix(at);
        attest::fixture::attested_filing_unverified_at(store, root, session, report, tool_use_id, &stamp)
            .unwrap_or_else(|e| panic!("the fixture filing of '{}' was refused: {e}", report.id));
        attest::record_write_verifications_in(store, session, Some(root));
        validated(store, root, session, report)
    }

    fn validated(store: &mut EventStore, root: &Path, session: &str, report: &Item) -> EvaluationValidation {
        validate_new_reports(store, Some(root), session);
        let record = current_validation(store, &report.id).expect("the Stop validated the filing");
        assert!(record.valid, "the fixture filing of '{}' does not count: {}", report.id, record.why);
        record
    }

    /// Close THOR's own open point `id` the way the machine pass closes one
    /// whose subject settled (`eval-machine`, with a derived reason).
    pub fn close_point(store: &mut EventStore, session: &str, id: &str) {
        let machine = SystemActor::new(model::store::EVAL_MACHINE_ACTOR);
        model::store::retract_machine_point(store, machine, session, id, "fixture: its subject settled")
            .expect("the fixture point closes");
    }

    /// An evaluation report of `project` with `text` as its TLDR.
    pub fn report(id: &str, project: &str, text: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: model::item::Kind::Report,
            text: text.to_string(),
            bindings: vec![],
            severity: None,
            project: Some(project.to_string()),
            tags: vec![model::store::EVALUATION_REPORT_TAG.to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        }
    }
}

#[cfg(test)]
mod tests;
