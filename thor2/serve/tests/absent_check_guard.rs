//! The Absent-check guard (`serve::absent_guard`), end to end through the
//! real compiled `serve hook` binary - mirrors `capture_guard_hook.rs`'s own
//! shape for exactly the same reason: the pure decision logic already has
//! its own unit tests in `serve::absent_guard`'s own test module; this file
//! proves the WIRING - the real store, the real marker file on disk, and the
//! real hook JSON shape on stdout, for a real Write/Edit PreToolUse payload.
//!
//! The forbidden literal used throughout is the em dash character, written
//! only ever as the Rust unicode escape `\u{2014}`, never as a literal
//! character in this source file, per this workspace's own typography rule.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

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

fn write_payload(session_id: &str, cwd: &Path, file_path: &Path, content: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Write",
        "tool_input": { "file_path": file_path.to_string_lossy(), "content": content },
    })
    .to_string()
}

/// `out` must not be a block decision. NOT the same as "must be empty": the
/// touched file may still carry the SAME item as an ordinary `Target::Path`
/// binding, which the pre-existing informational surface (surface 2) renders
/// as context on every touch regardless of this guard - that is the exact
/// "whisper" this guard exists ALONGSIDE, not something it silences. Only a
/// real `{"decision":"block",...}` counts as this guard (or anything else)
/// actually blocking the call.
fn assert_not_blocked(out: &str) {
    if out.trim().is_empty() {
        return;
    }
    let v: serde_json::Value =
        serde_json::from_str(out).unwrap_or_else(|e| panic!("expected valid JSON or empty output, got a parse error {e}: {out}"));
    assert_ne!(v.get("decision"), Some(&serde_json::Value::String("block".to_string())), "must not be a block decision: {out}");
}

fn edit_payload(session_id: &str, cwd: &Path, file_path: &Path, old_string: &str, new_string: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Edit",
        "tool_input": {
            "file_path": file_path.to_string_lossy(),
            "old_string": old_string,
            "new_string": new_string,
        },
    })
    .to_string()
}

/// A real store with one live Rule anchored to `NOTES.md` (relative to
/// `dir`), carrying a still-current `Check::Absent` forbidding the em dash
/// character. `NOTES.md` is created on disk right here so the anchor's
/// currency actually holds.
fn fixture_with_em_dash_rule(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.join("NOTES.md"), "# notes\n").unwrap();
    let item = model::item::Item {
        id: "no-em-dash-in-notes".to_string(),
        kind: model::item::Kind::Rule,
        text: "NOTES.md never carries an em dash character".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Path,
            value: "NOTES.md".to_string(),
        }],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("an em dash lands in NOTES.md and nobody notices".to_string()),
        check: Some(model::item::Check::Absent { path: "NOTES.md".to_string(), literal: "\u{2014}".to_string() }),
    };
    model::store::declare(&mut store, "s", "l", "a", &named(&item)).unwrap();
    db
}

/// THE REALISTIC CASE, END TO END: a rule anchored to a markdown file
/// forbidding the em dash character, and a real `Write` tool call whose
/// proposed content carries one, is blocked - through the real compiled
/// binary, not a unit-level call into the guard's own pure functions.
#[test]
fn a_write_introducing_an_em_dash_into_the_anchored_file_is_blocked_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule(dir.path());
    let notes = dir.path().join("NOTES.md");

    let out = run_hook(&db, &write_payload("s1", dir.path(), &notes, "a line with an em dash \u{2014} right here"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("[THOR]"), "{reason}");
    assert!(reason.contains("no-em-dash-in-notes"), "must name the rule id: {reason}");
    assert!(reason.contains("NOTES.md never carries an em dash character"), "must carry the rule's own text: {reason}");
    assert!(reason.contains('\u{2014}'), "the quoted fragment must carry the offending character itself: {reason}");
}

