//! Surface 1: SESSION START. Serves exactly the items bound `Always`, scoped
//! to global (no project) plus the current project - full, never capped,
//! never ranked: CONTRACT.md's promise here is "the standing rules, complete,
//! or nothing", not "the four most relevant ones".
//!
//! `Always` reaches an injection surface through exactly this door.
//! `rank::binding_matches` refuses `Always` outright (see its own doc
//! comment) precisely so the moment/prompt surfaces can never also serve it -
//! the five surfaces never take each other's places (CONTRACT.md).

use crate::live::LiveItem;
use crate::rank::RankedItem;
use model::item::Binding;

/// Every live item whose kind can fire, is bound `Always`, and whose own
/// project is either unset (global) or exactly `project` - in the same
/// deterministic id order `live::live_items` already provides (this is a
/// pure filter over that order, so no further sort is applied: "no ranking,
/// no threshold" is not just a policy here, it is the absence of a sort
/// call). A different project's `Always` item is excluded, never shown and
/// never counted.
pub fn select(candidates: &[LiveItem], project: Option<&str>) -> Vec<RankedItem> {
    candidates
        .iter()
        .filter(|c| c.item.kind.can_fire())
        .filter(|c| c.item.bindings.iter().any(|b| matches!(b, Binding::Always)))
        .filter(|c| crate::project::applies_to(c.item.project.as_deref(), project))
        .map(|c| RankedItem { id: c.id.clone(), item: c.item.clone() })
        .collect()
}

/// The block text, or `None` when nothing applies - never a truncated list:
/// every item `select` returns is rendered whole, with no `render::cap` in
/// this surface's path at all.
///
/// Each shown item's line opens with `[id]`, straight from `RankedItem.id`
/// (the event log's own entity id, not the copy carried inside the item
/// body - see `live::LiveItem`) - the same bracket form and the same field
/// `render::render_text` uses for the moment/prompt surfaces. See that
/// function's own doc comment for the defect this closes: a served fact
/// with no id forces an assistant that notices it is wrong to guess search
/// terms to find the item again before it can call `revise`. There is no
/// budget to protect the id from here, unlike that surface: this one never
/// truncates anything to begin with (see the paragraph above), so the id is
/// always additive, never a trade against how much of the body is delivered
/// (see `adding_the_id_never_shrinks_how_much_of_the_body_is_delivered`).
///
/// The `falsifier` is deliberately NOT printed here, and this is the one
/// place worth arguing about, so the argument is written down.
///
/// Its value is at WRITE time: it forces the author to name the observation
/// that would make the fact false, and refusing a rule without one is what
/// keeps the store honest. That value is already banked by the time anything
/// reaches this surface. Printing it back on every session start roughly
/// DOUBLED this block on the real store (1900 to 3700 characters) for a
/// benefit nothing has measured - and this whole rebuild exists because the
/// blocks were too noisy, so paying that with no evidence is the exact trade
/// 2.0 is against.
///
/// The line 2.0 draws instead: surfaces that PUSH at a reader stay minimal;
/// surfaces a reader ASKS carry everything. So the falsifier travels on `get`
/// (one item, on request, whole) and is counted in `status`/`doctor`, and
/// never rides along on session start, the moment of action, or the prompt.
pub fn render(items: &[RankedItem]) -> Option<String> {
    render_within(items, usize::MAX).map(|r| r.text)
}

/// The most bytes a hook output may carry. Claude Code keeps a hook output
/// longer than 10,000 characters in a file and shows the agent only its first
/// 2 KB (measured on this machine's own transcripts: the largest output that
/// arrived whole was 9,972 characters, the smallest that was cut was 9.8 KB,
/// i.e. 10,035 bytes). 9,500 stays under it with room for a multi-byte letter.
pub const HOOK_OUTPUT_LIMIT_BYTES: usize = 9_500;

/// Room kept for the one-line note about the rules that did not fit.
const NOT_FIT_NOTE_RESERVE: usize = 220;

/// What `render_within` put in the block.
pub struct Rendered {
    pub text: String,
    /// The ids that are really in `text`, in order. Only these were shown.
    pub shown: Vec<String>,
    /// How many standing rules did not fit and are not in `text`.
    pub hidden: usize,
}

