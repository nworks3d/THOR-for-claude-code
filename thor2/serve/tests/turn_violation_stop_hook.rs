//! A rule broken during a turn by a route no tool guard sees (`serve::
//! turn_violation`), end to end through the real compiled `serve hook`.
//!
//! The benchmark route: a rule with an `absent_all` check on a file, an
//! assistant refused at the Edit, and then a small script run outside the
//! project that writes the forbidden text into the file anyway. Here the
//! script is simulated by writing the file directly, between the turn start
//! (`UserPromptSubmit`) and the turn end (`Stop`).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const RULE: &str = "keep-pa6-off-the-materials-page";
const FILE: &str = "site/materialen.html";
const CLEAN: &str = "<h1>Materials</h1>\n<p>PLA and PETG</p>\n";
const BROKEN: &str = "<h1>Materials</h1>\n<p>PLA and PETG</p>\n<p>PA6 nylon</p>\n";

fn run_hook(db: &Path, payload: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    assert!(out.stderr.is_empty(), "the hook must never write to stderr: {:?}", out.stderr);
    String::from_utf8(out.stdout).unwrap()
}

/// A checkout folder holding the page, and a store with one rule: the page
/// never names PA6 (`absent_all`, bound to the page itself).
fn fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    let work = dir.join("work");
    std::fs::create_dir_all(work.join("site")).unwrap();
    std::fs::write(work.join(FILE), CLEAN).unwrap();
    let item = model::item::Item {
        id: RULE.to_string(),
        kind: model::item::Kind::Rule,
        text: "The materials page never names PA6 (the literal 'PA6')".to_string(),
        bindings: vec![model::item::Binding::Target { kind: model::item::TargetKind::Path, value: FILE.to_string() }],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("PA6 is added to the materials page on purpose and nobody objects".to_string()),
        check: Some(model::item::Check::AbsentAll { path: FILE.to_string(), literals: vec!["PA6".to_string()] }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    (db, work)
}

fn prompt(session: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "prompt": "please tidy up the materials page for the shop",
    })
    .to_string()
}

fn stop(session: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "stop_hook_active": false,
        "last_assistant_message": "",
    })
    .to_string()
}

/// The reason of a block decision, or `None` when the output holds nothing.
fn blocked(out: &str) -> Option<String> {
    if out.trim().is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(out).unwrap_or_else(|e| panic!("expected JSON: {e}: {out}"));
    (v["decision"] == "block").then(|| v["reason"].as_str().unwrap_or_default().to_string())
}

/// A stale-rule sidecar entry for the rule, as if an earlier write had
/// recorded it, so the tests can prove the stale-rule guard stays out.
fn plant_stale_entry(dir: &Path) {
    let sidecar = serde_json::json!({
        RULE: { "outcome": "failed", "check": "AbsentAll", "file": FILE, "count": 1, "seq_at_record": 0 }
    });
    std::fs::write(dir.join("absent-guard-stale.json"), sidecar.to_string()).unwrap();
}

#[test]
fn a_script_that_writes_the_forbidden_text_holds_the_turn_and_lifts_when_restored() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());

    begin(&db, "s1", &work);
    // The route no tool guard sees: not an Edit, not a Write, no literal in a command.
    std::fs::write(work.join(FILE), BROKEN).unwrap();

    let reason = blocked(&run_hook(&db, &stop("s1", &work))).expect("the turn must be held");
    assert!(reason.contains(RULE), "names the rule: {reason}");
    assert!(reason.contains(FILE), "names the file: {reason}");
    assert!(reason.contains("PA6"), "names the literal now present: {reason}");
    assert!(reason.contains("undo it so the file no longer contains that text"), "names the one honest exit: {reason}");
    assert!(reason.contains("ask the owner before revising"), "weakening is never the default: {reason}");
    assert!(!reason.contains("could not prove themselves"), "this is not the stale-rule guard: {reason}");
    assert!(!dir.path().join("stale-guard-blocked.json").exists(), "the stale-rule guard must not have fired");

    // Put right, in a next turn: nothing holds.
    std::fs::write(work.join(FILE), CLEAN).unwrap();
    begin(&db, "s1", &work);
    let out = run_hook(&db, &stop("s1", &work));
    assert!(blocked(&out).is_none(), "once the check holds again the hold lifts: {out}");
}

