//! THE REFUSAL TABLE (rule G1 of the two-tracks build): every refusal and
//! every "needs the agent" code the write gate, the trial and a retest can
//! print maps to an action the agent can run ITSELF, without the owner. A
//! refusal whose fix only the owner can carry out is a trap: it holds a turn
//! forever. So each code has one row here: a fixture that triggers it, the
//! words that name the way on, and that very call run on the fixture - which
//! must land.
//!
//! The codes come from the code itself - `serve::teeth::REFUSAL_TABLE` (the
//! trial and the retest) and `model::gate::TWO_TRACK_GROUND_CODES` (the write
//! grounds switched on in step 8) - and `every_code_has_exactly_one_row`
//! fails when a code is added without a row (R8.26), or a row outlives its
//! code. No row routes to the owner queue. The queue holds THOR's own open
//! points (a working copy nobody registered, a checkout mid-change, a rule
//! with no project whose check reads a file - step 20), which
//! are blockers and never refusals an agent is told to live with, so every
//! code here has an agent-runnable way on.
//!
//! Writes go through the real compiled `mcp` binary; a retest runs the
//! library's own `serve::teeth::retest` on the same store file.

mod common;

use common::*;
use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use serde_json::json;
use std::path::{Path, PathBuf};
use thor_core::event_store::EventStore;

/// Every row of this file, by its code.
const ROWS: &[&str] = &[
    "untryable",
    "a4",
    "false_now",
    "forcing",
    "majority",
    "needs_agent:a4",
    "needs_agent:forcing",
    "needs_agent:majority",
    "needs_agent:no_reach",
    "needs_agent:false_now",
    "needs_agent:crowded",
    "needs_agent:full_place",
    "a4_no_longer_named",
    "note_carries_check",
    "teeth_tag",
    "weakening_widened",
    "replica_evaluation_filing",
    "evaluation_start_reserved",
    "open_point_reserved",
    "machine_point_reserved",
    "evaluation_still_deciding",
    "evaluation_not_attested",
    "evaluation_not_open",
    "evaluation_changed_behind",
    "evaluation_turn_not_ended",
    "evaluation_machine_section",
    "frozen_item_side_door",
    "evaluation_judgement_owed",
    "evaluation_batch_owed",
];

/// R8.26: a code without a row, or a row without a code, fails here.
#[test]
fn every_code_has_exactly_one_row() {
    let mut codes: Vec<&str> = serve::teeth::REFUSAL_TABLE.iter().map(|(c, _)| *c).collect();
    codes.extend(model::gate::TWO_TRACK_GROUND_CODES.iter().copied());
    codes.sort_unstable();
    codes.dedup();
    let mut rows = ROWS.to_vec();
    rows.sort_unstable();
    assert_eq!(rows, codes, "one row per refusal code the build can print, and no row for a code that is gone");
}

/// The words a code's message always carries (the trial's own table).
fn way_on(code: &str) -> &'static str {
    serve::teeth::REFUSAL_TABLE.iter().find(|(c, _)| *c == code).map(|(_, w)| *w).unwrap_or_else(|| panic!("no words for {code}"))
}

struct Fx {
    tmp: tempfile::TempDir,
    db: PathBuf,
}

impl Fx {
    fn new(scopes: &[&str]) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path(), scopes);
        Fx { tmp, db }
    }

    fn dir(&self) -> &Path {
        self.tmp.path()
    }

    fn mcp(&self) -> Mcp {
        Mcp::start(&self.db, self.dir())
    }

    fn retest(&self, id: &str) -> serve::teeth::Retest {
        let store = EventStore::open_existing(&self.db).unwrap();
        serve::teeth::retest(&store, Some(&self.db), id, &serve::legit::Corpus::load(&store)).unwrap()
    }

    fn needs_agent(&self, id: &str) -> String {
        match self.retest(id).outcome {
            serve::teeth::Maintenance::NeedsAgent(why) => why,
            other => panic!("{id}: expected needs-the-agent, got {other:?}"),
        }
    }

    fn settled(&self, id: &str) -> bool {
        !matches!(verdict(&self.db, id), model::store::CurrentVerdict::Undecided { .. })
    }
}

fn item(id: &str, text: &str, bindings: Vec<Binding>, check: Option<Check>, project: Option<&str>) -> Item {
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
        falsifier: Some("it turns out to be harmless".to_string()),
        check,
    }
}

fn cmd(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Command, value: value.to_string() }
}

fn path(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Path, value: value.to_string() }
}

fn landed(reply: &str) -> bool {
    reply.starts_with("stored ") || reply.starts_with("revised ")
}

// ------------------------------------------------------------ the trial at the write

