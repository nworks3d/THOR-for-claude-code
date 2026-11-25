//! `install --set-helpers on|off`: helper-agent mode (first-session item 6) is
//! ON in a new memory (its two notes are seeded) and is switched off or on by one
//! command. These tests hold the promises: a new memory has both notes, an old
//! memory without them gets exactly the two when switched `on`, `off` retracts them with a reason, and both are safe to repeat. A
//! retracted id cannot simply be stored again (a create is never an upsert, a
//! second one would leave the id with two heads), so `on` after `off` must still
//! work: that round trip is tested too.

use model::store::{self, ReadError};
use std::path::PathBuf;
use std::process::{Command, Output};

/// The two notes of helper-agent mode (bound to the Agent tool).
const MODE: [&str; 2] = ["orchestrator-mode-saves-quota", "an-agent-brief-stands-alone"];
/// Every note about helper assistants: the two above and three of the starting set.
const NOTES: [&str; 5] = [
    "orchestrator-mode-saves-quota",
    "an-agent-brief-stands-alone",
    "name-a-model-on-every-agent",
    "mechanical-brief-forbids-subagents-and-review",
    "mid-task-message-does-not-redirect-an-agent",
];

struct Fx {
    home: tempfile::TempDir,
}

impl Fx {
    fn new() -> Fx {
        let fx = Fx { home: tempfile::tempdir().unwrap() };
        std::fs::create_dir_all(fx.db().parent().unwrap()).unwrap();
        ops::install::ensure_store(&fx.db()).unwrap();
        let seeded = ops::install::seed_working_contract(&fx.db()).unwrap();
        assert!(seeded.iter().all(|s| s.stored), "refused: {:?}", seeded.iter().filter(|s| !s.stored).collect::<Vec<_>>());
        fx
    }
    /// An OLD memory: a store made before helper mode was seeded, with no
    /// starting notes at all.
    fn empty() -> Fx {
        let fx = Fx { home: tempfile::tempdir().unwrap() };
        std::fs::create_dir_all(fx.db().parent().unwrap()).unwrap();
        ops::install::ensure_store(&fx.db()).unwrap();
        fx
    }
    fn db(&self) -> PathBuf {
        self.home.path().join("localappdata").join("thor2").join("thor.db")
    }
    fn install(&self, args: &[&str]) -> Output {
        let cwd = tempfile::tempdir().unwrap();
        Command::new(env!("CARGO_BIN_EXE_install"))
            .current_dir(cwd.path())
            .args(args)
            .arg("--db")
            .arg(self.db())
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("LOCALAPPDATA", self.home.path().join("localappdata"))
            .env("GIT_CONFIG_GLOBAL", self.home.path().join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    }
    fn read(&self, id: &str) -> Result<model::item::Item, ReadError> {
        let events = thor_core::event_store::EventStore::open_read_only(&self.db()).unwrap();
        store::show(&events, id)
    }
    fn event_count(&self) -> usize {
        let events = thor_core::event_store::EventStore::open_read_only(&self.db()).unwrap();
        events.get_all_events().unwrap().len()
    }
    fn history(&self, id: &str) -> Vec<store::Revision> {
        let events = thor_core::event_store::EventStore::open_read_only(&self.db()).unwrap();
        store::history(&events, id).unwrap()
    }
}

fn said(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

#[test]
fn a_new_memory_has_both_helper_notes() {
    let fx = Fx::new();
    for id in NOTES {
        assert!(fx.read(id).is_ok(), "{id} must be seeded into a new memory");
    }
    assert!(fx.read(NOTES[1]).unwrap().bindings.contains(&model::item::Binding::Always), "the brief note is pinned from the start");
}

#[test]
fn an_old_memory_without_them_is_not_touched_by_seeding_or_by_off() {
    let fx = Fx::empty();
    for id in NOTES {
        assert!(matches!(fx.read(id), Err(ReadError::NotFound(_))), "{id} must not appear in an existing memory");
    }
}

#[test]
fn on_stores_exactly_the_five_notes_and_is_idempotent() {
    let fx = Fx::empty();
    let out = fx.install(&["--set-helpers", "on"]);
    assert!(out.status.success(), "{}", said(&out));
    for id in NOTES {
        fx.read(id).unwrap_or_else(|e| panic!("{id} not stored: {e}"));
    }
    for id in MODE {
        let item = fx.read(id).unwrap_or_else(|e| panic!("{id} not stored: {e}"));
        assert!(item.bindings.iter().any(|b| matches!(b, model::item::Binding::Target { value, .. } if value == "Agent")), "{id} lost its Agent binding");
    }
    // The same source text the code holds, not a copy that can drift.
    for want in ops::install::helper_mode_notes() {
        let got = fx.read(&want.id).unwrap();
        assert_eq!((got.text, got.falsifier, got.bindings), (want.text, want.falsifier, want.bindings), "{}", want.id);
    }
    let before = fx.event_count();
    let again = fx.install(&["--set-helpers", "on"]);
    assert!(again.status.success(), "{}", said(&again));
    assert!(said(&again).contains("already on"), "{}", said(&again));
    assert_eq!(fx.event_count(), before, "a second 'on' writes nothing");
}

#[test]
fn off_retracts_both_with_a_reason_and_is_idempotent() {
    let fx = Fx::new();
    assert!(fx.install(&["--set-helpers", "on"]).status.success());
    let out = fx.install(&["--set-helpers", "off"]);
    assert!(out.status.success(), "{}", said(&out));
    for id in NOTES {
        match fx.read(id) {
            Err(ReadError::Retracted(why)) => assert!(why.chars().count() >= 20, "a real reason is kept: {why}"),
            other => panic!("{id} should be retracted, got {other:?}"),
        }
    }
    let before = fx.event_count();
    let again = fx.install(&["--set-helpers", "off"]);
    assert!(again.status.success(), "{}", said(&again));
    assert!(said(&again).contains("already off"), "{}", said(&again));
    assert_eq!(fx.event_count(), before, "a second 'off' writes nothing");
}

#[test]
fn off_on_a_memory_that_never_had_them_changes_nothing() {
    let fx = Fx::empty();
    let before = fx.event_count();
    let out = fx.install(&["--set-helpers", "off"]);
    assert!(out.status.success(), "{}", said(&out));
    assert!(said(&out).contains("already off"), "{}", said(&out));
    assert_eq!(fx.event_count(), before);
}

/// The retract/re-store answer. A retracted id is tombstoned for good, so the
/// way back must still end with ONE live head under the same id.
#[test]
fn on_after_off_brings_the_same_two_notes_back_and_off_works_again() {
    let fx = Fx::new();
    for round in 0..2 {
        let on = fx.install(&["--set-helpers", "on"]);
        assert!(on.status.success(), "round {round}: {}", said(&on));
        for id in NOTES {
            assert!(fx.read(id).is_ok(), "round {round}: {id} is not live after 'on': {:?}", fx.read(id).err());
        }
        let off = fx.install(&["--set-helpers", "off"]);
        assert!(off.status.success(), "round {round}: {}", said(&off));
        for id in NOTES {
            assert!(matches!(fx.read(id), Err(ReadError::Retracted(_))), "round {round}: {id} still live after 'off'");
        }
    }
    // History keeps the whole story, nothing is deleted.
    assert!(fx.history(NOTES[0]).iter().any(|r| r.kind == "fact_retracted"));
}

#[test]
fn the_switch_refuses_a_word_it_does_not_know_and_a_missing_memory() {
    let fx = Fx::new();
    let bad = fx.install(&["--set-helpers", "maybe"]);
    assert_eq!(bad.status.code(), Some(2), "{}", said(&bad));
    assert!(said(&bad).contains("'on' or 'off'"), "{}", said(&bad));
    let missing = Command::new(env!("CARGO_BIN_EXE_install"))
        .args(["--set-helpers", "on", "--db"])
        .arg(fx.home.path().join("no-such.db"))
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(said(&missing).contains("REFUSED"), "{}", said(&missing));
}

/// The brief note has to reach the session BEFORE its first Agent call: a
/// binding to the Agent tool alone fires only at that call, when the brief is
/// already written. `--set-helpers on` therefore also pins it (Always).
#[test]
fn the_brief_note_is_pinned_so_it_arrives_before_the_first_agent_call() {
    let fx = Fx::new();
    let out = fx.install(&["--set-helpers", "on"]);
    assert!(out.status.success(), "{}", said(&out));
    let brief = fx.read("an-agent-brief-stands-alone").unwrap();
    assert!(brief.bindings.contains(&model::item::Binding::Always), "the brief note is not pinned: {:?}", brief.bindings);
    assert!(
        brief.bindings.iter().any(|b| matches!(b, model::item::Binding::Target { value, .. } if value == "Agent")),
        "the brief note lost its Agent binding"
    );
    let orchestrator = fx.read("orchestrator-mode-saves-quota").unwrap();
    assert!(!orchestrator.bindings.contains(&model::item::Binding::Always), "only the brief note is pinned");
}

/// `install --set-starter-rules off|on`: one switch for the work habits, the
/// real program run against a real new memory. The notes THOR needs and the
/// helper notes stay, a second run changes nothing, and a refused word names
/// the two it accepts.
#[test]
fn set_starter_rules_off_and_on_run_through_the_real_installer() {
    let fx = Fx::new();
    let habits = ops::install::starter_rule_notes();
    assert!(habits.iter().all(|n| fx.read(&n.id).is_ok()));

    let off = fx.install(&["--set-starter-rules", "off"]);
    assert!(off.status.success(), "{}", said(&off));
    assert!(said(&off).contains("starter work rules are off"), "{}", said(&off));
    assert!(habits.iter().all(|n| matches!(fx.read(&n.id), Err(ReadError::Retracted(_)))), "every work habit is retracted");
    for id in NOTES.iter().chain(["anchor-what-the-fact-is-about", "correct-instead-of-duplicating", "changelog-flow-read-the-version-from-git"].iter()) {
        assert!(fx.read(id).is_ok(), "{id} stays");
    }

    let events = fx.event_count();
    let again = fx.install(&["--set-starter-rules", "off"]);
    assert!(again.status.success(), "{}", said(&again));
    assert_eq!(fx.event_count(), events, "a second off writes nothing");

    let on = fx.install(&["--set-starter-rules", "on"]);
    assert!(on.status.success(), "{}", said(&on));
    assert!(habits.iter().all(|n| fx.read(&n.id).is_ok()), "every work habit is back");

    let bad = fx.install(&["--set-starter-rules", "maybe"]);
    assert_eq!(bad.status.code(), Some(2), "{}", said(&bad));
    assert!(said(&bad).contains("'on' or 'off'"), "{}", said(&bad));
}
