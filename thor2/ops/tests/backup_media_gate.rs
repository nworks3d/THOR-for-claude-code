//! THOR's own backup must never be stopped by THOR's own media gate.
//!
//! The machine-wide `pre-commit` hook refuses a staged file above 25 MiB. The
//! backup writes export segments of up to 50 MB into a git repository and
//! commits them, so on a machine where the hook is wired the backup commit was
//! refused and the backup stopped. These tests wire the real generated
//! dispatchers (`ops::githooks`) as the machine's global hooks, then back up a
//! segment above the limit.
//!
//! Every test shares one fake home and global git config (set once, before any
//! git program starts), so the result does not depend on whose computer runs it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Once;
use thor_core::backup;
use thor_core::event_store::{EventKind, EventStore};

/// The gate's default limit is 26214400 bytes; one event this long exports to
/// a segment that is above it.
const ABOVE_THE_GATE_LIMIT: usize = 27_000_000;
const GATE_LIMIT_BYTES: u64 = 26_214_400;

fn slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Point git at a fake home whose global config wires THOR's dispatchers as
/// `core.hooksPath`, exactly as the installer does on a real machine. Runs once;
/// the other tests wait for it, so no git program starts before it is done.
fn machine_with_the_gate_wired() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join("backup-media-gate-home");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let hooks = home.join("hooks");
        ops::githooks::install_git_hooks(&hooks, "C:/fake/codeindex.exe", "C:/fake/codeindex").unwrap();
        let config = home.join("gitconfig");
        std::fs::write(&config, format!("[core]\n\thooksPath = {}\n", slashes(&hooks))).unwrap();
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        std::env::set_var("XDG_CONFIG_HOME", &home);
        std::env::set_var("GIT_CONFIG_GLOBAL", &config);
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    });
}

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git").arg("-C").arg(dir).args(args).output().expect("git must be on PATH")
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// The repository's own local value of the gate switch, as git reads a boolean.
fn local_gate_switch(repo: &Path) -> Option<String> {
    let out = git(repo, &["config", "--local", "--bool", "--get", "thor.mediagate.enabled"]);
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn identify(repo: &Path) {
    git_ok(repo, &["config", "user.email", "test@example.invalid"]);
    git_ok(repo, &["config", "user.name", "Test"]);
}

/// A store whose export is one segment above the gate's limit.
fn store_with_a_segment_above_the_gate_limit() -> EventStore {
    let mut store = EventStore::in_memory().unwrap();
    store
        .append_event("s", "l", "act", EventKind::FactCreated, "e1", None, &"x".repeat(ABOVE_THE_GATE_LIMIT))
        .unwrap();
    store
}

/// A plain repository with the big file staged: what the gate exists to refuse.
fn refused_by_the_gate(dir: &Path) {
    let repo = dir.join("control");
    git_ok(dir, &["init", "--quiet", repo.to_str().unwrap()]);
    identify(&repo);
    std::fs::write(repo.join("big.bin"), vec![b'x'; ABOVE_THE_GATE_LIMIT]).unwrap();
    git_ok(&repo, &["add", "big.bin"]);
    let out = git(&repo, &["commit", "--quiet", "-m", "big"]);
    assert!(!out.status.success(), "the control commit must be refused, or this test proves nothing");
    assert!(stderr_of(&out).contains("THOR stopped this commit"), "{}", stderr_of(&out));
}

/// A backup repository made the way it was before the exemption existed: a bare
/// remote and a clone on `main`, no local gate setting.
struct OldBackupRepo {
    dir: tempfile::TempDir,
    remote: PathBuf,
    work: PathBuf,
}

fn old_backup_repo() -> OldBackupRepo {
    machine_with_the_gate_wired();
    let dir = tempfile::tempdir().unwrap();
    let remote = dir.path().join("remote.git");
    let work = dir.path().join("work");
    git_ok(dir.path(), &["init", "--quiet", "--bare", remote.to_str().unwrap()]);
    git_ok(dir.path(), &["clone", "--quiet", remote.to_str().unwrap(), work.to_str().unwrap()]);
    git_ok(&work, &["checkout", "-b", "main"]);
    identify(&work);
    git_ok(&work, &["commit", "--quiet", "--allow-empty", "-m", "initial"]);
    git_ok(&work, &["push", "--quiet", "-u", "origin", "main"]);
    OldBackupRepo { dir, remote, work }
}

/// Put a hook of the repository's own in `.git/hooks/pre-commit`.
fn own_pre_commit(repo: &Path, body: &str) {
    let hook = repo.join(".git").join("hooks").join("pre-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// THE DEFECT: the gate is wired, the segment is above its limit, and the
/// backup commit used to be refused. The same file in a plain commit still is,
/// and the repository's own pre-commit hook still runs for the backup commit.
#[test]
fn a_backup_segment_above_the_gate_limit_is_committed_and_pushed() {
    let fx = old_backup_repo();
    let store = store_with_a_segment_above_the_gate_limit();
    let marker = fx.dir.path().join("own-hook-ran.txt");
    own_pre_commit(&fx.work, &format!("#!/bin/sh\necho ran > \"{}\"\n", slashes(&marker)));

    // Control: the very same file, committed by hand, is refused by the gate,
    // before the repository's own hook gets a turn.
    let out_dir = fx.work.join("thor2");
    std::fs::create_dir_all(&out_dir).unwrap();
    backup::export_segments(&store, &out_dir, backup::SEGMENT_MAX_BYTES).unwrap();
    assert_eq!(local_gate_switch(&fx.work), None, "nothing has exempted this repository yet");
    git_ok(&fx.work, &["add", "thor2/"]);
    let refused = git(&fx.work, &["commit", "--quiet", "-m", "by hand"]);
    assert!(!refused.status.success(), "the gate must refuse a segment above its limit");
    assert!(stderr_of(&refused).contains("THOR stopped this commit"), "{}", stderr_of(&refused));
    assert!(!marker.exists(), "the gate runs before the repository's own hook");
    git_ok(&fx.work, &["reset", "--quiet"]);

    // The backup itself gets through.
    let line = backup::backup_to_repo(&store, &fx.work, "thor2", true).unwrap();
    assert!(line.starts_with("pushed thor2 backup ("), "{line}");
    assert_eq!(local_gate_switch(&fx.work).as_deref(), Some("false"), "the backup exempts its own repository");
    assert!(marker.exists(), "the repository's own pre-commit hook must still run");
    let size = git_ok(&fx.remote, &["cat-file", "-s", "main:thor2/events-0001.jsonl"]);
    assert!(size.parse::<u64>().unwrap() > GATE_LIMIT_BYTES, "the segment on the remote is above the gate's limit: {size}");
}

/// Setup (`install --set-backup`) exempts the repository it makes, so even the
/// first backup commit is never refused; the exemption stays in that repository.
#[test]
fn a_backup_folder_made_by_setup_is_exempt_and_other_repositories_are_not() {
    machine_with_the_gate_wired();
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("backup-repo");
    let prepared = ops::backup_setup::prepare_repo(&repo, None).unwrap();
    assert!(prepared.created);
    assert_eq!(local_gate_switch(&repo).as_deref(), Some("false"));

    let store = store_with_a_segment_above_the_gate_limit();
    let line = backup::backup_to_repo(&store, &repo, "thor2", true).unwrap();
    assert!(line.contains("local backup"), "{line}");
    let size = git_ok(&repo, &["cat-file", "-s", "HEAD:thor2/events-0001.jsonl"]);
    assert!(size.parse::<u64>().unwrap() > GATE_LIMIT_BYTES, "{size}");

    refused_by_the_gate(dir.path());
}

/// Choosing a place sets the exemption too, and running setup again changes
/// nothing.
#[test]
fn connecting_a_place_sets_the_exemption_and_a_second_run_changes_nothing() {
    machine_with_the_gate_wired();
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("backup-repo");
    let place = dir.path().join("nas-share").join("thor.git");
    let place = place.to_str().unwrap();

    ops::backup_setup::prepare_repo(&repo, Some(place)).unwrap();
    assert_eq!(local_gate_switch(&repo).as_deref(), Some("false"));
    let config = std::fs::read_to_string(repo.join(".git").join("config")).unwrap();

    let second = ops::backup_setup::prepare_repo(&repo, Some(place)).unwrap();
    assert!(!second.created);
    assert_eq!(local_gate_switch(&repo).as_deref(), Some("false"));
    assert_eq!(std::fs::read_to_string(repo.join(".git").join("config")).unwrap(), config, "a second run rewrites nothing");
}
