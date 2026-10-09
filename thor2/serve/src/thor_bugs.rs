//! THOR finds THOR's own bugs from its own signals, and hands them upstream.
//!
//! THE GAP THIS CLOSES. Everything below used to depend on a person reading
//! evaluation reports and noticing a defect: a report that was refused as
//! version skew, a hook that never started, a start note that had to leave
//! an item out. THOR already records each of those. This module turns the ones
//! that point at THOR (and not at a user's rule or work) into CANDIDATES with a
//! stable fingerprint, keeps them in a sidecar beside the store, and once per
//! session offers ONE to the agent as a soft nudge (`bug_report::nudge`): draft
//! an issue for the public THOR repository from THOR's evidence, and publish
//! it only with the user's own go.
//!
//! WHICH SIGNALS COUNT. Only ones a user cannot cause by writing rules:
//! - a report refused as version skew, as a machine-section mismatch with the
//!   same format on both sides, or by the filing gate re-run at the Stop;
//! - a hook launch failure that is not host-wide (the machine going to sleep);
//! - several unrelated rules loosened after refusing the same kind of call (a
//!   pattern of false blocks, not one rule's wording) - and a sweep hit, for
//!   the sweep that replays real calls (`sweep_finding`, called by its owner);
//! - a panic inside a hook (`record_panic`);
//! - the write gate refusing one of THOR's own starting notes (`add`, called by
//!   the installer);
//! - a start note that had to cut an item that needs action;
//! - a doctor line the installed evaluation routine never mentions (`routine_gaps`,
//!   called by doctor).
//!
//! WHAT A CANDIDATE IS NOT. It carries no rule id, no project name, no path:
//! the evidence is built from counts, formats, exit codes and THOR's own
//! source locations. `bug_report::scrub` still runs over the whole write-up,
//! as a second line.
//!
//! STATUS. `new` (found), `presented` (offered to an agent), `reported` (a live
//! Report tagged `thor-bug:<fingerprint>` records the outcome - the agent's
//! `remember` call is the only way to say so, no new tool), `resolved` (that
//! report, or any note, is also tagged `thor-bug-resolved:<fingerprint>`, or the
//! signal stopped after an upgrade). A presented candidate nobody reported is
//! offered once more after a day, then left alone.
//!
//! MODE. `thor-mode.json` beside the store says `release` or `test`
//! (`install --mode`). With no file, nothing is offered at all: THOR never
//! guesses that it may talk about publishing.
//!
//! Best effort like every sidecar here: a file that cannot be read or written
//! loses a nudge, never a turn.

use crate::bug_report::{self, Known, Mode, WriteUp};
use model::store::{EvaluationStart, ATTESTER_ACTOR, SKEW_WHY};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thor_core::event_store::{EventKind, EventStore};

/// The candidates sidecar beside the store.
pub const FILE: &str = "thor-bug-candidates.json";
/// The mode sidecar beside the store: `{"mode": "release" | "test"}`.
pub const MODE_FILE: &str = "thor-mode.json";
/// Hook panics, newest last.
pub const PANIC_FILE: &str = "hook-panics.json";
/// The tag of the outcome Report: `thor-bug:<fingerprint>`.
pub const REPORT_TAG_PREFIX: &str = model::store::THOR_BUG_TAG_PREFIX;
/// The tag that closes a candidate: `thor-bug-resolved:<fingerprint>`.
pub const RESOLVED_TAG_PREFIX: &str = "thor-bug-resolved:";
/// A presented candidate nobody reported is offered again after this long.
pub const RENUDGE_AFTER_SECONDS: i64 = 24 * 3600;
/// ... and never more often than this in all.
pub const MAX_PRESENTATIONS: u32 = 2;
/// How many sessions the sidecar remembers having nudged.
const SESSIONS_KEPT: usize = 50;
/// How many panics the panic file keeps.
const PANICS_KEPT: usize = 20;
/// The most evidence lines a candidate keeps, and the longest one.
const EVIDENCE_LINES: usize = 5;
const EVIDENCE_LINE_CHARS: usize = 300;
/// A scan is not repeated within this many seconds.
pub const SCAN_EVERY_SECONDS: i64 = 300;
/// The first scan of a store with no watermark reads the log from this far
/// back (events), not from its beginning.
pub const FIRST_SCAN_EVENTS: i64 = 100_000;

