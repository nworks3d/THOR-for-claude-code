//! `absolute-verification-claim`, the shipped reply rule that catches a claim
//! that EVERYTHING was checked ("Ik heb alles nagekeken: de cijfers kloppen")
//! when the reply does not say what was covered.
//!
//! THE DEFECT THIS CLOSES (observed in three test sessions, 2026-10-09): the
//! assistant passed on a helper's report and wrote that it had checked
//! everything after ONE partial look. `checked-claim-needs-evidence` stays
//! quiet there, because the turn did contain one command. This rule does not
//! ask whether the turn looked at something; it asks whether the claim is
//! absolute and unscoped. It is judged through the guard's own
//! `guard_verdict` against the rulebook exactly as shipped.

use serve::respond::guard_verdict;

const SHIPPED: &str = include_str!("../../guard-response-rulebook.example.json");

/// Only the new entry, so no other shipped rule can be the reason for a hold.
fn only_this_rule() -> String {
    let all: serde_json::Value = serde_json::from_str(SHIPPED).unwrap();
    let entry = all
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "absolute-verification-claim")
        .expect("the shipped rulebook carries absolute-verification-claim")
        .clone();
    serde_json::to_string(&serde_json::json!([entry])).unwrap()
}

fn held(message: &str) -> bool {
    guard_verdict(Some(&only_this_rule()), message, "", false, None).block_reason.is_some()
}

#[test]
fn the_real_overclaim_is_held() {
    assert!(held("Ik heb alles nagekeken: de cijfers kloppen."));
}

#[test]
fn the_other_absolute_phrases_are_held_in_both_languages() {
    for message in [
        "Alles gecontroleerd, het werkt.",
        "Alles klopt, je kunt het gebruiken.",
        "Ik heb het volledig nagekeken en het staat goed.",
        "Alles is geverifieerd.",
        "I went through it: everything checked, nothing to fix.",
        "Everything checks out, so we can ship.",
        "All verified. Ready when you are.",
        "All of it checks out.",
    ] {
        assert!(held(message), "an unscoped absolute claim must be held: {message}");
    }
}

#[test]
fn a_reply_that_names_what_was_and_was_not_checked_passes() {
    for message in [
        "Nagekeken: de bestandsgroottes en de README. Niet nagekeken: de rest, volgens de helper.",
        "Ik heb alles nagekeken wat de helper meldt, behalve de tests: die zijn niet gecheckt.",
        "Alles klopt volgens het verslag van de helper; zelf heb ik alleen de README bekeken.",
        "Everything checks out according to the helper's report. I did not look at it myself.",
        "All verified except the build, which is not checked.",
    ] {
        assert!(!held(message), "a scoped claim must pass: {message}");
    }
}

#[test]
fn a_negated_absolute_word_passes() {
    for message in ["Ik heb niet alles nagekeken.", "I did not verify everything checked by the helper.", "Niet alles gecontroleerd, alleen de README."] {
        assert!(!held(message), "a negated claim must pass: {message}");
    }
}

#[test]
fn another_partys_claim_and_a_quoted_mention_do_not_count() {
    for message in [
        "De helper schrijft: \"alles nagekeken\". Ik heb dat nog niet zelf gedaan.",
        "De helper zegt dat alles klopt.",
        "The agent said everything checked out; I only read its summary.",
        "De zin `alles nagekeken` is precies wat de regel moet vangen.",
    ] {
        assert!(!held(message), "not the assistant's own claim: {message}");
    }
}

#[test]
fn an_ordinary_reply_is_left_alone() {
    assert!(!held("Gedaan. De README is bijgewerkt en de test draait groen."));
    assert!(!held("Nagekeken: het bestand is 12 KB groot en eindigt op een nieuwe regel."));
}

/// The shapes the first real-reply replay held wrongly (2026-10-09): a state
/// the assistant promises to check later, a claim that names what it covers,
/// and a status line whose list label already says what it is about.
#[test]
fn a_future_or_conditional_mention_is_not_a_claim() {
    for message in [
        "Het herstel draait nu. Ik laat het weten zodra het klaar is en controleer dan zelf of alles klopt.",
        "De agent is bezig. Pas als alles klopt, krijg je hem.",
        "Je hoort het zodra dev op de nieuwe versie draait en alles nagekeken is.",
        "I will tell you once everything is verified.",
    ] {
        assert!(!held(message), "a promise to check is not a claim: {message}");
    }
}

#[test]
fn a_claim_that_names_its_scope_passes() {
    for message in [
        "Alles klopt op aantallen, groottes en checksums.",
        "Alles klopt, op de tests na.",
        "Everything checks out except the build.",
        "- **Deploy config:** alles klopt.",
    ] {
        assert!(!held(message), "a scoped claim must pass: {message}");
    }
}

#[test]
fn a_reply_that_lists_what_it_saw_passes() {
    assert!(!held("TLDR: alles klopt.

Bron van wat ik net zag:
- een proces gestart om 19:17."));
}
