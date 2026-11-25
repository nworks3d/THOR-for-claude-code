//! KEEPING THE PER-SESSION STATE FILES SMALL.
//!
//! Five files beside the memory hold one entry per session and used to grow for ever (one real
//! machine: 336 entries in one of them, 221 older than 30 days): the session watermark, the
//! evaluation's turn marks, and the three "this attempt was already refused once" markers of
//! the guards. A session that has had no hook call for `KEEP_DAYS` days is over, so its
//! entries go.
//!
//! WHEN A SESSION WAS LAST USED comes from `session-last-seen.json`: every hook call notes its
//! session there (`touch`, written at most once an hour per session, so a busy session costs
//! nothing), and a long-lived session that is still used keeps its old watermark for as long as
//! it is used. A session found in a state file with no last-seen time yet (written before this
//! existed) is given the time of the first pruning as its last-seen time, so nothing is cut
//! early. The pruning (`prune_if_due`) runs at most once a day.
//!
//! Every file is read and rewritten under its own lock and through the atomic write
//! (`store_dir`), exactly as its own writer does; a file that cannot be read or parsed is left
//! alone. Best effort, like every sidecar: nothing here ever fails a hook.

use crate::store_dir;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The last-seen sidecar, beside the memory.
pub const LAST_SEEN_FILE: &str = "session-last-seen.json";
/// A session with no hook call for this many days is over.
pub const KEEP_DAYS: i64 = 90;
/// A session's last-seen time is rewritten at most this often.
const TOUCH_EVERY_SECS: i64 = 3_600;
/// The pruning runs at most this often.
const PRUNE_EVERY_SECS: i64 = 86_400;

/// Files keyed by session id (a JSON object).
const KEYED_FILES: [&str; 4] =
    ["session-watermark.json", "eval-turn-marks.json", "absent-guard-blocked.json", "absent-guard-command-blocked.json"];
/// A file that is a JSON list of session ids.
const LISTED_FILE: &str = "stale-guard-blocked.json";

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct LastSeen {
    /// When the pruning last ran, unix seconds.
    #[serde(default)]
    pruned_at: i64,
    /// Session id to when a hook call of it was last seen.
    #[serde(default)]
    seen: BTreeMap<String, i64>,
}

fn path_in(db: &Path, name: &str) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(name)
}

fn read_last_seen(path: &Path) -> Option<LastSeen> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).ok(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(LastSeen::default()),
        Err(_) => None,
    }
}

/// Note that `session` made a hook call at `now`. Cheap: nothing is written while its last-seen
/// time is under an hour old.
pub fn touch(db: &Path, session: &str, now: i64) {
    if session.is_empty() {
        return;
    }
    let path = path_in(db, LAST_SEEN_FILE);
    let fresh = |state: &LastSeen| state.seen.get(session).is_some_and(|at| now - at < TOUCH_EVERY_SECS && now >= *at);
    // The common case (seen within the hour) reads without taking the lock.
    if read_last_seen(&path).is_some_and(|state| fresh(&state)) {
        return;
    }
    let _lock = store_dir::lock(&store_dir::lock_path_of(&path));
    let Some(mut state) = read_last_seen(&path) else { return };
    if fresh(&state) {
        return;
    }
    state.seen.insert(session.to_string(), now);
    if let Ok(text) = serde_json::to_string(&state) {
        let _ = store_dir::write(&path, text);
    }
}

/// Run the pruning when it was last run more than a day ago. Returns how many entries went.
pub fn prune_if_due(db: &Path, now: i64) -> usize {
    let path = path_in(db, LAST_SEEN_FILE);
    let _lock = store_dir::lock(&store_dir::lock_path_of(&path));
    let Some(mut state) = read_last_seen(&path) else { return 0 };
    if now - state.pruned_at < PRUNE_EVERY_SECS && now >= state.pruned_at {
        return 0;
    }
    let removed = prune_files(db, &mut state, now);
    state.pruned_at = now;
    if let Ok(text) = serde_json::to_string(&state) {
        let _ = store_dir::write(&path, text);
    }
    removed
}