// ------------------------------------------------------------------ signals

/// Which of THOR's own signals a candidate came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Signal {
    FilingSkew,
    FilingMismatch,
    FilingGate,
    HookLaunch,
    HookPanic,
    FalseBlockPattern,
    Sweep,
    SeedRefused,
    StartNoteCut,
    DoctorRoutine,
}

impl Signal {
    pub fn as_str(self) -> &'static str {
        match self {
            Signal::FilingSkew => "filing-skew",
            Signal::FilingMismatch => "filing-mismatch",
            Signal::FilingGate => "filing-gate",
            Signal::HookLaunch => "hook-launch",
            Signal::HookPanic => "hook-panic",
            Signal::FalseBlockPattern => "false-block-pattern",
            Signal::Sweep => "sweep",
            Signal::SeedRefused => "seed-refused",
            Signal::StartNoteCut => "start-note-cut",
            Signal::DoctorRoutine => "doctor-routine",
        }
    }
}

/// What a detector saw, before it has a status. `key` is what makes two
/// sightings the same bug; it never holds a path, a name or a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub signal: Signal,
    pub key: String,
    pub summary: String,
    pub why_thor: String,
    pub evidence: Vec<String>,
    pub expected: String,
    pub actual: String,
    pub fix_area: String,
}

impl Finding {
    /// The stable name of this bug: the signal and a hash of its key. The
    /// same bug seen again - another session, another day - has the same one.
    pub fn fingerprint(&self) -> String {
        let hash = thor_core::event_store::hash_event("thor-bug-fingerprint-v1", &format!("{}|{}", self.signal.as_str(), self.key));
        format!("{}-{}", self.signal.as_str(), &hash[..8])
    }

    fn capped(mut self) -> Self {
        self.evidence.truncate(EVIDENCE_LINES);
        for line in &mut self.evidence {
            if line.chars().count() > EVIDENCE_LINE_CHARS {
                *line = format!("{}...", line.chars().take(EVIDENCE_LINE_CHARS).collect::<String>());
            }
        }
        self
    }
}

