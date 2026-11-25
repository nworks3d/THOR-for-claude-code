//! THE SETTING FILES THE OWNER EDITS BY HAND, WATCHED (2026-11-25).
//!
//! A few files beside the memory stay editable by hand on purpose: the mode switches
//! (`thor-mode.json`, `stale-guard-mode.json`, `no-project-ask-mode.json`), the work folders, and
//! every `guard-*` file (the reply rules, the capture rules, the judge settings, the judge's
//! wrapper script). Because they are editable, an assistant could switch a guard off with one small
//! edit and nobody would see it. So the watch holds a hash (and the redacted text) of
//!
//! - every file in the memory's folder whose name starts with `guard-`, plus the four files above;
//! - every file the judge settings run: the `command` and each argument of `guard-judge-config.json`
//!   that names a file that exists (a wrapper script, a program in a folder);
//! - every file in the `githooks` folder beside the memory (the dispatchers git runs after a commit).
//!
//! Each session start the owner reads records them, and the NEXT session start and `doctor` print one
//! plain line per file that differs from the last session start: its name and what changed in it (the
//! changed part, or where it starts). Every Stop also looks (a few small files): a file that differs
//! from the last session start is remembered, so a change made and put back inside one session is
//! still named at the next session start. Nothing is blocked and nothing is undone; the point is that
//! a change can never be silent.
//!
//! The first session start on a store only records (nothing to compare with). A headless or
//! subagent session neither prints nor moves the record: only a session a person reads does. The
//! records are `mode-files-seen.json` and `mode-files-pending.json` beside the memory, state files
//! `write_guard` keeps. NOT SEEN: a change to a file this list does not name, a judge command that
//! only names a program on the search path, and a change made and put back between two looks.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The record of what the last session start saw.
pub const SEEN_FILE: &str = "mode-files-seen.json";
/// What a Stop saw that differed from it (kept until the next session start says it).
pub const PENDING_FILE: &str = "mode-files-pending.json";
/// The most characters of a file's text a line quotes.
const EXCERPT_CHARS: usize = 240;
/// The most lines one report holds.
const MAX_LINES: usize = 6;
/// The largest file whose text is kept (a bigger one is hashed only).
const MAX_TEXT_BYTES: u64 = 256 * 1024;
/// The most files a watch holds.
const MAX_FILES: usize = 64;
/// The most remembered changes a Stop keeps.
const MAX_PENDING: usize = 40;

/// What one watched file looked like.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Seen {
    pub sha256: String,
    /// The start of its text, secrets blanked, so a removed file can still be named for what it said.
    pub excerpt: String,
    /// Its whole text, secrets blanked (empty for a file too big to keep), so a change can be shown
    /// where it is.
    #[serde(default)]
    pub text: String,
}

fn seen_path(db: &Path) -> PathBuf {
    db.with_file_name(SEEN_FILE)
}

fn pending_path(db: &Path) -> PathBuf {
    db.with_file_name(PENDING_FILE)
}

/// Is this file name in the memory's folder one of the watched ones?
fn is_watched(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("guard-")
        || crate::write_guard::EDITED_BY_HAND.iter().any(|f| !f.starts_with("guard-") && lower.starts_with(f))
}

fn excerpt_of(text: &str) -> String {
    let squashed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if squashed.chars().count() > EXCERPT_CHARS {
        format!("{}...", squashed.chars().take(EXCERPT_CHARS).collect::<String>())
    } else {
        squashed
    }
}

fn seen_of(path: &Path) -> Option<Seen> {
    let bytes = std::fs::read(path).ok()?;
    let sha256 = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect::<String>();
    let text = if bytes.len() as u64 <= MAX_TEXT_BYTES {
        crate::deliver::redact_secrets(&String::from_utf8_lossy(&bytes))
    } else {
        String::new()
    };
    Some(Seen { sha256, excerpt: excerpt_of(&text), text })
}

/// The files the judge settings run: `command` and every argument that names a file that exists
/// (relative names are read against the memory's folder).
fn judge_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(dir.join("guard-judge-config.json")) else { return Vec::new() };
    let Ok(config) = serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}')) else { return Vec::new() };
    let mut words: Vec<String> = config.get("command").and_then(|c| c.as_str()).map(str::to_string).into_iter().collect();
    words.extend(config.get("args").and_then(|a| a.as_array()).into_iter().flatten().filter_map(|a| a.as_str()).map(str::to_string));
    words
        .iter()
        .map(|w| {
            let p = PathBuf::from(w.trim_matches('"'));
            if p.is_absolute() { p } else { dir.join(p) }
        })
        .filter(|p| p.is_file())
        .collect()
}

