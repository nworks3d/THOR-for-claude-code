//! What the crowding check has already looked at, per session.
//!
//! The crowding check (`crowding_debt` in the serve binary) asks about the notes
//! a session wrote. Looking at one costs tens of milliseconds, and it used to
//! look at every note written since the session began at every Stop, so a long
//! session paid more with each note it wrote. This keeps, per session, the
//! highest log position already examined and the notes that were found in debt
//! and are not settled yet. The next Stop examines only what is above that
//! position, plus those notes.
//!
//! The file is `crowding-debt-state.json` beside the store; a session not seen
//! for 30 days is dropped when it is written.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FILE: &str = "crowding-debt-state.json";

/// A session is forgotten after this many seconds without a Stop.
pub const KEEP_SECONDS: i64 = 30 * 24 * 3600;

/// One session's progress.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    /// The highest log position the check has examined.
    pub seq: i64,
    /// Notes still owed an answer, in the order they were written.
    pub pending: Vec<String>,
    /// When this session last had a Stop, unix seconds.
    pub at: i64,
}

pub fn path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(FILE)
}

fn read_all(db: &Path) -> BTreeMap<String, Seen> {
    std::fs::read_to_string(path(db)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// This session's progress, `None` when it has none (the check then starts from
/// the session's watermark, as it always did).
pub fn load(db: &Path, session: &str) -> Option<Seen> {
    read_all(db).remove(session)
}

/// Record this session's progress and drop the sessions not seen for 30 days.
/// Nothing is written when the entry is unchanged and was refreshed within the
/// day.
pub fn save(db: &Path, session: &str, mut seen: Seen, now: i64) {
    let mut all = read_all(db);
    if let Some(old) = all.get(session) {
        if old.seq == seen.seq && old.pending == seen.pending && now - old.at < 24 * 3600 {
            return;
        }
    }
    seen.at = now;
    all.insert(session.to_string(), seen);
    all.retain(|_, s| now - s.at <= KEEP_SECONDS);
    if let Ok(text) = serde_json::to_string(&all) {
        let _ = crate::store_dir::write(&path(db), text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_without_state_has_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(&dir.path().join("thor.db"), "s"), None);
    }

    #[test]
    fn saved_progress_is_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        save(&db, "s", Seen { seq: 9, pending: vec!["a".into()], at: 0 }, 1_000);
        assert_eq!(load(&db, "s"), Some(Seen { seq: 9, pending: vec!["a".into()], at: 1_000 }));
    }

    #[test]
    fn sessions_not_seen_for_thirty_days_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        save(&db, "old", Seen { seq: 1, ..Default::default() }, 1_000);
        save(&db, "new", Seen { seq: 2, ..Default::default() }, 1_000 + KEEP_SECONDS + 1);
        assert_eq!(load(&db, "old"), None);
        assert!(load(&db, "new").is_some());
    }

    #[test]
    fn an_unchanged_entry_is_not_rewritten_within_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        save(&db, "s", Seen { seq: 5, ..Default::default() }, 1_000);
        save(&db, "s", Seen { seq: 5, ..Default::default() }, 2_000);
        assert_eq!(load(&db, "s").unwrap().at, 1_000);
        save(&db, "s", Seen { seq: 5, ..Default::default() }, 1_000 + 25 * 3600);
        assert_eq!(load(&db, "s").unwrap().at, 1_000 + 25 * 3600);
    }
}
