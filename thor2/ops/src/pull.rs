//! `sync pull`: refresh a COPY of the memory (see `thor_core::copy_mode`) from
//! the backup's git clone, without touching the main machine or the NAS.
//!
//! The source is the backup the main machine already keeps: a git repository
//! holding the event log as `<subdir>/events-NNNN.jsonl`, one JSON object per
//! line in exactly the shipped form (`ShippedEvent`). The copy machine keeps a
//! clone of it, fetches, and applies only the events its own chain is missing
//! through `EventStore::ingest_batch` - the same checked path the replica
//! receiver uses, so every event must continue the copy's hash chain or the
//! whole batch is refused.
//!
//! SAFE BY CONSTRUCTION
//! - The copy is never edited in place: the pull works on a staged copy of the
//!   store file (`<db>.pulling`) and swaps it in with two renames only when
//!   the staged chain tip equals the source's last event, hash included.
//! - The swap is refused (the old copy stays) if anything holds the live copy
//!   open, if a stale `-wal` sits beside it, or if a rename fails.
//! - A store that exists and carries no copy marker is NEVER replaced: the
//!   pull cannot be pointed at a main store by mistake.
//! - Every failure is recorded in the marker (`last_error`) and leaves the old
//!   copy in use: fail open, one line.

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use thor_core::backup::segment_files;
use thor_core::copy_mode::{self, Marker};
use thor_core::event_store::{EventStore, IngestOutcome, ShippedEvent};

/// Events applied per atomic batch.
const CHUNK: usize = 2000;
/// A staged copy younger than this means another pull is still running.
const STAGE_FRESH_SECS: u64 = 600;
/// How long `git` may take before the pull gives up and keeps the old copy.
const GIT_TIMEOUT: Duration = Duration::from_secs(90);

/// What to pull from, and how.
#[derive(Debug, Clone, Default)]
pub struct PullOptions {
    /// The git clone of the backup. Default: the marker's `repo`.
    pub repo: Option<PathBuf>,
    /// The folder in the clone holding the segments. Default: marker, else `thor2`.
    pub subdir: Option<String>,
    /// The remote to fetch from. Default: the clone's own upstream.
    pub remote: Option<String>,
    /// Skip `git` entirely and read the clone as it is (tests, offline).
    pub no_fetch: bool,
    /// Rebuild the copy from the first event instead of continuing it.
    pub rebuild: bool,
}

#[derive(Debug, PartialEq)]
pub enum PullOutcome {
    /// The copy already holds everything the source has.
    UpToDate { tip: i64 },
    /// A fresher copy was swapped in.
    Updated { from_tip: i64, to_tip: i64, applied: usize },
}

fn side(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn remove_store_files(db: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(side(db, suffix));
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn().map_err(|e| anyhow::anyhow!("git could not be started ({e})"))?;
    let started = Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) if status.success() => {
                let mut out = String::new();
                if let Some(mut o) = child.stdout.take() {
                    let _ = o.read_to_string(&mut out);
                }
                return Ok(out.trim().to_string());
            }
            Some(_) => {
                let mut why = String::new();
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_string(&mut why);
                }
                let why = why.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no reason given").trim().to_string();
                bail!("git {} failed: {why}", args.first().copied().unwrap_or(""));
            }
            None if started.elapsed() > GIT_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("git {} took longer than {} seconds", args.first().copied().unwrap_or(""), GIT_TIMEOUT.as_secs());
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// The seq on the last line of a segment, read from its tail.
fn last_seq_of(path: &Path) -> Result<i64> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut window: u64 = 64 * 1024;
    loop {
        let start = len.saturating_sub(window);
        file.seek(SeekFrom::Start(start))?;
        let mut tail = Vec::new();
        file.read_to_end(&mut tail)?;
        let text = String::from_utf8_lossy(&tail);
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        // A window that starts mid-line has a broken first line: only trust the
        // last line when it is the whole file or a later line.
        if let Some(last) = lines.next_back() {
            if let Ok(e) = serde_json::from_str::<ShippedEvent>(last) {
                return Ok(e.seq);
            }
        }
        if start == 0 {
            bail!("no readable event in {}", path.display());
        }
        window = window.saturating_mul(4);
    }
}

/// The copy's own tip, or (0, "") for a copy that does not exist yet.
fn copy_tip(db: &Path) -> Result<(i64, String)> {
    if !db.is_file() {
        return Ok((0, String::new()));
    }
    let store = EventStore::open_read_only(db).context("the current copy cannot be opened")?;
    Ok(store.contiguous_tip()?)
}

