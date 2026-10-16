//! What `doctor` says about a COPY of the memory (see `thor_core::copy_mode`).
//!
//! A copy is read-only and may hold only a selection of the main memory, so
//! three things that would be findings on a main store are not findings here:
//! the items of other projects are missing on purpose, the store's chain is its
//! own (a filtered copy is a derivative with a chain of its own), and notes are
//! saved on the main machine. This module says so in plain words, and warns
//! about the things that ARE wrong on a copy: it has not been refreshed for a
//! while, the last refresh failed, the selection was changed but not applied,
//! or the source of a selective copy sits on this very machine.

use std::path::Path;
use thor_core::copy_mode::{self, Marker, DOCTOR_STALE_SECS};

/// "3 hours", "2 days": how long ago, for a person.
fn ago(seconds: i64) -> String {
    let seconds = seconds.max(0);
    match seconds {
        s if s < 90 => "a minute".to_string(),
        s if s < 3600 * 2 => format!("{} minutes", s / 60),
        s if s < 3600 * 48 => format!("{} hours", s / 3600),
        s => format!("{} days", s / 86400),
    }
}

/// How many live items the copy holds, or `None` when it cannot be opened.
fn item_count(db: &Path) -> Option<usize> {
    let store = thor_core::event_store::EventStore::open_read_only(db).ok()?;
    Some(serve::live::live_items(&store).len())
}

/// Whether `repo` is a git clone on this machine that holds the segments: the
/// whole memory would then sit on this machine whatever the copy was built with.
fn source_is_a_local_clone(marker: &Marker) -> bool {
    let Some(repo) = marker.repo.as_deref() else { return false };
    let path = Path::new(repo);
    path.join(".git").exists() && path.join(marker.subdir.as_deref().unwrap_or("thor2")).is_dir()
}

/// The one-line summary, or `None` when the store is not a copy.
pub fn summary_line(db: &Path, now: i64) -> Option<String> {
    if !copy_mode::is_copy(db) {
        return None;
    }
    let marker = copy_mode::read_marker(db);
    let selection = marker.selection().describe();
    let last = match marker.checked_at_unix.max(marker.pulled_at_unix) {
        0 => "never".to_string(),
        at => format!("{} ago", ago(now - at)),
    };
    let items = item_count(db).map(|n| n.to_string()).unwrap_or_else(|| "an unknown number of".to_string());
    Some(format!("this is a copy, selection: {selection}, last pull: {last}, {items} items"))
}

