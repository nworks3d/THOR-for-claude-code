//! STOP WARNINGS REACH THE AGENT NEXT TURN (step 26).
//!
//! Since 2.5.3 a Stop notice that does not hold the turn - the Response
//! Guard's warn tier, "running in session X", "report still owed" - goes out
//! as `systemMessage` only: a line for the owner. It had to: at `Stop`, Claude
//! Code reads `additionalContext` as feedback that continues the conversation,
//! and a notice sent that way woke the model nine times in a row (field case
//! 2026-10-01). The price was that the agent never learned what the notice
//! said.
//!
//! So each such notice is also queued here, once, and handed to the same
//! session as context at its next `UserPromptSubmit` - the start of a turn the
//! owner began, where context informs and wakes nothing. Bounded: at most
//! `MAX_LINES` notices wait per session (the newest), each folded to one line
//! of at most `LINE_MAX_CHARS` characters, and a notice already waiting is not
//! queued twice. A sidecar beside the store, best effort like every other: a
//! sidecar that cannot be read or written loses a notice, never a turn.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The sidecar beside the store.
pub const FILE: &str = "next-turn-notes.json";

/// How many notices wait for one session at most.
pub const MAX_LINES: usize = 3;

/// The longest one notice is kept, in characters.
pub const LINE_MAX_CHARS: usize = 600;

/// How many sessions the sidecar keeps notices for (the most recent ones).
const SESSIONS_KEPT: usize = 50;

fn path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(FILE)
}

fn read(db: &Path) -> BTreeMap<String, Vec<String>> {
    std::fs::read_to_string(path(db)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn write(db: &Path, all: &BTreeMap<String, Vec<String>>) {
    if let Ok(text) = serde_json::to_string(all) {
        let _ = crate::store_dir::write(&path(db), text);
    }
}

/// `text` on one line, at most `LINE_MAX_CHARS` characters.
fn one_line(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= LINE_MAX_CHARS {
        return line;
    }
    format!("{}...", line.chars().take(LINE_MAX_CHARS).collect::<String>())
}

/// Queue a Stop notice for `session`'s next turn: once, newest last, at most
/// `MAX_LINES` waiting.
pub fn queue(db: &Path, session: &str, text: &str) {
    let line = one_line(text);
    if line.is_empty() {
        return;
    }
    let mut all = read(db);
    let waiting = all.entry(session.to_string()).or_default();
    if waiting.contains(&line) {
        return;
    }
    waiting.push(line);
    let skip = waiting.len().saturating_sub(MAX_LINES);
    waiting.drain(..skip);
    if all.len() > SESSIONS_KEPT {
        let first = all.keys().next().cloned();
        if let Some(first) = first.filter(|k| k != session) {
            all.remove(&first);
        }
    }
    write(db, &all);
}

/// Every notice waiting for `session`, oldest first, taken off the queue.
pub fn take(db: &Path, session: &str) -> Vec<String> {
    let mut all = read(db);
    let Some(waiting) = all.remove(session) else { return Vec::new() };
    write(db, &all);
    waiting
}

/// The context block the next turn opens with.
pub fn render(lines: &[String]) -> String {
    let mut out = "[THOR] At the end of your last turn THOR told the owner this, without holding the turn - it is for you to \
                   know, not a new task:"
        .to_string();
    for line in lines {
        out.push_str(&format!("\n- {line}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_are_queued_once_bounded_and_taken_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        queue(&db, "s1", "first\nnotice");
        queue(&db, "s1", "first notice");
        queue(&db, "s1", "second");
        queue(&db, "s1", "third");
        queue(&db, "s1", "fourth");
        queue(&db, "s2", "other session");
        assert_eq!(take(&db, "s1"), vec!["second", "third", "fourth"]);
        assert!(take(&db, "s1").is_empty(), "taken once");
        assert_eq!(take(&db, "s2"), vec!["other session"]);
        queue(&db, "s3", &"x".repeat(LINE_MAX_CHARS + 50));
        assert_eq!(take(&db, "s3")[0].chars().count(), LINE_MAX_CHARS + 3);
    }
}
