//! Maintenance pass: walk the store and give every fact that owes a proof
//! (`serve::prove::teeth_box`'s own applicability rule - heavy, or a
//! refusing-kind check, exactly the population `mcp::remember`/`revise`
//! already test at write time) the SAME box that write path would compute
//! right now, and record it as the reserved `model::store::TEETH_*_TAG` the
//! write path itself uses. This is the backfill Change 2's own top-of-file
//! doc comment promises: a store that predates Change 2 holds heavy rules
//! that were never tested at all, and rules resting on a written reason
//! with no teeth behind it - none of that needs a person to walk it by
//! hand, because the box is entirely computable from what is already
//! stored.
//!
//! IDEMPOTENT AND SAFE TO RE-RUN. An item that already carries exactly the
//! tag its box computes right now is left untouched - a second run over an
//! unchanged store reports zero changes. Every write goes through
//! `model::store::revise`, so it passes the same gate any other correction
//! does, lands as a `fact_revised` event, and is visible in `history`
//! afterwards; nothing is edited in place.
//!
//! NEVER INVENTS A PROVEN GUARD. `--root` is optional and applies to EVERY
//! item uniformly - the same simplification `mcp::remember`'s own fixed
//! startup root already makes for the identical reason (see that binary's
//! `self.root`), never a per-project checkout this tool has no way to
//! locate on its own. Without `--root`, an `Absent`/`AbsentAll` check can
//! never be tried (no root to resolve its own path against) and lands
//! honestly at `teeth:informing` ("nothing to try: no checkout root was
//! resolved") rather than being guessed at - exactly the standing rule
//! `serve::prove::teeth_box` already enforces for every caller, this one
//! included. `Forbidden`/`Requires` checks need no root at all, so they are
//! tested correctly regardless.
//!
//! `KeepRefusing` (a `Forbidden` check that structurally reaches neither an
//! `Always` binding nor a `Command` target) is not one of the four stored
//! boxes - a fresh write refuses that shape outright (`mcp::teeth_test`), so
//! it can only ever be found here on an item written before that door
//! closed. Filed as `teeth:informing`: it is not a proven guard, and it is
//! not indistinguishable from over-blocking (`refuses-legitimate-use`) or a
//! collapsed anchor either - it simply never reaches anything, which is
//! exactly what "informing only" already covers for every other reason a
//! fact cannot be tried.
//!
//! Usage:
//!     backfill_teeth --db <path>                     report what it would change
//!     backfill_teeth --db <path> --apply              make the changes
//!     backfill_teeth --db <path> --root <checkout>    also try Absent/AbsentAll checks
//!     backfill_teeth --db <path> --project <name>     limit to one project (repeatable)

use clap::Parser;
use model::store;
use serve::prove::{teeth_box, TeethBox};
use std::path::PathBuf;
use std::process::ExitCode;
use thor_core::event_store::EventStore;

#[derive(Parser)]
#[command(
    name = "backfill-teeth",
    version = env!("CARGO_PKG_VERSION"),
    about = "Compute and record the teeth box (proven guard / informing / refuses-legitimate-use / anchor-collapsed) \
             for every live fact that owes one, using the identical function the write path already runs"
)]
struct Cli {
    #[arg(long)]
    db: PathBuf,
    /// Without this, nothing is written - the run only reports what it would change.
    #[arg(long)]
    apply: bool,
    /// The checkout an Absent/AbsentAll check's own path is resolved against.
    /// Applied to EVERY item uniformly, whatever project it belongs to - see
    /// this file's own top-of-file doc comment. Omit it and every anchored
    /// check lands honestly at `teeth:informing` rather than being guessed.
    #[arg(long)]
    root: Option<PathBuf>,
    /// Limit the walk to one project's own facts (repeatable). Omit to walk
    /// every project, global included.
    #[arg(long = "project")]
    projects: Vec<String>,
}

const ACTOR: &str = "backfill-teeth";

/// The box `backfill_teeth` actually records, collapsing `TeethBox`'s five
/// write-time variants into the four STORED tags - `KeepRefusing` folds into
/// `Informing` here (see this file's own top-of-file doc comment for why),
/// and `NotApplicable` is not a box to record at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordedBox {
    Proven,
    RefusesLegitimateUse,
    AnchorCollapsed,
    Informing,
}

impl RecordedBox {
    fn tag(self) -> &'static str {
        match self {
            RecordedBox::Proven => store::TEETH_PROVEN_TAG,
            RecordedBox::RefusesLegitimateUse => store::TEETH_REFUSES_LEGITIMATE_TAG,
            RecordedBox::AnchorCollapsed => store::TEETH_ANCHOR_COLLAPSED_TAG,
            RecordedBox::Informing => store::TEETH_INFORMING_TAG,
        }
    }

    fn label(self) -> &'static str {
        match self {
            RecordedBox::Proven => "proven guard",
            RecordedBox::RefusesLegitimateUse => "refuses legitimate use",
            RecordedBox::AnchorCollapsed => "anchor collapsed",
            RecordedBox::Informing => "informing only",
        }
    }
}