/// The standing-rules block, kept within `budget` bytes. Nothing is cut in the
/// middle: every rule is in the block whole or not in it at all. The heaviest
/// rules (the ones that can refuse or cannot be undone) come first, then this
/// project's own before the global ones, then id order; when not everything
/// fits, the rest is replaced by ONE line saying how many were left out, and
/// the caller records as shown only `Rendered::shown`. `None` when there is
/// nothing to render.
pub fn render_within(items: &[RankedItem], budget: usize) -> Option<Rendered> {
    render_within_ranked(items, budget, &std::collections::BTreeSet::new())
}

/// [`render_within`], ranking with what this project has been using (step 31).
/// A project's first prompt is not known at session start, so relevance cannot
/// come from it; it comes from what the facts did here lately. Order:
///
/// 1. the facts that fired in this project's own sessions lately (`recent`:
///    shown at a prompt or before a tool call, never counting a session start),
///    heaviest first, this project's own before the global ones;
/// 2. then every other heavy rule (one that can refuse or cannot be undone),
///    this project's own first;
/// 3. then the rest, by weight and id.
///
/// The cap and the "N did not fit" line are exactly [`render_within`]'s.
pub fn render_within_ranked(
    items: &[RankedItem],
    budget: usize,
    recent: &std::collections::BTreeSet<String>,
) -> Option<Rendered> {
    if items.is_empty() {
        return None;
    }
    let head: Vec<String> = vec![crate::render::FRAMING_LINE.to_string(), "Standing rules for this session:".to_string()];
    // THE HEAVIEST FIRST (step 27, D12). Claude Code keeps a long hook output
    // in a file and shows the agent only its first 2 KB, and in id order those
    // were whichever six facts had the alphabetically first ids (measured
    // 2026-10-01: 53 items, 16 KB, 6 visible). A stable sort puts what can
    // refuse or cannot be undone first; a rule of this project goes before a
    // global one of the same weight (step 29); ids order what is left. Step 31
    // puts what fired in this project lately before all of that (field test 6:
    // 28 of 33 start facts had nothing to do with the task).
    let heavy = |r: &RankedItem| matches!(r.item.severity, Some(model::item::Severity::Irreversible | model::item::Severity::Costly));
    let mut ordered: Vec<&RankedItem> = items.iter().collect();
    ordered.sort_by_key(|r| {
        let group: u8 = if recent.contains(&r.id) {
            0
        } else if heavy(r) {
            1
        } else {
            2
        };
        (group, r.item.severity.map_or(3u8, |s| s as u8), r.item.project.is_none())
    });
    let lines: Vec<String> = ordered.iter().map(|r| format!("- [{}] {}", r.id, r.item.text)).collect();
    let head_len: usize = head.iter().map(|l| l.len() + 1).sum();
    let all_len: usize = head_len + lines.iter().map(|l| l.len() + 1).sum::<usize>();
    let room = if all_len <= budget { usize::MAX } else { budget.saturating_sub(NOT_FIT_NOTE_RESERVE) };

    let mut out = head;
    let mut shown = Vec::new();
    let mut used = head_len;
    for (ranked, line) in ordered.iter().zip(lines) {
        if used.saturating_add(line.len() + 1) > room {
            continue;
        }
        used += line.len() + 1;
        shown.push(ranked.id.clone());
        out.push(line);
    }
    let hidden = ordered.len() - shown.len();
    if hidden > 0 {
        out.push(format!(
            "[THOR] {hidden} more standing rule(s) did not fit in this block and are not shown. Look them up with the lookup tool when something you are about to do may be covered."
        ));
    }
    Some(Rendered { text: out.join("\n"), shown, hidden })
}

/// A fact tagged this way is written for the owner's interactive chat (he
/// orchestrates through background agents, wants an answer before any agent,
/// pastes prompts) and is kept out of the session start and the prompt
/// injection of a headless session, such as `claude -p` (step 31). Field test
/// 6 (2026-10-02): a headless run was served such facts, delegated everything
/// to a background agent that could get no shell permission, and delivered
/// nothing. Facts that fire at a tool call or a file are not affected.
pub const INTERACTIVE_ONLY_TAG: &str = "interactive-only";

/// Whether a Claude Code entrypoint is a headless one. Claude Code names how
/// it was started in `CLAUDE_CODE_ENTRYPOINT` (and in every transcript line):
/// the owner's chats read `claude-desktop` or `cli`, `claude -p` reads
/// `sdk-cli`, the SDKs `sdk-ts` and `sdk-py`. Every `sdk...` one is headless.
pub fn entrypoint_is_headless(entrypoint: &str) -> bool {
    entrypoint.trim().to_ascii_lowercase().starts_with("sdk")
}

