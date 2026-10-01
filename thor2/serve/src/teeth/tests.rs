//! Units of `serve::teeth` - one per outcome of `decide`, plus the red and
//! green probes of step 7b of the build (R7b.x / G7b.x in the names).

#![cfg(test)]

use super::*;
use model::item::{Kind, Severity};
use std::fs;

const S1: &str = "0de7dc52-1b97-4b66-948f-61e4438043b2";
const S2: &str = "373e8610-8130-490d-ac29-56aa98709fd1";

/// A reason tag as rules written before step 8 carry it, naming what the
/// rule is about. Since step 8 it answers nothing.
fn no_literal(subject: &str) -> String {
    format!("no-literal:a change to {subject} leaves no fixed text in any call")
}

/// A store on disk (so `command_root` never falls back to the temp folder
/// for a registered project) and the folder its checkouts live in.
struct Fx {
    dir: tempfile::TempDir,
    db: PathBuf,
    store: EventStore,
}

impl Fx {
    fn new() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let store = EventStore::new(&db).unwrap();
        Fx { dir, db, store }
    }

    fn base(&self) -> &Path {
        self.dir.path()
    }

    /// A git checkout of project `key` under the fixture folder, holding
    /// `files`, committed when `commit`; its git hooks point at an empty
    /// folder, so no shared hook ever acts on it (builder rule 11).
    fn checkout(&self, key: &str, files: &[(&str, &str)], commit: bool) -> PathBuf {
        let root = self.base().join(key);
        fs::create_dir_all(&root).unwrap();
        let hooks = self.base().join("no-hooks");
        fs::create_dir_all(&hooks).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "core.hooksPath", &hooks.to_string_lossy()]);
        git(&root, &["config", "user.email", "fixture@example.invalid"]);
        git(&root, &["config", "user.name", "fixture"]);
        fs::write(root.join(crate::project::MARKER_FILE_NAME), format!("{key}\n")).unwrap();
        for (name, content) in files {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        if commit {
            git(&root, &["add", "-A"]);
            git(&root, &["commit", "-q", "-m", "fixture"]);
        }
        crate::project::checkout_top(&root)
    }

    /// Register `root` as `key`'s working copy, raw, the way the installer
    /// writes it (a test holds no `SystemActor`).
    fn register(&mut self, key: &str, root: &Path) {
        let item = Item {
            id: model::store::project_opened_id(key),
            kind: Kind::Report,
            text: format!("Project {key}: a fixture record."),
            bindings: vec![],
            severity: None,
            project: Some(key.to_string()),
            tags: vec!["project-record".to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        let body = model::store::canonical_body(&item).unwrap();
        let body = format!(
            "{},\"root\":{},\"root_at\":\"2026-09-28T10:00:00Z\"}}",
            &body[..body.len() - 1],
            serde_json::to_string(&root.to_string_lossy()).unwrap()
        );
        self.store.append_event("install", "install", "installer", EventKind::FactCreated, &item.id, None, &body).unwrap();
    }

    fn served(&mut self, session: &str, trigger: &str) {
        crate::deliver::record_delivery_with_trigger(&mut self.store, session, session, "hook", "2026-09-28T00:00:00Z", &["any".to_string()], Some(trigger));
    }

    /// A write, raw, the way an older store holds it (no gate).
    fn legacy(&mut self, item: &Item) -> Event {
        let body = model::store::canonical_body(item).unwrap();
        let kind = if self.store.get_events_by_entity(&item.id).unwrap().is_empty() {
            EventKind::FactCreated
        } else {
            EventKind::FactRevised
        };
        if kind == EventKind::FactCreated {
            self.store.append_event("legacy", &item.id, "migration", kind, &item.id, None, &body).unwrap()
        } else {
            self.store.append_mutate_checked("legacy", &item.id, "migration", kind, &item.id, None, &body).unwrap()
        }
    }

    fn run(&self, item: &Item, before: Option<&Item>, session: &SessionCtx, disputes: &Disputes) -> Trial {
        self.run_because(item, before, None, session, disputes)
    }

    fn run_because(&self, item: &Item, before: Option<&Item>, because: Option<&str>, session: &SessionCtx, disputes: &Disputes) -> Trial {
        let root = crate::project::registered_checkout(&self.store, item.project.as_deref());
        let env = Env { store: &self.store, db: Some(&self.db) };
        let corpus = Corpus::load(&self.store);
        let hist = history_for_write(&self.store, before, item, because);
        let write = Write { item, before, because, parent_rev: None };
        trial(&env, &write, &root, session, &corpus, disputes, &hist)
    }

    fn write(&self, item: &Item) -> Trial {
        self.run(item, None, &SessionCtx::default(), &Disputes::default())
    }

    fn retest(&self, id: &str) -> Retest {
        retest(&self.store, Some(&self.db), id, &Corpus::load(&self.store)).unwrap()
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn cmd(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Command, value: value.to_string() }
}

fn path(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Path, value: value.to_string() }
}

fn rule(id: &str, text: &str, bindings: Vec<Binding>, check: Option<Check>, project: Option<&str>) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings,
        severity: Some(Severity::Irreversible),
        project: project.map(str::to_string),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("It turns out to be harmless.".to_string()),
        check,
    }
}

fn forbidden(literals: &[&str]) -> Option<Check> {
    Some(Check::Forbidden { literals: literals.iter().map(|l| l.to_string()).collect() })
}

fn contains(p: &str, literal: &str) -> Option<Check> {
    Some(Check::Contains { path: p.to_string(), literal: literal.to_string() })
}

fn receipt(t: &Trial) -> String {
    t.receipt.join("\n")
}

fn refusal(t: &Trial) -> (RefuseCode, String) {
    match &t.outcome {
        Outcome::Refuse { code, message } => (*code, message.clone()),
        other => panic!("expected a refusal, got {other:?}\n{}", receipt(t)),
    }
}

fn session_in(project: &str, checkout: Option<&Path>, id: &str) -> SessionCtx {
    SessionCtx {
        own_checkout: checkout.map(Path::to_path_buf),
        own_project: Some(project.to_string()),
        session_id: Some(id.to_string()),
    }
}

// ------------------------------------------------------ (1) untryable, K20

/// R7b.12 (F6, K20): a contains check for project gamma, which has no
/// registered working copy, written from inside gamma: refused, naming the
/// one command that registers it, with `--db`.
#[test]
fn r7b_12_an_unregistered_project_refuses_a_file_check_from_inside_it() {
    let fx = Fx::new();
    let gamma = fx.checkout("gamma", &[("README.md", "x marks it\n")], true);
    let item = rule("g-rule", "Keep `x marks it` in README.md.", vec![path("README.md")], contains("README.md", "x marks it"), Some("gamma"));
    let t = fx.run(&item, None, &session_in("gamma", Some(&gamma), S1), &Disputes::default());
    let (code, message) = refusal(&t);
    assert_eq!(code, RefuseCode::Untryable);
    assert!(message.contains("THOR cannot try this file check: 'gamma' has no registered working copy"), "{message}");
    assert!(message.contains("--register-root --db") && message.contains(&fx.db.display().to_string()), "{message}");
    assert!(message.contains("then write this again"), "{message}");
    assert!(t.record.is_none() && t.check.is_none());
}

/// G7b.5 (K20): the same rule written from a session in alpha is a note
/// that keeps its check PENDING; once gamma is registered and its file holds
/// the literal, a retest makes it a rule and `apply` hangs the author's own
/// check.
#[test]
fn g7b_5_a_pending_check_is_hung_once_its_project_is_registered() {
    let mut fx = Fx::new();
    let item = rule("g-rule", "Keep `x marks it` in README.md.", vec![path("README.md")], contains("README.md", "x marks it"), Some("gamma"));
    let t = fx.run(&item, None, &session_in("alpha", None, S1), &Disputes::default());
    match &t.outcome {
        Outcome::Note { pending: Some(p), by_dispute: false } => {
            assert_eq!(p.why, PendingWhy::NoWorkingCopy);
            assert_eq!(Some(&p.check), item.check.as_ref());
        }
        other => panic!("{other:?}"),
    }
    assert!(t.check.is_none(), "a note carries no check");
    assert!(receipt(&t).contains("could not be tried here because 'gamma' has no registered working copy"), "{}", receipt(&t));
    let stored = stored_item(&item, &t).unwrap();
    assert_eq!(stored.tags, vec![model::store::TEETH_NOTE_TAG.to_string()]);
    model::store::declare_in_scoped(&mut fx.store, "s", "l", "a", &stored, None, None, t.record.as_ref(), None).unwrap();
    assert_eq!(model::store::current_verdict(&fx.store, "g-rule"), model::store::CurrentVerdict::Note);

    // Still unregistered: the retest says blocked, and there is nothing to write.
    let r = fx.retest("g-rule");
    assert!(matches!(r.outcome, Maintenance::Blocked(Blocker::NoWorkingCopy { .. })), "{:?}", r.outcome);
    assert!(r.write.is_none());

    let gamma = fx.checkout("gamma", &[("README.md", "x marks it\n")], true);
    fx.register("gamma", &gamma);
    let r = fx.retest("g-rule");
    assert!(matches!(&r.outcome, Maintenance::Rule { check, .. } if Some(check) == item.check.as_ref()), "{:?}\n{:?}", r.outcome, r.lines);
    apply(&mut fx.store, &r, retest_actor()).unwrap();
    let now = model::store::show(&fx.store, "g-rule").unwrap();
    assert_eq!(now.check, item.check, "the machine hangs the author's own check");
    assert!(matches!(
        model::store::current_verdict(&fx.store, "g-rule"),
        model::store::CurrentVerdict::RuleProven | model::store::CurrentVerdict::RuleWired
    ));
}

