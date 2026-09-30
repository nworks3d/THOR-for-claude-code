//! Where an evaluation starts: in the log, with the lists its report is held
//! to frozen inside the start record (step 13 of the watertight teeth build),
//! and with THOR deciding the project's old rules itself before anything is
//! asked of the session (step 14).
//!
//! WHY THE LOG AND NOT A SIDECAR. Until step 13 the hook handed the start
//! over to the tool server in a small file beside the store
//! (`evaluation-sessions.json`). Any local process could write that file:
//! editing it moved the start, emptied the list, or tied a filing to a
//! session that never ran the evaluation. Now the start is ONE event on the
//! log, written by THOR's own `eval-machine` (a reserved actor, on a reserved
//! id - `model::store::record_evaluation_start`), and every reader - the
//! filing gate in the tool server, the hook's own messages - reads it back
//! from the log (`model::store::open_evaluation_start`). The old sidecar is
//! deleted on the first Stop of this build and never read again.
//!
//! WHAT STARTS ONE. Three signals, all in the hook (`bin/serve.rs`):
//! - the Stop hook asking for the evaluation;
//! - the owner typing the routine's command as the first word of a prompt
//!   (`/t-eval` or `/thor-eval`, or a name listed in `extra_names_path`) -
//!   always, whether or not the evaluation is due yet, and never from a
//!   prompt a subagent receives;
//! - a `Skill` (or `SlashCommand`) call naming the routine, but ONLY while
//!   the evaluation is actually due (the Stop hook would ask for it now).
//!   Otherwise that call starts nothing and gets one of two plain answers
//!   (`not_due_note`, `continue_note`).
//!
//! An evaluation stays open until its own report is on the log (or it grows
//! older than `model::store::EVALUATION_OPEN_HOURS`), so a second signal - an
//! ask on a later turn, the command typed again - never moves the start and
//! never grows the lists: it gets the open one back.
//!
//! THE MACHINE DECIDES THE OLD RULES AT THE START (step 14). `start` writes
//! the start record FIRST (the judgement list and the open points), then
//! runs `machine_pass` under a time box, then writes ONE more event on the
//! same record - the batch record - with what is left for the session: the
//! frozen batch (rules that need a fix only the agent can make, at most 8),
//! the A4 list (rules whose check bites and whose text only has to name its
//! literal, at most 16) and the machine's own decisions. Two writes, in that
//! order, because the hook runs under Claude Code's hook timeout: a hook
//! killed in between leaves a start record without a batch, which is the
//! marker `resume` reads at the session's next Stop, and the filing gate
//! refuses until the batch is there.
//!
//! WHAT THE PASS DOES (`machine_pass`): THOR's own retest (`teeth::retest`,
//! the root from the registration only, never `git log -S`) on every old rule
//! of the project with no current verdict, on every note that keeps a
//! pending check, and on every rule-track `contains` check whose literal was
//! uncommitted or whose file changed. A rule or a note it can decide it
//! WRITES, as `teeth-eval` (an old contains check counted under Q24 as
//! `retest`, the only name the store takes that mark from). A rule held by
//! something only the owner can lift
//! - no registered working copy, a working tree that is mid-change - goes to
//! THOR's own open point for that subject, written and closed by
//! `eval-machine`. Only what needs the agent is handed to the session.

use crate::legit::Corpus;
use crate::project;
use crate::teeth::{self, ApplyError, Maintenance};
use model::item::{Check, Item, Kind};
use model::store::{
    Blocker, EvaluationStart, Frozen, MachineBlocker, MachineItem, MachineOutcome, MachinePass, MachinePoint, Settled,
    Settlement, SystemActor,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use thor_core::event_store::{EventKind, EventStore};

/// The evaluation routine's command name as `install` seeds it
/// (`~/.claude/commands/thor-eval.md`).
pub const EVAL_COMMAND_NAME: &str = "thor-eval";

/// Every built-in name of the routine: the seeded one, and the name the
/// owner's own routine uses (`/t-eval`). Typing either one, or opening either
/// as a skill, is the same signal (F16).
pub const EVAL_COMMAND_NAMES: &[&str] = &[EVAL_COMMAND_NAME, "t-eval"];

/// Where an owner who keeps the routine under yet another name lists that
/// name, one per line (a leading `/` is fine), beside the store - so opening
/// it under that name starts an evaluation too. Optional.
pub fn extra_names_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("eval-command-names.txt")
}

/// Every name that opens the evaluation routine on this machine:
/// `EVAL_COMMAND_NAMES`, plus whatever `extra_names_path` lists.
pub fn command_names(db: &Path) -> Vec<String> {
    let extra = std::fs::read_to_string(extra_names_path(db)).unwrap_or_default();
    let mut names: Vec<String> = EVAL_COMMAND_NAMES.iter().map(|n| n.to_string()).collect();
    for name in extra.lines().map(|l| l.trim().trim_start_matches('/').to_string()).filter(|l| !l.is_empty()) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// The retired handover sidecar (see this module's doc comment). Named only
/// so it can be deleted.
pub fn retired_handover_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("evaluation-sessions.json")
}

/// Delete the retired handover sidecar when it is still there. Called at
/// every Stop; never reads it. A failure is ignored: nothing reads the file
/// any more, so a copy that survives changes nothing.
pub fn retire_handover_sidecar(db: &Path) {
    let path = retired_handover_path(db);
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
}

/// The open evaluation `session_id` runs for `project`, read from the log.
pub fn open_evaluation(store: &EventStore, session_id: &str, project: &str) -> Option<EvaluationStart> {
    model::store::open_evaluation_start(store, session_id, project)
}

/// Whether `start` is still the evaluation running: not older than
/// `model::store::EVALUATION_OPEN_HOURS`, and its own report - one filed for
/// its project by its own session after it started - not on the log yet.
pub fn still_open(store: &EventStore, start: &EvaluationStart) -> bool {
    model::store::evaluation_still_open(store, start, crate::time::now_unix())
}

/// What `start` gave back: the evaluation running now, and whether this call
/// wrote its start record (`fresh`) or found it already open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub start: EvaluationStart,
    pub fresh: bool,
}

/// How long THOR's machine pass may run inside one hook call, in seconds
/// (Q32). Claude Code kills a command hook at its timeout - measured in step
/// 1 of the build at 600 s, with nothing the hook would have said arriving -
/// so the pass stops itself well before that and leaves what it did not
/// reach for the next start.
pub const MACHINE_PASS_BOX_SECS: u64 = 20;

/// The most the time box can be set to: one third of the measured hook
/// timeout (600 s).
pub const MACHINE_PASS_BOX_MAX_SECS: u64 = 200;

/// The environment variable that sets the time box, in whole seconds, between
/// 1 and `MACHINE_PASS_BOX_MAX_SECS`. A smaller box only moves rules to a
/// later start; it never settles or waives one.
pub const MACHINE_PASS_BOX_ENV: &str = "THOR_MACHINE_PASS_SECS";

