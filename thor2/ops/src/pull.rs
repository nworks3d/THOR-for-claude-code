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
//!
//! SELECTIVE COPY. When the marker holds a selection (`scopes`,
//! `include_global`), the copy is not the whole log but a filtered derivative
//! with its own hash chain (see `copy_filter`): the events of every other
//! project, the personal library and the session text in refusals never reach
//! the copy file. Such a copy is built again from the source whenever the
//! source moves (its chain differs from the source's, so it cannot be
//! continued), always on a staged file that replaces the old copy in one
//! swap. The source is read in place (a plain folder) or, when it is an
//! address or a bare repository, cloned into a scratch folder that is deleted
//! when the pull ends, so no copy of the whole memory stays on this machine.

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use thor_core::backup::segment_files;
use thor_core::copy_mode::{self, Marker, Selection};
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
    /// SELECTIVE COPY: the project scopes to hold. Replaces the remembered selection.
    pub scopes: Vec<String>,
    /// SELECTIVE COPY: also hold the items that belong to no project.
    pub include_global: bool,
    /// Hold the whole memory again (clears the remembered selection).
    pub whole: bool,
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
                // The first "fatal:" line says what went wrong; the lines after it are advice.
                let first_fatal = why.lines().find_map(|l| l.trim().strip_prefix("fatal:").map(|r| r.trim().to_string()));
                let why = first_fatal
                    .unwrap_or_else(|| why.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no reason given").trim().to_string());
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
    Ok(last_event_of(path)?.seq)
}

