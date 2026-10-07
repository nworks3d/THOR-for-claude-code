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

    // The evaluation's second call, "confirm the dispute": a revise naming
    // only the id and a reason is accepted, keeps the verdict, and is in the
    // history beside the first reason.
    let restated = "checked the rule against its file: the example call is incidental, the rule stays true";
    let reply = mcp.call("revise", json!({ "id": "bundle", "because": restated }));
    println!("{reply}");
    assert!(reply.starts_with("revised 'bundle'"), "{reply}");
    let history = mcp.call("history", json!({ "id": "bundle" }));
    assert!(history.contains(&format!("(because: {restated})")) && history.contains(&format!("(because: {REASON})")), "{history}");
    assert!(get(&mut mcp, "bundle").check.is_none(), "still a note by dispute");
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

/// THE CONFIRM CALL (call 2) holds the same floor as writing a dispute: a
/// revise that carries only a reason, on a rule that stands on a dispute, is
/// refused when the reason is shorter than twenty characters - the 2026-10-07
/// audit found a one-word reason would otherwise settle the item.
#[test]
fn the_confirm_call_needs_twenty_characters_too() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = push_rule();
    args["because"] = json!(REASON);
    let reply = mcp.call("remember", args);
    assert!(reply.starts_with("stored 'bundle'") && reply.contains("teeth: note by dispute"), "{reply}");

    for filler in ["ok", "it is fine", "checked, it stands"] {
        let reply = mcp.call("revise", json!({ "id": "bundle", "because": filler }));
        println!("{reply}");
        assert!(reply.starts_with("REFUSED:") && reply.contains("20 characters") && reply.contains("dispute"), "'{filler}': {reply}");
    }
    let history = mcp.call("history", json!({ "id": "bundle" }));
    assert!(!history.contains("(because: ok)") && !history.contains("(because: it is fine)"), "nothing was written: {history}");
    let reply = mcp.call("revise", json!({ "id": "bundle", "because": "checked the rule against its file: the example call is incidental" }));
    assert!(reply.starts_with("revised 'bundle'"), "{reply}");
}

/// A reason on a rule that carries no dispute is the ordinary `because` of any
/// weakening: the floor is the dispute's, not every revise's.
#[test]
fn a_short_because_on_a_rule_without_a_dispute_is_not_refused_by_that_floor() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = rule("plain", "Keep the build green.", Some("demo"));
    args["moments"] = json!(["push"]);
    assert!(mcp.call("remember", args).starts_with("stored 'plain'"));
    let reply = mcp.call("revise", json!({ "id": "plain", "because": "tidy" }));
    assert!(!reply.contains("a dispute needs"), "{reply}");
}

/// A rule retracted by mistake comes back with a `remember` under a NEW id,
/// in the shape THOR's restore call prints: the old id stays retracted (a
/// second remember on it would leave the id with two heads, which `get`
/// refuses to read until a resolve), the new rule is live with its check, and
/// the history of the old id keeps the retract.
#[test]
fn a_retracted_rule_comes_back_with_a_remember_under_a_new_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = rule("no-prune", "Never run `docker system prune` on the build host.", Some("demo"));
    args["targets"] = json!([{ "kind": "command", "value": "docker" }]);
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["system prune"]);
    let reply = mcp.call("remember", args.clone());
    assert!(reply.starts_with("stored 'no-prune'"), "{reply}");
    let reply = mcp.call("retract", json!({ "id": "no-prune", "reason": "the docker system prune ban is replaced by a cleanup job" }));
    assert!(reply.starts_with("retracted no-prune"), "{reply}");
    assert!(get_missing(&mut mcp, "no-prune"));

    // The call THOR prints: id "<old>-restored", a tag naming the old id, the
    // rule's own text, bindings and check.
    let mut restored = args.clone();
    restored["id"] = json!("no-prune-restored");
    restored["tags"] = json!(["restored-from:no-prune"]);
    restored["moments"] = json!([]);
    restored["always"] = json!(false);
    let reply = mcp.call("remember", restored);
    println!("{reply}");
    assert!(reply.starts_with("stored 'no-prune-restored'"), "{reply}");
    let back = get(&mut mcp, "no-prune-restored");
    assert!(back.check.is_some() && back.tags.contains(&"restored-from:no-prune".to_string()), "{back:?}");
    assert!(get_missing(&mut mcp, "no-prune"), "the old id stays retracted");
    let history = mcp.call("history", json!({ "id": "no-prune" }));
    assert!(history.contains("fact_retracted"), "the history keeps the retract: {history}");
}

