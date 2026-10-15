//! The store's own directory, and the one promise a hook call can make about
//! it: `serve hook --no-record` leaves every file there exactly as it found
//! it - the store, its SQLite companions, and every state file beside it
//! (`eval-debt-state.json`, `session-watermark.json`, `capture-owed.json`,
//! `teeth-asked.json`, the guard markers, the decay stamp, ...).
//!
//! THE DEFECT THIS CLOSES (measured 2026-09-26 on a copy of a real store).
//! `--no-record` skipped the three delivery writes and nothing else, so a
//! measurement run still wrote a session line into `eval-debt-state.json`,
//! and every other state file a hook arm keeps was just as open to it. The
//! flag's own promise, "leave no trace", held for the log and not for the
//! directory the log lives in.
//!
//! HOW IT HOLDS. For the length of one hook call the store's directory is
//! HELD (`hold`, released when the returned guard drops). While it is held,
//! every state file write beside the store goes through `write`, which does
//! nothing, and every store open goes through `open_existing`/`open_for_hook`,
//! which hand back a read-only connection (`EventStore::open_read_only`), so a
//! write that slips past this module still fails instead of landing. The
//! decisions a hook call makes are computed exactly as before; only the writes
//! are skipped, which is what keeps the output of a held call the same as an
//! ordinary one's.
//!
//! Keyed by directory, not a process-wide switch: the unit tests call the hook
//! in-process, many at once, each in its own temporary directory, and a held
//! directory must never silence a neighbour's writes.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use thor_core::event_store::EventStore;

/// The held directories; the flag is true for a COPY hold (`hold_copy`).
static HELD: Mutex<Vec<(PathBuf, bool)>> = Mutex::new(Vec::new());

/// The directory a store (or a state file beside it) lives in, exactly as the
/// caller's own path spells it - every state file is built by joining a name
/// onto the store path's own parent, so the two always compare equal.
fn dir_of(path: &Path) -> PathBuf {
    path.parent().map(Path::to_path_buf).unwrap_or_default()
}

/// Releases one hold when dropped.
pub struct Hold {
    dir: PathBuf,
    copy: bool,
}

impl Drop for Hold {
    fn drop(&mut self) {
        let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = held.iter().position(|(d, c)| *d == self.dir && *c == self.copy) {
            held.remove(i);
        }
    }
}

/// Hold the directory of `db_path` read-only until the returned guard drops.
pub fn hold(db_path: &Path) -> Hold {
    let dir = dir_of(db_path);
    HELD.lock().unwrap_or_else(|e| e.into_inner()).push((dir.clone(), false));
    Hold { dir, copy: false }
}

/// Hold the directory of a COPY store (`thor_core::copy_mode`): the same as
/// `hold`, except the per-machine telemetry sidecar stays writable. The store
/// itself is opened read-only, so no event can be appended; every other state
/// file beside it is left alone. (The once-per-context counts live in the
/// `shown-counts` folder below the store's directory, which `hold` never
/// covered either, so they keep working.)
pub fn hold_copy(db_path: &Path) -> Hold {
    let dir = dir_of(db_path);
    HELD.lock().unwrap_or_else(|e| e.into_inner()).push((dir.clone(), true));
    Hold { dir, copy: true }
}

/// Whether the store at `db_path` is a copy (marker beside it, or THOR_COPY=1).
pub fn is_copy(db_path: &Path) -> bool {
    thor_core::copy_mode::is_copy(db_path)
}

/// Hold `db_path`'s directory the way its mode asks: a copy hold for a copy
/// store, a full hold when `no_record`, nothing otherwise.
pub fn hold_for(db_path: &Path, no_record: bool) -> Option<Hold> {
    match (is_copy(db_path), no_record) {
        (true, _) => Some(hold_copy(db_path)),
        (false, true) => Some(hold(db_path)),
        (false, false) => None,
    }
}

/// Whether `path` (the store, or a file beside it) sits in a held directory.
pub fn is_held(path: &Path) -> bool {
    let dir = dir_of(path);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    HELD.lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|(d, copy)| *d == dir && !(*copy && name == thor_core::copy_mode::TELEMETRY))
}

/// `std::fs::write` for a state file beside the store - skipped, successfully,
/// while that directory is held.
pub fn write(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    if is_held(path) {
        return Ok(());
    }
    std::fs::write(path, contents)
}

/// `EventStore::open_existing`, or a read-only open while the directory is held.
pub fn open_existing(db_path: &Path) -> anyhow::Result<EventStore> {
    if is_held(db_path) {
        return EventStore::open_read_only(db_path);
    }
    EventStore::open_existing(db_path)
}

/// The hook's own main open: `EventStore::new` (which may create the store,
/// finish its schema and checkpoint it), or a read-only open while held - a
/// held call never creates a store that is not there.
pub fn open_for_hook(db_path: &Path) -> Option<EventStore> {
    if is_held(db_path) {
        return EventStore::open_read_only(db_path).ok();
    }
    EventStore::new(db_path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_directory_skips_writes_and_a_released_one_takes_them_again() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let state = dir.path().join("state.json");
        {
            let _hold = hold(&db);
            assert!(is_held(&state));
            write(&state, "held").unwrap();
            assert!(!state.exists(), "a held directory must take no write");
        }
        assert!(!is_held(&state));
        write(&state, "free").unwrap();
        assert_eq!(std::fs::read_to_string(&state).unwrap(), "free");
    }

    #[test]
    fn holding_one_directory_never_silences_another() {
        let held_dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let _hold = hold(&held_dir.path().join("t.db"));
        let state = other.path().join("state.json");
        write(&state, "written").unwrap();
        assert_eq!(std::fs::read_to_string(&state).unwrap(), "written");
    }

    #[test]
    fn a_copy_hold_skips_every_state_write_but_the_telemetry_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        std::fs::write(&db, b"x").unwrap();
        thor_core::copy_mode::write_marker(&db, &Default::default()).unwrap();
        assert!(is_copy(&db));
        let _hold = hold_for(&db, false).expect("a copy store is always held");
        let state = dir.path().join("eval-debt-state.json");
        write(&state, "no").unwrap();
        assert!(!state.exists(), "a copy takes no state write");
        let telemetry = thor_core::copy_mode::telemetry_path(&db);
        write(&telemetry, "yes\n").unwrap();
        assert!(telemetry.exists(), "the per-machine telemetry sidecar stays writable");
    }

    #[test]
    fn a_plain_store_is_not_held_unless_asked() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert!(hold_for(&db, false).is_none());
        let _h = hold_for(&db, true).expect("no_record holds");
        assert!(is_held(&dir.path().join("state.json")));
    }
}
