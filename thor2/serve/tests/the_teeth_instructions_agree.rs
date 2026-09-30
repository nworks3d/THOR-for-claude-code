//! Every place that tells an agent how a rule gets its teeth must describe
//! the same trial (step 8): AGENTS.md, the seeded working-contract notes, the
//! Stop hook's teeth question, the fields remember and revise really take,
//! and the receipts THOR's own trial writes.
//!
//! THE DEFECT THIS PINS DOWN (2026-09-26, and again at step 8). The routine
//! told an agent that a justified `no-literal` tag settles a heavy rule while
//! the Stop hook said it did not; an agent that followed one was held by the
//! other, and the cheapest way out of both was a label. Since step 8 THOR
//! tries every rule when it is written and a reason tag answers nothing, so
//! every text an agent reads must say that, name the three outcomes by the
//! words the trial writes, and name only arguments the door accepts.
//!
//! The evaluation routine (`eval-command.example.md`, step 18 of the build)
//! says the same: THOR tries the old rules itself, the agent handles only the
//! frozen batch and the frozen A4 list with the call the start note gives, a
//! reason settles nothing, and the report is filed with the THOR tool alone.
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

/// The evaluation routine with one space for every run of whitespace, so a
/// line break in the prose never hides a phrase.
fn routine() -> String {
    read("eval-command.example.md").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// AGENTS.md's part on writing a note, from its teeth paragraph to the
/// answer-guard paragraph.
fn agents_teeth_section() -> String {
    let agents = std::fs::read_to_string(thor2().parent().expect("thor2/ sits in the repository").join("AGENTS.md"))
        .expect("AGENTS.md");
    let start = agents.find("**Every rule is tried the moment you write it.**").expect("AGENTS.md describes the trial");
    let end = start + agents[start..].find("**A rule about what you SAY").expect("the section ends at the answer guard");
    // One space for every run of whitespace, so a line break in the prose
    // never hides a phrase.
    agents[start..end].split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The body of one `pub struct` in a source file, up to its closing brace.
fn struct_body<'a>(source: &'a str, name: &str) -> &'a str {
    let start = source.find(&format!("pub struct {name} {{")).unwrap_or_else(|| panic!("{name} exists"));
    let end = start + source[start..].find("\n}\n").expect("the struct ends");
    &source[start..end]
}

/// The three outcomes AGENTS.md names are the words the trial writes.
#[test]
fn agents_md_names_the_three_outcomes_by_the_words_the_trial_writes() {
    let section = agents_teeth_section();
    let teeth = read("serve/src/teeth.rs");
    for (outcome, label) in [("**RULE**", "teeth: wired, not proven"), ("**RULE**", "teeth: proven guard"), ("**NOTE**", "teeth: note")] {
        assert!(section.contains(outcome), "AGENTS.md must name the outcome {outcome}");
        assert!(section.contains(&format!("`{label}`")), "AGENTS.md must quote the label '{label}'");
        assert!(teeth.contains(&format!("\"{label}")), "the trial must write the label '{label}' that AGENTS.md quotes");
    }
    assert!(section.contains("**REFUSED**"), "AGENTS.md must say a refusal writes nothing");
    assert!(
        section.contains("\"Hang it: check_kind forbidden, check_literals ['--force']\""),
        "AGENTS.md must quote the hang-it advice in the trial's own form"
    );
    assert!(
        teeth.contains("\"hang it\"") && teeth.contains("capitalise(&hang)"),
        "the trial's refusal must open its advice with 'Hang it'"
    );
    assert!(
        teeth.contains("format!(\"check_kind forbidden, check_literals {}\", list(literals))"),
        "the trial must spell a forbidden tooth the way AGENTS.md quotes it"
    );
}

