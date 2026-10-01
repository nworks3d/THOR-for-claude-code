//! Maintenance pass: THOR's trial, run over every live Rule/Orientation that
//! has no verdict yet - a loop over `serve::teeth::retest` and, with
//! `--apply`, `serve::teeth::apply` (actor `retest`).
//!
//! WHAT CHANGED (step 8 of the two tracks). This pass used to compute a
//! "teeth box" and write it as a label - `teeth:informing` and the like -
//! through a plain revise. A label is only a readable copy of a trial record
//! now, and the store refuses a new `teeth:` label that no record implies, so
//! the old pass could no longer write at all. It now runs the very trial a
//! write runs, against each rule's REGISTERED working copy (never a folder
//! given on the command line: `--root` is gone, because a copy passed there
//! could prove a check the real checkout does not hold), and writes what the
//! trial decides:
//!
//! - rule track (proven or wired): the rule keeps its check, with the record;
//! - note: the check (if any) is cleared, with the trial's reason as the
//!   because, and the record keeps what was tried;
//! - blocked: its project has no registered working copy, or the file differs
//!   from the default branch - nothing is written, it stays as it is;
//! - needs the agent: the trial found a fix only a writer can make (an unnamed
//!   literal, a tooth that bites, a check refusing most real calls) - nothing
//!   is written, and the line names the fix.
//!
//! IDEMPOTENT. A rule whose current trial record still matches it is skipped
//! ("already decided"), so a second run over an unchanged store writes
//! nothing. Every write is a `fact_revised` with the record, visible in
//! `history`, and lands only while the head is still the one tried.
//!
//! Usage:
//!     backfill_teeth --db <path>                     report what the trial decides, write nothing
//!     backfill_teeth --db <path> --apply              write the rule and note verdicts
//!     backfill_teeth --db <path> --project <name>     limit to one project (repeatable)

use clap::Parser;
use serve::teeth::{self, Blocker, Maintenance};
use std::path::PathBuf;
use std::process::ExitCode;
use thor_core::event_store::EventStore;

#[derive(Parser)]
#[command(
    name = "backfill_teeth",
    version = env!("CARGO_PKG_VERSION"),
    about = "Run THOR's trial over every live rule that has no verdict yet, against its registered working copy, \
             and (with --apply) write the rule or note verdict it reaches"
)]
struct Cli {
    #[arg(long)]
    db: PathBuf,
    /// Without this, nothing is written - the run only reports what the trial decides.
    #[arg(long)]
    apply: bool,
    /// Limit the walk to one project's own facts (repeatable). Omit to walk
    /// every project, global included.
    #[arg(long = "project")]
    projects: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut store = match EventStore::open_existing(&cli.db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open the store at {}: {e}", cli.db.display());
            return ExitCode::FAILURE;
        }
    };
    let corpus = serve::legit::Corpus::load(&store);
    let ids: Vec<String> = serve::live::live_items(&store)
        .into_iter()
        .filter(|li| li.item.kind.can_fire())
        .filter(|li| cli.projects.is_empty() || cli.projects.iter().any(|p| li.item.project.as_deref() == Some(p.as_str())))
        .map(|li| li.id)
        .collect();

    let (mut proven, mut wired, mut notes, mut blocked, mut needs_agent, mut decided) = (0, 0, 0, 0, 0, 0);
    let mut to_write: Vec<teeth::Retest> = Vec::new();
    for id in &ids {
        if !matches!(model::store::current_verdict(&store, id), model::store::CurrentVerdict::Undecided { .. }) {
            decided += 1;
            continue;
        }
        let retest = match teeth::retest(&store, Some(cli.db.as_path()), id, &corpus) {
            Ok(r) => r,
            Err(why) => {
                println!("{id}: cannot be tried - {why}");
                needs_agent += 1;
                continue;
            }
        };
        let first = retest.lines.first().cloned().unwrap_or_default();
        match &retest.outcome {
            Maintenance::Rule { proven: true, .. } => {
                proven += 1;
                println!("{id}: rule track, proven - {first}");
            }
            Maintenance::Rule { proven: false, .. } => {
                wired += 1;
                println!("{id}: rule track, wired - {first}");
            }
            Maintenance::Note { because } => {
                notes += 1;
                println!("{id}: note - {because}");
            }
            Maintenance::Blocked(Blocker::NoWorkingCopy { project }) => {
                blocked += 1;
                println!("{id}: blocked - '{project}' has no registered working copy; nothing written");
            }
            Maintenance::Blocked(Blocker::WorkingTreeDiffers { path }) => {
                blocked += 1;
                println!("{id}: blocked - {path} differs from the default branch in the registered checkout; nothing written");
            }
            Maintenance::Blocked(Blocker::AnchorFull { path }) => {
                blocked += 1;
                println!("{id}: blocked - {path} is already full, so the rule cannot be anchored to it; nothing written");
            }
            Maintenance::NeedsAgent(fix) => {
                needs_agent += 1;
                println!("{id}: needs the agent - {fix}");
            }
        }
        if retest.write.is_some() {
            to_write.push(retest);
        }
    }

    println!("rule track, proven: {proven}");
    println!("rule track, wired:  {wired}");
    println!("note:               {notes}");
    println!("blocked:            {blocked} (nothing is written for these)");
    println!("needs the agent:    {needs_agent} (nothing is written for these)");
    println!("already decided:    {decided}");

    if !cli.apply {
        println!("\nnothing written (pass --apply to write the {} rule and note verdict(s) above)", to_write.len());
        return ExitCode::SUCCESS;
    }

    let (mut written, mut failed) = (0usize, 0usize);
    for retest in &to_write {
        match teeth::apply_as_retest(&mut store, retest) {
            Ok(event) => {
                written += 1;
                println!("{}: written (event seq {})", retest.id, event.seq);
            }
            Err(e) => {
                failed += 1;
                eprintln!("{}: not written - {e}", retest.id);
            }
        }
    }
    println!("\nwritten: {written}");
    if failed > 0 {
        eprintln!("not written: {failed}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
