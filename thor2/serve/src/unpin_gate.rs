//! The owner's rule for taking a pinned note down (2026-11-20): "unpinning MUST be
//! proven STALE, without use AND without a replacement".
//!
//! A pinned note is the owner's standing choice. Three tool calls can take one down
//! and all three pass this gate: the unpin tool, a revise that removes `Always`
//! (`always` false, or a binding list without it) and a retract of a pinned note.
//! Each is REFUSED, with nothing written, unless it carries a reason (at least
//! `BECAUSE_MIN_CHARS` characters) AND the note is not still in use:
//!
//! - (a) it carries a check that holds right now (its own proof says it is current),
//! - (b) it got a useful mark in the last `USEFUL_DAYS` days,
//! - (c) it was revised in the last `REVISED_DAYS` days (someone just maintained it).
//!
//! The owner's own decision goes through regardless: when the reason quotes, between
//! double quotes, one WHOLE message the owner typed in one of his last two messages AND the note's id
//! appears in that message or in the assistant message directly before it (so he was
//! answering about THIS note, not saying "ok" to something else).
//!
//! WHERE EACH HALF LIVES. The tool server enforces everything it can see on its own
//! (the reason, the three tests) and cannot see the transcript. The PreToolUse hook
//! sees the transcript and the tool call: it checks the quote against the owner's own
//! messages (`witness`) and leaves a short-lived record beside the memory, which is
//! the only thing that lets the tool server accept a quote (`owner_go_recorded`).
//!
//! TRANSPARENCY IS THE REAL PROTECTION. The record is a plain file an assistant could
//! try to write, so the hook refuses any Edit, Write or shell command that names it
//! (`protected_file_refusal`) and a record is only good for the same note, the same
//! reason text, the same session and ten minutes. None of that is airtight against a
//! determined writer, and none of it has to be: whatever gets through, EVERY note taken
//! down through a tool call is listed for the owner, whatever its reason, with how the
//! tool server let it through - recorded in the event's own `att.unpin` field, never
//! read out of the reason text (`model::store::tool_unpins`).

use model::item::{Binding, Item};
use model::store::{UnpinStamp, UNPIN_CHECK_FAILED, UNPIN_FOLDED, UNPIN_NO_USE, UNPIN_OWNER_WORDS};
use std::path::{Path, PathBuf};
use thor_core::event_store::{EventKind, EventStore};

/// The shortest reason a take-down takes, trimmed.
pub const BECAUSE_MIN_CHARS: usize = 20;
/// A useful mark younger than this keeps a pinned note in use.
pub const USEFUL_DAYS: i64 = 90;
/// A revision younger than this means someone just maintained the note.
pub const REVISED_DAYS: i64 = 30;
/// How long a witnessed owner quote stays usable by the tool server.
const GO_VALID_SECS: i64 = 600;
/// How many witnessed quotes are kept.
const GO_KEPT: usize = 20;
/// The record file beside the memory.
pub const PROTECTED_FILE: &str = "unpin-owner-go.json";

/// The three ways a pinned note can come down through a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Takedown {
    Unpin,
    /// A revise that removes the `Always` binding.
    Revise,
    Retract,
}

/// Is this tool name one that can take a pinned note down (the unpin, revise or retract tool)?
pub fn is_gated_call(tool_name: &str) -> bool {
    ["unpin", "revise", "retract"].iter().any(|t| tool_name == *t || tool_name.ends_with(&format!("__{t}")))
}

/// Is this tool name one that can ADD a pin (remember, revise or pin)? Its `because` may quote the
/// owner's own go for an overflow pin, which the hook witnesses the same way (`witness`).
pub fn is_pin_call(tool_name: &str) -> bool {
    ["remember", "revise", "pin", "resolve"].iter().any(|t| tool_name == *t || tool_name.ends_with(&format!("__{t}")))
}

/// Does this binding list carry `Always`?
pub fn is_pinned(bindings: &[Binding]) -> bool {
    bindings.iter().any(|b| matches!(b, Binding::Always))
}

/// What a call must carry before anything else is looked at: `Ok(reason)` or the
/// refusal to give.
pub fn require_because(because: Option<&str>, id: &str, how: Takedown) -> Result<String, String> {
    let given = because.map(str::trim).unwrap_or_default();
    if given.chars().count() < BECAUSE_MIN_CHARS {
        let (verb, field) = match how {
            Takedown::Unpin => ("Unpinning", "`because`"),
            Takedown::Revise => ("Removing the pin of", "`because`"),
            Takedown::Retract => ("Retracting the pinned note", "`reason` (at least 20 characters)"),
        };
        return Err(format!(
            "REFUSED: nothing was changed. {verb} '{id}' needs {field}: one sentence of at least {BECAUSE_MIN_CHARS} \
             characters saying why it is proven out of date, has no use any more and cannot be replaced by a corrected \
             text. A pinned note is the owner's standing choice. If the owner asked for this, put his whole message, \
             between double quotes, in the reason."
        ));
    }
    Ok(given.to_string())
}

/// The reasons the note is still in use, empty when it is proven out of date.
/// `root` is where a check's file is read from. `now` is unix seconds.
pub fn still_in_use(store: &EventStore, item: &Item, root: &Path, now: i64) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(check) = &item.check {
        if model::check::run(check, root) == model::check::Outcome::Holds {
            reasons.push("its own check holds right now, so its own proof says it is current".to_string());
        }
    }
    let useful_since = now - USEFUL_DAYS * 86_400;
    let marked_useful = store.get_events_by_entity(&item.id).unwrap_or_default().iter().any(|e| {
        e.kind == EventKind::ItemMarkedUseful && crate::usefulness::event_time(e).is_some_and(|at| at >= useful_since)
    });
    if marked_useful {
        reasons.push(format!("it got a useful mark in the last {USEFUL_DAYS} days, so it is still used"));
    }
    // A REAL revision only: the note's creation is no maintenance (a pinned note made by
    // mistake last week can be taken down).
    if crate::sweep::last_revised_at(store, &item.id).is_some_and(|at| at >= now - REVISED_DAYS * 86_400) {
        reasons.push(format!("it was revised in the last {REVISED_DAYS} days, so someone just maintained it"));
    }
    reasons
}