/// The three arguments AGENTS.md teaches are real fields on both doors, and
/// the retired `trial_check` is on neither: a caller still sending it gets
/// the field list (`deny_unknown_fields`), never a silent pass.
#[test]
fn the_three_arguments_are_real_fields_on_both_doors_and_trial_check_is_gone() {
    let section = agents_teeth_section();
    let mcp = read("mcp/src/lib.rs");
    for args in ["RememberArgs", "ReviseArgs"] {
        let body = struct_body(&mcp, args);
        for field in ["legitimate_calls", "may_change", "accept_refusing"] {
            assert!(body.contains(&format!("pub {field}: Vec<String>")), "{args} must take {field}");
        }
        assert!(!body.contains("trial_check"), "{args} must not take the retired trial_check");
        let before = &mcp[..mcp.find(&format!("pub struct {args} {{")).expect("the struct exists")];
        let attrs = &before[before.rfind("\n\n").unwrap_or(0)..];
        assert!(attrs.contains("deny_unknown_fields"), "{args} must name its fields to a caller sending an old one");
    }
    for field in ["legitimate_calls", "may_change", "accept_refusing"] {
        assert!(section.contains(&format!("`{field}: [")), "AGENTS.md must show {field} with an example");
    }
    assert!(!section.contains("trial_check"), "AGENTS.md must not send an agent to trial_check");
}

/// A reason tag answers nothing: AGENTS.md, the seeded note and the Stop
/// hook all say so, and the hook sends an agent to the trial instead of
/// asking for a quoted call.
#[test]
fn a_reason_tag_answers_nothing_in_every_text_an_agent_reads() {
    let section = agents_teeth_section();
    let seeds = read("ops/src/install.rs");
    let hook = read("serve/src/bin/serve.rs");
    assert!(section.contains("A `no-literal:` tag answers nothing"), "AGENTS.md must say a reason answers nothing");
    assert!(seeds.contains("A no-literal tag answers nothing."), "the seeded note must say a reason answers nothing");
    assert!(
        hook.contains("A tag, a reason, an open point or a refusal on record settles"),
        "the Stop hook must say a reason settles nothing"
    );
    assert!(hook.contains("retest_command_for"), "the Stop hook must send an agent to the trial, by its command");
    assert!(
        hook.contains("mcp__thor__revise with only the rule's id runs the same trial"),
        "and name the write tool's door to the same trial"
    );
    assert!(!hook.contains("QUOTES the legitimate call"), "the Stop hook must not ask for a reason that answers nothing");
    assert!(seeds.contains("Every rule is tried when written."), "the seeded note must say every rule is tried");
}

/// AGENTS.md and the seeded note tell an agent to name a glyph by its code
/// point; the gate really accepts that spelling as naming the glyph.
#[test]
fn a_glyph_named_by_its_code_point_is_named_for_the_gate() {
    let section = agents_teeth_section();
    let seeds = read("ops/src/install.rs");
    assert!(section.contains("`U+2014` in the text for an em dash"), "AGENTS.md must say how to name a glyph");
    assert!(seeds.contains("(U+2014 for a glyph)"), "the seeded note must say how to name a glyph");
    assert!(model::gate::literal_is_named("Never write an em dash (U+2014) into any file.", "\u{2014}"));
    assert!(!model::gate::literal_is_named("Never write an em dash into any file.", "\u{2014}"));
}

/// The routine names the two list sizes in words; the filing gate holds a
/// report to the constants. Changing one without the other fails here.
#[test]
fn the_routine_names_the_list_sizes_the_filing_gate_holds_a_report_to() {
    let words = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve", "thirteen",
        "fourteen", "fifteen", "sixteen",
    ];
    let routine = routine();
    for (name, size) in [("batch", model::store::EVALUATION_TEETH_BATCH), ("A4 list", model::store::EVALUATION_A4_LIST)] {
        let word = words.get(size).unwrap_or_else(|| panic!("name a {name} of {size} in the routine and extend this list"));
        assert!(routine.contains(&format!("at most {word} rules")), "the routine must say the {name} holds at most {word} rules");
    }
}

