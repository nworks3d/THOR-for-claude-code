//! The write port into `core`: an item is stored as canonical JSON in the
//! body of an event, and nothing here ever composes or parses a prose
//! footer (see `FINDINGS-1.0.md` F1; `model/tests/no_footer_calls.rs`
//! enforces this by grepping this crate's own source).
//!
//! Two failure policies, as the workspace design note requires: this module
//! is the "write and declare" side, so every error type here is surfaced to
//! the caller, never swallowed.

use crate::gate::{self, Refusal};
use crate::item::{Binding, Check, Item, Kind, Severity};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use thor_core::cas::compute_head_sets;
use thor_core::event_store::{Event, EventKind, EventStore};

/// Everything that can go wrong declaring or revising an item. Never
/// swallowed: every variant is meant to be shown to the writer.
#[derive(Debug)]
pub enum WriteError {
    /// The write gate refused the item before anything was appended.
    Refused(Refusal),
    /// Canonical serialisation failed (should not happen for a well-formed
    /// `Item`; surfaced rather than unwrapped so a caller never panics).
    Serialize(serde_json::Error),
    /// The underlying log append failed (I/O, a stale parent_rev, a
    /// retracted head - see `thor_core::event_store::MutateConflict`).
    Store(anyhow::Error),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Refused(r) => write!(f, "{r}"),
            WriteError::Serialize(e) => write!(f, "could not serialize item: {e}"),
            WriteError::Store(e) => write!(f, "store error: {e}"),
        }
    }
}

impl std::error::Error for WriteError {}

/// Everything that can go wrong reading an item back.
#[derive(Debug)]
pub enum ReadError {
    /// No live event exists for this id.
    NotFound(String),
    /// The entity has more than one current head (a resolve is needed first);
    /// carries how many.
    Diverged(usize),
    /// The stored body was not valid canonical JSON for an `Item`.
    Parse(serde_json::Error),
    /// The head is a tombstone: this item was retracted, on purpose, and is no
    /// longer live. Reported by kind rather than left to fail as a parse error,
    /// so "someone decided this is wrong" never reaches a caller disguised as
    /// "the body is corrupt". Carries the reason given at retraction.
    Retracted(String),
    /// The underlying log read failed.
    Store(anyhow::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::NotFound(id) => write!(f, "no item found for id '{id}'"),
            ReadError::Diverged(n) => write!(f, "id has {n} diverged heads; resolve before reading"),
            ReadError::Parse(e) => write!(f, "stored body is not a valid item: {e}"),
            ReadError::Retracted(why) => write!(
                f,
                "this item was retracted and is no longer live ({why}); a retraction is a \
                 decision, so bring it back with a fresh declare, never by revising the tombstone"
            ),
            ReadError::Store(e) => write!(f, "store error: {e}"),
        }
    }
}

impl std::error::Error for ReadError {}

/// The canonical JSON body for an item: `serde_json::to_string` on the
/// struct directly, never through a `serde_json::Value` map, so the field
/// order is exactly the struct's declared order - deterministic by
/// construction (see `item::Item`'s doc comment and its round-trip tests).
pub fn canonical_body(item: &Item) -> Result<String, serde_json::Error> {
    serde_json::to_string(item)
}

/// Parse a stored body back into an item.
pub fn parse_body(body: &str) -> Result<Item, serde_json::Error> {
    serde_json::from_str(body)
}

/// Validate a new item against the write gate and, only if it passes,
/// append it to the log as a `fact_created` event whose body is its
/// canonical JSON. Nothing is written when the gate refuses.
/// An item with its project put through the one normalisation that decides
/// what a project identity IS (`normalize::normalize_project`). Applied on
/// every write, so a value meaning "the global tier" can never be stored as a
/// project named "global" - see that function for the near-miss that made
/// this necessary.
fn normalized(item: &Item) -> Item {
    let mut out = item.clone();
    out.project = crate::normalize::normalize_project(out.project.as_deref());
    out
}

/// How much normalised word-set overlap (Jaccard: |intersection| / |union|
/// over each text's normalised words) between a new declaration and a live
/// item of the same kind counts as the same fact told twice. 0.8 is a
/// judgement call, not a measured value - no labelled corpus of "same fact,
/// reworded" pairs exists yet to calibrate it against. It is set high enough
/// that two sentences sharing a topic but making a different point should
/// still pass, while two close paraphrases of the same fact (the defect this
/// check exists for) should not.
/// Made public so a census can ask "which pairs ALREADY in the store would
/// this rule have refused" using the one definition, rather than a second
/// one that drifts from it. The write gate refuses a near-duplicate on the
/// way in; nothing ever looked at the pairs that were already there.
pub const NEAR_DUPLICATE_JACCARD_THRESHOLD: f64 = 0.8;

/// Normalise item text for the near-duplicate comparison in `declare`:
/// lowercase, then keep only maximal runs of letters/digits as words, joined
/// by a single space. This collapses every whitespace run to one space AND
/// strips punctuation in the same pass - a punctuation character (a hyphen
/// included) acts as a word boundary rather than being deleted in place, so
/// "force-push" and "force push" normalise to the identical two words
/// instead of "force-push" gluing into one "forcepush" that would then match
/// nothing. This is a content-comparison normalisation, a different job from
/// `normalize::normalize_target` (which normalises a TARGET VALUE for
/// path/host/route identity) - it does not reuse or replace that function.
pub fn normalize_for_comparison(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The set of words in an already-normalised text, for a Jaccard comparison.
/// Splits on the single spaces `normalize_for_comparison` already collapsed
/// every gap down to.
pub fn word_set(normalized_text: &str) -> HashSet<&str> {
    normalized_text.split(' ').filter(|word| !word.is_empty()).collect()
}

/// Jaccard similarity of two word sets: |intersection| / |union|. Two empty
/// sets (two texts with no comparable word at all) count as identical rather
/// than as "no overlap" - an empty text is not a wildcard that matches
/// nothing.
pub fn jaccard_similarity(a: &HashSet<&str>, b: &HashSet<&str>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    intersection as f64 / union as f64
}

/// Every live item in the store - single current head, not a retraction
/// tombstone, body parses as a valid `Item` - as `(id, item)` pairs in
/// ascending id order (deterministic; never hashmap order). Only
/// `find_near_duplicate` (below) calls this.
///
/// Mirrors the same live/single-head/non-retracted rule `show` (above) and
/// `serve::live::live_items` already use, reimplemented here rather than
/// imported because `model` cannot depend on `serve` (the dependency runs
/// the other way) and this change may only touch this one file.
///
/// Reads the `head_state` projection (`EventStore::projected_head_events`)
/// instead of folding the whole event log whenever that projection is
/// current (`EventStore::heads_projection_current`), and falls back to the
/// untouched old fold (`live_items_from_fold`) otherwise - the identical
/// fallback rule `serve::live::live_items` already established for this same
/// question (see `serve/src/live.rs`'s own doc comment for the read-side
/// defect this closed; this closes the matching defect on the write side).
/// Correctness therefore never depends on the projection, only speed does: a
/// bypass writer (a direct log restore that skips `append_event`, a pre-M2
/// binary, or a store this binary has never opened with a write handle)
/// simply leaves the fold as the answer until the next append catches the
/// projection up.
fn live_items_from_projection(store: &EventStore) -> anyhow::Result<Vec<(String, Item)>> {
    let rows = store.projected_head_events()?;
    Ok(rows
        .into_iter()
        .filter(|(_, head_count, _)| *head_count == 1) // diverged: not a live item, never guessed at
        .filter(|(event, _, _)| event.kind != EventKind::FactRetracted) // a tombstone is not a live item
        .filter_map(|(event, _, _)| {
            parse_body(&event.body).ok().map(|existing| (event.entity_id.clone(), existing))
        })
        .collect())
}

/// The always-correct path (see `live_items_from_projection`): the original
/// whole-log fold, byte-for-byte unchanged in behaviour. Used whenever the
/// `head_state` projection is not current, so a stale or bypassed projection
/// can never change which items a new declaration is compared against.
fn live_items_from_fold(store: &EventStore) -> anyhow::Result<Vec<(String, Item)>> {
    let events = store.get_all_events()?;
    let heads = compute_head_sets(&events);
    let by_hash: HashMap<&str, &Event> = events.iter().map(|e| (e.this_hash.as_str(), e)).collect();

    let mut ids: Vec<&String> = heads.keys().collect();
    ids.sort(); // deterministic: never depend on hashmap iteration order
    let mut out = Vec::new();
    for id in ids {
        let head_set = &heads[id];
        if head_set.heads.len() != 1 {
            continue; // diverged: not a live item, never guessed at
        }
        let head_hash = head_set.heads.iter().next().expect("len checked above");
        let Some(head_event) = by_hash.get(head_hash.as_str()) else { continue };
        if head_event.kind == EventKind::FactRetracted {
            continue; // a tombstone is not a live item
        }
        let Ok(existing) = parse_body(&head_event.body) else { continue };
        out.push((id.clone(), existing));
    }
    Ok(out)
}

/// The id of a live item of the SAME kind this new declaration would be a
/// near-duplicate of, if any - the first one found in id order
/// (deterministic; never hashmap order - see `live_items_from_projection`/
/// `live_items_from_fold`). A near-duplicate is an identical normalised
/// text, or a Jaccard word-set overlap at or above
/// `NEAR_DUPLICATE_JACCARD_THRESHOLD`. Only `declare` calls this: `revise`
/// corrects an item that is already allowed to exist, so it never runs
/// through this check.
///
/// COST: bounded by the number of LIVE entities the store holds, not by the
/// number of events ever appended, whenever the `head_state` projection is
/// current - see `live_items_from_projection`'s own doc comment for how and
/// why that is safe to trust. Either way, every live item found is still
/// parsed and normalised here to compare against the new one: no projection
/// narrows by an item's business `kind` (Rule/Orientation/...) or its
/// `text`, only by liveness (`core`'s `item_binding` table projects
/// bindings, not kind or text), so that part of the cost is unavoidable
/// without a schema change.
/// Why a new item collided with a live one. The two need different words: a
/// duplicate TEXT says "revise that item instead"; a duplicate KEY says "that
/// address is taken", and the fix is a different address, not a revise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collision {
    /// Same kind, same or near-same text.
    Text,
    /// Same kind, and a key that normalises to an existing one.
    Key,
}

fn find_near_duplicate(store: &EventStore, item: &Item) -> anyhow::Result<Option<(String, Collision)>> {
    let candidates = if store.heads_projection_current() {
        live_items_from_projection(store)?
    } else {
        live_items_from_fold(store)?
    };

    let new_normalized = normalize_for_comparison(&item.text);
    let new_words = word_set(&new_normalized);

    for (id, existing) in candidates {
        if existing.kind != item.kind {
            continue; // only a live item of the SAME kind can be a near-duplicate
        }
        // A register is found by its KEY, so two registers whose keys differ
        // only in case, spacing or a hyphen are the same address written twice
        // - and the text comparison below can never catch them, because two
        // registers about one subject legitimately hold completely different
        // rows. `by_key` returns the first match, so the second one is
        // unreachable from the moment it is written: a whole collection that
        // exists and cannot be asked for.
        //
        // Checked BEFORE the project rule below on purpose. An address is an
        // address whatever scope it was declared under, and unlike a rule there
        // is no case where two projects each legitimately need their own copy
        // of the same key.
        //
        // THE DEFECT THIS CLOSES, measured 2026-08-16: food knowledge lived in
        // three places at once ("eten", "BBQ ideetjes", and no scope), and
        // nothing at write time ever said so. This is that guard, on the one
        // field where a second spelling costs a whole collection.
        if let (Some(existing_key), Some(new_key)) = (&existing.key, &item.key) {
            if normalize_for_comparison(existing_key) == normalize_for_comparison(new_key) {
                return Ok(Some((id, Collision::Key)));
            }
        }
        // Two items scoped to DIFFERENT projects are not copies of each other,
        // however alike they read. Two repositories can genuinely carry the
        // same constraint - the same licence wording, the same release step -
        // and each needs its own, because a fact serves the project it is
        // scoped to and no other.
        //
        // THE DEFECT THIS CLOSES, and it is the exact opposite of what this
        // function is for. Comparing across every project meant the second
        // project could not have the rule at all, and the only shape that
        // satisfied both was ONE GLOBAL fact - which then fires in every
        // project on the machine, asserting something true of two of them and
        // false everywhere else. Observed 2026-08-07 with a licence rule: it
        // was global, so it fired on a GPLv3 repository claiming that
        // repository was non-commercial.
        //
        // Global against scoped is still compared, and must be: a global item
        // already fires inside every project, so a scoped copy of it really is
        // a second copy of something already being served there.
        if let (Some(existing_project), Some(new_project)) = (&existing.project, &item.project) {
            if existing_project != new_project {
                continue;
            }
        }
        let existing_normalized = normalize_for_comparison(&existing.text);
        if existing_normalized == new_normalized {
            return Ok(Some((id, Collision::Text)));
        }
        if jaccard_similarity(&new_words, &word_set(&existing_normalized)) >= NEAR_DUPLICATE_JACCARD_THRESHOLD {
            return Ok(Some((id, Collision::Text)));
        }
    }
    Ok(None)
}

/// `Some(refusal)` when a live item's own check is in `gate::
/// check_opposition` (GROUND 29) with `item`'s own check, on the same file -
/// `None` when nothing opposes it, including when `item` carries no check at
/// all, or one that names no file (`Forbidden`/`Requires` - see `crate::
/// check::named_path`). Only `declare` calls this: `revise` corrects an item
/// that is already allowed to exist - and may be the very fix an opposition
/// here asks for - so it never runs through this check, the same reasoning
/// `find_near_duplicate`'s own doc comment gives for the identical choice.
///
/// PROJECT SCOPING is strict equality, `None` matching only `None` - the
/// same rule `unsettled_neighbours` above already established, for the
/// identical reason: "a path is only unique WITHIN a project". This is NOT
/// `find_near_duplicate`'s own looser rule (which also compares a global
/// item against every project-scoped one): a check's `path` names a real
/// FILE on disk, and a global item's own path has no single real file to be
/// compared against - see `gate::check_opposition`'s own doc comment for the
/// full reasoning, and `serve/examples/check_contradictions.rs` for the
/// read-only census that measured this exact rule before this ground
/// existed.
fn find_opposing_check(store: &EventStore, item: &Item) -> anyhow::Result<Option<Refusal>> {
    let Some(new_check) = item.check.as_ref() else { return Ok(None) };
    let Some(new_path) = crate::check::named_path(new_check) else { return Ok(None) };
    let new_path = crate::normalize::normalize_target(new_path);

    let candidates = if store.heads_projection_current() {
        live_items_from_projection(store)?
    } else {
        live_items_from_fold(store)?
    };

    for (id, existing) in candidates {
        if id == item.id {
            continue; // revising your own neighbourhood is not a conflict
        }
        if existing.project != item.project {
            continue;
        }
        let Some(existing_check) = existing.check.as_ref() else { continue };
        let Some(existing_path) = crate::check::named_path(existing_check) else { continue };
        if crate::normalize::normalize_target(existing_path) != new_path {
            continue;
        }
        if let Err(refusal) = gate::check_opposition(new_check, &id, &existing.text, existing_check) {
            return Ok(Some(refusal));
        }
    }
    Ok(None)
}

/// Validate a new item against the write gate, then refuse it if it is a
/// near-duplicate of a live item of the same kind; only if both checks pass
/// is it appended to the log as a `fact_created` event whose body is its
/// canonical JSON. Nothing is written when either check refuses.
///
/// THE DEFECT THIS CLOSES. Storing a second near-identical copy of a fact
/// used to be prevented only by an instruction in a tool description telling
/// the caller to search first - a convention, not a mechanism, and it has
/// been observed failing in practice: a caller told a rule already existed
/// stored a second copy of it anyway. `find_near_duplicate` (above) is the
/// mechanism: same `kind`, and either an identical normalised text or a
/// Jaccard word-set overlap at or above `NEAR_DUPLICATE_JACCARD_THRESHOLD`,
/// is refused before anything is appended. `revise` does not call this - an
/// existing item is already allowed to exist, so correcting it is never
/// blocked by this check.
///
/// COST: `find_near_duplicate` reads the `head_state` projection instead of
/// folding the whole event log whenever that projection is current, falling
/// back to the old whole-log fold only when it is not (see
/// `find_near_duplicate`'s own doc comment) - then normalises and compares
/// the new text against every LIVE item of the same kind found that way. No
/// projection narrows by an item's business kind or text, only by liveness,
/// so every live item is still parsed.
/// A live item that shares a target with something being written now, and
/// whose own proof currently comes out FALSE.
#[derive(Debug, Clone)]
pub struct UnsettledNeighbour {
    pub id: String,
    /// The target both items are bound to, as `kind:value`.
    pub target: String,
}

/// The targets an item is bound to, as comparable `kind:value` strings.
fn target_keys(item: &Item) -> Vec<String> {
    item.bindings
        .iter()
        .filter_map(|b| match b {
            Binding::Target { kind, value } => Some(format!("{kind:?}:{value}")),
            _ => None,
        })
        .collect()
}

/// Live items sharing a target with `item` whose check currently FAILS.
///
/// WHY ONLY `Fails`, AND NEVER `CannotRun`. A `Fails` is a positive
/// statement about this root: the checker looked and the condition was not
/// true, so that neighbour is very probably stale. A `CannotRun` is the
/// absence of information - the file is not here, which usually means this
/// is simply a different checkout - and the doctrine is explicit that an
/// unresolvable check must never block anything. Blocking on it would fire
/// on every write made from the wrong directory, which is how a gate teaches
/// people to route around it.
pub fn unsettled_neighbours(
    store: &EventStore,
    item: &Item,
    root: &Path,
) -> anyhow::Result<Vec<UnsettledNeighbour>> {
    let keys = target_keys(item);
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let candidates = if store.heads_projection_current() {
        live_items_from_projection(store)?
    } else {
        live_items_from_fold(store)?
    };

    let mut out = Vec::new();
    for (id, existing) in candidates {
        if id == item.id {
            continue; // revising your own neighbourhood is not a debt
        }
        // A path is only unique WITHIN a project. `README.md` names a
        // different file in every checkout, so a fact about one repo's
        // README must never hold a write to another's hostage - and its
        // check, run against the wrong root, would report a failure that
        // says nothing about the file the writer is actually touching.
        // Caught 2026-08-06, one revise after attaching the first checks:
        // a fact about the 1.0 README would have blocked writes to this
        // repo's README, which shares only its name.
        if existing.project != item.project {
            continue;
        }
        let Some(check) = existing.check.as_ref() else { continue };
        let shared: Vec<String> = target_keys(&existing).into_iter().filter(|k| keys.contains(k)).collect();
        let Some(target) = shared.into_iter().next() else { continue };
        if crate::check::run(check, root) == crate::check::Outcome::Fails {
            out.push(UnsettledNeighbour { id, target });
        }
    }
    Ok(out)
}

/// `declare`, with a root to run the neighbourhood toll against.
///
/// THE TOLL, AND WHY IT IS HERE RATHER THAN IN A CHORE LIST SOMEWHERE.
/// Maintenance that lives beside the work is a suggestion, and suggestions
/// are exactly what an agent skips - measured on this project repeatedly, by
/// its own author and by this assistant on the day this was written. The
/// only thing that is not a suggestion is a refusal on something the writer
/// actually wants. So: adding a fact to a target where an EXISTING fact's
/// own proof has gone false is refused, and the refusal names them.
///
/// Deliberately narrow, in three ways:
/// - It is local. You pay for the one target you are touching, never for a
///   backlog somewhere else. A toll that says "clear everything first" gets
///   routed around, and then the store stops growing, which is worse than a
///   store that is slightly stale.
/// - It fires on `declare` only, never on `revise` or `retract`. Those ARE
///   the maintenance; tolling them would charge people for doing the thing.
/// - It needs a root. Without one there is nothing to run a check against
///   and the toll simply does not exist, exactly like every other check in
///   this system when its anchor does not resolve.
pub fn declare_in(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    item: &Item,
    root: Option<&Path>,
) -> Result<Event, WriteError> {
    declare_in_scoped(store, session_id, lineage_id, actor, item, root, None)
}

/// `declare_in`, for a write that may FILE an evaluation report: `scope`
/// says which session files it and where its evaluation started (see
/// `EvaluationScope`). Every other write passes `None` and is untouched.
pub fn declare_in_scoped(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    item: &Item,
    root: Option<&Path>,
    scope: Option<&EvaluationScope>,
) -> Result<Event, WriteError> {
    if let Some(root) = root {
        // A `Contains`/`Absent`/`AbsentAll` check_path may now name a
        // DIRECTORY (see `Check::Absent`'s own doc comment) - checked ONLY
        // in the one shape this can decide without breaking the existing,
        // deliberate leniency for a FILE check_path that is not written yet
        // (see the `deliberate-anchor` tag `archive` already honours for
        // that pattern): when `check_path` ALREADY resolves, right now, to a
        // real directory, `gate::check_dir_check_path_exists` re-proves it
        // is not one of the filesystem's own roots. A check_path that
        // resolves to nothing at all is left exactly as ambiguous as it
        // always was between "a file not written yet" and "a directory not
        // created yet" - this cannot tell the two apart any more than
        // `declare`'s own pure validation could, so it is not refused here;
        // see this project's own report on this change for the exact,
        // narrower gap that leaves open (a directory check_path with a typo
        // is not caught until something is written under it).
        let dir_check_path = item.check.as_ref().and_then(|check| match check {
            Check::Contains { path, .. } | Check::Absent { path, .. } | Check::AbsentAll { path, .. } => {
                Some(path.as_str())
            }
            Check::PathExists { .. } | Check::Forbidden { .. } | Check::Requires { .. } => None,
        });
        if let Some(path) = dir_check_path {
            if root.join(path).is_dir() {
                gate::check_dir_check_path_exists(path, root).map_err(WriteError::Refused)?;
            }
        }
        let stale = unsettled_neighbours(store, &normalized(item), root).map_err(WriteError::Store)?;
        if !stale.is_empty() {
            let named = stale
                .iter()
                .map(|n| format!("'{}' (on {})", n.id, n.target))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(WriteError::Refused(Refusal {
                problem: format!(
                    "the target you are anchoring to already holds {} fact(s) whose own proof now comes out FALSE: {named}. Adding a fact next to one that has gone stale is how a memory stops being worth trusting",
                    stale.len()
                ),
                fix: "settle those first, then write this. Fixing one is cheap: revise it with a corrected check (its text is kept when you do not pass any), or retract it with a reason if it is simply gone. Then this write goes through untouched".to_string(),
            }));
        }
    }
    declare_scoped(store, session_id, lineage_id, actor, item, scope)
}

pub fn declare(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    item: &Item,
) -> Result<Event, WriteError> {
    declare_scoped(store, session_id, lineage_id, actor, item, None)
}

/// `declare`, with the evaluation scope `declare_in_scoped` describes.
pub fn declare_scoped(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    item: &Item,
    scope: Option<&EvaluationScope>,
) -> Result<Event, WriteError> {
    let item = normalized(item);
    gate::declare(&item).map_err(WriteError::Refused)?;
    if let Some((existing_id, why)) = find_near_duplicate(store, &item).map_err(WriteError::Store)? {
        return Err(WriteError::Refused(match why {
            Collision::Text => Refusal {
                problem: format!("this is a near-duplicate of an existing live item (id '{existing_id}')"),
                fix: format!(
                    "revise the existing item '{existing_id}' instead of storing a second copy of the same fact"
                ),
            },
            // A different problem needs different words: the texts may share
            // nothing at all. What collides is the ADDRESS, and a second one
            // is unreachable - `by_key` answers with the first.
            Collision::Key => Refusal {
                problem: format!(
                    "an item already answers to this key, give or take case and punctuation (id '{existing_id}')"
                ),
                fix: format!(
                    "add to '{existing_id}' instead - a second item on the same key can never be reached, since a key \
                     lookup answers with the first. If this really is a different collection, give it a key that \
                     differs by more than a capital or a hyphen"
                ),
            },
        }));
    }
    // GROUND 29 (gate::check_opposition): a live item's check requiring the
    // exact opposite of this new item's own check, on the same file, is
    // refused before either near-duplicate detection or capacity gets a say
    // - see `find_opposing_check`'s own doc comment for the project-scoping
    // rule and `gate::check_opposition`'s for the full reasoning and the
    // measurement behind it.
    if let Some(refusal) = find_opposing_check(store, &item).map_err(WriteError::Store)? {
        return Err(WriteError::Refused(refusal));
    }
    // CONTRACT R1's own refusal class, unpaid until 2026-08-08: an item that
    // cannot reach a block is cover that looks real and never fires. Only the
    // provable half refuses here; the rest comes back as a note on the write
    // (see `capacity`). `Full` is the same provable half extended to a single
    // Path/Dir/Command binding that is already at capacity on its own - see
    // `full_reliable_anchor` for why that count is reliable where a Moment's
    // is not.
    if let Capacity::DeadOnArrival(refusal) | Capacity::Full(refusal) =
        capacity(store, &item).map_err(WriteError::Store)?
    {
        return Err(WriteError::Refused(refusal));
    }
    // THE EVALUATION-FILING GATE (2026-09-25) - see this file's own
    // "evaluation obligation" section, right above `OWNER_SETUP_ANSWERS_ID`.
    // A no-op for every write that is not itself an evaluation report.
    if let Some(refusal) = check_evaluation_report_filing(store, &item, scope) {
        return Err(WriteError::Refused(refusal));
    }
    let body = canonical_body(&item).map_err(WriteError::Serialize)?;
    store
        .append_event(session_id, lineage_id, actor, EventKind::FactCreated, &item.id, None, &body)
        .map_err(WriteError::Store)
}

/// Validate a revise (everything `declare` requires, plus no field the
/// existing item had may be dropped) and, only if it passes, append it as a
/// `fact_revised` event. `existing` should be the item just read with
/// `show`, so the field-preservation check has something real to compare
/// against.
pub fn revise(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    existing: &Item,
    updated: &Item,
) -> Result<Event, WriteError> {
    // BOTH sides are normalised before the dropped-field check. An item
    // already stored with project "global" is a global item; rewriting it as
    // `None` says the same thing, so it must not read as dropping the field.
    // Comparing a normalised update against an unnormalised original would
    // refuse exactly the repair that fixes the store.
    let existing = normalized(existing);
    let updated = normalized(updated);
    gate::revise(&existing, &updated).map_err(WriteError::Refused)?;
    // The same capacity refusal `declare` makes. Without this the gate was
    // one door wide: declare refused an item that could never be shown, and
    // revise walked the identical binding in through the back.
    if let Capacity::DeadOnArrival(refusal) | Capacity::Full(refusal) =
        capacity(store, &updated).map_err(WriteError::Store)?
    {
        return Err(WriteError::Refused(refusal));
    }
    // THE SAME EVALUATION-FILING GATE `declare` RUNS - see this file's own
    // "evaluation obligation" section - for the one OTHER way a write could
    // file a report: revising an EXISTING item onto the `EVALUATION_REPORT_
    // TAG` it did not carry before. Gated on the tag being NEWLY added
    // (`existing` lacked it, `updated` carries it) so an already-filed
    // report stays freely correctable afterwards (a typo fix must never be
    // held hostage to conditions that were only ever about the FILING
    // moment) - exactly the same "moment, not the item's whole life"
    // distinction `declare` itself draws.
    if newly_filed_as_evaluation(&existing, &updated) {
        if let Some(refusal) = check_evaluation_report_filing(store, &updated, None) {
            return Err(WriteError::Refused(refusal));
        }
    }
    let body = canonical_body(&updated).map_err(WriteError::Serialize)?;
    store
        .append_mutate_checked(session_id, lineage_id, actor, EventKind::FactRevised, &updated.id, None, &body)
        .map_err(WriteError::Store)
}

