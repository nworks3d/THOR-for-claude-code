//! THE STANDING SWEEP (`serve::sweep`): every rule that can refuse a call, and
//! every entry of the answer rulebook, tried against the real calls and
//! replies of the last 30 days, through the functions the hook uses.
//!
//! Each case writes a small record of real calls (`serve::corpus`) beside a
//! store holding a few rules, runs the sweep and reads what it reports. The
//! names are neutral: `acme-tool` is no real program.

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use serde_json::json;
use serve::corpus::{self, CallRow, ReplyRow};
use serve::sweep::{self, Opts};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thor_core::event_store::{EventKind, EventStore};

/// When the rules were written (the served event just before them dates it).
const BASE: i64 = 1_790_000_000;
/// "Now" of the sweep: ten days after.
const NOW: i64 = BASE + 10 * 86_400;
/// A call made well after the rules stood.
const LATER: i64 = BASE + 2 * 86_400;

fn rule(id: &str, anchor: &str, check: Check, tags: &[&str]) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("probe rule {id}: the {anchor} call needs its own answer, nothing else will do"),
        bindings: vec![Binding::Target { kind: TargetKind::Command, value: anchor.to_string() }],
        severity: Some(Severity::Costly),
        project: None,
        tags: tags.iter().map(|t| t.to_string()).collect(),
        expires: None,
        key: None,
        falsifier: Some("the guard turns out to refuse nothing of this kind".to_string()),
        check: Some(check),
    }
}

fn wipe_ban() -> Item {
    rule("acme-wipe-ban", "acme-tool", Check::Forbidden { literals: vec!["--wipe".to_string()] }, &[])
}

fn deploy_needs_dry_run() -> Item {
    rule(
        "acme-deploy-needs-dry-run",
        "acme-deploy",
        Check::Requires { when: "acme-deploy".to_string(), required: vec!["--dry-run".to_string()] },
        &[],
    )
}

fn publish_needs_the_owner() -> Item {
    rule(
        "acme-publish-needs-the-owner",
        "acme-publish",
        Check::Requires { when: "acme-publish".to_string(), required: vec!["go-owner".to_string()] },
        &[model::store::OWNER_APPROVAL_TAG],
    )
}

/// `item` with every check literal its text does not name appended to the
/// text, as the write gate requires.
fn named(item: &Item) -> Item {
    let mut out = item.clone();
    let unnamed = model::gate::unnamed_literals(&out);
    if !unnamed.is_empty() {
        let shown: Vec<String> = unnamed.iter().map(|l| format!("'{}'", model::gate::shown_literal(l))).collect();
        out.text = format!("{} (names {})", out.text, shown.join(" "));
    }
    out
}

struct Fx {
    dir: tempfile::TempDir,
}

impl Fx {
    /// A store whose rules were written at `BASE`: a serving dated `BASE`
    /// comes first, so the log knows when the rules stood.
    fn new(rules: &[Item]) -> Fx {
        let fx = Fx { dir: tempfile::tempdir().unwrap() };
        let mut store = EventStore::new(&fx.db()).unwrap();
        let served = serde_json::to_string(&model::served::ItemServed {
            served_at: serve::time::iso8601_from_unix(BASE),
            trigger: None,
        })
        .unwrap();
        store.append_event("hook", "l", "hook", EventKind::ItemServed, "some-rule", None, &served).unwrap();
        for item in rules {
            model::store::declare(&mut store, "s", "l", "a", &named(item)).unwrap_or_else(|e| panic!("{}: {e}", item.id));
        }
        fx
    }

    fn db(&self) -> PathBuf {
        self.dir.path().join("thor.db")
    }

    fn store(&self) -> EventStore {
        EventStore::open_existing(&self.db()).unwrap()
    }

    fn cwd(&self) -> String {
        self.dir.path().to_string_lossy().to_string()
    }

    /// A Bash call in the fixture folder.
    fn bash(&self, id: &str, command: &str, t: i64) -> CallRow {
        CallRow {
            k: "c".into(),
            id: id.into(),
            t,
            s: "sess0001".into(),
            a: false,
            tool: "Bash".into(),
            cwd: self.cwd(),
            p: None,
            d: None,
            input: json!({"command": command}),
            r: vec![],
            oth: false,
            ok: Some(true),
            h: true,
            cut: false,
            b: serve::BUILD_ID.to_string(),
        }
    }