/// THE DEFECT THIS PREVENTS. This test used to assert the OPPOSITE: that an
/// identical resend was let through, on the reasoning that a gate saying no
/// forever invites a retry loop. That reasoning traded away the one moment a
/// gate exists for - the moment somebody insists - and it bought loop safety
/// with the forbidden write actually landing.
///
/// A repeat is refused again. The loop answer is now the WORDING: the second
/// refusal differs from the first and says stop resending, so a caller that
/// reads its input at all has something new to act on, and a caller that
/// does not was never going to be saved by being allowed through.
#[test]
fn the_same_write_sent_again_in_the_same_session_is_refused_again_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule(dir.path());
    let notes = dir.path().join("NOTES.md");

    let same = "one em dash \u{2014} here";
    let first = run_hook(&db, &write_payload("s1", dir.path(), &notes, same));
    let v: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(v["decision"], "block", "{first}");
    let first_reason = v["reason"].as_str().unwrap().to_string();

    let second = run_hook(&db, &write_payload("s1", dir.path(), &notes, same));
    let v: serde_json::Value = serde_json::from_str(&second)
        .unwrap_or_else(|e| panic!("an identical resend must still be judged: {e}: {second}"));
    assert_eq!(v["decision"], "block", "the forbidden write must not land on the second try: {second}");
    let second_reason = v["reason"].as_str().unwrap();
    assert!(second_reason.starts_with(&first_reason), "the original reason must survive: {second_reason}");
    assert!(second_reason.contains("ALREADY been refused"), "a repeat must say it is one: {second_reason}");
}

/// THE DEFECT THIS PREVENTS, and this test exists because the one above used
/// to assert it by accident. Its own doc comment says "the SAME write, sent
/// again", but its body sent a DIFFERENT one carrying a fresh violation of
/// the same rule, and asserted that it passed. The marker was keyed on the
/// file alone, so it did: one block disarmed this guard for that file for the
/// rest of the session, under any rule, including an Irreversible one, and
/// the allow path records nothing so nothing measured it.
///
/// A refusal that nothing records is a refusal nobody can count, and for the
/// whole of 2.0 up to 2026-08-08 that was every refusal there had ever been:
/// the gate was the reason this version exists and the one capability with no
/// measurement at all. So the event is part of the behaviour, not a nicety,
/// and it gets a test like any other behaviour here.
#[test]
fn a_refusal_is_written_into_the_log_naming_the_rule_that_refused() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule(dir.path());
    let notes = dir.path().join("NOTES.md");

    let before = gate_events(&db);
    assert!(before.is_empty(), "nothing should have refused anything yet: {before:?}");

    let out = run_hook(&db, &write_payload("s1", dir.path(), &notes, "one em dash \u{2014} here"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["decision"], "block", "{out}");

    let after = gate_events(&db);
    assert_eq!(after.len(), 1, "exactly one refusal, recorded once: {after:?}");
    assert_eq!(after[0].0, "gate_refused");
    assert_eq!(after[0].1, "no-em-dash-in-notes", "the event must name the rule that refused");

    // And a repeat is still a refusal, so it is still counted. This is the
    // number that tells you whether callers are arguing with the gate.
    run_hook(&db, &write_payload("s1", dir.path(), &notes, "one em dash \u{2014} here"));
    assert_eq!(gate_events(&db).len(), 2, "a repeat refusal must be counted too");
}

/// (kind, entity_id) for every gate telemetry event in the store, oldest
/// first. Reads the real log through the real reader - the point of the test
/// is that the binary WROTE these, so nothing here may fake them.
fn gate_events(db: &Path) -> Vec<(String, String)> {
    let store = thor_core::event_store::EventStore::open_existing(db).expect("store must open");
    store
        .get_all_events()
        .expect("events must read")
        .into_iter()
        .filter(|e| matches!(e.kind, thor_core::event_store::EventKind::GateRefused | thor_core::event_store::EventKind::GateStoodAside))
        .map(|e| (e.kind.as_str().to_string(), e.entity_id))
        .collect()
}

/// The test above holds the unchanged-retry half of the same rule; this one
/// holds the changed half. A different attempt is a different decision and
/// gets its own verdict, not an inherited one.
#[test]
fn a_different_write_carrying_a_fresh_violation_is_blocked_again() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule(dir.path());
    let notes = dir.path().join("NOTES.md");

    let first = run_hook(&db, &write_payload("s1", dir.path(), &notes, "one em dash \u{2014} here"));
    let v: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(v["decision"], "block", "{first}");

    let second = run_hook(&db, &write_payload("s1", dir.path(), &notes, "another em dash \u{2014} here too"));
    let v: serde_json::Value = serde_json::from_str(&second)
        .unwrap_or_else(|e| panic!("a fresh violation must still be judged: {e}: {second}"));
    assert_eq!(v["decision"], "block", "a second, different violation must not ride in free: {second}");
}