/// Outcome (1), the other two fixes: a global rule's file check, and a
/// malformed dispute.
#[test]
fn a_global_file_check_and_an_empty_dispute_are_refused_naming_the_fix() {
    let fx = Fx::new();
    let item = rule("glob", "Keep `x` in README.md.", vec![path("README.md")], contains("README.md", "x"), None);
    let (code, message) = refusal(&fx.write(&item));
    assert_eq!(code, RefuseCode::Untryable);
    assert!(message.contains("give it the project whose checkout holds README.md"), "{message}");

    let item = rule("d", "Never run `git push --force`.", vec![cmd("git push")], None, None);
    let disputes = Disputes { legitimate_calls: vec!["  ".to_string()], ..Default::default() };
    let (code, message) = refusal(&fx.run(&item, None, &SessionCtx::default(), &disputes));
    assert_eq!(code, RefuseCode::Untryable);
    assert!(message.contains("one entry is empty") && message.contains("then write this again"), "{message}");
}

// ------------------------------------------------------------------ (2) A4

/// Outcome (2): a check whose literal the text does not name is refused
/// when the write attaches it (the measured 2026-09-26 leak).
#[test]
fn a_check_whose_literal_the_text_never_names_is_refused() {
    let fx = Fx::new();
    let item = rule("disk", "Never repartition the data disk.", vec![cmd("diskpart")], forbidden(&["zqxj-never-typed"]), None);
    let (code, message) = refusal(&fx.write(&item));
    assert_eq!(code, RefuseCode::A4);
    assert!(message.contains("'zqxj-never-typed' is not named anywhere in the rule's own text"), "{message}");
    assert!(message.contains("or drop the check"), "{message}");
}

/// R7b.14 (F12, K23): an unpin of a rule whose forbidden literal was never
/// named lands; its verdict reads Undecided("A4: ...") and the receipt names
/// the fix. A later write that names it gives the rule its verdict.
#[test]
fn r7b_14_an_unpin_with_an_old_a4_failure_lands_undecided() {
    let mut fx = Fx::new();
    let pinned = rule("old", "Never force-push to main.", vec![cmd("git push"), Binding::Always], forbidden(&["--force"]), None);
    fx.legacy(&pinned);
    let mut unpinned = pinned.clone();
    unpinned.bindings = vec![cmd("git push")];
    let t = fx.run(&unpinned, Some(&pinned), &SessionCtx::default(), &Disputes::default());
    match &t.outcome {
        Outcome::Undecided { why } => assert!(why.starts_with("A4: '--force'") && why.contains("revise the text to name it"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(t.check, unpinned.check, "the check stays");
    let stored = stored_item(&unpinned, &t).unwrap();
    let because = Some("no longer needed at every session start");
    model::store::revise_tried_scoped(&mut fx.store, "s", "l", "a", &pinned, &stored, &pinned, because, None, t.record.as_ref(), None, None)
        .unwrap();
    match model::store::current_verdict(&fx.store, "old") {
        model::store::CurrentVerdict::Undecided { why } => assert!(why.starts_with("A4: '--force'"), "{why}"),
        other => panic!("{other:?}"),
    }

    let mut named = stored.clone();
    named.text = "Never run `git push --force` on main.".to_string();
    let t = fx.run(&named, Some(&stored), &SessionCtx::default(), &Disputes::default());
    assert!(matches!(t.outcome, Outcome::Rule { .. }), "{:?}\n{}", t.outcome, receipt(&t));
}

// ------------------------------------------------------ (3) false right now

/// Outcome (3): a contains check whose literal is not in the registered
/// checkout is refused, naming the root and what is missing.
#[test]
fn a_file_check_that_is_false_now_is_refused() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "intro only\n")], true);
    fx.register("beta", &beta);
    let item = rule("co", "NOTES.md keeps `keep me`.", vec![path("NOTES.md")], contains("NOTES.md", "keep me"), Some("beta"));
    let (code, message) = refusal(&fx.write(&item));
    assert_eq!(code, RefuseCode::FalseNow);
    assert!(message.contains("your check is false in") && message.contains("NOTES.md does not contain 'keep me'"), "{message}");
}

/// R7b.13 (F11, K20): the literal is only in the session's own linked
/// worktree of beta, not in beta's registered checkout: a note with the
/// check pending, never a rule (hanging it would refuse every edit of that
/// file in the main checkout).
#[test]
fn r7b_13_true_only_in_the_sessions_worktree_is_a_pending_note() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "intro only\n")], true);
    fx.register("beta", &beta);
    let wt = fx.base().join("beta-wt");
    git(&beta, &["worktree", "add", "-q", "-b", "wt", &wt.to_string_lossy()]);
    fs::write(wt.join("NOTES.md"), "intro only\nkeep me\n").unwrap();
    let item = rule("co", "NOTES.md keeps `keep me`.", vec![path("NOTES.md")], contains("NOTES.md", "keep me"), Some("beta"));
    let t = fx.run(&item, None, &session_in("beta", Some(&wt), S1), &Disputes::default());
    match &t.outcome {
        Outcome::Note { pending: Some(p), .. } => assert_eq!(p.why, PendingWhy::NotTrueInRegisteredCheckout),
        other => panic!("{other:?}\n{}", receipt(&t)),
    }
    assert!(t.check.is_none());
}

// ----------------------------------------------- (4) a whole-command ban, K19

/// R7b.7 (FB 1, K19): anchor 'sshpass' with the literal 'sshpass' is a
/// whole-command ban: a rule, wired - not a refusal, not a note. The same
/// for 'git push' / 'git push'.
#[test]
fn r7b_7_a_collapsed_check_is_a_whole_command_ban() {
    let fx = Fx::new();
    let item = rule("sp", "Never run `sshpass`; use keys.", vec![cmd("sshpass")], forbidden(&["sshpass"]), None);
    let t = fx.write(&item);
    assert_eq!(t.outcome, Outcome::Rule { proven: false }, "{}", receipt(&t));
    assert_eq!(t.check, item.check);
    assert!(receipt(&t).contains("whole-command ban: refuses 'sshpass x' (refused), lets 'echo sshpass' through"), "{}", receipt(&t));

    let item = rule("gp", "Never `git push` this repository until the owner says go.", vec![cmd("git push")], forbidden(&["git push"]), None);
    let t = fx.write(&item);
    assert_eq!(t.outcome, Outcome::Rule { proven: false }, "{}", receipt(&t));
    assert!(receipt(&t).contains("lets 'echo git push' and 'git --version' through"), "{}", receipt(&t));
}

/// K19's advice: 'git push --force' with the literal '--force' prints the
/// cleaner anchor THOR tried and found biting.
#[test]
fn a_whole_command_ban_prints_the_cleaner_anchor_as_advice() {
    let fx = Fx::new();
    let item = rule("fp", "Never run `git push --force` on main.", vec![cmd("git push --force")], forbidden(&["--force"]), None);
    let t = fx.write(&item);
    assert_eq!(t.outcome, Outcome::Rule { proven: false }, "{}", receipt(&t));
    let r = receipt(&t);
    assert!(r.contains("refuses 'git push --force x' (refused), lets 'echo git push --force', 'git --version' and 'git push' through"), "{r}");
    assert!(r.contains("Advice: bind to 'git push' with the literal '--force' - THOR tried exactly that and it bites"), "{r}");
}

// ------------------------------------------------------------ (5) it bites

/// G7b.1 (K18): NOTES.md = 'keep me\nsecond line keep me\nplain line',
/// committed, rule bound to NOTES.md: proven, and the receipt names every
/// form - the minus-a-line form removes line 3, the one unrelated line.
#[test]
fn g7b_1_a_committed_contains_check_is_a_proven_rule() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "keep me\nsecond line keep me\nplain line")], true);
    fx.register("beta", &beta);
    let item = rule("co", "NOTES.md keeps `keep me`.", vec![path("NOTES.md")], contains("NOTES.md", "keep me"), Some("beta"));
    let t = fx.write(&item);
    assert_eq!(t.outcome, Outcome::Rule { proven: true }, "{}", receipt(&t));
    let r = receipt(&t);
    assert!(r.starts_with("teeth: proven guard - NOTES.md in the working copy registered for beta"), "{r}");
    assert!(
        r.contains(
            "holds it; tried an Edit removing 'keep me' (refused), the file as it stands (let through), an Edit keeping \
             'keep me' (let through), the file without line 3 (let through)"
        ),
        "{r}"
    );
    let f = t.record.as_ref().unwrap().fields();
    assert!(matches!(f.evidence.as_slice(), [model::store::TrialEvidence::InHead { .. }]), "{:?}", f.evidence);
    assert!(!f.uncommitted && f.file_blob.is_some() && f.root.is_some() && f.head_commit.is_some());
}