    fn write_calls(&self, rows: &[CallRow]) {
        let text: String = rows.iter().map(|r| format!("{}\n", serde_json::to_string(r).unwrap())).collect();
        std::fs::write(corpus::calls_path(&self.db()), text).unwrap();
    }

    fn append_calls(&self, rows: &[CallRow]) {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new().append(true).create(true).open(corpus::calls_path(&self.db())).unwrap();
        for r in rows {
            writeln!(file, "{}", serde_json::to_string(r).unwrap()).unwrap();
        }
    }

    fn sweep(&self) -> sweep::Report {
        sweep::run(&self.db(), &self.store(), &Opts { budget: Duration::from_secs(60), full: false, now: NOW })
    }
}

fn refused_by(mut row: CallRow, rule: &str) -> CallRow {
    row.r = vec![rule.to_string()];
    row.ok = Some(false);
    row
}

fn line<'a>(report: &'a sweep::Report, id: &str) -> &'a sweep::RuleLine {
    report.calls.iter().find(|l| l.id == id).unwrap_or_else(|| panic!("no line for {id}: {:?}", report.calls))
}

#[test]
fn a_rule_that_would_refuse_a_call_that_ran_fine_is_named_with_its_example() {
    let fx = Fx::new(&[wipe_ban()]);
    fx.write_calls(&[
        fx.bash("toolu_1", "acme-tool --wipe cache", LATER),
        fx.bash("toolu_2", "acme-tool list", LATER),
        fx.bash("toolu_3", "ls -la", LATER),
    ]);
    let report = fx.sweep();
    let l = line(&report, "acme-wipe-ban");
    assert_eq!((l.refuses, l.of, l.ran_fine), (1, 3, 1), "{l:?}");
    assert!(l.flagged());
    assert!(l.examples[0].contains("acme-tool --wipe cache") && l.examples[0].contains("ran fine"), "{:?}", l.examples);
    assert_eq!(
        sweep::doctor_lines(&report),
        vec!["false-block sweep: 1 rule(s) would refuse real calls that ran fine: acme-wipe-ban (run serve sweep for detail)".to_string()]
    );
}

#[test]
fn a_rule_that_only_ever_refused_what_it_should_is_silent() {
    let fx = Fx::new(&[wipe_ban()]);
    // The hook refused it at the time and nothing like it ran afterwards.
    fx.write_calls(&[refused_by(fx.bash("toolu_1", "acme-tool --wipe cache", LATER), "acme-wipe-ban"), fx.bash("toolu_2", "ls", LATER)]);
    let report = fx.sweep();
    let l = line(&report, "acme-wipe-ban");
    assert_eq!((l.refuses, l.ran_fine, l.refused_then), (1, 0, 1));
    assert!(!l.flagged());
    assert!(sweep::doctor_lines(&report).is_empty(), "silent at zero: {:?}", sweep::doctor_lines(&report));
}

#[test]
fn a_call_the_hook_refused_and_that_later_ran_unchanged_counts_as_ran_fine() {
    let fx = Fx::new(&[wipe_ban()]);
    // Both inside the hour after the rule was written, when a call is no
    // evidence of its own (see the test below): only the refusal followed by
    // the very same call running can make this one count.
    let first = refused_by(fx.bash("toolu_1", "acme-tool --wipe cache", BASE + 30), "acme-wipe-ban");
    let again = fx.bash("toolu_2", "acme-tool --wipe cache", BASE + 90);
    fx.write_calls(&[first, again]);
    let l = fx.sweep().calls.into_iter().find(|l| l.id == "acme-wipe-ban").unwrap();
    assert_eq!((l.refuses, l.ran_fine, l.refused_then), (1, 1, 1), "one distinct call, refused then run unchanged: {l:?}");
}

