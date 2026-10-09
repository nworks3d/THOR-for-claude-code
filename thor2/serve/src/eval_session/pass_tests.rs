//! Units of THOR's machine pass at an evaluation start (`machine_pass`,
//! `begin`, `decide`, `resume` - step 14 of the watertight teeth build): what
//! the machine decides itself, what it hands to the agent, what it leaves to
//! the owner through its own open points, and how its two writes are ordered
//! around a time box.

#![cfg(test)]

use super::*;
use model::item::{Binding, Severity, TargetKind};
use model::store::{CurrentVerdict, PendingWhy, TrialEvidence};
use std::fs;
use thor_core::event_store::Event;

const S1: &str = "0de7dc52-1b97-4b66-948f-61e4438043b2";
const S2: &str = "373e8610-8130-490d-ac29-56aa98709fd1";
const S3: &str = "5e1f0c3a-0000-4000-8000-000000000003";
const S4: &str = "5e1f0c3a-0000-4000-8000-000000000004";
const S5: &str = "5e1f0c3a-0000-4000-8000-000000000005";

/// A box no fixture ever runs out of.
const WIDE: Duration = Duration::from_secs(120);

/// A store on disk and the folder its checkouts live in.
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

    /// A git checkout of project `key` under the fixture folder, holding
    /// `files`, committed when `commit`; its git hooks point at an empty
    /// folder, so no shared hook ever acts on it (builder rule 11).
    fn checkout(&self, key: &str, files: &[(&str, &str)], commit: bool) -> PathBuf {
        let root = self.dir.path().join(key);
        fs::create_dir_all(&root).unwrap();
        let hooks = self.dir.path().join("no-hooks");
        fs::create_dir_all(&hooks).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
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
    /// writes it (a test holds no installer token).
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

    fn served(&mut self, session: &str, id: &str, trigger: &str) {
        crate::deliver::record_delivery_with_trigger(&mut self.store, session, session, "hook", "2026-09-28T00:00:00Z", &[id.to_string()], Some(trigger));
    }

    /// A write, raw, the way an older store holds it: no gate, no trial.
    fn legacy(&mut self, item: &Item) -> Event {
        let body = model::store::canonical_body(item).unwrap();
        self.store.append_event("legacy", &item.id, "migration", EventKind::FactCreated, &item.id, None, &body).unwrap()
    }

    /// Close every other session's open evaluation of `project` as
    /// superseded, the way a start finds one whose session went quiet (step
    /// 24: one project runs one evaluation at a time). These cases are about
    /// what one start leaves the next, not about two running at once.
    fn quiet_others(&mut self, session: &str, project: &str) {
        let now = crate::time::now_unix();
        for other in model::store::open_evaluation_starts_of_project(&self.store, project, now) {
            if other.frozen.session == session {
                continue;
            }
            let superseded = model::store::FrozenSuperseded { by_session: session.to_string(), at_seq: 0, idle_minutes: None };
            model::store::record_evaluation_superseded(&mut self.store, eval_machine(), &other, &superseded).unwrap();
        }
    }

    /// One evaluation start of `project` by `session`, both writes, with a
    /// box nothing here runs out of.
    fn start(&mut self, session: &str, project: &str) -> EvaluationStart {
        self.quiet_others(session, project);
        let began = begin(&self.db, &mut self.store, session, project, "command").unwrap();
        assert!(began.fresh, "a fresh start for {session}");
        decide(&mut self.store, began.start, WIDE)
    }

    fn events_of(&self, id: &str) -> Vec<Event> {
        self.store.get_events_by_entity(id).unwrap()
    }

    /// The writes `teeth-eval` made on `id`.
    fn machine_writes(&self, id: &str) -> Vec<Event> {
        self.events_of(id).into_iter().filter(|e| e.actor == "teeth-eval").collect()
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

fn outcome_of(start: &EvaluationStart, id: &str) -> MachineOutcome {
    let machine = start.frozen.machine.as_ref().expect("the batch record holds the machine's decisions");
    machine.items.iter().find(|i| i.id == id).unwrap_or_else(|| panic!("'{id}' is not in {:?}", machine.items)).outcome
}

fn item_of<'a>(start: &'a EvaluationStart, id: &str) -> &'a MachineItem {
    start.frozen.machine.as_ref().unwrap().items.iter().find(|i| i.id == id).unwrap()
}

fn report(id: &str, project: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Report,
        text: format!("fixture evaluation report {id}"),
        bindings: vec![],
        severity: None,
        project: Some(project.to_string()),
        tags: vec![model::store::EVALUATION_REPORT_TAG.to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    }
}

/// File `project`'s evaluation report for `start`, the way the tool server
/// files one (step 15): a filing context - the call seen in flight, the start
/// record it files for.
fn file(fx: &mut Fx, start: &EvaluationStart, id: &str) -> Result<Event, model::store::WriteError> {
    let item = report(id, &start.frozen.project);
    let ctx = model::store::FilingContext::new(model::store::INFLIGHT_SEEN, start.clone());
    model::store::declare_scoped(&mut fx.store, &start.frozen.session, "l", "mcp", &item, ctx.as_ref(), None)
}

/// EV's `machine_pass_writes_one_verdict_per_rule`, restated per K1: every
/// old rule the machine can decide gets exactly ONE write - a `fact_revised`
/// by `teeth-eval` carrying its trial record - and a rule it cannot decide
/// gets none. The totals and every item's outcome are in the start record.
#[test]
fn machine_pass_applies_one_write_per_decided_rule() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "keep me\nplain line\n")], true);
    fx.register("beta", &beta);
    fx.legacy(&rule("co", "NOTES.md keeps `keep me`.", vec![path("NOTES.md")], contains("NOTES.md", "keep me"), Some("beta")));
    fx.legacy(&rule("fp", "Never run `git push --force` on main.", vec![cmd("git push")], forbidden(&["--force"]), Some("beta")));
    fx.legacy(&rule("subj", "Keep commit subjects under 60 characters.", vec![Binding::Moment(intent::Action::Commit)], None, Some("beta")));
    fx.legacy(&rule("wipe", "Never wipe the data volume.", vec![cmd("acme-tool")], forbidden(&["--wipe"]), Some("beta")));

    let start = fx.start(S1, "beta");
    let machine = start.frozen.machine.clone().unwrap();
    assert_eq!((machine.bites, machine.notes, machine.a4_only, machine.needs_agent, machine.blocked, machine.not_reached), (2, 1, 1, 0, 0, 0), "{machine:?}");
    assert_eq!(outcome_of(&start, "co"), MachineOutcome::Rule);
    assert_eq!(outcome_of(&start, "fp"), MachineOutcome::Rule);
    assert_eq!(outcome_of(&start, "subj"), MachineOutcome::Note);
    assert_eq!(outcome_of(&start, "wipe"), MachineOutcome::A4Only);

    for id in ["co", "fp", "subj"] {
        let writes = fx.machine_writes(id);
        assert_eq!(writes.len(), 1, "{id}: one write");
        assert_eq!(writes[0].kind, EventKind::FactRevised);
        assert!(matches!(model::store::trial_of(&writes[0]), Ok(Some(_))), "{id}: the write carries its record");
        assert_eq!(item_of(&start, id).seq, Some(writes[0].seq), "{id}: the start record names the write");
    }
    assert!(fx.machine_writes("wipe").is_empty(), "the machine cannot choose words: no write");
    assert_eq!(model::store::current_verdict(&fx.store, "co"), CurrentVerdict::RuleProven);
    assert_eq!(model::store::current_verdict(&fx.store, "fp"), CurrentVerdict::RuleWired);
    assert_eq!(model::store::current_verdict(&fx.store, "subj"), CurrentVerdict::Note);
    assert!(matches!(model::store::current_verdict(&fx.store, "wipe"), CurrentVerdict::Undecided { .. }));

    // A second start (another session) finds the three decided and tries
    // only the one still open: nothing is written twice.
    let again = fx.start(S2, "beta");
    assert_eq!(again.frozen.machine.as_ref().unwrap().items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["wipe"]);
    assert_eq!(fx.machine_writes("co").len(), 1);
}

/// G14.2 (K8): a requires check on 'git tag' that fired on 'git tag -l'
/// earlier. The listing form is one THOR builds itself, the check refuses it,
/// so the machine clears the check and the because names 'git tag -l'.
#[test]
fn real_call_corpus_names_git_tag_l() {
    let mut fx = Fx::new();
    let item = rule(
        "tag",
        "Tag every release annotated with -a.",
        vec![cmd("git tag")],
        Some(Check::Requires { when: "git tag".to_string(), required: vec!["-a".to_string()] }),
        Some("beta"),
    );
    fx.legacy(&item);
    fx.served(S2, "tag", "git tag -l");
    fx.served(S2, "tag", "git tag -a v1.2 -m release");

    let start = fx.start(S1, "beta");
    let decided = item_of(&start, "tag");
    assert_eq!(decided.outcome, MachineOutcome::Note, "{decided:?}");
    assert!(decided.detail.contains("'git tag -l'"), "the note names the legitimate call it refused: {}", decided.detail);
    let now = model::store::show(&fx.store, "tag").unwrap();
    assert_eq!(now.check, None, "the over-blocking check is cleared");
    let write = fx.machine_writes("tag").pop().unwrap();
    assert!(write.body.contains("'git tag -l'"), "the because on the log names it: {}", write.body);
}

/// G14.1: a contains check whose literal really sits in the file is proven
/// by the machine pass - through THOR's own calls, never by writing the file.
#[test]
fn contains_is_proven_without_touching_the_file() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "keep me\nplain line\n")], true);
    fx.register("beta", &beta);
    fx.legacy(&rule("co", "NOTES.md keeps `keep me`.", vec![path("NOTES.md")], contains("NOTES.md", "keep me"), Some("beta")));
    let file = beta.join("NOTES.md");
    let bytes = fs::read(&file).unwrap();
    let modified = fs::metadata(&file).unwrap().modified().unwrap();
    let digest = teeth::blob_hashes(&beta, &["NOTES.md".to_string()]);

    let start = fx.start(S1, "beta");
    assert_eq!(outcome_of(&start, "co"), MachineOutcome::Rule);
    assert_eq!(
        model::store::settlement(&fx.store, "co", Some(&start)),
        Settlement::Settled(Settled::Rule { proven: true })
    );
    assert_eq!(fs::read(&file).unwrap(), bytes, "not one byte changed");
    assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), modified, "and the file was never written");
    assert_eq!(teeth::blob_hashes(&beta, &["NOTES.md".to_string()]), digest);
    assert!(git_clean(&beta), "the working tree is as it was");
}

fn git_clean(root: &Path) -> bool {
    let out = std::process::Command::new("git").arg("-C").arg(root).args(["status", "--porcelain"]).output().unwrap();
    out.status.success() && out.stdout.is_empty()
}

/// Three file-check rules of a project with no registered working copy, and
/// one command rule of it (which needs no working copy).
fn unregistered_gamma(fx: &mut Fx) {
    for (n, literal) in ["alpha line", "beta line", "gamma line"].into_iter().enumerate() {
        fx.legacy(&rule(
            &format!("g-file-{n}"),
            &format!("README.md keeps `{literal}`."),
            vec![path("README.md")],
            contains("README.md", literal),
            Some("gamma"),
        ));
    }
    fx.legacy(&rule("g-cmd", "Never run `git push --force` on main.", vec![cmd("git push")], forbidden(&["--force"]), Some("gamma")));
}

