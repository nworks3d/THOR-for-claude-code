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
//! double quotes, one WHOLE message the owner typed in this session AND the note's id
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
        " THOR could not confirm that the quoted words are one whole message the owner typed in this session about this \
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

/// How long before a fold the surviving note must have been created when the tool
/// server does not know when its session started.
const FOLD_SURVIVOR_MIN_AGE_SECS: i64 = 24 * 3_600;
/// How recently the survivor's text must have been rewritten for a retract to be its fold.
const FOLD_REWRITE_WITHIN_SECS: i64 = 3_600;

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
        let mut rewritten_just_now = false;
        for e in events.iter().filter(|e| matches!(e.kind, EventKind::FactCreated | EventKind::FactRevised)) {
            let Ok(version) = model::store::parse_body(&e.body) else { continue };
            let changed = before_text.as_ref().is_some_and(|t| *t != version.text);
            if e.kind == EventKind::FactRevised
                && changed
                && e.seq > own_last_write
                && crate::sweep::time_of_seq(store, e.seq).is_some_and(|at| now - at <= FOLD_REWRITE_WITHIN_SECS && now >= at)
            {
                rewritten_just_now = true;
            }
            before_text = Some(version.text);
        }
        if !rewritten_just_now {
            missing = Some(format!(
                "'{id}' was not rewritten in the last 60 minutes, after this note's last change, to carry this point"
            ));
            continue;
        }
        return Ok(id);
    }
    Err(missing)
}

/// Every message the owner typed in the transcript, each with the text of the
/// assistant's turn directly before it (empty when there was none).
pub fn owner_pairs(transcript: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(transcript) else { return Vec::new() };
    let mut out = Vec::new();
    let mut assistant = String::new();
    for raw in text.lines() {
        if let Some(person) = crate::attest::owner_text(raw) {
            out.push((person, std::mem::take(&mut assistant)));
            continue;
        }
        let Ok(line) = serde_json::from_str::<serde_json::Value>(raw) else { continue };
        let main_assistant = line.get("type").and_then(|t| t.as_str()) == Some("assistant")
            && line.get("isSidechain").and_then(|b| b.as_bool()) != Some(true);
        if main_assistant {
            for block in line.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()).into_iter().flatten() {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    assistant.push_str(block.get("text").and_then(|t| t.as_str()).unwrap_or(""));
                    assistant.push('\n');
                }
            }
        }
    }
    out
}

/// Does `because` quote, between double quotes, one WHOLE message among the
/// owner's (trimmed, white space squashed, quote characters ignored, case kept)
/// AND does the note's id appear in that message or in the assistant's turn
/// directly before it? Only the owner's own typed messages are passed in, never a
/// helper's, a hook's or the assistant's text. No word list: the id is the link
/// between his answer and this note.
pub fn owner_quote_matches(because: &str, id: &str, pairs: &[(String, String)]) -> bool {
    let quotes = crate::order_earlier::quote_candidates(because);
    pairs.iter().any(|(message, before)| {
        let about_this_note = message.contains(id) || before.contains(id);
        about_this_note && quotes.iter().any(|q| crate::order_earlier::plain(message) == *q)
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
    if !owner_quote_matches(because, id, &owner_pairs(Path::new(transcript))) {
        return;
    }
    let session = payload.get("session_id").and_then(|v| v.as_str()).unwrap_or_default();
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

/// The hook's refusal of any Edit, Write or shell call that names the record file
/// (the same style as the other files THOR protects: a plain sentence, nothing
/// written). `None` for every other call. Not airtight (a name can be built up in a
/// shell); transparency is the real protection - see the module's own comment.
pub fn protected_file_refusal(tool_name: &str, tool_input: Option<&serde_json::Value>) -> Option<String> {
    let input = tool_input?;
    let names_it = match tool_name {
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ["file_path", "notebook_path", "path"]
            .iter()
            .any(|k| input.get(k).and_then(|v| v.as_str()).is_some_and(|p| p.contains(PROTECTED_FILE))),
        "Bash" | "PowerShell" => input.get("command").and_then(|v| v.as_str()).is_some_and(|c| c.contains(PROTECTED_FILE)),
        _ => false,
    };
    names_it.then(|| {
        format!(
            "[THOR] {PROTECTED_FILE} is THOR's own record of what the owner said about taking a pinned note down; it is written \
             only by THOR's hook and is never edited by hand or by a command. Nothing was changed. To take a pinned note \
             down, use the unpin tool with a reason: it is allowed when the note is proven out of date and unused, or when \
             the owner's own whole message about that note is quoted in the reason."
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
