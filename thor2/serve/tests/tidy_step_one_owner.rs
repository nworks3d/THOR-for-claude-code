//! The tidy step belongs to ONE conversation. Field finding: a conversation
//! prepared for another subject was held for the same step, so two
//! conversations were held at once. Only the owner is held; any other
//! conversation gets at most one plain line and is never held; an owner quiet
//! for 30 minutes is taken over; and the owner cannot leave without the
//! evidence (the person's answer plus THOR's own tidy record).
//! Isolated: HOME and USERPROFILE point at a throwaway folder.

use model::store::{ORDER_NOTE_ID, OWNER_SETUP_ANSWERS_ID, SETUP_NOTE_ID};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use thor_core::event_store::EventStore;

const A: &str = "aaaaaaaa-2222-3333-4444-555555555555";
const B: &str = "bbbbbbbb-2222-3333-4444-555555555555";

struct Fx {
    root: tempfile::TempDir,
}

impl Fx {
    fn new() -> Self {
        let f = Self { root: tempfile::tempdir().unwrap() };
        std::fs::create_dir_all(f.projects().join("enc")).unwrap();
        // The folder belongs to no project; the ask to move is not what these tests are about.
        std::fs::write(f.root.path().join("no-project-ask-mode.json"), "{\"mode\": \"off\"}").unwrap();
        let item = |id: &str, kind: model::item::Kind| model::item::Item {
            id: id.to_string(),
            kind,
            text: "fixture note".to_string(),
            bindings: vec![model::item::Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("fixture falsifier".to_string()),
            check: None,
        };
        let mut store = EventStore::new(&f.db()).unwrap();
        model::store::declare(&mut store, "install", "install", "install-fixture", &item(SETUP_NOTE_ID, model::item::Kind::Rule)).unwrap();
        model::store::declare(&mut store, "install", "install", "install-fixture", &item(ORDER_NOTE_ID, model::item::Kind::Orientation)).unwrap();
        let mut answers = item(OWNER_SETUP_ANSWERS_ID, model::item::Kind::Report);
        answers.bindings = vec![];
        model::store::declare(&mut store, "install", "install", "install-fixture", &answers).unwrap();
        model::store::retract(&mut store, "s", "l", "fixture", SETUP_NOTE_ID, "answered", None).unwrap();
        f
    }
    fn db(&self) -> PathBuf {
        self.root.path().join("thor.db")
    }
    fn projects(&self) -> PathBuf {
        self.root.path().join(".claude").join("projects")
    }
    fn transcript(&self, session: &str) -> PathBuf {
        self.projects().join("enc").join(format!("{session}.jsonl"))
    }
    fn write(&self, session: &str, lines: &[String]) {
        std::fs::write(self.transcript(session), lines.join("\n") + "\n").unwrap();
        // The owner typed each of his messages: the hook recorded each prompt (`serve::prompt_record`).
        for text in lines.iter().filter_map(|l| serve::attest::owner_text(l)) {
            serve::prompt_record::record(&self.db(), session, &text, 0);
        }
    }
    fn stop(&self, session: &str) -> serde_json::Value {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
        cmd.arg("--db").arg(self.db()).arg("hook").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.env("HOME", self.root.path()).env("USERPROFILE", self.root.path()).env_remove("CLAUDE_CODE_ENTRYPOINT");
        let mut child = cmd.spawn().unwrap();
        let payload = serde_json::json!({
            "hook_event_name": "Stop", "session_id": session, "stop_hook_active": false, "last_assistant_message": "",
            "transcript_path": self.transcript(session).display().to_string(), "cwd": self.root.path().display().to_string(),
        });
        let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        if text.trim().is_empty() {
            return serde_json::Value::Null;
        }
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON: {e}: {text}"))
    }
    fn live(&self) -> bool {
        let store = EventStore::open_existing(&self.db()).unwrap();
        model::store::show(&store, ORDER_NOTE_ID).is_ok()
    }
}

fn person(at: &str, text: &str) -> String {
    serde_json::json!({"type":"user","timestamp":at,"message":{"role":"user","content":text}}).to_string()
}

fn assistant(at: &str, text: &str) -> String {
    serde_json::json!({"type":"assistant","timestamp":at,"message":{"role":"assistant","content":[{"type":"text","text":text}]}}).to_string()
}

fn setup_call(at: &str) -> String {
    serde_json::json!({"type":"assistant","timestamp":at,"message":{"role":"assistant","content":[{"type":"tool_use","name":"mcp__thor__remember","input":{"id":"owner-setup-answers","kind":"report","text":"Setup finished."}}]}}).to_string()
}

/// A conversation whose welcome was answered (the first-session setup ran in it), no proposal yet.
fn welcome_answered() -> Vec<String> {
    vec![
        person("2026-03-02T10:00:00Z", "Help me with my CV"),
        assistant("2026-03-02T10:01:00Z", "Welcome. Shall I set it up like this?"),
        person("2026-03-02T10:02:00Z", "yes"),
        setup_call("2026-03-02T10:02:30Z"),
    ]
}

/// A conversation with nothing to do with the setup (for example one prepared for another subject).
fn no_stake() -> Vec<String> {
    vec![
        person("2026-03-02T10:00:00Z", "Plan my kitchen"),
        assistant("2026-03-02T10:01:00Z", "Sure."),
        person("2026-03-02T10:02:00Z", "ok"),
    ]
}

fn blocked(v: &serde_json::Value) -> bool {
    v["decision"] == "block"
}

fn say(v: &serde_json::Value) -> String {
    v["systemMessage"].as_str().unwrap_or_default().to_string()
}

#[allow(dead_code)]
fn unused(_: &Path) {}

#[test]
fn only_the_owner_is_held_and_the_other_conversation_gets_one_line() {
    let f = Fx::new();
    f.write(A, &welcome_answered());
    f.write(B, &no_stake());

    let first = f.stop(A);
    assert!(blocked(&first), "the first conversation held for the step owns it: {first}");
    assert_eq!(serve::order_earlier::owner_on_file(&f.db()).unwrap().session, A);

    let second = f.stop(B);
    assert!(!blocked(&second), "a second conversation is never held for the same step: {second}");
    assert!(say(&second).contains("Another conversation is finishing the last setup step"), "{second}");
    assert!(!say(&second).contains(A) && !say(&second).contains("minute"), "no session id and no changing minutes: {second}");
    // Across many turns the line comes once per conversation, never again.
    for turn in 0..6 {
        let again = f.stop(B);
        assert!(!blocked(&again) && !say(&again).contains("Another conversation"), "turn {turn}: {again}");
    }

    // The owner is still held: nothing was settled by the other conversation.
    assert!(blocked(&f.stop(A)));
    assert!(f.live());
}

#[test]
fn a_quiet_owner_is_taken_over_after_thirty_minutes() {
    let f = Fx::new();
    f.write(A, &welcome_answered());
    f.write(B, &no_stake());
    assert!(blocked(&f.stop(A)));
    // Quiet for 31 minutes: the owner's last sign of life on file is that old, and the log holds none.
    let mut owner = serve::order_earlier::owner_on_file(&f.db()).unwrap();
    owner.seen_unix -= 31 * 60;
    std::fs::write(f.root.path().join(serve::order_earlier::OWNER_FILE), serde_json::to_string(&owner).unwrap()).unwrap();
    let store = EventStore::open_existing(&f.db()).unwrap();
    assert!(
        serve::usefulness::session_idle_minutes(&store, A, serve::time::now_unix(), 30).is_none(),
        "fixture sanity: the owner left no recent trace on the log"
    );
    drop(store);

    let taken = f.stop(B);
    assert!(blocked(&taken), "the next conversation that ends a turn becomes the owner: {taken}");
    assert_eq!(serve::order_earlier::owner_on_file(&f.db()).unwrap().session, B);
    // B owns it with no stake only as a fallback: A, which has a stake, takes it back at once.
    assert!(blocked(&f.stop(A)));
    assert_eq!(serve::order_earlier::owner_on_file(&f.db()).unwrap().session, A);
    assert!(!blocked(&f.stop(B)));
}

#[test]
fn the_owner_cannot_leave_without_the_evidence_and_the_persons_answer() {
    let f = Fx::new();
    let call = serde_json::json!({"type":"assistant","timestamp":"2026-03-02T10:03:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"\"C:/thor2/serve.exe\" --db x inventory"}}]}}).to_string();
    let mut lines = welcome_answered();
    lines.push(call);
    lines.push(assistant("2026-03-02T10:04:00Z", "Proposal:\n1. CV and job hunting\nShall I do it? Say yes."));
    f.write(A, &lines);
    f.write(B, &no_stake());
    assert!(!blocked(&f.stop(A)), "a proposal waiting for the answer ends the turn");
    lines.push(person("2026-03-02T10:05:00Z", "ja"));
    f.write(A, &lines);
    // The person said yes but nothing was carried out: the owner stays held, again and again.
    assert!(blocked(&f.stop(A)));
    assert!(blocked(&f.stop(A)));
    // The other conversation cannot settle it either.
    assert!(!blocked(&f.stop(B)));
    assert!(f.live());
    // THOR's own tidy record after the answer lets the owner go, and the note is retracted.
    {
        let mut store = EventStore::open_existing(&f.db()).unwrap();
        model::store::record_order_evidence(&mut store, "tidy", serde_json::json!({"folders": ["x"], "moved": 0, "refused": 0})).unwrap();
    }
    assert!(!blocked(&f.stop(A)));
    assert!(!f.live());
}