/// The once-per-session marker keys on (session, file), never on session
/// alone: a DIFFERENT file in the same session, carrying no rule of its own,
/// must never be blocked - proves the marker is not suppressing the whole
/// session's writes wholesale.
#[test]
fn a_different_file_in_the_same_session_is_not_touched_by_the_first_files_marker() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule(dir.path());
    let notes = dir.path().join("NOTES.md");
    let other = dir.path().join("OTHER.md");

    let first = run_hook(&db, &write_payload("s1", dir.path(), &notes, "one em dash \u{2014} here"));
    let v: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(v["decision"], "block", "{first}");

    let second = run_hook(&db, &write_payload("s1", dir.path(), &other, "an em dash \u{2014} in an unrelated file"));
    assert!(second.trim().is_empty(), "an unrelated, unanchored file must never be blocked by this guard: {second}");
}

/// The Edit tool's own payload shape: the replacement half (`new_string`),
/// never the removed half (`old_string`) - proves the same guard reads the
/// correct field for a different tool, through the real binary.
#[test]
fn an_edit_introducing_an_em_dash_via_new_string_is_blocked_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule(dir.path());
    let notes = dir.path().join("NOTES.md");

    let out = run_hook(&db, &edit_payload("s1", dir.path(), &notes, "old text", "new text with an em dash \u{2014} in it"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    assert!(v["reason"].as_str().unwrap().contains('\u{2014}'));
}

/// Sanity check that the fixture is meaningful: a write with no forbidden
/// character at all must never be blocked - without this, a bug that
/// blocked EVERY write to NOTES.md regardless of content would pass every
/// test above undetected.
#[test]
fn a_write_with_no_forbidden_character_is_never_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule(dir.path());
    let notes = dir.path().join("NOTES.md");

    let out = run_hook(&db, &write_payload("s1", dir.path(), &notes, "an entirely ordinary line, no such character"));
    assert_not_blocked(&out);
}

// ------------------------------------------- a session in a subfolder
//
// Check paths resolve against the top of the checkout the session works in
// (`project::checkout_top`), never against the folder it happens to stand in.
// MEASURED before the change (PR, re-run by FE as H10): a rule requiring a line
// to stay in the checkout's NOTES.md let an Edit remove it when the payload's
// cwd was a subfolder - the check path resolved to `<checkout>/sub/NOTES.md`,
// which does not exist.

/// A checkout at `dir/real` (a `.git` folder and a marker naming `demo`), an
/// empty subfolder `sub`, and a store beside it holding one `demo` rule that
/// requires 'keep this line' to stay in NOTES.md.
fn checkout_with_required_line(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let real = dir.join("real");
    std::fs::create_dir_all(real.join(".git")).unwrap();
    std::fs::create_dir_all(real.join("sub")).unwrap();
    std::fs::write(real.join(".thor-project"), "demo\n").unwrap();
    std::fs::write(real.join("NOTES.md"), "hello world\nkeep this line\n").unwrap();
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    let item = model::item::Item {
        id: "c-path".to_string(),
        kind: model::item::Kind::Rule,
        text: "NOTES.md must keep the line keep this line".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Path,
            value: "NOTES.md".to_string(),
        }],
        severity: Some(model::item::Severity::Irreversible),
        project: Some("demo".to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("The line may go.".to_string()),
        check: Some(model::item::Check::Contains { path: "NOTES.md".to_string(), literal: "keep this line".to_string() }),
    };
    model::store::declare(&mut store, "s", "l", "a", &named(&item)).unwrap();
    (db, real)
}

#[test]
fn a_session_in_a_subfolder_still_refuses_losing_a_required_literal() {
    let dir = tempfile::tempdir().unwrap();
    let (db, real) = checkout_with_required_line(dir.path());
    let notes = real.join("NOTES.md");
    let sub = real.join("sub");

    let out = run_hook(&db, &edit_payload("s1", &sub, &notes, "keep this line\n", ""));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "removing the line from a subfolder must be refused: {out}");
    assert!(
        v["reason"].as_str().unwrap().starts_with("[THOR] rule c-path requires this to stay in NOTES.md"),
        "{out}"
    );

    // The same refusal from the top of the checkout, as before.
    let out = run_hook(&db, &edit_payload("s1", &real, &notes, "keep this line\n", ""));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");

    // An edit that keeps the line passes from the subfolder too.
    assert_not_blocked(&run_hook(&db, &edit_payload("s1", &sub, &notes, "keep this line\n", "keep this line\n\n")));
}