/// Whether this revise FILES an evaluation report for a project it was not
/// filed for before: the tag is newly added, OR a report that already
/// carried it is moved to a different project.
///
/// THE HOLE THE SECOND HALF CLOSES (2026-09-26). The filing gate ran only
/// when the tag was newly added, so an evaluation report already filed for
/// ANOTHER project, revised onto this one, counted as this project's report
/// without a single condition ever being checked here - measured on a copy
/// of a real store: an old report of one project re-scoped to another went
/// straight through. A move is a filing for the project it lands in.
fn newly_filed_as_evaluation(existing: &Item, updated: &Item) -> bool {
    let tagged = |item: &Item| item.tags.iter().any(|t| t == EVALUATION_REPORT_TAG);
    updated.kind == Kind::Report && tagged(updated) && (!tagged(existing) || existing.project != updated.project)
}

/// The `revise` entry point for a caller that can explain a WEAKENING
/// revision - see `gate::revise_weakening`'s own doc comment for the five
/// shapes it refuses without one. Everything `revise` above does, plus that
/// one extra gate and, when it fires, `because` travelling into the stored
/// event body (`body_with_because`) so `history` can show it later.
///
/// `existing` keeps its exact meaning from `revise` above - the ground-9
/// comparison baseline, which a caller may legitimately hand a
/// `ClearedFields`-blanked value for. `before` must be different: the item
/// exactly as `show` returns it, NEVER blanked, because ground 9's baseline
/// is precisely what a `check_kind ""` or `severity ""` clear hides the
/// truth behind - see `gate::weakenings`'s own doc comment for why this
/// function cannot reuse `existing` for both jobs. In every call site with
/// nothing to clear, `existing` and `before` are the same value.
///
/// `revise` above is untouched by this addition and stays the entry point
/// for every caller with nothing to explain - `pin`, `unpin`, every one-off
/// repair binary, every test written before this existed. This is strictly
/// additive.
pub fn revise_because(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    existing: &Item,
    updated: &Item,
    before: &Item,
    because: Option<&str>,
) -> Result<Event, WriteError> {
    revise_because_scoped(store, session_id, lineage_id, actor, existing, updated, before, because, None)
}

/// `revise_because`, with the evaluation scope `declare_in_scoped`
/// describes - for a revise that files an evaluation report.
#[allow(clippy::too_many_arguments)]
pub fn revise_because_scoped(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    existing: &Item,
    updated: &Item,
    before: &Item,
    because: Option<&str>,
    scope: Option<&EvaluationScope>,
) -> Result<Event, WriteError> {
    let existing = normalized(existing);
    let updated = normalized(updated);
    let before = normalized(before);
    gate::revise(&existing, &updated).map_err(WriteError::Refused)?;
    gate::revise_weakening(&before, &updated, because).map_err(WriteError::Refused)?;
    if let Capacity::DeadOnArrival(refusal) | Capacity::Full(refusal) =
        capacity(store, &updated).map_err(WriteError::Store)?
    {
        return Err(WriteError::Refused(refusal));
    }
    // The same evaluation-filing gate `revise` runs. This entry point - the
    // one the assistant's own `revise` tool uses - never ran it at all, so
    // adding the tag to an existing report, or moving a tagged report to
    // another project, filed an evaluation with no condition checked. Judged
    // against `before`, the item exactly as stored: `existing` may be a
    // cleared baseline (see this function's own doc comment) whose project
    // was blanked on purpose.
    if newly_filed_as_evaluation(&before, &updated) {
        if let Some(refusal) = check_evaluation_report_filing(store, &updated, scope) {
            return Err(WriteError::Refused(refusal));
        }
    }
    let reason = because.map(str::trim).filter(|r| !r.is_empty());
    let body = match reason {
        Some(reason) => body_with_because(&updated, reason).map_err(WriteError::Serialize)?,
        None => canonical_body(&updated).map_err(WriteError::Serialize)?,
    };
    store
        .append_mutate_checked(session_id, lineage_id, actor, EventKind::FactRevised, &updated.id, None, &body)
        .map_err(WriteError::Store)
}

/// `canonical_body`, with one extra trailing key appended: `"because":
/// "<reason>"`. Spliced onto the end of the already-serialised bytes rather
/// than merged through a `serde_json::Value` map - going through `Value`
/// would lose the declared field order `canonical_body`'s own doc comment
/// relies on (`serde_json::Map` sorts its keys unless the crate's
/// `preserve_order` feature is on, which this workspace deliberately does
/// not enable). `parse_body` (a plain `Item` deserialize, with no
/// `deny_unknown_fields`) reads a body built this way exactly as it always
/// has - the extra key is simply ignored; `because_reason` below is the one
/// place that reads it back out.
fn body_with_because(item: &Item, because: &str) -> Result<String, serde_json::Error> {
    let body = canonical_body(item)?;
    let reason = serde_json::to_string(because)?;
    debug_assert!(body.ends_with('}'), "canonical_body always produces a JSON object");
    Ok(format!("{},\"because\":{reason}}}", &body[..body.len() - 1]))
}

/// The `because` a `fact_revised` event body carries, if any - present only
/// on one written by `revise_because` for a revision `gate::revise_weakening`
/// judged a weakening; `None` for every other event, an ordinary `revise`
/// included. Mirrors `tombstone_reason` above: never fails, a body this
/// cannot parse simply carries none.
fn because_reason(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body).ok()?.get("because")?.as_str().map(str::to_string)
}

/// What the capacity check concluded about a new item's chances of ever
/// being shown.
#[derive(Debug)]
pub enum Capacity {
    /// At least one of its bindings has room, or it outranks enough rivals.
    Fine,
    /// Every binding is already full of rivals it can never outrank. The item
    /// would be stored, fire nowhere, and nothing would say so.
    DeadOnArrival(Refusal),
    /// A Path, Dir or Command binding - a place `pool_rivals` can count
    /// completely - already holds `item::MAX_ITEMS` live rivals, AT ANY
    /// WEIGHT. Distinct from `DeadOnArrival`: that variant means the item can
    /// never reach ANY block on ANY binding it carries; this one means ONE
    /// binding it names is already spoken for, regardless of what its other
    /// bindings might do. Both refuse the write the same way - see
    /// `full_reliable_anchor` for why counting every rival, not only the
    /// equal-or-heavier ones, is what closes the silent-displacement gap.
    Full(Refusal),
    /// A binding is full of rivals at the same weight or heavier. Whether
    /// this item is seen then depends on closeness at some future moment,
    /// which nobody can decide now - so it is said, not enforced.
    Crowded(String),
    /// Every binding on this item is a DIRECTORY (`is_all_dir_binding_set`) -
    /// the note is about BREADTH (this reaches every file under a whole
    /// tree, read after anything anchored at the exact file touched), never
    /// about a rival pool being full. Split out from `Crowded` 2026-09-17:
    /// the two used to share one shape, and `crowding_debt` (`bin/serve.rs`)
    /// read either one as "stored onto a place that is already full" and
    /// nagged the writer to fold, re-anchor or tag it crowded-on-purpose.
    /// Measured the same day: `claude-app-sandbox-echte-schijf-via-wmi` and
    /// `nas-naar-prullenbak-via-rename-naar-recycle`, both bound to
    /// directories only, were nagged at Stop although `serve why --file` on
    /// a file under the anchor answered "1 item(s) apply; the block would
    /// show 1" - the place was never full, so treating the advisory as a
    /// displacement was simply wrong. `Broad` carries the identical wording
    /// `Crowded` used to for this case (nothing about the note itself was
    /// wrong, only what read it), so every surface that already shows a
    /// `Crowded` note to the writer of a remember/revise shows this one the
    /// same way; only `crowding_debt`'s own displacement nag is narrowed to
    /// `Crowded` alone.
    Broad(String),
}

/// Which live items would compete with `item` at the moment `binding` stands
/// for.
///
/// `Always` is deliberately not a pool: session start serves every pinned
/// item in full and caps nothing, so a pin never takes another pin's place
/// (CONTRACT: the five surfaces never share a candidate pool).
///
/// Scope follows what actually gets served: a project item competes with its
/// own project and with the global tier that reaches every project. A GLOBAL
/// item is compared only against other globals, because the projects it will
/// land in are not knowable at write time and guessing them would refuse
/// writes for crowding that may never happen.
/// A DIRECTORY rival is deliberately NOT counted against a file inside it,
/// and this note exists because the opposite was tried for a few hours on
/// 2026-08-08 and was wrong.
///
/// The reasoning that failed held that a file touch offers its parent
/// directory as a second target beside the path, putting a directory anchor in
/// the pool every time a file under it is touched. It does not.
/// `ServeInput::add_file` adds
/// a Path target only, and `normalize::target_matches` refuses a kind
/// mismatch, so that pool is never assembled anywhere.
///
/// Why that mattered rather than being a harmless extra: this count decides a
/// REFUSAL. Over-counting refuses an honest write for rivals it will never
/// meet, and the comment justifying it claimed the safe direction while doing
/// the unsafe one. Two independent reviews found it the same evening, both by
/// reading `input.rs` instead of the comment.
///
/// What is true about a Dir binding is said where it belongs, in `capacity`:
/// it reaches no automatic serving surface at all.
fn pool_rivals<'a>(
    candidates: &'a [(String, Item)],
    item: &Item,
    binding: &Binding,
) -> Vec<&'a (String, Item)> {
    let Some(kind_match): Option<&dyn Fn(&Binding) -> bool> = (match binding {
        Binding::Always => None,
        Binding::Moment(_) => Some(&|b: &Binding| matches!((b, binding), (Binding::Moment(x), Binding::Moment(y)) if x == y)),
        Binding::Target { .. } => Some(&|b: &Binding| match (b, binding) {
            (Binding::Target { kind: bk, value: bv }, Binding::Target { kind, value }) => {
                crate::normalize::target_matches(*bk, bv, *kind, value)
            }
            _ => false,
        }),
    }) else {
        return Vec::new();
    };

    candidates
        .iter()
        .filter(|(id, other)| {
            if *id == item.id || !other.kind.can_fire() {
                return false;
            }
            match (item.project.as_deref(), other.project.as_deref()) {
                (None, None) => true,
                (None, Some(_)) => false,
                (Some(_), None) => true,
                (Some(mine), Some(theirs)) => mine == theirs,
            }
        })
        .filter(|(_, other)| other.bindings.iter().any(|b| kind_match(b)))
        .collect()
}

/// Would this item ever reach a block, and if not, is that provable?
///
/// WHY THIS EXISTS AND WHY IT IS NOT A KNOB. CONTRACT R1 names "anchors on a
/// full target" as a refusal class and nothing ever implemented it, so by
/// R1's own last line that refusal did not exist. Version 1 had one, counted
/// per anchor; that unit is wrong here. Measured 2026-08-08: of the 38 items
/// competing at the worst target, only 5 were anchored there and 33 were
/// bound to a MOMENT, so a per-anchor gate would have let every one of them
/// through. The unit that competes is the pool `rank::select` assembles.
///
/// TWO ANSWERS, ALONG THE LINE THE DOCTRINE ALREADY DRAWS. A refusal has to
/// be provable, so it fires only when EVERY binding the item carries is
/// already full of rivals of STRICTLY heavier severity. Severity is compared
/// before closeness, so no context at any future moment can lift this item
/// into the block: it is dead on arrival, and storing it would be cover that
/// looks real and never fires. Anything less is a warning, because whether a
/// same-weight rival wins depends on closeness to something that has not
/// happened yet, and refusing on a guess is how a gate teaches people to
/// route around it.
///
/// No new constant: the line is `item::MAX_ITEMS`, the number of places a
/// block actually has. A configurable capacity would be exactly the
/// compensating knob CONTRACT R9 calls a reported design failure.
///
/// A THIRD ANSWER SITS BETWEEN THOSE TWO, since a crowding tightening made
/// 2026-09: a Path/Dir/Command binding's sitting rivals are a place
/// `pool_rivals` counts completely (see `full_reliable_anchor`), so a write
/// that would put this item on one already holding `item::MAX_ITEMS` of them
/// - at ANY weight, not only equal-or-heavier - is refused too, before the
/// two answers above are even asked. A Moment binding still only ever gets
/// the warning: the facts that would truly collide with it live at whatever
/// file or command the moment next fires on, a set this function cannot
/// enumerate before that happens, and refusing on a guess is how a gate
/// teaches people to route around it.
pub fn capacity(store: &EventStore, item: &Item) -> anyhow::Result<Capacity> {
    let bindings: Vec<&Binding> =
        item.bindings.iter().filter(|b| !matches!(b, Binding::Always)).collect();
    if bindings.is_empty() || !item.kind.can_fire() {
        return Ok(Capacity::Fine);
    }
    let candidates = if store.heads_projection_current() {
        live_items_from_projection(store)?
    } else {
        live_items_from_fold(store)?
    };

    if let Some(refusal) = full_reliable_anchor(&candidates, item, &bindings) {
        return Ok(Capacity::Full(refusal));
    }

    // A Dir target reaches NO automatic serving surface. `ServeInput::add_file`
    // adds a Path target only, and `normalize::target_matches` refuses a kind
    // mismatch, so `rank::select` drops a Dir-bound item before it compares a
    // single path. Such an item can still REFUSE - the guard's location and
    // dir-content arms narrow their own pool by kind and decide containment
    // themselves - but it will never appear as advice at a file touch.
    //
    // Said out loud rather than refused, because "only ever refuses" is a
    // legitimate thing for a rule to be. But it has to be SAID, because this
    // is exactly the case the crowding count below cannot see: a directory
    // pool holding three items reads as roomy while being unreachable.
    // `bindings` above already dropped every Always, so this has to ask the
    // ITEM, not the filtered list. A pinned item is served in full at every
    // session start, so "never shown as advice" would be a flat lie about an
    // [Always, Dir] pair - and both reviews caught exactly that wording.
    if is_all_dir_binding_set(item, &bindings) {
        // Note, not a refusal: since 2026-08-19 a directory anchor DOES reach
        // the files inside it (`normalize::target_matches`), so this is about
        // breadth, not about silence. It used to say such an item could never
        // be shown at all, which was true of the matcher as it stood and is
        // the defect that fix closes.
        //
        // `Broad`, not `Crowded`, since 2026-09-17 - see `Capacity::Broad`'s
        // own doc comment for why a note about reaching every file under a
        // tree must never be read as a rival pool being full.
        return Ok(Capacity::Broad(
            "every binding on this item is a DIRECTORY, so it reaches every file inside that \
             directory. That is broad on purpose and it is read AFTER anything anchored at the \
             exact file being touched. If this fact is really about one file or one command, bind \
             it there and it will be read first."
                .to_string(),
        ));
    }

    let mine = crate::item::severity_rank(item.severity);
    let mut every_binding_is_hopeless = true;

    for binding in &bindings {
        let rivals = pool_rivals(&candidates, item, binding);
        let heavier = rivals.iter().filter(|(_, o)| crate::item::severity_rank(o.severity) < mine).count();
        if heavier < crate::item::MAX_ITEMS {
            every_binding_is_hopeless = false;
        }
    }
    // The note half (never a refusal) is its own function - see
    // `crowded_binding_note`'s own doc comment for why: `capacity_for_revise`
    // needs to ask this exact question again, against the same candidates, to
    // measure a REVISE's own crowding note. Before 2026-09-17 it also had to
    // ask `is_all_dir_binding_set` a second time here, to tell that note
    // apart from the different, unconditional directory note just above -
    // both shared the one `Crowded` shape. Now they do not: the directory
    // note is `Capacity::Broad`, so `capacity_for_revise` only ever sees a
    // `Crowded` value here in the first place, nothing left to tell apart.
    let crowded = crowded_binding_note(&candidates, item, &bindings).map(|(note, _, _)| note);

    if every_binding_is_hopeless {
        let worst = bindings
            .iter()
            .map(|b| (describe(b), pool_rivals(&candidates, item, b)))
            .max_by_key(|(_, r)| r.len())
            .map(|(d, r)| {
                let ids: Vec<&str> = r.iter().take(3).map(|(id, _)| id.as_str()).collect();
                format!("{d} is held by {} heavier item(s), among them {}", r.len(), ids.join(", "))
            })
            .unwrap_or_default();
        return Ok(Capacity::DeadOnArrival(Refusal {
            problem: format!(
                "every binding on this item is already full of heavier rivals, so it can never \
                 reach a block: {worst}"
            ),
            fix: "give it a binding that is actually free - the exact file or command it is about \
                  rather than a broad moment - or fold the constraint into the item that already \
                  holds that ground. Do not raise its severity to win the place: that pushes a \
                  heavier warning out to show a lighter one. Storing it as it stands would be \
                  cover that looks real and never fires."
                .to_string(),
        }));
    }
    Ok(match crowded {
        Some(note) => Capacity::Crowded(note),
        None => Capacity::Fine,
    })
}

/// Every binding this item carries (besides `Always`, already dropped by
/// `capacity`'s own caller) is a DIRECTORY - the one case `capacity` reports
/// unconditionally, before it ever asks `pool_rivals` a single question. A
/// pinned item is exempt: `Always` next to a `Dir` still means "everywhere",
/// not "this one broad place" - see `capacity`'s own comment on why the
/// filtered `bindings` list is not enough to decide this alone.
fn is_all_dir_binding_set(item: &Item, bindings: &[&Binding]) -> bool {
    let pinned = item.bindings.iter().any(|b| matches!(b, Binding::Always));
    !pinned && bindings.iter().all(|b| matches!(b, Binding::Target { kind: crate::item::TargetKind::Dir, .. }))
}

/// The first binding (in order) already holding `item::MAX_ITEMS` live rivals
/// of the same weight or heavier, the note `capacity`'s own `Crowded` case
/// builds from it, and the count the note quotes - factored out of
/// `capacity`'s own loop so `capacity_for_revise` can ask the identical
/// question, against the same candidates, without re-deriving the wording and
/// risking a second copy that silently drifts from the first.
///
/// Returns the earliest binding whose rivals reach the cap, not every one
/// that does - exactly what `capacity`'s own `crowded.is_none()` guard kept
/// before this was its own function: a note names ONE place to fix, not an
/// exhaustive report of every full pool this item touches.
fn crowded_binding_note<'a>(
    candidates: &[(String, Item)],
    item: &Item,
    bindings: &[&'a Binding],
) -> Option<(String, &'a Binding, usize)> {
    let mine = crate::item::severity_rank(item.severity);
    for binding in bindings {
        let rivals = pool_rivals(candidates, item, binding);
        let at_least_equal = rivals.iter().filter(|(_, o)| crate::item::severity_rank(o.severity) <= mine).count();
        if at_least_equal >= crate::item::MAX_ITEMS {
            let note = format!(
                "{} already holds AT LEAST {at_least_equal} item(s) of the same weight or heavier, \
                 for {} place(s) in a block - this one may well never be shown there. At least, \
                 because this count sees only the rivals sharing this one binding; the real crowd \
                 also includes everything reaching that place through a moment or a command, which \
                 only doctor's crowding line can count, per file and per command anchor in a \
                 checkout. Bind it to the exact file or command it is about instead \
                 of the broad moment, or fold it into whichever item already carries that ground.",
                describe(binding),
                crate::item::MAX_ITEMS
            );
            return Some((note, binding, at_least_equal));
        }
    }
    None
}

/// `capacity`, sharpened for a REVISE that leaves every binding exactly as it
/// was.
///
/// MEASURED, NOT PREDICTED, AND HERE IS WHY THAT DIFFERENCE IS SOUND. A
/// revise that keeps its bindings does not move into a place and wonder if it
/// will be seen there - it is already the live occupant of every binding it
/// carries (`full_reliable_anchor`'s own doc comment: a write never refuses
/// the binding an item already stands on), and has been since some earlier
/// write. So "will this be shown at that binding" is not a question about the
/// future the way it is for a brand new `declare` or a revise that just
/// changed WHERE the item sits - the store's own `item_served` log already
/// answered it, every time a gate actually reached this item and chose to
/// show it or not.
///
/// THE DAY THIS WAS MEASURED. A revise that changed only an item's falsifier
/// text, bindings untouched, got the ordinary "may well never be shown there"
/// prediction - while that same item had been served 55 times at that exact
/// binding since its last verdict, and doctor's own crowding line counted
/// nothing outranked in the project. The note guessed at a future the log had
/// already settled.
///
/// WHY A CHANGED BINDING OR A BRAND NEW `declare` DOES NOT GET THIS. Either
/// one is a real unknown again: a new binding has no service history AT THAT
/// BINDING to read (the item's OLD servings happened somewhere else, and
/// counting them here would measure the wrong place), and a `declare` has no
/// `existing` at all to compare against. Both keep the ordinary prediction
/// from `capacity` untouched.
///
/// `now` is an ISO-8601 UTC timestamp (`served_at`'s own shape) - threaded in
/// by the caller rather than read from the clock in here, so this stays as
/// testable against a fixed instant as everything else this crate proves
/// against a temp store.
pub fn capacity_for_revise(
    store: &EventStore,
    existing: &Item,
    updated: &Item,
    now: &str,
) -> anyhow::Result<Capacity> {
    let cap = capacity(store, updated)?;
    if existing.bindings != updated.bindings {
        return Ok(cap);
    }
    // The one note this function knows how to measure is the per-binding
    // one `crowded_binding_note` builds (`Capacity::Crowded`). The
    // all-directory note `capacity` returns unconditionally, before ever
    // reaching that computation, says something about BREADTH, not about a
    // rival pool - there is nothing here for a servings count to confirm or
    // replace. Before 2026-09-17 the two shared one shape, so telling them
    // apart took a second `is_all_dir_binding_set` call, against the same
    // candidates `capacity` had already asked it against. Now the directory
    // note is its own variant (`Capacity::Broad` - see its own doc comment),
    // so this guard alone already excludes it: a `Broad` value can never
    // match `Crowded(_)` in the first place.
    if !matches!(cap, Capacity::Crowded(_)) {
        return Ok(cap);
    }
    let bindings: Vec<&Binding> =
        updated.bindings.iter().filter(|b| !matches!(b, Binding::Always)).collect();
    let candidates = if store.heads_projection_current() {
        live_items_from_projection(store)?
    } else {
        live_items_from_fold(store)?
    };
    let Some((_, binding, _)) = crowded_binding_note(&candidates, updated, &bindings) else {
        return Ok(cap);
    };
    let served = crate::served::served_count(store, &updated.id, now, crate::served::SERVED_WINDOW_DAYS);
    if served == 0 {
        return Ok(cap);
    }
    Ok(Capacity::Crowded(format!(
        "shown {served} time(s) in the last {} days at {} - not crowded out",
        crate::served::SERVED_WINDOW_DAYS,
        describe(binding)
    )))
}

/// One binding, in the words a refusal can use.
fn describe(binding: &Binding) -> String {
    match binding {
        Binding::Always => "the pinned layer".to_string(),
        Binding::Moment(a) => format!("the moment '{}'", a.as_str()),
        Binding::Target { kind, value } => format!("the target {kind:?}:{value}"),
    }
}

/// Refuses a write that would put `item` on a Path, Dir or Command binding
/// already holding `item::MAX_ITEMS` live rivals, counting every rival AT ANY
/// WEIGHT rather than only the equal-or-heavier ones `capacity`'s own
/// hopeless/crowded pass below counts.
///
/// WHY A SEPARATE, UNWEIGHTED COUNT. The pass below only ever compares a
/// candidate against rivals ranked at least as heavy as itself, because that
/// is the comparison a REFUSAL can prove (a lighter newcomer cannot outrank
/// heavier sitting rivals) or a WARNING has to hedge (an equal-weight rival
/// might still lose on closeness at some future moment). Neither one ever
/// looks at a rival LIGHTER than the newcomer, so a heavier arrival landing
/// on a place already sitting at capacity with lighter facts sailed through
/// both checks and silently bumped the lightest one - the gap a real session's
/// hygiene pass fell into, leaving a displaced fact behind with no refusal and
/// no note. Counting every rival regardless of weight closes it: a place at
/// capacity is refused whether the arrival outranks its neighbours or not,
/// and which sitting fact would give way is left to the agent to decide, not
/// guessed at here.
///
/// WHY Path/Dir/Command AND NOTHING ELSE. Those are the bindings whose
/// sitting rivals `pool_rivals` enumerates completely: `rank::select` mixes a
/// Moment's pool INTO a Target's at real delivery (a file touch fires both
/// what is anchored there and what a matching Moment reaches), never the
/// other way, so the count below is a true lower bound on what will compete
/// at that exact anchor regardless of what else the store holds - refusing on
/// it can never be an over-refusal. A Moment binding has no such floor: the
/// facts that would actually collide with it live at whatever file or
/// command the moment next fires on, a set that is not enumerable before it
/// happens, so it keeps the existing note rather than gaining a refusal (see
/// `capacity`'s own doc comment). Symbol, Project, Route and Host stay on
/// that same note for now: Symbol reaches through `serve::prompt`'s own
/// pool-free surface rather than a place this count can see, and Project and
/// Route carry no live delivery path this workspace has measured the way
/// Path/Dir/Command's has been - widening the refusal to them without that
/// measurement would be exactly the shaky enforcement this doctrine refuses
/// to ship.
///
/// NEVER REFUSES A BINDING THE ITEM ALREADY STOOD ON. `candidates` still
/// carries this item's own previous live version on a `revise`, so a fold
/// that revises the survivor to carry a retracted item's point BEFORE
/// retracting it - the order this function's own refusal recommends - must
/// not itself be refused for standing exactly where it already stood; only a
/// binding new to this write is judged.
fn full_reliable_anchor(candidates: &[(String, Item)], item: &Item, bindings: &[&Binding]) -> Option<Refusal> {
    let previously_bound: &[Binding] = candidates
        .iter()
        .find(|(id, _)| *id == item.id)
        .map(|(_, prev)| prev.bindings.as_slice())
        .unwrap_or(&[]);

    for binding in bindings {
        let Binding::Target { kind, .. } = binding else { continue };
        if !matches!(
            kind,
            crate::item::TargetKind::Path | crate::item::TargetKind::Dir | crate::item::TargetKind::Command
        ) {
            continue;
        }
        if previously_bound.contains(binding) {
            continue;
        }
        let rivals = pool_rivals(candidates, item, binding);
        if rivals.len() < crate::item::MAX_ITEMS {
            continue;
        }
        let named = rivals
            .iter()
            .map(|(id, other)| format!("'{id}' ({}): {}", severity_word(other.severity), other.text))
            .collect::<Vec<_>>()
            .join("; ");
        return Some(Refusal {
            problem: format!(
                "{} already holds {} live fact(s) for the {} place(s) a block ever has, so this \
                 write guarantees a displacement - its own, or one of theirs: {named}",
                describe(binding),
                rivals.len(),
                crate::item::MAX_ITEMS
            ),
            fix: "no crowding, no loose ends: fold, move or archive one of them first, then this \
                  write goes through untouched. Fold means revise one of them to carry this point \
                  too and retract the rest; move means re-anchor the sitting fact to the narrower \
                  file, directory or command it is really about; archive means retract whichever \
                  one is dated or its subject is gone. A full place is not fixed by adding another \
                  claimant."
                .to_string(),
        });
    }
    None
}