/// The step follows the proposal. B was held first and owns the step; the
/// proposal is shown in A (the person answered there and the tidy ran): A takes
/// the step as soon as its proposal is seen, and the judgement settles it for
/// both conversations, so B is released too.
#[test]
fn the_step_follows_the_proposal_and_settles_for_every_conversation() {
    let f = Fx::new();
    f.write(B, &welcome_answered());
    assert!(blocked(&f.stop(B)), "B is held first and owns the step");
    assert_eq!(serve::order_earlier::owner_on_file(&f.db()).unwrap().session, B);

    let call = serde_json::json!({"type":"assistant","timestamp":"2026-03-02T10:03:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"\"C:/thor2/serve.exe\" --db x inventory"}}]}}).to_string();
    let mut lines = welcome_answered();
    lines.push(call);
    lines.push(assistant("2026-03-02T10:04:00Z", "Proposal:
1. CV and job hunting
Shall I do it? Say yes."));
    f.write(A, &lines);
    assert!(!blocked(&f.stop(A)), "a proposal waiting for the answer ends the turn");
    let owner = serve::order_earlier::owner_on_file(&f.db()).unwrap();
    assert_eq!((owner.session.as_str(), owner.rank), (A, serve::order_earlier::RANK_PROPOSED), "ownership moved to the conversation that showed the proposal");
    // B is no longer held: it gets the plain line.
    let line = f.stop(B);
    assert!(!blocked(&line) && say(&line).contains("Another conversation"), "{line}");

    // The person answers in A and the tidy runs: settled in A, and B is free for good.
    lines.push(person("2026-03-02T10:05:00Z", "ja"));
    f.write(A, &lines);
    {
        let mut store = EventStore::open_existing(&f.db()).unwrap();
        model::store::record_order_evidence(&mut store, "tidy", serde_json::json!({"folders": ["x"], "moved": 0, "refused": 0})).unwrap();
    }
    assert!(!blocked(&f.stop(A)));
    assert!(!f.live());
    assert!(!blocked(&f.stop(B)));
}

/// An owner whose last-seen time lies in the future (a clock set back) is taken
/// over once it is quiet, not later.
#[test]
fn an_owner_time_in_the_future_does_not_delay_the_take_over() {
    let f = Fx::new();
    f.write(A, &welcome_answered());
    f.write(B, &no_stake());
    assert!(blocked(&f.stop(A)));
    let mut owner = serve::order_earlier::owner_on_file(&f.db()).unwrap();
    owner.seen_unix = serve::time::now_unix() + 10 * 24 * 3600;
    std::fs::write(f.root.path().join(serve::order_earlier::OWNER_FILE), serde_json::to_string(&owner).unwrap()).unwrap();
    // Still alive by the clamp for now (seen counts as now): B is not held.
    assert!(!blocked(&f.stop(B)));
    // Quiet for good: clamped to now, then aged 31 minutes, it is taken over.
    let store = EventStore::open_existing(&f.db()).unwrap();
    assert!(serve::usefulness::session_idle_minutes(&store, A, serve::time::now_unix(), 30).is_none());
    drop(store);
    let (now, aged) = (serve::time::now_unix(), 31 * 60);
    let mut owner = serve::order_earlier::owner_on_file(&f.db()).unwrap();
    owner.seen_unix = now - aged;
    std::fs::write(f.root.path().join(serve::order_earlier::OWNER_FILE), serde_json::to_string(&owner).unwrap()).unwrap();
    assert!(blocked(&f.stop(B)));
}

fn stop_retry(f: &Fx, session: &str) -> serde_json::Value {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(f.db()).arg("hook").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.env("HOME", f.root.path()).env("USERPROFILE", f.root.path()).env_remove("CLAUDE_CODE_ENTRYPOINT");
    let mut child = cmd.spawn().unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "Stop", "session_id": session, "stop_hook_active": true, "last_assistant_message": "",
        "transcript_path": f.transcript(session).display().to_string(), "cwd": f.root.path().display().to_string(),
    });
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    if text.trim().is_empty() { serde_json::Value::Null } else { serde_json::from_str(&text).unwrap() }
}