/// The time box of one machine pass: `MACHINE_PASS_BOX_ENV` when it holds a
/// whole number of seconds (clamped to 1..=`MACHINE_PASS_BOX_MAX_SECS`), else
/// `MACHINE_PASS_BOX_SECS`.
pub fn pass_box() -> Duration {
    let secs = std::env::var(MACHINE_PASS_BOX_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(MACHINE_PASS_BOX_SECS, |s| s.clamp(1, MACHINE_PASS_BOX_MAX_SECS));
    Duration::from_secs(secs)
}

fn eval_machine() -> SystemActor {
    SystemActor::new(model::store::EVAL_MACHINE_ACTOR)
}

fn teeth_eval() -> SystemActor {
    SystemActor::new(model::store::TEETH_EVAL_ACTOR)
}

/// Start an evaluation of `project` for `session_id` - or, when one is
/// already open, keep that one untouched: a second start signal never moves
/// the start and never grows its lists.
///
/// A fresh start is two writes (see this module's doc comment): the start
/// record, then - after THOR's machine pass, inside `pass_box()` - the batch
/// record. An open evaluation whose batch record is still missing (the hook
/// that started it was cut off) gets its pass finished here first. `None`
/// only when the log cannot be read or written (a hook run that holds the
/// store read-only included).
pub fn start(db: &Path, store: &mut EventStore, session_id: &str, project: &str, started_by: &str) -> Option<Started> {
    let began = begin(db, store, session_id, project, started_by)?;
    let decided = decide(store, began.start, pass_box());
    Some(Started { start: decided, fresh: began.fresh })
}

/// The FIRST of a start's two writes: the start record, with the judgement
/// list and the open points and no batch - or the evaluation already open,
/// untouched. `start` is `begin` followed by `decide`.
pub fn begin(db: &Path, store: &mut EventStore, session_id: &str, project: &str, started_by: &str) -> Option<Started> {
    retire_handover_sidecar(db);
    if let Some(open) = open_evaluation(store, session_id, project) {
        return Some(Started { start: open, fresh: false });
    }
    let frozen = model::store::frozen_now(store, session_id, project, started_by, crate::time::now_unix())?;
    let id = model::store::evaluation_start_id(project, frozen.started_seq);
    let item = Item {
        id: id.clone(),
        kind: Kind::Report,
        text: render_start_note(&id, &frozen),
        bindings: vec![],
        severity: None,
        project: Some(project.to_string()),
        tags: vec![model::store::EVALUATION_START_TAG.to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    match model::store::record_evaluation_start(store, eval_machine(), &item, &frozen) {
        Ok(start) => Some(Started { start, fresh: true }),
        // Raced by another start for the same position: read the one that
        // landed.
        Err(_) => open_evaluation(store, session_id, project).map(|start| Started { start, fresh: false }),
    }
}

/// The SECOND of a start's two writes: run THOR's machine pass for `start`
/// inside `time_box`, then write the batch record. A start that already
/// holds its batch is returned as it is. When the batch record cannot be
/// written (another hook run wrote it meanwhile, or the store is read-only)
/// the record on the log is read back; failing that, `start` comes back
/// undecided and the next Stop tries again.
pub fn decide(store: &mut EventStore, start: EvaluationStart, time_box: Duration) -> EvaluationStart {
    if start.frozen.decided() {
        return start;
    }
    let project = start.frozen.project.clone();
    let passed = machine_pass(store, &project, &start, Instant::now() + time_box, time_box.as_secs());
    let mut frozen = start.frozen.clone();
    frozen.batch = Some(passed.batch.clone());
    frozen.batch_later = passed.batch_later;
    frozen.a4_list = Some(passed.a4_list.clone());
    frozen.machine = Some(passed.machine.clone());
    let text = render_start_note(&start.id, &frozen);
    match model::store::record_evaluation_batch(
        store,
        eval_machine(),
        &start,
        &text,
        passed.batch,
        passed.batch_later,
        passed.a4_list,
        passed.machine,
    ) {
        Ok(decided) => decided,
        Err(_) => open_evaluation(store, &start.frozen.session, &project).unwrap_or(start),
    }
}

/// Finish the machine pass of every evaluation `session_id` has open whose
/// batch record is still missing - the hook that started it was killed
/// between its two writes. Called at the session's Stop before anything
/// else. Returns the evaluations it decided now.
pub fn resume(store: &mut EventStore, session_id: &str) -> Vec<EvaluationStart> {
    let now = crate::time::now_unix();
    let mut newest: HashMap<String, EvaluationStart> = HashMap::new();
    for start in model::store::evaluation_starts_of_session(store, session_id) {
        newest.insert(start.frozen.project.clone(), start);
    }
    let mut pending: Vec<EvaluationStart> = newest
        .into_values()
        .filter(|s| !s.frozen.decided() && model::store::evaluation_still_open(store, s, now))
        .collect();
    pending.sort_by_key(|s| s.seq);
    pending.into_iter().map(|start| decide(store, start, pass_box())).filter(|s| s.frozen.decided()).collect()
}

/// What the session is told when its Stop finished a machine pass that an
/// earlier hook run was cut off in: the start note, now with its lists.
pub fn resumed_note(start: &EvaluationStart) -> String {
    format!(
        "[THOR] THOR finished deciding the old rules of '{}' - the hook that started this evaluation was cut off before \
         it could. What is yours to do is listed now.\n\n{}",
        start.frozen.project,
        start_note(start)
    )
}

// ----------------------------------------------------------- the machine pass

/// What one machine pass hands back for the batch record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passed {
    /// The frozen batch: the first `EVALUATION_TEETH_BATCH` rules that need
    /// the agent for anything but the A4 naming.
    pub batch: Vec<String>,
    /// How many more need the agent the same way and wait for a later start.
    pub batch_later: usize,
    /// The A4 list: the first `EVALUATION_A4_LIST` rules whose check bites
    /// and whose text only has to name the literal.
    pub a4_list: Vec<String>,
    pub machine: MachinePass,
}

/// One old rule the pass will try.
struct Candidate {
    id: String,
    /// A note that keeps a pending check (K20).
    pending: bool,
    /// A rule-track contains check tried again (K9): its verdict stands
    /// unless this pass finds otherwise.
    retry: bool,
}

/// The seq of the last write of `id` (create, revise, resolve, move).
fn last_write(store: &EventStore, id: &str) -> i64 {
    let writes = [EventKind::FactCreated, EventKind::FactRevised, EventKind::FactResolved, EventKind::FactReprojected];
    store
        .get_events_by_entity(id)
        .ok()
        .and_then(|events| events.iter().filter(|e| writes.contains(&e.kind)).map(|e| e.seq).max())
        .unwrap_or(i64::MAX)
}

/// Every old rule this pass tries, in the order it tries them: what the
/// previous start did not reach first (in the order it left them), then the
/// rules with no verdict that fired since the previous report, then the notes
/// that keep a pending check, then the rest - rules with no verdict that did
/// not fire, rules a blocker holds, and rule-track contains checks that have
/// to be tried again - each group oldest write first.
fn candidates(store: &EventStore, project: &str, start: &EvaluationStart) -> Vec<Candidate> {
    let live: Vec<crate::live::LiveItem> = crate::live::live_items(store)
        .into_iter()
        .filter(|li| li.item.kind.can_fire() && project::applies_to(li.item.project.as_deref(), Some(project)))
        .collect();
    let fired: HashSet<String> = store
        .events_of_kinds(&[EventKind::ItemServed], start.frozen.prev_report_seq)
        .map(|events| events.into_iter().filter(|e| e.seq <= start.frozen.started_seq).map(|e| e.entity_id).collect())
        .unwrap_or_default();
    // The current blob of every file a contains check of this project reads,
    // from one git call - what tells a changed file from an untouched one.
    let root = project::registered_checkout(store, Some(project)).ok();
    let own = |item: &Item| model::normalize::normalize_project(item.project.as_deref()).as_deref() == Some(project);
    let mut paths: Vec<String> = live
        .iter()
        .filter(|li| own(&li.item))
        .filter_map(|li| match &li.item.check {
            Some(Check::Contains { path, .. }) => Some(path.clone()),
            _ => None,
        })
        .collect();
    paths.sort();
    paths.dedup();
    let blobs = root.as_ref().map(|root| teeth::blob_hashes(root, &paths)).unwrap_or_default();

    // (group, last write, candidate)
    let mut found: Vec<(u8, i64, Candidate)> = Vec::new();
    for li in live {
        let (group, pending, retry) = match model::store::settlement(store, &li.id, None) {
            Settlement::Owed { .. } => (if fired.contains(&li.id) { 0 } else { 2 }, false, false),
            Settlement::Settled(Settled::Blocked(_)) => (2, false, false),
            Settlement::Settled(Settled::Note { pending: Some(_), .. }) => (1, true, false),
            Settlement::Settled(Settled::Rule { .. }) => {
                let due = own(&li.item)
                    && root.as_ref().is_some_and(|root| teeth::contains_retry_due(store, &li.id, &li.item, root, &blobs).is_some());
                if !due {
                    continue;
                }
                (2, false, true)
            }
            Settlement::Settled(_) => continue,
        };
        let key = last_write(store, &li.id);
        found.push((group, key, Candidate { id: li.id, pending, retry }));
    }
    let left_over: Vec<String> = model::store::previous_evaluation_start(store, project, start.seq)
        .and_then(|previous| previous.frozen.machine)
        .map(|machine| machine.not_reached_ids)
        .unwrap_or_default();
    let place = |id: &str| left_over.iter().position(|left| left == id);
    found.sort_by(|a, b| {
        let (pa, pb) = (place(&a.2.id), place(&b.2.id));
        match (pa, pb) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then_with(|| a.2.id.cmp(&b.2.id)),
        }
    });
    found.into_iter().map(|(_, _, candidate)| candidate).collect()
}

/// The exact call the agent runs for a rule that needs it for anything but
/// the A4 naming: THOR's trial of the rule as it stands, which writes its
/// verdict once the fix its line names is made, or names the fix again.
fn agent_call(id: &str) -> String {
    format!("mcp__thor__revise(id: \"{id}\")")
}

/// The exact call for a rule whose check bites and whose text only has to
/// name the check's literal (A4).
fn a4_call(store: &EventStore, id: &str) -> String {
    let unnamed = model::store::show(store, id).map(|item| model::gate::unnamed_literals(&item)).unwrap_or_default();
    let literals = unnamed.iter().map(|l| format!("'{}'", model::gate::shown_literal(l))).collect::<Vec<_>>().join(" and ");
    let naming = if literals.is_empty() { "the literal of its check".to_string() } else { literals };
    format!("mcp__thor__revise(id: \"{id}\", text: \"<its text, reworded so it names {naming}>\")")
}

fn machine_item(id: &str, outcome: MachineOutcome, detail: String) -> MachineItem {
    MachineItem { id: id.to_string(), outcome, detail, call: None, blocker: None, seq: None }
}

/// Try one old rule and do with it what the machine can: write a rule or a
/// note as `teeth-eval`, or say who has to act.
fn try_one(store: &mut EventStore, db: Option<&Path>, candidate: &Candidate, corpus: &Corpus) -> MachineItem {
    let id = candidate.id.as_str();
    let needs_agent = |why: String| MachineItem { call: Some(agent_call(id)), ..machine_item(id, MachineOutcome::NeedsAgent, why) };
    let tried = match teeth::retest(store, db, id, corpus) {
        Ok(tried) => tried,
        Err(why) => return needs_agent(why),
    };
    let first_line = tried.lines.first().cloned().unwrap_or_default();
    match &tried.outcome {
        // A note whose pending check cannot be tried yet is still a note
        // (K20): it is settled, nothing holds it, and it gets no open point -
        // its check is hung by a later start, once it can be tried and bites.
        Maintenance::Blocked(blocker) if candidate.pending => {
            machine_item(id, MachineOutcome::PendingKept, format!("its pending check still cannot be tried: {blocker}"))
        }
        Maintenance::Blocked(blocker) => MachineItem {
            blocker: Some(MachineBlocker::of(blocker)),
            ..machine_item(id, MachineOutcome::Blocked, format!("blocked: {blocker}"))
        },
        Maintenance::NeedsAgent(why) if tried.a4_only => {
            MachineItem { call: Some(a4_call(store, id)), ..machine_item(id, MachineOutcome::A4Only, why.clone()) }
        }
        Maintenance::NeedsAgent(why) => needs_agent(why.clone()),
        Maintenance::Rule { .. } | Maintenance::Note { .. } => {
            let note = matches!(tried.outcome, Maintenance::Note { .. });
            let detail = match &tried.outcome {
                Maintenance::Note { because } => because.clone(),
                _ => first_line,
            };
            if tried.write.is_none() {
                // A note whose pending check is still not a working tooth.
                return machine_item(id, MachineOutcome::PendingKept, detail);
            }
            if candidate.retry && teeth::same_as_current_record(store, &tried) {
                return machine_item(id, MachineOutcome::Unchanged, detail);
            }
            // Every verdict is written as `teeth-eval`, but one: an old
            // contains check counted as a tooth although its text does not
            // name it (Q24). The owner's decision lets only THOR's retest
            // write that mark, and the store refuses it from every other
            // name - the pass runs that very retest, so it writes that one
            // verdict as `retest`.
            let written = if teeth::is_old_stock_verdict(&tried) {
                teeth::apply_as_retest(store, &tried)
            } else {
                teeth::apply(store, &tried, teeth_eval())
            };
            match written {
                Ok(event) => {
                    let outcome = match (note, candidate.pending) {
                        (true, _) => MachineOutcome::Note,
                        (false, true) => MachineOutcome::PendingHung,
                        (false, false) => MachineOutcome::Rule,
                    };
                    MachineItem { seq: Some(event.seq), ..machine_item(id, outcome, detail) }
                }
                // Changed while it was being tried: nothing was written, and
                // the next start tries it as it then stands.
                Err(ApplyError::Moved(why)) => machine_item(id, MachineOutcome::Unchanged, why),
                Err(ApplyError::NeedsAgent(why)) | Err(ApplyError::Store(why)) | Err(ApplyError::Nothing(why)) => needs_agent(why),
            }
        }
    }
}

/// Up to twenty ids in quotes, then how many more.
fn named(ids: &[String]) -> String {
    const SHOWN: usize = 20;
    let mut out = ids.iter().take(SHOWN).map(|id| format!("'{id}'")).collect::<Vec<_>>().join(", ");
    if ids.len() > SHOWN {
        out.push_str(&format!(" and {} more", ids.len() - SHOWN));
    }
    out
}

/// The text of THOR's open point for `blocker`: the rules it holds and the
/// step that clears it, with the exact command (F7).
fn point_text(
    store: &EventStore,
    project: &str,
    blocker: &Blocker,
    rules: &[String],
    reached_all: bool,
    carried_by: &[model::store::Carrier],
) -> String {
    let more = if reached_all { "" } else { " THOR's pass did not reach every rule this time, so the list can grow." };
    let text = point_text_of(store, project, blocker, rules, more);
    if carried_by.is_empty() {
        return text;
    }
    format!(
        "IN THE OWNER QUEUE - the owner decides - carried by {}: {} valid evaluation(s) were filed while this still held,          and no agent can lift it. {text}",
        model::store::carriers_in_words(carried_by),
        carried_by.len()
    )
}

/// `point_text` without the owner-queue opening.
fn point_text_of(store: &EventStore, project: &str, blocker: &Blocker, rules: &[String], more: &str) -> String {
    match blocker {
        Blocker::NoWorkingCopy { .. } if blocker.is_global_rules() => format!(
            "THOR's own open point: {} rule(s) with no project carry a check that reads a file, and a rule with no \
             project has no working copy THOR could try that check in: {}.{more} Nothing needs doing: each keeps its \
             check and is counted apart - clearing a check that is true is never the way past this. Only when a rule \
             is really about one project: give it that project (mcp__thor__revise with the rule's id, project and a \
             because); otherwise the owner decides. This point closes itself at the first evaluation start after no \
             rule with no project carries a file check.",
            rules.len(),
            named(rules)
        ),
        Blocker::NoWorkingCopy { project: held } => {
            let why = match project::registered_checkout(store, Some(held)) {
                Err(problem) => problem.to_string(),
                Ok(_) => "no working copy is registered".to_string(),
            };
            format!(
                "THOR's own open point: '{held}' has no working copy THOR can use on this machine ({why}), so it cannot \
                 try the file checks of {} rule(s): {}.{more} What clears it: open that project's folder on this machine \
                 and run: {} - if the project is not on this machine, nothing needs doing: its rules stay as they are \
                 and are counted apart. This point closes itself at the first evaluation start after the folder is \
                 registered.",
                rules.len(),
                named(rules),
                model::store::register_command_for(store.path().as_deref())
            )
        }
        Blocker::WorkingTreeDiffers { path } => {
            let root = project::registered_checkout(store, Some(project)).ok();
            let literals: Vec<String> = rules
                .iter()
                .filter_map(|id| model::store::show(store, id).ok())
                .filter_map(|item| match item.check {
                    Some(Check::Contains { literal, .. }) => Some(format!("'{}'", model::gate::shown_literal(&literal))),
                    _ => None,
                })
                .collect::<BTreeSet<String>>()
                .into_iter()
                .collect();
            format!(
                "THOR's own open point: {path} in {} no longer holds {}, while the default branch still does - the \
                 checkout is mid-change; nothing to do until that change is committed or dropped. It holds {} rule(s): \
                 {}.{more} This point closes itself at the first evaluation start after the file holds the text again.",
                root.map_or_else(|| "the registered working copy".to_string(), |r| r.display().to_string().replace('\\', "/")),
                literals.join(" and "),
                rules.len(),
                named(rules)
            )
        }
    }
}

/// Whether the blocker of one of THOR's open points still holds, read now -
/// for a point this pass found no rule blocked by. `None` when it holds;
/// else the derived reason the point is closed with.
fn lifted(store: &EventStore, project: &str, blocker: &Blocker) -> Option<String> {
    match blocker {
        Blocker::NoWorkingCopy { .. } if blocker.is_global_rules() => {
            let still = crate::live::live_items(store).into_iter().any(|li| {
                li.item.kind.can_fire()
                    && model::normalize::normalize_project(li.item.project.as_deref()).is_none()
                    && li.item.check.as_ref().is_some_and(model::checkout::is_file_check)
            });
            (!still).then(|| "no rule with no project carries a file check any more".to_string())
        }
        Blocker::NoWorkingCopy { project: held } => {
            project::registered_checkout(store, Some(held)).is_ok().then(|| "the working copy is registered now".to_string())
        }
        Blocker::WorkingTreeDiffers { path } => {
            let Ok(root) = project::registered_checkout(store, Some(project)) else {
                return Some("the working copy is no longer registered - the rules are held by that instead".to_string());
            };
            let branch = model::checkout::default_branch(&root);
            let still = crate::live::live_items(store).into_iter().any(|li| {
                let Some(check @ Check::Contains { path: p, literal }) = &li.item.check else { return false };
                li.item.kind.can_fire()
                    && p == path
                    && model::normalize::normalize_project(li.item.project.as_deref()).as_deref() == Some(project)
                    && model::check::run(check, &root) == model::check::Outcome::Fails
                    && model::checkout::in_blob(&root, &branch, p, literal)
            });
            (!still).then(|| format!("the registered working tree and the default branch agree about {path} again"))
        }
    }
}

/// THOR's machine pass for one evaluation start (see this module's doc
/// comment): the project's old rules, tried one by one until `deadline`.
///
/// Writes as it goes: a rule or a note it decides (one `fact_revised` each,
/// as `teeth-eval`), and THOR's own open points (as `eval-machine`) - one per
/// subject a blocker holds, closed with a derived reason once the blocker no
/// longer holds. It never writes the batch record: `decide` does, from what
/// this returns. `box_secs` is only recorded.
pub fn machine_pass(store: &mut EventStore, project: &str, start: &EvaluationStart, deadline: Instant, box_secs: u64) -> Passed {
    let db = store.path();
    let session = start.frozen.session.clone();
    let corpus = Corpus::load(store);
    let order = candidates(store, project, start);
    let mut machine = MachinePass { box_secs, ..MachinePass::default() };
    for (place, candidate) in order.iter().enumerate() {
        if Instant::now() >= deadline {
            machine.not_reached_ids = order[place..].iter().map(|c| c.id.clone()).collect();
            break;
        }
        machine.items.push(try_one(store, db.as_deref(), candidate, &corpus));
    }
    machine.not_reached = machine.not_reached_ids.len();
    let count = |machine: &MachinePass, outcome: MachineOutcome| machine.items.iter().filter(|i| i.outcome == outcome).count();
    machine.bites = count(&machine, MachineOutcome::Rule);
    machine.notes = count(&machine, MachineOutcome::Note);
    machine.pending_hung = count(&machine, MachineOutcome::PendingHung);
    machine.blocked = count(&machine, MachineOutcome::Blocked);
    machine.needs_agent = count(&machine, MachineOutcome::NeedsAgent);
    machine.a4_only = count(&machine, MachineOutcome::A4Only);

    // THOR's own open points: one per subject a blocker holds, naming the
    // rules it holds.
    let mut held: Vec<(Blocker, Vec<String>)> = Vec::new();
    for item in &machine.items {
        let Some(blocker) = item.blocker.as_ref().map(MachineBlocker::to_blocker) else { continue };
        match held.iter_mut().find(|(b, _)| *b == blocker) {
            Some((_, rules)) => rules.push(item.id.clone()),
            None => held.push((blocker, vec![item.id.clone()])),
        }
    }
    let reached_all = machine.not_reached == 0;
    for (blocker, rules) in &held {
        // THE OWNER QUEUE (step 16, B5): a subject a valid evaluation already
        // filed past, still blocked now, is the owner's to decide - the
        // point says so, naming every carrying session with its date.
        let carried_by = model::store::carriers_of(store, project, blocker.subject(), i64::MAX);
        let text = point_text(store, project, blocker, rules, reached_all, &carried_by);
        let _ = model::store::write_machine_point(store, eval_machine(), &session, project, blocker, &text);
        machine.points.push(MachinePoint {
            id: model::store::machine_point_id(project, blocker),
            blocker: MachineBlocker::of(blocker),
            rules: rules.clone(),
            carried_by,
        });
    }
    // A point of an earlier start that this pass found no rule blocked by:
    // closed when its blocker no longer holds, kept when it does (the pass
    // may not have reached its rules).
    for point in model::store::open_points(store, Some(project)).into_iter().filter(|p| p.machine) {
        if machine.points.iter().any(|p| p.id == point.id) {
            continue;
        }
        let (Some(code), Some(subject)) = (point.blocker.clone(), point.subject.clone()) else { continue };
        let blocker = MachineBlocker { code, subject };
        match lifted(store, project, &blocker.to_blocker()) {
            Some(reason) => {
                if model::store::retract_machine_point(store, eval_machine(), &session, &point.id, &reason).is_ok() {
                    machine.points_closed.push((point.id, reason));
                }
            }
            None => {
                let carried_by = model::store::carriers_of(store, project, &blocker.subject, i64::MAX);
                machine.points.push(MachinePoint { id: point.id, blocker, rules: Vec::new(), carried_by })
            }
        }
    }

    let of = |outcome: MachineOutcome| -> Vec<String> {
        machine.items.iter().filter(|i| i.outcome == outcome).map(|i| i.id.clone()).collect()
    };
    let needing = of(MachineOutcome::NeedsAgent);
    let naming = of(MachineOutcome::A4Only);
    let batch: Vec<String> = needing.iter().take(model::store::EVALUATION_TEETH_BATCH).cloned().collect();
    let a4_list: Vec<String> = naming.iter().take(model::store::EVALUATION_A4_LIST).cloned().collect();
    Passed { batch_later: needing.len() - batch.len(), batch, a4_list, machine }
}

fn quoted(ids: &[String]) -> String {
    ids.iter().map(|id| format!("'{id}'")).collect::<Vec<_>>().join(", ")
}

/// The start note: N, and the lists with the exact call that settles each
/// item. It is the start record's own text, and what the session is shown -
/// as the Stop block reason, or as context for the typed command and the
/// skill call. Rendered from the frozen lists alone, so the note and the
/// record can never name different items.
pub fn render_start_note(id: &str, frozen: &Frozen) -> String {
    let mut out = format!(
        "[THOR] The evaluation of '{}' started at log position {} ({}, {}; THOR's record '{id}'). The lists its report \
         is held to are fixed now and cannot grow: nothing that fires or is written from here on is added to them, and \
         nothing that fired only in another session is on them.",
        frozen.project,
        frozen.started_seq,
        frozen.started_by,
        crate::time::iso8601_from_unix(frozen.started_unix)
    );

    out.push_str("\n\n1. JUDGEMENT - ");
    if frozen.judgement.is_empty() {
        out.push_str("nothing that fired in this session owes a verdict, so the report is held to no judgement at all.");
    } else {
        out.push_str(&format!(
            "{} item(s) that fired in this session and had fired {}+ times since their last verdict: {}. For each \
             one, read where it fired (`get` shows the item), then judge it with the THOR tool itself - a verdict \
             from a shell, a script or another session settles nothing:",
            frozen.judgement.len(),
            model::store::JUDGEMENT_DEBT_AFTER,
            quoted(&frozen.judgement)
        ));
        for id in &frozen.judgement {
            out.push_str(&format!(
                "\n   - '{id}': mcp__thor__mark(id: \"{id}\") when it belonged where it fired, or \
                 mcp__thor__mark(id: \"{id}\", noise: true) when it did not"
            ));
        }
        out.push_str(&format!(
            "\n   If you cannot judge one honestly, do not guess: add the tag '{}<id>:<reason>' to the report instead.",
            model::store::NOT_JUDGEABLE_TAG_PREFIX
        ));
    }

    out.push_str("\n\n2. TEETH - ");
    match frozen.machine.as_ref().filter(|_| frozen.decided()) {
        None => out.push_str(&format!("{}.", model::store::STILL_DECIDING)),
        Some(machine) => teeth_section(&mut out, frozen, machine),
    }

    out.push_str("\n\n3. OPEN POINTS - ");
    open_points_section(&mut out, frozen);
    out
}

/// Section 2 of the start note once the machine pass has finished: what THOR
/// decided itself, then the two lists that are the agent's, each item with
/// its reason and its exact call.
fn teeth_section(out: &mut String, frozen: &Frozen, machine: &MachinePass) {
    let reached = machine.items.len();
    if reached == 0 && machine.not_reached == 0 {
        out.push_str("no rule of this project is owed a verdict from THOR's trial.");
        return;
    }
    out.push_str(&format!(
        "THOR tried {reached} old rule(s) of this project itself at the start (time box {} s): {} bite and are on the rule \
         track now, {} became a note, {} pending check(s) hung, {} held by a blocker only the owner can lift (see 3), \
         {} need you; not reached this time: {} (not owed in this evaluation - the next one starts with them).",
        machine.box_secs,
        machine.bites,
        machine.notes,
        machine.pending_hung,
        machine.blocked,
        machine.needs_agent + machine.a4_only,
        machine.not_reached
    ));
    let hung: Vec<String> =
        machine.items.iter().filter(|i| i.outcome == MachineOutcome::PendingHung).map(|i| i.id.clone()).collect();
    if !hung.is_empty() {
        out.push_str(&format!("\n   pending checks hung: {}.", quoted(&hung)));
    }
    let line = |id: &String| {
        let item = machine.items.iter().find(|i| &i.id == id);
        let why = item.map(|i| i.detail.as_str()).unwrap_or("");
        let call = item.and_then(|i| i.call.clone()).unwrap_or_else(|| agent_call(id));
        format!("\n      - '{id}': {why}. Call: {call}")
    };
    let batch = frozen.batch();
    if batch.is_empty() {
        out.push_str("\n   a. THE BATCH - no rule needs a fix from you.");
    } else {
        out.push_str(&format!(
            "\n   a. THE BATCH - {} rule(s) that need a fix only you can make (at most {}): {}. For each one, make the \
             fix its line names with the call shown - THOR tries the rule again as it is written and says rule, note, \
             or what is still missing. A tag, a reason or an open point settles nothing:",
            batch.len(),
            model::store::EVALUATION_TEETH_BATCH,
            quoted(batch)
        ));
        batch.iter().for_each(|id| out.push_str(&line(id)));
        if frozen.batch_later > 0 {
            out.push_str(&format!(
                "\n      {} more rule(s) need the same and are not this evaluation's: a later one starts with them.",
                frozen.batch_later
            ));
        }
    }
    let a4_list = frozen.a4_list();
    if a4_list.is_empty() {
        out.push_str("\n   b. THE A4 LIST - no rule only lacks the naming of its check's literal.");
    } else {
        out.push_str(&format!(
            "\n   b. THE A4 LIST - {} rule(s) whose check bites and whose text only has to name the check's literal (at \
             most {}): {}. For each one, reword its text so it names the literal - nothing else has to change:",
            a4_list.len(),
            model::store::EVALUATION_A4_LIST,
            quoted(a4_list)
        ));
        a4_list.iter().for_each(|id| out.push_str(&line(id)));
        let later = machine.a4_only.saturating_sub(a4_list.len());
        if later > 0 {
            out.push_str(&format!("\n      {later} more rule(s) like these wait for the next evaluation."));
        }
    }
}

/// Section 3 of the start note: THOR's own open points (nothing for the
/// agent to do), then the legacy ones an agent filed before step 14.
fn open_points_section(out: &mut String, frozen: &Frozen) {
    // THOR's own: as the machine pass left them once it has run; before
    // that, the ones that were open at the start.
    let (own, closed): (Vec<String>, Vec<(String, String)>) = match &frozen.machine {
        Some(machine) => (
            machine
                .points
                .iter()
                .map(|p| {
                    let held = match p.rules.len() {
                        0 => format!("'{}'", p.id),
                        n => format!("'{}' (holds {n} rule(s): {})", p.id, named(&p.rules)),
                    };
                    match p.carried_by.is_empty() {
                        true => held,
                        false => format!(
                            "{held} - owner decides - carried by {}",
                            model::store::carriers_in_words(&p.carried_by)
                        ),
                    }
                })
                .collect(),
            machine.points_closed.clone(),
        ),
        None => (
            frozen.open_points.iter().filter(|id| model::store::is_machine_point_id(id)).map(|id| format!("'{id}'")).collect(),
            Vec::new(),
        ),
    };
    let legacy: Vec<String> =
        frozen.open_points.iter().filter(|id| !model::store::is_machine_point_id(id)).cloned().collect();
    if own.is_empty() && closed.is_empty() && legacy.is_empty() {
        out.push_str("none.");
        return;
    }
    out.push_str("written and closed by THOR itself; none of them holds this report.");
    if !own.is_empty() {
        out.push_str(&format!(
            "\n   - THOR's own, open ({}): {}. Nothing for you to do: each one says in its own text what clears it \
             (`get` shows it), and it closes itself when its subject settles.",
            own.len(),
            own.join(", ")
        ));
    }
    for (id, reason) in &closed {
        out.push_str(&format!("\n   - closed at this start: '{id}' ({reason})."));
    }
    if !legacy.is_empty() {
        out.push_str(&format!(
            "\n   - legacy ({}), filed by an agent before THOR wrote them itself: {}. They settle nothing and hold no \
             filing; doctor lists them for the owner. Retract one with a reason only when what it names is resolved.",
            legacy.len(),
            quoted(&legacy)
        ));
    }
}

/// The start note of `start`, as shown again when the evaluation is already
/// open: rendered from its frozen lists, exactly as when it started.
pub fn start_note(start: &EvaluationStart) -> String {
    render_start_note(&start.id, &start.frozen)
}

/// What the session is told when it opens the routine while an evaluation
/// of `project` is already open: where, since when, and - when it is this
/// session's own - the start note again.
pub fn continue_note(start: &EvaluationStart, this_session: bool) -> String {
    let when = crate::time::iso8601_from_unix(start.frozen.started_unix);
    if this_session {
        format!(
            "[THOR] an evaluation of {} is already open since log seq {} (started {when} in this session) - continue \
             it: the start note is repeated below.\n\n{}",
            start.frozen.project,
            start.frozen.started_seq,
            start_note(start)
        )
    } else {
        format!(
            "[THOR] an evaluation of {} is already open since log seq {} (started {when} in another session) - \
             continue it there: nothing was started here.",
            start.frozen.project, start.frozen.started_seq
        )
    }
}

/// What the session is told when it opens the routine while no evaluation
/// of `project` is due and none is open: nothing starts.
pub fn not_due_note(project: &str) -> String {
    format!("[THOR] no evaluation of {project} is due yet - nothing to do.")
}

/// The bare command name at the end of a skill or command reference:
/// `/thor-eval`, `user:thor-eval` and `thor-eval` all read `thor-eval`.
fn command_name(raw: &str) -> &str {
    let raw = raw.trim().trim_start_matches('/');
    raw.rsplit(':').next().unwrap_or(raw)
}

fn is_eval_command(raw: &str, names: &[String]) -> bool {
    let name = command_name(raw);
    names.iter().any(|n| n == name)
}

/// Whether a prompt the owner typed runs the evaluation command: its first
/// word is the command itself, or Claude Code's own expansion of it names it.
/// Never a mere mention further along - "and end with /thor-eval" at the end
/// of a task is a plan for later, not the start of an evaluation.
pub fn prompt_starts_evaluation(prompt: &str, names: &[String]) -> bool {
    let first = prompt.split_whitespace().next().unwrap_or("");
    if first.starts_with('/') && is_eval_command(first, names) {
        return true;
    }
    names.iter().any(|name| prompt.contains(&format!("<command-name>/{name}</command-name>")))
}

/// Whether a tool call opens the evaluation routine: the `Skill` tool naming
/// it, or the older `SlashCommand` tool running it.
pub fn call_starts_evaluation(tool_name: &str, tool_input: Option<&serde_json::Value>, names: &[String]) -> bool {
    let field = match tool_name {
        "Skill" => "skill",
        "SlashCommand" => "command",
        _ => return false,
    };
    tool_input
        .and_then(|input| input.get(field))
        .and_then(|v| v.as_str())
        .and_then(|v| v.split_whitespace().next())
        .is_some_and(|v| is_eval_command(v, names))
}

/// Close the evaluation `start` records as VOID (step 15, D10): `outside` is
/// the write to one of its frozen items that came from outside an attested
/// THOR call of it. One more `fact_revised` by `eval-machine` on the start
/// record (`model::store::record_evaluation_void`); from then on it is no
/// longer open, so the next start signal - the next Stop ask - starts a fresh
/// evaluation with a fresh list. Called by the tool server when a filing
/// meets such a write. `None` when the record could not be written (a store
/// held read-only): the filing is refused all the same, and the next filing
/// attempt closes it.
pub fn void(store: &mut EventStore, start: &EvaluationStart, outside: &model::store::FrozenVoid) -> Option<EvaluationStart> {
    model::store::record_evaluation_void(store, eval_machine(), start, outside).ok()
}

#[cfg(test)]
mod pass_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        vec![EVAL_COMMAND_NAME.to_string(), "t-eval".to_string(), "my-eval".to_string()]
    }

    #[test]
    fn only_the_first_word_of_a_prompt_starts_an_evaluation() {
        assert!(prompt_starts_evaluation("/thor-eval", &names()));
        assert!(prompt_starts_evaluation("/t-eval", &names()));
        assert!(prompt_starts_evaluation("  /my-eval please", &names()));
        assert!(prompt_starts_evaluation("<command-message>x</command-message>\n<command-name>/thor-eval</command-name>", &names()));
        assert!(!prompt_starts_evaluation("Fix the button. End with /thor-eval.", &names()), "a plan for later is not a start");
        assert!(!prompt_starts_evaluation("/thor-evaluate", &names()));
        assert!(!prompt_starts_evaluation("/my-eval", &[EVAL_COMMAND_NAME.to_string()]), "an unlisted name starts nothing");
    }

    #[test]
    fn the_skill_tool_naming_the_routine_starts_an_evaluation() {
        let skill = serde_json::json!({"skill": "thor-eval"});
        assert!(call_starts_evaluation("Skill", Some(&skill), &names()));
        let short = serde_json::json!({"skill": "t-eval"});
        assert!(call_starts_evaluation("Skill", Some(&short), &names()));
        let namespaced = serde_json::json!({"skill": "user:my-eval"});
        assert!(call_starts_evaluation("Skill", Some(&namespaced), &names()));
        let other = serde_json::json!({"skill": "my-brief"});
        assert!(!call_starts_evaluation("Skill", Some(&other), &names()));
        let slash = serde_json::json!({"command": "/thor-eval"});
        assert!(call_starts_evaluation("SlashCommand", Some(&slash), &names()));
        assert!(!call_starts_evaluation("Read", Some(&skill), &names()));
    }

    /// Both built-in names, plus the owner's own name listed beside the store.
    #[test]
    fn both_built_in_names_and_a_listed_one_open_the_routine() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert_eq!(command_names(&db), vec!["thor-eval".to_string(), "t-eval".to_string()], "nothing listed, the two built-in names");
        std::fs::write(extra_names_path(&db), "/my-eval\n\n/t-eval\n").unwrap();
        assert_eq!(command_names(&db), vec!["thor-eval".to_string(), "t-eval".to_string(), "my-eval".to_string()]);
    }

    fn store_with_events(dir: &Path, n: usize) -> (PathBuf, EventStore) {
        let db = dir.join("thor.db");
        let mut store = EventStore::new(&db).unwrap();
        for _ in 0..n {
            store.append_event("x", "x", "hook", EventKind::ItemServed, "some-id", None, "{}").unwrap();
        }
        (db, store)
    }

    fn start_records(store: &EventStore) -> Vec<thor_core::event_store::Event> {
        store
            .get_all_events()
            .unwrap()
            .into_iter()
            .filter(|e| e.entity_id.starts_with(model::store::EVALUATION_START_PREFIX))
            .collect()
    }

    /// A fresh start is ONE record on the log, by `eval-machine`, in the
    /// evaluating session, at the log's tip - written in two events: the
    /// start, then the batch record once THOR's pass has run.
    #[test]
    fn a_start_is_one_record_in_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let (db, mut store) = store_with_events(dir.path(), 3);
        let started = start(&db, &mut store, "sess", "Acme Demo", "skill").unwrap();
        assert!(started.fresh);
        assert_eq!(started.start.frozen.started_seq, 3);
        assert_eq!(started.start.id, "evaluation-start-acme-demo-3");
        assert!(started.start.frozen.decided(), "the batch record is written before `start` returns");
        let records = start_records(&store);
        assert_eq!(records.iter().map(|e| e.kind).collect::<Vec<_>>(), vec![EventKind::FactCreated, EventKind::FactRevised], "{records:?}");
        assert!(records.iter().all(|e| e.entity_id == started.start.id), "both on the one record");
        assert_eq!((records[0].actor.as_str(), records[0].session_id.as_str()), ("eval-machine", "sess"));
        assert_eq!((records[1].actor.as_str(), records[1].session_id.as_str()), ("eval-machine", "sess"));
        let item = model::store::show(&store, "evaluation-start-acme-demo-3").unwrap();
        assert_eq!(item.kind, Kind::Report);
        assert_eq!(item.tags, vec![model::store::EVALUATION_START_TAG.to_string()]);
        assert!(item.text.starts_with("[THOR] The evaluation of 'Acme Demo' started at log position 3"), "{}", item.text);
    }

    /// A second start signal - the Stop hook asking again on the next turn,
    /// the command typed again - never moves the start and writes nothing.
    #[test]
    fn a_second_start_keeps_the_first_start() {
        let dir = tempfile::tempdir().unwrap();
        let (db, mut store) = store_with_events(dir.path(), 3);
        let first = start(&db, &mut store, "sess", "p", "skill").unwrap();
        store.append_event("x", "x", "hook", EventKind::ItemServed, "some-id", None, "{}").unwrap();
        let again = start(&db, &mut store, "sess", "p", "stop-ask").unwrap();
        assert!(!again.fresh);
        assert_eq!(again.start, first.start, "the start never moves while the evaluation is open");
        assert_eq!(start_records(&store).len(), 2, "no second record: still the start and its batch record");
        let other = start(&db, &mut store, "other-sess", "p", "skill").unwrap();
        assert!(other.fresh, "another session starts its own");
        assert_eq!(other.start.frozen.started_seq, 6);
    }

    /// File a report for `session`'s open evaluation of "p" the way Claude
    /// Code files one (`crate::attest::fixture::attested_filing`), with the
    /// session's transcript under `root`.
    fn file_report(store: &mut EventStore, root: &Path, session: &str, id: &str, text: &str) {
        let report = Item {
            id: id.to_string(),
            kind: Kind::Report,
            text: text.to_string(),
            bindings: vec![],
            severity: None,
            project: Some("p".to_string()),
            tags: vec![model::store::EVALUATION_REPORT_TAG.to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        crate::attest::fixture::attested_filing(store, root, session, &report, &format!("toolu_{id}")).unwrap();
    }

    /// Only the evaluation's OWN report closes it: a report another session
    /// files for the same project in parallel must not restart this one.
    #[test]
    fn only_its_own_filed_report_closes_an_evaluation() {
        let dir = tempfile::tempdir().unwrap();
        let (db, mut store) = store_with_events(dir.path(), 3);
        let first = start(&db, &mut store, "sess", "p", "skill").unwrap();

        file_report(&mut store, dir.path(), "other", "eval-p-other", "Another session reviewed the quoter rules today.");
        assert!(still_open(&store, &first.start), "a parallel report closes nothing");
        assert_eq!(start(&db, &mut store, "sess", "p", "stop-ask").unwrap().start, first.start);

        file_report(&mut store, dir.path(), "sess", "eval-p-mine", "This session settled its batch and judged its list.");
        assert!(!still_open(&store, &first.start), "its own filed report closes it");
        let next = start(&db, &mut store, "sess", "p", "skill").unwrap();
        assert!(next.fresh && next.start.frozen.started_seq > first.start.frozen.started_seq, "{next:?}");
    }

    /// Correcting a report filed BEFORE the start is not a filing: the
    /// evaluation stays open.
    #[test]
    fn a_typo_fix_of_an_earlier_report_closes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (db, mut store) = store_with_events(dir.path(), 3);
        file_report(&mut store, dir.path(), "sess", "eval-p-earlier", "Last week this session settled its batch and its list.");
        let started = start(&db, &mut store, "sess", "p", "command").unwrap();
        assert!(started.fresh);

        let filed = model::store::show(&store, "eval-p-earlier").unwrap();
        let mut fixed = filed.clone();
        // Below the machine section THOR wrote: the only part a revise may
        // change (step 15).
        fixed.text = filed.text.replace("its batch and its list.", "its batch and its whole list.");
        assert_ne!(fixed.text, filed.text, "fixture sanity");
        model::store::revise(&mut store, "sess", "l", "a", &filed, &fixed).unwrap();
        assert!(still_open(&store, &started.start), "a correction of an older report files nothing");
    }

    /// An evaluation abandoned for longer than the open window is closed: the
    /// next signal starts a fresh one.
    #[test]
    fn an_evaluation_older_than_its_window_is_closed() {
        let dir = tempfile::tempdir().unwrap();
        let (_db, mut store) = store_with_events(dir.path(), 2);
        let mut frozen = model::store::frozen_now(&store, "sess", "p", "skill", 1_000).unwrap();
        frozen.started_unix = crate::time::now_unix() - (model::store::EVALUATION_OPEN_HOURS * 3600 + 60);
        let item = Item {
            id: model::store::evaluation_start_id("p", frozen.started_seq),
            kind: Kind::Report,
            text: "an old start".to_string(),
            bindings: vec![],
            severity: None,
            project: Some("p".to_string()),
            tags: vec![model::store::EVALUATION_START_TAG.to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        model::store::record_evaluation_start(&mut store, SystemActor::new(model::store::EVAL_MACHINE_ACTOR), &item, &frozen).unwrap();
        assert_eq!(open_evaluation(&store, "sess", "p"), None, "too old to be the one running");
    }

    /// The note names exactly what the record froze.
    #[test]
    fn the_note_names_the_lists_the_record_froze() {
        let dir = tempfile::tempdir().unwrap();
        let (db, mut store) = store_with_events(dir.path(), 1);
        let started = start(&db, &mut store, "sess", "p", "command").unwrap();
        let note = start_note(&started.start);
        let stored = model::store::show(&store, &started.start.id).unwrap();
        assert_eq!(note, stored.text, "the record's text is the note");
        assert!(note.contains("1. JUDGEMENT - nothing that fired in this session owes a verdict"), "{note}");
        assert!(note.contains("2. TEETH - no rule"), "{note}");
        assert!(note.contains("3. OPEN POINTS - none."), "{note}");
    }

    /// The retired handover sidecar is deleted, and a planted one changes
    /// nothing: nothing reads it.
    #[test]
    fn the_retired_handover_sidecar_is_deleted_and_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let (db, mut store) = store_with_events(dir.path(), 2);
        let planted = serde_json::json!({
            "evaluations": [{"session_id": "sess", "project": "p", "started_seq": 1, "started_unix": crate::time::now_unix(), "started_by": "skill"}],
            "filings": [],
        });
        std::fs::write(retired_handover_path(&db), planted.to_string()).unwrap();
        assert_eq!(open_evaluation(&store, "sess", "p"), None, "a planted sidecar opens nothing");
        let started = start(&db, &mut store, "sess", "p", "skill").unwrap();
        assert!(started.fresh && started.start.frozen.started_seq == 2, "the start is the log's own: {started:?}");
        retire_handover_sidecar(&db);
        assert!(!retired_handover_path(&db).exists());
    }

    /// D10: an evaluation whose list was changed behind it is closed as
    /// void by eval-machine - once - and the next start signal of the session
    /// writes a fresh start with a fresh list, never the void one again.
    #[test]
    fn a_void_evaluation_is_closed_and_the_next_start_is_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let (db, mut store) = store_with_events(dir.path(), 4);
        let started = start(&db, &mut store, "sess", "p", "stop-ask").unwrap();
        let outside = model::store::FrozenVoid {
            id: "some-id".to_string(),
            seq: 3,
            door: "written by 'cli' with no stamp of THOR's tool server".to_string(),
        };
        let void = super::void(&mut store, &started.start, &outside).expect("closed");
        assert_eq!(void.frozen.void.as_ref(), Some(&outside));
        assert!(!still_open(&store, &void), "a void evaluation is not the one running");
        assert_eq!(open_evaluation(&store, "sess", "p"), None);
        let records = start_records(&store);
        assert_eq!(records.len(), 3, "start, batch record, void mark: {records:?}");
        assert!(records.iter().all(|e| e.actor == "eval-machine" && e.session_id == "sess"));
        assert_eq!(super::void(&mut store, &started.start, &outside), Some(void.clone()), "voided once");
        assert_eq!(start_records(&store).len(), 3);
        assert!(resume(&mut store, "sess").is_empty(), "nothing to resume in a void evaluation");

        let fresh = start(&db, &mut store, "sess", "p", "stop-ask").unwrap();
        assert!(fresh.fresh, "the next signal starts a fresh evaluation: {fresh:?}");
        assert_ne!(fresh.start.id, started.start.id);
        assert!(fresh.start.frozen.started_seq > started.start.frozen.started_seq);
    }
}