/// A finished evaluation report that did not count, from the validator's
/// reason (`eval_filing::judge`). `None` for every reason that is the agent's
/// or the machine's and not THOR's: not attested, void, a missing transcript,
/// a write outside the tools, a drifted transcript.
pub fn filing_finding(why: &str) -> Option<Finding> {
    if why.starts_with(SKEW_WHY) {
        let theirs = between(why, "rendered by format ", ", this checker reads format ").unwrap_or("?").to_string();
        let ours = why.split("this checker reads format ").nth(1).and_then(|r| r.split(' ').next()).unwrap_or("?").to_string();
        return Some(Finding {
            signal: Signal::FilingSkew,
            key: format!("{theirs}>{ours}"),
            summary: format!(
                "A finished evaluation report was refused because the tool server rendered its machine section in \
                 format {theirs} while the checker reads format {ours}."
            ),
            why_thor: "Both programs are THOR's own and ship together; neither an agent nor a user's rule can change a \
                       renderer format. Either the write-time check that refuses such a filing before it is written did \
                       not run, or the install left the two programs at different builds."
                .to_string(),
            evidence: vec![
                format!("the validator's record says the report does not count, with the version-skew reason"),
                format!("tool server format: {theirs}; checker format: {ours}"),
            ],
            expected: "After an upgrade a filing either counts, or is refused before it is written with the instruction \
                       to restart Claude."
                .to_string(),
            actual: "The report was written, and refused only at the next end of turn, so the evaluation was lost.".to_string(),
            fix_area: "eval_filing::checker_skew_refusal in serve, and the installer's step that replaces the tool server".to_string(),
        });
    }
    if why.contains("its machine section is not the one THOR derives from the log") {
        return Some(Finding {
            signal: Signal::FilingMismatch,
            key: format!("mismatch-rev{}", model::gate::MACHINE_RENDERER_REV),
            summary: "A finished evaluation report was refused because its machine section differs from the one the \
                      checker derives from the log, although both use the same format."
                .to_string(),
            why_thor: "The machine section is written by THOR from the log, not typed by the agent. With the same format \
                       on both sides, a difference means the renderer is not deterministic, or the log the checker replays \
                       is not the log the writer read."
                .to_string(),
            evidence: vec![
                "the validator's record says the machine section is not the derived one".to_string(),
                format!("renderer format on both sides: {}", model::gate::MACHINE_RENDERER_REV),
            ],
            expected: "The same log renders to the same machine section, byte for byte, in the tool server and in the checker."
                .to_string(),
            actual: "The two renderings differed and the report did not count.".to_string(),
            fix_area: "model::store::render_machine_section and the truncated_copy replay in serve::eval_filing".to_string(),
        });
    }
    if why.starts_with("the filing gate, run again on the log as it stood") {
        return Some(Finding {
            signal: Signal::FilingGate,
            key: "gate-rerun".to_string(),
            summary: "The filing gate, run again at the end of the turn, refused a report the write-time gate had accepted."
                .to_string(),
            why_thor: "The same gate ran twice on the same log and gave two answers, so one of the two paths differs from \
                       the other; that is not something the agent's text decides."
                .to_string(),
            evidence: vec!["the validator's record says the re-run of the filing gate refused the report".to_string()],
            expected: "The write-time gate and the Stop-time re-run agree.".to_string(),
            actual: "The write was accepted and then judged not to count.".to_string(),
            fix_area: "model::store::evaluation_filing_refusal and its two callers (mcp write, serve::eval_filing)".to_string(),
        });
    }
    None
}

fn between<'a>(text: &'a str, from: &str, to: &str) -> Option<&'a str> {
    let start = text.find(from)? + from.len();
    let end = text[start..].find(to)? + start;
    Some(&text[start..end])
}

/// A hook launch failure that was not the machine going to sleep.
pub fn hook_launch_finding(event: &str, exit_code: i64) -> Finding {
    let cause = crate::hook_faults::cause_in_words(exit_code, "");
    Finding {
        signal: Signal::HookLaunch,
        key: format!("{event}:{exit_code}"),
        summary: format!("Claude Code could not start THOR's {event} hook (exit code {exit_code})."),
        why_thor: "No other program's hook failed at the same moment, so the machine was not suspending; the failing \
                   program is THOR's own hook command."
            .to_string(),
        evidence: vec![
            format!("hook event: {event}; exit code: {exit_code}"),
            format!("what that code usually means: {cause}"),
        ],
        expected: "The hook starts and answers.".to_string(),
        actual: "Claude Code reported a launch failure and carried on without THOR, so the memory was silent.".to_string(),
        fix_area: "the command the installer writes for the hook (ops::install::standard_hooks) and how the programs are packaged"
            .to_string(),
    }
}

/// A panic inside a hook: always THOR's.
pub fn hook_panic_finding(event: &str, location: &str) -> Finding {
    Finding {
        signal: Signal::HookPanic,
        key: location.to_string(),
        summary: format!("THOR's {event} hook panicked at {location} and said nothing."),
        why_thor: "A panic is a defect in the program by definition; the hook fails open and silent, so nothing else ever \
                   reports it."
            .to_string(),
        evidence: vec![format!("hook event: {event}"), format!("source location: {location}")],
        expected: "The hook answers, or stays silent on purpose.".to_string(),
        actual: "The hook failed internally after starting and wrote nothing.".to_string(),
        fix_area: location.to_string(),
    }
}