#[test]
fn untryable_names_the_register_command_and_registering_lets_the_write_land() {
    let fx = Fx::new(&["gamma"]);
    let gamma = checkout(fx.dir(), "gamma", &[("README.md", "keep marker-g1\n")], true);
    let mut args = rule("g-rule", "README.md must keep marker-g1.", Some("gamma"));
    args["targets"] = json!([{ "kind": "path", "value": "README.md" }]);
    args["check_kind"] = json!("contains");
    args["check_path"] = json!("README.md");
    args["check_literal"] = json!("marker-g1");
    let mut inside = Mcp::start(&fx.db, &gamma);
    let reply = inside.call("remember", args.clone());
    assert!(reply.starts_with("REFUSED:") && reply.contains(way_on("untryable")), "{reply}");
    assert!(reply.contains(&format!("--register-root --db \"{}\"", fx.db.display())), "the command names this store: {reply}");
    // The fix: register the checkout (the installer's own record; the
    // command itself is proven by ops/tests/install_registers_root.rs).
    register(&fx.db, "gamma", &gamma);
    let reply = inside.call("remember", args);
    assert!(landed(&reply) && reply.contains("teeth: proven guard"), "{reply}");
}

#[test]
fn a4_names_the_literal_and_naming_it_lets_the_write_land() {
    let fx = Fx::new(&["demo"]);
    let mut mcp = fx.mcp();
    let mut args = rule("disk", "Never repartition the data disk.", Some("demo"));
    args["targets"] = json!([{ "kind": "command", "value": "diskpart" }]);
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["zqxj-never-typed"]);
    let reply = mcp.call("remember", args.clone());
    assert!(reply.starts_with("REFUSED:") && reply.contains(way_on("a4")), "{reply}");
    args["text"] = json!("Never repartition the data disk: never run diskpart zqxj-never-typed.");
    let reply = mcp.call("remember", args);
    assert!(landed(&reply), "{reply}");
}

#[test]
fn false_now_names_what_is_missing_and_fixing_the_file_lets_the_write_land() {
    let fx = Fx::new(&["beta"]);
    let beta = checkout(fx.dir(), "beta", &[("NOTES.md", "intro\n")], true);
    register(&fx.db, "beta", &beta);
    let mut mcp = fx.mcp();
    let mut args = rule("notes", "NOTES.md must keep the line `keep me`.", Some("beta"));
    args["targets"] = json!([{ "kind": "path", "value": "NOTES.md" }]);
    args["check_kind"] = json!("contains");
    args["check_path"] = json!("NOTES.md");
    args["check_literal"] = json!("keep me");
    let reply = mcp.call("remember", args.clone());
    assert!(reply.starts_with("REFUSED:") && reply.contains(way_on("false_now")), "{reply}");
    std::fs::write(beta.join("NOTES.md"), "intro\nkeep me\n").unwrap();
    let reply = mcp.call("remember", args);
    assert!(landed(&reply), "{reply}");
}

#[test]
fn forcing_names_the_check_to_hang_and_hanging_it_lets_the_write_land() {
    let fx = Fx::new(&["demo"]);
    let mut mcp = fx.mcp();
    let mut args = rule("force", "Never run `git push --force` on main.", Some("demo"));
    args["targets"] = json!([{ "kind": "command", "value": "git push" }]);
    let reply = mcp.call("remember", args.clone());
    assert!(reply.starts_with("REFUSED:") && reply.contains(way_on("forcing")), "{reply}");
    assert!(reply.contains("check_kind forbidden, check_literals ['--force']"), "{reply}");
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["--force"]);
    let reply = mcp.call("remember", args);
    assert!(landed(&reply), "{reply}");
}

#[test]
fn majority_names_accept_refusing_and_naming_a_listed_call_lets_the_write_land() {
    let fx = Fx::new(&["demo"]);
    served(&fx.db, OTHER_SESSION, &["git grep -n p0 src", "git grep -n p1 src", "git grep -n p2 src", "git grep -n p3 src", "git grep -n p4 src"]);
    let mut mcp = fx.mcp();
    let mut args = rule("grep-ban", "Never run `git grep` in this repository.", Some("demo"));
    args["targets"] = json!([{ "kind": "command", "value": "git grep" }]);
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["git grep"]);
    let reply = mcp.call("remember", args.clone());
    assert!(reply.starts_with("REFUSED:") && reply.contains(way_on("majority")), "{reply}");
    args["accept_refusing"] = json!(["git grep -n p0 src"]);
    let reply = mcp.call("remember", args);
    assert!(landed(&reply), "{reply}");
}

// ------------------------------------------------------------ needs the agent (retest)

#[test]
fn needs_agent_a4_names_the_revise_and_it_lands_decided() {
    let fx = Fx::new(&["demo"]);
    seed_raw(&fx.db, &item("na4", "Never repartition the disk.", vec![cmd("diskpart")], Some(Check::Forbidden { literals: vec!["zq-y1".to_string()] }), Some("demo")));
    let why = fx.needs_agent("na4");
    assert!(why.contains(way_on("needs_agent:a4")), "{why}");
    let reply = fx.mcp().call("revise", json!({"id": "na4", "text": "Never repartition the disk: never run diskpart zq-y1."}));
    assert!(landed(&reply) && fx.settled("na4"), "{reply}");
}