/// R7b.3 (Z2 b): the literal twice in the file - the forbidden attempt
/// removes both and is refused (a first-occurrence-only edit leaves one and
/// would judge a working tooth toothless).
#[test]
fn r7b_3_the_forbidden_edit_removes_every_occurrence() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "keep me\nkeep me\nplain line\n")], true);
    fx.register("beta", &beta);
    let item = rule("co", "NOTES.md keeps `keep me`.", vec![path("NOTES.md")], contains("NOTES.md", "keep me"), Some("beta"));
    let t = fx.write(&item);
    assert!(matches!(t.outcome, Outcome::Rule { .. }), "{}", receipt(&t));
    let attempts = &t.record.as_ref().unwrap().fields().attempts;
    let removing = attempts.iter().find(|a| a.form.contains("an Edit removing 'keep me'")).unwrap();
    assert_eq!(removing.outcome, AttemptOutcome::Refused);
}

/// R7b.4 and G7b.2 (K9): the literal only in the working tree - a rule,
/// wired, `uncommitted`, never proven; after the commit, proven.
#[test]
fn r7b_4_and_g7b_2_an_uncommitted_literal_is_wired_until_committed() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "intro only\n")], true);
    fx.register("beta", &beta);
    fs::write(beta.join("NOTES.md"), "intro only\nkeep me\n").unwrap();
    let item = rule("co", "NOTES.md keeps `keep me`.", vec![path("NOTES.md")], contains("NOTES.md", "keep me"), Some("beta"));
    let t = fx.write(&item);
    assert_eq!(t.outcome, Outcome::Rule { proven: false }, "{}", receipt(&t));
    assert!(t.record.as_ref().unwrap().fields().uncommitted);
    git(&beta, &["commit", "-q", "-am", "keep me"]);
    let t = fx.write(&item);
    assert_eq!(t.outcome, Outcome::Rule { proven: true }, "{}", receipt(&t));
    assert!(!t.record.as_ref().unwrap().fields().uncommitted);
}

/// R7b.10 (F17): a literal spanning lines 1 and 2 of a four-line file: the
/// minus-a-line clean form removes line 3, never 1 or 2, and the verdict is
/// a rule.
#[test]
fn r7b_10_a_multi_line_literal_keeps_every_line_it_spans() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "line one\nline two\nline three\nline four\n")], true);
    fx.register("beta", &beta);
    let item = rule("ml", "NOTES.md keeps 'line one\nline two' together.", vec![path("NOTES.md")], contains("NOTES.md", "line one\nline two"), Some("beta"));
    let t = fx.write(&item);
    assert!(matches!(t.outcome, Outcome::Rule { .. }), "{:?}\n{}", t.outcome, receipt(&t));
    assert!(receipt(&t).contains("the file without line 3 (let through)"), "{}", receipt(&t));
    assert_eq!(minus_unrelated_line("line one\nline two\nline three\nline four\n", "line one\nline two").map(|x| x.1), Some(3));
    assert_eq!(minus_unrelated_line("a keep\nb keep\n", "keep"), None, "every line overlaps: no such form");
}

/// R7b.11 (FB 5, K8(d)): a ban on 'git grep' that refuses all six real calls
/// it reaches is refused, listing them and both ways on; naming one of them
/// in accept_refusing keeps it a rule; a made-up entry is refused.
#[test]
fn r7b_11_a_check_refusing_most_real_calls_needs_the_agent() {
    let mut fx = Fx::new();
    for n in 0..6 {
        fx.served(S2, &format!("git grep -n pattern-{n} src"));
    }
    let item = rule("gg", "Never run `git grep` here; use rg.", vec![cmd("git grep")], forbidden(&["git grep"]), None);
    let (code, message) = refusal(&fx.write(&item));
    assert_eq!(code, RefuseCode::Majority);
    assert!(message.contains("would refuse 6 of the 6 real calls"), "{message}");
    for n in 0..6 {
        assert!(message.contains(&format!("git grep -n pattern-{n} src")), "{message}");
    }
    assert!(message.contains("narrow the check") && message.contains("accept_refusing"), "{message}");

    let accept = Disputes { accept_refusing: vec!["git grep -n pattern-3 src".to_string()], ..Default::default() };
    let t = fx.run(&item, None, &SessionCtx::default(), &accept);
    assert!(matches!(t.outcome, Outcome::Rule { .. }), "{:?}", t.outcome);
    assert_eq!(t.record.as_ref().unwrap().fields().accept_refusing, vec!["git grep -n pattern-3 src".to_string()]);

    let made_up = Disputes { accept_refusing: vec!["git grep made-up".to_string()], ..Default::default() };
    let (code, message) = refusal(&fx.run(&item, None, &SessionCtx::default(), &made_up));
    assert_eq!(code, RefuseCode::Majority);
    assert!(message.contains("'git grep made-up', which is not one of the listed calls"), "{message}");
}

// ------------------------------------------------ (6) it does not bite

/// R7b.1 (Z2 a): a contains check bound Always never reaches a write: a
/// note, and the receipt says to bind it to the file.
#[test]
fn r7b_1_a_contains_check_bound_always_is_a_note() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "keep me\n")], true);
    fx.register("beta", &beta);
    let item = rule("co", "NOTES.md keeps keep me, always.", vec![Binding::Always], contains("NOTES.md", "keep me"), Some("beta"));
    let t = fx.write(&item);
    assert_eq!(t.outcome, Outcome::Note { pending: None, by_dispute: false }, "{}", receipt(&t));
    assert!(t.check.is_none());
    assert!(
        receipt(&t).contains("a contains check only reaches a write when the rule is anchored to that file or its directory - bind it to NOTES.md"),
        "{}",
        receipt(&t)
    );
}

