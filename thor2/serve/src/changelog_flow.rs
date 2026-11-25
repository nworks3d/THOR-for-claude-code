//! The changelog-and-version-numbers flow, asked once per code project.
//!
//! WHAT IT IS. A project that has its own memory (a scope) is asked ONE
//! question, with its two reasons: shall the assistant keep a changelog and
//! version numbers there? On yes, a few starter notes (`ops::install::
//! changelog_flow_notes` and the commit note) already know what has to happen
//! at a commit, a tag and a release. On no, those notes are not shown in that
//! project and the question never comes back there. It is never a hard stop:
//! nothing is refused, whatever the answer.
//!
//! THE ANSWER lives in one small file beside the store, `changelog-flow.json`:
//! `{"<project key>": "on" | "off"}`, the key being `checkout::project_match_key`
//! of the project name (case, spaces and hyphens do not open a second answer).
//! Written atomically through `store_dir`, under its lock. A project with no
//! entry has not been asked yet.
//!
//! THE FILTER. `DecayContext::load` reads the projects that said "off" once
//! per call, and `is_stale_at` hides the flow notes (`NOTE_IDS`) at those
//! projects only. No other note is touched, and a project with no answer or
//! with "on" still sees them.
//!
//! THE SAFETY NET. The installer prints the question when it registers a git
//! project for the first time. A git project that never got an answer is asked
//! again by one line at its first commit moment of a session, once per session,
//! until an answer is on record (`ask_once_this_session`). A folder that is not
//! a code project (`install --topic`) is never asked.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// The file beside the store.
pub const FILE: &str = "changelog-flow.json";

/// The file that remembers which session was already asked about which project.
pub const ASKED_FILE: &str = "changelog-flow-asked.json";

/// The tag every flow note carries, so they can be recognised.
pub const TAG: &str = "changelog-flow";

/// The ids of every flow note: the commit note (it keeps the id the starter
/// pack always had) and the three release notes. The filter works on these ids.
pub const NOTE_IDS: &[&str] = &[
    "changelog-with-every-user-visible-change",
    "changelog-flow-release-opens-with-what-changed",
    "changelog-flow-versions-move-in-small-steps",
    "changelog-flow-read-the-version-from-git",
];

/// The question, exactly as the assistant puts it to the person (it is
/// written in English here and the assistant says it in the person's language).
/// No figure stands behind either reason, so none is given.
pub const QUESTION: &str = "Shall I keep a changelog and version numbers for this project? Two reasons: a fault is found much faster, because you can see in which version it came in, and it saves effort and cost, because I read the changelog instead of digging through the whole history. Just say yes or no.";

/// The project key the answers are filed under.
pub fn key_of(project: &str) -> String {
    model::checkout::project_match_key(project)
}

fn path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(FILE)
}

fn read_map(file: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default()
}

/// The recorded answer of `project`: `Some(true)` yes, `Some(false)` no, `None`
/// not asked yet (or a file that cannot be read).
pub fn answer(db: &Path, project: &str) -> Option<bool> {
    let key = key_of(project);
    if key.is_empty() {
        return None;
    }
    match read_map(&path(db)).get(&key).map(String::as_str) {
        Some("on") => Some(true),
        Some("off") => Some(false),
        _ => None,
    }
}

/// The keys of the projects that said no.
pub fn off_keys(db: &Path) -> HashSet<String> {
    read_map(&path(db)).into_iter().filter(|(_, v)| v == "off").map(|(k, _)| k).collect()
}

/// What `set` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A first answer for this project.
    Recorded,
    /// The answer was the other one and is now this one.
    Switched,
    /// The same answer was on record already: nothing written.
    Unchanged,
}

/// Record the answer of `project`. Atomic and locked; asking twice with the
/// same answer writes nothing the second time.
pub fn set(db: &Path, project: &str, on: bool) -> Result<Change, String> {
    let key = key_of(project);
    if key.is_empty() {
        return Err("this project has no name to record an answer under".to_string());
    }
    let file = path(db);
    let _lock = crate::store_dir::lock(&crate::store_dir::lock_path_of(&file));
    let Some(existing) = crate::store_dir::read_json_for_update::<BTreeMap<String, String>>(&file) else {
        return Err(format!("{} could not be read, so nothing was changed", file.display()));
    };
    let mut map = existing.unwrap_or_default();
    let word = if on { "on" } else { "off" };
    let before = map.get(&key).cloned();
    if before.as_deref() == Some(word) {
        return Ok(Change::Unchanged);
    }
    map.insert(key, word.to_string());
    let text = serde_json::to_string_pretty(&map).map_err(|e| e.to_string())?;
    crate::store_dir::write(&file, text).map_err(|e| format!("could not write {}: {e}", file.display()))?;
    Ok(if before.is_none() { Change::Recorded } else { Change::Switched })
}