/// A severity, in the same words `Severity::from_str` accepts back - so a
/// refusal never invents a vocabulary the writer cannot also type.
fn severity_word(severity: Option<crate::item::Severity>) -> &'static str {
    match severity {
        Some(crate::item::Severity::Irreversible) => "irreversible",
        Some(crate::item::Severity::Costly) => "costly",
        Some(crate::item::Severity::HouseStyle) => "house_style",
        None => "no severity",
    }
}

/// The longest reason `archive` will carry. It becomes a tag, and a tag is
/// something search matches on: a paragraph in there would match everything.
pub const ARCHIVE_REASON_LIMIT: usize = 120;

/// Marks an anchor that points at something DELIBERATELY absent, so it must
/// never be read as decay.
///
/// THE DEFECT THIS CLOSES, and it was found the hard way on 2026-08-07. A
/// sweep archived six rules anchored at `firmware/src/secrets.h` - a file
/// that is gitignored and is SUPPOSED not to exist in a checkout. The anchor
/// was never stale: it was there precisely so the rules fire the moment
/// anybody creates or touches that file. One of the six said so in its own
/// text, in capitals. Prose could not stop the sweep, because a sweep reads
/// paths and not sentences, and that rule was scoped to a project the sweep
/// was not even running in.
///
/// So the distinction has to be something a machine can see. An item carrying
/// this tag is skipped by the decay count and refused by `archive`.
pub const DELIBERATE_ANCHOR_TAG: &str = "deliberate-anchor";

/// A heavy rule that carries no check informs and never refuses. Sometimes
/// that is correct - "ask before pushing to main" has no text whose presence
/// means the mistake, because an authorised push looks exactly like an
/// unauthorised one. Sometimes it is an oversight nobody ever revisits.
///
/// Prose cannot tell those apart, and nothing else can either: a rule that
/// merely informs is indistinguishable, from the outside, from a rule someone
/// forgot to give teeth. Measured 2026-08-09 on the real store: 2757 rules,
/// 331 carrying any check at all, and 17 able to refuse an introduction. Not
/// one thing in the system had ever asked the question.
///
/// So the distinction has to be something a machine can see. An item carrying
/// this tag has been through the question and the answer was no. Without it, a
/// heavy rule with no check is refused by `gate::declare` - not because it must
/// have teeth, but because it must have been ASKED.
pub const NO_LITERAL_TAG: &str = "no-literal";

/// The same answer, with the reason it was given: `no-literal:<why>`.
///
/// THE DEFECT THIS CLOSES, found in a session review on 2026-08-14. The tag
/// above is an exit from the gate, and it was the only exit here that cost
/// nothing. `archive` demands a reason. `retract` demands a reason. Every Rule
/// demands a falsifier. But "there is nothing a guard could catch here" was a
/// bare word, taken on the writer's say-so, and after it nothing in the system
/// ever asked again. An exit that is cheaper to take than the work it excuses
/// is the exit that gets taken: the review found a rule about a command tagged
/// rather than checked, and the tag ended the conversation.
///
/// Nothing can verify the reason, and that is not the point. No machine can
/// tell an honest "nothing to catch" from a lazy one - if one could, this
/// would be a check and not a tag (see `NO_LITERAL_TAG` above). What a reason
/// buys is that the answer can be read back, counted, and disagreed with by
/// whoever comes next. A bare word cannot be argued with.
pub const NO_LITERAL_REASON_PREFIX: &str = "no-literal:";

/// Short enough to write, long enough that "no" is not a reason.
pub const NO_LITERAL_REASON_MIN: usize = 20;

/// What one tag says about the teeth question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeethAnswer<'a> {
    /// The bare `no-literal`, with no reason. Still honoured for an item that
    /// already carried it - see `gate::revise` - and never accepted as a new
    /// answer.
    Bare,
    /// `no-literal:<why>`, carrying its reason (already trimmed, possibly
    /// empty if the writer typed the prefix and stopped).
    Reasoned(&'a str),
}

/// Read one tag as an answer to the teeth question, or `None` if it is not
/// one. The single definition of what answering looks like, so the gate that
/// accepts an answer and the debt that stops asking after one can never
/// disagree about which tags count.
pub fn teeth_answer(tag: &str) -> Option<TeethAnswer<'_>> {
    if let Some(reason) = tag.strip_prefix(NO_LITERAL_REASON_PREFIX) {
        return Some(TeethAnswer::Reasoned(reason.trim()));
    }
    (tag == NO_LITERAL_TAG).then_some(TeethAnswer::Bare)
}

/// The command-anchored counterpart to `NO_LITERAL_TAG`. A heavy Rule or
/// Orientation with no check ordinarily answers the teeth question with
/// `NO_LITERAL_TAG`/`NO_LITERAL_REASON_PREFIX` above - but not once it is
/// also anchored to a Command: there, two concrete checks almost always
/// exist to try first (a forbidden fragment, or a requires check for
/// something forgotten), so the generic answer is no longer enough. See
/// `gate::shape_problems`, ground 11 (continued, command), for the full
/// reasoning and the refusal a writer actually sees.
pub const NO_CATCH_ON_COMMAND_TAG: &str = "no-catch-on-command";

/// The same answer, with its reason: `no-catch-on-command:<why>`. Held to
/// the exact same shape as `NO_LITERAL_REASON_PREFIX`'s own reason (see
/// `gate::no_literal_reason_problem`, reused rather than duplicated) - only
/// the prefix differs.
pub const NO_CATCH_ON_COMMAND_REASON_PREFIX: &str = "no-catch-on-command:";

/// Sometimes a fact belongs exactly where it is and the place is honestly full
/// of heavier things. The crowding debt has always SAID so - "leave it and say
/// why" is the third of the three ways out it offers - but saying why happened
/// in prose, and prose settles nothing. Measured 2026-08-09, on this codebase:
/// the debt re-fired on the same item every turn, because folding and
/// re-anchoring were the only exits that actually closed it. The one honest
/// answer left was to delete a true fact to stop the nagging, which is the
/// wrong incentive to build into a memory.
///
/// So the third exit gets the same shape as the other two: something a machine
/// can see. An item carrying this tag has been through the question and the
/// answer was that the crowd is deserved. The debt skips it; nothing else
/// changes, and it stays as findable and as crowded as it was.
pub const CROWDED_ON_PURPOSE_TAG: &str = "crowded-on-purpose";

/// A rule about what gets SAID rather than written can never carry a check:
/// a check reaches file writes and commands, and an answer is neither. Its
/// enforcement lives in the response guard's own rulebook, a file this store
/// knows nothing about - so a tag naming the entry is the link between the
/// two, and it is a BETTER answer to the teeth question than "nothing to
/// catch here", because it says where the catching actually happens.
///
/// Added 2026-08-09, the same evening ground 11 started asking: the first
/// rule to deserve this answer was refused for not having one, which is the
/// gate being right about the question and wrong about the vocabulary.
pub const ANSWER_GUARD_TAG_PREFIX: &str = "answer-guard:";

/// THE BOX A FACT FALLS INTO, ONCE THE TOOL HAS ACTUALLY TRIED ITS TEETH -
/// never a question a writer answers, unlike every tag above this one.
///
/// `ANSWER_GUARD_TAG_PREFIX`/`NO_LITERAL_TAG`/`NO_CATCH_ON_COMMAND_TAG` all
/// exist because the write gate could only ask whether a writer had been
/// ASKED the teeth question - it could never verify the answer, so a lazy
/// "nothing to catch here" and an honest one look identical on the page.
/// This prefix is the different, compulsory half: whenever a fact is heavy
/// (irreversible/costly) or carries a check of a refusing kind (Forbidden,
/// Absent, AbsentAll, Requires), the tool itself synthesises payloads from
/// the fact's OWN binding and literals and runs them through the SAME guard
/// functions the real door runs (see `serve::prove`) - never something the
/// writer supplies. The outcome, never the writer's wording, decides which
/// of the four values below gets appended, and a write is never refused for
/// which one it gets: the knowledge always gets in, only the label is
/// compulsory.
///
/// TWO SIDES, NOT ONE (2026-09-24, continued): a claim now has to survive
/// BOTH a FORBIDDEN payload (the anchor carrying one of its own literals,
/// which the guard must refuse) AND a CLEAN one (the identical anchor with
/// none of them, which the guard must let through) - see `serve::prove`'s
/// own top-of-file doc comment for why one side alone could never tell a
/// working guard apart from a rule that refuses everything. `TEETH_PROVEN_
/// TAG` is the only one of the four below that means both sides held;
/// `TEETH_REFUSES_LEGITIMATE_TAG` and `TEETH_ANCHOR_COLLAPSED_TAG` are the
/// two ways a check that DOES reach can still fail to earn it.
///
/// RESERVED FROM A WRITER: `mcp::remember`/`mcp::revise` refuse outright a
/// tag starting with this prefix among the tags a CALLER supplies, before
/// either tool ever constructs the `Item` it hands to the write gate - the
/// gate itself never re-decides this, because by the time an item reaches
/// `gate::declare`/`gate::revise` it may already carry the tool's OWN
/// legitimate tag, appended after testing, and a second check there could
/// not tell that apart from a forged one. This is why the label cannot be
/// set, cleared or faked by wording: the one door that ever writes it is not
/// the one a writer's arguments pass through.
pub const TEETH_TAG_PREFIX: &str = "teeth:";

/// The check has bitten REALITY: the log holds a real call the real hook
/// refused under it, or a real call its rule fired on that it would now
/// refuse while still letting that rule's ordinary calls through - and where
/// a synthesised two-sided test can be built at all, it held too. Since
/// 2026-09-26 the synthesised test alone earns `TEETH_WIRED_TAG` instead: an
/// attempt built from the writer's own literal can never tell a real tooth
/// from an invented one.
pub const TEETH_PROVEN_TAG: &str = "teeth:proven-guard";

/// THE THIRD OUTCOME (2026-09-24): the guard refused BOTH the forbidden
/// payload AND the clean, legitimate one - it cannot tell this fact's own
/// genuine violation apart from ordinary, authorised use of the very same
/// anchor. Never filed under `TEETH_PROVEN_TAG` (it is not one) and never
/// filed quietly under `TEETH_INFORMING_TAG` either: a rule that refuses
/// legitimate work is the most expensive failure this system has - see
/// `serve::prove`'s own top-of-file doc comment - and it earns its own
/// plain label so the daily health review can never mistake it for either
/// of the other two. The write still goes through; only the label says what
/// was found, exactly like every other value here.
pub const TEETH_REFUSES_LEGITIMATE_TAG: &str = "teeth:refuses-legitimate-use";

/// THE COLLAPSED CASE (2026-09-24): the fact's own anchor already contains
/// one of the check's own literals (an anchor "git push --force" whose
/// own literal is "--force", for example, or an anchor that simply IS
/// the literal) - there is no
/// literal-free form of the anchor left to build a clean payload from at
/// all, so the two-sided test never even ran. Structurally as
/// indistinguishable from an over-blocking rule as `TEETH_REFUSES_
/// LEGITIMATE_TAG` is, it just gets there without running anything - never
/// dressed up as `TEETH_PROVEN_TAG`.
pub const TEETH_ANCHOR_COLLAPSED_TAG: &str = "teeth:anchor-collapsed";

/// Any of: the guard was tried and let the synthesised forbidden payload
/// straight through (a check that looks like a lock and is not one, never
/// even reaching the clean side - see `serve::prove::teeth_box`'s own doc
/// comment on why the clean side is only tried once the forbidden one
/// fires), or there was nothing to try at all (a heavy fact with no
/// refusing check, or one whose check can only be proven against a real
/// working copy). All land here, in the one box the daily health review has
/// to meet again - the receipt at write time is where they are told apart,
/// in prose, with the concrete correction a reader needs.
pub const TEETH_INFORMING_TAG: &str = "teeth:informing";

/// WIRED, NOT PROVEN (2026-09-26): both synthesised attempts held - the
/// forbidden one refused, the clean one let through - but the log holds no
/// real call the check ever met. That is reach, and reach is all a writer
/// can show at the moment of writing: the attempt is built from the writer's
/// own literal, so an invented literal nobody will ever type comes back
/// exactly as wired as a real one. Only `TEETH_PROVEN_TAG` means a real tooth
/// (see `serve::prove`'s "against reality" section for what counts).
pub const TEETH_WIRED_TAG: &str = "teeth:wired-not-proven";

/// The id of the first-session note `ops::install::working_contract` seeds
/// (`walk-through-the-answer-guard-once`), and the id an agent files the
/// owner's own setup answers under once he has actually been asked - see
/// `OWNER_SETUP_ANSWERS_ID` below. Both constants live here rather than
/// beside the seed text in `ops`, because `retract` (below) and
/// `gate::retract` are the places that have to compare an entity id against
/// them, and `ops` depends on `model`, never the other way round.
pub const SETUP_NOTE_ID: &str = "walk-through-the-answer-guard-once";

/// See `SETUP_NOTE_ID` above. A Report is the natural kind for it - a
/// record of what was decided, never served at a gate - but nothing here
/// requires that: `gate::retract`'s own ground only asks whether ONE LIVE
/// ITEM answers to this id, any kind, so a store that filed it differently
/// is still read correctly.
///
/// This exact id also carries its own exemption from `gate::declare`'s
/// ground 21 (archive material needs a scope) and from the MCP collection
/// gate (`mcp::refuse_a_new_collection`): it may be declared with no
/// project at all. Neither guard's own doc comment repeats this constant's
/// purpose, only why the exemption exists - a brand-new store, the one this
/// id is first written to, has declared no project yet and so holds no
/// scope this could possibly be filed under.
pub const OWNER_SETUP_ANSWERS_ID: &str = "owner-setup-answers";

// ------------------------------------------------------- evaluation obligation
//
// WHAT THIS SECTION CLOSES (2026-09-25). Filing an evaluation report used to
// discharge the Stop hook's evaluation debt (`serve::usefulness::
// eval_debt_owed`/`newest_evaluation_report`) by merely EXISTING: a live
// `Report` scoped to the project and carrying `EVALUATION_REPORT_TAG`.
// Nobody read it, and nothing checked the log for the work the evaluation
// routine actually demands, so a session could discharge the
// whole obligation with prose. `check_evaluation_report_filing` below is the
// machine-checked replacement, called from `declare` - every condition is
// computed from the EVENT LOG, never from what the report's own text claims,
// and the report's prose stays completely unchecked; it no longer discharges
// anything by existing.
//
// Three conditions, all of them per-project: the project's judgement debt is
// zero, every heavy (irreversible/costly) Rule or Orientation in this
// filing's batch - at most `EVALUATION_TEETH_BATCH` of the ones that apply to
// this project and have fired since the PREVIOUS report, the ones written
// longest ago first (`teeth_batch_since`) - is settled (`teeth_state`), and
// every open point (see below) older than the previous report is either
// closed (retracted) or has been carried forward exactly once. The heavy rules
// beyond the batch stay owed and are named by `doctor`; the next filing
// asks for them first.
//
// WHAT "PER-PROJECT" MEANS FOR THE FIRST TWO. An item applies to a project
// when it names that project or names none at all (global) - the same
// `applies_to` rule judgement debt has always used. A firing is counted
// wherever it happened: an `ItemServed` event records when an item fired and
// on what trigger, never in which project, so "fired in this project" is not
// a question the log can answer, and a gate that claims to compute everything
// from the log must not pretend it can. A global heavy rule that fired in
// another checkout therefore also holds this project's report until it
// carries a box - one revise, or one backfill run, settles it everywhere.

/// The tag `EVALUATION_REPORT_TAG`-carrying `Report`s already used
/// (`serve::usefulness::evaluation_report_ids` matches the identical string
/// literal) - given a name here too so this file's own gate check and that
/// one can never drift apart in spelling. Not re-exported from there: `model`
/// cannot depend on `serve` (the dependency runs the other way), so this is
/// the same "reimplemented, not imported" concession `live_items_from_
/// projection`'s own doc comment already makes.
pub const EVALUATION_REPORT_TAG: &str = "evaluation-report";

/// How many times a live item must have fired since its own last verdict
/// before it counts as judgement debt - mirrors `serve::usefulness::
/// JUDGEMENT_DEBT_AFTER` exactly (kept equal by hand: `model` cannot import
/// it, for the same reason every other constant in this section is
/// reimplemented rather than shared).
pub const JUDGEMENT_DEBT_AFTER: usize = 40;

/// The actor a real delivery is stamped with - mirrors `serve::usefulness::
/// ASSISTANT_DELIVERY_ACTOR` exactly, same reason.
pub const ASSISTANT_DELIVERY_ACTOR: &str = "hook";

/// An open point: a small, project-scoped `Report` tagged with this, holding
/// one sentence naming something an evaluation could not finish - see this
/// section's own doc comment. Never served at a gate (it is a `Report`,
/// archive-kind, exactly like an evaluation report itself), found by
/// `lookup` and handed to a session directly by `serve::bin::serve`'s own
/// SessionStart and evaluation-debt messages instead - see that file's
/// `open_points_line`/`open_points_block`.
pub const OPEN_POINT_TAG: &str = "open-point";

/// Prefix for the tag naming the evaluation report an open point was raised
/// in, e.g. `from-report:eval-acme-2026-01-01`. Informational only - nothing
/// in this file matches on it - but it is what lets a reader trace an open
/// point back to the evaluation that found it.
pub const OPEN_POINT_SOURCE_PREFIX: &str = "from-report:";

/// Prefix for the tag recording how many times an open point has been
/// carried forward, e.g. `carried:1`. `open_point_carried_count` below reads
/// it; `gate::revise`'s own GROUND 30 refuses ever writing a count above 1 -
/// see that ground's doc comment for why a second carry needs the owner
/// rather than another automated pass.
pub const OPEN_POINT_CARRIED_PREFIX: &str = "carried:";

/// The carried-forward count a set of tags records, `0` when none of them
/// match `OPEN_POINT_CARRIED_PREFIX` at all (never carried) or the tag's own
/// number does not parse (never written by this system, so never trusted as
/// a count either).
pub fn open_point_carried_count(tags: &[String]) -> u32 {
    tags.iter()
        .find_map(|t| t.strip_prefix(OPEN_POINT_CARRIED_PREFIX))
        .and_then(|n| n.parse::<u32>().ok())
        .unwrap_or(0)
}

/// Every live item, current single head, not a retraction tombstone, body
/// parses as a valid `Item` - as `(id, item)` pairs, in NO particular order
/// (unlike `live_items_from_projection`/`live_items_from_fold`'s own id-
/// sorted contract: nothing below needs that order, and sorting here would
/// only be paid for and thrown away).
///
/// FAILS OPEN: an unreadable log reads back as "no live items" rather than
/// an error. Deliberately, and differently from `declare`'s other store-
/// level checks (`find_near_duplicate`/`find_opposing_check`/`capacity`,
/// all of which propagate a read failure as `WriteError::Store` and refuse
/// the write): every condition built on this helper exists to stop a report
/// from being filed while real work sits undone, and a store this cannot
/// even read is not evidence that the work is undone - refusing on that
/// account would trade a maintenance question for a permanently wedged
/// project no evaluation could ever again be filed for. See this section's
/// own top-of-file doc comment.
fn all_live_items(store: &EventStore) -> Vec<(String, Item)> {
    let read = if store.heads_projection_current() {
        live_items_from_projection(store)
    } else {
        live_items_from_fold(store)
    };
    read.unwrap_or_default()
}

/// Mirrors `serve::project::applies_to` exactly (an item with no project of
/// its own is global and applies everywhere; one that names a project
/// applies only inside it) - reimplemented rather than imported for the
/// identical reason every other function in this section is.
fn project_applies_to(item_project: Option<&str>, checkout_project: Option<&str>) -> bool {
    item_project.is_none() || item_project == checkout_project
}

/// Mirrors `serve::usefulness::served_since_last_verdict` exactly: how many
/// times each item has fired (`ItemServed`, stamped by `ASSISTANT_DELIVERY_
/// ACTOR`) since its own last verdict (`ItemMarkedUseful`/`ItemMarkedNoise`
/// reset the count to zero, from any actor - a verdict is about the item,
/// not the audience). Fails open to an empty map, the same stance
/// `all_live_items` takes and for the same reason.
fn served_since_last_verdict_local(store: &EventStore) -> HashMap<String, usize> {
    let Ok(events) = store.event_kinds_with_actor() else { return HashMap::new() };
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (kind, id, actor) in events {
        match kind {
            EventKind::ItemServed if actor == ASSISTANT_DELIVERY_ACTOR => {
                *counts.entry(id).or_default() += 1;
            }
            EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise => {
                counts.insert(id, 0);
            }
            _ => {}
        }
    }
    counts
}

/// How many live, non-pinned items THIS project owes a verdict on right now,
/// store-wide - mirrors `serve::usefulness::judgement_debt_counts`'s own
/// `in_project` half exactly (threshold, pinned exclusion, project scoping all
/// identical), reimplemented for the identical reason every other function
/// in this section is. No longer what the filing gate refuses on (see
/// `evaluation_judgement_owed` below); kept as the store-wide count the
/// filing receipt reports, so a report that files is still told how much
/// stays owed elsewhere.
pub fn judgement_debt_in_project(store: &EventStore, project: Option<&str>) -> Vec<String> {
    let served = served_since_last_verdict_local(store);
    let live = all_live_items(store);
    let live_by_id: HashMap<&str, &Item> = live.iter().map(|(id, it)| (id.as_str(), it)).collect();
    let mut owed: Vec<String> = served
        .into_iter()
        .filter(|(_, count)| *count >= JUDGEMENT_DEBT_AFTER)
        .filter_map(|(id, _)| live_by_id.get(id.as_str()).map(|it| (id, *it)))
        .filter(|(_, it)| !it.bindings.iter().any(|b| matches!(b, Binding::Always)))
        .filter(|(_, it)| project_applies_to(it.project.as_deref(), project))
        .map(|(id, _)| id)
        .collect();
    owed.sort();
    owed
}

/// Which evaluation a filing belongs to: the session that runs it, and the
/// log position at which it started.
///
/// WHY THE FILING GATE NEEDS THIS (2026-09-26). The judgement condition used
/// to be computed live and store-wide at every filing attempt. Measured in a
/// fresh evaluation that day: a session filed its report eight times and was
/// refused eight times, each time on a different, growing list (14, 6, 13, 4,
/// 4, 1, 5, 4). Two things fed it. The rules bound to the `remember` moment
/// fire on every one of the evaluation's own writes, so the evaluation pushed
/// items over the threshold by doing its work; and parallel sessions in the
/// same repository did the same, for items this session never saw fire. It
/// ended up marking sixty items useful against two noise, including items it
/// had never seen - the exact false record the verdict exists to prevent.
///
/// So the set a report must settle is fixed when its evaluation STARTS, the
/// same way the teeth batch is fixed by the previous report, and it holds only
/// what fired in the evaluating session itself. The log already records the
/// session of every firing (the hook stamps it); what it cannot record is
/// which session is FILING, because every write through the tool server is
/// stamped with one shared constant. The hook knows, and hands it over in a
/// small sidecar (`serve::eval_session`); the tool server reads that back and
/// passes it in here. `None` - no hook record of the filing at all - means no
/// session is known, and then no verdict can honestly be demanded: a session
/// the hook never saw has no firings on record either. The store-wide debt
/// stays with doctor and the Stop hook's own per-turn ask either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationScope {
    /// The Claude Code session id the hook stamped on this session's firings.
    pub session_id: String,
    /// The highest log seq that existed when the evaluation started. Nothing
    /// after it can add to the set this report must settle.
    pub started_seq: i64,
}

/// The tag an evaluation report carries for one item it could not judge,
/// shaped `not-judgeable:<id>:<reason>`. The explicit "when in doubt, skip"
/// the evaluation routine asks for: a guessed verdict is worse than none, so
/// a skip with a reason settles the item for THIS filing, and the item stays
/// owed store-wide, where doctor and the Stop hook keep asking about it.
pub const NOT_JUDGEABLE_TAG_PREFIX: &str = "not-judgeable:";

/// How long a `not-judgeable` reason must be, trimmed. A reason, not a
/// placeholder: "no" or "skip" say nothing a reader could check.
pub const NOT_JUDGEABLE_REASON_MIN_CHARS: usize = 12;

/// The reason `tags` gives for skipping `id`, when it gives one long enough
/// to count (`NOT_JUDGEABLE_REASON_MIN_CHARS`). Matched against the id as a
/// whole, so an id that itself contains a colon still parses.
pub fn not_judgeable_reason<'a>(tags: &'a [String], id: &str) -> Option<&'a str> {
    tags.iter().find_map(|t| {
        let reason = t.strip_prefix(NOT_JUDGEABLE_TAG_PREFIX)?.strip_prefix(id)?.strip_prefix(':')?.trim();
        (reason.chars().count() >= NOT_JUDGEABLE_REASON_MIN_CHARS).then_some(reason)
    })
}

/// The judgement half of one evaluation's filing: every item this evaluation
/// must settle, and whether each one already is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvaluationJudgement {
    /// Still owed: fired in this session before the evaluation started, 40+
    /// times since its last verdict as of that moment, no verdict since, and
    /// not skipped with a reason.
    pub owed: Vec<String>,
    /// Given a verdict since the evaluation started, or no longer live.
    pub settled: Vec<String>,
    /// Skipped with a `not-judgeable` reason on the report being filed.
    pub skipped: Vec<String>,
}

/// The set one evaluation must settle (see `EvaluationScope`), split by
/// state. `report_tags` are the tags of the report being filed - only its
/// `not-judgeable` entries are read. FAILS OPEN to an empty result on an
/// unreadable log, the same stance `all_live_items` takes.
///
/// THE SET, computed from the log alone: an item belongs to it when, counting
/// only events at or before `scope.started_seq`, it had fired
/// `JUDGEMENT_DEBT_AFTER`+ times (assistant deliveries only, exactly as the
/// store-wide debt counts) since its own last verdict, AND it fired in
/// `scope.session_id` at least once at or before the start - the same "was
/// the reader being asked ever in the room" test the Stop hook's own per-turn
/// ask applies (`EventStore::served_ids_in_session`). It must also still be live, still apply to `project`
/// and still not be pinned. A firing after the start never adds anything,
/// whichever session it came from, which is what keeps the evaluation's own
/// writes - and every rule bound to the `remember` moment they fire - from
/// growing the list while it is being paid.
pub fn evaluation_judgement_owed(
    store: &EventStore,
    project: Option<&str>,
    scope: &EvaluationScope,
    report_tags: &[String],
) -> EvaluationJudgement {
    let Ok(events) = store.firings_and_verdicts() else { return EvaluationJudgement::default() };
    let mut count_at_start: HashMap<&str, usize> = HashMap::new();
    let mut fired_here: HashSet<&str> = HashSet::new();
    let mut judged_after: HashSet<&str> = HashSet::new();
    for (seq, kind, id, actor, session) in &events {
        let after = *seq > scope.started_seq;
        match kind {
            EventKind::ItemServed if !after && actor == ASSISTANT_DELIVERY_ACTOR => {
                *count_at_start.entry(id.as_str()).or_default() += 1;
                if *session == scope.session_id {
                    fired_here.insert(id.as_str());
                }
            }
            EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise => {
                if after {
                    judged_after.insert(id.as_str());
                } else {
                    count_at_start.insert(id.as_str(), 0);
                }
            }
            _ => {}
        }
    }
    let live = all_live_items(store);
    let live_by_id: HashMap<&str, &Item> = live.iter().map(|(id, it)| (id.as_str(), it)).collect();
    let mut result = EvaluationJudgement::default();
    let mut ids: Vec<&str> = count_at_start
        .iter()
        .filter(|(id, count)| **count >= JUDGEMENT_DEBT_AFTER && fired_here.contains(**id))
        .map(|(id, _)| *id)
        .collect();
    ids.sort();
    for id in ids {
        let Some(item) = live_by_id.get(id) else {
            // Retracted since: a verdict on something gone is no verdict,
            // and nothing is left to ask about.
            result.settled.push(id.to_string());
            continue;
        };
        if item.bindings.iter().any(|b| matches!(b, Binding::Always)) || !project_applies_to(item.project.as_deref(), project) {
            continue;
        }
        if judged_after.contains(id) {
            result.settled.push(id.to_string());
        } else if not_judgeable_reason(report_tags, id).is_some() {
            result.skipped.push(id.to_string());
        } else {
            result.owed.push(id.to_string());
        }
    }
    result
}