/// R7b.2 (PR, at a retest): a contains check bound to a command never reaches
/// a write to NOTES.md: the retest needs the agent and names the binding -
/// never a quiet note.
#[test]
fn r7b_2_a_retest_names_the_binding_a_file_check_lacks() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "keep this line\n")], true);
    fx.register("beta", &beta);
    let item = rule("c-cmd", "NOTES.md keeps `keep this line`.", vec![cmd("make release")], contains("NOTES.md", "keep this line"), Some("beta"));
    fx.legacy(&item);
    let r = fx.retest("c-cmd");
    match &r.outcome {
        Maintenance::NeedsAgent(why) => {
            assert!(why.contains("an Edit removing 'keep this line' -> let through"), "{why}");
            assert!(why.contains("this rule's bindings never reach a write to NOTES.md") && why.contains("bind it to path:NOTES.md"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    assert!(r.write.is_none());
}

/// K8(b): a requires check that refuses the listing forms of its anchor
/// (`git tag`, `git tag -l`) is not stored; whole hits in the log are
/// listed and never drop a check; a majority asks and never demotes
/// (R7b.11 above).
#[test]
fn listing_forms_drop_a_check_and_whole_hits_never_do() {
    let mut fx = Fx::new();
    let item = rule(
        "tag",
        "Tag every release annotated with -a.",
        vec![cmd("git tag")],
        Some(Check::Requires { when: "git tag".to_string(), required: vec!["-a".to_string()] }),
        None,
    );
    let t = fx.write(&item);
    assert!(matches!(t.outcome, Outcome::Note { .. }), "{:?}\n{}", t.outcome, receipt(&t));
    let r = receipt(&t);
    assert!(r.contains("your check (requires '-a' after 'git tag' on 'git tag') was not stored") && r.contains("'git tag -l'"), "{r}");
    assert!(t.record.as_ref().unwrap().fields().clean_failed.contains(&"git tag --list".to_string()));

    fx.served(S2, "git push --force origin main");
    fx.served(S2, "git push origin main");
    let push = rule("fp", "Never run `git push --force` on main.", vec![cmd("git push")], forbidden(&["--force"]), None);
    let t = fx.write(&push);
    assert!(matches!(t.outcome, Outcome::Rule { .. }), "a whole hit never drops a check: {}", receipt(&t));
    assert!(receipt(&t).contains("would refuse 1, e.g. 'git push --force origin main'"), "{}", receipt(&t));
}

// ------------------------------------------------------ (7) nothing at all

/// Outcome (7): no check and nothing in the text to build one from - a
/// note, and the record says derivation ran.
#[test]
fn a_checkless_rule_with_nothing_to_build_from_is_a_note_after_derive_ran() {
    let fx = Fx::new();
    let item = rule("subj", "Keep commit subjects under 60 characters.", vec![Binding::Moment(intent::Action::Commit)], None, None);
    let t = fx.write(&item);
    assert_eq!(t.outcome, Outcome::Note { pending: None, by_dispute: false });
    assert!(receipt(&t).contains("nothing in the text or anchors to build a tooth from"), "{}", receipt(&t));
    let attempts = &t.record.as_ref().unwrap().fields().attempts;
    assert!(attempts.iter().any(|a| a.via == "derive"), "the record must say derive ran: {attempts:?}");
}

// -------------------------------------------- (8) a derived tooth forces

/// R7b.6 (A5): the force-push text forces (both exits named); bound only to
/// a moment, it proposes the binding; `list-heads` forces; a proposed
/// binding at a full place never forces; a prose flag outside the 3-token
/// window derives nothing; a span present in the anchored file derives a
/// forcing contains whose refusal offers may_change.
#[test]
fn r7b_6_derived_teeth_force_and_name_both_exits() {
    let mut fx = Fx::new();
    let item = rule("fp", "Never run `git push --force` on main.", vec![cmd("git push")], None, None);
    let t = fx.write(&item);
    let (code, message) = refusal(&t);
    assert_eq!(code, RefuseCode::Forcing);
    assert!(message.contains("forbidden '--force' on 'git push' refuses 'git push --force'"), "{message}");
    assert!(message.contains("check_kind forbidden, check_literals ['--force']"), "{message}");
    assert!(message.contains("legitimate_calls: ['git push --force <the rest of the real call>']"), "{message}");
    assert!(message.contains("Both polarities bite"), "a forbidden and a requires on one fragment: {message}");
    assert!(!t.forcing.is_empty());

    let moment = rule("fpm", "Never run `git push --force` on main.", vec![Binding::Moment(intent::Action::Push)], None, None);
    let (_, message) = refusal(&fx.write(&moment));
    assert!(message.contains("bind it to the command 'git push'"), "{message}");

    let heads = rule("lh", "Never run `git bundle list-heads` on a backup.", vec![cmd("git bundle")], None, None);
    let (_, message) = refusal(&fx.write(&heads));
    assert!(message.contains("forbidden 'list-heads' on 'git bundle'"), "{message}");

    let prose = rule("far", "Run cscript for the build, and pass the -Encoding option only when the log says so.", vec![cmd("cscript")], None, None);
    let t = fx.write(&prose);
    assert!(matches!(t.outcome, Outcome::Note { .. }), "a flag outside the window derives nothing: {:?}", t.outcome);

    // The proposed binding 'git push' is full: four live rivals hold it.
    for n in 0..4 {
        fx.legacy(&rule(&format!("rival-{n}"), &format!("Rival {n} on push."), vec![cmd("git push")], None, None));
    }
    let t = fx.write(&moment);
    assert!(matches!(t.outcome, Outcome::Note { .. }), "a full proposed binding never forces: {:?}\n{}", t.outcome, receipt(&t));
    assert!(receipt(&t).contains("does not force"), "{}", receipt(&t));

    let beta = fx.checkout("beta", &[("package.json", "{\"name\":\"demo\",\"version\":\"1.0.0\"}\n")], true);
    fx.register("beta", &beta);
    let span = rule("pk", "Bump the 'version' in package.json with every release.", vec![path("package.json")], None, Some("beta"));
    let (code, message) = refusal(&fx.write(&span));
    assert_eq!(code, RefuseCode::Forcing);
    assert!(message.contains("contains 'version' in package.json") && message.contains("may_change: ['version']"), "{message}");
}

/// G7b.3 (A5): '-sb' does not force when real calls of ANOTHER session,
/// made before any refusal, carry it (and one without it); a valid dispute
/// on `list-heads` gives a note by dispute, and a dispute naming THOR's own
/// forbidden form is refused.
#[test]
fn g7b_3_real_calls_and_disputes_stop_a_derived_tooth() {
    let mut fx = Fx::new();
    fx.served(S2, "git status -sb");
    fx.served(S2, "git status --short");
    let item = rule("st", "Check with `git status -sb` first.", vec![cmd("git status")], None, None);
    let t = fx.run(&item, None, &session_in("alpha", None, S1), &Disputes::default());
    assert!(matches!(t.outcome, Outcome::Note { by_dispute: false, .. }), "{:?}\n{}", t.outcome, receipt(&t));
    let r = receipt(&t);
    assert!(r.contains("does not force: it would refuse the real call 'git status -sb' (session 373e8610"), "{r}");
    let stopped = &t.record.as_ref().unwrap().fields().corpus.stopped_forcing;
    assert!(stopped.iter().any(|s| s.session == S2 && s.call == "git status -sb"), "{stopped:?}");

    let heads = rule("lh", "Never run `git bundle list-heads` on a backup.", vec![cmd("git bundle")], None, None);
    let dispute = Disputes { legitimate_calls: vec!["git bundle list-heads backup.bundle".to_string()], ..Default::default() };
    let t = fx.run(&heads, None, &SessionCtx::default(), &dispute);
    assert_eq!(t.outcome, Outcome::Note { pending: None, by_dispute: true }, "{}", receipt(&t));
    assert!(receipt(&t).starts_with("teeth: note by dispute"), "{}", receipt(&t));
    assert_eq!(t.record.as_ref().unwrap().fields().disputes.legitimate_calls, dispute.legitimate_calls);

    let own = Disputes { legitimate_calls: vec!["git bundle list-heads".to_string()], ..Default::default() };
    let (code, message) = refusal(&fx.run(&heads, None, &SessionCtx::default(), &own));
    assert_eq!(code, RefuseCode::Forcing);
    assert!(message.contains("is the forbidden form THOR built itself, not a legitimate call"), "{message}");
}

/// G7b.4 (K22): a derived contains on package.json 'version' with
/// may_change ['version'] is a note by dispute, and the record holds it.
#[test]
fn g7b_4_may_change_turns_a_derived_contains_into_a_note_by_dispute() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("package.json", "{\"name\":\"demo\",\"version\":\"1.0.0\"}\n")], true);
    fx.register("beta", &beta);
    let item = rule("pk", "Bump the 'version' in package.json with every release.", vec![path("package.json")], None, Some("beta"));
    let disputes = Disputes { may_change: vec!["version".to_string()], ..Default::default() };
    let t = fx.run(&item, None, &SessionCtx::default(), &disputes);
    assert_eq!(t.outcome, Outcome::Note { pending: None, by_dispute: true }, "{}", receipt(&t));
    assert_eq!(t.record.as_ref().unwrap().fields().disputes.may_change, vec!["version".to_string()]);
}

/// FIELD TEST 2, defect 2 (2026-10-01): the rule RECOMMENDS a flag ("use git
/// grep --no-index there"), THOR built a forbidden tooth on that very flag,
/// and the only exit it offered was a legitimate call carrying the flag -
/// which did not exist, so the agent invented one. A literal the text
/// recommends is never forbidden; the other tooth (requires) is still tried,
/// and real calls without the flag stop it, so the rule is a note.
#[test]
fn a_flag_the_text_recommends_is_never_forbidden() {
    let mut fx = Fx::new();
    fx.served(S2, "git grep -n \"TODO\" -- src");
    fx.served(S2, "git grep -n \"FIXME\" HEAD -- lib");
    for text in [
        "git grep in a folder without its own .git searches the parent repository silently. Use git grep --no-index there instead.",
        "git grep in een map zonder eigen .git zoekt stil in de bovenliggende repo. Gebruik daar git grep --no-index.",
        "In a folder without its own .git, always run `git grep --no-index` there.",
    ] {
        let item = rule("gg", text, vec![cmd("git grep")], None, None);
        let t = fx.run(&item, None, &session_in("alpha", None, S1), &Disputes::default());
        assert!(matches!(t.outcome, Outcome::Note { .. }), "{text}\n{:?}\n{}", t.outcome, receipt(&t));
        assert!(!receipt(&t).contains("forbidden '--no-index'"), "a recommended flag is never forbidden: {}", receipt(&t));
    }
    // The prohibition the same words can carry still derives its tooth.
    let never = rule("gn", "Never run `git grep --no-index` inside a real repository.", vec![cmd("git grep")], None, None);
    let (code, message) = refusal(&fx.run(&never, None, &session_in("alpha", None, S1), &Disputes::default()));
    assert_eq!(code, RefuseCode::Forcing);
    assert!(message.contains("forbidden '--no-index' on 'git grep'"), "{message}");
}

/// R7b.8 (D6): the writing session ran 'git push --force --dry-run' itself:
/// the derived tooth still forces, and the receipt says that call does not
/// count.
#[test]
fn r7b_8_a_call_by_the_writing_session_never_stops_forcing() {
    let mut fx = Fx::new();
    fx.served(S1, "git push --force --dry-run");
    let item = rule("fp", "Never run `git push --force` on main.", vec![cmd("git push")], None, None);
    let t = fx.run(&item, None, &session_in("alpha", None, S1), &Disputes::default());
    let (code, _) = refusal(&t);
    assert_eq!(code, RefuseCode::Forcing);
    assert!(receipt(&t).contains("'git push --force --dry-run' (session 0de7dc52"), "{}", receipt(&t));
    assert!(receipt(&t).contains("was made by the writing session and does not count"), "{}", receipt(&t));
}

// --------------------------------------------------------- history, D7

/// R7b.5 (Z5, D7): an earlier biting check replaced by a raw revise with no
/// because, whose literal the current text still names, turns a would-be
/// note into "needs the agent", naming that check.
#[test]
fn r7b_5_an_earlier_biting_check_removed_without_a_because_needs_the_agent() {
    let mut fx = Fx::new();
    let armed = rule("f-rule", "Never push with --force to main.", vec![cmd("git push")], forbidden(&["--force"]), None);
    let first = fx.legacy(&armed);
    let mut bare = armed.clone();
    bare.check = None;
    fx.legacy(&bare);
    let r = fx.retest("f-rule");
    match &r.outcome {
        Maintenance::NeedsAgent(why) => {
            assert!(why.contains(&format!("the check it carried at log seq {} (forbidden '--force' on 'git push') still bites", first.seq)), "{why}");
            assert!(why.contains("Restore it: check_kind forbidden, check_literals ['--force']"), "{why}");
        }
        other => panic!("{other:?}\n{:?}", r.lines),
    }
}