/// The plain refusal for a note that is still in use.
pub fn in_use_refusal(id: &str, reasons: &[String], quote_unwitnessed: bool, how: Takedown) -> String {
    let unwitnessed = if quote_unwitnessed {
        " THOR could not confirm that the quoted words are one whole message the owner typed in one of his last two messages about this \
         note (a piece of it, a message about something else, a helper's text and your own text do not count)."
    } else {
        ""
    };
    let instead = match how {
        Takedown::Retract => {
            "rewrite the text so it is right again (mcp__thor__revise), fold it (rewrite an OLD pinned note so it carries \
             the point, then retract this one naming that note's id as a whole word within the hour), or bring the owner's \
             own words"
        }
        _ => "revise the text so it is right again (mcp__thor__revise)",
    };
    format!(
        "REFUSED: nothing was changed. '{id}' is still in use: {}.{unwitnessed} This is the owner's rule for taking a \
         pinned note down. A pinned note is the owner's standing choice. Take one down only when it is proven out of date, has no use any more and cannot be replaced by a \
         corrected text; otherwise {instead}. If the owner asked for it, put his whole message, between double quotes, \
         in the reason.",
        reasons.join("; ")
    )
}

/// THE TOOL SERVER'S GATE for one take-down of the pinned note `item`: the reason
/// first, then proof that it is out of date and unused - or the owner's own words,
/// which only a hook record can vouch for. On success the reason and the stamp the
/// write carries (`Att::unpin`).
#[allow(clippy::too_many_arguments)]
pub fn judge(
    store: &EventStore,
    item: &Item,
    text: Option<&str>,
    root: &Path,
    session: Option<&str>,
    now: i64,
    how: Takedown,
) -> Result<(String, UnpinStamp), String> {
    let because = require_because(text, &item.id, how)?;
    // A FOLD IS NOT A LOSS: a retract whose reason names the id of a LIVE note that is itself
    // PINNED (the survivor that now carries the point) passes. Listed as "folded into <id>".
    let mut fold_missing: Option<String> = None;
    if how == Takedown::Retract {
        match fold_survivor(store, item, &because, now) {
            Ok(survivor) => {
                return Ok((because, UnpinStamp { kind: UNPIN_FOLDED.to_string(), witnessed: false, into: Some(survivor) }));
            }
            Err(missing) => fold_missing = missing,
        }
    }
    let reasons = still_in_use(store, item, root, now);
    let db = store.path();
    let witnessed = db.as_deref().is_some_and(|db| owner_go_recorded(db, &item.id, &because, session, now));
    if !reasons.is_empty() && !witnessed {
        let quoted = !crate::order_earlier::quote_candidates(&because).is_empty();
        let mut refusal = in_use_refusal(&item.id, &reasons, quoted, how);
        refusal.push_str(&crate::prompt_record::empty_record_hint(db.as_deref(), session));
        if let Some(missing) = fold_missing {
            refusal.push_str(&format!(
                " It is not a fold either: {missing}. To fold, rewrite the surviving pinned note so it carries this point, \
                 then retract this one naming its id."
            ));
        }
        return Err(refusal);
    }
    let kind = if witnessed {
        UNPIN_OWNER_WORDS
    } else if item.check.is_some() {
        UNPIN_CHECK_FAILED
    } else {
        UNPIN_NO_USE
    };
    Ok((because, UnpinStamp { kind: kind.to_string(), witnessed, into: None }))
}

/// THE OWNER'S GO FOR A RULE THAT NEEDS HIS GO (`model::store::is_owner_approval`).
/// A rule only the owner's own message can answer is not the assistant's to
/// loosen either: a revise that loosens it (`model::gate::owner_rule_loosened`,
/// `what` says how) or a retract of it passes only when `because` (`field`
/// says which argument carries it) quotes, between double quotes, one WHOLE
/// message the owner typed (one of his last two), with the rule's id in that message
/// or in the assistant message right before it - the very check an unpin on the
/// owner's words uses (the hook witnesses the quote, `owner_go_recorded` reads
/// it). Narrowing or strengthening the rule needs none of this. THOR's own
/// writers (the installer, the sweep) write through the store, never through
/// this gate.
pub fn owner_rule_go(
    store: &EventStore,
    id: &str,
    because: Option<&str>,
    session: Option<&str>,
    now: i64,
    what: &[String],
    field: &str,
) -> Result<(), String> {
    let given = because.map(str::trim).unwrap_or_default();
    let witnessed = store.path().as_deref().is_some_and(|db| owner_go_recorded(db, id, given, session, now));
    if witnessed {
        return Ok(());
    }
    let quoted = !crate::order_earlier::quote_candidates(given).is_empty();
    let unwitnessed = if quoted {
        " The words you quoted are not one whole message of the last two the owner typed (this turn's or the one before) that names this rule (a piece of it, a message about something else, a helper's text and your own text do not count)."
    } else {
        ""
    };
    let unwitnessed = format!("{unwitnessed}{}", crate::prompt_record::empty_record_hint(store.path().as_deref(), session));
    Err(format!(
        "REFUSED: nothing was changed. '{id}' is a rule that needs the owner's explicit go, and this call loosens it: {}.{unwitnessed} Only the owner can loosen such a rule, and THOR accepts it only on his own typed words. Ask the owner whether he wants this (name the rule's id '{id}' in your question). When he has said so in one of his last two messages, repeat the call with `{field}` holding his whole message between double quotes. To take the whole rule down instead, one retract with his whole message between double quotes in its reason is enough.",
        what.join("; ")
    ))
}

/// How long before a fold the surviving note must have been created when the tool
/// server does not know when its session started.
const FOLD_SURVIVOR_MIN_AGE_SECS: i64 = 24 * 3_600;
/// How recently the survivor's text must have been rewritten for a retract to be its fold.
const FOLD_REWRITE_WITHIN_SECS: i64 = 3_600;

/// How many characters longer the survivor's text must have become for its rewrite to be a fold.
const FOLD_MIN_ADDED_CHARS: usize = 20;

/// Does `reason` name `id` as a whole token (not as part of a longer word or id)?
fn names_as_a_token(reason: &str, id: &str) -> bool {
    let id_char = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    reason.match_indices(id).any(|(at, _)| {
        let before = reason[..at].chars().next_back();
        let after = reason[at + id.len()..].chars().next();
        !before.is_some_and(id_char) && !after.is_some_and(id_char)
    })
}

