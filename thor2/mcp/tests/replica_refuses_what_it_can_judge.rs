//! A replica judges on the spot what it can (step 8, F13), through its real
//! HTTP door (`mcp --http ... --capture-inbox ...`).
//!
//! A replica queues a write for the main machine, and a refusal there reaches
//! only the drain's report and doctor - never the session on the replica that
//! wrote it. So everything that needs no working copy is judged right here
//! and refused on the spot: an evaluation report (only the main machine, in
//! the session that ran it, can attest one), a check whose literal the text
//! does not name (A4), the note track, a `teeth:` label, a reserved id, the
//! widened weakenings. What passes all of that queues exactly as before; the
//! trial itself still runs at the drain.

mod common;

use common::*;
use serde_json::json;

fn replica() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let inbox = tmp.path().join("inbox.jsonl");
    (tmp, db, inbox)
}

/// R8.24: an evaluation report is refused on the spot and nothing is queued.
#[test]
fn r8_24_an_evaluation_report_is_refused_on_the_spot() {
    let (_tmp, db, inbox) = replica();
    let mut r = Replica::start(&db, &inbox);
    let reply = r.call(
        "remember",
        json!({"id": "eval-demo-1", "kind": "report", "project": "demo", "tags": ["evaluation-report"], "text": "An evaluation of demo."}),
    );
    assert!(
        reply.starts_with(
            "REFUSED: an evaluation is filed on the main machine, in the session that ran it - a replica cannot attest it"
        ),
        "{reply}"
    );
    assert!(queued(&inbox).is_empty(), "nothing queued");
}

/// R8.24: a rule whose check names a literal its text never names is
/// refused on the spot (A4), not queued to be refused unseen at the drain; a
/// `teeth:` label and a reserved id likewise.
#[test]
fn r8_24_a_rule_write_the_replica_can_judge_is_refused_on_the_spot() {
    let (_tmp, db, inbox) = replica();
    let mut r = Replica::start(&db, &inbox);
    let mut args = rule("diskpart", "Never repartition the data disk.", Some("demo"));
    args["targets"] = json!([{ "kind": "command", "value": "diskpart" }]);
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["zqxj-never-typed"]);
    let reply = r.call("remember", args);
    assert!(reply.starts_with("REFUSED:") && reply.contains("'zqxj-never-typed' is not named anywhere in the rule's own text"), "{reply}");

    let mut labelled = rule("labelled", "A fixture rule with a forged label.", Some("demo"));
    labelled["always"] = json!(true);
    labelled["tags"] = json!(["teeth:proven-guard"]);
    let reply = r.call("remember", labelled);
    assert!(reply.starts_with("invalid 'tags': 'teeth:proven-guard' starts with 'teeth:'"), "{reply}");

    let mut reserved = rule("project-demo-opened-by-install", "A fixture pretending to be the installer's record.", Some("demo"));
    reserved["always"] = json!(true);
    let reply = r.call("remember", reserved);
    assert!(reply.starts_with("REFUSED:") && reply.contains("THOR's own registration"), "{reply}");
    assert!(queued(&inbox).is_empty(), "nothing queued: {:?}", queued(&inbox));
}

/// G8.13: a rule write that passes every ground the replica can judge
/// queues exactly as before.
#[test]
fn g8_13_a_write_that_passes_every_ground_queues_as_before() {
    let (_tmp, db, inbox) = replica();
    let mut r = Replica::start(&db, &inbox);
    let mut args = rule("no-force", "Never run `git push --force` on main.", Some("demo"));
    args["targets"] = json!([{ "kind": "command", "value": "git push" }]);
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["--force"]);
    let reply = r.call("remember", args);
    assert!(reply.starts_with("queued for the main machine (remember"), "{reply}");
    let ops = queued(&inbox);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].tool, "remember");
}
