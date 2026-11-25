//! THE SETTING FILES THE OWNER EDITS BY HAND, WATCHED (2026-11-25).
//!
//! A few files beside the memory stay editable by hand on purpose (`write_guard::EDITED_BY_HAND`):
//! the mode switches (`thor-mode.json`, `stale-guard-mode.json`, `no-project-ask-mode.json`), the
//! work folders, and every `guard-*` file (the reply rules, the capture rules, the judge settings).
//! Because they are editable, an assistant could switch a guard off with one small edit and nobody
//! would see it. So each session start records a hash of every one of these files, and the NEXT
//! session start and `doctor` print one plain line per file that is different from what the last
//! session start saw: its name and what it now says. Nothing is blocked and nothing is undone; the
//! point is that a change can never be silent.
//!
//! The first session start on a store only records (nothing to compare with). A headless or
//! subagent session neither prints nor moves the record: only a session the owner reads does, so a
//! change cannot be "seen" by a session nobody reads. The record is `mode-files-seen.json` beside
//! the memory, a state file `write_guard` keeps.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The record of what the last session start saw.
pub const SEEN_FILE: &str = "mode-files-seen.json";
/// The most characters of a file's text a line quotes.
const EXCERPT_CHARS: usize = 240;
/// The most lines one report holds.
const MAX_LINES: usize = 6;

/// What one watched file looked like.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Seen {
    pub sha256: String,
    /// The start of its text, secrets blanked, so a removed file can still be named for what it said.
    pub excerpt: String,
}

fn seen_path(db: &Path) -> PathBuf {
    db.with_file_name(SEEN_FILE)
}

/// Is this file name one of the watched ones?
fn is_watched(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    crate::write_guard::EDITED_BY_HAND.iter().any(|prefix| lower.starts_with(prefix))
}

fn excerpt_of(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let squashed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let blanked = crate::deliver::redact_secrets(&squashed);
    if blanked.chars().count() > EXCERPT_CHARS {
        format!("{}...", blanked.chars().take(EXCERPT_CHARS).collect::<String>())
    } else {
        blanked
    }
}

/// Every watched file in the memory's folder as it is now.
pub fn snapshot(db: &Path) -> BTreeMap<String, Seen> {
    let mut out = BTreeMap::new();
    let Some(dir) = db.parent() else { return out };
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_watched(&name) || !entry.path().is_file() {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else { continue };
        let sha256 = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect::<String>();
        out.insert(name, Seen { sha256, excerpt: excerpt_of(&bytes) });
    }
    out
}

