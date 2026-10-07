//! Where an evaluation starts (`serve::eval_session`, step 13), end to end
//! through the real compiled `serve hook` binary: the start is ONE record in
//! the log with the lists its report is held to frozen inside it, the note
//! names exactly those lists, a later signal never moves the start, and the
//! old handover file beside the store means nothing any more.
//!
//! Since step 14 the record is written in two events: the start (judgement
//! list and open points), then - after THOR's own pass over the project's
//! old rules - the batch record on the same id. A hook cut off in between
//! leaves a start with no batch, and the session's next Stop finishes it.
//!
//! MEASURED 2026-09-26: a fresh evaluation filed its report eight times and
//! was refused eight times, on a judgement list that grew between attempts
//! (the evaluation's own writes fired the remember-bound rules, and parallel
//! sessions fired items it never saw). It ended up marking sixty items
//! useful. The list was then fixed at the start - in a sidecar any local
//! process could edit. Since step 13 it is fixed in the log itself.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use model::item::{Binding, Item, Kind, TargetKind};
use model::store::EvaluationStart;
use thor_core::event_store::{Event, EventKind, EventStore};

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

fn skill(session: &str, cwd: &Path, name: &str) -> serde_json::Value {
    pretooluse(session, cwd, "Skill", serde_json::json!({"skill": name}))
}

fn prompt(session: &str, cwd: &Path, text: &str) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "prompt": text,
    })
}

fn stop(session: &str, cwd: &Path) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name": "Stop", "session_id": session, "stop_hook_active": false,
        "last_assistant_message": "", "cwd": cwd.to_string_lossy(),
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
    for i in 0..n {
        serve::deliver::record_delivery(store, &serve::deliver::nth_session(session, i), session, serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-26T00:00:00Z", &[id.to_string()]);
    }
}

/// An hour and a half of work by `session` in `project` and no report yet:
/// the evaluation is due, so the Stop hook asks for it.
fn make_the_evaluation_due(db: &Path, project: &str, session: &str) {
    let mut all = serve::usefulness::read_eval_debt_state(db);
    let mut entry = all.get(project).cloned().unwrap_or_default();
    let now = serve::time::now_unix();
    entry.sessions.insert(
        session.to_string(),
        serve::usefulness::SessionWorkState {
            anchor_unix: Some(now - 90 * 60),
            accrued_secs: 90 * 60,
            last_event_unix: Some(now),
            edits_since_test: 0,
            compacted_since_anchor: false,
            // It ended a turn here before: a first Stop is never held (Q10).
            stops: 1,
        },
    );
    // The project's own clock (step 17) carries the same work.
    entry.work = serve::usefulness::SessionWorkState {
        anchor_unix: Some(now - 90 * 60),
        accrued_secs: 90 * 60,
        last_event_unix: Some(now),
        ..Default::default()
    };
    all.insert(project.to_string(), entry);
    std::fs::write(serve::usefulness::eval_debt_state_path(db), serde_json::to_string(&all).unwrap()).unwrap();
}