#[test]
fn needs_agent_forcing_names_the_check_and_hanging_it_lands_decided() {
    let fx = Fx::new(&["demo"]);
    seed_raw(&fx.db, &item("nfo", "Never run `git push --force` on main.", vec![cmd("git push")], None, Some("demo")));
    let why = fx.needs_agent("nfo");
    assert!(why.contains(way_on("needs_agent:forcing")), "{why}");
    let reply = fx.mcp().call("revise", json!({"id": "nfo", "check_kind": "forbidden", "check_literals": ["--force"]}));
    assert!(landed(&reply) && fx.settled("nfo"), "{reply}");
}

#[test]
fn needs_agent_majority_names_accept_refusing_and_it_lands_decided() {
    let fx = Fx::new(&["demo"]);
    served(&fx.db, OTHER_SESSION, &["git grep -n q0 src", "git grep -n q1 src", "git grep -n q2 src", "git grep -n q3 src", "git grep -n q4 src"]);
    seed_raw(&fx.db, &item("nmj", "Never run `git grep` there.", vec![cmd("git grep")], Some(Check::Forbidden { literals: vec!["git grep".to_string()] }), Some("demo")));
    let why = fx.needs_agent("nmj");
    assert!(why.contains(way_on("needs_agent:majority")), "{why}");
    let reply = fx.mcp().call("revise", json!({"id": "nmj", "accept_refusing": ["git grep -n q0 src"]}));
    assert!(landed(&reply) && fx.settled("nmj"), "a revise naming only the id and the accepted call runs the trial: {reply}");
}

#[test]
fn needs_agent_no_reach_names_the_binding_and_adding_it_lands_decided() {
    let fx = Fx::new(&["beta"]);
    let beta = checkout(fx.dir(), "beta", &[("NOTES.md", "intro\nkeep this line\n")], true);
    register(&fx.db, "beta", &beta);
    seed_raw(&fx.db, &item("nnr", "NOTES.md keeps `keep this line`.", vec![cmd("make release")], Some(Check::Contains { path: "NOTES.md".to_string(), literal: "keep this line".to_string() }), Some("beta")));
    let why = fx.needs_agent("nnr");
    assert!(why.contains(way_on("needs_agent:no_reach")) && why.contains("path:NOTES.md"), "{why}");
    let reply = fx.mcp().call(
        "revise",
        json!({"id": "nnr", "targets": [{"kind": "command", "value": "make release"}, {"kind": "path", "value": "NOTES.md"}]}),
    );
    assert!(landed(&reply) && fx.settled("nnr"), "{reply}");
}

#[test]
fn needs_agent_false_now_names_the_fix_and_restoring_the_file_settles_it() {
    let fx = Fx::new(&["beta"]);
    let beta = checkout(fx.dir(), "beta", &[("NOTES.md", "intro\n")], true);
    register(&fx.db, "beta", &beta);
    seed_raw(&fx.db, &item("nfn", "NOTES.md keeps `gone line`.", vec![path("NOTES.md")], Some(Check::Contains { path: "NOTES.md".to_string(), literal: "gone line".to_string() }), Some("beta")));
    let why = fx.needs_agent("nfn");
    assert!(why.contains(way_on("needs_agent:false_now")), "{why}");
    std::fs::write(beta.join("NOTES.md"), "intro\ngone line\n").unwrap();
    let reply = fx.mcp().call("revise", json!({"id": "nfn"}));
    assert!(landed(&reply) && fx.settled("nfn"), "the restored file, tried by a bare revise: {reply}");
}

/// A rule with no project and a file check is no refusal an agent is told
/// to live with (step 20, K10): at a retest it is THOR's blocker - routed to
/// the owner queue by an evaluation's machine pass - and the way on it names
/// is not destructive: giving it a project still lands, decided.
#[test]
fn a_global_file_check_is_blocked_and_giving_it_the_project_still_lands() {
    let fx = Fx::new(&["beta"]);
    let beta = checkout(fx.dir(), "beta", &[("README.md", "marker-r1\n")], true);
    register(&fx.db, "beta", &beta);
    seed_raw(&fx.db, &item("ngl", "Keep `marker-r1` in README.md.", vec![path("README.md")], Some(Check::Contains { path: "README.md".to_string(), literal: "marker-r1".to_string() }), None));
    match fx.retest("ngl").outcome {
        serve::teeth::Maintenance::Blocked(blocker) => assert!(blocker.is_global_rules(), "{blocker:?}"),
        other => panic!("expected THOR's blocker, got {other:?}"),
    }
    assert!(!fx.settled("ngl"), "blocked is not a verdict: nothing was written");
    let reply = fx.mcp().call("revise", json!({"id": "ngl", "project": "beta", "because": "the file lives in the beta checkout only"}));
    assert!(landed(&reply) && fx.settled("ngl"), "{reply}");
}