/// What the last session start saw, `None` when it never ran (or the record cannot be read).
fn read_seen(db: &Path) -> Option<BTreeMap<String, Seen>> {
    let text = std::fs::read_to_string(seen_path(db)).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// One plain line per watched file that differs from `before`: changed, new or removed.
fn lines_between(before: &BTreeMap<String, Seen>, now: &BTreeMap<String, Seen>) -> Vec<String> {
    let mut lines = Vec::new();
    for (name, seen) in now {
        let what = match before.get(name) {
            Some(old) if old.sha256 == seen.sha256 => continue,
            Some(_) => "changed",
            None => "appeared",
        };
        lines.push(format!(
            "[THOR] The setting file {name} {what} since the last session start. It now says: {}",
            if seen.excerpt.is_empty() { "(nothing, it is empty)" } else { seen.excerpt.as_str() }
        ));
    }
    for (name, old) in before {
        if !now.contains_key(name) {
            lines.push(format!("[THOR] The setting file {name} was removed since the last session start. It used to say: {}", old.excerpt));
        }
    }
    if lines.len() > MAX_LINES {
        let more = lines.len() - MAX_LINES;
        lines.truncate(MAX_LINES);
        lines.push(format!("[THOR] ... and {more} more setting file(s) changed since the last session start (doctor lists them)."));
    }
    if !lines.is_empty() {
        lines.push(
            "[THOR] If the owner did not ask for these changes himself, tell him in one plain sentence which file changed and what it now says, and do not change a setting file again without his word."
                .to_string(),
        );
    }
    lines
}

/// The lines for what changed since the last session start, without moving the record (doctor).
pub fn changed_lines(db: &Path) -> Vec<String> {
    match read_seen(db) {
        Some(before) => lines_between(&before, &snapshot(db)),
        None => Vec::new(),
    }
}

/// THE SESSION START: the lines for what changed since the last session start, and, when this
/// session is one the owner reads (`record`), the record moved to what the files say now.
pub fn session_start_lines(db: &Path, record: bool) -> Vec<String> {
    let now = snapshot(db);
    let lines = match read_seen(db) {
        Some(before) => lines_between(&before, &now),
        None => Vec::new(),
    };
    if record {
        if let Ok(text) = serde_json::to_string(&now) {
            let _ = crate::store_dir::write(&seen_path(db), text);
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        (dir, db)
    }

    #[test]
    fn the_first_start_only_records_and_a_silent_edit_is_named_at_the_next() {
        let (dir, db) = fixture();
        std::fs::write(dir.path().join("thor-mode.json"), r#"{"mode":"release"}"#).unwrap();
        std::fs::write(dir.path().join("guard-response-rulebook.json"), "[]").unwrap();
        std::fs::write(dir.path().join("notes.json"), "not watched").unwrap();
        assert!(session_start_lines(&db, true).is_empty(), "nothing to compare with yet");
        assert!(session_start_lines(&db, true).is_empty(), "nothing changed");
        assert!(changed_lines(&db).is_empty());

        // An assistant switches a guard off with one small edit.
        std::fs::write(dir.path().join("stale-guard-mode.json"), r#"{"mode": "off"}"#).unwrap();
        std::fs::write(dir.path().join("thor-mode.json"), r#"{"mode":"off"}"#).unwrap();
        std::fs::write(dir.path().join("notes.json"), "changed but not watched").unwrap();

        let doctor = changed_lines(&db);
        let text = doctor.join("\n");
        assert!(text.contains("thor-mode.json changed") && text.contains(r#"{"mode":"off"}"#), "{text}");
        assert!(text.contains("stale-guard-mode.json appeared") && text.contains(r#"{"mode": "off"}"#), "{text}");
        assert!(!text.contains("notes.json") && !text.contains("guard-response-rulebook"), "{text}");
        assert!(text.contains("did not ask for these changes"), "{text}");
        // Doctor does not move the record: the next session start still says it.
        assert_eq!(changed_lines(&db), doctor);
        assert_eq!(session_start_lines(&db, true), doctor);
        // After that start it is said once.
        assert!(session_start_lines(&db, true).is_empty());
        assert!(changed_lines(&db).is_empty());
    }

    #[test]
    fn a_session_nobody_reads_does_not_move_the_record() {
        let (dir, db) = fixture();
        std::fs::write(dir.path().join("work-folders.json"), "{}").unwrap();
        session_start_lines(&db, true);
        std::fs::write(dir.path().join("work-folders.json"), r#"{"tools": "x"}"#).unwrap();
        assert!(!session_start_lines(&db, false).is_empty(), "a headless start still sees it");
        assert!(!session_start_lines(&db, true).is_empty(), "and left the record where it was, so the owner's session says it");
    }

    #[test]
    fn a_removed_file_is_named_for_what_it_said_and_secrets_are_blanked() {
        let (dir, db) = fixture();
        let file = dir.path().join("guard-judge-config.json");
        std::fs::write(&file, r#"{"command": "judge", "token": "ghp_0123456789abcdefghijklmnopqrstuvwxyz"}"#).unwrap();
        session_start_lines(&db, true);
        std::fs::remove_file(&file).unwrap();
        let text = changed_lines(&db).join("\n");
        assert!(text.contains("guard-judge-config.json was removed") && text.contains("judge"), "{text}");
        assert!(!text.contains("ghp_0123456789"), "{text}");
    }
}
