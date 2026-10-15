//! COPY MODE: a store that is only a read-only copy of the main machine's log.
//!
//! The main machine is the only writer of the event log (a single hash chain).
//! A second machine (a laptop) can still run the full hooks if its store is a
//! COPY: the hooks open it read-only and never append an event, so the chain
//! cannot fork. What the hooks would have recorded goes to a per-machine
//! sidecar (`copy-telemetry.jsonl`) instead.
//!
//! A store is a copy when a marker file `thor-copy.json` sits beside it (the
//! pull writes it), or when the environment variable `THOR_COPY` is `1`. The
//! marker alone suffices, so a laptop install needs no settings change.
//!
//! The marker also remembers where the copy is pulled from (`repo`, a git
//! clone of the backup) and how the last pull went, so a session start can say
//! "the copy is stale" in one line and decide whether a refresh is due.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The marker file beside the store.
pub const MARKER: &str = "thor-copy.json";
/// The per-machine sidecar the hooks write instead of log events.
pub const TELEMETRY: &str = "copy-telemetry.jsonl";
/// The telemetry file is cut when it grows past this many bytes.
const TELEMETRY_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// A session start asks for a pull at most this often, unless the marker says
/// otherwise.
pub const DEFAULT_MIN_INTERVAL_SECS: i64 = 3600;

/// What the marker says. Every field is optional: an empty `{}` is a valid
/// marker that only says "this store is a copy".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Marker {
    /// A git clone of the backup repository (it holds `<subdir>/events-NNNN.jsonl`).
    #[serde(default)]
    pub repo: Option<String>,
    /// The folder inside the clone that holds the segments. Default `thor2`.
    #[serde(default)]
    pub subdir: Option<String>,
    /// The git remote to fetch from. Default: the clone's own upstream.
    #[serde(default)]
    pub remote: Option<String>,
    /// Seconds between pulls. Default `DEFAULT_MIN_INTERVAL_SECS`.
    #[serde(default)]
    pub min_interval_secs: Option<i64>,
    /// When the copy last changed, as a unix time.
    #[serde(default)]
    pub pulled_at_unix: i64,
    /// The copy's chain tip when it last changed.
    #[serde(default)]
    pub tip_seq: i64,
    /// When a pull was last tried (ok or not).
    #[serde(default)]
    pub last_attempt_unix: i64,
    /// Why the last pull failed; empty when it did not.
    #[serde(default)]
    pub last_error: String,
}

pub fn marker_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(MARKER)
}

pub fn telemetry_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(TELEMETRY)
}

/// Whether the store at `db` is a copy: the marker is there, or `THOR_COPY=1`.
pub fn is_copy(db: &Path) -> bool {
    std::env::var("THOR_COPY").map(|v| v == "1").unwrap_or(false) || marker_path(db).is_file()
}

/// The marker as written, or the default when it is missing or unreadable.
pub fn read_marker(db: &Path) -> Marker {
    std::fs::read_to_string(marker_path(db)).ok().and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok()).unwrap_or_default()
}

/// Write the marker (atomically: a temporary file, then a rename).
pub fn write_marker(db: &Path, marker: &Marker) -> std::io::Result<()> {
    let path = marker_path(db);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(marker).unwrap_or_else(|_| "{}".into()))?;
    std::fs::rename(&tmp, &path)
}

/// Append one telemetry line: what a normal hook would have written to the log.
/// Best effort, like every sidecar: a line that cannot be written is lost.
pub fn telemetry(db: &Path, kind: &str, mut fields: serde_json::Value) {
    use std::io::Write as _;
    let path = telemetry_path(db);
    if let Some(object) = fields.as_object_mut() {
        object.insert("kind".into(), kind.into());
        object.insert("at_unix".into(), now_unix().into());
    }
    let Ok(line) = serde_json::to_string(&fields) else { return };
    if std::fs::metadata(&path).map(|m| m.len() > TELEMETRY_MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("jsonl.old"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{line}");
    }
}

/// The telemetry lines of one kind, oldest first (for a status line and tests).
pub fn telemetry_lines(db: &Path, kind: &str) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(telemetry_path(db)).unwrap_or_default();
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("kind").and_then(|k| k.as_str()) == Some(kind))
        .collect()
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Whether a pull is due: the marker names a `repo`, and the last attempt is
/// older than the interval.
pub fn pull_due(marker: &Marker, now: i64) -> bool {
    marker.repo.is_some() && now - marker.last_attempt_unix >= marker.min_interval_secs.unwrap_or(DEFAULT_MIN_INTERVAL_SECS)
}

/// The one plain line a session start says on a copy machine. It also says,
/// when the last pull failed, that the old copy is still in use.
pub fn session_line(marker: &Marker) -> String {
    let mut line = String::from(
        "This machine runs a copy of the memory; the weekly check runs on the main machine. Changes you make here are queued for it and show up here after the next pull.",
    );
    if !marker.last_error.is_empty() {
        line.push_str(&format!(" The last refresh of the copy failed ({}), so the older copy is still in use.", marker.last_error));
    }
    line
}

/// Start `sync pull` for this copy in the background, with no console window,
/// and do not wait for it. The session uses the copy as it is now; the pull
/// swaps in a fresher one for the next session. Returns whether it started.
/// The `sync` program is looked for beside the running program.
pub fn spawn_pull(db: &Path) -> bool {
    let Ok(me) = std::env::current_exe() else { return false };
    let Some(dir) = me.parent() else { return false };
    let sync = dir.join(if cfg!(windows) { "sync.exe" } else { "sync" });
    if !sync.is_file() {
        return false;
    }
    let mut command = std::process::Command::new(sync);
    command
        .arg("pull")
        .arg("--db")
        .arg(db)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW | DETACHED_PROCESS: no console window, outlives the hook.
        command.creation_flags(0x0800_0000 | 0x0000_0008);
    }
    command.spawn().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_beside_the_store_makes_it_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert!(!is_copy(&db));
        write_marker(&db, &Marker::default()).unwrap();
        assert!(is_copy(&db));
    }

    #[test]
    fn an_empty_or_bom_marker_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        std::fs::write(marker_path(&db), "\u{feff}{}").unwrap();
        assert_eq!(read_marker(&db), Marker::default());
    }

    #[test]
    fn telemetry_lines_come_back_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        telemetry(&db, "item_served", serde_json::json!({ "id": "a" }));
        telemetry(&db, "gate_refused", serde_json::json!({ "rule": "r" }));
        assert_eq!(telemetry_lines(&db, "item_served").len(), 1);
        assert_eq!(telemetry_lines(&db, "gate_refused")[0]["rule"], "r");
    }

    #[test]
    fn a_pull_is_due_only_with_a_repo_and_an_old_enough_attempt() {
        let mut m = Marker::default();
        assert!(!pull_due(&m, 10_000), "no repo named: nothing to pull from");
        m.repo = Some("x".into());
        assert!(pull_due(&m, 10_000));
        m.last_attempt_unix = 9_000;
        assert!(!pull_due(&m, 10_000));
        assert!(pull_due(&m, 9_000 + DEFAULT_MIN_INTERVAL_SECS));
    }

    #[test]
    fn the_session_line_names_a_failed_refresh() {
        let mut m = Marker::default();
        assert!(!session_line(&m).contains("failed"));
        m.last_error = "git fetch timed out".into();
        assert!(session_line(&m).contains("git fetch timed out"));
    }
}
