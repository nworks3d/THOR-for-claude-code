//! The Stop-hook half of the tidy step: THOR holds the end of a turn until
//! the person was offered to put their earlier work in order and either it was
//! carried out or they refused it themselves.
//!
//! The model layer (`model::store`, section "ordering earlier work") keeps the
//! pinned note, the evidence only THOR's own programs can write, and the guard
//! that refuses to retract or rewrite the note before it is settled. This
//! module judges what only the session's own transcript can show:
//!
//! 1. a proposal: the first assistant message with visible text that comes
//!    after a tool call running `serve ... inventory` (not `--digest`) and holds
//!    a numbered list starting at 1. (nothing is added to the message: this is
//!    read from what really happened in the record);
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
use std::path::{Path, PathBuf};
use thor_core::event_store::EventStore;

// ------------------------------------------------------------------ one owner
//
// The tidy step belongs to ONE conversation: the first one that is held for it
// (the one in whose record the welcome was answered). Any other conversation
// gets at most one plain line and is never held. If the owner shows no activity
// for `model::store::EVALUATION_IDLE_MINUTES` (the same idleness the weekly
// evaluation uses for "another session runs this"), the next conversation that
// ends a turn takes over. Nothing here settles the step: only the evidence and
// the person's own answer do (`judge`).

/// The small file beside the store that names the owning conversation.
pub const OWNER_FILE: &str = "order-owner.json";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Owner {
    pub session: String,
    pub since_unix: i64,
    /// When the owner was last held for the step (written at every Stop of it).
    pub seen_unix: i64,
    /// How strong this conversation's stake is (`RANK_*`): the proposal was
    /// shown in its own record, the first-session setup ran in it, or neither.
    #[serde(default)]
    pub rank: u8,
}

/// The proposal was shown in this conversation's own record.
pub const RANK_PROPOSED: u8 = 2;
/// The first-session setup ran in this conversation (the person answered the welcome there).
pub const RANK_SETUP: u8 = 1;
/// No stake: nothing in the record ties it to the setup.
pub const RANK_NONE: u8 = 0;

/// Whose step it is, as seen by the conversation that asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owns {
    /// This conversation owns it (it just took it, or already had it).
    Me,
    /// Another conversation owns it and was active within the idle window.
    Other { session: String, idle_minutes: i64 },
}

fn owner_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(OWNER_FILE)
}

/// The owner on file, `None` when there is none or the file is unreadable (the
/// next conversation then simply takes it).
pub fn owner_on_file(db: &Path) -> Option<Owner> {
    std::fs::read_to_string(owner_path(db)).ok().and_then(|t| serde_json::from_str(&t).ok())
}

/// A conversation with no stake that asked while nobody with a stake had: when
/// it first asked (sidecar), so the step is never left undone for ever.
const WAITING_FILE: &str = "order-waiting.json";

fn waiting_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(WAITING_FILE)
}

fn waiting_since(db: &Path) -> Option<i64> {
    std::fs::read_to_string(waiting_path(db)).ok().and_then(|t| t.trim().parse().ok())
}

