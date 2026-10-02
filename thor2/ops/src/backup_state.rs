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
//!
//! Since the backup moved to the NAS it also remembers where the last
//! successful copy went (the remote's name and address) and when, so the
//! health check can name the target while all is well, not only when it fails.

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
    /// The remote the last attempt pushed to (for example `nas`); empty when
    /// the attempt failed before one was chosen.
    #[serde(default)]
    pub remote: String,
    /// Where that remote points (a folder on the NAS, or a web address).
    #[serde(default)]
    pub address: String,
    /// When the last successful push finished (Unix seconds); 0 when none yet.
    #[serde(default)]
    pub last_ok_unix: u64,
}

impl BackupState {
    /// A run of failures is in progress.
    pub fn is_failing(&self) -> bool {
        self.failing_since_unix > 0
    }
}

pub fn path_for(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("backup-state.json")
}

/// What the sidecar holds, or `None` when there is none yet or it cannot be
/// read - the same fail-open stance as `ship_state`. A healthy backup has a
/// sidecar too (its last success); `BackupState::is_failing` tells them apart.
pub fn read(db: &Path) -> Option<BackupState> {
    let text = std::fs::read_to_string(path_for(db)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn repo_name(repo: &Path) -> String {
    repo.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

fn write_state(db: &Path, state: &BackupState) -> std::io::Result<()> {
    let json = serde_json::to_string(state).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(path_for(db), json)
}

/// Record a failed attempt before a remote was chosen. See `record_failure_for`.
pub fn record_failure_at(db: &Path, repo: &Path, reason: &str, now: u64) -> std::io::Result<()> {
    record_failure_for(db, repo, "", "", reason, now)
}

/// Record a failed attempt to `remote` (at `address`). Keeps the time of the
/// FIRST failure of the run, and the time of the last success.
pub fn record_failure_for(
    db: &Path,
    repo: &Path,
    remote: &str,
    address: &str,
    reason: &str,
    now: u64,
) -> std::io::Result<()> {
    let before = read(db);
    let since = before.as_ref().map(|s| s.failing_since_unix).filter(|t| *t > 0).unwrap_or(now);
    let last_ok_unix = before.map(|s| s.last_ok_unix).unwrap_or(0);
    write_state(
        db,
        &BackupState {
            repo: repo_name(repo),
            failing_since_unix: since,
            last_attempt_unix: now,
            last_error: one_line(reason),
            remote: remote.to_string(),
            address: address.to_string(),
            last_ok_unix,
        },
    )
}

/// A success ends the run of failures. What stays is the proof of where the
/// last copy went and when, so the health check can say so while all is well.
///
/// `pushed` is false for a run that did nothing because the last copy is still
/// fresh: the attempt went well, but the time of the last copy must not move.
pub fn record_success_for(db: &Path, repo: &Path, remote: &str, address: &str, now: u64, pushed: bool) {
    let last_ok_unix = if pushed { now } else { read(db).map(|s| s.last_ok_unix).unwrap_or(0) };
    let _ = write_state(
        db,
        &BackupState {
            repo: repo_name(repo),
            failing_since_unix: 0,
            last_attempt_unix: now,
            last_error: String::new(),
            remote: remote.to_string(),
            address: address.to_string(),
            last_ok_unix,
        },
    );
}

/// Seconds still to wait before the next attempt, when the last attempt failed
/// less than `BACKOFF_SECS` ago; `None` when an attempt may run (a success
/// never makes the next attempt wait).
pub fn backoff_left_at(db: &Path, now: u64) -> Option<u64> {
    let state = read(db).filter(BackupState::is_failing)?;
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
    fn a_success_ends_the_failure_and_remembers_where_the_copy_went() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        record_failure_at(&db, Path::new("acme-backup"), "x", 1_000_000).unwrap();
        record_success_for(&db, Path::new("acme-backup"), "nas", "//nas/share/acme-backup", 1_000_500, true);
        let s = read(&db).unwrap();
        assert!(!s.is_failing());
        assert_eq!(s.remote, "nas");
        assert_eq!(s.address, "//nas/share/acme-backup");
        assert_eq!(s.last_ok_unix, 1_000_500);
        assert_eq!(backoff_left_at(&db, 1_000_501), None, "a success never makes the next attempt wait");
    }

    #[test]
    fn a_run_that_pushed_nothing_does_not_move_the_time_of_the_last_copy() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        record_success_for(&db, Path::new("acme-backup"), "nas", "//nas/x", 1_000, true);
        record_success_for(&db, Path::new("acme-backup"), "nas", "//nas/x", 5_000, false);
        assert_eq!(read(&db).unwrap().last_ok_unix, 1_000);
    }

    #[test]
    fn a_failure_keeps_the_time_of_the_last_success() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        record_success_for(&db, Path::new("acme-backup"), "nas", "//nas/x", 1_000, true);
        record_failure_for(&db, Path::new("acme-backup"), "nas", "//nas/x", "share is gone", 2_000).unwrap();
        let s = read(&db).unwrap();
        assert!(s.is_failing());
        assert_eq!(s.failing_since_unix, 2_000);
        assert_eq!(s.last_ok_unix, 1_000);
        assert_eq!(s.remote, "nas");
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