/// The entrypoint a transcript names, read from the head of its text.
pub fn entrypoint_in_transcript_head(head: &str) -> Option<String> {
    let at = head.find("\"entrypoint\":\"")? + "\"entrypoint\":\"".len();
    let value: String = head[at..].chars().take_while(|c| *c != '"').collect();
    (!value.is_empty()).then_some(value)
}

/// `items` without the interactive-only facts when the session is headless;
/// untouched otherwise.
pub fn for_session_kind(items: Vec<RankedItem>, headless: bool) -> Vec<RankedItem> {
    if !headless {
        return items;
    }
    items.into_iter().filter(|r| !r.item.tags.iter().any(|t| t == INTERACTIVE_ONLY_TAG)).collect()
}

/// Appended after the standing-rules block exactly once: right after Claude
/// Code has just summarized the conversation. A replay on two real sessions
/// found the one real miss was not a "tested" claim made carelessly, but one
/// made right after a `/compact` with nothing re-checked - the summary reads
/// like a fact but is a paraphrase, and paraphrases lose the difference
/// between "I ran this" and "the summary says I ran this". This never
/// blocks; it is additive text on the one surface that already fires at the
/// top of a session, addressed to the assistant rather than framed as one of
/// the owner's own stored rules (see `render::FRAMING_LINE`'s doc comment
/// for that other, narrower framing problem - this text is THOR's own,
/// not the owner's, so it does not need it).
pub const COMPACT_REMINDER: &str = "[THOR] This session's context was just summarized, so the summary is not proof of anything in it. Before you call something done, tested, flashed, deployed or live, check it again now - run the check, read the file, or ask the machine.";