fn context(out: &str) -> String {
    if out.trim().is_empty() {
        return String::new();
    }
    let v: serde_json::Value = serde_json::from_str(out).unwrap_or_else(|e| panic!("expected JSON: {e}: {out}"));
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

/// Every evaluation-start event in the log.
fn start_events(db: &Path) -> Vec<Event> {
    EventStore::open_existing(db)
        .unwrap()
        .get_all_events()
        .unwrap()
        .into_iter()
        .filter(|e| e.entity_id.starts_with(model::store::EVALUATION_START_PREFIX))
        .collect()
}

/// How many evaluations the log holds a start of: the `fact_created` events
/// on start records (each start's batch record is a revise of the same id).
fn starts_created(db: &Path) -> usize {
    start_events(db).iter().filter(|e| e.kind == EventKind::FactCreated).count()
}

/// The one start record `session` wrote, read back the way every reader does.
fn only_start(db: &Path, session: &str) -> EvaluationStart {
    let store = EventStore::open_existing(db).unwrap();
    let mut starts = model::store::evaluation_starts_of_session(&store, session);
    assert_eq!(starts.len(), 1, "{starts:?}");
    assert_eq!(starts_created(db), 1, "and the log holds no other start");
    starts.remove(0)
}

fn tip(db: &Path) -> i64 {
    EventStore::open_existing(db).unwrap().get_next_seq().unwrap() - 1
}

/// The ids a note names in its judgement section.
fn assert_note_names_the_frozen_lists(note: &str, start: &EvaluationStart) {
    assert!(note.contains(&format!("started at log position {}", start.frozen.started_seq)), "{note}");
    for id in &start.frozen.judgement {
        assert!(note.contains(&format!("mcp__thor__mark(id: \"{id}\")")), "the exact call for '{id}': {note}");
    }
    for id in start.frozen.batch() {
        assert!(note.contains(&format!("mcp__thor__revise(id: \"{id}\")")), "the exact call for '{id}': {note}");
    }
    for id in start.frozen.a4_list() {
        assert!(note.contains(&format!("mcp__thor__revise(id: \"{id}\", text: ")), "the exact call for '{id}': {note}");
    }
    assert!(note.contains("1. JUDGEMENT") && note.contains("2. TEETH") && note.contains("4. OPEN POINTS"), "{note}");
}

/// The Stop hook's ask starts the evaluation: one start record, by
/// `eval-machine`, in the asking session, and the block reason carries the
/// note with the lists the record froze. A second ask writes nothing more.
#[test]
fn stop_ask_writes_one_start_record() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    sandbox.eval_command();
    let cwd = sandbox.project_dir("proj");
    {
        let mut store = EventStore::new(&db).unwrap();
        fired_rule(&mut store, "seen-here", "proj", "s1", AFTER);
        fired_rule(&mut store, "seen-elsewhere", "proj", "another-session", AFTER);
    }
    make_the_evaluation_due(&db, "proj", "s1");
    let before = tip(&db);

    let out = run_hook(&db, stop("s1", &cwd), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("The evaluation of 'proj' started"), "{reason}");

    let start = only_start(&db, "s1");
    assert_eq!((start.frozen.session.as_str(), start.frozen.project.as_str()), ("s1", "proj"));
    assert_eq!(start.frozen.started_seq, before, "it starts at the log's tip when the hook asks");
    assert_eq!(start.frozen.started_by, "stop-ask");
    assert_eq!(start.frozen.judgement, vec!["seen-here".to_string()], "what fired only elsewhere is not on the list");
    // Both rules had no verdict: THOR's pass tried them itself, oldest write
    // first, found nothing to build a tooth from, and wrote each as a note -
    // so nothing is left for the session's batch.
    let machine = start.frozen.machine.clone().expect("the batch record holds the machine's decisions");
    assert_eq!(machine.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["seen-here", "seen-elsewhere"]);
    assert_eq!((machine.notes, machine.needs_agent, machine.not_reached), (2, 0, 0), "{machine:?}");
    assert_eq!(start.frozen.batch, Some(vec![]));
    assert_eq!(start.frozen.a4_list, Some(vec![]));
    assert_note_names_the_frozen_lists(reason, &start);
    assert!(reason.contains("THOR tried 2 old rule(s) of this project itself") && reason.contains("2 became a note"), "{reason}");
    // The start record first, with no batch; then the one batch record.
    let events = start_events(&db);
    assert_eq!(events.iter().map(|e| e.kind).collect::<Vec<_>>(), vec![EventKind::FactCreated, EventKind::FactRevised]);
    assert!(events.iter().all(|e| e.actor == "eval-machine" && e.session_id == "s1"), "{events:?}");
    assert!(!events[0].body.contains("\"batch\":"), "{}", events[0].body);
    {
        let store = EventStore::open_existing(&db).unwrap();
        for id in ["seen-here", "seen-elsewhere"] {
            assert_eq!(model::store::current_verdict(&store, id), model::store::CurrentVerdict::Note, "{id}");
            let write = store.get_events_by_entity(id).unwrap().into_iter().rev().find(|e| e.kind == EventKind::FactRevised).unwrap();
            assert_eq!(write.actor, "teeth-eval", "{id}: decided by THOR's own pass");
            assert!(write.seq > events[0].seq && write.seq < events[1].seq, "{id}: between the start record and the batch record");
        }
    }

    // The next turn's ask finds it open: same start, nothing written.
    let again = run_hook(&db, stop("s1", &cwd), &sandbox);
    assert!(again.contains(&format!("started at log position {before}")), "{again}");
    assert_eq!(only_start(&db, "s1"), start);
}