/// Several unrelated rules loosened after refusing the same kind of call.
pub fn false_block_pattern_finding(command_word: &str, rules: usize, blocks: usize) -> Finding {
    Finding {
        signal: Signal::FalseBlockPattern,
        key: command_word.to_string(),
        summary: format!(
            "{rules} different rules were weakened or retracted soon after refusing calls that start with '{command_word}'."
        ),
        why_thor: "When unrelated rules are loosened after refusing the same kind of call, the common factor is the engine \
                   that decided to refuse, not any one rule's wording."
            .to_string(),
        evidence: vec![format!("{blocks} refusals of calls starting with '{command_word}', each followed by a weakening or retract by someone other than THOR")],
        expected: "A rule refuses the call it forbids and lets the ordinary calls of the same program through.".to_string(),
        actual: "Ordinary calls were refused, and the rules were then loosened by hand.".to_string(),
        fix_area: "serve::guard_verdict and serve::shell (how a command is split and matched), and the trial in serve::teeth and serve::legit"
            .to_string(),
    }
}

/// What a sweep over real calls found about one engine class. The sweep that
/// replays real calls against rules calls `sweep_finding` and then `add`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepHit {
    /// The engine behaviour, without a rule id: "tool pseudo-segments", "Monitor loops", ...
    pub engine_class: String,
    pub refused: usize,
    pub of_calls: usize,
    /// The SHAPE of one refused call (a command word and its flags), no paths.
    pub example_shape: String,
}

pub fn sweep_finding(hit: &SweepHit) -> Finding {
    Finding {
        signal: Signal::Sweep,
        key: hit.engine_class.clone(),
        summary: format!("A replay of real calls found the guard refusing ordinary ones ({}).", hit.engine_class),
        why_thor: "The sweep replays real calls against every rule that can refuse; a class of calls refused across \
                   several rules points at the engine's reading of the call, not at one rule."
            .to_string(),
        evidence: vec![
            format!("engine class: {}", hit.engine_class),
            format!("{} of {} replayed calls were refused", hit.refused, hit.of_calls),
            format!("shape of one refused call: {}", hit.example_shape),
        ],
        expected: "Calls that do not do what a rule forbids pass.".to_string(),
        actual: "They were refused in the replay.".to_string(),
        fix_area: format!("the guard's handling of: {}", hit.engine_class),
    }
}

/// The write gate refused one of THOR's own starting notes.
pub fn seed_refused_finding(note_id: &str, refusal: &str) -> Finding {
    let first: String = refusal.lines().next().unwrap_or("").chars().take(200).collect();
    Finding {
        signal: Signal::SeedRefused,
        key: note_id.to_string(),
        summary: format!("The write gate refused THOR's own starting note '{note_id}' at install."),
        why_thor: "The starting notes ship with THOR and are written by the installer; a user cannot have caused their \
                   refusal, so the note and the gate disagree."
            .to_string(),
        evidence: vec![format!("starting note: {note_id}"), format!("the gate's first line: {first}")],
        expected: "Every starting note passes the gate it ships with.".to_string(),
        actual: "The note was refused and is missing from a new memory.".to_string(),
        fix_area: "ops::install::working_contract against model::gate".to_string(),
    }
}

/// A start note that had to leave out items that need action.
pub fn start_note_cut_finding(lists: &[&str]) -> Finding {
    let names = lists.join(", ");
    Finding {
        signal: Signal::StartNoteCut,
        key: names.clone(),
        summary: format!("The evaluation start note had to leave out items of: {names}."),
        why_thor: "The size budget and the order in which lists are given up are THOR's; items that need action were held \
                   back, so the evaluation cannot be completed from the note alone."
            .to_string(),
        evidence: vec![
            format!("lists with items not shown: {names}"),
            "the note says how many are held back and where the whole note is".to_string(),
        ],
        expected: "Every item that needs action is in the note the agent is shown.".to_string(),
        actual: "The note was cut to fit the size Claude Code shows whole.".to_string(),
        fix_area: "serve::eval_session::START_NOTE_BUDGET and Caps::shrink".to_string(),
    }
}

