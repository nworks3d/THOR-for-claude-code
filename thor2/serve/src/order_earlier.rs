//! The Stop-hook half of the tidy step: THOR holds the end of a turn until
//! the person was offered to put their earlier work in order and either it was
//! carried out or they refused it themselves.
//!
//! The model layer (`model::store`, section "ordering earlier work") keeps the
//! pinned note, the evidence only THOR's own programs can write, and the guard
//! that refuses to retract or rewrite the note before it is settled. This
//! module judges what only the session's own transcript can show:
//!
//! 1. a proposal: an assistant message that carries
//!    `model::store::ORDER_PROPOSAL_MARKER` and a numbered list;
//! 2. the person's answer: a real message they typed AFTER it (never a hook's
//!    text, a tool result, a helper's text or a quoted line: the same reader
//!    the owner-approval guard uses, `attest::owner_text`);
//! 3. what settles it: `install --tidy` ran after that answer (its own
//!    `tidy` evidence), or the assistant filed the Report
//!    `order-earlier-work-answer` quoting the person's refusal exactly and
//!    THOR found those words in the answer. An answer that refuses only some
//!    items needs both.
//!
//! "Nothing to order" is not judged here: `serve inventory` writes that
//! evidence itself (`model::store::nothing_evidence_counts`).

use model::store::OrderEvidence;
use std::path::Path;

/// One thing in the transcript that matters here, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// An assistant message carrying the proposal marker and a numbered list;
    /// `items` is how many numbered items it holds.
    Proposal { at: i64, items: usize },
    /// A real message the person typed.
    Person { at: i64, text: String },
}

/// The words that make an answer a refusal, lower case, whole words only.
const REFUSAL_WORDS: [&str; 11] = ["no", "nee", "neen", "niet", "geen", "nein", "nicht", "kein", "non", "nope", "nah"];

fn words(text: &str) -> Vec<String> {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// Whether `text` holds a refusal word (any language of the list above).
pub fn has_refusal_word(text: &str) -> bool {
    words(text).iter().any(|w| REFUSAL_WORDS.contains(&w.as_str()))
}

/// How many numbered items a proposal holds: the length of the run 1, 2, 3 ...
/// of lines that start with a number and a dot or bracket.
fn numbered_items(text: &str) -> usize {
    let mut next = 1usize;
    for line in text.lines() {
        let t = line.trim_start().trim_start_matches(['*', '#', ' ']);
        let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        let rest = &t[digits.len()..];
        if (rest.starts_with('.') || rest.starts_with(')')) && digits.parse::<usize>().ok() == Some(next) {
            next += 1;
        }
    }
    next - 1
}

fn assistant_text(line: &serde_json::Value) -> Option<String> {
    if line.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return None;
    }
    if line.get("isSidechain").and_then(|b| b.as_bool()) == Some(true) {
        return None;
    }
    let items = line.get("message")?.get("content")?.as_array()?;
    let text: Vec<&str> = items
        .iter()
        .filter(|i| i.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
        .collect();
    (!text.is_empty()).then(|| text.join("\n"))
}

/// The proposals and the person's real messages of one transcript, in order.
pub fn timeline_of(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let Ok(line) = serde_json::from_str::<serde_json::Value>(raw) else { continue };
        let at = line.get("timestamp").and_then(|t| t.as_str()).and_then(crate::eval_filing::unix_of_timestamp).unwrap_or(0);
        if let Some(said) = assistant_text(&line) {
            let items = numbered_items(&said);
            if said.contains(model::store::ORDER_PROPOSAL_MARKER) && items >= 1 {
                out.push(Entry::Proposal { at, items });
            }
        } else if let Some(person) = crate::attest::owner_text(raw) {
            out.push(Entry::Person { at, text: person });
        }
    }
    out
}

/// The timeline of a transcript file, `None` when it cannot be read.
pub fn timeline(transcript: &Path) -> Option<Vec<Entry>> {
    std::fs::read_to_string(transcript).ok().map(|t| timeline_of(&t))
}

/// The double-quoted stretches of `text` (straight or curly quotes).
pub fn quotes_of(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut open: Option<usize> = None;
    for (i, c) in text.char_indices() {
        match (c, open) {
            ('"', None) | ('\u{201C}', None) => open = Some(i + c.len_utf8()),
            ('"', Some(start)) | ('\u{201D}', Some(start)) => {
                let q = text[start..i].trim();
                if q.chars().count() >= 2 {
                    out.push(q.to_string());
                }
                open = None;
            }
            _ => {}
        }
    }
    out
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Which item numbers a refusal names ("2 no" refuses item 2), and whether it
/// also holds a refusal that names no number (that refuses everything).
fn refused_items(quote: &str) -> (Vec<usize>, bool) {
    let w = words(quote);
    let mut numbers = Vec::new();
    let mut bare = false;
    for (i, word) in w.iter().enumerate() {
        if !REFUSAL_WORDS.contains(&word.as_str()) {
            continue;
        }
        match i.checked_sub(1).and_then(|p| w[p].parse::<usize>().ok()) {
            Some(n) => numbers.push(n),
            None => bare = true,
        }
    }
    (numbers, bare)
}

/// What the Stop hook decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Settled: how, in a few words (written into the log).
    Settled(String),
    /// The person was shown the proposal and has not answered yet: the turn
    /// may end, the question IS the end of the turn.
    Waiting,
    /// Still owed: what is missing, in the assistant's words.
    Owed(String),
}