/// G14.4: a project with no registered working copy - ONE machine point
/// holding all its file-check rules, written by `eval-machine`, with the
/// exact command that clears it; none of them in the agent's batch, and the
/// filing is not refused for them.
#[test]
fn one_point_per_project_wide_blocker() {
    let mut fx = Fx::new();
    unregistered_gamma(&mut fx);
    let start = fx.start(S1, "gamma");
    let machine = start.frozen.machine.clone().unwrap();
    assert_eq!((machine.blocked, machine.bites, machine.needs_agent, machine.a4_only), (3, 1, 0, 0), "{machine:?}");
    assert_eq!(start.frozen.batch(), &[] as &[String]);
    assert_eq!(start.frozen.a4_list(), &[] as &[String]);

    let points = model::store::open_points(&fx.store, Some("gamma"));
    assert_eq!(points.len(), 1, "one point for the whole project: {points:?}");
    let point = &points[0];
    assert_eq!(point.id, "open-no-working-copy-gamma");
    assert!(point.machine);
    assert_eq!((point.subject.as_deref(), point.blocker.as_deref()), (Some("gamma"), Some("no-working-copy")));
    for id in ["g-file-0", "g-file-1", "g-file-2"] {
        assert!(point.text.contains(&format!("'{id}'")), "the point names {id}: {}", point.text);
    }
    assert!(!point.text.contains("'g-cmd'"), "a command rule needs no working copy: {}", point.text);
    let command = model::store::register_command_for(Some(&fx.db));
    assert!(point.text.contains(&format!("open that project's folder on this machine and run: {command}")), "{}", point.text);
    assert!(point.text.contains("if the project is not on this machine, nothing needs doing"), "{}", point.text);
    let events = fx.events_of(&point.id);
    assert_eq!(events.len(), 1);
    assert_eq!((events[0].actor.as_str(), events[0].session_id.as_str()), ("eval-machine", S1));
    assert_eq!(machine.points.iter().map(|p| (p.id.as_str(), p.rules.len())).collect::<Vec<_>>(), vec![("open-no-working-copy-gamma", 3)]);

    // Held by the machine's blocker at this start: not the agent's, and the
    // report files.
    for id in ["g-file-0", "g-file-1", "g-file-2"] {
        assert!(matches!(model::store::settlement(&fx.store, id, Some(&start)), Settlement::Settled(Settled::Blocked(_))), "{id}");
    }
    let note = start_note(&start);
    assert!(note.contains("3 held by a blocker only the owner can lift") && note.contains("'open-no-working-copy-gamma' (holds 3 rule(s)"), "{note}");
    file(&mut fx, &start, "eval-gamma-1").expect("rules held by a machine blocker hold no filing");

    // The same state at the next start: the point is left exactly as it is.
    fx.start(S2, "gamma");
    assert_eq!(fx.events_of("open-no-working-copy-gamma").len(), 1, "nothing new to say: nothing written");
}

/// Step 20 (C3 B2 L416-L439, K10): two OLD rules with NO project carry a
/// file check - one absolute, one relative. Neither can be tried anywhere,
/// and giving either a project would narrow a rule meant for every
/// repository, so they are THOR's blocker: never on the agent's batch (whose
/// only exit was to delete the two true checks), one point for the owner
/// naming the non-destructive way, and the report files. Their checks stay.
/// Once no such rule is left, the next start closes the point itself.
#[test]
fn a_global_file_check_is_a_machine_blocker_never_the_agents_batch() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "keep me\n")], true);
    fx.register("beta", &beta);
    fx.legacy(&rule("glob-abs", "The doctor binary lives at C:/fxp/bin/doctor.exe.", vec![cmd("doctor")], Some(Check::PathExists { path: "C:/fxp/bin/doctor.exe".to_string() }), None));
    fx.legacy(&rule("glob-rel", "Every repo keeps `eol=lf` in .gitattributes.", vec![cmd("git add")], contains(".gitattributes", "eol=lf"), None));
    let start = fx.start(S1, "beta");
    let machine = start.frozen.machine.clone().unwrap();
    assert_eq!(outcome_of(&start, "glob-abs"), MachineOutcome::Blocked);
    assert_eq!(outcome_of(&start, "glob-rel"), MachineOutcome::Blocked);
    assert!(start.frozen.batch().is_empty() && start.frozen.a4_list().is_empty(), "{:?} {:?}", start.frozen.batch(), start.frozen.a4_list());
    // Step 21: one point for the whole store, in no project.
    assert!(model::store::open_points(&fx.store, Some("beta")).is_empty(), "not beta's own point");
    let points = model::store::open_points(&fx.store, None);
    let point = points.iter().find(|p| p.id == model::store::GLOBAL_RULES_POINT_ID).unwrap_or_else(|| panic!("{points:?}"));
    assert_eq!((point.subject.as_deref(), point.blocker.as_deref()), (Some(model::store::GLOBAL_RULES_SUBJECT), Some("no-working-copy")));
    assert!(point.text.contains("'glob-abs'") && point.text.contains("'glob-rel'"), "{}", point.text);
    assert!(point.text.contains("Nothing needs doing") && point.text.contains("clearing a check that is true is never the way"), "{}", point.text);
    assert!(point.text.contains("give it that project (mcp__thor__revise"), "{}", point.text);
    assert!(point.text.contains("one for the whole memory"), "{}", point.text);
    assert_eq!(machine.points.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec![model::store::GLOBAL_RULES_POINT_ID]);
    assert!(start_note(&start).contains("one point for the whole memory: holds 2 rule(s) with no project"), "{}", start_note(&start));
    for id in ["glob-abs", "glob-rel"] {
        assert!(matches!(model::store::settlement(&fx.store, id, Some(&start)), Settlement::Settled(Settled::Blocked(_))), "{id}");
        assert!(matches!(model::store::settlement(&fx.store, id, None), Settlement::Settled(Settled::Blocked(_))), "{id}: not rot at read time either");
        assert!(model::store::show(&fx.store, id).unwrap().check.is_some(), "{id}: its check stays");
    }
    file(&mut fx, &start, "eval-beta-1").expect("rules held by THOR's blocker hold no filing");

    // Both turn out to be about beta and get beta (the named way). Nothing
    // with no project carries a file check now: the next start closes the
    // point.
    for id in ["glob-abs", "glob-rel"] {
        let before = model::store::show(&fx.store, id).unwrap();
        let moved = Item { project: Some("beta".to_string()), ..before.clone() };
        model::store::revise_because(&mut fx.store, "s", "l", "a", &before, &moved, &before, Some("fixture: the rule is about beta")).unwrap();
    }
    let next = fx.start(S2, "beta");
    let closed = &next.frozen.machine.as_ref().unwrap().points_closed;
    assert!(
        closed.iter().any(|(id, why)| id == model::store::GLOBAL_RULES_POINT_ID && why.contains("no rule with no project carries a file check")),
        "{closed:?}"
    );
}

