//! The hook's half of the evaluation handover (`serve::eval_session`), end
//! to end through the real compiled `serve hook` binary: where an evaluation
//! starts, that a later signal never moves the start, that the start names
//! the fixed list the report will be held to, and that the call filing the
//! report records which session files it - the one fact the tool server
//! cannot know itself.
//!
//! MEASURED 2026-09-26: a fresh evaluation filed its report eight times and
//! was refused eight times, on a judgement list that grew between attempts
//! (the evaluation's own writes fired the remember-bound rules, and parallel
//! sessions fired items it never saw). It ended up marking sixty items
//! useful. The filing gate now holds a report only to what fired in its own
//! session before its evaluation started; this file proves the hook records
//! both of those facts where the tool server reads them.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use model::item::{Binding, Item, Kind, TargetKind};
use thor_core::event_store::EventStore;

const AFTER: usize = serve::usefulness::JUDGEMENT_DEBT_AFTER;

/// A HOME of its own (so no real `~/.claude` is read) and a checkout that
/// resolves to `project` through its marker file.
struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self { home: tempfile::tempdir().unwrap() }
    }

    fn project_dir(&self, project: &str) -> PathBuf {
        let dir = self.home.path().join("checkouts").join(project);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".thor-project"), format!("{project}\n")).unwrap();
        dir
    }

    fn eval_command(&self) {
        let path = self.home.path().join(".claude").join("commands").join("thor-eval.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "fixture eval routine").unwrap();
    }
}

fn run_hook(db: &Path, payload: serde_json::Value, sandbox: &Sandbox) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(db).arg("hook").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.env("HOME", sandbox.home.path());
    cmd.env("USERPROFILE", sandbox.home.path());
    let mut child = cmd.spawn().expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    assert!(out.stderr.is_empty(), "the hook must never write to stderr: {:?}", out.stderr);
    String::from_utf8(out.stdout).unwrap()
}

fn pretooluse(session: &str, cwd: &Path, tool_name: &str, tool_input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "tool_name": tool_name,
        "tool_input": tool_input,
    })
}

fn prompt(session: &str, cwd: &Path, text: &str) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "prompt": text,
    })
}

/// A light rule of `project`, fired `n` times under `session`.
fn fired_rule(store: &mut EventStore, id: &str, project: &str, session: &str, n: usize) {
    let item = Item {
        id: id.to_string(),
        kind: Kind::Rule,
        // Texts far enough apart that no two read as near-duplicates.
        text: match id {
            "seen-here" => "Restart the print queue before swapping a spool.".to_string(),
            "seen-elsewhere" => "Label every archive box with its shelf number.".to_string(),
            _ => format!("Rotate the {id} credentials each quarter."),
        },
        bindings: vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-command-{id}") }],
        severity: None,
        project: Some(project.to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(format!("fixture rule {id} turns out to be wrong")),
        check: None,
    };
    model::store::declare(store, "fixture", "fixture", "fixture", &item).unwrap();
    for _ in 0..n {
        serve::deliver::record_delivery(store, session, session, serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-26T00:00:00Z", &[id.to_string()]);
    }
}

fn context(out: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(out).unwrap_or_else(|e| panic!("expected JSON: {e}: {out}"));
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

fn only_evaluation(db: &Path) -> serve::eval_session::Evaluation {
    let state = serve::eval_session::read(db);
    assert_eq!(state.evaluations.len(), 1, "{state:?}");
    state.evaluations[0].clone()
}

fn tip(db: &Path) -> i64 {
    EventStore::open_existing(db).unwrap().get_next_seq().unwrap() - 1
}

#[test]
fn opening_the_evaluation_skill_starts_it_and_names_its_fixed_list() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.project_dir("proj");
    {
        let mut store = EventStore::new(&db).unwrap();
        fired_rule(&mut store, "seen-here", "proj", "s1", AFTER);
        fired_rule(&mut store, "seen-elsewhere", "proj", "another-session", AFTER);
    }
    let start = tip(&db);

    let out = run_hook(&db, pretooluse("s1", &cwd, "Skill", serde_json::json!({"skill": "thor-eval"})), &sandbox);
    let note = context(&out);
    assert!(note.contains("The evaluation of 'proj' started"), "{note}");
    assert!(note.contains("'seen-here'"), "what fired in this session is on the list: {note}");
    assert!(!note.contains("seen-elsewhere"), "what fired only elsewhere is not: {note}");

    let evaluation = only_evaluation(&db);
    assert_eq!((evaluation.session_id.as_str(), evaluation.project.as_str()), ("s1", "proj"));
    assert_eq!(evaluation.started_seq, start, "it starts at the log's tip when the skill opens");
    assert_eq!(evaluation.started_by, "skill");
}

#[test]
fn a_later_start_signal_never_moves_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.project_dir("proj");
    {
        let mut store = EventStore::new(&db).unwrap();
        fired_rule(&mut store, "seen-here", "proj", "s1", AFTER);
    }
    run_hook(&db, pretooluse("s1", &cwd, "Skill", serde_json::json!({"skill": "thor-eval"})), &sandbox);
    let first = only_evaluation(&db);
    {
        let mut store = EventStore::open_existing(&db).unwrap();
        fired_rule(&mut store, "fired-during", "proj", "s1", AFTER);
    }
    run_hook(&db, prompt("s1", &cwd, "/thor-eval"), &sandbox);
    assert_eq!(only_evaluation(&db), first, "the start stays where it was");
}

#[test]
fn filing_the_report_records_the_session_for_the_tool_server() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.project_dir("proj");
    {
        let mut store = EventStore::new(&db).unwrap();
        fired_rule(&mut store, "seen-here", "proj", "s1", AFTER);
    }
    // The owner's own name for the routine, listed beside the store.
    std::fs::write(serve::eval_session::extra_names_path(&db), "my-eval\n").unwrap();
    run_hook(&db, prompt("s1", &cwd, "/my-eval"), &sandbox);
    let started = only_evaluation(&db);
    assert_eq!(started.started_by, "command");

    let filing = serde_json::json!({
        "id": "eval-proj-2026-09-26", "kind": "report", "project": "proj",
        "tags": ["evaluation-report", "proj"], "text": "fixture report"
    });
    run_hook(&db, pretooluse("s1", &cwd, "mcp__thor__remember", filing), &sandbox);
    let scope = serve::eval_session::scope_for_filing(&db, "eval-proj-2026-09-26", serve::time::now_unix())
        .expect("the tool server finds who files it");
    assert_eq!(scope, model::store::EvaluationScope { session_id: "s1".to_string(), started_seq: started.started_seq });
}