/// Judge the session. `evidence` is the log's `order_evidence`, `answer` the
/// text of the Report `order-earlier-work-answer` when one is on file.
pub fn judge(timeline: &[Entry], evidence: &[OrderEvidence], answer: Option<&str>) -> Verdict {
    let marker = model::store::ORDER_PROPOSAL_MARKER;
    let proposal = timeline.iter().position(|e| matches!(e, Entry::Proposal { .. }));
    let Some(p) = proposal else {
        return Verdict::Owed(format!(
            "No proposal was shown to the person in this session. Run `serve inventory`: when it finds nothing to order it \
             settles this by itself; otherwise write the numbered proposal of THE TIDY STEP in the person's language and end \
             it with the marker {marker} on a line of its own, then wait for their answer"
        ));
    };
    let Entry::Proposal { items, .. } = timeline[p] else { unreachable!() };
    let after: Vec<(i64, &str)> = timeline[p + 1..]
        .iter()
        .filter_map(|e| match e {
            Entry::Person { at, text } => Some((*at, text.as_str())),
            _ => None,
        })
        .collect();
    let Some(&(first_at, first_text)) = after.first() else { return Verdict::Waiting };

    let tidy_after = evidence.iter().any(|e| e.kind == "tidy" && e.at >= first_at);

    // The person's words, quoted by the assistant, found in a real message of the person after the proposal.
    let mut quote_problem = None;
    let mut refusal_covers_all = false;
    let mut refusal_found = false;
    if let Some(answer) = answer {
        let quotes = quotes_of(answer);
        let found: Vec<&String> =
            quotes.iter().filter(|q| after.iter().any(|(_, t)| squash(t).contains(&squash(q)))).collect();
        if quotes.is_empty() {
            quote_problem = Some("the Report holds no quoted words: put the person's own words between double quotes, exactly as they typed them".to_string());
        } else if found.is_empty() {
            quote_problem = Some("none of the quoted words is in a message the person typed after the proposal (their message must contain them verbatim; a helper's text, a hook's text and your own text do not count)".to_string());
        } else if !found.iter().any(|q| has_refusal_word(q)) {
            quote_problem = Some("the person's words read as a yes, so nothing was refused: run `install --tidy` for what they accepted".to_string());
        } else {
            refusal_found = true;
            let mut refused = std::collections::BTreeSet::new();
            let mut bare = false;
            for q in found.iter().filter(|q| has_refusal_word(q)) {
                let (n, b) = refused_items(q);
                refused.extend(n);
                bare |= b;
            }
            refusal_covers_all = bare || (items > 0 && (1..=items).all(|i| refused.contains(&i)));
        }
    }

    if refusal_found && refusal_covers_all {
        return Verdict::Settled("the person refused it in their own words".to_string());
    }
    if refusal_found && tidy_after {
        return Verdict::Settled("part carried out by install --tidy, part refused in the person's own words".to_string());
    }
    if tidy_after && !has_refusal_word(first_text) {
        return Verdict::Settled("carried out by install --tidy after the person's answer".to_string());
    }
    if tidy_after {
        // The answer holds a refusal word but no valid answer Report covers it.
        return Verdict::Owed(format!(
            "`install --tidy` ran, but the person's answer holds a refusal for some items. File the Report '{}' (kind report, \
             no project) that quotes the refusal exactly, between double quotes{}",
            model::store::ORDER_ANSWER_ID,
            quote_problem.map(|p| format!(" (what is wrong now: {p})")).unwrap_or_default()
        ));
    }
    if refusal_found {
        return Verdict::Owed(
            "the person refused only some items: run `install --tidy` for the items they accepted (write the plan file with only those)"
                .to_string(),
        );
    }
    if let Some(problem) = quote_problem {
        return Verdict::Owed(format!("the Report '{}' settles nothing: {problem}", model::store::ORDER_ANSWER_ID));
    }
    let hint = if has_refusal_word(first_text) {
        format!(
            "the person answered and refused (some of) it: file the Report '{}' (kind report, no project) that quotes their own \
             words exactly between double quotes, and run `install --tidy` for any item they accepted",
            model::store::ORDER_ANSWER_ID
        )
    } else {
        "the person answered and their words read as a yes: write the plan file and run `install --tidy --db <store> --plan <file>` for the accepted items".to_string()
    };
    Verdict::Owed(hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tidy_at(at: i64) -> OrderEvidence {
        OrderEvidence { seq: 1, kind: "tidy".to_string(), at, body: serde_json::json!({}) }
    }

    fn proposal(items: usize) -> Entry {
        Entry::Proposal { at: 10, items }
    }

    fn person(at: i64, text: &str) -> Entry {
        Entry::Person { at, text: text.to_string() }
    }

    #[test]
    fn refusal_words_are_whole_words() {
        assert!(has_refusal_word("2 no, because it is private"));
        assert!(has_refusal_word("Nee, liever niet"));
        assert!(!has_refusal_word("yes, go ahead, nothing to add"));
        assert!(!has_refusal_word("ja graag, mooi voorstel"));
    }

    #[test]
    fn numbered_items_counts_the_run() {
        assert_eq!(numbered_items("intro\n1. CV\n2. Trip\n3. Garden\nShall I?"), 3);
        assert_eq!(numbered_items("no list here"), 0);
        assert_eq!(numbered_items("1. a\n5. b"), 1);
    }

    #[test]
    fn nothing_after_the_proposal_is_waiting() {
        assert_eq!(judge(&[proposal(2)], &[], None), Verdict::Waiting);
    }

    #[test]
    fn no_proposal_is_owed() {
        assert!(matches!(judge(&[person(5, "hello")], &[], None), Verdict::Owed(_)));
    }

    #[test]
    fn a_yes_followed_by_a_tidy_event_settles() {
        let t = [proposal(2), person(20, "yes please")];
        assert!(matches!(judge(&t, &[tidy_at(25)], None), Verdict::Settled(_)));
        // The tidy event came BEFORE the person's answer: it settles nothing.
        assert!(matches!(judge(&t, &[tidy_at(15)], None), Verdict::Owed(_)));
    }

    #[test]
    fn a_verified_full_refusal_settles_and_a_yes_quote_does_not() {
        let t = [proposal(2), person(20, "no thanks, leave it all as it is")];
        let ok = "He said: \"no thanks, leave it all as it is\"";
        assert!(matches!(judge(&t, &[], Some(ok)), Verdict::Settled(_)));
        let invented = "He said: \"no, absolutely not\"";
        assert!(matches!(judge(&t, &[], Some(invented)), Verdict::Owed(_)));
        let t2 = [proposal(2), person(20, "yes, do it")];
        assert!(matches!(judge(&t2, &[], Some("He said \"yes, do it\"")), Verdict::Owed(m) if m.contains("read as a yes")));
    }

    #[test]
    fn a_partial_refusal_needs_both_halves() {
        let t = [proposal(3), person(20, "yes, but 2 no, because it is private")];
        let answer = "\"2 no, because it is private\"";
        assert!(matches!(judge(&t, &[], Some(answer)), Verdict::Owed(m) if m.contains("install --tidy")));
        assert!(matches!(judge(&t, &[tidy_at(30)], Some(answer)), Verdict::Settled(_)));
        // Tidy ran but the refusal is not on file.
        assert!(matches!(judge(&t, &[tidy_at(30)], None), Verdict::Owed(m) if m.contains("order-earlier-work-answer")));
    }

    #[test]
    fn a_quote_from_before_the_proposal_is_refused() {
        let t = [person(5, "no way"), proposal(2), person(20, "sure")];
        assert!(matches!(judge(&t, &[], Some("\"no way\"")), Verdict::Owed(_)));
    }

    #[test]
    fn timeline_reads_only_real_prompts_and_marked_proposals() {
        let marker = model::store::ORDER_PROPOSAL_MARKER;
        let lines = [
            r#"{"type":"assistant","timestamp":"2026-03-02T10:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":"Plain chat, 1. not a proposal"}]}}"#.to_string(),
            serde_json::json!({"type":"assistant","timestamp":"2026-03-02T10:01:00Z","message":{"role":"assistant","content":[{"type":"text","text":format!("Proposal\n1. CV\n2. Trip\n{marker}")}]}}).to_string(),
            r#"{"type":"user","timestamp":"2026-03-02T10:02:00Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"no way"}]}}"#.to_string(),
            r#"{"type":"user","isMeta":true,"timestamp":"2026-03-02T10:03:00Z","message":{"role":"user","content":"no, hook text"}}"#.to_string(),
            r#"{"type":"user","timestamp":"2026-03-02T10:04:00Z","message":{"role":"user","content":"> quoted no\nyes please"}}"#.to_string(),
        ];
        let t = timeline_of(&lines.join("\n"));
        assert_eq!(t.len(), 2, "{t:?}");
        assert!(matches!(t[0], Entry::Proposal { items: 2, .. }));
        assert!(matches!(&t[1], Entry::Person { text, .. } if text == "yes please"));
    }
}