/// Pull once. Does not touch the marker; `pull` does that around it.
fn pull_inner(db: &Path, repo: &Path, subdir: &str, opts: &PullOptions, marker: &Marker) -> Result<PullOutcome> {
    if !opts.no_fetch {
        match opts.remote.clone().or_else(|| marker.remote.clone()) {
            Some(remote) => {
                let branch = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
                git(repo, &["pull", "--ff-only", "--quiet", &remote, &branch])?;
            }
            None => {
                git(repo, &["pull", "--ff-only", "--quiet"])?;
            }
        }
    }
    let segments = segment_files(&repo.join(subdir)).with_context(|| format!("no backup segments in {}", repo.join(subdir).display()))?;
    if segments.is_empty() {
        bail!("no events-NNNN.jsonl in {}", repo.join(subdir).display());
    }
    let (tip, tip_hash) = if opts.rebuild { (0, String::new()) } else { copy_tip(db)? };
    let source_last = last_seq_of(segments.last().expect("checked non-empty"))?;
    if source_last <= tip && !opts.rebuild {
        return Ok(PullOutcome::UpToDate { tip });
    }

    // Stage: a private copy of the store file, or a fresh store.
    let staged = side(db, ".pulling");
    if let Ok(meta) = std::fs::metadata(&staged) {
        let age = meta.modified().ok().and_then(|m| m.elapsed().ok()).map(|d| d.as_secs()).unwrap_or(u64::MAX);
        if age < STAGE_FRESH_SECS {
            bail!("another pull is still running (a staged copy is {age} seconds old)");
        }
    }
    remove_store_files(&staged);
    let result = stage_and_swap(db, &staged, &segments, tip, &tip_hash, opts.rebuild);
    remove_store_files(&staged);
    result
}

fn stage_and_swap(db: &Path, staged: &Path, segments: &[PathBuf], tip: i64, tip_hash: &str, rebuild: bool) -> Result<PullOutcome> {
    if db.is_file() && !rebuild {
        std::fs::copy(db, staged).context("the current copy cannot be copied to stage the pull")?;
    }
    let mut applied = 0usize;
    let (last_seq, _) = {
        let mut store = EventStore::new(staged).context("the staged copy cannot be created")?;
        let mut batch: Vec<ShippedEvent> = Vec::with_capacity(CHUNK);
        let mut last: (i64, String) = (tip, tip_hash.to_string());
        let flush = |store: &mut EventStore, batch: &mut Vec<ShippedEvent>, applied: &mut usize| -> Result<()> {
            if batch.is_empty() {
                return Ok(());
            }
            match store.ingest_batch(batch)? {
                IngestOutcome::Applied { applied: n, .. } => *applied += n,
                IngestOutcome::Rejected { reason, at_seq, .. } => {
                    bail!("the source does not continue this copy (seq {at_seq}): {reason}")
                }
            }
            batch.clear();
            Ok(())
        };
        for file in segments {
            // A whole segment that ends before the copy's tip has nothing new.
            if tip > 0 && last_seq_of(file)? < tip {
                continue;
            }
            let reader = BufReader::new(std::fs::File::open(file).with_context(|| format!("cannot read {}", file.display()))?);
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let event: ShippedEvent = serde_json::from_str(&line).with_context(|| format!("unreadable line in {}", file.display()))?;
                // The event AT the tip is sent too: the receiver checks its hash,
                // so a copy that has forked from the source is refused.
                if event.seq < tip {
                    continue;
                }
                last = (event.seq, event.this_hash.clone());
                batch.push(event);
                if batch.len() >= CHUNK {
                    flush(&mut store, &mut batch, &mut applied)?;
                }
            }
        }
        flush(&mut store, &mut batch, &mut applied)?;
        let (staged_seq, staged_hash) = store.contiguous_tip()?;
        if (staged_seq, staged_hash.as_str()) != (last.0, last.1.as_str()) {
            bail!("the staged copy ends at seq {staged_seq}, not at the source's last event {}", last.0);
        }
        last
    };
    if applied == 0 {
        return Ok(PullOutcome::UpToDate { tip });
    }
    // The staged store is closed above, which folds its log into the file; a
    // companion left behind would belong to the wrong file after the swap.
    let wal = side(staged, "-wal");
    if std::fs::metadata(&wal).map(|m| m.len() > 0).unwrap_or(false) {
        bail!("the staged copy left an unmerged write-ahead log; keeping the old copy");
    }
    swap_in(db, staged)?;
    Ok(PullOutcome::Updated { from_tip: tip, to_tip: last_seq, applied })
}