/// Ask for the step. A conversation can OWN it only with a stake in its own
/// record, in this order: it showed the proposal (`RANK_PROPOSED`), then the
/// first-session setup ran in it (`RANK_SETUP`). A higher rank takes the step
/// from a lower one at once; the same rank takes it over only from an owner
/// that was quiet for the idle window. A conversation with no stake
/// (`RANK_NONE`) is never held while a conversation with a stake owns the step
/// and is active, and while none has yet; but if no conversation with a stake
/// has been active for 30 minutes, the next one that asks owns it with that
/// rank, so nothing stays undone for ever. `record` false (a read-only run)
/// writes nothing.
pub fn claim_or_defer(store: &EventStore, db: &Path, session: &str, now_unix: i64, record: bool, rank: u8) -> Owns {
    let window = model::store::EVALUATION_IDLE_MINUTES;
    let current = owner_on_file(db);
    let others = current.as_ref().filter(|o| o.session != session);
    if let Some(owner) = others {
        // A time in the future (a clock set back) counts as now, never as later.
        let seen = owner.seen_unix.min(now_unix);
        let by_file = (now_unix - seen) / 60;
        let by_log = crate::usefulness::session_idle_minutes(store, &owner.session, now_unix, window);
        let alive = by_file < window || by_log.is_some();
        if alive && rank <= owner.rank {
            let idle_minutes = by_log.map_or(by_file, |l| l.min(by_file)).max(0);
            return Owns::Other { session: owner.session.clone(), idle_minutes };
        }
    } else if current.is_none() && rank == RANK_NONE {
        // Nobody owns it yet and this conversation has no stake: it waits for the
        // conversation that has one, for at most the idle window.
        let waited = waiting_since(db).map(|t| t.min(now_unix));
        match waited {
            Some(t) if (now_unix - t) / 60 >= window => {}
            Some(_) => return Owns::Other { session: String::new(), idle_minutes: 0 },
            None => {
                if record {
                    let _ = crate::store_dir::write(&waiting_path(db), now_unix.to_string());
                }
                return Owns::Other { session: String::new(), idle_minutes: 0 };
            }
        }
    }
    if record {
        let mine_before = current.filter(|o| o.session == session);
        let since = mine_before.as_ref().map_or(now_unix, |o| o.since_unix);
        let kept_rank = mine_before.map_or(RANK_NONE, |o| o.rank);
        let mine = Owner { session: session.to_string(), since_unix: since, seen_unix: now_unix, rank: rank.max(kept_rank) };
        if let Ok(text) = serde_json::to_string(&mine) {
            let _ = crate::store_dir::write(&owner_path(db), text);
        }
        if rank > RANK_NONE {
            let _ = std::fs::remove_file(waiting_path(db));
        }
    }
    Owns::Me
}

/// The one plain line a conversation that does not own the step gets. It names
/// no session and no minutes, so it never changes and is said once.
pub fn not_yours_line() -> String {
    "[THOR] Another conversation is finishing the last setup step, so this one can carry on. Nothing is needed from you here."
        .to_string()
}

/// Whether the timeline shows the proposal.
pub fn has_proposal(timeline: &[Entry]) -> bool {
    timeline.iter().any(|e| matches!(e, Entry::Proposal { .. }))
}

/// This conversation's stake in the step, read from its own record.
pub fn rank_of(timeline: &[Entry]) -> u8 {
    if has_proposal(timeline) {
        RANK_PROPOSED
    } else if timeline.iter().any(|e| matches!(e, Entry::Setup)) {
        RANK_SETUP
    } else {
        RANK_NONE
    }
}

/// One thing in the transcript that matters here, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// The proposal (see the module doc);
    /// `items` is how many numbered items it holds.
    Proposal { at: i64, items: usize },
    /// A real message the person typed.
    Person { at: i64, text: String },
    /// The first-session setup ran here: the assistant stored the setup answers or
    /// retracted the setup note in this conversation.
    Setup,
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
/// of lines that start with a number. Whatever symbols come first on a line
/// are skipped (a bullet, bold stars, a quote mark, a heading hash, an emoji,
/// a table bar), and the number may be followed by a dot, a bracket, a
/// backslash-escaped dot, bold stars or, in a table, a bar. A line that starts
/// with a letter is never counted, so ordinary prose is not read as a list.
fn numbered_items(text: &str) -> usize {
    let mut next = 1usize;
    for line in text.lines() {
        let t = line.trim_start_matches(|c: char| !c.is_alphanumeric());
        let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        let rest = t[digits.len()..].trim_start_matches(['*', '_', '\\']);
        let in_table = line.trim_start().starts_with('|') && rest.trim_start().starts_with('|');
        // A dot or bracket that is followed by white space (or ends the line), so
        // a decimal such as 1.2 GB is not item 1.
        let list_mark = (rest.starts_with('.') || rest.starts_with(')'))
            && rest[1..].trim_start_matches(['*', '_']).chars().next().is_none_or(char::is_whitespace);
        let after_number = list_mark || in_table;
        if after_number && digits.parse::<usize>().ok() == Some(next) {
            next += 1;
        }
    }
    next - 1
}