/// R13.1: a skill call while no evaluation is due starts nothing - the log
/// gains no start event - and says so in plain words.
#[test]
fn skill_call_before_the_debt_starts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.project_dir("proj");
    {
        let mut store = EventStore::new(&db).unwrap();
        fired_rule(&mut store, "seen-here", "proj", "s1", AFTER);
    }
    for name in ["t-eval", "thor-eval"] {
        let note = context(&run_hook(&db, skill("s1", &cwd, name), &sandbox));
        assert!(note.contains("no evaluation of proj is due yet - nothing to do"), "{name}: {note}");
        assert!(!note.contains("started at log position"), "{name}: {note}");
    }
    assert!(start_events(&db).is_empty(), "nothing started");
}

/// While the evaluation IS due, the same skill call starts it - and a second
/// call, under either name, is told to continue the open one: the start note
/// again, and no second start event (R13.3).
#[test]
fn skill_call_during_an_open_evaluation_says_continue_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.project_dir("proj");
    {
        let mut store = EventStore::new(&db).unwrap();
        fired_rule(&mut store, "seen-here", "proj", "s1", AFTER);
    }
    make_the_evaluation_due(&db, "proj", "s1");

    let note = context(&run_hook(&db, skill("s1", &cwd, "t-eval"), &sandbox));
    assert!(note.contains("The evaluation of 'proj' started"), "{note}");
    let start = only_start(&db, "s1");
    assert_eq!(start.frozen.started_by, "skill");
    assert_note_names_the_frozen_lists(&note, &start);

    {
        let mut store = EventStore::open_existing(&db).unwrap();
        fired_rule(&mut store, "fired-during", "proj", "s1", AFTER);
    }
    let again = context(&run_hook(&db, skill("s1", &cwd, "thor-eval"), &sandbox));
    assert!(
        again.contains(&format!("already open since log seq {}", start.frozen.started_seq)) && again.contains("in this session) - continue it"),
        "{again}"
    );
    assert_note_names_the_frozen_lists(&again, &start);
    assert!(!again.contains("fired-during"), "what fired after the start is not on the list: {again}");
    assert_eq!(only_start(&db, "s1"), start, "no second start event");

    // Another session opening the routine, with no evaluation due for it:
    // told where the open one runs, and nothing starts.
    let other = context(&run_hook(&db, skill("s2", &cwd, "thor-eval"), &sandbox));
    assert!(other.contains("in another session) - continue it there"), "{other}");
    assert_eq!(starts_created(&db), 1);
}

/// F16: `/t-eval` and `/thor-eval` are the same signal, and so is a name the
/// owner listed beside the store.
#[test]
fn both_command_names_start_one() {
    for (n, command) in ["/t-eval", "/thor-eval", "/my-eval"].into_iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let sandbox = Sandbox::new();
        let cwd = sandbox.project_dir("proj");
        {
            let mut store = EventStore::new(&db).unwrap();
            fired_rule(&mut store, "seen-here", "proj", "s1", AFTER);
        }
        std::fs::write(serve::eval_session::extra_names_path(&db), "my-eval\n").unwrap();
        let note = context(&run_hook(&db, prompt("s1", &cwd, command), &sandbox));
        let start = only_start(&db, "s1");
        assert_eq!(start.frozen.started_by, "command", "case {n}: {command}");
        assert_note_names_the_frozen_lists(&note, &start);
    }
}