/// WHEN A RETRACT IS A FOLD, NOT A LOSS. A pinned note that says the same as another may
/// go when the other now carries its point; but "a fold" must not be a way round the
/// owner's rule (pin a fresh empty note, retract the real one "into" it, retract the fresh
/// one). So it counts only when ALL of these hold:
/// 1. the survivor's id stands in the reason as a whole token;
/// 2. the survivor is live and pinned (and is another note);
/// 3. the survivor is OLD: created more than 24 hours ago (the tool server does not know
///    when its session started);
/// 4. the survivor's TEXT was changed by a revise AFTER this note's last change and within
///    the last 60 minutes - the fold itself, the survivor rewritten to carry the point.
///
/// Rule 4 makes the survivor "revised in the last 30 days", so it is protected afterwards.
/// Returns the survivor's id, or what is missing for the nearest candidate named in the reason.
fn fold_survivor(store: &EventStore, item: &Item, reason: &str, now: i64) -> Result<String, Option<String>> {
    let candidates: Vec<String> = crate::live::live_items(store)
        .into_iter()
        .filter(|li| li.id != item.id && is_pinned(&li.item.bindings) && names_as_a_token(reason, &li.id))
        .map(|li| li.id)
        .collect();
    if candidates.is_empty() {
        return Err(None);
    }
    let own_last_write = store
        .get_events_by_entity(&item.id)
        .unwrap_or_default()
        .iter()
        .filter(|e| matches!(e.kind, EventKind::FactCreated | EventKind::FactRevised))
        .map(|e| e.seq)
        .max()
        .unwrap_or(0);
    let mut missing = None;
    for id in candidates {
        let events = store.get_events_by_entity(&id).unwrap_or_default();
        let created = events.iter().find(|e| e.kind == EventKind::FactCreated).and_then(|e| crate::sweep::time_of_seq(store, e.seq));
        if !created.is_some_and(|at| now - at >= FOLD_SURVIVOR_MIN_AGE_SECS) {
            missing = Some(format!("'{id}' is not an old pinned note (it was created in the last 24 hours, or its age cannot be told)"));
            continue;
        }
        let mut before_text: Option<String> = None;
        // The length the survivor had just before the rewrite that counts as the fold.
        let mut fold_grew_from: Option<usize> = None;
        let mut current_text = String::new();
        for e in events.iter().filter(|e| matches!(e.kind, EventKind::FactCreated | EventKind::FactRevised)) {
            let Ok(version) = model::store::parse_body(&e.body) else { continue };
            // A real fold ADDS the point: the text grows by a real margin, and the revise said
            // why with the retracted note's id (a trivial edit of an old pin is no fold).
            let grew = before_text
                .as_ref()
                .is_some_and(|t| version.text.chars().count() >= t.chars().count() + FOLD_MIN_ADDED_CHARS);
            // ... and what it ADDS says a real share of the retracted note's own points, not any filler.
            let carries = carries_its_points(&item.text, before_text.as_deref().unwrap_or(""), &version.text);
            let named = serde_json::from_str::<serde_json::Value>(&e.body)
                .ok()
                .and_then(|v| v.get("because").and_then(|b| b.as_str()).map(|b| names_as_a_token(b, &item.id)))
                .unwrap_or(false);
            if e.kind == EventKind::FactRevised
                && grew
                && carries
                && named
                && e.seq > own_last_write
                && crate::sweep::time_of_seq(store, e.seq).is_some_and(|at| now - at <= FOLD_REWRITE_WITHIN_SECS && now >= at)
            {
                fold_grew_from = before_text.as_ref().map(|t| t.chars().count());
            }
            current_text = version.text.clone();
            before_text = Some(version.text);
        }
        // The survivor's CURRENT text must still be the fold: a rewrite that was reverted (or cut
        // back down) afterwards is no fold any more, whatever it once was.
        let rewritten_just_now = fold_grew_from.is_some_and(|from| {
            current_text.chars().count() >= from + FOLD_MIN_ADDED_CHARS && carries_its_points(&item.text, "", &current_text)
        });
        if !rewritten_just_now {
            missing = Some(format!(
                "'{id}' was not rewritten in the last 60 minutes, after this note's last change, to carry this point (the rewrite must add at least {FOLD_MIN_ADDED_CHARS} characters of text that say what '{}' says, and its `because` must name '{}')",
                item.id, item.id
            ));
            continue;
        }
        return Ok(id);
    }
    Err(missing)
}

/// The distinct words of a text that are long enough to mean something, in lower case.
fn meaningful_words(text: &str) -> std::collections::BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 5)
        .map(str::to_lowercase)
        .collect()
}

/// Whether a survivor's new text carries the retracted note's points: a real share of the
/// retracted note's distinct meaningful words stand in it, at least two thirds of them and at least
/// three (all of them when it has fewer). A handful of words picked out of a long note is a word
/// salad, no fold. And at least half of that share must be ADDED by the rewrite (`before` is the
/// survivor's text before it; empty when only the survivor's current text is judged), so a survivor
/// that already used the words and got a filler sentence is no fold either.
/// An old pinned note that is about something else, with a filler sentence added, has none.
fn carries_its_points(retracted: &str, before: &str, new: &str) -> bool {
    let wanted = meaningful_words(retracted);
    if wanted.is_empty() {
        return true;
    }
    let after = meaningful_words(new);
    let old = meaningful_words(before);
    let carried = wanted.iter().filter(|w| after.contains(*w)).count();
    let added = wanted.iter().filter(|w| after.contains(*w) && !old.contains(*w)).count();
    let need = (wanted.len() * 2).div_ceil(3).max(wanted.len().min(3));
    carried >= need && (before.is_empty() || added >= need.div_ceil(2))
}

/// How many of the owner's last typed messages a quote may come from: this turn's and the
/// one before, the same window a go-word has.
pub const OWNER_WINDOW: usize = 2;