/// Every item whose history on the log shows a check was ever attached to it
/// - the evidence `gate::teeth_answer_settles` needs before a no-literal
/// reason can count (see its own "A REASON COUNTS ONLY AFTER A REAL TRY").
/// Read from the stored bodies of the item's own writes: a check is only
/// ever serialized as `"check":{`, never as a null, so an item that never
/// carried one never matches. FAILS OPEN to an empty set, which only ever
/// makes a reason count LESS, never more.
pub fn ids_whose_check_was_tried(store: &EventStore) -> HashSet<String> {
    store.ids_with_a_write_containing("\"check\":{").unwrap_or_default()
}

/// The highest creation seq among this project's own live evaluation
/// reports - `0` when it has none yet, so "since the previous report" reads
/// as "since the beginning of the log" for a project's first-ever
/// evaluation. Mirrors `serve::usefulness::newest_evaluation_report`'s own
/// EXACT-project match (never `project_applies_to`'s "or global" rule: a
/// report filed for one project must never silence another's debt),
/// reimplemented for the identical reason every other function in this
/// section is.
fn newest_evaluation_report_seq(store: &EventStore, project: Option<&str>) -> i64 {
    all_live_items(store)
        .into_iter()
        .filter(|(_, item)| item.kind == Kind::Report)
        .filter(|(_, item)| item.project.as_deref() == project)
        .filter(|(_, item)| item.tags.iter().any(|t| t == EVALUATION_REPORT_TAG))
        .filter_map(|(id, _)| store.get_events_by_entity(&id).ok()?.first().map(|e| e.seq))
        .max()
        .unwrap_or(0)
}

/// How many heavy rules one evaluation filing is held to.
///
/// WHY A BATCH (2026-09-26). The filing gate used to demand every heavy rule
/// that fired since the previous report, all at once. Measured on a copy of a
/// real store: the next evaluation of one project would first have had to
/// settle about 80 of them - a wall that size is exactly what invites the
/// dodging this gate exists to stop (a label in bulk, a reason that fits any
/// rule). Eight is about what one evaluation can settle honestly, one real
/// check or one checkable reason per rule, and it still clears a backlog of 80
/// in ten evaluations; the rest stays owed, named by `doctor`, and comes first
/// next time.
pub const EVALUATION_TEETH_BATCH: usize = 8;

/// The heavy rules one evaluation filing is held to (see
/// `EVALUATION_TEETH_BATCH`), and the ones left for a later filing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeethBatch {
    /// In this filing's batch and not settled yet, each with what is missing.
    pub owed_now: Vec<(String, &'static str)>,
    /// In this filing's batch and settled since the previous report.
    pub settled: Vec<String>,
    /// Not settled, and beyond this filing's batch: still owed, asked first
    /// by a later filing.
    pub owed_later: Vec<(String, &'static str)>,
}

/// The batch the NEXT evaluation report for `project` is held to - what the
/// filing gate refuses on, and what `doctor` shows.
pub fn evaluation_teeth_batch(store: &EventStore, project: Option<&str>) -> TeethBatch {
    teeth_batch_since(store, project, newest_evaluation_report_seq(store, project))
}

/// The kinds of event that WRITE an item (create, correct, settle a
/// divergence, move it to another project), as opposed to the telemetry kinds
/// (served, marked, refused) that only record what happened to it.
fn writes_the_item(kind: EventKind) -> bool {
    matches!(kind, EventKind::FactCreated | EventKind::FactRevised | EventKind::FactResolved | EventKind::FactReprojected)
}

/// Every live, heavy (irreversible/costly) Rule or Orientation that applies
/// to `project`, has fired (any `ItemServed` event, whatever the actor - a
/// firing to a local model is still a firing its own teeth mattered for) with
/// a seq strictly greater than `cutoff_seq`, and was not settled as of that
/// seq or is not settled now (`teeth_state`) - split into one filing's batch
/// and the rest.
///
/// THE ORDER, AND WHY IT ROTATES. The rules written longest ago come first:
/// each rule's key is its last write at or before `cutoff_seq`, so a rule
/// created after the previous report waits behind every older one. Nothing
/// that happens after the cutoff moves a key, so settling a rule never pulls
/// the next one into the batch while this evaluation works through it - a
/// settled batch member stays in its place and counts as done. At the next
/// filing the cutoff moves to that report: every rule settled in this round
/// was settled as of that cutoff and drops out, and the ones left unsettled
/// lead, oldest first. That is the "least recently asked first" the Stop
/// hook's teeth question keeps with a sidecar, computed here from the log
/// alone, because this gate reads nothing but the log. The one thing that
/// can still change the batch mid-evaluation is a rule firing for the first
/// time since the cutoff - during the evaluation itself - with an older key.
///
/// WHY "SETTLED AS OF THE CUTOFF" IS ASKED AT ALL: a rule settled before the
/// previous report and still settled is not this filing's business, and must
/// not take a batch place either, or a handful of long-settled rules that
/// fire often would fill every batch and the backlog behind them would never
/// be asked. The state as of the cutoff is the item's last created or revised
/// body at or before it; open points are read as they stand now.
///
/// NEVER RE-COMPUTES THE BOX ITSELF. `serve::prove::teeth_box` needs
/// `serve::absent_guard`/`serve::rank`, both unreachable from here - see this
/// section's own top-of-file doc comment. `teeth_state` reads only what the
/// write path or the maintenance backfill recorded on the item.
fn teeth_batch_since(store: &EventStore, project: Option<&str>, cutoff_seq: i64) -> TeethBatch {
    let Ok(events) = store.get_all_events() else { return TeethBatch::default() };
    let mut fired_after: HashSet<String> = HashSet::new();
    // Per item: the seq of its last write at or before the cutoff, of its
    // first write ever (the key of an item born after the cutoff), and the
    // index of its last created/revised body at or before the cutoff.
    let mut last_write_by_cutoff: HashMap<&str, i64> = HashMap::new();
    let mut first_write: HashMap<&str, i64> = HashMap::new();
    let mut body_by_cutoff: HashMap<&str, usize> = HashMap::new();
    for (i, e) in events.iter().enumerate() {
        if e.kind == EventKind::ItemServed && e.seq > cutoff_seq {
            fired_after.insert(e.entity_id.clone());
        }
        if !writes_the_item(e.kind) {
            continue;
        }
        first_write.entry(e.entity_id.as_str()).and_modify(|s| *s = (*s).min(e.seq)).or_insert(e.seq);
        if e.seq <= cutoff_seq {
            last_write_by_cutoff.entry(e.entity_id.as_str()).and_modify(|s| *s = (*s).max(e.seq)).or_insert(e.seq);
            if matches!(e.kind, EventKind::FactCreated | EventKind::FactRevised)
                && body_by_cutoff.get(e.entity_id.as_str()).map_or(true, |&j| events[j].seq < e.seq)
            {
                body_by_cutoff.insert(e.entity_id.as_str(), i);
            }
        }
    }
    let live = all_live_items(store);
    // A no-literal reason counts only for an item whose history shows a
    // check was tried (`gate::teeth_answer_settles`). Read once, from the
    // same log, as of NOW, and used for the state at the cutoff too. That
    // only matters for a rule that is settled now (an unsettled one takes
    // its place either way), and such a rule then simply keeps no batch
    // place - the same as any rule settled before the cutoff.
    let tried = ids_whose_check_was_tried(store);
    // Every live open point of this project, lowercased once: a wired or
    // doubtful check is settled for THIS filing only when an open point
    // names it (see `teeth_state` below).
    let open_point_texts: Vec<String> = live
        .iter()
        .filter(|(_, item)| item.kind == Kind::Report)
        .filter(|(_, item)| item.project.as_deref() == project)
        .filter(|(_, item)| item.tags.iter().any(|t| t == OPEN_POINT_TAG))
        .map(|(_, item)| item.text.to_lowercase())
        .collect();
    // (key, id, what is missing now - `None` when settled)
    let mut candidates: Vec<(i64, String, Option<&'static str>)> = live
        .into_iter()
        .filter(|(_, item)| item.kind.can_fire())
        .filter(|(_, item)| project_applies_to(item.project.as_deref(), project))
        .filter(|(_, item)| matches!(item.severity, Some(Severity::Irreversible) | Some(Severity::Costly)))
        .filter(|(id, _)| fired_after.contains(id))
        .filter_map(|(id, item)| {
            let named_in_open_point = open_point_texts.iter().any(|t| t.contains(&id.to_lowercase()));
            let check_tried = tried.contains(&id);
            let now = teeth_state(&item, named_in_open_point, check_tried);
            // An item with no body at or before the cutoff did not exist then,
            // and one whose old body will not parse cannot prove it was
            // settled: both count as unsettled as of the cutoff.
            let settled_at_cutoff = body_by_cutoff
                .get(id.as_str())
                .and_then(|&i| parse_body(&events[i].body).ok())
                .is_some_and(|then| teeth_state(&then, named_in_open_point, check_tried).is_none());
            if now.is_none() && settled_at_cutoff {
                return None;
            }
            let key = last_write_by_cutoff
                .get(id.as_str())
                .or_else(|| first_write.get(id.as_str()))
                .copied()
                .unwrap_or(i64::MAX);
            Some((key, id, now))
        })
        .collect();
    candidates.sort();
    let mut batch = TeethBatch::default();
    for (place, (_, id, now)) in candidates.into_iter().enumerate() {
        match (place < EVALUATION_TEETH_BATCH, now) {
            (true, Some(missing)) => batch.owed_now.push((id, missing)),
            (true, None) => batch.settled.push(id),
            (false, Some(missing)) => batch.owed_later.push((id, missing)),
            (false, None) => {}
        }
    }
    batch
}

/// Whether one heavy rule's teeth question is SETTLED for an evaluation
/// filing - `None` when it is, or the plain name of what is still missing.
///
/// THE RULE (2026-09-26): more real teeth, or a clear proof it cannot have
/// one - never a label that only satisfies the count. So exactly three things
/// settle it:
/// - `TEETH_PROVEN_TAG`: the check has bitten a real call on record;
/// - a teeth answer whose reason quotes the legitimate call or names the
///   rule's own trigger (`gate::teeth_answer_settles`) - a reason that only
///   fills the length does not;
/// - a check that is wired but not yet proven (or whose anchor needs a
///   person to look at it) AND a live open point of this project naming the
///   rule's id: the honest "this still has to meet a real call", carried where
///   the next evaluation must close it (see `open_points_needing_action`).
///
/// `TEETH_INFORMING_TAG` alone settles nothing: it only records that nothing
/// could be tried, which is the question, not an answer to it. Before this,
/// ANY machine label settled the filing, so a maintenance pass that labelled
/// every heavy rule "informing" lifted the gate for the whole store at once.
fn teeth_state(item: &Item, named_in_open_point: bool, check_tried: bool) -> Option<&'static str> {
    if item.tags.iter().any(|t| t == TEETH_PROVEN_TAG) || gate::teeth_answer_settles(item, check_tried) {
        return None;
    }
    let doubtful = [TEETH_WIRED_TAG, TEETH_ANCHOR_COLLAPSED_TAG, TEETH_REFUSES_LEGITIMATE_TAG];
    if item.tags.iter().any(|t| doubtful.contains(&t.as_str())) {
        if named_in_open_point {
            return None;
        }
        if item.tags.iter().any(|t| t == TEETH_REFUSES_LEGITIMATE_TAG) {
            return Some("its check refuses legitimate use too");
        }
        if item.tags.iter().any(|t| t == TEETH_ANCHOR_COLLAPSED_TAG) {
            return Some("its anchor contains its own literal and it has not refused a real call yet");
        }
        return Some("its check is wired but has never met a real call");
    }
    if item.check.is_some() {
        return Some("its check has never been tried, or cannot refuse anything");
    }
    Some("no check, and no reason that quotes the legitimate call or names its trigger")
}

/// Every live open point for `project`, exactly scoped (never `project_
/// applies_to`'s "or global" rule - the same reason `newest_evaluation_
/// report_seq` is exact) whose own creation seq is at or before
/// `cutoff_seq` (it already existed as of the previous report - a point
/// raised THIS session, while preparing the report about to be filed, is
/// never held against that same filing) and that has not been carried
/// forward since that previous report. It keeps being handed to the session
/// regardless (`serve::bin::serve`'s own open-points messages), so it is
/// never silently forgotten.
///
/// A CARRY BUYS ONE CYCLE, NOT FOREVER (2026-09-26). A point carried once
/// used to stop blocking for good the moment its tag read `carried:1` - one
/// revise, and the point could ride along unanswered through every later
/// evaluation. Now the carry is dated: it covers the ONE filing that follows
/// it. Once a report has been filed after the carry, the point blocks again,
/// and since a second carry is refused (`gate::revise`, ground 30) the only
/// way on is to close it, or to take it to the owner.
fn open_points_needing_action(store: &EventStore, project: Option<&str>, cutoff_seq: i64) -> Vec<String> {
    let mut out: Vec<String> = all_live_items(store)
        .into_iter()
        .filter(|(_, item)| item.kind == Kind::Report)
        .filter(|(_, item)| item.project.as_deref() == project)
        .filter(|(_, item)| item.tags.iter().any(|t| t == OPEN_POINT_TAG))
        .filter(|(id, _)| {
            let Ok(events) = store.get_events_by_entity(id) else { return false };
            let Some(created) = events.first().map(|e| e.seq) else { return false };
            if created > cutoff_seq {
                return false;
            }
            // The first revision that recorded a carry, if any.
            let carried_at = events
                .iter()
                .filter(|e| e.kind == EventKind::FactRevised)
                .find(|e| parse_body(&e.body).is_ok_and(|item| open_point_carried_count(&item.tags) >= 1))
                .map(|e| e.seq);
            match carried_at {
                None => true,
                Some(seq) => seq <= cutoff_seq,
            }
        })
        .map(|(id, _)| id)
        .collect();
    out.sort();
    out
}

/// One live open point, as handed to a reader rather than only to the
/// filing gate above: its id (so a writer can `retract` or `revise` it),
/// its one-sentence text, and how many times it has already been carried
/// forward (`open_point_carried_count`) - `serve::bin::serve`'s own
/// SessionStart and evaluation-debt messages format this, never the raw
/// `Item`, so a display change on either surface never has to relearn the
/// tag shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPoint {
    pub id: String,
    pub text: String,
    pub carried: u32,
}

/// Every live open point scoped EXACTLY to `project` (never `project_
/// applies_to`'s "or global" rule - an open point belongs to the project
/// that raised it, the same exact-match every other function in this
/// section uses), oldest first (ascending creation seq, so the point that
/// has waited longest leads) - a failed per-item seq lookup sorts an item
/// last rather than dropping it, so a read glitch narrows the order, never
/// the membership.
pub fn open_points(store: &EventStore, project: Option<&str>) -> Vec<OpenPoint> {
    let mut with_seq: Vec<(i64, OpenPoint)> = all_live_items(store)
        .into_iter()
        .filter(|(_, item)| item.kind == Kind::Report)
        .filter(|(_, item)| item.project.as_deref() == project)
        .filter(|(_, item)| item.tags.iter().any(|t| t == OPEN_POINT_TAG))
        .map(|(id, item)| {
            let seq = store.get_events_by_entity(&id).ok().and_then(|e| e.first().map(|e| e.seq)).unwrap_or(i64::MAX);
            let carried = open_point_carried_count(&item.tags);
            (seq, OpenPoint { id, text: item.text, carried })
        })
        .collect();
    with_seq.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
    with_seq.into_iter().map(|(_, p)| p).collect()
}

/// THE GATE ITSELF. `Ok(None)` for every write that is not an evaluation
/// report (`item.kind != Kind::Report`, or a `Report` with no
/// `EVALUATION_REPORT_TAG`) - untouched. For one that is, the three
/// conditions this section's own top-of-file doc comment names, each
/// computed fresh from the event log; `Ok(Some(refusal))` names every
/// condition that is not yet met, with the concrete action that settles it,
/// never just the first one found (the same "collect every problem, not
/// just the first" stance `gate::declare_inner`'s own doc comment explains
/// and measures).
///
/// `item.project` is read as given rather than asserted `Some`: `gate::
/// declare`'s own ground 21 already refuses a scopeless `Report`, but this
/// function is cheap to make correct for a scopeless one too (every helper
/// above already takes `Option<&str>`) rather than relying on a guarantee
/// from a call it does not control the order against.
///
/// THE JUDGEMENT CONDITION IS SCOPED (2026-09-26): only the set
/// `evaluation_judgement_owed` fixes for `scope` - what fired in the filing
/// session before its evaluation started. `scope: None` (no hook record of
/// which session files this) demands no verdict at all; see
/// `EvaluationScope`'s own doc comment for why that is the honest answer.
fn check_evaluation_report_filing(store: &EventStore, item: &Item, scope: Option<&EvaluationScope>) -> Option<Refusal> {
    if item.kind != Kind::Report || !item.tags.iter().any(|t| t == EVALUATION_REPORT_TAG) {
        return None;
    }
    let project = item.project.as_deref();
    let mut missing: Vec<String> = Vec::new();

    let judgement = scope.map(|scope| (scope, evaluation_judgement_owed(store, project, scope, &item.tags)));
    if let Some((scope, judgement)) = judgement.as_ref().filter(|(_, j)| !j.owed.is_empty()) {
        let named = judgement.owed.iter().map(|id| format!("'{id}'")).collect::<Vec<_>>().join(", ");
        missing.push(format!(
            "{} item(s) fired in this session before this evaluation started (log position {}) and still owe a \
             judgement - each had fired {JUDGEMENT_DEBT_AFTER}+ times since its last verdict by then: {named}. This \
             list was fixed when the evaluation started and cannot grow: nothing that fires after that, and nothing \
             that fired only in another session, is this report's to settle. For each one, read where it fired \
             (`get` shows the item) and decide whether it belonged there: mcp__thor__mark(id: \"<id>\") when it \
             did, mcp__thor__mark(id: \"<id>\", noise: true) when it did not. If you cannot honestly judge one \
             here, do not guess - add the tag '{NOT_JUDGEABLE_TAG_PREFIX}<id>:<reason>' to this report (a reason \
             of at least {NOT_JUDGEABLE_REASON_MIN_CHARS} characters); it then stays owed store-wide instead. A \
             verdict you did not read the item for is a false record, and it is worse than the debt",
            judgement.owed.len(),
            scope.started_seq
        ));
    }

    let cutoff_seq = newest_evaluation_report_seq(store, project);
    let batch = teeth_batch_since(store, project, cutoff_seq);
    if !batch.owed_now.is_empty() {
        let named = batch.owed_now.iter().map(|(id, why)| format!("'{id}' ({why})")).collect::<Vec<_>>().join(", ");
        let later = match batch.owed_later.len() {
            0 => String::new(),
            n => format!(
                " {n} more heavy rule(s) in the same state are not this report's to settle: they stay owed, doctor \
                 names them, and a later evaluation asks for them first."
            ),
        };
        missing.push(format!(
            "{} heavy rule(s) in this report's batch (at most {EVALUATION_TEETH_BATCH}, the ones written longest ago \
             first) apply to this project (its own or global), have fired since the previous report - the log \
             records when a rule fired, never in which project - and still have neither a real tooth nor a real \
             reason why they cannot: {named}.{later} For each one, do the work, in this order: (1) find the \
             text whose presence MEANS the mistake is happening and give the rule a check with it (forbidden, \
             absent, absent_all, contains or requires) - it counts as proven once the log shows it biting a real \
             call, and until then add an open point naming the rule's id and what real call would prove it; (2) \
             only when no text marks the mistake, tag it 'no-literal:<why>' (or 'no-catch-on-command:<why>' on a \
             command) with a reason that QUOTES the legitimate call that carries the same fragment, or NAMES the \
             command, file or moment the rule is about - a reason that would fit any rule does not count; (3) \
             retract it with a reason if it is simply wrong. A label a maintenance pass writes ('teeth:informing') \
             is not an answer: it only says nothing could be tried",
            batch.owed_now.len()
        ));
    }

    let stuck = open_points_needing_action(store, project, cutoff_seq);
    if !stuck.is_empty() {
        let named = stuck.iter().map(|id| format!("'{id}'")).collect::<Vec<_>>().join(", ");
        missing.push(format!(
            "{} open point(s) from an earlier report are still neither closed nor carried: {named} - retract \
             each with a reason if it is resolved, or revise it to add the tag '{}1' if it is being carried \
             forward for the first time",
            stuck.len(),
            OPEN_POINT_CARRIED_PREFIX
        ));
    }

    if missing.is_empty() {
        return None;
    }
    Some(Refusal {
        problem: format!("this evaluation report cannot be filed yet: {}", missing.join("; ")),
        fix: "settle every item named above by doing the work it names, then file this report again - each \
              condition is computed fresh from the log and from the items themselves, never from what this \
              report's own text says, so nothing here can be answered by rewording the report or by relabelling \
              in bulk"
            .to_string(),
    })
}

/// Turn a fireable item into archive material: same id, same text, same
/// history, still fully findable by `lookup` - but it stops claiming to fire.
///
/// WHY THIS IS NOT A `revise`. A Report may carry no bindings (gate ground 6)
/// and `gate::revise` refuses dropping bindings unconditionally, on purpose.
/// Those two rules are both right and they meet head on here, which is the
/// signal that archiving is not a correction at all. A correction says the
/// fact was wrong. This says the fact is still true and no longer has a place
/// to fire - a different act, so it gets its own door, the same way `retract`
/// does.
///
/// THE DEFECT THIS CLOSES. A store fills up with items anchored to things
/// that stopped existing: the one-off script that produced a measurement, the
/// experiment directory, the log file. The knowledge in them is often still
/// true and worth finding. But they are counted as live rules, they answer
/// "how many rules do you have" with a yes, and every one of them fires
/// nowhere while nothing says so. Retracting would take the knowledge out of
/// `lookup` as well, which is too blunt. This keeps the words and drops only
/// the claim that they fire.
///
/// A rule carrying a runnable check is REFUSED. That is exactly the kind that
/// can still block a wrong write, so archiving one would be throwing away the
/// only enforcement this memory has. Settle or remove the check first if that
/// is really what you mean.
/// `root` is the checkout a check's own path is resolved against, and it is
/// what lets this tell live protection from dead weight.
///
/// THE DEFECT THIS CLOSES, measured 2026-08-15. Retiring version 1 deleted the
/// tree 120 items were anchored to. 53 archived cleanly; the other 67 carried a
/// check, and the refusal below turned them away - so they stayed live, firing
/// nowhere, counted as decay forever. Their checks inspect files that no longer
/// exist: they cannot block anything, so refusing to archive them protected
/// nothing and cost the only cleanup that was ever going to happen.
///
/// So the refusal now asks whether the check still has something to look AT.
/// A `Forbidden` check names no file and reaches every write, so it is always
/// live protection and always refused. Any other check whose own path still
/// resolves inside `root` is refused exactly as before. Only a check pointing
/// at something that is not there gives way - and `root: None` (nowhere to
/// look) keeps the old, strict refusal, because a check that cannot be judged
/// must never be assumed dead.
pub fn archive(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    entity_id: &str,
    reason: &str,
    root: Option<&Path>,
) -> Result<Event, WriteError> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(WriteError::Refused(Refusal {
            problem: "an archive with no reason".to_string(),
            fix: "say why this item can no longer fire - six weeks later nobody can tell a \
                  deliberate archive from an accident without it"
                .to_string(),
        }));
    }
    if reason.chars().count() > ARCHIVE_REASON_LIMIT {
        return Err(WriteError::Refused(Refusal {
            problem: format!("the reason is {} characters", reason.chars().count()),
            fix: format!("keep it under {ARCHIVE_REASON_LIMIT}: it becomes a tag, and search matches on tags"),
        }));
    }
    if reason.contains(',') || reason.contains('\n') || reason.contains('\r') {
        return Err(WriteError::Refused(Refusal {
            problem: "the reason contains a comma or a line break".to_string(),
            fix: "write it as one plain phrase - it becomes a single tag".to_string(),
        }));
    }

    let existing = show(store, entity_id).map_err(|e| WriteError::Store(anyhow::anyhow!("{e}")))?;
    if !existing.kind.can_fire() {
        return Err(WriteError::Refused(Refusal {
            problem: format!("'{entity_id}' is already archive material ({:?})", existing.kind),
            fix: "nothing to do - it does not claim to fire".to_string(),
        }));
    }
    if let Some(check) = existing.check.as_ref() {
        // Still something to look at? See this function's own doc comment for
        // the three cases and why only the third one gives way.
        let still_live = match (crate::check::named_path(check), root) {
            (None, _) => true,
            (Some(_), None) => true,
            (Some(named), Some(root)) => root.join(named.replace('\\', "/")).exists(),
        };
        if still_live {
            return Err(WriteError::Refused(Refusal {
                problem: format!("'{entity_id}' carries a runnable check"),
                fix: "a rule that can prove itself is the only kind that can ever block a wrong \
                      write; settle or clear the check first if archiving it is really the intent"
                    .to_string(),
            }));
        }
    }
    if existing.tags.iter().any(|t| t == DELIBERATE_ANCHOR_TAG) {
        return Err(WriteError::Refused(Refusal {
            problem: format!("'{entity_id}' anchors at something deliberately absent"),
            fix: "its anchor is not decay - it is there so the rule fires the moment that file \
                  appears or is touched; drop the deliberate-anchor tag first if that has really \
                  stopped being true"
                .to_string(),
        }));
    }

    let mut updated = existing.clone();
    updated.kind = Kind::Report;
    updated.bindings = Vec::new(); // a Report may carry none, and must not
    updated.severity = None; // meaningless once it cannot fire
    // A Report may carry no check (the gate refuses one), and the only checks
    // that ever reach this line are the ones the refusal above already found
    // dead - they inspect a file that is not there, so nothing is lost by
    // dropping them here. The `archived:` tag says where they went.
    updated.check = None;
    updated.tags.push(format!("archived:{reason}"));

    gate::declare(&updated).map_err(WriteError::Refused)?;
    let body = canonical_body(&updated).map_err(WriteError::Serialize)?;
    store
        .append_mutate_checked(session_id, lineage_id, actor, EventKind::FactRevised, &updated.id, None, &body)
        .map_err(WriteError::Store)
}

