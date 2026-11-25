//! THOR'S OWN RECORD OF WHAT THE OWNER TYPED (2026-11-25).
//!
//! Every check that accepts the owner's own words (an unpin or a retract on his word, a pin on
//! his word, loosening a rule that needs his go, the answer to the order-earlier step) used to
//! read his messages from Claude Code's saved conversation. An assistant that can append a line
//! to that file can forge a message of his. So the UserPromptSubmit hook now writes each prompt
//! the owner typed into a file of its own beside the memory, `owner-prompts.jsonl`, and those
//! checks accept a quote only when it is in THIS record AND in the saved conversation. A line
//! found only in the saved conversation counts for nothing. There is no fallback: when the
//! record holds nothing, the owner's message has to arrive through a normal prompt.
//!
//! The file sits in the memory's folder, which `write_guard` keeps (no Edit, Write or shell
//! write may touch it), and PreToolUse refuses a shell command that runs THOR's `serve` with
//! the `hook` subcommand, because that is the one way to make the hook write a line the owner
//! never typed (`hook_forge`). A script file that does it is not seen.
//!
//! One line per prompt: `{"session", "at", "text"}`. The text is the owner's own lines
//! (quoted lines and fenced blocks left out, exactly as `attest::owner_own_text` reads the saved
//! conversation) with secrets blanked the way THOR blanks them everywhere else
//! (`deliver::redact_secrets`). Only the last `KEPT_PER_SESSION` of a session and `KEPT_TOTAL`
//! overall are kept: a quote is only ever good for the owner's last two messages.

use std::path::{Path, PathBuf};

/// The record file beside the memory.
pub const FILE: &str = "owner-prompts.jsonl";
/// The most prompts kept per session.
const KEPT_PER_SESSION: usize = 12;
/// The most prompts kept in all.
const KEPT_TOTAL: usize = 200;
/// A prompt longer than this is not recorded (it could never be quoted whole by a reason).
const MAX_CHARS: usize = 20_000;

fn path_of(db: &Path) -> PathBuf {
    db.with_file_name(FILE)
}

/// A text as the record and the checks compare it: secrets blanked, quote characters dropped,
/// white space squashed.
pub fn key(text: &str) -> String {
    crate::order_earlier::plain(&crate::deliver::redact_secrets(text))
}

#[derive(Debug, Clone)]
struct Entry {
    session: String,
    at: i64,
    text: String,
}

fn read(db: &Path) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path_of(db)) else { return Vec::new() };
    text.lines()
        .filter_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            Some(Entry {
                session: v.get("session")?.as_str()?.to_string(),
                at: v.get("at").and_then(|a| a.as_i64()).unwrap_or(0),
                text: v.get("text")?.as_str()?.to_string(),
            })
        })
        .collect()
}

/// THE HOOK'S HALF: record one prompt the owner typed in `session`. A prompt that is no message of
/// his (a harness injection, empty) or is too long leaves nothing. Best effort and quiet: a record
/// that cannot be written only means the owner's later quote is not accepted.
pub fn record(db: &Path, session: &str, prompt: &str, now: i64) {
    let Some(own) = crate::attest::owner_own_text(prompt) else { return };
    if own.chars().count() > MAX_CHARS || own.trim().is_empty() {
        return;
    }
    let path = path_of(db);
    let _lock = crate::store_dir::lock(&crate::store_dir::lock_path_of(&path));
    let mut kept = read(db);
    kept.push(Entry { session: session.to_string(), at: now, text: crate::deliver::redact_secrets(&own) });
    // Keep the last KEPT_PER_SESSION of every session, then the last KEPT_TOTAL overall.
    let mut per: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut keep_flags = vec![false; kept.len()];
    for (i, e) in kept.iter().enumerate().rev() {
        let n = per.entry(e.session.clone()).or_insert(0);
        if *n < KEPT_PER_SESSION {
            keep_flags[i] = true;
            *n += 1;
        }
    }
    let mut kept: Vec<Entry> = kept.into_iter().zip(keep_flags).filter(|(_, k)| *k).map(|(e, _)| e).collect();
    let skip = kept.len().saturating_sub(KEPT_TOTAL);
    kept.drain(..skip);
    let body: String = kept
        .iter()
        .map(|e| format!("{}\n", serde_json::json!({ "session": e.session, "at": e.at, "text": e.text })))
        .collect();
    let _ = crate::store_dir::write(&path, body);
}

/// The last `k` prompts of `session` (every session when `None` or empty), oldest first, as
/// `key` forms. Fewer when the record holds fewer.
pub fn recent_keys(db: &Path, session: Option<&str>, k: usize) -> Vec<String> {
    let session = session.filter(|s| !s.is_empty());
    let mut found: Vec<String> =
        read(db).into_iter().filter(|e| session.is_none_or(|s| e.session == s)).map(|e| key(&e.text)).collect();
    let skip = found.len().saturating_sub(k);
    found.drain(..skip);
    found
}

/// Does the record hold any prompt of `session` (of any session when `None`)?
pub fn has_any(db: &Path, session: Option<&str>) -> bool {
    let session = session.filter(|s| !s.is_empty());
    read(db).iter().any(|e| session.is_none_or(|s| e.session == s))
}