#[test]
fn a_call_that_ran_before_the_rule_stood_is_no_evidence() {
    let fx = Fx::new(&[wipe_ban()]);
    // Made a minute after the rule was written: the grace period has not passed.
    fx.write_calls(&[fx.bash("toolu_1", "acme-tool --wipe cache", BASE + 60), fx.bash("toolu_2", "acme-tool --wipe old", BASE - 5 * 86_400)]);
    let l = fx.sweep().calls.into_iter().find(|l| l.id == "acme-wipe-ban").unwrap();
    assert_eq!((l.refuses, l.ran_fine), (2, 0), "the rule would refuse both, but neither is proof it was wrong: {l:?}");
}

#[test]
fn a_session_in_which_no_hook_was_alive_is_no_evidence() {
    let fx = Fx::new(&[wipe_ban()]);
    let mut quiet = fx.bash("toolu_1", "acme-tool --wipe cache", LATER);
    quiet.h = false; // no hook spoke in this session, so nothing could have refused it
    fx.write_calls(&[quiet]);
    let l = fx.sweep().calls.into_iter().find(|l| l.id == "acme-wipe-ban").unwrap();
    assert_eq!((l.refuses, l.ran_fine), (1, 0), "{l:?}");
    // One call of the same session that THOR did refuse shows the hook was there.
    let mut alive = refused_by(fx.bash("toolu_0", "acme-tool --wipe other", LATER - 10), "acme-wipe-ban");
    alive.h = false;
    let mut quiet = fx.bash("toolu_1", "acme-tool --wipe cache", LATER);
    quiet.h = false;
    fx.write_calls(&[alive, quiet]);
    let report = fx.sweep_full();
    let l = line(&report, "acme-wipe-ban");
    assert_eq!(l.ran_fine, 1, "a session whose hook refused something had a live hook: {l:?}");
}

#[test]
fn a_rule_that_waits_for_the_owners_go_is_never_flagged() {
    let fx = Fx::new(&[publish_needs_the_owner()]);
    fx.write_calls(&[fx.bash("toolu_1", "acme-publish release", LATER)]);
    let report = fx.sweep();
    let l = line(&report, "acme-publish-needs-the-owner");
    assert_eq!((l.refuses, l.ran_fine), (1, 0), "it ran because the owner said so; that is the rule working: {l:?}");
    assert!(l.owner_approval && !l.flagged());
    assert!(sweep::doctor_lines(&report).is_empty());
}

#[test]
fn a_requires_rule_is_swept_like_any_other_and_a_call_that_meets_it_is_not_a_hit() {
    let fx = Fx::new(&[deploy_needs_dry_run()]);
    fx.write_calls(&[fx.bash("toolu_1", "acme-deploy staging", LATER), fx.bash("toolu_2", "acme-deploy staging --dry-run", LATER)]);
    let l = fx.sweep().calls.into_iter().find(|l| l.id == "acme-deploy-needs-dry-run").unwrap();
    assert_eq!((l.refuses, l.of, l.ran_fine), (1, 2, 1));
}

#[test]
fn a_second_run_replays_nothing_that_was_done_and_a_third_only_what_is_new() {
    let fx = Fx::new(&[wipe_ban()]);
    fx.write_calls(&[fx.bash("toolu_1", "acme-tool --wipe a", LATER), fx.bash("toolu_2", "ls", LATER)]);
    let first = fx.sweep();
    assert_eq!(first.covered.calls_replayed, 2);
    let second = fx.sweep();
    assert_eq!(second.covered.calls_replayed, 0, "nothing new since the last run");
    assert_eq!(line(&second, "acme-wipe-ban").refuses, 1, "and what it found is kept");
    fx.append_calls(&[fx.bash("toolu_3", "acme-tool --wipe b", LATER + 10), fx.bash("toolu_4", "pwd", LATER + 11)]);
    let third = fx.sweep();
    assert_eq!(third.covered.calls_replayed, 2, "only the two new rows");
    let l = line(&third, "acme-wipe-ban");
    assert_eq!((l.refuses, l.of), (2, 4));
}

#[test]
fn the_same_call_made_again_is_one_call_made_twice() {
    let fx = Fx::new(&[wipe_ban()]);
    fx.write_calls(&[fx.bash("toolu_1", "acme-tool --wipe a", LATER)]);
    fx.sweep();
    fx.append_calls(&[fx.bash("toolu_2", "acme-tool --wipe a", LATER + 100)]);
    let l = fx.sweep().calls.into_iter().find(|l| l.id == "acme-wipe-ban").unwrap();
    assert_eq!((l.refuses, l.of), (1, 2), "distinct calls, rows counted: {l:?}");
}

