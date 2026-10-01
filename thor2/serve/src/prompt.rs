//! Surface 3: PER PROMPT. Silent by construction unless the prompt itself
//! resolves to a moment or names a target some live item declares -
//! CONTRACT.md's "STIL, tenzij de prompt zelf oplost naar een moment of een
//! doel noemt dat een item declareert". There is no ranked pool to fall back
//! to here: this module's only job is turning a raw prompt's text into the
//! `ServeInput` that `rank::select` decides everything else from, and
//! `select` already requires an actual binding match (and never matches
//! `Always` - see `rank::binding_matches`), so a prompt that resolves to
//! nothing simply selects nothing. Zero candidates in means zero items out;
//! there is no separate "nothing resolved" branch to get wrong.
//!
//! FLAGGED CHOICE (not measured, unlike the write gate's corpus-backed
//! rules): "the prompt names a moment" is decided by running the exact same
//! closed detectors a real command/file path already use
//! (`intent::from_command`, `intent::from_path`) over the prompt's raw text.
//! "The prompt names a target" is decided by checking, for every live item's
//! own declared `Target` value, whether that value's last path segment (the
//! part a sentence realistically names - see `normalize::last_segment`'s own
//! doc comment) appears as a whole token in the prompt. Both are testable and
//! tested below; neither has been measured against real prompts the way
//! `model::gate`'s ground 7 removal was measured against 2962 real items.

use crate::input::ServeInput;
use crate::live::LiveItem;
use model::item::Binding;
use model::normalize::{last_segment, normalize_target};

/// Split on anything that is not a path/word character, so a target's last
/// segment ("swap-binary.ps1", "main.rs") is found only as a WHOLE token in
/// the prompt - never as a slice of a longer word (a prompt about
/// "personas" must never be treated as naming a target named "rs"). Each
/// token is reduced to ITS OWN last segment before comparing (same
/// `normalize::last_segment` doctrine used everywhere else in this
/// workspace), so a prompt naming the target by its full path ("src/main.rs")
/// still resolves a binding declared by bare name alone ("main.rs").
fn contains_word(haystack_normalized: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    haystack_normalized.split(is_break).any(|token| last_segment(token) == needle)
}

/// The one place this file decides where a token ends, so the prompt and the
/// needle can never be split by different rules - which is how a comparison
/// silently stops matching anything.
fn is_break(c: char) -> bool {
    !(c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '/')
}

/// Is this anchor named in the prompt, as one word or as a run of words?
///
/// THE DEFECT THIS CLOSES, measured on the owner's live store 2026-08-16: 305
/// fireable items - 21.8% of them - carry an anchor whose value contains a
/// space ("Aluminium 6061", "10T pinion", "git reset --hard"). `contains_word`
/// compares each token of the prompt against the WHOLE needle, and a single
/// token can never equal a two-word string, so every one of those was
/// unreachable here. Not stale, not crowded out: unreachable by construction,
/// and invisible to every count because their anchors resolve fine.
///
/// `rank::binding_matches` was taught about multi-word anchors on 2026-08-14.
/// This surface never was. This is that same repair, finished.
///
/// A single-word needle takes the old path unchanged, so nothing that resolved
/// before can resolve differently now - the only outcomes this adds are matches
/// where there were none.
fn contains_phrase(haystack_normalized: &str, needle: &str) -> bool {
    let needle_tokens: Vec<&str> = needle.split(is_break).filter(|t| !t.is_empty()).collect();
    match needle_tokens.len() {
        0 => false,
        1 => contains_word(haystack_normalized, needle_tokens[0]),
        n => {
            let hay: Vec<&str> =
                haystack_normalized.split(is_break).filter(|t| !t.is_empty()).collect();
            // Consecutive, in order: "aluminium 6061" is named by "voor aluminium
            // 6061 geldt", never by "aluminium is niet 6061". A bag of words here
            // would fire on any prompt that happens to use both, which is exactly
            // the widening this workspace refuses.
            hay.windows(n).any(|window| {
                window.iter().zip(&needle_tokens).all(|(t, w)| last_segment(t) == last_segment(w))
            })
        }
    }
}

/// The wrappers a harness puts around text that is NOT the owner's own typing:
/// a message from another session, a subagent's hand-back, a background task's
/// notification, a local-command echo, a system reminder. Field case
/// 2026-10-01: such texts were read as the owner's prompts and drew prod-data
/// and deploy facts for words some other party wrote.
const NOT_THE_OWNERS: &[&str] = &[
    "<cross-session-message",
    "<agent-message",
    "<task-notification",
    "[subagent hand-back]",
    "<local-command-",
    "<system-reminder",
];

/// Whether `prompt` can be the owner's own typing. A prompt that opens with one
/// of the harness wrappers above is somebody else's words.
pub fn is_owners_own_text(prompt: &str) -> bool {
    let head = prompt.trim_start().to_lowercase();
    !NOT_THE_OWNERS.iter().any(|w| head.starts_with(w))
}

