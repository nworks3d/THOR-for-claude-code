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
/// middle: every rule is in the block whole or not in it at all. This project's
/// own facts come first, then the heavy global rules, then house style, then the
/// global facts with no weight (see [`render_within_ranked`]); when not
/// everything fits, the rest is replaced by ONE line saying how many were left
/// out, and the caller records as shown only `Rendered::shown`. `None` when
/// there is nothing to render.
pub fn render_within(items: &[RankedItem], budget: usize) -> Option<Rendered> {
    render_within_ranked(items, budget, &std::collections::BTreeSet::new())
}

/// [`render_within`], ranked for relevance (steps 31 and 32). A project's first
/// prompt is not known at session start, so relevance comes from whose fact it
/// is and from what the facts did here lately. Order, one place after the other:
///
/// 1. this project's own facts;
/// 2. the global heavy rules (irreversible, then costly);
/// 3. the global house-style rules;
/// 4. the global facts with no weight at all (no severity): reply habits, other
///    areas, experiments - last, so they only fill what is left.
///
/// Inside a place, a fact that fired in this project's own sessions lately
/// (`recent`: shown at a prompt or before a tool call, never counting a session
/// start) comes first, then the heavier, then id order. Field test 7: about 60
/// percent of a code task's block was noise while the project's own facts shared
/// slots with global style rules.
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
    // Claude Code keeps a long hook output in a file and shows the agent only
    // its first 2 KB (measured 2026-10-01: 53 items, 16 KB, 6 visible), so the
    // order decides what is seen. The sort is stable: ids order what is left.
    let place = |r: &RankedItem| -> u8 {
        use model::item::Severity::{Costly, HouseStyle, Irreversible};
        match (r.item.project.is_some(), r.item.severity) {
            (true, _) => 0,
            (false, Some(Irreversible | Costly)) => 1,
            (false, Some(HouseStyle)) => 2,
            (false, None) => 3,
        }
    };
    let mut ordered: Vec<&RankedItem> = items.iter().collect();
    ordered.sort_by_key(|r| (place(r), !recent.contains(&r.id), r.item.severity.map_or(3u8, |s| s as u8)));
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

/// What a headless session start left out, by ids.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LeftOut {
    pub interactive_only: Vec<String>,
    pub answer_only: Vec<String>,
}

/// Whether the only thing besides `Always` that a fact is bound to is the
/// moment of answering or of claiming something done. Those moments fire
/// nowhere on their own, so such a fact reaches a session only through its
/// `Always`, and what it says is about the reply, which the Stop guard watches.
fn bound_only_to_the_reply(item: &model::item::Item) -> bool {
    use intent::Action::{Answer, ClaimDone};
    let others: Vec<&Binding> = item.bindings.iter().filter(|b| !matches!(b, Binding::Always)).collect();
    !others.is_empty() && others.iter().all(|b| matches!(b, Binding::Moment(Answer | ClaimDone)))
}

/// The start block's own filter (step 32): [`for_session_kind`], plus - for a
/// headless session - the facts bound only to the reply, and the ids of what
/// was left out, so the volume record can say what a headless start did not
/// carry. An interactive session is served everything.
pub fn split_for_session_start(items: Vec<RankedItem>, headless: bool) -> (Vec<RankedItem>, LeftOut) {
    let mut left_out = LeftOut::default();
    if !headless {
        return (items, left_out);
    }
    let mut kept = Vec::with_capacity(items.len());
    for r in items {
        if r.item.tags.iter().any(|t| t == INTERACTIVE_ONLY_TAG) {
            left_out.interactive_only.push(r.id);
        } else if bound_only_to_the_reply(&r.item) {
            left_out.answer_only.push(r.id);
        } else {
            kept.push(r);
        }
    }
    (kept, left_out)
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

    // ------------------------------------------------ the ranking (step 32)

    fn standing(id: &str, project: Option<&str>, severity: Option<model::item::Severity>) -> LiveItem {
        let mut c = item(id, Kind::Rule, vec![Binding::Always], project);
        c.item.severity = severity;
        c
    }

    fn order_of(items: &[LiveItem], project: Option<&str>, recent: &[&str]) -> Vec<String> {
        use std::collections::BTreeSet;
        let recent: BTreeSet<String> = recent.iter().map(|s| s.to_string()).collect();
        render_within_ranked(&select(items, project), HOOK_OUTPUT_LIMIT_BYTES, &recent).unwrap().shown
    }

    /// THE FIELD CASE (field test 7): about 60 percent of a code task's start
    /// block was global style rules and other areas. This project's own facts
    /// come first, then heavy rules, then house style, then facts with no project
    /// and no weight at all.
    #[test]
    fn this_projects_facts_come_first_then_heavy_then_house_style_then_the_unweighted_global_ones() {
        use model::item::Severity::{Costly, HouseStyle, Irreversible};
        let items = [
            standing("a-global-none", None, None),
            standing("b-global-house", None, Some(HouseStyle)),
            standing("c-global-costly", None, Some(Costly)),
            standing("d-own-none", Some("alpha"), None),
            standing("e-global-irreversible", None, Some(Irreversible)),
            standing("f-own-costly", Some("alpha"), Some(Costly)),
        ];
        assert_eq!(
            order_of(&items, Some("alpha"), &[]),
            ["f-own-costly", "d-own-none", "e-global-irreversible", "c-global-costly", "b-global-house", "a-global-none"]
        );
    }

    /// What this project's sessions used lately goes first inside its own place
    /// in that order, and never jumps a place.
    #[test]
    fn a_recent_showing_lifts_a_fact_only_inside_its_own_place() {
        use model::item::Severity::HouseStyle;
        let items = [
            standing("a-own", Some("alpha"), None),
            standing("b-own-used", Some("alpha"), None),
            standing("c-global-house", None, Some(HouseStyle)),
            standing("d-global-none-used", None, None),
        ];
        assert_eq!(
            order_of(&items, Some("alpha"), &["b-own-used", "d-global-none-used"]),
            ["b-own-used", "a-own", "c-global-house", "d-global-none-used"]
        );
    }

    /// A headless run has the Stop guard and no chat: a fact whose only binding
    /// besides Always is the moment of answering or of claiming done is left out
    /// of its start block, and the volume record says how many and which.
    #[test]
    fn a_headless_start_leaves_out_interactive_only_and_answer_only_facts_and_names_them() {
        use intent::Action;
        let plain = item("plain", Kind::Rule, vec![Binding::Always], None);
        let mut chat = item("chat", Kind::Rule, vec![Binding::Always], None);
        chat.item.tags = vec![INTERACTIVE_ONLY_TAG.to_string()];
        let answer = item("answer", Kind::Rule, vec![Binding::Moment(Action::Answer), Binding::Always], None);
        let done = item("done", Kind::Rule, vec![Binding::Always, Binding::Moment(Action::ClaimDone)], None);
        let mixed = item("mixed", Kind::Rule, vec![Binding::Always, Binding::Moment(Action::Answer), Binding::Moment(Action::Push)], None);
        let all = select(&[plain, chat, answer, done, mixed], None);

        let (kept, left_out) = split_for_session_start(all.clone(), true);
        let kept_ids: Vec<&str> = kept.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(kept_ids, ["plain", "mixed"]);
        assert_eq!(left_out.interactive_only, ["chat"]);
        assert_eq!(left_out.answer_only, ["answer", "done"]);

        let (kept, left_out) = split_for_session_start(all, false);
        assert_eq!(kept.len(), 5, "an interactive session is served everything");
        assert!(left_out.interactive_only.is_empty() && left_out.answer_only.is_empty());
    }
}