#[test]
fn a_rule_that_changed_is_replayed_over_the_old_rows_and_one_that_was_retracted_is_forgotten() {
    let fx = Fx::new(&[wipe_ban(), deploy_needs_dry_run()]);
    fx.write_calls(&[
        fx.bash("toolu_1", "acme-tool --wipe a", LATER),
        fx.bash("toolu_2", "acme-tool --erase a", LATER),
        fx.bash("toolu_3", "acme-deploy staging", LATER),
    ]);
    let first = fx.sweep();
    assert_eq!(line(&first, "acme-wipe-ban").refuses, 1);
    assert_eq!(line(&first, "acme-deploy-needs-dry-run").refuses, 1);

    // The ban is widened to another literal: the call that names it is now a hit.
    let mut store = fx.store();
    let existing = named(&wipe_ban());
    let mut wider = existing.clone();
    wider.check = Some(Check::Forbidden { literals: vec!["--wipe".to_string(), "--erase".to_string()] });
    wider.text = format!("{} Also --erase is forbidden.", existing.text);
    model::store::revise(&mut store, "s", "l", "a", &existing, &named(&wider)).unwrap();
    // The other rule is retracted.
    store
        .append_mutate_checked("s", "l", "a", EventKind::FactRetracted, "acme-deploy-needs-dry-run", None, "tombstone")
        .unwrap();
    drop(store);

    let second = fx.sweep();
    assert_eq!(second.covered.calls_replayed, 0, "no new row: only the changed rule is looked at again");
    assert_eq!(line(&second, "acme-wipe-ban").refuses, 2, "the widened rule reaches the old row");
    assert!(second.calls.iter().all(|l| l.id != "acme-deploy-needs-dry-run"), "a retracted rule is gone from the report");
}

#[test]
fn a_run_that_runs_out_of_time_says_so_and_the_next_one_carries_on() {
    let fx = Fx::new(&[wipe_ban()]);
    fx.write_calls(&[fx.bash("toolu_1", "acme-tool --wipe a", LATER), fx.bash("toolu_2", "ls", LATER)]);
    let none = sweep::run(&fx.db(), &fx.store(), &Opts { budget: Duration::ZERO, full: false, now: NOW });
    assert!(!none.covered.complete, "no time at all: not finished");
    assert_eq!(none.covered.calls_replayed, 0);
    let done = fx.sweep();
    assert!(done.covered.complete);
    assert_eq!(line(&done, "acme-wipe-ban").refuses, 1, "and the next run does all of it");
}

#[test]
fn a_full_run_forgets_and_tries_everything_again() {
    let fx = Fx::new(&[wipe_ban()]);
    fx.write_calls(&[fx.bash("toolu_1", "acme-tool --wipe a", LATER)]);
    fx.sweep();
    let again = sweep::run(&fx.db(), &fx.store(), &Opts { budget: Duration::from_secs(60), full: true, now: NOW });
    assert_eq!(again.covered.calls_replayed, 1);
}

#[test]
fn a_new_build_starts_over_because_the_engine_may_decide_differently() {
    let fx = Fx::new(&[wipe_ban()]);
    fx.write_calls(&[fx.bash("toolu_1", "acme-tool --wipe a", LATER)]);
    fx.sweep();
    let mut state = sweep::load_state(&fx.db());
    state.engine = "another-build".to_string();
    std::fs::write(sweep::state_path(&fx.db()), serde_json::to_string(&state).unwrap()).unwrap();
    assert_eq!(fx.sweep().covered.calls_replayed, 1);
}