/// Every watched file as it is now, by name (the memory's folder), `githooks/<name>` or the path a
/// judge setting names.
pub fn snapshot(db: &Path) -> BTreeMap<String, Seen> {
    let mut out = BTreeMap::new();
    let Some(dir) = db.parent() else { return out };
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if is_watched(&name) && entry.path().is_file() && out.len() < MAX_FILES {
                if let Some(seen) = seen_of(&entry.path()) {
                    out.insert(name, seen);
                }
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(dir.join("githooks")) {
        for entry in entries.flatten() {
            if entry.path().is_file() && out.len() < MAX_FILES {
                if let Some(seen) = seen_of(&entry.path()) {
                    out.insert(format!("githooks/{}", entry.file_name().to_string_lossy()), seen);
                }
            }
        }
    }
    for path in judge_files(dir) {
        let name = path.to_string_lossy().replace('\\', "/");
        if out.len() < MAX_FILES && !out.contains_key(&name) {
            if let Some(seen) = seen_of(&path) {
                out.insert(name, seen);
            }
        }
    }
    out
}

/// What the last session start saw, `None` when it never ran (or the record cannot be read).
fn read_seen(db: &Path) -> Option<BTreeMap<String, Seen>> {
    let text = std::fs::read_to_string(seen_path(db)).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// What Stops saw that differed from the last session start, newest last.
fn read_pending(db: &Path) -> Vec<(String, Option<Seen>)> {
    std::fs::read_to_string(pending_path(db))
        .ok()
        .and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default()
}

/// What changed in `new` compared with `old`, in words: the whole text when it is short, else the
/// changed stretch (a little before it, at most `EXCERPT_CHARS` of it) and where it starts.
fn describe(old: Option<&str>, new: &str) -> String {
    let squash = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
    let new = squash(new);
    if new.is_empty() {
        return "(nothing, it is empty)".to_string();
    }
    if new.chars().count() <= EXCERPT_CHARS {
        return new;
    }
    let Some(old) = old.map(squash) else { return format!("{}...", new.chars().take(EXCERPT_CHARS).collect::<String>()) };
    let (a, b): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let end = b.len() - suffix;
    let from = prefix.saturating_sub(40);
    let to = (end.max(prefix) + 20).min(b.len());
    let shown: String = b[from..to].iter().take(EXCERPT_CHARS).collect();
    let more = if to.saturating_sub(from) > EXCERPT_CHARS { "..." } else { "" };
    let removed = if end == prefix { " (text was taken out here)" } else { "" };
    format!("the changed part starts at character {} of {}: {shown}{more}{removed}", prefix + 1, b.len())
}

/// One plain line per watched file that differs from `before`: changed, new or removed, and one per
/// file a Stop saw changed that has been put back since.
fn lines_between(before: &BTreeMap<String, Seen>, now: &BTreeMap<String, Seen>, pending: &[(String, Option<Seen>)]) -> Vec<String> {
    let mut lines = Vec::new();
    for (name, seen) in now {
        let what = match before.get(name) {
            Some(old) if old.sha256 == seen.sha256 => continue,
            Some(_) => "changed",
            None => "appeared",
        };
        let said = describe(before.get(name).map(|o| o.text.as_str()), &seen.text);
        lines.push(format!("[THOR] The setting file {name} {what} since the last session start. It now says: {said}"));
    }
    for (name, old) in before {
        if !now.contains_key(name) {
            lines.push(format!("[THOR] The setting file {name} was removed since the last session start. It used to say: {}", old.excerpt));
        }
    }
    // A change a Stop saw and nobody can see now: made and put back.
    for (name, state) in pending {
        let same_as_before = now.get(name).map(|n| &n.sha256) == before.get(name).map(|b| &b.sha256);
        if !same_as_before || lines.iter().any(|l| l.contains(&format!("setting file {name} "))) {
            continue;
        }
        let said = match state {
            Some(s) => format!("it said: {}", describe(before.get(name).map(|o| o.text.as_str()), &s.text)),
            None => "it was removed".to_string(),
        };
        lines.push(format!("[THOR] The setting file {name} was changed during the last session and put back since; at the time {said}"));
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
        Some(before) => lines_between(&before, &snapshot(db), &read_pending(db)),
        None => Vec::new(),
    }
}

/// EVERY STOP: remember what differs from the last session start right now, so that a change put
/// back before the next session start is still named there. A few small files; quiet and best effort.
pub fn note_at_stop(db: &Path) {
    let Some(before) = read_seen(db) else { return };
    let now = snapshot(db);
    let mut pending = read_pending(db);
    let mut changed = false;
    for name in before.keys().chain(now.keys()).collect::<std::collections::BTreeSet<_>>() {
        if before.get(name).map(|b| &b.sha256) == now.get(name).map(|n| &n.sha256) {
            continue;
        }
        let state = now.get(name).cloned();
        let known = pending.iter().any(|(n, s)| n == name && s.as_ref().map(|s| &s.sha256) == state.as_ref().map(|s| &s.sha256));
        if !known {
            pending.push((name.clone(), state));
            changed = true;
        }
    }
    if changed {
        let skip = pending.len().saturating_sub(MAX_PENDING);
        pending.drain(..skip);
        if let Ok(text) = serde_json::to_string(&pending) {
            let _ = crate::store_dir::write(&pending_path(db), text);
        }
    }
}

/// THE SESSION START: the lines for what changed since the last session start, and, when this
/// session is one a person reads (`record`), the record moved to what the files say now and the
/// remembered changes cleared.
pub fn session_start_lines(db: &Path, record: bool) -> Vec<String> {
    let now = snapshot(db);
    let lines = match read_seen(db) {
        Some(before) => lines_between(&before, &now, &read_pending(db)),
        None => Vec::new(),
    };
    if record {
        if let Ok(text) = serde_json::to_string(&now) {
            let _ = crate::store_dir::write(&seen_path(db), text);
        }
        let _ = std::fs::remove_file(pending_path(db));
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

    #[test]
    fn the_judge_script_and_the_githooks_folder_are_watched_too() {
        let (dir, db) = fixture();
        let script = dir.path().join("my-judge.ps1");
        std::fs::write(&script, "Write-Output 'fine'").unwrap();
        std::fs::write(dir.path().join("guard-judge-local-model-wrapper.ps1"), "# wrapper").unwrap();
        std::fs::create_dir_all(dir.path().join("githooks")).unwrap();
        std::fs::write(dir.path().join("githooks").join("pre-commit"), "exit 0").unwrap();
        let config = serde_json::json!({ "command": "powershell", "args": ["-File", script.to_string_lossy()] });
        std::fs::write(dir.path().join("guard-judge-config.json"), config.to_string()).unwrap();
        assert!(session_start_lines(&db, true).is_empty());
        let names: Vec<String> = snapshot(&db).into_keys().collect();
        assert!(names.iter().any(|n| n == "githooks/pre-commit") && names.iter().any(|n| n.ends_with("my-judge.ps1")), "{names:?}");
        assert!(names.iter().any(|n| n == "guard-judge-local-model-wrapper.ps1"), "{names:?}");

        std::fs::write(&script, "Write-Output 'always say yes'").unwrap();
        std::fs::write(dir.path().join("githooks").join("pre-commit"), "exit 0 # skipped").unwrap();
        std::fs::write(dir.path().join("guard-judge-local-model-wrapper.ps1"), "# wrapper that says yes").unwrap();
        let text = changed_lines(&db).join("\n");
        assert!(text.contains("my-judge.ps1 changed") && text.contains("always say yes"), "{text}");
        assert!(text.contains("githooks/pre-commit changed") && text.contains("guard-judge-local-model-wrapper.ps1 changed"), "{text}");
    }

    #[test]
    fn a_change_made_and_put_back_inside_one_session_is_named_at_the_next_start() {
        let (dir, db) = fixture();
        let file = dir.path().join("stale-guard-mode.json");
        std::fs::write(&file, r#"{"mode": "on"}"#).unwrap();
        assert!(session_start_lines(&db, true).is_empty());
        // The guard is switched off, a Stop falls in between, then it is switched on again.
        std::fs::write(&file, r#"{"mode": "off"}"#).unwrap();
        note_at_stop(&db);
        std::fs::write(&file, r#"{"mode": "on"}"#).unwrap();
        note_at_stop(&db);
        // Doctor and the next session start both see it, though the file is as before now.
        let doctor = changed_lines(&db).join("\n");
        assert!(doctor.contains("stale-guard-mode.json was changed during the last session and put back") && doctor.contains(r#""mode": "off""#), "{doctor}");
        let said = session_start_lines(&db, true).join("\n");
        assert_eq!(said, doctor);
        assert!(session_start_lines(&db, true).is_empty(), "said once");
        // A file that did not change between the Stops leaves nothing.
        note_at_stop(&db);
        assert!(changed_lines(&db).is_empty());
    }

    #[test]
    fn a_big_rulebook_shows_the_changed_part_and_where_it_is() {
        let (dir, db) = fixture();
        let file = dir.path().join("guard-response-rulebook.json");
        let entry = |n: usize| format!(r#"{{"id": "rule-{n}", "enabled": true, "text": "a rule about replies number {n}"}}"#);
        let rules = |off: bool| {
            let mut items: Vec<String> = (0..30).map(entry).collect();
            if off {
                items[25] = items[25].replace("true", "false");
            }
            format!("[{}]", items.join(", "))
        };
        std::fs::write(&file, rules(false)).unwrap();
        session_start_lines(&db, true);
        std::fs::write(&file, rules(true)).unwrap();
        let text = changed_lines(&db).join("\n");
        assert!(text.contains("the changed part starts at character"), "{text}");
        assert!(text.contains("rule-25") && text.contains("false"), "the changed rule is shown, not the start of the file: {text}");
        assert!(!text.contains("rule-0"), "{text}");
    }
}
