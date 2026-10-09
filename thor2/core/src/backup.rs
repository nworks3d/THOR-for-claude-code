//! GitHub backup + restore for THOR's event log.
//!
//! The backup IS the event log, exported as canonical append-only JSONL. Because
//! the log only ever grows, each day's export is a near-pure git append -
//! delta-compresses to almost nothing, diffs are human-readable, retention is
//! just git history. Restore replays the log into a fresh store and REQUIRES
//! every replayed `this_hash` to equal the recorded one, so a restore that does
//! not faithfully reconstruct the store fails loudly instead of silently
//! producing a different memory. (Replay-determinism is THOR's own proven M0
//! property; this makes it the backup's integrity guarantee.)

use crate::event_store::{EventKind, EventStore};
use serde_json::Value;
use std::io::{BufRead, Write};
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Do not push more than once per this many hours (debounce for the SessionStart hook).
const DEBOUNCE_HOURS: u64 = 20;

/// One export segment never grows past this many bytes. GitHub refuses any
/// file over 100 MiB (104,857,600 bytes) and warns above 50 MB, so a segment
/// is cut well under both: the single `events.jsonl` crossed the limit on
/// 2026-10-01 and every backup since then failed at the push.
pub const SEGMENT_MAX_BYTES: usize = 50_000_000;

/// A local, never-pushed commit holding an object bigger than this can never
/// be pushed to GitHub, so it is replaced rather than retried forever.
pub const PUSH_LIMIT_BYTES: u64 = 90_000_000;

/// The two sizes `backup_to_repo_sized` works with - a seam so tests can use
/// kilobytes instead of megabytes.
#[derive(Debug, Clone, Copy)]
pub struct Sizes {
    pub segment_bytes: usize,
    pub push_limit_bytes: u64,
}

impl Sizes {
    pub const DEFAULT: Sizes = Sizes { segment_bytes: SEGMENT_MAX_BYTES, push_limit_bytes: PUSH_LIMIT_BYTES };
}

fn git(repo: &Path, args: &[&str]) -> anyhow::Result<()> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output()?;
    if !out.status.success() {
        anyhow::bail!("git {:?} failed: {}", args, failure_reason(&String::from_utf8_lossy(&out.stderr)));
    }
    Ok(())
}

/// The part of git's stderr that says why: the `error:`/`fatal:` lines (a
/// remote's refusal is buried under progress and hint lines), or everything
/// when no line says so.
fn failure_reason(stderr: &str) -> String {
    let said: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| l.contains("error:") || l.contains("fatal:") || l.contains("rejected"))
        .collect();
    if said.is_empty() {
        stderr.trim().to_string()
    } else {
        said.join(" | ")
    }
}

/// Age in hours of the last commit reachable from local `HEAD` that touched
/// `subdir`, or None if there is none. Used only when there is no upstream
/// to measure from instead - see `last_commit_age_hours_at`.
fn last_commit_age_hours(repo: &Path, subdir: &str) -> Option<u64> {
    last_commit_age_hours_at(repo, subdir, "HEAD")
}

/// `last_commit_age_hours`, measured from an arbitrary `rev` instead of
/// always `HEAD` - the seam `backup_to_repo` uses to measure the debounce
/// from the upstream (what the REMOTE actually has) rather than the local
/// branch. A commit that was made locally but never pushed must not make
/// the NEXT run think a backup just landed - see this file's own doc
/// comment on the defect this closes.
fn last_commit_age_hours_at(repo: &Path, subdir: &str, rev: &str) -> Option<u64> {
    let pathspec = format!("{subdir}/");
    let out = Command::new("git")
        .arg("-C").arg(repo)
        .args(["log", "-1", "--format=%ct", rev, "--", &pathspec])
        .output().ok()?;
    let ts: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(now.saturating_sub(ts) / 3600)
}

/// Where a backup goes: one remote, one branch, and the local copy of what
/// that remote has (`tracking`, for example `refs/remotes/nas/main`), when git has one.
///
/// WHY THE REMOTE IS CHOSEN HERE AND NOT BY GIT'S "UPSTREAM". The private
/// backup moved from GitHub to the NAS on 2026-10-02. A clone that still has
/// both remotes would keep pushing to whichever one its branch tracks, which
/// is the old one. So a remote named `nas` wins by itself, with no change to
/// the hook or the scheduled task that start the backup; `--remote` names
/// another one on purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub remote: String,
    pub branch: String,
    pub tracking: Option<String>,
}

fn git_text(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The names of the remotes the clone has.
fn remote_names(repo: &Path) -> Vec<String> {
    git_text(repo, &["remote"]).map(|t| t.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()).unwrap_or_default()
}

/// Whether the repository has any remote at all. None is a deliberate local
/// copy (`install --set-backup local`).
pub fn has_remotes(repo: &Path) -> bool {
    !remote_names(repo).is_empty()
}

/// Where `remote` points (its fetch address), for messages and for the doctor.
pub fn remote_url(repo: &Path, remote: &str) -> Option<String> {
    git_text(repo, &["remote", "get-url", remote]).filter(|u| !u.is_empty())
}

/// The checked-out branch's configured upstream, split into (remote,
/// branch) - e.g. `("origin", "main")` for a branch tracking `origin/main`,
/// but just as well `("upstream", "release/2.0")` for one tracking
/// `upstream/release/2.0` (a branch name may itself contain `/`, so only
/// the FIRST `/` splits remote from branch - a remote name never does).
/// `None` with no upstream configured.
fn upstream_remote_and_branch(repo: &Path) -> Option<(String, String)> {
    let full = git_text(repo, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])?;
    let (remote, branch) = full.split_once('/')?;
    Some((remote.to_string(), branch.to_string()))
}

