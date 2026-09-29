//! The shell write hole (`serve::input::shell_write_targets`), wired into
//! `serve::guard_verdict::command`, end to end through the
//! real compiled `serve hook` binary - mirrors `command_absent_guard.rs`'s
//! own shape: the pure parsing logic already has its own unit tests in
//! `serve::input`'s own test module; this file proves the WIRING - a real
//! store, a real Bash PreToolUse payload, and the real hook JSON shape on
//! stdout.
//!
//! MEASURED: a rule anchored at a file with a `contains` or a location check
//! refuses an Edit/Write of that file, but `rm file`, `truncate -s 0 file`
//! and `echo x > file` from Bash walked straight past it - a shell write is
//! a write.

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

fn bash_payload(session_id: &str, cwd: &Path, command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Bash",
        "tool_input": { "command": command, "description": "run a command" },
    })
    .to_string()
}

/// `out` must not be a block decision - see `absent_check_guard.rs`'s own
/// identically-named helper for why this is not the same as "must be empty".
fn assert_not_blocked(out: &str) {
    if out.trim().is_empty() {
        return;
    }
    let v: serde_json::Value = serde_json::from_str(out)
        .unwrap_or_else(|e| panic!("expected valid JSON or empty output, got a parse error {e}: {out}"));
    assert_ne!(
        v.get("decision"),
        Some(&serde_json::Value::String("block".to_string())),
        "must not be a block decision: {out}"
    );
}

// ------------------------------------------------------------ contains rule
//
// A rule anchored to a real file with a `Check::Contains` - the shape that
// already refuses an Edit/Write which drops the required literal
// (`absent_guard::find_missing_required`). `rm`/`truncate -s 0` are the two
// shell writes whose resulting content is KNOWN to be empty (`Removed`/
// `Emptied`), so this is the check they get fed through, with "" standing in
// for what the file would hold afterward.

fn fixture_with_contains_rule(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.join("NOTES.md"), "the agreed rule: never force-push main\n").unwrap();
    let item = model::item::Item {
        id: "notes-keeps-the-rule".to_string(),
        kind: model::item::Kind::Rule,
        text: "NOTES.md always keeps the force-push rule".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Path,
            value: "NOTES.md".to_string(),
        }],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the rule goes missing from NOTES.md and nobody notices".to_string()),
        check: Some(model::item::Check::Contains {
            path: "NOTES.md".to_string(),
            literal: "never force-push main".to_string(),
        }),
    };
    model::store::declare(&mut store, "s", "l", "a", &named(&item)).unwrap();
    db
}

/// THE REALISTIC CASE, END TO END: `rm` on a file whose required literal
/// would vanish with it is refused - through the real compiled binary, not
/// a unit-level call into `find_missing_required` itself.
#[test]
fn rm_on_a_file_with_a_contains_rule_is_refused_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_contains_rule(dir.path());

    let out = run_hook(&db, &bash_payload("s1", dir.path(), "rm NOTES.md"));
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("[THOR]"), "{reason}");
    assert!(reason.contains("notes-keeps-the-rule"), "must name the rule id: {reason}");
    assert!(reason.contains("never force-push main"), "must name the literal that would be lost: {reason}");
    assert!(reason.contains("rm NOTES.md"), "must name the command: {reason}");
    assert!(reason.contains("removed"), "must say what the command would have done: {reason}");
    assert!(reason.contains("Nothing was done"), "{reason}");
}

/// The second shell write whose result is knowably empty: `truncate -s 0`
/// refuses the same way `rm` does, and says EMPTIED rather than REMOVED.
#[test]
fn truncate_s_zero_on_a_file_with_a_contains_rule_is_refused_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_contains_rule(dir.path());

    let out = run_hook(&db, &bash_payload("s1", dir.path(), "truncate -s 0 NOTES.md"));
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("notes-keeps-the-rule"), "{reason}");
    assert!(reason.contains("emptied"), "must say emptied, not removed: {reason}");
}

