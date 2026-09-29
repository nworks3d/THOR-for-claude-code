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
//! The evaluation routine (`eval-command.example.md`) is rewritten to the
//! trial in step 18 of the build; until then only its batch, skip and
//! filing parts are compared here.
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