/// Bring an item back out of the archive: restore the kind and the anchor it
/// had, and mark that anchor deliberate so no later sweep takes it again.
///
/// The counterpart to `archive`, and it exists because that operation was
/// used wrongly once. It is deliberately narrow - it takes the kind and the
/// one anchor explicitly rather than digging them out of history - because
/// undoing a mistake should be an act someone states in full, not a magic
/// rewind that might restore something nobody looked at.
pub fn restore_deliberate_anchor(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    entity_id: &str,
    kind: Kind,
    anchor: &str,
) -> Result<Event, WriteError> {
    let existing = show(store, entity_id).map_err(|e| WriteError::Store(anyhow::anyhow!("{e}")))?;
    if existing.kind.can_fire() {
        return Err(WriteError::Refused(Refusal {
            problem: format!("'{entity_id}' is not archived ({:?})", existing.kind),
            fix: "nothing to restore - it already fires".to_string(),
        }));
    }
    if !kind.can_fire() {
        return Err(WriteError::Refused(Refusal {
            problem: format!("{kind:?} is not a kind that fires"),
            fix: "restore it as a Rule or an Orientation, or leave it archived".to_string(),
        }));
    }

    let mut updated = existing.clone();
    updated.kind = kind;
    updated.bindings =
        vec![Binding::Target { kind: crate::item::TargetKind::Path, value: anchor.to_string() }];
    updated.tags.retain(|t| !t.starts_with("archived:"));
    if !updated.tags.iter().any(|t| t == DELIBERATE_ANCHOR_TAG) {
        updated.tags.push(DELIBERATE_ANCHOR_TAG.to_string());
    }

    gate::declare(&updated).map_err(WriteError::Refused)?;
    let body = canonical_body(&updated).map_err(WriteError::Serialize)?;
    store
        .append_mutate_checked(session_id, lineage_id, actor, EventKind::FactRevised, &updated.id, None, &body)
        .map_err(WriteError::Store)
}

/// Read the current item stored under `entity_id`. Refuses (well, errors -
/// this is the read side, so per R5 a caller may choose to treat this as
/// "nothing to show" rather than a hard failure) when the entity does not
/// exist, is diverged, or its head body is not valid canonical JSON.
pub fn show(store: &EventStore, entity_id: &str) -> Result<Item, ReadError> {
    let events = store.get_events_by_entity(entity_id).map_err(|e| ReadError::Store(e.into()))?;
    if events.is_empty() {
        return Err(ReadError::NotFound(entity_id.to_string()));
    }
    let heads = thor_core::cas::compute_head_sets(&events);
    let head_set = heads.get(entity_id).ok_or_else(|| ReadError::NotFound(entity_id.to_string()))?;
    if head_set.heads.len() != 1 {
        return Err(ReadError::Diverged(head_set.heads.len()));
    }
    let head_hash = head_set.heads.iter().next().expect("checked len == 1 above");
    let head_event = events
        .iter()
        .rev()
        .find(|e| &e.this_hash == head_hash)
        .ok_or_else(|| ReadError::NotFound(entity_id.to_string()))?;
    if head_event.kind == EventKind::FactRetracted {
        return Err(ReadError::Retracted(tombstone_reason(&head_event.body)));
    }
    parse_body(&head_event.body).map_err(ReadError::Parse)
}

/// The reason out of a tombstone body, or a plain stand-in when the body is not
/// one this version wrote. Never fails: a caller asking "why is this gone" must
/// get an answer, not a second error on top of the first.
fn tombstone_reason(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(str::to_string))
        .unwrap_or_else(|| "no reason recorded".to_string())
}

/// Retract an item: append a tombstone so it stops being live everywhere that
/// reads through `live_items` or `show`. Nothing is deleted - the log keeps the
/// whole chain and `history` still walks it, which is the point: a memory that
/// can quietly lose a fact is not a memory.
///
/// `reason` is required and not allowed to be blank. A retraction with no
/// reason is the same silent-decision problem the whole contract is against:
/// six weeks later nobody can tell a deliberate removal from a mistake.
///
/// One id carries a second condition on top of that: `SETUP_NOTE_ID` (the
/// seeded first-session note) is refused until `OWNER_SETUP_ANSWERS_ID`
/// exists as a live item - see `gate::retract`'s own doc comment for why.
/// Every other id is retracted exactly as before.
pub fn retract(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    entity_id: &str,
    reason: &str,
) -> Result<Event, WriteError> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(WriteError::Refused(Refusal {
            problem: "a retraction with no reason".to_string(),
            fix: "say why this item is wrong or no longer applies - a future reader cannot tell \
                  a deliberate removal from an accident without it"
                .to_string(),
        }));
    }
    // Reading first turns "no such id" and "already retracted" into their own
    // honest errors instead of a CAS conflict that says neither.
    match show(store, entity_id) {
        Ok(item) => {
            // A HEAVY RULE IS NOT RETRACTED ON A REASON THAT FITS ANY RULE
            // (2026-09-26). Measured on a copy of a real store: a rule marked
            // costly went out on "no longer relevant for this project". That
            // is the one exit left once weakening needs a reason, and it has
            // to cost the same honesty: say what changed about THIS rule -
            // name its command, file or moment, or quote what replaced it or
            // what is gone. The same test a teeth reason is held to.
            let heavy = item.kind.can_fire()
                && matches!(item.severity, Some(Severity::Irreversible) | Some(Severity::Costly));
            if heavy && !gate::reason_names_its_subject(reason, &item) {
                let triggers = gate::teeth_triggers(&item);
                let words = if triggers.is_empty() {
                    String::new()
                } else {
                    format!(
                        " - for this rule that means naming one of: {}",
                        triggers.iter().take(8).map(|t| format!("'{t}'")).collect::<Vec<_>>().join(" ")
                    )
                };
                return Err(WriteError::Refused(Refusal {
                    problem: format!(
                        "'{entity_id}' is marked {} and the reason '{reason}' says nothing about this rule - it would \
                         fit any rule in the store",
                        severity_word(item.severity)
                    ),
                    fix: format!(
                        "say what changed about THIS rule: name what it is about, or quote (between quotes) the item \
                         that replaces it or the thing that is gone{words}. If the rule is still true, keep it and \
                         give it teeth or a reason instead"
                    ),
                }));
            }
        }
        Err(ReadError::Parse(_)) => {} // unreadable body, still retractable
        Err(e) => return Err(WriteError::Store(anyhow::anyhow!("{e}"))),
    }
    // The one ground `retract` itself enforces (see `gate::retract`'s own
    // doc comment). The `entity_id == SETUP_NOTE_ID` guard means the extra
    // read on the right only ever runs for the one id this could possibly
    // refuse - every other id costs nothing beyond the comparison.
    let answers_recorded = entity_id == SETUP_NOTE_ID && show(store, OWNER_SETUP_ANSWERS_ID).is_ok();
    gate::retract(entity_id, answers_recorded).map_err(WriteError::Refused)?;
    let body = serde_json::json!({ "retracted": entity_id, "reason": reason }).to_string();
    store
        .append_mutate_checked(
            session_id,
            lineage_id,
            actor,
            EventKind::FactRetracted,
            entity_id,
            None,
            &body,
        )
        .map_err(WriteError::Store)
}

/// Settle a diverged entity by naming which head survives. Every current head
/// must be accounted for - `keep` plus `discard` has to be the whole set - and
/// `core` recomputes that set under the write lock, so a head that appeared
/// while the caller was deciding makes this fail instead of silently dropping
/// someone else's revision.
pub fn resolve(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    entity_id: &str,
    keep: &str,
    discard: &[String],
) -> Result<Event, WriteError> {
    store
        .append_resolve(session_id, lineage_id, actor, entity_id, keep, discard)
        .map_err(WriteError::Store)
}

/// One step in an item's life, oldest first.
/// There is no wall-clock field here because the event log has none: order is
/// `seq`, and `seq` is what the chain actually guarantees. Inventing a
/// timestamp from the row's insertion time would read as recorded fact.
pub struct Revision {
    pub seq: i64,
    pub kind: String,
    pub rev_hash: String,
    pub actor: String,
    /// The item as it stood after this step, when the body still parses as one.
    /// A tombstone or an unreadable body leaves this `None` rather than
    /// dropping the row: the step happened either way.
    pub item: Option<Item>,
    /// Why, for a revision `gate::revise_weakening` judged a weakening -
    /// `None` for every other step, an ordinary revise, a declare or a
    /// retraction (see `tombstone_reason` for that one's own reason) alike.
    /// See `because_reason`, the one place that reads it out of the body.
    pub because: Option<String>,
}

