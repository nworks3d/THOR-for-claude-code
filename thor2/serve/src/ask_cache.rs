//! The two slow readings behind the weekly-check ask, kept while nothing they read has changed.
//!
//! The ask says how many notes will be on the verdict list and, on a daily rhythm, why
//! (`eval_filing::project_rot`). Each reading walks the whole log: about 2 seconds each on a log
//! of 340,000 events. A held turn asks again at every Stop, and between two Stops of the same
//! conversation nothing they read has usually changed, so the same 4 to 5 seconds were spent
//! again at every Stop.
//!
//! The answer is kept in one small file beside the store, for one project and session and one
//! hour at the most, and for one of two things:
//! - the log position, together with the two small files the readings also read (the bug
//!   candidates and the sweep): anything else that is written changes it, and the readings are
//!   made again;
//! - the evaluation the ask started. While it is open the readings are the ones it was asked
//!   with, so every later Stop of the held turn finds them, whatever else the log gained
//!   meanwhile (the start's own records, its sweep).
//!
//! A file that cannot be read or written only means they are made again.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thor_core::event_store::EventStore;

const FILE: &str = "eval-ask-cache.json";

/// How long a kept reading may be used at the most, in seconds.
const MAX_AGE_SECS: i64 = 3600;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Kept {
    /// What the readings are about: `tip <seq>` or `start <record id>`.
    anchor: String,
    /// The project, the session and the hour they were made for.
    scope: String,
    /// The two small files they also read, as they were (not looked at for a `start` anchor).
    stamps: String,
    owed_words: String,
    /// Whether `rot` was worked out (only a project on the weekly rhythm needs it).
    rot_known: bool,
    rot: Option<String>,
}

fn path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(FILE)
}

/// A small stamp of a file: its size and modification time, or nothing when it is not there.
fn stamp(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
            format!("{}:{modified}", meta.len())
        }
        Err(_) => "-".to_string(),
    }
}

fn scope_of(project: &str, session: &str, now: i64) -> String {
    format!("{project}|{session}|{}", now / MAX_AGE_SECS)
}

fn stamps_of(db: &Path) -> String {
    let dir = db.parent().unwrap_or_else(|| Path::new("."));
    format!(
        "{}|{}|{}",
        stamp(&dir.join(crate::thor_bugs::FILE)),
        stamp(&dir.join(crate::thor_bugs::MODE_FILE)),
        stamp(&crate::sweep::state_path(db))
    )
}

fn tip_anchor(tip: i64) -> String {
    format!("tip {tip}")
}

fn start_anchor(start_id: &str) -> String {
    format!("start {start_id}")
}

fn read_kept(db: &Path) -> Option<Kept> {
    std::fs::read_to_string(path(db)).ok().and_then(|text| serde_json::from_str(&text).ok())
}

fn write_kept(db: &Path, kept: &Kept) {
    if let Ok(text) = serde_json::to_string(kept) {
        let _ = crate::store_dir::write(&path(db), text);
    }
}

/// The ask that made the readings goes on to start the evaluation it asks for. The readings
/// were kept under the log position before that; the list the ask names is the list the start
/// freezes, so they are now kept under the start itself and the next Stop of the held turn
/// finds them. Only readings kept under `tip_before` move; anything else is left as it is.
pub fn carry_over(db: &Path, project: &str, session: &str, now: i64, tip_before: Option<i64>, start_id: &str) {
    let Some(tip_before) = tip_before else { return };
    let Some(mut kept) = read_kept(db) else { return };
    if kept.anchor != tip_anchor(tip_before) || kept.scope != scope_of(project, session, now) {
        return;
    }
    kept.anchor = start_anchor(start_id);
    write_kept(db, &kept);
}