/// The last event of a segment, read from its tail.
fn last_event_of(path: &Path) -> Result<ShippedEvent> {
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
                return Ok(e);
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

/// Pull once. Does not write the marker file; `pull` does that around it. On
/// success it records in `marker` what the copy was built from.
fn pull_inner(db: &Path, repo: &Path, subdir: &str, opts: &PullOptions, marker: &mut Marker, selection: &Selection) -> Result<PullOutcome> {
    if selection.is_active() {
        return pull_selective(db, repo, subdir, opts, marker, selection);
    }
    pull_whole(db, repo, subdir, opts, marker)
}

/// The whole-memory copy: continue the copy's chain with what the source added.
fn pull_whole(db: &Path, repo: &Path, subdir: &str, opts: &PullOptions, marker: &mut Marker) -> Result<PullOutcome> {
    // A copy that was selective has a chain of its own: it cannot be continued.
    let rebuild = opts.rebuild || !marker.built_selection.is_empty();
    let whole = Selection::default();
    let Some(source) = open_source(db, repo, subdir, opts, marker, &whole)? else {
        return Ok(PullOutcome::UpToDate { tip: if db.is_file() { copy_tip(db)?.0 } else { 0 } });
    };
    let dir = source.root.join(subdir);
    install_rulebook(db, &dir);
    let segments = segment_files(&dir).with_context(|| format!("no backup segments in {}", dir.display()))?;
    if segments.is_empty() {
        bail!("no events-NNNN.jsonl in {}", dir.display());
    }
    let (tip, tip_hash) = if rebuild { (0, String::new()) } else { copy_tip(db)? };
    let source_event = last_event_of(segments.last().expect("checked non-empty"))?;
    let source_last = source_event.seq;
    if source_last <= tip && !rebuild {
        marker.source_seq = source_last;
        marker.source_hash = source_event.this_hash;
        marker.source_commit = source.commit.clone();
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
    let result = stage_and_swap(db, &staged, &segments, tip, &tip_hash, rebuild);
    remove_store_files(&staged);
    if result.is_ok() {
        marker.built_selection.clear();
        marker.source_seq = source_last;
        marker.source_hash = source_event.this_hash;
        marker.source_commit = source.commit.clone();
    }
    result
}

/// What a pull reads the source from.
struct Source {
    /// The folder that holds `<subdir>`.
    root: PathBuf,
    /// The backup's commit when it was fetched from an address.
    commit: String,
    /// A scratch clone to delete when the pull ends.
    scratch: Option<PathBuf>,
}

impl Drop for Source {
    fn drop(&mut self) {
        if let Some(scratch) = &self.scratch {
            remove_tree(scratch);
        }
    }
}

/// Delete a folder tree, including the read-only files git leaves in it.
#[allow(clippy::permissions_set_readonly_false)]
fn remove_tree(path: &Path) {
    fn writable(path: &Path) {
        let Ok(entries) = std::fs::read_dir(path) else { return };
        for entry in entries.flatten() {
            let child = entry.path();
            if let Ok(meta) = entry.metadata() {
                let mut permissions = meta.permissions();
                permissions.set_readonly(false);
                let _ = std::fs::set_permissions(&child, permissions);
            }
            if child.is_dir() {
                writable(&child);
            }
        }
    }
    if std::fs::remove_dir_all(path).is_ok() {
        return;
    }
    writable(path);
    let _ = std::fs::remove_dir_all(path);
}

/// The scratch folder a pull clones an address into, beside the copy.
fn scratch_path(db: &Path) -> PathBuf {
    side(db, ".source")
}

/// Find the source of a SELECTIVE pull. A git clone is pulled (as for a whole
/// copy); a plain folder is read as it is; anything else (an address, a bare
/// repository) is cloned shallow into a scratch folder, unless its commit is
/// the one the copy was built from already (`Ok(None)`).
fn open_source(db: &Path, repo: &Path, subdir: &str, opts: &PullOptions, marker: &Marker, selection: &Selection) -> Result<Option<Source>> {
    let is_clone = repo.join(".git").exists();
    if is_clone || repo.join(subdir).is_dir() {
        if is_clone && !opts.no_fetch {
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
        return Ok(Some(Source { root: repo.to_path_buf(), commit: String::new(), scratch: None }));
    }
    if opts.no_fetch {
        bail!("{} is not a folder with backup segments, and git is not allowed to fetch it", repo.display());
    }
    let address = repo.display().to_string();
    let home = std::path::absolute(db).unwrap_or_else(|_| db.to_path_buf());
    let home = home.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let remote_head =
        git(&home, &["ls-remote", &address, "HEAD"]).with_context(|| format!("the backup at {address} could not be reached"))?;
    let commit = remote_head.split_whitespace().next().unwrap_or_default().to_string();
    if !commit.is_empty()
        && !opts.rebuild
        && db.is_file()
        && marker.source_commit == commit
        && marker.built_selection == selection.canonical()
    {
        return Ok(None);
    }
    let scratch = scratch_path(&home.join(db.file_name().unwrap_or_default()));
    remove_tree(&scratch);
    // From here on the guard deletes the scratch folder, however the pull ends.
    let source = Source { root: scratch.clone(), commit, scratch: Some(scratch.clone()) };
    git(&home, &["clone", "--quiet", "--depth", "1", &address, &scratch.display().to_string()])?;
    Ok(Some(source))
}

/// Feed every source event, in order, to `visit`, checking as it goes that the
/// source is the one unbroken hash chain it claims to be.
fn for_each_event(segments: &[PathBuf], mut visit: impl FnMut(&ShippedEvent) -> Result<()>) -> Result<()> {
    let mut expected = 1i64;
    let mut prev_hash = String::new();
    for file in segments {
        let reader = BufReader::new(std::fs::File::open(file).with_context(|| format!("cannot read {}", file.display()))?);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let event: ShippedEvent = serde_json::from_str(&line).with_context(|| format!("unreadable line in {}", file.display()))?;
            if event.seq != expected {
                bail!("the source does not run in order: expected seq {expected}, found {}", event.seq);
            }
            let kind = thor_core::event_store::EventKind::from_str(&event.kind)
                .with_context(|| format!("unknown event kind '{}' at seq {} (this program is older than the source)", event.kind, event.seq))?;
            let canonical = thor_core::event_store::canonical_repr_parts(
                kind,
                &event.entity_id,
                event.parent_rev.as_deref().filter(|p| !p.is_empty()),
                &thor_core::event_store::canonicalize_body(&event.body),
            );
            let hash = thor_core::event_store::hash_event(&prev_hash, &canonical);
            if hash != event.this_hash {
                bail!("the source does not verify at seq {}: its recorded hash does not match its content", event.seq);
            }
            prev_hash = hash;
            expected += 1;
            visit(&event)?;
        }
    }
    Ok(())
}

/// The selective copy: rebuilt from the source whenever the source (or the
/// selection) changed, on a staged file, swapped in at the end.
fn pull_selective(db: &Path, repo: &Path, subdir: &str, opts: &PullOptions, marker: &mut Marker, selection: &Selection) -> Result<PullOutcome> {
    let Some(source) = open_source(db, repo, subdir, opts, marker, selection)? else {
        // The backup's commit is the one the copy was built from.
        return Ok(PullOutcome::UpToDate { tip: if db.is_file() { copy_tip(db)?.0 } else { 0 } });
    };
    let dir = source.root.join(subdir);
    install_rulebook(db, &dir);
    let segments = segment_files(&dir).with_context(|| format!("no backup segments in {}", dir.display()))?;
    if segments.is_empty() {
        bail!("no events-NNNN.jsonl in {}", dir.display());
    }
    let source_event = last_event_of(segments.last().expect("checked non-empty"))?;
    let wanted = selection.canonical();
    let old_tip = if db.is_file() { copy_tip(db)?.0 } else { 0 };
    let unchanged = db.is_file()
        && !opts.rebuild
        && marker.built_selection == wanted
        && marker.source_seq == source_event.seq
        && marker.source_hash == source_event.this_hash;
    if unchanged {
        marker.source_commit = source.commit.clone();
        return Ok(PullOutcome::UpToDate { tip: old_tip });
    }

    let staged = side(db, ".pulling");
    if let Ok(meta) = std::fs::metadata(&staged) {
        let age = meta.modified().ok().and_then(|m| m.elapsed().ok()).map(|d| d.as_secs()).unwrap_or(u64::MAX);
        if age < STAGE_FRESH_SECS {
            bail!("another pull is still running (a staged copy is {age} seconds old)");
        }
    }
    remove_store_files(&staged);
    let built = build_selective(&staged, &segments, selection);
    let result = built.and_then(|(applied, new_tip, new_hash)| {
        let current = if db.is_file() { Some(copy_tip(db)?) } else { None };
        if current.as_ref() == Some(&(new_tip, new_hash)) {
            // Nothing the selection holds has changed: the copy stays as it is.
            return Ok(PullOutcome::UpToDate { tip: old_tip });
        }
        let wal = side(&staged, "-wal");
        if std::fs::metadata(&wal).map(|m| m.len() > 0).unwrap_or(false) {
            bail!("the staged copy left an unmerged write-ahead log; keeping the old copy");
        }
        swap_in(db, &staged)?;
        Ok(PullOutcome::Updated { from_tip: old_tip, to_tip: new_tip, applied })
    });
    remove_store_files(&staged);
    if result.is_ok() {
        marker.built_selection = wanted;
        marker.source_seq = source_event.seq;
        marker.source_hash = source_event.this_hash;
        marker.source_commit = source.commit.clone();
    }
    result
}

/// Build the selective copy at `staged`: returns the number of events it holds
/// and its chain tip.
fn build_selective(staged: &Path, segments: &[PathBuf], selection: &Selection) -> Result<(usize, i64, String)> {
    let mut classes = crate::copy_filter::Classes::default();
    for_each_event(segments, |event| {
        classes.observe(event);
        Ok(())
    })?;
    let mut rewriter = crate::copy_filter::Rewriter::new(classes.keep(selection));
    let mut store = EventStore::new(staged).context("the staged copy cannot be created")?;
    let mut batch: Vec<ShippedEvent> = Vec::with_capacity(CHUNK);
    let mut applied = 0usize;
    let mut flush = |store: &mut EventStore, batch: &mut Vec<ShippedEvent>| -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        match store.ingest_batch(batch)? {
            IngestOutcome::Applied { applied: n, .. } => applied += n,
            IngestOutcome::Rejected { reason, at_seq, .. } => bail!("the filtered copy does not verify (seq {at_seq}): {reason}"),
        }
        batch.clear();
        Ok(())
    };
    for_each_event(segments, |event| {
        if let Some(kept) = rewriter.rewrite(event)? {
            batch.push(kept);
            if batch.len() >= CHUNK {
                flush(&mut store, &mut batch)?;
            }
        }
        Ok(())
    })?;
    flush(&mut store, &mut batch)?;
    let (tip, hash) = store.contiguous_tip()?;
    if tip != applied as i64 {
        bail!("the filtered copy ends at seq {tip} but holds {applied} events");
    }
    Ok((applied, tip, hash))
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

/// The file name of the owner's reply rulebook, in the backup and beside a store.
pub const RULEBOOK_FILE: &str = "guard-response-rulebook.json";

/// Backup side: copy the owner's reply rulebook (beside the store `db`) into
/// the backup folder `out_dir`. `Ok(false)` when there is no rulebook.
pub fn carry_rulebook(db: &Path, out_dir: &Path) -> std::io::Result<bool> {
    let source = serve::respond::default_rulebook_path(db);
    if !source.is_file() {
        return Ok(false);
    }
    std::fs::create_dir_all(out_dir)?;
    std::fs::copy(&source, out_dir.join(RULEBOOK_FILE))?;
    Ok(true)
}

/// Put the owner's reply rulebook, if the backup carries one, beside the
/// copy's store. A different file already there is kept as `<name>.bak`
/// first; an identical one is left alone. A failure is never fatal to the pull.
fn install_rulebook(db: &Path, backup_dir: &Path) {
    let source = backup_dir.join(RULEBOOK_FILE);
    let Ok(wanted) = std::fs::read(&source) else { return };
    let dest = serve::respond::default_rulebook_path(db);
    if let Ok(existing) = std::fs::read(&dest) {
        if existing == wanted {
            return;
        }
        let _ = std::fs::write(side(&dest, ".bak"), existing);
    }
    let _ = std::fs::write(&dest, wanted);
}

/// When git refused a folder as "dubious ownership", the exact line that
/// lets this user open it, built from the path git names in its message.
pub fn safe_directory_fix(error_text: &str) -> Option<String> {
    let after = error_text.split("dubious ownership in repository at").nth(1)?;
    let path = after.split('\'').nth(1).filter(|p| !p.is_empty())?;
    Some(format!("git config --global --add safe.directory \"{path}\""))
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(300).collect()
}

/// A scope must name a project. The words that mean "no project" are the
/// general rules' switch, not a scope.
fn check_scopes(selection: &Selection) -> Result<()> {
    for scope in &selection.scopes {
        if model::normalize::normalize_project(Some(scope)).is_none() {
            bail!("'{scope}' is not a project name; to hold the items that belong to no project, add --include-global");
        }
    }
    Ok(())
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
    // The selection: named now (it replaces the remembered one), else the
    // remembered one, else nothing selected - the whole memory.
    let selection = if opts.whole {
        Selection::default()
    } else if !opts.scopes.is_empty() || opts.include_global {
        Selection { scopes: opts.scopes.clone(), include_global: opts.include_global }
    } else {
        marker.selection()
    };
    check_scopes(&selection)?;
    marker.set_selection(&selection);
    marker.last_attempt_unix = copy_mode::now_unix();
    copy_mode::write_marker(db, &marker).context("the copy marker could not be written")?;

    let selection = marker.selection();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pull_inner(db, &repo, &subdir, opts, &mut marker, &selection)))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("the pull panicked")));
    match &outcome {
        Ok(result) => {
            marker.last_error.clear();
            marker.checked_at_unix = copy_mode::now_unix();
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
    use thor_core::event_store::EventKind;

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
    // ---------------------------------------------------------------- selective copy

    const CANARY_BETA: &str = "CANARYBETAXQZ";
    const CANARY_SECRET_CMD: &str = "CANARYSESSIONCOMMANDXQZ";

    fn scoped(id: &str, project: Option<&str>, words: &str) -> Item {
        Item { project: project.map(str::to_string), text: format!("{id} {words}"), ..rule(id) }
    }

    /// A main store with two projects, general rules and telemetry that carries
    /// session text, exported as a fake backup folder.
    fn mixed_world() -> World {
        let dir = tempfile::tempdir().unwrap();
        let mut main = EventStore::new(&dir.path().join("main.db")).unwrap();
        let items = [
            scoped("alpha-one", Some("alpha"), "keeps the alpha pipeline honest every release"),
            scoped("alpha-two", Some("alpha"), "guards the alpha database migrations carefully"),
            scoped("beta-one", Some("beta"), &format!("{CANARY_BETA} describes the beta customer contract")),
            scoped("general-one", None, "never commit credentials into any repository whatsoever"),
        ];
        for item in &items {
            model::store::declare(&mut main, "s", "s", "s", item).unwrap();
        }
        // Telemetry that holds text from the session it happened in.
        let refusal = serde_json::json!({ "target": CANARY_SECRET_CMD }).to_string();
        main.append_event("s", "s", "hook", EventKind::GateRefused, "alpha-one", None, &refusal).unwrap();
        let served = serde_json::json!({ "served_at": "2026-01-01T00:00:00Z", "trigger": CANARY_SECRET_CMD }).to_string();
        main.append_event("s", "s", "hook", EventKind::ItemServed, "general-one", None, &served).unwrap();
        let repo = dir.path().join("clone");
        std::fs::create_dir_all(repo.join("thor2")).unwrap();
        export_segments(&main, &repo.join("thor2"), 2_000).unwrap();
        let copy_dir = dir.path().join("laptop");
        std::fs::create_dir_all(&copy_dir).unwrap();
        World { _dir: dir, main, repo, copy: copy_dir.join("thor.db") }
    }

    fn selecting(w: &World, scopes: &[&str], global: bool) -> PullOptions {
        PullOptions {
            repo: Some(w.repo.clone()),
            no_fetch: true,
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            include_global: global,
            ..Default::default()
        }
    }

    fn remembered(w: &World) -> PullOptions {
        PullOptions { repo: Some(w.repo.clone()), no_fetch: true, ..Default::default() }
    }

    fn ids_in(db: &Path) -> Vec<String> {
        let store = EventStore::open_read_only(db).unwrap();
        let mut ids: Vec<String> = serve::live::live_items(&store).into_iter().map(|i| i.id).collect();
        ids.sort();
        ids
    }

    /// Every byte the copy has on disk: the store file and its companions.
    fn bytes_of_copy(db: &Path) -> Vec<u8> {
        let mut all = Vec::new();
        for suffix in ["", "-wal", "-shm"] {
            all.extend(std::fs::read(side(db, suffix)).unwrap_or_default());
        }
        all
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    #[test]
    fn a_selection_holds_the_chosen_scopes_and_the_general_rules_only() {
        let w = mixed_world();
        pull(&w.copy, &selecting(&w, &["alpha"], true)).unwrap();
        assert_eq!(ids_in(&w.copy), vec!["alpha-one", "alpha-two", "general-one"]);
        let marker = copy_mode::read_marker(&w.copy);
        assert_eq!(marker.scopes, vec!["alpha".to_string()]);
        assert!(marker.include_global);
        assert_eq!(marker.built_selection, marker.selection().canonical());
    }

    #[test]
    fn scopes_without_the_general_switch_leave_the_general_rules_out_and_the_switch_alone_leaves_the_projects_out() {
        let w = mixed_world();
        pull(&w.copy, &selecting(&w, &["alpha", "beta"], false)).unwrap();
        assert_eq!(ids_in(&w.copy), vec!["alpha-one", "alpha-two", "beta-one"]);
        pull(&w.copy, &selecting(&w, &[], true)).unwrap();
        assert_eq!(ids_in(&w.copy), vec!["general-one"]);
    }

    #[test]
    fn what_was_left_out_is_absent_from_the_copy_file_itself() {
        let w = mixed_world();
        pull(&w.copy, &selecting(&w, &["alpha"], true)).unwrap();
        let bytes = bytes_of_copy(&w.copy);
        assert!(contains(&bytes, "alpha pipeline"), "fixture sanity: the chosen text is there");
        assert!(!contains(&bytes, CANARY_BETA), "the canary of an unselected project must not be anywhere in the copy");
        assert!(!contains(&bytes, "beta-one"), "not even its id");
        assert!(!contains(&bytes, CANARY_SECRET_CMD), "session text from refusals and served records is not copied");
        assert!(!side(&w.copy, ".pulling").exists());
    }

    #[test]
    fn the_filtered_copy_verifies_as_a_chain_of_its_own() {
        let w = mixed_world();
        pull(&w.copy, &selecting(&w, &["alpha"], true)).unwrap();
        let store = EventStore::open_read_only(&w.copy).unwrap();
        let events = store.get_all_events().unwrap();
        assert!(thor_core::auditor::verify_chain_integrity(&events).is_ok());
        assert!(thor_core::auditor::DifferentialAuditor::verify_consistency(&events).is_ok());
        assert!(crate::health::store_line(&w.copy).contains("chain intact"), "{}", crate::health::store_line(&w.copy));
        assert_ne!(tip_of(&w.copy), w.main.contiguous_tip().unwrap(), "it is a derivative, not the source chain");
        assert_eq!(tip_of(&w.copy).0 as usize, events.len(), "the copy's own numbering has no holes");
        assert!(store.heads_projection_current());
    }

    #[test]
    fn a_revised_item_keeps_a_working_history_in_the_copy() {
        let mut w = mixed_world();
        let first = scoped("alpha-one", Some("alpha"), "keeps the alpha pipeline honest every release");
        let second = Item { text: "alpha-one now also watches the alpha release notes".to_string(), ..first.clone() };
        model::store::revise(&mut w.main, "s", "s", "s", &first, &second).unwrap();
        export_segments(&w.main, &w.repo.join("thor2"), 2_000).unwrap();
        pull(&w.copy, &selecting(&w, &["alpha"], true)).unwrap();
        let store = EventStore::open_read_only(&w.copy).unwrap();
        let shown = model::store::show(&store, "alpha-one").unwrap();
        assert!(shown.text.contains("release notes"), "the head is the revision");
        assert_eq!(model::store::history(&store, "alpha-one").unwrap().len(), 2, "both revisions, one head");
    }

    #[test]
    fn an_item_that_mentions_a_left_out_item_still_serves() {
        let mut w = mixed_world();
        let mut pointer = scoped("pointer", None, "see the rule beta-one before touching the contract");
        pointer.tags = vec!["related:beta-one".to_string()];
        model::store::declare(&mut w.main, "s", "s", "s", &pointer).unwrap();
        export_segments(&w.main, &w.repo.join("thor2"), 2_000).unwrap();
        pull(&w.copy, &selecting(&w, &["alpha"], true)).unwrap();
        let store = EventStore::open_read_only(&w.copy).unwrap();
        let shown = model::store::show(&store, "pointer").unwrap();
        assert!(shown.text.contains("beta-one"), "it is only words");
        assert!(matches!(model::store::show(&store, "beta-one"), Err(model::store::ReadError::NotFound(_))), "the target is plainly not there");
        assert!(ids_in(&w.copy).contains(&"pointer".to_string()));
    }

    #[test]
    fn the_selection_is_remembered_and_a_later_pull_repeats_it() {
        let mut w = mixed_world();
        pull(&w.copy, &selecting(&w, &["alpha"], true)).unwrap();
        model::store::declare(&mut w.main, "s", "s", "s", &scoped("alpha-three", Some("alpha"), "a third alpha rule about tagging builds")).unwrap();
        model::store::declare(&mut w.main, "s", "s", "s", &scoped("beta-two", Some("beta"), "CANARYBETATWO another beta contract clause")).unwrap();
        export_segments(&w.main, &w.repo.join("thor2"), 2_000).unwrap();
        // No selection named this time: the marker's is used.
        let outcome = pull(&w.copy, &remembered(&w)).unwrap();
        assert!(matches!(outcome, PullOutcome::Updated { .. }), "{outcome:?}");
        assert_eq!(ids_in(&w.copy), vec!["alpha-one", "alpha-three", "alpha-two", "general-one"]);
        assert!(!contains(&bytes_of_copy(&w.copy), "CANARYBETATWO"));
    }

    #[test]
    fn a_source_that_did_not_move_leaves_the_copy_untouched() {
        let w = mixed_world();
        pull(&w.copy, &selecting(&w, &["alpha"], true)).unwrap();
        let bytes = std::fs::read(&w.copy).unwrap();
        let outcome = pull(&w.copy, &remembered(&w)).unwrap();
        assert!(matches!(outcome, PullOutcome::UpToDate { .. }), "{outcome:?}");
        assert!(bytes == std::fs::read(&w.copy).unwrap());
        assert!(copy_mode::read_marker(&w.copy).checked_at_unix > 0, "a successful check is recorded even when nothing changed");
    }

    #[test]
    fn a_new_unselected_item_does_not_touch_the_copy_file() {
        let mut w = mixed_world();
        pull(&w.copy, &selecting(&w, &["alpha"], true)).unwrap();
        let bytes = std::fs::read(&w.copy).unwrap();
        model::store::declare(&mut w.main, "s", "s", "s", &scoped("beta-three", Some("beta"), "yet another beta contract clause about invoices")).unwrap();
        export_segments(&w.main, &w.repo.join("thor2"), 2_000).unwrap();
        let outcome = pull(&w.copy, &remembered(&w)).unwrap();
        assert!(matches!(outcome, PullOutcome::UpToDate { .. }), "{outcome:?}");
        assert!(bytes == std::fs::read(&w.copy).unwrap(), "nothing the selection holds changed, so the file is left as it was");
        assert_eq!(copy_mode::read_marker(&w.copy).source_seq, w.main.contiguous_tip().unwrap().0, "yet the pull knows the source moved on");
    }

    #[test]
    fn changing_the_selection_rebuilds_and_the_old_content_is_gone() {
        let w = mixed_world();
        pull(&w.copy, &selecting(&w, &["beta"], false)).unwrap();
        assert!(contains(&bytes_of_copy(&w.copy), CANARY_BETA));
        pull(&w.copy, &selecting(&w, &["alpha"], true)).unwrap();
        assert_eq!(ids_in(&w.copy), vec!["alpha-one", "alpha-two", "general-one"]);
        assert!(!contains(&bytes_of_copy(&w.copy), CANARY_BETA), "narrowing the selection removes what it no longer covers");
    }

    #[test]
    fn selecting_nothing_is_today_the_whole_memory_with_the_sources_own_chain() {
        let w = mixed_world();
        pull(&w.copy, &opts(&w)).unwrap();
        assert_eq!(tip_of(&w.copy), w.main.contiguous_tip().unwrap(), "the same chain as the source");
        let marker = copy_mode::read_marker(&w.copy);
        assert!(!marker.selection().is_active());
        assert_eq!(marker.built_selection, "");
        assert!(contains(&bytes_of_copy(&w.copy), CANARY_BETA), "everything is there");
    }

    #[test]
    fn going_back_to_the_whole_memory_continues_from_the_source_chain() {
        let w = mixed_world();
        pull(&w.copy, &selecting(&w, &["alpha"], true)).unwrap();
        let back = PullOptions { repo: Some(w.repo.clone()), no_fetch: true, whole: true, ..Default::default() };
        pull(&w.copy, &back).unwrap();
        assert_eq!(tip_of(&w.copy), w.main.contiguous_tip().unwrap());
        assert!(!copy_mode::read_marker(&w.copy).selection().is_active());
        // And the next ordinary pull is a plain continuation.
        assert!(matches!(pull(&w.copy, &opts(&w)).unwrap(), PullOutcome::UpToDate { .. }));
    }

    #[test]
    fn a_failed_selective_pull_keeps_the_old_copy_byte_for_byte() {
        let mut w = mixed_world();
        pull(&w.copy, &selecting(&w, &["alpha"], true)).unwrap();
        let bytes = std::fs::read(&w.copy).unwrap();
        model::store::declare(&mut w.main, "s", "s", "s", &scoped("alpha-four", Some("alpha"), "a fourth alpha rule about release branches")).unwrap();
        export_segments(&w.main, &w.repo.join("thor2"), 2_000).unwrap();
        let last = segment_files(&w.repo.join("thor2")).unwrap().pop().unwrap();
        let text = std::fs::read_to_string(&last).unwrap();
        let broken = text.replacen("a fourth alpha rule", "a fifth alpha rule", 1);
        assert_ne!(text, broken, "fixture sanity: a body was changed");
        std::fs::write(&last, broken).unwrap();
        let result = pull(&w.copy, &remembered(&w));
        assert!(result.unwrap_err().to_string().contains("does not verify"));
        assert!(bytes == std::fs::read(&w.copy).unwrap(), "the old copy is exactly as it was");
        assert!(!side(&w.copy, ".pulling").exists(), "the staged copy is removed");
        let marker = copy_mode::read_marker(&w.copy);
        assert!(!marker.last_error.is_empty());
        assert_eq!(marker.scopes, vec!["alpha".to_string()], "and the selection is still remembered");
    }

    #[test]
    fn a_scope_that_means_no_project_is_refused_with_the_fix() {
        let w = mixed_world();
        let err = pull(&w.copy, &selecting(&w, &["global"], false)).unwrap_err().to_string();
        assert!(err.contains("--include-global"), "{err}");
        assert!(!w.copy.exists());
    }

    #[test]
    fn a_selective_pull_never_replaces_a_store_that_is_not_a_copy() {
        let w = mixed_world();
        EventStore::new(&w.copy).unwrap();
        let bytes = std::fs::read(&w.copy).unwrap();
        assert!(pull(&w.copy, &selecting(&w, &["alpha"], true)).unwrap_err().to_string().contains("not marked as a copy"));
        assert!(bytes == std::fs::read(&w.copy).unwrap());
    }

    /// An address or a bare repository is cloned into a scratch folder that is
    /// gone again when the pull ends.
    #[test]
    fn a_bare_repository_is_read_through_a_scratch_clone_that_is_deleted() {
        let mut w = mixed_world();
        if !run_git(&w.repo, &["init", "-q", "-b", "main"]) {
            return; // no git on this machine
        }
        assert!(run_git(&w.repo, &["add", "-A"]) && run_git(&w.repo, &["commit", "-q", "-m", "backup one"]));
        let bare = w._dir.path().join("backup.git");
        let cloned = Command::new("git").arg("clone").arg("-q").arg("--bare").arg(&w.repo).arg(&bare).status().unwrap();
        assert!(cloned.success());
        let options = PullOptions { repo: Some(bare.clone()), scopes: vec!["alpha".into()], include_global: true, ..Default::default() };
        pull(&w.copy, &options).unwrap();
        assert_eq!(ids_in(&w.copy), vec!["alpha-one", "alpha-two", "general-one"]);
        assert!(!side(&w.copy, ".source").exists(), "no copy of the whole memory stays on this machine");
        let commit = copy_mode::read_marker(&w.copy).source_commit;
        assert!(!commit.is_empty(), "the commit it was built from is remembered");

        // The same commit again: nothing is downloaded and nothing changes.
        let bytes = std::fs::read(&w.copy).unwrap();
        let again = pull(&w.copy, &PullOptions { repo: Some(bare.clone()), ..Default::default() }).unwrap();
        assert!(matches!(again, PullOutcome::UpToDate { .. }), "{again:?}");
        assert!(bytes == std::fs::read(&w.copy).unwrap());

        // A new backup commit is picked up (pushed into the bare repository).
        model::store::declare(&mut w.main, "s", "s", "s", &scoped("alpha-five", Some("alpha"), "a fifth alpha rule about nightly jobs")).unwrap();
        export_segments(&w.main, &w.repo.join("thor2"), 2_000).unwrap();
        assert!(run_git(&w.repo, &["add", "-A"]) && run_git(&w.repo, &["commit", "-q", "-m", "backup two"]));
        assert!(run_git(&w.repo, &["push", "-q", &bare.display().to_string(), "main"]));
        pull(&w.copy, &PullOptions { repo: Some(bare), ..Default::default() }).unwrap();
        assert!(ids_in(&w.copy).contains(&"alpha-five".to_string()));
        assert!(!side(&w.copy, ".source").exists());
    }

    /// The whole-memory copy can also be fed from a plain folder (a share on a
    /// NAS) or a bare repository, without keeping a clone of its own.
    #[test]
    fn a_whole_copy_reads_a_plain_folder_and_a_bare_repository_without_a_clone() {
        let mut w = mixed_world();
        let from_folder = PullOptions { repo: Some(w.repo.clone()), ..Default::default() };
        pull(&w.copy, &from_folder).unwrap();
        assert_eq!(tip_of(&w.copy), w.main.contiguous_tip().unwrap(), "a folder with no git in it is read as it is");

        if !run_git(&w.repo, &["init", "-q", "-b", "main"]) {
            return;
        }
        assert!(run_git(&w.repo, &["add", "-A"]) && run_git(&w.repo, &["commit", "-q", "-m", "backup one"]));
        let bare = w._dir.path().join("backup.git");
        assert!(Command::new("git").arg("clone").arg("-q").arg("--bare").arg(&w.repo).arg(&bare).status().unwrap().success());
        let other = w._dir.path().join("laptop").join("second.db");
        let from_bare = PullOptions { repo: Some(bare.clone()), ..Default::default() };
        pull(&other, &from_bare).unwrap();
        assert_eq!(tip_of(&other), w.main.contiguous_tip().unwrap());
        assert!(!side(&other, ".source").exists());

        model::store::declare(&mut w.main, "s", "s", "s", &scoped("late", None, "a late general rule about release notes")).unwrap();
        export_segments(&w.main, &w.repo.join("thor2"), 2_000).unwrap();
        assert!(run_git(&w.repo, &["add", "-A"]) && run_git(&w.repo, &["commit", "-q", "-m", "backup two"]));
        assert!(run_git(&w.repo, &["push", "-q", &bare.display().to_string(), "main"]));
        let outcome = pull(&other, &from_bare).unwrap();
        assert!(matches!(outcome, PullOutcome::Updated { applied: 1, .. }), "only the new event is applied: {outcome:?}");
        assert_eq!(tip_of(&other), w.main.contiguous_tip().unwrap());
    }

    #[test]
    fn the_last_seq_is_read_from_the_tail() {
        let w = world(30);
        let last = segment_files(&w.repo.join("thor2")).unwrap().pop().unwrap();
        assert_eq!(last_seq_of(&last).unwrap(), w.main.contiguous_tip().unwrap().0);
    }

    #[test]
    fn git_s_dubious_ownership_refusal_becomes_the_exact_safe_directory_line() {
        let text = "git ls-remote failed: detected dubious ownership in repository at 'D:/nas/backup.git'";
        assert_eq!(
            safe_directory_fix(text).as_deref(),
            Some("git config --global --add safe.directory \"D:/nas/backup.git\"")
        );
        assert_eq!(safe_directory_fix("git clone failed: repository not found"), None);
    }

    #[test]
    fn the_owners_reply_rulebook_follows_the_backup_into_the_copy_and_the_old_one_is_kept() {
        let w = world(3);
        let main_db = w._dir.path().join("main.db");
        std::fs::write(main_db.parent().unwrap().join(RULEBOOK_FILE), "[\"owner rules\"]").unwrap();
        assert!(carry_rulebook(&main_db, &w.repo.join("thor2")).unwrap());
        assert!(!carry_rulebook(&w._dir.path().join("elsewhere").join("x.db"), &w.repo.join("thor2")).unwrap());

        let beside = serve::respond::default_rulebook_path(&w.copy);
        std::fs::write(&beside, "[\"default\"]").unwrap();
        pull(&w.copy, &opts(&w)).unwrap();
        assert_eq!(std::fs::read_to_string(&beside).unwrap(), "[\"owner rules\"]");
        assert_eq!(std::fs::read_to_string(side(&beside, ".bak")).unwrap(), "[\"default\"]", "the file that was there is kept");

        // An identical file is left alone: no churn in the .bak.
        std::fs::write(side(&beside, ".bak"), "marker").unwrap();
        pull(&w.copy, &opts(&w)).unwrap();
        assert_eq!(std::fs::read_to_string(side(&beside, ".bak")).unwrap(), "marker");
    }

    #[test]
    fn a_whole_copy_never_brings_the_personal_library_along() {
        let w = world(3);
        std::fs::write(w.repo.join("thor2").join("library.jsonl"), "{\"shelf\":\"recipes\"}\n").unwrap();
        let whole = PullOptions { whole: true, ..opts(&w) };
        pull(&w.copy, &whole).unwrap();
        let beside = std::fs::read_dir(w.copy.parent().unwrap()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string());
        assert!(beside.into_iter().all(|name| !name.contains("library")));
    }

    #[test]
    fn a_backup_without_a_rulebook_leaves_the_copys_own_alone() {
        let w = world(3);
        let beside = serve::respond::default_rulebook_path(&w.copy);
        std::fs::write(&beside, "[\"mine\"]").unwrap();
        pull(&w.copy, &opts(&w)).unwrap();
        assert_eq!(std::fs::read_to_string(&beside).unwrap(), "[\"mine\"]");
        assert!(!side(&beside, ".bak").exists());
    }
}
