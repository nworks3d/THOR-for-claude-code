//! ONE HOLDER PER PROJECT (step 26), end to end through the real `serve hook`
//! binary. The owner's decision: never two sessions in the same repository
//! owe judgement at once. Field case 2026-10-01, in one business project: two sessions
//! in the same checkout were both held for the same project's judgement debt
//! and evaluation, and did the same work twice.
//!
//! Per project the holder is the session that runs its open evaluation, else
//! the session currently held for its debt, else the first one asked. Another
//! session working there is not held for any of that while the holder showed
//! activity in the last `EVALUATION_IDLE_MINUTES`; it gets one non-blocking
//! line naming the holder. A quiet holder is taken over: the lease moves.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use model::item::{Binding, Item, Kind, TargetKind};
use thor_core::event_store::EventStore;

const AFTER: usize = serve::usefulness::JUDGEMENT_DEBT_AFTER;
const S1: &str = "11111111-1111-4111-8111-111111111111";
const S2: &str = "22222222-2222-4222-8222-222222222222";

struct Fx {
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Fx {
    fn new() -> Self {
        let fx = Fx { dir: tempfile::tempdir().unwrap(), home: tempfile::tempdir().unwrap() };
        std::fs::create_dir_all(fx.checkout().join(".git")).unwrap();
        fx
    }
    fn db(&self) -> PathBuf {
        self.dir.path().join("thor.db")
    }
    /// A folder named `alpha` with a `.git` in it: the project `alpha`.
    fn checkout(&self) -> PathBuf {
        self.dir.path().join("alpha")
    }
    fn hook(&self, payload: &str) -> String {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
        cmd.arg("--db").arg(self.db()).arg("hook").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.env("HOME", self.home.path()).env("USERPROFILE", self.home.path());
        let mut child = cmd.spawn().expect("spawn serve");
        let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
        let out = child.wait_with_output().expect("wait serve");
        assert!(out.status.success(), "the hook must always exit 0");
        String::from_utf8(out.stdout).unwrap()
    }
    fn stop(&self, session: &str) -> serde_json::Value {
        let payload = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": session,
            "stop_hook_active": false,
            "last_assistant_message": "",
            "cwd": self.checkout().to_string_lossy(),
        });
        let out = self.hook(&payload.to_string());
        if out.trim().is_empty() {
            return serde_json::Value::Null;
        }
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("not json ({e}): {out}"))
    }
}