/// A doctor line the installed routine does not mention.
pub fn doctor_routine_finding(label: &str) -> Finding {
    Finding {
        signal: Signal::DoctorRoutine,
        key: label.to_string(),
        summary: format!("doctor prints a line labelled '{label}:' that the installed evaluation routine never mentions."),
        why_thor: "Doctor and the routine are written together and ship together; the routine is what tells the agent what \
                   each doctor line means and what to do about it, so a line it does not know is read wrongly or skipped."
            .to_string(),
        evidence: vec![format!("doctor line label: {label}"), "the installed routine text does not contain the label".to_string()],
        expected: "Every line doctor --eval prints is described in the routine.".to_string(),
        actual: "The routine is silent about it.".to_string(),
        fix_area: "ops::health::eval_report against thor2/eval-command.example.md".to_string(),
    }
}

/// The labels of the `lines` (the text before the first colon of an unindented
/// line) that `routine` never mentions. Case does not matter. A line without
/// a short label (no colon within 40 characters) has none to check.
pub fn routine_gaps(routine: &str, lines: &[String]) -> Vec<String> {
    let routine = routine.to_ascii_lowercase();
    let mut gaps: Vec<String> = Vec::new();
    for line in lines {
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let Some((label, _)) = line.split_once(':') else { continue };
        let label = label.trim().to_ascii_lowercase();
        let plain = !label.is_empty()
            && label.len() <= 40
            && label.chars().all(|c| c.is_ascii_lowercase() || c == ' ' || c == '-' || c == '(' || c == ')' || c.is_ascii_digit());
        if plain && !routine.contains(&label) && !gaps.contains(&label) {
            gaps.push(label);
        }
    }
    gaps
}

// ------------------------------------------------------------------- the book

/// Where a candidate stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    New,
    Presented,
    Reported,
    Resolved,
}

/// A bug candidate: a finding with its history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub fingerprint: String,
    #[serde(flatten)]
    pub finding: Finding,
    /// "2.5.0 (build <id>, renderer <n>)" of the build that last saw it.
    pub version: String,
    pub first_seen: i64,
    pub last_seen: i64,
    pub seen: u32,
    pub status: Status,
    #[serde(default)]
    pub presented_unix: Option<i64>,
    #[serde(default)]
    pub presentations: u32,
}

/// The whole sidecar.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    /// The log position the last scan read to.
    #[serde(default)]
    pub scanned_seq: i64,
    /// When the last scan ran (unix seconds): a hook never scans twice within
    /// `SCAN_EVERY_SECONDS`.
    #[serde(default)]
    pub last_scan_unix: i64,
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    /// Sessions that were nudged, by unix time: at most one candidate each.
    #[serde(default)]
    pub nudged: BTreeMap<String, i64>,
}

fn sidecar(db: &Path, name: &str) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(name)
}

