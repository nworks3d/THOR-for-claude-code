//! THE LENGTH REMINDER ON A SESSION'S FIRST PROMPT.
//!
//! FOUND IN REAL EVALUATIONS (reports of 2026-10-20, 10-28 and 10-29): the
//! reply guard's length rule holds a reply over the limit, and the assistant
//! then writes the whole answer a second time, so the owner sees it twice. A
//! Stop runs after the reply, so the guard cannot warn first. What can be done
//! is to say the limit to the assistant BEFORE it writes: one short line in the
//! context of the session's first prompt.
//!
//! Narrow on purpose. Only when the owner's own rulebook has the length rule
//! (`answer-is-too-long`, with a limit above zero), so an owner who removed or
//! changed it gets the limit he set, or nothing. Once per session, on the first
//! prompt the owner typed, and never for a helper agent, a copy or a run that
//! records nothing (the caller, `hook_once`, decides that). The limit is read
//! from the rulebook itself, never written into this file. A sidecar beside the
//! store remembers the sessions already told, best effort like every other: a
//! sidecar that cannot be read or written costs a repeated line, never a turn.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The sidecar beside the store.
pub const FILE: &str = "length-reminder-sessions.json";

/// The rulebook entry this reminder is about.
pub const RULE_ID: &str = "answer-is-too-long";

/// How many sessions the sidecar remembers (the most recent ones).
const SESSIONS_KEPT: usize = 50;

fn path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(FILE)
}

fn read(db: &Path) -> BTreeMap<String, i64> {
    std::fs::read_to_string(path(db)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn write(db: &Path, told: &BTreeMap<String, i64>) {
    if let Ok(text) = serde_json::to_string(told) {
        let _ = crate::store_dir::write(&path(db), text);
    }
}

/// The length limit, in characters, of the length rule in `rulebook_text`.
/// `None` when the rulebook has no such rule or it has no limit.
pub fn limit_of(rulebook_text: &str) -> Option<usize> {
    crate::respond::parse_opt_in_rules(rulebook_text)
        .into_iter()
        .find(|r| r.base.id == RULE_ID)
        .map(|r| r.base.min_chars)
        .filter(|limit| *limit > 0)
}

/// The line itself.
pub fn text(limit: usize) -> String {
    format!(
        "[THOR] Reminder: the reply guard holds an answer over {limit} characters and then you must write it again, so the \
         owner reads it twice. Keep each answer under {limit} characters unless he asked for a list, a summary or a report."
    )
}

/// The reminder for `session`'s first prompt: `Some` once per session while
/// the rulebook beside the store carries the length rule, `None` otherwise.
/// The session is remembered only when the line is returned, so a prompt
/// that has no room for it (the caller's choice) does not spend it.
pub fn for_session(db: &Path, session: &str, now: i64) -> Option<String> {
    let limit = limit_of(&std::fs::read_to_string(crate::respond::default_rulebook_path(db)).ok()?)?;
    let mut told = read(db);
    if told.contains_key(session) {
        return None;
    }
    told.insert(session.to_string(), now);
    while told.len() > SESSIONS_KEPT {
        let oldest = told.iter().min_by_key(|(_, at)| **at).map(|(k, _)| k.clone());
        match oldest {
            Some(k) => told.remove(&k),
            None => break,
        };
    }
    write(db, &told);
    Some(text(limit))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULEBOOK: &str = r#"[
      {"id":"other","any_of":["x"],"reminder":"r"},
      {"id":"answer-is-too-long","tier":"block","min_chars":1000,"reminder":"r"}
    ]"#;

    fn store_with(rulebook: Option<&str>) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        if let Some(rulebook) = rulebook {
            std::fs::write(crate::respond::default_rulebook_path(&db), rulebook).unwrap();
        }
        (dir, db)
    }

    #[test]
    fn the_limit_is_the_rulebooks_own() {
        assert_eq!(limit_of(RULEBOOK), Some(1000));
        assert_eq!(limit_of(&RULEBOOK.replace("1000", "1500")), Some(1500));
        assert_eq!(limit_of(&RULEBOOK.replace("\"min_chars\":1000,", "")), None, "a rule with no limit says nothing");
        assert_eq!(limit_of(&RULEBOOK.replace("answer-is-too-long", "renamed")), None);
        assert_eq!(limit_of("not json"), None);
    }

    #[test]
    fn a_session_is_told_once_with_the_limit_in_the_line() {
        let (_dir, db) = store_with(Some(RULEBOOK));
        let line = for_session(&db, "s1", 100).expect("the first prompt of a session is told");
        assert!(line.contains("1000 characters"), "{line}");
        assert!(line.len() < 300, "one short line: {} bytes", line.len());
        assert!(line.is_ascii());
        assert_eq!(for_session(&db, "s1", 200), None, "once per session");
        assert!(for_session(&db, "s2", 300).is_some(), "another session is told again");
    }

    #[test]
    fn nothing_is_said_without_the_rule_and_nothing_is_written() {
        let (_dir, db) = store_with(None);
        assert_eq!(for_session(&db, "s1", 1), None, "no rulebook");
        let (_dir2, db2) = store_with(Some(r#"[{"id":"other","any_of":["x"],"reminder":"r"}]"#));
        assert_eq!(for_session(&db2, "s1", 1), None, "no length rule");
        assert!(!path(&db2).exists() && !path(&db).exists(), "a session that was not told is not remembered");
    }

    #[test]
    fn the_sidecar_keeps_only_the_most_recent_sessions() {
        let (_dir, db) = store_with(Some(RULEBOOK));
        for n in 0..(SESSIONS_KEPT as i64 + 10) {
            assert!(for_session(&db, &format!("s{n}"), n).is_some());
        }
        let kept = read(&db);
        assert_eq!(kept.len(), SESSIONS_KEPT);
        assert!(!kept.contains_key("s0") && kept.contains_key(&format!("s{}", SESSIONS_KEPT as i64 + 9)));
    }
}