#[test]
fn a_turn_that_changes_nothing_is_never_held() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    begin(&db, "s1", &work);
    let out = run_hook(&db, &stop("s1", &work));
    assert!(blocked(&out).is_none(), "{out}");
}

#[test]
fn a_violation_that_stays_holds_twice_then_only_a_line_never_a_third_hold() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    // The stale sidecar names this rule at every Stop below (planted after the
    // shell command, which would otherwise rescan and drop it): the stale-rule
    // guard must stay out of it each time.

    begin(&db, "s1", &work);
    std::fs::write(work.join(FILE), BROKEN).unwrap();
    let first = blocked(&{ plant_stale_entry(dir.path()); run_hook(&db, &stop("s1", &work)) }).expect("first hold");
    assert!(first.contains(RULE), "{first}");

    begin(&db, "s1", &work);
    let second = blocked(&{ plant_stale_entry(dir.path()); run_hook(&db, &stop("s1", &work)) }).expect("second hold: the violation stayed");
    assert!(second.contains(RULE), "{second}");

    begin(&db, "s1", &work);
    let third = { plant_stale_entry(dir.path()); run_hook(&db, &stop("s1", &work)) };
    assert!(blocked(&third).is_none(), "never a third hold: {third}");
    assert!(third.contains("will not hold the turn again"), "one plain line instead: {third}");
    assert!(!third.contains("could not prove themselves"), "and not the stale-rule guard either: {third}");

    begin(&db, "s1", &work);
    let fourth = { plant_stale_entry(dir.path()); run_hook(&db, &stop("s1", &work)) };
    assert!(blocked(&fourth).is_none(), "{fourth}");
    assert!(!fourth.contains("will not hold the turn again"), "the line is said once: {fourth}");
    assert!(!dir.path().join("stale-guard-blocked.json").exists(), "the stale-rule guard never fired");

    // Recorded, so doctor and the evaluation can count it.
    let store = thor_core::event_store::EventStore::open_existing(&db).unwrap();
    let stood = store.events_of_kinds(&[thor_core::event_store::EventKind::GateStoodAside], 0).unwrap();
    let wanted = format!("turn-violation:{RULE}");
    assert!(stood.iter().any(|e| e.entity_id == wanted), "the stand-aside must be on record");
}

#[test]
fn a_check_that_already_failed_before_the_turn_is_not_held_by_this_guard() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    std::fs::write(work.join(FILE), BROKEN).unwrap(); // broken BEFORE the turn

    begin(&db, "s1", &work);
    let out = run_hook(&db, &stop("s1", &work));
    assert!(blocked(&out).is_none(), "already failing at the start: the stale-rule guard's business, not a hold: {out}");
}

#[test]
fn a_turn_that_was_cut_off_before_its_stop_is_still_held_at_the_next_stop() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    begin(&db, "s1", &work);
    std::fs::write(work.join(FILE), BROKEN).unwrap();
    // No Stop: the owner interrupted. The next turn starts with the file broken.
    begin(&db, "s1", &work);
    let reason = blocked(&run_hook(&db, &stop("s1", &work))).expect("the interrupted turn's violation must not be lost");
    assert!(reason.contains(RULE), "{reason}");
    assert!(reason.contains("earlier, interrupted turn"), "names the earlier turn: {reason}");
}

