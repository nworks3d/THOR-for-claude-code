//! Which session runs an evaluation, and where on the log it started - the
//! handover from the hook (which knows the session) to the tool server
//! (which files the report, and does not).
//!
//! WHY A SIDECAR (2026-09-26). The evaluation filing gate
//! (`model::store::check_evaluation_report_filing`) now asks for verdicts
//! only on items that fired in the FILING session before its evaluation
//! STARTED (see `model::store::EvaluationScope` for the measurement that made
//! this necessary). The log already records the session of every firing: the
//! hook stamps it on each delivery. What the log cannot record is which
//! session is filing, because every write through the tool server carries
//! one shared session constant - the same limit `bin/serve.rs`'s
//! `crowding_debt` and `judgement_debt` already work around with a watermark
//! sidecar. So the hook writes two small facts here, and the tool server
//! reads them back:
//!
//! - an EVALUATION: this session started evaluating this project at this log
//!   position. Recorded the first time any of these happens in a session:
//!   the Stop hook asks for the evaluation, the owner types the evaluation
//!   command, the session opens it as a skill - or, if none of those was
//!   seen, its first attempt to file the report. An evaluation stays open
//!   until its own report is on the log (or it grows older than
//!   `EVALUATION_OPEN_HOURS`), so asking again on a later turn, a second
//!   filing attempt, or another session filing its own report for the same
//!   project never moves the start and never grows the list.
//! - a FILING: this session is about to file this report id. Written by the
//!   hook on the very `remember`/`revise` call that files it, which Claude
//!   Code runs before the tool itself - so the tool server always finds it.
//!
//! Fails open everywhere, like every sidecar beside the store: unreadable or
//! missing reads as "nothing recorded", and a write that fails is skipped.
//! Nothing here can block a tool call.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thor_core::event_store::EventStore;

/// The evaluation routine's command name as `install` seeds it
/// (`~/.claude/commands/thor-eval.md`). A skill call or a typed command with
/// this name starts an evaluation.
pub const EVAL_COMMAND_NAME: &str = "thor-eval";

/// Where an owner who keeps the routine under a name of his own lists that
/// name, one per line (a leading `/` is fine), beside the store - so opening
/// it under that name starts an evaluation too. Optional; missing means only
/// `EVAL_COMMAND_NAME`.
pub fn extra_names_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("eval-command-names.txt")
}

/// Every name that opens the evaluation routine on this machine:
/// `EVAL_COMMAND_NAME`, plus whatever `extra_names_path` lists.
pub fn command_names(db: &Path) -> Vec<String> {
    let extra = std::fs::read_to_string(extra_names_path(db)).unwrap_or_default();
    std::iter::once(EVAL_COMMAND_NAME.to_string())
        .chain(extra.lines().map(|l| l.trim().trim_start_matches('/').to_string()).filter(|l| !l.is_empty()))
        .collect()
}

/// How long an evaluation that never filed a report stays open. After this a
/// new start signal opens a fresh one: an evaluation abandoned yesterday must
/// not fix today's list.
pub const EVALUATION_OPEN_HOURS: i64 = 16;

/// How recent a filing record must be for the tool server to trust it. The
/// hook writes it moments before the tool call it belongs to; an hour is far
/// more than that ever takes, and short enough that an old attempt under the
/// same id by another session is never read as this one.
pub const FILING_FRESH_SECS: i64 = 3600;

