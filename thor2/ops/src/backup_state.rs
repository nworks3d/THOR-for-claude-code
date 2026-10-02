//! What the off-site backup remembers about its own failures: a small sidecar
//! beside the store (`backup-state.json`), the same shape as `ship_state`.
//!
//! WHY IT EXISTS. On 2026-10-01 the single exported log passed GitHub's file
//! size limit and every backup since then failed at the push. The backup runs
//! as a SessionStart hook, and a failing hook fails open: nothing said a word,
//! and every session start paid several seconds retrying a push that could not
//! work. This file lets the backup back off for an hour after a failed attempt
//! instead of retrying at every start, and lets the health check say plainly
//! that the off-site copy is stale and since when.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// After a failed attempt the backup does not try again for this long, unless
/// it is forced. A failure is not a reason to slow every session start.
pub const BACKOFF_SECS: u64 = 3600;

/// The longest failure reason kept, as one line.
const REASON_MAX_CHARS: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BackupState {
    /// The backup repository's folder name (for example `acme-backup`).
    #[serde(default)]
    pub repo: String,
    /// When the first failure of the current run of failures happened (Unix
    /// seconds). A success removes the whole file, so this is only ever set
    /// while the backup is failing.
    #[serde(default)]
    pub failing_since_unix: u64,
    /// When the most recent attempt ran.
    #[serde(default)]
    pub last_attempt_unix: u64,
    /// Why the most recent attempt failed, on one line.
    #[serde(default)]
    pub last_error: String,
}

pub fn path_for(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("backup-state.json")
}

/// The recorded failure, or `None` while the backup is healthy (no sidecar) or
/// the sidecar cannot be read - the same fail-open stance as `ship_state`.
pub fn read(db: &Path) -> Option<BackupState> {
    let text = std::fs::read_to_string(path_for(db)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Record a failed attempt. Keeps the time of the FIRST failure of the run.
pub fn record_failure_at(db: &Path, repo: &Path, reason: &str, now: u64) -> std::io::Result<()> {
    let since = read(db).map(|s| s.failing_since_unix).filter(|t| *t > 0).unwrap_or(now);
    let state = BackupState {
        repo: repo.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        failing_since_unix: since,
        last_attempt_unix: now,
        last_error: one_line(reason),
    };
    let json = serde_json::to_string(&state).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(path_for(db), json)
}

/// A success ends the run of failures: the sidecar goes away, so a healthy
/// backup leaves nothing behind and the health check says nothing.
pub fn record_success(db: &Path) {
    let _ = std::fs::remove_file(path_for(db));
}

/// Seconds still to wait before the next attempt, when the last attempt failed
/// less than `BACKOFF_SECS` ago; `None` when an attempt may run.
pub fn backoff_left_at(db: &Path, now: u64) -> Option<u64> {
    let state = read(db)?;
    let waited = now.saturating_sub(state.last_attempt_unix);
    (waited < BACKOFF_SECS).then(|| BACKOFF_SECS - waited)
}

fn one_line(reason: &str) -> String {
    let joined = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() <= REASON_MAX_CHARS {
        return joined;
    }
    let cut: String = joined.chars().take(REASON_MAX_CHARS).collect();
    format!("{cut}...")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_recorded_while_the_backup_is_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert_eq!(read(&db), None);
        assert_eq!(backoff_left_at(&db, 1_000_000), None);
    }

    #[test]
    fn a_failure_makes_the_next_attempts_wait_for_an_hour() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        record_failure_at(&db, Path::new("C:/x/acme-backup"), "push refused", 1_000_000).unwrap();
        assert_eq!(backoff_left_at(&db, 1_000_000 + 600), Some(3000));
        assert_eq!(backoff_left_at(&db, 1_000_000 + BACKOFF_SECS), None, "after an hour it tries again");
    }

    #[test]
    fn the_first_failure_time_survives_later_failures() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        record_failure_at(&db, Path::new("acme-backup"), "one", 1_000_000).unwrap();
        record_failure_at(&db, Path::new("acme-backup"), "two", 1_090_000).unwrap();
        let s = read(&db).unwrap();
        assert_eq!(s.failing_since_unix, 1_000_000);
        assert_eq!(s.last_attempt_unix, 1_090_000);
        assert_eq!(s.last_error, "two");
        assert_eq!(s.repo, "acme-backup");
    }

    #[test]
    fn a_success_clears_everything() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        record_failure_at(&db, Path::new("acme-backup"), "x", 1_000_000).unwrap();
        record_success(&db);
        assert_eq!(read(&db), None);
        assert_eq!(backoff_left_at(&db, 1_000_001), None);
    }

    #[test]
    fn a_long_multi_line_reason_becomes_one_short_line() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let reason = format!("remote: {}\nremote: more", "x".repeat(900));
        record_failure_at(&db, Path::new("acme-backup"), &reason, 5).unwrap();
        let s = read(&db).unwrap();
        assert!(!s.last_error.contains('\n'));
        assert!(s.last_error.chars().count() <= REASON_MAX_CHARS + 3);
    }
}