/// The owner's LAST `OWNER_WINDOW` typed messages in the transcript, each with the text of the
/// assistant message directly before it (empty when there was none). Older messages are not
/// returned: a "yes" from earlier in the session unlocks nothing now. The transcript is read
/// backwards until those messages are found, however long the turns between them were.
pub fn owner_pairs(transcript: &Path) -> Vec<(String, String)> {
    let lines = crate::attest::lines_with_owner_messages(transcript, OWNER_WINDOW + 1);
    let mut out = Vec::new();
    // The assistant message directly before the next owner message: the text lines of the
    // latest assistant message (lines that share a message id are one message).
    let mut assistant = String::new();
    let mut assistant_id: Option<String> = None;
    for raw in &lines {
        if let Some(person) = crate::attest::owner_text(raw) {
            out.push((person, std::mem::take(&mut assistant)));
            assistant_id = None;
            continue;
        }
        let Ok(line) = serde_json::from_str::<serde_json::Value>(raw) else { continue };
        let main_assistant = line.get("type").and_then(|t| t.as_str()) == Some("assistant")
            && line.get("isSidechain").and_then(|b| b.as_bool()) != Some(true);
        if !main_assistant {
            continue;
        }
        let message = line.get("message");
        let text: String = message
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
            .into_iter()
            .flatten()
            .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"))
            .map(|block| format!("{}\n", block.get("text").and_then(|t| t.as_str()).unwrap_or("")))
            .collect();
        if text.is_empty() {
            continue;
        }
        let id = message.and_then(|m| m.get("id")).and_then(|i| i.as_str()).map(str::to_string);
        if id.is_none() || id != assistant_id {
            assistant.clear();
        }
        assistant.push_str(&text);
        assistant_id = id;
    }
    let skip = out.len().saturating_sub(OWNER_WINDOW);
    out.split_off(skip)
}

/// Does `because` quote, between double quotes, one WHOLE message among the
/// owner's (trimmed, white space squashed, quote characters ignored, case kept)
/// AND does the note's id appear in that message or in the assistant's turn
/// directly before it? Only the owner's own typed messages are passed in, never a
/// helper's, a hook's or the assistant's text. No word list: the id is the link
/// between his answer and this note.
pub fn owner_quote_matches(because: &str, id: &str, pairs: &[(String, String)]) -> bool {
    owner_quote_matches_among(because, id, pairs, &[])
}

/// `owner_quote_matches` that also knows the ids of the live notes (`live_ids`), so a question that
/// names a second real note does not count as a question about this one.
pub fn owner_quote_matches_among(because: &str, id: &str, pairs: &[(String, String)], live_ids: &[String]) -> bool {
    let quotes = crate::order_earlier::quote_candidates(because);
    pairs.iter().any(|(message, before)| {
        // An answer that does not name the note counts as a go only when it is no refusal.
        let about_this_note = names_as_a_token(message, id) || (is_a_go(message) && asks_only_about(before, id, live_ids));
        about_this_note && quotes.iter().any(|q| crate::order_earlier::plain(message) == *q)
    })
}

/// Words that make an answer something other than a plain yes, in nl, en, de, fr and es: a no, a
/// "not", a "never", or "first / wait" (he wants something else done before). Kept simple on
/// purpose: when in doubt the answer is no go, and the owner says the note's id in his own words.
const NOT_A_YES: [&str; 26] = [
    "no", "nee", "neen", "nein", "non", "nope", "nah", "niet", "not", "don't", "dont", "nicht", "kein", "nunca", "never", "nooit",
    "first", "eerst", "zuerst", "wait", "wacht", "warte", "stop", "hold", "attends", "espera",
];

/// Is this owner answer a go (no refusal, no "not", no "first")?
fn is_a_go(answer: &str) -> bool {
    let lower = hidden_chars_out(answer).to_lowercase();
    !lower
        .split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '\u{2019}'))
        .map(|w| w.replace('\u{2019}', "'"))
        .any(|w| NOT_A_YES.contains(&w.as_str()))
}