/// Step 22 (C3b B4 L433-L470), narrowed by step 26: an OLD orientation
/// carries a `path_exists` check on a file it is not anchored to, and the only
/// way to name the file (anchor the rule to it) is refused by the crowding
/// cap. When every fact on that place is a rule whose check can refuse AND is
/// heavier than this one, no fact there honestly gives way: THOR's own point
/// holds the rule, its check kept, never the agent's batch - and the point no
/// longer hands the decision to the owner. When an occupant goes, the point
/// closes and the rule is the agent's again.
#[test]
fn a_path_check_on_a_full_place_of_heavier_guards_stays_thors_point() {
    let mut fx = Fx::new();
    let alpha = fx.checkout("alpha", &[("files/PLAN.md", "the plan
")], true);
    fx.register("alpha", &alpha);
    let occupants = ["plan-guard-a", "plan-guard-b", "plan-guard-c", "plan-guard-d"];
    for id in occupants {
        let mut guard = rule(id, &format!("files/PLAN.md keeps `the plan` ({id})."), vec![path("files/PLAN.md")], contains("files/PLAN.md", "the plan"), Some("alpha"));
        guard.severity = Some(model::item::Severity::Costly);
        fx.legacy(&guard);
        crate::teeth::fixture::decide(&mut fx.store, id);
    }
    let project_binding = Binding::Target { kind: TargetKind::Project, value: "alpha".to_string() };
    let orientation = rule("plan-here", "The plan lives in files/PLAN.md of this repo.", vec![project_binding], Some(Check::PathExists { path: "files/PLAN.md".to_string() }), Some("alpha"));
    fx.legacy(&Item { kind: Kind::Orientation, severity: None, ..orientation });

    let start = fx.start(S1, "alpha");
    assert_eq!(outcome_of(&start, "plan-here"), MachineOutcome::Blocked, "{:?}", item_of(&start, "plan-here"));
    assert!(start.frozen.batch().is_empty() && start.frozen.a4_list().is_empty(), "{:?} {:?}", start.frozen.batch(), start.frozen.a4_list());
    let points = model::store::open_points(&fx.store, Some("alpha"));
    let point = points.iter().find(|p| p.machine && p.id.starts_with("open-anchor-full-")).unwrap_or_else(|| panic!("{points:?}"));
    assert_eq!((point.subject.as_deref(), point.blocker.as_deref()), (Some("files/PLAN.md"), Some("anchor-full")));
    assert!(point.text.contains("'plan-here'") && point.text.contains("files/PLAN.md"), "{}", point.text);
    for id in occupants {
        assert!(point.text.contains(&format!("'{id}'")), "the point names the occupant {id}: {}", point.text);
    }
    assert!(point.text.contains("clearing a check that is true is never the way past this"), "{}", point.text);
    assert!(point.text.contains("no fact there honestly gives way"), "{}", point.text);
    assert!(!point.text.contains("owner decides"), "a full place is never the owner's to decide: {}", point.text);
    assert!(!point.text.contains("drop the check"), "never a text that makes clearing the cheapest exit: {}", point.text);
    for at in [Some(&start), None] {
        assert!(matches!(model::store::settlement(&fx.store, "plan-here", at), Settlement::Settled(Settled::Blocked(_))), "{at:?}");
    }
    assert!(model::store::show(&fx.store, "plan-here").unwrap().check.is_some(), "its check stays");
    file(&mut fx, &start, "eval-alpha-1").expect("a rule held by THOR's blocker holds no filing");

    // One occupant goes: the place has room, the point closes at the next
    // start, and the rule is the agent's again - anchoring works.
    model::store::retract(&mut fx.store, "s", "l", "a", "plan-guard-d", "fixture: folded into 'plan-guard-a' about files/PLAN.md", None).unwrap();
    let next = fx.start(S2, "alpha");
    let machine = next.frozen.machine.clone().unwrap();
    assert!(machine.points_closed.iter().any(|(id, why)| id.starts_with("open-anchor-full-") && why.contains("has room")), "{:?}", machine.points_closed);
    assert_eq!(outcome_of(&next, "plan-here"), MachineOutcome::NeedsAgent);
    assert_eq!(next.frozen.batch(), &["plan-here".to_string()]);
}

/// THE OWNER'S DECISION (step 26): a full place is the AGENT's to solve, by
/// THOR's rules - never "the owner decides". Field case 2026-10-01: the point
/// said the owner decides which fact gives way, the agent asked the owner, and
/// the owner answered "decide yourself by THOR's rules". When a fact on the
/// place is lighter, or carries no check that can refuse, the rule goes to the
/// agent's batch, and its line names every occupant with its severity and
/// whether it carries a check, and the three honest exits.
#[test]
fn a_full_place_with_a_light_occupant_is_the_agents_batch_naming_occupants_and_exits() {
    let mut fx = Fx::new();
    let alpha = fx.checkout("alpha", &[("files/PLAN.md", "the plan
")], true);
    fx.register("alpha", &alpha);
    let occupants = ["plan-note-a", "plan-note-b", "plan-note-c", "plan-note-d"];
    for id in occupants {
        let note = rule(id, &format!("A decision about the plan ({id})."), vec![path("files/PLAN.md")], None, Some("alpha"));
        fx.legacy(&Item { kind: Kind::Orientation, severity: None, ..note });
    }
    let project_binding = Binding::Target { kind: TargetKind::Project, value: "alpha".to_string() };
    let orientation = rule("plan-here", "The plan lives in files/PLAN.md of this repo.", vec![project_binding], Some(Check::PathExists { path: "files/PLAN.md".to_string() }), Some("alpha"));
    fx.legacy(&Item { kind: Kind::Orientation, severity: None, ..orientation });

    let start = fx.start(S1, "alpha");
    assert_eq!(outcome_of(&start, "plan-here"), MachineOutcome::NeedsAgent, "{:?}", item_of(&start, "plan-here"));
    assert_eq!(start.frozen.batch(), &["plan-here".to_string()]);
    let detail = item_of(&start, "plan-here").detail.clone();
    for id in occupants {
        assert!(detail.contains(&format!("'{id}' (no severity, no check")), "each occupant with its weight and check: {detail}");
    }
    for exit in ["FOLD", "MOVE", "RETRACT"] {
        assert!(detail.contains(exit), "the three honest exits: {detail}");
    }
    assert!(detail.contains("\"value\": \"files/PLAN.md\""), "then the binding that names the file: {detail}");
    assert!(!detail.contains("owner decides"), "{detail}");
    let points = model::store::open_points(&fx.store, Some("alpha"));
    assert!(!points.iter().any(|p| p.id.starts_with("open-anchor-full-")), "no point for the owner: {points:?}");
    assert!(start_note(&start).contains("'plan-note-a' (no severity, no check"), "{}", start_note(&start));
    assert!(model::store::settlement(&fx.store, "plan-here", None).is_owed(), "owed by the agent, not held by THOR");
}

/// Step 21: a per-project point for the rules with no project - the shape
/// the build before step 21 wrote, `open-no-working-copy-global-rules-in-<p>`
/// - is closed by the next start of that project, replaced by the one
/// store-wide point, so the rules are never held twice.
#[test]
fn a_per_project_point_for_global_rules_is_replaced_by_the_store_wide_one() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "keep me\n")], true);
    fx.register("beta", &beta);
    fx.legacy(&rule("glob-rel", "Every repo keeps `eol=lf` in .gitattributes.", vec![cmd("git add")], contains(".gitattributes", "eol=lf"), None));
    let old_id = "open-no-working-copy-global-rules-in-beta";
    let blocker = Blocker::global_rules();
    let old = Item {
        id: old_id.to_string(),
        kind: Kind::Report,
        text: "THOR's own open point: fixture text of the older build".to_string(),
        bindings: vec![],
        severity: None,
        project: Some("beta".to_string()),
        tags: model::store::machine_point_tags(&blocker),
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    let body = model::store::canonical_body(&old).unwrap();
    fx.store.append_event(S1, S1, model::store::EVAL_MACHINE_ACTOR, EventKind::FactCreated, old_id, None, &body).unwrap();
    assert!(model::store::is_machine_point(&fx.store, old_id), "fixture sanity: THOR's own point");

    let start = fx.start(S2, "beta");
    let machine = start.frozen.machine.clone().unwrap();
    assert_eq!(machine.points.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec![model::store::GLOBAL_RULES_POINT_ID]);
    assert!(machine.points_closed.iter().any(|(id, why)| id == old_id && why.contains("replaced by THOR's one store-wide point")), "{:?}", machine.points_closed);
    assert!(model::store::open_points(&fx.store, Some("beta")).is_empty(), "the old point is closed");
}

/// G14.4, second half, and R14.5: once the folder is registered, the next
/// start closes the point itself - reason 'the working copy is registered
/// now' - and tries those rules for real; no carried point remains.
#[test]
fn point_is_retracted_by_the_machine_when_its_subject_settles() {
    let mut fx = Fx::new();
    unregistered_gamma(&mut fx);
    let first = fx.start(S1, "gamma");
    assert_eq!(first.frozen.machine.as_ref().unwrap().blocked, 3);

    // The owner registers the folder. Its README holds two of the three
    // lines; the third rule's literal is nowhere.
    let gamma = fx.checkout("gamma", &[("README.md", "alpha line\nbeta line\nplain line\n")], true);
    fx.register("gamma", &gamma);
    let second = fx.start(S2, "gamma");
    let machine = second.frozen.machine.clone().unwrap();
    assert_eq!(machine.points_closed, vec![("open-no-working-copy-gamma".to_string(), "the working copy is registered now".to_string())]);
    assert!(machine.points.is_empty() && model::store::open_points(&fx.store, Some("gamma")).is_empty(), "no point remains");
    let closing = fx.events_of("open-no-working-copy-gamma").pop().unwrap();
    assert_eq!((closing.kind, closing.actor.as_str()), (EventKind::FactRetracted, "eval-machine"));
    assert!(closing.body.contains("the working copy is registered now"), "{}", closing.body);

    // Tested for real: two are rules now, the third is the agent's.
    assert_eq!(outcome_of(&second, "g-file-0"), MachineOutcome::Rule);
    assert_eq!(outcome_of(&second, "g-file-1"), MachineOutcome::Rule);
    assert_eq!(outcome_of(&second, "g-file-2"), MachineOutcome::NeedsAgent);
    assert_eq!(second.frozen.batch(), &["g-file-2".to_string()]);
    let err = file(&mut fx, &second, "eval-gamma-2").unwrap_err().to_string();
    assert!(err.contains("'g-file-2'") && !err.contains("'g-file-0'"), "{err}");
}

/// K9: a rule-track contains check whose literal was not committed when it
/// was tried is tried again at every start - and proven once it is. One
/// whose file did not change is left alone.
#[test]
fn an_uncommitted_contains_is_retried_at_the_start() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "intro only\n"), ("STABLE.md", "stays here\n")], true);
    fx.register("beta", &beta);
    fs::write(beta.join("NOTES.md"), "intro only\nkeep me\n").unwrap();
    fx.legacy(&rule("co", "NOTES.md keeps `keep me`.", vec![path("NOTES.md")], contains("NOTES.md", "keep me"), Some("beta")));
    fx.legacy(&rule("st", "STABLE.md keeps `stays here`.", vec![path("STABLE.md")], contains("STABLE.md", "stays here"), Some("beta")));

    let first = fx.start(S1, "beta");
    assert_eq!(outcome_of(&first, "co"), MachineOutcome::Rule);
    assert_eq!(model::store::current_verdict(&fx.store, "co"), CurrentVerdict::RuleWired, "uncommitted: wired");
    assert!(model::store::current_record(&fx.store, "co").unwrap().fields().uncommitted);
    assert_eq!(model::store::current_verdict(&fx.store, "st"), CurrentVerdict::RuleProven);

    // Still uncommitted at the next start: tried again, found the same,
    // nothing rewritten. The untouched file's rule is not even tried.
    let second = fx.start(S2, "beta");
    assert_eq!(second.frozen.machine.as_ref().unwrap().items.iter().map(|i| (i.id.as_str(), i.outcome)).collect::<Vec<_>>(), vec![("co", MachineOutcome::Unchanged)]);
    assert_eq!(fx.machine_writes("co").len(), 1);
    // The start note's TEETH sentence names every outcome, so its parts add
    // up to the number tried (field test 11: 'tried 36' summed to 35).
    assert!(start_note(&second).contains("tried 1 old rule(s)") && start_note(&second).contains("1 tried again and left as they were"), "{}", start_note(&second));

    git(&beta, &["commit", "-q", "-am", "keep me"]);
    let third = fx.start(S3, "beta");
    assert_eq!(outcome_of(&third, "co"), MachineOutcome::Rule);
    assert_eq!(model::store::current_verdict(&fx.store, "co"), CurrentVerdict::RuleProven, "committed: proven");
    let record = model::store::current_record(&fx.store, "co").unwrap();
    assert!(!record.fields().uncommitted && matches!(record.fields().evidence.as_slice(), [TrialEvidence::InHead { .. }]));
    assert_eq!(fx.machine_writes("co").len(), 2);
}

/// F10: `begin` writes the start record - judgement list and open points, no
/// batch - before the pass runs at all; every write of the pass lands after
/// it, and the batch record last.
#[test]
fn the_start_record_is_written_before_the_pass() {
    let mut fx = Fx::new();
    fx.legacy(&rule("subj", "Keep commit subjects under 60 characters.", vec![Binding::Moment(intent::Action::Commit)], None, Some("beta")));
    let began = begin(&fx.db, &mut fx.store, S1, "beta", "stop-ask").unwrap();
    assert!(began.fresh && !began.start.frozen.decided());
    let created = fx.events_of(&began.start.id);
    assert_eq!(created.len(), 1);
    assert!(!created[0].body.contains("\"batch\":") && !created[0].body.contains("\"machine\":"), "{}", created[0].body);
    assert!(created[0].body.contains(model::store::STILL_DECIDING), "the note says the lists are not there yet: {}", created[0].body);
    assert!(fx.machine_writes("subj").is_empty(), "the pass has not run");
    assert_eq!(open_evaluation(&fx.store, S1, "beta"), Some(began.start.clone()), "and the evaluation is open already");

    let decided = decide(&mut fx.store, began.start.clone(), WIDE);
    let record = fx.events_of(&decided.id);
    assert_eq!(record.iter().map(|e| e.kind).collect::<Vec<_>>(), vec![EventKind::FactCreated, EventKind::FactRevised]);
    let write = fx.machine_writes("subj").pop().expect("the pass wrote its verdict");
    assert!(record[0].seq < write.seq && write.seq < record[1].seq, "start record, then the pass, then the batch record");
    assert_eq!(decided.batch_seq, Some(record[1].seq));
    assert_eq!(decide(&mut fx.store, decided.clone(), WIDE), decided, "a decided start is never decided twice");
    assert_eq!(fx.events_of(&decided.id).len(), 2);
}

/// R14.7 and G14.6 (F10): a box too small to reach anything - the batch
/// record says how many were not reached, none of them holds the filing, and
/// the next start begins with them, in the order they were left. R14.8's
/// marker: a start record with no batch is finished by `resume`.
#[test]
fn the_pass_stops_at_its_time_box_and_resumes() {
    let mut fx = Fx::new();
    let ids: Vec<String> = (0..12).map(|n| format!("rot-{n:02}")).collect();
    for id in &ids {
        fx.legacy(&rule(id, &format!("Keep the {id} ledger under 60 lines."), vec![Binding::Moment(intent::Action::Commit)], None, Some("beta")));
    }
    // A box that is over before the first rule.
    let began = begin(&fx.db, &mut fx.store, S1, "beta", "stop-ask").unwrap();
    let first = decide(&mut fx.store, began.start, Duration::ZERO);
    let machine = first.frozen.machine.clone().unwrap();
    assert!(first.frozen.decided(), "the batch record is written whatever the pass reached");
    assert_eq!((machine.not_reached, machine.items.len()), (12, 0), "{machine:?}");
    assert_eq!(machine.not_reached_ids, ids, "oldest write first");
    assert!(start_note(&first).contains("not reached this time: 12"), "{}", start_note(&first));
    // The count says what it means: why, that it is not owed here, and who goes first next time.
    assert!(
        start_note(&first).contains("the time box ended before THOR reached 12 old rule(s); they are not owed in this evaluation and they lead the next start"),
        "{}",
        start_note(&first)
    );
    assert!(model::store::evaluation_frozen_batch_owed(&fx.store, &first).is_empty(), "what was not reached is not owed here");
    file(&mut fx, &first, "eval-beta-1").expect("none of the unreached rules holds the filing");

    // One more old rule, which fired: it would lead the pass on its own.
    fx.legacy(&rule("fired", "Keep the release notes under 40 lines.", vec![Binding::Moment(intent::Action::Commit)], None, Some("beta")));
    fx.served(S2, "fired", "git commit -m notes");
    let second = fx.start(S2, "beta");
    let machine = second.frozen.machine.clone().unwrap();
    assert_eq!((machine.not_reached, machine.notes), (0, 13), "a wide box decides every one: {machine:?}");
    let order: Vec<&str> = machine.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(&order[..12], ids.iter().map(String::as_str).collect::<Vec<_>>().as_slice(), "the next start begins with what was left");
    assert_eq!(order[12], "fired");

    // The marker of a hook killed between its two writes.
    fx.legacy(&rule("late", "Keep the changelog under 80 lines.", vec![Binding::Moment(intent::Action::Commit)], None, Some("beta")));
    fx.quiet_others(S3, "beta");
    let cut = begin(&fx.db, &mut fx.store, S3, "beta", "stop-ask").unwrap().start;
    assert!(!cut.frozen.decided());
    assert!(file(&mut fx, &cut, "eval-beta-cut").unwrap_err().to_string().contains(model::store::STILL_DECIDING));
    let resumed = resume(&mut fx.store, S3);
    assert_eq!(resumed.len(), 1);
    assert!(resumed[0].frozen.decided() && resumed[0].id == cut.id);
    assert_eq!(outcome_of(&resumed[0], "late"), MachineOutcome::Note);
    assert!(resume(&mut fx.store, S3).is_empty(), "nothing left to finish");
    assert!(resumed_note(&resumed[0]).contains("THOR finished deciding the old rules of 'beta'"));
}

