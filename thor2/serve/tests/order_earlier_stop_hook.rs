//! THE LAST SETUP STEP (`serve::order_earlier`, `order_debt` in `serve.rs`),
//! end to end through the real `serve hook` and `serve inventory` programs.
//!
//! After the first-session welcome is answered, THOR holds the end of every
//! turn until the person was offered to put their earlier work in order and it
//! was carried out or refused in their own words. Every transcript here is
//! invented (a person called Ann and her CV); every program runs with HOME and
//! USERPROFILE pointed at a throwaway folder.

use model::store::{ORDER_ANSWER_ID, ORDER_NOTE_ID, OWNER_SETUP_ANSWERS_ID, SETUP_NOTE_ID};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use thor_core::event_store::EventStore;

const SESSION: &str = "11111111-2222-3333-4444-555555555555";

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let f = Self { root: tempfile::tempdir().unwrap() };
        std::fs::create_dir_all(f.projects().join("enc")).unwrap();
        // The folder belongs to no project; the ask to move is not what these tests are about.
        std::fs::write(f.root.path().join("no-project-ask-mode.json"), "{\"mode\": \"off\"}").unwrap();
        f
    }
    fn db(&self) -> PathBuf {
        self.root.path().join("thor.db")
    }
    fn projects(&self) -> PathBuf {
        self.root.path().join(".claude").join("projects")
    }
    fn transcript(&self) -> PathBuf {
        self.projects().join("enc").join(format!("{SESSION}.jsonl"))
    }
    fn env(&self, cmd: &mut Command) {
        cmd.env("HOME", self.root.path()).env("USERPROFILE", self.root.path()).env_remove("CLAUDE_CODE_ENTRYPOINT");
    }

    /// A store with the setup note (live or retracted) and the tidy note.
    fn store(&self, setup_retracted: bool) -> EventStore {
        let mut store = EventStore::new(&self.db()).unwrap();
        let item = |id: &str, kind: model::item::Kind| model::item::Item {
            id: id.to_string(),
            kind,
            text: "fixture note".to_string(),
            bindings: vec![model::item::Binding::Always],
            severity: None,
            project: None,
            tags: vec!["working-contract".to_string()],
            expires: None,
            key: None,
            falsifier: Some("fixture falsifier".to_string()),
            check: None,
        };
        model::store::declare(&mut store, "install", "install", "install-fixture", &item(SETUP_NOTE_ID, model::item::Kind::Rule)).unwrap();
        model::store::declare(&mut store, "install", "install", "install-fixture", &item(ORDER_NOTE_ID, model::item::Kind::Orientation)).unwrap();
        if setup_retracted {
            let mut answers = item(OWNER_SETUP_ANSWERS_ID, model::item::Kind::Report);
            answers.bindings = vec![];
            model::store::declare(&mut store, "install", "install", "install-fixture", &answers).unwrap();
            model::store::retract(&mut store, "s", "l", "fixture", SETUP_NOTE_ID, "answered", None).unwrap();
        }
        store
    }

    fn write_transcript(&self, lines: &[String]) {
        std::fs::write(self.transcript(), lines.join("\n") + "\n").unwrap();
    }

    fn hook(&self, payload: &serde_json::Value) -> serde_json::Value {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
        cmd.arg("--db").arg(self.db()).arg("hook").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        self.env(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        if text.trim().is_empty() {
            return serde_json::Value::Null;
        }
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON: {e}: {text}"))
    }

    fn stop(&self) -> serde_json::Value {
        self.hook(&serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": SESSION,
            "stop_hook_active": false,
            "last_assistant_message": "",
            "transcript_path": self.transcript().display().to_string(),
            "cwd": self.root.path().display().to_string(),
        }))
    }

    fn live(&self, id: &str) -> bool {
        let store = EventStore::open_existing(&self.db()).unwrap();
        model::store::show(&store, id).is_ok()
    }
}

fn person(at: &str, text: &str) -> String {
    serde_json::json!({"type":"user","timestamp":at,"message":{"role":"user","content":text}}).to_string()
}

fn assistant(at: &str, text: &str) -> String {
    serde_json::json!({"type":"assistant","timestamp":at,"message":{"role":"assistant","content":[{"type":"text","text":text}]}}).to_string()
}