/// `n` rules of project `alpha`, each on its own command, decided as notes
/// and served `AFTER` times in `session`, `minutes_ago` minutes ago.
fn owed(store: &mut EventStore, prefix: &str, n: usize, session: &str, minutes_ago: i64) {
    let at = serve::time::iso8601_from_unix(serve::time::now_unix() - minutes_ago * 60);
    for i in 0..n {
        let id = format!("{prefix}-{i}");
        let item = Item {
            id: id.clone(),
            kind: Kind::Rule,
            text: format!("fixture rule {prefix} number {i} of project alpha"),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-{prefix}-command-{i}") }],
            severity: None,
            project: Some("alpha".to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some(format!("fixture rule {prefix} number {i} turns out to be wrong")),
            check: None,
        };
        model::store::declare(store, "fixture", "fixture", "fixture", &item).unwrap();
        serve::teeth::fixture::decide(store, &id);
        for _ in 0..AFTER {
            serve::deliver::record_delivery(store, session, session, serve::usefulness::ASSISTANT_DELIVERY_ACTOR, &at, &[id.clone()]);
        }
    }
}

fn is_hold(v: &serde_json::Value) -> bool {
    v.get("decision").and_then(|d| d.as_str()) == Some("block")
}

fn reason(v: &serde_json::Value) -> String {
    v.get("reason").and_then(|r| r.as_str()).unwrap_or_default().to_string()
}

fn notice(v: &serde_json::Value) -> String {
    v.get("systemMessage").and_then(|r| r.as_str()).unwrap_or_default().to_string()
}

#[test]
fn two_sessions_with_judgement_debt_in_one_project_only_the_holder_is_held() {
    let fx = Fx::new();
    let mut store = EventStore::new(&fx.db()).unwrap();
    owed(&mut store, "one", 2, S1, 1);
    owed(&mut store, "two", 2, S2, 1);
    drop(store);

    let first = fx.stop(S1);
    assert!(is_hold(&first), "the first session asked holds the project's debt: {first}");
    assert!(reason(&first).contains("fired repeatedly"), "{first}");

    let second = fx.stop(S2);
    assert!(!is_hold(&second), "a second session in the same project is never held while the holder is active: {second}");
    let line = notice(&second);
    assert!(line.contains(S1), "the one line names the holder: {second}");
    assert!(line.contains("alpha"), "and the project: {second}");
}

#[test]
fn a_quiet_holder_is_taken_over_by_the_next_session_asked() {
    let fx = Fx::new();
    let mut store = EventStore::new(&fx.db()).unwrap();
    owed(&mut store, "one", 2, S1, 45);
    owed(&mut store, "two", 2, S2, 1);
    drop(store);

    // S1 held the debt, then went quiet: its last activity is 45 minutes old.
    let first = fx.stop(S1);
    assert!(is_hold(&first), "{first}");
    let second = fx.stop(S2);
    assert!(is_hold(&second), "a holder quiet for longer than the idle window is taken over: {second}");
}

#[test]
fn a_holder_that_owes_nothing_any_more_lets_the_next_session_hold() {
    let fx = Fx::new();
    let mut store = EventStore::new(&fx.db()).unwrap();
    owed(&mut store, "one", 1, S1, 1);
    owed(&mut store, "two", 1, S2, 1);
    drop(store);

    assert!(is_hold(&fx.stop(S1)));
    // S1 judges its item with an attested-looking verdict path is not needed
    // here: a Stop of S1 that holds nothing releases the lease. Its debt is
    // paid by pinning the item (a pinned item owes no verdict).
    let mut store = EventStore::new(&fx.db()).unwrap();
    let mut item = model::store::show(&store, "one-0").unwrap();
    let before = item.clone();
    item.bindings = vec![Binding::Always];
    model::store::revise(&mut store, "fixture", "fixture", "fixture", &before, &item).unwrap();
    drop(store);
    let released = fx.stop(S1);
    assert!(!is_hold(&released), "S1 owes nothing now: {released}");

    let second = fx.stop(S2);
    assert!(is_hold(&second), "the lease was released, so the next session asked holds: {second}");
}

/// Seed `session`'s own work in `project` and the project's clock, so the
/// evaluation is due for it at a Stop that is not its first there.
fn due_for(db: &std::path::Path, project: &str, session: &str, minutes: i64) {
    let mut all = serve::usefulness::read_eval_debt_state(db);
    let entry = all.entry(project.to_string()).or_default();
    let now = serve::time::now_unix();
    let work = serve::usefulness::SessionWorkState {
        anchor_unix: Some(now - minutes * 60),
        accrued_secs: minutes * 60,
        last_event_unix: Some(now),
        stops: 2,
        ..Default::default()
    };
    entry.work = serve::usefulness::SessionWorkState { stops: 0, ..work.clone() };
    entry.sessions.insert(session.to_string(), work);
    std::fs::write(serve::usefulness::eval_debt_state_path(db), serde_json::to_string(&all).unwrap()).unwrap();
}

#[test]
fn the_evaluation_ask_waits_for_the_session_held_for_the_projects_debt() {
    let fx = Fx::new();
    let mut store = EventStore::new(&fx.db()).unwrap();
    owed(&mut store, "one", 1, S1, 1);
    drop(store);

    // S1 is held for the project's judgement debt: it holds the project.
    let first = fx.stop(S1);
    assert!(is_hold(&first) && reason(&first).contains("fired repeatedly"), "{first}");

    // S2 has worked long enough here that the evaluation is due for it.
    due_for(&fx.db(), "alpha", S2, 120);
    let second = fx.stop(S2);
    assert!(!is_hold(&second), "the evaluation ask waits while another session holds the project's debt: {second}");
    assert!(notice(&second).contains(S1), "{second}");
    let store = EventStore::new(&fx.db()).unwrap();
    assert!(model::store::open_evaluation_start(&store, S2, "alpha").is_none(), "no evaluation was started beside it");
}