/// Variant S2: a second conversation, with nothing to do with the setup, ends a turn
/// BEFORE the first conversation was ever held. It must never be held. The first,
/// once it has its stake, is held and settles. And if no conversation with a stake
/// has been active for 30 minutes, the next one that asks may own the step.
#[test]
fn a_conversation_without_a_stake_is_never_held_first_and_the_fallback_comes_after_thirty_minutes() {
    let f = Fx::new();
    f.write(B, &no_stake());
    // B ends turns first, twice: never held, and at most one plain line.
    let first = f.stop(B);
    assert!(!blocked(&first), "no stake, nobody owns the step yet: {first}");
    assert!(say(&first).contains("Another conversation"), "{first}");
    assert!(!blocked(&f.stop(B)));
    assert!(serve::order_earlier::owner_on_file(&f.db()).is_none(), "a conversation without a stake owns nothing");

    // A (the welcome ran there) ends its turn: it owns the step and is held, and B stays free.
    f.write(A, &welcome_answered());
    assert!(blocked(&f.stop(A)));
    assert_eq!(serve::order_earlier::owner_on_file(&f.db()).unwrap().session, A);
    assert!(!blocked(&f.stop(B)));

    // A shows the proposal, the person says yes and the tidy runs: settled for everybody.
    let call = serde_json::json!({"type":"assistant","timestamp":"2026-03-02T10:03:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"\"C:/thor2/serve.exe\" --db x inventory"}}]}}).to_string();
    let mut lines = welcome_answered();
    lines.push(call);
    lines.push(assistant("2026-03-02T10:04:00Z", "Proposal:
1. CV
Shall I do it? Say yes."));
    lines.push(person("2026-03-02T10:05:00Z", "ja"));
    f.write(A, &lines);
    {
        let mut store = EventStore::open_existing(&f.db()).unwrap();
        model::store::record_order_evidence(&mut store, "tidy", serde_json::json!({"folders": ["x"], "moved": 0, "refused": 0})).unwrap();
    }
    assert!(!blocked(&f.stop(A)));
    assert!(!f.live());
}

#[test]
fn the_fallback_lets_a_conversation_without_a_stake_own_the_step_after_thirty_quiet_minutes() {
    let f = Fx::new();
    f.write(B, &no_stake());
    assert!(!blocked(&f.stop(B)));
    // Nobody with a stake showed up for 31 minutes: the waiting mark is that old.
    let old = serve::time::now_unix() - 31 * 60;
    std::fs::write(f.root.path().join("order-waiting.json"), old.to_string()).unwrap();
    let held = f.stop(B);
    assert!(blocked(&held), "the step is never left undone for ever: {held}");
    assert_eq!(serve::order_earlier::owner_on_file(&f.db()).unwrap().rank, serve::order_earlier::RANK_NONE);
}

/// The proposal shown in the retry Stop of a held turn is recorded as the owner's stake.
#[test]
fn the_proposal_stake_is_recorded_in_a_retry_stop() {
    let f = Fx::new();
    f.write(A, &welcome_answered());
    assert!(blocked(&f.stop(A)));
    assert_eq!(serve::order_earlier::owner_on_file(&f.db()).unwrap().rank, serve::order_earlier::RANK_SETUP);
    // The held turn runs the inventory and shows the proposal; the retry Stop follows.
    let call = serde_json::json!({"type":"assistant","timestamp":"2026-03-02T10:03:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"\"C:/thor2/serve.exe\" --db x inventory"}}]}}).to_string();
    let mut lines = welcome_answered();
    lines.push(call);
    lines.push(assistant("2026-03-02T10:04:00Z", "Proposal:
1. CV
Shall I do it? Say yes."));
    f.write(A, &lines);
    let retry = stop_retry(&f, A);
    assert!(!blocked(&retry), "a retry Stop never holds the step again: {retry}");
    let owner = serve::order_earlier::owner_on_file(&f.db()).unwrap();
    assert_eq!((owner.session.as_str(), owner.rank), (A, serve::order_earlier::RANK_PROPOSED));
}