/// Whether a shell command runs THOR's `serve` with the `inventory` subcommand
/// as a separate word and without `--digest`: the program name is `serve` or
/// `serve.exe`, bare or as the end of a quoted or plain path. The PowerShell
/// `Start-Process -FilePath serve.exe -ArgumentList '--db','x','inventory'`
/// form counts too (a comma separates words like a space).
pub fn runs_inventory(command: &str) -> bool {
    let tokens: Vec<String> = command
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(|t| t.trim_matches(|c| c == '"' || c == '\'' || c == '&' || c == ';' || c == '|' || c == '(' || c == ')').to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.iter().any(|t| t == "--digest" || t.starts_with("--digest=")) {
        return false;
    }
    // Text that is only printed is no run: echo, printf, Write-Host, Write-Output.
    // (Other ways to quote the words are not caught: that would need a full shell parser.)
    let first = tokens.first().map(|t| t.to_lowercase()).unwrap_or_default();
    if ["echo", "printf", "write-host", "write-output"].contains(&first.as_str()) {
        return false;
    }
    let is_serve = |t: &str| {
        let base = t.rsplit(['/', '\\']).next().unwrap_or("").to_lowercase();
        base == "serve" || base == "serve.exe"
    };
    tokens.iter().position(|t| is_serve(t)).is_some_and(|at| tokens[at + 1..].iter().any(|t| t == "inventory"))
}

/// The proposal and the person's real messages of one transcript, in order.
/// The proposal is the first assistant message with visible text after an
/// inventory call (`runs_inventory`) that holds a numbered list; text in the
/// same message as an inventory call counts only when it comes after it.
pub fn timeline_of(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut inventory_seen = false;
    let mut proposed = false;
    let mut setup_seen = false;
    for raw in text.lines() {
        let Ok(line) = serde_json::from_str::<serde_json::Value>(raw) else { continue };
        let at = line.get("timestamp").and_then(|t| t.as_str()).and_then(crate::eval_filing::unix_of_timestamp).unwrap_or(0);
        let main_assistant = line.get("type").and_then(|t| t.as_str()) == Some("assistant")
            && line.get("isSidechain").and_then(|b| b.as_bool()) != Some(true);
        if main_assistant {
            let items = line.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array());
            for item in items.into_iter().flatten() {
                match item.get("type").and_then(|t| t.as_str()) {
                    Some("tool_use") => {
                        let shell = matches!(item.get("name").and_then(|n| n.as_str()), Some("Bash") | Some("PowerShell"));
                        let command = item.get("input").and_then(|i| i.get("command")).and_then(|c| c.as_str()).unwrap_or("");
                        if shell && runs_inventory(command) {
                            inventory_seen = true;
                        }
                        if !setup_seen {
                            let input = item.get("input").map(|i| i.to_string()).unwrap_or_default();
                            if input.contains(model::store::OWNER_SETUP_ANSWERS_ID) || input.contains(model::store::SETUP_NOTE_ID) {
                                setup_seen = true;
                                out.push(Entry::Setup);
                            }
                        }
                    }
                    Some("text") if inventory_seen && !proposed => {
                        let said = item.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        let n = numbered_items(said);
                        if n >= 1 {
                            proposed = true;
                            out.push(Entry::Proposal { at, items: n });
                        }
                    }
                    _ => {}
                }
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

/// How many of the person's last messages are checked against THOR's own record.
const CONFIRMED_WINDOW: usize = 12;

/// The timeline with only the person's messages that THOR's own record of what he typed confirms
/// (`prompt_record`): the last messages of the saved conversation must be the last prompts of the
/// record, in the same order, so a message line added to the saved conversation by anyone else is
/// no answer of his. Older messages (outside the window the record keeps) and every message when the
/// record holds nothing or disagrees leave the timeline without a person's message: an answer to the
/// proposal then has to arrive through a normal prompt. The proposal and the setup are kept as they were.
pub fn confirm_with_record(db: &Path, session: &str, timeline: Vec<Entry>) -> Vec<Entry> {
    let persons: Vec<usize> = timeline.iter().enumerate().filter(|(_, e)| matches!(e, Entry::Person { .. })).map(|(i, _)| i).collect();
    let tail: Vec<usize> = persons[persons.len().saturating_sub(CONFIRMED_WINDOW)..].to_vec();
    let pairs: Vec<(String, String)> = tail
        .iter()
        .filter_map(|i| match &timeline[*i] {
            Entry::Person { text, .. } => Some((text.clone(), String::new())),
            _ => None,
        })
        .collect();
    let confirmed: Vec<String> = crate::prompt_record::confirmed_pairs(db, Some(session), pairs).into_iter().map(|(t, _)| t).collect();
    timeline
        .into_iter()
        .enumerate()
        .filter(|(i, e)| !matches!(e, Entry::Person { .. }) || (tail.contains(i) && matches!(e, Entry::Person { text, .. } if confirmed.contains(text))))
        .map(|(_, e)| e)
        .collect()
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

/// Every double-quote character, straight and curly, opening and closing.
fn is_double_quote(c: char) -> bool {
    matches!(c, '"' | '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}')
}

/// A text as compared for a quote: every double-quote character removed, white
/// space squashed and trimmed, case kept.
pub fn plain(text: &str) -> String {
    squash(&text.chars().filter(|c| !is_double_quote(*c)).collect::<String>())
}

/// Every stretch of `text` between two double-quote characters (not only
/// neighbouring ones), in `plain` form. A message that itself holds quote
/// characters ("nee, laat \"Reizen\" maar staan") is quoted whole by the stretch
/// from the first to the last quote character. A one-character quote counts.
/// At most 24 quote characters are looked at.
pub fn quote_candidates(text: &str) -> Vec<String> {
    let marks: Vec<(usize, usize)> = text.char_indices().filter(|(_, c)| is_double_quote(*c)).map(|(i, c)| (i, c.len_utf8())).take(24).collect();
    let mut out: Vec<String> = Vec::new();
    for (a, &(start, len)) in marks.iter().enumerate() {
        for &(end, _) in &marks[a + 1..] {
            let q = plain(&text[start + len..end]);
            if !q.is_empty() && !out.contains(&q) {
                out.push(q);
            }
        }
    }
    out
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Which proposal items a quoted message of the person names ("2 no, because
/// it is private" names item 2). A quote that names no item of the proposal
/// refuses all of it, in any language: there is no word list on this route.
fn named_items(quote: &str, items: usize) -> Vec<usize> {
    words(quote).iter().filter_map(|w| w.parse::<usize>().ok()).filter(|n| (1..=items).contains(n)).collect()
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
    let proposal = timeline.iter().position(|e| matches!(e, Entry::Proposal { .. }));
    let Some(p) = proposal else {
        return Verdict::Owed(format!(
            "No proposal was shown to the person in this session. Run `serve inventory`: when it finds nothing to order it \
             settles this by itself; otherwise write the numbered proposal of THE TIDY STEP in the person's language, as a numbered list \
             (a line starting with 1.) in the message right after the inventory call, then wait for their answer"
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
    // THE ANSWER is the first thing the person typed after the proposal that is
    // not a question (a question is asked about the proposal, it does not answer it).
    // One exception, so that a real answer that happens to end in a question mark
    // is not lost: when THOR's tidy record came before any message that is not a
    // question, the last message of the person before that record is the answer.
    // A tidy with no message of the person before it answers nothing.
    let proposal_at = match timeline[p] {
        Entry::Proposal { at, .. } => at,
        _ => 0,
    };
    let is_question = |t: &str| t.trim().ends_with('?');
    let first_tidy = evidence.iter().filter(|e| e.kind == "tidy" && e.at >= proposal_at).map(|e| e.at).min();
    let first_real: Option<(i64, &str)> = after.iter().find(|(_, t)| !is_question(t)).copied();
    let last_before = |t: i64| after.iter().filter(|(a, _)| *a <= t).last().copied();
    let answer_msg: Option<(i64, &str)> = match (first_real, first_tidy) {
        (Some((at, _)), Some(t)) if at > t => last_before(t).or(first_real),
        (Some(f), _) => Some(f),
        (None, Some(t)) => last_before(t),
        (None, None) => None,
    };
    let (first_at, first_text) = answer_msg.unwrap_or((i64::MAX, ""));
    let tidy_after = answer_msg.is_some() && evidence.iter().any(|e| e.kind == "tidy" && e.at >= first_at);

    // THE REFUSAL ROUTE. The assistant files the Report quoting, exactly and
    // between double quotes, one WHOLE message the person typed after the
    // proposal (compared after trimming and squashing white space; a piece of a
    // message does not count, so "Wait" is not "Wait, I first want to think").
    // THOR checks the quote against the person's own messages (never a helper's,
    // a hook's or the assistant's words). There is no word list here: a refusal
    // in any language, however short, is accepted this way. A message that names
    // some items of the proposal refuses only those; any other refuses all.
    let mut quote_problem = None;
    let mut refusal_covers_all = false;
    let mut refusal_found = false;
    if let Some(answer) = answer {
        // Quote characters inside a message are ignored on both sides, so a message
        // that holds quotes (or is a single character) can still be quoted whole.
        let quotes = quote_candidates(answer);
        let found: Vec<&String> = quotes.iter().filter(|q| after.iter().any(|(_, t)| plain(t) == **q)).collect();
        if quotes.is_empty() {
            quote_problem = Some("the Report holds no quoted words: put the person's own words between double quotes, exactly as they typed them".to_string());
        } else if found.is_empty() {
            quote_problem = Some("none of the quoted words is a whole message the person typed after the proposal (quote one message completely, verbatim; a piece of it, a helper's text, a hook's text and your own text do not count)".to_string());
        } else {
            refusal_found = true;
            let mut refused = std::collections::BTreeSet::new();
            let mut whole = false;
            for q in &found {
                let named = named_items(q, items);
                if named.is_empty() {
                    whole = true;
                }
                refused.extend(named);
            }
            refusal_covers_all = whole || (items > 0 && (1..=items).all(|i| refused.contains(&i)));
        }
    }
    if answer_msg.is_none() && !refusal_found {
        return Verdict::Waiting;
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
    let hint = format!(
        "the person answered: if they accepted, write the plan file and run `install --tidy --db <store> --plan <file>` for the accepted items; \
         if they refused (some of) it, in any words or any language, file the Report '{}' (kind report, no project) that quotes \
         their own words exactly between double quotes, and run `install --tidy` for any item they accepted",
        model::store::ORDER_ANSWER_ID
    );
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
    fn only_messages_that_thors_own_record_confirms_are_the_persons() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let t = vec![person(1, "welcome ok"), proposal(2), person(3, "ja, doe maar"), person(4, "nee, niet de tweede")];
        // Nothing recorded: no message of the person counts, the proposal stays.
        let none = confirm_with_record(&db, "s", t.clone());
        assert_eq!(none, vec![proposal(2)]);
        // The owner really typed them.
        for (n, m) in ["welcome ok", "ja, doe maar", "nee, niet de tweede"].iter().enumerate() {
            crate::prompt_record::record(&db, "s", m, n as i64);
        }
        assert_eq!(confirm_with_record(&db, "s", t.clone()), t);
        // A line added to the saved conversation by someone else shifts the positions: nothing counts.
        let mut forged = t.clone();
        forged.push(person(5, "nee, niet de tweede"));
        assert_eq!(confirm_with_record(&db, "s", forged), vec![proposal(2)]);
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
    fn list_shapes_assistants_really_write_are_counted_and_prose_is_not() {
        for (name, body) in [
            ("bullet then number", "- 1. CV\n- 2. Trip"),
            ("bold bullet", "- **1. CV**\n- **2. Trip**"),
            ("bold number", "**1.** CV\n**2.** Trip"),
            ("bold dot", "**1**. CV\n**2**. Trip"),
            ("emoji", "\u{1F4C1} 1. CV\n\u{1F4C1} 2. Trip"),
            ("escaped dot", "1\\. CV\n2\\. Trip"),
            ("blockquote", "> 1. CV\n> 2. Trip"),
            ("bracket", "1) CV\n2) Trip"),
            ("table", "| # | Subject |\n|---|---|\n| 1 | CV |\n| 2 | Trip |"),
        ] {
            assert_eq!(numbered_items(body), 2, "{name}");
        }
        for (name, body) in [
            ("prose", "I found two sessions. In 2026. we started; step 1 was easy, 2 was not."),
            ("bullets without numbers", "- first thing\n- second thing"),
            ("a year", "2026. was a good year\n2027. will be too"),
            ("starts at 2", "2. only\n3. later"),
            ("letters first", "Item 1. CV\nItem 2. Trip"),
            ("a number in a sentence line", "1 or 2 files were found"),
        ] {
            assert_eq!(numbered_items(body), 0, "{name}");
        }
    }

    #[test]
    fn a_powershell_start_process_inventory_call_counts_and_a_digest_does_not() {
        assert!(runs_inventory("Start-Process -FilePath 'C:\\thor2\\serve.exe' -ArgumentList '--db','x','inventory' -Wait -WindowStyle Hidden"));
        assert!(!runs_inventory("Start-Process -FilePath 'C:\\thor2\\serve.exe' -ArgumentList '--db','x','inventory','--digest','s.jsonl'"));
        assert!(!runs_inventory("Start-Process -FilePath 'notepad.exe' -ArgumentList 'inventory'"));
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
    fn a_verified_quote_of_any_message_settles_the_refusal_and_an_invented_one_does_not() {
        let t = [proposal(2), person(20, "no thanks, leave it all as it is")];
        let ok = "He said: \"no thanks, leave it all as it is\"";
        assert!(matches!(judge(&t, &[], Some(ok)), Verdict::Settled(_)));
        let invented = "He said: \"no, absolutely not\"";
        assert!(matches!(judge(&t, &[], Some(invented)), Verdict::Owed(_)));
        // Refusals that hold none of the listed words, in two languages.
        for said in ["not now", "laat maar", "skip it", "later", "niks doen"] {
            let t = [proposal(2), person(20, said)];
            let report = format!("He said \"{said}\"");
            assert!(matches!(judge(&t, &[], Some(&report)), Verdict::Settled(_)), "{said}");
        }
    }

    #[test]
    fn a_question_is_not_the_answer_while_a_real_answer_follows_and_a_tidy_must_follow_the_answer() {
        // The real answer comes after the question, and the tidy after the real answer: settled.
        let t = [proposal(2), person(20, "what does that mean?"), person(40, "ok do it")];
        assert!(matches!(judge(&t, &[tidy_at(45)], None), Verdict::Settled(_)));
        // A tidy that ran before the real answer, with a question already asked, answers that question (see
        // `an_answer_that_ends_in_a_question_mark_counts_when_the_tidy_followed_it`); with NO person message
        // before it, it answers nothing.
        assert!(matches!(judge(&[proposal(2), person(40, "ok do it")], &[tidy_at(30)], None), Verdict::Owed(_)));
        // Only a question so far, no tidy: the person has not answered.
        assert_eq!(judge(&[proposal(2), person(20, "why?")], &[], None), Verdict::Waiting);
    }

    #[test]
    fn a_piece_of_a_message_does_not_settle_but_the_whole_message_does() {
        let said = "Wait, I first want to think about it";
        let t = [proposal(2), person(20, said)];
        assert!(matches!(judge(&t, &[], Some("He said \"Wait\"")), Verdict::Owed(m) if m.contains("whole message")));
        assert!(matches!(judge(&t, &[], Some("He said \"Wait, I first want to think\"")), Verdict::Owed(_)));
        assert!(matches!(judge(&t, &[], Some(&format!("He said \"{said}\""))), Verdict::Settled(_)));
        // White space and line breaks do not matter; letters do.
        assert!(matches!(judge(&t, &[], Some("He said \"Wait,  I first want to think\nabout it\"")), Verdict::Settled(_)));
        assert!(matches!(judge(&t, &[], Some("He said \"wait, i first want to think about it\"")), Verdict::Owed(_)));
    }

    #[test]
    fn an_answer_that_ends_in_a_question_mark_counts_when_the_tidy_followed_it() {
        let said = "ja, en mag nummer 2 ook in de cv-map?";
        let t = [proposal(2), person(20, said), person(60, "thanks")];
        // The tidy ran after the question-shaped answer and before "thanks": settled, and "thanks" does not bring the hold back.
        assert!(matches!(judge(&t, &[tidy_at(30)], None), Verdict::Settled(_)));
        // A tidy that ran before the person said anything after the proposal answers nothing.
        let early = [proposal(2), person(100, said)];
        assert!(!matches!(judge(&early, &[tidy_at(30)], None), Verdict::Settled(_)));
        // A real (non-question) answer before the tidy stays the answer.
        let t2 = [proposal(2), person(20, "why?"), person(25, "ok do it"), person(60, "thanks")];
        assert!(matches!(judge(&t2, &[tidy_at(30)], None), Verdict::Settled(_)));
    }

    #[test]
    fn a_message_with_quote_characters_or_one_letter_can_be_quoted_whole() {
        let said = "nee, laat \"Reizen\" maar staan";
        let t = [proposal(2), person(20, said)];
        // Quoted with the inner quotes kept, removed or curly.
        for report in [
            format!("He said \"{said}\""),
            "He said \"nee, laat Reizen maar staan\"".to_string(),
            "He said \u{201C}nee, laat \u{201C}Reizen\u{201D} maar staan\u{201D}".to_string(),
        ] {
            assert!(matches!(judge(&t, &[], Some(&report)), Verdict::Settled(_)), "{report}");
        }
        // A piece of it is still not the message.
        assert!(matches!(judge(&t, &[], Some("He said \"Reizen\"")), Verdict::Owed(_)));
        assert!(matches!(judge(&t, &[], Some("He said \"nee, laat\"")), Verdict::Owed(_)));
        // One character.
        let one = [proposal(2), person(20, "n")];
        assert!(matches!(judge(&one, &[], Some("He said \"n\"")), Verdict::Settled(_)));
        assert!(matches!(judge(&one, &[], Some("He said n")), Verdict::Owed(_)), "no quote marks: no quote");
    }

    #[test]
    fn a_decimal_is_not_a_list_item_and_an_echo_is_no_run() {
        assert_eq!(numbered_items("1.2 GB were found\n2.5 GB more"), 0);
        assert_eq!(numbered_items("1. a\n2. b"), 2);
        assert!(!runs_inventory("echo serve.exe --db x inventory"));
        assert!(!runs_inventory("Write-Host 'serve.exe --db x inventory'"));
    }

    #[test]
    fn a_partial_refusal_needs_both_halves() {
        let t = [proposal(3), person(20, "yes, but 2 no, because it is private")];
        let answer = "\"yes, but 2 no, because it is private\"";
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

    fn tool(command: &str) -> String {
        serde_json::json!({"type":"assistant","timestamp":"2026-03-02T10:00:30Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":command}}]}}).to_string()
    }

    fn said(at: &str, text: &str) -> String {
        serde_json::json!({"type":"assistant","timestamp":at,"message":{"role":"assistant","content":[{"type":"text","text":text}]}}).to_string()
    }

    #[test]
    fn only_a_plain_inventory_call_counts() {
        assert!(runs_inventory("serve --db thor.db inventory"));
        assert!(runs_inventory("\"C:\\Users\\ann\\thor2\\serve.exe\" --db \"C:\\x\\thor.db\" inventory"));
        assert!(runs_inventory("& 'C:/thor2/serve.exe' --db x inventory --json"));
        assert!(runs_inventory("cd x && ~/.thor2/serve --db y inventory"));
        assert!(!runs_inventory("serve --db thor.db inventory --digest session.jsonl"));
        assert!(!runs_inventory("serve --db thor.db status"));
        assert!(!runs_inventory("echo inventory"));
        assert!(!runs_inventory("observe --db x inventory"));
    }

    #[test]
    fn the_proposal_is_read_from_what_happened_not_from_a_marker() {
        let list = "Proposal:\n1. CV\n2. Trip\nShall I?";
        // A numbered message BEFORE any inventory call is not a proposal.
        let before = [said("2026-03-02T10:00:00Z", list), tool("serve --db x inventory")].join("\n");
        assert!(!timeline_of(&before).iter().any(|e| matches!(e, Entry::Proposal { .. })));
        // An inventory --digest call alone does not count.
        let digest = [tool("serve --db x inventory --digest s.jsonl"), said("2026-03-02T10:01:00Z", list)].join("\n");
        assert!(!timeline_of(&digest).iter().any(|e| matches!(e, Entry::Proposal { .. })));
        // After the inventory call, a message with no numbered list is not it; the next one that has a list is.
        let real = [
            tool("serve --db x inventory"),
            said("2026-03-02T10:01:00Z", "Let me read some of the sessions first."),
            said("2026-03-02T10:02:00Z", list),
            said("2026-03-02T10:03:00Z", "1. a later numbered message"),
        ]
        .join("\n");
        let t = timeline_of(&real);
        assert_eq!(t.len(), 1, "{t:?}");
        assert!(matches!(t[0], Entry::Proposal { items: 2, at } if at == crate::eval_filing::unix_of_timestamp("2026-03-02T10:02:00Z").unwrap()));
        // A sidechain (helper) message never counts.
        let helper = format!("{}\n{}", tool("serve --db x inventory"), r#"{"type":"assistant","isSidechain":true,"timestamp":"2026-03-02T10:01:00Z","message":{"role":"assistant","content":[{"type":"text","text":"1. helper list"}]}}"#);
        assert!(timeline_of(&helper).is_empty());
    }

    #[test]
    fn timeline_reads_only_real_prompts() {
        let lines = [
            tool("serve --db x inventory"),
            said("2026-03-02T10:01:00Z", "Proposal\n1. CV\n2. Trip"),
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
