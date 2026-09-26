//! `own_claims_only`, end to end through the real compiled `serve hook`
//! binary on a Stop payload: a rule about the assistant's OWN claims does not
//! fire on what the assistant reports another party said.
//!
//! THE FALSE BLOCK THIS CLOSES (2026-09-26): the rule that demands "verify
//! before claiming you cannot" held an answer that only quoted another model
//! ("het model meldde geen toegang te hebben") and claimed nothing itself. A
//! real "I cannot", in either language, still has to be held.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// The owner's own `claim-no-access-without-checking` shape, with the new
/// field set.
const RULEBOOK: &str = r#"[
  {"id":"claim-no-access-without-checking",
   "own_claims_only": true,
   "any_of":["kan ik van hieruit niet","geen tailnet","geen nas-toegang","geen toegang","ik heb geen toegang",
             "i can't reach","i don't have access","no access to the nas"],
   "none_of":["read-only","alleen-lezen","vals alarm","false positive","use versus mention","als voorbeeld","any_of"],
   "reminder":"Verify before claiming you cannot."}
]"#;

fn run_hook(db: &Path, message: &str) -> String {
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "s1",
        "stop_hook_active": false,
        "last_assistant_message": message,
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    String::from_utf8(out.stdout).unwrap()
}

fn fixture(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("thor.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    std::fs::write(dir.join("guard-response-rulebook.json"), RULEBOOK).unwrap();
    db
}

fn blocked(out: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(out.trim()).ok().and_then(|v| v.get("decision").cloned())
        == Some(serde_json::json!("block"))
}

/// THE EXACT ANSWER that was held, and its English twin: another party's
/// words, reported, with no claim of the assistant's own.
#[test]
fn another_models_reported_inability_is_not_held() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    for message in [
        "Het model meldde geen toegang te hebben tot de NAS, dus ik heb het zelf nagekeken en de share staat er.",
        "The subagent reported: I don't have access to the NAS. I checked it myself and the share is there.",
        "\"Ik heb geen toegang\", zei de agent. Ik heb het daarna zelf geprobeerd en het werkt.",
    ] {
        let out = run_hook(&db, message);
        assert!(!blocked(&out), "reported speech of another party must not be held: {message}: {out}");
    }
}

/// The other half, kept honest: the assistant's own inability claim, in
/// both languages, is still held - also when it sits next to a report.
#[test]
fn an_own_inability_claim_is_still_held_in_both_languages() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    for message in [
        "Ik heb geen toegang tot de NAS, dus dat moet jij doen.",
        "I don't have access to the NAS from here, so you will have to do it.",
        "Het model meldde niets bijzonders en ik heb geen toegang tot de NAS.",
        "The agent said it was fine, but I can't reach the NAS myself.",
    ] {
        let out = run_hook(&db, message);
        assert!(blocked(&out), "the assistant's own claim must still be held: {message}: {out}");
    }
}