/// The lines for the detailed report. Empty when the store is not a copy.
pub fn lines(db: &Path, now: i64) -> Vec<String> {
    let Some(summary) = summary_line(db, now) else { return Vec::new() };
    let marker = copy_mode::read_marker(db);
    let mut out = vec![format!("copy: {summary}")];
    out.push(
        "copy: read-only on purpose. Notes are saved on the main machine and appear here after the next pull. Items of projects that were not selected are absent on purpose; that is not a problem, and this copy's hash chain is its own."
            .to_string(),
    );
    let last_ok = marker.checked_at_unix.max(marker.pulled_at_unix);
    if marker.repo.is_none() {
        out.push("copy: WARNING no backup source is recorded, so this copy cannot refresh. Run: sync pull --db <this store> --repo <the backup>".to_string());
    } else if last_ok == 0 {
        out.push("copy: WARNING no pull has succeeded yet. Run: sync pull --db <this store> and read what it says.".to_string());
    } else if now - last_ok > DOCTOR_STALE_SECS {
        out.push(format!(
            "copy: WARNING the last successful pull was {} ago (more than {} days), so this copy may be out of date. Run: sync pull --db <this store>",
            ago(now - last_ok),
            DOCTOR_STALE_SECS / 86400
        ));
    }
    if !marker.last_error.is_empty() {
        out.push(format!("copy: the last refresh failed ({}); the older copy is still in use.", marker.last_error));
    }
    if marker.built_selection != marker.selection().canonical() && last_ok != 0 {
        out.push(
            "copy: the selection was changed but the copy still holds the old one. It changes at the next successful pull (run: sync pull --db <this store>)."
                .to_string(),
        );
    }
    if marker.selection().is_active() && source_is_a_local_clone(&marker) {
        out.push(format!(
            "copy: NOTE the backup source {} is a git clone on this machine, and it holds the COMPLETE memory. The selection keeps other projects out of the copy, not off this disk. For a machine that must not hold them, give sync pull the backup's address (or a bare repository) instead.",
            marker.repo.as_deref().unwrap_or_default()
        ));
    }
    let leftover = {
        let mut name = db.as_os_str().to_owned();
        name.push(".source");
        std::path::PathBuf::from(name)
    };
    if leftover.exists() {
        out.push(format!(
            "copy: NOTE {} is left over from a pull that did not finish. It holds the complete memory; delete the folder.",
            leftover.display()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pull::{pull, PullOptions};
    use model::item::{Binding, Item, Kind};
    use thor_core::backup::export_segments;
    use thor_core::event_store::EventStore;

    fn rule(id: &str, project: Option<&str>, words: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("{id} {words}"),
            bindings: vec![Binding::Always],
            severity: None,
            project: project.map(str::to_string),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("it stops being true".to_string()),
            check: None,
        }
    }

    /// A selective copy of two of three items, from a plain backup folder.
    fn copy() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let mut main = EventStore::new(&dir.path().join("main.db")).unwrap();
        for item in [
            rule("alpha-one", Some("alpha"), "guards the alpha pipeline every single release"),
            rule("beta-one", Some("beta"), "describes the beta customer contract in detail"),
            rule("general-one", None, "never commit credentials into any repository whatsoever"),
        ] {
            model::store::declare(&mut main, "s", "s", "s", &item).unwrap();
        }
        let repo = dir.path().join("backup");
        std::fs::create_dir_all(repo.join("thor2")).unwrap();
        export_segments(&main, &repo.join("thor2"), 2_000).unwrap();
        std::fs::create_dir_all(dir.path().join("laptop")).unwrap();
        let db = dir.path().join("laptop").join("thor.db");
        let options = PullOptions { repo: Some(repo), no_fetch: true, scopes: vec!["alpha".into()], include_global: true, ..Default::default() };
        pull(&db, &options).unwrap();
        (dir, db)
    }

    #[test]
    fn a_main_store_gets_no_copy_lines() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        EventStore::new(&db).unwrap();
        assert!(lines(&db, copy_mode::now_unix()).is_empty());
        assert_eq!(summary_line(&db, copy_mode::now_unix()), None);
    }

    #[test]
    fn a_fresh_selective_copy_says_what_it_is_and_reports_no_problem() {
        let (_dir, db) = copy();
        let now = copy_mode::now_unix();
        let report = lines(&db, now);
        let first = &report[0];
        assert!(first.contains("this is a copy"), "{first}");
        assert!(first.contains("selection: project alpha, general rules (no project)"), "{first}");
        assert!(first.contains("last pull: a minute ago"), "{first}");
        assert!(first.contains("2 items"), "{first}");
        assert!(report.iter().all(|l| !l.contains("WARNING")), "{report:?}");
        assert!(report.iter().any(|l| l.contains("absent on purpose")));
        assert!(!report.join("\n").contains("beta"), "the report names no excluded project");
        assert!(crate::health::store_line(&db).contains("chain intact"));
    }

    #[test]
    fn an_old_pull_is_warned_about_and_a_failed_one_is_named() {
        let (_dir, db) = copy();
        let now = copy_mode::now_unix();
        let report = lines(&db, now + 5 * 86400);
        assert!(report.iter().any(|l| l.contains("WARNING") && l.contains("5 days")), "{report:?}");
        let mut marker = copy_mode::read_marker(&db);
        marker.last_error = "git fetch timed out".to_string();
        copy_mode::write_marker(&db, &marker).unwrap();
        assert!(lines(&db, now).iter().any(|l| l.contains("git fetch timed out") && l.contains("older copy is still in use")));
    }

    #[test]
    fn a_changed_selection_that_is_not_applied_yet_is_said() {
        let (_dir, db) = copy();
        let mut marker = copy_mode::read_marker(&db);
        marker.scopes = vec!["alpha".into(), "beta".into()];
        copy_mode::write_marker(&db, &marker).unwrap();
        assert!(lines(&db, copy_mode::now_unix()).iter().any(|l| l.contains("selection was changed")));
    }

    #[test]
    fn a_selective_copy_fed_from_a_local_clone_is_told_the_whole_memory_is_on_this_disk() {
        let (dir, db) = copy();
        let repo = dir.path().join("backup");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert!(lines(&db, copy_mode::now_unix()).iter().any(|l| l.contains("COMPLETE memory")));
    }

    #[test]
    fn a_whole_memory_copy_says_so() {
        let (dir, db) = copy();
        let options = PullOptions { repo: Some(dir.path().join("backup")), no_fetch: true, whole: true, ..Default::default() };
        pull(&db, &options).unwrap();
        let first = lines(&db, copy_mode::now_unix()).remove(0);
        assert!(first.contains("selection: the whole memory"), "{first}");
        assert!(first.contains("3 items"), "{first}");
    }
}
