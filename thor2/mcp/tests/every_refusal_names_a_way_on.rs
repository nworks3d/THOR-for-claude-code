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
//! code. No row routes to the owner queue: that queue arrives with part B,
//! and until then every code here has an agent-runnable way on.
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
    "needs_agent:global",
    "needs_agent:crowded",
    "a4_no_longer_named",
    "note_carries_check",
    "teeth_tag",
    "weakening_widened",
    "replica_evaluation_filing",
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

#[test]
fn needs_agent_global_names_the_project_and_giving_it_lands_decided() {
    let fx = Fx::new(&["beta"]);
    let beta = checkout(fx.dir(), "beta", &[("README.md", "marker-r1\n")], true);
    register(&fx.db, "beta", &beta);
    seed_raw(&fx.db, &item("ngl", "Keep `marker-r1` in README.md.", vec![path("README.md")], Some(Check::Contains { path: "README.md".to_string(), literal: "marker-r1".to_string() }), None));
    let why = fx.needs_agent("ngl");
    assert!(why.contains(way_on("needs_agent:global")), "{why}");
    let reply = fx.mcp().call("revise", json!({"id": "ngl", "project": "beta", "because": "the file lives in the beta checkout only"}));
    assert!(landed(&reply) && fx.settled("ngl"), "{reply}");
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
fn replica_evaluation_filing_says_file_it_on_the_main_machine_and_that_lands() {
    let fx = Fx::new(&["demo"]);
    let inbox = fx.dir().join("inbox.jsonl");
    let report = json!({"id": "eval-demo-1", "kind": "report", "project": "demo", "tags": ["evaluation-report"], "text": "An evaluation of demo."});
    let mut replica = Replica::start(&fx.db, &inbox);
    let reply = replica.call("remember", report.clone());
    assert!(reply.starts_with("REFUSED:") && reply.contains("file the report from a session on the main machine"), "{reply}");
    drop(replica);
    let reply = fx.mcp().call("remember", report);
    assert!(landed(&reply), "on the main machine it files: {reply}");
}