/// The inventory call and, right after it, the numbered proposal: what really
/// happens in a session, which is all THOR reads (no marker anywhere).
fn proposal_lines(at: &str) -> Vec<String> {
    let call = serde_json::json!({"type":"assistant","timestamp":at,"message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"\"C:/thor2/serve.exe\" --db \"C:/thor2/thor.db\" inventory"}}]}}).to_string();
    let text = assistant(at, "I also looked at what you did before. Proposal:\n1. CV and job hunting\n2. Garden plans\nShall I do it? Say yes.");
    vec![call, text]
}

fn setup_call(at: &str) -> String {
    serde_json::json!({"type":"assistant","timestamp":at,"message":{"role":"assistant","content":[{"type":"tool_use","name":"mcp__thor__remember","input":{"id":"owner-setup-answers","kind":"report","text":"Setup finished."}}]}}).to_string()
}

fn welcome_answered() -> Vec<String> {
    vec![
        person("2026-03-02T10:00:00Z", "Help me with my CV"),
        assistant("2026-03-02T10:01:00Z", "Welcome. Shall I set it up like this?"),
        person("2026-03-02T10:02:00Z", "yes"),
        setup_call("2026-03-02T10:02:30Z"),
    ]
}

fn reason_of(v: &serde_json::Value) -> String {
    v["reason"].as_str().unwrap_or_default().to_string()
}