/// A path_exists check on a file the rule is not anchored to, whose one fix
/// (anchor the rule there) the crowding cap refuses because four facts sit on
/// that path. Step 22 made it THOR's blocker so the agent would not clear a
/// true check; step 26 (the owner's decision: a full place is the agent's)
/// hands it to the agent with every occupant - its severity and whether it
/// carries a check - and the three honest exits, never "the owner decides" and
/// never "drop the check". Once a place has room, the way on it names (anchor
/// the rule there) lands decided.
#[test]
fn a_path_check_on_a_full_anchor_names_the_occupants_and_room_makes_it_land() {
    let fx = Fx::new(&["beta"]);
    let beta = checkout(fx.dir(), "beta", &[("files/PLAN.md", "the plan\n")], true);
    register(&fx.db, "beta", &beta);
    for n in 0..4 {
        let mut occupant = item(&format!("occ{n}"), &format!("Decision number {n} about the plan."), vec![path("files/PLAN.md")], None, Some("beta"));
        occupant.severity = None;
        seed_raw(&fx.db, &occupant);
    }
    let project = Binding::Target { kind: TargetKind::Project, value: "beta".to_string() };
    let mut plan = item("npa", "The plan lives in files/PLAN.md of this repo.", vec![project], Some(Check::PathExists { path: "files/PLAN.md".to_string() }), Some("beta"));
    plan.kind = Kind::Orientation;
    seed_raw(&fx.db, &plan);
    match fx.retest("npa").outcome {
        serve::teeth::Maintenance::NeedsAgent(why) => {
            assert!(why.contains("'occ0' (no severity, no check)") && why.contains("FOLD") && why.contains("RETRACT"), "{why}");
            assert!(why.contains(way_on("needs_agent:full_place")), "{why}");
            assert!(!why.contains("drop the check") && !why.contains("owner decides"), "{why}");
        }
        other => panic!("expected the agent's way on, got {other:?}"),
    }
    assert!(!fx.settled("npa"), "needing the agent is not a verdict: nothing was written");

    let reply = fx.mcp().call("retract", json!({"id": "occ3", "reason": "the owner folded it into another decision"}));
    assert!(reply.contains("retracted"), "{reply}");
    let why = fx.needs_agent("npa");
    assert!(why.contains("anchor the rule to 'files/PLAN.md'"), "the place has room: the agent's anchor to make: {why}");
    let reply = fx.mcp().call(
        "revise",
        json!({"id": "npa", "targets": [{"kind": "project", "value": "beta"}, {"kind": "path", "value": "files/PLAN.md"}]}),
    );
    assert!(landed(&reply) && fx.settled("npa"), "{reply}");
}

#[test]
fn needs_agent_crowded_names_a_narrower_anchor_and_moving_it_lands_decided() {
    let fx = Fx::new(&["demo"]);
    let mut light = item("ncr", "Never run `npm publish --force` here.", vec![cmd("npm publish")], Some(Check::Forbidden { literals: vec!["--force".to_string()] }), Some("demo"));
    light.severity = None;
    seed_raw(&fx.db, &light);
    for n in 0..4 {
        seed_raw(&fx.db, &item(&format!("h{n}"), &format!("Heavy rival number {n} on the publish command."), vec![cmd("npm publish")], None, Some("demo")));
    }
    let retest = fx.retest("ncr");
    let mut store = EventStore::open_existing(&fx.db).unwrap();
    let why = match serve::teeth::apply_as_retest(&mut store, &retest) {
        Err(serve::teeth::ApplyError::NeedsAgent(why)) => why,
        other => panic!("{other:?}"),
    };
    drop(store);
    assert!(why.contains(way_on("needs_agent:crowded")), "{why}");
    let reply = fx.mcp().call(
        "revise",
        json!({"id": "ncr", "targets": [{"kind": "command", "value": "npm publish --tag beta"}],
               "text": "Never run `npm publish --tag beta --force` here.", "because": "moved to the one publish form it is about"}),
    );
    assert!(landed(&reply) && fx.settled("ncr"), "{reply}");
}

// ------------------------------------------------------------ the write grounds of step 8

#[test]
fn a4_no_longer_named_says_keep_it_named_and_keeping_it_lands() {
    let fx = Fx::new(&["demo"]);
    let mut mcp = fx.mcp();
    let mut args = rule("fp", "Never `git push --force` on main.", Some("demo"));
    args["targets"] = json!([{ "kind": "command", "value": "git push" }]);
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["--force"]);
    assert!(landed(&mcp.call("remember", args)));
    let reply = mcp.call("revise", json!({"id": "fp", "text": "Never rewrite shared history.", "because": "fixture"}));
    assert!(reply.starts_with("REFUSED:") && reply.contains("no longer named in the text") && reply.contains("keep it named in the text"), "{reply}");
    let reply = mcp.call("revise", json!({"id": "fp", "text": "Never rewrite shared history with `git push --force`."}));
    assert!(landed(&reply), "{reply}");
}

