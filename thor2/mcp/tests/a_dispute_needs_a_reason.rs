//! A dispute (`legitimate_calls`, `may_change`, `accept_refusing`) turns a
//! rule that bites into a note that cannot refuse anything, or keeps a check
//! that refuses real calls. Measured 2026-10-07: 7 of 11 disputes a
//! maintenance agent saved in one pass were wrong - the disputed literal was
//! the rule's real subject, or a better check existed. Nothing asked the
//! agent to say why, so nothing could catch it afterwards.
//!
//! So the door now refuses a dispute that carries no `because`, with a
//! message that offers the better path first, and the reason is kept in the
//! item's history. One test per argument, each red beside its green.

mod common;

use common::*;
use serde_json::json;

fn push_rule() -> serde_json::Value {
    let mut args = rule("bundle", "Prove a bundle with `git clone --mirror` plus fsck, never with `git bundle list-heads`.", Some("demo"));
    args["targets"] = json!([{ "kind": "command", "value": "git bundle" }]);
    args["legitimate_calls"] = json!(["git bundle list-heads backup.bundle", "git clone --mirror origin.git backup.git", "git clone origin.git work"]);
    args
}

const REASON: &str = "list-heads is only the example here; the rule is about proving a bundle by cloning it";

/// legitimate_calls with no because: refused before anything is tried, and
/// the refusal names the better path first, then exactly what to add.
#[test]
fn legitimate_calls_without_a_because_is_refused_with_the_better_path_first() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let reply = mcp.call("remember", push_rule());
    println!("{reply}");
    assert!(reply.starts_with("REFUSED:"), "{reply}");
    assert!(reply.contains("because"), "{reply}");
    let better = reply.find("first try to hang a check on the literal that IS the rule's subject").expect(&reply);
    let dispute = reply.find("dispute only an incidental span").expect(&reply);
    assert!(better < dispute, "the better path comes first: {reply}");
    assert!(reply.contains("20 characters"), "{reply}");
    assert!(get_missing(&mut mcp, "bundle"), "the rule was not stored");
}

fn get_missing(mcp: &mut Mcp, id: &str) -> bool {
    !mcp.call("get", json!({ "id": id })).trim_start().starts_with('{')
}

/// A one-word or throwaway reason is no reason: at least twenty characters.
#[test]
fn a_reason_shorter_than_twenty_characters_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = push_rule();
    args["because"] = json!("it is fine");
    let reply = mcp.call("remember", args);
    println!("{reply}");
    assert!(reply.starts_with("REFUSED:") && reply.contains("20 characters"), "{reply}");
    assert!(get_missing(&mut mcp, "bundle"));
}

/// With a real because the rule lands as a note by dispute, and the reason
/// is in the item's history, the same way revise's because already is.
#[test]
fn a_dispute_with_a_because_lands_and_the_reason_is_in_history() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = push_rule();
    args["because"] = json!(REASON);
    let reply = mcp.call("remember", args);
    println!("{reply}");
    assert!(reply.starts_with("stored 'bundle'") && reply.contains("teeth: note by dispute"), "{reply}");
    let history = mcp.call("history", json!({ "id": "bundle" }));
    assert!(history.contains(&format!("(because: {REASON})")), "{history}");
}

/// may_change: the same demand, the same message.
#[test]
fn may_change_needs_a_because_too() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["beta"]);
    let beta = checkout(tmp.path(), "beta", &[("package.json", "{\"name\":\"demo\",\"version\":\"1.0.0\"}\n")], true);
    register(&db, "beta", &beta);
    let mut mcp = Mcp::start(&db, &beta);
    let mut args = rule("pk", "Bump the 'version' in package.json with every release.", Some("beta"));
    args["targets"] = json!([{ "kind": "path", "value": "package.json" }]);
    args["may_change"] = json!(["version"]);
    let reply = mcp.call("remember", args.clone());
    println!("{reply}");
    assert!(reply.starts_with("REFUSED:") && reply.contains("dispute only an incidental span"), "{reply}");
    args["because"] = json!("the version number moves with every release, so the line may change while the rule stays true");
    let reply = mcp.call("remember", args);
    println!("{reply}");
    assert!(reply.starts_with("stored 'pk'") && reply.contains("teeth: note by dispute"), "{reply}");
}

/// accept_refusing keeps the check but says refusing those calls is the
/// intent: the same demand.
#[test]
fn accept_refusing_needs_a_because_too() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    served(&db, "373e8610-0000-4000-8000-000000000000", &["git push origin main", "git push origin topic", "git push"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = rule("no-push", "Never run `git push` in this repository until the owner says so.", Some("demo"));
    args["targets"] = json!([{ "kind": "command", "value": "git push" }]);
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["git push"]);
    args["accept_refusing"] = json!(["git push origin main"]);
    let reply = mcp.call("remember", args.clone());
    println!("{reply}");
    assert!(reply.starts_with("REFUSED:") && reply.contains("first try to hang a check"), "{reply}");
    args["because"] = json!("refusing every push is the whole intent of this ban until the owner lifts it");
    let reply = mcp.call("remember", args);
    println!("{reply}");
    assert!(reply.starts_with("stored 'no-push'"), "{reply}");
}

/// A write with no dispute argument never needs the reason.
#[test]
fn a_write_with_no_dispute_needs_no_because() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = rule("plain", "Keep the build green.", Some("demo"));
    args["moments"] = json!(["push"]);
    let reply = mcp.call("remember", args);
    assert!(reply.starts_with("stored 'plain'"), "{reply}");
}
