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
    &["plan", "hooks", "git-hooks", "weekly-check", "work-rules", "helper-agents", "bug-reports", "replies", "topic-folders", "backups", "recording", "changelog"];

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
        assert!(!q.line.trim().is_empty() && !q.line.contains('\n'),"item {} ({}): the line is ONE line", q.n, q.id);
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
    let max = questionnaire::lookup_text(Some(Detection { plan: Plan::Max, tier_found: true }));
    assert!(max.contains("The settings say: Max. Is that right?"), "{max}");
    let small = questionnaire::lookup_text(Some(Detection { plan: Plan::Small, tier_found: true }));
    assert!(small.contains("The settings say: Pro or smaller. Is that right?"), "{small}");
    let missing = questionnaire::lookup_text(Some(Detection { plan: Plan::Small, tier_found: false }));
    assert!(missing.contains("do not say, so which is it: Max, or Pro and smaller?"), "a missing key is asked outright: {missing}");
    assert!(max.contains(plan.why) && missing.contains(plan.why), "the reason rides with the question in every case, in the detail");
    // The short list the user sees carries the question itself, in one line.
    let shown = |d| questionnaire::render_list(Some(d)).lines().find(|l| l.starts_with("1. ")).unwrap().to_string();
    assert!(shown(Detection { plan: Plan::Max, tier_found: true }).contains("the settings say Max - is that right?"));
    assert!(shown(Detection { plan: Plan::Small, tier_found: true }).contains("the settings say Pro or smaller - is that right?"));
    assert!(shown(Detection { plan: Plan::Small, tier_found: false }).contains("which is it, Max or Pro and smaller?"));
}

/// The owner found a ten-item list with four parts each overwhelming. What the
/// user sees is now a short intro, one line per item and the way to answer; the
/// Why and the way to say no are kept for the assistant and shown only when the
/// user asks about a number or refuses one.
#[test]
fn the_list_the_user_sees_is_an_intro_of_three_lines_at_most_then_one_line_per_item() {
    let shown = questionnaire::render_list(None);
    assert!(!shown.contains("Why it exists") && !shown.contains("To say no") && !shown.contains("What I chose"), "no four-part items:\n{shown}");
    let lines: Vec<&str> = shown.lines().filter(|l| !l.trim().is_empty()).collect();
    let first_item = lines.iter().position(|l| l.starts_with("1. ")).expect("item 1");
    assert!(first_item <= 3, "the intro is at most 3 lines, it is {first_item}:\n{shown}");
    let items = &lines[first_item..lines.len() - 1];
    assert_eq!(items.len(), questions().len(), "one line per item:\n{shown}");
    for (i, line) in items.iter().enumerate() {
        assert!(line.starts_with(&format!("{}. ", i + 1)), "{line}");
        assert!(line.chars().count() <= 280, "item {} is not short: {} characters", i + 1, line.chars().count());
        assert!(line.contains("Default:"), "each line carries what was chosen: {line}");
    }
    let closing = lines.last().unwrap();
    assert!(closing.contains("by number") && closing.contains("defaults"), "the way to answer ends the list: {closing}");
}

#[test]
fn the_why_and_the_way_to_say_no_sit_in_the_appendix_for_the_assistant() {
    let text = questionnaire::lookup_text(None);
    let appendix = &text[text.find("DETAIL PER ITEM").expect("the appendix is there")..];
    assert!(appendix.contains("only when he asks about a number or refuses it"), "{appendix}");
    for q in questions() {
        let at = appendix.find(&format!("\n{}. {}\n", q.n, q.title)).unwrap_or_else(|| panic!("item {} has no detail", q.n));
        let block = &appendix[at..];
        let block = &block[..block.find("\n\n").unwrap_or(block.len())];
        for part in [q.why, q.default] {
            assert!(block.contains(part), "item {} detail lacks '{part}'", q.n);
        }
        assert!(block.contains(&questionnaire::fill(&q.say_no, &questionnaire::Paths::default())), "item {} detail lacks how to say no", q.n);
    }
    let rules = questionnaire::ASSISTANT_RULES.to_lowercase();
    assert!(rules.contains("short list") && rules.contains("only when he asks about a number") && rules.contains("why once"), "{rules}");
}

#[test]
fn the_backup_item_tells_the_truth_that_nothing_is_backed_up_until_the_user_answers() {
    let q = questions().into_iter().find(|q| q.id == "backups").unwrap();
    for must in ["NOTHING is backed up until you choose", "a copy on this machine", "a place you name", "none"] {
        assert!(q.line.contains(must), "the one line lacks '{must}': {}", q.line);
    }
    for liar in ["daily copy", "About once a day"] {
        assert!(!q.what.contains(liar) && !q.default.contains(liar), "a fresh install wires no backup, so the item must not say '{liar}'");
    }
    assert!(q.default.contains("nothing is backed up until you answer"), "{}", q.default);
    assert_eq!(q.why, "The memory is a single file, and if the disk dies everything it learned dies with it.");
    assert!(q.what.contains("not against the disk dying"), "the honest limit of a local copy: {}", q.what);
    assert!(q.say_no.contains("--set-backup off"), "{}", q.say_no);
    assert!(questionnaire::ASSISTANT_RULES.contains("--set-backup local"), "the assistant knows the command");
}