/// A RELATIVE file path in a Write is relative to where the session stands:
/// from `sub`, `NOTES.md` is `sub/NOTES.md` (no rule there) and `../NOTES.md`
/// is the guarded file.
#[test]
fn a_relative_write_from_a_subfolder_is_judged_against_the_file_it_really_names() {
    let dir = tempfile::tempdir().unwrap();
    let (db, real) = checkout_with_required_line(dir.path());
    let sub = real.join("sub");

    let out = run_hook(&db, &write_payload("s1", &sub, Path::new("../NOTES.md"), "hello world\n"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "../NOTES.md from sub is the guarded file: {out}");
    assert!(v["reason"].as_str().unwrap().contains("c-path"), "{out}");

    assert_not_blocked(&run_hook(&db, &write_payload("s1", &sub, Path::new("NOTES.md"), "hello world\n")));
}

// ------------------------------------------- a session that drifts (D4)
//
// A session that worked in a checkout and then stands in a folder of no
// project keeps that project (`project::session_project`), and its PATH
// guards resolve against the project's registered checkout
// (`project::guard_root`), not against the folder. MEASURED before (the
// step 4 verifier's probe X1): from a scratch folder, an Edit of
// `<checkout>/NOTES.md` by its absolute path removing the required line went
// through, and so did `rm` of that file - while a scratch COPY of NOTES.md
// was refused as if it were the project's own file.

const DRIFTING_SESSION: &str = "9a3d5e70-1111-4222-8333-444455556661";

/// The checkout of `checkout_with_required_line`, registered the way the
/// installer registers one (written straight into the log: THOR's own
/// writer token is not for tests), plus two more `demo` rules on its files:
/// an absent check on NOTES.md and a protected directory `frozen`.
fn registered_checkout_with_file_rules(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let (db, real) = checkout_with_required_line(dir);
    std::fs::create_dir_all(real.join("frozen")).unwrap();
    let mut store = thor_core::event_store::EventStore::open_existing(&db).unwrap();
    let rule = |id: &str, text: &str, binding: model::item::Binding, check: model::item::Check| model::item::Item {
        id: id.to_string(),
        kind: model::item::Kind::Rule,
        text: text.to_string(),
        bindings: vec![binding],
        severity: Some(model::item::Severity::Irreversible),
        project: Some("demo".to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the guard turns out to refuse nothing in its own project".to_string()),
        check: Some(check),
    };
    let no_em_dash = rule(
        "no-em-dash-in-demo-notes",
        "NOTES.md in demo never carries an em dash character",
        model::item::Binding::Target { kind: model::item::TargetKind::Path, value: "NOTES.md".to_string() },
        model::item::Check::Absent { path: "NOTES.md".to_string(), literal: "\u{2014}".to_string() },
    );
    let frozen = rule(
        "never-touch-demo-frozen",
        "frozen/ in demo is a frozen archive and must never be written to",
        model::item::Binding::Target { kind: model::item::TargetKind::Dir, value: "frozen".to_string() },
        model::item::Check::PathExists { path: "frozen".to_string() },
    );
    for item in [no_em_dash, frozen] {
        model::store::declare(&mut store, "s", "l", "a", &named(&item)).unwrap();
    }

    let record = model::item::Item {
        id: model::store::project_opened_id("demo"),
        kind: model::item::Kind::Report,
        text: "Project demo: a fixture record.".to_string(),
        bindings: vec![],
        severity: None,
        project: Some("demo".to_string()),
        tags: vec!["project-record".to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    let body = model::store::canonical_body(&record).unwrap();
    let root = real.to_string_lossy().replace('\\', "/");
    let body = format!(
        "{},\"root\":{},\"root_at\":\"2026-09-28T10:00:00Z\"}}",
        &body[..body.len() - 1],
        serde_json::to_string(&root).unwrap()
    );
    let installer = model::store::INSTALLER_ACTOR;
    let kind = thor_core::event_store::EventKind::FactCreated;
    store.append_event(installer, installer, installer, kind, &record.id, None, &body).unwrap();
    (db, real)
}

fn bash_payload(session_id: &str, cwd: &Path, command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string()
}

fn refused_by(out: &str, rule_id: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(out) else { return false };
    v["decision"] == "block" && v["reason"].as_str().is_some_and(|r| r.contains(&format!("rule {rule_id} ")))
}

#[test]
fn a_session_that_drifted_out_of_its_project_keeps_its_file_guards() {
    let dir = tempfile::tempdir().unwrap();
    let (db, real) = registered_checkout_with_file_rules(dir.path());
    let notes = real.join("NOTES.md");
    let scratch = dir.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let slashed = |p: &Path| p.to_string_lossy().replace('\\', "/");

    // The session works in the checkout first.
    assert_not_blocked(&run_hook(&db, &bash_payload(DRIFTING_SESSION, &real, "git status")));

    // Then stands in a folder of no project, and names the checkout's own
    // files by their absolute paths: every path-anchored guard still holds.
    let out = run_hook(&db, &edit_payload(DRIFTING_SESSION, &scratch, &notes, "keep this line\n", ""));
    assert!(refused_by(&out, "c-path"), "an Edit removing the required line: {out}");
    let out = run_hook(&db, &bash_payload(DRIFTING_SESSION, &scratch, &format!("rm {}", slashed(&notes))));
    assert!(refused_by(&out, "c-path"), "rm of the checkout's NOTES.md: {out}");
    assert!(out.contains("would have removed"), "{out}");
    let out = run_hook(&db, &write_payload(DRIFTING_SESSION, &scratch, &notes, "hello world\nkeep this line \u{2014}\n"));
    assert!(refused_by(&out, "no-em-dash-in-demo-notes"), "a forbidden literal written into NOTES.md: {out}");
    let out = run_hook(&db, &write_payload(DRIFTING_SESSION, &scratch, &real.join("frozen").join("a.rs"), "fn main() {}"));
    assert!(refused_by(&out, "never-touch-demo-frozen"), "a write into the protected folder: {out}");
    let out = run_hook(&db, &bash_payload(DRIFTING_SESSION, &scratch, &format!("echo x > {}/frozen/a.rs", slashed(&real))));
    assert!(refused_by(&out, "never-touch-demo-frozen"), "a redirect into the protected folder: {out}");

    // What the rules allow still passes from there.
    assert_not_blocked(&run_hook(
        &db,
        &edit_payload(DRIFTING_SESSION, &scratch, &notes, "keep this line\n", "keep this line\n\n"),
    ));

    // A scratch COPY of those files is not the project's file: the project's
    // rules do not judge it.
    let copy = scratch.join("NOTES.md");
    std::fs::write(&copy, "hello world\nkeep this line\n").unwrap();
    std::fs::create_dir_all(scratch.join("frozen")).unwrap();
    assert_not_blocked(&run_hook(&db, &edit_payload(DRIFTING_SESSION, &scratch, &copy, "keep this line\n", "")));
    assert_not_blocked(&run_hook(&db, &bash_payload(DRIFTING_SESSION, &scratch, "rm NOTES.md")));
    assert_not_blocked(&run_hook(&db, &write_payload(DRIFTING_SESSION, &scratch, &copy, "a copy \u{2014}\n")));
    assert_not_blocked(&run_hook(&db, &write_payload(DRIFTING_SESSION, &scratch, &scratch.join("frozen").join("a.rs"), "x")));
}

// ------------------------------------------------------ staleness sidecar
//
// The staleness half of the doctrine (`serve::absent_guard`'s own module doc
// comment): a rule that matched the file being written and carried a check
// that could not prove its own currency is recorded to a sidecar beside the
// store, never affecting the block decision. The pure scan/fold logic
// already has its own unit tests in `serve::absent_guard`'s own test module;
// these prove the WIRING - the real store, the real sidecar file on disk,
// and a real Write PreToolUse payload, through the real compiled binary.

/// The SAME fixture as `fixture_with_em_dash_rule` above, except `NOTES.md`
/// is deliberately never created - so the rule's own currency can never be
/// proven.
fn fixture_with_em_dash_rule_but_no_anchored_file(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    let item = model::item::Item {
        id: "no-em-dash-in-notes".to_string(),
        kind: model::item::Kind::Rule,
        text: "NOTES.md never carries an em dash character".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Path,
            value: "NOTES.md".to_string(),
        }],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("an em dash lands in NOTES.md and nobody notices".to_string()),
        check: Some(model::item::Check::Absent { path: "NOTES.md".to_string(), literal: "\u{2014}".to_string() }),
    };
    model::store::declare(&mut store, "s", "l", "a", &named(&item)).unwrap();
    db
}

/// THE REALISTIC CASE, END TO END: a rule whose anchored file was deleted
/// (or never restored) never blocks a write that would otherwise have
/// tripped it - the unit-level guarantee
/// `a_rule_whose_anchored_path_no_longer_exists_never_blocks` already proves
/// in `serve::absent_guard`'s own test module - but here it ALSO leaves a
/// record in the staleness sidecar beside the store, through the real
/// compiled binary: the exact wiring a pure unit test cannot prove on its
/// own.
#[test]
fn a_write_that_could_not_prove_its_rules_currency_is_allowed_and_recorded_in_the_stale_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule_but_no_anchored_file(dir.path());
    let notes = dir.path().join("NOTES.md");
    let stale_path = db.parent().unwrap().join("absent-guard-stale.json");
    assert!(!stale_path.exists(), "fixture sanity: nothing recorded before the hook ever runs");

    let out = run_hook(&db, &write_payload("s1", dir.path(), &notes, "a line with an em dash \u{2014} right here"));
    assert_not_blocked(&out);

    let stale_text = std::fs::read_to_string(&stale_path)
        .unwrap_or_else(|e| panic!("the staleness sidecar must exist and be readable after the hook ran: {e}"));
    let stale: serde_json::Value = serde_json::from_str(&stale_text).unwrap();
    let record = &stale["no-em-dash-in-notes"];
    assert_eq!(record["outcome"].as_str(), Some("failed"), "{stale_text}");
    // "the file that was being written" is recorded exactly as the hook
    // payload named it - Claude Code's own `file_path` is absolute, and
    // nothing here normalizes it, the same stance the LOCATION guard's own
    // block message already takes for this same field (see
    // `location_block_message`).
    assert_eq!(record["file"].as_str(), Some(notes.to_string_lossy().as_ref()), "{stale_text}");
    assert_eq!(record["count"].as_u64(), Some(1), "{stale_text}");
    assert!(record["check"].as_str().unwrap().contains("Absent"), "{stale_text}");
}

/// The marker file the BLOCK path already uses (`absent-guard-blocked.json`)
/// must stay untouched by this feature when nothing ever blocks - proven by
/// confirming it is never even created by a call that only ever records
/// staleness.
#[test]
fn recording_staleness_never_creates_the_existing_block_marker() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule_but_no_anchored_file(dir.path());
    let notes = dir.path().join("NOTES.md");
    let blocked_marker = db.parent().unwrap().join("absent-guard-blocked.json");

    let out = run_hook(&db, &write_payload("s1", dir.path(), &notes, "a line with an em dash \u{2014} right here"));
    assert_not_blocked(&out);

    assert!(!blocked_marker.exists(), "a write that never blocked must never create the block marker either");
}

