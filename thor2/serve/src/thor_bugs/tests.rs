#![cfg(test)]

use super::*;
use model::item::{Item, Kind};
use model::store::{EvaluationValidation, HookFault, VerdictShares, EVAL_VALIDATOR_ACTOR};

const T0: i64 = 1_800_000_000;

struct Fx {
    _dir: tempfile::TempDir,
    db: PathBuf,
    store: EventStore,
}

impl Fx {
    fn new() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let store = EventStore::new(&db).unwrap();
        Fx { _dir: dir, db, store }
    }

    /// A report the Stop refused, with the validator's reason.
    fn refused_report(&mut self, report: &str, why: &str) {
        let record = EvaluationValidation {
            valid: false,
            why: why.to_string(),
            drift: false,
            report: report.to_string(),
            report_seq: 1,
            project: Some("demo-project".to_string()),
            session: "s-filing".to_string(),
            agent: None,
            started_seq: None,
            filed_seq: None,
            filed_at: None,
            machine_sha256: None,
            verdicts: VerdictShares::default(),
            carry: Vec::new(),
        };
        // A fixture event written raw: a test holds no THOR actor token.
        let body = serde_json::to_string(&record).unwrap();
        self.store.append_event("s-filing", "s-filing", EVAL_VALIDATOR_ACTOR, EventKind::GateStoodAside, report, None, &body).unwrap();
    }

    fn hook_fault(&mut self, id: &str, event: &str, exit_code: i64, host_wide: bool) {
        let fault = HookFault {
            tool_use_id: id.to_string(),
            hook_event: event.to_string(),
            exit_code,
            at: "2026-10-06T22:29:00.231Z".to_string(),
            host_wide,
        };
        let body = serde_json::json!({ "hook_fault": fault }).to_string();
        let entity = model::store::hook_fault_entity("s-fault");
        self.store.append_event("s-fault", "s-fault", ATTESTER_ACTOR, EventKind::GateStoodAside, &entity, None, &body).unwrap();
    }
}

fn skew_why(theirs: u32, ours: u32) -> String {
    format!(
        "{SKEW_WHY}: its machine section was rendered by format {theirs}, this checker reads format {ours} - restart Claude \
         so the tool server is the current one, and file again; nothing was edited"
    )
}

const MISMATCH_WHY: &str = "its machine section is not the one THOR derives from the log for seq 10 to 20 - it was edited, or \
                            written past the filing gate; it first differs at line 4 of its machine section";

// -------------------------------------------------------------- fingerprints

#[test]
fn the_same_bug_has_the_same_fingerprint_whatever_the_session_or_the_counts() {
    let a = hook_launch_finding("SessionStart", 126);
    let b = hook_launch_finding("SessionStart", 126);
    assert_eq!(a.fingerprint(), b.fingerprint());
    assert!(a.fingerprint().starts_with("hook-launch-"), "{}", a.fingerprint());
    assert_ne!(a.fingerprint(), hook_launch_finding("SessionStart", 66).fingerprint());
    assert_ne!(a.fingerprint(), hook_launch_finding("Stop", 126).fingerprint());
    let sweep = |refused| sweep_finding(&SweepHit { engine_class: "Monitor loops".into(), refused, of_calls: 99, example_shape: "until".into() });
    assert_eq!(sweep(3).fingerprint(), sweep(40).fingerprint(), "counts are evidence, not identity");
}