/// A relative path in a shell write is relative to where the session stands,
/// while the rule's own check path resolves against the top of its checkout.
/// From `sub`, `rm NOTES.md` removes `sub/NOTES.md` - a different file, never
/// refused on behalf of the top-level one (R4.2) - and `rm ../NOTES.md` is the
/// guarded file itself.
#[test]
fn a_relative_rm_from_a_subfolder_is_judged_against_the_file_it_really_names() {
    let dir = tempfile::tempdir().unwrap();
    let top = dir.path().join("checkout");
    std::fs::create_dir_all(top.join(".git")).unwrap();
    std::fs::create_dir_all(top.join("sub")).unwrap();
    let db = fixture_with_contains_rule(&top);
    let sub = top.join("sub");

    assert_not_blocked(&run_hook(&db, &bash_payload("s1", &sub, "rm NOTES.md")));
    std::fs::write(sub.join("NOTES.md"), "scratch notes, nothing required\n").unwrap();
    assert_not_blocked(&run_hook(&db, &bash_payload("s1", &sub, "rm NOTES.md")));

    for command in ["rm ../NOTES.md", "truncate -s 0 ../NOTES.md", "rm ./../NOTES.md"] {
        let out = run_hook(&db, &bash_payload("s1", &sub, command));
        let v: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|e| panic!("{command}: expected a decision JSON: {e}: {out}"));
        assert_eq!(v["decision"], "block", "{command}: {out}");
        let reason = v["reason"].as_str().unwrap();
        assert!(reason.contains("notes-keeps-the-rule"), "{command}: {reason}");
        assert!(reason.contains(command), "the refusal quotes the command as written: {reason}");
    }

    // From the top of the checkout, as before.
    let out = run_hook(&db, &bash_payload("s1", &top, "rm NOTES.md"));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
}

/// A plain read is not a write at all: `shell_write_targets` finds nothing
/// for `cat`, so the guarded file is never even examined.
#[test]
fn a_plain_read_is_never_blocked_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_contains_rule(dir.path());

    let out = run_hook(&db, &bash_payload("s1", dir.path(), "cat NOTES.md"));
    assert_not_blocked(&out);
}

// ------------------------------------------------------------- location rule
//
// A rule protecting a whole path as a LOCATION (`severity: Irreversible`
// plus a `Check::PathExists` for the SAME path a binding names - see
// `serve::absent_guard::location_anchor`'s own doc comment for the
// three-condition rule). Unlike the `Contains` fixture above, this arm fires
// on EVERY shell write effect, `Rewritten` included, which is what makes
// `echo x > <file>` - a redirect WITH a producer - refusable at all: it is
// never fed to `find_missing_required`, but it is still a write to a
// protected PLACE, and the location guard does not care what the write
// leaves behind, only where it lands.

fn fixture_with_location_rule(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.join("NOTES.md"), "# notes\n").unwrap();
    let item = model::item::Item {
        id: "notes-is-frozen".to_string(),
        kind: model::item::Kind::Rule,
        text: "NOTES.md is frozen, never touch it from a shell".to_string(),
        bindings: vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Path,
            value: "NOTES.md".to_string(),
        }],
        severity: Some(model::item::Severity::Irreversible),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("NOTES.md turns out to be safe to touch after all".to_string()),
        check: Some(model::item::Check::PathExists { path: "NOTES.md".to_string() }),
    };
    model::store::declare(&mut store, "s", "l", "a", &named(&item)).unwrap();
    db
}

/// THE REALISTIC CASE, END TO END: a redirect into a location-protected file
/// is refused, proving `echo x > <file>` reaches the SAME location guard a
/// real Write/Edit already goes through.
#[test]
fn a_redirect_into_a_location_protected_file_is_refused_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_location_rule(dir.path());

    let out = run_hook(&db, &bash_payload("s1", dir.path(), "echo x > NOTES.md"));
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("notes-is-frozen"), "{reason}");
    assert!(reason.contains("echo x > NOTES.md"), "must name the command: {reason}");
}

/// THE DEFECT THIS PREVENTS: a redirect is refused ONLY when a rule at that
/// exact place says so - never a blanket ban on `>` itself. A redirect into
/// a DIFFERENT, unprotected file must sail through.
#[test]
fn a_redirect_into_an_unprotected_file_is_not_blocked_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_with_location_rule(dir.path());

    let out = run_hook(&db, &bash_payload("s1", dir.path(), "echo x > OTHER.md"));
    assert_not_blocked(&out);
}