/// Replace the live copy with the staged one, or change nothing.
fn swap_in(db: &Path, staged: &Path) -> Result<()> {
    if !db.exists() {
        std::fs::rename(staged, db).context("the new copy could not be put in place")?;
        let _ = std::fs::remove_file(side(staged, "-wal"));
        let _ = std::fs::remove_file(side(staged, "-shm"));
        return Ok(());
    }
    // A write-ahead log beside the live copy may hold data its file does not.
    if std::fs::metadata(side(db, "-wal")).map(|m| m.len() > 0).unwrap_or(false) {
        bail!("the current copy has a write-ahead log beside it; leaving it alone");
    }
    let old = side(db, ".old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(db, &old).context("the current copy is in use; keeping it")?;
    if let Err(e) = std::fs::rename(staged, db) {
        let _ = std::fs::rename(&old, db);
        bail!("the new copy could not be put in place ({e}); the old copy is back");
    }
    let _ = std::fs::remove_file(side(db, "-wal"));
    let _ = std::fs::remove_file(side(db, "-shm"));
    let _ = std::fs::remove_file(&old);
    Ok(())
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(300).collect()
}

/// Refresh the copy at `db`. Records the attempt, the new tip and any failure
/// in the copy's marker. On failure the old copy stays in use.
pub fn pull(db: &Path, opts: &PullOptions) -> Result<PullOutcome> {
    let mut marker = copy_mode::read_marker(db);
    let marked = copy_mode::marker_path(db).is_file();
    if db.is_file() && !marked {
        bail!(
            "{} is a store that is not marked as a copy (no {} beside it); a pull replaces the store, so it will not touch it",
            db.display(),
            copy_mode::MARKER
        );
    }
    let repo = opts
        .repo
        .clone()
        .or_else(|| marker.repo.clone().map(PathBuf::from))
        .context("no backup clone named: give --repo once, or put \"repo\" in the copy's marker")?;
    let subdir = opts.subdir.clone().or_else(|| marker.subdir.clone()).unwrap_or_else(|| "thor2".to_string());
    // Marked BEFORE the store exists, so no hook ever sees an unmarked copy.
    marker.repo = Some(repo.display().to_string());
    marker.subdir = Some(subdir.clone());
    if opts.remote.is_some() {
        marker.remote = opts.remote.clone();
    }
    marker.last_attempt_unix = copy_mode::now_unix();
    copy_mode::write_marker(db, &marker).context("the copy marker could not be written")?;

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pull_inner(db, &repo, &subdir, opts, &marker)))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("the pull panicked")));
    match &outcome {
        Ok(result) => {
            marker.last_error.clear();
            match result {
                PullOutcome::Updated { to_tip, .. } => {
                    marker.pulled_at_unix = copy_mode::now_unix();
                    marker.tip_seq = *to_tip;
                }
                PullOutcome::UpToDate { tip } => marker.tip_seq = *tip,
            }
        }
        Err(e) => marker.last_error = one_line(&format!("{e:#}")),
    }
    let _ = copy_mode::write_marker(db, &marker);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Item, Kind};
    use thor_core::backup::export_segments;

    fn rule(id: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("rule {id} says something worth keeping"),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("it stops being true".to_string()),
            check: None,
        }
    }

    struct World {
        _dir: tempfile::TempDir,
        main: EventStore,
        repo: PathBuf,
        copy: PathBuf,
    }

    /// A main store exported as small segments into a fake backup clone, and a
    /// path where the copy will live.
    fn world(events: usize) -> World {
        let dir = tempfile::tempdir().unwrap();
        let mut main = EventStore::new(&dir.path().join("main.db")).unwrap();
        for i in 0..events {
            model::store::declare(&mut main, "s", "s", "s", &rule(&format!("r{i}"))).unwrap();
        }
        let repo = dir.path().join("clone");
        std::fs::create_dir_all(repo.join("thor2")).unwrap();
        export_segments(&main, &repo.join("thor2"), 2_000).unwrap();
        let copy_dir = dir.path().join("laptop");
        std::fs::create_dir_all(&copy_dir).unwrap();
        World { _dir: dir, main, repo, copy: copy_dir.join("thor.db") }
    }

    fn opts(w: &World) -> PullOptions {
        PullOptions { repo: Some(w.repo.clone()), no_fetch: true, ..Default::default() }
    }

    fn tip_of(db: &Path) -> (i64, String) {
        EventStore::open_read_only(db).unwrap().contiguous_tip().unwrap()
    }

    #[test]
    fn the_first_pull_builds_a_marked_copy_with_the_sources_tip() {
        let w = world(12);
        let outcome = pull(&w.copy, &opts(&w)).unwrap();
        assert!(matches!(outcome, PullOutcome::Updated { from_tip: 0, .. }), "{outcome:?}");
        assert_eq!(tip_of(&w.copy), w.main.contiguous_tip().unwrap());
        assert!(copy_mode::is_copy(&w.copy), "the pull marks what it builds");
        let marker = copy_mode::read_marker(&w.copy);
        assert_eq!(marker.tip_seq, w.main.contiguous_tip().unwrap().0);
        assert_eq!(marker.last_error, "");
        assert!(!side(&w.copy, ".pulling").exists() && !side(&w.copy, ".old").exists(), "no staging leftovers");
    }

    #[test]
    fn a_later_pull_applies_only_what_is_new_and_swaps_atomically() {
        let mut w = world(12);
        pull(&w.copy, &opts(&w)).unwrap();
        let before = tip_of(&w.copy).0;
        for i in 100..110 {
            model::store::declare(&mut w.main, "s", "s", "s", &rule(&format!("r{i}"))).unwrap();
        }
        export_segments(&w.main, &w.repo.join("thor2"), 2_000).unwrap();
        let outcome = pull(&w.copy, &opts(&w)).unwrap();
        match outcome {
            PullOutcome::Updated { from_tip, to_tip, applied } => {
                assert_eq!(from_tip, before);
                assert_eq!(to_tip, w.main.contiguous_tip().unwrap().0);
                assert_eq!(applied as i64, to_tip - from_tip, "only the new events were applied");
            }
            other => panic!("expected an update, got {other:?}"),
        }
        assert_eq!(tip_of(&w.copy), w.main.contiguous_tip().unwrap());
        assert!(!side(&w.copy, ".pulling").exists() && !side(&w.copy, ".old").exists());
    }

    #[test]
    fn a_pull_with_nothing_new_leaves_the_copy_file_untouched() {
        let w = world(6);
        pull(&w.copy, &opts(&w)).unwrap();
        let bytes = std::fs::read(&w.copy).unwrap();
        assert!(matches!(pull(&w.copy, &opts(&w)).unwrap(), PullOutcome::UpToDate { .. }));
        assert!(bytes == std::fs::read(&w.copy).unwrap());
    }

    #[test]
    fn a_failed_pull_keeps_the_old_copy_byte_for_byte_and_says_why() {
        let mut w = world(8);
        pull(&w.copy, &opts(&w)).unwrap();
        let bytes = std::fs::read(&w.copy).unwrap();
        for i in 200..205 {
            model::store::declare(&mut w.main, "s", "s", "s", &rule(&format!("r{i}"))).unwrap();
        }
        export_segments(&w.main, &w.repo.join("thor2"), 2_000).unwrap();
        // Damage the newest segment: one body no longer matches its recorded hash.
        let last = segment_files(&w.repo.join("thor2")).unwrap().pop().unwrap();
        let text = std::fs::read_to_string(&last).unwrap();
        let broken = text.replacen("says something worth keeping", "says something else entirely", 1);
        assert_ne!(text, broken, "fixture sanity: a body was changed");
        std::fs::write(&last, broken).unwrap();

        let result = pull(&w.copy, &opts(&w));
        assert!(result.is_err(), "a source that does not verify is refused");
        assert!(bytes == std::fs::read(&w.copy).unwrap(), "the old copy is exactly as it was");
        assert!(!side(&w.copy, ".pulling").exists(), "the staged copy is removed");
        let marker = copy_mode::read_marker(&w.copy);
        assert!(!marker.last_error.is_empty(), "the failure is recorded in the marker");
        assert!(!marker.last_error.contains('\n'), "and is one line");
        assert!(copy_mode::session_line(&marker).contains("older copy is still in use"));
    }

    #[test]
    fn a_missing_clone_fails_open_and_keeps_the_copy() {
        let w = world(5);
        pull(&w.copy, &opts(&w)).unwrap();
        let bytes = std::fs::read(&w.copy).unwrap();
        let gone = PullOptions { repo: Some(w.repo.join("nope")), no_fetch: true, ..Default::default() };
        assert!(pull(&w.copy, &gone).is_err());
        assert!(bytes == std::fs::read(&w.copy).unwrap());
    }

    #[test]
    fn a_store_without_a_marker_is_never_replaced() {
        let w = world(5);
        // A "main" store sitting where the copy would go.
        EventStore::new(&w.copy).unwrap();
        let bytes = std::fs::read(&w.copy).unwrap();
        let result = pull(&w.copy, &opts(&w));
        assert!(result.unwrap_err().to_string().contains("not marked as a copy"));
        assert!(bytes == std::fs::read(&w.copy).unwrap());
        assert!(!copy_mode::marker_path(&w.copy).exists(), "and it is not marked behind the owner's back");
    }

    #[test]
    fn a_copy_that_forked_from_the_source_is_refused() {
        let w = world(6);
        pull(&w.copy, &opts(&w)).unwrap();
        // A different history in the clone: same length, other content.
        let other = tempfile::tempdir().unwrap();
        let mut main2 = EventStore::new(&other.path().join("m.db")).unwrap();
        for i in 0..9 {
            model::store::declare(&mut main2, "s", "s", "s", &rule(&format!("other{i}"))).unwrap();
        }
        for f in segment_files(&w.repo.join("thor2")).unwrap() {
            std::fs::remove_file(f).unwrap();
        }
        export_segments(&main2, &w.repo.join("thor2"), 2_000).unwrap();
        let bytes = std::fs::read(&w.copy).unwrap();
        let err = pull(&w.copy, &opts(&w)).unwrap_err().to_string();
        assert!(err.contains("fork") || err.contains("does not continue"), "{err}");
        assert!(bytes == std::fs::read(&w.copy).unwrap());
    }

    #[test]
    fn a_fresh_staged_copy_means_another_pull_is_running() {
        let w = world(5);
        pull(&w.copy, &opts(&w)).unwrap();
        std::fs::write(side(&w.copy, ".pulling"), b"busy").unwrap();
        let mut w = w;
        model::store::declare(&mut w.main, "s", "s", "s", &rule("late")).unwrap();
        export_segments(&w.main, &w.repo.join("thor2"), 2_000).unwrap();
        assert!(pull(&w.copy, &opts(&w)).unwrap_err().to_string().contains("another pull"));
    }


    fn run_git(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// The real path: a clone, `git pull`, and only the new events applied.
    #[test]
    fn a_pull_fetches_the_clone_with_git_first() {
        let mut w = world(10);
        // The "backup" repository is `w.repo`; make it a git repo and clone it.
        if !run_git(&w.repo, &["init", "-q", "-b", "main"]) {
            return; // no git on this machine: the rest of the suite covers the pull itself
        }
        assert!(run_git(&w.repo, &["add", "-A"]) && run_git(&w.repo, &["commit", "-q", "-m", "backup one"]));
        let clone = w._dir.path().join("laptop-clone");
        let status = Command::new("git").arg("clone").arg("-q").arg(&w.repo).arg(&clone).status().unwrap();
        assert!(status.success());
        let options = PullOptions { repo: Some(clone.clone()), ..Default::default() };
        pull(&w.copy, &options).unwrap();
        assert_eq!(tip_of(&w.copy), w.main.contiguous_tip().unwrap());

        for i in 300..306 {
            model::store::declare(&mut w.main, "s", "s", "s", &rule(&format!("r{i}"))).unwrap();
        }
        export_segments(&w.main, &w.repo.join("thor2"), 2_000).unwrap();
        assert!(run_git(&w.repo, &["add", "-A"]) && run_git(&w.repo, &["commit", "-q", "-m", "backup two"]));
        let outcome = pull(&w.copy, &options).unwrap();
        assert!(matches!(outcome, PullOutcome::Updated { applied: 6, .. }), "{outcome:?}");
        assert_eq!(tip_of(&w.copy), w.main.contiguous_tip().unwrap());
    }
    #[test]
    fn the_last_seq_is_read_from_the_tail() {
        let w = world(30);
        let last = segment_files(&w.repo.join("thor2")).unwrap().pop().unwrap();
        assert_eq!(last_seq_of(&last).unwrap(), w.main.contiguous_tip().unwrap().0);
    }
}