/// The whole revision log for one id, oldest first, including retractions.
/// A read path: an entity with nothing in it comes back as an empty list, not
/// an error.
pub fn history(store: &EventStore, entity_id: &str) -> Result<Vec<Revision>, ReadError> {
    let events = store.get_events_by_entity(entity_id).map_err(|e| ReadError::Store(e.into()))?;
    Ok(events
        .into_iter()
        .map(|e| Revision {
            seq: e.seq,
            kind: e.kind.as_str().to_string(),
            rev_hash: e.this_hash.clone(),
            actor: e.actor.clone(),
            item: parse_body(&e.body).ok(),
            because: because_reason(&e.body),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{Binding, Check, Kind, Severity, TargetKind};
    use intent::Action;

    fn sample() -> Item {
        Item {
            id: "test-item".to_string(),
            kind: Kind::Orientation,
            text: "the config lives in config/app.toml".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: "config/app.toml".to_string() }],
            severity: Some(Severity::Costly),
            project: Some("thor2".to_string()),
            // Costly with no check answers gate ground 11's teeth question
            // since it was widened to Orientation (2026-09-17) - a plain
            // fixture describing where a file lives has nothing literal to
            // catch, so the honest answer is the tag, not a check. Kept
            // alongside "config" rather than replacing it: nothing below
            // asserts on that tag, but severity/kind/bindings are load-
            // bearing for the crowding and near-duplicate tests that reuse
            // this fixture, so only the field ground 11 actually asks about
            // changes.
            tags: vec!["config".to_string(), format!("{}a fixture where 'the same text' is also the legitimate one", crate::store::NO_LITERAL_REASON_PREFIX)],
            expires: None,
            key: None,
            falsifier: Some("config/app.toml is removed or the app stops reading it".to_string()),
            check: None,
        }
    }

    /// `sample()` with just the id and text swapped in - every other field
    /// (bindings, severity, project, tags, falsifier) stays constant across
    /// the near-duplicate tests below, since only `kind` and `text` are
    /// meant to matter to that check.
    fn sample_with(id: &str, text: &str) -> Item {
        let mut item = sample();
        item.id = id.to_string();
        item.text = text.to_string();
        item
    }

    #[test]
    fn declare_then_show_round_trips_through_a_real_store() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();
        let back = show(&store, &item.id).unwrap();
        assert_eq!(item, back);
    }

    #[test]
    fn declare_refuses_and_writes_nothing_on_a_bad_item() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = sample();
        item.kind = Kind::Rule;
        item.bindings = vec![]; // ground 1: a Rule with no binding
        let err = declare(&mut store, "s1", "l1", "test", &item).unwrap_err();
        assert!(matches!(err, WriteError::Refused(_)));
        assert!(store.get_all_events().unwrap().is_empty(), "a refused declare must write nothing");
    }

    #[test]
    fn show_reports_not_found_for_an_unknown_id() {
        let store = EventStore::in_memory().unwrap();
        let err = show(&store, "does-not-exist").unwrap_err();
        assert!(matches!(err, ReadError::NotFound(_)));
    }

    #[test]
    fn revise_round_trips_and_keeps_the_hash_chain_valid() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let mut updated = item.clone();
        updated.bindings.push(Binding::Moment(Action::Configure));
        revise(&mut store, "s1", "l1", "test", &item, &updated).unwrap();

        let back = show(&store, &item.id).unwrap();
        assert_eq!(back, updated);

        let events = store.get_all_events().unwrap();
        assert_eq!(events.len(), 2);
        thor_core::auditor::verify_chain_integrity(&events).expect("hash chain stays intact across a model-level revise");
    }

    #[test]
    fn revise_refuses_and_writes_nothing_when_a_field_is_dropped() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let mut updated = item.clone();
        updated.tags = vec![]; // ground 9: existing had tags, this drops them

        let err = revise(&mut store, "s1", "l1", "test", &item, &updated).unwrap_err();
        assert!(matches!(err, WriteError::Refused(_)));
        assert_eq!(store.get_all_events().unwrap().len(), 1, "a refused revise must append nothing");
    }

    // ---------------------------------------------------- revise_because

    /// `sample()` with a real check - `sample()` itself carries none, and
    /// `gate::revise_weakening` (GROUND 28) has nothing to say about an item
    /// that never carried one.
    fn checked_sample() -> Item {
        let mut item = sample();
        item.check = Some(Check::PathExists { path: "config/app.toml".to_string() });
        item
    }

    /// THE HOLE THIS TEST PROVES CLOSED: before GROUND 28 existed, clearing a
    /// real check through `revise` cost nothing but the four bytes `""` on
    /// `check_kind` - see `model::gate::weakenings`'s own doc comment.
    #[test]
    fn revise_because_refuses_a_weakening_with_no_because_and_writes_nothing() {
        let mut store = EventStore::in_memory().unwrap();
        let item = checked_sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let mut updated = item.clone();
        updated.check = None; // GROUND 28's own shape 1: clearing the check

        // `existing` (the ground-9 baseline) is deliberately blanked here,
        // exactly the way `mcp::revise` blanks it for a real `check_kind ""`
        // call (see `gate::ClearedFields`) - otherwise ground 9 would refuse
        // this same call first, for an unrelated reason, and this test would
        // pass for the wrong one. `before` stays the true, unblanked item:
        // that is the whole point of GROUND 28.
        let existing_baseline = gate::ClearedFields { check: true, ..Default::default() }.baseline(&item);
        let err =
            revise_because(&mut store, "s1", "l1", "test", &existing_baseline, &updated, &item, None).unwrap_err();
        assert!(matches!(err, WriteError::Refused(_)));
        assert_eq!(store.get_all_events().unwrap().len(), 1, "a refused weakening must append nothing beyond the declare");
    }

    #[test]
    fn revise_because_accepts_a_weakening_with_a_because_and_the_event_body_carries_it() {
        let mut store = EventStore::in_memory().unwrap();
        let item = checked_sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let mut updated = item.clone();
        updated.check = None;
        let existing_baseline = gate::ClearedFields { check: true, ..Default::default() }.baseline(&item);

        let event = revise_because(
            &mut store,
            "s1",
            "l1",
            "test",
            &existing_baseline,
            &updated,
            &item,
            Some("the config file check was replaced by CI"),
        )
        .unwrap();
        assert!(
            event.body.contains("\"because\":\"the config file check was replaced by CI\""),
            "the stored event body must carry the reason verbatim: {}",
            event.body
        );

        let back = show(&store, &item.id).unwrap();
        assert_eq!(back, updated, "the extra `because` key must not disturb the item itself");
    }

    #[test]
    fn history_shows_the_because_line_for_a_weakening_revise_and_none_for_an_ordinary_one() {
        let mut store = EventStore::in_memory().unwrap();
        let item = checked_sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let mut weakened = item.clone();
        weakened.check = None;
        let existing_baseline = gate::ClearedFields { check: true, ..Default::default() }.baseline(&item);
        revise_because(&mut store, "s1", "l1", "test", &existing_baseline, &weakened, &item, Some("superseded by CI"))
            .unwrap();

        let mut reworded = weakened.clone();
        reworded.text = "the config lives in config/app.toml, reworded".to_string();
        revise(&mut store, "s1", "l1", "test", &weakened, &reworded).unwrap();

        let log = history(&store, &item.id).unwrap();
        assert_eq!(log.len(), 3, "declare, the weakening revise and the ordinary revise must all be there");
        assert_eq!(log[0].because, None, "a declare carries no because");
        assert_eq!(
            log[1].because,
            Some("superseded by CI".to_string()),
            "history must show why the check was cleared"
        );
        assert_eq!(log[2].because, None, "an ordinary revise with nothing to explain carries none");
    }

    /// Point 2 of the task's own rule: `because` is accepted and stored if
    /// given, never REQUIRED, on a revise `weakenings` finds nothing in.
    #[test]
    fn revise_because_stores_a_because_even_when_the_revise_is_not_a_weakening() {
        let mut store = EventStore::in_memory().unwrap();
        let item = checked_sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let mut updated = item.clone();
        updated.text = "the config lives in config/app.toml, read once at start".to_string();

        revise_because(&mut store, "s1", "l1", "test", &item, &updated, &item, Some("clarifying when it is read"))
            .unwrap();

        let log = history(&store, &item.id).unwrap();
        assert_eq!(
            log[1].because,
            Some("clarifying when it is read".to_string()),
            "a because given on a non-weakening revise is still stored, never required but never discarded either"
        );
    }

    /// The defect this guards against: a retracted item that still answers
    /// `show` as if it were live. A wrong fact that cannot be taken out is
    /// worse than no memory, and one that reads as live after removal is worse
    /// still - the caller cannot tell.
    #[test]
    fn a_retracted_item_stops_answering_show_and_says_why() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();
        show(&store, &item.id).expect("live before the retraction");

        retract(&mut store, "s1", "l1", "test", &item.id, "config/app.toml moved").unwrap();

        match show(&store, &item.id) {
            Err(ReadError::Retracted(why)) => assert!(
                why.contains("config/app.toml moved"),
                "the reason given at retraction must come back out, got {why:?}"
            ),
            other => panic!("a retracted item must report itself retracted, got {other:?}"),
        }
    }

    /// The defect this guards against: a silent removal. Six weeks on, nobody
    /// can tell a deliberate retraction from an accident without a reason.
    #[test]
    fn a_retraction_with_no_reason_is_refused_and_writes_nothing() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let err = retract(&mut store, "s1", "l1", "test", &item.id, "   ").unwrap_err();
        assert!(matches!(err, WriteError::Refused(_)));
        assert_eq!(
            store.get_all_events().unwrap().len(),
            1,
            "a refused retraction must append nothing"
        );
        show(&store, &item.id).expect("and the item must still be live");
    }

    /// Nothing is deleted: the log keeps every step, retraction included, so
    /// the history is still walkable after the item stops being live.
    #[test]
    fn history_still_walks_an_item_that_was_retracted() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s1", "l1", "test", &item).unwrap();
        let mut updated = item.clone();
        updated.text = "the config lives in config/app.toml, and is read once at start".to_string();
        revise(&mut store, "s1", "l1", "test", &item, &updated).unwrap();
        retract(&mut store, "s1", "l1", "test", &item.id, "app.toml was superseded by the new layout").unwrap();

        let log = history(&store, &item.id).unwrap();
        assert_eq!(log.len(), 3, "declare, revise and retract must all still be there");
        assert_eq!(log[0].kind, "fact_created");
        assert_eq!(log[2].kind, "fact_retracted");
        assert!(log[0].item.is_some(), "a readable body parses back into an item");
        assert!(log[2].item.is_none(), "a tombstone is not an item, and is not pretended to be one");
        assert!(log[0].seq < log[1].seq && log[1].seq < log[2].seq, "oldest first");
    }

    #[test]
    fn history_of_an_unknown_id_is_empty_not_an_error() {
        let store = EventStore::in_memory().unwrap();
        assert!(history(&store, "never-existed").unwrap().is_empty());
    }

    // ---------------------------------------------------- near-duplicate check
    //
    // THE DEFECT THESE PREVENT: storing a second near-identical copy of a
    // fact used to be stopped only by a tool description telling the caller
    // to search first - a convention, not a mechanism, and one that has been
    // observed failing: a caller told a rule already existed stored a second
    // copy of it anyway. Each test below is named after the exact failure
    // mode it pins shut.

    #[test]
    fn declare_refuses_an_exact_duplicate_and_names_the_existing_id() {
        let mut store = EventStore::in_memory().unwrap();
        let first = sample_with("first-item", "the config lives in config/app.toml");
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let second = sample_with("second-item", "the config lives in config/app.toml");
        let err = declare(&mut store, "s1", "l1", "test", &second).unwrap_err();
        match err {
            WriteError::Refused(r) => {
                assert!(
                    r.problem.contains("first-item") || r.fix.contains("first-item"),
                    "the refusal must name the existing item's id, got: {r}"
                );
                assert!(
                    r.fix.to_lowercase().contains("revise"),
                    "the refusal must say to revise the existing item instead: {r}"
                );
            }
            other => panic!("expected a Refused error for an exact duplicate, got {other:?}"),
        }
    }

    #[test]
    fn a_refused_duplicate_appends_nothing_to_the_log() {
        let mut store = EventStore::in_memory().unwrap();
        let first = sample_with("first-item", "the config lives in config/app.toml");
        declare(&mut store, "s1", "l1", "test", &first).unwrap();
        let before = store.get_all_events().unwrap().len();

        let second = sample_with("second-item", "the config lives in config/app.toml");
        let err = declare(&mut store, "s1", "l1", "test", &second).unwrap_err();
        assert!(matches!(err, WriteError::Refused(_)));

        let after = store.get_all_events().unwrap().len();
        assert_eq!(before, after, "a refused near-duplicate must append nothing to the log");
        assert!(
            matches!(show(&store, &second.id), Err(ReadError::NotFound(_))),
            "the refused item must not be readable either - nothing was ever written for it"
        );
    }

    #[test]
    fn a_paraphrase_with_high_word_overlap_is_refused_as_a_near_duplicate() {
        // Not an identical normalised string (the last word differs), but the
        // word-set overlap is well past the threshold - proves the Jaccard
        // path refuses on its own, not just the exact-text-match path.
        let mut store = EventStore::in_memory().unwrap();
        let first =
            sample_with("first-item", "never force-push to the main branch under any circumstance");
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let second =
            sample_with("second-item", "never force-push to the main branch under any circumstances");
        let err = declare(&mut store, "s1", "l1", "test", &second).unwrap_err();
        assert!(
            matches!(err, WriteError::Refused(_)),
            "a high-overlap paraphrase must be refused too, got {err:?}"
        );
    }

    #[test]
    fn a_genuinely_different_fact_of_the_same_kind_is_accepted() {
        let mut store = EventStore::in_memory().unwrap();
        let first = sample_with("first-item", "the config lives in config/app.toml");
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let second = sample_with("second-item", "never force-push to the main branch");
        declare(&mut store, "s1", "l1", "test", &second)
            .expect("a genuinely different fact of the same kind must not be refused");
    }

    /// The defect this guards against, and it is this check working against
    /// its own purpose. Two repositories can genuinely need the same rule -
    /// the same licence wording, the same release step. Comparing across every
    /// project meant the second one could not have it, and the only shape that
    /// satisfied both was ONE GLOBAL fact, which then fires in every project on
    /// the machine. Observed 2026-08-07 with a licence rule that was global and
    /// therefore fired on a GPLv3 repository claiming it was non-commercial.
    #[test]
    fn the_same_rule_is_accepted_in_a_different_project() {
        let mut store = EventStore::in_memory().unwrap();
        let mut first = sample_with("licence-a", "never call this project free and open-source");
        first.project = Some("project-a".to_string());
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let mut second = sample_with("licence-b", "never call this project free and open-source");
        second.project = Some("project-b".to_string());
        declare(&mut store, "s1", "l1", "test", &second).expect(
            "the same constraint in a DIFFERENT project is not a second copy - refusing it \
             leaves one global fact as the only option, which is the defect",
        );
    }

    #[test]
    fn the_same_rule_twice_in_one_project_is_still_refused() {
        let mut store = EventStore::in_memory().unwrap();
        let mut first = sample_with("licence-a", "never call this project free and open-source");
        first.project = Some("project-a".to_string());
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let mut second = sample_with("licence-b", "never call this project free and open-source");
        second.project = Some("project-a".to_string());
        assert!(
            matches!(declare(&mut store, "s1", "l1", "test", &second), Err(WriteError::Refused(_))),
            "within ONE project the duplicate check must still bite"
        );
    }

    /// A global item already fires inside every project, so a scoped copy of
    /// it really is a second copy of something already being served there.
    /// Skipping this pairing would let the store fill with per-project echoes
    /// of one global rule.
    #[test]
    fn a_scoped_copy_of_a_global_rule_is_still_refused() {
        let mut store = EventStore::in_memory().unwrap();
        let mut global = sample_with("global-rule", "never call this project free and open-source");
        global.project = None;
        declare(&mut store, "s1", "l1", "test", &global).unwrap();

        let mut scoped = sample_with("scoped-rule", "never call this project free and open-source");
        scoped.project = Some("project-a".to_string());
        assert!(
            matches!(declare(&mut store, "s1", "l1", "test", &scoped), Err(WriteError::Refused(_))),
            "a global rule already fires here, so a scoped copy is a second copy"
        );
    }

    #[test]
    fn the_same_text_under_a_different_kind_is_accepted() {
        let mut store = EventStore::in_memory().unwrap();
        let first = sample_with("first-item", "the config lives in config/app.toml");
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let mut second = sample_with("second-item", "the config lives in config/app.toml");
        second.kind = Kind::Chunk;
        second.bindings = vec![]; // archive kinds may carry no binding (gate ground 3)
        second.falsifier = None; // only Rule/Orientation require one (gate ground 10)
        declare(&mut store, "s1", "l1", "test", &second)
            .expect("identical text under a DIFFERENT kind must not be refused as a near-duplicate");
    }

    // --------------------------------------------- GROUND 29: check_opposition
    //
    // Deliberately DISTINCT text between the two sides of every pair below,
    // the same reason the capacity tests further down use `DISTINCT`: the
    // near-duplicate check (above) runs BEFORE this ground in `declare`, so a
    // near-identical pair would be refused for the wrong reason and quietly
    // test nothing about opposition at all.

    /// A minimal, otherwise-valid Rule carrying (or not carrying) `check` -
    /// built fresh for this section rather than reusing `sample_with`, whose
    /// own default text is close enough between any two calls to trip the
    /// near-duplicate check first (see this section's own banner comment).
    fn opposable(id: &str, text: &str, check: Option<Check>) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: text.to_string(),
            bindings: vec![Binding::Always],
            severity: Some(Severity::Costly),
            project: Some("thor2".to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some(format!("{text} turns out to be false")),
            check,
        }
    }

    /// THE GROUND ITSELF: `Contains` and `Absent` on the exact same literal,
    /// in the exact same file, can never both hold - refused, and the
    /// refusal names the rival by id and quotes its own text.
    #[test]
    fn declare_refuses_a_direct_opposition_and_names_the_rival() {
        let mut store = EventStore::in_memory().unwrap();
        let first = opposable(
            "license-says-gpl",
            "the license file must say GPLv3",
            Some(Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() }),
        );
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let second = opposable(
            "license-must-not-say-gpl",
            "the license file must never mention GPLv3",
            Some(Check::Absent { path: "LICENSE".to_string(), literal: "GPLv3".to_string() }),
        );
        let err = declare(&mut store, "s1", "l1", "test", &second).unwrap_err();
        match err {
            WriteError::Refused(r) => {
                assert!(r.problem.contains("license-says-gpl"), "the refusal must name the rival's id, got: {r}");
                assert!(
                    r.problem.contains("the license file must say GPLv3"),
                    "the refusal must quote the rival's own text, got: {r}"
                );
                assert!(
                    r.fix.to_lowercase().contains("correct one of the two"),
                    "the refusal must say to correct one of the two rather than store both, got: {r}"
                );
            }
            other => panic!("expected a Refused error for a direct opposition, got {other:?}"),
        }
        assert!(
            matches!(show(&store, &second.id), Err(ReadError::NotFound(_))),
            "a refused opposition must write nothing"
        );
    }

    /// The same file, but a DIFFERENT literal: both checks can hold at once
    /// (the license file can perfectly well mention GPLv3 and never mention
    /// MIT), so this is not an opposition at all and must be stored.
    #[test]
    fn a_same_file_pair_that_is_not_an_opposition_is_stored() {
        let mut store = EventStore::in_memory().unwrap();
        let first = opposable(
            "license-says-gpl",
            "the license file must say GPLv3",
            Some(Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() }),
        );
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let second = opposable(
            "license-must-not-say-mit",
            "the license file must never mention the MIT license",
            Some(Check::Absent { path: "LICENSE".to_string(), literal: "MIT".to_string() }),
        );
        declare(&mut store, "s1", "l1", "test", &second)
            .expect("a same-file pair on different literals is not an opposition and must be stored");
    }

    /// A note with no check at all can never be in opposition with anything,
    /// however loud its text is about the same file a rival's check anchors.
    #[test]
    fn a_note_without_a_check_is_unaffected() {
        let mut store = EventStore::in_memory().unwrap();
        let first = opposable(
            "license-says-gpl",
            "the license file must say GPLv3",
            Some(Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() }),
        );
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        // Severity dropped to `HouseStyle` (never Costly/Irreversible) so
        // this exercises ONLY the fact under test - GROUND 29 - and not
        // GROUND 11's own, unrelated demand that a HEAVY rule with no check
        // carry a reasoned `no-literal` tag instead.
        let mut second =
            opposable("license-note", "the license choice was made deliberately, see the project README", None);
        second.severity = Some(Severity::HouseStyle);
        declare(&mut store, "s1", "l1", "test", &second)
            .expect("an item with no check at all can never be in opposition with anything");
    }

    /// The exact same literal, opposite polarity - but a DIFFERENT file, so
    /// the two are not about the same fact at all and this ground must stay
    /// silent.
    #[test]
    fn the_refusal_does_not_fire_for_a_pair_on_different_files() {
        let mut store = EventStore::in_memory().unwrap();
        let first = opposable(
            "license-says-gpl",
            "the license file must say GPLv3",
            Some(Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() }),
        );
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let second = opposable(
            "readme-must-not-say-gpl",
            "the README must never mention GPLv3",
            Some(Check::Absent { path: "README.md".to_string(), literal: "GPLv3".to_string() }),
        );
        declare(&mut store, "s1", "l1", "test", &second)
            .expect("an opposition on a DIFFERENT file must never be refused");
    }

    /// PROJECT SCOPING, the same rule `unsettled_neighbours` already
    /// established and `find_opposing_check`'s own doc comment explains: a
    /// path is only unique WITHIN a project, so an opposition across two
    /// DIFFERENT projects' own copies of a same-named file is not a real
    /// conflict - each checkout has its own file.
    #[test]
    fn a_direct_opposition_in_a_different_project_is_not_refused() {
        let mut store = EventStore::in_memory().unwrap();
        let mut first = opposable(
            "license-says-gpl",
            "the license file must say GPLv3",
            Some(Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() }),
        );
        first.project = Some("project-a".to_string());
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let mut second = opposable(
            "license-must-not-say-gpl",
            "the license file must never mention GPLv3",
            Some(Check::Absent { path: "LICENSE".to_string(), literal: "GPLv3".to_string() }),
        );
        second.project = Some("project-b".to_string());
        declare(&mut store, "s1", "l1", "test", &second)
            .expect("the same path in a DIFFERENT project names a different real file, not an opposition");
    }

    /// Wildly different sentences on purpose: the near-duplicate check runs
    /// before the capacity check, and near-identical fixtures would be
    /// refused for the wrong reason and quietly test nothing.
    const DISTINCT: [&str; 9] = [
        "the changelog belongs at the repository root and nowhere else",
        "database migrations run forward only, never in reverse",
        "every uploaded image is stripped of its location metadata",
        "the nightly job writes its report before it deletes anything",
        "a released version number is never reused for a different build",
        "connection pools close on shutdown, in the opposite order they opened",
        "the search index rebuilds from source, never from its own output",
        "temporary credentials expire within the hour they were issued",
        "a queue consumer acknowledges only after the work has landed",
    ];

    fn bound_to(id: &str, nth: usize, moment: intent::Action, severity: Option<Severity>) -> Item {
        let mut item = sample_with(id, DISTINCT[nth % DISTINCT.len()]);
        item.bindings = vec![Binding::Moment(moment)];
        item.severity = severity;
        item
    }

    /// THE DEFECT THIS PREVENTS, and CONTRACT R1 named it long before anything
    /// implemented it: an item every one of whose bindings is already full of
    /// heavier rivals is stored, fires nowhere, and nothing says so. Severity
    /// is compared before closeness, so no future moment can lift it in - it
    /// is not unlucky, it is unreachable.
    #[test]
    fn an_item_that_can_never_reach_a_block_is_refused() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let heavy = bound_to(&format!("heavy-{i}"), i, intent::Action::Commit, Some(Severity::Irreversible));
            declare(&mut store, "s", "l", "t", &heavy).expect("fixture must store");
        }
        let light = bound_to("light-one", 8, intent::Action::Commit, Some(Severity::HouseStyle));
        let err = declare(&mut store, "s", "l", "t", &light).expect_err("it can never be shown");
        let msg = format!("{err}");
        assert!(msg.contains("never reach a block"), "the refusal must say why: {msg}");
        assert!(msg.contains("heavy-"), "and name what holds the places: {msg}");
        // FIX 2026-09-17: the fix text used to float "or raise its severity
        // if that is honestly what it is" as one of the ways out - exactly
        // the compensating knob the evaluation routine forbids (it pushes a
        // heavier warning out to show a lighter one). That INVITATION must
        // be gone; the new text still names the same three words, but only
        // inside a PROHIBITION ("Do not raise its severity"), which is why
        // this checks the old lead-in specifically rather than banning the
        // phrase outright.
        assert!(!msg.contains("or raise its severity"), "must not invite winning the place by raising severity: {msg}");
        assert!(
            msg.contains("Do not raise its severity to win the place"),
            "must say plainly why not, instead: {msg}"
        );
    }

    /// The other half, and the reason the refusal is narrow: a rival of the
    /// SAME weight might still lose to this item on closeness at some future
    /// moment. Refusing on that would be refusing on a guess, which is how a
    /// gate teaches people to route around it.
    #[test]
    fn a_full_pool_of_equals_is_a_note_and_never_a_refusal() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS + 2 {
            let peer = bound_to(&format!("peer-{i}"), i, intent::Action::Commit, Some(Severity::Costly));
            declare(&mut store, "s", "l", "t", &peer).expect("fixture must store");
        }
        let another = bound_to("another-peer", 8, intent::Action::Commit, Some(Severity::Costly));
        declare(&mut store, "s", "l", "t", &another).expect("an equal must still be storable");

        match capacity(&store, &another).unwrap() {
            Capacity::Crowded(note) => assert!(note.contains("same weight or heavier"), "{note}"),
            other => panic!("a full pool of equals must be reported, got {other:?}"),
        }
    }

    /// A second binding that is free saves the item: it is only dead when
    /// EVERY door is shut. Refusing while one is open would delete a fact that
    /// works perfectly well somewhere else.
    #[test]
    fn one_free_binding_is_enough_to_be_storable() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let heavy = bound_to(&format!("h-{i}"), i, intent::Action::Commit, Some(Severity::Irreversible));
            declare(&mut store, "s", "l", "t", &heavy).expect("fixture must store");
        }
        let mut light = bound_to("two-doors", 8, intent::Action::Commit, Some(Severity::HouseStyle));
        light.bindings.push(Binding::Target { kind: TargetKind::Path, value: "src/lonely.rs".to_string() });
        declare(&mut store, "s", "l", "t", &light).expect("the free target binding must save it");
    }

    /// THE DEFECT THIS PREVENTS, and it is one this file caused itself. For a
    /// few hours on 2026-08-08 a rule here made a Dir-bound rival compete with
    /// a Path-bound item, on the belief that a file touch offers the parent
    /// directory as a second target. It does not: `ServeInput::add_file` adds
    /// one target, a Path, and `normalize::target_matches` refuses a kind
    /// mismatch, so that pool is never assembled anywhere. (The false sentence
    /// itself is not repeated here; `serve/tests/
    /// a_comment_never_claims_what_the_code_does_not_do.rs` fails if it comes
    /// back, because copying it from one file to another is how it spread.)
    ///
    /// This count decides a REFUSAL, so an over-count is not an error in the
    /// safe direction: it refuses an honest write for rivals it will never
    /// meet. Two independent reviews found it the same evening, both by
    /// reading `input.rs` rather than the comment that claimed otherwise.
    ///
    /// FOUR RIVALS, NOT SIX. This fixture used to over-provision the Dir
    /// anchor on purpose, to prove a refusal survives more rivals than the cap
    /// needs. Since `full_reliable_anchor` now refuses a FIFTH Dir-bound
    /// arrival at the very same anchor too - the same "no crowding" rule this
    /// test is not about - the fixture can no longer be built past the cap
    /// through the front door, so it stops at exactly `MAX_ITEMS`, which is
    /// still every bit as full.
    #[test]
    fn a_directory_bound_rival_now_shares_the_pool_of_a_file_inside_it() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let mut heavy = sample_with(&format!("dir-{i}"), DISTINCT[i % DISTINCT.len()]);
            heavy.bindings = vec![Binding::Target { kind: TargetKind::Dir, value: "src/deep".to_string() }];
            heavy.severity = Some(Severity::Irreversible);
            declare(&mut store, "s", "l", "t", &heavy).expect("fixture must store");
        }
        let mut light = sample_with("newcomer", "a webhook retry backs off before it gives up entirely");
        light.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "src/deep/mod.rs".to_string() }];
        light.severity = Some(Severity::HouseStyle);

        // BEFORE 2026-08-19 this write went through, because a directory
        // anchor could not reach a file inside it at all - which also meant
        // those four rules were silent at every file they were written for.
        // Now they do reach it, so they are real rivals and the count says
        // so. The refusal names them, and a heavier or more precise binding
        // is the way past it.
        let refused = declare(&mut store, "s", "l", "t", &light)
            .expect_err("four heavier rivals at a place with four seats is exactly what crowding means");
        let said = format!("{refused:?}");
        assert!(said.contains("src/deep/mod.rs"), "the place it could not get into: {said}");
        assert!(said.contains("dir-0"), "and who is holding it: {said}");
    }

    /// A note, never a refusal: a directory anchor is a legitimate thing to
    /// write. But its breadth has to be SAID, because a rule about a whole
    /// tree reads like a rule about the file you are touching, and is served
    /// after one.
    ///
    /// `Capacity::Broad`, not `Capacity::Crowded`, since 2026-09-17 - see
    /// `Capacity::Broad`'s own doc comment for the measured case a shared
    /// shape with `Crowded` caused: `crowding_debt` (`bin/serve.rs`) read
    /// this exact note as a rival pool being full and nagged the writer to
    /// fold, re-anchor, or tag the item crowded-on-purpose, for a place that
    /// was never actually full.
    #[test]
    fn a_directory_only_item_is_told_how_broadly_it_reaches() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = sample_with("dir-only", "a webhook retry backs off before it gives up entirely");
        item.bindings = vec![Binding::Target { kind: TargetKind::Dir, value: "src/deep".to_string() }];

        match capacity(&store, &item).unwrap() {
            Capacity::Broad(note) => {
                assert!(note.contains("DIRECTORY"), "{note}");
                assert!(note.contains("every file inside that directory"), "how far it reaches: {note}");
                assert!(note.contains("read AFTER"), "and where that puts it in the order: {note}");
            }
            other => panic!("a directory-only item must be warned about, with Broad specifically, got {other:?}"),
        }
        declare(&mut store, "s", "l", "t", &item).expect("a note never refuses the write");

        // A Path binding beside it takes the note away, because then the item
        // really is reachable at a file touch.
        let mut reachable = sample_with("dir-plus-path", "the estimator rounds a quote up to whole cents");
        reachable.bindings = vec![
            Binding::Target { kind: TargetKind::Dir, value: "src/deep".to_string() },
            Binding::Target { kind: TargetKind::Path, value: "src/deep/mod.rs".to_string() },
        ];
        assert!(
            matches!(capacity(&store, &reachable).unwrap(), Capacity::Fine),
            "a path binding beside the directory makes it reachable"
        );
    }

    /// A pinned item is never in a pool: session start serves every pin in
    /// full and caps nothing, so a pin cannot take another pin's place.
    /// Counting them would refuse writes for crowding that does not exist.
    #[test]
    fn a_pinned_item_is_never_refused_for_crowding() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS + 3 {
            let mut pin = sample_with(&format!("pin-{i}"), DISTINCT[i % DISTINCT.len()]);
            pin.bindings = vec![Binding::Always];
            pin.severity = Some(Severity::Irreversible);
            declare(&mut store, "s", "l", "t", &pin).expect("fixture must store");
        }
        let mut another = sample_with("pin-more", "a webhook retry backs off before it gives up entirely");
        another.bindings = vec![Binding::Always];
        another.severity = Some(Severity::HouseStyle);
        declare(&mut store, "s", "l", "t", &another).expect("a pin never competes for a place");
    }

    fn path_bound(id: &str, nth: usize, path: &str, severity: Option<Severity>) -> Item {
        let mut item = sample_with(id, DISTINCT[nth % DISTINCT.len()]);
        item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: path.to_string() }];
        item.severity = severity;
        item
    }

    /// THE DEFECT THIS CLOSES: a big hygiene pass left a displaced fact behind
    /// because the write gate only ever NOTED a full pool of equals, never
    /// refused it - an agent could ignore the note and move on. A Path
    /// binding's sitting rivals are countable exactly the way
    /// `full_reliable_anchor` requires, so this is now provable, not a guess -
    /// see that function's own doc comment for why a Moment binding still
    /// only gets the note (`a_full_pool_of_equals_is_a_note_and_never_a_refusal`
    /// above proves that half is unchanged).
    #[test]
    fn a_fifth_equal_weight_fact_on_a_full_path_anchor_is_refused() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let sitting = path_bound(&format!("sit-{i}"), i, "src/shared/config.rs", Some(Severity::Costly));
            declare(&mut store, "s", "l", "t", &sitting).expect("fixture must store");
        }
        let newcomer = path_bound("newcomer", 8, "src/shared/config.rs", Some(Severity::Costly));
        let err = declare(&mut store, "s", "l", "t", &newcomer).expect_err("the anchor already holds the cap");
        let msg = format!("{err}");
        assert!(msg.contains("src/shared/config.rs"), "names the place: {msg}");
        assert!(msg.contains("sit-0"), "names a sitting fact so the agent can choose: {msg}");
        assert!(msg.contains("fold"), "says how to make room: {msg}");
        assert!(msg.contains("no crowding, no loose ends"), "names the rule by its own words: {msg}");
    }

    /// The other half of the same defect: a HEAVIER newcomer is not a free
    /// pass either. Neither the hopeless check (needs rivals STRICTLY
    /// heavier) nor the equal-or-heavier note (needs rivals AT LEAST as
    /// heavy) ever looks at a rival LIGHTER than the newcomer, so before this
    /// fix a heavier arrival on a full anchor of lighter facts sailed through
    /// both unmentioned, silently bumping the lightest one.
    #[test]
    fn a_heavier_fact_on_a_full_path_anchor_is_also_refused() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let sitting = path_bound(&format!("light-{i}"), i, "src/shared/legacy.rs", Some(Severity::HouseStyle));
            declare(&mut store, "s", "l", "t", &sitting).expect("fixture must store");
        }
        let newcomer = path_bound("heavy-newcomer", 8, "src/shared/legacy.rs", Some(Severity::Irreversible));
        let err = declare(&mut store, "s", "l", "t", &newcomer)
            .expect_err("a heavier arrival must not silently displace a lighter sitting fact");
        assert!(format!("{err}").contains("light-0"), "{err}");
    }

    /// A place under the cap is exactly as free as it looks - three sitting
    /// facts leave a fourth seat open, no fold or archive required.
    #[test]
    fn a_path_anchor_with_room_accepts_one_more() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS - 1 {
            let sitting = path_bound(&format!("room-{i}"), i, "src/shared/roomy.rs", Some(Severity::Costly));
            declare(&mut store, "s", "l", "t", &sitting).expect("fixture must store");
        }
        let newcomer = path_bound("fits", 8, "src/shared/roomy.rs", Some(Severity::Costly));
        declare(&mut store, "s", "l", "t", &newcomer).expect("three sitting facts leave a fourth seat open");
    }

    /// The scope is "Path, Dir or Command", not "Path alone" - a Command
    /// anchor at capacity is counted and refused the same way.
    #[test]
    fn a_command_anchor_at_capacity_is_also_refused() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let mut sitting = sample_with(&format!("cmd-{i}"), DISTINCT[i % DISTINCT.len()]);
            sitting.bindings =
                vec![Binding::Target { kind: TargetKind::Command, value: "terraform apply".to_string() }];
            // Heavy and anchored to a command, so ground 11 (continued) asks
            // its own sharper question here - this fixture is about a FULL
            // anchor, not about teeth, and answers it so the two grounds stay
            // testable apart.
            sitting.tags.push(format!(
                "{}this fixture fills an anchor where 'the same text' is also legitimate",
                crate::store::NO_CATCH_ON_COMMAND_REASON_PREFIX
            ));
            declare(&mut store, "s", "l", "t", &sitting).expect("fixture must store");
        }
        let mut newcomer = sample_with("cmd-newcomer", "a webhook retry backs off before it gives up entirely");
        newcomer.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "terraform apply".to_string() }];
        newcomer.tags.push(format!(
            "{}this fixture fills an anchor where 'the same text' is also legitimate",
            crate::store::NO_CATCH_ON_COMMAND_REASON_PREFIX
        ));
        declare(&mut store, "s", "l", "t", &newcomer).expect_err("a full Command anchor refuses too");
    }

    /// Clearing room first, then writing, is exactly the order the refusal's
    /// own fix text asks for - retracting one sitting fact is enough on its
    /// own for the write that was refused a moment ago to go through
    /// untouched.
    #[test]
    fn retracting_a_sitting_fact_clears_room_for_the_next_write() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let sitting = path_bound(&format!("sit-{i}"), i, "src/shared/queue.rs", Some(Severity::Costly));
            declare(&mut store, "s", "l", "t", &sitting).expect("fixture must store");
        }
        let newcomer = path_bound("newcomer", 8, "src/shared/queue.rs", Some(Severity::Costly));
        declare(&mut store, "s", "l", "t", &newcomer).expect_err("fixture sanity: the anchor starts full");

        retract(&mut store, "s", "l", "t", "sit-0", "folded into 'sit-1'").unwrap();
        declare(&mut store, "s", "l", "t", &newcomer).expect("one retraction is enough room");
    }

    /// The fold order the refusal itself recommends - revise the survivor to
    /// carry the point BEFORE retracting the one it absorbs - must not be
    /// refused for standing exactly where it already stood: `full_reliable_
    /// anchor` only judges a binding new to this write, never one the item
    /// already carried, so the survivor can be revised while the anchor is
    /// still at capacity.
    #[test]
    fn revising_a_sitting_fact_in_place_on_a_full_anchor_is_not_refused() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let sitting = path_bound(&format!("sit-{i}"), i, "src/shared/worker.rs", Some(Severity::Costly));
            declare(&mut store, "s", "l", "t", &sitting).expect("fixture must store");
        }
        let existing = show(&store, "sit-0").unwrap();
        let mut folded = existing.clone();
        folded.text = format!("{} - and also covers the point sit-1 made", existing.text);
        revise(&mut store, "s", "l", "t", &existing, &folded)
            .expect("revising a sitting fact in place must not be refused for a crowd it did not add to");
    }

    /// A revise that ADDS a Path/Dir/Command binding the item did not already
    /// carry is a genuinely new arrival at that anchor, and `revise` judges it
    /// exactly like `declare` would.
    #[test]
    fn revise_adding_a_new_binding_onto_a_full_anchor_is_refused() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let sitting = path_bound(&format!("sit-{i}"), i, "src/shared/gateway.rs", Some(Severity::Costly));
            declare(&mut store, "s", "l", "t", &sitting).expect("fixture must store");
        }
        let elsewhere = path_bound("elsewhere", 8, "src/shared/other.rs", Some(Severity::Costly));
        declare(&mut store, "s", "l", "t", &elsewhere).expect("fixture must store");

        let existing = show(&store, "elsewhere").unwrap();
        let mut widened = existing.clone();
        widened.bindings.push(Binding::Target { kind: TargetKind::Path, value: "src/shared/gateway.rs".to_string() });
        revise(&mut store, "s", "l", "t", &existing, &widened)
            .expect_err("adding a binding to a full anchor is a new arrival there, judged as one");
    }

    // ------------------------------------------------ capacity_for_revise

    /// Appends a live `FactCreated` directly, bypassing `declare`'s own write
    /// gate.
    ///
    /// WHY A TEST NEEDS A BACK DOOR HERE. The real crowding this fix is
    /// about - measured 2026-09-12, "5 item(s) of the same weight or
    /// heavier, for 4 place(s)" - cannot be built through `declare` any more:
    /// `full_reliable_anchor` refuses a fifth arrival at a Path/Dir/Command
    /// binding already holding `item::MAX_ITEMS`, so five co-resident rivals
    /// is exactly the state today's front door no longer allows a NEW item to
    /// reach (see `a_fifth_equal_weight_fact_on_a_full_path_anchor_is_refused`
    /// above). The real fixture that surfaced this bug predates that refusal
    /// (added 2.2.1, 2026-09-05); a raw append is the only way this test can
    /// put a temp store back in that same, still-live shape.
    fn seed_live(store: &mut EventStore, item: &Item) {
        let body = canonical_body(item).unwrap();
        store.append_event("s", "l", "t", EventKind::FactCreated, &item.id, None, &body).unwrap();
    }

    /// One `item_served` event for `id`, appended directly - the shape
    /// `serve::deliver::record_delivery` writes in production
    /// (`model::served::ItemServed`), built here because `model`'s own tests
    /// cannot depend on `serve` to call that function (see `served.rs`'s own
    /// top comment for why the dependency runs the other way).
    fn seed_served(store: &mut EventStore, id: &str, served_at: &str) {
        let body = serde_json::to_string(&crate::served::ItemServed { served_at: served_at.to_string(), trigger: None })
            .unwrap();
        store.append_event("s", "l", "t", EventKind::ItemServed, id, None, &body).unwrap();
    }

    /// `mine`, live at `path` alongside `item::MAX_ITEMS` other live items of
    /// the SAME weight - exactly the shape `crowded_binding_note` reports on
    /// (`at_least_equal >= item::MAX_ITEMS`), via `seed_live` rather than
    /// `declare` for the reason that function's own doc comment gives.
    fn seeded_crowded_item(store: &mut EventStore, path: &str) -> Item {
        let mine = path_bound("mine", 0, path, Some(Severity::Costly));
        seed_live(store, &mine);
        for i in 0..crate::item::MAX_ITEMS {
            let rival = path_bound(&format!("rival-{i}"), i + 1, path, Some(Severity::Costly));
            seed_live(store, &rival);
        }
        mine
    }

    const NOW: &str = "2026-09-12T00:00:00Z";
    const RECENTLY: &str = "2026-09-10T00:00:00Z";

    /// THE MEASUREMENT ITSELF: a revise that leaves its bindings untouched,
    /// on an item the log shows was actually served at that very binding,
    /// gets a measured line instead of a guess - see `capacity_for_revise`'s
    /// own doc comment for the day a prediction and a 55-servings measurement
    /// disagreed about the same binding.
    #[test]
    fn capacity_for_revise_reports_a_measured_line_when_served_and_bindings_are_unchanged() {
        let mut store = EventStore::in_memory().unwrap();
        let existing = seeded_crowded_item(&mut store, "src/ops/install.rs");
        seed_served(&mut store, &existing.id, RECENTLY);
        seed_served(&mut store, &existing.id, RECENTLY);

        let mut updated = existing.clone();
        updated.falsifier = Some("a different falsifier entirely".to_string());

        match capacity_for_revise(&store, &existing, &updated, NOW).unwrap() {
            Capacity::Crowded(note) => {
                assert!(note.contains("shown 2 time(s)"), "{note}");
                assert!(note.contains("not crowded out"), "{note}");
                assert!(!note.contains("may well never be shown"), "the measured line replaces the guess: {note}");
            }
            other => panic!("expected a measured Crowded note, got {other:?}"),
        }
    }

    /// THE OTHER HALF: no servings on record at all, so there is nothing to
    /// measure - the ordinary prediction stands exactly as it did before this
    /// fix existed.
    #[test]
    fn capacity_for_revise_keeps_the_prediction_when_never_served() {
        let mut store = EventStore::in_memory().unwrap();
        let existing = seeded_crowded_item(&mut store, "src/ops/never-served.rs");

        let mut updated = existing.clone();
        updated.falsifier = Some("a different falsifier entirely".to_string());

        match capacity_for_revise(&store, &existing, &updated, NOW).unwrap() {
            Capacity::Crowded(note) => assert!(note.contains("may well never be shown there"), "{note}"),
            other => panic!("expected the ordinary predicted Crowded note, got {other:?}"),
        }
    }

    /// A revise that also moves the item - even just ADDING a binding, the
    /// old ones untouched - is a real unknown again: the service history at
    /// the binding that stayed put says nothing about whether the new one
    /// will ever be reached, so this keeps the prediction too.
    #[test]
    fn capacity_for_revise_keeps_the_prediction_when_bindings_changed() {
        let mut store = EventStore::in_memory().unwrap();
        let existing = seeded_crowded_item(&mut store, "src/ops/moved.rs");
        seed_served(&mut store, &existing.id, RECENTLY);

        let mut updated = existing.clone();
        updated.bindings.push(Binding::Target { kind: TargetKind::Path, value: "src/ops/also.rs".to_string() });

        match capacity_for_revise(&store, &existing, &updated, NOW).unwrap() {
            Capacity::Crowded(note) => {
                assert!(note.contains("may well never be shown there"), "a changed binding is a new unknown: {note}")
            }
            other => panic!("expected the ordinary predicted Crowded note, got {other:?}"),
        }
    }

    /// A brand new `declare` has no `existing` to compare against at all -
    /// plain `capacity` (what `declare`'s own response note still reads)
    /// never measures, however often the id happens to have fired under some
    /// earlier life of the same string.
    #[test]
    fn plain_capacity_never_measures_even_when_served() {
        let mut store = EventStore::in_memory().unwrap();
        let mine = seeded_crowded_item(&mut store, "src/ops/fresh.rs");
        seed_served(&mut store, &mine.id, RECENTLY);

        match capacity(&store, &mine).unwrap() {
            Capacity::Crowded(note) => assert!(note.contains("may well never be shown there"), "{note}"),
            other => panic!("expected the ordinary predicted Crowded note, got {other:?}"),
        }
    }

    /// GATE-LEVEL: the same measurement, proved through the real `revise`
    /// entry point on a temp store rather than against the decision function
    /// in isolation - the write actually lands (bindings unchanged, so
    /// `full_reliable_anchor` never sees a new arrival at a full anchor), and
    /// the note `with_warnings` would attach is computed the same way
    /// production does.
    #[test]
    fn a_real_revise_that_keeps_its_binding_gets_the_measured_line() {
        let mut store = EventStore::in_memory().unwrap();
        let existing = seeded_crowded_item(&mut store, "src/ops/real-revise.rs");
        for _ in 0..3 {
            seed_served(&mut store, &existing.id, RECENTLY);
        }

        let mut updated = existing.clone();
        updated.falsifier = Some("the install script stops writing this file".to_string());
        revise(&mut store, "s", "l", "t", &existing, &updated).expect("bindings unchanged, must not be refused");

        match capacity_for_revise(&store, &existing, &updated, NOW).unwrap() {
            Capacity::Crowded(note) => {
                assert!(note.contains("shown 3 time(s)"), "{note}");
                assert!(note.contains("not crowded out"), "{note}");
            }
            other => panic!("expected a measured Crowded note after the real revise, got {other:?}"),
        }
    }

    /// The promise archiving makes: the words survive, the claim to fire does
    /// not. If this ever stops holding, archiving has quietly become a
    /// retraction with extra steps.
    #[test]
    fn archiving_keeps_the_words_and_drops_only_the_claim_to_fire() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = sample_with("measured-thing", "that reranker failed both gates");
        item.project = Some("some-project".to_string());
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        archive(&mut store, "s1", "l1", "test", "measured-thing", "the script it measured is gone", None).unwrap();

        let after = show(&store, "measured-thing").unwrap();
        assert_eq!(after.kind, Kind::Report, "it must stop being a kind that claims to fire");
        assert!(!after.kind.can_fire());
        assert_eq!(after.text, "that reranker failed both gates", "the words must survive untouched");
        assert_eq!(after.project.as_deref(), Some("some-project"), "the scope must survive");
        assert!(after.bindings.is_empty(), "a Report may carry no bindings");
        assert!(
            after.tags.iter().any(|t| t == "archived:the script it measured is gone"),
            "the reason must survive as a tag, or nobody can tell this from an accident: {:?}",
            after.tags
        );
    }

    /// An archived item stays in the live set. That is the whole difference
    /// from retracting: `lookup` must still find it.
    #[test]
    fn an_archived_item_is_still_live_and_findable() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample_with("measured-thing", "that reranker failed both gates");
        declare(&mut store, "s1", "l1", "test", &item).unwrap();
        archive(&mut store, "s1", "l1", "test", "measured-thing", "anchor gone", None).unwrap();

        assert!(
            show(&store, "measured-thing").is_ok(),
            "archiving must not remove the item - that is what retract is for"
        );
    }

    #[test]
    fn archiving_refuses_without_a_reason() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample_with("thing", "some fact");
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        assert!(matches!(
            archive(&mut store, "s1", "l1", "test", "thing", "   ", None),
            Err(WriteError::Refused(_))
        ));
        assert_eq!(show(&store, "thing").unwrap().kind, Kind::Orientation, "a refused archive changes nothing");
    }

    /// The defect this guards against: archiving a rule that can prove itself
    /// would throw away the only enforcement this memory actually has. A rule
    /// with a runnable check is the one kind that can block a wrong write.
    #[test]
    fn archiving_refuses_a_rule_that_can_still_prove_itself() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = sample_with("provable", "the config lives in config/app.toml");
        item.check = Some(Check::PathExists { path: "config/app.toml".to_string() });
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let err = archive(&mut store, "s1", "l1", "test", "provable", "anchor gone", None)
            .expect_err("a provable rule must not be archivable in one step");
        assert!(format!("{err:?}").contains("check"), "the refusal must name why: {err:?}");
        assert_eq!(show(&store, "provable").unwrap().kind, Kind::Orientation);
    }

    /// The other half, and the reason `root` exists at all (measured
    /// 2026-08-15): a check whose own file was deleted protects nothing, so
    /// refusing to archive it only kept 67 items alive that fire nowhere. With
    /// a root to judge against, a live check still refuses and a dead one gives
    /// way.
    #[test]
    fn archiving_allows_a_check_whose_file_is_gone_but_not_one_that_is_there() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("here.toml"), b"real").unwrap();

        let mut store = EventStore::in_memory().unwrap();
        let mut live = sample_with("still-there", "the config lives in here.toml");
        live.check = Some(Check::PathExists { path: "here.toml".to_string() });
        declare(&mut store, "s1", "l1", "test", &live).unwrap();
        let mut dead = sample_with("file-gone", "the tree this one watched was deleted");
        dead.check = Some(Check::Contains {
            path: "gone/src/lib.rs".to_string(),
            literal: "fn main".to_string(),
        });
        declare(&mut store, "s1", "l1", "test", &dead).unwrap();

        assert!(
            archive(&mut store, "s1", "l1", "test", "still-there", "tidying up", Some(dir.path()))
                .is_err(),
            "a check that can still inspect a real file is live protection"
        );
        archive(&mut store, "s1", "l1", "test", "file-gone", "the tree it watched is gone", Some(dir.path()))
            .expect("a check pointing at nothing cannot block, so it must not block its own archive");
        assert_eq!(show(&store, "file-gone").unwrap().kind, Kind::Report);
    }

    #[test]
    fn archiving_something_already_archived_is_refused_rather_than_repeated() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample_with("thing", "some fact");
        declare(&mut store, "s1", "l1", "test", &item).unwrap();
        archive(&mut store, "s1", "l1", "test", "thing", "anchor gone", None).unwrap();

        assert!(matches!(
            archive(&mut store, "s1", "l1", "test", "thing", "anchor gone", None),
            Err(WriteError::Refused(_))
        ));
        let after = show(&store, "thing").unwrap();
        assert_eq!(
            after.tags.iter().filter(|t| t.starts_with("archived:")).count(),
            1,
            "a second archive must not stack a second reason tag"
        );
    }

    /// The defect this guards against, and it is the one that actually
    /// happened. Six rules anchored at a gitignored secrets file were swept
    /// into the archive as "dead anchors". They were not dead: the anchor was
    /// there so they fire the moment anybody touches that file. One of them
    /// said so in its own text and the sweep read paths, not sentences.
    #[test]
    fn archiving_refuses_an_anchor_that_is_absent_on_purpose() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = sample_with("guards-secrets", "this file is gitignored and must never be committed");
        // Overwrites sample()'s tags wholesale, so its no-literal answer to
        // ground 11 has to come back too - this fixture is Costly with no
        // check, same as sample() itself.
        item.tags = vec![
            DELIBERATE_ANCHOR_TAG.to_string(),
            format!("{}a fixture about app.toml being absent on purpose", crate::store::NO_LITERAL_REASON_PREFIX),
        ];
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let err = archive(&mut store, "s1", "l1", "test", "guards-secrets", "anchor resolves to nothing", None)
            .expect_err("an anchor marked deliberate must survive any sweep");
        assert!(format!("{err:?}").contains("deliberately absent"), "the refusal must say why: {err:?}");
        assert!(show(&store, "guards-secrets").unwrap().kind.can_fire(), "it must still fire");
    }

    #[test]
    fn restoring_brings_back_the_kind_and_the_anchor_and_marks_it_deliberate() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample_with("guards-secrets", "this file is gitignored and must never be committed");
        declare(&mut store, "s1", "l1", "test", &item).unwrap();
        archive(&mut store, "s1", "l1", "test", "guards-secrets", "swept by mistake", None).unwrap();

        restore_deliberate_anchor(
            &mut store,
            "s1",
            "l1",
            "test",
            "guards-secrets",
            Kind::Rule,
            "firmware/src/secrets.h",
        )
        .unwrap();

        let after = show(&store, "guards-secrets").unwrap();
        assert_eq!(after.kind, Kind::Rule);
        assert_eq!(
            after.bindings,
            vec![Binding::Target { kind: TargetKind::Path, value: "firmware/src/secrets.h".to_string() }]
        );
        assert!(after.tags.iter().any(|t| t == DELIBERATE_ANCHOR_TAG), "it must be marked: {:?}", after.tags);
        assert!(!after.tags.iter().any(|t| t.starts_with("archived:")), "the archive reason must go: {:?}", after.tags);

        // And now it is immune to the same mistake.
        assert!(matches!(
            archive(&mut store, "s1", "l1", "test", "guards-secrets", "anchor resolves to nothing", None),
            Err(WriteError::Refused(_))
        ));
    }

    #[test]
    fn restoring_something_that_was_never_archived_is_refused() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample_with("thing", "some fact");
        declare(&mut store, "s1", "l1", "test", &item).unwrap();
        assert!(matches!(
            restore_deliberate_anchor(&mut store, "s1", "l1", "test", "thing", Kind::Rule, "a/b.rs"),
            Err(WriteError::Refused(_))
        ));
    }

    #[test]
    fn archiving_refuses_a_reason_that_would_pollute_search() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample_with("thing", "some fact");
        declare(&mut store, "s1", "l1", "test", &item).unwrap();

        let long = "x".repeat(ARCHIVE_REASON_LIMIT + 1);
        assert!(matches!(archive(&mut store, "s1", "l1", "test", "thing", &long, None), Err(WriteError::Refused(_))));
        assert!(matches!(
            archive(&mut store, "s1", "l1", "test", "thing", "two, reasons", None),
            Err(WriteError::Refused(_))
        ));
    }

    /// THE DEFECT THIS CLOSES, measured on the owner's live store 2026-08-16:
    /// one subject lived at three addresses at once, and nothing at write time
    /// ever said so. A register is reached ONLY by its key, and `by_key`
    /// answers with the first match - so a second register whose key differs
    /// by a capital or a hyphen is a whole collection that exists and can
    /// never be asked for.
    ///
    /// The text check cannot catch this: two registers about one subject hold
    /// completely different rows, which is exactly what the texts below are.
    #[test]
    fn a_second_register_on_the_same_key_is_refused_however_it_is_spelled() {
        let mut store = EventStore::in_memory().unwrap();

        let mut first = sample_with("reg-boeken", "2026-08-11 Dune\n2026-08-16 Piranesi");
        first.kind = Kind::Lookup;
        first.key = Some("boeken".to_string());
        first.bindings = vec![];
        first.falsifier = None;
        declare(&mut store, "s1", "l1", "test", &first).unwrap();

        let mut second = sample_with("reg-boeken-2", "2025-01-02 Anna Karenina");
        second.kind = Kind::Lookup;
        second.key = Some("Boeken".to_string());
        second.bindings = vec![];
        second.falsifier = None;

        match declare(&mut store, "s1", "l1", "test", &second) {
            Err(WriteError::Refused(r)) => {
                assert!(r.problem.contains("key"), "the refusal must name the real reason: {}", r.problem);
                assert!(r.problem.contains("reg-boeken"), "and name the item holding it: {}", r.problem);
            }
            other => panic!("a second register on the same key must be refused, got {other:?}"),
        }
    }

    /// A key that is genuinely different is not a collision. The guard exists
    /// to stop a second SPELLING, never a second collection - a monthly log is
    /// meant to have one register per month.
    #[test]
    fn two_registers_with_genuinely_different_keys_both_stand() {
        let mut store = EventStore::in_memory().unwrap();

        let mut july = sample_with("reg-uitgaven-07", "2026-07-04 filament 42 euro");
        july.kind = Kind::Lookup;
        july.key = Some("uitgaven 2026-07".to_string());
        july.bindings = vec![];
        july.falsifier = None;
        declare(&mut store, "s1", "l1", "test", &july).unwrap();

        let mut august = sample_with("reg-uitgaven-08", "2026-08-16 tandarts 85 euro");
        august.kind = Kind::Lookup;
        august.key = Some("uitgaven 2026-08".to_string());
        august.bindings = vec![];
        august.falsifier = None;
        declare(&mut store, "s1", "l1", "test", &august)
            .expect("one register per month is the shape this lane is built on");
    }

    #[test]
    fn revise_is_never_refused_by_the_near_duplicate_check() {
        let mut store = EventStore::in_memory().unwrap();
        let a = sample_with("item-a", "the config lives in config/app.toml");
        declare(&mut store, "s1", "l1", "test", &a).unwrap();

        let b = sample_with("item-b", "never force-push to the main branch");
        declare(&mut store, "s1", "l1", "test", &b).unwrap();

        // Revise b so its text becomes IDENTICAL to a's live text. If revise
        // ran through the same check declare does, this would be refused as
        // a near-duplicate of a - it must not be: a revise corrects an item
        // that is already allowed to exist, and is never blocked by this
        // check.
        let mut updated_b = b.clone();
        updated_b.text = a.text.clone();
        let result = revise(&mut store, "s1", "l1", "test", &b, &updated_b);
        assert!(result.is_ok(), "a revise must never be refused by the near-duplicate check, got {result:?}");

        let back = show(&store, &b.id).unwrap();
        assert_eq!(back.text, a.text, "the revise must have gone through with the now-duplicate text");
    }

    /// THE DEFECT THIS CLOSES: `find_near_duplicate` now reads the
    /// `head_state` projection (`live_items_from_projection`) instead of
    /// folding the whole log (`live_items_from_fold`) on every `declare`
    /// call. This pins that the fast path names exactly the same live
    /// candidates the fold would, across the cases that decide liveness: a
    /// plain live item, one whose head moved under a revise, a tombstoned
    /// one (must be absent from both), and a diverged one (must be absent
    /// from both) - so swapping the read strategy could never quietly
    /// change which items a new declaration is compared against.
    #[test]
    fn near_duplicate_projection_path_matches_the_fold_over_a_mixed_store() {
        let mut store = EventStore::in_memory().unwrap();

        // p1: a plain live item.
        let p1 = sample_with("p1", "back up the database nightly");
        declare(&mut store, "s1", "l1", "test", &p1).unwrap();

        // p2: revised - its head must have moved off the original create.
        // revise is never subject to the near-duplicate gate, so the
        // revised text is free to be anything.
        let original = sample_with("p2", "rotate the api keys quarterly");
        declare(&mut store, "s1", "l1", "test", &original).unwrap();
        let mut updated = original.clone();
        updated.text = "do the other thing entirely".to_string();
        revise(&mut store, "s1", "l1", "test", &original, &updated).unwrap();

        // p3: retracted - a tombstone, must be absent from both paths.
        let p3 = sample_with("p3", "run linting before every commit");
        declare(&mut store, "s1", "l1", "test", &p3).unwrap();
        retract(&mut store, "s1", "l1", "test", &p3.id, "app.toml was superseded by the new layout").unwrap();

        // p4: diverged (hand-crafted, same shape as
        // `serve::live`'s own equivalence test) - must be absent too.
        store.append_event("s1", "l1", "test", EventKind::FactCreated, "p4", None, "not json").unwrap();
        store
            .append_event("s1", "l1", "test", EventKind::FactRevised, "p4", Some("stale"), "also not json")
            .unwrap();

        assert!(store.heads_projection_current(), "no bypass writes here: projection must be current");

        let fast = live_items_from_projection(&store).unwrap();
        let slow = live_items_from_fold(&store).unwrap();

        let fast_ids: Vec<&str> = fast.iter().map(|(id, _)| id.as_str()).collect();
        let slow_ids: Vec<&str> = slow.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            fast_ids, slow_ids,
            "the projection path must name exactly the same live items as the fold, in the same order"
        );
        assert_eq!(fast_ids, vec!["p1", "p2"], "p3 (retracted) and p4 (diverged) must both be absent");

        let fast_texts: Vec<&str> = fast.iter().map(|(_, item)| item.text.as_str()).collect();
        let slow_texts: Vec<&str> = slow.iter().map(|(_, item)| item.text.as_str()).collect();
        assert_eq!(fast_texts, slow_texts, "both paths must carry the identical body for each id");
        assert_eq!(fast[1].1.text, "do the other thing entirely", "p2 must carry the REVISED body, not the original");
    }

    // ---------------------------------------------- setup note retract ground

    fn owner_setup_answers() -> Item {
        Item {
            id: OWNER_SETUP_ANSWERS_ID.to_string(),
            kind: Kind::Report,
            text: "reply length: short. language: Dutch. lanes: work only. rules: kept as seeded. \
                   project scoping: yes, per checkout."
                .to_string(),
            bindings: vec![], // ground 6: archive material may carry none
            severity: None,
            project: Some("test-project".to_string()), // ground 21: archive material needs a scope
            tags: vec![],
            expires: None,
            key: None,
            falsifier: None, // ground 10 does not reach a Report
            check: None,
        }
    }

    fn setup_note() -> Item {
        Item {
            id: SETUP_NOTE_ID.to_string(),
            kind: Kind::Rule,
            text: "walk the owner through setup once, then retract this note".to_string(),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec!["working-contract".to_string()],
            expires: None,
            key: None,
            falsifier: Some("this note is still served after the owner already answered".to_string()),
            check: None,
        }
    }

    /// THE DEFECT THIS CLOSES: retracting the seeded note used to cost
    /// nothing but the words in the reason field - see `gate::retract`'s own
    /// doc comment. Nothing is written when the gate refuses it.
    #[test]
    fn retracting_the_setup_note_with_no_answers_on_record_is_refused() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &setup_note()).unwrap();

        let err = retract(&mut store, "s", "l", "a", SETUP_NOTE_ID, "done").unwrap_err();
        assert!(matches!(err, WriteError::Refused(_)), "{err}");
        assert!(show(&store, SETUP_NOTE_ID).is_ok(), "a refused retract must leave the note live");
    }

    /// Storing the answers first is exactly what the refusal above asks for
    /// - once they exist as a live item, the same retract goes through.
    #[test]
    fn retracting_the_setup_note_once_answers_are_on_record_succeeds() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &setup_note()).unwrap();
        declare(&mut store, "s", "l", "a", &owner_setup_answers()).unwrap();

        retract(&mut store, "s", "l", "a", SETUP_NOTE_ID, "owner walked through setup").unwrap();
        assert!(matches!(show(&store, SETUP_NOTE_ID).unwrap_err(), ReadError::Retracted(_)));
    }

    /// A reluctant owner is not trapped: recording "no" is still recording an
    /// answer, and this is the same code path either way - `retract` never
    /// inspects the CONTENT of `OWNER_SETUP_ANSWERS_ID`, only that it exists.
    #[test]
    fn a_flat_no_recorded_as_the_answer_still_clears_the_note() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &setup_note()).unwrap();
        let mut declines = owner_setup_answers();
        declines.text = "he does not want any of this - no rules, no lanes, nothing kept".to_string();
        declare(&mut store, "s", "l", "a", &declines).unwrap();

        retract(&mut store, "s", "l", "a", SETUP_NOTE_ID, "owner declined setup").unwrap();
    }

    /// THE NARROW SCOPE THIS GROUND MUST KEEP: an ordinary retract, on an
    /// ordinary id, must never even notice this ground exists.
    #[test]
    fn retracting_an_unrelated_id_never_needs_the_answers_item() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s", "l", "a", &item).unwrap();

        retract(&mut store, "s", "l", "a", &item.id, "config/app.toml is no longer true").unwrap();
    }

    /// A store that never had the seeded note at all (an old install, or one
    /// that predates this feature) must retract normally - `show` on an
    /// unknown id already refuses with `NotFound` before this ground is ever
    /// reached, so there is nothing new here to trip over.
    #[test]
    fn a_store_with_no_seeded_note_is_unaffected() {
        let mut store = EventStore::in_memory().unwrap();
        let err = retract(&mut store, "s", "l", "a", SETUP_NOTE_ID, "cleanup").unwrap_err();
        assert!(matches!(err, WriteError::Store(_)), "an id that was never declared is NotFound, not Refused: {err}");
    }

    // --------------------------------------- the evaluation-filing gate (2026-09-25)

    /// A heavy, command-bound, checkless Rule - carrying the pre-existing
    /// ground 11 (continued, command) answer by default (`no-catch-on-
    /// command:<why>`), the ONLY tag that ground accepts for a heavy item
    /// anchored to a command with no check. Unrelated to, and never
    /// satisfying, the evaluation-filing gate's own condition 2 below: that
    /// gate only ever reads a `TEETH_TAG_PREFIX` tag, a completely different
    /// vocabulary - see `evaluation_report_filing_is_refused_when_a_heavy_
    /// rule_fired_since_the_previous_report_carries_no_teeth_box`, which
    /// proves exactly that this tag alone does not satisfy it.
    fn heavy_command_rule(id: &str, project: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("fixture heavy rule {id}"),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-command-{id}") }],
            severity: Some(Severity::Irreversible),
            project: Some(project.to_string()),
            tags: vec![format!("{}a fixture where 'the same command' is also the legitimate one", NO_CATCH_ON_COMMAND_REASON_PREFIX)],
            expires: None,
            key: None,
            falsifier: Some("this turns out to be safe after all".to_string()),
            check: None,
        }
    }

    /// A LIGHT (no severity), command-bound, checkless Rule - judgement
    /// debt (`judgement_debt_in_project`) never looks at severity at all,
    /// so this is enough to owe a verdict on its own, and being light means
    /// it owes no teeth box (`TeethBox::NotApplicable`) and so can never
    /// ALSO trip the evaluation-filing gate's condition 2 - the isolation
    /// `evaluation_report_filing_is_refused_while_judgement_debt_is_
    /// outstanding`/`..._succeeds_once_judgement_debt_is_paid_down` need to
    /// each prove exactly one condition.
    fn light_command_rule(id: &str, project: &str) -> Item {
        let mut item = heavy_command_rule(id, project);
        item.severity = None;
        item.tags = vec![];
        item
    }

    fn eval_report(id: &str, project: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Report,
            text: format!("fixture evaluation report {id}"),
            bindings: vec![],
            severity: None,
            project: Some(project.to_string()),
            tags: vec![EVALUATION_REPORT_TAG.to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        }
    }

    fn open_point(id: &str, project: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Report,
            text: format!("fixture open point sentence {id}"),
            bindings: vec![],
            severity: None,
            project: Some(project.to_string()),
            tags: vec![OPEN_POINT_TAG.to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        }
    }

    fn serve_n(store: &mut EventStore, id: &str, actor: &str, n: usize) {
        for _ in 0..n {
            store.append_event("s", "l", actor, EventKind::ItemServed, id, None, "{}").unwrap();
        }
    }

    fn mark_useful(store: &mut EventStore, id: &str) {
        store.append_event("s", "l", "a", EventKind::ItemMarkedUseful, id, None, "{}").unwrap();
    }

    /// Firings under a named session, the way the hook stamps them.
    fn serve_in(store: &mut EventStore, session: &str, id: &str, n: usize) {
        for _ in 0..n {
            store.append_event(session, session, ASSISTANT_DELIVERY_ACTOR, EventKind::ItemServed, id, None, "{}").unwrap();
        }
    }

    /// An evaluation by `session`, started right now (at the log's tip).
    fn started_now(store: &EventStore, session: &str) -> EvaluationScope {
        EvaluationScope { session_id: session.to_string(), started_seq: store.get_next_seq().unwrap() - 1 }
    }

    fn file(store: &mut EventStore, report: &Item, scope: &EvaluationScope) -> Result<Event, WriteError> {
        declare_scoped(store, "s", "l", "a", report, Some(scope))
    }

    #[test]
    fn a_write_that_is_not_an_evaluation_report_is_never_touched_by_this_gate() {
        let mut store = EventStore::in_memory().unwrap();
        // Real, outstanding judgement debt for "p" - would refuse an
        // evaluation report, must never so much as be consulted for an
        // ordinary Rule or for a Report that carries no evaluation-report
        // tag at all.
        let heavy = heavy_command_rule("heavy-1", "p");
        declare(&mut store, "s", "l", "a", &heavy).unwrap();
        serve_n(&mut store, "heavy-1", ASSISTANT_DELIVERY_ACTOR, JUDGEMENT_DEBT_AFTER);
        declare(&mut store, "s", "l", "a", &heavy_command_rule("heavy-2", "p")).expect("an ordinary Rule must never consult this gate");

        let mut untagged_report = eval_report("plain-report-not-tagged", "p");
        untagged_report.tags = vec![];
        declare(&mut store, "s", "l", "a", &untagged_report)
            .expect("a Report with no evaluation-report tag must never be touched by this gate");
    }

    /// THE OTHER DOOR. `declare` is not the only way an item could end up
    /// carrying `EVALUATION_REPORT_TAG` - a `revise` that adds the tag to
    /// an item that did not have it is filing exactly as much as a fresh
    /// `declare` is, and must be held to the identical conditions.
    #[test]
    fn revising_an_ordinary_report_onto_the_evaluation_report_tag_is_also_gated() {
        let mut store = EventStore::in_memory().unwrap();
        // The teeth condition, not the judgement one: this door carries no
        // evaluation scope, so only the conditions every filing is held to
        // can refuse here (see `EvaluationScope`).
        legacy_thinly_answered(&mut store, "unboxed-3", Some("z"));
        serve_n(&mut store, "unboxed-3", "hook", 1);

        let mut plain = eval_report("not-yet-an-eval-report", "z");
        plain.tags = vec!["some-other-tag".to_string()];
        declare(&mut store, "s", "l", "a", &plain).expect("an ordinary, untagged Report may be declared freely");

        let mut now_tagged = plain.clone();
        now_tagged.tags.push(EVALUATION_REPORT_TAG.to_string());
        let err = revise(&mut store, "s", "l", "a", &plain, &now_tagged).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("neither a real tooth nor a real reason"), "{}", refusal.problem);
    }

    /// THE CONCESSION THIS MUST NOT COST: a report that has ALREADY been
    /// filed (it already carries the tag) must stay freely correctable
    /// afterwards - conditions that were only ever about the FILING moment
    /// must never re-hold a plain typo fix hostage, the same distinction
    /// `declare` draws between "may this be created" and "the finished
    /// item's own later life".
    #[test]
    fn revising_an_already_filed_report_that_keeps_the_tag_is_never_gated() {
        let mut store = EventStore::in_memory().unwrap();
        let light = light_command_rule("owed-4", "y");
        declare(&mut store, "s", "l", "a", &light).unwrap();
        serve_n(&mut store, "owed-4", ASSISTANT_DELIVERY_ACTOR, JUDGEMENT_DEBT_AFTER);

        // A filed report exists (bypassing declare's own gate on purpose -
        // this fixture is about proving `revise`'s own SEPARATE gate stays
        // out of the way once the tag is already there, not about how it
        // got there).
        let filed = eval_report("already-filed", "y");
        let body = serde_json::to_string(&filed).unwrap();
        store.append_event("legacy", "already-filed", "migration", EventKind::FactCreated, "already-filed", None, &body).unwrap();

        let mut typo_fixed = filed.clone();
        typo_fixed.text = format!("{} (typo fixed)", filed.text);
        revise(&mut store, "s", "l", "a", &filed, &typo_fixed)
            .expect("a report that already carried the tag must stay correctable regardless of live debt");
    }

    #[test]
    fn evaluation_report_filing_is_refused_while_judgement_debt_is_outstanding() {
        let mut store = EventStore::in_memory().unwrap();
        let light = light_command_rule("owed-1", "p");
        declare(&mut store, "s", "l", "a", &light).unwrap();
        serve_n(&mut store, "owed-1", ASSISTANT_DELIVERY_ACTOR, JUDGEMENT_DEBT_AFTER);

        let scope = started_now(&store, "s");
        let err = file(&mut store, &eval_report("eval-p-1", "p"), &scope).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("owe a judgement"), "{}", refusal.problem);
        assert!(show(&store, "eval-p-1").is_err(), "a refused report must not be live");
    }

    #[test]
    fn evaluation_report_filing_succeeds_once_judgement_debt_is_paid_down() {
        let mut store = EventStore::in_memory().unwrap();
        let light = light_command_rule("owed-2", "p");
        declare(&mut store, "s", "l", "a", &light).unwrap();
        serve_n(&mut store, "owed-2", ASSISTANT_DELIVERY_ACTOR, JUDGEMENT_DEBT_AFTER);
        mark_useful(&mut store, "owed-2");

        let scope = started_now(&store, "s");
        file(&mut store, &eval_report("eval-p-2", "p"), &scope).expect("debt is settled, filing must succeed");
    }

    /// A heavy rule that predates the content rule for reasons: its answer
    /// fits any rule at all. Written straight into the log, the way such a
    /// rule really sits in an older store - the write gate would refuse it
    /// fresh today.
    fn legacy_thinly_answered(store: &mut EventStore, id: &str, project: Option<&str>) {
        let mut item = heavy_command_rule(id, project.unwrap_or("unused"));
        item.project = project.map(str::to_string);
        item.tags = vec![format!("{}a fixture reason that fits any rule at all", NO_CATCH_ON_COMMAND_REASON_PREFIX)];
        let body = serde_json::to_string(&item).unwrap();
        store.append_event("legacy", id, "migration", EventKind::FactCreated, id, None, &body).unwrap();
    }

    #[test]
    fn evaluation_report_filing_is_refused_when_a_fired_heavy_rule_has_neither_a_real_tooth_nor_a_real_reason() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-q-old", "q")).unwrap();
        legacy_thinly_answered(&mut store, "unboxed-heavy", Some("q"));
        serve_n(&mut store, "unboxed-heavy", "hook", 1);

        let err = declare(&mut store, "s", "l", "a", &eval_report("eval-q-new", "q")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("unboxed-heavy"), "{}", refusal.problem);
        assert!(refusal.problem.contains("neither a real tooth nor a real reason"), "{}", refusal.problem);
    }

    /// C (2026-09-26): the refusal used to point at a maintenance pass that
    /// labels in bulk as the way out. It must send a fresh agent to the real
    /// work instead - a check that bites, or a reason that quotes the
    /// legitimate call or names the trigger - and never to relabelling.
    #[test]
    fn the_filing_refusal_names_the_real_work_and_never_the_bulk_relabel() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-c-old", "c")).unwrap();
        legacy_thinly_answered(&mut store, "needs-work", Some("c"));
        serve_n(&mut store, "needs-work", "hook", 1);
        let err = declare(&mut store, "s", "l", "a", &eval_report("eval-c-new", "c")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        let text = format!("{} {}", refusal.problem, refusal.fix);
        assert!(!text.contains("backfill_teeth"), "a bulk relabel is never the way out: {text}");
        assert!(text.contains("QUOTES the legitimate call"), "{text}");
        assert!(text.contains("open point naming the rule's id"), "{text}");
    }

    /// C (2026-09-26): the judgement half names the owed items, so a writer
    /// does not have to go and find them - and says what a verdict must rest
    /// on.
    #[test]
    fn the_filing_refusal_names_every_item_that_owes_a_judgement() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &light_command_rule("owed-a", "o")).unwrap();
        declare(&mut store, "s", "l", "a", &light_command_rule("owed-b", "o")).unwrap();
        serve_n(&mut store, "owed-a", ASSISTANT_DELIVERY_ACTOR, JUDGEMENT_DEBT_AFTER);
        serve_n(&mut store, "owed-b", ASSISTANT_DELIVERY_ACTOR, JUDGEMENT_DEBT_AFTER);
        let scope = started_now(&store, "s");
        let err = file(&mut store, &eval_report("eval-o-1", "o"), &scope).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("'owed-a'") && refusal.problem.contains("'owed-b'"), "{}", refusal.problem);
        assert!(refusal.problem.contains("not-judgeable:<id>:<reason>"), "the refusal names the honest skip: {}", refusal.problem);
    }

    // ------------------------------------------ the scoped, frozen judgement
    //
    // MEASURED 2026-09-26 in a fresh evaluation: eight filing attempts, eight
    // refusals, on a list that kept growing (14, 6, 13, 4, 4, 1, 5, 4) - the
    // evaluation's own writes fired the remember-bound rules, and parallel
    // sessions fired items this session never saw. It ended up stamping sixty
    // items useful. These pin the fix (see `EvaluationScope`).

    /// Items that fired only in another session stay owed store-wide, but
    /// never hold this session's report.
    #[test]
    fn an_item_that_fired_only_in_another_session_never_holds_this_report() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &light_command_rule("elsewhere", "w")).unwrap();
        serve_in(&mut store, "other-session", "elsewhere", JUDGEMENT_DEBT_AFTER);
        let scope = started_now(&store, "this-session");

        file(&mut store, &eval_report("eval-w-1", "w"), &scope).expect("another session's firings are not this report's to settle");
        assert_eq!(
            judgement_debt_in_project(&store, Some("w")),
            vec!["elsewhere".to_string()],
            "it stays owed store-wide, where doctor keeps naming it"
        );
    }

    /// The list is fixed when the evaluation starts: an item pushed over the
    /// threshold afterwards - by the evaluation's own writes firing the
    /// remember-bound rules, or by anything else - never joins it.
    #[test]
    fn nothing_that_fires_after_the_evaluation_started_can_add_to_its_list() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &light_command_rule("owed-before", "f")).unwrap();
        declare(&mut store, "s", "l", "a", &light_command_rule("remember-bound", "f")).unwrap();
        serve_in(&mut store, "eval-session", "owed-before", JUDGEMENT_DEBT_AFTER);
        serve_in(&mut store, "eval-session", "remember-bound", JUDGEMENT_DEBT_AFTER - 1);
        let scope = started_now(&store, "eval-session");
        // The evaluation's own writes fire it again, well past the threshold.
        serve_in(&mut store, "eval-session", "remember-bound", 10);

        let err = file(&mut store, &eval_report("eval-f-1", "f"), &scope).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("'owed-before'"), "{}", refusal.problem);
        assert!(!refusal.problem.contains("remember-bound"), "the list cannot grow: {}", refusal.problem);

        mark_useful(&mut store, "owed-before");
        file(&mut store, &eval_report("eval-f-1", "f"), &scope).expect("the fixed list is settled, so the report files");
    }

    /// "When in doubt, skip" and "the list must be settled" no longer
    /// collide: an explicit skip with a reason settles an item for this
    /// filing, and the item stays owed store-wide. A skip without a real
    /// reason settles nothing.
    #[test]
    fn a_skip_with_a_reason_settles_an_item_for_this_filing_only() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &light_command_rule("unsure", "n")).unwrap();
        serve_in(&mut store, "eval-session", "unsure", JUDGEMENT_DEBT_AFTER);
        let scope = started_now(&store, "eval-session");

        let mut thin = eval_report("eval-n-1", "n");
        thin.tags.push(format!("{NOT_JUDGEABLE_TAG_PREFIX}unsure:no"));
        assert!(file(&mut store, &thin, &scope).is_err(), "a skip with no real reason settles nothing");

        let mut honest = eval_report("eval-n-1", "n");
        honest.tags.push(format!("{NOT_JUDGEABLE_TAG_PREFIX}unsure:it fired on a grep of a log I never opened"));
        file(&mut store, &honest, &scope).expect("an honest skip settles it for this filing");
        assert!(judgement_debt_in_project(&store, Some("n")).contains(&"unsure".to_string()), "and it stays owed store-wide");
    }

    /// No hook record of the filing session: no verdict can honestly be
    /// demanded (see `EvaluationScope`), so none is.
    #[test]
    fn a_filing_with_no_known_session_demands_no_verdict() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &light_command_rule("owed-x", "x")).unwrap();
        serve_n(&mut store, "owed-x", ASSISTANT_DELIVERY_ACTOR, JUDGEMENT_DEBT_AFTER);
        declare(&mut store, "s", "l", "a", &eval_report("eval-x-1", "x")).expect("no session known, no verdict demanded");
    }

    /// The smaller half of 2026-09-26: a reason that passes both the shape
    /// and the content rule still settles nothing on a rule whose history
    /// never carried a check. Once the log shows a check was tried on it,
    /// the same reason counts.
    #[test]
    fn a_no_literal_reason_settles_a_heavy_rule_only_after_a_check_was_tried() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-t-old", "t")).unwrap();
        let mut rule = heavy_command_rule("never-tried", "t");
        rule.tags = vec![format!("{NO_CATCH_ON_COMMAND_REASON_PREFIX}the same text is in 'fixture-command-never-tried --help' which is legitimate")];
        let body = serde_json::to_string(&rule).unwrap();
        store.append_event("legacy", "never-tried", "migration", EventKind::FactCreated, "never-tried", None, &body).unwrap();
        serve_n(&mut store, "never-tried", "hook", 1);

        let err = declare(&mut store, "s", "l", "a", &eval_report("eval-t-new", "t")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("'never-tried'"), "untried, so the reason is no answer yet: {}", refusal.problem);

        // The history now shows a check was attached once, and later
        // removed with the same reason: the try is on record.
        try_a_check_then_give_it_up(&mut store, &rule);
        assert!(ids_whose_check_was_tried(&store).contains("never-tried"));
        assert!(show(&store, "never-tried").unwrap().check.is_none(), "the rule itself carries no check now");
        declare(&mut store, "s", "l", "a", &eval_report("eval-t-new", "t")).expect("tried, and the reason quotes the legitimate call");
    }

    /// THE SCOPE THIS CONDITION ACTUALLY HAS, stated where it is enforced. An
    /// `ItemServed` event records when an item fired and on what trigger,
    /// never in which project, so "fired in this project" is not something
    /// the log can answer. What the gate counts is every heavy rule that
    /// APPLIES to the project (its own, and global ones - the same
    /// `applies_to` rule judgement debt uses) and has fired since the
    /// previous report. A global rule is therefore named here too, and the
    /// refusal must say that scope instead of claiming the rule fired here.
    #[test]
    fn a_fired_global_heavy_rule_blocks_filing_and_the_refusal_names_the_scope_it_really_counts() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-g-old", "g")).unwrap();
        legacy_thinly_answered(&mut store, "global-heavy", None);
        serve_n(&mut store, "global-heavy", "hook", 1);

        let err = declare(&mut store, "s", "l", "a", &eval_report("eval-g-new", "g")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("global-heavy"), "a global rule applies here too: {}", refusal.problem);
        assert!(
            refusal.problem.contains("apply to this project (its own or global)"),
            "the refusal must name the scope it really counts: {}",
            refusal.problem
        );
        assert!(
            !refusal.problem.contains("fired in this project"),
            "the log cannot tell where a rule fired, so the refusal must not claim it: {}",
            refusal.problem
        );
    }

    /// REPLACES `evaluation_report_filing_succeeds_once_the_fired_heavy_rule_
    /// carries_any_teeth_box` (2026-09-26): that test pinned the defect. Any
    /// machine label settled the filing, so a pass that labelled every heavy
    /// rule "informing" lifted the gate for a whole store at once. "Informing"
    /// only says nothing could be tried - it is the question, not an answer.
    #[test]
    fn an_informing_label_alone_never_settles_a_fired_heavy_rule() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-r-old", "r")).unwrap();
        legacy_thinly_answered(&mut store, "boxed-heavy", Some("r"));
        let existing = show(&store, "boxed-heavy").unwrap();
        let mut labelled = existing.clone();
        labelled.tags.push(TEETH_INFORMING_TAG.to_string());
        revise(&mut store, "s", "l", "a", &existing, &labelled).unwrap();
        serve_n(&mut store, "boxed-heavy", "hook", 1);

        let err = declare(&mut store, "s", "l", "a", &eval_report("eval-r-new", "r")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("boxed-heavy"), "{}", refusal.problem);
    }

    // ------------------------------------ the batch one filing is held to

    /// Settles a heavy fixture rule the honest way: a reason that names its
    /// own command.
    /// The honest route to a no-literal answer since 2026-09-26: a check is
    /// attached first (a real try, on the log), and only then given up for
    /// the reason `rule` carries. Written through the checked revise path.
    fn try_a_check_then_give_it_up(store: &mut EventStore, rule: &Item) {
        let mut with_check = rule.clone();
        with_check.check = Some(Check::Forbidden { literals: vec![format!("{} --force", rule.id)] });
        for body in [serde_json::to_string(&with_check).unwrap(), serde_json::to_string(rule).unwrap()] {
            store.append_mutate_checked("legacy", &rule.id, "migration", EventKind::FactRevised, &rule.id, None, &body).unwrap();
        }
    }

    fn settle_by_naming_the_trigger(store: &mut EventStore, id: &str) {
        let existing = show(store, id).unwrap();
        let mut settled = existing.clone();
        settled.tags = vec![format!(
            "{}an authorised fixture-command-{id} run is typed exactly like a wrong one",
            NO_CATCH_ON_COMMAND_REASON_PREFIX
        )];
        try_a_check_then_give_it_up(store, &settled);
    }

    fn filing_refusal(store: &mut EventStore, report: &str, project: &str) -> Option<String> {
        match declare(store, "s", "l", "a", &eval_report(report, project)) {
            Ok(_) => None,
            Err(WriteError::Refused(refusal)) => Some(refusal.problem),
            Err(other) => panic!("expected a refusal or a filing: {other}"),
        }
    }

    /// `n` heavy rules without a tooth or a real reason, written in order
    /// (`w-00` first), each fired once.
    fn unsettled_fired_rules(store: &mut EventStore, project: &str, n: usize) -> Vec<String> {
        let ids: Vec<String> = (0..n).map(|i| format!("w-{i:02}")).collect();
        for id in &ids {
            legacy_thinly_answered(store, id, Some(project));
        }
        for id in &ids {
            serve_n(store, id, "hook", 1);
        }
        ids
    }

    /// THE WALL THIS REMOVES: with twelve heavy rules owed since the previous
    /// report, one filing is held to a batch - the eight written longest ago -
    /// and files once those eight are settled, while the other four stay owed.
    #[test]
    fn a_filing_is_held_to_one_batch_and_the_rest_stays_owed() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-w-0", "w")).unwrap();
        let ids = unsettled_fired_rules(&mut store, "w", 12);

        let (batch, rest) = ids.split_at(EVALUATION_TEETH_BATCH.min(ids.len()));
        let problem = filing_refusal(&mut store, "eval-w-1", "w").expect("the batch is owed");
        for id in batch {
            assert!(problem.contains(&format!("'{id}'")), "the batch must name {id}: {problem}");
        }
        for id in rest {
            assert!(!problem.contains(&format!("'{id}'")), "{id} is beyond the batch: {problem}");
        }
        assert!(problem.contains("4 more heavy rule(s)"), "the rest must be counted, not hidden: {problem}");

        for id in batch {
            settle_by_naming_the_trigger(&mut store, id);
        }
        assert_eq!(filing_refusal(&mut store, "eval-w-1", "w"), None, "a settled batch must let the report through");
        let owed: Vec<String> = evaluation_teeth_batch(&store, Some("w")).owed_now.into_iter().map(|(id, _)| id).collect();
        assert!(owed.is_empty(), "nothing has fired since the new report: {owed:?}");
    }

    /// Settling one rule of the batch never pulls the next one in: the batch
    /// holds still while an evaluation works through it.
    #[test]
    fn settling_a_batch_member_does_not_pull_the_next_rule_in() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-s-0", "s")).unwrap();
        let ids = unsettled_fired_rules(&mut store, "s", 10);
        settle_by_naming_the_trigger(&mut store, &ids[0]);

        let batch = evaluation_teeth_batch(&store, Some("s"));
        assert_eq!(batch.settled, vec![ids[0].clone()]);
        let owed: Vec<&str> = batch.owed_now.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(owed, ids[1..EVALUATION_TEETH_BATCH].iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(batch.owed_later.len(), 2);
    }

    /// The next filing asks for what the last one left, first.
    #[test]
    fn the_next_filing_asks_first_for_what_the_last_one_left() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-r0", "rot")).unwrap();
        let ids = unsettled_fired_rules(&mut store, "rot", 11);
        for id in &ids[..EVALUATION_TEETH_BATCH] {
            settle_by_naming_the_trigger(&mut store, id);
        }
        declare(&mut store, "s", "l", "a", &eval_report("eval-r1", "rot")).unwrap();
        // Every rule fires again, and one more heavy rule is written after the
        // report: the three left behind lead, and the new one waits behind.
        legacy_thinly_answered(&mut store, "z-newer", Some("rot"));
        for id in ids.iter().map(String::as_str).chain(["z-newer"]) {
            serve_n(&mut store, id, "hook", 1);
        }
        let batch = evaluation_teeth_batch(&store, Some("rot"));
        let owed: Vec<&str> = batch.owed_now.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(owed, vec!["w-08", "w-09", "w-10", "z-newer"], "left-behind rules first, then the newer one");
        assert!(batch.settled.is_empty(), "rules settled before the previous report take no place: {:?}", batch.settled);
    }

    /// Rules settled long ago that keep firing never fill the batch, or the
    /// backlog behind them would never be asked.
    #[test]
    fn rules_settled_before_the_previous_report_take_no_place_in_the_batch() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..EVALUATION_TEETH_BATCH {
            let mut settled = heavy_command_rule(&format!("old-{i}"), "k");
            settled.tags = vec![format!(
                "{}an authorised fixture-command-old-{i} run is typed exactly like a wrong one",
                NO_CATCH_ON_COMMAND_REASON_PREFIX
            )];
            declare(&mut store, "s", "l", "a", &settled).unwrap();
            try_a_check_then_give_it_up(&mut store, &settled);
        }
        declare(&mut store, "s", "l", "a", &eval_report("eval-k-0", "k")).unwrap();
        let ids = unsettled_fired_rules(&mut store, "k", 3);
        for i in 0..EVALUATION_TEETH_BATCH {
            serve_n(&mut store, &format!("old-{i}"), "hook", 3);
        }
        let batch = evaluation_teeth_batch(&store, Some("k"));
        let owed: Vec<String> = batch.owed_now.into_iter().map(|(id, _)| id).collect();
        assert_eq!(owed, ids);
        assert!(batch.settled.is_empty(), "{:?}", batch.settled);
    }

    /// A reason a reader can check against the rule - it names the rule's own
    /// command - settles it: that is the clear proof it cannot have a tooth.
    #[test]
    fn a_reason_that_names_the_trigger_settles_a_fired_heavy_rule() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-n-old", "n")).unwrap();
        let mut heavy = heavy_command_rule("named-heavy", "n");
        heavy.tags = vec![format!(
            "{}an authorised fixture-command-named-heavy run is typed exactly like a wrong one",
            NO_CATCH_ON_COMMAND_REASON_PREFIX
        )];
        declare(&mut store, "s", "l", "a", &heavy).unwrap();
        serve_n(&mut store, "named-heavy", "hook", 1);
        assert!(
            declare(&mut store, "s", "l", "a", &eval_report("eval-n-new", "n")).is_err(),
            "the reason alone, with no check ever tried, is not the answer yet"
        );
        try_a_check_then_give_it_up(&mut store, &heavy);
        declare(&mut store, "s", "l", "a", &eval_report("eval-n-new", "n")).expect("a real reason after a real try settles it");
    }

    /// A check that is wired but has not met a real call yet settles the
    /// filing only when an open point carries it by name - the honest "still
    /// to be proven" the next evaluation has to close. A proven label settles
    /// it on its own.
    #[test]
    fn a_wired_check_settles_only_when_an_open_point_names_it_and_a_proven_one_settles_alone() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-w9-old", "w9")).unwrap();
        let mut wired = heavy_command_rule("wired-heavy", "w9");
        wired.tags = vec![TEETH_WIRED_TAG.to_string()];
        wired.check = Some(Check::Forbidden { literals: vec!["--force".to_string()] });
        declare(&mut store, "s", "l", "a", &wired).unwrap();
        let mut proven = heavy_command_rule("proven-heavy", "w9");
        proven.tags = vec![TEETH_PROVEN_TAG.to_string()];
        proven.check = Some(Check::Forbidden { literals: vec!["--mirror".to_string()] });
        declare(&mut store, "s", "l", "a", &proven).unwrap();
        serve_n(&mut store, "wired-heavy", "hook", 1);
        serve_n(&mut store, "proven-heavy", "hook", 1);

        let err = declare(&mut store, "s", "l", "a", &eval_report("eval-w9-new", "w9")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("wired-heavy") && !refusal.problem.contains("proven-heavy"), "{}", refusal.problem);

        let mut point = open_point("open-wired", "w9");
        point.text = "wired-heavy still has to refuse a real --force push".to_string();
        declare(&mut store, "s", "l", "a", &point).unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-w9-new", "w9")).expect("the open point carries it");
    }

    #[test]
    fn a_heavy_rule_that_never_fired_since_the_previous_report_is_never_named() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-t-old", "t")).unwrap();
        // Declared AFTER the previous report but never actually served -
        // must not be treated as owing a fresh box.
        declare(&mut store, "s", "l", "a", &heavy_command_rule("never-fired", "t")).unwrap();

        declare(&mut store, "s", "l", "a", &eval_report("eval-t-new", "t"))
            .expect("an unfired heavy rule must never hold up filing");
    }

    #[test]
    fn evaluation_report_filing_is_refused_by_an_open_point_older_than_the_previous_report() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &open_point("open-u-1", "u")).unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-u-1", "u")).expect("a brand-new open point must never block its own contemporaneous report");

        let err = declare(&mut store, "s", "l", "a", &eval_report("eval-u-2", "u")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("open-u-1"), "{}", refusal.problem);
        assert!(refusal.problem.contains("neither closed nor carried"), "{}", refusal.problem);
    }

    #[test]
    fn closing_the_open_point_with_a_retraction_lets_filing_through() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &open_point("open-v-1", "v")).unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-v-1", "v")).unwrap();
        retract(&mut store, "s", "l", "a", "open-v-1", "resolved during the next evaluation").unwrap();

        declare(&mut store, "s", "l", "a", &eval_report("eval-v-2", "v")).expect("a closed open point must never block filing");
    }

    #[test]
    fn carrying_the_open_point_forward_once_lets_filing_through() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &open_point("open-w-1", "w")).unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-w-1", "w")).unwrap();
        let existing = show(&store, "open-w-1").unwrap();
        let mut carried = existing.clone();
        carried.tags.push(format!("{OPEN_POINT_CARRIED_PREFIX}1"));
        revise(&mut store, "s", "l", "a", &existing, &carried).unwrap();

        declare(&mut store, "s", "l", "a", &eval_report("eval-w-2", "w"))
            .expect("an open point carried forward exactly once must not block filing");
    }

    #[test]
    fn open_point_carried_count_reads_the_tag_and_defaults_to_zero() {
        assert_eq!(open_point_carried_count(&[]), 0);
        assert_eq!(open_point_carried_count(&["open-point".to_string()]), 0);
        assert_eq!(open_point_carried_count(&["carried:1".to_string()]), 1);
        assert_eq!(open_point_carried_count(&["carried:2".to_string()]), 2);
        assert_eq!(open_point_carried_count(&["carried:not-a-number".to_string()]), 0);
    }

    #[test]
    fn open_points_lists_only_the_named_projects_own_points_oldest_first() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &open_point("open-x-1", "x")).unwrap();
        declare(&mut store, "s", "l", "a", &open_point("open-x-2", "x")).unwrap();
        declare(&mut store, "s", "l", "a", &open_point("open-y-1", "y")).unwrap();

        let points = open_points(&store, Some("x"));
        assert_eq!(points.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["open-x-1", "open-x-2"]);
        assert!(points.iter().all(|p| p.carried == 0));
    }

    /// G (2026-09-26): a carry covers ONE filing. Once a report was filed
    /// after the carry, the point blocks again - it used to stop blocking for
    /// good the moment its tag read `carried:1`.
    #[test]
    fn a_carried_open_point_blocks_again_once_a_report_was_filed_after_the_carry() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &open_point("open-g-1", "gg")).unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-gg-1", "gg")).unwrap();
        let existing = show(&store, "open-g-1").unwrap();
        let mut carried = existing.clone();
        carried.tags.push(format!("{OPEN_POINT_CARRIED_PREFIX}1"));
        revise(&mut store, "s", "l", "a", &existing, &carried).unwrap();
        declare(&mut store, "s", "l", "a", &eval_report("eval-gg-2", "gg")).expect("the one carry covers this filing");

        let err = declare(&mut store, "s", "l", "a", &eval_report("eval-gg-3", "gg")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("open-g-1"), "the carry is used up: {}", refusal.problem);
    }

    /// G (2026-09-26): an open point is closed by a retraction with a reason,
    /// never by revising its tag away or its carry count down.
    #[test]
    fn an_open_point_cannot_lose_its_tag_or_its_carry_by_revise() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &open_point("open-h-1", "hh")).unwrap();
        let existing = show(&store, "open-h-1").unwrap();
        let mut untagged = existing.clone();
        untagged.tags = vec!["from-report:eval-hh-1".to_string()];
        assert!(revise(&mut store, "s", "l", "a", &existing, &untagged).is_err(), "dropping the tag must be refused");

        let mut carried = existing.clone();
        carried.tags.push(format!("{OPEN_POINT_CARRIED_PREFIX}1"));
        revise(&mut store, "s", "l", "a", &existing, &carried).unwrap();
        let now = show(&store, "open-h-1").unwrap();
        let mut uncarried = now.clone();
        uncarried.tags.retain(|t| !t.starts_with(OPEN_POINT_CARRIED_PREFIX));
        assert!(revise(&mut store, "s", "l", "a", &now, &uncarried).is_err(), "lowering the carry must be refused");
    }

    /// D (2026-09-26): an evaluation report already filed for ANOTHER
    /// project, moved onto this one, is a filing for this one - through both
    /// revise doors.
    #[test]
    fn moving_a_filed_report_to_another_project_is_held_to_that_projects_filing_gate() {
        let mut store = EventStore::in_memory().unwrap();
        legacy_thinly_answered(&mut store, "unboxed-d", Some("dd"));
        serve_n(&mut store, "unboxed-d", "hook", 1);
        declare(&mut store, "s", "l", "a", &eval_report("eval-elsewhere", "other")).unwrap();

        let filed = show(&store, "eval-elsewhere").unwrap();
        let mut moved = filed.clone();
        moved.project = Some("dd".to_string());
        let err = revise(&mut store, "s", "l", "a", &filed, &moved).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("neither a real tooth"), "{}", refusal.problem);

        let err = revise_because(&mut store, "s", "l", "a", &filed, &moved, &filed, None).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("neither a real tooth"), "the assistant's own revise door too: {}", refusal.problem);
        assert_eq!(show(&store, "eval-elsewhere").unwrap().project.as_deref(), Some("other"), "nothing moved");
    }

    /// D, the other half: adding the tag through the assistant's own door
    /// (`revise_because`) is a filing too - it never ran the gate at all.
    #[test]
    fn tagging_a_report_through_the_assistants_revise_door_is_gated() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &light_command_rule("owed-e", "ee")).unwrap();
        serve_n(&mut store, "owed-e", ASSISTANT_DELIVERY_ACTOR, JUDGEMENT_DEBT_AFTER);
        let mut plain = eval_report("plain-e", "ee");
        plain.tags = vec!["notes".to_string()];
        declare(&mut store, "s", "l", "a", &plain).unwrap();
        let mut tagged = plain.clone();
        tagged.tags.push(EVALUATION_REPORT_TAG.to_string());
        let scope = started_now(&store, "s");
        assert!(revise_because_scoped(&mut store, "s", "l", "a", &plain, &tagged, &plain, None, Some(&scope)).is_err());
    }

    /// E (2026-09-26): a heavy rule is not retracted on a reason that fits
    /// any rule; one that names what changed about it goes through.
    #[test]
    fn a_heavy_rule_needs_a_reason_about_itself_to_be_retracted() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s", "l", "a", &item).unwrap();
        assert!(retract(&mut store, "s", "l", "a", &item.id, "no longer relevant for this project").is_err());
        retract(&mut store, "s", "l", "a", &item.id, "config/app.toml no longer exists").unwrap();
    }
}