/// Entries older than this are dropped on the next write, so the file stays
/// small.
const RETAIN_SECS: i64 = 7 * 24 * 3600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evaluation {
    pub session_id: String,
    pub project: String,
    /// The highest log seq that existed when the evaluation started.
    pub started_seq: i64,
    pub started_unix: i64,
    /// What started it: "stop-ask", "command", "skill" or "filing".
    pub started_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filing {
    pub report_id: String,
    pub session_id: String,
    pub project: String,
    pub at_unix: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalSessions {
    #[serde(default)]
    pub evaluations: Vec<Evaluation>,
    #[serde(default)]
    pub filings: Vec<Filing>,
}

/// Beside the store, like every other sidecar on this boundary.
pub fn path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("evaluation-sessions.json")
}

pub fn read(db: &Path) -> EvalSessions {
    std::fs::read_to_string(path(db)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn write(db: &Path, state: &mut EvalSessions, now: i64) {
    state.evaluations.retain(|e| now - e.started_unix <= RETAIN_SECS);
    state.filings.retain(|f| now - f.at_unix <= RETAIN_SECS);
    if let Ok(text) = serde_json::to_string_pretty(state) {
        let _ = crate::store_dir::write(&path(db), text);
    }
}

/// The last log seq that exists right now - where an evaluation starting now
/// starts.
fn log_tip(store: &EventStore) -> Option<i64> {
    store.get_next_seq().ok().map(|next| next - 1)
}

/// Whether `evaluation` is still the one running: not too old, and its OWN
/// report - one this session was recorded filing for this project - is not
/// on the log yet. A report another session files in parallel for the same
/// project closes nothing here: it was never this evaluation's report, and
/// closing on it would restart this evaluation at its next signal, with a
/// list that already holds what its own writes fired.
fn still_open(state: &EvalSessions, store: &EventStore, evaluation: &Evaluation, now: i64) -> bool {
    if now - evaluation.started_unix > EVALUATION_OPEN_HOURS * 3600 {
        return false;
    }
    let own_report_filed = state
        .filings
        .iter()
        .filter(|f| f.session_id == evaluation.session_id && f.project == evaluation.project)
        .filter(|f| f.at_unix >= evaluation.started_unix)
        .any(|f| {
            let live = model::store::show(store, &f.report_id).is_ok_and(|item| item.project.as_deref() == Some(&evaluation.project));
            let created_after_start = store
                .get_events_by_entity(&f.report_id)
                .ok()
                .and_then(|events| events.first().map(|e| e.seq))
                .is_some_and(|seq| seq > evaluation.started_seq);
            live && created_after_start
        });
    !own_report_filed
}

/// The open evaluation `session_id` runs for `project`, if any.
pub fn open_evaluation(db: &Path, store: &EventStore, session_id: &str, project: &str, now: i64) -> Option<Evaluation> {
    let state = read(db);
    state
        .evaluations
        .iter()
        .filter(|e| e.session_id == session_id && e.project == project)
        .max_by_key(|e| e.started_seq)
        .filter(|e| still_open(&state, store, e, now))
        .cloned()
}

/// Start an evaluation for `session_id` in `project` - or, when one is
/// already open, keep that one untouched: a second start signal never moves
/// the start. Returns the evaluation that is running now, `None` only when
/// the log cannot be read.
pub fn start(db: &Path, store: &EventStore, session_id: &str, project: &str, now: i64, started_by: &str) -> Option<Evaluation> {
    if let Some(open) = open_evaluation(db, store, session_id, project, now) {
        return Some(open);
    }
    let evaluation = Evaluation {
        session_id: session_id.to_string(),
        project: project.to_string(),
        started_seq: log_tip(store)?,
        started_unix: now,
        started_by: started_by.to_string(),
    };
    let mut state = read(db);
    state.evaluations.push(evaluation.clone());
    write(db, &mut state, now);
    Some(evaluation)
}

/// Record that `session_id` is filing `report_id` for `project`, starting an
/// evaluation first when none is open (the fallback start: the first filing
/// attempt).
pub fn record_filing(db: &Path, store: &EventStore, session_id: &str, project: &str, report_id: &str, now: i64) {
    if start(db, store, session_id, project, now, "filing").is_none() {
        return;
    }
    let mut state = read(db);
    state.filings.retain(|f| f.report_id != report_id);
    state.filings.push(Filing {
        report_id: report_id.to_string(),
        session_id: session_id.to_string(),
        project: project.to_string(),
        at_unix: now,
    });
    write(db, &mut state, now);
}

/// The scope the tool server hands the filing gate for `report_id`: the
/// session the hook saw filing it, and where that session's evaluation of
/// the project started. `None` when the hook recorded no such filing (no
/// hook, or an old one) - the gate then demands no verdict, see
/// `model::store::EvaluationScope`.
pub fn scope_for_filing(db: &Path, report_id: &str, now: i64) -> Option<model::store::EvaluationScope> {
    let state = read(db);
    let filing = state
        .filings
        .iter()
        .filter(|f| f.report_id == report_id && now - f.at_unix <= FILING_FRESH_SECS)
        .max_by_key(|f| f.at_unix)?;
    let evaluation = state
        .evaluations
        .iter()
        .filter(|e| e.session_id == filing.session_id && e.project == filing.project && e.started_unix <= filing.at_unix)
        .max_by_key(|e| e.started_seq)?;
    Some(model::store::EvaluationScope { session_id: evaluation.session_id.clone(), started_seq: evaluation.started_seq })
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

/// The report id and project a `remember`/`revise` call files an evaluation
/// report under, when it does: the call's own `tags` carry the evaluation
/// tag, and a `remember` says it is a report. `project` is `None` when the
/// call names none (a `revise` that keeps the item's own project).
pub fn filing_in_call(tool_suffix: &str, tool_input: Option<&serde_json::Value>) -> Option<(String, Option<String>)> {
    if tool_suffix != "remember" && tool_suffix != "revise" {
        return None;
    }
    let input = tool_input?;
    let tagged = input
        .get("tags")
        .and_then(|t| t.as_array())
        .is_some_and(|tags| tags.iter().any(|t| t.as_str() == Some(model::store::EVALUATION_REPORT_TAG)));
    if !tagged {
        return None;
    }
    if tool_suffix == "remember" && !input.get("kind").and_then(|k| k.as_str()).is_some_and(|k| k.eq_ignore_ascii_case("report")) {
        return None;
    }
    let id = input.get("id").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())?;
    let project = input.get("project").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    Some((id.to_string(), project))
}

/// The note a session gets the moment its evaluation starts: the list its
/// report will be held to, fixed now, so it never has to learn it from a
/// refusal at the end.
pub fn start_note(store: &EventStore, evaluation: &Evaluation) -> String {
    let scope = model::store::EvaluationScope {
        session_id: evaluation.session_id.clone(),
        started_seq: evaluation.started_seq,
    };
    let judgement = model::store::evaluation_judgement_owed(store, Some(&evaluation.project), &scope, &[]);
    let list = if judgement.owed.is_empty() {
        "nothing that fired in this session owes a verdict, so the report is held to no judgement at all".to_string()
    } else {
        format!(
            "{} item(s) that fired in this session and have fired {}+ times since their last verdict: {}. Judge each \
             with mark (noise:true when it did not belong where it fired), or, when you cannot judge one honestly, \
             add the tag '{}<id>:<reason>' to the report instead of guessing",
            judgement.owed.len(),
            model::store::JUDGEMENT_DEBT_AFTER,
            judgement.owed.iter().map(|id| format!("'{id}'")).collect::<Vec<_>>().join(", "),
            model::store::NOT_JUDGEABLE_TAG_PREFIX
        )
    };
    format!(
        "[THOR] The evaluation of '{}' started at log position {}. The judgement list its report is held to is \
         fixed now: {list}. Nothing that fires from here on - your own remember and revise calls included - and \
         nothing that fired only in another session is added to it; doctor keeps naming the store-wide debt.",
        evaluation.project, evaluation.started_seq
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        vec![EVAL_COMMAND_NAME.to_string(), "my-eval".to_string()]
    }

    #[test]
    fn only_the_first_word_of_a_prompt_starts_an_evaluation() {
        assert!(prompt_starts_evaluation("/thor-eval", &names()));
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
        let namespaced = serde_json::json!({"skill": "user:my-eval"});
        assert!(call_starts_evaluation("Skill", Some(&namespaced), &names()));
        let other = serde_json::json!({"skill": "my-brief"});
        assert!(!call_starts_evaluation("Skill", Some(&other), &names()));
        let slash = serde_json::json!({"command": "/thor-eval"});
        assert!(call_starts_evaluation("SlashCommand", Some(&slash), &names()));
        assert!(!call_starts_evaluation("Read", Some(&skill), &names()));
    }

    /// The owner's own name for the routine, listed beside the store.
    #[test]
    fn a_listed_name_joins_the_seeded_one() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert_eq!(command_names(&db), vec![EVAL_COMMAND_NAME.to_string()], "nothing listed, only the seeded name");
        std::fs::write(extra_names_path(&db), "/my-eval\n\n").unwrap();
        assert_eq!(command_names(&db), vec![EVAL_COMMAND_NAME.to_string(), "my-eval".to_string()]);
    }

    #[test]
    fn a_filing_is_a_tagged_report_through_remember_or_a_tagged_revise() {
        let remember = serde_json::json!({"id": "eval-p-1", "kind": "report", "project": "p", "tags": ["evaluation-report", "p"]});
        assert_eq!(filing_in_call("remember", Some(&remember)), Some(("eval-p-1".to_string(), Some("p".to_string()))));
        let rule = serde_json::json!({"id": "r", "kind": "rule", "tags": ["evaluation-report"]});
        assert_eq!(filing_in_call("remember", Some(&rule)), None);
        let untagged = serde_json::json!({"id": "eval-p-1", "kind": "report", "tags": ["p"]});
        assert_eq!(filing_in_call("remember", Some(&untagged)), None);
        let revise = serde_json::json!({"id": "eval-p-1", "tags": ["evaluation-report"]});
        assert_eq!(filing_in_call("revise", Some(&revise)), Some(("eval-p-1".to_string(), None)));
        assert_eq!(filing_in_call("lookup", Some(&remember)), None);
    }

    fn store_with_events(dir: &Path, n: usize) -> (PathBuf, EventStore) {
        let db = dir.join("thor.db");
        let mut store = EventStore::new(&db).unwrap();
        for _ in 0..n {
            store.append_event("x", "x", "hook", thor_core::event_store::EventKind::ItemServed, "some-id", None, "{}").unwrap();
        }
        (db, store)
    }

    /// A second start signal - the Stop hook asking again on the next turn,
    /// a second filing attempt - never moves the start.
    #[test]
    fn a_second_start_keeps_the_first_start() {
        let dir = tempfile::tempdir().unwrap();
        let (db, mut store) = store_with_events(dir.path(), 3);
        let first = start(&db, &store, "sess", "p", 1_000, "skill").unwrap();
        assert_eq!(first.started_seq, 3);
        store.append_event("x", "x", "hook", thor_core::event_store::EventKind::ItemServed, "some-id", None, "{}").unwrap();
        let again = start(&db, &store, "sess", "p", 1_100, "stop-ask").unwrap();
        assert_eq!(again, first, "the start never moves while the evaluation is open");
        let other = start(&db, &store, "other-sess", "p", 1_100, "skill").unwrap();
        assert_eq!(other.started_seq, 4, "another session starts its own");
    }

    fn file_report(store: &mut EventStore, id: &str, text: &str) {
        let report = model::item::Item {
            id: id.to_string(),
            kind: model::item::Kind::Report,
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
        model::store::declare(store, "mcp", "l", "a", &report).unwrap();
    }

    /// Only the evaluation's OWN report closes it: a report another session
    /// files for the same project in parallel must not restart this one.
    #[test]
    fn only_its_own_filed_report_closes_an_evaluation() {
        let dir = tempfile::tempdir().unwrap();
        let (db, mut store) = store_with_events(dir.path(), 3);
        let first = start(&db, &store, "sess", "p", 1_000, "skill").unwrap();

        record_filing(&db, &store, "other", "p", "eval-p-other", 1_010);
        file_report(&mut store, "eval-p-other", "Another session reviewed the quoter rules today.");
        assert_eq!(start(&db, &store, "sess", "p", 1_020, "stop-ask").unwrap(), first, "a parallel report closes nothing");

        record_filing(&db, &store, "sess", "p", "eval-p-mine", 1_030);
        file_report(&mut store, "eval-p-mine", "This session settled its batch and judged its list.");
        let next = start(&db, &store, "sess", "p", 1_040, "skill").unwrap();
        assert!(next.started_seq > first.started_seq, "its own filed report closes it: {next:?}");
    }

    /// The filing record is what the tool server reads back: session and
    /// start, by report id - and only while it is fresh.
    #[test]
    fn a_recorded_filing_hands_the_tool_server_its_scope() {
        let dir = tempfile::tempdir().unwrap();
        let (db, store) = store_with_events(dir.path(), 5);
        record_filing(&db, &store, "sess", "p", "eval-p-1", 2_000);
        let scope = scope_for_filing(&db, "eval-p-1", 2_010).expect("the scope is found");
        assert_eq!(scope, model::store::EvaluationScope { session_id: "sess".to_string(), started_seq: 5 });
        assert_eq!(scope_for_filing(&db, "eval-p-2", 2_010), None, "another report id has no scope");
        assert_eq!(scope_for_filing(&db, "eval-p-1", 2_000 + FILING_FRESH_SECS + 1), None, "a stale record is not trusted");
    }
}