#[test]
fn every_kind_of_finding_has_a_short_title_and_the_whole_template() {
    let findings = [
        filing_finding(&skew_why(6, 7)).unwrap(),
        filing_finding(MISMATCH_WHY).unwrap(),
        filing_finding("the filing gate, run again on the log as it stood at seq 1, refuses it: x").unwrap(),
        hook_launch_finding("Stop", 126),
        hook_panic_finding("hook", "serve/src/live.rs:3"),
        false_block_pattern_finding("git", 2, 3),
        sweep_finding(&SweepHit { engine_class: "Monitor loops".into(), refused: 1, of_calls: 2, example_shape: "until".into() }),
        seed_refused_finding("starter", "REFUSED: x"),
        start_note_cut_finding(&["A4 list"]),
        doctor_routine_finding("zebra"),
    ];
    for f in &findings {
        assert!(!f.title.is_empty() && f.title.chars().count() <= 100 && !f.title.ends_with('.'), "{}: {}", f.signal.as_str(), f.title);
        for part in [&f.summary, &f.why_thor, &f.expected, &f.actual, &f.fix_area] {
            assert!(!part.trim().is_empty(), "{}", f.signal.as_str());
        }
        assert!(!f.evidence.is_empty(), "{}", f.signal.as_str());
        for text in std::iter::once(&f.title).chain(std::iter::once(&f.summary)).chain(f.evidence.iter()) {
            assert!(text.is_ascii(), "plain ASCII only: {text}");
        }
    }
}

#[test]
fn a_fingerprint_is_a_legal_tag() {
    for finding in [filing_finding(&skew_why(6, 7)).unwrap(), hook_panic_finding("hook", "serve/src/live.rs:3"), start_note_cut_finding(&["A4 list"])] {
        let tag = format!("{REPORT_TAG_PREFIX}{}", finding.fingerprint());
        assert!(tag.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == ':'), "{tag}");
    }
}

// ---------------------------------------------------------------- the signals

#[test]
fn a_report_refused_as_skew_is_a_bug_candidate_with_both_formats_in_the_evidence() {
    let finding = filing_finding(&skew_why(6, 7)).expect("skew is THOR's");
    assert_eq!(finding.signal, Signal::FilingSkew);
    assert_eq!(finding.key, "6>7");
    assert!(finding.evidence.iter().any(|l| l.contains("format: 6") && l.contains("format: 7")), "{:?}", finding.evidence);
}

#[test]
fn a_mismatch_with_the_same_format_and_a_rerun_gate_refusal_are_candidates() {
    assert_eq!(filing_finding(MISMATCH_WHY).unwrap().signal, Signal::FilingMismatch);
    let gate = "the filing gate, run again on the log as it stood at seq 20, refuses it: something";
    assert_eq!(filing_finding(gate).unwrap().signal, Signal::FilingGate);
}

#[test]
fn a_refusal_that_is_the_agents_or_the_machines_is_no_candidate() {
    for why in [
        "not filed through an attested THOR tool call (carries no stamp)",
        "the session's transcript is gone - no transcript file of session x is on this machine",
        "its evaluation 'evaluation-start-demo-5' is void: 'a' was changed at log seq 3 outside an attested THOR call of it (cli)",
        "the start note of 'evaluation-start-demo-5' is not in session x's transcript",
        "'x' was changed at log seq 9 outside an attested THOR call of this evaluation (cli)",
    ] {
        assert!(filing_finding(why).is_none(), "{why}");
    }
}

#[test]
fn detect_finds_refused_filings_and_hook_failures_but_not_host_wide_ones() {
    let mut fx = Fx::new();
    fx.refused_report("eval-demo-1", &skew_why(6, 7));
    fx.refused_report("eval-demo-2", "not filed through an attested THOR tool call (carries no stamp)");
    fx.hook_fault("toolu-1", "SessionStart", 126, false);
    fx.hook_fault("toolu-2", "Stop", 66, true);
    let found = detect(&fx.store, 0);
    let signals: Vec<Signal> = found.iter().map(|f| f.signal).collect();
    assert_eq!(signals, vec![Signal::FilingSkew, Signal::HookLaunch], "{found:?}");
    assert_eq!(found[1].key, "SessionStart:126");
    assert!(detect(&fx.store, i64::MAX).is_empty(), "nothing after the end of the log");
}

