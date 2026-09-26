//! Maintenance pass: walk the store and give every fact that owes a proof
//! (`serve::prove::teeth_box`'s own applicability rule - heavy, or a
//! refusing-kind check, exactly the population `mcp::remember`/`revise`
//! already test at write time) the SAME box that write path would compute
//! right now, and record it as the reserved `model::store::TEETH_*_TAG` the
//! write path itself uses.
//!
//! WHAT A LABEL IS, AND IS NOT (2026-09-26). A label records what trying the
//! teeth found; it never answers the teeth question. Only a check that has
//! bitten a real call on record (`teeth:proven-guard`, judged against the log -
//! see `serve::prove`'s "against reality" section) or a reason that quotes the
//! legitimate call or names the rule's trigger settles a heavy rule for the
//! Stop hook and the evaluation-filing gate. Running this pass therefore
//! changes no debt by itself: labelling every heavy rule "informing" used to
//! lift the filing gate for a whole store at once, and it no longer does.
//!
//! IDEMPOTENT AND SAFE TO RE-RUN. An item that already carries exactly the
//! tag its box computes right now is left untouched - a second run over an
//! unchanged store reports zero changes. Every write goes through
//! `model::store::revise`, so it passes the same gate any other correction
//! does, lands as a `fact_revised` event, and is visible in `history`
//! afterwards; nothing is edited in place.
//!
//! THE DRY RUN PREDICTS WHAT THE GATE WILL REFUSE (2026-09-26). It used to
//! report only what it would label, and the real run then refused hundreds
//! of those writes on grounds the dry run never looked at (a heavy rule with
//! no answer to the teeth question, a full anchor). The dry run now runs the
//! same pure checks the write would - `model::gate::revise` and
//! `model::store::capacity` - and reports the ones that would be refused,
//! with the reason, so the numbers it prints are the numbers `--apply` gets.
//!
//! NEVER GUESSES A FILE CHECK (2026-09-26). Without `--root`, an `Absent`/
//! `AbsentAll` check cannot be tried against its own file. It used to be
//! labelled `teeth:informing` anyway, which read as "tried, and nothing
//! there" while nothing had been tried at all. Such an item is now SKIPPED
//! and counted as needing `--root` - unless the log already proves it (a real
//! refusal under this very check), which needs no working copy.
//!
//! `KeepRefusing` (a `Forbidden` check that structurally reaches neither an
//! `Always` binding nor a `Command` target) is not one of the stored boxes -
//! a fresh write refuses that shape outright (`mcp::teeth_test`), so it can
//! only ever be found here on an item written before that door closed. Filed
//! as `teeth:informing`: it simply never reaches anything.
//!
//! Usage:
//!     backfill_teeth --db <path>                     report what it would change, and what would be refused
//!     backfill_teeth --db <path> --apply              make the changes
//!     backfill_teeth --db <path> --root <checkout>    also try Absent/AbsentAll checks
//!     backfill_teeth --db <path> --project <name>     limit to one project (repeatable)

use clap::Parser;
use model::item::Check;
use model::store;
use serve::prove::{teeth_box, TeethBox};
use std::path::PathBuf;
use std::process::ExitCode;
use thor_core::event_store::EventStore;

#[derive(Parser)]
#[command(
    name = "backfill_teeth",
    version = env!("CARGO_PKG_VERSION"),
    about = "Compute and record the teeth box (proven guard / wired, not proven / informing / refuses-legitimate-use / \
             anchor-collapsed) for every live fact that owes one, using the identical function the write path runs"
)]
struct Cli {
    #[arg(long)]
    db: PathBuf,
    /// Without this, nothing is written - the run only reports what it would change.
    #[arg(long)]
    apply: bool,
    /// The checkout an Absent/AbsentAll check's own path is resolved against.
    /// Applied to EVERY item uniformly, whatever project it belongs to. Omit
    /// it and every anchored check the log cannot already prove is skipped
    /// and counted, never labelled.
    #[arg(long)]
    root: Option<PathBuf>,
    /// Limit the walk to one project's own facts (repeatable). Omit to walk
    /// every project, global included.
    #[arg(long = "project")]
    projects: Vec<String>,
}

const ACTOR: &str = "backfill_teeth";

/// The box `backfill_teeth` actually records - `KeepRefusing` folds into
/// `Informing` here (see this file's own top-of-file doc comment for why),
/// and `NotApplicable` is not a box to record at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordedBox {
    Proven,
    Wired,
    RefusesLegitimateUse,
    AnchorCollapsed,
    Informing,
}

impl RecordedBox {
    fn tag(self) -> &'static str {
        match self {
            RecordedBox::Proven => store::TEETH_PROVEN_TAG,
            RecordedBox::Wired => store::TEETH_WIRED_TAG,
            RecordedBox::RefusesLegitimateUse => store::TEETH_REFUSES_LEGITIMATE_TAG,
            RecordedBox::AnchorCollapsed => store::TEETH_ANCHOR_COLLAPSED_TAG,
            RecordedBox::Informing => store::TEETH_INFORMING_TAG,
        }
    }

    fn label(self) -> &'static str {
        match self {
            RecordedBox::Proven => "proven guard",
            RecordedBox::Wired => "wired, not proven",
            RecordedBox::RefusesLegitimateUse => "refuses legitimate use",
            RecordedBox::AnchorCollapsed => "anchor collapsed",
            RecordedBox::Informing => "informing only",
        }
    }
}

