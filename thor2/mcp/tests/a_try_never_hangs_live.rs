//! A try never hangs live - and since step 8 of the two tracks there is no
//! separate "try" any more: every write IS the trial.
//!
//! MEASURED (2026-09-27, a fresh evaluation on the real store): to be allowed
//! a reason, a session hung a check it already knew was too broad, and for
//! about a minute that check refused every store write that mentioned the
//! file it named. `revise` then grew a `trial_check` field that tried a check
//! without storing it. Now THOR's own trial runs on every Rule/Orientation
//! write and a check that does not bite is never stored at all, so the field
//! is gone: a caller still sending it gets rmcp's own field list (X7, R8.14)
//! and nothing is written. Driven end to end through the real compiled `mcp`
//! binary, the same door an assistant uses.

mod common;

use common::*;
use serde_json::json;

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["p"]);
    (tmp, db)
}

#[test]
fn trial_check_is_an_unknown_field_and_nothing_is_written() {
    let (tmp, db) = fixture();
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = rule("deploy-needs-a-look", "After fixture-deploy look at the running service.", Some("p"));
    args["targets"] = json!([{ "kind": "command", "value": "fixture-deploy" }]);
    let stored = mcp.call("remember", args);
    assert!(stored.starts_with("stored"), "{stored}");
    let before = all_events(&db);

    let reply = mcp.call(
        "revise",
        json!({"id": "deploy-needs-a-look", "trial_check": true, "check_kind": "forbidden", "check_literals": ["fixture-deploy"]}),
    );
    assert!(reply.contains("unknown field `trial_check`, expected one of"), "rmcp names the fields that exist: {reply}");
    assert!(reply.contains("legitimate_calls") && reply.contains("accept_refusing"), "{reply}");
    assert_eq!(all_events(&db), before, "nothing written");
}

/// The property the old field existed for, held by the trial itself: a
/// check that cannot work is NEVER hung - the write lands as a note that
/// quotes the dropped check - while a check that bites is stored.
#[test]
fn a_check_that_does_not_bite_is_never_hung_and_one_that_bites_is() {
    let (tmp, db) = fixture();
    let mut mcp = Mcp::start(&db, tmp.path());
    // A forbidden check on a rule anchored only to a file reaches no call
    // at all: it could never refuse anything, so it is never hung.
    let mut wrong = rule("too-broad", "Never run fixture-deploy with --wipe-volumes.", Some("p"));
    wrong["targets"] = json!([{ "kind": "path", "value": "deploy/compose.yml" }]);
    wrong["check_kind"] = json!("forbidden");
    wrong["check_literals"] = json!(["--wipe-volumes"]);
    let reply = mcp.call("remember", wrong);
    assert!(reply.starts_with("stored") && reply.contains("teeth: note - your check (forbidden '--wipe-volumes') was not stored"), "{reply}");
    assert!(get(&mut mcp, "too-broad").check.is_none(), "a check that cannot bite never hangs");

    let mut right = rule("wipe", "The volume wipe flag --wipe-volumes never goes on a fixture-deploy call.", Some("p"));
    right["targets"] = json!([{ "kind": "command", "value": "fixture-deploy" }]);
    right["check_kind"] = json!("forbidden");
    right["check_literals"] = json!(["--wipe-volumes"]);
    let reply = mcp.call("remember", right);
    assert!(reply.contains("teeth: wired, not proven"), "{reply}");
    assert!(get(&mut mcp, "wipe").check.is_some());
}