#[test]
fn the_recording_item_says_what_is_kept_where_why_and_how_to_switch_it_off() {
    let q = questions().into_iter().find(|q| q.id == "recording").unwrap();
    for must in ["30 days", "this machine", "blanked", "wrongly block"] {
        assert!(q.line.contains(must) || q.what.contains(must), "recording item lacks '{must}': {} / {}", q.line, q.what);
    }
    assert!(q.say_no.contains("--set-recording off") && q.say_no.contains("no data"), "{}", q.say_no);
    assert!(questionnaire::ASSISTANT_RULES.contains("--set-recording on|off"));
}

#[test]
fn the_changelog_item_is_the_owners_decision_with_a_default_of_yes() {
    let q = questions().into_iter().find(|q| q.id == "changelog").unwrap();
    // The user's line is a statement of what happens, never a question.
    assert!(!q.line.contains('?'), "item 12 is a statement, not a question: {}", q.line);
    assert!(q.line.contains("I create CHANGELOG.md") && q.line.contains("say no and I do not"), "{}", q.line);
    assert_eq!(q.why, "Every later session, and you, know exactly what changed and which version runs, without digging through history.");
    assert!(q.default.starts_with("Yes") && q.default.contains("has none"), "{}", q.default);
    assert!(questionnaire::ASSISTANT_RULES.contains("CHANGELOG.md"), "on yes the assistant creates the file");
}

/// Every command the list tells the assistant to run must exist in the
/// installer, or the list promises something the install cannot do.
#[test]
fn every_install_flag_the_list_names_exists_in_the_installer() {
    let help = std::process::Command::new(env!("CARGO_BIN_EXE_install")).arg("--help").output().unwrap();
    let help = String::from_utf8_lossy(&help.stdout).to_string();
    let text = questionnaire::lookup_text(None);
    let mut named = 0;
    for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
        if word.starts_with("--set-") {
            named += 1;
            assert!(help.contains(word), "the list names {word}, which the installer does not have");
        }
    }
    assert!(named >= 6, "the list names its commands: {named}");
}