/// Choose the remote and branch a backup uses.
///
/// Remote: the one asked for (`requested`, which must exist), else a remote
/// named `nas`, else the remote the checked-out branch tracks, else `origin`.
/// Branch: the tracked branch when the tracked remote is the one chosen, else
/// the checked-out branch, else `main`.
pub fn resolve_target(repo: &Path, requested: Option<&str>) -> anyhow::Result<Target> {
    let remotes = remote_names(repo);
    let upstream = upstream_remote_and_branch(repo);
    let remote = match requested {
        Some(name) => {
            anyhow::ensure!(
                remotes.iter().any(|r| r == name),
                "{} has no remote named '{name}' - add it first (git remote add {name} <address>) or leave --remote off",
                repo.display()
            );
            name.to_string()
        }
        None if remotes.iter().any(|r| r == "nas") => "nas".to_string(),
        None => upstream.as_ref().map(|(r, _)| r.clone()).unwrap_or_else(|| "origin".to_string()),
    };
    let branch = match &upstream {
        Some((r, b)) if *r == remote => b.clone(),
        _ => git_text(repo, &["symbolic-ref", "--short", "HEAD"]).unwrap_or_else(|| "main".to_string()),
    };
    let tracking_ref = format!("refs/remotes/{remote}/{branch}");
    let has_tracking = Command::new("git")
        .arg("-C").arg(repo)
        .args(["rev-parse", "--verify", "-q", &tracking_ref])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    Ok(Target { remote, branch, tracking: has_tracking.then_some(tracking_ref) })
}

/// How many commits local `HEAD` carries that `tracking` does not - the exact
/// count of backup commits a previous run made but could not push. `None`
/// when it cannot be determined (git failed to run at all), which callers
/// treat the same as "nothing to catch up" rather than guessing.
fn commits_ahead_of(repo: &Path, tracking: &str) -> Option<u64> {
    git_text(repo, &["rev-list", "--count", &format!("{tracking}..HEAD")])?.parse().ok()
}

/// `commits_ahead_of` against git's own upstream (`@{u}`); the tests use it.
#[cfg(test)]
fn commits_ahead_of_upstream(repo: &Path) -> Option<u64> {
    commits_ahead_of(repo, "@{u}")
}

/// Automated backup: export the log to <repo>/<subdir>/events-0001.jsonl ..., then commit
/// and push (git handles the credentials). Debounced to once per DEBOUNCE_HOURS
/// unless `force`. Only ever touches `subdir` - it pulls --rebase first so it
/// never collides with anything else living in the same repo. Returns a
/// human-readable status line.
///
/// `subdir` is a parameter, and this is the one place this file departs from the
/// 1.0 original it was ported from. 1.0 hardcoded "thor". A 2.0 store is a
/// DIFFERENT hash chain over the same memory, so writing it to the same
/// `thor/events.jsonl` would overwrite 1.0's backup with a log that does not
/// continue it - destroying the fallback at the exact moment a switchover makes
/// you need it. Two chains, two directories, no shared file.
pub fn backup_to_repo(
    store: &EventStore,
    repo: &Path,
    subdir: &str,
    force: bool,
) -> anyhow::Result<String> {
    backup_to_repo_sized(store, repo, subdir, force, Sizes::DEFAULT)
}

/// `backup_to_repo` pushing to the remote `requested` names (see
/// `resolve_target` for what happens when it is None).
pub fn backup_to_remote(
    store: &EventStore,
    repo: &Path,
    subdir: &str,
    force: bool,
    requested: Option<&str>,
) -> anyhow::Result<String> {
    backup_inner(store, repo, subdir, force, Sizes::DEFAULT, requested)
}

/// `backup_to_repo` with the segment and push sizes given - tests pass
/// kilobytes, the real run passes `Sizes::DEFAULT`.
pub fn backup_to_repo_sized(
    store: &EventStore,
    repo: &Path,
    subdir: &str,
    force: bool,
    sizes: Sizes,
) -> anyhow::Result<String> {
    backup_inner(store, repo, subdir, force, sizes, None)
}

fn backup_inner(
    store: &EventStore,
    repo: &Path,
    subdir: &str,
    force: bool,
    sizes: Sizes,
    requested: Option<&str>,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        !subdir.is_empty()
            && !subdir.contains('/')
            && !subdir.contains('\\')
            && subdir != "..",
        "backup subdirectory must be a single plain directory name, got {subdir:?}"
    );

    // A previous run may have committed and then failed to PUSH (network
    // down, the NAS asleep, whatever) - that commit is still sitting on the
    // local branch, invisible to anyone who only ever reads the remote.
    // Push it now, before anything else and regardless of the debounce
    // below: the data is only actually backed up once it leaves this
    // machine, and the debounce exists to limit how often we commit, never
    // to sit on a commit that already happened.
    // Resolved once (see `resolve_target`): a remote named `nas` wins by
    // itself, else the branch's own upstream, else `origin`/`main`.
    let target = resolve_target(repo, requested)?;
    let Target { remote, branch, tracking } = target;
    let mut pushed_note: Option<String> = None;
    if let Some(tracking) = tracking.as_deref() {
        if let Some(ahead) = commits_ahead_of(repo, tracking) {
            if ahead > 0 {
                // A local commit holding a file GitHub will never take (the
                // old single events.jsonl) would fail every push for ever.
                let dropped = drop_unpushable_backup_commits(repo, subdir, sizes.push_limit_bytes, tracking)?;
                if dropped > 0 {
                    pushed_note = Some(format!(
                        "dropped {dropped} local backup commit(s) holding a file too big to push (the export is regenerated below)"
                    ));
                } else {
                    git(repo, &["push", &remote, &branch])?;
                    pushed_note = Some(format!("pushed {ahead} backup commit(s) that had not reached the remote"));
                }
            }
        }
    }
    let prefix = |msg: String| match &pushed_note {
        Some(note) => format!("{note}; {msg}"),
        None => msg,
    };

    if !force {
        // Measured from what the REMOTE has, not the local branch - a local
        // commit that never reached the upstream must not make the NEXT run
        // think a backup just landed (see this file's own doc comment). With
        // no upstream configured at all, there is nothing to measure from
        // but the local branch, so that stays today's behaviour unchanged.
        let age = match tracking.as_deref() {
            Some(tracking) => last_commit_age_hours_at(repo, subdir, tracking),
            None => last_commit_age_hours(repo, subdir),
        };
        if let Some(age) = age {
            if age < DEBOUNCE_HOURS {
                return Ok(prefix(format!("backup is {age}h old (< {DEBOUNCE_HOURS}h) - skipping")));
            }
        }
    }
    let out_dir = repo.join(subdir);
    std::fs::create_dir_all(&out_dir)?;
    let n = export_segments(store, &out_dir, sizes.segment_bytes)?;
    // sync with the shared repo (other backups push here too), then stage ours only
    let pathspec = format!("{subdir}/");
    // A repository with no remote at all is a deliberate local copy: there is
    // nothing to sync with and nothing to push to.
    let local_only = remote_names(repo).is_empty();
    if !local_only {
        git(repo, &["pull", "--rebase", "--autostash", &remote, &branch])?;
    }
    git(repo, &["add", &pathspec])?;
    // nothing changed? do not make an empty commit
    let clean = Command::new("git").arg("-C").arg(repo)
        .args(["diff", "--cached", "--quiet", "--", &pathspec]).status()?.success();
    if clean {
        return Ok(prefix(format!("no change since last backup ({n} events) - nothing to commit")));
    }
    git(repo, &["commit", "-m", &format!("{subdir} backup ({n} events)")])?;
    if local_only {
        return Ok(prefix(format!("committed {subdir} local backup ({n} events); no remote, so nothing left this machine")));
    }
    git(repo, &["push", &remote, &branch])?;
    Ok(prefix(format!("pushed {subdir} backup ({n} events)")))
}

