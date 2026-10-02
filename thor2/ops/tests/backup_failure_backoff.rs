//! The real `backup` binary, run the way the SessionStart hook runs it, against
//! a backup repository whose remote cannot be reached: the first run fails
//! loudly and remembers it, the next run inside the hour does not retry (exit
//! 0, one line), and `doctor` names the failure plainly.

use std::path::Path;
use std::process::Command;
use thor_core::event_store::{EventKind, EventStore};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn run_backup(db: &Path, repo: &Path, extra: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_backup"))
        .arg("--db")
        .arg(db)
        .arg("--repo")
        .arg(repo)
        .args(extra)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn broken_backup_setup() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let remote = dir.path().join("remote.git");
    let work = dir.path().join("acme-backup");
    git(dir.path(), &["init", "--bare", remote.to_str().unwrap()]);
    let out = Command::new("git").args(["clone", remote.to_str().unwrap(), work.to_str().unwrap()]).output().unwrap();
    assert!(out.status.success());
    git(&work, &["checkout", "-b", "main"]);
    git(&work, &["config", "user.email", "test@example.invalid"]);
    git(&work, &["config", "user.name", "Test"]);
    git(&work, &["commit", "--allow-empty", "-m", "initial"]);
    git(&work, &["push", "-u", "origin", "main"]);
    // From here on the remote cannot be reached.
    git(&work, &["remote", "set-url", "origin", dir.path().join("gone.git").to_str().unwrap()]);
    let db = dir.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    store.append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "body").unwrap();
    drop(store);
    (dir, db, work)
}

#[test]
fn a_failed_backup_is_not_retried_at_every_session_start_and_doctor_names_it() {
    let (_dir, db, work) = broken_backup_setup();

    let (ok, _out, err) = run_backup(&db, &work, &[]);
    assert!(!ok, "an unreachable remote must fail the first run");
    assert!(err.contains("backup failed"), "{err}");

    let (ok2, out2, _err2) = run_backup(&db, &work, &[]);
    assert!(ok2, "inside the back-off the hook must not fail again");
    assert!(out2.contains("next try in"), "{out2}");

    let doctor = Command::new(env!("CARGO_BIN_EXE_doctor")).arg("--db").arg(&db).output().unwrap();
    let text = String::from_utf8_lossy(&doctor.stdout).to_string();
    assert!(
        text.lines().any(|l| l.starts_with("backup to acme-backup (remote origin: ") && l.contains(") failing since")),
        "doctor must say the backup is failing: {text}"
    );

    // --force tries now, whatever the back-off says.
    let (ok3, _out3, err3) = run_backup(&db, &work, &["--force"]);
    assert!(!ok3, "forced, it tries again and fails again: {err3}");
}

/// The private backup lives on the NAS: a clone with a remote called `nas` is
/// backed up there by the plain hook command (no --remote), the doctor names
/// that target, and the GitHub-style `origin` gets nothing.
#[test]
fn the_backup_goes_to_the_nas_remote_by_itself_and_doctor_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin.git");
    let nas = dir.path().join("nas-share").join("acme-backup.git");
    let work = dir.path().join("acme-backup");
    git(dir.path(), &["init", "--bare", origin.to_str().unwrap()]);
    std::fs::create_dir_all(nas.parent().unwrap()).unwrap();
    git(dir.path(), &["init", "--bare", nas.to_str().unwrap()]);
    let out = Command::new("git").args(["clone", origin.to_str().unwrap(), work.to_str().unwrap()]).output().unwrap();
    assert!(out.status.success());
    git(&work, &["checkout", "-b", "main"]);
    git(&work, &["config", "user.email", "test@example.invalid"]);
    git(&work, &["config", "user.name", "Test"]);
    git(&work, &["commit", "--allow-empty", "-m", "initial"]);
    git(&work, &["push", "-u", "origin", "main"]);
    git(&work, &["remote", "add", "nas", nas.to_str().unwrap()]);
    git(&work, &["push", "nas", "main"]);
    let db = dir.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    store.append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "body").unwrap();
    drop(store);

    let (ok, out, err) = run_backup(&db, &work, &[]);
    assert!(ok, "{out} {err}");
    assert!(out.contains("pushed thor2 backup") && out.contains("(to nas)"), "{out}");
    let count = |git_dir: &Path| {
        let o = Command::new("git").arg("-C").arg(git_dir).args(["rev-list", "--count", "main"]).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    assert_eq!(count(&nas), "2");
    assert_eq!(count(&origin), "1", "origin must get nothing");

    let doctor = Command::new(env!("CARGO_BIN_EXE_doctor")).arg("--db").arg(&db).output().unwrap();
    let text = String::from_utf8_lossy(&doctor.stdout).to_string();
    assert!(
        text.lines().any(|l| l.starts_with("backup: last copy went to acme-backup (remote nas: ") && l.contains("0h ago")),
        "doctor must name the NAS target: {text}"
    );

    // A second run inside the debounce window pushes nothing and says so, and
    // the doctor line keeps the time of the copy that really happened.
    let (ok2, out2, _) = run_backup(&db, &work, &[]);
    assert!(ok2 && out2.contains("skipping"), "{out2}");
    // An unknown remote is refused with the fix.
    let (ok3, _o3, err3) = run_backup(&db, &work, &["--remote", "vault", "--force"]);
    assert!(!ok3);
    assert!(err3.contains("no remote named 'vault'"), "{err3}");
}