#[test]
fn note_carries_check_says_write_it_through_revise_and_that_lands() {
    let fx = Fx::new(&["demo"]);
    let mut mcp = fx.mcp();
    let mut args = rule("n1", "Never run `sshpass` here; use keys.", Some("demo"));
    args["always"] = json!(true);
    assert!(mcp.call("remember", args).contains("teeth: note"));
    // A bin writing past the trial: the store refuses the note label beside a check.
    let mut store = EventStore::open_existing(&fx.db).unwrap();
    let note = model::store::show(&store, "n1").unwrap();
    let mut hung = note.clone();
    hung.check = Some(Check::Forbidden { literals: vec!["sshpass".to_string()] });
    let err = model::store::revise(&mut store, "bin", "bin", "repair-project", &note, &hung).unwrap_err();
    assert!(err.to_string().contains("write it through remember/revise so THOR can try the new one"), "{err}");
    drop(store);
    let reply = mcp.call("revise", json!({"id": "n1", "check_kind": "forbidden", "check_literals": ["sshpass"]}));
    assert!(landed(&reply) && reply.contains("teeth: wired, not proven"), "{reply}");
}

#[test]
fn teeth_tag_says_drop_it_and_dropping_it_lands() {
    let fx = Fx::new(&["demo"]);
    let mut store = EventStore::open_existing(&fx.db).unwrap();
    let mut forged = item("forged", "Never run `git push --force` on main.", vec![cmd("git push")], Some(Check::Forbidden { literals: vec!["--force".to_string()] }), Some("demo"));
    forged.tags = vec!["teeth:proven-guard".to_string()];
    let err = model::store::declare(&mut store, "s", "l", "cli", &forged).unwrap_err();
    assert!(err.to_string().contains("drop it from the tags"), "{err}");
    drop(store);
    let mut args = rule("forged", "Never run `git push --force` on main.", Some("demo"));
    args["targets"] = json!([{ "kind": "command", "value": "git push" }]);
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["--force"]);
    let reply = fx.mcp().call("remember", args);
    assert!(landed(&reply), "{reply}");
}

#[test]
fn weakening_widened_says_give_because_and_giving_it_lands() {
    let fx = Fx::new(&["demo"]);
    let mut mcp = fx.mcp();
    let mut args = rule("sev", "A fixture rule about a costly thing.", Some("demo"));
    args["severity"] = json!("costly");
    args["always"] = json!(true);
    assert!(landed(&mcp.call("remember", args)));
    let reply = mcp.call("revise", json!({"id": "sev", "severity": "house_style"}));
    assert!(reply.starts_with("REFUSED:") && reply.contains("give `because`"), "{reply}");
    let reply = mcp.call("revise", json!({"id": "sev", "severity": "house_style", "because": "it turned out cheap to undo"}));
    assert!(landed(&reply), "{reply}");
}

#[test]
fn evaluation_start_reserved_says_pick_another_id_and_that_lands() {
    let fx = Fx::new(&["demo"]);
    let mut mcp = fx.mcp();
    let report = |id: &str| json!({"id": id, "kind": "report", "project": "demo", "text": "Notes on where the demo evaluation began."});
    let reply = mcp.call("remember", report("evaluation-start-demo-7"));
    assert!(reply.contains("Give your own item an id that does not begin with 'evaluation-start-'"), "{reply}");
    let mut tagged = report("notes-demo-start");
    tagged["tags"] = json!(["evaluation-start"]);
    let reply = mcp.call("remember", tagged);
    assert!(reply.contains("Drop 'evaluation-start' from the tags"), "{reply}");
    let reply = mcp.call("remember", report("notes-demo-start"));
    assert!(landed(&reply), "{reply}");
}

#[test]
fn replica_evaluation_filing_says_file_it_on_the_main_machine_and_that_lands() {
    let fx = Fx::new(&["demo"]);
    let inbox = fx.dir().join("inbox.jsonl");
    let report = json!({"id": "eval-demo-1", "kind": "report", "project": "demo", "tags": ["evaluation-report"], "text": "An evaluation of demo."});
    let mut replica = Replica::start(&fx.db, &inbox);
    let reply = replica.call("remember", report.clone());
    assert!(reply.starts_with("REFUSED:") && reply.contains("file the report from a session on the main machine"), "{reply}");
    drop(replica);
    // On the main machine, in the session that runs the evaluation, through
    // THOR's own tool (step 15): it files.
    let mut s = session(&fx);
    s.prompt("/t-eval");
    let mut mcp = s.mcp();
    let reply = s.call(&mut mcp, "remember", report, "toolu_main_filing");
    assert!(landed(&reply), "on the main machine it files: {reply}");
}

