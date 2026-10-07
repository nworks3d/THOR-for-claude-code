//! `serve retest --compare`, end to end through the real compiled binary: a
//! rule that already holds a verdict is tried again with this build, and the
//! verdicts that would change are printed - without writing anything.
//!
//! What it must never do: write without `--apply`, write a demotion (a rule
//! that bit and would now be a note) without `--allow-demote`, skip a rule
//! because it already has a verdict, or touch a rule that has none.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use model::store::{CurrentVerdict, RecordedVerdict, TrialCorpus, TrialDisputes, TrialFields};
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

/// A real checkout named `name` under `root`, with `README.md` committed. Its
/// hooks path is an empty folder, so no shared hook acts on it.
fn checkout(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let hooks = root.join("no-hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    git(&dir, &hooks, &["init", "-q", "-b", "main"]);
    git(&dir, &hooks, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
    git(&dir, &hooks, &["config", "user.email", "fixture@example.invalid"]);
    git(&dir, &hooks, &["config", "user.name", "fixture"]);
    std::fs::write(dir.join("README.md"), "x marks it\n").unwrap();
    git(&dir, &hooks, &["add", "README.md"]);
    git(&dir, &hooks, &["commit", "-q", "-m", "readme"]);
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

fn rule(id: &str, project: &str, text: &str, bindings: Vec<Binding>, check: Option<Check>) -> Item {
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
        check,
    }
}

fn command(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Command, value: value.to_string() }
}

/// A rule written straight into the log, the way an older store holds it: no
/// trial record, so no verdict yet.
fn legacy(store: &mut EventStore, item: &Item) {
    let body = serde_json::to_string(item).unwrap();
    store.append_event("legacy", &item.id, "migration", EventKind::FactCreated, &item.id, None, &body).unwrap();
}

/// A rule written into the log with a verdict an older build decided: its
/// trial record claims `verdict` and names `matcher_rev`.
fn decided_long_ago(store: &mut EventStore, item: &Item, verdict: RecordedVerdict, matcher_rev: &str) {
    let fields = TrialFields {
        v: model::store::TRIAL_RECORD_VERSION,
        fingerprint: model::store::fingerprint(item),
        root: None,
        project: model::normalize::normalize_project(item.project.as_deref()),
        head_commit: None,
        candidates: Vec::new(),
        attempts: Vec::new(),
        corpus: TrialCorpus { reached: 0, whole_hits: 0, cut: 0, stopped_forcing: Vec::new() },
        clean_failed: Vec::new(),
        disputes: TrialDisputes { legitimate_calls: Vec::new(), may_change: Vec::new() },
        accept_refusing: Vec::new(),
        history: Vec::new(),
        pending: None,
        a4: Vec::new(),
        verdict,
        evidence: Vec::new(),
        uncommitted: false,
        parent_rev: None,
        file_blob: None,
        a4_old_stock: None,
        unreached: None,
        matcher_rev: Some(matcher_rev.to_string()),
    };
    let body = model::store::canonical_body(item).unwrap();
    let body = format!("{},\"teeth_trial\":{}}}", &body[..body.len() - 1], serde_json::to_string(&fields).unwrap());
    store.append_event("legacy", &item.id, "migration", EventKind::FactCreated, &item.id, None, &body).unwrap();
}

/// The field case b-559c4de06b30d8d3: a ban on `git` whose built forms are
/// only partly refused. This build puts it on the note track.
fn partly_refused_ban(id: &str) -> Item {
    rule(
        id,
        "beta",
        "Never hard-reset the deploy checkouts alpha-dev and beta-prod. Refused: `alpha-dev reset`, `beta-prod reset`, `alpha-dev && git reset`, `beta-prod && git reset`.",
        vec![command("git")],
        Some(Check::Forbidden {
            literals: ["alpha-dev reset", "beta-prod reset", "alpha-dev && git reset", "beta-prod && git reset"].map(String::from).to_vec(),
        }),
    )
}

struct Fixture {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    scratch: PathBuf,
}

/// Store with: `ban`, decided long ago as a bite by "old-matcher" (this build
/// would make it a note); `fp`, a rule this build decided itself; `fresh`, a
/// rule with no verdict at all.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let beta = checkout(tmp.path(), "beta");
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let db = tmp.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    register(&mut store, "beta", &beta);
    decided_long_ago(&mut store, &partly_refused_ban("ban"), RecordedVerdict::RuleWired, "old-matcher");
    legacy(
        &mut store,
        &rule("fp", "beta", "Never run `git push --force` on main.", vec![command("git push")], Some(Check::Forbidden { literals: vec!["--force".to_string()] })),
    );
    legacy(
        &mut store,
        &rule("fresh", "beta", "Never run `git push --mirror` here.", vec![command("git push")], Some(Check::Forbidden { literals: vec!["--mirror".to_string()] })),
    );
    drop(store);
    let f = Fixture { _tmp: tmp, db, scratch };
    let out = serve(&f, &["retest", "--id", "fp", "--apply"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    f
}

fn serve(f: &Fixture, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_serve")).current_dir(&f.scratch).arg("--db").arg(&f.db).args(args).output().expect("serve runs")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn events(db: &Path) -> Vec<thor_core::event_store::Event> {
    EventStore::open_existing(db).unwrap().get_all_events().unwrap()
}

fn verdict(db: &Path, id: &str) -> CurrentVerdict {
    model::store::current_verdict(&EventStore::open_existing(db).unwrap(), id)
}

#[test]
fn compare_lists_a_demotion_loudly_and_writes_nothing() {
    let f = fixture();
    let before = events(&f.db).len();
    let out = serve(&f, &["retest", "--all", "--compare"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(5), "a verdict that would change was not written: {text}");
    assert!(text.contains("DEMOTION ban: rule track, wired -> note"), "{text}");
    assert!(text.contains("decided by matcher old-matcher; this build is matcher "), "{text}");
    assert!(text.contains("NOT WRITTEN: a demotion needs --compare --apply --allow-demote"), "{text}");
    assert!(text.contains("DEMOTIONS NOT WRITTEN (1): ban"), "{text}");
    assert!(text.contains("compared 2 rule(s) that hold a verdict"), "{text}");
    assert!(text.contains("0 promote, 1 DEMOTE, 0 this build cannot decide, 1 unchanged"), "{text}");
    assert!(text.contains("1 hold no verdict and were skipped"), "{text}");
    assert!(!text.contains("fp:") && !text.contains("fresh"), "only a changed verdict is printed: {text}");
    assert_eq!(events(&f.db).len(), before, "compare writes nothing");
    assert_eq!(verdict(&f.db, "ban"), CurrentVerdict::RuleWired);
}

#[test]
fn a_rule_with_a_current_verdict_is_tried_again_not_skipped() {
    let f = fixture();
    // The plain batch leaves every rule with a verdict alone; compare does not.
    let plain = stdout(&serve(&f, &["retest", "--all"]));
    assert!(plain.contains("2 already have one and were left as they are"), "{plain}");
    let compared = stdout(&serve(&f, &["retest", "--id", "fp", "--compare"]));
    assert!(compared.contains("compared 1 rule(s) that hold a verdict") && compared.contains("1 unchanged"), "{compared}");
}

#[test]
fn apply_alone_never_writes_a_demotion() {
    let f = fixture();
    let before = events(&f.db).len();
    let out = serve(&f, &["retest", "--all", "--compare", "--apply"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(5), "{text}");
    assert!(text.contains("DEMOTIONS NOT WRITTEN (1): ban"), "{text}");
    assert_eq!(events(&f.db).len(), before, "nothing written");
    assert_eq!(verdict(&f.db, "ban"), CurrentVerdict::RuleWired, "it keeps its tooth");
}

#[test]
fn allow_demote_writes_the_note_as_the_retest_actor() {
    let f = fixture();
    let before = events(&f.db).len();
    let out = serve(&f, &["retest", "--all", "--compare", "--apply", "--allow-demote"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    let after = events(&f.db);
    assert_eq!(after.len(), before + 1, "exactly one write: {text}");
    assert_eq!(after.last().unwrap().actor, "retest");
    assert!(text.contains(&format!("written: fact_revised at log seq {} by 'retest'", after.last().unwrap().seq)), "{text}");
    assert_eq!(verdict(&f.db, "ban"), CurrentVerdict::Note);
    // The verdict is this build's now: compare finds nothing left to change.
    let again = serve(&f, &["retest", "--all", "--compare"]);
    assert_eq!(again.status.code(), Some(0), "{}", stdout(&again));
}

#[test]
fn allow_demote_alone_is_a_usage_error() {
    let f = fixture();
    let before = events(&f.db).len();
    for args in [&["retest", "--all", "--allow-demote"][..], &["retest", "--all", "--compare", "--allow-demote"][..]] {
        let out = serve(&f, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {:?}", out);
    }
    assert_eq!(events(&f.db).len(), before);
}

/// A note that keeps a pending check, whose check now bites: this build
/// finds it a bite. `--compare` lists it; `--apply` writes it (a promotion
/// needs no `--allow-demote`).
#[test]
fn compare_promotes_a_note_whose_check_now_bites() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("thor.db");
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let mut store = EventStore::new(&db).unwrap();
    let item = rule(
        "g-rule",
        "gamma",
        "Keep `x marks it` in README.md.",
        vec![Binding::Target { kind: TargetKind::Path, value: "README.md".to_string() }],
        Some(Check::Contains { path: "README.md".to_string(), literal: "x marks it".to_string() }),
    );
    // Written from a session in another project, before gamma has a working
    // copy: a note, the check pending.
    let corpus = serve::legit::Corpus::load(&store);
    let session = serve::teeth::SessionCtx { own_checkout: None, own_project: Some("alpha".to_string()), session_id: Some("s1".to_string()) };
    let tried = {
        let env = serve::teeth::Env { store: &store, db: Some(&db) };
        let root = serve::project::registered_checkout(&store, item.project.as_deref());
        let write = serve::teeth::Write { item: &item, before: None, because: None, parent_rev: None };
        serve::teeth::trial(&env, &write, &root, &session, &corpus, &serve::teeth::Disputes::default(), &[])
    };
    let stored = serve::teeth::stored_item(&item, &tried).unwrap();
    model::store::declare_in_scoped(&mut store, "s", "l", "a", &stored, None, None, tried.record.as_ref(), None).unwrap();
    assert_eq!(model::store::current_verdict(&store, "g-rule"), CurrentVerdict::Note);
    let gamma = checkout(tmp.path(), "gamma");
    register(&mut store, "gamma", &gamma);
    drop(store);

    let run = |args: &[&str]| Command::new(env!("CARGO_BIN_EXE_serve")).current_dir(&scratch).arg("--db").arg(&db).args(args).output().unwrap();
    let before = events(&db).len();
    let out = run(&["retest", "--all", "--compare"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(5), "{text}");
    assert!(text.contains("PROMOTE g-rule: note -> rule track"), "{text}");
    assert!(text.contains("1 promote, 0 DEMOTE"), "{text}");
    assert_eq!(events(&db).len(), before, "compare writes nothing");

    let out = run(&["retest", "--all", "--compare", "--apply"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("written: fact_revised"), "{text}");
    assert_eq!(events(&db).len(), before + 1);
    assert!(matches!(verdict(&db, "g-rule"), CurrentVerdict::RuleWired | CurrentVerdict::RuleProven));
}