/// Whether `id` is one of the flow notes.
pub fn is_flow_note(id: &str) -> bool {
    NOTE_IDS.contains(&id)
}

/// The installer, as the question block names it: the `install` program that
/// sits next to the running program, with forward slashes.
pub fn install_command() -> String {
    let name = if cfg!(windows) { "install.exe" } else { "install" };
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.join(name)))
        .map(|p| p.display().to_string().replace('\\', "/"))
        .unwrap_or_else(|| "install".to_string())
}

/// The block that tells the assistant to ask the question now, and what to
/// run with the answer. `intro` says why it shows up here.
pub fn ask_block(intro: &str, install: &str, db: &Path) -> String {
    let db = db.display().to_string().replace('\\', "/");
    format!(
        "{intro}\n  Ask the person this ONE question now, in their own language, in your next reply:\n  \"{QUESTION}\"\n  On yes, from inside this project's folder run:  \"{install}\" --set-changelog-flow on --db \"{db}\"\n  On no, from inside this project's folder run:  \"{install}\" --set-changelog-flow off --db \"{db}\"\n  A no is final for this project: it is never raised there again, and nothing is ever blocked either way."
    )
}

/// The one line the first commit of a session carries while a git project has
/// no answer on record.
pub fn commit_line(project: &str, install: &str, db: &Path) -> String {
    ask_block(
        &format!("THOR: the project '{project}' has not been asked yet whether to keep a changelog and version numbers."),
        install,
        db,
    )
}

/// Whether the question may ride along with this session's commit moment: true
/// the first time for (session, project), and with `record` the pair is
/// remembered so the rest of the session stays quiet. A file that cannot be
/// read or written means the line is shown, never lost.
pub fn ask_once_this_session(db: &Path, session_id: &str, project: &str, record: bool) -> bool {
    let file = db.parent().unwrap_or_else(|| Path::new(".")).join(ASKED_FILE);
    let entry = format!("{session_id}\t{}", key_of(project));
    let _lock = crate::store_dir::lock(&crate::store_dir::lock_path_of(&file));
    let mut asked: Vec<String> = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if asked.contains(&entry) {
        return false;
    }
    if record {
        asked.push(entry);
        // A few hundred sessions are plenty to remember; older ones are gone.
        let skip = asked.len().saturating_sub(200);
        asked.drain(..skip);
        if let Ok(text) = serde_json::to_string(&asked) {
            let _ = crate::store_dir::write(&file, text);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_recorded_read_back_and_a_repeat_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        assert_eq!(answer(&db, "Alpha tool"), None);
        assert_eq!(set(&db, "Alpha tool", false), Ok(Change::Recorded));
        assert_eq!(answer(&db, "alpha-tool"), Some(false), "case and spaces do not open a second answer");
        assert_eq!(set(&db, "Alpha tool", false), Ok(Change::Unchanged));
        assert_eq!(set(&db, "Alpha tool", true), Ok(Change::Switched));
        assert_eq!(answer(&db, "Alpha tool"), Some(true));
        assert_eq!(answer(&db, "beta"), None, "another project is not asked about");
        assert!(off_keys(&db).is_empty());
        set(&db, "beta", false).unwrap();
        assert_eq!(off_keys(&db), HashSet::from(["beta".to_string()]));
    }

    #[test]
    fn the_question_names_both_reasons_and_no_figure() {
        assert!(QUESTION.contains("a fault is found much faster") && QUESTION.contains("saves effort and cost"));
        assert!(QUESTION.contains("yes or no"));
        assert!(!QUESTION.chars().any(|c| c.is_ascii_digit()), "no figure stands behind either reason");
    }

    #[test]
    fn a_session_is_asked_once_per_project() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        assert!(ask_once_this_session(&db, "s1", "alpha", true));
        assert!(!ask_once_this_session(&db, "s1", "Alpha", true), "same session, same project");
        assert!(ask_once_this_session(&db, "s2", "alpha", true), "a new session is asked again");
        assert!(ask_once_this_session(&db, "s1", "beta", true), "another project is its own question");
        assert!(ask_once_this_session(&db, "s3", "alpha", false) && ask_once_this_session(&db, "s3", "alpha", false), "no record, no silence");
    }
}