/// `Some(COMPACT_REMINDER)` exactly when this SessionStart payload is the
/// main session's own (`is_subagent` false) and its `source` field reads
/// `"compact"`; `None` for every other source ("startup", "resume",
/// "clear", absent, or anything else) and `None` for a subagent payload
/// regardless of source.
///
/// The subagent check is defense in depth, not a live gate: Claude Code's
/// `SessionStart` never actually fires inside a subagent at all (confirmed
/// 2026-08-05, see `payload_is_from_a_subagent`'s own doc comment in
/// `bin/serve.rs`, and `serve/tests/subagent_hook_behavior.rs` for the dead
/// code this same fact already retired once). Kept here anyway because this
/// function is a cheap, pure predicate that has no other way to prove its
/// own "never for a subagent" half except by being handed one - the same
/// reasoning `hook_once`'s other gates already apply at each of their own
/// call sites (see the module-level rule against a shared early return).
pub fn compact_reminder(source: Option<&str>, is_subagent: bool) -> Option<String> {
    if is_subagent || source != Some("compact") {
        return None;
    }
    Some(COMPACT_REMINDER.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use intent::Action;
    use model::item::{Item, Kind, TargetKind};

    fn item(id: &str, kind: Kind, bindings: Vec<Binding>, project: Option<&str>) -> LiveItem {
        LiveItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind,
                text: format!("rule {id}"),
                bindings,
                severity: None,
                project: project.map(str::to_string),
                tags: vec![],
                expires: None,
                key: None,
                falsifier: None,
                check: None,
            },
        }
    }

    #[test]
    fn a_global_always_item_is_served_regardless_of_the_current_project() {
        let c = item("g1", Kind::Rule, vec![Binding::Always], None);
        let hits = select(&[c], Some("thor2"));
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn an_always_item_for_the_current_project_is_served() {
        let c = item("p1", Kind::Rule, vec![Binding::Always], Some("thor2"));
        let hits = select(&[c], Some("thor2"));
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn an_always_item_scoped_to_a_different_project_is_never_served() {
        // Named after the defect it prevents: session start must never leak
        // another project's standing rules into this one's session.
        let c = item("other1", Kind::Rule, vec![Binding::Always], Some("some-other-project"));
        let hits = select(&[c], Some("thor2"));
        assert!(hits.is_empty(), "a different project's Always item must never appear here");
    }

    #[test]
    fn with_no_current_project_only_global_always_items_are_served() {
        let global = item("g2", Kind::Rule, vec![Binding::Always], None);
        let scoped = item("p2", Kind::Rule, vec![Binding::Always], Some("thor2"));
        let hits = select(&[global, scoped], None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "g2");
    }

    #[test]
    fn an_item_bound_only_to_a_moment_is_never_served_here() {
        // The five surfaces never take each other's places: a Moment/Target
        // binding is surface 2/3's door, never surface 1's.
        let c = item("m1", Kind::Rule, vec![Binding::Moment(Action::Push)], None);
        let hits = select(&[c], None);
        assert!(hits.is_empty(), "a Moment-only binding must never be served by session start");
    }

    #[test]
    fn an_item_bound_only_to_a_target_is_never_served_here() {
        let c = item(
            "t1",
            Kind::Rule,
            vec![Binding::Target { kind: TargetKind::Path, value: "src/main.rs".to_string() }],
            None,
        );
        let hits = select(&[c], None);
        assert!(hits.is_empty(), "a Target-only binding must never be served by session start");
    }

    #[test]
    fn a_report_bound_always_never_reaches_session_start_even_bypassing_the_write_gate() {
        // The write gate already refuses a Report with a binding; this
        // constructs one directly (as if that check had never run) to prove
        // this surface is a second, independent lock on the door by KIND -
        // mirroring rank::rs's own `a_report_can_never_reach_a_gate_even_bypassing_the_write_gate`.
        let c = item("r1", Kind::Report, vec![Binding::Always], None);
        let hits = select(&[c], None);
        assert!(hits.is_empty(), "a Report must never be selected, gate or no gate");
    }

    #[test]
    fn a_chunk_bound_always_never_reaches_session_start_even_bypassing_the_write_gate() {
        let c = item("k1", Kind::Chunk, vec![Binding::Always], None);
        let hits = select(&[c], None);
        assert!(hits.is_empty(), "a Chunk must never be selected, gate or no gate");
    }

    #[test]
    fn session_start_never_caps_even_past_the_moment_surfaces_four_item_limit() {
        // "volledig, niet afgekapt" (CONTRACT.md): render::MAX_ITEMS (4) must
        // not apply here at all - ten items must all come back.
        let items: Vec<LiveItem> =
            (0..10).map(|i| item(&format!("i{i}"), Kind::Rule, vec![Binding::Always], None)).collect();
        let hits = select(&items, None);
        assert_eq!(hits.len(), 10, "session start must never cap, unlike the moment/prompt surfaces");
        let block = render(&hits).unwrap();
        for i in 0..10 {
            assert!(block.contains(&format!("rule i{i}")), "block must contain item i{i} verbatim: {block}");
        }
    }

    #[test]
    fn no_always_items_renders_no_block() {
        assert!(render(&[]).is_none());
    }

    // ------------------------------------------------------------- framing

    /// The defect this guards against: a standing rule is written as a
    /// constraint on the OWNER's own behaviour ("never do X"), because that
    /// is what a rule is FOR - a blank subagent (no project, no prior turns
    /// to place it in) has no way to tell that apart from an actual task
    /// instruction. Same requirement as `render::render_text`'s own guard
    /// test; this surface must carry it too. See INJECTION-FRAMING.md for the
    /// incident and `render::FRAMING_LINE`'s own doc comment for the argument.
    #[test]
    fn the_pushed_block_always_opens_with_the_framing_line() {
        let c = item("g1", Kind::Rule, vec![Binding::Always], None);
        let block = render(&select(&[c], None)).unwrap();
        assert!(
            block.starts_with(crate::render::FRAMING_LINE),
            "session start must open with the same framing line as every other surface: {block}"
        );
        let second_line = block.lines().nth(1).unwrap_or("");
        assert_eq!(second_line, "Standing rules for this session:", "{block}");
    }

    // ---------------------------------------------------------- falsifier

    /// The defect this guards against: a second line per item on the surface
    /// that fires at EVERY session start. Measured on the real store it took
    /// the standing-rules block from about 1900 to 3700 characters, for a
    /// benefit nothing has measured - in a system rebuilt because its blocks
    /// were too noisy. The falsifier's value is at write time and on `get`;
    /// see `render`'s own doc comment for the whole argument.
    #[test]
    fn a_falsifier_never_rides_along_on_the_pushed_block() {
        let mut c = item("f1", Kind::Rule, vec![Binding::Always], None);
        c.item.falsifier = Some("this stops holding once the store is retired".to_string());
        let hits = select(&[c], None);
        let block = render(&hits).unwrap();
        assert!(
            !block.contains("falsified by"),
            "an injection surface must not carry the falsifier: {block}"
        );
        assert!(
            !block.contains("this stops holding once the store is retired"),
            "and not its text under any other wording either: {block}"
        );
    }

    /// The item itself still renders whole, falsifier or not - the field is
    /// invisible here, never a reason to drop or shorten a rule.
    #[test]
    fn the_rule_itself_is_rendered_whole_either_way() {
        let mut with = item("f1", Kind::Rule, vec![Binding::Always], None);
        with.item.falsifier = Some("some observation".to_string());
        let without = item("f2", Kind::Rule, vec![Binding::Always], None);
        assert_eq!(without.item.falsifier, None, "fixture sanity");

        let with_text = with.item.text.clone();
        let without_text = without.item.text.clone();
        let a = render(&select(&[with], None)).unwrap();
        let b = render(&select(&[without], None)).unwrap();
        assert!(a.contains(&with_text), "the rule text must survive: {a}");
        assert!(b.contains(&without_text), "the rule text must survive: {b}");
        assert_eq!(a.lines().count(), b.lines().count(), "same shape either way");
    }

    // -------------------------------------------------------------------- id

    /// THE DEFECT THIS PREVENTS: a served fact carried no id at all, so an
    /// assistant that noticed the fact was wrong had no way to correct it
    /// directly - it had to guess search terms to find the item again first,
    /// turning a one-call `revise` into three steps. Every shown item now
    /// carries its id. Same defect `render::render_text` already closed for
    /// the moment/prompt surfaces; this closes it here too.
    #[test]
    fn a_session_start_item_shows_its_id() {
        let c = item("g1", Kind::Rule, vec![Binding::Always], None);
        let block = render(&select(&[c], None)).unwrap();
        assert!(block.contains("g1"), "the block must carry the item's id: {block}");
    }

    /// Showing the id somewhere is not enough - it has to come out whole and
    /// cleanly delimited, so an assistant can lift it out and pass it
    /// straight to `revise`'s own `id` argument with no editing. Uses an id
    /// shaped like a real one (a project prefix plus a colon, matching
    /// `render.rs`'s own test of the same name) to prove the bracket, not
    /// the id's own content, is what an assistant would split on.
    #[test]
    fn the_shown_id_is_in_a_form_that_can_be_passed_straight_to_revise() {
        let c = item("thor2:01ARZ3NDEKTSV4RRFFQ69G5FAV", Kind::Rule, vec![Binding::Always], None);
        let block = render(&select(&[c], None)).unwrap();
        assert!(
            block.contains("[thor2:01ARZ3NDEKTSV4RRFFQ69G5FAV]"),
            "the id must appear whole, inside brackets, exactly as `revise` expects it: {block}"
        );
    }

    /// THE CRITICAL DEFECT THIS PREVENTS: the id riding in on the item's own
    /// text, so adding it silently shrinks how much of the fact's body is
    /// delivered compared to before. This surface never truncates to begin
    /// with (see `render`'s own doc comment: no `render::cap` in this path
    /// at all, unlike the moment/prompt surfaces), so the id can only ever
    /// be additive here - proven directly with a long body, distinct from
    /// the id's own characters so a dropped body character could never hide
    /// inside it.
    #[test]
    fn adding_the_id_never_shrinks_how_much_of_the_body_is_delivered() {
        let mut c = item("i0", Kind::Rule, vec![Binding::Always], None);
        c.item.text = "y".repeat(300);
        let long_text = c.item.text.clone();
        let block = render(&select(&[c], None)).unwrap();
        assert!(block.contains(&long_text), "the full 300-char body must still appear unmodified: {block}");
        assert!(block.contains("[i0]"), "and the id must appear alongside it, not instead of any of it: {block}");
    }

    // --------------------------------------------------- compact reminder

    #[test]
    fn a_compact_source_in_the_main_session_returns_the_reminder() {
        assert_eq!(compact_reminder(Some("compact"), false).as_deref(), Some(COMPACT_REMINDER));
    }

    #[test]
    fn startup_resume_and_clear_sources_return_nothing() {
        for source in ["startup", "resume", "clear"] {
            assert!(
                compact_reminder(Some(source), false).is_none(),
                "source {source} must never return the reminder"
            );
        }
    }

    #[test]
    fn a_missing_source_returns_nothing() {
        assert!(compact_reminder(None, false).is_none());
    }

    #[test]
    fn an_unrecognised_source_returns_nothing() {
        assert!(compact_reminder(Some("something-future-claude-code-adds"), false).is_none());
    }

    #[test]
    fn a_subagent_payload_never_returns_the_reminder_even_with_source_compact() {
        assert!(compact_reminder(Some("compact"), true).is_none());
    }

    #[test]
    fn the_heaviest_standing_rules_come_first_and_none_is_left_out() {
        use model::item::Severity;
        let mut light = item("a-light", Kind::Orientation, vec![Binding::Always], None);
        let mut costly = item("b-costly", Kind::Rule, vec![Binding::Always], None);
        let mut hard = item("c-hard", Kind::Rule, vec![Binding::Always], None);
        let plain = item("d-plain", Kind::Orientation, vec![Binding::Always], None);
        costly.item.severity = Some(Severity::Costly);
        hard.item.severity = Some(Severity::Irreversible);
        light.item.severity = Some(Severity::HouseStyle);
        let items = select(&[light, costly, hard, plain], None);
        let block = render(&items).unwrap();
        let at = |id: &str| block.find(&format!("[{id}]")).unwrap_or_else(|| panic!("{id} missing: {block}"));
        assert!(at("c-hard") < at("b-costly") && at("b-costly") < at("a-light") && at("a-light") < at("d-plain"), "{block}");
    }

    // ------------------------------------------------ the size limit (step 29)

    fn many(n: usize, len: usize) -> Vec<LiveItem> {
        (0..n)
            .map(|i| {
                let mut c = item(&format!("id-{i:03}"), Kind::Rule, vec![Binding::Always], None);
                c.item.text = format!("{i:03} {}", "w".repeat(len));
                c
            })
            .collect()
    }

    /// THE FIELD CASE (2026-10-01): 53 rules, 15.6 KB. Claude Code showed the
    /// agent 2 KB of it while THOR counted all 53 as shown.
    #[test]
    fn a_block_over_the_limit_keeps_whole_rules_only_and_says_how_many_were_left_out() {
        let items = select(&many(53, 280), None);
        let r = render_within(&items, HOOK_OUTPUT_LIMIT_BYTES).unwrap();
        assert!(r.text.len() <= HOOK_OUTPUT_LIMIT_BYTES, "{} bytes", r.text.len());
        assert_eq!(r.shown.len() + r.hidden, 53);
        assert!(r.hidden > 0 && !r.shown.is_empty());
        assert!(r.text.contains(&format!("{} more standing rule(s) did not fit", r.hidden)), "{}", r.text);
        for id in &r.shown {
            assert!(r.text.contains(&format!("[{id}]")), "a shown id must be in the text: {id}");
        }
        let in_text = r.text.lines().filter(|l| l.starts_with("- [")).count();
        assert_eq!(in_text, r.shown.len(), "what is reported shown is exactly what is in the block");
    }

    #[test]
    fn a_block_that_fits_is_unchanged_and_has_no_note() {
        let items = select(&many(5, 100), None);
        let r = render_within(&items, HOOK_OUTPUT_LIMIT_BYTES).unwrap();
        assert_eq!(r.hidden, 0);
        assert_eq!(r.shown.len(), 5);
        assert_eq!(r.text, render(&items).unwrap());
        assert!(!r.text.contains("did not fit"));
    }

    #[test]
    fn the_heaviest_rules_are_the_ones_that_survive_the_limit() {
        use model::item::Severity;
        let mut all = many(40, 280);
        all[39].item.severity = Some(Severity::Irreversible);
        let items = select(&all, None);
        let r = render_within(&items, 3_000).unwrap();
        assert_eq!(r.shown.first().map(String::as_str), Some("id-039"), "the irreversible rule must come first and fit");
    }

    #[test]
    fn this_projects_own_rules_come_before_global_ones_of_the_same_weight() {
        let global = item("a-global", Kind::Rule, vec![Binding::Always], None);
        let own = item("z-own", Kind::Rule, vec![Binding::Always], Some("thor2"));
        let items = select(&[global, own], Some("thor2"));
        let r = render_within(&items, HOOK_OUTPUT_LIMIT_BYTES).unwrap();
        assert_eq!(r.shown, vec!["z-own".to_string(), "a-global".to_string()]);
    }
}
