//! The owner's recorded NOs: the three things the installer wires by default
//! that a person can say no to (the connections with Claude Code, the git
//! helpers, the reply rules). The choice is a small file beside the memory
//! (`install-choices.json`), written ONLY by `install --set-connections`,
//! `--set-git-hooks` and `--set-reply-rules`, so an assistant never edits a
//! settings file by hand to say no.
//!
//! WHY IT EXISTS. Without a record, the next plain installer run wired
//! everything again (a person who said no got the connections back), and
//! doctor called the no a PROBLEM. A recorded no is respected by every later
//! installer run, and doctor reports it as the person's choice.

use std::path::{Path, PathBuf};

/// The file beside the memory.
pub const FILE: &str = "install-choices.json";

/// One thing a person can say no to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// THOR's four connections in Claude Code's settings.
    Connections,
    /// The machine-wide git helpers (code index refresh and the video and size check).
    GitHooks,
    /// The reply rulebook as a whole.
    ReplyRules,
}

impl Choice {
    fn key(self) -> &'static str {
        match self {
            Choice::Connections => "connections",
            Choice::GitHooks => "git_hooks",
            Choice::ReplyRules => "reply_rules",
        }
    }

    /// The switch that changes it again, for a plain line.
    pub fn switch(self) -> &'static str {
        match self {
            Choice::Connections => "--set-connections",
            Choice::GitHooks => "--set-git-hooks",
            Choice::ReplyRules => "--set-reply-rules",
        }
    }

    /// What it is, in words a person knows.
    pub fn words(self) -> &'static str {
        match self {
            Choice::Connections => "the connection between THOR and Claude Code",
            Choice::GitHooks => "THOR's helpers in git",
            Choice::ReplyRules => "the reply rules",
        }
    }
}

fn path_of(db: &Path) -> PathBuf {
    db.parent().map(|d| d.join(FILE)).unwrap_or_else(|| PathBuf::from(FILE))
}

fn read(db: &Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(path_of(db))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Whether the person said no to this one. Anything unreadable counts as no
/// recorded choice, so the default (on) holds.
pub fn is_off(db: &Path, choice: Choice) -> bool {
    read(db).get(choice.key()).and_then(|v| v.as_str()) == Some("off")
}

/// Record the choice. `on` removes the record, so the file only ever holds noes.
pub fn set(db: &Path, choice: Choice, on: bool) -> std::io::Result<()> {
    let mut map = read(db);
    if on {
        map.remove(choice.key());
    } else {
        map.insert(choice.key().to_string(), serde_json::Value::String("off".to_string()));
    }
    let path = path_of(db);
    if map.is_empty() {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
    }
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(map)).map_err(std::io::Error::other)? + "\n";
    std::fs::write(path, text)
}

/// The one plain line an installer run prints instead of wiring something the
/// person said no to.
pub fn respected_line(choice: Choice) -> String {
    format!(
        "= you chose no for {}, so it was left off; to switch it on again the assistant runs install {} on",
        choice.words(),
        choice.switch()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_no_is_recorded_and_a_yes_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert!(!is_off(&db, Choice::Connections));
        set(&db, Choice::Connections, false).unwrap();
        set(&db, Choice::ReplyRules, false).unwrap();
        assert!(is_off(&db, Choice::Connections) && is_off(&db, Choice::ReplyRules) && !is_off(&db, Choice::GitHooks));
        set(&db, Choice::Connections, true).unwrap();
        assert!(!is_off(&db, Choice::Connections) && is_off(&db, Choice::ReplyRules));
        set(&db, Choice::ReplyRules, true).unwrap();
        assert!(!dir.path().join(FILE).exists(), "no noes left, no file left");
    }

    #[test]
    fn a_damaged_file_means_no_recorded_choice() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        std::fs::write(dir.path().join(FILE), "{ not json").unwrap();
        assert!(!is_off(&db, Choice::GitHooks));
    }
}
