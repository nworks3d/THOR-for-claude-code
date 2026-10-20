//! Off-site backup of the event log into a git clone.
//!
//! The backup IS the log, exported as canonical append-only JSONL (see
//! `thor_core::backup`), cut into numbered segments (`events-0001.jsonl`, ...)
//! so no single file reaches the size GitHub refuses. This binary is only the
//! CLI around it: it opens the store, names the subdirectory to write into,
//! and prints what happened. A failed attempt is remembered
//! (`ops::backup_state`) so the next session starts do not retry for an hour
//! and `doctor` can say the off-site copy is stale.
//!
//! Why the subdirectory has a default that is NOT "thor": a 2.0 store is a
//! different hash chain over the same memory, and 1.0's backup already owns
//! `thor/events.jsonl` in the same repository. Sharing that file would replace
//! the fallback with a log that does not continue it. So 2.0 writes to `thor2/`
//! and both chains survive side by side for as long as they need to.

use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;
use thor_core::backup;
use thor_core::event_store::EventStore;

#[derive(Parser)]
#[command(
    name = "backup",
    version = env!("CARGO_PKG_VERSION"),
    about = "Export the event log into a git clone, then commit and push it"
)]
struct Cli {
    /// Path to the store to back up, or - with --restore-from - the FRESH store
    /// to rebuild into.
    #[arg(long)]
    db: PathBuf,
    /// Path to an existing git clone (it needs a remote to push to).
    /// Required for a backup; ignored when restoring.
    #[arg(long)]
    repo: Option<PathBuf>,
    /// The name of the remote to push to. Without it, a remote called `nas` is
    /// used when the clone has one (the private backup lives on the NAS), else
    /// the remote the branch already follows, else `origin`.
    #[arg(long)]
    remote: Option<String>,
    /// Rebuild a store from an exported log instead of backing one up: point it
    /// at the `thor2` folder of this repo (it reads every events-NNNN.jsonl in
    /// order). An old single `events.jsonl` still works.
    ///
    /// WHY THIS FLAG EXISTS. `thor_core::backup::restore_jsonl` was written,
    /// tested and reachable from nowhere: 2.0 could write a backup every session
    /// and had no way on earth to read one back. A backup you cannot restore is
    /// a promise, not a fallback - found 2026-08-15 while documenting the
    /// restore steps and discovering there were none.
    ///
    /// Restores only into a store that does not exist yet or is empty, and
    /// verifies every replayed hash against the recorded one, so a log that was
    /// tampered with fails loudly instead of quietly becoming your memory.
    #[arg(long = "restore-from")]
    restore_from: Option<PathBuf>,
    /// Directory inside the repo to write into. One plain name, no slashes.
    #[arg(long, default_value = "thor2")]
    subdir: String,
    /// Back up even if the last one was recent. Without this, a run inside the
    /// debounce window prints why it did nothing and exits 0 - that is a hook
    /// behaving, not a failure.
    #[arg(long)]
    force: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Restore mode, checked first: it is the one path that must work when the
    // live store is gone, so it never opens an existing store.
    if let Some(from) = &cli.restore_from {
        let mut store = match EventStore::new(&cli.db) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cannot create the store at {}: {e}", cli.db.display());
                return ExitCode::FAILURE;
            }
        };
        return match backup::restore_path(&mut store, from) {
            Ok(n) => {
                println!("restored {n} events into {}", cli.db.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("restore failed: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let store = match EventStore::open_existing(&cli.db) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("cannot open the store at {}: {e}", cli.db.display());
            return ExitCode::FAILURE;
        }
    };
    let Some(repo) = cli.repo.clone() else {
        eprintln!("--repo is required to back up (or pass --restore-from to rebuild a store instead)");
        return ExitCode::FAILURE;
    };
    if !repo.join(".git").exists() {
        eprintln!(
            "{} is not a git clone (no .git) - point --repo at a clone that already has an origin",
            repo.display()
        );
        return ExitCode::FAILURE;
    }

    // A backup is a write and a declaration, so it fails loudly (CONTRACT R5).
    // THE SECOND LANE TRAVELS WITH THE FIRST. Until 2026-08-18 the hourly
    // backup carried the event log and nothing else, so the library - the
    // owner's recipes, books, training log - existed in exactly one file on
    // one machine. Written before the export below, so both land in the same
    // commit; a debounced run that commits nothing simply leaves a fresh file
    // for the next one.
    let now = ops::backup_state::now_unix();
    // After a failed attempt, wait before trying again instead of paying the
    // same failure at every session start. Exit 0: the hook behaved; `doctor`
    // is where the failure is named.
    if !cli.force {
        if let Some(left) = ops::backup_state::backoff_left_at(&cli.db, now) {
            println!(
                "backup: the last attempt failed, next try in {} min (run with --force to try now; doctor names the reason)",
                left.div_ceil(60)
            );
            return ExitCode::SUCCESS;
        }
    }
    let library_path = library::default_path(&cli.db);
    if library_path.exists() {
        let out_dir = repo.join(&cli.subdir);
        match std::fs::create_dir_all(&out_dir)
            .map_err(|e| e.to_string())
            .and_then(|()| library::Library::open(&library_path))
            .and_then(|lib| {
                let mut f = std::fs::File::create(out_dir.join("library.jsonl")).map_err(|e| e.to_string())?;
                lib.export_jsonl(&mut f)
            }) {
            Ok(n) => println!("library: {n} line(s) exported"),
            // Never let this stop the event-log backup: half a backup beats
            // none, and the failure is printed rather than swallowed.
            Err(e) => eprintln!("library: NOT exported ({e}) - the event log backup still runs"),
        }
    }

    // The owner's reply rulebook travels too, so a copy made from this backup
    // tidies replies the same way. Plain file copy; a failure never stops the backup.
    match ops::pull::carry_rulebook(&cli.db, &repo.join(&cli.subdir)) {
        Ok(true) => println!("reply rulebook: carried in the backup"),
        Ok(false) => {}
        Err(e) => eprintln!("reply rulebook: NOT carried ({e}) - the event log backup still runs"),
    }

    // Where this run pushes, named once so the result and the doctor can say it.
    //
    // A repository with no remote at all is a deliberate local copy
    // (`install --set-backup local`): it names no remote, and the copy stays on
    // this machine.
    let local_only = !backup::has_remotes(&repo);
    let (remote_name, address) = if local_only {
        (String::new(), String::new())
    } else {
        match backup::resolve_target(&repo, cli.remote.as_deref()) {
            Ok(target) => {
                let address = backup::remote_url(&repo, &target.remote).unwrap_or_default();
                (target.remote, address)
            }
            Err(e) => {
                let _ = ops::backup_state::record_failure_at(&cli.db, &repo, &e.to_string(), now);
                eprintln!("backup failed: {e}");
                return ExitCode::FAILURE;
            }
        }
    };
    let requested = (!local_only).then_some(remote_name.as_str());
    match backup::backup_to_remote(&store, &repo, &cli.subdir, cli.force, requested) {
        Ok(line) => {
            // "backup is Nh old ... skipping" sent nothing: the time of the last copy stays.
            let pushed = !line.starts_with("backup is ");
            ops::backup_state::record_success_for(&cli.db, &repo, &remote_name, &address, ops::backup_state::now_unix(), pushed);
            if local_only {
                println!("{line}");
            } else {
                println!("{line} (to {remote_name})");
            }
            // The person who only ever reads this one line (this binary runs
            // far more often than `doctor` does) still learns the copy on
            // the NAS has gone stale - see `ops::health::ship_line`'s own
            // doc comment for why it prints nothing at all when this machine
            // was never the one doing the shipping.
            if let Some(ship) = ops::health::ship_line(&cli.db) {
                println!("{ship}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            let _ = ops::backup_state::record_failure_for(&cli.db, &repo, &remote_name, &address, &e.to_string(), now);
            eprintln!("backup failed: {e}");
            ExitCode::FAILURE
        }
    }
}