/// F10: the pass never runs `git log -S` - the search that made a machine
/// pass outrun the hook timeout. An absent check whose literal once lived in
/// the file's history gets no history evidence from the pass, and this
/// module never names the search.
#[test]
fn the_pass_never_runs_git_log_s() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("conf.ini", "debug = true\nmode = fast\n")], true);
    fs::write(beta.join("conf.ini"), "mode = fast\n").unwrap();
    git(&beta, &["commit", "-q", "-am", "drop debug"]);
    fx.register("beta", &beta);
    let check = Some(Check::Absent { path: "conf.ini".to_string(), literal: "debug = true".to_string() });
    fx.legacy(&rule("no-debug", "conf.ini never holds `debug = true`.", vec![path("conf.ini")], check, Some("beta")));
    assert!(crate::prove::lived_in_history(&beta, "conf.ini", "debug = true").is_some(), "the fixture's history does hold it");

    let start = fx.start(S1, "beta");
    assert_eq!(outcome_of(&start, "no-debug"), MachineOutcome::Rule);
    let record = model::store::current_record(&fx.store, "no-debug").unwrap();
    assert!(
        !record.fields().evidence.iter().any(|e| matches!(e, TrialEvidence::LivedInHistory { .. })),
        "the pass searched no history: {:?}",
        record.fields().evidence
    );
    let source = include_str!("../eval_session.rs");
    assert!(!source.contains("lived_in_history") && !source.contains("\"-S\""), "the pass must never call the history search");
}

/// R14.9 and G14.7 (F11): a contains rule whose literal an uncommitted edit
/// removed from the registered working tree, while the default branch still
/// holds it - a machine blocker, one point about that file, never the
/// agent's. Once the edit is dropped the next start closes the point, and
/// the rule is a rule again without anyone touching it.
#[test]
fn working_tree_differs_is_a_machine_blocker() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "keep me\nplain line\n")], true);
    fx.register("beta", &beta);
    fx.legacy(&rule("co", "NOTES.md keeps `keep me`.", vec![path("NOTES.md")], contains("NOTES.md", "keep me"), Some("beta")));
    let first = fx.start(S1, "beta");
    assert_eq!(outcome_of(&first, "co"), MachineOutcome::Rule);

    fs::write(beta.join("NOTES.md"), "mid-edit\nplain line\n").unwrap();
    let second = fx.start(S2, "beta");
    let blocked = item_of(&second, "co");
    assert_eq!(blocked.outcome, MachineOutcome::Blocked, "{blocked:?}");
    assert_eq!(blocked.blocker.as_ref().map(|b| (b.code.as_str(), b.subject.as_str())), Some(("working-tree-differs", "NOTES.md")));
    assert_eq!(second.frozen.batch(), &[] as &[String], "not in the agent's batch");
    let points = model::store::open_points(&fx.store, Some("beta"));
    assert_eq!(points.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["open-working-tree-differs-beta-notes-md"]);
    assert!(points[0].machine && points[0].subject.as_deref() == Some("NOTES.md"));
    assert!(
        points[0].text.contains("NOTES.md in ")
            && points[0].text.contains("no longer holds 'keep me', while the default branch still does - the checkout is mid-change; nothing to do until that change is committed or dropped")
            && points[0].text.contains("'co'"),
        "{}",
        points[0].text
    );
    assert_eq!(model::store::current_verdict(&fx.store, "co"), CurrentVerdict::RuleProven, "the rule keeps its check and its verdict");
    file(&mut fx, &second, "eval-beta-2").expect("a working tree mid-change holds no filing");

    fs::write(beta.join("NOTES.md"), "keep me\nplain line\n").unwrap();
    let third = fx.start(S3, "beta");
    let machine = third.frozen.machine.clone().unwrap();
    assert_eq!(machine.points_closed.len(), 1, "{machine:?}");
    assert_eq!(machine.points_closed[0].0, "open-working-tree-differs-beta-notes-md");
    assert!(model::store::open_points(&fx.store, Some("beta")).is_empty());
    assert_eq!(model::store::current_verdict(&fx.store, "co"), CurrentVerdict::RuleProven, "a rule again, without the agent");
    assert_eq!(third.frozen.batch(), &[] as &[String]);
}

/// A literal gone from the working tree AND from the default branch is the
/// agent's: in the batch, and it holds the filing until the file holds the
/// literal again or the rule has a verdict written after the start.
#[test]
fn a_literal_gone_from_both_needs_the_agent() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "keep me\nplain line\n")], true);
    fx.register("beta", &beta);
    fx.legacy(&rule("co", "NOTES.md keeps `keep me`.", vec![path("NOTES.md")], contains("NOTES.md", "keep me"), Some("beta")));
    fx.start(S1, "beta");
    fs::write(beta.join("NOTES.md"), "rewritten\nplain line\n").unwrap();
    git(&beta, &["commit", "-q", "-am", "rewrite"]);

    let second = fx.start(S2, "beta");
    assert_eq!(outcome_of(&second, "co"), MachineOutcome::NeedsAgent);
    assert_eq!(second.frozen.batch(), &["co".to_string()]);
    assert!(model::store::open_points(&fx.store, Some("beta")).is_empty(), "no machine point: the agent can fix this");
    let err = file(&mut fx, &second, "eval-beta-2").unwrap_err().to_string();
    assert!(err.contains("'co'") && err.contains("NOTES.md does not contain 'keep me'"), "{err}");
    fs::write(beta.join("NOTES.md"), "rewritten\nkeep me\n").unwrap();
    file(&mut fx, &second, "eval-beta-2").expect("the file holds the literal again");
}

/// R14.10 (K23, Q24): twenty old rules whose checks bite and whose text only
/// fails to name the literal - sixteen on the A4 list with the exact revise
/// call each, four wait for the next start, none in the batch of eight.
#[test]
fn a4_only_items_get_their_own_list_of_16() {
    let mut fx = Fx::new();
    let ids: Vec<String> = (0..20).map(|n| format!("a4-{n:02}")).collect();
    for (n, id) in ids.iter().enumerate() {
        fx.legacy(&rule(
            id,
            &format!("Never wipe data volume {n} with the acme tool."),
            vec![cmd(&format!("acme-tool-{n:02}"))],
            forbidden(&[&format!("--wipe-{n:02}")]),
            Some("beta"),
        ));
    }
    let start = fx.start(S1, "beta");
    let machine = start.frozen.machine.clone().unwrap();
    assert_eq!((machine.a4_only, machine.needs_agent, machine.bites, machine.notes), (20, 0, 0, 0), "{machine:?}");
    assert_eq!(start.frozen.a4_list(), &ids[..16]);
    assert_eq!(start.frozen.batch(), &[] as &[String], "none in the batch of 8");
    // The record keeps the whole note; what the session is shown is the same
    // note cut to the size Claude Code shows, with the cut named.
    let shown = start_note(&start);
    assert!(shown.len() <= START_NOTE_BUDGET, "{} bytes", shown.len());
    let note = render_start_note(&start.id, &start.frozen);
    for (n, id) in ids.iter().enumerate().take(16) {
        let call = format!("mcp__thor__revise(id: \"{id}\", text: \"<its text, reworded so it names '--wipe-{n:02}'>\")");
        assert_eq!(item_of(&start, id).call.as_deref(), Some(call.as_str()));
        assert!(note.contains(&call), "{id}: {note}");
    }
    assert!(note.contains("4 more rule(s) like these wait for the next evaluation"), "{note}");
    // The room the rewording has (step 24, field defect 5): the A4 line says
    // how many characters are left, so a reword fits the first time.
    let used = "Never wipe data volume 0 with the acme tool.".chars().count();
    let budget = format!("text budget: {} of 300 characters left (the text is {used} now)", 300 - used);
    assert!(note.contains(&budget), "{budget}: {note}");
    assert!(!note.contains("mcp__thor__revise(id: \"a4-16\""), "the seventeenth is not this evaluation's: {note}");
    let err = file(&mut fx, &start, "eval-beta-1").unwrap_err().to_string();
    assert!(err.contains("'a4-00'") && err.contains("'a4-15'") && !err.contains("'a4-16'"), "{err}");

    // The agent names the literal in one of them: a write with its own
    // trial, after the start - settled. The next start's list moves up by
    // one: the fifteen still open, then the first of the four that waited.
    let before = model::store::show(&fx.store, "a4-00").unwrap();
    let mut named = before.clone();
    named.text = "Never wipe data volume 0 with `--wipe-00`.".to_string();
    let corpus = Corpus::load(&fx.store);
    let tried = {
        let env = teeth::Env { store: &fx.store, db: Some(&fx.db) };
        let root = project::registered_checkout(&fx.store, named.project.as_deref());
        let write = teeth::Write { item: &named, before: Some(&before), because: None, parent_rev: None };
        teeth::trial(&env, &write, &root, &teeth::SessionCtx::default(), &corpus, &teeth::Disputes::default(), &[])
    };
    let stored = teeth::stored_item(&named, &tried).expect("naming the literal makes it a rule");
    model::store::revise_tried_scoped(&mut fx.store, S1, "l", "mcp", &before, &stored, &before, None, None, tried.record.as_ref(), None, None).unwrap();
    assert!(!model::store::settlement(&fx.store, "a4-00", Some(&start)).is_owed());

    let next = fx.start(S2, "beta");
    assert_eq!(next.frozen.machine.as_ref().unwrap().a4_only, 19);
    assert_eq!(next.frozen.a4_list(), &ids[1..17]);
}

/// A rule stored by dispute, the way the write tools store one: THOR's own
/// trial with the three legitimate calls named, the writer's `because`
/// (when it gave one) in the event body. Distinct words per rule, or the
/// gate reads two of them as one fact.
fn disputed(fx: &mut Fx, n: usize, because: Option<&str>) -> String {
    let id = format!("bundle-{n:02}");
    let mut item = rule(
        &id,
        &format!("Prove w{n}a w{n}b w{n}c w{n}d with `git clone --mirror` plus fsck, never with `git bundle list-heads`."),
        vec![cmd("git bundle")],
        None,
        Some("demo"),
    );
    item.severity = None;
    let corpus = Corpus::load(&fx.store);
    let disputes = teeth::Disputes {
        legitimate_calls: vec![
            "git bundle list-heads backup.bundle".to_string(),
            "git clone --mirror origin.git backup.git".to_string(),
            "git clone origin.git work".to_string(),
        ],
        ..Default::default()
    };
    let tried = {
        let env = teeth::Env { store: &fx.store, db: Some(&fx.db) };
        let root = project::registered_checkout(&fx.store, item.project.as_deref());
        let write = teeth::Write { item: &item, before: None, because: None, parent_rev: None };
        teeth::trial(&env, &write, &root, &teeth::SessionCtx::default(), &corpus, &disputes, &[])
    };
    let stored = teeth::stored_item(&item, &tried).expect("a dispute makes it a note");
    model::store::declare_in_scoped_because(&mut fx.store, "s", "l", "a", &stored, None, None, tried.record.as_ref(), None, because).unwrap();
    id
}