/// The whole event log as one JSON line per event (no trailing newline), by seq.
fn export_lines(store: &EventStore) -> anyhow::Result<Vec<String>> {
    let mut events = store.get_all_events()?;
    events.sort_by_key(|e| e.seq);
    let mut lines = Vec::with_capacity(events.len());
    for e in &events {
        let rec = serde_json::json!({
            "seq": e.seq,
            "session_id": e.session_id,
            "lineage_id": e.lineage_id,
            "actor": e.actor,
            "kind": e.kind.as_str(),
            "entity_id": e.entity_id,
            "parent_rev": e.parent_rev,
            "body": e.body,
            "this_hash": e.this_hash,
        });
        lines.push(serde_json::to_string(&rec)?);
    }
    Ok(lines)
}

/// Write the whole event log as one JSON object per line, ordered by seq.
/// Returns the number of events written.
pub fn export_jsonl(store: &EventStore, out: &mut impl Write) -> anyhow::Result<usize> {
    let lines = export_lines(store)?;
    for line in &lines {
        writeln!(out, "{line}")?;
    }
    Ok(lines.len())
}

fn segment_name(index: usize) -> String {
    format!("events-{index:04}.jsonl")
}

fn is_segment_name(name: &str) -> bool {
    name.strip_prefix("events-")
        .and_then(|rest| rest.strip_suffix(".jsonl"))
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

/// Every segment file in `dir`, in order (`events-0001.jsonl`, ...).
pub fn segment_files(dir: &Path) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if is_segment_name(&entry.file_name().to_string_lossy()) {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

/// Export the log into `dir` as numbered segments, each at most
/// `segment_bytes` long, cut at event boundaries. The log only ever grows, so
/// every closed segment keeps the same bytes from run to run and only the
/// last one changes - git stores that as a tiny delta, and no file ever
/// reaches the size GitHub refuses. Removes the old single `events.jsonl` and
/// any stale segment, so the old file leaves the export tree in the same
/// commit that adds the segments. Returns the number of events written.
pub fn export_segments(store: &EventStore, dir: &Path, segment_bytes: usize) -> anyhow::Result<usize> {
    let lines = export_lines(store)?;
    let mut segments: Vec<String> = vec![String::new()];
    for line in &lines {
        let current = segments.last_mut().expect("at least one segment");
        if !current.is_empty() && current.len() + line.len() + 1 > segment_bytes {
            segments.push(String::new());
        }
        let current = segments.last_mut().expect("at least one segment");
        current.push_str(line);
        current.push('\n');
    }
    if lines.is_empty() {
        segments.clear();
    }
    let old = dir.join("events.jsonl");
    if old.exists() {
        std::fs::remove_file(&old)?;
    }
    let keep: Vec<String> = (1..=segments.len()).map(segment_name).collect();
    for existing in segment_files(dir)? {
        let name = existing.file_name().unwrap_or_default().to_string_lossy().to_string();
        if !keep.contains(&name) {
            std::fs::remove_file(&existing)?;
        }
    }
    for (name, text) in keep.iter().zip(&segments) {
        let path = dir.join(name);
        // Leave an identical closed segment alone: no rewrite, no new mtime.
        if std::fs::read(&path).map(|b| b == text.as_bytes()).unwrap_or(false) {
            continue;
        }
        std::fs::write(&path, text)?;
    }
    Ok(lines.len())
}

/// Replay an export into `store` from a path: a folder holding the segments,
/// one segment or legacy single file, or the old `events.jsonl` path (which
/// no longer exists once the export is segmented - the segments beside it are
/// read instead). Returns the number of events restored.
pub fn restore_path(store: &mut EventStore, path: &Path) -> anyhow::Result<usize> {
    let files: Vec<std::path::PathBuf> = if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        let dir = if path.is_dir() { path } else { path.parent().unwrap_or_else(|| Path::new(".")) };
        let segments = if dir.is_dir() { segment_files(dir)? } else { Vec::new() };
        if !segments.is_empty() {
            segments
        } else if path.is_dir() && dir.join("events.jsonl").is_file() {
            vec![dir.join("events.jsonl")]
        } else {
            anyhow::bail!(
                "no exported log found at {}: point --restore-from at the folder that holds events-0001.jsonl, events-0002.jsonl, ...",
                path.display()
            );
        }
    };
    let mut reader: Box<dyn std::io::Read> = Box::new(std::io::empty());
    for file in &files {
        let f = std::fs::File::open(file).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", file.display()))?;
        reader = Box::new(std::io::Read::chain(reader, f));
    }
    restore_jsonl(store, std::io::BufReader::new(reader))
}

/// Replace local, never-pushed backup commits that hold an object bigger than
/// `limit` (the remote will refuse it for ever). Only when EVERY unpushed
/// commit is one of this tool's own backup commits touching nothing but
/// `subdir`; anything else is somebody's work and is reported, never
/// rewritten. Nothing on the remote changes (no force push). Returns how many
/// commits were dropped; the export is regenerated by the caller.
fn drop_unpushable_backup_commits(repo: &Path, subdir: &str, limit: u64, tracking: &str) -> anyhow::Result<usize> {
    let git_out = |args: &[&str]| -> anyhow::Result<String> {
        let out = Command::new("git").arg("-C").arg(repo).args(args).output()?;
        if !out.status.success() {
            anyhow::bail!("git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    };
    // Any object in the unpushed range over the limit?
    let objects = git_out(&["rev-list", "--objects", &format!("{tracking}..HEAD")])?;
    let mut batch_in = String::new();
    for line in objects.lines() {
        if let Some(id) = line.split_whitespace().next() {
            batch_in.push_str(id);
            batch_in.push('\n');
        }
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "--batch-check=%(objectsize)"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let feed = std::thread::spawn(move || {
        let _ = stdin.write_all(batch_in.as_bytes());
    });
    let out = child.wait_with_output()?;
    let _ = feed.join();
    let too_big = String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.trim().parse::<u64>().map(|n| n > limit).unwrap_or(false));
    if !too_big {
        return Ok(0);
    }
    // Only our own export commits may be dropped.
    let subjects = git_out(&["log", "--format=%s", &format!("{tracking}..HEAD")])?;
    let names = git_out(&["diff", "--name-only", tracking, "HEAD"])?;
    let prefix = format!("{subdir} backup (");
    let own_subjects = subjects.lines().all(|s| s.starts_with(&prefix));
    let own_paths = names.lines().all(|n| n.starts_with(&format!("{subdir}/")));
    anyhow::ensure!(
        own_subjects && own_paths,
        "an unpushed commit in {} holds a file over {} bytes that the remote will refuse, and it is not one of the backup's own commits, so it was left alone - push or fix it by hand",
        repo.display(),
        limit
    );
    let count = subjects.lines().count();
    git_out(&["reset", "--soft", tracking])?;
    git_out(&["reset", "--quiet"])?;
    Ok(count)
}

/// Replay an exported log into `store` (which MUST be empty) in seq order,
/// verifying replay-determinism: every reconstructed `this_hash` must equal the
/// recorded one. Returns the number of events restored. Fails if the store is
/// not empty, a line is malformed, a kind is unknown, or any hash diverges.
pub fn restore_jsonl(store: &mut EventStore, reader: impl BufRead) -> anyhow::Result<usize> {
    if !store.get_all_events()?.is_empty() {
        anyhow::bail!("restore target is not empty - restore only into a fresh store");
    }
    // Parse all lines, then sort by seq so an out-of-order file still replays
    // in chain order.
    let mut recs: Vec<(i64, Value)> = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line)?;
        let seq = v.get("seq").and_then(|x| x.as_i64()).ok_or_else(|| anyhow::anyhow!("record missing seq"))?;
        recs.push((seq, v));
    }
    recs.sort_by_key(|(seq, _)| *seq);

    let s = |v: &Value, k: &str| -> anyhow::Result<String> {
        Ok(v.get(k).and_then(|x| x.as_str()).ok_or_else(|| anyhow::anyhow!("record missing field {k}"))?.to_string())
    };
    for (seq, v) in &recs {
        let kind_str = s(v, "kind")?;
        let kind = EventKind::from_str(&kind_str).ok_or_else(|| anyhow::anyhow!("unknown kind {kind_str} at seq {seq}"))?;
        let parent = v.get("parent_rev").and_then(|x| x.as_str());
        let ev = store.append_event(
            &s(v, "session_id")?,
            &s(v, "lineage_id")?,
            &s(v, "actor")?,
            kind,
            &s(v, "entity_id")?,
            parent,
            &s(v, "body")?,
        )?;
        let recorded = s(v, "this_hash")?;
        if ev.this_hash != recorded {
            anyhow::bail!(
                "replay mismatch at seq {seq}: reconstructed {} != recorded {recorded} - the backup does not faithfully reconstruct the store",
                ev.this_hash
            );
        }
    }
    Ok(recs.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn seed(store: &mut EventStore) -> Vec<String> {
        let a = store.append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "first body").unwrap();
        let b = store.append_event("s", "l", "act", EventKind::FactRevised, "e1", Some(&a.this_hash), "second body").unwrap();
        store.append_event("s", "l", "act", EventKind::FactCreated, "e2", None, "other entity").unwrap();
        vec![a.this_hash, b.this_hash]
    }

    #[test]
    fn test_export_restore_roundtrip_is_bit_identical() {
        let mut src = EventStore::in_memory().unwrap();
        let src_hashes = seed(&mut src);
        // export
        let mut buf: Vec<u8> = Vec::new();
        let n = export_jsonl(&src, &mut buf).unwrap();
        assert_eq!(n, 3);
        // restore into a fresh store
        let mut dst = EventStore::in_memory().unwrap();
        let restored = restore_jsonl(&mut dst, Cursor::new(&buf)).unwrap();
        assert_eq!(restored, 3);
        // the restored store is byte-identical: same events, same hashes
        let src_all = src.get_all_events().unwrap();
        let dst_all = dst.get_all_events().unwrap();
        assert_eq!(src_all.len(), dst_all.len());
        for (a, b) in src_all.iter().zip(dst_all.iter()) {
            assert_eq!(a.this_hash, b.this_hash, "restored hash must match original");
            assert_eq!(a.body, b.body);
            assert_eq!(a.entity_id, b.entity_id);
        }
        // the head hashes survive (the head of e1 is the revised rev)
        assert!(dst_all.iter().any(|e| e.this_hash == src_hashes[1]));
    }

    #[test]
    fn test_reproject_survives_roundtrip() {
        let mut src = EventStore::in_memory().unwrap();
        src.append_event("s", "l", "act", EventKind::FactCreated, "ProjA:mem-x", None, "a decision")
            .unwrap();
        src.append_event(
            "s",
            "l",
            "act",
            EventKind::FactReprojected,
            "ProjA:mem-x",
            None,
            r#"{"project":"ProjB"}"#,
        )
        .unwrap();
        let mut buf: Vec<u8> = Vec::new();
        export_jsonl(&src, &mut buf).unwrap();
        let mut dst = EventStore::in_memory().unwrap();
        let n = restore_jsonl(&mut dst, Cursor::new(&buf)).unwrap();
        assert_eq!(n, 2, "the fact_reprojected event replays with a verified hash");
        // the effective project reassignment travelled to the fresh replica
        let projects = crate::cas::compute_projects(&dst.get_all_events().unwrap());
        assert_eq!(projects["ProjA:mem-x"], Some("ProjB".to_string()));
    }

    #[test]
    fn test_restore_refuses_nonempty_store() {
        let mut dst = EventStore::in_memory().unwrap();
        seed(&mut dst);
        let err = restore_jsonl(&mut dst, Cursor::new(b"".to_vec()));
        assert!(err.is_err(), "restore must refuse a non-empty target");
    }

    #[test]
    fn test_restore_detects_tampered_hash() {
        let mut src = EventStore::in_memory().unwrap();
        seed(&mut src);
        let mut buf: Vec<u8> = Vec::new();
        export_jsonl(&src, &mut buf).unwrap();
        // corrupt a body but keep the recorded this_hash -> replay must diverge
        let tampered = String::from_utf8(buf).unwrap().replace("second body", "TAMPERED body");
        let mut dst = EventStore::in_memory().unwrap();
        let err = restore_jsonl(&mut dst, Cursor::new(tampered.into_bytes()));
        assert!(err.is_err(), "a tampered body must fail the replay-hash check");
    }

    /// The defect this guards against: a subdirectory carrying a path could
    /// escape the directory it is supposed to own - `--subdir ../thor` would
    /// write over 1.0's backup, which is the one fallback a switchover must
    /// not destroy. Refused before anything is exported or staged.
    #[test]
    fn a_subdirectory_that_is_a_path_is_refused() {
        let store = EventStore::in_memory().unwrap();
        let repo = std::path::Path::new(".");
        for bad in ["", "..", "../thor", "a/b", "a\\b"] {
            let err = backup_to_repo(&store, repo, bad, true);
            assert!(
                err.is_err(),
                "subdir {bad:?} must be refused before any git command runs"
            );
        }
    }

    // ---------------------------------------------- segments (step 29)

    /// A store whose export is a few hundred KB, big enough to need several
    /// segments at the tiny segment size the tests use.
    fn big_store(events: usize) -> EventStore {
        let mut store = EventStore::in_memory().unwrap();
        let body = "x".repeat(900);
        for i in 0..events {
            store
                .append_event("s", "l", "act", EventKind::FactCreated, &format!("e{i}"), None, &body)
                .unwrap();
        }
        store
    }

    #[test]
    fn the_export_is_split_into_segments_that_stay_under_the_limit() {
        let store = big_store(300);
        let dir = tempfile::tempdir().unwrap();
        let n = export_segments(&store, dir.path(), 100_000).unwrap();
        assert_eq!(n, 300);
        let names = segment_files(dir.path()).unwrap();
        assert!(names.len() >= 3, "300 events of ~1.2 KB at 100 KB each need several segments: {names:?}");
        for f in &names {
            let size = std::fs::metadata(f).unwrap().len();
            assert!(size <= 100_000, "{} is {size} bytes, over the segment limit", f.display());
        }
        let first = names[0].file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(first, "events-0001.jsonl");
    }

    #[test]
    fn closed_segments_do_not_change_when_the_log_grows() {
        let mut store = big_store(300);
        let dir = tempfile::tempdir().unwrap();
        export_segments(&store, dir.path(), 100_000).unwrap();
        let before = segment_files(dir.path()).unwrap();
        let first_bytes = std::fs::read(&before[0]).unwrap();
        store
            .append_event("s", "l", "act", EventKind::FactCreated, "later", None, "one more")
            .unwrap();
        export_segments(&store, dir.path(), 100_000).unwrap();
        assert_eq!(std::fs::read(&segment_files(dir.path()).unwrap()[0]).unwrap(), first_bytes);
    }

    #[test]
    fn restoring_from_the_folder_reads_every_segment_in_order() {
        let src = big_store(300);
        let dir = tempfile::tempdir().unwrap();
        export_segments(&src, dir.path(), 100_000).unwrap();
        let mut dst = EventStore::in_memory().unwrap();
        let n = restore_path(&mut dst, dir.path()).unwrap();
        assert_eq!(n, 300);
        let a = src.get_all_events().unwrap();
        let b = dst.get_all_events().unwrap();
        assert_eq!(a.last().unwrap().this_hash, b.last().unwrap().this_hash);
    }

    #[test]
    fn restoring_from_the_old_single_file_path_finds_the_segments_next_to_it() {
        let src = big_store(300);
        let dir = tempfile::tempdir().unwrap();
        export_segments(&src, dir.path(), 100_000).unwrap();
        let mut dst = EventStore::in_memory().unwrap();
        let n = restore_path(&mut dst, &dir.path().join("events.jsonl")).unwrap();
        assert_eq!(n, 300, "the old file name must lead to the segments that replaced it");
    }

    #[test]
    fn restoring_from_a_legacy_single_file_still_works() {
        let src = big_store(5);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        export_jsonl(&src, &mut f).unwrap();
        drop(f);
        let mut dst = EventStore::in_memory().unwrap();
        assert_eq!(restore_path(&mut dst, &path).unwrap(), 5);
    }

    #[test]
    fn a_missing_segment_fails_the_restore_loudly() {
        let src = big_store(300);
        let dir = tempfile::tempdir().unwrap();
        export_segments(&src, dir.path(), 100_000).unwrap();
        std::fs::remove_file(dir.path().join("events-0002.jsonl")).unwrap();
        let mut dst = EventStore::in_memory().unwrap();
        assert!(restore_path(&mut dst, dir.path()).is_err(), "a gap in the segments must not restore quietly");
    }

    /// A bare remote that refuses any object over `limit` bytes, the way
    /// GitHub refuses a file over 100 MB - the exact failure of 2026-10-01.
    fn refuse_big_objects(remote: &Path, limit: u64) {
        let hook = remote.join("hooks").join("pre-receive");
        let script = [
            "#!/bin/sh".to_string(),
            "while read old new ref; do".to_string(),
            "  if [ \"$old\" = 0000000000000000000000000000000000000000 ]; then range=\"$new\"; else range=\"$old..$new\"; fi".to_string(),
            format!("  git rev-list --objects $range | git cat-file --batch-check='%(objectsize) %(rest)' | awk '$1 > {limit} {{bad=1}} END {{exit bad}}' || {{ echo 'file is too large for this remote' >&2; exit 1; }}"),
            "done".to_string(),
            String::new(),
        ]
        .join("\n");
        std::fs::write(&hook, script).unwrap();
        // The machine's shared git hooks (core.hooksPath) would otherwise
        // replace this remote's own hooks folder.
        git_ok(remote, &["config", "core.hooksPath", hook.parent().unwrap().to_str().unwrap()]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn a_log_bigger_than_the_remote_allows_still_backs_up_in_segments() {
        let fx = make_git_fixture();
        refuse_big_objects(&fx.remote, 150_000);
        let store = big_store(300);
        let sizes = Sizes { segment_bytes: 100_000, push_limit_bytes: 150_000 };
        let out = backup_to_repo_sized(&store, &fx.work, "thor2", true, sizes).unwrap();
        assert!(out.starts_with("pushed thor2 backup ("), "{out}");
        assert_eq!(commits_ahead_of_upstream(&fx.work), Some(0));
        assert!(!fx.work.join("thor2").join("events.jsonl").exists());
        assert!(fx.work.join("thor2").join("events-0001.jsonl").exists());
    }

    /// Control for the test above: the fixture remote really does refuse one
    /// big file, so the segment test proves something.
    #[test]
    fn the_fixture_remote_refuses_a_single_big_file() {
        let fx = make_git_fixture();
        refuse_big_objects(&fx.remote, 150_000);
        std::fs::write(fx.work.join("big.bin"), vec![b'x'; 200_000]).unwrap();
        git_ok(&fx.work, &["add", "big.bin"]);
        git_ok(&fx.work, &["commit", "-m", "big"]);
        let out = Command::new("git").arg("-C").arg(&fx.work).args(["push", "origin", "main"]).output().unwrap();
        assert!(!out.status.success(), "the control remote must refuse the big file");
    }

    #[test]
    fn the_old_single_file_leaves_the_export_tree_in_the_same_commit() {
        let fx = make_git_fixture();
        let store = big_store(20);
        std::fs::create_dir_all(fx.work.join("thor2")).unwrap();
        std::fs::write(fx.work.join("thor2").join("events.jsonl"), "{}\n").unwrap();
        git_ok(&fx.work, &["add", "thor2/"]);
        git_ok(&fx.work, &["commit", "-m", "old single-file backup"]);
        git_ok(&fx.work, &["push", "origin", "main"]);
        let sizes = Sizes { segment_bytes: 100_000, push_limit_bytes: 150_000 };
        backup_to_repo_sized(&store, &fx.work, "thor2", true, sizes).unwrap();
        let tree = Command::new("git")
            .arg("-C")
            .arg(&fx.work)
            .args(["ls-tree", "-r", "--name-only", "origin/main"])
            .output()
            .unwrap();
        let tree = String::from_utf8_lossy(&tree.stdout).to_string();
        assert!(!tree.contains("thor2/events.jsonl"), "old file still in the remote tree: {tree}");
        assert!(tree.contains("thor2/events-0001.jsonl"), "{tree}");
    }

    /// The stuck state the live repo is in: a local backup commit that holds
    /// a file the remote will never take. The next run drops that one local
    /// commit (it is only an export, regenerated below) and pushes the
    /// segments - no force push, nothing on the remote is rewritten.
    #[test]
    fn a_local_commit_the_remote_refuses_is_replaced_not_pushed_forever() {
        let fx = make_git_fixture();
        refuse_big_objects(&fx.remote, 150_000);
        let store = big_store(300);
        std::fs::create_dir_all(fx.work.join("thor2")).unwrap();
        let mut f = std::fs::File::create(fx.work.join("thor2").join("events.jsonl")).unwrap();
        export_jsonl(&store, &mut f).unwrap();
        drop(f);
        git_ok(&fx.work, &["add", "thor2/"]);
        git_ok(&fx.work, &["commit", "-m", "thor2 backup (300 events)"]);
        assert_eq!(commits_ahead_of_upstream(&fx.work), Some(1));

        let sizes = Sizes { segment_bytes: 100_000, push_limit_bytes: 150_000 };
        let out = backup_to_repo_sized(&store, &fx.work, "thor2", true, sizes).unwrap();
        assert!(out.contains("dropped 1 local backup commit"), "must say what it did: {out}");
        assert_eq!(commits_ahead_of_upstream(&fx.work), Some(0));
        let log = Command::new("git")
            .arg("-C")
            .arg(&fx.work)
            .args(["log", "--format=%s", "origin/main"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).lines().count(), 2, "initial + one new backup commit");
    }

    #[test]
    fn an_unpushed_commit_that_is_not_a_backup_is_never_dropped() {
        let fx = make_git_fixture();
        let store = big_store(300);
        std::fs::write(fx.work.join("big.bin"), vec![b'x'; 200_000]).unwrap();
        git_ok(&fx.work, &["add", "big.bin"]);
        git_ok(&fx.work, &["commit", "-m", "someone's own work"]);
        let sizes = Sizes { segment_bytes: 100_000, push_limit_bytes: 150_000 };
        let err = backup_to_repo_sized(&store, &fx.work, "thor2", true, sizes);
        assert!(err.is_err(), "a commit that is not ours must be reported, never rewritten");
        assert_eq!(commits_ahead_of_upstream(&fx.work), Some(1), "the commit must still be there");
    }

    // ---------------------------------------------- real-git fixture tests
    //
    // Everything below runs the real `git` binary against a throwaway bare
    // remote plus a working clone - the only way to prove the catch-up push
    // (step 2) against the actual failure shape it exists to heal: a commit
    // that landed locally while a push to origin failed.

    /// Run `git <args>` in `dir`, panicking with stdout+stderr on failure -
    /// test fixture plumbing only. Production code never panics on a failed
    /// git call; see `git` above, which turns failure into an `Err`.
    fn git_ok(dir: &Path, args: &[&str]) {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?} in {} failed:\nstdout: {}\nstderr: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A bare remote plus a working clone tracking it on `main`, with one
    /// initial (empty) commit already pushed so `origin/main` exists - the
    /// smallest fixture every test below builds on.
    struct GitFixture {
        _dir: tempfile::TempDir,
        remote: std::path::PathBuf,
        work: std::path::PathBuf,
    }

    fn make_git_fixture() -> GitFixture {
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        let work = dir.path().join("work");
        git_ok(dir.path(), &["init", "--bare", remote.to_str().unwrap()]);
        let out = Command::new("git")
            .args(["clone", remote.to_str().unwrap(), work.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "clone failed: {}", String::from_utf8_lossy(&out.stderr));
        git_ok(&work, &["checkout", "-b", "main"]);
        git_ok(&work, &["config", "user.email", "test@example.invalid"]);
        git_ok(&work, &["config", "user.name", "Test"]);
        git_ok(&work, &["commit", "--allow-empty", "-m", "initial"]);
        git_ok(&work, &["push", "-u", "origin", "main"]);
        GitFixture { _dir: dir, remote, work }
    }

    /// THE EXACT DEFECT, end to end: a run whose commit lands locally but
    /// whose push fails (here, a broken PUSH url - the deterministic way to
    /// get "pull succeeds, push fails" without racing a real network) must
    /// leave that commit stuck ahead of `origin/main`. The NEXT run, even
    /// well inside the debounce window, must push the stuck commit first
    /// and say so - never silently wait out another DEBOUNCE_HOURS while
    /// origin stays behind.
    #[test]
    fn a_commit_that_never_reached_origin_is_pushed_on_the_next_run_and_reported() {
        let fx = make_git_fixture();
        let mut store = EventStore::in_memory().unwrap();
        seed(&mut store);

        // Break ONLY the push URL: `pull` (fetch) still reaches the real
        // remote and succeeds, so the run gets all the way to `commit`
        // before `push` fails - exactly the shape backup_to_repo's own doc
        // comment on this defect describes.
        git_ok(&fx.work, &["remote", "set-url", "--push", "origin", "/no/such/path"]);

        let first = backup_to_repo(&store, &fx.work, "thor2", true);
        assert!(first.is_err(), "a broken push URL must fail the run: {first:?}");
        assert_eq!(
            commits_ahead_of_upstream(&fx.work),
            Some(1),
            "the commit must land locally even though the push failed"
        );

        // Restore connectivity, then run again well inside the debounce
        // window (force = false): the catch-up push must still happen.
        git_ok(&fx.work, &["remote", "set-url", "--push", "origin", fx.remote.to_str().unwrap()]);
        let second = backup_to_repo(&store, &fx.work, "thor2", false).unwrap();
        assert!(
            second.contains("pushed 1 backup commit(s) that had not reached the remote"),
            "must say what it caught up: {second}"
        );
        assert_eq!(commits_ahead_of_upstream(&fx.work), Some(0), "origin must now have the commit");
    }

    /// The two "nothing to catch up" shapes together: right after a clean
    /// push, local and upstream are equal, so the new catch-up logic must
    /// add nothing to the output and must read exactly as it always has -
    /// and a second, forced run with nothing new to commit must still
    /// succeed rather than fail.
    #[test]
    fn local_equal_to_upstream_is_unchanged_and_a_forced_no_op_run_still_succeeds() {
        let fx = make_git_fixture();
        let mut store = EventStore::in_memory().unwrap();
        seed(&mut store);

        let out = backup_to_repo(&store, &fx.work, "thor2", true).unwrap();
        assert!(
            out.starts_with("pushed thor2 backup ("),
            "a first real backup with nothing ahead of origin must read exactly as before: {out}"
        );
        assert!(!out.contains("had not reached the remote"));
        assert_eq!(commits_ahead_of_upstream(&fx.work), Some(0));

        // Forced again, nothing changed: must not fail, and must read
        // exactly as the existing "nothing to commit" line always has.
        let out2 = backup_to_repo(&store, &fx.work, "thor2", true);
        assert!(out2.is_ok(), "a forced run with nothing new to commit must not fail: {out2:?}");
        assert_eq!(out2.unwrap(), "no change since last backup (3 events) - nothing to commit");
    }

    /// A repo with no `origin` remote at all (or one whose `main` was never
    /// fetched) has nothing to catch up and nothing to measure the debounce
    /// against upstream with - the debounce must keep working from local
    /// history alone, exactly as it did before this file learned about
    /// upstreams, and it must never attempt a push that would only fail
    /// loudly against a remote that does not exist.
    #[test]
    fn no_upstream_configured_keeps_todays_local_only_debounce() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        git_ok(dir.path(), &["init", "-b", "main", work.to_str().unwrap()]);
        git_ok(&work, &["config", "user.email", "test@example.invalid"]);
        git_ok(&work, &["config", "user.name", "Test"]);
        std::fs::create_dir_all(work.join("thor2")).unwrap();
        std::fs::write(work.join("thor2").join("events.jsonl"), "{}\n").unwrap();
        git_ok(&work, &["add", "thor2/"]);
        git_ok(&work, &["commit", "-m", "prior local backup, no remote configured yet"]);

        let store = EventStore::in_memory().unwrap();
        let out = backup_to_repo(&store, &work, "thor2", false).unwrap();
        assert!(
            out.contains("h old (< 20h) - skipping"),
            "with no origin at all, the debounce must still work from local history alone: {out}"
        );
        assert!(!out.contains("had not reached the remote"));
    }

    /// A backup repository with NO remote at all is a deliberate local copy
    /// (`install --set-backup local`): it commits and says so, and neither
    /// pulls nor pushes. A repository that has any remote still pulls and
    /// pushes as before, so a typo'd remote name never turns into a silent
    /// local-only backup.
    #[test]
    fn a_repository_with_no_remote_is_a_local_copy_that_commits_and_never_pushes() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        git_ok(dir.path(), &["init", "-b", "main", work.to_str().unwrap()]);
        git_ok(&work, &["config", "user.email", "test@example.invalid"]);
        git_ok(&work, &["config", "user.name", "Test"]);

        let mut store = EventStore::in_memory().unwrap();
        seed(&mut store);
        let out = backup_to_repo(&store, &work, "thor2", true).unwrap();
        assert!(out.contains("local backup") && out.contains("no remote"), "{out}");
        let log = git_text(&work, &["log", "--oneline"]).unwrap();
        assert_eq!(log.lines().count(), 1, "one commit holds the export: {log}");

        let again = backup_to_repo(&store, &work, "thor2", true).unwrap();
        assert!(again.contains("nothing to commit"), "{again}");
    }

    /// THE DEFECT STEP 2c CLOSES: every upstream check here used to be
    /// hard-coded to `origin/main`. A branch tracking a DIFFERENT remote
    /// name AND a DIFFERENT branch name - so a hard-coded pair could not
    /// accidentally still work - proves the catch-up push, the debounce
    /// age check, and the final pull/push all resolve the REAL configured
    /// upstream (`@{u}`) instead.
    #[test]
    fn catch_up_and_backup_work_against_an_upstream_that_is_not_origin_main() {
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        let work = dir.path().join("work");
        git_ok(dir.path(), &["init", "--bare", remote.to_str().unwrap()]);
        let out = Command::new("git")
            .args(["clone", remote.to_str().unwrap(), work.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "clone failed: {}", String::from_utf8_lossy(&out.stderr));
        git_ok(&work, &["checkout", "-b", "release"]);
        git_ok(&work, &["config", "user.email", "test@example.invalid"]);
        git_ok(&work, &["config", "user.name", "Test"]);
        git_ok(&work, &["remote", "rename", "origin", "upstream"]);
        git_ok(&work, &["commit", "--allow-empty", "-m", "initial"]);
        git_ok(&work, &["push", "-u", "upstream", "release"]);

        let mut store = EventStore::in_memory().unwrap();
        seed(&mut store);

        // Break ONLY the push URL, the same deterministic shape the
        // origin/main fixture above uses: `pull` (fetch) still reaches the
        // real remote and succeeds, so the run gets all the way to `commit`
        // before `push` fails.
        git_ok(&work, &["remote", "set-url", "--push", "upstream", "/no/such/path"]);
        let first = backup_to_repo(&store, &work, "thor2", true);
        assert!(first.is_err(), "a broken push URL must fail the run: {first:?}");
        assert_eq!(
            commits_ahead_of_upstream(&work),
            Some(1),
            "commits_ahead_of_upstream must resolve @{{u}} (upstream/release here), not a hard-coded \
             origin/main that does not exist in this fixture"
        );

        // Restore connectivity, then run again well inside the debounce
        // window (force = false): the catch-up push must still happen,
        // against the real upstream/release, not origin/main.
        git_ok(&work, &["remote", "set-url", "--push", "upstream", remote.to_str().unwrap()]);
        let second = backup_to_repo(&store, &work, "thor2", false).unwrap();
        assert!(
            second.contains("pushed 1 backup commit(s) that had not reached the remote"),
            "must catch up against the real upstream (upstream/release): {second}"
        );
        assert_eq!(commits_ahead_of_upstream(&work), Some(0), "the real upstream must now have the commit");
    }

    // ------------------------------------------------ the NAS remote (step 30)

    /// The fixture plus a second bare remote called `nas` that already has
    /// `main` (the state after the first push to the NAS by hand).
    fn fixture_with_nas() -> (GitFixture, std::path::PathBuf) {
        let fx = make_git_fixture();
        let nas = fx._dir.path().join("nas.git");
        git_ok(fx._dir.path(), &["init", "--bare", nas.to_str().unwrap()]);
        git_ok(&fx.work, &["remote", "add", "nas", nas.to_str().unwrap()]);
        git_ok(&fx.work, &["push", "nas", "main"]);
        (fx, nas)
    }

    fn count_commits(git_dir: &Path, rev: &str) -> usize {
        let out = Command::new("git").arg("-C").arg(git_dir).args(["rev-list", "--count", rev]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }

    #[test]
    fn a_remote_named_nas_gets_the_backup_by_itself_and_origin_is_left_alone() {
        let (fx, nas) = fixture_with_nas();
        let mut store = EventStore::in_memory().unwrap();
        seed(&mut store);
        let out = backup_to_remote(&store, &fx.work, "thor2", true, None).unwrap();
        assert!(out.contains("pushed thor2 backup"), "{out}");
        assert_eq!(count_commits(&nas, "main"), 2, "the NAS has the initial commit and the backup");
        assert_eq!(count_commits(&fx.remote, "main"), 1, "origin must not receive the backup");
    }

    #[test]
    fn the_plain_entry_point_also_follows_the_nas_remote() {
        // The hook and the scheduled task never pass a remote: they must end
        // up at the NAS anyway.
        let (fx, nas) = fixture_with_nas();
        let mut store = EventStore::in_memory().unwrap();
        seed(&mut store);
        backup_to_repo(&store, &fx.work, "thor2", true).unwrap();
        assert_eq!(count_commits(&nas, "main"), 2);
        assert_eq!(count_commits(&fx.remote, "main"), 1);
    }

    #[test]
    fn a_remote_asked_for_by_name_beats_the_nas_default() {
        let (fx, nas) = fixture_with_nas();
        let mut store = EventStore::in_memory().unwrap();
        seed(&mut store);
        backup_to_remote(&store, &fx.work, "thor2", true, Some("origin")).unwrap();
        assert_eq!(count_commits(&fx.remote, "main"), 2);
        assert_eq!(count_commits(&nas, "main"), 1);
    }

    #[test]
    fn a_remote_that_does_not_exist_is_refused_with_the_fix() {
        let fx = make_git_fixture();
        let mut store = EventStore::in_memory().unwrap();
        seed(&mut store);
        let err = backup_to_remote(&store, &fx.work, "thor2", true, Some("vault")).unwrap_err().to_string();
        assert!(err.contains("no remote named 'vault'") && err.contains("git remote add vault"), "{err}");
    }

    #[test]
    fn a_commit_that_never_reached_the_nas_is_pushed_on_the_next_run() {
        let (fx, nas) = fixture_with_nas();
        let mut store = EventStore::in_memory().unwrap();
        seed(&mut store);
        git_ok(&fx.work, &["remote", "set-url", "--push", "nas", "/no/such/path"]);
        assert!(backup_to_remote(&store, &fx.work, "thor2", true, None).is_err());
        assert_eq!(count_commits(&nas, "main"), 1);
        git_ok(&fx.work, &["remote", "set-url", "--push", "nas", nas.to_str().unwrap()]);
        let second = backup_to_remote(&store, &fx.work, "thor2", false, None).unwrap();
        assert!(second.contains("pushed 1 backup commit(s) that had not reached the remote"), "{second}");
        assert_eq!(count_commits(&nas, "main"), 2);
    }

    #[test]
    fn the_target_names_the_nas_remote_and_its_address() {
        let (fx, nas) = fixture_with_nas();
        let target = resolve_target(&fx.work, None).unwrap();
        assert_eq!(target.remote, "nas");
        assert_eq!(target.branch, "main");
        assert_eq!(target.tracking.as_deref(), Some("refs/remotes/nas/main"));
        assert_eq!(remote_url(&fx.work, "nas").as_deref(), Some(nas.to_str().unwrap()));
    }
}