/// A second write in the same session, still against the same never-created
/// anchor, increments the sidecar's own count rather than duplicating the
/// row - through the real binary, mirroring the pure-function-level
/// `a_second_stale_occurrence_increments_the_count_instead_of_duplicating`
/// test in `serve::absent_guard`'s own test module.
#[test]
fn a_second_write_in_the_same_session_increments_the_stale_count() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_em_dash_rule_but_no_anchored_file(dir.path());
    let notes = dir.path().join("NOTES.md");
    let stale_path = db.parent().unwrap().join("absent-guard-stale.json");

    run_hook(&db, &write_payload("s1", dir.path(), &notes, "one em dash \u{2014} here"));
    run_hook(&db, &write_payload("s1", dir.path(), &notes, "another em dash \u{2014} here too"));

    let stale_text = std::fs::read_to_string(&stale_path).unwrap();
    let stale: serde_json::Value = serde_json::from_str(&stale_text).unwrap();
    assert_eq!(stale["no-em-dash-in-notes"]["count"].as_u64(), Some(2), "{stale_text}");
}

/// `item` with every check literal its text does not name appended to the
/// text (in `U+XXXX` form for a glyph): since step 8 of the two tracks the
/// write gate refuses a check whose literal the rule's own words never name
/// (A4), and these fixtures are about the hook, not about naming.
fn named(item: &model::item::Item) -> model::item::Item {
    let mut out = item.clone();
    let unnamed = model::gate::unnamed_literals(&out);
    if !unnamed.is_empty() {
        let shown: Vec<String> = unnamed.iter().map(|l| format!("'{}'", model::gate::shown_literal(l))).collect();
        out.text = format!("{} (names {})", out.text, shown.join(" "));
    }
    out
}