/// A rule stored by a may_change dispute on `pkg<n>.json` in demo's working
/// copy: its own place (a path anchor), so ten of them fit.
fn disputed_span(fx: &mut Fx, n: usize, because: Option<&str>) -> String {
    let id = format!("span-{n:02}");
    let mut item = rule(
        &id,
        &format!("Bump the 'version' in pkg{n}.json with every release of w{n}a w{n}b w{n}c."),
        vec![path(&format!("pkg{n}.json"))],
        None,
        Some("demo"),
    );
    item.severity = None;
    let corpus = Corpus::load(&fx.store);
    let disputes = teeth::Disputes { may_change: vec!["version".to_string()], ..Default::default() };
    let tried = {
        let env = teeth::Env { store: &fx.store, db: Some(&fx.db) };
        let root = project::registered_checkout(&fx.store, item.project.as_deref());
        let write = teeth::Write { item: &item, before: None, because: None, parent_rev: None };
        teeth::trial(&env, &write, &root, &teeth::SessionCtx::default(), &corpus, &disputes, &[])
    };
    let stored = teeth::stored_item(&item, &tried).expect("a dispute makes it a note");
    model::store::declare_in_scoped_because(&mut fx.store, "s", "l", "a", &stored, None, None, tried.record.as_ref(), None, because).unwrap();
    id
}

/// What the agent is handed for a dispute: the item with its argument, its
/// spans and the writer's reason (or "no reason recorded"), the exact call
/// that hangs the real check, the exact call that confirms the dispute, when
/// it is done, and one example.
#[test]
fn a_dispute_written_since_the_previous_report_is_on_the_start_note_with_its_calls() {
    let mut fx = Fx::new();
    let why = "list-heads is only the example here; the rule is about proving a bundle by cloning it";
    let reasoned = disputed(&mut fx, 1, Some(why));
    let silent = disputed(&mut fx, 2, None);
    let start = fx.start(S1, "demo");
    let ids: Vec<String> = start.frozen.dispute_list().iter().map(|d| d.id.clone()).collect();
    assert_eq!(ids, [reasoned.clone(), silent.clone()]);
    assert_eq!(start.frozen.dispute_list()[0].because.as_deref(), Some(why));
    assert_eq!(start.frozen.dispute_list()[1].because, None);

    let note = start_note(&start);
    assert!(note.contains("c. DISPUTES TO VERIFY - 2 rule(s) stored by dispute"), "{note}");
    assert!(note.contains("WHAT THIS MEANS:") && note.contains("WHEN AN ITEM IS DONE:") && note.contains("EXAMPLE:"), "{note}");
    assert!(note.contains(&format!("writer's reason: '{}", &why[..45])), "{note}");
    assert!(note.contains("no reason recorded"), "{note}");
    for id in [&reasoned, &silent] {
        assert!(
            note.contains(&format!(
                "Call 1 (hang the check): mcp__thor__revise(id: \"{id}\", text: \"<its text, reworded to name the subject>\", \
                 check_kind: \"forbidden\", check_literals: [\"<the subject>\"], because: \"<only if it drops a quoted form"
            )),
            "{id}: {note}"
        );
        assert!(note.contains(&format!("Call 2 (confirm the dispute): mcp__thor__revise(id: \"{id}\", because: \"<why 'git bundle list-heads backup.bundle' is legitimate, 20+ chars>\")")), "{id}: {note}");
    }
    assert!(note.contains("WHEN AN ITEM IS DONE: THOR accepted the call"), "{note}");
    assert!(note.contains("under 20 characters is refused"), "call 2 says THOR enforces the length: {note}");
    assert!(note.contains("or you retracted a rule that is simply wrong"), "a retract is named as the other exit: {note}");
    // Frozen: the record carries them, so the note and the record agree.
    assert_eq!(model::store::frozen_ids(&start).iter().filter(|id| id.starts_with("bundle-")).count(), 2);
    // Nothing to verify: the section still says so, in one line.
    let mut quiet = Fx::new();
    let none = quiet.start(S1, "demo");
    assert!(start_note(&none).contains("c. DISPUTES TO VERIFY - no rule stored by dispute since the previous evaluation waits to be verified."));
}

/// THE SIGNALS BEFORE FILING: the start note carries THOR's health and the
/// refusal outcomes known at the start, each with its meaning, the exact calls,
/// when it is done and an example - so the agent can act before it files, not
/// only read about them after.
#[test]
fn the_start_note_shows_health_and_refusal_outcomes_known_so_far_with_their_calls() {
    let mut fx = Fx::new();
    let demo = fx.checkout("demo", &[("NOTES.md", "x\n")], true);
    fx.register("demo", &demo);
    // Raw fixture events, the way the attester's records read on the log: a test
    // holds no token of THOR's own.
    let fault = serde_json::json!({"hook_fault": {
        "tool_use_id": "toolu_01FaultExample", "hook_event": "PreToolUse", "exit_code": 126, "at": "2026-10-07T09:12:00.000Z" }});
    fx.store.append_event(S2, S2, model::store::ATTESTER_ACTOR, EventKind::GateStoodAside, &model::store::hook_fault_entity(S2), None, &fault.to_string()).unwrap();
    fx.legacy(&rule("rule-zeta", "Never run `git push --force` on the main branch of this project.", vec![cmd("git push")], forbidden(&["--force"]), Some("demo")));
    let body = r#"{"at":"2026-10-07T09:20:00Z","target":"git push --force-with-lease origin main","tool_use_id":"toolu_02"}"#;
    let seq = fx.store.append_event(S2, S2, "hook", EventKind::GateRefused, "rule-zeta", None, body).unwrap().seq;
    let verified = serde_json::json!({"refusal_seq": seq, "field": true, "why": "fixture"});
    fx.store.append_event(S2, S2, model::store::ATTESTER_ACTOR, EventKind::GateStoodAside, "rule-zeta", None, &verified.to_string()).unwrap();
    let zeta = model::store::show(&fx.store, "rule-zeta").unwrap();
    let mut cleared = zeta.clone();
    cleared.check = None;
    let baseline = model::gate::ClearedFields { check: true, ..Default::default() }.baseline(&zeta);
    model::store::revise_because(&mut fx.store, S2, "l", "mcp", &baseline, &cleared, &zeta, Some("it refused a lease push, which is safe")).unwrap();

    let start = fx.start(S1, "demo");
    assert!(start.frozen.machine.as_ref().unwrap().signals.is_some(), "frozen with the start");
    let note = start_note(&start);
    let section = &note[note.find("\n\n3. SIGNALS BEFORE YOU FILE").expect(&note)..];
    assert!(
        section.contains("\n   a. THOR health since the previous report: 1 hook launch failure(s) (PreToolUse, exit 126, session 373e8610, at 2026-10-07T09:12:00Z), 0 version-skew event(s)."),
        "{section}"
    );
    assert!(
        section.contains("\n   b. REFUSAL OUTCOMES since the previous report: 1 verified refusal(s), 1 followed by a weakening or retract"),
        "{section}"
    );
    assert!(section.contains("\n      - 'rule-zeta': refused 'git push --force-with-lease origin main' at log seq"), "{section}");
    assert!(section.contains(", then weakened (check cleared) at log seq"), "{section}");
    assert!(section.contains("\n         Read: mcp__thor__history(id: \"rule-zeta\")"), "{section}");
    assert!(
        section.contains(
            "\n         If the refusal was right, restore: mcp__thor__revise(id: \"rule-zeta\", check_kind: \"forbidden\", \
             check_literals: [\"--force\"], because: \"<why the weakening was a mistake - at least 20 characters>\")"
        ),
        "{section}"
    );
    for part in ["WHAT THIS MEANS:", "WHAT TO DO", "DONE WHEN:", "EXAMPLE:"] {
        assert_eq!(section.matches(part).count(), 2, "both signals say {part}: {section}");
    }
    assert!(section.is_ascii(), "plain ASCII");

    // Nothing to report: each signal says so and what to do about it.
    let mut quiet = Fx::new();
    let none = quiet.start(S1, "demo");
    let note = start_note(&none);
    assert!(note.contains("\n   a. THOR health since the previous report: 0 hook launch failure(s), 0 version-skew event(s)."), "{note}");
    assert!(note.contains("\n   b. REFUSAL OUTCOMES since the previous report: no verified refusal since the previous report to follow."), "{note}");
    assert_eq!(note.matches("Nothing to do here.").count(), 2, "{note}");
}

/// At most the batch's own cap of eight per evaluation; the rest are named
/// in the record and listed first by the next start, which an answered one
/// never re-enters.
#[test]
fn disputes_follow_the_batch_cap_and_the_rest_come_first_next_time() {
    let mut fx = Fx::new();
    let files: Vec<(String, String)> = (0..10).map(|n| (format!("pkg{n}.json"), "{\"version\":\"1.0.0\"}\n".to_string())).collect();
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
    let demo = fx.checkout("demo", &refs, true);
    fx.register("demo", &demo);
    let ids: Vec<String> =
        (0..10).map(|n| disputed_span(&mut fx, n, Some("a reason of at least twenty characters, written for the test"))).collect();
    let start = fx.start(S1, "demo");
    let machine = start.frozen.machine.clone().unwrap();
    let listed: Vec<String> = machine.disputes.iter().map(|d| d.id.clone()).collect();
    assert_eq!(listed, &ids[..8]);
    assert_eq!(machine.disputes_later_ids, &ids[8..]);
    let note = start_note(&start);
    assert!(note.contains("2 more dispute(s) wait for the next evaluation, which lists them first."), "{note}");
    assert!(!note.contains("Call 1 (hang the check): mcp__thor__revise(id: \"span-08\""), "{note}");
    assert!(
        note.contains("check_kind: \"contains\", check_path: \"pkg0.json\", check_literal: \"<the subject>\""),
        "a span dispute hangs a contains check on the file the rule is bound to: {note}"
    );
    assert!(note.contains("because: \"<why 'version' may change, 20+ chars>\""), "{note}");

    // A later evaluation after a report was filed since: the two that did not
    // fit are written BEFORE that report and still come first.
    let report_id = report("eval-demo-1", "demo");
    let body = model::store::canonical_body(&report_id).unwrap();
    fx.store.append_event("s", "eval", "agent", EventKind::FactCreated, &report_id.id, None, &body).unwrap();
    let carried = model::store::unverified_disputes(&fx.store, "demo", model::store::newest_evaluation_report_seq(&fx.store, Some("demo")), &machine.disputes_later_ids);
    let again: Vec<String> = carried.iter().map(|d| d.id.clone()).collect();
    assert_eq!(again, &ids[8..], "only the left-over ones, nothing else is newer than the report");
    let none = model::store::unverified_disputes(&fx.store, "demo", model::store::newest_evaluation_report_seq(&fx.store, Some("demo")), &[]);
    assert!(none.is_empty(), "left alone they fall out of the window: {none:?}");
}

/// G14.5 (K20): a note that kept its check pending because its project had
/// no working copy - once the project is registered and the file holds the
/// literal, the start hangs the check itself, and lists it as hung.
#[test]
fn a_pending_check_is_hung_when_it_bites() {
    let mut fx = Fx::new();
    let item = rule("g-rule", "Keep `x marks it` in README.md.", vec![path("README.md")], contains("README.md", "x marks it"), Some("gamma"));
    // Written from a session in another project: a note, the check pending.
    let corpus = Corpus::load(&fx.store);
    let session = teeth::SessionCtx { own_checkout: None, own_project: Some("alpha".to_string()), session_id: Some(S1.to_string()) };
    let tried = {
        let env = teeth::Env { store: &fx.store, db: Some(&fx.db) };
        let root = project::registered_checkout(&fx.store, item.project.as_deref());
        let write = teeth::Write { item: &item, before: None, because: None, parent_rev: None };
        teeth::trial(&env, &write, &root, &session, &corpus, &teeth::Disputes::default(), &[])
    };
    let stored = teeth::stored_item(&item, &tried).unwrap();
    model::store::declare_in_scoped(&mut fx.store, "s", "l", "a", &stored, None, None, tried.record.as_ref(), None).unwrap();
    assert_eq!(
        model::store::settlement(&fx.store, "g-rule", None),
        Settlement::Settled(Settled::Note { pending: Some(PendingWhy::NoWorkingCopy), by_dispute: false })
    );

    // Still unregistered: the note stays a note, held by nothing.
    let first = fx.start(S1, "gamma");
    assert_eq!(first.frozen.machine.as_ref().unwrap().pending_hung, 0);
    assert_eq!(outcome_of(&first, "g-rule"), MachineOutcome::PendingKept);
    assert_eq!(first.frozen.machine.as_ref().unwrap().blocked, 0, "a note is settled: no blocker holds it");
    assert!(model::store::open_points(&fx.store, Some("gamma")).is_empty(), "and it gets no open point");
    assert_eq!(model::store::show(&fx.store, "g-rule").unwrap().check, None);

    let gamma = fx.checkout("gamma", &[("README.md", "x marks it\n")], true);
    fx.register("gamma", &gamma);
    let second = fx.start(S2, "gamma");
    assert_eq!(outcome_of(&second, "g-rule"), MachineOutcome::PendingHung);
    assert_eq!(second.frozen.machine.as_ref().unwrap().pending_hung, 1);
    let now = model::store::show(&fx.store, "g-rule").unwrap();
    assert_eq!(now.check, item.check, "the machine hangs the author's own check");
    assert_eq!(fx.machine_writes("g-rule").len(), 1, "as teeth-eval");
    assert!(matches!(model::store::current_verdict(&fx.store, "g-rule"), CurrentVerdict::RuleProven | CurrentVerdict::RuleWired));
    assert!(start_note(&second).contains("pending checks hung: 'g-rule'"), "{}", start_note(&second));
}