#[test]
fn the_replay_decides_exactly_as_the_hook_does_for_the_same_store() {
    let fx = Fx::new(&[wipe_ban(), deploy_needs_dry_run()]);
    let store = fx.store();
    let cwd = PathBuf::from(fx.cwd());
    let calls: Vec<(&str, serde_json::Value)> = vec![
        ("Bash", json!({"command": "acme-tool --wipe a"})),
        ("Bash", json!({"command": "acme-tool list && acme-deploy staging"})),
        ("Bash", json!({"command": "acme-deploy staging --dry-run"})),
        ("Bash", json!({"command": "ls"})),
        ("Write", json!({"file_path": "notes.txt", "content": "acme-tool --wipe"})),
        ("Agent", json!({"prompt": "run acme-deploy"})),
    ];
    let live = serve::guard_verdict::Live::for_replay(&store);
    for (tool, input) in calls {
        let hook = serve::guard_verdict::pre_tool(&store, tool, Some(&input), None, Some(&cwd), Some(&cwd), None).verdict.map(|v| v.reason);
        for _ in 0..2 {
            let replay = serve::guard_verdict::pre_tool_in(&live, tool, Some(&input), None, Some(&cwd), Some(&cwd), None).verdict.map(|v| v.reason);
            assert_eq!(hook, replay, "{tool} {input}");
        }
    }
}

#[test]
fn the_write_gate_says_how_many_real_calls_a_rule_would_refuse_with_up_to_three() {
    let fx = Fx::new(&[]);
    let now = serve::time::now_unix();
    let day = 86_400;
    let mut rows = vec![
        fx.bash("toolu_1", "acme-tool --wipe cache", now - 3 * day),
        refused_by(fx.bash("toolu_2", "acme-tool --wipe all", now - 2 * day), "some-other-rule"),
        fx.bash("toolu_3", "acme-tool --wipe cache", now - day),
        fx.bash("toolu_4", "acme-tool --wipe four", now - day + 5),
        fx.bash("toolu_5", "acme-tool --wipe five", now - day + 6),
        fx.bash("toolu_6", "acme-tool list", now - day),
        fx.bash("toolu_7", "ls", now - day),
        fx.bash("toolu_8", "ancient --wipe", now - 40 * day),
    ];
    rows[7].t = now - 40 * day;
    fx.write_calls(&rows);
    let note = sweep::would_refuse_note(&fx.store(), &named(&wipe_ban()), Duration::from_secs(10)).expect("a rule that can refuse gets the note");
    assert!(
        note.starts_with("real calls: this rule would refuse 4 of 7 real calls in the last 30 days (3 of them ran fine when they were made)"),
        "{note}"
    );
    assert_eq!(note.matches("\n    e.g. ").count(), 3, "at most three examples: {note}");
    assert!(note.contains("acme-tool --wipe cache") && note.contains("made 2 times"), "{note}");
    // A fact that cannot refuse gets no note, and a rule is never refused for it.
    let mut note_only = wipe_ban();
    note_only.check = None;
    assert!(sweep::would_refuse_note(&fx.store(), &note_only, Duration::from_secs(10)).is_none());
}

#[test]
fn the_write_gate_note_is_silent_when_there_is_no_record_of_calls() {
    let fx = Fx::new(&[]);
    assert!(sweep::would_refuse_note(&fx.store(), &named(&wipe_ban()), Duration::from_secs(10)).is_none());
}

// ---------------------------------------------------------------- the answers

fn write_rulebook(dir: &Path) {
    std::fs::write(
        dir.join("guard-response-rulebook.json"),
        r#"[{"id":"ask-user-to-check","any_of":["can you check"],"none_of":["any_of"],"reminder":"check it yourself first"}]"#,
    )
    .unwrap();
}

fn reply(t: i64, text: &str, held: bool) -> ReplyRow {
    corpus::reply_row(t, "0a0a0a0a-1111-4222-8333-444444444444", false, false, "", text, "a question", &[], held, false, if held { "[THOR] check it yourself first" } else { "" })
}

fn write_replies(db: &Path, rows: &[ReplyRow]) {
    let text: String = rows.iter().map(|r| format!("{}\n", serde_json::to_string(r).unwrap())).collect();
    std::fs::write(corpus::replies_path(db), text).unwrap();
}

