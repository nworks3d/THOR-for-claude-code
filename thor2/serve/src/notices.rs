//! Info lines a session start repeats only once a day per project (step 31).
//!
//! Field test 6 (2026-10-02): every start repeated "working copy ... is
//! registered at ..." and "N open point(s) ... are the owner's to read", about
//! 250 characters of the 9,500-byte budget each time, with nothing for the
//! agent to do about either. They are shown at most once per day per project;
//! `doctor` has both whenever the owner wants them.
//!
//! One small file beside the store, `info-notice-log.json`: `{"<project>\t<kind>":
//! <unix time last shown>}`. Best effort like every sidecar: a file that cannot be
//! read or written means the line is shown, never that it is lost.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The file beside the store.
pub const FILE: &str = "info-notice-log.json";

/// How long a shown line stays quiet.
pub const QUIET_SECONDS: i64 = 24 * 3600;

fn path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(FILE)
}

/// Whether the `kind` line of `project` is due now: it was never shown, or
/// last shown a day or more ago (or in the future, a clock gone wrong). With
/// `record`, a due line is stamped shown now, so the next start stays quiet.
pub fn due(db: &Path, project: &str, kind: &str, now: i64, record: bool) -> bool {
    let file = path(db);
    let mut log: BTreeMap<String, i64> =
        std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let key = format!("{project}\t{kind}");
    if log.get(&key).is_some_and(|last| *last <= now && now - *last < QUIET_SECONDS) {
        return false;
    }
    if record {
        log.insert(key, now);
        if let Ok(text) = serde_json::to_string(&log) {
            let _ = crate::store_dir::write(&file, text);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_shown_once_a_day_per_project_and_kind() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let t0 = 1_800_000_000;
        assert!(due(&db, "alpha", "registration", t0, true));
        assert!(!due(&db, "alpha", "registration", t0 + 3_600, true), "quiet for a day");
        assert!(due(&db, "alpha", "open-points", t0 + 3_600, true), "another kind is its own line");
        assert!(due(&db, "beta", "registration", t0 + 3_600, true), "another project is its own line");
        assert!(due(&db, "alpha", "registration", t0 + QUIET_SECONDS, true), "a day later it is due again");
    }

    #[test]
    fn asking_without_recording_never_silences_the_line() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        assert!(due(&db, "alpha", "registration", 1_800_000_000, false));
        assert!(due(&db, "alpha", "registration", 1_800_000_001, false));
    }

    #[test]
    fn an_unreadable_log_shows_the_line() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        std::fs::write(dir.path().join(FILE), "not json").unwrap();
        assert!(due(&db, "alpha", "registration", 1_800_000_000, true));
    }
}