/// X9: a rule the trial puts on the rule track, at a binding full of heavier
/// rivals - the apply is refused for capacity, nothing is written, and the
/// rule is the agent's ("crowded": fold it, or move it to a narrower anchor).
#[test]
fn a_capacity_refusal_on_apply_needs_the_agent() {
    let mut fx = Fx::new();
    let mut light = rule("light", "Never run `git push --force` on main.", vec![cmd("git push")], forbidden(&["--force"]), Some("beta"));
    light.severity = None;
    fx.legacy(&light);
    for n in 0..4 {
        fx.legacy(&rule(&format!("heavy-{n}"), &format!("Heavy rival {n} of the push."), vec![cmd("git push")], None, Some("beta")));
    }
    let start = fx.start(S1, "beta");
    let crowded = item_of(&start, "light");
    assert_eq!(crowded.outcome, MachineOutcome::NeedsAgent, "{crowded:?}");
    assert!(crowded.detail.starts_with("crowded at git push: fold this rule into a neighbour"), "{}", crowded.detail);
    assert!(fx.machine_writes("light").is_empty(), "nothing written");
    assert!(start.frozen.batch().contains(&"light".to_string()));
    let call = crowded.call.as_deref().unwrap();
    assert!(call.starts_with("mcp__thor__revise(id: \"light\", <the change its line names>, because: \"<needed when the change takes a quoted"), "{call}");
    assert!(call.contains("20+ characters"), "{call}");
}

/// Q24 at the machine pass: an old contains check whose text does not name
/// its literal, and that bites in the registered working copy, is decided by
/// the machine with the fragment it guards in the record - it is not handed
/// to the agent. The store takes that mark from THOR's retest only, so this
/// one verdict is written as `retest`, not as `teeth-eval`.
#[test]
fn an_old_contains_check_is_decided_by_the_machine() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "keep me\nplain line\n")], true);
    fx.register("beta", &beta);
    fx.legacy(&rule("old", "The notes file keeps its first line.", vec![path("NOTES.md")], contains("NOTES.md", "keep me"), Some("beta")));
    let start = fx.start(S1, "beta");
    assert_eq!(outcome_of(&start, "old"), MachineOutcome::Rule, "{:?}", item_of(&start, "old"));
    assert!(fx.machine_writes("old").is_empty(), "never as teeth-eval: the store refuses the mark from that name");
    let write = fx.events_of("old").into_iter().rev().find(|e| e.actor == "retest").expect("written as THOR's retest");
    assert_eq!(item_of(&start, "old").seq, Some(write.seq));
    let record = model::store::trial_of(&write).unwrap().unwrap();
    assert!(record.fields().a4_old_stock.is_some(), "the record shows the fragment it guards");
    assert!(!model::store::settlement(&fx.store, "old", None).is_owed());
    assert!(model::store::a4_cutoff(&fx.store).is_some(), "and the cutoff is recorded once");
}

/// STEP 27 (D4): the full-place point an older build wrote still says the owner
/// decides which fact gives way. The first session start in its project judges
/// it again under the current rule: reworded while a full place of heavier
/// guards still holds a rule, closed once it no longer does - and never touched
/// a second time.
fn old_anchor_full_point(fx: &mut Fx) -> (String, Blocker) {
    let alpha = fx.checkout("alpha", &[("files/PLAN.md", "the plan\n")], true);
    fx.register("alpha", &alpha);
    for id in ["plan-guard-a", "plan-guard-b", "plan-guard-c", "plan-guard-d"] {
        let mut guard = rule(id, &format!("files/PLAN.md keeps `the plan` ({id})."), vec![path("files/PLAN.md")], contains("files/PLAN.md", "the plan"), Some("alpha"));
        guard.severity = Some(model::item::Severity::Costly);
        fx.legacy(&guard);
        crate::teeth::fixture::decide(&mut fx.store, id);
    }
    let project_binding = Binding::Target { kind: TargetKind::Project, value: "alpha".to_string() };
    let orientation = rule("plan-here", "The plan lives in files/PLAN.md of this repo.", vec![project_binding], Some(Check::PathExists { path: "files/PLAN.md".to_string() }), Some("alpha"));
    fx.legacy(&Item { kind: Kind::Orientation, severity: None, ..orientation });
    fx.start(S1, "alpha");
    let blocker = Blocker::AnchorFull { path: "files/PLAN.md".to_string() };
    let id = model::store::machine_point_id("alpha", &blocker);
    let old = "THOR's own open point: files/PLAN.md already holds 4 live fact(s). Nothing needs doing from a session. The owner decides which fact gives way.";
    model::store::write_machine_point(&mut fx.store, eval_machine(), S1, "alpha", &blocker, old).unwrap();
    (id, blocker)
}

#[test]
fn an_old_full_place_point_still_held_is_reworded_at_the_next_session_start() {
    let mut fx = Fx::new();
    let (id, _) = old_anchor_full_point(&mut fx);
    let text_of = |fx: &Fx| model::store::open_points(&fx.store, Some("alpha")).into_iter().find(|p| p.id == id).map(|p| p.text);
    assert!(text_of(&fx).unwrap().contains("owner decides which fact gives way"));

    let done = refresh_old_anchor_full_points(&mut fx.store, "alpha");
    assert_eq!(done, vec![id.clone()]);
    let text = text_of(&fx).expect("still open: every occupant is a heavier guard");
    assert!(!text.contains("owner decides"), "{text}");
    assert!(text.contains("no fact there honestly gives way") && text.contains("'plan-here'"), "{text}");

    assert!(refresh_old_anchor_full_points(&mut fx.store, "alpha").is_empty(), "nothing left to judge the second time");
}

#[test]
fn the_housekeeping_at_a_session_start_carries_no_sessions_id() {
    let mut fx = Fx::new();
    let (id, _) = old_anchor_full_point(&mut fx);
    let before = fx.store.get_all_events().unwrap().len();
    let done = refresh_old_anchor_full_points(&mut fx.store, "alpha");
    assert_eq!(done, vec![id]);
    let events = fx.store.get_all_events().unwrap();
    assert!(events.len() > before, "the refresh must have written");
    for event in &events[before..] {
        assert_eq!(event.session_id, MACHINE_SESSION, "seq {} carries a session's id", event.seq);
        assert_ne!(event.session_id, S2);
        assert_eq!(event.actor, "eval-machine");
    }
}

#[test]
fn an_old_full_place_point_whose_place_has_room_is_closed_at_the_next_session_start() {
    let mut fx = Fx::new();
    let (id, _) = old_anchor_full_point(&mut fx);
    model::store::retract(&mut fx.store, "s", "l", "a", "plan-guard-d", "fixture: folded into 'plan-guard-a' about files/PLAN.md", None).unwrap();

    let done = refresh_old_anchor_full_points(&mut fx.store, "alpha");
    assert_eq!(done, vec![id.clone()]);
    assert!(!model::store::open_points(&fx.store, Some("alpha")).iter().any(|p| p.id == id), "closed: the place has room again");
}

#[test]
fn a_point_written_by_this_build_is_left_alone() {
    let mut fx = Fx::new();
    let (id, blocker) = old_anchor_full_point(&mut fx);
    let fresh = "THOR's own open point: files/PLAN.md holds heavier guards; no fact there honestly gives way.";
    model::store::write_machine_point(&mut fx.store, eval_machine(), S1, "alpha", &blocker, fresh).unwrap();
    assert!(refresh_old_anchor_full_points(&mut fx.store, "alpha").is_empty());
    assert!(model::store::open_points(&fx.store, Some("alpha")).iter().any(|p| p.id == id && p.text == fresh));
}

// ------------------------------------------- a verdict an older matcher made

/// A rule written straight into the log with a verdict an older build
/// decided: its trial record claims `verdict` and names `matcher_rev` (none
/// for a build that recorded none). The record is spliced in by hand, as an
/// older store holds it.
fn decided_long_ago(fx: &mut Fx, item: &Item, verdict: model::store::RecordedVerdict, matcher_rev: Option<&str>) {
    let fields = model::store::TrialFields {
        v: model::store::TRIAL_RECORD_VERSION,
        fingerprint: model::store::fingerprint(item),
        root: None,
        project: model::normalize::normalize_project(item.project.as_deref()),
        head_commit: None,
        candidates: Vec::new(),
        attempts: Vec::new(),
        corpus: model::store::TrialCorpus { reached: 0, whole_hits: 0, cut: 0, stopped_forcing: Vec::new() },
        clean_failed: Vec::new(),
        disputes: model::store::TrialDisputes { legitimate_calls: Vec::new(), may_change: Vec::new() },
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
        matcher_rev: matcher_rev.map(str::to_string),
    };
    let body = model::store::canonical_body(item).unwrap();
    let body = format!("{},\"teeth_trial\":{}}}", &body[..body.len() - 1], serde_json::to_string(&fields).unwrap());
    fx.store.append_event("legacy", &item.id, "migration", EventKind::FactCreated, &item.id, None, &body).unwrap();
}

/// The field case b-559c4de06b30d8d3: a ban whose built forms are only partly
/// refused. The current matcher puts it on the note track.
fn partly_refused_ban(id: &str) -> Item {
    rule(
        id,
        "Never hard-reset the deploy checkouts alpha-dev and beta-prod. Refused: `alpha-dev reset`, `beta-prod reset`, `alpha-dev && git reset`, `beta-prod && git reset`.",
        vec![cmd("git")],
        forbidden(&["alpha-dev reset", "beta-prod reset", "alpha-dev && git reset", "beta-prod && git reset"]),
        Some("beta"),
    )
}

fn under_matcher(rev: &str, body: impl FnOnce()) {
    crate::teeth::MATCHER_REV_OVERRIDE.with(|r| *r.borrow_mut() = Some(rev.to_string()));
    body();
    crate::teeth::MATCHER_REV_OVERRIDE.with(|r| *r.borrow_mut() = None);
}