/// The verdict-list words and (when `want_rot`) the reason for the daily rhythm, from the kept
/// answer when it is still valid, else worked out by the two functions and kept. `open_start`
/// is the record of the evaluation this session has open for the project, when it has one.
#[allow(clippy::too_many_arguments)]
pub fn readings(
    db: &Path,
    store: &EventStore,
    project: &str,
    session: &str,
    now: i64,
    open_start: Option<&str>,
    want_rot: bool,
    owed_words: impl FnOnce() -> String,
    rot: impl FnOnce() -> Option<String>,
) -> (String, Option<String>) {
    let anchor = match open_start {
        Some(id) => Some(start_anchor(id)),
        None => store.cache_fingerprint().ok().map(tip_anchor),
    };
    let Some(anchor) = anchor else {
        return (owed_words(), if want_rot { rot() } else { None });
    };
    let scope = scope_of(project, session, now);
    let stamps = stamps_of(db);
    let follows_start = anchor.starts_with("start ");
    let still_valid = |k: &Kept| k.anchor == anchor && k.scope == scope && (follows_start || k.stamps == stamps);
    let mut kept = read_kept(db).filter(still_valid).unwrap_or_else(|| Kept { anchor, scope, stamps, ..Kept::default() });
    let mut changed = false;
    if kept.owed_words.is_empty() {
        kept.owed_words = owed_words();
        changed = true;
    }
    if want_rot && !kept.rot_known {
        kept.rot = rot();
        kept.rot_known = true;
        changed = true;
    }
    if changed {
        write_kept(db, &kept);
    }
    (kept.owed_words, if want_rot { kept.rot } else { None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn store_in(dir: &Path) -> (PathBuf, EventStore) {
        let db = dir.join("thor.db");
        let store = EventStore::new(&db).unwrap();
        (db, store)
    }

    #[test]
    fn the_second_ask_uses_the_kept_readings_until_the_log_moves() {
        let dir = tempfile::tempdir().unwrap();
        let (db, mut store) = store_in(dir.path());
        let (owed_calls, rot_calls) = (Cell::new(0), Cell::new(0));
        let ask = |store: &EventStore, want_rot: bool| {
            readings(
                &db,
                store,
                "demo",
                "s1",
                1_000_000,
                None,
                want_rot,
                || {
                    owed_calls.set(owed_calls.get() + 1);
                    "3 item(s)".to_string()
                },
                || {
                    rot_calls.set(rot_calls.get() + 1);
                    Some("1 item(s) owe a verdict".to_string())
                },
            )
        };
        assert_eq!(ask(&store, true), ("3 item(s)".to_string(), Some("1 item(s) owe a verdict".to_string())));
        assert_eq!(ask(&store, true).0, "3 item(s)");
        assert_eq!((owed_calls.get(), rot_calls.get()), (1, 1), "the second ask worked nothing out");
        // A project on the weekly rhythm never asked for the rot: it is not worked out, and
        // asked later it is worked out once.
        assert_eq!(ask(&store, false).1, None);
        assert_eq!(rot_calls.get(), 1);
        // The log moved: both are made again.
        store.append_event("s", "l", "t", thor_core::event_store::EventKind::FactCreated, "P:a", None, "a").unwrap();
        ask(&store, true);
        assert_eq!((owed_calls.get(), rot_calls.get()), (2, 2));
    }

    /// THE HELD TURN: the first ask makes the readings and starts the evaluation (which writes
    /// to the log); every later Stop of the turn asks again with the evaluation open and finds
    /// the same readings, however much the log gained in between.
    #[test]
    fn the_readings_follow_the_evaluation_the_ask_started() {
        let dir = tempfile::tempdir().unwrap();
        let (db, mut store) = store_in(dir.path());
        let calls = Cell::new(0);
        let ask = |store: &EventStore, open: Option<&str>| {
            readings(&db, store, "demo", "s1", 1_000_000, open, true, || {
                calls.set(calls.get() + 1);
                "3 item(s)".to_string()
            }, || {
                calls.set(calls.get() + 1);
                None
            })
        };
        let tip_before = store.cache_fingerprint().ok();
        ask(&store, None);
        assert_eq!(calls.get(), 2);
        // The ask starts the evaluation: its records move the log.
        store.append_event("s", "l", "t", thor_core::event_store::EventKind::FactCreated, "P:start", None, "a").unwrap();
        carry_over(&db, "demo", "s1", 1_000_000, tip_before, "evaluation-start-demo-1");
        // More writes, then the next Stop: the evaluation is open, the readings are the kept ones.
        store.append_event("s", "l", "t", thor_core::event_store::EventKind::FactCreated, "P:later", None, "b").unwrap();
        assert_eq!(ask(&store, Some("evaluation-start-demo-1")).0, "3 item(s)");
        assert_eq!(calls.get(), 2, "nothing was worked out again");
        // Readings kept for something else are not moved: a later start finds none.
        assert_eq!(ask(&store, Some("evaluation-start-demo-2")).0, "3 item(s)");
        assert_eq!(calls.get(), 4, "another evaluation has readings of its own");
    }

    #[test]
    fn another_session_a_later_hour_or_a_changed_sidecar_asks_again() {
        let dir = tempfile::tempdir().unwrap();
        let (db, store) = store_in(dir.path());
        let owed_calls = Cell::new(0);
        let ask = |session: &str, now: i64| {
            readings(&db, &store, "demo", session, now, None, false, || {
                owed_calls.set(owed_calls.get() + 1);
                "x".to_string()
            }, || None)
        };
        ask("s1", 1_000_000);
        ask("s1", 1_000_100);
        assert_eq!(owed_calls.get(), 1);
        ask("s2", 1_000_100);
        assert_eq!(owed_calls.get(), 2, "another session");
        ask("s2", 1_000_100 + MAX_AGE_SECS);
        assert_eq!(owed_calls.get(), 3, "an hour later");
        std::fs::write(dir.path().join(crate::thor_bugs::FILE), "{}").unwrap();
        ask("s2", 1_000_100 + MAX_AGE_SECS);
        assert_eq!(owed_calls.get(), 4, "a new bug candidate file");
    }
}