/// G13.1: the owner's typed command starts an evaluation whether or not one
/// is due; the log holds one start event whose frozen judgement list equals
/// the ids in the note. Typing it again keeps the open one.
#[test]
fn typed_command_always_starts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.project_dir("proj");
    {
        let mut store = EventStore::new(&db).unwrap();
        fired_rule(&mut store, "seen-here", "proj", "s1", AFTER);
        fired_rule(&mut store, "seen-elsewhere", "proj", "another-session", AFTER);
    }
    let before = tip(&db);
    // No work accrued at all: nothing is due, and the command starts anyway.
    let note = context(&run_hook(&db, prompt("s1", &cwd, "/t-eval"), &sandbox));
    assert!(note.contains(&format!("started at log position {before}")), "{note}");
    let start = only_start(&db, "s1");
    assert_eq!(start.frozen.judgement, vec!["seen-here".to_string()]);
    assert_note_names_the_frozen_lists(&note, &start);
    assert!(!note.contains("mcp__thor__mark(id: \"seen-elsewhere\")"), "only the frozen ids are named for a verdict: {note}");

    let again = context(&run_hook(&db, prompt("s1", &cwd, "/thor-eval"), &sandbox));
    assert!(again.contains("already open since log seq") && again.contains("started at log position"), "{again}");
    assert_eq!(only_start(&db, "s1"), start, "the start stays where it was");
}

/// A prompt a subagent receives is not the owner typing: it starts nothing.
/// Nor does a task that merely says to end with the command.
#[test]
fn agent_id_prompt_starts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.project_dir("proj");
    EventStore::new(&db).unwrap();
    let mut from_agent = prompt("s1", &cwd, "/t-eval");
    from_agent["agent_id"] = serde_json::json!("a1");
    let note = context(&run_hook(&db, from_agent, &sandbox));
    assert!(!note.contains("evaluation of"), "{note}");
    run_hook(&db, prompt("s1", &cwd, "Fix the Decline button. End with /thor-eval."), &sandbox);
    assert!(start_events(&db).is_empty());
}