/// The sessions named in the state files.
fn named_sessions(db: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for name in KEYED_FILES {
        if let Some(serde_json::Value::Object(map)) =
            std::fs::read_to_string(path_in(db, name)).ok().and_then(|t| serde_json::from_str(&t).ok())
        {
            out.extend(map.keys().cloned());
        }
    }
    if let Some(serde_json::Value::Array(list)) =
        std::fs::read_to_string(path_in(db, LISTED_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok())
    {
        out.extend(list.iter().filter_map(|v| v.as_str().map(str::to_string)));
    }
    out
}

fn prune_files(db: &Path, state: &mut LastSeen, now: i64) -> usize {
    // A session with no last-seen time yet starts its 90 days now.
    for session in named_sessions(db) {
        state.seen.entry(session).or_insert(now);
    }
    let cutoff = now - KEEP_DAYS * 86_400;
    let over: BTreeSet<String> = state.seen.iter().filter(|(_, at)| **at < cutoff).map(|(s, _)| s.clone()).collect();
    if over.is_empty() {
        return 0;
    }
    let mut removed = 0;
    for name in KEYED_FILES {
        let path = path_in(db, name);
        let _lock = store_dir::lock(&store_dir::lock_path_of(&path));
        let Some(serde_json::Value::Object(mut map)) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok())
        else {
            continue;
        };
        let before = map.len();
        map.retain(|session, _| !over.contains(session));
        if map.len() != before {
            removed += before - map.len();
            if let Ok(text) = serde_json::to_string(&serde_json::Value::Object(map)) {
                let _ = store_dir::write(&path, text);
            }
        }
    }
    let path = path_in(db, LISTED_FILE);
    let _lock = store_dir::lock(&store_dir::lock_path_of(&path));
    if let Some(serde_json::Value::Array(mut list)) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()) {
        let before = list.len();
        list.retain(|v| v.as_str().is_none_or(|s| !over.contains(s)));
        if list.len() != before {
            removed += before - list.len();
            if let Ok(text) = serde_json::to_string(&serde_json::Value::Array(list)) {
                let _ = store_dir::write(&path, text);
            }
        }
    }
    state.seen.retain(|session, _| !over.contains(session));
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    fn read(dir: &Path, name: &str) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join(name)).unwrap()).unwrap()
    }

    #[test]
    fn a_session_unused_for_ninety_days_loses_its_entries_and_a_used_one_keeps_them() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let now = 1_800_000_000;
        write(dir.path(), "session-watermark.json", r#"{"old":10,"live":20,"unknown":30}"#);
        write(dir.path(), "eval-turn-marks.json", r#"{"old":{"at_seq":1},"live":{"at_seq":2}}"#);
        write(dir.path(), "absent-guard-blocked.json", r#"{"old":["a#1"],"live":["b#2"]}"#);
        write(dir.path(), "absent-guard-command-blocked.json", r#"{"old":["bash#1"]}"#);
        write(dir.path(), "stale-guard-blocked.json", r#"["old","live"]"#);
        // "old" was last seen 100 days ago, "live" a day ago and ran a long time before that.
        touch(&db, "old", now - 100 * DAY);
        touch(&db, "live", now - 100 * DAY);
        touch(&db, "live", now - DAY);
        let removed = prune_if_due(&db, now);
        assert_eq!(removed, 5, "one entry of old in each of the five files");
        assert_eq!(read(dir.path(), "session-watermark.json"), serde_json::json!({"live":20,"unknown":30}));
        assert_eq!(read(dir.path(), "eval-turn-marks.json"), serde_json::json!({"live":{"at_seq":2}}));
        assert_eq!(read(dir.path(), "absent-guard-blocked.json"), serde_json::json!({"live":["b#2"]}));
        assert_eq!(read(dir.path(), "absent-guard-command-blocked.json"), serde_json::json!({}));
        assert_eq!(read(dir.path(), "stale-guard-blocked.json"), serde_json::json!(["live"]));
        // An unknown session starts its 90 days at the first pruning, so it is not cut early.
        assert_eq!(read(dir.path(), LAST_SEEN_FILE)["seen"]["unknown"], now);
        // The pruning runs once a day: a second call the same day does nothing.
        write(dir.path(), "session-watermark.json", r#"{"live":20,"stale-again":5}"#);
        assert_eq!(prune_if_due(&db, now + 3_600), 0);
        // 91 days later the unknown session, unused since, goes too.
        assert!(prune_if_due(&db, now + 91 * DAY) >= 1);
    }

    #[test]
    fn touching_is_cheap_and_a_broken_file_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        touch(&db, "s1", 1_000);
        let before = std::fs::read_to_string(dir.path().join(LAST_SEEN_FILE)).unwrap();
        touch(&db, "s1", 1_100);
        assert_eq!(std::fs::read_to_string(dir.path().join(LAST_SEEN_FILE)).unwrap(), before, "within the hour nothing is written");
        write(dir.path(), "session-watermark.json", "not json at all");
        write(dir.path(), LAST_SEEN_FILE, "{ broken");
        touch(&db, "s2", 5_000_000);
        assert_eq!(prune_if_due(&db, 5_000_000), 0);
        assert_eq!(std::fs::read_to_string(dir.path().join("session-watermark.json")).unwrap(), "not json at all");
    }
}