/// `teeth_box`'s own variants, collapsed to what this tool records - `None`
/// for `NotApplicable` (nothing owed, nothing to record).
fn recorded_box(box_: &TeethBox) -> Option<RecordedBox> {
    match box_ {
        TeethBox::NotApplicable => None,
        TeethBox::ProvenGuard { .. } => Some(RecordedBox::Proven),
        TeethBox::Wired { .. } => Some(RecordedBox::Wired),
        TeethBox::RefusesLegitimateUse { .. } => Some(RecordedBox::RefusesLegitimateUse),
        TeethBox::AnchorCollapsed { .. } => Some(RecordedBox::AnchorCollapsed),
        TeethBox::Informing { .. } | TeethBox::KeepRefusing(_) => Some(RecordedBox::Informing),
    }
}

/// An anchored check that can only be tried against a working copy.
fn needs_a_working_copy(check: Option<&Check>) -> bool {
    matches!(check, Some(Check::Absent { .. }) | Some(Check::AbsentAll { .. }))
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

    // (id, RecordedBox it computes to right now, the write receipt's own note)
    let mut plan: Vec<(String, RecordedBox, String)> = Vec::new();
    let mut unchanged = 0usize;
    let mut not_applicable = 0usize;
    let mut need_root = 0usize;
    for li in &scoped {
        let reality = if li.item.check.is_some() {
            serve::prove::reality(&store, &li.id, li.item.check.as_ref())
        } else {
            serve::prove::Reality::default()
        };
        let box_ = teeth_box(&li.id, &li.item, cli.root.as_deref(), &reality);
        let Some(recorded) = recorded_box(&box_) else {
            not_applicable += 1;
            continue;
        };
        if cli.root.is_none() && needs_a_working_copy(li.item.check.as_ref()) && recorded != RecordedBox::Proven {
            need_root += 1;
            continue;
        }
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
            "nothing to backfill: {unchanged} already carry the box they compute to right now, {not_applicable} owe no proof at all (light, no refusing-kind check), {need_root} need --root to be tried"
        );
        return ExitCode::SUCCESS;
    }

    // The same pure checks the write itself runs, so the dry run predicts
    // what `--apply` will be refused rather than leaving it to find out.
    let mut would_refuse: Vec<(String, String)> = Vec::new();
    for (id, recorded, _) in &plan {
        let Ok(existing) = store::show(&store, id) else { continue };
        let updated = relabelled(&existing, *recorded);
        let refusal = match model::gate::revise(&existing, &updated) {
            Err(r) => Some(r.to_string()),
            Ok(()) => match store::capacity(&store, &updated) {
                Ok(store::Capacity::DeadOnArrival(r)) | Ok(store::Capacity::Full(r)) => Some(r.to_string()),
                _ => None,
            },
        };
        if let Some(r) = refusal {
            would_refuse.push((id.clone(), r));
        }
    }

    let mut by_box: std::collections::BTreeMap<&str, usize> = Default::default();
    for (_, recorded, _) in &plan {
        *by_box.entry(recorded.label()).or_default() += 1;
    }
    println!("facts that would gain or change a teeth box: {}", plan.len());
    for (label, n) in &by_box {
        println!("  {label:24} {n}");
    }
    println!("of those, the write gate would refuse: {}", would_refuse.len());
    println!("already correct:  {unchanged}");
    println!("owe no proof:     {not_applicable}");
    println!("need --root:      {need_root} (a file check the log does not already prove; not labelled)");

    if !cli.apply {
        for (id, why) in &would_refuse {
            println!("would be refused: {id}: {why}");
        }
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
        let updated = relabelled(&existing, *recorded);
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

/// `existing` with its teeth label replaced by `recorded`'s - the one change
/// this pass ever makes.
fn relabelled(existing: &model::item::Item, recorded: RecordedBox) -> model::item::Item {
    let mut updated = existing.clone();
    updated.tags.retain(|t| !t.starts_with(store::TEETH_TAG_PREFIX));
    updated.tags.push(recorded.tag().to_string());
    updated
}

/// A one-line, human-readable receipt for `box_` - the identical text
/// `mcp::teeth_test` would append to a fresh write's own reply, reused here
/// rather than re-derived a second way (see each `TeethBox` variant's own
/// doc comment for what it means).
fn describe(box_: &TeethBox) -> String {
    match box_ {
        TeethBox::NotApplicable => "not applicable".to_string(),
        TeethBox::KeepRefusing(msg) => msg.strip_prefix("REFUSED: ").unwrap_or(msg).to_string(),
        TeethBox::ProvenGuard { evidence, .. } => evidence.clone(),
        TeethBox::Wired { note, .. }
        | TeethBox::RefusesLegitimateUse { note, .. }
        | TeethBox::AnchorCollapsed { note, .. }
        | TeethBox::Informing { note, .. } => note.clone(),
    }
}
