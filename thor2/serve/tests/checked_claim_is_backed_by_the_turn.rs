//! The shipped `checked-claim-needs-evidence` rule against replies from real
//! sessions (field sweep of 2026-10-09): a claim of having checked something is
//! backed by what the turn itself looked at, not by a path and a line number
//! the owner's short-answer rule forbids in a reply to him. A claim made with
//! nothing looked at in the turn is still held.
//!
//! THE FALSE BLOCK (owner, acme-shop session, 2026-10-09): "Zelf nagekeken.
//! Dev draait 1.0.151, en ik heb de scripts van de admin-pagina zelf
//! gecontroleerd: alle drie in orde. Ingelogd heb ik het niet gezien, dus dat
//! stukje moet jij bevestigen." was held for "no evidence" after the turn had
//! run curl, git and a node check.

use serve::respond::{guard_verdict_in_turn, turn_commands};

const SHIPPED_RULEBOOK: &str = include_str!("../../guard-response-rulebook.example.json");

const REPORTED_REPLY: &str = "**Kort:** de dev-admin zou weer moeten werken, met versie 1.0.151 op dev. \
Herlaad de pagina op je gsm om het te bevestigen.\n\n**Zelf nagekeken.** Dev draait 1.0.151, en ik heb de scripts \
van de admin-pagina zelf gecontroleerd: alle drie in orde. Ingelogd heb ik het niet gezien, dus dat stukje moet jij \
bevestigen.\n\nProd is niet aangeraakt.";

fn transcript(calls: &[(&str, &str)]) -> String {
    let mut lines = vec![r#"{"type":"user","message":{"role":"user","content":"Er laad niks, tabs werken ook niet"}}"#.to_string()];
    for (name, input) in calls {
        lines.push(format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"{name}","input":{input}}}]}}}}"#
        ));
    }
    lines.join("\n")
}

fn held(reply: &str, calls: &[(&str, &str)]) -> Option<String> {
    let commands = turn_commands(&transcript(calls));
    guard_verdict_in_turn(Some(SHIPPED_RULEBOOK), reply, "Er laad niks, tabs werken ook niet", false, None, &commands).block_reason
}

#[test]
fn the_reported_reply_is_held_when_the_turn_looked_at_nothing() {
    let reason = held(REPORTED_REPLY, &[]).expect("a claim with nothing looked at stays held");
    assert!(reason.contains("this turn shows no look at it"), "{reason}");
    assert!(reason.contains("Check it now"), "the way out is to look, not to paste a path: {reason}");
}

#[test]
fn the_reported_reply_passes_when_the_turn_ran_a_command_or_read_something() {
    for calls in [
        vec![("Bash", r#"{"command":"curl -s http://dev.example/health"}"#)],
        vec![("Bash", r#"{"command":"node check-admin-scripts.mjs dashboard.html"}"#)],
        vec![("Bash", r#"{"command":"node check-dash.mjs dashboard.html","description":"Syntax check of the admin page scripts"}"#)],
        vec![("Read", r#"{"file_path":"server/public/admin.html"}"#)],
        vec![("Grep", r#"{"pattern":"<script"}"#)],
    ] {
        assert_eq!(held(REPORTED_REPLY, &calls), None, "{calls:?}");
    }
}

/// Collect 17b: a look at something the claim does not name backs nothing.
#[test]
fn the_reported_reply_is_held_when_the_turn_looked_only_at_something_else() {
    for calls in [
        vec![("Bash", r#"{"command":"ls -la"}"#)],
        vec![("Read", r#"{"file_path":"server/lib/pricing.js"}"#)],
        vec![("Bash", r#"{"command":"git -C C:/Users/me/dev/other status -sb"}"#)],
    ] {
        assert!(held(REPORTED_REPLY, &calls).is_some(), "{calls:?}");
    }
}

#[test]
fn a_claim_relayed_from_an_agent_is_held_unless_it_says_so() {
    let relayed = "Alles staat klaar.\n\n**Gecontroleerd:**\n- alle tests slagen;\n- de quotepagina's dragen geen fade.";
    let launched = [("Agent", r#"{"prompt":"scope the fade"}"#), ("mcp__thor__remember", r#"{"id":"x"}"#)];
    assert!(held(relayed, &launched).is_some(), "launching an agent and writing a note looked at nothing");
    let attributed = "Alles staat klaar.\n\nVolgens het verslag van de agent is gecontroleerd: alle tests slagen.";
    assert_eq!(held(attributed, &launched), None, "saying whose check it was is the honest way out");
    let own = "Alles staat klaar. Ik heb zelf niet gecontroleerd: de tests liep de agent.";
    assert_eq!(held(own, &launched), None);
}

#[test]
fn a_qualified_denial_is_no_claim() {
    // "Niet volledig gecheckt:" and "Niet zelf gecheckt:" say the opposite of a claim.
    for reply in [
        "Ja, met twee kanttekeningen.\n\nNiet volledig gecheckt: vijf cachemappen zijn alleen op grootte vergeleken.",
        "Ik heb een deel zelf nagemeten.\n\nNiet zelf gecheckt: de overige onderdelen, daarvoor heb ik alleen het verslag.",
    ] {
        assert_eq!(held(reply, &[]), None, "{reply}");
    }
}

#[test]
fn a_claim_inside_a_drafted_message_for_someone_else_is_no_claim() {
    let reply = "Hier is het blok:\n\n```\nBUSINESS-KANT -> LOCAL-LLM-KANT\n\nWij hebben de code nagelezen. \
Alles hieronder is gecontroleerd tegen wat er echt staat.\n```";
    assert_eq!(held(reply, &[]), None);
}

#[test]
fn a_claim_in_the_assistants_own_words_next_to_a_quote_is_still_held() {
    let reply = "De agent meldt: \"alles is gecheckt\". Zelf heb ik het ook gecheckt: het klopt.";
    assert!(held(reply, &[]).is_some());
}