/// `teeth_box`'s own five variants, collapsed to what this tool records -
/// `None` for `NotApplicable` (nothing owed, nothing to record).
fn recorded_box(box_: &TeethBox) -> Option<RecordedBox> {
    match box_ {
        TeethBox::NotApplicable => None,
        TeethBox::ProvenGuard { .. } => Some(RecordedBox::Proven),
        TeethBox::RefusesLegitimateUse { .. } => Some(RecordedBox::RefusesLegitimateUse),
        TeethBox::AnchorCollapsed { .. } => Some(RecordedBox::AnchorCollapsed),
        TeethBox::Informing { .. } | TeethBox::KeepRefusing(_) => Some(RecordedBox::Informing),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let store = match EventStore::open_existing(&cli.db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open the store at {}: {e}", cli.db.display());
            return ExitCode::FAILURE;
        }
    };

    let live = serve::live::live_items(&store);
    let scoped: Vec<_> = live
        .into_iter()
        .filter(|li| cli.projects.is_empty() || cli.projects.iter().any(|p| li.item.project.as_deref() == Some(p.as_str())))
        .collect();

    // (id, current tags, RecordedBox it computes to right now, the write
    // receipt's own note - printed in the report, never stored)
    let mut plan: Vec<(String, RecordedBox, String)> = Vec::new();
    let mut unchanged = 0usize;
    let mut not_applicable = 0usize;
    for li in &scoped {
        let box_ = teeth_box(&li.id, &li.item, cli.root.as_deref());
        let Some(recorded) = recorded_box(&box_) else {
            not_applicable += 1;
            continue;
        };
        let already_has_it = li.item.tags.iter().any(|t| t == recorded.tag());
        let carries_a_different_teeth_tag = li.item.tags.iter().any(|t| t.starts_with(store::TEETH_TAG_PREFIX) && t != recorded.tag());
        if already_has_it && !carries_a_different_teeth_tag {
            unchanged += 1;
            continue;
        }
        let note = describe(&box_);
        plan.push((li.id.clone(), recorded, note));
    }

    if plan.is_empty() {
        println!(
            "nothing to backfill: {unchanged} already carry the box they compute to right now, {not_applicable} owe no proof at all (light, no refusing-kind check)"
        );
        return ExitCode::SUCCESS;
    }

    let mut by_box: std::collections::BTreeMap<&str, usize> = Default::default();
    for (_, recorded, _) in &plan {
        *by_box.entry(recorded.label()).or_default() += 1;
    }
    println!("facts that would gain or change a teeth box: {}", plan.len());
    for (label, n) in &by_box {
        println!("  {label:24} {n}");
    }
    println!("already correct:  {unchanged}");
    println!("owe no proof:     {not_applicable}");

    if !cli.apply {
        println!("\nnothing written (pass --apply to make these changes)");
        return ExitCode::SUCCESS;
    }

    let mut store = store;
    let mut written = 0usize;
    let mut failed = 0usize;
    for (id, recorded, note) in &plan {
        let existing = match store::show(&store, id) {
            Ok(item) => item,
            Err(e) => {
                eprintln!("{id}: cannot read: {e}");
                failed += 1;
                continue;
            }
        };
        let mut updated = existing.clone();
        updated.tags.retain(|t| !t.starts_with(store::TEETH_TAG_PREFIX));
        updated.tags.push(recorded.tag().to_string());
        match store::revise(&mut store, ACTOR, ACTOR, ACTOR, &existing, &updated) {
            Ok(_) => {
                written += 1;
                println!("{id}: teeth:{:?} -> {} ({note})", existing.tags.iter().find(|t| t.starts_with(store::TEETH_TAG_PREFIX)), recorded.label());
            }
            Err(e) => {
                eprintln!("{id}: refused: {e}");
                failed += 1;
            }
        }
    }

    println!("\nwritten: {written}");
    if failed > 0 {
        eprintln!("failed:  {failed}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// A one-line, human-readable receipt for `box_` - the identical text
/// `mcp::teeth_test` would append to a fresh write's own reply, reused here
/// rather than re-derived a second way (see each `TeethBox` variant's own
/// doc comment for what it means).
fn describe(box_: &TeethBox) -> String {
    match box_ {
        TeethBox::NotApplicable => "not applicable".to_string(),
        TeethBox::KeepRefusing(msg) => msg.strip_prefix("REFUSED: ").unwrap_or(msg).to_string(),
        TeethBox::ProvenGuard { forbidden_tried, clean_tried } => {
            format!("tried forbidden: {forbidden_tried}; refused. tried clean: {clean_tried}; let through")
        }
        TeethBox::RefusesLegitimateUse { note, .. }
        | TeethBox::AnchorCollapsed { note, .. }
        | TeethBox::Informing { note, .. } => note.clone(),
    }
}