fn declare_answer(store: &mut EventStore, text: &str) {
    let item = model::item::Item {
        id: ORDER_ANSWER_ID.to_string(),
        kind: model::item::Kind::Report,
        text: text.to_string(),
        bindings: vec![],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    model::store::declare(store, "mcp", "mcp", "fixture-assistant", &item).unwrap();
}

// ----------------------------------------------------------------- the hold

#[test]
fn the_hold_appears_after_the_setup_note_is_retracted_and_not_before() {
    let f = Fixture::new();
    drop(f.store(false));
    f.write_transcript(&welcome_answered());
    let before = f.stop();
    assert!(reason_of(&before).contains("walked through setup"), "the setup hold comes first: {before}");

    let f = Fixture::new();
    drop(f.store(true));
    f.write_transcript(&welcome_answered());
    let held = f.stop();
    assert_eq!(held["decision"], "block", "{held}");
    let reason = reason_of(&held);
    assert!(reason.contains("order-earlier-work") && reason.contains("serve inventory") && reason.contains(ORDER_ANSWER_ID), "{reason}");
    assert!(reason.contains("numbered list"), "it says how the proposal is shown: {reason}");
    let line = held["systemMessage"].as_str().unwrap();
    assert!(line.contains("one last setup step") && line.contains("earlier work in order"), "{line}");
    assert!(line.contains("nothing is needed from you"), "{line}");
}

#[test]
fn the_welcome_turn_itself_is_not_held() {
    let f = Fixture::new();
    drop(f.store(true));
    f.write_transcript(&welcome_answered()[..2]);
    assert!(f.stop().is_null(), "the person has not answered the welcome yet");
}

#[test]
fn a_proposal_with_no_answer_yet_is_not_held() {
    let f = Fixture::new();
    drop(f.store(true));
    let mut t = welcome_answered();
    t.extend(proposal_lines("2026-03-02T10:03:00Z"));
    f.write_transcript(&t);
    assert!(f.stop().is_null(), "the turn ends on the question, that is what it should do");
}

#[test]
fn a_subagent_is_never_held_and_neither_is_a_store_without_the_note() {
    let f = Fixture::new();
    drop(f.store(true));
    f.write_transcript(&welcome_answered());
    let v = f.hook(&serde_json::json!({
        "hook_event_name": "Stop", "session_id": SESSION, "stop_hook_active": false, "last_assistant_message": "",
        "agent_id": "a1dca2c0feb7f44fb", "agent_type": "general-purpose",
        "transcript_path": f.transcript().display().to_string(),
    }));
    assert!(v.is_null(), "{v}");

    let old = Fixture::new();
    drop(EventStore::new(&old.db()).unwrap());
    old.write_transcript(&welcome_answered());
    assert!(old.stop().is_null(), "an existing install never got the note, so nothing holds");
}

// ------------------------------------------------------------- the refusals

#[test]
fn the_note_cannot_be_retracted_rewritten_or_archived_before_it_is_settled() {
    let f = Fixture::new();
    let mut store = f.store(true);
    let err = model::store::retract(&mut store, "s", "l", "assistant", ORDER_NOTE_ID, "done with it", None).unwrap_err();
    assert!(err.to_string().contains("cannot be retracted, revised or unpinned yet"), "{err}");
    assert!(err.to_string().contains(ORDER_ANSWER_ID) && err.to_string().contains("serve inventory"), "{err}");

    let existing = model::store::show(&store, ORDER_NOTE_ID).unwrap();
    let mut unpinned = existing.clone();
    unpinned.bindings = vec![model::item::Binding::Moment(intent::Action::Commit)];
    assert!(model::store::revise(&mut store, "s", "l", "assistant", &existing, &unpinned).is_err());
    let mut reworded = existing.clone();
    reworded.text = "never mind".to_string();
    assert!(model::store::revise(&mut store, "s", "l", "assistant", &existing, &reworded).is_err());
    assert!(model::store::archive(&mut store, "s", "l", "assistant", ORDER_NOTE_ID, "no longer needed", None).is_err());
    assert!(model::store::show(&store, ORDER_NOTE_ID).is_ok(), "every refused write leaves the note live");

    // A claim is not evidence: the evidence id belongs to THOR's own actor.
    let forged = store.append_event("s", "l", "assistant", thor_core::event_store::EventKind::GateStoodAside, model::store::ORDER_EVIDENCE_ID, None, "{}");
    drop(forged);
    assert!(!model::store::order_settled(&store));
    let as_report = model::item::Item { id: model::store::ORDER_EVIDENCE_ID.to_string(), kind: model::item::Kind::Report, text: "done".into(), ..existing.clone() };
    assert!(model::store::declare(&mut store, "s", "l", "assistant", &as_report).is_err(), "the evidence id is reserved");

    // Once THOR's own evidence says settled, the assistant may retract it too.
    model::store::record_order_evidence(&mut store, "settled", serde_json::json!({"how": "fixture"})).unwrap();
    model::store::retract(&mut store, "s", "l", "assistant", ORDER_NOTE_ID, "settled", None).unwrap();
}

// ---------------------------------------------------------- what settles it

#[test]
fn nothing_to_order_found_by_serve_inventory_settles_it() {
    let f = Fixture::new();
    drop(f.store(true));
    f.write_transcript(&welcome_answered());
    // An empty transcripts folder: nothing to order. (The fixture transcript
    // is moved out of the way first, it is the session being held.)
    let kept = std::fs::read(f.transcript()).unwrap();
    std::fs::remove_file(f.transcript()).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(f.db()).arg("inventory");
    f.env(&mut cmd);
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("Nothing to order"));
    std::fs::write(f.transcript(), kept).unwrap();

    let v = f.stop();
    assert!(v.is_null(), "{v}");
    assert!(!f.live(ORDER_NOTE_ID), "THOR retracted the note itself");
    // A folder named by hand proves nothing: the same call with an empty folder settles nothing.
    let g = Fixture::new();
    drop(g.store(true));
    g.write_transcript(&welcome_answered());
    let empty = g.root.path().join("empty-projects");
    std::fs::create_dir_all(&empty).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(g.db()).arg("inventory").arg("--projects-dir").arg(&empty);
    g.env(&mut cmd);
    assert!(cmd.output().unwrap().status.success());
    assert_eq!(g.stop()["decision"], "block");
    assert!(g.live(ORDER_NOTE_ID));
}

#[test]
fn something_to_order_does_not_settle_it() {
    let f = Fixture::new();
    drop(f.store(true));
    let desktop = f.root.path().join("Desktop");
    std::fs::create_dir_all(&desktop).unwrap();
    let cwd = desktop.display().to_string();
    let mut lines = welcome_answered();
    for l in &mut lines {
        let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
        v["cwd"] = serde_json::Value::String(cwd.clone());
        *l = v.to_string();
    }
    f.write_transcript(&lines);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(f.db()).arg("inventory");
    f.env(&mut cmd);
    let out = cmd.output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("NOT in order: Desktop"), "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(f.stop()["decision"], "block");
    assert!(f.live(ORDER_NOTE_ID));
}

