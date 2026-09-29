//! `serve retest`, end to end through the real compiled binary (step 9): the
//! one maintenance door to THOR's trial, which replaced `probe-teeth`.
//!
//! What it must never do: write without `--apply`, write over a rule that
//! changed while it was being tried, take a working copy from the command
//! line or from the folder it runs in, write anything for a rule that is
//! blocked or needs the agent, or ever stamp an event `teeth-probe`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use thor_core::event_store::{EventKind, EventStore};

fn git(dir: &Path, hooks: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A real checkout named `name` under `root`, with `NOTES.md` committed on
/// `main`. Its hooks path is an empty folder, so no shared hook acts on it.
fn checkout(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let hooks = root.join("no-hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    git(&dir, &hooks, &["init", "-q", "-b", "main"]);
    git(&dir, &hooks, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
    git(&dir, &hooks, &["config", "user.email", "fixture@example.invalid"]);
    git(&dir, &hooks, &["config", "user.name", "fixture"]);
    std::fs::write(dir.join("NOTES.md"), "keep me\nsecond line keep me\nplain line\n").unwrap();
    git(&dir, &hooks, &["add", "NOTES.md"]);
    git(&dir, &hooks, &["commit", "-q", "-m", "notes"]);
    dir
}

/// The installer's registration of `root` for `key`, as its own event lands
/// on the log (a raw append: THOR's actor token is the installer's own).
fn register(store: &mut EventStore, key: &str, root: &Path) {
    let record = Item {
        id: model::store::project_opened_id(key),
        kind: Kind::Report,
        text: format!("Project {key}: a fixture record."),
        bindings: Vec::new(),
        severity: None,
        project: Some(key.to_string()),
        tags: vec!["project-record".to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    let body = model::store::canonical_body(&record).unwrap();
    let body = format!(
        "{},\"root\":{},\"root_at\":\"2026-09-29T00:00:00Z\"}}",
        &body[..body.len() - 1],
        serde_json::to_string(root.to_str().unwrap()).unwrap()
    );
    store
        .append_event("install", "install", model::store::INSTALLER_ACTOR, EventKind::FactCreated, &record.id, None, &body)
        .unwrap();
}

/// A rule written straight into the log, the way an older store holds it:
/// no trial record, so no verdict yet.
fn legacy(store: &mut EventStore, item: &Item) {
    let body = serde_json::to_string(item).unwrap();
    store.append_event("legacy", &item.id, "migration", EventKind::FactCreated, &item.id, None, &body).unwrap();
}

fn rule(id: &str, project: &str, text: &str, bindings: Vec<Binding>, check: Check) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings,
        severity: Some(Severity::Irreversible),
        project: Some(project.to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the mistake turns out to cost nothing".to_string()),
        check: Some(check),
    }
}

fn command(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Command, value: value.to_string() }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    beta: PathBuf,
    scratch: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let beta = checkout(tmp.path(), "beta");
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let db = tmp.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    register(&mut store, "beta", &beta);
    legacy(
        &mut store,
        &rule(
            "co-rule",
            "beta",
            "NOTES.md must keep the line `keep me`.",
            vec![Binding::Target { kind: TargetKind::Path, value: "NOTES.md".to_string() }],
            Check::Contains { path: "NOTES.md".to_string(), literal: "keep me".to_string() },
        ),
    );
    legacy(
        &mut store,
        &rule(
            "f-rule",
            "beta",
            "Never run `git push --force` on main.",
            vec![command("git push")],
            Check::Forbidden { literals: vec!["--force".to_string()] },
        ),
    );
    legacy(
        &mut store,
        &rule(
            "a4-rule",
            "beta",
            "Never push a mirror of this repository.",
            vec![command("git push")],
            Check::Forbidden { literals: vec!["--mirr0r-zz".to_string()] },
        ),
    );
    legacy(
        &mut store,
        &rule(
            "gamma-rule",
            "gamma",
            "NOTES.md must keep the line `keep me`.",
            vec![Binding::Target { kind: TargetKind::Path, value: "NOTES.md".to_string() }],
            Check::Contains { path: "NOTES.md".to_string(), literal: "keep me".to_string() },
        ),
    );
    Fixture { _tmp: tmp, db, beta, scratch }
}

fn serve_in(db: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_serve")).current_dir(cwd).arg("--db").arg(db).args(args).output().expect("serve runs")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn events(db: &Path) -> Vec<thor_core::event_store::Event> {
    EventStore::open_existing(db).unwrap().get_all_events().unwrap()
}

#[test]
fn a_dry_run_prints_root_attempts_and_verdict_and_writes_nothing() {
    let f = fixture();
    let before = events(&f.db).len();
    let out = serve_in(&f.db, &f.scratch, &["retest", "--id", "co-rule"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains(&format!("root: {} (registered for 'beta')", f.beta.display())), "{text}");
    assert!(text.contains("tried "), "every attempt is printed: {text}");
    assert!(text.contains("verdict: rule track, proven"), "{text}");
    assert!(text.contains("nothing written - run this again with --apply"), "{text}");
    assert_eq!(events(&f.db).len(), before, "a dry run writes nothing");
}

#[test]
fn apply_writes_one_event_by_retest_carrying_the_record() {
    let f = fixture();
    let before = events(&f.db).len();
    let out = serve_in(&f.db, &f.scratch, &["retest", "--id", "co-rule", "--apply"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    let after = events(&f.db);
    assert_eq!(after.len(), before + 1, "exactly one event: {text}");
    let written = after.last().unwrap();
    assert_eq!(written.actor, "retest");
    assert_eq!(written.kind, EventKind::FactRevised);
    assert!(matches!(model::store::trial_of(written), Ok(Some(_))), "it carries the trial record");
    let store = EventStore::open_existing(&f.db).unwrap();
    assert_eq!(model::store::current_verdict(&store, "co-rule"), model::store::CurrentVerdict::RuleProven);
    assert!(text.contains(&format!("written: fact_revised at log seq {} by 'retest'", written.seq)), "{text}");
}

#[test]
fn a_rule_that_changed_while_it_was_tried_is_refused_and_nothing_is_written() {
    let f = fixture();
    let mut store = EventStore::open_existing(&f.db).unwrap();
    let corpus = serve::legit::Corpus::load(&store);
    let retest = serve::teeth::retest(&store, Some(f.db.as_path()), "f-rule", &corpus).unwrap();
    // Another process revises the text between the read and the write.
    let mut moved = model::store::show(&store, "f-rule").unwrap();
    moved.text = "Never run `git push --force` on main, nor on any shared branch.".to_string();
    let body = serde_json::to_string(&moved).unwrap();
    store.append_mutate_checked("other", "f-rule", "migration", EventKind::FactRevised, "f-rule", None, &body).unwrap();
    let before = store.get_all_events().unwrap().len();
    let err = serve::teeth::apply_as_retest(&mut store, &retest).unwrap_err();
    assert_eq!(err.to_string(), "REFUSED: f-rule changed while it was being tried (fingerprint moved) - run retest again");
    assert_eq!(store.get_all_events().unwrap().len(), before, "no event written");
}

#[test]
fn an_a4_failure_needs_the_agent_and_leaves_the_rule_undecided() {
    let f = fixture();
    let before = events(&f.db).len();
    let out = serve_in(&f.db, &f.scratch, &["retest", "--id", "a4-rule", "--apply"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(4), "{text}");
    assert!(text.contains("verdict: needs the agent - A4:"), "{text}");
    assert!(text.contains("nothing written"), "{text}");
    assert_eq!(events(&f.db).len(), before);
    let store = EventStore::open_existing(&f.db).unwrap();
    assert!(matches!(model::store::current_verdict(&store, "a4-rule"), model::store::CurrentVerdict::Undecided { .. }));
}

#[test]
fn a_blocked_rule_exits_3_and_writes_nothing() {
    let f = fixture();
    let before = events(&f.db).len();
    let out = serve_in(&f.db, &f.scratch, &["retest", "--id", "gamma-rule", "--apply"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(3), "{text}");
    assert!(text.contains("verdict: blocked: gamma - no registered working copy; nothing written"), "{text}");
    assert_eq!(events(&f.db).len(), before);
}

#[test]
fn a_root_on_the_command_line_is_a_usage_error_and_writes_nothing() {
    let f = fixture();
    let before = events(&f.db).len();
    let out = serve_in(&f.db, &f.scratch, &["retest", "--id", "co-rule", "--root", f.scratch.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unexpected argument '--root'"), "{:?}", out);
    assert_eq!(events(&f.db).len(), before);

    let out = serve_in(&f.db, &f.scratch, &["probe-teeth", "co-rule", "--root", f.beta.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unrecognized subcommand 'probe-teeth'"), "{:?}", out);
    assert_eq!(events(&f.db).len(), before);
}

#[test]
fn the_outcome_is_the_same_from_any_working_folder() {
    let f = fixture();
    let from_scratch = stdout(&serve_in(&f.db, &f.scratch, &["retest", "--id", "co-rule"]));
    let from_beta = stdout(&serve_in(&f.db, &f.beta, &["retest", "--id", "co-rule"]));
    let from_temp = stdout(&serve_in(&f.db, &std::env::temp_dir(), &["retest", "--id", "co-rule"]));
    assert_eq!(from_scratch, from_beta);
    assert_eq!(from_scratch, from_temp);
}

#[test]
fn a_batch_tries_every_rule_without_a_verdict_and_exits_with_the_worst() {
    let f = fixture();
    let out = serve_in(&f.db, &f.scratch, &["retest", "--project", "beta", "--apply"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(4), "a4-rule needs the agent: {text}");
    assert!(text.contains("co-rule:") && text.contains("f-rule:") && text.contains("a4-rule:"), "{text}");
    assert!(!text.contains("gamma-rule"), "another project's rule is not in this batch: {text}");
    // A second run leaves what has a verdict alone.
    let again = stdout(&serve_in(&f.db, &f.scratch, &["retest", "--project", "beta"]));
    assert!(again.contains("2 already have one and were left as they are"), "{again}");
    // And a store-wide run reaches the blocked one too.
    let all = serve_in(&f.db, &f.scratch, &["retest", "--all"]);
    assert!(stdout(&all).contains("gamma-rule:"), "{}", stdout(&all));
}

#[test]
fn no_event_is_ever_stamped_teeth_probe() {
    let f = fixture();
    for args in [
        vec!["retest", "--id", "co-rule", "--apply"],
        vec!["retest", "--id", "f-rule", "--apply"],
        vec!["retest", "--all", "--apply"],
    ] {
        let _ = serve_in(&f.db, &f.scratch, &args);
    }
    assert!(!events(&f.db).iter().any(|e| e.actor == model::store::TEETH_PROBE_ACTOR));
}

#[test]
fn a_missing_store_is_an_error_and_is_never_created() {
    let f = fixture();
    let missing = f.scratch.join("no-such.db");
    let out = serve_in(&missing, &f.scratch, &["retest", "--id", "co-rule"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!missing.exists(), "retest never creates a store");
}