#[test]
fn helper_agent_mode_is_off_by_default_and_states_the_measurement_the_free_chat_and_the_costs() {
    let q = questions().into_iter().find(|q| q.id == "helper-agents").unwrap();
    assert!(q.line.ends_with("Default: off."), "the one line the user sees says it is off: {}", q.line);
    assert_eq!(q.default, "Off.");
    assert!(q.why.contains("12 to 62 percent cheaper"), "{}", q.why);
    assert!(q.why.contains("chat stays free"), "{}", q.why);
    // The honest side: the reasons somebody may not want it, and when no helper is used.
    for must in [
        "do not see the work happen",
        "cannot steer a helper halfway",
        "fixed context cost",
        "small tasks cost more",
        "knows nothing of the conversation beyond its brief",
        "collide in the same files",
        "extra turn",
        "No helper is used for a question",
    ] {
        assert!(q.what.contains(must), "item 6 lacks '{must}': {}", q.what);
    }
    // The way to switch it on, and back off.
    assert!(q.what.contains("install --set-helpers on --db <store>"), "{}", q.what);
    assert!(q.say_no.contains("install --set-helpers off --db <store>"), "{}", q.say_no);
    assert!(questionnaire::ASSISTANT_RULES.contains("--set-helpers on|off"), "the assistant knows the command");
    // The two notes live outside the pack, and the questionnaire names exactly them.
    let helper_ids: Vec<String> = ops::install::helper_mode_notes().into_iter().map(|n| n.id).collect();
    assert_eq!(helper_ids, questionnaire::HELPER_MODE_NOTES.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    for id in questionnaire::HELPER_MODE_NOTES {
        assert!(q.what.contains(id) && q.say_no.contains(id), "item 6 must name the note {id}");
        assert!(!ops::install::workflow_starter_pack().iter().any(|n| n.id == *id), "{id} is seeded by default again");
    }
}

#[test]
fn the_work_rules_item_lists_every_other_starter_note_and_none_twice() {
    let pack: Vec<String> = ops::install::workflow_starter_pack().into_iter().map(|n| n.id).collect();
    let rules = questionnaire::work_rule_ids();
    assert_eq!(rules, pack, "item 5 lists the whole starter pack, which holds no helper-mode note");
    assert!(rules.iter().all(|id| !questionnaire::HELPER_MODE_NOTES.contains(&id.as_str())));
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
    let words = ["no", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve"];
    let q = questions().into_iter().find(|q| q.id == "replies").unwrap();
    assert_eq!(ops::install::response_rulebook_rule_count(), entries);
    assert!(q.what.contains(&format!("holds {} reply rules", words[entries])), "{}", q.what);
    assert!(q.line.contains(&format!("{} reply rules", words[entries])), "{}", q.line);
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

fn real_paths() -> questionnaire::Paths {
    questionnaire::Paths {
        installer: Some(PathBuf::from("C:/Users/x/thor2/install.exe")),
        db: Some(PathBuf::from("C:/Users/x/AppData/Local/thor2/thor.db")),
        settings: Some(PathBuf::from("C:/Users/x/.claude/settings.json")),
        tool_server_file: Some(PathBuf::from("C:/Users/x/.claude.json")),
    }
}

/// A blind test (an assistant with ONLY the served text, no AGENTS.md) had to
/// guess the two calls, where the installer lives and which settings file to
/// edit. The lookup now carries them, with the real paths of this install.
#[test]
fn the_lookup_alone_carries_the_two_calls_and_the_real_paths() {
    let text = questionnaire::lookup_text_with(Some(Detection { plan: Plan::Small, tier_found: true }), &real_paths());
    // The two calls, exactly.
    assert!(text.contains("remember(\n  id:   \"owner-setup-answers\",\n  kind: \"report\",\n  text: \"Setup finished with the defaults, nothing asked first. Plan: Pro or smaller"), "{text}");
    assert!(text.contains("retract(id: \"walk-through-the-answer-guard-once\", reason: \"Setup finished: the owner's answers are on record as owner-setup-answers.\")"), "{text}");
    assert!(text.contains("NO project"), "the report takes no project");
    // The real paths, quoted, in their own section and where the commands use them.
    let paths = &text[text.find("PATHS ON THIS MACHINE").expect("a paths section")..];
    for want in [
        "\"C:/Users/x/thor2/install.exe\"",
        "\"C:/Users/x/AppData/Local/thor2/thor.db\"",
        "\"C:/Users/x/.claude/settings.json\"",
        "\"C:/Users/x/.claude.json\"",
    ] {
        assert!(paths.contains(want), "the paths section lacks {want}");
    }
    assert!(text.contains("install --set-helpers on --db \"C:/Users/x/AppData/Local/thor2/thor.db\""), "<store> is filled in: {text}");
    assert!(!text.contains("<store>"), "no placeholder is left when the store is known");
    // The tool server stays when connections go off.
    let hooks_item = &text[text.find("\n2. The connections to Claude Code").unwrap()..];
    let hooks_item = &hooks_item[..hooks_item.find("\n\n").unwrap()];
    assert!(hooks_item.contains("only THOR's four hook entries") && hooks_item.contains("from \"C:/Users/x/.claude/settings.json\""), "{hooks_item}");
    assert!(hooks_item.contains("tool server in \"C:/Users/x/.claude.json\" stays"), "{hooks_item}");
    assert!(hooks_item.contains("after you restart Claude Code"), "{hooks_item}");
    assert!(text.contains("SAY FIRST") && text.contains("THREE MORE FACTS") && text.contains("Lanes.") && text.contains("Project names.") && text.contains("theirs to correct"), "{text}");
}

#[test]
fn without_known_paths_the_text_describes_each_one_instead_of_leaving_a_hole() {
    let text = questionnaire::lookup_text(None);
    let paths = &text[text.find("PATHS ON THIS MACHINE").unwrap()..];
    assert!(paths.contains("install program in the thor2 folder") && paths.contains("~/.claude/settings.json") && paths.contains("~/.claude.json"), "{paths}");
    assert!(text.contains("Plan: <as detected or told>"), "the page for any user keeps the plan open");
}

#[test]
fn an_unknown_anchor_is_stored_pinned_and_offered_once() {
    let rules = questionnaire::ASSISTANT_RULES;
    assert!(rules.contains("whose anchor you do not know") && rules.contains("never invent a hostname"), "{rules}");
    assert!(rules.contains("kind orientation with always true") && rules.contains("offer once"), "{rules}");
    assert!(rules.contains("revise that note with the target"), "{rules}");
}

#[test]
fn the_seeded_store_carries_the_installs_real_paths_and_the_setup_note_names_the_one_place_to_read() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    ops::install::ensure_store(&db).unwrap();
    let paths = real_paths();
    let seeded = ops::install::seed_working_contract_at(&db, None, &paths).unwrap();
    assert!(seeded.iter().all(|s| s.stored), "refused: {:?}", seeded.iter().filter(|s| !s.stored).collect::<Vec<_>>());
    let store = thor_core::event_store::EventStore::open_read_only(&db).unwrap();
    let item = model::store::show(&store, questionnaire::LOOKUP_ID).unwrap();
    assert!(item.text.contains("\"C:/Users/x/thor2/install.exe\"") && item.text.contains("\"C:/Users/x/.claude/settings.json\""), "{}", item.text);
    let note = model::store::show(&store, model::store::SETUP_NOTE_ID).unwrap();
    assert!(note.text.contains("`lookup first-session-questions`") && note.text.contains("one place to read"), "{}", note.text);
}