/// The pitfall the routine names: a second remember on a RETRACTED id writes a
/// second head, and `get` then refuses to read the id until a resolve - so a
/// restore always takes a new id.
#[test]
fn remembering_a_retracted_id_again_leaves_it_with_two_heads() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = rule("no-prune", "Never run `docker system prune` on the build host.", Some("demo"));
    args["targets"] = json!([{ "kind": "command", "value": "docker" }]);
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["system prune"]);
    assert!(mcp.call("remember", args.clone()).starts_with("stored 'no-prune'"));
    assert!(mcp.call("retract", json!({ "id": "no-prune", "reason": "the docker system prune ban is replaced by a cleanup job" })).starts_with("retracted no-prune"));
    let again = mcp.call("remember", args);
    println!("{again}");
    assert!(mcp.call("get", json!({ "id": "no-prune" })).contains("diverged"), "same id twice: two heads");
}

/// A refusal in an item's history shows the call the hook refused - what the
/// agent needs to judge whether the refusal was right - and a rule's step
/// shows the rule as it stood (its check and bindings).
#[test]
fn history_shows_the_call_a_refusal_refused_and_the_rule_as_it_stood() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = rule("no-force", "Never run `git push --force` on the main branch.", Some("demo"));
    args["targets"] = json!([{ "kind": "command", "value": "git push" }]);
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["--force"]);
    assert!(mcp.call("remember", args).starts_with("stored 'no-force'"));
    {
        let mut store = thor_core::event_store::EventStore::open_existing(&db).unwrap();
        let body = r#"{"at":"2026-10-07T09:20:00Z","target":"git push --force-with-lease origin main","tool_use_id":"toolu_02"}"#;
        store.append_event("other-session", "other-session", "hook", thor_core::event_store::EventKind::GateRefused, "no-force", None, body).unwrap();
        store.append_event("other-session", "other-session", "hook", thor_core::event_store::EventKind::GateRefused, "no-force", None, "not json").unwrap();
    }
    let history = mcp.call("history", json!({ "id": "no-force" }));
    println!("{history}");
    assert!(history.contains(" gate_refused by hook ["), "{history}");
    assert!(history.contains("]: refused call: 'git push --force-with-lease origin main'"), "{history}");
    assert!(history.contains("]: refused call: (the log kept no text for it)"), "{history}");
    assert!(!history.contains("no readable item body"), "{history}");
    assert!(history.contains("\n    as it stood: check forbidden '--force'; bound to command 'git push'; severity "), "{history}");
}

/// A text change that gives a reason of its own, and names no dispute, is the
/// agent confirming the dispute the rule stands on while it rewords: the trial
/// runs again with the dispute, so the rule stays a note by dispute and the
/// new reason is the one doctor shows. Before, the retry dropped the dispute
/// and the reason with it.
#[test]
fn a_reworded_rule_with_a_reason_keeps_its_confirmed_dispute_and_the_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = push_rule();
    args["because"] = json!(REASON);
    assert!(mcp.call("remember", args).starts_with("stored 'bundle'"));

    let restated = "reworded the rule and checked it: the example call stays incidental, the rule stays true";
    let reply = mcp.call(
        "revise",
        json!({ "id": "bundle", "text": "Prove a bundle with `git clone --mirror` plus fsck, never with `git bundle list-heads` alone.", "because": restated }),
    );
    println!("{reply}");
    assert!(reply.starts_with("revised 'bundle'"), "{reply}");
    let store = thor_core::event_store::EventStore::open_existing(&db).unwrap();
    assert!(model::store::current_dispute_in_words(&store, "bundle").is_some(), "the dispute is still carried: {reply}");
    assert_eq!(model::store::current_dispute_reason(&store, "bundle").as_deref(), Some(restated), "{reply}");
}