#[test]
fn a_tidy_run_after_the_persons_yes_settles_it_but_not_before_their_answer() {
    let f = Fixture::new();
    let mut store = f.store(true);
    // The tidy event is older than the answer (transcript times in 2099): it settles nothing.
    let mut t = vec![person("2099-03-02T10:00:00Z", "Help me with my CV"), assistant("2099-03-02T10:01:00Z", "Welcome"), person("2099-03-02T10:02:00Z", "yes")];
    t.extend(proposal_lines("2099-03-02T10:03:00Z"));
    t.push(person("2099-03-02T10:04:00Z", "yes please, all of it"));
    f.write_transcript(&t);
    model::store::record_order_evidence(&mut store, "tidy", serde_json::json!({"folders": ["x"], "moved": 1})).unwrap();
    assert_eq!(f.stop()["decision"], "block", "a tidy run before the answer proves nothing");

    // Answered in the past: the same event now follows the answer.
    let mut t2 = vec![person("2026-03-02T10:00:00Z", "Help me with my CV"), assistant("2026-03-02T10:01:00Z", "Welcome"), person("2026-03-02T10:02:00Z", "yes")];
    t2.extend(proposal_lines("2026-03-02T10:03:00Z"));
    t2.push(person("2026-03-02T10:04:00Z", "yes please, all of it"));
    f.write_transcript(&t2);
    assert!(f.stop().is_null());
    assert!(!f.live(ORDER_NOTE_ID));
    // And once settled the hold is gone for good.
    assert!(f.stop().is_null());
}

#[test]
fn a_refusal_quoted_from_the_persons_own_message_settles_it() {
    let f = Fixture::new();
    let mut store = f.store(true);
    let mut t = welcome_answered();
    t.extend(proposal_lines("2026-03-02T10:03:00Z"));
    t.push(person("2026-03-02T10:04:00Z", "nee, laat maar zoals het is"));
    f.write_transcript(&t);
    declare_answer(&mut store, "She declined: \"nee, laat maar zoals het is\"");
    drop(store);
    assert!(f.stop().is_null());
    assert!(!f.live(ORDER_NOTE_ID));
}