#[test]
fn a_rule_revised_during_the_turn_is_the_owners_decision_and_never_held() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    begin(&db, "s1", &work);

    // The owner said yes: the rule is revised to allow the text, then the file changes.
    let mut store = thor_core::event_store::EventStore::open_existing(&db).unwrap();
    let existing = model::store::show(&store, RULE).unwrap();
    let mut updated = existing.clone();
    updated.check = Some(model::item::Check::AbsentAll { path: FILE.to_string(), literals: vec!["PA12".to_string()] });
    updated.text = "The materials page never names PA12 (the literal 'PA12')".to_string();
    model::store::revise(&mut store, "s", "l", "a", &existing, &updated).unwrap();
    drop(store);
    std::fs::write(work.join(FILE), BROKEN).unwrap();

    let out = run_hook(&db, &stop("s1", &work));
    assert!(blocked(&out).is_none(), "a revised rule has no baseline: {out}");
}

#[test]
fn a_contains_fact_whose_text_is_removed_in_the_turn_is_never_held_by_this_guard() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    // A fact that quotes the page: it must keep the text 'PLA and PETG'.
    let mut store = thor_core::event_store::EventStore::open_existing(&db).unwrap();
    let item = model::item::Item {
        id: "page-quotes-pla-and-petg".to_string(),
        kind: model::item::Kind::Rule,
        text: "The materials page lists 'PLA and PETG'".to_string(),
        bindings: vec![model::item::Binding::Target { kind: model::item::TargetKind::Path, value: FILE.to_string() }],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the page no longer lists those materials".to_string()),
        check: Some(model::item::Check::Contains { path: FILE.to_string(), literal: "PLA and PETG".to_string() }),
    };
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
    drop(store);

    begin(&db, "s1", &work);
    std::fs::write(work.join(FILE), "<h1>Materials</h1>\n<p>PLA only</p>\n").unwrap();
    let out = run_hook(&db, &stop("s1", &work));
    assert!(blocked(&out).is_none(), "contains is never this guard's business: {out}");

    // And the stale-rule guard behaves exactly as before for such a fact.
    let sidecar = serde_json::json!({
        "page-quotes-pla-and-petg": { "outcome": "failed", "check": "Contains", "file": FILE, "count": 1, "seq_at_record": 0 }
    });
    begin(&db, "s1", &work);
    std::fs::write(dir.path().join("absent-guard-stale.json"), sidecar.to_string()).unwrap();
    let reason = blocked(&run_hook(&db, &stop("s1", &work))).expect("the stale-rule guard still asks about it");
    assert!(reason.contains("could not prove themselves") && reason.contains("page-quotes-pla-and-petg"), "{reason}");
}

fn git(work: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(work)
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_checkout_during_the_turn_makes_the_turn_silent() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    git(&work, &["init", "-q"]);
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "first"]);
    git(&work, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(work.join(FILE), BROKEN).unwrap();
    git(&work, &["commit", "-q", "-am", "second"]);
    git(&work, &["checkout", "-q", "-"]);

    begin(&db, "s1", &work);
    // A branch switch (or a pull) brings the forbidden text in: nobody wrote it.
    git(&work, &["checkout", "-q", "feature"]);
    let out = run_hook(&db, &stop("s1", &work));
    assert!(blocked(&out).is_none() && !out.contains(RULE), "files that changed under the session are nobody's violation: {out}");

    begin(&db, "s1", &work);
    let out = run_hook(&db, &stop("s1", &work));
    assert!(blocked(&out).is_none(), "{out}");
}

#[test]
fn a_plain_commit_after_the_script_does_not_silence_the_guard() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    git(&work, &["init", "-q"]);
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "first"]);

    begin(&db, "s1", &work);
    std::fs::write(work.join(FILE), BROKEN).unwrap(); // the script
    git(&work, &["commit", "-q", "-am", "second"]); // and the assistant commits it
    let reason = blocked(&run_hook(&db, &stop("s1", &work))).expect("a commit moves HEAD but not the files: still held");
    assert!(reason.contains(RULE), "{reason}");
}

/// A shell command about to run (the first one of a turn takes the snapshot).
fn shell(session: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Bash",
        "tool_input": { "command": "node /tmp/somewhere/script.mjs" },
    })
    .to_string()
}