/// The old handover file is never read again: one planted beside the store -
/// claiming an evaluation that started later, with nothing on its list -
/// changes neither the start nor the list, and the next Stop deletes it.
#[test]
fn a_planted_handover_sidecar_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.project_dir("proj");
    {
        let mut store = EventStore::new(&db).unwrap();
        fired_rule(&mut store, "seen-here", "proj", "s1", AFTER);
    }
    let note = context(&run_hook(&db, prompt("s1", &cwd, "/t-eval"), &sandbox));
    let start = only_start(&db, "s1");
    assert_note_names_the_frozen_lists(&note, &start);

    let sidecar = serve::eval_session::retired_handover_path(&db);
    let planted = serde_json::json!({
        "evaluations": [{"session_id": "s1", "project": "proj", "started_seq": tip(&db) + 50, "started_unix": serve::time::now_unix(), "started_by": "skill"}],
        "filings": [{"report_id": "eval-proj-1", "session_id": "someone-else", "project": "proj", "at_unix": serve::time::now_unix()}],
    });
    std::fs::write(&sidecar, planted.to_string()).unwrap();

    let store = EventStore::open_existing(&db).unwrap();
    assert_eq!(serve::eval_session::open_evaluation(&store, "s1", "proj"), Some(start.clone()), "the log's start stands");
    let report = Item {
        id: "eval-proj-1".to_string(),
        kind: Kind::Report,
        text: "fixture report".to_string(),
        bindings: vec![],
        severity: None,
        project: Some("proj".to_string()),
        tags: vec![model::store::EVALUATION_REPORT_TAG.to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    // The filing is held to the start record the log holds for s1 (the
    // tool server reads exactly this one: `open_evaluation_start`).
    let open = model::store::open_evaluation_start(&store, "s1", &report.project.clone().unwrap()).expect("the filing finds the log's start");
    assert_eq!(open.frozen.started_seq, start.frozen.started_seq);
    let judgement = model::store::evaluation_judgement_owed(&store, &open, &[]);
    assert_eq!(judgement.owed, vec!["seen-here".to_string()], "the list is the frozen one, not the sidecar's empty one");
    drop(store);

    run_hook(&db, stop("s1", &cwd), &sandbox);
    assert!(!sidecar.exists(), "the first Stop deletes the retired file");
    assert_eq!(only_start(&db, "s1"), start);
}

/// R14.8 (F10): the hook that started an evaluation was killed after the
/// start record and before the batch record. A filing attempted in between
/// is refused with the "still deciding" sentence; the session's next Stop
/// finishes THOR's pass, writes the batch record and shows the start note.
#[test]
fn a_cut_off_machine_pass_is_finished_at_the_next_stop() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.project_dir("proj");
    {
        let mut store = EventStore::new(&db).unwrap();
        fired_rule(&mut store, "seen-here", "proj", "s1", 1);
        // What a killed hook leaves: the first of the two writes only.
        let began = serve::eval_session::begin(&db, &mut store, "s1", "proj", "stop-ask").unwrap();
        assert!(began.fresh && !began.start.frozen.decided());
        let report = Item {
            id: "eval-proj-1".to_string(),
            kind: Kind::Report,
            text: "fixture report".to_string(),
            bindings: vec![],
            severity: None,
            project: Some("proj".to_string()),
            tags: vec![model::store::EVALUATION_REPORT_TAG.to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        let open = serve::eval_session::open_evaluation(&store, "s1", "proj").expect("the open start is found");
        let ctx = model::store::FilingContext::new(model::store::INFLIGHT_SEEN, open);
        let err = model::store::declare_scoped(&mut store, "s1", "l", "mcp", &report, ctx.as_ref(), None).unwrap_err().to_string();
        assert!(err.contains("THOR is still deciding the old rules - end this turn once"), "{err}");
        assert!(model::store::show(&store, "eval-proj-1").is_err(), "nothing filed");
    }
    assert_eq!(start_events(&db).len(), 1, "only the start record so far");

    let out = run_hook(&db, stop("s1", &cwd), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("THOR finished deciding the old rules of 'proj'") && reason.contains("2. TEETH - THOR tried 1 old rule(s)"), "{reason}");
    let start = only_start(&db, "s1");
    assert!(start.frozen.decided(), "{start:?}");
    assert_eq!(start_events(&db).len(), 2, "the batch record is written");

    // Once: the next Stop has nothing to resume.
    let again = run_hook(&db, stop("s1", &cwd), &sandbox);
    assert!(!again.contains("finished deciding"), "{again}");
    assert_eq!(start_events(&db).len(), 2);
}

/// The same cut-off pass, met by a Stop that already held this turn once
/// (`stop_hook_active`): the pass is finished all the same, and the start
/// note arrives as a warning - that Stop may not hold the turn again.
#[test]
fn a_cut_off_pass_is_finished_even_when_the_stop_may_not_hold_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.project_dir("proj");
    {
        let mut store = EventStore::new(&db).unwrap();
        fired_rule(&mut store, "seen-here", "proj", "s1", 1);
        serve::eval_session::begin(&db, &mut store, "s1", "proj", "command").unwrap();
    }
    let mut payload = stop("s1", &cwd);
    payload["stop_hook_active"] = serde_json::json!(true);
    let out = run_hook(&db, payload, &sandbox);
    assert!(out.contains("THOR finished deciding the old rules of 'proj'"), "{out}");
    assert!(!out.contains("\"decision\":\"block\""), "a second hold of the same turn is never issued: {out}");
    assert!(only_start(&db, "s1").frozen.decided());

    // A subagent's Stop finishes nothing: the evaluation is the main session's.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    {
        let mut store = EventStore::new(&db).unwrap();
        serve::eval_session::begin(&db, &mut store, "s1", "proj", "command").unwrap();
    }
    let mut payload = stop("s1", &cwd);
    payload["agent_id"] = serde_json::json!("a1");
    run_hook(&db, payload, &sandbox);
    assert!(!only_start(&db, "s1").frozen.decided(), "left for the main session's own Stop");
}
