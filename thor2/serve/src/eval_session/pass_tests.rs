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
    let note = start_note(&start);
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
    assert_eq!(crowded.call.as_deref(), Some("mcp__thor__revise(id: \"light\")"));
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