/// A text with the characters that are not seen removed and in lower case, so an id written in
/// capitals or with a zero-width space inside it is still that id.
fn hidden_chars_out(text: &str) -> String {
    text.chars().filter(|c| !matches!(c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}' | '\u{00AD}')).collect()
}

fn normalised(text: &str) -> String {
    hidden_chars_out(text).to_lowercase()
}

/// Stems of the verbs that take a note down or loosen it, in nl, en, de, fr and es.
const TAKE_DOWN_STEMS: [&str; 34] = [
    "unpin", "retract", "remov", "delet", "drop", "loosen", "take down", "taking down", "lift", "detach", "relax", "loskoppel",
    "losmaken", "weghalen", "haal", "verwijder", "intrek", "schrap", "entfern", "l\u{f6}sch", "losch", "zur\u{fc}ckzieh", "aufheb",
    "retir", "supprim", "d\u{e9}tach", "annul", "d\u{e9}s\u{e9}pingl", "quitar", "elimin", "desanclar", "borr", "ontkoppel", "weg",
];

/// Is `before` (the assistant message directly before the owner's answer) a QUESTION that names
/// exactly this one note? Only then does an owner's answer that does not name the note itself ("ok")
/// count as an answer about it. The id must stand in it as a whole token, in a sentence that ends in
/// a question mark, and the id of no other LIVE note may stand anywhere in the message (an ordinary
/// word with a hyphen in it is not an id, so it blocks nothing). A message that merely mentions the
/// id, or lists several notes, is no question to answer yes to.
fn asks_only_about(before: &str, id: &str, live_ids: &[String]) -> bool {
    let (before, id) = (normalised(before), normalised(id));
    let id_char = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    let others = before.split(|c: char| !id_char(c)).any(|w| w != id && live_ids.iter().any(|l| normalised(l) == w));
    if others || !names_as_a_token(&before, &id) {
        return false;
    }
    // The id must be the SUBJECT of the question: the sentence that names it also says what is to be
    // done to it (unpin, retract, remove, loosen). A passing mention ("Shall I run the build (<id>)?")
    // is not a question about the note. A short question that names nothing ("Mag dat?") counts when
    // the sentence right before it, in the same message, is the one that names the note and the verb.
    let about = |sentence: &str| names_as_a_token(sentence, &id) && TAKE_DOWN_STEMS.iter().any(|v| sentence.contains(v));
    let mut sentences: Vec<(String, char)> = Vec::new();
    let mut current = String::new();
    for c in before.chars() {
        if matches!(c, '.' | '!' | '?' | '\n') {
            sentences.push((std::mem::take(&mut current), c));
        } else {
            current.push(c);
        }
    }
    (0..sentences.len()).any(|i| {
        let (question, end) = &sentences[i];
        if *end != '?' {
            return false;
        }
        about(question)
            || (question.split_whitespace().count() <= 8 && !names_as_a_token(question, &id) && i > 0 && about(&sentences[i - 1].0))
    })
}

fn go_path(db: &Path) -> PathBuf {
    db.with_file_name(PROTECTED_FILE)
}

fn go_key(text: &str) -> String {
    crate::order_earlier::plain(text)
}

/// The reason text a gated call carries: `because` (unpin, revise) or `reason` (retract).
fn reason_of(input: &serde_json::Value) -> Option<&str> {
    input.get("because").and_then(|v| v.as_str()).or_else(|| input.get("reason").and_then(|v| v.as_str()))
}

/// THE HOOK'S HALF: the call about to run takes a note down and its reason quotes a
/// whole message of the owner's that is about this note. Leaves a record the tool
/// server reads; nothing else about the call changes (the hook never blocks one
/// itself). The record names the session, the note and the reason text.
pub fn witness(db: &Path, payload: &serde_json::Value, now: i64) {
    let Some(input) = payload.get("tool_input") else { return };
    let (Some(id), Some(because)) = (input.get("id").and_then(|v| v.as_str()), reason_of(input)) else { return };
    let Some(transcript) = payload.get("transcript_path").and_then(|v| v.as_str()) else { return };
    let session = payload.get("session_id").and_then(|v| v.as_str()).unwrap_or_default();
    // Only what THOR's own record of the owner's prompts confirms counts (`prompt_record`).
    let pairs = crate::prompt_record::confirmed_pairs(db, Some(session), owner_pairs(Path::new(transcript)));
    // The ids of the live notes: a question that names a second one is no question about this one.
    let live_ids: Vec<String> = thor_core::event_store::EventStore::open_existing(db)
        .map(|store| crate::live::live_items(&store).into_iter().map(|l| l.id).collect())
        .unwrap_or_default();
    if !owner_quote_matches_among(because, id, &pairs, &live_ids) {
        return;
    }
    let path = go_path(db);
    let mut kept: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<serde_json::Value>>(&t).ok())
        .unwrap_or_default();
    kept.retain(|e| e.get("at").and_then(|a| a.as_i64()).is_some_and(|at| now - at < GO_VALID_SECS));
    kept.push(serde_json::json!({ "id": id, "because": go_key(because), "session": session, "at": now }));
    let skip = kept.len().saturating_sub(GO_KEPT);
    let kept: Vec<_> = kept.into_iter().skip(skip).collect();
    let _ = std::fs::write(&path, serde_json::to_string(&kept).unwrap_or_default());
}