/// The routine must spell the skip tag exactly as the filing gate parses it,
/// and say what the gate holds a report to: the judgement list is frozen when
/// the evaluation starts (in the log, never at filing), and a reason settles
/// nothing.
#[test]
fn the_routine_describes_the_judgement_list_and_the_skip_the_gate_accepts() {
    let routine = routine();
    let skip = format!("{}<id>:<reason>", model::store::NOT_JUDGEABLE_TAG_PREFIX);
    assert!(routine.contains(&skip), "the routine must name the skip tag as '{skip}'");
    assert!(routine.contains("frozen when the evaluation started"), "the routine must say the list is frozen at the start");
    assert!(routine.contains("THE EVALUATION STARTS IN THOR'S LOG"), "the routine must say where an evaluation starts");
    assert!(
        routine.contains("A tag, a reason, an open point or a refusal on record settles nothing"),
        "the routine must say a reason settles nothing, in the words the Stop hook uses"
    );
    assert!(!routine.contains("A reason counts only after a real try"), "the old no-literal rule is gone");
    assert!(
        routine.contains(&format!("at least {}", ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve"][model::store::NOT_JUDGEABLE_REASON_MIN_CHARS])),
        "the routine must name the minimum reason length the gate holds a skip to"
    );
}

/// The routine names the three arguments the door really takes, and the tool
/// call it gives for a batch rule is the one the start note prints.
#[test]
fn the_routine_names_the_real_arguments_and_the_start_notes_call() {
    let routine = routine();
    let mcp = read("mcp/src/lib.rs");
    for field in ["legitimate_calls", "may_change", "accept_refusing"] {
        assert!(routine.contains(&format!("`{field}`")), "the routine must name {field}");
        assert!(struct_body(&mcp, "ReviseArgs").contains(&format!("pub {field}: Vec<String>")), "revise must take {field}");
    }
    assert!(routine.contains("mcp__thor__revise(id: \"<id>\")"), "the routine must give the call a batch rule gets");
    assert!(read("serve/src/eval_session.rs").contains("mcp__thor__revise(id: \\\"{id}\\\")"), "the start note must print that call");
}

/// AGENTS.md describes the evaluation with numbers; each one is the code's own
/// constant, so changing one without the page fails here.
#[test]
fn agents_md_gives_the_numbers_the_evaluation_really_uses() {
    let agents = std::fs::read_to_string(thor2().parent().expect("thor2/ sits in the repository").join("AGENTS.md"))
        .expect("AGENTS.md");
    let page = agents.split_whitespace().collect::<Vec<_>>().join(" ");
    let expected = [
        format!("at most {} that need a fix", model::store::EVALUATION_TEETH_BATCH),
        format!("at most {} that only have to name", model::store::EVALUATION_A4_LIST),
        format!("at least {} minutes of its own work", serve::usefulness::EVAL_OWN_WORK_MINUTES),
        format!("{} times or more without a verdict", model::store::JUDGEMENT_DEBT_AFTER),
        format!("at least {} characters", model::store::NOT_JUDGEABLE_REASON_MIN_CHARS),
        format!("covers the next {} hours", serve::usefulness::EVAL_REPORT_COVERS_HOURS),
        format!("last {} days", serve::usefulness::EVAL_FLOOR_DAYS),
        format!("more than {} of them", model::store::OPEN_POINTS_OWNER_BUDGET),
        format!("for up to {} seconds", serve::eval_session::MACHINE_PASS_BOX_SECS),
    ];
    for phrase in expected {
        assert!(page.contains(&phrase), "AGENTS.md must say '{phrase}'");
    }
    assert!(
        serve::usefulness::EVAL_ELSEWHERE_MAX_MINUTES == 60 && page.contains("started less than an hour ago"),
        "AGENTS.md must give the hour a running evaluation elsewhere exempts a session for"
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