#[test]
fn open_point_reserved_says_drop_the_tag_and_that_lands() {
    let fx = Fx::new(&["demo"]);
    let mut mcp = fx.mcp();
    let note = |tags: serde_json::Value| json!({"id": "push-rule-still-open", "kind": "report", "project": "demo", "tags": tags, "text": "The push rule still needs a real call, later."});
    for tag in ["open-point", "about:push-rule", "blocker:no-working-copy"] {
        let reply = mcp.call("remember", note(json!([tag])));
        assert!(reply.starts_with("REFUSED: open points are written and closed by THOR itself; settle the rule instead"), "{tag}: {reply}");
        assert!(reply.contains("Drop the tag") && reply.contains("mcp__thor__revise with only the rule's id"), "{tag}: {reply}");
    }
    assert_eq!(events_of(&fx.db, "push-rule-still-open").len(), 0, "nothing written");
    let reply = mcp.call("remember", note(json!([])));
    assert!(landed(&reply), "without the tag it is an ordinary note: {reply}");
}

/// A rule of a project with no registered working copy, held by THOR's own
/// open point after an evaluation start by `session`.
fn evaluation_with_a_machine_point(fx: &Fx, session: &str) -> model::store::EvaluationStart {
    let held = item("demo-keeps", "README.md keeps `x marks it`.", vec![path("README.md")], Some(Check::Contains { path: "README.md".to_string(), literal: "x marks it".to_string() }), Some("demo"));
    seed_raw(&fx.db, &held);
    let mut store = EventStore::open_existing(&fx.db).unwrap();
    serve::eval_session::start(&fx.db, &mut store, session, "demo", "command").expect("the evaluation starts").start
}

#[test]
fn machine_point_reserved_says_it_closes_itself_and_it_holds_nothing_of_the_agents() {
    let fx = Fx::new(&["demo"]);
    let start = evaluation_with_a_machine_point(&fx, WRITING_SESSION);
    assert_eq!(start.frozen.machine.as_ref().map(|m| m.blocked), Some(1));
    let point = "open-no-working-copy-demo";
    let before = events_of(&fx.db, point).len();
    assert_eq!(before, 1, "THOR wrote its own point");

    let mut mcp = Mcp::start_with(Start { db: &fx.db, cwd: fx.dir(), session: Some(WRITING_SESSION), args: vec![], home: None });
    let reply = mcp.call("retract", json!({"id": point, "reason": "closing it by hand, the folder is registered"}));
    assert!(reply.starts_with("REFUSED: this open point is THOR's own; it closes itself when its subject settles"), "{reply}");
    assert!(reply.contains("the owner registers the folder"), "it names whose step it is: {reply}");
    let reply = mcp.call("revise", json!({"id": point, "text": "Closed by hand."}));
    assert!(reply.starts_with("REFUSED: this open point is THOR's own"), "{reply}");
    assert_eq!(events_of(&fx.db, point).len(), before, "nothing written");

    // The way on for the agent is to leave it: the point holds no filing
    // (filed through THOR's own tool in the evaluating session, step 15).
    let mut s = session(&fx);
    let mut filer = s.mcp();
    let reply = s.call(&mut filer, "remember", report_args("eval-demo-1", "demo", "An evaluation of demo."), "toolu_mp_filing");
    assert!(landed(&reply), "a rule held by THOR's own point holds no filing: {reply}");
}

#[test]
fn evaluation_still_deciding_says_end_the_turn_once_and_then_it_lands() {
    let fx = Fx::new(&["demo"]);
    {
        // What a hook killed between its two writes leaves: the start record
        // with no batch.
        let mut store = EventStore::open_existing(&fx.db).unwrap();
        let began = serve::eval_session::begin(&fx.db, &mut store, WRITING_SESSION, "demo", "stop-ask").unwrap();
        assert!(!began.start.frozen.decided());
    }
    let mut s = session(&fx);
    let mut mcp = s.mcp();
    let report = report_args("eval-demo-1", "demo", "An evaluation of demo.");
    let reply = s.call(&mut mcp, "remember", report.clone(), "toolu_deciding_1");
    assert!(reply.starts_with("REFUSED:") && reply.contains("THOR is still deciding the old rules - end this turn once and the start note will list what is yours to do"), "{reply}");
    assert_eq!(events_of(&fx.db, "eval-demo-1").len(), 0, "nothing written");
    // Ending the turn: the session's Stop finishes the pass (`resume`).
    {
        let mut store = EventStore::open_existing(&fx.db).unwrap();
        assert_eq!(serve::eval_session::resume(&mut store, WRITING_SESSION).len(), 1);
    }
    let reply = s.call(&mut mcp, "remember", report, "toolu_deciding_2");
    assert!(landed(&reply), "{reply}");
}

// ------------------------------------------------------------ the filing, step 15

/// The evaluating session of `fx`'s store: `WRITING_SESSION`, working in a
/// folder of project demo (`common::Session`, the shared attested fixture).
fn session(fx: &Fx) -> Session {
    Session::with_store(fx.dir(), fx.db.clone(), WRITING_SESSION)
}

