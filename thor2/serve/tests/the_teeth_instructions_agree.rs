//! The two places that tell an agent how to answer the teeth question - the
//! shipped evaluation routine and the Stop hook's own teeth question - must
//! say the same thing.
//!
//! THE DEFECT THIS PINS DOWN (2026-09-26). The routine told an agent that a
//! justified `no-literal` tag settles a heavy rule, while the Stop hook said a
//! written reason "no longer settles this on its own" and pointed at
//! retracting the rule instead. An agent that followed one was held by the
//! other, and the cheapest way out of both was a label. Both now say the same
//! rule: a check that bites reality, or a reason that quotes the legitimate
//! call or names the trigger.
//!
//! Reads the repository's own files, the same technique
//! `a_comment_never_claims_what_the_code_does_not_do.rs` uses.

use std::path::{Path, PathBuf};

fn thor2() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("serve/ sits inside thor2/").to_path_buf()
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(thor2().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn the_routine_and_the_stop_hook_accept_the_same_answers() {
    let routine = read("eval-command.example.md");
    let hook = read("serve/src/bin/serve.rs");
    assert!(routine.contains("QUOTE that legitimate call"), "the routine must ask for the quoted call");
    assert!(hook.contains("QUOTES the legitimate call"), "the Stop hook must ask for the quoted call");
    assert!(routine.contains("open point"), "the routine must carry a wired check by an open point");
    assert!(hook.contains("open point naming"), "the Stop hook must accept the same open point");
    assert!(
        !hook.contains("no longer settles this on its own"),
        "the Stop hook must not refuse a real reason the routine accepts"
    );
}

/// The routine names the batch size in words; the filing gate holds a report
/// to the constant. Changing one without the other fails here.
#[test]
fn the_routine_names_the_batch_size_the_filing_gate_holds_a_report_to() {
    let words = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];
    let size = model::store::EVALUATION_TEETH_BATCH;
    let word = words.get(size).unwrap_or_else(|| panic!("name a batch of {size} in the routine and extend this list"));
    let routine = read("eval-command.example.md");
    assert!(routine.contains(&format!("at most {word} heavy facts")), "the routine must say the batch holds at most {word}");
}

/// The routine must spell the skip tag exactly as the filing gate parses it,
/// and say the two things the gate now holds a report to (2026-09-26): the
/// judgement list is fixed when the evaluation starts, and a no-literal
/// reason counts only after a check was tried.
#[test]
fn the_routine_describes_the_judgement_list_and_the_skip_the_gate_accepts() {
    let routine = read("eval-command.example.md");
    let skip = format!("{}<id>:<reason>", model::store::NOT_JUDGEABLE_TAG_PREFIX);
    assert!(routine.contains(&skip), "the routine must name the skip tag as '{skip}'");
    assert!(routine.contains("Your list is fixed the moment this evaluation starts"), "the routine must say the list is fixed");
    assert!(routine.contains("A reason counts only after a real try"), "the routine must say a reason needs a real try");
    assert!(
        routine.contains(&format!("at least {}", ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve"][model::store::NOT_JUDGEABLE_REASON_MIN_CHARS])),
        "the routine must name the minimum reason length the gate holds a skip to"
    );
}

#[test]
fn neither_sends_an_agent_to_a_bulk_relabel() {
    let refusal = read("model/src/store.rs");
    let start = refusal.find("fn check_evaluation_report_filing").expect("the filing gate exists");
    let end = start + refusal[start..].find("\n}\n").expect("the function ends");
    assert!(
        !refusal[start..end].contains("backfill_teeth"),
        "the filing refusal must send a writer to real work, never to a pass that labels in bulk"
    );
}