#[test]
fn several_rules_loosened_after_refusing_the_same_command_word_are_one_pattern() {
    let block = |rule: &str, call: &str| model::store::FalseBlock {
        rule: rule.to_string(),
        line: format!("'{rule}': refused '{call}' at log seq 5, then weakened (check cleared) at log seq 9"),
        restore: String::new(),
    };
    let one_rule_twice = [block("r1", "git status"), block("r1", "git log")];
    assert!(false_block_patterns(&one_rule_twice).is_empty(), "one rule is that rule's wording");
    let two_rules = [block("r1", "git status"), block("r2", "git log --oneline"), block("r3", "node build.js")];
    let found = false_block_patterns(&two_rules);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].signal, Signal::FalseBlockPattern);
    assert_eq!(found[0].key, "git");
    assert!(!found[0].summary.contains("r1"), "no rule id leaves the machine: {}", found[0].summary);
    let by_path = [block("r1", "C:\\tools\\run.cmd a"), block("r2", "/usr/bin/run.cmd b")];
    assert_eq!(false_block_patterns(&by_path)[0].key, "run.cmd", "the program, not the folder it sits in");
}

#[test]
fn a_sweep_hit_becomes_a_candidate_through_the_hook_function() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let hit = SweepHit { engine_class: "Monitor loops".into(), refused: 4, of_calls: 120, example_shape: "until <test>; do sleep 1; done".into() };
    assert!(add(&db, sweep_finding(&hit)), "new");
    assert!(!add(&db, sweep_finding(&hit)), "the same bug is not new twice");
    let book = load(&db);
    assert_eq!(book.candidates.len(), 1);
    assert_eq!(book.candidates[0].seen, 2);
    assert!(book.candidates[0].finding.evidence.iter().any(|l| l.contains("4 of 120")));
}

#[test]
fn a_refused_starting_note_is_a_candidate() {
    let finding = seed_refused_finding("some-seeded-note", "REFUSED: this note has no falsifier\nfix: add one");
    assert_eq!(finding.signal, Signal::SeedRefused);
    assert!(finding.evidence.iter().any(|l| l.contains("REFUSED: this note has no falsifier")));
    assert!(!finding.evidence.iter().any(|l| l.contains("fix: add one")), "only the first line");
}