/// No start signal before the filing: the first filing attempt is the start,
/// so a second attempt is held to the same list as the first.
#[test]
fn with_no_earlier_signal_the_first_filing_attempt_starts_the_evaluation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.project_dir("proj");
    EventStore::new(&db).unwrap();
    let filing = serde_json::json!({"id": "eval-proj-1", "kind": "report", "project": "proj", "tags": ["evaluation-report"]});
    run_hook(&db, pretooluse("s1", &cwd, "mcp__thor__remember", filing.clone()), &sandbox);
    let first = only_evaluation(&db);
    assert_eq!(first.started_by, "filing");
    run_hook(&db, pretooluse("s1", &cwd, "mcp__thor__remember", filing), &sandbox);
    assert_eq!(only_evaluation(&db), first, "a second attempt never moves the start");
}

/// A task that merely says to end with the evaluation has not started one:
/// the start comes when the routine is actually opened.
#[test]
fn a_task_that_only_mentions_the_command_starts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.project_dir("proj");
    EventStore::new(&db).unwrap();
    run_hook(&db, prompt("s1", &cwd, "Fix the Decline button. End with /thor-eval."), &sandbox);
    assert!(serve::eval_session::read(&db).evaluations.is_empty());
}

/// The Stop hook's own ask is a start too, and its message names the list.
#[test]
fn the_stop_ask_starts_the_evaluation_and_names_its_list() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    sandbox.eval_command();
    let cwd = sandbox.project_dir("proj");
    {
        let mut store = EventStore::new(&db).unwrap();
        fired_rule(&mut store, "seen-here", "proj", "s1", AFTER);
    }
    // An hour and a half of work in this project, no report yet.
    let mut all = serve::usefulness::read_eval_debt_state(&db);
    let mut entry = all.get("proj").cloned().unwrap_or_default();
    let now = serve::time::now_unix();
    entry.sessions.insert(
        "s1".to_string(),
        serve::usefulness::SessionWorkState {
            anchor_unix: Some(now - 90 * 60),
            accrued_secs: 90 * 60,
            last_event_unix: Some(now),
            edits_since_test: 0,
            compacted_since_anchor: false,
        },
    );
    all.insert("proj".to_string(), entry);
    std::fs::write(serve::usefulness::eval_debt_state_path(&db), serde_json::to_string(&all).unwrap()).unwrap();

    let stop = serde_json::json!({
        "hook_event_name": "Stop", "session_id": "s1", "stop_hook_active": false,
        "last_assistant_message": "", "cwd": cwd.to_string_lossy(),
    });
    let out = run_hook(&db, stop, &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("The evaluation of 'proj' started"), "{reason}");
    assert!(reason.contains("'seen-here'"), "{reason}");
    assert_eq!(only_evaluation(&db).started_by, "stop-ask");
}
