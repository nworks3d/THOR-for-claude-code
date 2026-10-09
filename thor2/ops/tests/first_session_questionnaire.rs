//! The numbered first-session questionnaire: everything the install arranges,
//! each item in plain words with its WHY, rendered from ONE catalogue
//! (`ops::questionnaire`) into the lookup item the seeded setup note points at
//! and into the page in AGENTS.md. These tests are that agreement.

use ops::questionnaire::{self, questions, BEGIN_MARKER, END_MARKER};
use serve::plan::{Detection, Plan};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn agents_md() -> String {
    std::fs::read_to_string(repo_root().join("AGENTS.md")).expect("AGENTS.md")
}

const EVERYTHING_THE_INSTALL_ARRANGES: &[&str] =
    &["plan", "hooks", "git-hooks", "weekly-check", "work-rules", "helper-agents", "bug-reports", "replies", "topic-folders", "backups"];

/// The page a person (or an agent) reads holds exactly what the catalogue
/// renders. Run with THOR_UPDATE_GOLDEN=1 to rewrite the block after editing
/// the catalogue.
#[test]
fn the_page_in_agents_md_is_the_catalogue_rendered() {
    let agents = agents_md();
    let begin = agents.find(BEGIN_MARKER).expect("AGENTS.md lost the questionnaire begin marker");
    let end = agents.find(END_MARKER).expect("AGENTS.md lost the questionnaire end marker") + END_MARKER.len();
    let want = questionnaire::agents_md_block();
    if std::env::var("THOR_UPDATE_GOLDEN").is_ok() {
        let rewritten = format!("{}{}{}", &agents[..begin], want, &agents[end..]);
        std::fs::write(repo_root().join("AGENTS.md"), rewritten).unwrap();
        return;
    }
    assert!(
        agents[begin..end] == want,
        "AGENTS.md is out of date with ops/src/questionnaire.rs: run this test with THOR_UPDATE_GOLDEN=1 to rewrite the block"
    );
}

#[test]
fn every_item_the_install_arranges_is_on_the_list_in_order() {
    let ids: Vec<&str> = questions().iter().map(|q| q.id).collect();
    assert_eq!(ids, EVERYTHING_THE_INSTALL_ARRANGES);
    for (i, q) in questions().iter().enumerate() {
        assert_eq!(q.n, i + 1, "numbered 1.. in order");
    }
}

#[test]
fn each_item_carries_what_why_default_and_how_to_say_no() {
    for q in questions() {
        for (label, text) in [("what", q.what.as_str()), ("why", q.why), ("say_no", q.say_no.as_str())] {
            assert!(text.chars().count() > 12, "item {} ({}) has no {label}", q.n, q.id);
        }
        assert!(!q.default.trim().is_empty(), "item {} ({}) has no default", q.n, q.id);
        assert!(!q.why.contains(". "), "item {} ({}): the why is ONE sentence: {}", q.n, q.id, q.why);
        assert!(q.why.ends_with('.'), "item {}: {}", q.n, q.why);
    }
}

#[test]
fn the_text_is_plain_ascii_without_dashes_curly_quotes_or_ellipsis_glyphs() {
    let text = questionnaire::lookup_text(None);
    for c in text.chars() {
        assert!(c.is_ascii(), "non-ASCII character {c:?} in the questionnaire");
    }
}

#[test]
fn the_plan_item_asks_for_confirmation_and_gives_the_reason_for_each_case() {
    let plan = &questions()[0];
    assert!(plan.why.contains("usage limit") && plan.why.contains("once a week"), "{}", plan.why);
    let max = questionnaire::render_list(Some(Detection { plan: Plan::Max, tier_found: true }));
    assert!(max.contains("The settings say: Max. Is that right?"), "{max}");
    let small = questionnaire::render_list(Some(Detection { plan: Plan::Small, tier_found: true }));
    assert!(small.contains("The settings say: Pro or smaller. Is that right?"), "{small}");
    let missing = questionnaire::render_list(Some(Detection { plan: Plan::Small, tier_found: false }));
    assert!(missing.contains("do not say, so which is it: Max, or Pro and smaller?"), "a missing key is asked outright: {missing}");
    assert!(max.contains(plan.why) && missing.contains(plan.why), "the reason rides with the question in every case");
}

