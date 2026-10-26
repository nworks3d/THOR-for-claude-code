//! A replica judges on the spot what it can (step 8, F13), through its real
//! HTTP door (`mcp --http ... --capture-inbox ...`).
//!
//! A replica queues a write for the main machine, and a refusal there reaches
//! only the drain's report and doctor - never the session on the replica that
//! wrote it. So everything that needs no working copy is judged right here
//! and refused on the spot: a revise that would make an evaluation report
//! (a weekly-check report is filed new, and the copy's Stop attests it), a check whose literal the text
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

/// Owner decision 2026-10-26 ("the report is made where the session runs"): a
/// weekly-check report is no longer refused at a replica. It is queued, and
/// the reply says it counts once the copy's Stop has sent its proof. The
/// main machine holds it until the attestation arrives (`ops::drain`).
#[test]
fn a_weekly_check_report_is_queued_with_a_note_on_how_it_counts() {
    let (_tmp, db, inbox) = replica();
    let mut r = Replica::start(&db, &inbox);
    let reply = r.call(
        "remember",
        json!({"id": "eval-demo-1", "kind": "report", "project": "demo", "tags": ["evaluation-report"], "text": "An evaluation of demo."}),
    );
    assert!(reply.starts_with("queued for the main machine (remember, capture "), "{reply}");
    assert!(reply.contains("weekly-check report") && reply.contains("sent its proof"), "{reply}");
    assert_eq!(queued(&inbox).len(), 1, "queued");
}

/// The copy's Stop hook speaks plain HTTP to the connector: a ping, and the
/// attestation, queued into the same inbox as a `copy_attest` capture. What is
/// not an attestation is refused and queues nothing.
#[test]
fn the_connector_queues_a_copys_attestation_and_refuses_anything_else() {
    let (_tmp, db, inbox) = replica();
    let r = Replica::start(&db, &inbox);
    assert_eq!(r.http("GET", "/copy/ping", "").0, 200, "the ping answers");

    let att = serve::copy_eval::CopyAttestation {
        v: serve::copy_eval::ATTESTATION_VERSION,
        session: "11111111-2222-4333-8444-555555555555".to_string(),
        project: "demo".to_string(),
        report: "eval-demo-1".to_string(),
        tool_use_id: "toolu_1".to_string(),
        capture_id: "cap-1".to_string(),
        text_sha256: serve::copy_eval::text_digest("An evaluation of demo."),
        attested_at: "2026-10-26T10:00:00.000Z".to_string(),
        covered: vec![],
        seal: String::new(),
    }
    .sealed();
    let (code, _) = r.http("POST", "/copy/attest", &serde_json::to_string(&att).unwrap());
    assert_eq!(code, 200);
    let ops = queued(&inbox);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].tool, "copy_attest");
    assert_eq!(ops[0].args["capture_id"], "cap-1");

    let (code, why) = r.http("POST", "/copy/attest", "{\"not\": \"an attestation\"}");
    assert_eq!(code, 400, "{why}");
    let (code, _) = r.http("POST", "/copy/attest", "no json at all");
    assert_eq!(code, 400);
    assert_eq!(queued(&inbox).len(), 1, "nothing else was queued");
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