#[test]
fn an_answer_rulebook_entry_that_fires_on_a_reply_that_went_through_is_named() {
    let fx = Fx::new(&[]);
    write_rulebook(fx.dir.path());
    let real_now = serve::time::now_unix();
    let sweep_now = real_now + 200_000;
    write_replies(
        &fx.db(),
        &[
            reply(real_now + 50_000, "can you check whether the file exists?", false),
            reply(real_now + 60_000, "can you check the other one too?", true),
            reply(real_now + 70_000, "all done, nothing left", false),
        ],
    );
    let report = sweep::run(&fx.db(), &fx.store(), &Opts { budget: Duration::from_secs(60), full: false, now: sweep_now });
    let l = report.answers.iter().find(|l| l.id == "ask-user-to-check").expect("the entry is swept");
    assert_eq!((l.refuses, l.of, l.ran_fine, l.refused_then), (2, 3, 1, 1), "{l:?}");
    assert!(l.examples[0].contains("can you check whether the file exists?") && l.examples[0].contains("went through"), "{:?}", l.examples);
    assert_eq!(
        sweep::doctor_lines(&report),
        vec!["answer-guard sweep: 1 entr(y/ies) would fire on real replies that went through: ask-user-to-check (run serve sweep for detail)".to_string()]
    );
}

#[test]
fn a_reply_older_than_the_rulebook_entry_is_no_evidence_against_it() {
    let fx = Fx::new(&[]);
    write_rulebook(fx.dir.path());
    let real_now = serve::time::now_unix();
    // Made before the entry was last written (the file is as old as this test).
    write_replies(&fx.db(), &[reply(real_now - 86_400, "can you check whether the file exists?", false)]);
    let report = sweep::run(&fx.db(), &fx.store(), &Opts { budget: Duration::from_secs(60), full: false, now: real_now + 1 });
    let l = report.answers.iter().find(|l| l.id == "ask-user-to-check").unwrap();
    assert_eq!((l.refuses, l.ran_fine), (1, 0), "{l:?}");
    assert!(sweep::doctor_lines(&report).is_empty());
}

#[test]
fn a_changed_rulebook_entry_is_tried_again_over_the_replies_already_seen() {
    let fx = Fx::new(&[]);
    write_rulebook(fx.dir.path());
    let real_now = serve::time::now_unix();
    write_replies(&fx.db(), &[reply(real_now + 50_000, "can you check whether the file exists?", false), reply(real_now + 60_000, "please verify it", false)]);
    let opts = Opts { budget: Duration::from_secs(60), full: false, now: real_now + 200_000 };
    let first = sweep::run(&fx.db(), &fx.store(), &opts);
    assert_eq!(first.answers[0].refuses, 1);
    let second = sweep::run(&fx.db(), &fx.store(), &opts);
    assert_eq!(second.covered.replies_replayed, 0, "nothing new, nothing changed");
    std::fs::write(
        fx.dir.path().join("guard-response-rulebook.json"),
        r#"[{"id":"ask-user-to-check","any_of":["can you check","please verify"],"none_of":["any_of"],"reminder":"check it yourself first"}]"#,
    )
    .unwrap();
    let third = sweep::run(&fx.db(), &fx.store(), &opts);
    assert_eq!(third.answers[0].refuses, 2, "the widened entry reaches the old reply");
}

impl Fx {
    /// A run that forgets what an earlier one kept.
    fn sweep_full(&self) -> sweep::Report {
        sweep::run(&self.db(), &self.store(), &Opts { budget: Duration::from_secs(60), full: true, now: NOW })
    }
}

#[test]
fn a_new_build_keeps_what_was_found_in_sight_until_its_pass_replaces_it_call_by_call() {
    let fx = Fx::new(&[wipe_ban()]);
    fx.write_calls(&[fx.bash("toolu_1", "acme-tool --wipe a", LATER), fx.bash("toolu_2", "acme-tool --wipe b", LATER + 5)]);
    assert_eq!(line(&fx.sweep(), "acme-wipe-ban").ran_fine, 2);
    let mut state = sweep::load_state(&fx.db());
    state.engine = "another-build".to_string();
    std::fs::write(sweep::state_path(&fx.db()), serde_json::to_string(&state).unwrap()).unwrap();

    // No time to try anything again: the earlier finding is still there.
    let none = sweep::run(&fx.db(), &fx.store(), &Opts { budget: Duration::ZERO, full: false, now: NOW });
    assert!(!none.covered.complete);
    assert_eq!(line(&none, "acme-wipe-ban").refuses, 2, "a new build must not blank the list while it works");

    // The ban no longer names --wipe; the pass that tries every call again finds that.
    let mut store = fx.store();
    let existing = named(&wipe_ban());
    let mut narrower = existing.clone();
    narrower.check = Some(Check::Forbidden { literals: vec!["--erase".to_string()] });
    narrower.text = "probe rule acme-wipe-ban: the acme-tool call needs its own answer, nothing else will do (names '--erase')".to_string();
    model::store::revise(&mut store, "s", "l", "a", &existing, &narrower).unwrap();
    drop(store);
    let done = fx.sweep();
    assert!(done.covered.complete);
    assert!(done.calls.iter().all(|l| l.id != "acme-wipe-ban"), "the calls it no longer refuses are gone from its line: {:?}", done.calls);
}