#[test]
fn a_doctor_line_the_routine_never_mentions_is_a_gap() {
    let routine = "doctor prints `tool server:` and `installed programs:` when one is wrong, and `gate:`, `teeth:` and `ship:` lines.";
    let lines: Vec<String> = [
        "doctor 2.5.0",
        "installed programs: serve.exe is build a - they were not built from the same source",
        "gate: 399 refusals by 161 distinct rules",
        "  refused by: some-rule (57x)",
        "teeth: 62 of 305 rule(s)",
        "ship: last succeeded 0h ago",
        "Zebra Crossing: a new line nobody wrote down",
        "Zebra Crossing: twice",
        "a sentence without any label at all that goes on for a very long time before a colon: x",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(routine_gaps(routine, &lines), vec!["zebra crossing".to_string()]);
}

// ----------------------------------------------------------------- the panics

#[test]
fn a_hook_panic_is_noted_once_and_taken_off_the_panic_file() {
    let fx = Fx::new();
    record_panic(&fx.db, "hook", "serve/src/live.rs:3");
    record_panic(&fx.db, "hook", "serve/src/live.rs:3");
    let mut book = Book::default();
    scan(&fx.db, &fx.store, &mut book, T0, "v");
    assert_eq!(book.candidates.len(), 1, "one place, one bug");
    assert_eq!(book.candidates[0].finding.signal, Signal::HookPanic);
    assert_eq!(book.candidates[0].seen, 2);
    scan(&fx.db, &fx.store, &mut book, T0 + 1, "v");
    assert_eq!(book.candidates[0].seen, 2, "the panic file was emptied by the scan");
}

#[test]
fn a_held_store_directory_records_no_panic_and_no_candidate() {
    let fx = Fx::new();
    let _hold = crate::store_dir::hold(&fx.db);
    record_panic(&fx.db, "hook", "serve/src/live.rs:3");
    save(&fx.db, &Book::default());
    assert!(!fx.db.parent().unwrap().join(PANIC_FILE).exists());
    assert!(!fx.db.parent().unwrap().join(FILE).exists());
}

// ------------------------------------------------------------ status handling

#[test]
fn upsert_counts_sightings_and_reopens_a_resolved_bug_only_for_a_later_build() {
    let mut book = Book::default();
    assert!(upsert(&mut book, hook_launch_finding("Stop", 126), T0, "2.5.0 (build a)"));
    assert!(!upsert(&mut book, hook_launch_finding("Stop", 126), T0 + 10, "2.5.0 (build a)"));
    assert_eq!((book.candidates[0].seen, book.candidates[0].first_seen, book.candidates[0].last_seen), (2, T0, T0 + 10));
    book.candidates[0].status = Status::Resolved;
    upsert(&mut book, hook_launch_finding("Stop", 126), T0 + 20, "2.5.0 (build a)");
    assert_eq!(book.candidates[0].status, Status::Resolved, "the same build: still resolved");
    upsert(&mut book, hook_launch_finding("Stop", 126), T0 + 30, "2.6.0 (build b)");
    assert_eq!(book.candidates[0].status, Status::New, "a later build sees it again: reopened");
}

#[test]
fn evidence_is_bounded() {
    let mut finding = hook_launch_finding("Stop", 126);
    finding.evidence = (0..9).map(|i| format!("{i} {}", "x".repeat(500))).collect();
    let mut book = Book::default();
    upsert(&mut book, finding, T0, "v");
    let kept = &book.candidates[0].finding.evidence;
    assert_eq!(kept.len(), EVIDENCE_LINES);
    assert!(kept.iter().all(|l| l.chars().count() <= EVIDENCE_LINE_CHARS + 3));
}

#[test]
fn a_report_tagged_with_the_fingerprint_marks_the_candidate_reported_and_a_resolved_tag_closes_it() {
    let mut fx = Fx::new();
    let mut book = Book::default();
    upsert(&mut book, hook_launch_finding("Stop", 126), T0, "v");
    upsert(&mut book, hook_panic_finding("hook", "serve/src/live.rs:3"), T0, "v");
    let reported = book.candidates[0].fingerprint.clone();
    let resolved = book.candidates[1].fingerprint.clone();
    let summaries = [book.candidates[0].finding.summary.clone(), book.candidates[1].finding.summary.clone()];
    let report = |id_fp: &str, tags: Vec<String>| Item {
        id: format!("thor-bug-{id_fp}"),
        kind: Kind::Report,
        text: format!(
            "OUTCOME for {id_fp} ({}): shown to the owner, not pushed (test environment).",
            summaries[usize::from(id_fp != reported)]
        ),
        bindings: vec![],
        severity: None,
        project: None,
        tags,
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    model::store::declare(&mut fx.store, "t", "t", "t", &report(&reported, vec![format!("thor-bug:{reported}")]))
        .expect("an outcome report needs no scope");
    model::store::declare(
        &mut fx.store,
        "t",
        "t",
        "t",
        &report(&resolved, vec![format!("thor-bug:{resolved}"), format!("thor-bug-resolved:{resolved}")]),
    )
    .unwrap();
    reconcile(&mut book, &fx.store);
    assert_eq!(book.candidates[0].status, Status::Reported);
    assert_eq!(book.candidates[1].status, Status::Resolved);
}

// ---------------------------------------------------------------- the nudge

fn two_bugs(fx: &mut Fx) {
    fx.refused_report("eval-demo-1", &skew_why(6, 7));
    fx.hook_fault("toolu-1", "SessionStart", 126, false);
}

#[test]
fn with_no_mode_installed_nothing_is_ever_offered() {
    let mut fx = Fx::new();
    two_bugs(&mut fx);
    assert_eq!(read_mode(&fx.db), None);
    assert!(nudge_for_session(&fx.db, "s1", T0).is_none());
    assert!(!fx.db.parent().unwrap().join(FILE).exists(), "not even a scan was written");
}

#[test]
fn the_mode_sidecar_round_trips_and_a_damaged_one_reads_as_none() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    write_mode(&db, Mode::Test).unwrap();
    assert_eq!(read_mode(&db), Some(Mode::Test));
    write_mode(&db, Mode::Release).unwrap();
    assert_eq!(read_mode(&db), Some(Mode::Release));
    std::fs::write(dir.path().join(MODE_FILE), "{\"mode\": \"production\"}").unwrap();
    assert_eq!(read_mode(&db), None);
    std::fs::write(dir.path().join(MODE_FILE), "not json").unwrap();
    assert_eq!(read_mode(&db), None);
}

#[test]
fn a_session_gets_at_most_one_candidate_and_the_next_session_the_next_one() {
    let mut fx = Fx::new();
    two_bugs(&mut fx);
    write_mode(&fx.db, Mode::Release).unwrap();
    let first = nudge_for_session(&fx.db, "s1", T0).expect("a bug is waiting");
    assert!(first.contains("filing-skew"), "{first}");
    assert!(nudge_for_session(&fx.db, "s1", T0 + 1).is_none(), "once per session");
    let second = nudge_for_session(&fx.db, "s2", T0 + 2).expect("the other bug");
    assert!(second.contains("hook-launch"), "{second}");
    assert!(nudge_for_session(&fx.db, "s3", T0 + 3).is_none(), "both are presented and it is too soon to ask again");
    let book = load(&fx.db);
    assert!(book.candidates.iter().all(|c| c.status == Status::Presented && c.presentations == 1));
}

#[test]
fn a_candidate_nobody_reported_comes_back_once_after_a_day_and_then_never() {
    let mut fx = Fx::new();
    fx.hook_fault("toolu-1", "SessionStart", 126, false);
    write_mode(&fx.db, Mode::Release).unwrap();
    assert!(nudge_for_session(&fx.db, "s1", T0).is_some());
    assert!(nudge_for_session(&fx.db, "s2", T0 + 3_600).is_none(), "an hour later: too soon");
    assert!(nudge_for_session(&fx.db, "s3", T0 + RENUDGE_AFTER_SECONDS + 1).is_some(), "a day later: once more");
    assert!(nudge_for_session(&fx.db, "s4", T0 + 3 * RENUDGE_AFTER_SECONDS).is_none(), "two offers are all it gets");
}

#[test]
fn a_reported_candidate_is_not_offered_again() {
    let mut fx = Fx::new();
    fx.hook_fault("toolu-1", "SessionStart", 126, false);
    write_mode(&fx.db, Mode::Release).unwrap();
    let nudge = nudge_for_session(&fx.db, "s1", T0).unwrap();
    let fp = nudge.split("thor-bug:").nth(1).unwrap().split('"').next().unwrap().to_string();
    let item = Item {
        id: format!("thor-bug-{fp}"),
        kind: Kind::Report,
        text: format!("OUTCOME for {fp}: owner declined."),
        bindings: vec![],
        severity: None,
        project: None,
        tags: vec![format!("thor-bug:{fp}")],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    model::store::declare(&mut fx.store, "agent-session", "l", "a", &item).unwrap();
    assert!(nudge_for_session(&fx.db, "s2", T0 + 2 * RENUDGE_AFTER_SECONDS).is_none());
    assert_eq!(load(&fx.db).candidates[0].status, Status::Reported);
}

#[test]
fn the_test_nudge_says_it_is_a_test_environment_and_the_release_nudge_does_not() {
    for (mode, test_env) in [(Mode::Test, true), (Mode::Release, false)] {
        let mut fx = Fx::new();
        fx.hook_fault("toolu-1", "SessionStart", 126, false);
        write_mode(&fx.db, mode).unwrap();
        let text = nudge_for_session(&fx.db, "s1", T0).unwrap();
        assert_eq!(text.contains("THIS IS A TEST ENVIRONMENT"), test_env, "{text}");
        assert_eq!(text.contains("normally I would push this to the THOR repo"), test_env);
        assert!(text.contains("THOR vond een fout in zichzelf") && text.contains("saying no is fine"), "the one question is in both: {text}");
        assert!(text.contains("TITLE: ") && text.contains("**Expected:**"), "{text}");
    }
}

#[test]
fn a_scan_is_not_repeated_within_five_minutes() {
    let mut fx = Fx::new();
    write_mode(&fx.db, Mode::Release).unwrap();
    assert!(nudge_for_session(&fx.db, "s1", T0).is_none(), "nothing yet");
    fx.hook_fault("toolu-1", "SessionStart", 126, false);
    assert!(nudge_for_session(&fx.db, "s2", T0 + 60).is_none(), "scanned a minute ago");
    assert!(nudge_for_session(&fx.db, "s3", T0 + SCAN_EVERY_SECONDS + 1).is_some(), "the next scan finds it");
}

#[test]
fn the_nudge_carries_nothing_private_even_when_a_detector_was_careless() {
    let fx = Fx::new();
    write_mode(&fx.db, Mode::Release).unwrap();
    let dir = fx.db.parent().unwrap().display().to_string();
    let leak = format!("REFUSED: could not write C:\\Users\\someone\\thor2\\notes at {dir} for host build-box.internal 10.9.8.7");
    add(&fx.db, seed_refused_finding("starter", &leak));
    let text = nudge_for_session(&fx.db, "s1", T0).unwrap();
    for private in ["someone", "build-box", "10.9.8.7", dir.as_str()] {
        assert!(!text.contains(private), "{private} leaked: {text}");
    }
    assert!(text.contains("<path>") && text.contains("<ip>"), "{text}");
}

#[test]
fn the_doctor_line_says_the_mode_and_the_counts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    assert!(doctor_line(&db).is_none());
    write_mode(&db, Mode::Test).unwrap();
    add(&db, hook_launch_finding("Stop", 126));
    let line = doctor_line(&db).unwrap();
    assert!(line.contains("mode test") && line.contains("1 new, 0 presented, 0 reported, 0 resolved"), "{line}");
}

// ---------------------------------------------------- an install closes old evidence

#[test]
fn an_install_closes_every_older_candidate_and_only_new_evidence_raises_one_again() {
    let mut fx = Fx::new();
    // The evidence an earlier build left in the log, and the candidates it made.
    two_bugs(&mut fx);
    write_mode(&fx.db, Mode::Release).unwrap();
    let old_build = "2.5.0 (build old)";
    let mut book = Book::default();
    scan(&fx.db, &fx.store, &mut book, T0, old_build);
    assert_eq!(book.candidates.len(), 2, "the old build found both");
    book.candidates[1].status = Status::Presented;
    save(&fx.db, &book);

    // The install of the new build: it closes both and moves the watermark past the old log.
    let mut book = load(&fx.db);
    let head = fx.store.get_next_seq().unwrap() - 1;
    assert_eq!(retire_old_evidence(&mut book, head, "2.5.0 (build new)"), 2);
    assert!(book.candidates.iter().all(|c| c.status == Status::Resolved && c.resolved_by_upgrade));
    assert_eq!(book.scanned_seq, head);
    save(&fx.db, &book);
    assert!(nudge_for_session(&fx.db, "s1", T0 + 10_000).is_none(), "old evidence is not nudged");

    // The same fault, logged again AFTER the install: the new build raised it, so it is offered.
    fx.hook_fault("toolu-2", "SessionStart", 126, false);
    let text = nudge_for_session(&fx.db, "s2", T0 + 20_000).expect("new evidence from the new build");
    assert!(text.contains("hook-launch"), "{text}");
}

#[test]
fn an_install_leaves_a_reported_candidate_and_one_its_own_build_found() {
    let mut book = Book::default();
    upsert(&mut book, hook_launch_finding("Stop", 126), T0, "2.5.0 (build old)");
    upsert(&mut book, hook_launch_finding("SessionStart", 66), T0, "2.5.0 (build new)");
    upsert(&mut book, hook_panic_finding("hook", "serve/src/live.rs:3"), T0, "2.5.0 (build old)");
    book.candidates[2].status = Status::Reported;
    assert_eq!(retire_old_evidence(&mut book, 50, "2.5.0 (build new)"), 1);
    let statuses: Vec<Status> = book.candidates.iter().map(|c| c.status).collect();
    assert_eq!(statuses, vec![Status::Resolved, Status::New, Status::Reported]);
}