/// A rule an older matcher decided is tried again at the start: the same kind
/// of verdict writes NOTHING to the log (no `fact_revised` to stamp the
/// matcher), this build's revision is remembered beside the store, and the
/// next start leaves it alone. A newer matcher revision tries it again.
#[test]
fn a_verdict_an_older_matcher_made_is_tried_again_once_and_writes_nothing() {
    let mut fx = Fx::new();
    fx.legacy(&rule("fp", "Never run `git push --force` on main.", vec![cmd("git push")], forbidden(&["--force"]), Some("beta")));
    fx.legacy(&rule("subj", "Keep commit subjects under 60 characters.", vec![Binding::Moment(intent::Action::Commit)], None, Some("beta")));
    let mut first = None;
    under_matcher("old-matcher", || first = Some(fx.start(S1, "beta")));
    assert_eq!(outcome_of(&first.unwrap(), "fp"), MachineOutcome::Rule);
    assert_eq!(crate::teeth::decided_by(&fx.store, "fp"), Some(Some("old-matcher".to_string())));
    assert_eq!(crate::teeth::decided_by(&fx.store, "subj"), Some(Some("old-matcher".to_string())));

    let events_before = fx.machine_writes("fp").len() + fx.machine_writes("subj").len();
    let second = fx.start(S2, "beta");
    for id in ["fp", "subj"] {
        assert_eq!(outcome_of(&second, id), MachineOutcome::Unchanged, "{id}: the same kind of verdict");
        assert_eq!(fx.machine_writes(id).len(), 1, "{id}: no stamp written for the new matcher");
        assert_eq!(item_of(&second, id).seq, None, "{id}: nothing was written");
    }
    assert_eq!(fx.machine_writes("fp").len() + fx.machine_writes("subj").len(), events_before, "no write at all");
    assert_eq!(model::store::current_verdict(&fx.store, "fp"), CurrentVerdict::RuleWired);
    assert_eq!(model::store::current_verdict(&fx.store, "subj"), CurrentVerdict::Note);
    let tried = crate::teeth::read_matcher_tried(&fx.db);
    assert!(tried.contains("fp") && tried.contains("subj"), "remembered beside the store: {tried:?}");

    // The next start has nothing owed, and the log did not grow for the rules.
    let third = fx.start(S3, "beta");
    assert!(third.frozen.machine.as_ref().unwrap().items.is_empty(), "nothing is owed any more: {:?}", third.frozen.machine);
    assert_eq!(fx.machine_writes("fp").len(), 1);

    // A lost file only costs one more trial, never a write.
    std::fs::remove_file(fx.dir.path().join(crate::teeth::MATCHER_TRIED_FILE)).unwrap();
    let fourth = fx.start(S4, "beta");
    assert_eq!(outcome_of(&fourth, "fp"), MachineOutcome::Unchanged);
    assert_eq!(fx.machine_writes("fp").len(), 1);

    // Another edit to the matcher sources: tried once more, still no write.
    under_matcher("newer-matcher", || {
        let fifth = fx.start(S5, "beta");
        assert_eq!(outcome_of(&fifth, "fp"), MachineOutcome::Unchanged);
        assert_eq!(fx.machine_writes("fp").len(), 1, "no write after a second matcher edit either");
        let tried = crate::teeth::read_matcher_tried(&fx.db);
        assert!(tried.contains("fp"), "{tried:?}");
    });
}

/// The machine pass's stamp-free refresh is no door at all: the rules it
/// compared are not rewritten, so no THOR-actor write lands on an evaluation's
/// frozen lists and nothing is left for the weakening readers to skip.
#[test]
fn an_unchanged_rule_compared_with_a_newer_matcher_leaves_the_history_alone() {
    let mut fx = Fx::new();
    fx.legacy(&rule("fp", "Never run `git push --force` on main.", vec![cmd("git push")], forbidden(&["--force"]), Some("beta")));
    under_matcher("old-matcher", || {
        fx.start(S1, "beta");
    });
    let history_before = fx.store.get_events_by_entity("fp").unwrap().len();
    fx.start(S2, "beta");
    assert_eq!(fx.store.get_events_by_entity("fp").unwrap().len(), history_before, "history did not grow");
}

/// A rule that bit when an older build tried it and that this build would
/// make a note keeps its tooth: the machine writes nothing, and the rule goes
/// to the agent's batch with its reason.
#[test]
fn a_demotion_by_a_newer_matcher_goes_to_the_batch_never_to_the_log() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "x\n")], true);
    fx.register("beta", &beta);
    decided_long_ago(&mut fx, &partly_refused_ban("ban"), model::store::RecordedVerdict::RuleWired, Some("old-matcher"));
    assert_eq!(model::store::current_verdict(&fx.store, "ban"), CurrentVerdict::RuleWired);

    let start = fx.start(S1, "beta");
    assert_eq!(outcome_of(&start, "ban"), MachineOutcome::NeedsAgent, "{:?}", start.frozen.machine);
    let item = item_of(&start, "ban");
    assert!(item.detail.starts_with("DEMOTION, nothing written: it bit when matcher old-matcher tried it"), "{}", item.detail);
    // Exit (a): the real command, the real store, forward slashes, no
    // placeholder; and the agent decides, not the owner.
    let accept = format!("{} --apply", model::store::retest_command_for(Some(&fx.db), "ban"));
    assert!(item.detail.contains(&accept), "{}\nwanted: {accept}", item.detail);
    assert!(accept.contains("--db ") && accept.contains("retest --id 'ban' --apply") && !accept.contains('\\'), "{accept}");
    assert!(!item.detail.contains("<their store>") && !item.detail.contains("owner agrees"), "{}", item.detail);
    // Exit (b): the revise THOR accepts, with the literals the check catches.
    let call = item.call.clone().expect("a demotion line carries a call");
    assert_ne!(call, "mcp__thor__revise(id: \"ban\")", "a bare revise changes nothing and is never the call");
    assert!(call.starts_with("mcp__thor__revise(id: \"ban\", text: \"<its text, reworded so it names only these forms:"), "{call}");
    assert!(call.contains("check_kind: \"forbidden\", check_literals: [\"alpha-dev reset\", \"beta-prod reset\"], because: \"<"), "{call}");
    assert!(start.frozen.batch().contains(&"ban".to_string()), "{:?}", start.frozen.batch());
    assert!(fx.machine_writes("ban").is_empty(), "a demotion is never applied by the machine");
    assert_eq!(model::store::current_verdict(&fx.store, "ban"), CurrentVerdict::RuleWired, "it keeps its tooth");
}

/// A demoted rule's start, in a store of its own: 'ban' bit under
/// 'old-matcher' and the current matcher would make it a note.
fn demoted_start() -> (Fx, EvaluationStart) {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "x\n")], true);
    fx.register("beta", &beta);
    decided_long_ago(&mut fx, &partly_refused_ban("ban"), model::store::RecordedVerdict::RuleWired, Some("old-matcher"));
    let start = fx.start(S1, "beta");
    (fx, start)
}

/// The literals a printed call names in `check_literals: [...]`.
fn literals_of(call: &str) -> Vec<String> {
    let from = call.find("check_literals: [").expect(call) + "check_literals: [".len();
    let to = from + call[from..].find(']').expect(call);
    serde_json::from_str(&format!("[{}]", &call[from..to])).expect(call)
}

/// Both exits a DEMOTION line prints settle the item for the filing gate.
/// (a) accept: the printed command, tried as `serve retest --id --apply` does,
/// writes the note. (b) keep the tooth: the printed revise, with the literals
/// it names and a reason, is accepted by THOR's trial and leaves the rule on
/// the rule track. A bare revise is neither: it changes nothing.
#[test]
fn either_exit_of_a_demotion_settles_the_batch_item_and_a_bare_revise_does_not() {
    // A bare revise is no exit.
    let (mut fx, start) = demoted_start();
    assert_eq!(model::store::evaluation_frozen_batch_owed(&fx.store, &start).len(), 1);
    {
        let mut corpus = crate::legit::Corpus::load(&fx.store);
        let head = model::store::show(&fx.store, "ban").unwrap();
        let bare = teeth::trial_for_write(&mut fx.store, Some(&fx.db), &teeth::SessionCtx::default(), &mut corpus, &head, Some(&head), None, &teeth::Disputes::default(), true);
        let teeth::WriteTrial::Unchanged { receipt, .. } = bare else { panic!("a bare revise writes no verdict") };
        assert_eq!(model::store::evaluation_frozen_batch_owed(&fx.store, &start).len(), 1, "still owed");
        // And the reply says so: not "teeth unchanged" alone, as if it were done.
        let said = receipt.join(" | ");
        assert!(said.starts_with("NOTHING WAS SETTLED, 'ban' IS STILL OWED"), "{said}");
        assert!(said.contains("TWO EXITS") && said.contains("retest --id 'ban' --apply"), "the exits are printed: {said}");
        assert!(said.contains("The call that settles it: mcp__thor__revise(id: \"ban\", text:"), "{said}");
        assert!(said.contains("teeth unchanged"), "{said}");
    }

    // Exit (a): accept the demotion.
    let corpus = crate::legit::Corpus::load(&fx.store);
    let tried = teeth::retest(&fx.store, Some(&fx.db), "ban", &corpus).unwrap();
    assert!(matches!(tried.outcome, teeth::Maintenance::Note { .. }), "{:?}", tried.outcome);
    teeth::apply_as_retest(&mut fx.store, &tried).unwrap();
    assert!(model::store::evaluation_frozen_batch_owed(&fx.store, &start).is_empty(), "accepted: settled");
    assert_eq!(model::store::current_verdict(&fx.store, "ban"), CurrentVerdict::Note);

    // Exit (b): keep the tooth by narrowing, with the literals the call names.
    let (mut fx, start) = demoted_start();
    let call = item_of(&start, "ban").call.clone().unwrap();
    let literals = literals_of(&call);
    assert_eq!(literals, ["alpha-dev reset", "beta-prod reset"], "{call}");
    let head = model::store::show(&fx.store, "ban").unwrap();
    let mut narrowed = head.clone();
    narrowed.text = "Never hard-reset the deploy checkouts alpha-dev and beta-prod. Refused: `alpha-dev reset`, `beta-prod reset`.".to_string();
    narrowed.check = Some(Check::Forbidden { literals });
    let because = "the two forms joined with && are not refused by the matcher, so the text names only what the check catches";
    let mut corpus = crate::legit::Corpus::load(&fx.store);
    let tried = teeth::trial_for_write(&mut fx.store, Some(&fx.db), &teeth::SessionCtx::default(), &mut corpus, &narrowed, Some(&head), Some(because), &teeth::Disputes::default(), true);
    let teeth::WriteTrial::Tried { item, record, outcome, .. } = tried else { panic!("the narrowed rule is tried") };
    assert!(matches!(outcome, teeth::Outcome::Rule { .. }), "{outcome:?}");
    model::store::revise_tried_scoped(&mut fx.store, S1, "l", "mcp", &head, &item, &head, Some(because), None, Some(&record), None, None).unwrap();
    assert!(model::store::evaluation_frozen_batch_owed(&fx.store, &start).is_empty(), "narrowed: settled");
    assert_eq!(model::store::current_verdict(&fx.store, "ban"), CurrentVerdict::RuleWired);
}

/// A rule's check kind other than a forbidden command check has no literals
/// to name: the call still carries the shape, with the agent's own words
/// where only the agent knows them.
#[test]
fn a_demotion_whose_check_names_no_caught_form_still_prints_a_revise() {
    let mut fx = Fx::new();
    let beta = fx.checkout("beta", &[("NOTES.md", "x\n")], true);
    fx.register("beta", &beta);
    let mut item = partly_refused_ban("ban");
    item.check = forbidden(&["alpha-dev && git reset", "beta-prod && git reset"]);
    decided_long_ago(&mut fx, &item, model::store::RecordedVerdict::RuleWired, Some("old-matcher"));
    let start = fx.start(S1, "beta");
    let call = item_of(&start, "ban").call.clone().unwrap();
    assert!(call.starts_with("mcp__thor__revise(id: \"ban\", text: \"<"), "{call}");
    assert!(call.contains("check_kind: \"forbidden\", check_literals: [\"<"), "no literal is caught, so none is named: {call}");
}

/// A rule whose record names no matcher at all (an older build) is old too,
/// and an unchanged class of verdict is only refreshed.
#[test]
fn a_verdict_with_no_matcher_recorded_is_older_than_any() {
    let mut fx = Fx::new();
    let item = rule("fp", "Never run `git push --force` on main.", vec![cmd("git push")], forbidden(&["--force"]), Some("beta"));
    decided_long_ago(&mut fx, &item, model::store::RecordedVerdict::RuleWired, None);
    assert!(crate::teeth::decided_by_older_matcher(&fx.store, "fp"));
    let start = fx.start(S1, "beta");
    assert_eq!(outcome_of(&start, "fp"), MachineOutcome::Unchanged);
    assert!(fx.machine_writes("fp").is_empty(), "compared, not rewritten");
    assert!(crate::teeth::read_matcher_tried(&fx.db).contains("fp"));
    let next = fx.start(S2, "beta");
    assert!(next.frozen.machine.as_ref().unwrap().items.is_empty(), "{:?}", next.frozen.machine);
}