/// THE TOOL SERVER'S HALF: did the hook witness this very quote for this very note
/// in this very session a moment ago? A server that does not know its session
/// (none in a plain test) checks the note, the text and the time only.
pub fn owner_go_recorded(db: &Path, id: &str, because: &str, session: Option<&str>, now: i64) -> bool {
    // The quoted message must also stand in THOR's own record of what the owner typed.
    let in_record = crate::order_earlier::quote_candidates(because)
        .iter()
        .any(|q| crate::prompt_record::holds_quote(db, session, q, OWNER_WINDOW));
    if !in_record {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(go_path(db)) else { return false };
    let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else { return false };
    let key = go_key(because);
    entries.iter().any(|e| {
        e.get("id").and_then(|v| v.as_str()) == Some(id)
            && e.get("because").and_then(|v| v.as_str()) == Some(key.as_str())
            && session.is_none_or(|s| e.get("session").and_then(|v| v.as_str()) == Some(s))
            && e.get("at").and_then(|a| a.as_i64()).is_some_and(|at| now - at < GO_VALID_SECS && now >= at)
    })
}

/// The hook's refusal of any Edit, Write or shell call that WRITES to the record file: a redirect,
/// Set-Content, Add-Content, Out-File, Tee-Object, a copy or move onto it (or out of it), a delete,
/// or a one-line program that opens it for writing. The name is recognised in its brace forms, with
/// backslash escapes and with quotes split inside it, in any folder. A call that only NAMES the file
/// (a script that edits this source file and has the name in a comment, a read, a commit message)
/// is not refused. `None` for every other call. Not airtight (a name can be built up in a shell
/// variable, and a script file is not read); transparency is the real protection - see the module's
/// own comment.
pub fn protected_file_refusal(tool_name: &str, tool_input: Option<&serde_json::Value>) -> Option<String> {
    let guard = crate::write_guard::Protected::with_name_lock(&[PROTECTED_FILE, crate::prompt_record::FILE]);
    let writes_it = crate::write_guard::writes_protected(&guard, tool_name, tool_input, None);
    writes_it.then(|| {
        format!(
            "[THOR] {PROTECTED_FILE} and {} are THOR's own records of what the owner typed and said about taking a pinned \
             note down; they are written only by THOR's hook and are never edited by hand or by a command. Nothing was \
             changed. To take a pinned note \
             down, use the unpin tool with a reason: it is allowed when the note is proven out of date and unused, or when \
             the owner's own whole message about that note is quoted in the reason.",
            crate::prompt_record::FILE
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs() -> Vec<(String, String)> {
        vec![
            ("Ja, haal pinned-n1 maar weg, hij is nergens meer voor nodig.".to_string(), String::new()),
            ("ok".to_string(), "Zal ik de notitie pinned-n2 loskoppelen?".to_string()),
            ("ok".to_string(), "Zal ik de tests nu draaien?".to_string()),
        ]
    }

    #[test]
    fn the_protected_file_is_protected_whatever_its_case() {
        for name in ["unpin-owner-go.json", "UNPIN-OWNER-GO.JSON", "Unpin-Owner-Go.Json"] {
            for tool in ["Write", "Edit", "MultiEdit", "NotebookEdit"] {
                let input = serde_json::json!({ "file_path": format!("C:/data/store/{name}") });
                assert!(protected_file_refusal(tool, Some(&input)).is_some(), "{tool} on {name}");
            }
            let command = serde_json::json!({ "command": format!("echo [] > store/{name}") });
            assert!(protected_file_refusal("Bash", Some(&command)).is_some(), "Bash on {name}");
        }
        let other = serde_json::json!({ "file_path": "C:/data/store/notes.json" });
        assert!(protected_file_refusal("Write", Some(&other)).is_none());
    }

    #[test]
    fn a_whole_message_about_this_note_counts_and_a_piece_or_another_topic_does_not() {
        let whole = "the owner said \"Ja, haal pinned-n1 maar  weg, hij is nergens meer voor nodig.\"";
        assert!(owner_quote_matches(whole, "pinned-n1", &pairs()));
        assert!(!owner_quote_matches(whole, "pinned-other", &pairs()), "his message names another note");
        assert!(!owner_quote_matches("the owner said \"haal pinned-n1 maar weg\"", "pinned-n1", &pairs()), "a piece is not the message");
        assert!(!owner_quote_matches("no quotes at all, only a long enough sentence", "pinned-n1", &pairs()));
        // "ok" counts only after a question about THIS note.
        assert!(owner_quote_matches("he said \"ok\"", "pinned-n2", &pairs()));
        assert!(!owner_quote_matches("he said \"ok\"", "pinned-n3", &pairs()), "an ok about another note unlocks nothing");
    }

    fn user_line(text: &str) -> String {
        serde_json::json!({"type": "user", "message": {"role": "user", "content": text}}).to_string()
    }

    fn assistant_line(id: &str, text: &str) -> String {
        serde_json::json!({"type": "assistant", "message": {"id": id, "content": [{"type": "text", "text": text}]}}).to_string()
    }

    #[test]
    fn only_the_last_two_owner_messages_count_and_the_id_must_be_directly_before() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("t.jsonl");
        let lines = [
            assistant_line("m0", "Shall I unpin note-old?"),
            user_line("Ja, doe maar, haal die oude notitie weg."),
            assistant_line("m1", "Done. Anything else?"),
            user_line("nee"),
            assistant_line("m2", "Working on it."),
            user_line("verder"),
            assistant_line("m3", "Shall I unpin note-new?"),
            assistant_line("m4", "Also, a long unrelated text that names note-sneaky."),
            user_line("ja graag, doe dat"),
        ];
        std::fs::write(&transcript, lines.join("\n") + "\n").unwrap();
        let pairs = owner_pairs(&transcript);
        assert_eq!(pairs.len(), 2, "only the last two owner messages");
        let old = "he said \"Ja, doe maar, haal die oude notitie weg.\"";
        assert!(!owner_quote_matches(old, "note-old", &pairs), "an older yes is out of the window");
        let recent = "he said \"ja graag, doe dat\"";
        assert!(!owner_quote_matches(recent, "note-sneaky", &pairs), "a message that only mentions the id is no question about it");
        assert!(!owner_quote_matches(recent, "note-new", &pairs), "an older assistant message is not directly before");
    }

    #[test]
    fn an_id_matches_as_a_whole_token_never_inside_a_longer_one() {
        let pairs = vec![
            ("Ja, haal a-12 maar weg, dat mag.".to_string(), String::new()),
            ("ok".to_string(), "Zal ik a-1 loskoppelen?".to_string()),
        ];
        let long = "he said \"Ja, haal a-12 maar weg, dat mag.\"";
        assert!(owner_quote_matches(long, "a-12", &pairs));
        assert!(!owner_quote_matches(long, "a-1", &pairs), "a-1 is not inside a-12");
        assert!(!owner_quote_matches(long, "a", &pairs));
        assert!(owner_quote_matches("he said \"ok\"", "a-1", &pairs));
        assert!(!owner_quote_matches("he said \"ok\"", "a-12", &pairs));
    }

    #[test]
    fn an_ok_counts_only_after_a_question_that_names_exactly_that_one_note() {
        let live: Vec<String> = ["note-x", "note-y", "note-z", "note-xy"].iter().map(|s| s.to_string()).collect();
        let ok = |before: &str| owner_quote_matches_among("he said \"ok\"", "note-x", &[("ok".to_string(), before.to_string())], &live);
        assert!(ok("Zal ik note-x loskoppelen?"));
        assert!(ok("Done with the tests.\nShall I unpin `note-x`?"));
        assert!(!ok("I will unpin note-x now."), "a statement is no question");
        assert!(ok("Ik haal note-x weg. Mag dat?"), "the sentence before a short question names the note and what is done to it");
        assert!(ok("Unpinning note-x now. Is that all right?"));
        assert!(!ok("I ran the build for note-x. Is that all right?"), "the sentence before names it but no take-down");
        assert!(!ok("Shall I run the build (note-x)?"), "a mention in passing is no question about the note");
        assert!(!ok("Unpinning note-x now. Shall I also rebuild the whole index, run the tests and push the result to the remote?"), "a long question about something else");
        assert!(!ok("Shall I unpin NOTE-Y and NOTE-X?"), "a second real id in capitals counts");
        assert!(!ok("Shall I unpin note-x? Also note\u{200b}-y."), "a second real id with a zero-width space counts");
        assert!(!ok("Shall I unpin note\u{200b}-y or note-x?"), "a second real id with a zero-width space in the question counts");
        assert!(ok("Shall I unpin NOTE-X?"), "capitals do not hide the note itself");
        assert!(ok("Soll ich note-x entfernen?") && ok("Dois-je retirer note-x ?") && ok("Debo quitar note-x?"), "de, fr, es");
        assert!(!ok("Shall I unpin note-x, note-y and note-z?"), "a second real note blocks it");
        assert!(!ok("Shall I unpin note-x? note-y says the same."), "a second real note anywhere in the message blocks it");
        assert!(ok("Shall I unpin note-x? It sits in the start-block."), "an ordinary hyphenated word is no id");
        assert!(ok("Shall I unpin note-x or fix the pre-commit hook?"), "an ordinary hyphenated word is no id");
        assert!(!ok("Shall I unpin note-xy?"), "a longer id is another note");
        assert!(!ok(""));
    }

    #[test]
    fn an_answer_that_is_a_refusal_or_a_first_is_no_go_after_a_question() {
        let live: Vec<String> = vec!["note-x".to_string()];
        let answer = |said: &str| {
            owner_quote_matches_among(&format!("he said \"{said}\""), "note-x", &[(said.to_string(), "Shall I unpin note-x?".to_string())], &live)
        };
        for yes in ["ok", "ja", "ja, doe maar", "yes please", "oui", "si, adelante", "go ahead", "Ja gerne"] {
            assert!(answer(yes), "{yes}");
        }
        for no in [
            "nee, niet doen",
            "no",
            "No, leave it",
            "nein",
            "non merci",
            "ok, but first something else",
            "ok, eerst iets anders",
            "ja maar niet nu, wacht",
            "don't",
            "do not do that, not now",
            "ok, wait",
            "n\u{200b}o",
        ] {
            assert!(!answer(no), "{no}");
        }
        // Naming the note himself is judged by the quote rule, as before.
        assert!(owner_quote_matches_among("he said \"nee, laat note-x staan\"", "note-x", &[("nee, laat note-x staan".to_string(), String::new())], &live));
    }

    #[test]
    fn the_hook_reads_the_live_note_ids_from_the_store_for_the_one_question_rule() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            for id in ["note-x", "note-y"] {
                let item = Item {
                    id: id.to_string(),
                    kind: model::item::Kind::Rule,
                    text: format!("Fixture note {id} about something plain."),
                    bindings: vec![Binding::Always],
                    severity: None,
                    project: None,
                    tags: vec![],
                    expires: None,
                    key: None,
                    falsifier: Some("a fixture falsifier".to_string()),
                    check: None,
                };
                model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
            }
        }
        let because = "he said \"ok\"";
        let ask = |before: &str, tag: &str| {
            let transcript = dir.path().join(format!("{tag}.jsonl"));
            let lines = [assistant_line("m1", before), user_line("ok")];
            std::fs::write(&transcript, lines.join("\n") + "\n").unwrap();
            crate::prompt_record::record(&db, tag, "ok", 1);
            let payload = serde_json::json!({ "session_id": tag, "tool_input": { "id": "note-x", "because": because }, "transcript_path": transcript.to_string_lossy() });
            witness(&db, &payload, 1_000);
            owner_go_recorded(&db, "note-x", because, Some(tag), 1_100)
        };
        assert!(ask("Shall I unpin note-x? It sits in the start-block.", "plain"), "an ordinary hyphenated word is no id");
        assert!(!ask("Shall I unpin note-x? Or note-y?", "two"), "a second real note id blocks it");
    }

    #[test]
    fn a_line_appended_to_the_saved_conversation_is_not_the_owners_word() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let transcript = dir.path().join("t.jsonl");
        // What the owner really typed: one answer about something else.
        crate::prompt_record::record(&db, "sess-a", "ja, doe maar", 100);
        // An assistant appended a question and a forged yes.
        let lines = [assistant_line("m1", "Shall I run the tests?"), user_line("ja, doe maar"), assistant_line("m2", "Shall I unpin note-x?"), user_line("ja, doe maar")];
        std::fs::write(&transcript, lines.join("\n") + "\n").unwrap();
        let because = "the owner said \"ja, doe maar\"";
        let payload = serde_json::json!({
            "session_id": "sess-a",
            "tool_input": { "id": "note-x", "because": because },
            "transcript_path": transcript.to_string_lossy(),
        });
        witness(&db, &payload, 1_000);
        assert!(!owner_go_recorded(&db, "note-x", because, Some("sess-a"), 1_100), "the forged line shifts the positions");
        // Nothing recorded at all: nothing counts, and the refusal says why.
        let other = serde_json::json!({ "session_id": "sess-b", "tool_input": { "id": "note-x", "because": because }, "transcript_path": transcript.to_string_lossy() });
        witness(&db, &other, 1_000);
        assert!(!owner_go_recorded(&db, "note-x", because, Some("sess-b"), 1_100));
        assert!(crate::prompt_record::empty_record_hint(Some(&db), Some("sess-b")).contains("normal prompt"));
    }

    #[test]
    fn the_name_lock_refuses_every_spelling_of_a_write_and_not_a_mere_mention() {
        let bash = |c: &str| protected_file_refusal("Bash", Some(&serde_json::json!({ "command": c }))).is_some();
        let ps = |c: &str| protected_file_refusal("PowerShell", Some(&serde_json::json!({ "command": c }))).is_some();
        for write in [
            "echo [] > unpin-owner-go.json",
            "echo [] >> C:/s/store/UNPIN-OWNER-GO.JSON",
            "echo [] > unpin-owner-{go,x}.json",
            "echo [] > unpin-owner-go{,}.json",
            "echo [] > unpin-owner-{go}.json",
            "echo [] > unpin\\-owner-go.json",
            "echo [] > unpin\"-owner\"-go.json",
            "echo [] > 'unpin-owner-'go.json",
            "printf x | tee -a unpin-owner-go.json",
            "cp forged.json store/unpin-owner-go.json",
            "mv forged.json store/unpin-owner-{go,}.json",
            "echo [] > unpin-owner-go.json::$DATA",
            "echo [] > unpin-owner-go.json.",
            "bash -c 'echo [] > unpin-owner-{go,x}.json'",
            "python -c \"open('unpin-owner-go.json','w').write('[]')\"",
            "node -e \"require('fs').writeFileSync('unpin-owner-go.json','[]')\"",
            "perl -pi -e 's/a/b/' unpin-owner-go.json",
            "rm unpin-owner-go.json",
        ] {
            assert!(bash(write), "{write}");
        }
        for write in [
            "Set-Content -Path unpin-owner-go.json -Value '[]'",
            "Set-Content unpin-owner-{go,x}.json '[]'",
            "Add-Content -Path .\\store\\unpin-owner-go.json -Value x",
            "'[]' | Out-File -FilePath unpin-owner-go.json",
            "'[]' | Out-File 'unpin-owner-'go.json",
            "Copy-Item forged.json -Destination .\\store\\unpin-owner-go.json",
            "Move-Item forged.json .\\unpin-owner-go.json",
            "[IO.File]::WriteAllText('unpin-owner-go.json','[]')",
            "(New-Object IO.StreamWriter('unpin-owner-go.json')).Write('x')",
        ] {
            assert!(ps(write), "{write}");
        }
        // Naming the file is not writing it: the real replay was a script that edits this very source
        // file and carries the name in a comment.
        for mention in [
            "python - <<'EOF'\n# the record file is unpin-owner-go.json\nopen('unpin_gate.rs','w').write('x')\nEOF",
            "git commit -m \"keep unpin-owner-go.json out of reach\"",
            "cat unpin-owner-go.json",
            "grep -rn unpin-owner-go.json serve/src",
            "echo unpin-owner-go.json",
            "cp unpin-owner-go.json /tmp/copy.json",
            "ls store/unpin-owner-go.json",
        ] {
            assert!(!bash(mention), "{mention}");
        }
        for tool in ["Write", "Edit", "MultiEdit"] {
            for path in ["store/unpin-owner-go.json", "C:\\s\\Unpin-Owner-Go.json", "store/unpin-owner-go.json:stream", "store/owner-prompts.jsonl"] {
                let input = serde_json::json!({ "file_path": path });
                assert!(protected_file_refusal(tool, Some(&input)).is_some(), "{tool} {path}");
            }
        }
        let src = serde_json::json!({ "file_path": "thor2/serve/src/unpin_gate.rs", "new_string": "const F: &str = \"unpin-owner-go.json\";" });
        assert!(protected_file_refusal("Edit", Some(&src)).is_none(), "editing the source that names it is fine");
    }

    #[test]
    fn a_go_before_a_huge_turn_is_still_found() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("t.jsonl");
        let big = "x".repeat(1024 * 1024);
        let mut lines = vec![assistant_line("m1", "Shall I unpin note-far?"), user_line("ja, doe maar, haal hem weg")];
        for n in 0..20 {
            lines.push(serde_json::json!({"type": "assistant", "message": {"id": format!("t{n}"), "content": [{"type": "tool_use", "name": "Bash", "input": {"command": big}}]}}).to_string());
        }
        std::fs::write(&transcript, lines.join("\n") + "\n").unwrap();
        assert!(std::fs::metadata(&transcript).unwrap().len() > crate::attest::TRANSCRIPT_TAIL_BYTES);
        let pairs = owner_pairs(&transcript);
        assert!(owner_quote_matches("he said \"ja, doe maar, haal hem weg\"", "note-far", &pairs));
    }

    #[test]
    fn a_missing_or_short_reason_is_refused() {
        assert!(require_because(None, "x", Takedown::Unpin).unwrap_err().contains("needs `because`"));
        assert!(require_because(Some("too short"), "x", Takedown::Revise).is_err());
        assert!(require_because(Some("too short"), "x", Takedown::Retract).unwrap_err().contains("`reason`"));
        assert!(require_because(Some("   twenty characters or more here   "), "x", Takedown::Unpin).is_ok());
    }

    #[test]
    fn a_witness_is_good_for_one_note_one_reason_one_session_and_ten_minutes() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let transcript = dir.path().join("t.jsonl");
        let line = serde_json::json!({"type": "user", "message": {"role": "user", "content": "Ja, haal n1 maar weg, hij is nergens meer voor nodig."}});
        std::fs::write(&transcript, format!("{line}\n")).unwrap();
        let because = "the owner asked: \"Ja, haal n1 maar weg, hij is nergens meer voor nodig.\"";
        let payload = serde_json::json!({
            "session_id": "sess-a",
            "tool_input": { "id": "n1", "because": because },
            "transcript_path": transcript.to_string_lossy(),
        });
        // The hook recorded the prompt when the owner typed it; the saved conversation alone is not enough.
        witness(&db, &payload, 1_000);
        assert!(!owner_go_recorded(&db, "n1", because, Some("sess-a"), 1_100), "not in THOR's own record");
        crate::prompt_record::record(&db, "sess-a", "Ja, haal n1 maar weg, hij is nergens meer voor nodig.", 999);
        witness(&db, &payload, 1_000);
        assert!(owner_go_recorded(&db, "n1", because, Some("sess-a"), 1_100));
        assert!(!owner_go_recorded(&db, "n1", because, Some("sess-b"), 1_100), "another session");
        assert!(!owner_go_recorded(&db, "n2", because, Some("sess-a"), 1_100), "another note");
        assert!(!owner_go_recorded(&db, "n1", "the owner asked: \"something else entirely, quoted here\"", Some("sess-a"), 1_100));
        assert!(!owner_go_recorded(&db, "n1", because, Some("sess-a"), 1_000 + GO_VALID_SECS + 1), "too late");
        // A quote that is not the owner's message leaves no record; neither does a message about another note.
        let forged = serde_json::json!({ "session_id": "sess-a", "tool_input": { "id": "n3", "because": "he said \"I typed this, honestly\"" }, "transcript_path": transcript.to_string_lossy() });
        witness(&db, &forged, 1_000);
        assert!(!owner_go_recorded(&db, "n3", "he said \"I typed this, honestly\"", Some("sess-a"), 1_100));
        // A retract carries its text in `reason`.
        let retract = serde_json::json!({ "session_id": "sess-a", "tool_input": { "id": "n1", "reason": because }, "transcript_path": transcript.to_string_lossy() });
        witness(&db, &retract, 2_000);
        assert!(owner_go_recorded(&db, "n1", because, Some("sess-a"), 2_100));
    }

    #[test]
    fn the_hook_refuses_a_call_that_names_the_record_file() {
        let edit = serde_json::json!({ "file_path": format!("C:/memory/{PROTECTED_FILE}"), "content": "[]" });
        assert!(protected_file_refusal("Write", Some(&edit)).unwrap().contains("Nothing was changed"));
        let shell = serde_json::json!({ "command": format!("echo '[]' > /m/{PROTECTED_FILE}") });
        assert!(protected_file_refusal("Bash", Some(&shell)).is_some() && protected_file_refusal("PowerShell", Some(&shell)).is_some());
        // Another file, and a mention in what is written, are not refused.
        let other = serde_json::json!({ "file_path": "C:/work/notes.md", "content": format!("see {PROTECTED_FILE}") });
        assert!(protected_file_refusal("Write", Some(&other)).is_none());
        assert!(protected_file_refusal("Read", Some(&edit)).is_none());
    }

    #[test]
    fn the_gated_tools_are_recognised_by_their_names() {
        for name in ["mcp__thor__unpin", "mcp__thor__revise", "mcp__thor__retract", "unpin"] {
            assert!(is_gated_call(name), "{name}");
        }
        assert!(!is_gated_call("mcp__thor__pin") && !is_gated_call("mcp__thor__lookup") && !is_gated_call("Edit"));
    }
}