// ------------------------------------------------ the same segments (5d)
//
// The shell write guard reads the call with the SAME segments every command
// tooth reads (`serve::shell`). Measured against the tokenizer before it: a
// write on its own line after a `cd`, a write inside `$(...)`, and a write
// inside a `bash -lc` that is not at the head of the line all walked past a
// rule anchored at the file. A relative path is read against the folder its
// segment runs in.

fn contains_rule(id: &str, path: &str, text: &str) -> model::item::Item {
    model::item::Item {
        id: id.to_string(),
        kind: model::item::Kind::Rule,
        text: text.to_string(),
        bindings: vec![model::item::Binding::Target { kind: model::item::TargetKind::Path, value: path.to_string() }],
        severity: Some(model::item::Severity::HouseStyle),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(format!("the agreement goes missing from {path} and nobody notices")),
        check: Some(model::item::Check::Contains { path: path.to_string(), literal: "agreement: keep".to_string() }),
    }
}

/// A root `CLAUDE.md` and an `x/CLAUDE.md`, each with a contains rule, and a
/// `vendor` folder under a location prohibition.
fn fixture_for_segments(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::create_dir_all(dir.join("x")).unwrap();
    std::fs::create_dir_all(dir.join("vendor")).unwrap();
    std::fs::write(dir.join("CLAUDE.md"), "agreement: keep\n").unwrap();
    std::fs::write(dir.join("x").join("CLAUDE.md"), "agreement: keep\n").unwrap();
    std::fs::write(dir.join("x").join("other.md"), "free text\n").unwrap();
    std::fs::write(dir.join("vendor").join("a.txt"), "vendored\n").unwrap();
    for item in [
        contains_rule("root-claude-keeps", "CLAUDE.md", "the top CLAUDE.md always keeps its agreement line"),
        contains_rule("x-claude-keeps", "x/CLAUDE.md", "folder x mirrors the build contract in its own notes file"),
    ] {
        model::store::declare(&mut store, "s", "l", "a", &named(&item)).unwrap();
    }
    let vendor = model::item::Item {
        id: "vendor-is-frozen".to_string(),
        kind: model::item::Kind::Rule,
        text: "vendor is frozen, never write in it".to_string(),
        bindings: vec![model::item::Binding::Target { kind: model::item::TargetKind::Dir, value: "vendor".to_string() }],
        severity: Some(model::item::Severity::Irreversible),
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("vendor turns out safe to write after all".to_string()),
        check: Some(model::item::Check::PathExists { path: "vendor".to_string() }),
    };
    model::store::declare(&mut store, "s", "l", "a", &named(&vendor)).unwrap();
    db
}

fn refused_with(out: &str, id: &str, verb: &str) {
    let v: serde_json::Value =
        serde_json::from_str(out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains(id), "must name {id}: {reason}");
    assert!(reason.contains(&format!("would have {verb}")), "must say it would have {verb} the file: {reason}");
}

/// sh forms: a newline after a `cd`, `$(...)`, and `bash -lc` after `&&`.
#[test]
fn a_write_in_any_segment_is_refused_and_read_in_its_own_folder() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_for_segments(dir.path());
    refused_with(&run_hook(&db, &bash_payload("s1", dir.path(), "cd x\nrm CLAUDE.md")), "x-claude-keeps", "removed");
    refused_with(&run_hook(&db, &bash_payload("s1", dir.path(), "x=$(rm CLAUDE.md)")), "root-claude-keeps", "removed");
    refused_with(
        &run_hook(&db, &bash_payload("s1", dir.path(), "cd x && bash -lc 'rm CLAUDE.md'")),
        "x-claude-keeps",
        "removed",
    );
    refused_with(&run_hook(&db, &bash_payload("s1", dir.path(), "cd x\n: > CLAUDE.md")), "x-claude-keeps", "emptied");
    // The location prohibition reads the same segments.
    refused_with(&run_hook(&db, &bash_payload("s1", dir.path(), "cd vendor\nrm a.txt")), "vendor-is-frozen", "removed");
    refused_with(&run_hook(&db, &bash_payload("s1", dir.path(), "x=$(rm vendor/a.txt)")), "vendor-is-frozen", "removed");
}

