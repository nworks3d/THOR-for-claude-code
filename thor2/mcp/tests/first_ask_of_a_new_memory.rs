//! The first evaluation of a new memory, end to end through the real `serve hook`.
//!
//! A new user used to meet the weekly check within the hour: a memory minutes old was held with
//! "no evaluation in the last 7 day(s)" although nothing was on any list. Now the first ask of a
//! project younger than a week waits until something can be judged, and says plainly that the
//! project has not had an evaluation yet. And a turn held AGAIN for the same open evaluation
//! gets a short text, not the whole ask again.

mod common;

use common::*;
use serde_json::{json, Value};

use model::item::{Binding, Item, Kind};
use thor_core::event_store::EventStore;

/// Give this session 90 minutes of accrued work in demo (and one earlier Stop).
fn worked(s: &Session, minutes: i64) {
    let mut all = serve::usefulness::read_eval_debt_state(&s.db);
    let now = serve::time::now_unix();
    let entry = all.entry("demo".to_string()).or_default();
    entry.sessions.insert(
        s.id.clone(),
        serve::usefulness::SessionWorkState {
            anchor_unix: Some(now - minutes * 60),
            accrued_secs: minutes * 60,
            last_event_unix: Some(now),
            stops: 1,
            ..Default::default()
        },
    );
    entry.work = serve::usefulness::SessionWorkState {
        anchor_unix: Some(now - minutes * 60),
        accrued_secs: minutes * 60,
        last_event_unix: Some(now),
        ..Default::default()
    };
    std::fs::write(serve::usefulness::eval_debt_state_path(&s.db), serde_json::to_string(&all).unwrap()).unwrap();
}

/// The project "demo" opened `days_ago` days ago: something was served at that time, and the
/// installer's record of the project follows it.
fn project_opened(s: &Session, days_ago: i64) {
    let at = serve::time::iso8601_from_unix(serve::time::now_unix() - days_ago * 86400);
    let mut store = EventStore::open_existing(&s.db).unwrap();
    serve::deliver::record_delivery_with_trigger(&mut store, "install-session", "install-session", "hook", &at, &["seed".to_string()], Some("start"));
    drop(store);
    register(&s.db, "demo", &s.cwd);
}

/// A pinned note that an evaluation would list for a re-read.
fn pinned_note(s: &Session) {
    let item = Item {
        id: "pinned-to-reread".to_string(),
        kind: Kind::Rule,
        text: "Always say which folder a command ran in, before and after it.".to_string(),
        bindings: vec![Binding::Always],
        severity: None,
        project: Some("demo".to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("a command ran with no folder named and nobody minded".to_string()),
        check: None,
    };
    // A pin reviewed in the last 30 days is not listed: the note is written, and served, long ago.
    let long_ago = serve::time::iso8601_from_unix(serve::time::now_unix() - 40 * 86400);
    {
        let mut store = EventStore::open_existing(&s.db).unwrap();
        serve::deliver::record_delivery(&mut store, "an-earlier-session", "an-earlier-session", "hook", &long_ago, &["seed".to_string()]);
    }
    seed_raw(&s.db, &item);
    let mut store = EventStore::open_existing(&s.db).unwrap();
    serve::teeth::fixture::decide(&mut store, &item.id);
    serve::deliver::record_delivery(&mut store, "an-earlier-session", "an-earlier-session", "hook", &long_ago, &[item.id.clone()]);
}

fn blocks(out: &str) -> bool {
    serde_json::from_str::<Value>(out).is_ok_and(|v| v["decision"] == "block")
}

#[test]
fn a_new_memory_with_nothing_to_judge_is_not_held_for_an_evaluation() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Session::new(tmp.path());
    project_opened(&s, 0);
    worked(&s, 90);
    let out = s.stop();
    assert!(!blocks(&out), "nothing to judge in a project minutes old: {out}");
    assert!(!out.contains("evaluation"), "and nothing is said about one: {out}");
}

#[test]
fn something_to_judge_ends_the_wait_and_the_ask_says_there_was_no_evaluation_yet() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Session::new(tmp.path());
    project_opened(&s, 0);
    pinned_note(&s);
    worked(&s, 90);
    let out = s.stop();
    assert!(blocks(&out), "a pinned note to re-read is something to judge: {out}");
    assert!(out.contains("This project has not had an evaluation yet"), "{out}");
    assert!(!out.contains("no evaluation in the last"), "{out}");
}

#[test]
fn a_project_a_week_old_is_asked_even_with_nothing_to_judge() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Session::new(tmp.path());
    project_opened(&s, 8);
    worked(&s, 90);
    let out = s.stop();
    // Asked as before: the evaluation starts, finds nothing on its lists and THOR files it itself.
    assert!(out.contains("The evaluation of 'demo' had nothing on any list"), "a week without an evaluation is asked, as before: {out}");
}

#[test]
fn a_turn_held_again_for_the_same_open_evaluation_gets_the_short_ask() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Session::new(tmp.path());
    project_opened(&s, 0);
    pinned_note(&s);
    worked(&s, 90);
    let first = s.stop();
    assert!(blocks(&first) && first.len() > 2500, "the first hold carries the whole ask ({} bytes)", first.len());

    worked(&s, 95);
    let second = s.stop();
    let parsed: Value = serde_json::from_str(&second).unwrap_or_else(|e| panic!("JSON: {e}: {second}"));
    assert_eq!(parsed["decision"], "block", "still held: {second}");
    let reason = parsed["reason"].as_str().unwrap_or_default();
    assert!(reason.len() < 1300, "short ({} bytes): {reason}", reason.len());
    assert!(reason.contains("asked 2 time(s)"), "{reason}");
    assert!(reason.contains("1 pinned note(s) not re-read, for example 'pinned-to-reread'"), "{reason}");
    assert!(reason.contains("call the Skill tool with skill \"thor-eval\""), "the exact call that prints it all: {reason}");
    assert!(parsed["systemMessage"].as_str().is_some_and(|m| !m.is_empty()), "the plain sentence for the person stays: {second}");

    // The call it names prints the whole start note again, and starts nothing new.
    let again = s.prompt("/thor-eval");
    assert!(again.contains("already open") && again.contains("PINNED NOTES TO RE-READ"), "{again}");

    // A third hold counts on.
    worked(&s, 100);
    let third = s.stop();
    assert!(third.contains("asked 3 time(s)") && third.len() < 1700, "{third}");
    let _ = json!(null);
}