#[test]
fn helper_agent_mode_states_the_measurement_and_the_free_chat() {
    let q = questions().into_iter().find(|q| q.id == "helper-agents").unwrap();
    assert!(q.why.contains("12 to 62 percent cheaper"), "{}", q.why);
    assert!(q.why.contains("chat stays free"), "{}", q.why);
    assert_eq!(q.default, "On.");
    for id in questionnaire::HELPER_MODE_NOTES {
        assert!(q.say_no.contains(id), "saying no must name the note {id} to retract");
        assert!(ops::install::workflow_starter_pack().iter().any(|n| n.id == *id), "{id} is not in the pack any more");
    }
}

#[test]
fn the_work_rules_item_lists_every_other_starter_note_and_none_twice() {
    let pack: Vec<String> = ops::install::workflow_starter_pack().into_iter().map(|n| n.id).collect();
    let rules = questionnaire::work_rule_ids();
    assert_eq!(rules.len() + questionnaire::HELPER_MODE_NOTES.len(), pack.len(), "the two items partition the pack");
    let item = questions().into_iter().find(|q| q.id == "work-rules").unwrap();
    for id in &rules {
        assert!(item.say_no.contains(id.as_str()), "{id} cannot be named for removal");
        assert!(item.what.contains(&questionnaire::plain_name(id)), "{id} is missing from the one-line list");
    }
    assert!(item.what.contains(&format!("A set of {} general work habits", rules.len())));
}

#[test]
fn the_bug_report_item_reassures_in_plain_words() {
    let q = questions().into_iter().find(|q| q.id == "bug-reports").unwrap();
    for must in ["exact full text", "nothing is sent without your yes", "saying no costs nothing"] {
        assert!(q.what.contains(must), "the reassurance lacks '{must}': {}", q.what);
    }
    for removed in ["file paths", "project and folder names", "computer and user names", "addresses", "keys and passwords"] {
        assert!(q.what.contains(removed), "it must say plainly that {removed} are removed");
    }
}

#[test]
fn the_reply_item_counts_the_rules_the_shipped_rulebook_really_has() {
    let rulebook = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../guard-response-rulebook.example.json")).unwrap();
    let entries = serde_json::from_str::<serde_json::Value>(&rulebook).unwrap().as_array().unwrap().len();
    let q = questions().into_iter().find(|q| q.id == "replies").unwrap();
    assert_eq!(entries, 5, "the item says five reply rules; change the item and this together");
    assert!(q.what.contains("five reply rules"), "{}", q.what);
}

#[test]
fn a_fresh_store_holds_the_list_under_its_key_and_the_setup_note_points_at_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let claude = dir.path().join("claude.json");
    std::fs::write(&claude, r#"{"oauthAccount":{"organizationRateLimitTier":"default_claude_max_20x"}}"#).unwrap();
    ops::install::ensure_store(&db).unwrap();
    let seeded = ops::install::seed_working_contract_with(&db, Some(&claude)).unwrap();
    assert!(seeded.iter().all(|s| s.stored), "refused: {:?}", seeded.iter().filter(|s| !s.stored).collect::<Vec<_>>());

    let store = thor_core::event_store::EventStore::open_read_only(&db).unwrap();
    let item = model::store::show(&store, questionnaire::LOOKUP_ID).expect("the list is in the memory");
    assert_eq!(item.key.as_deref(), Some(questionnaire::LOOKUP_ID), "lookup by key finds it");
    for n in 1..=questions().len() {
        assert!(item.text.contains(&format!("\n{n}. ")), "item {n} missing");
    }
    assert!(item.text.contains("The settings say: Max. Is that right?"), "the detected plan rides in the list");
    assert!(item.text.contains("FOR THE ASSISTANT"), "and so does what to do with the answers");

    let note = model::store::show(&store, model::store::SETUP_NOTE_ID).unwrap();
    assert!(note.text.contains(questionnaire::LOOKUP_ID), "the setup note must name the key: {}", note.text);
    assert!(note.text.to_lowercase().contains("nothing waits"), "{}", note.text);
    assert!(note.text.chars().count() <= 300, "{} characters", note.text.chars().count());
}

#[test]
fn the_assistant_rules_never_block_and_never_ask_twice() {
    let rules = questionnaire::ASSISTANT_RULES;
    let lowered = rules.to_lowercase();
    for must in ["without waiting", "now", "once", "final word", "never block", "never ask the same question twice", "revise 'owner-setup-answers'"] {
        assert!(lowered.contains(must), "the assistant rules lack '{must}'");
    }
}