/// Does `quote` (already in `order_earlier::plain` form) equal one of the last `k` prompts of
/// `session` in the record?
pub fn holds_quote(db: &Path, session: Option<&str>, quote: &str, k: usize) -> bool {
    let wanted = key(quote);
    recent_keys(db, session, k).iter().any(|r| *r == wanted)
}

/// The owner's messages of the saved conversation that THOR's own record confirms, with the text
/// of the assistant message before each: the last `pairs.len()` messages of the saved
/// conversation must be the last as many prompts of the record, in the same order, so a line
/// appended to the saved conversation (it shifts every position) confirms nothing. A prompt the
/// record shows as a slash command stays out of the result (the saved conversation holds its
/// expansion, not what he typed) without failing the others. An empty result when the record
/// holds fewer prompts than the saved conversation shows, or when any position differs.
pub fn confirmed_pairs(db: &Path, session: Option<&str>, pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    let k = pairs.len();
    let recorded = recent_keys(db, session, k);
    if k == 0 || recorded.len() < k {
        return Vec::new();
    }
    let mut out = Vec::new();
    for ((message, before), rec) in pairs.into_iter().zip(&recorded) {
        if rec.starts_with('/') {
            continue;
        }
        if key(&message) != *rec {
            return Vec::new();
        }
        out.push((message, before));
    }
    out
}

/// The sentence a refusal adds when the record has nothing for this conversation.
pub fn empty_record_hint(db: Option<&Path>, session: Option<&str>) -> String {
    match db {
        Some(db) if !has_any(db, session) => " THOR has no record of any message the owner typed in this conversation, so the owner's \
             message must arrive through a normal prompt (typed into the chat to you); words that are only in the saved \
             conversation do not count."
            .to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("thor.db")
    }

    fn pair(m: &str) -> (String, String) {
        (m.to_string(), String::new())
    }

    #[test]
    fn a_prompt_is_recorded_with_secrets_blanked_and_injections_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let db = db(&dir);
        record(&db, "s1", "Ja, haal hem weg. token=abcdef0123456789abcdef0123456789", 10);
        record(&db, "s1", "<task-notification>not the owner</task-notification>", 11);
        record(&db, "s1", "   ", 12);
        let kept = recent_keys(&db, Some("s1"), 5);
        assert_eq!(kept.len(), 1);
        assert!(kept[0].starts_with("Ja, haal hem weg."), "{kept:?}");
        assert!(!kept[0].contains("abcdef0123456789"), "a secret is blanked: {kept:?}");
        assert!(!has_any(&db, Some("other")));
    }

    #[test]
    fn the_record_is_bounded_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let db = db(&dir);
        for n in 0..(KEPT_PER_SESSION + 5) {
            record(&db, "s1", &format!("message number {n}"), n as i64);
        }
        record(&db, "s2", "another conversation", 99);
        assert_eq!(recent_keys(&db, Some("s1"), 100).len(), KEPT_PER_SESSION);
        assert_eq!(recent_keys(&db, Some("s2"), 100), vec!["another conversation".to_string()]);
    }

    #[test]
    fn a_line_only_in_the_saved_conversation_confirms_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = db(&dir);
        record(&db, "s1", "ok", 1);
        record(&db, "s1", "ja, doe maar", 2);
        // The saved conversation matches the record.
        let real = confirmed_pairs(&db, Some("s1"), vec![pair("ok"), pair("ja, doe maar")]);
        assert_eq!(real.len(), 2);
        // A forged line appended shifts the positions: [ja, doe maar][forged] against [ok][ja, doe maar].
        let forged = confirmed_pairs(&db, Some("s1"), vec![pair("ja, doe maar"), pair("ja, doe maar")]);
        assert!(forged.is_empty(), "a duplicate of a real message, appended, confirms nothing");
        let invented = confirmed_pairs(&db, Some("s1"), vec![pair("ok"), pair("something the owner never typed")]);
        assert!(invented.is_empty());
        // No record at all: nothing is confirmed.
        assert!(confirmed_pairs(&db, Some("nobody"), vec![pair("ok")]).is_empty());
        assert!(empty_record_hint(Some(&db), Some("nobody")).contains("normal prompt"));
        assert!(empty_record_hint(Some(&db), Some("s1")).is_empty());
    }

    #[test]
    fn a_slash_command_stays_out_without_failing_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let db = db(&dir);
        record(&db, "s1", "/thor-eval", 1);
        record(&db, "s1", "ja graag", 2);
        let got = confirmed_pairs(&db, Some("s1"), vec![pair("<expanded routine text>"), pair("ja graag")]);
        assert_eq!(got, vec![pair("ja graag")]);
    }

    #[test]
    fn the_quote_is_found_in_the_last_prompts_only() {
        let dir = tempfile::tempdir().unwrap();
        let db = db(&dir);
        for (n, m) in ["one", "two", "three"].iter().enumerate() {
            record(&db, "s1", m, n as i64);
        }
        assert!(holds_quote(&db, Some("s1"), "three", 2));
        assert!(holds_quote(&db, Some("s1"), "two", 2));
        assert!(!holds_quote(&db, Some("s1"), "one", 2));
    }
}