/// A turn that starts and runs its first shell command.
fn begin(db: &Path, session: &str, cwd: &Path) {
    run_hook(db, &prompt(session, cwd));
    run_hook(db, &shell(session, cwd));
}

#[test]
fn a_turn_with_no_shell_command_takes_no_snapshot_and_is_never_held() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    run_hook(&db, &prompt("s1", &work));
    std::fs::write(work.join(FILE), BROKEN).unwrap(); // by some route no shell command announced
    let out = run_hook(&db, &stop("s1", &work));
    assert!(blocked(&out).is_none(), "{out}");
    assert!(!dir.path().join("turn-violations.json").exists(), "no snapshot record may exist");
    assert!(!dir.path().join("turn-violation-rules.json").exists(), "and no rule list was even read");
}

#[test]
fn a_second_shell_command_in_the_same_turn_does_not_retake_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    begin(&db, "s1", &work);
    std::fs::write(work.join(FILE), BROKEN).unwrap();
    // The second command runs after the first one broke the page: a fresh
    // snapshot here would record the broken page as the start and lose it.
    run_hook(&db, &shell("s1", &work));
    let reason = blocked(&run_hook(&db, &stop("s1", &work))).expect("still held");
    assert!(reason.contains(RULE), "{reason}");
}

#[test]
fn a_new_prompt_retires_the_snapshot_so_a_turn_without_a_shell_command_is_not_judged() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    begin(&db, "s1", &work);
    run_hook(&db, &stop("s1", &work));
    run_hook(&db, &prompt("s1", &work)); // next turn: no shell command in it
    std::fs::write(work.join(FILE), BROKEN).unwrap();
    let out = run_hook(&db, &stop("s1", &work));
    assert!(blocked(&out).is_none(), "the previous turn's snapshot must not judge this turn: {out}");
}

#[test]
fn an_interrupted_turn_followed_by_a_pull_is_silent() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    git(&work, &["init", "-q"]);
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "first"]);

    begin(&db, "s1", &work); // turn one: snapshot, then the owner interrupts (no Stop)
    // Between the turns a pull / checkout brings the forbidden text in.
    git(&work, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(work.join(FILE), BROKEN).unwrap();
    git(&work, &["commit", "-q", "-am", "pulled"]);

    begin(&db, "s1", &work);
    let out = run_hook(&db, &stop("s1", &work));
    assert!(blocked(&out).is_none(), "the old snapshot must not be carried across a changed checkout: {out}");
}

/// A write by a tool that is neither a shell command nor an Edit (an MCP file
/// tool, a notebook edit) before any shell command in the turn.
fn mcp_write(session: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "mcp__files__write_file",
        "tool_input": { "path": "somewhere" },
    })
    .to_string()
}

#[test]
fn an_mcp_style_write_before_any_shell_command_is_caught() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    run_hook(&db, &prompt("s1", &work));
    run_hook(&db, &mcp_write("s1", &work)); // takes the snapshot, before the write lands
    std::fs::write(work.join(FILE), BROKEN).unwrap();
    let reason = blocked(&run_hook(&db, &stop("s1", &work))).expect("held");
    assert!(reason.contains(RULE), "{reason}");
}

#[test]
fn read_only_calls_never_take_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    run_hook(&db, &prompt("s1", &work));
    for tool in ["Read", "Grep", "Glob", "LS", "ToolSearch", "TodoWrite", "WebFetch", "WebSearch", "mcp__thor__lookup", "mcp__thor__outline"] {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse", "session_id": "s1", "cwd": work.to_string_lossy(),
            "tool_name": tool, "tool_input": {},
        });
        run_hook(&db, &payload.to_string());
    }
    assert!(!dir.path().join("turn-violations.json").exists(), "reads take no snapshot");
}

#[test]
fn the_rule_cache_is_named_after_its_store() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    begin(&db, "s1", &work);
    assert!(dir.path().join("turn-violation-rules.thor.json").exists());
    assert!(!dir.path().join("turn-violation-rules.json").exists());
}