/// D7 at the write itself: a revise that drops a biting check the text
/// still names, with no because, is refused - that check forces as history;
/// the same revise with a because lands as a note and lists the check.
#[test]
fn a_write_dropping_a_biting_check_needs_a_because_or_it_forces() {
    let mut fx = Fx::new();
    let armed = rule("f-rule", "Never push with --force to main.", vec![cmd("git push")], forbidden(&["--force"]), None);
    let head = fx.legacy(&armed);
    let mut bare = armed.clone();
    bare.check = None;
    let (code, message) = refusal(&fx.run(&bare, Some(&armed), &SessionCtx::default(), &Disputes::default()));
    assert_eq!(code, RefuseCode::Forcing);
    assert!(message.contains(&format!("the check it carried at log seq {} (forbidden '--force' on 'git push') still bites", head.seq)), "{message}");
    let t = fx.run_because(&bare, Some(&armed), Some("the flag is allowed on the mirror"), &SessionCtx::default(), &Disputes::default());
    assert!(matches!(t.outcome, Outcome::Note { .. }), "{:?}\n{}", t.outcome, receipt(&t));
    assert!(receipt(&t).contains("because: 'the flag is allowed on the mirror'"), "{}", receipt(&t));
}

/// D7: the teeth of an earlier text are tried and recorded, and force
/// nothing - the current text no longer composes them.
#[test]
fn the_teeth_of_an_earlier_text_are_tried_and_force_nothing() {
    let mut fx = Fx::new();
    let first = rule("old-text", "Never run `git push --force` on main.", vec![cmd("git push")], None, None);
    let mut first_stored = first.clone();
    first_stored.tags = vec![no_literal("git push")];
    fx.legacy(&first_stored);
    let mut now = first_stored.clone();
    now.text = "Ask before pushing to main.".to_string();
    fx.legacy(&now);
    let t = fx.run(&now, Some(&now), &SessionCtx::default(), &Disputes::default());
    assert!(matches!(t.outcome, Outcome::Note { .. }), "{:?}\n{}", t.outcome, receipt(&t));
    assert!(receipt(&t).contains("built from the rule's earlier texts were tried and recorded"), "{}", receipt(&t));
    let attempts = &t.record.as_ref().unwrap().fields().attempts;
    assert!(
        attempts.iter().any(|a| a.form.starts_with("derived forbidden '--force' on 'git push'") && a.outcome == AttemptOutcome::Refused),
        "the earlier text's tooth was tried: {attempts:?}"
    );
}

/// R7b.9 (FB 12, D7): a contains check removed by a revise carrying a
/// because is listed "dropped at seq N, because: ..." and never makes the
/// rule need the agent.
#[test]
fn r7b_9_a_check_removed_with_a_because_forces_nothing() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("AGENTS.md", "# AGENTS\n")], true);
    fx.register("beta", &beta);
    let armed = rule("ag", "Keep the AGENTS heading in AGENTS.md.", vec![path("AGENTS.md")], contains("AGENTS.md", "AGENTS"), Some("beta"));
    let first = fx.legacy(&armed);
    let mut bare = armed.clone();
    bare.check = None;
    // A revise carrying a because, raw, the shape every binary writes it in.
    let body = model::store::canonical_body(&bare).unwrap();
    let body = format!("{},\"because\":\"the heading is renamed next week\"}}", &body[..body.len() - 1]);
    fx.store.append_mutate_checked("s", "ag", "a", EventKind::FactRevised, "ag", None, &body).unwrap();
    let r = fx.retest("ag");
    assert!(matches!(r.outcome, Maintenance::Note { .. }), "{:?}\n{:?}", r.outcome, r.lines);
    let lines = r.lines.join("\n");
    assert!(lines.contains(&format!("dropped at seq {}, because: 'the heading is renamed next week'", first.seq)), "{lines}");
}

// ----------------------------------------------------------- retest, apply

/// G7b.6 (F9, K10): the literal is missing from the registered working tree
/// but present in the default branch's HEAD: blocked, nothing written.
#[test]
fn g7b_6_a_working_tree_mid_change_is_blocked() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "keep me\n")], true);
    fx.register("beta", &beta);
    let item = rule("co", "NOTES.md keeps `keep me`.", vec![path("NOTES.md")], contains("NOTES.md", "keep me"), Some("beta"));
    fx.legacy(&item);
    fs::write(beta.join("NOTES.md"), "mid-edit\n").unwrap();
    let before = fx.store.get_all_events().unwrap().len();
    let r = fx.retest("co");
    assert_eq!(
        match &r.outcome {
            Maintenance::Blocked(b) => Some(b.clone()),
            _ => None,
        },
        Some(Blocker::WorkingTreeDiffers { path: "NOTES.md".to_string() }),
        "{:?}",
        r.outcome
    );
    assert!(r.write.is_none());
    assert!(apply(&mut fx.store, &r, retest_actor()).is_err());
    assert_eq!(fx.store.get_all_events().unwrap().len(), before, "nothing written");
}