// ---------------------------------------------------------------- the size of the start note

/// A start record as a busy store makes one: demotions on the batch, an A4
/// list, disputes, health trouble, probable false blocks and an owner queue of
/// carrying sessions. Built raw (a test holds no THOR actor token).
fn busy_frozen(batch: usize, a4: usize, disputes: usize, blocks: usize, carriers: usize, judgement: usize) -> Frozen {
    let long_because = "your check (forbidden 'alpha-dev reset', 'beta-prod reset', 'alpha-dev && git reset', 'beta-prod && git reset' \
                        on 'git') was not stored: it refuses 'git alpha-dev reset' and 'git beta-prod reset' but lets \
                        'git alpha-dev && git reset' and 'git beta-prod && git reset' through, and a check must refuse every form it names";
    let mut machine = MachinePass { box_secs: 20, needs_agent: batch, a4_only: a4, ..MachinePass::default() };
    let mut batch_ids = Vec::new();
    for n in 0..batch {
        let id = format!("batch-rule-{n}");
        machine.items.push(model::store::MachineItem {
            id: id.clone(),
            outcome: MachineOutcome::NeedsAgent,
            detail: format!("DEMOTION, nothing written: it bit when matcher 7c1e9d02ab35 tried it - {long_because}"),
            call: Some(format!("mcp__thor__revise(id: \"{id}\", text: \"<its text, reworded>\", check_kind: \"forbidden\", check_literals: [\"alpha-dev reset\"], because: \"<why - at least 20 characters>\")")),
            blocker: None,
            seq: None,
        });
        batch_ids.push(id);
    }
    let mut a4_ids = Vec::new();
    for n in 0..a4 {
        let id = format!("a4-rule-{n}");
        machine.items.push(model::store::MachineItem {
            id: id.clone(),
            outcome: MachineOutcome::A4Only,
            detail: "its check bites, but its text does not name the literal 'system prune'".to_string(),
            call: Some(format!("mcp__thor__revise(id: \"{id}\", text: \"<its text, reworded so it names 'system prune'>\")")),
            blocker: None,
            seq: None,
        });
        a4_ids.push(id);
    }
    machine.disputes = (0..disputes)
        .map(|n| model::store::DisputeItem {
            id: format!("dispute-rule-{n}"),
            seq: 100 + n as i64,
            arguments: vec![("legitimate_calls".to_string(), vec!["git grep --no-index -n \"TODO\" some/scratch-folder".to_string()])],
            because: Some("requiring the flag refuses all 7 real git grep calls inside real repositories".to_string()),
            files: Vec::new(),
        })
        .collect();
    let carried_by: Vec<model::store::Carrier> = (0..carriers)
        .map(|n| model::store::Carrier {
            session: format!("{n:08x}-1111-4222-8333-444455556666"),
            agent: None,
            filed_at: format!("2026-10-{:02}T10:00:00Z", 1 + n % 7),
            report: format!("eval-demo-{n}"),
        })
        .collect();
    machine.points = vec![model::store::MachinePoint {
        id: model::store::GLOBAL_RULES_POINT_ID.to_string(),
        blocker: model::store::MachineBlocker { code: "no-working-copy".to_string(), subject: String::new() },
        rules: vec!["rule-without-project-a".to_string(), "rule-without-project-b".to_string()],
        carried_by,
    }];
    machine.signals = Some(model::store::SignalsSoFar {
        health: "THOR health since the previous report: 1 hook launch failure(s) (PreToolUse, exit 126, session 5e1f0c3a), 0 version-skew event(s).".to_string(),
        hook_failures: 1,
        host_wide_failures: 0,
        skew_events: 0,
        refusals: format!("REFUSAL OUTCOMES since the previous report: {blocks} verified refusal(s), {blocks} followed by a weakening or retract of the refusing rule (probable false blocks), rate 100.0 percent, one-sided 95 percent upper bound 100.0 percent. A proxy read from the log: a rule can change for other reasons, and it feeds no ranking and no gate."),
        false_blocks: (0..blocks)
            .map(|n| model::store::FalseBlock {
                rule: format!("refused-rule-{n}"),
                line: format!("'refused-rule-{n}': refused 'docker system prune --filter until=24h' at log seq {}, then weakened (check cleared) at log seq {}", 10 + n, 20 + n),
                restore: format!("mcp__thor__revise(id: \"refused-rule-{n}\", check_kind: \"forbidden\", check_literals: [\"system prune\"], because: \"<why the weakening was a mistake - at least 20 characters>\")"),
            })
            .collect(),
        false_blocks_more: 0,
    });
    Frozen {
        session: S1.to_string(),
        project: "demo".to_string(),
        started_seq: 500,
        started_by: "command".to_string(),
        started_unix: 1_790_000_000,
        prev_report_seq: 0,
        judgement: (0..judgement).map(|n| format!("judged-rule-{n}")).collect(),
        batch: Some(batch_ids),
        batch_later: 0,
        a4_list: Some(a4_ids),
        open_points: vec![model::store::GLOBAL_RULES_POINT_ID.to_string()],
        machine: Some(machine),
        void: None,
        superseded: None,
        fired: std::collections::BTreeMap::new(),
    }
}

const BUSY_ID: &str = "evaluation-start-demo-500";

/// Bytes the live-state note keeps free below `START_NOTE_BUDGET`, for a
/// signal and a nudge that are added to it later.
const START_NOTE_ROOM: usize = 650;

/// THE SIGNALS NEVER ARRIVED (field 2026-10-07): on a store like the live one
/// the start note was cut at about 9 KB right after the heading of the signals
/// section, because the open point listed 25 carrying sessions and the disputes
/// block carried 1.4 KB of fixed text. A state as busy as the live one must
/// show every section a session acts on, whole, inside the size Claude Code
/// shows, with the open points - which need nothing - last.
#[test]
fn a_busy_start_note_shows_every_section_the_session_acts_on_within_the_hook_limit() {
    let frozen = busy_frozen(1, 0, 4, 1, 25, 0);
    let whole = render_start_note(BUSY_ID, &frozen);
    assert!(whole.len() <= START_NOTE_BUDGET, "this state is the live one: whole, uncut ({} bytes)", whole.len());
    // ROOM FOR WHAT IS ADDED NEXT (collect 2: the note had 10 bytes of slack, so
    // the next signal or nudge would have cut a list): the live state keeps at
    // least this much below the budget.
    assert!(whole.len() + START_NOTE_ROOM <= START_NOTE_BUDGET, "only {} bytes of room: {whole}", START_NOTE_BUDGET - whole.len());
    let note = render_start_note_within(BUSY_ID, &frozen, START_NOTE_BUDGET);
    assert!(note.len() <= START_NOTE_BUDGET, "{} bytes: {note}", note.len());
    assert!(note.len() < crate::session_start::HOOK_OUTPUT_LIMIT_BYTES, "{} bytes", note.len());
    assert!(!note.contains("NOT shown here"), "this state fits whole, nothing is cut: {note}");
    for must in [
        "a. THE BATCH",
        "'batch-rule-0'",
        "b. THE A4 LIST",
        "c. DISPUTES TO VERIFY",
        "'dispute-rule-3'",
        "3. SIGNALS BEFORE YOU FILE",
        "a. THOR health since the previous report",
        "b. REFUSAL OUTCOMES since the previous report",
        "'refused-rule-0'",
        "4. OPEN POINTS",
    ] {
        assert!(note.contains(must), "missing {must}: {note}");
    }
    // The order: what the session acts on first, what needs no action last.
    let at = |needle: &str| note.find(needle).unwrap_or_else(|| panic!("missing {needle}"));
    assert!(at("a. THE BATCH") < at("c. DISPUTES TO VERIFY") && at("c. DISPUTES TO VERIFY") < at("3. SIGNALS BEFORE YOU FILE"));
    assert!(at("3. SIGNALS BEFORE YOU FILE") < at("4. OPEN POINTS"));
    // The owner queue is a count and the last three sessions, not 25 sessions.
    assert!(note.contains("carried by 25 valid evaluation(s), the last 3: session"), "{note}");
    assert!(note.matches(" on 2026-10-").count() <= 4, "three carriers and the audit date, not 25 sessions: {note}");
}

/// A note that cannot fit cuts from the least actionable end, and says what it
/// held back and where the whole note is: never a silent cut, and the sections
/// a session acts on (the batch, the disputes, the signals' health) survive.
#[test]
fn an_overfull_start_note_cuts_from_the_least_actionable_end_and_says_so() {
    let frozen = busy_frozen(8, 16, 8, 8, 25, 0);
    let whole = render_start_note(BUSY_ID, &frozen);
    assert!(whole.len() > START_NOTE_BUDGET, "fixture is overfull: {} bytes", whole.len());
    let note = render_start_note_within(BUSY_ID, &frozen, START_NOTE_BUDGET);
    assert!(note.len() <= START_NOTE_BUDGET, "{} bytes", note.len());
    for must in ["a. THE BATCH", "c. DISPUTES TO VERIFY", "3. SIGNALS BEFORE YOU FILE", "a. THOR health since", "b. REFUSAL OUTCOMES", "4. OPEN POINTS", "'batch-rule-0'"] {
        assert!(note.contains(must), "missing {must}: {note}");
    }
    // Every cut names its list's held-back count and the record to read.
    let cuts = note.matches("more item(s) of this list are NOT shown here").count();
    assert!(cuts >= 1, "the note was cut and says so: {note}");
    assert!(note.contains(&format!("mcp__thor__get(id: \"{BUSY_ID}\")")), "{note}");
    // The least actionable go first: the A4 list is cut before the batch is.
    let shown = |prefix: &str| note.matches(&format!("\n      - '{prefix}")).count();
    assert!(shown("a4-rule-") < 16, "the A4 list gave way: {note}");
    // The whole note, with every item, is what the record keeps.
    for n in 0..16 {
        assert!(whole.contains(&format!("\n      - 'a4-rule-{n}'")), "{n}");
    }
}

/// A hook failure that was the machine going to sleep or shutting down is
/// explained in the start note with its meaning, the one thing to do (nothing),
/// when it is done and an example; it is not a THOR failure, so the real
/// launch-failure explanation does not appear for it alone.
#[test]
fn the_start_note_explains_hook_failures_that_were_the_machine_and_not_thor() {
    let signals = |hook_failures: usize, host_wide_failures: usize| model::store::SignalsSoFar {
        health: "THOR health since the previous report: x.".to_string(),
        hook_failures,
        host_wide_failures,
        refusals: "REFUSAL OUTCOMES: none.".to_string(),
        ..Default::default()
    };
    let render = |s: &model::store::SignalsSoFar| {
        let mut out = String::new();
        signals_section(&mut out, "eval-demo", s, &Caps::all());
        out
    };

    let only = render(&signals(0, 3));
    for part in ["WHAT THIS MEANS: 3 hook(s) failed while the machine was going to sleep or shutting down - not THOR", "WHAT TO DO: nothing", "DONE WHEN:", "EXAMPLE:"] {
        assert!(only.contains(part), "missing {part}: {only}");
    }
    assert!(!only.contains("a hook launch failure is a THOR hook"), "no THOR alarm for it: {only}");
    assert!(only.is_ascii(), "plain ASCII");

    let mixed = render(&signals(1, 2));
    assert!(mixed.contains("a hook launch failure is a THOR hook"), "{mixed}");
    assert!(mixed.contains("The 2 marked \"failed while the machine was going to sleep or shutting down - not THOR\" are not launch failures"), "{mixed}");

    let neither = render(&signals(0, 0));
    assert!(neither.contains("Nothing to do here."), "{neither}");
}