#[test]
fn a_quote_that_is_not_in_a_real_message_of_the_person_is_refused() {
    let f = Fixture::new();
    let mut store = f.store(true);
    let mut t = welcome_answered();
    t.extend(proposal_lines("2026-03-02T10:03:00Z"));
    // The words only appear in a tool result and in a hook's meta line.
    t.push(r#"{"type":"user","timestamp":"2026-03-02T10:04:00Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"no thanks, never"}]}}"#.to_string());
    t.push(r#"{"type":"user","isMeta":true,"timestamp":"2026-03-02T10:05:00Z","message":{"role":"user","content":"no thanks, never"}}"#.to_string());
    t.push(person("2026-03-02T10:06:00Z", "hmm, what does that mean"));
    f.write_transcript(&t);
    declare_answer(&mut store, "She said \"no thanks, never\"");
    drop(store);
    let v = f.stop();
    assert_eq!(v["decision"], "block", "{v}");
    assert!(reason_of(&v).contains("none of the quoted words is a whole message the person typed"), "{}", reason_of(&v));
    assert!(f.live(ORDER_NOTE_ID));
}

/// No word list on the refusal route: a refusal in any language, however short
/// ("not now", "laat maar"), is settled by a Report that quotes the person's own
/// message exactly. Words that are not in any message of the person still settle nothing.
#[test]
fn a_quote_of_any_message_the_person_typed_settles_the_refusal_in_any_language() {
    for said in ["not now", "laat maar", "skip it", "later", "niks doen"] {
        let f = Fixture::new();
        let mut store = f.store(true);
        let mut t = welcome_answered();
        t.extend(proposal_lines("2026-03-02T10:03:00Z"));
        t.push(person("2026-03-02T10:04:00Z", said));
        f.write_transcript(&t);
        // Before the Report: held, and the hold names the refusal route in any words.
        let held = f.stop();
        assert_eq!(held["decision"], "block", "{said}");
        assert!(reason_of(&held).contains("in any words or any language"), "{}", reason_of(&held));
        declare_answer(&mut store, &format!("She said \"{said}\""));
        drop(store);
        assert!(f.stop().is_null(), "{said}");
        assert!(!f.live(ORDER_NOTE_ID), "{said}");
    }
    // Words that are in no message of the person settle nothing.
    let f = Fixture::new();
    let mut store = f.store(true);
    let mut t = welcome_answered();
    t.extend(proposal_lines("2026-03-02T10:03:00Z"));
    t.push(person("2026-03-02T10:04:00Z", "not now"));
    f.write_transcript(&t);
    declare_answer(&mut store, "She said \"never ever\"");
    drop(store);
    assert_eq!(f.stop()["decision"], "block");
}

#[test]
fn a_quote_from_before_the_proposal_is_refused() {
    let f = Fixture::new();
    let mut store = f.store(true);
    let mut t = vec![
        person("2026-03-02T09:00:00Z", "no, I do not want that"),
        assistant("2026-03-02T09:01:00Z", "ok"),
        person("2026-03-02T10:00:00Z", "yes"),
    ];
    t.extend(proposal_lines("2026-03-02T10:03:00Z"));
    t.push(person("2026-03-02T10:04:00Z", "sure"));
    f.write_transcript(&t);
    declare_answer(&mut store, "She said \"no, I do not want that\"");
    drop(store);
    assert_eq!(f.stop()["decision"], "block");
    assert!(f.live(ORDER_NOTE_ID));
}

#[test]
fn a_partial_refusal_needs_the_tidy_run_as_well() {
    let f = Fixture::new();
    let mut store = f.store(true);
    let mut t = welcome_answered();
    t.extend(proposal_lines("2026-03-02T10:03:00Z"));
    t.push(person("2026-03-02T10:04:00Z", "yes, but 2 no, because it is private"));
    f.write_transcript(&t);
    declare_answer(&mut store, "She said \"yes, but 2 no, because it is private\"");
    let v = f.stop();
    assert_eq!(v["decision"], "block");
    assert!(reason_of(&v).contains("install --tidy"), "{}", reason_of(&v));
    model::store::record_order_evidence(&mut store, "tidy", serde_json::json!({"folders": ["x"], "moved": 2})).unwrap();
    drop(store);
    assert!(f.stop().is_null());
    assert!(!f.live(ORDER_NOTE_ID));
}

#[allow(dead_code)]
fn unused(_: &Path) {}

/// A piece of a message is not the message: quoting "Wait" from "Wait, I first want
/// to think about it" settles nothing; the whole message does.
#[test]
fn a_piece_of_a_message_does_not_settle_the_step_but_the_whole_message_does() {
    for (quote, settles) in [("Wait", false), ("Wait, I first want to think about it", true)] {
        let f = Fixture::new();
        let mut store = f.store(true);
        let mut t = welcome_answered();
        t.extend(proposal_lines("2026-03-02T10:03:00Z"));
        t.push(person("2026-03-02T10:04:00Z", "Wait, I first want to think about it"));
        f.write_transcript(&t);
        declare_answer(&mut store, &format!("She said \"{quote}\""));
        drop(store);
        assert_eq!(f.stop().is_null(), settles, "{quote}");
        assert_eq!(f.live(ORDER_NOTE_ID), !settles, "{quote}");
    }
}

/// A message that holds quote characters, or is one letter, can be quoted whole.
#[test]
fn a_message_with_quote_characters_or_one_letter_settles_when_quoted_whole() {
    for said in ["nee, laat \"Reizen\" maar staan", "n"] {
        let f = Fixture::new();
        let mut store = f.store(true);
        let mut t = welcome_answered();
        t.extend(proposal_lines("2026-03-02T10:03:00Z"));
        t.push(person("2026-03-02T10:04:00Z", said));
        f.write_transcript(&t);
        declare_answer(&mut store, &format!("She said \"{said}\""));
        drop(store);
        assert!(f.stop().is_null(), "{said}");
        assert!(!f.live(ORDER_NOTE_ID), "{said}");
    }
}