/// G7b.7 (X9): a light checked rule at a binding full of heavier rivals:
/// `apply` answers "needs the agent (crowded at ...)" and writes nothing.
#[test]
fn g7b_7_apply_at_a_crowded_binding_needs_the_agent() {
    let mut fx = Fx::new();
    let mut light = rule("light", "Never run `git push --force` on main.", vec![cmd("git push")], forbidden(&["--force"]), None);
    light.severity = None;
    fx.legacy(&light);
    for n in 0..4 {
        fx.legacy(&rule(&format!("heavy-{n}"), &format!("Heavy rival {n}."), vec![cmd("git push")], None, None));
    }
    let r = fx.retest("light");
    assert!(matches!(r.outcome, Maintenance::Rule { .. }), "{:?}\n{:?}", r.outcome, r.lines);
    let before = fx.store.get_all_events().unwrap().len();
    match apply(&mut fx.store, &r, retest_actor()) {
        Err(ApplyError::NeedsAgent(why)) => assert!(why.starts_with("crowded at git push: fold this rule into a neighbour"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(fx.store.get_all_events().unwrap().len(), before);
}

/// PR's compare-and-swap: `apply` is refused when the rule moved between
/// the retest and the write; with the head unchanged it lands with its
/// record.
#[test]
fn apply_is_refused_on_a_moved_head() {
    let mut fx = Fx::new();
    let item = rule("fp", "Never run `git push --force` on main.", vec![cmd("git push")], forbidden(&["--force"]), None);
    fx.legacy(&item);
    let r = fx.retest("fp");
    assert!(matches!(r.outcome, Maintenance::Rule { .. }), "{:?}", r.outcome);
    let mut moved = item.clone();
    moved.text = "Never run `git push --force` on main or release.".to_string();
    fx.legacy(&moved);
    let before = fx.store.get_all_events().unwrap().len();
    match apply(&mut fx.store, &r, retest_actor()) {
        Err(ApplyError::Moved(why)) => assert!(why.contains("changed while it was being tried (fingerprint moved) - run retest again"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(fx.store.get_all_events().unwrap().len(), before);

    let r = fx.retest("fp");
    let written = apply(&mut fx.store, &r, retest_actor()).unwrap();
    assert_eq!(written.actor, "retest");
    assert_eq!(model::store::current_verdict(&fx.store, "fp"), model::store::CurrentVerdict::RuleWired);
}

/// `apply` of a note clears the check on purpose, with the trial's because,
/// and the item reads as a note from its record.
#[test]
fn apply_writes_a_note_and_clears_the_check() {
    let mut fx = Fx::new();
    let item = rule(
        "tag",
        "Tag every release annotated with -a.",
        vec![cmd("git tag")],
        Some(Check::Requires { when: "git tag".to_string(), required: vec!["-a".to_string()] }),
        None,
    );
    fx.legacy(&item);
    let r = fx.retest("tag");
    assert!(matches!(&r.outcome, Maintenance::Note { because } if because.contains("was not stored")), "{:?}", r.outcome);
    let written = apply(&mut fx.store, &r, retest_actor()).unwrap();
    assert!(written.body.contains("\"because\":"), "the note carries the trial's because");
    let now = model::store::show(&fx.store, "tag").unwrap();
    assert!(now.check.is_none());
    assert!(now.tags.contains(&model::store::TEETH_NOTE_TAG.to_string()), "{:?}", now.tags);
    assert_eq!(model::store::current_verdict(&fx.store, "tag"), model::store::CurrentVerdict::Note);
}

/// A retest never demotes on an A4 failure: it needs the agent, and prints
/// the call that fixes it.
#[test]
fn a_retest_never_demotes_on_a4() {
    let mut fx = Fx::new();
    let item = rule("disk", "Never repartition the data disk.", vec![cmd("diskpart")], forbidden(&["zqxj-never-typed"]), None);
    fx.legacy(&item);
    match fx.retest("disk").outcome {
        Maintenance::NeedsAgent(why) => assert!(why.starts_with("A4: ") && why.contains("revise the text to name it"), "{why}"),
        other => panic!("{other:?}"),
    }
}

// ------------------------------------------------------- the real calls, M8

/// DR M8: the replay counts only the calls the anchor reaches, and the
/// whole-hit replay changes the receipt only - the stored item is the same
/// with and without real calls in the log.
#[test]
fn the_replay_counts_reached_calls_and_changes_only_the_text() {
    let mut fx = Fx::new();
    let item = rule("fp", "Never run `git push --force` on main.", vec![cmd("git push")], forbidden(&["--force"]), None);
    let quiet = fx.write(&item);
    fx.served(S2, "git push --force origin main");
    fx.served(S2, "echo git push --force");
    fx.served(S2, "ls -la");
    let loud = fx.write(&item);
    assert!(receipt(&loud).contains("reaches 1 real call(s) in the log (0 more cut at 120 characters); would refuse 1"), "{}", receipt(&loud));
    assert!(receipt(&quiet).contains("reaches 0 real call(s)"), "{}", receipt(&quiet));
    assert_eq!(quiet.outcome, loud.outcome);
    assert_eq!(stored_item(&item, &quiet), stored_item(&item, &loud));
}

/// DR M8: an inner trigger that occurs in reached calls but that the matcher
/// never finds there is named - end it with * for a family of names.
#[test]
fn a_blind_inner_trigger_is_named() {
    let mut fx = Fx::new();
    fx.served(S2, "mkdir /c/tmp/thor-probe-eval");
    let item = rule(
        "mk",
        "A mkdir under /thor must carry -p.",
        vec![cmd("mkdir")],
        Some(Check::Requires { when: "/thor".to_string(), required: vec!["-p".to_string()] }),
        None,
    );
    let t = fx.write(&item);
    assert!(
        receipt(&t).contains("the trigger is blind to how it is really typed: it occurs in 1 reached call(s) and the matcher finds it in 0"),
        "{}",
        receipt(&t)
    );
}

// ------------------------------------------------- the fences around it

/// The trial and the legacy proof never call a guard arm directly: every
/// attempt goes through `guard_verdict`, the hook's own pipeline.
#[test]
fn prove_and_teeth_never_call_a_guard_arm_directly() {
    let offenders = |text: &str| -> bool {
        let code = text.split("#[cfg(test)]").next().unwrap_or(text);
        code.lines().filter(|l| !l.trim_start().starts_with("//")).any(|l| l.contains("absent_guard::find_"))
    };
    for (name, text) in [("prove.rs", include_str!("../prove.rs")), ("teeth.rs", include_str!("../teeth.rs"))] {
        assert!(!offenders(text), "{name} calls an absent_guard::find_* arm directly");
    }
    // Red: the same scan finds one.
    assert!(offenders("fn f() { absent_guard::find_command_violation(&a, b, None); }"));
    assert!(!offenders("fn f() {}\n#[cfg(test)]\nmod t { fn g() { absent_guard::find_violation(); } }"));
    assert!(!offenders("/// see `absent_guard::find_violation`\nfn f() {}"), "a doc comment naming one is no call");
}

/// Every refusal and needs-the-agent answer of this module carries the words
/// its row of `REFUSAL_TABLE` names (G1): each one is produced here and
/// checked, and a code with no row fails.
#[test]
fn every_refusal_and_needs_agent_text_is_in_the_refusal_table() {
    let row = |code: &str| REFUSAL_TABLE.iter().find(|(c, _)| *c == code).map(|(_, w)| *w).unwrap_or_else(|| panic!("no row for {code}"));
    let mut fx = Fx::new();
    let mut seen: Vec<String> = Vec::new();
    let mut check = |code: &str, text: &str| {
        assert!(text.contains(row(code)), "{code}: '{}' missing from: {text}", row(code));
        seen.push(code.to_string());
    };

    let gamma = fx.checkout("gamma", &[("README.md", "x\n")], true);
    let g = rule("g", "Keep the `x` in README.md.", vec![path("README.md")], contains("README.md", "x"), Some("gamma"));
    let (c, m) = refusal(&fx.run(&g, None, &session_in("gamma", Some(&gamma), S1), &Disputes::default()));
    check(c.as_str(), &m);
    let (c, m) = refusal(&fx.write(&rule("a4", "Never repartition the disk.", vec![cmd("diskpart")], forbidden(&["zq-x"]), None)));
    check(c.as_str(), &m);
    let beta = fx.checkout("beta", &[("NOTES.md", "intro\n")], true);
    fx.register("beta", &beta);
    let (c, m) = refusal(&fx.write(&rule("fn", "NOTES.md keeps `keep me`.", vec![path("NOTES.md")], contains("NOTES.md", "keep me"), Some("beta"))));
    check(c.as_str(), &m);
    let (c, m) = refusal(&fx.write(&rule("fo", "Never run `git push --force` on main.", vec![cmd("git push")], None, None)));
    check(c.as_str(), &m);
    for n in 0..6 {
        fx.served(S2, &format!("git grep -n p{n} src"));
    }
    let (c, m) = refusal(&fx.write(&rule("mj", "Never run `git grep` here.", vec![cmd("git grep")], forbidden(&["git grep"]), None)));
    check(c.as_str(), &m);

    // Needs the agent, at a retest.
    let needs = |fx: &mut Fx, item: &Item| -> String {
        fx.legacy(item);
        match fx.retest(&item.id).outcome {
            Maintenance::NeedsAgent(why) => why,
            other => panic!("{other:?}"),
        }
    };
    let m = needs(&mut fx, &rule("na4", "Never repartition the disk.", vec![cmd("diskpart")], forbidden(&["zq-y"]), None));
    check("needs_agent:a4", &m);
    let m = needs(&mut fx, &rule("nfo", "Never run `git push --force` on main.", vec![cmd("git push")], None, None));
    check("needs_agent:forcing", &m);
    let m = needs(&mut fx, &rule("nmj", "Never run `git grep` there.", vec![cmd("git grep")], forbidden(&["git grep"]), None));
    check("needs_agent:majority", &m);
    fs::write(beta.join("NOTES.md"), "intro\nkeep this line\n").unwrap();
    git(&beta, &["commit", "-q", "-am", "line"]);
    let m = needs(&mut fx, &rule("nnr", "NOTES.md keeps `keep this line`.", vec![cmd("make release")], contains("NOTES.md", "keep this line"), Some("beta")));
    check("needs_agent:no_reach", &m);
    let m = needs(&mut fx, &rule("nfn", "NOTES.md keeps `gone line`.", vec![path("NOTES.md")], contains("NOTES.md", "gone line"), Some("beta")));
    check("needs_agent:false_now", &m);
    // A rule with no project and a file check is THOR's blocker at a retest
    // (step 20, K10), never the agent's: it has no row here.
    let global = rule("ngl", "Keep `x` in README.md.", vec![path("README.md")], contains("README.md", "x"), None);
    fx.legacy(&global);
    assert!(matches!(fx.retest("ngl").outcome, Maintenance::Blocked(ref b) if b.is_global_rules()));
    let mut light = rule("ncr", "Never run `npm publish --force` here.", vec![cmd("npm publish")], forbidden(&["--force"]), None);
    light.severity = None;
    fx.legacy(&light);
    for n in 0..4 {
        fx.legacy(&rule(&format!("h{n}"), &format!("Heavy {n}."), vec![cmd("npm publish")], None, None));
    }
    let r = fx.retest("ncr");
    match apply(&mut fx.store, &r, retest_actor()) {
        Err(ApplyError::NeedsAgent(why)) => check("needs_agent:crowded", &why),
        other => panic!("{other:?}"),
    }
    // A full place a rule's check needs as its anchor is the agent's to clear
    // (step 26): the line names every occupant, the exits and the binding.
    for n in 0..4 {
        let mut sitting = rule(&format!("np{n}"), &format!("NOTES.md fact {n}."), vec![path("NOTES.md")], None, Some("beta"));
        sitting.severity = None;
        fx.legacy(&sitting);
    }
    let m = needs(&mut fx, &rule("nfp", "NOTES.md keeps `keep this line` for deploys.", vec![cmd("make deploy")], contains("NOTES.md", "keep this line"), Some("beta")));
    check("needs_agent:full_place", &m);

    for (code, _) in REFUSAL_TABLE {
        assert!(seen.iter().any(|s| s == code), "the table row {code} was never produced here");
    }
    for code in [RefuseCode::Untryable, RefuseCode::A4, RefuseCode::FalseNow, RefuseCode::Forcing, RefuseCode::Majority] {
        assert!(REFUSAL_TABLE.iter().any(|(c, _)| *c == code.as_str()), "{code:?} has no row");
    }
}

// --------------------------------------------- exits the wiring exposed (step 8)

/// A reason tag on the rule must never keep a tooth built from its own text
/// from forcing: the rule with the tooth AND the reason would be refused by
/// the gate (a check and "nothing can catch this" together), so an earlier
/// build judged the tooth blocked and stored the rule as a note - tagging a
/// reason was a way past the trial. The tooth is judged without the reason
/// now, and forces.
#[test]
fn a_reason_tag_never_keeps_a_derived_tooth_from_forcing() {
    let fx = Fx::new();
    let mut item = rule("reasoned", "Never force-push: `git push --force` rewrites shared history.", vec![cmd("git push")], None, None);
    item.tags = vec![no_literal("git push")];
    let t = fx.write(&item);
    let (code, message) = refusal(&t);
    assert_eq!(code, RefuseCode::Forcing, "{message}");
    assert!(message.contains("forbidden '--force' on 'git push'"), "{message}");
    assert!(message.contains("A no-literal reason no longer stands in for a tooth that works."), "{message}");
    // Green: the same rule without the reason forces too, and does not
    // claim a reason it never carried.
    item.tags = vec![];
    let (_, message) = refusal(&fx.write(&item));
    assert!(!message.contains("no-literal"), "{message}");
}

/// The note label of the head a retest tries must never keep a derived tooth
/// from forcing either: an old note whose text now composes a biting tooth
/// needs the agent, it never stays a note.
#[test]
fn a_note_label_never_keeps_a_derived_tooth_from_forcing_at_a_retest() {
    let mut fx = Fx::new();
    let mut noted = rule("old-note", "Never force-push: `git push --force` rewrites shared history.", vec![cmd("git push")], None, None);
    noted.tags = vec![model::store::TEETH_NOTE_TAG.to_string()];
    fx.legacy(&noted);
    let r = fx.retest("old-note");
    assert!(matches!(&r.outcome, Maintenance::NeedsAgent(fix) if fix.contains("forbidden '--force' on 'git push'")), "{:?}", r.outcome);
    assert!(r.write.is_none(), "nothing written for it");
}

/// A tooth built from a span that starts with a command the rule is NOT
/// bound to (derive (b)) is tried, described and hung on THAT command -
/// never on the first command anchor the rule happens to list.
#[test]
fn a_derived_tooth_for_another_command_is_tried_on_that_command() {
    let fx = Fx::new();
    let item = rule(
        "bundle",
        "Prove a bundle with `git clone --mirror` plus fsck.",
        vec![cmd("git bundle")],
        None,
        None,
    );
    let (_, message) = refusal(&fx.write(&item));
    assert!(message.contains("forbidden '--mirror' on 'git clone' refuses 'git clone --mirror'"), "{message}");
    assert!(message.contains("and bind it to the command 'git clone'"), "{message}");
    assert!(!message.contains("'git bundle --mirror'"), "never tried on the other anchor: {message}");
}

/// A forcing refusal names the writer's own check first when it was dropped
/// on the way (it refused a clean form), so the writer is never left
/// guessing why the check they sent is not the one discussed.
#[test]
fn a_forcing_refusal_names_the_writers_dropped_check_first() {
    let fx = Fx::new();
    let item = rule(
        "tags",
        "Tag every release annotated: `git tag -a v1 -m msg`.",
        vec![cmd("git tag")],
        Some(Check::Requires { when: "git tag".to_string(), required: vec!["-a".to_string()] }),
        None,
    );
    let (code, message) = refusal(&fx.write(&item));
    assert_eq!(code, RefuseCode::Forcing);
    assert!(
        message.starts_with("Your check (requires '-a' after 'git tag' on 'git tag') was not stored: it refuses 'git tag'"),
        "{message}"
    );
}

/// The "hang it" advice names a requires check the way the remember/revise
/// door takes it: check_literals with the trigger first, never a check_path
/// (the door refuses a check_path on requires, so that advice was a dead end).
#[test]
fn hang_it_names_a_requires_check_the_way_the_door_takes_it() {
    let check = Check::Requires { when: "git tag".to_string(), required: vec!["-a".to_string(), "-s".to_string()] };
    assert_eq!(hang_it(&check), "check_kind requires, check_literals ['git tag', '-a', '-s']");
}

/// A tooth that real calls stop from forcing says so in ONE receipt line,
/// with the count of the rest, while the record keeps every call: on a copy
/// of a real store one rule got a line per ordinary push in the log.
#[test]
fn a_tooth_stopped_by_many_real_calls_says_so_in_one_line() {
    let mut fx = Fx::new();
    for n in 0..5 {
        fx.served(S2, &format!("git status -sb --branch-{n}"));
    }
    // One call without '-sb' stops the requires polarity, as in G7b.3.
    fx.served(S2, "git status --short");
    let item = rule("st-many", "Check with `git status -sb` first.", vec![cmd("git status")], None, None);
    let t = fx.run(&item, None, &session_in("alpha", None, S1), &Disputes::default());
    let r = receipt(&t);
    let lines: Vec<&str> = r.lines().filter(|l| l.starts_with("forbidden '-sb' on 'git status' does not force")).collect();
    assert_eq!(lines.len(), 1, "{r}");
    assert!(lines[0].ends_with(", and 4 more real call(s) like it"), "{r}");
    assert!(matches!(t.outcome, Outcome::Note { by_dispute: false, .. }), "{:?}
{r}", t.outcome);
    let stopped = &t.record.as_ref().unwrap().fields().corpus.stopped_forcing;
    assert_eq!(stopped.iter().filter(|s| s.call.starts_with("git status -sb")).count(), 5, "the record keeps every call: {stopped:?}");
}

// ------------------------------------------ old contains stock (Q24)

/// Beta's checkout holding `notes` as NOTES.md, committed and registered.
fn beta_with(fx: &mut Fx, notes: &str) -> PathBuf {
    let beta = fx.checkout("beta", &[("NOTES.md", notes)], true);
    fx.register("beta", &beta);
    beta
}

/// An old rule of beta whose contains check keeps 'keep me' in NOTES.md
/// while its text never names it - written raw, the way an older build wrote
/// it, so it has no verdict yet.
fn old_contains(fx: &mut Fx, id: &str) -> Item {
    let item = rule(id, "NOTES.md keeps its opening line.", vec![path("NOTES.md")], contains("NOTES.md", "keep me"), Some("beta"));
    fx.legacy(&item);
    item
}

fn mark_of(r: &Retest) -> Option<model::store::OldContainsStock> {
    r.write.as_ref().and_then(|(_, record)| record.fields().a4_old_stock.clone())
}

/// Q24, green: an OLD contains check that bites both ways in the working copy
/// registered for its project is a rule although its text does not name the
/// literal; the receipt and the record carry the fragment it guards, the first
/// apply records the A4 cutoff, and `history` shows the fragment.
#[test]
fn q24_an_old_contains_check_that_bites_is_a_rule_showing_the_fragment_it_guards() {
    let mut fx = Fx::new();
    beta_with(&mut fx, "keep me\nplain line\n");
    old_contains(&mut fx, "old");
    let r = fx.retest("old");
    assert!(matches!(r.outcome, Maintenance::Rule { proven: true, .. }), "{:?}\n{:?}", r.outcome, r.lines);
    let lines = r.lines.join("\n");
    assert!(
        lines.contains("old contains check (Q24): counted as a tooth although the text does not name its literal - it guards 'keep me' in NOTES.md (its check was set at log seq"),
        "{lines}"
    );
    let mark = mark_of(&r).expect("the record carries the fragment");
    assert_eq!((mark.literal.as_str(), mark.path.as_str()), ("keep me", "NOTES.md"));
    assert_eq!(model::store::a4_cutoff(&fx.store), None, "a retest alone writes nothing");
    assert!(lines.contains(&format!("the A4 cutoff is not recorded yet: --apply records log seq {} as the cutoff", mark.cutoff)), "{lines}");

    apply(&mut fx.store, &r, retest_actor()).unwrap();
    assert_eq!(model::store::a4_cutoff(&fx.store), Some(mark.cutoff), "the first old-stock verdict records the cutoff it was judged by");
    assert_eq!(model::store::current_verdict(&fx.store, "old"), model::store::CurrentVerdict::RuleProven);
    assert!(!model::store::settlement(&fx.store, "old", None).is_owed());
    let log = model::store::history(&fx.store, "old").unwrap();
    assert_eq!(log.last().unwrap().a4_old_stock.as_ref(), Some(&mark));
    assert_eq!(model::store::show(&fx.store, "old").unwrap().tags, vec![model::store::TEETH_PROVEN_TAG.to_string()]);
}

/// Q24, red: an old contains check whose literal is no longer in its file is
/// not a tooth - A4 answers exactly as before, with one line saying why.
#[test]
fn q24_an_old_contains_check_whose_literal_left_its_file_is_not_a_tooth() {
    let mut fx = Fx::new();
    beta_with(&mut fx, "plain line only\n");
    old_contains(&mut fx, "old");
    let r = fx.retest("old");
    match &r.outcome {
        Maintenance::NeedsAgent(why) => assert!(why.starts_with("A4:") && why.contains("'keep me'"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(r.write.is_none(), "nothing to write");
    assert!(
        r.lines.iter().any(|l| l.contains("not counted as a tooth despite A4 - NOTES.md does not contain 'keep me'")),
        "{:?}",
        r.lines
    );
}

/// Q24, red: A4 stays in force for every NEW write, contains included, and
/// for every write that sets or changes the check of an old rule.
#[test]
fn q24_a_new_or_changed_contains_check_whose_literal_the_text_does_not_name_is_refused() {
    let mut fx = Fx::new();
    beta_with(&mut fx, "keep me\nplain line\n");
    let fresh = rule("fresh", "NOTES.md keeps its opening line.", vec![path("NOTES.md")], contains("NOTES.md", "keep me"), Some("beta"));
    let (code, message) = refusal(&fx.write(&fresh));
    assert_eq!(code, RefuseCode::A4);
    assert!(message.contains("'keep me' is not named anywhere in the rule's own text"), "{message}");

    let old = old_contains(&mut fx, "old");
    let r = fx.retest("old");
    apply(&mut fx.store, &r, retest_actor()).unwrap();
    let mut changed = old.clone();
    changed.check = contains("NOTES.md", "plain line");
    let (code, _) = refusal(&fx.run(&changed, Some(&old), &SessionCtx::default(), &Disputes::default()));
    assert_eq!(code, RefuseCode::A4, "an exempted rule's new check meets A4 like any other");
}

/// Q24, red: a revise of the text, or of the anchors, ends the exemption for
/// good - the write lands undecided (K23), and the next retest needs the
/// agent for A4 instead of counting the old check again.
#[test]
fn q24_a_revise_of_the_text_or_the_anchors_ends_the_exemption() {
    for change in ["text", "anchors"] {
        let mut fx = Fx::new();
        beta_with(&mut fx, "keep me\nplain line\n");
        let old = old_contains(&mut fx, "old");
        let r = fx.retest("old");
        apply(&mut fx.store, &r, retest_actor()).unwrap();
        let before = model::store::show(&fx.store, "old").unwrap();
        let mut revised = before.clone();
        match change {
            "text" => revised.text = "NOTES.md keeps its first line, always.".to_string(),
            _ => revised.bindings.push(Binding::Always),
        }
        let t = fx.run(&revised, Some(&before), &SessionCtx::default(), &Disputes::default());
        assert!(matches!(&t.outcome, Outcome::Undecided { why } if why.starts_with("A4: 'keep me'")), "{change}: {:?}", t.outcome);
        assert!(t.record.as_ref().unwrap().fields().a4_old_stock.is_none(), "{change}: a write never carries the mark");
        let stored = stored_item(&revised, &t).unwrap();
        // As the write tool does: THOR's own label is the trial's to drop.
        let mut gate_existing = before.clone();
        gate_existing.tags.retain(|t| !t.starts_with(model::store::TEETH_TAG_PREFIX));
        model::store::revise_tried_scoped(&mut fx.store, "s", "l", "mcp", &gate_existing, &stored, &before, None, None, t.record.as_ref(), None, None)
            .unwrap();
        assert!(
            matches!(model::store::current_verdict(&fx.store, "old"), model::store::CurrentVerdict::Undecided { why } if why.starts_with("A4:")),
            "{change}"
        );
        let again = fx.retest("old");
        assert!(matches!(&again.outcome, Maintenance::NeedsAgent(why) if why.starts_with("A4:")), "{change}: {:?}", again.outcome);
        assert!(mark_of(&again).is_none() && !again.lines.iter().any(|l| l.contains("(Q24)")), "{change}: {:?}", again.lines);
        let _ = old;
    }
}

/// Q24: the exemption is granted by a retest only - a write that changes
/// nothing about the check, text or anchors (here the severity) lands
/// undecided, exactly as before, and never carries the mark.
#[test]
fn q24_the_exemption_is_never_granted_at_a_write() {
    let mut fx = Fx::new();
    beta_with(&mut fx, "keep me\nplain line\n");
    let old = old_contains(&mut fx, "old");
    let mut lighter = old.clone();
    lighter.severity = Some(Severity::Costly);
    let t = fx.run(&lighter, Some(&old), &SessionCtx::default(), &Disputes::default());
    assert!(matches!(&t.outcome, Outcome::Undecided { why } if why.starts_with("A4: 'keep me'")), "{:?}", t.outcome);
    assert!(t.record.as_ref().unwrap().fields().a4_old_stock.is_none());
}

/// Q24, 5: no other check kind is exempt - an old forbidden or absent check
/// whose literal the text does not name needs the agent for A4, however well
/// it bites.
#[test]
fn q24_no_other_check_kind_is_exempt() {
    let mut fx = Fx::new();
    beta_with(&mut fx, "keep me\nplain line\n");
    let forbidden_old = rule("f-old", "Never push a mirror of this repository.", vec![cmd("git push")], forbidden(&["--mirr0r-zz"]), Some("beta"));
    fx.legacy(&forbidden_old);
    let absent_old = rule(
        "a-old",
        "NOTES.md stays free of a stray marker.",
        vec![path("NOTES.md")],
        Some(Check::Absent { path: "NOTES.md".to_string(), literal: "drop me".to_string() }),
        Some("beta"),
    );
    fx.legacy(&absent_old);
    for id in ["f-old", "a-old"] {
        let r = fx.retest(id);
        assert!(matches!(&r.outcome, Maintenance::NeedsAgent(why) if why.starts_with("A4:")), "{id}: {:?}", r.outcome);
        assert!(mark_of(&r).is_none(), "{id}");
    }
}

/// Q24: `apply` writes an old-stock verdict only against the cutoff it was
/// judged by - one recorded in between (another run) sends it back to retest.
#[test]
fn q24_apply_refuses_when_the_cutoff_was_recorded_in_between() {
    let mut fx = Fx::new();
    beta_with(&mut fx, "keep me\nplain line\n");
    old_contains(&mut fx, "old");
    let r = fx.retest("old");
    let judged = mark_of(&r).unwrap().cutoff;
    fx.legacy(&plain(&["unrelated"]));
    let later = model::store::prospective_a4_cutoff(&fx.store);
    assert!(later > judged);
    model::store::record_a4_cutoff(&mut fx.store, retest_actor(), later).unwrap();
    let before = fx.store.get_all_events().unwrap().len();
    match apply(&mut fx.store, &r, retest_actor()) {
        Err(ApplyError::Moved(why)) => assert!(why.contains("run retest again"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(fx.store.get_all_events().unwrap().len(), before, "nothing written");
}

/// A rule of project alpha with nothing to try, named after `words`.
fn plain(words: &[&str]) -> Item {
    rule(&words.join("-"), "Run the linter before a release.", vec![cmd("make release")], None, Some("alpha"))
}

// ------------------------------------- step 24: a text revise keeps a true check

/// The setup of field defect 5 (2026-10-01): a true contains check on a rule
/// bound to a command, which never reaches a write to NOTES.md, with a text
/// that does not name the literal - and the rewording the evaluation asked
/// for, naming it.
fn unreached_contains(fx: &mut Fx) -> (Item, Item) {
    let beta = fx.checkout("beta", &[("NOTES.md", "keep this line\n")], true);
    fx.register("beta", &beta);
    let before = rule("c-kept", "NOTES.md must keep its first line.", vec![cmd("make release")], contains("NOTES.md", "keep this line"), Some("beta"));
    fx.legacy(&before);
    let mut reworded = before.clone();
    reworded.text = "NOTES.md must keep `keep this line` as its first line.".to_string();
    (before, reworded)
}

/// Field defect 5: rewording a rule to name its check's literal cleared its
/// TRUE check, because its bindings never reach a write to the file. A
/// text-only revise keeps the check now: undecided, with the rebind named as
/// a step of its own - and the store reads it back as owed, not as a note.
#[test]
fn a_text_revise_keeps_a_true_check_its_bindings_never_reach() {
    let mut fx = Fx::new();
    let (before, reworded) = unreached_contains(&mut fx);
    let t = fx.run(&reworded, Some(&before), &SessionCtx::default(), &Disputes::default());
    assert!(matches!(t.outcome, Outcome::Undecided { .. }), "{:?}\n{}", t.outcome, receipt(&t));
    assert_eq!(t.check, before.check, "a text revise never clears the check: {}", receipt(&t));
    let said = receipt(&t);
    assert!(said.contains("is kept") && said.contains("never reach a write to NOTES.md"), "{said}");
    assert!(said.contains("The rebind is a step of its own") && said.contains("\"value\": \"NOTES.md\""), "{said}");
    let stored = stored_item(&reworded, &t).unwrap();
    assert_eq!(stored.check, before.check);
    model::store::revise_tried_scoped(&mut fx.store, "s", "l", "mcp", &before, &stored, &before, None, None, t.record.as_ref(), None, None)
        .unwrap();
    match model::store::current_verdict(&fx.store, "c-kept") {
        model::store::CurrentVerdict::Undecided { why } => assert!(why.contains("is kept"), "{why}"),
        other => panic!("{other:?}"),
    }
    // The rebind it names, as its own step: now the check bites.
    let mut bound = stored.clone();
    bound.bindings.push(path("NOTES.md"));
    let t = fx.run(&bound, Some(&stored), &SessionCtx::default(), &Disputes::default());
    assert!(matches!(t.outcome, Outcome::Rule { .. }), "{:?}\n{}", t.outcome, receipt(&t));
}

/// The same rewording when NOTES.md already holds the most facts a place
/// shows: the check is kept, and - the owner's decision, step 26 - making room
/// is the AGENT's: the reply names every fact there with its severity and
/// whether it carries a check, and the three honest exits; THOR's own retest
/// hands it back to the agent's batch with the same line, never "the owner
/// decides".
#[test]
fn a_kept_check_on_a_full_place_names_the_occupants_and_the_exits() {
    let mut fx = Fx::new();
    let (before, reworded) = unreached_contains(&mut fx);
    for n in 0..model::item::MAX_ITEMS {
        let mut sitting = rule(&format!("sits-{n}"), &format!("NOTES.md fact number {n}."), vec![path("NOTES.md")], None, Some("beta"));
        sitting.severity = None;
        fx.legacy(&sitting);
    }
    let t = fx.run(&reworded, Some(&before), &SessionCtx::default(), &Disputes::default());
    assert!(matches!(t.outcome, Outcome::Undecided { .. }), "{:?}\n{}", t.outcome, receipt(&t));
    assert_eq!(t.check, before.check);
    let said = receipt(&t);
    assert!(said.contains("'sits-0' (no severity, no check)") && said.contains("FOLD") && said.contains("RETRACT"), "{said}");
    assert!(!said.contains("owner decides"), "{said}");
    let stored = stored_item(&reworded, &t).unwrap();
    model::store::revise_tried_scoped(&mut fx.store, "s", "l", "mcp", &before, &stored, &before, None, None, t.record.as_ref(), None, None)
        .unwrap();
    let r = fx.retest("c-kept");
    match &r.outcome {
        Maintenance::NeedsAgent(why) => assert!(why.contains("'sits-3' (no severity, no check)") && why.contains("MOVE"), "{why}"),
        other => panic!("{other:?}\n{:?}", r.lines),
    }
}
