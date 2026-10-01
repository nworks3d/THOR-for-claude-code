//! ONE HOLDER PER PROJECT (step 26), end to end through the real `serve hook`
//! binary. The owner's decision: never two sessions in the same repository
//! owe judgement at once. Field case 2026-10-01, in one business project: two sessions
//! in the same checkout were both held for the same project's judgement debt
//! and evaluation, and did the same work twice.
//!
//! Per project the holder is the session that runs its open evaluation, else
//! the session holding the lease the evaluation ask took, else the first one
//! asked. Another session working there is not asked while the holder showed
//! activity in the last `EVALUATION_IDLE_MINUTES`; it gets one non-blocking
//! line naming the holder. Since step 28 the evaluation is the only debt that
//! holds a turn (no Stop holds one for a judgement or the rule upkeep), so it
//! is the only thing that takes the lease.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use thor_core::event_store::EventStore;

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

fn is_hold(v: &serde_json::Value) -> bool {
    v.get("decision").and_then(|d| d.as_str()) == Some("block")
}

fn reason(v: &serde_json::Value) -> String {
    v.get("reason").and_then(|r| r.as_str()).unwrap_or_default().to_string()
}

fn notice(v: &serde_json::Value) -> String {
    v.get("systemMessage").and_then(|r| r.as_str()).unwrap_or_default().to_string()
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

fn retry_stop(fx: &Fx, session: &str) {
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session,
        "stop_hook_active": true,
        "last_assistant_message": "",
        "cwd": fx.checkout().to_string_lossy(),
    });
    fx.hook(&payload.to_string());
}

#[test]
fn two_sessions_due_for_the_evaluation_in_one_project_only_the_first_asked_is_held() {
    let fx = Fx::new();
    EventStore::new(&fx.db()).unwrap();
    due_for(&fx.db(), "alpha", S1, 120);
    due_for(&fx.db(), "alpha", S2, 120);

    let first = fx.stop(S1);
    assert!(is_hold(&first), "the first session asked holds the project's evaluation: {first}");
    assert!(reason(&first).contains("evaluation"), "{first}");
    assert!(serve::holder::leases(&fx.db()).values().any(|l| l.session == S1), "and takes the lease");

    let second = fx.stop(S2);
    assert!(!is_hold(&second), "a second session in the same project is never asked while the holder is active: {second}");
    let line = notice(&second);
    assert!(line.contains(S1), "the one line names the holder: {second}");
    assert!(line.contains("alpha"), "and the project: {second}");
}

/// STEP 27 (D5), field case 2026-10-01: the retry Stop (`stop_hook_active`)
/// returns before the release at the end of the first-pass arm, so a session
/// kept the project's lease for as long as it stayed quiet. Since step 28 the
/// lease is the evaluation's: the retry frees it once the evaluation is no
/// longer open.
#[test]
fn the_retry_stop_after_the_evaluation_is_closed_releases_the_lease() {
    let fx = Fx::new();
    EventStore::new(&fx.db()).unwrap();
    due_for(&fx.db(), "alpha", S1, 120);
    assert!(is_hold(&fx.stop(S1)), "S1 is asked and takes the lease");
    assert!(serve::holder::leases(&fx.db()).values().any(|l| l.session == S1));

    // The evaluation is closed (another session's start supersedes it).
    let mut store = EventStore::open_existing(&fx.db()).unwrap();
    serve::attest::fixture::supersede_others(&mut store, S2, "alpha");
    drop(store);

    retry_stop(&fx, S1);
    let leases = serve::holder::leases(&fx.db());
    assert!(!leases.values().any(|l| l.session == S1), "the session no longer holds the lease: {leases:?}");
}

/// The same retry Stop while the evaluation is still open keeps the lease.
#[test]
fn the_retry_stop_keeps_the_lease_while_the_evaluation_is_still_open() {
    let fx = Fx::new();
    EventStore::new(&fx.db()).unwrap();
    due_for(&fx.db(), "alpha", S1, 120);
    assert!(is_hold(&fx.stop(S1)));
    retry_stop(&fx, S1);
    assert!(serve::holder::leases(&fx.db()).values().any(|l| l.session == S1));
}