/// sh forms that write nothing local: a heredoc body is data, a command sent
/// over ssh runs elsewhere, a mention is not a write, and a relative path in
/// another folder names another file.
#[test]
fn a_mention_a_heredoc_or_a_remote_write_is_never_refused() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_for_segments(dir.path());
    for command in [
        "cat > notes.md <<'EOF'\nrm CLAUDE.md\nEOF",
        "ssh nas 'rm /srv/CLAUDE.md'",
        "ssh nas 'cd /srv && rm CLAUDE.md'",
        "echo 'rm CLAUDE.md'",
        "grep -n 'rm CLAUDE.md' notes.md",
        "cd x && rm other.md",
    ] {
        assert_not_blocked(&run_hook(&db, &bash_payload("s1", dir.path(), command)));
    }
}

/// A `cd` that did not take effect never moves a write to another file. The
/// same leak as the door's project scope (2026-09-28): a `cd` inside a
/// subshell, in a pipeline, or into a folder that is not there moved every
/// later write into that folder, so `(cd free); rm CLAUDE.md` was judged as
/// removing `free/CLAUDE.md` while it removed the guarded top file. A write
/// after a `cd` that may not have run is judged in both folders.
#[test]
fn a_write_after_a_cd_that_did_not_take_effect_is_judged_where_it_lands() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_for_segments(dir.path());
    std::fs::create_dir_all(dir.path().join("free")).unwrap();
    for command in [
        "(cd free); rm CLAUDE.md",
        "(cd free && ls) && rm CLAUDE.md",
        "cd free | cat; rm CLAUDE.md",
        "cd not-there; rm CLAUDE.md",
    ] {
        refused_with(&run_hook(&db, &bash_payload("s1", dir.path(), command)), "root-claude-keeps", "removed");
    }
    for command in ["if true; then cd vendor; fi; rm a.txt", "true && cd vendor; rm a.txt"] {
        refused_with(&run_hook(&db, &bash_payload("s1", dir.path(), command)), "vendor-is-frozen", "removed");
    }
    // A cd that certainly took effect still moves the write.
    assert_not_blocked(&run_hook(&db, &bash_payload("s1", dir.path(), "cd free; rm CLAUDE.md")));
    assert_not_blocked(&run_hook(&db, &bash_payload("s1", dir.path(), "cd x; rm other.md")));
}

/// A `cd` the shell refuses moves nothing, and arithmetic is no heredoc - so
/// each write below removes the guarded top file, and is judged there
/// (measured against Git Bash 5.3, 2026-09-28: bash refuses `cd free extra`
/// and `cd -x free`, has no `chdir`, and reads `<<` in `(( ... ))` as a
/// shift). Before, the first three were judged as removing `free/CLAUDE.md`
/// and the last three hid the `rm` in a heredoc that is not there.
#[test]
fn a_write_after_a_refused_cd_or_after_arithmetic_is_judged_where_it_lands() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture_for_segments(dir.path());
    std::fs::create_dir_all(dir.path().join("free")).unwrap();
    for command in [
        "cd free extra; rm CLAUDE.md",
        "cd -x free; rm CLAUDE.md",
        "chdir free; rm CLAUDE.md",
        "(( y = 1 << 2 ))
rm CLAUDE.md",
        "echo $[1<<2]
rm CLAUDE.md",
        "for ((i=0; i<<1; i++)); do :; done
rm CLAUDE.md",
    ] {
        refused_with(&run_hook(&db, &bash_payload("s1", dir.path(), command)), "root-claude-keeps", "removed");
    }
    // What bash takes still moves the write, and arithmetic alone writes nothing.
    assert_not_blocked(&run_hook(&db, &bash_payload("s1", dir.path(), "cd -P free; rm CLAUDE.md")));
    assert_not_blocked(&run_hook(&db, &bash_payload("s1", dir.path(), "(( y = 1 << 2 ))
ls")));
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
