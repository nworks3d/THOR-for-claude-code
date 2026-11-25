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

/// Write a state file beside the store - skipped, successfully, while that
/// directory is held. ATOMIC: the text goes to a temporary file in the same
/// folder and is then renamed over the target, so a reader (or a writer that
/// loses the race) never sees half a file or one with extra text behind it.
/// The rename is retried for a moment when the target is held open.
pub fn write(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    if is_held(path) {
        return Ok(());
    }
    write_atomic(path, contents.as_ref())
}

fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("state");
    let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = path.with_file_name(format!(".{name}.{}.{unique}.tmp", std::process::id()));
    std::fs::write(&temp, contents)?;
    let mut last = Ok(());
    for _ in 0..40 {
        match std::fs::rename(&temp, path) {
            Ok(()) => return Ok(()),
            Err(e) => last = Err(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let _ = std::fs::remove_file(&temp);
    last
}

/// How long a change waits for a state file's lock before it goes on without.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(1);
/// A lock older than this was left by a hook that died.
const LOCK_STALE: std::time::Duration = std::time::Duration::from_secs(10);

/// A lock file, removed when dropped. `None` inside means none was taken: the
/// directory is held, or the lock stayed taken past the wait.
pub struct FileLock(Option<PathBuf>);

impl Drop for FileLock {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The lock that guards a read-modify-write of one state file: `lock_path` is
/// a file beside it (`lock_path_of`). Waits up to a second for it, then goes
/// on without: a hook that is slow or a lock that is stuck must never hold a
/// tool call or a turn end.
pub fn lock(lock_path: &Path) -> FileLock {
    if is_held(lock_path) {
        return FileLock(None);
    }
    let started = std::time::Instant::now();
    loop {
        if std::fs::OpenOptions::new().write(true).create_new(true).open(lock_path).is_ok() {
            return FileLock(Some(lock_path.to_path_buf()));
        }
        let stale = std::fs::metadata(lock_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age > LOCK_STALE);
        if stale {
            let _ = std::fs::remove_file(lock_path);
            continue;
        }
        if started.elapsed() > LOCK_WAIT {
            return FileLock(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// The lock file of the state file `state`: the same name with `.lock` added.
pub fn lock_path_of(state: &Path) -> PathBuf {
    let name = state.file_name().and_then(|n| n.to_str()).unwrap_or("state");
    state.with_file_name(format!("{name}.lock"))
}

/// Read a state file that holds JSON for a read-modify-write. A file that is
/// not there is `Some(None)` (start from nothing); a file that is there but
/// cannot be read or parsed is `None`: the caller keeps the file and skips its
/// update, never writing an empty state over it.
pub fn read_json_for_update<T: serde::de::DeserializeOwned>(path: &Path) -> Option<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(text.trim_start_matches('\u{feff}')).ok().map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(None),
        Err(_) => None,
    }
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
    fn many_threads_writing_one_state_file_always_leave_one_whole_value() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state.json");
        let handles: Vec<_> = (0..10)
            .map(|n| {
                let state = state.clone();
                std::thread::spawn(move || {
                    for round in 0..20 {
                        write(&state, format!("[\"{n}-{round}\"]{}", " ".repeat(n * 700))).unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let text = std::fs::read_to_string(&state).unwrap();
        let parsed: Vec<String> = serde_json::from_str(&text).expect("whole JSON, nothing left over from a longer write");
        assert_eq!(parsed.len(), 1);
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 1, "no temporary file is left behind: {names:?}");
    }

    #[test]
    fn a_state_file_that_cannot_be_parsed_is_told_apart_from_one_that_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state.json");
        assert_eq!(read_json_for_update::<Vec<String>>(&state), Some(None));
        std::fs::write(&state, "[\"a\"] and more").unwrap();
        assert_eq!(read_json_for_update::<Vec<String>>(&state), None);
        std::fs::write(&state, "[\"a\"]").unwrap();
        assert_eq!(read_json_for_update::<Vec<String>>(&state), Some(Some(vec!["a".to_string()])));
    }

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