/// Words that turn what follows into something NOT to do.
pub const NEGATORS: &[&str] = &[
    "niet", "niets", "geen", "nooit", "nergens", "zonder", "no", "not", "never", "without", "nothing", "don't", "dont",
    "doesn't", "doesnt", "won't", "wont", "shouldn't", "mustn't", "cannot", "can't", "cant",
];

/// Words that end a negation's reach inside one sentence ("niet pushen, maar
/// wel committen").
pub const CONTRASTS: &[&str] = &["maar", "but", "behalve", "except", "wel", "instead", "however"];

/// `prompt` with every negated stretch blanked: from a negation word to the end
/// of its sentence (a full stop followed by a space, `!`, `?`, `;`, `:` or a
/// line break), or to a contrast word, whichever comes first. A comma does not
/// end it: "niets deployen, pushen of mergen" negates all three. Everything
/// outside such a stretch is returned as it was, so an affirmative instruction
/// in the same prompt still resolves its moments.
pub fn without_negated(prompt: &str) -> String {
    let chars: Vec<char> = prompt.chars().collect();
    let mut out = String::with_capacity(prompt.len());
    let mut negating = false;
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String, negating: &mut bool| {
        if word.is_empty() {
            return;
        }
        let lower = word.to_lowercase().replace('\u{2019}', "'");
        if CONTRASTS.contains(&lower.as_str()) {
            *negating = false;
        }
        if NEGATORS.contains(&lower.as_str()) {
            *negating = true;
        }
        if !*negating {
            out.push_str(word);
        }
        word.clear();
    };
    for (i, &c) in chars.iter().enumerate() {
        let next_is_space = chars.get(i + 1).is_none_or(|n| n.is_whitespace());
        let word_char = c.is_alphanumeric() || matches!(c, '\'' | '\u{2019}' | '-' | '_' | '/' | '\\')
            || (c == '.' && !word.is_empty() && !next_is_space);
        if word_char {
            word.push(c);
            continue;
        }
        flush(&mut word, &mut out, &mut negating);
        let ends_sentence = matches!(c, '!' | '?' | ';' | ':' | '\n') || (c == '.' && next_is_space);
        if ends_sentence {
            negating = false;
            out.push(c);
        } else if negating {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    flush(&mut word, &mut out, &mut negating);
    out
}

/// Build the `ServeInput` a raw prompt resolves to. Moments: the union of
/// what `intent::from_command` and `intent::from_path` detect in the raw
/// text (a prompt that literally instructs a destructive command must
/// resolve the same way running it would). Targets: one entry per live
/// item's own `Target` binding whose last segment is named, verbatim, as a
/// token in the prompt - never a fuzzy/ranked closeness match, only "is this
/// exact thing named or not".
pub fn resolve(prompt: &str, candidates: &[LiveItem]) -> ServeInput {
    let mut input = ServeInput::default();
    if prompt.trim().is_empty() {
        return input;
    }

    // A negated action is no moment: "niet pushen", "no deploy", "don't push
    // --force" say the opposite of what the detectors look for.
    let asserted = without_negated(prompt);
    for signal in intent::from_command(&asserted) {
        if !input.moments.contains(&signal.action) {
            input.moments.push(signal.action);
        }
    }
    for signal in intent::from_path(&asserted) {
        if !input.moments.contains(&signal.action) {
            input.moments.push(signal.action);
        }
    }

    let normalized_prompt = normalize_target(prompt);
    for c in candidates {
        for binding in &c.item.bindings {
            if let Binding::Target { kind, value } = binding {
                let seg = last_segment(&normalize_target(value)).to_string();
                if contains_phrase(&normalized_prompt, &seg) {
                    let entry = (*kind, value.clone());
                    if !input.targets.contains(&entry) {
                        input.targets.push(entry);
                    }
                }
            }
        }
    }

    input
}

#[cfg(test)]
mod tests {
    use super::*;
    use intent::Action;
    use model::item::{Item, Kind, TargetKind};

    fn target_item(id: &str, kind: TargetKind, value: &str) -> LiveItem {
        LiveItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Orientation,
                text: format!("about {id}"),
                bindings: vec![Binding::Target { kind, value: value.to_string() }],
                severity: None,
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: None,
                check: None,
            },
        }
    }

    /// THE DEFECT THIS CLOSES, measured 2026-08-16: 305 fireable items carry an
    /// anchor with a space in it, and this surface compared each token of the
    /// prompt against the whole value - so not one of them could ever resolve
    /// here, however plainly the prompt named it.
    #[test]
    fn a_multi_word_anchor_resolves_when_the_prompt_names_it() {
        let items = vec![target_item("legering", TargetKind::Command, "Aluminium 6061")];
        let input = resolve("welke boutmaat neem ik voor aluminium 6061", &items);
        assert_eq!(input.targets.len(), 1, "the prompt names it word for word");
    }

    /// Consecutive and in order, never a bag of words: a prompt that merely uses
    /// both words is not naming the thing. Widening this to "any word" would
    /// fire every anchor whose first word is common.
    #[test]
    fn the_same_words_apart_do_not_resolve_it() {
        let items = vec![target_item("legering", TargetKind::Command, "Aluminium 6061")];
        let input = resolve("aluminium is hier niet hetzelfde als 6061", &items);
        assert!(input.targets.is_empty(), "the words are present but the phrase is not");
    }

    /// The property that makes this safe to ship: a single-word anchor takes
    /// exactly the path it always took, so nothing that resolved before can
    /// resolve differently now.
    #[test]
    fn a_single_word_anchor_is_unchanged() {
        let items = vec![target_item("bestand", TargetKind::Path, "src/main.rs")];
        assert_eq!(resolve("kijk eens in main.rs", &items).targets.len(), 1);
        assert!(resolve("dit gaat over personas", &items).targets.is_empty());
    }

    #[test]
    fn an_empty_prompt_resolves_to_nothing() {
        let input = resolve("", &[]);
        assert!(input.is_empty());
    }

    #[test]
    fn a_whitespace_only_prompt_resolves_to_nothing() {
        let input = resolve("   \n\t  ", &[]);
        assert!(input.is_empty());
    }

    #[test]
    fn an_ordinary_question_resolves_to_nothing() {
        let input = resolve("What is the weather like today, in general?", &[]);
        assert!(input.is_empty());
    }

    #[test]
    fn a_prompt_that_names_a_destructive_command_resolves_its_moment() {
        let input = resolve("please run git push --force origin main for me", &[]);
        assert!(input.moments.contains(&Action::Push));
    }

    #[test]
    fn a_prompt_naming_a_declared_targets_last_segment_resolves_that_target() {
        let candidates = vec![target_item("t1", TargetKind::Path, r"C:\repo\swap-binary.ps1")];
        let input = resolve("can you check swap-binary.ps1 before we continue", &candidates);
        assert!(
            input.targets.contains(&(TargetKind::Path, r"C:\repo\swap-binary.ps1".to_string())),
            "expected the declared target to resolve, got {:?}",
            input.targets
        );
    }

    #[test]
    fn a_prompt_that_only_partially_contains_a_targets_name_does_not_match() {
        // "personas.rs" must never be treated as naming a target whose last
        // segment is "rs" alone - this proves the match is whole-token, not
        // substring.
        let candidates = vec![target_item("t2", TargetKind::Symbol, "rs")];
        let input = resolve("let's talk about user personas.rs handling", &candidates);
        assert!(input.targets.is_empty(), "a partial word match must never resolve a target: {:?}", input.targets);
    }

    #[test]
    fn a_prompt_naming_no_declared_target_resolves_no_targets() {
        let candidates = vec![target_item("t3", TargetKind::Path, "src/main.rs")];
        let input = resolve("let's discuss the roadmap for next quarter", &candidates);
        assert!(input.targets.is_empty());
    }

    #[test]
    fn the_same_target_is_never_added_twice() {
        let candidates =
            vec![target_item("t4", TargetKind::Path, "src/main.rs"), target_item("t5", TargetKind::Path, "src/main.rs")];
        let input = resolve("look at src/main.rs please", &candidates);
        assert_eq!(input.targets.len(), 1);
    }

    #[test]
    fn a_negated_stretch_is_blanked_to_the_end_of_its_sentence() {
        assert!(!without_negated("niets deployen of pushen").contains("pushen"));
        assert!(!without_negated("Alleen op dev, niets deployen of pushen.").contains("deployen"));
        assert!(without_negated("Alleen op dev, niets deployen of pushen.").contains("Alleen op dev"));
        assert!(!without_negated("don't git push --force").contains("push"));
        assert!(!without_negated("no deploy today").contains("deploy"));
    }

    #[test]
    fn a_contrast_word_or_a_full_stop_ends_the_negation() {
        let text = without_negated("niet git push, maar wel git commit");
        assert!(text.contains("git commit") && !text.contains("push"), "{text}");
        let text = without_negated("Do not touch it. Now git push it.");
        assert!(text.contains("git push"), "{text}");
    }

    #[test]
    fn a_prompt_without_a_negation_is_unchanged() {
        let text = "please run git push --force-with-lease origin main.";
        assert_eq!(without_negated(text), text);
    }

    #[test]
    fn a_word_that_merely_contains_a_negator_is_not_one() {
        assert!(without_negated("node --test and nothing_else_here git push").contains("git push"));
        assert!(without_negated("run the notebook, then git push").contains("git push"));
    }

    #[test]
    fn harness_wrappers_are_not_the_owners_typing() {
        for wrapped in [
            "<cross-session-message from=\"x\">hi</cross-session-message>",
            "  <agent-message from=\"a\">[Subagent hand-back] done",
            "<task-notification><summary>x</summary>",
            "[Subagent hand-back] the report",
            "<local-command-caveat>Caveat",
            "<system-reminder>x</system-reminder>",
        ] {
            assert!(!is_owners_own_text(wrapped), "{wrapped}");
        }
        assert!(is_owners_own_text("Loop de testsuite na en verbeter ze"));
        assert!(is_owners_own_text("/thor-eval"));
    }
}