/// A rule of demo decided by THOR's trial (a note) that fired 40 times in
/// the evaluating session: on the next start's judgement list.
fn listed(fx: &Fx, id: &str) {
    let mut rule = item(id, &format!("Run the fixture check {id} first."), vec![cmd(&format!("fixture-{id}"))], None, Some("demo"));
    rule.severity = None;
    seed_raw(&fx.db, &rule);
    let mut store = EventStore::open_existing(&fx.db).unwrap();
    serve::teeth::fixture::decide(&mut store, id);
    for i in 0..40 {
        serve::deliver::record_delivery(&mut store, &serve::deliver::nth_session(WRITING_SESSION, i), WRITING_SESSION, "hook", "2026-09-28T00:00:00Z", &[id.to_string()]);
    }
}

fn demo_report() -> serde_json::Value {
    report_args("eval-demo-1", "demo", "An evaluation of demo.")
}

#[test]
fn evaluation_not_attested_says_file_it_with_the_thor_tool_and_that_lands() {
    let fx = Fx::new(&["demo"]);
    let mut s = session(&fx);
    s.prompt("/t-eval");
    let mut mcp = s.mcp();
    let reply = mcp.call("remember", demo_report());
    assert!(reply.starts_with("REFUSED: this evaluation report is not coming through a THOR tool call"), "{reply}");
    assert!(reply.contains("File it with the THOR tool itself") && reply.contains("load it with ToolSearch"), "{reply}");
    assert_eq!(events_of(&fx.db, "eval-demo-1").len(), 0, "nothing written");
    let reply = s.call(&mut mcp, "remember", demo_report(), "toolu_na_file");
    assert!(landed(&reply), "the same call through the THOR tool: {reply}");
}

#[test]
fn evaluation_not_open_says_file_nothing_until_one_starts_and_then_it_lands() {
    let fx = Fx::new(&["demo"]);
    let mut s = session(&fx);
    let mut mcp = s.mcp();
    let reply = s.call(&mut mcp, "remember", demo_report(), "toolu_no_1");
    assert!(
        reply.starts_with(
            "REFUSED: this evaluation report cannot be filed: no evaluation of 'demo' is open in this session (THOR's log holds no start record for it)"
        ),
        "{reply}"
    );
    assert!(reply.contains("There is nothing to file now, so file nothing"), "{reply}");
    assert_eq!(events_of(&fx.db, "eval-demo-1").len(), 0, "nothing written");
    // Once an evaluation starts (the Stop ask, or the owner's /thor-eval),
    // the report files.
    s.prompt("/t-eval");
    let reply = s.call(&mut mcp, "remember", demo_report(), "toolu_no_2");
    assert!(landed(&reply), "{reply}");
}

#[test]
fn evaluation_changed_behind_says_end_the_turn_and_the_fresh_evaluation_lands() {
    let fx = Fx::new(&["demo"]);
    listed(&fx, "listed-a");
    let mut s = session(&fx);
    s.prompt("/t-eval");
    {
        let mut store = EventStore::open_existing(&fx.db).unwrap();
        model::store::retract(&mut store, "shell", "shell", "cli", "listed-a", "fixture-listed-a is gone", None).unwrap();
    }
    let mut mcp = s.mcp();
    let reply = s.call(&mut mcp, "remember", demo_report(), "toolu_cb_1");
    assert!(reply.starts_with("REFUSED: 'listed-a' was changed at log seq") && reply.contains("End this turn"), "{reply}");
    // The turn ends; the evaluation starts afresh (here by the owner's
    // command; the Stop ask does the same while it is due), with a fresh
    // list, and the report files.
    s.stop();
    let note = s.prompt("/t-eval");
    assert!(note.contains("started at log position"), "a fresh start: {note}");
    let reply = s.call(&mut mcp, "remember", demo_report(), "toolu_cb_2");
    assert!(landed(&reply), "{reply}");
}

#[test]
fn evaluation_turn_not_ended_says_end_the_turn_once_and_then_it_lands() {
    let fx = Fx::new(&["demo"]);
    listed(&fx, "listed-t");
    let mut s = session(&fx);
    s.prompt("/t-eval");
    let mut mcp = s.mcp();
    assert!(s.call(&mut mcp, "mark", json!({"id": "listed-t"}), "toolu_tn_mark").starts_with("marked useful"));
    let reply = s.call(&mut mcp, "remember", demo_report(), "toolu_tn_1");
    assert!(reply.starts_with("REFUSED:") && reply.contains(model::store::TURN_NOT_ENDED), "{reply}");
    s.stop();
    let reply = s.call(&mut mcp, "remember", demo_report(), "toolu_tn_2");
    assert!(landed(&reply), "{reply}");
}