#[test]
fn a_cut_away_corpus_does_not_strand_the_sweep_at_a_row_that_is_no_longer_there() {
    let fx = Fx::new(&[wipe_ban()]);
    fx.write_calls(&[fx.bash("toolu_1", "acme-tool --wipe a", LATER), fx.bash("toolu_2", "ls", LATER + 1)]);
    fx.sweep();
    // The record is cut back so that the row the sweep stopped at is gone.
    fx.write_calls(&[fx.bash("toolu_9", "acme-tool --wipe z", LATER + 100)]);
    let after = fx.sweep();
    assert_eq!(after.covered.calls_replayed, 1, "every row that is left is new to it");
    assert!(line(&after, "acme-wipe-ban").refuses >= 1);
}

#[test]
fn a_call_another_build_judged_is_no_evidence_so_an_install_does_not_flag_rules_for_thirty_days() {
    let fx = Fx::new(&[wipe_ban()]);
    let mut older = fx.bash("toolu_1", "acme-tool --wipe cache", LATER);
    older.b = "an-older-build".to_string(); // the hook of that day decided with another engine
    let mut unknown = fx.bash("toolu_2", "acme-tool --wipe other", LATER);
    unknown.b = String::new(); // read back from a transcript: no engine known
    fx.write_calls(&[older, unknown]);
    let l = fx.sweep().calls.into_iter().find(|l| l.id == "acme-wipe-ban").unwrap();
    assert_eq!((l.refuses, l.ran_fine, l.uncounted), (2, 0, 2), "{l:?}");
}

#[test]
fn a_reply_another_build_judged_is_no_evidence_against_an_answer_entry() {
    let fx = Fx::new(&[]);
    write_rulebook(fx.dir.path());
    let real_now = serve::time::now_unix();
    let mut older = reply(real_now + 50_000, "can you check whether the file exists?", false);
    older.b = String::new(); // back-filled from a transcript
    write_replies(&fx.db(), &[older]);
    let report = sweep::run(&fx.db(), &fx.store(), &Opts { budget: Duration::from_secs(60), full: false, now: real_now + 200_000 });
    let l = report.answers.iter().find(|l| l.id == "ask-user-to-check").unwrap();
    assert_eq!((l.refuses, l.ran_fine), (1, 0), "{l:?}");
}

#[test]
fn a_note_that_ran_out_of_time_says_it_tried_only_the_newest_and_that_there_may_be_more() {
    let fx = Fx::new(&[]);
    let now = serve::time::now_unix();
    fx.write_calls(&[fx.bash("toolu_1", "acme-tool --wipe a", now - 86_400), fx.bash("toolu_2", "acme-tool --wipe b", now - 3_600)]);
    let note = sweep::would_refuse_note(&fx.store(), &named(&wipe_ban()), Duration::ZERO).expect("a note");
    assert!(
        note.starts_with("real calls: this rule would refuse at least 0 of 2 real calls in the last 30 days"),
        "{note}"
    );
    assert!(note.contains("only the newest 0 of the calls it could reach were tried, so there may be more"), "{note}");
    let whole = sweep::would_refuse_note(&fx.store(), &named(&wipe_ban()), Duration::from_secs(30)).expect("a note");
    assert!(whole.starts_with("real calls: this rule would refuse 2 of 2 real calls"), "{whole}");
    assert!(!whole.contains("may be more"), "{whole}");
}