pub fn load(db: &Path) -> Book {
    std::fs::read_to_string(sidecar(db, FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save(db: &Path, book: &Book) {
    if let Ok(text) = serde_json::to_string_pretty(book) {
        let _ = crate::store_dir::write(&sidecar(db, FILE), text);
    }
}

/// "2.5.0 (build <id>, renderer <n>)": which THOR this is.
pub fn version_text() -> String {
    format!("{} (build {}, renderer {})", env!("CARGO_PKG_VERSION"), crate::BUILD_ID, model::gate::MACHINE_RENDERER_REV)
}

/// Note a sighting: a new candidate, or one more sighting of a known one.
/// Returns whether the candidate is new. A resolved candidate seen again by a
/// LATER build is reopened; by the same build it stays resolved.
pub fn upsert(book: &mut Book, finding: Finding, now: i64, version: &str) -> bool {
    let finding = finding.capped();
    let fingerprint = finding.fingerprint();
    if let Some(known) = book.candidates.iter_mut().find(|c| c.fingerprint == fingerprint) {
        known.last_seen = now;
        known.seen = known.seen.saturating_add(1);
        if known.status == Status::Resolved && known.version != version {
            known.status = Status::New;
            known.presented_unix = None;
            known.presentations = 0;
        }
        known.version = version.to_string();
        known.finding = finding;
        return false;
    }
    book.candidates.push(Candidate {
        fingerprint,
        finding,
        version: version.to_string(),
        first_seen: now,
        last_seen: now,
        seen: 1,
        status: Status::New,
        presented_unix: None,
        presentations: 0,
    });
    true
}

/// Record one finding at once, from a caller that knows the store (the
/// installer, doctor, the sweep). Returns whether it is a new candidate.
pub fn add(db: &Path, finding: Finding) -> bool {
    let mut book = load(db);
    let new = upsert(&mut book, finding, crate::time::now_unix(), &version_text());
    save(db, &book);
    new
}

// ----------------------------------------------------------------------- mode

/// The mode the install wrote, `None` when it wrote none.
pub fn read_mode(db: &Path) -> Option<Mode> {
    let text = std::fs::read_to_string(sidecar(db, MODE_FILE)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    match value.get("mode")?.as_str()? {
        "release" => Some(Mode::Release),
        "test" => Some(Mode::Test),
        _ => None,
    }
}

pub fn mode_word(mode: Mode) -> &'static str {
    match mode {
        Mode::Release => "release",
        Mode::Test => "test",
    }
}

/// Write the mode. Called by the installer, never by a hook.
pub fn write_mode(db: &Path, mode: Mode) -> std::io::Result<()> {
    let text = serde_json::json!({ "mode": mode_word(mode) }).to_string();
    std::fs::write(sidecar(db, MODE_FILE), text)
}

// --------------------------------------------------------------------- panics

/// One hook panic as the sidecar keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanicRecord {
    pub at: i64,
    pub event: String,
    /// `file:line` of THOR's own source.
    pub location: String,
}

fn read_panics(db: &Path) -> Vec<PanicRecord> {
    std::fs::read_to_string(sidecar(db, PANIC_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Remember that a hook panicked. Must itself never panic: every step is
/// best effort.
pub fn record_panic(db: &Path, event: &str, location: &str) {
    let mut all = read_panics(db);
    all.push(PanicRecord { at: crate::time::now_unix(), event: event.chars().take(40).collect(), location: location.chars().take(120).collect() });
    let skip = all.len().saturating_sub(PANICS_KEPT);
    all.drain(..skip);
    if let Ok(text) = serde_json::to_string(&all) {
        let _ = crate::store_dir::write(&sidecar(db, PANIC_FILE), text);
    }
}

// ----------------------------------------------------------------------- scan

/// Every finding in the log after `since`, without touching any sidecar.
pub fn detect(store: &EventStore, since: i64) -> Vec<Finding> {
    let mut found = Vec::new();
    for (seq, v) in model::store::evaluation_validations(store) {
        if seq > since && !v.valid && !v.drift {
            found.extend(filing_finding(&v.why));
        }
    }
    if let Ok(events) = store.events_of_kinds_by_actor(&[EventKind::GateStoodAside], ATTESTER_ACTOR) {
        for event in events.iter().filter(|e| e.seq > since) {
            let Some(body) = serde_json::from_str::<serde_json::Value>(&event.body).ok() else { continue };
            let Some(fault) = body.get("hook_fault").and_then(|f| serde_json::from_value::<model::store::HookFault>(f.clone()).ok())
            else {
                continue;
            };
            if !fault.host_wide {
                found.push(hook_launch_finding(&fault.hook_event, fault.exit_code));
            }
        }
    }
    for start in model::store::evaluation_starts_after(store, since) {
        found.extend(start_findings(&start));
    }
    found
}

/// What one evaluation start record says about THOR.
fn start_findings(start: &EvaluationStart) -> Vec<Finding> {
    let mut found = Vec::new();
    if start.frozen.decided() {
        let cut = crate::eval_session::lists_cut(&start.id, &start.frozen, crate::eval_session::START_NOTE_BUDGET);
        if !cut.is_empty() {
            found.push(start_note_cut_finding(&cut));
        }
    }
    if let Some(signals) = start.frozen.machine.as_ref().and_then(|m| m.signals.as_ref()) {
        found.extend(false_block_patterns(&signals.false_blocks));
    }
    found
}

/// The command word of the call a false-block line says was refused:
/// `'rule': refused 'git push --force' at log seq 12, then weakened ...`.
fn refused_command_word(line: &str) -> Option<String> {
    let call = between(line, ": refused '", "' at log seq")?;
    let first = call.split_whitespace().next()?;
    let word = first.rsplit(['/', '\\']).next().unwrap_or(first);
    let word: String = word.chars().take(40).collect();
    (!word.is_empty()).then_some(word)
}

/// A pattern: two or more different rules loosened after refusing calls that
/// start with the same word.
pub fn false_block_patterns(blocks: &[model::store::FalseBlock]) -> Vec<Finding> {
    let mut by_word: BTreeMap<String, (std::collections::BTreeSet<&str>, usize)> = BTreeMap::new();
    for block in blocks {
        let Some(word) = refused_command_word(&block.line) else { continue };
        let entry = by_word.entry(word).or_default();
        entry.0.insert(block.rule.as_str());
        entry.1 += 1;
    }
    by_word
        .into_iter()
        .filter(|(_, (rules, _))| rules.len() >= 2)
        .map(|(word, (rules, n))| false_block_pattern_finding(&word, rules.len(), n))
        .collect()
}

/// Mark the candidates the memory already knows an outcome for: a live item
/// tagged `thor-bug:<fingerprint>` is reported, one also (or only) tagged
/// `thor-bug-resolved:<fingerprint>` is resolved.
fn reconcile(book: &mut Book, store: &EventStore) {
    if !book.candidates.iter().any(|c| matches!(c.status, Status::New | Status::Presented | Status::Reported)) {
        return;
    }
    let mut reported = std::collections::HashSet::new();
    let mut resolved = std::collections::HashSet::new();
    for live in crate::live::live_items(store) {
        for tag in &live.item.tags {
            if let Some(fp) = tag.strip_prefix(REPORT_TAG_PREFIX) {
                reported.insert(fp.to_string());
            } else if let Some(fp) = tag.strip_prefix(RESOLVED_TAG_PREFIX) {
                resolved.insert(fp.to_string());
            }
        }
    }
    for c in &mut book.candidates {
        if c.status == Status::Resolved {
            continue;
        }
        if resolved.contains(&c.fingerprint) {
            c.status = Status::Resolved;
        } else if reported.contains(&c.fingerprint) {
            c.status = Status::Reported;
        }
    }
}

/// Read the log since the last scan, note every finding, reconcile with the
/// memory's own reports, and move the watermark.
pub fn scan(db: &Path, store: &EventStore, book: &mut Book, now: i64, version: &str) {
    let head = store.get_next_seq().map(|n| n - 1).unwrap_or(0);
    let since = if book.scanned_seq > 0 { book.scanned_seq } else { (head - FIRST_SCAN_EVENTS).max(0) };
    if head > since {
        for finding in detect(store, since) {
            upsert(book, finding, now, version);
        }
    }
    // A panic is noted once and then taken off the panic file.
    let panics = read_panics(db);
    if !panics.is_empty() {
        for panic in &panics {
            upsert(book, hook_panic_finding(&panic.event, &panic.location), now, version);
        }
        let _ = crate::store_dir::write(&sidecar(db, PANIC_FILE), "[]");
    }
    book.scanned_seq = head.max(book.scanned_seq);
    reconcile(book, store);
}

/// The candidate to offer now: the oldest new one, else a presented one
/// nobody reported for a day (at most `MAX_PRESENTATIONS` times in all).
fn pick(book: &Book, now: i64) -> Option<usize> {
    let new = book.candidates.iter().enumerate().filter(|(_, c)| c.status == Status::New).min_by_key(|(_, c)| c.first_seen);
    if let Some((i, _)) = new {
        return Some(i);
    }
    book.candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            c.status == Status::Presented
                && c.presentations < MAX_PRESENTATIONS
                && c.presented_unix.is_some_and(|at| now - at >= RENUDGE_AFTER_SECONDS || at > now)
        })
        .min_by_key(|(_, c)| c.first_seen)
        .map(|(i, _)| i)
}

/// The names the scrubber must know: the memory's project names plus this
/// machine's user and host names.
fn known_for(store: &EventStore) -> Known {
    let mut projects: Vec<String> =
        crate::live::live_items(store).into_iter().filter_map(|l| l.item.project).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    projects.retain(|p| p.chars().count() >= 3);
    Known::gather(projects)
}

/// THE NUDGE OF A SESSION'S FIRST PROMPT: at most one candidate per session,
/// never holding. `None` when no mode is installed, the session was already
/// nudged, or nothing is waiting. A candidate that is offered is marked
/// presented and the session is remembered, in one write.
pub fn nudge_for_session(db: &Path, session: &str, now: i64) -> Option<String> {
    let mode = read_mode(db)?;
    let mut book = load(db);
    if book.nudged.contains_key(session) {
        return None;
    }
    let store = crate::store_dir::open_existing(db).ok()?;
    let version = version_text();
    if now - book.last_scan_unix >= SCAN_EVERY_SECONDS || book.last_scan_unix > now {
        scan(db, &store, &mut book, now, &version);
        book.last_scan_unix = now;
    }
    let chosen = pick(&book, now);
    let text = chosen.map(|i| {
        let known = known_for(&store);
        let c = &book.candidates[i];
        let w = WriteUp {
            fingerprint: &c.fingerprint,
            signal: c.finding.signal.as_str(),
            summary: &c.finding.summary,
            why_thor: &c.finding.why_thor,
            evidence: &c.finding.evidence,
            expected: &c.finding.expected,
            actual: &c.finding.actual,
            fix_area: &c.finding.fix_area,
            version: &c.version,
        };
        bug_report::nudge(&w, mode, &known)
    });
    if let Some(i) = chosen {
        let c = &mut book.candidates[i];
        c.status = Status::Presented;
        c.presented_unix = Some(now);
        c.presentations = c.presentations.saturating_add(1);
        book.nudged.insert(session.to_string(), now);
        while book.nudged.len() > SESSIONS_KEPT {
            let oldest = book.nudged.iter().min_by_key(|(_, at)| **at).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => book.nudged.remove(&k),
                None => break,
            };
        }
    }
    save(db, &book);
    text
}

/// `doctor_line`, only while a candidate is waiting (new or presented): the
/// evaluation routine reads it as a THOR health line.
pub fn open_line(db: &Path) -> Option<String> {
    let book = load(db);
    book.candidates.iter().any(|c| matches!(c.status, Status::New | Status::Presented)).then(|| doctor_line(db)).flatten()
}

/// One line for doctor: the mode and the candidates by status. `None` when
/// there is nothing to say (no mode and no candidates).
pub fn doctor_line(db: &Path) -> Option<String> {
    let mode = read_mode(db);
    let book = load(db);
    if mode.is_none() && book.candidates.is_empty() {
        return None;
    }
    let count = |s: Status| book.candidates.iter().filter(|c| c.status == s).count();
    let mode_words = match mode {
        Some(Mode::Release) => "release (a bug candidate is offered to the agent, and published only with your go)",
        Some(Mode::Test) => "test (a bug candidate is shown to the agent and reported to you, never published)",
        None => "not set (nothing is offered; run install again to set it)",
    };
    Some(format!(
        "THOR bug candidates: mode {mode_words}; {} new, {} presented, {} reported, {} resolved",
        count(Status::New),
        count(Status::Presented),
        count(Status::Reported),
        count(Status::Resolved)
    ))
}

#[cfg(test)]
mod tests;