#[test]
fn evaluation_machine_section_says_change_only_the_tldr_and_that_lands() {
    let fx = Fx::new(&["demo"]);
    let mut s = session(&fx);
    s.prompt("/t-eval");
    let mut mcp = s.mcp();
    // At the filing: a typed section header is refused; the report without
    // it lands.
    let forged = report_args("eval-demo-1", "demo", "=== MACHINE SECTION: derived from the log, seq 1 to 2; it cannot be edited ===");
    let reply = s.call(&mut mcp, "remember", forged, "toolu_ms_forged");
    assert!(reply.starts_with("REFUSED: the text of this evaluation report carries a section header only THOR writes"), "{reply}");
    assert!(reply.contains("send only your own TLDR and remarks"), "{reply}");
    assert!(landed(&s.call(&mut mcp, "remember", demo_report(), "toolu_ms_file")));
    s.stop();
    let stored = get(&mut mcp, "eval-demo-1").text;
    let edited = stored.replacen("machine section", "machine sectiom", 1);
    let reply = s.call(&mut mcp, "revise", json!({"id": "eval-demo-1", "text": edited}), "toolu_ms_1");
    assert!(reply.starts_with("REFUSED: the machine section of an evaluation report cannot be edited; revise only the TLDR and remarks below it"), "{reply}");
    assert!(reply.contains("change only what follows that line"), "{reply}");
    let reply = s.call(&mut mcp, "revise", json!({"id": "eval-demo-1", "text": "An evaluation of demo, all settled."}), "toolu_ms_2");
    assert!(landed(&reply), "{reply}");
}

#[test]
fn frozen_item_side_door_says_use_the_thor_tool_and_that_lands() {
    let fx = Fx::new(&["demo"]);
    listed(&fx, "listed-s");
    let mut s = session(&fx);
    s.prompt("/t-eval");
    let mut mcp = s.mcp();
    let retract = json!({"id": "listed-s", "reason": "fixture-listed-s is gone"});
    let reply = mcp.call("retract", retract.clone());
    assert!(reply.starts_with("REFUSED: 'listed-s' is on the frozen lists of an open evaluation"), "{reply}");
    assert!(reply.contains("Make this change with the THOR tool itself, inside Claude Code"), "{reply}");
    let reply = s.call(&mut mcp, "retract", retract, "toolu_fs_1");
    assert!(reply.starts_with("retracted listed-s"), "{reply}");
}

// ------------------------------------------------------------ the filing, step 19
//
// The two filing refusals older than this table, rowed at the final gate:
// each names the one call that settles its item, and that call - then the
// same filing - lands.

#[test]
fn evaluation_judgement_owed_names_the_mark_and_marking_lets_the_filing_land() {
    let fx = Fx::new(&["demo"]);
    listed(&fx, "listed-j");
    let mut s = session(&fx);
    s.prompt("/t-eval");
    let mut mcp = s.mcp();
    let reply = s.call(&mut mcp, "remember", demo_report(), "toolu_jo_1");
    assert!(reply.starts_with("REFUSED: this evaluation report cannot be filed yet: 1 item(s) fired in this session"), "{reply}");
    assert!(reply.contains("'listed-j'") && reply.contains("mcp__thor__mark(id: \"<id>\")"), "it names the item and the call: {reply}");
    assert_eq!(events_of(&fx.db, "eval-demo-1").len(), 0, "nothing written");
    assert!(s.call(&mut mcp, "mark", json!({"id": "listed-j"}), "toolu_jo_mark").starts_with("marked useful"));
    s.stop();
    let reply = s.call(&mut mcp, "remember", demo_report(), "toolu_jo_2");
    assert!(landed(&reply), "{reply}");
}

#[test]
fn evaluation_batch_owed_names_the_trial_and_the_named_fix_lets_the_filing_land() {
    let fx = Fx::new(&["demo"]);
    let mut forcing = item("demo-no-wipe", "Never run `acme-tool --wipe` on the ledger.", vec![cmd("acme-tool")], None, Some("demo"));
    forcing.severity = Some(Severity::Costly);
    seed_raw(&fx.db, &forcing);
    let mut s = session(&fx);
    s.prompt("/t-eval");
    let start = s.open_start("demo").expect("started");
    assert_eq!(start.frozen.batch(), &["demo-no-wipe".to_string()], "fixture sanity: {:?}", start.frozen.machine);
    let mut mcp = s.mcp();
    let reply = s.call(&mut mcp, "remember", demo_report(), "toolu_bo_1");
    assert!(reply.starts_with("REFUSED: this evaluation report cannot be filed yet: 1 rule(s) on this evaluation's frozen lists"), "{reply}");
    assert!(reply.contains("'demo-no-wipe'") && reply.contains("mcp__thor__revise with only the rule's id"), "it names the rule and the call: {reply}");
    assert_eq!(events_of(&fx.db, "eval-demo-1").len(), 0, "nothing written");
    // The fix its derived tooth names: hang the check.
    let reply = s.call(&mut mcp, "revise", json!({"id": "demo-no-wipe", "check_kind": "forbidden", "check_literals": ["--wipe"]}), "toolu_bo_fix");
    assert!(landed(&reply), "{reply}");
    s.stop();
    let reply = s.call(&mut mcp, "remember", demo_report(), "toolu_bo_2");
    assert!(landed(&reply), "{reply}");
}
