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
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
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

// ------------------------------------------------------------ the actor fence
//
// THE LEAK THIS CLOSES (D1). Reserved actor names used to be refused by one
// binary at a time, so any other binary - a repair tool, the pins tool, a
// migration, a mark - could still write as `eval-machine` or `installer`, and
// a reader that trusts an actor then trusts a forgery. The fence now lives
// HERE, in the one module every write goes through: each public write
// function refuses a reserved name passed as a plain string, and THOR's own
// writers pass a `SystemActor` instead - a token whose constructor a grep test
// (`system_actor_is_built_only_in_the_named_modules`, below) allows in a short
// list of modules only.

/// The actor THOR's installer writes as. The only actor a project's
/// working-copy registration is ever read from (`root_registrations`).
pub const INSTALLER_ACTOR: &str = "installer";

/// Every actor name that belongs to THOR itself. None of them may be passed as
/// a plain string to a write function in this module (see `fence`).
///
/// `teeth-probe` is a legacy name that is never written again, and `hook` is
/// the name the hook itself stamps its own recorded events with: both stay
/// reserved so nothing else can write under them. Spelled as literals on
/// purpose - the constants that carry the same names elsewhere in this file may
/// retire, and the reservation must not retire with them.
pub const RESERVED_ACTORS: &[&str] = &[
    INSTALLER_ACTOR,
    "retest",
    "teeth-eval",
    "teeth-write",
    "eval-machine",
    "eval-validator",
    "attester",
    "teeth-probe",
    "hook",
];

/// A write by THOR itself. The field is private, so the only way to hold one
/// is `SystemActor::new`, and a grep test fails when that is called outside
/// the modules allowed to (see `system_actor_is_built_only_in_the_named_modules`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemActor {
    name: &'static str,
}

impl SystemActor {
    /// Panics when `name` is not one of `RESERVED_ACTORS`: a system actor
    /// with an ordinary name would be a token that means nothing, and that is
    /// a programming mistake, never a runtime condition.
    pub fn new(name: &'static str) -> Self {
        assert!(RESERVED_ACTORS.contains(&name), "SystemActor::new: '{name}' is not a reserved actor name");
        Self { name }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }
}

/// Who is writing: an ordinary name (the tool server, a CLI's `--actor`, a
/// repair tool) or THOR itself. Every write function in this module takes
/// `impl Into<WriteActor>`, so a plain `&str` still reads exactly as before
/// and only the reserved names are refused (see `fence`).
#[derive(Debug, Clone, Copy)]
pub enum WriteActor<'a> {
    Named(&'a str),
    System(SystemActor),
}

impl<'a> WriteActor<'a> {
    pub fn name(&self) -> &'a str {
        match self {
            WriteActor::Named(name) => name,
            WriteActor::System(actor) => actor.name(),
        }
    }
}

impl<'a> From<&'a str> for WriteActor<'a> {
    fn from(name: &'a str) -> Self {
        WriteActor::Named(name)
    }
}

impl<'a> From<&'a String> for WriteActor<'a> {
    fn from(name: &'a String) -> Self {
        WriteActor::Named(name.as_str())
    }
}

impl<'a> From<SystemActor> for WriteActor<'a> {
    fn from(actor: SystemActor) -> Self {
        WriteActor::System(actor)
    }
}

impl<'a> From<&SystemActor> for WriteActor<'a> {
    fn from(actor: &SystemActor) -> Self {
        WriteActor::System(*actor)
    }
}

/// The refusal for a reserved actor name passed as a plain string, or `Ok`.
/// Public so a binary can refuse the name EARLY, before it opens anything,
/// with the very same sentence the store gives (`model.exe --actor`,
/// `serve hook --actor`).
pub fn refuse_reserved_actor(actor: &str) -> Result<(), Refusal> {
    if RESERVED_ACTORS.contains(&actor) {
        return Err(Refusal {
            problem: format!("the actor name {actor} is reserved for THOR itself"),
            fix: "write without --actor, or with your own name".to_string(),
        });
    }
    Ok(())
}

/// The id of project `key`'s working-copy record, written by the installer.
pub fn project_opened_id(key: &str) -> String {
    format!("project-{key}-opened-by-install")
}

/// The id the installer registers a working copy under when the project's
/// own record (`project_opened_id`) was retracted before this build - a
/// tombstone can never be revised, so the registration needs a record of its
/// own.
pub fn project_root_id(key: &str) -> String {
    format!("project-{key}-root-by-install")
}

/// The one actor allowed to write any event on `entity_id`, when the id is
/// reserved for THOR itself; `None` for every ordinary id.
///
/// - `project-<key>-opened-by-install` and `project-<key>-root-by-install`:
///   only `installer`. Retracting the project record would otherwise
///   tombstone it, and no registration could ever land again (D2).
/// - `evaluation-start-*`: only `eval-machine`.
/// - `open-no-working-copy-*` and `open-working-tree-differs-*` (THOR's own
///   open points, `machine_point_id`): only `eval-machine` - it writes one
///   when a blocker holds a rule and closes it when the blocker lifts.
/// - `teeth-a4-cutoff` (`A4_CUTOFF_ID`): only `retest` - THOR's record of the
///   one point before which a contains check counts as old stock (Q24).
pub fn reserved_id_owner(entity_id: &str) -> Option<&'static str> {
    let project_record = entity_id.starts_with("project-")
        && (entity_id.ends_with("-opened-by-install") || entity_id.ends_with("-root-by-install"));
    if project_record {
        return Some(INSTALLER_ACTOR);
    }
    if entity_id.starts_with("evaluation-start-") || is_machine_point_id(entity_id) {
        return Some("eval-machine");
    }
    if entity_id == A4_CUTOFF_ID {
        return Some(RETEST_ACTOR);
    }
    None
}

/// The command a reader runs, from inside a checkout, to register it as its
/// project's working copy - always with `--db`, so it registers into the
/// memory the printing process has open and never into a default one (F7).
///
/// Lives here rather than in `ops` because `serve` and `mcp` print it too, and
/// neither can depend on `ops` (it depends on both of them);
/// `ops::install::register_command_for` is this same function.
pub fn register_command_for(db: Option<&Path>) -> String {
    format!("install{} --register-root --db \"{}\"", std::env::consts::EXE_SUFFIX, db_argument(db))
}

/// The full per-project install command, run from inside a checkout, with
/// `--db` for the same reason `register_command_for` carries it.
pub fn install_command_for(db: Option<&Path>) -> String {
    format!("install{} --db \"{}\"", std::env::consts::EXE_SUFFIX, db_argument(db))
}

/// The store path a printed command names, or a placeholder that says what
/// belongs there when the printing process has no file of its own open.
fn db_argument(db: Option<&Path>) -> String {
    match db {
        Some(db) => db.display().to_string(),
        None => "<path to thor.db>".to_string(),
    }
}

/// Let a write through the fence, or refuse it. Returns the actor name the
/// event is stamped with.
///
/// Two grounds, both refused before anything is read or appended:
/// - a reserved actor name passed as a plain string (`refuse_reserved_actor`);
/// - an event on a reserved id (`reserved_id_owner`) by any other actor - a
///   `SystemActor` of the wrong name included.
///
/// Public so a write outside this module that appends an event on an item
/// (the mark writers in `serve::mark`) goes through the very same fence.
pub fn fence<'a>(store: &EventStore, actor: WriteActor<'a>, entity_id: &str) -> Result<&'a str, WriteError> {
    if let WriteActor::Named(name) = actor {
        refuse_reserved_actor(name).map_err(WriteError::Refused)?;
    }
    let name = actor.name();
    match reserved_id_owner(entity_id) {
        Some(owner) if owner != name => Err(WriteError::Refused(reserved_id_refusal(store, entity_id, owner))),
        _ => Ok(name),
    }
}

fn reserved_id_refusal(store: &EventStore, entity_id: &str, owner: &str) -> Refusal {
    if owner == INSTALLER_ACTOR {
        Refusal {
            problem: format!("this record is THOR's own registration of a project's working copy ('{entity_id}')"),
            fix: format!(
                "run {} from inside the checkout instead",
                register_command_for(store.path().as_deref())
            ),
        }
    } else if owner == RETEST_ACTOR {
        Refusal {
            problem: format!(
                "'{entity_id}' is THOR's own record of the A4 cutoff for old contains checks, written once by THOR's \
                 retest and never moved"
            ),
            fix: format!("give your own item an id other than '{A4_CUTOFF_ID}'"),
        }
    } else if is_machine_point_id(entity_id) {
        Refusal {
            problem: format!("this open point is THOR's own ('{entity_id}'); it closes itself when its subject settles"),
            fix: "leave it alone - settle the rules it names, or do what its own text says, and the next evaluation start closes it. Give your own item an id that does not begin with 'open-no-working-copy-' or 'open-working-tree-differs-'"
                .to_string(),
        }
    } else {
        Refusal {
            problem: format!("'{entity_id}' is THOR's own record of an evaluation start, written only by THOR itself"),
            fix: "give your own item an id that does not begin with 'evaluation-start-'".to_string(),
        }
    }
}

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
        // TWO EVALUATION REPORTS ARE NEVER "THE SAME FACT TOLD TWICE" (step
        // 15, G15.2): each is the record of one evaluation, and two quiet
        // ones - nothing on any list, the same one-line TLDR - read alike on
        // purpose. Their text collision is skipped; the key rule above
        // still holds.
        if gate::is_evaluation_report(&existing) && gate::is_evaluation_report(item) {
            continue;
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
pub fn declare_in<'a>(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: impl Into<WriteActor<'a>>,
    item: &Item,
    root: Option<&Path>,
) -> Result<Event, WriteError> {
    declare_in_scoped(store, session_id, lineage_id, actor, item, root, None, None, None)
}

/// `declare_in`, for a write that may FILE an evaluation report: `ctx` says
/// which evaluation it files for and that the filing comes through a THOR
/// tool call (see `FilingContext`). Every other write passes `None` and is
/// untouched; an evaluation report with `None` is refused.
///
/// `trial` is the record of THOR's own trial of this item, spliced into the
/// event body as `"teeth_trial"` (`body_with_trial`); `att` is the tool
/// server's stamp of the call that makes the write, spliced after it as
/// `"att"` (`body_with_att`, step 12). `None` for either writes no key.
#[allow(clippy::too_many_arguments)]
pub fn declare_in_scoped<'a>(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: impl Into<WriteActor<'a>>,
    item: &Item,
    root: Option<&Path>,
    ctx: Option<&FilingContext>,
    trial: Option<&TrialRecord>,
    att: Option<&Att>,
) -> Result<Event, WriteError> {
    declare_in_scoped_because(store, session_id, lineage_id, actor, item, root, ctx, trial, att, None)
}

/// `declare_in_scoped`, for a writer that gives a reason: `because` is spliced
/// into the event body (`body_with_because`) the way a revise's is, so `history`
/// shows it. A dispute (`legitimate_calls`, `may_change`, `accept_refusing`)
/// on a new rule carries one, and the evaluation reads it back from here.
#[allow(clippy::too_many_arguments)]
pub fn declare_in_scoped_because<'a>(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: impl Into<WriteActor<'a>>,
    item: &Item,
    root: Option<&Path>,
    ctx: Option<&FilingContext>,
    trial: Option<&TrialRecord>,
    att: Option<&Att>,
    because: Option<&str>,
) -> Result<Event, WriteError> {
    let actor = actor.into();
    fence(store, actor, &item.id)?;
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
    declare_gated(store, session_id, lineage_id, actor, item, ctx, trial, att, None, because)
}

pub fn declare<'a>(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: impl Into<WriteActor<'a>>,
    item: &Item,
) -> Result<Event, WriteError> {
    declare_scoped(store, session_id, lineage_id, actor, item, None, None)
}

/// `declare`, with the filing context and the call's stamp
/// `declare_in_scoped` describes.
pub fn declare_scoped<'a>(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: impl Into<WriteActor<'a>>,
    item: &Item,
    ctx: Option<&FilingContext>,
    att: Option<&Att>,
) -> Result<Event, WriteError> {
    declare_gated(store, session_id, lineage_id, actor.into(), item, ctx, None, att, None, None)
}

/// Everything `declare_scoped` does, plus the trial record `declare_in_scoped`
/// carries (`body_with_trial`) and - for the installer's project record only
/// (`declare_project_record`) - the registered working copy spliced onto the
/// body as the trailing `"root"`/`"root_at"` keys.
#[allow(clippy::too_many_arguments)]
fn declare_gated(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: WriteActor<'_>,
    item: &Item,
    ctx: Option<&FilingContext>,
    trial: Option<&TrialRecord>,
    att: Option<&Att>,
    root: Option<(&str, &str)>,
    because: Option<&str>,
) -> Result<Event, WriteError> {
    let actor = fence(store, actor, &item.id)?;
    let item = normalized(item);
    gate::declare(&item).map_err(WriteError::Refused)?;
    old_stock_ground(None, trial).map_err(WriteError::Refused)?;
    teeth_tag_ground(None, &item, trial).map_err(WriteError::Refused)?;
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
    // A no-op for every write that is not itself an evaluation report. One
    // that passes is stored with THOR's machine section above the writer's
    // own text (step 15).
    let item = filed_evaluation_report(store, &item, None, ctx).map_err(WriteError::Refused)?.unwrap_or(item);
    let reason = because.map(str::trim).filter(|r| !r.is_empty());
    let mut body = match reason {
        Some(reason) => body_with_because(&item, reason).map_err(WriteError::Serialize)?,
        None => canonical_body(&item).map_err(WriteError::Serialize)?,
    };
    if let Some(trial) = trial {
        body = body_with_trial(&body, trial).map_err(WriteError::Serialize)?;
    }
    body = with_att(body, att).map_err(WriteError::Serialize)?;
    if let Some((root, at)) = root {
        body = body_with_root(&body, root, at).map_err(WriteError::Serialize)?;
    }
    store
        .append_event(session_id, lineage_id, actor, EventKind::FactCreated, &item.id, None, &body)
        .map_err(WriteError::Store)
}

/// Validate a revise (everything `declare` requires, plus no field the
/// existing item had may be dropped) and, only if it passes, append it as a
/// `fact_revised` event. `existing` should be the item just read with
/// `show`, so the field-preservation check has something real to compare
/// against.
pub fn revise<'a>(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: impl Into<WriteActor<'a>>,
    existing: &Item,
    updated: &Item,
) -> Result<Event, WriteError> {
    let actor = fence(store, actor.into(), &updated.id)?;
    // BOTH sides are normalised before the dropped-field check. An item
    // already stored with project "global" is a global item; rewriting it as
    // `None` says the same thing, so it must not read as dropping the field.
    // Comparing a normalised update against an unnormalised original would
    // refuse exactly the repair that fixes the store.
    let existing = normalized(existing);
    let updated = normalized(updated);
    gate::revise(&existing, &updated).map_err(WriteError::Refused)?;
    gate::a4_ground(Some(&existing), &updated, None).map_err(WriteError::Refused)?;
    teeth_tag_ground(Some(&existing), &updated, None).map_err(WriteError::Refused)?;
    // The same capacity refusal `declare` makes. Without this the gate was
    // one door wide: declare refused an item that could never be shown, and
    // revise walked the identical binding in through the back. Judged as
    // `capacity_for_revise` judges it (step 24): an item that keeps every
    // binding it stands on is answered by its own servings first.
    if let Capacity::DeadOnArrival(refusal) | Capacity::Full(refusal) =
        capacity_for_revise(store, &existing, &updated, &crate::served::now_iso8601()).map_err(WriteError::Store)?
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
    // This entry point carries no filing context (`FilingContext`), so a
    // revise that files through it is always refused (step 15): only the
    // THOR tool's own revise can file.
    if gate::newly_filed_as_evaluation(&existing, &updated) {
        if let Some(refusal) = check_evaluation_report_filing(store, &updated, None) {
            return Err(WriteError::Refused(refusal));
        }
    }
    let body = canonical_body(&without_stale_teeth_tags(&existing, &updated, None)).map_err(WriteError::Serialize)?;
    store
        .append_mutate_checked(session_id, lineage_id, actor, EventKind::FactRevised, &updated.id, None, &body)
        .map_err(WriteError::Store)
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
pub fn revise_because<'a>(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: impl Into<WriteActor<'a>>,
    existing: &Item,
    updated: &Item,
    before: &Item,
    because: Option<&str>,
) -> Result<Event, WriteError> {
    revise_because_scoped(store, session_id, lineage_id, actor, existing, updated, before, because, None)
}

/// `revise_because`, with the filing context `declare_in_scoped`
/// describes - for a revise that files an evaluation report.
#[allow(clippy::too_many_arguments)]
pub fn revise_because_scoped<'a>(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: impl Into<WriteActor<'a>>,
    existing: &Item,
    updated: &Item,
    before: &Item,
    because: Option<&str>,
    ctx: Option<&FilingContext>,
) -> Result<Event, WriteError> {
    revise_tried_scoped(store, session_id, lineage_id, actor, existing, updated, before, because, ctx, None, None, None)
}

/// `revise_because_scoped`, for a revise that carries THOR's trial record.
///
/// `trial` is the record of THOR's own trial of `updated`, spliced into the
/// event body as `"teeth_trial"`, right after `because` (`body_with_trial`).
/// `att` is the tool server's stamp of the call (step 12), spliced after the
/// record as `"att"`; `None` writes no key.
/// `parent_rev` is the head the caller read the item from: the write lands
/// only while that is still the head, so an item that changed between the
/// read and this write is refused (the log's own compare-and-swap,
/// `append_mutate_checked`) and nothing is appended. `None` keeps the old
/// behaviour: the single current head.
///
/// The grounds the two tracks switched on (step 8), for every writer that
/// comes through here: `gate::revise_weakening` (every widened weakening
/// needs a because), `gate::a4_ground` (a check whose literal the text does
/// not name, when this write attaches or changes it or unnames it),
/// `old_stock_ground` (THOR's old-stock mark, Q24, only from its retest),
/// `teeth_tag_ground` (a new `teeth:` label only with the record that
/// implies it), and `without_stale_teeth_tags` (a revise that changes what a
/// trial tried, with no record, drops every old label).
#[allow(clippy::too_many_arguments)]
pub fn revise_tried_scoped<'a>(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: impl Into<WriteActor<'a>>,
    existing: &Item,
    updated: &Item,
    before: &Item,
    because: Option<&str>,
    ctx: Option<&FilingContext>,
    trial: Option<&TrialRecord>,
    parent_rev: Option<&str>,
    att: Option<&Att>,
) -> Result<Event, WriteError> {
    let actor = fence(store, actor.into(), &updated.id)?;
    let existing = normalized(existing);
    let updated = normalized(updated);
    let before = normalized(before);
    gate::revise(&existing, &updated).map_err(WriteError::Refused)?;
    gate::revise_weakening(&before, &updated, because).map_err(WriteError::Refused)?;
    gate::a4_ground(Some(&before), &updated, None).map_err(WriteError::Refused)?;
    old_stock_ground(Some(actor), trial).map_err(WriteError::Refused)?;
    teeth_tag_ground(Some(&before), &updated, trial).map_err(WriteError::Refused)?;
    // Judged as `capacity_for_revise` judges it (step 24): an item that keeps
    // every binding it stands on is answered by its own servings first, the
    // same rule the reply's note and THOR's own retest use.
    if let Capacity::DeadOnArrival(refusal) | Capacity::Full(refusal) =
        capacity_for_revise(store, &before, &updated, &crate::served::now_iso8601()).map_err(WriteError::Store)?
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
    // A revise that files is stored like a new filing: THOR's machine
    // section for THIS evaluation, then the writer's own text - the part
    // below the machine block the report carried before, when it had one.
    let updated = if gate::newly_filed_as_evaluation(&before, &updated) {
        filed_evaluation_report(store, &updated, gate::machine_block(&before.text), ctx)
            .map_err(WriteError::Refused)?
            .unwrap_or(updated)
    } else {
        updated
    };
    let reason = because.map(str::trim).filter(|r| !r.is_empty());
    let written = without_stale_teeth_tags(&before, &updated, trial);
    let mut body = match reason {
        Some(reason) => body_with_because(&written, reason).map_err(WriteError::Serialize)?,
        None => canonical_body(&written).map_err(WriteError::Serialize)?,
    };
    if let Some(trial) = trial {
        body = body_with_trial(&body, trial).map_err(WriteError::Serialize)?;
    }
    body = with_att(body, att).map_err(WriteError::Serialize)?;
    store
        .append_mutate_checked(session_id, lineage_id, actor, EventKind::FactRevised, &updated.id, parent_rev, &body)
        .map_err(WriteError::Store)
}

/// THE K16 GROUND (step 8): a `teeth:` tag that is new on this write - not
/// carried by the head it replaces - is refused unless the write carries a
/// trial record whose verdict implies exactly that tag. Removing a tag is
/// always allowed. The label is only a readable copy of the record, so a
/// writer (a bin, the CLI, a hand-built call) can never set one THOR's trial
/// did not decide: `model.exe declare --tags teeth:proven-guard` used to
/// store a proven label nothing had ever tried.
fn teeth_tag_ground(before: Option<&Item>, updated: &Item, trial: Option<&TrialRecord>) -> Result<(), Refusal> {
    let implied = trial.and_then(implied_teeth_tag);
    for tag in updated.tags.iter().filter(|t| t.starts_with(TEETH_TAG_PREFIX)) {
        let carried = before.is_some_and(|b| b.tags.contains(tag));
        if carried || implied == Some(tag.as_str()) {
            continue;
        }
        return Err(Refusal {
            problem: format!("'{tag}' starts with '{TEETH_TAG_PREFIX}', which only THOR's own trial writes"),
            fix: "drop it from the tags - write the rule through remember or revise, and THOR tries it and adds the \
                  one label its verdict implies"
                .to_string(),
        });
    }
    Ok(())
}

/// THE Q24 FENCE: a trial record carrying THOR's old-stock mark
/// (`TrialFields::a4_old_stock`) is written only by THOR's own retest, on an
/// item that already exists - never by a declare, and never by any other
/// writer, a write tool's own trial included. `actor` is the name `fence`
/// let through (`None` for a declare); a reserved name only ever gets that
/// far as a `SystemActor`, so `retest` here is THOR's retest itself.
fn old_stock_ground(actor: Option<&str>, trial: Option<&TrialRecord>) -> Result<(), Refusal> {
    let Some(mark) = trial.and_then(|t| t.fields().a4_old_stock.as_ref()) else { return Ok(()) };
    if actor == Some(RETEST_ACTOR) {
        return Ok(());
    }
    Err(Refusal {
        problem: format!(
            "this write carries THOR's mark that an old contains check ('{}' in {}) counts as a tooth although the \
             text does not name it - only THOR's retest writes that mark, on a rule that already exists",
            gate::shown_literal(&mark.literal),
            mark.path
        ),
        fix: "write the rule without it: name the literal in the text, and THOR's trial of the write decides the rest"
            .to_string(),
    })
}

/// The one `teeth:` label a trial record implies: proven with evidence,
/// wired, or note - none while an A4 failure keeps the verdict undecided,
/// unless THOR's retest counted it as old contains stock (Q24).
fn implied_teeth_tag(record: &TrialRecord) -> Option<&'static str> {
    let fields = record.fields();
    if !fields.a4.is_empty() && fields.a4_old_stock.is_none() {
        return None;
    }
    Some(match fields.verdict {
        RecordedVerdict::RuleProven if !fields.evidence.is_empty() => TEETH_PROVEN_TAG,
        RecordedVerdict::RuleProven | RecordedVerdict::RuleWired => TEETH_WIRED_TAG,
        RecordedVerdict::Note => TEETH_NOTE_TAG,
    })
}

/// STRIP-ON-CHANGE (step 8): `updated` as it is written. A revise that changes
/// what a trial tries (its `fingerprint`: kind, text, anchors, check, project
/// or severity) and carries no trial record drops every `teeth:` tag, so a bin
/// that copies tags forward can never carry a stale label onto an item nobody
/// tried again. The item is then undecided until it is tried, which doctor
/// shows.
fn without_stale_teeth_tags(before: &Item, updated: &Item, trial: Option<&TrialRecord>) -> Item {
    if trial.is_some() || fingerprint(before) == fingerprint(updated) {
        return updated.clone();
    }
    let mut written = updated.clone();
    written.tags.retain(|t| !t.starts_with(TEETH_TAG_PREFIX));
    written
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

/// `body` with two more trailing keys, `"root":"<path>","root_at":"<time>"` -
/// the working copy the installer registered, and when. Spliced on exactly
/// the way `body_with_because` splices `because`. Only `declare_project_record`
/// and `revise_project_root` ever call this; `root_registrations` is the one
/// place that reads it back.
///
/// `root_at` exists because an event carries no wall-clock time of its own
/// (see `Revision`): without it neither doctor nor the session start could
/// say since when a working copy is registered.
fn body_with_root(body: &str, root: &str, at: &str) -> Result<String, serde_json::Error> {
    let root = serde_json::to_string(root)?;
    let at = serde_json::to_string(at)?;
    debug_assert!(body.ends_with('}'), "canonical_body always produces a JSON object");
    Ok(format!("{},\"root\":{root},\"root_at\":{at}}}", &body[..body.len() - 1]))
}

/// The `because` a `fact_revised` event body carries, if any - present only
/// on one written by `revise_because` for a revision `gate::revise_weakening`
/// judged a weakening; `None` for every other event, an ordinary `revise`
/// included. Mirrors `tombstone_reason` above: never fails, a body this
/// cannot parse simply carries none.
fn because_reason(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body).ok()?.get("because")?.as_str().map(str::to_string)
}

// ------------------------------------------------ a dispute and its reason
//
// A dispute (`legitimate_calls`, `may_change`, `accept_refusing`) turns a rule
// that bites into a note that cannot refuse anything, or keeps a check that
// refuses real calls. Measured 2026-10-07: of 11 disputes one maintenance pass
// saved, 7 were wrong (the disputed literal was the rule's real subject, or a
// better check existed), and nothing had asked the writer why. So a dispute is
// written with a `because` (the write tools refuse one without it), the reason
// lives in the item's history like a revise's, and everything below reads it
// back from the LOG: the evaluation's machine section lists every dispute with
// its reason, the evaluation's batch hands each unverified one to the agent,
// and doctor shows the reason beside it. A dispute from before this (no
// reason) reads as "no reason recorded" everywhere, and THOR's retest never
// carries one forward silently.

/// The fewest characters a dispute's `because` may have.
pub const DISPUTE_REASON_MIN_CHARS: usize = 20;

/// What a dispute of `fields` names, as (argument, calls or spans) - only the
/// arguments the writer used, in the order the door documents them.
pub fn dispute_arguments(fields: &TrialFields) -> Vec<(String, Vec<String>)> {
    [
        ("legitimate_calls", &fields.disputes.legitimate_calls),
        ("may_change", &fields.disputes.may_change),
        ("accept_refusing", &fields.accept_refusing),
    ]
    .into_iter()
    .filter(|(_, values)| !values.is_empty())
    .map(|(name, values)| (name.to_string(), values.clone()))
    .collect()
}

/// The reason the dispute recorded at log seq `seq` was written with: the
/// `because` of the newest write by someone other than THOR's own actors, at
/// or before `seq`, whose record named this same dispute. THOR's own retest
/// carries a dispute forward under its own name, so its events are looked
/// through. `None` when the writer gave none (every dispute stored before the
/// reason was required), or when `seq` carries no dispute.
pub fn dispute_reason(events: &[Event], seq: i64) -> Option<String> {
    let mut wanted: Option<Vec<(String, Vec<String>)>> = None;
    for event in events.iter().rev().filter(|e| e.seq <= seq) {
        let Ok(Some(record)) = trial_of(event) else { continue };
        let arguments = dispute_arguments(record.fields());
        match &wanted {
            None if arguments.is_empty() => return None,
            None => wanted = Some(arguments),
            Some(first) if *first != arguments => return None,
            Some(_) => {}
        }
        if RESERVED_ACTORS.contains(&event.actor.as_str()) {
            continue;
        }
        if let Some(reason) = because_reason(&event.body).map(|r| r.trim().to_string()).filter(|r| !r.is_empty()) {
            return Some(reason);
        }
    }
    None
}

/// The log seq where the run of records naming the dispute that `seq` carries
/// began: a retest that carries a dispute forward writes a newer record of the
/// same dispute, and that is not a new dispute.
fn dispute_origin(events: &[Event], seq: i64) -> i64 {
    let mut wanted: Option<Vec<(String, Vec<String>)>> = None;
    let mut origin = seq;
    for event in events.iter().rev().filter(|e| e.seq <= seq) {
        let Ok(Some(record)) = trial_of(event) else { continue };
        let arguments = dispute_arguments(record.fields());
        match &wanted {
            None => wanted = Some(arguments),
            Some(first) if *first != arguments => break,
            Some(_) => {}
        }
        origin = event.seq;
    }
    origin
}

/// The reason the dispute `id` stands on today was written with: see
/// `dispute_reason`. `None` for an item that carries no dispute, too.
pub fn current_dispute_reason(store: &EventStore, id: &str) -> Option<String> {
    let events = store.get_events_by_entity(id).ok()?;
    let last = events.iter().rev().find(|e| matches!(trial_of(e), Ok(Some(_))))?.seq;
    dispute_reason(&events, last)
}

/// A later write by the agent that answers a dispute: a revise with a reason
/// (`because`) after log seq `after` by anyone but THOR's own actors. It
/// confirms the dispute (the reason restates why) - or it came with the check
/// that replaced the dispute, which no longer carries one.
fn dispute_answered_since(events: &[Event], after: i64) -> Option<&Event> {
    events.iter().find(|e| {
        e.seq > after
            && e.kind == EventKind::FactRevised
            && !RESERVED_ACTORS.contains(&e.actor.as_str())
            && because_reason(&e.body).is_some_and(|r| !r.trim().is_empty())
    })
}

/// One dispute an evaluation hands to the agent to verify (`MachinePass::disputes`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisputeItem {
    pub id: String,
    /// The log seq of the write that first carried this dispute.
    pub seq: i64,
    /// What it names: (argument, calls or spans), only the arguments used.
    pub arguments: Vec<(String, Vec<String>)>,
    /// The writer's reason; `None` for a dispute stored before one was required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub because: Option<String>,
}

/// Every dispute of `project`'s rules (its own and global) that nobody has
/// answered yet and that was written after log seq `since` (the project's
/// previous evaluation report) or was left over from the previous evaluation's
/// list (`carried`): oldest first, the left-over ones before the rest. A
/// dispute is "answered" once the agent revised the rule with a reason after
/// it was written (`dispute_answered_since`); one that has been replaced by a
/// check is no dispute any more and is not listed.
pub fn unverified_disputes(store: &EventStore, project: &str, since: i64, carried: &[String]) -> Vec<DisputeItem> {
    let mut found: Vec<(Option<usize>, i64, DisputeItem)> = Vec::new();
    for (id, item) in all_live_items(store) {
        if !item.kind.can_fire() || !project_applies_to(item.project.as_deref(), Some(project)) {
            continue;
        }
        let Some(record) = current_record(store, &id) else { continue };
        let arguments = dispute_arguments(record.fields());
        if arguments.is_empty() {
            continue;
        }
        let Ok(events) = store.get_events_by_entity(&id) else { continue };
        let Some(last) = events.iter().rev().find(|e| matches!(trial_of(e), Ok(Some(_)))).map(|e| e.seq) else { continue };
        let origin = dispute_origin(&events, last);
        let left_over = carried.iter().position(|c| *c == id);
        if dispute_answered_since(&events, origin).is_some() || (origin <= since && left_over.is_none()) {
            continue;
        }
        let because = dispute_reason(&events, last);
        found.push((left_over, origin, DisputeItem { id, seq: origin, arguments, because }));
    }
    found.sort_by(|a, b| {
        match (a.0, b.0) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.1.cmp(&b.1),
        }
        .then_with(|| a.2.id.cmp(&b.2.id))
    });
    found.into_iter().map(|(_, _, item)| item).collect()
}

/// The dispute on `id`, frozen on `start`'s dispute list, in words, when it
/// is still to be answered: `None` once the rule is gone, off the rule track,
/// carries no dispute any more (a check replaced it), or the agent revised it
/// with a reason after the start. What the filing gate holds a report to.
fn dispute_owed(store: &EventStore, id: &str, start: &EvaluationStart) -> Option<String> {
    let item = show(store, id).ok()?;
    if !item.kind.can_fire() {
        return None;
    }
    let record = current_record(store, id)?;
    let arguments = dispute_arguments(record.fields());
    if arguments.is_empty() {
        return None;
    }
    let events = store.get_events_by_entity(id).ok()?;
    if dispute_answered_since(&events, start.frozen.started_seq).is_some() {
        return None;
    }
    Some(format!("a dispute ({}) nobody has verified", dispute_in_words(&arguments)))
}

/// A dispute's arguments in words: "may_change ['version'], legitimate_calls ['a call']".
pub fn dispute_in_words(arguments: &[(String, Vec<String>)]) -> String {
    arguments
        .iter()
        .map(|(name, values)| {
            let list = values.iter().map(|v| format!("'{}'", plain(v))).collect::<Vec<_>>().join(", ");
            format!("{name} [{list}]")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The disputes on `start`'s frozen dispute list that are still to be
/// answered, each with its reason in words, in the order they were frozen.
pub fn evaluation_frozen_disputes_owed(store: &EventStore, start: &EvaluationStart) -> Vec<(String, String)> {
    start
        .frozen
        .dispute_list()
        .iter()
        .filter(|d| credited_to_other_session(store, start, &d.id).is_none())
        .filter_map(|d| dispute_owed(store, &d.id, start).map(|why| (d.id.clone(), why)))
        .collect()
}

// ------------------------------------------------ the verdict in the log (A1, A6)
//
// A rule's verdict - on the rule track (proven or wired) or on the note
// track - lives in ONE place: a trial record spliced into the fact event it
// was written with, as the trailing key `"teeth_trial"` (after `because`, by
// the same byte splice). It counts only while its fingerprint equals the
// current head's: change the kind, the text, an anchor, the check, the
// project or the severity and it stops counting, and `current_verdict` says
// which one changed. Tags never decide it.
//
// SEALED, because a record is data that is built in another crate and read
// back from the log, so no type can hold it alone (X6):
// - `TrialRecord` has a private field and no `Deserialize`. From the log a
//   record comes only through `trial_of`; a new one only through
//   `TrialRecord::seal`; and its fields are read-only (`fields`), so a record
//   read back can never be edited into another one.
// - Two grep tests below (`trial_record_is_built_only_in_teeth_rs`,
//   `trial_record_is_deserialized_only_in_trial_of`) fail when `seal` is
//   called, a record is built, or the key is read anywhere else.
//
// Nothing writes a record yet: every caller of the write functions passes
// `None`, so no write changes behaviour until the trial is switched on.

/// The version of `TrialFields` this build writes and `trial_of` reads. A
/// record of any other version reads as unreadable, never as this one.
pub const TRIAL_RECORD_VERSION: u32 = 1;

/// The verdict a trial recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordedVerdict {
    /// On the rule track, and reality agrees: see `TrialFields::evidence`.
    /// Without evidence it reads as `RuleWired` (`current_verdict`).
    RuleProven,
    /// On the rule track: the forbidden form was refused and every clean
    /// form let through, and nothing real has met the check yet.
    RuleWired,
    /// On the note track: no tooth held. A note carries no check.
    Note,
}

/// What the hook's own pipeline did with one form a trial tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Refused,
    LetThrough,
}

/// One form a trial tried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialAttempt {
    /// What was tried, in words a reader can repeat ("git push --force x",
    /// "an Edit of NOTES.md removing every 'keep me'").
    pub form: String,
    /// The tool the synthetic call was made as ("Bash", "Edit", "Write").
    pub via: String,
    pub outcome: AttemptOutcome,
}

/// A real call that stopped a derived tooth from forcing (K8(c)).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoppedForcing {
    pub call: String,
    pub session: String,
    pub seq: i64,
}

/// What the trial read from the real calls in the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialCorpus {
    /// Real calls the check's anchor reaches.
    pub reached: usize,
    /// Of those, the ones that carry the literal as a whole token.
    pub whole_hits: usize,
    /// Reached calls the log recorded cut (an older build kept 120 characters,
    /// a longer call is cut at `serve::deliver::TRIGGER_LIMIT`), which prove
    /// nothing absent: they were not tried.
    pub cut: usize,
    pub stopped_forcing: Vec<StoppedForcing>,
}

/// The writer's disputes (K22), kept for the owner to read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialDisputes {
    pub legitimate_calls: Vec<String>,
    pub may_change: Vec<String>,
}

/// An earlier check on the rule's own log, and whether a revise that carried
/// a because removed it (D7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialHistory {
    pub seq: i64,
    pub check: Check,
    /// The because of the revise that removed or replaced it; `None` when no
    /// revise with a because did.
    pub removed_with_because: Option<String>,
}

/// Why a note keeps a check it could not try or could not hang yet (K20).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingWhy {
    /// The project has no registered working copy on this machine.
    NoWorkingCopy,
    /// The check holds in the session's own checkout, not in the registered one.
    NotTrueInRegisteredCheckout,
}

/// A check a note keeps in its record only - never live, never on the item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCheck {
    pub check: Check,
    pub why: PendingWhy,
}

/// What made a rule proven rather than wired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialEvidence {
    /// A refusal by the real hook in a real session, at this log seq.
    FieldRefusal { seq: i64 },
    /// A contains literal present in the file at this commit of the
    /// registered checkout.
    InHead { commit: String },
    /// A literal that really lived in the checked file, at this commit.
    LivedInHistory { commit: String },
}

/// Everything a trial records, in the order it is written. Only ever held
/// inside a `TrialRecord` once it is written; building one of these alone
/// proves nothing and writes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialFields {
    /// Always `TRIAL_RECORD_VERSION` for a record this build writes.
    pub v: u32,
    /// `fingerprint` of the item exactly as it was tried.
    pub fingerprint: String,
    /// The registered checkout the trial ran against, when it needed one.
    pub root: Option<String>,
    pub project: Option<String>,
    /// The commit at the top of `root` when the trial ran.
    pub head_commit: Option<String>,
    /// `gate::teeth_candidates` of the item as it was tried.
    pub candidates: Vec<gate::Candidate>,
    pub attempts: Vec<TrialAttempt>,
    pub corpus: TrialCorpus,
    /// The clean forms the writer's own check refused.
    pub clean_failed: Vec<String>,
    pub disputes: TrialDisputes,
    /// Real calls the writer accepted being refused (K8(d)).
    pub accept_refusing: Vec<String>,
    pub history: Vec<TrialHistory>,
    pub pending: Option<PendingCheck>,
    /// `gate::unnamed_literals` of the item as it was tried: empty when its
    /// check's every literal is named in its text. Anything here keeps the
    /// verdict undecided (K23).
    pub a4: Vec<String>,
    pub verdict: RecordedVerdict,
    pub evidence: Vec<TrialEvidence>,
    /// A contains literal found in the working tree but not in HEAD.
    pub uncommitted: bool,
    /// The head the trial read the item from (the compare-and-swap of X4).
    pub parent_rev: Option<String>,
    /// For a file check: the git blob hash of the checked file in `root` as it
    /// was tried (`git hash-object`), so a later pass can tell the file
    /// changed. Absent from a record written before this field existed.
    #[serde(default)]
    pub file_blob: Option<String>,
    /// THOR's own mark (Q24) that this rule's OLD contains check counts as a
    /// tooth although `a4` names its literal as unnamed: the guarded fragment,
    /// shown to every reader so they can judge whether it guards the rule's
    /// subject. Only THOR's retest writes it (`old_stock_ground`), and
    /// `current_verdict` re-reads the log before it counts. Left out of the
    /// JSON when absent, so every record without it reads and writes exactly
    /// as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a4_old_stock: Option<OldContainsStock>,
    /// Set when a revise kept the rule's check although its bindings never
    /// reach a write to the file it reads (step 24): a text revise never
    /// clears a check as a side effect. Says what is still missing (the
    /// binding, or the owner's decision when that place is full), and keeps
    /// the verdict undecided until a later trial finds the check reaching.
    /// Left out of the JSON when absent, so every other record reads and
    /// writes exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreached: Option<String>,
    /// The matcher revision of the build that decided this verdict
    /// (`serve::teeth::matcher_rev`): a hash of the sources that decide
    /// whether a call is refused. A record written before this field existed
    /// has none, and reads as decided by an older matcher. Never part of the
    /// fingerprint, so a rule is not made undecided by a newer build - it is
    /// only tried again (`serve retest --compare`, the evaluation start).
    /// Left out of the JSON when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher_rev: Option<String>,
}

/// THE Q24 EXEMPTION, in one value: an OLD contains check - one that stood on
/// the rule, with its text and anchors, at the A4 cutoff (`a4_cutoff`) and
/// that nothing changed since - whose text does not name its literal. THOR's
/// retest counts it as a tooth when it bites in the working copy registered
/// for the rule's project (the file proves the literal is real), and writes
/// this into the record so the fragment it guards is visible to every reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OldContainsStock {
    /// The guarded fragment: the literal the check keeps in its file.
    pub literal: String,
    /// The file (or directory) the check reads, relative to the checkout.
    pub path: String,
    /// The log seq of the fact event that gave the check this value.
    pub set_at: i64,
    /// The A4 cutoff it was judged against.
    pub cutoff: i64,
}

impl OldContainsStock {
    /// The fragment in words, for a receipt, `history` and doctor: "it guards
    /// 'keep me' in NOTES.md (its check was set at log seq 12, before the A4
    /// cutoff at log seq 40)".
    pub fn describe(&self) -> String {
        format!(
            "it guards '{}' in {} (its check was set at log seq {}, before the A4 cutoff at log seq {})",
            gate::shown_literal(&self.literal),
            self.path,
            self.set_at,
            self.cutoff
        )
    }
}

/// A trial record: `TrialFields`, sealed. See this section's banner for why
/// it has a private field and no `Deserialize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TrialRecord {
    fields: TrialFields,
}

impl TrialRecord {
    /// Seal a new record. Called only by THOR's own trial
    /// (`serve/src/teeth.rs`); a grep test fails on any other caller.
    pub fn seal(fields: TrialFields) -> Self {
        Self { fields }
    }

    /// The record's fields, read-only.
    pub fn fields(&self) -> &TrialFields {
        &self.fields
    }

    /// A record read back from the log - `trial_of` only.
    fn from_log(fields: TrialFields) -> Self {
        Self { fields }
    }
}

/// Keeps the fingerprint of one thing from ever equalling the hash of
/// another thing `hash_event` is used for.
const FINGERPRINT_DOMAIN: &str = "thor-teeth-fingerprint-v1";

/// The fingerprint a trial record is matched on: the existing
/// `thor_core::event_store::hash_event` over the canonical JSON of exactly
/// the item's kind, text, bindings, check, project (normalised, the way every
/// write stores it) and severity. Tags, falsifier, expires and key are not
/// part of it: changing them changes nothing a trial tried.
pub fn fingerprint(item: &Item) -> String {
    #[derive(Serialize)]
    struct Tried<'a> {
        kind: Kind,
        text: &'a str,
        bindings: &'a [Binding],
        check: Option<&'a Check>,
        project: Option<String>,
        severity: Option<Severity>,
    }
    let tried = Tried {
        kind: item.kind,
        text: &item.text,
        bindings: &item.bindings,
        check: item.check.as_ref(),
        project: crate::normalize::normalize_project(item.project.as_deref()),
        severity: item.severity,
    };
    let json = serde_json::to_string(&tried).expect("an item's own fields always serialize");
    thor_core::event_store::hash_event(FINGERPRINT_DOMAIN, &json)
}

/// `body` with one more trailing key, `"teeth_trial":{...}`, spliced on
/// exactly the way `body_with_because` splices `because`, so the item's
/// fields keep their order and `parse_body` reads the body unchanged. The one
/// writer of the key; `trial_of` is its one reader.
fn body_with_trial(body: &str, trial: &TrialRecord) -> Result<String, serde_json::Error> {
    let record = serde_json::to_string(trial)?;
    debug_assert!(body.ends_with('}'), "canonical_body always produces a JSON object");
    Ok(format!("{},\"teeth_trial\":{record}}}", &body[..body.len() - 1]))
}

/// The trial record `event` carries: `Ok(None)` when it carries none (or its
/// body is not JSON at all), `Err` with the reason when it carries one this
/// build cannot read - a record of another version, or one that does not
/// parse - so a reader never mistakes an unreadable record for no record.
/// The ONE place a record is read back from the log.
pub fn trial_of(event: &Event) -> Result<Option<TrialRecord>, String> {
    let Ok(body) = serde_json::from_str::<serde_json::Value>(&event.body) else { return Ok(None) };
    let Some(raw) = body.get("teeth_trial") else { return Ok(None) };
    let version = raw.get("v").and_then(serde_json::Value::as_u64);
    if version != Some(u64::from(TRIAL_RECORD_VERSION)) {
        let found = version.map_or_else(|| "without a version".to_string(), |v| format!("version {v}"));
        return Err(format!(
            "its trial record at log seq {} is {found}, and this THOR reads version {TRIAL_RECORD_VERSION} only",
            event.seq
        ));
    }
    let fields = TrialFields::deserialize(raw)
        .map_err(|e| format!("its trial record at log seq {} does not read ({e})", event.seq))?;
    Ok(Some(TrialRecord::from_log(fields)))
}

/// Where a rule stands right now, read from the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrentVerdict {
    RuleProven,
    RuleWired,
    Note,
    /// No verdict counts: `why` says what is missing, in words a reader can
    /// act on.
    Undecided { why: String },
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Why the head no longer matches its last trial: the fields that differ
/// between the item that trial was written with (`carrier`) and the head.
fn changed_since_trial(carrier: &Event, head: &Item) -> String {
    let Ok(tried) = parse_body(&carrier.body) else {
        return "its last trial record rides on an event that no longer reads as an item".to_string();
    };
    let mut changed = Vec::new();
    if tried.kind != head.kind {
        changed.push("kind");
    }
    if tried.text != head.text {
        changed.push("text");
    }
    if tried.bindings != head.bindings {
        changed.push("anchors");
    }
    if tried.check != head.check {
        changed.push("check");
    }
    let project = |item: &Item| crate::normalize::normalize_project(item.project.as_deref());
    if project(&tried) != project(head) {
        changed.push("project");
    }
    if tried.severity != head.severity {
        changed.push("severity");
    }
    if changed.is_empty() {
        return "its last trial record was made for a different version of it".to_string();
    }
    format!("its {} changed after its last trial", and_list(&changed))
}

/// The verdict of rule `id` right now: the LAST trial record on the item's
/// own line (its head, then each parent in turn), and only when that
/// record's fingerprint equals the head's. A tags-, falsifier-, expires- or
/// key-only revise keeps it; any other change makes the item Undecided until
/// it is tried again. Tags are never read, so a reason tag or an old
/// `teeth:` label changes nothing here.
///
/// Also Undecided, each with its reason: no item, a diverged or retracted
/// one, a kind that is never tried, a record this build cannot read, a
/// record whose `a4` names an unnamed literal (K23) - unless it carries
/// THOR's old-stock mark and the log still bears it out (`old_stock_stands`,
/// Q24) - and a record that contradicts the item it matches (a rule verdict
/// on an item with no check, a note that still carries one). A proven verdict
/// with no evidence reads as wired.
pub fn current_verdict(store: &EventStore, id: &str) -> CurrentVerdict {
    verdict_and_record(store, id).verdict
}

/// What one read of an item's log answers: `current_verdict`, the trial
/// record it counted (`Some` only for a verdict that is not Undecided), and
/// the head as it stands - read once, so `settlement` never reads the log of
/// an item twice.
struct VerdictRead {
    verdict: CurrentVerdict,
    record: Option<TrialRecord>,
    /// The head item, when there is exactly one head and it reads as an item.
    head: Option<Item>,
    /// The one head is a tombstone.
    retracted: bool,
}

fn verdict_and_record(store: &EventStore, id: &str) -> VerdictRead {
    let undecided = |why: String| VerdictRead { verdict: CurrentVerdict::Undecided { why }, record: None, head: None, retracted: false };
    let events = match store.get_events_by_entity(id) {
        Ok(events) => events,
        Err(e) => return undecided(format!("its log could not be read ({e})")),
    };
    let heads = compute_head_sets(&events);
    let Some(head_set) = heads.get(id) else { return undecided(format!("there is no item '{id}'")) };
    if head_set.heads.len() != 1 {
        return undecided(format!("its log has {} heads - resolve it first", head_set.heads.len()));
    }
    let by_hash: HashMap<&str, &Event> = events.iter().map(|e| (e.this_hash.as_str(), e)).collect();
    let head_hash = head_set.heads.iter().next().expect("checked len == 1 above");
    let Some(head) = by_hash.get(head_hash.as_str()).copied() else {
        return undecided(format!("there is no item '{id}'"));
    };
    if head.kind == EventKind::FactRetracted {
        return VerdictRead { retracted: true, ..undecided("it was retracted".to_string()) };
    }
    let Ok(item) = parse_body(&head.body) else {
        return undecided("its current body does not read as an item".to_string());
    };
    // From here on the head reads as an item: every answer carries it.
    let undecided = |why: String, item: Item| VerdictRead {
        verdict: CurrentVerdict::Undecided { why },
        record: None,
        head: Some(item),
        retracted: false,
    };
    if !item.kind.can_fire() {
        let kind = serde_json::to_value(item.kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        return undecided(format!("a {kind} is never tried - only a rule or an orientation carries a verdict"), item);
    }
    // The last trial on the item's own line. Bounded by the number of events,
    // so a malformed chain can never loop.
    let mut at = Some(head);
    let mut found = None;
    for _ in 0..events.len() {
        let Some(event) = at else { break };
        match trial_of(event) {
            Err(why) => return undecided(why, item),
            Ok(Some(record)) => {
                found = Some((record, event));
                break;
            }
            Ok(None) => at = event.parent_rev.as_deref().and_then(|p| by_hash.get(p).copied()),
        }
    }
    let Some((record, carrier)) = found else {
        return undecided("it has never been tried - there is no trial record on its log".to_string(), item);
    };
    let fields = record.fields();
    if fields.fingerprint != fingerprint(&item) {
        let why = changed_since_trial(carrier, &item);
        return undecided(why, item);
    }
    if let Some(why) = &fields.unreached {
        return undecided(why.clone(), item);
    }
    if !fields.a4.is_empty() && !old_stock_stands(store, &events, carrier, &item, fields) {
        let list = fields.a4.iter().map(|l| format!("'{}'", gate::shown_literal(l))).collect::<Vec<_>>().join(", ");
        let (verb, pronoun) = if fields.a4.len() == 1 { ("is", "it") } else { ("are", "them") };
        return undecided(
            format!(
                "A4: {list} {verb} not named in the text - revise the text to name {pronoun}, or clear the check with \
                 a because"
            ),
            item,
        );
    }
    let verdict = match (fields.verdict, item.check.is_some()) {
        (RecordedVerdict::Note, true) => {
            return undecided("its last trial made it a note, yet it carries a check - a note carries none".to_string(), item)
        }
        (RecordedVerdict::RuleProven | RecordedVerdict::RuleWired, false) => {
            return undecided("its last trial put it on the rule track, yet it carries no check".to_string(), item)
        }
        (RecordedVerdict::RuleProven, true) if fields.evidence.is_empty() => CurrentVerdict::RuleWired,
        (RecordedVerdict::RuleProven, true) => CurrentVerdict::RuleProven,
        (RecordedVerdict::RuleWired, true) => CurrentVerdict::RuleWired,
        (RecordedVerdict::Note, false) => CurrentVerdict::Note,
    };
    VerdictRead { verdict, record: Some(record), head: Some(item), retracted: false }
}

/// The trial record `current_verdict` counts for `id` right now, `None` while
/// it is Undecided - for a reader that lists what the verdict rests on
/// (disputes, accepted refusals, a pending check).
pub fn current_record(store: &EventStore, id: &str) -> Option<TrialRecord> {
    verdict_and_record(store, id).record
}

// ------------------------------------------------ old contains stock (Q24)
//
// THE OWNER'S DECISION (Q24). A4 keeps an agent from tying a meaningless
// literal to a rule: a check's literal has to be named in the rule's own
// text. Exempting `contains` from A4 outright would open a new exit - any
// line of any file hung on any rule as a "tooth". So:
// - A4 stays in force for every new write and every revise that sets or
//   changes a check, of every kind (`gate::a4_ground`, untouched here).
// - The OLD stock gets a one-time exemption: a contains check that stood on
//   the rule, with its text and anchors, at a FIXED cutoff, and that THOR's
//   retest finds biting both ways in the working copy registered for the
//   rule's project, counts as a tooth although its text does not name it.
// - The cutoff is recorded ONCE, by THOR's retest, as its own record on the
//   reserved id `A4_CUTOFF_ID` in the log the migration keeps its state in
//   (the trial records); the first record is the only one ever read, so no
//   later write can move it (`a4_cutoff`).
// - The record carries the guarded fragment (`OldContainsStock`) - which
//   literal, in which file - in a form only THOR's retest writes
//   (`old_stock_ground`), so every reader can judge whether it guards the
//   rule's subject.
// - Any revise of the rule's check, text or anchors after the cutoff ends it
//   for good: `old_contains_stock` reads the whole log, so even a revise that
//   puts them back does not bring it back, and A4 applies from then on.
// - No other check kind is ever exempt.

/// The reserved id THOR's record of the A4 cutoff lives on (Q24). Only
/// `retest` writes it (`reserved_id_owner`).
pub const A4_CUTOFF_ID: &str = "teeth-a4-cutoff";

/// The actor of THOR's own retest (`serve retest --apply`), which alone
/// records the cutoff and writes the old-stock mark.
const RETEST_ACTOR: &str = "retest";

/// The actor the evaluation's machine pass writes a rule's or a note's
/// verdict as (`serve::eval_session::machine_pass`, step 14). An old-stock
/// verdict (Q24) is the one exception: only `retest` writes that mark, so
/// the pass writes it as THOR's retest, which is what it runs.
pub const TEETH_EVAL_ACTOR: &str = "teeth-eval";

/// The A4 cutoff recorded on this store: the log seq at or before which a
/// contains check counts as old stock. `None` until THOR's retest records it.
///
/// Only the FIRST record by `retest` on `A4_CUTOFF_ID` is ever read - a later
/// one, however it got there, changes nothing - and a record can never claim
/// a point after its own place in the log.
pub fn a4_cutoff(store: &EventStore) -> Option<i64> {
    let events = store.get_events_by_entity(A4_CUTOFF_ID).ok()?;
    a4_cutoff_in(&events)
}

/// `a4_cutoff`, over the events of `A4_CUTOFF_ID` already in hand.
fn a4_cutoff_in(events: &[Event]) -> Option<i64> {
    let first = events
        .iter()
        .filter(|e| e.kind == EventKind::GateStoodAside && e.actor == RETEST_ACTOR)
        .min_by_key(|e| e.seq)?;
    let body = serde_json::from_str::<serde_json::Value>(&first.body).ok()?;
    let cutoff = body.get("cutoff_seq")?.as_i64()?;
    Some(cutoff.min(first.seq - 1))
}

/// The cutoff a retest judges old stock by when none is recorded yet: the
/// last event of the log right now - what `record_a4_cutoff` records when
/// the first old-stock verdict is written.
pub fn prospective_a4_cutoff(store: &EventStore) -> i64 {
    store.get_next_seq().map(|next| next - 1).unwrap_or(0)
}

/// Record the A4 cutoff, once: `cutoff` is the point the retest judged by
/// (`prospective_a4_cutoff` when it read the store). Returns the cutoff that
/// stands - the one already recorded when there is one, which is never
/// replaced. Only THOR's retest records it; a cutoff after the last event of
/// the log is refused.
pub fn record_a4_cutoff(store: &mut EventStore, actor: SystemActor, cutoff: i64) -> Result<i64, WriteError> {
    if actor.name() != RETEST_ACTOR {
        return Err(WriteError::Refused(Refusal {
            problem: format!("the A4 cutoff is recorded by THOR's retest only, not by '{}'", actor.name()),
            fix: "let `serve retest --apply` record it when it first counts an old contains check".to_string(),
        }));
    }
    if let Some(recorded) = a4_cutoff(store) {
        return Ok(recorded);
    }
    let last = store.get_next_seq().map_err(|e| WriteError::Store(e.into()))? - 1;
    if !(0..=last).contains(&cutoff) {
        return Err(WriteError::Refused(Refusal {
            problem: format!("an A4 cutoff at log seq {cutoff} lies outside the log (its last event is seq {last})"),
            fix: "run `serve retest --apply` again: it reads the log afresh".to_string(),
        }));
    }
    let name = fence(store, actor.into(), A4_CUTOFF_ID)?;
    let body = serde_json::json!({
        "cutoff_seq": cutoff,
        "why": "a contains check that stood on its rule, with its text and anchors, at this log seq and that nothing \
                changed since is old stock: THOR's retest counts it as a tooth when it bites, although its text does \
                not name its literal (Q24)",
    })
    .to_string();
    store
        .append_event(RETEST_ACTOR, RETEST_ACTOR, name, EventKind::GateStoodAside, A4_CUTOFF_ID, None, &body)
        .map_err(|e| WriteError::Store(e.into()))?;
    Ok(cutoff)
}

/// Whether a fact event moves an item's head: the only events whose body
/// can change its check, text or anchors.
fn moves_the_head(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::FactCreated
            | EventKind::FactRevised
            | EventKind::FactSuperseded
            | EventKind::FactRetracted
            | EventKind::FactResolved
    )
}

/// Whether an item's own log (`events`, every event on its id) makes its
/// check OLD CONTAINS STOCK under `cutoff`: the last fact event at or before
/// the cutoff holds a contains check, and every fact event after it holds
/// exactly the same check, text and anchors - a retraction, or any change to
/// one of the three, ends it for good, even when a later revise puts them
/// back. The answer carries the guarded fragment and the seq the check got
/// this value; `None` for anything else, every other check kind included.
pub fn old_contains_stock(events: &[Event], cutoff: i64) -> Option<OldContainsStock> {
    let mut facts: Vec<&Event> = events.iter().filter(|e| moves_the_head(e.kind)).collect();
    facts.sort_by_key(|e| e.seq);
    let shape = |e: &Event| -> Option<(Option<Check>, String, Vec<Binding>)> {
        if e.kind == EventKind::FactRetracted {
            return None;
        }
        parse_body(&e.body).ok().map(|i| (i.check, i.text, i.bindings))
    };
    let (before, after): (Vec<&Event>, Vec<&Event>) = facts.into_iter().partition(|e| e.seq <= cutoff);
    let last = *before.last()?;
    let at_cutoff = shape(last)?;
    let Some(Check::Contains { path, literal }) = at_cutoff.0.clone() else { return None };
    if after.iter().any(|e| shape(*e).as_ref() != Some(&at_cutoff)) {
        return None;
    }
    let mut set_at = last.seq;
    for e in before.iter().rev() {
        match shape(*e) {
            Some((check, ..)) if check == at_cutoff.0 => set_at = e.seq,
            _ => break,
        }
    }
    Some(OldContainsStock { literal, path, set_at, cutoff })
}

/// Whether a record's old-stock mark still counts for `item` (Q24): THOR's
/// retest wrote the record, the mark names exactly the item's contains check
/// and the one literal A4 finds unnamed, and the log still bears it out under
/// the RECORDED cutoff - so a revise of the check, text or anchors after the
/// cutoff ends it, even a bin's revise that carries no record at all.
fn old_stock_stands(store: &EventStore, events: &[Event], carrier: &Event, item: &Item, fields: &TrialFields) -> bool {
    let Some(mark) = &fields.a4_old_stock else { return false };
    if carrier.actor != RETEST_ACTOR {
        return false;
    }
    let Some(Check::Contains { path, literal }) = &item.check else { return false };
    if *path != mark.path || *literal != mark.literal || fields.a4.as_slice() != std::slice::from_ref(literal) {
        return false;
    }
    let Some(cutoff) = a4_cutoff(store) else { return false };
    old_contains_stock(events, cutoff).is_some_and(|now| now.path == mark.path && now.literal == mark.literal)
}

// ------------------------------------------------ settlement (step 9, F9, D8)
//
// THE ONE READER. The evaluation-filing gate, the Stop hook's teeth question
// and doctor all ask `settlement` whether a rule is settled - never a tag,
// never a reason, never a refusal on record, never a probe event. A rule is
// settled by its current verdict (a trial record whose fingerprint equals the
// head's), by being retracted or taken off the rule track, or - only while it
// holds - by a machine blocker, judged at READ time
// (`checkout::read_time_blocker`) the same way the retest judges it, so no
// reader ever demands a retest whose answer is "blocked" (F9). Severity plays
// no part: lowering it changes the fingerprint, so the rule is owed again
// (D8).

/// Why a maintenance verdict cannot be reached right now, on the machine's
/// side (K10) - nothing the agent can run settles it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocker {
    /// The rule's project has no usable registered working copy on this
    /// machine, and its check reads a file.
    NoWorkingCopy { project: String },
    /// A `contains` literal is missing from the registered working tree while
    /// the default branch's HEAD still has it.
    WorkingTreeDiffers { path: String },
    /// A file check needs a place the rule is not anchored to, the only way
    /// to name it (anchor the rule there) is refused because that place
    /// already holds the most facts a place ever shows, AND no fact there
    /// honestly gives way: every one is a heavier rule whose check can refuse
    /// (step 22, C3b B4; narrowed by step 26). A full place with an honest
    /// exit is the AGENT's to clear - fold, move or retract by THOR's rules -
    /// never the owner's (`FullPlace`).
    AnchorFull { path: String },
}

/// The subject of THOR's open point for rules with NO project whose check
/// reads a file - `Blocker::NoWorkingCopy { project: GLOBAL_RULES_SUBJECT }`
/// (K10: "a global rule with a file check included"; step 20). Such a rule
/// has no working copy THOR could try its check in, and giving it a project
/// would narrow a rule meant for every repository, so it is the machine's
/// blocker, never the agent's fix. "global" never names a project
/// (`normalize::normalize_project` folds it to none), so it cannot collide
/// with one.
pub const GLOBAL_RULES_SUBJECT: &str = "global";

/// The id of THOR's ONE open point for rules with no project held for lack
/// of a working copy (step 21): one for the whole store, never one per
/// evaluated project, so it counts once in the owner queue and against no
/// project's budget. It keeps the reserved prefix (only `eval-machine` writes
/// it), and the double hyphen keeps it apart from every project's own point:
/// a project's part of an id (`slug`) never starts with a hyphen.
pub const GLOBAL_RULES_POINT_ID: &str = "open-no-working-copy--global-rules";

impl Blocker {
    /// The blocker of a rule with no project whose check reads a file.
    pub fn global_rules() -> Self {
        Blocker::NoWorkingCopy { project: GLOBAL_RULES_SUBJECT.to_string() }
    }

    /// Whether this is `Blocker::global_rules`.
    pub fn is_global_rules(&self) -> bool {
        matches!(self, Blocker::NoWorkingCopy { project } if project == GLOBAL_RULES_SUBJECT)
    }

    /// The blocker's code, as THOR's open point carries it in its
    /// `blocker:` tag.
    pub fn code(&self) -> &'static str {
        match self {
            Blocker::NoWorkingCopy { .. } => "no-working-copy",
            Blocker::WorkingTreeDiffers { .. } => "working-tree-differs",
            Blocker::AnchorFull { .. } => "anchor-full",
        }
    }

    /// What the blocker is about - the subject of THOR's open point: the
    /// project for `NoWorkingCopy`, the file for `WorkingTreeDiffers`.
    pub fn subject(&self) -> &str {
        match self {
            Blocker::NoWorkingCopy { project } => project,
            Blocker::WorkingTreeDiffers { path } | Blocker::AnchorFull { path } => path,
        }
    }
}

impl std::fmt::Display for Blocker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Blocker::NoWorkingCopy { .. } if self.is_global_rules() => {
                write!(f, "a rule with no project - no working copy to try its file check in")
            }
            Blocker::NoWorkingCopy { project } => write!(f, "{project} - no registered working copy"),
            Blocker::WorkingTreeDiffers { path } => {
                write!(f, "{path} - the registered working tree lacks the literal the default branch still has")
            }
            Blocker::AnchorFull { path } => {
                write!(f, "{path} - the anchor this rule's check needs is full of heavier rules whose checks can refuse")
            }
        }
    }
}

/// How a rule is settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// On the rule track, by its current verdict.
    Rule { proven: bool },
    /// On the note track, by its current verdict - a note keeping a pending
    /// check (K20) and a note by dispute (K22) included.
    Note { pending: Option<PendingWhy>, by_dispute: bool },
    /// Held by a machine blocker, only while it holds.
    Blocked(Blocker),
    /// Retracted: no longer live.
    Retracted,
    /// Its kind can no longer fire (archived, or moved to a Report).
    OffTheRuleTrack,
}

/// Whether a rule is settled, and if not, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settlement {
    Settled(Settled),
    /// Owed: `why` is what `current_verdict` says is missing.
    Owed { why: String },
}

impl Settlement {
    pub fn is_owed(&self) -> bool {
        matches!(self, Settlement::Owed { .. })
    }
}

/// Whether rule `id` is settled - the one reader (see this section's banner).
///
/// `start: None` judges it right now: a blocker is read at READ time
/// (`checkout::read_time_blocker`). That is what doctor and the Stop hook's
/// backlog burn ask, outside any evaluation.
///
/// `start: Some(evaluation)` judges it for that evaluation (step 14, v2): what
/// holds a rule is what THOR's machine pass found AT THE START, written in the
/// start record (`Frozen::machine`), never the state of a folder at filing
/// time:
/// - a rule the pass found BLOCKED is settled by that blocker for this
///   evaluation - the machine's open point holds it, not the agent;
/// - a rule the pass handed to the agent (the frozen batch or the A4 list)
///   is settled only by a verdict written AFTER the start, by being retracted
///   or taken off the rule track, or - for a contains check whose literal was
///   gone - by the literal being back in the registered working copy. A
///   blocker that appears later settles nothing here: it did not hold at the
///   start, so the rule was this evaluation's to decide;
/// - any other rule is read by its current verdict alone, with no blocker.
pub fn settlement(store: &EventStore, id: &str, start: Option<&EvaluationStart>) -> Settlement {
    match start {
        None => settlement_now(store, id),
        Some(start) => settlement_at(store, id, start),
    }
}

/// `settlement` for one evaluation (see its doc comment).
fn settlement_at(store: &EventStore, id: &str, start: &EvaluationStart) -> Settlement {
    let read = verdict_and_record(store, id);
    if read.retracted {
        return Settlement::Settled(Settled::Retracted);
    }
    let Some(item) = read.head else {
        let why = match read.verdict {
            CurrentVerdict::Undecided { why } => why,
            _ => format!("'{id}' does not read as an item"),
        };
        return Settlement::Owed { why };
    };
    if !item.kind.can_fire() {
        return Settlement::Settled(Settled::OffTheRuleTrack);
    }
    let found = start.frozen.machine.as_ref().and_then(|m| m.items.iter().find(|i| i.id == id));
    let handed_to_agent = found.is_some_and(|i| matches!(i.outcome, MachineOutcome::NeedsAgent | MachineOutcome::A4Only));
    let by_verdict = |verdict: CurrentVerdict, record: Option<TrialRecord>| match verdict {
        CurrentVerdict::RuleProven => Some(Settled::Rule { proven: true }),
        CurrentVerdict::RuleWired => Some(Settled::Rule { proven: false }),
        CurrentVerdict::Note => {
            let fields = record.as_ref().map(TrialRecord::fields);
            Some(Settled::Note {
                pending: fields.and_then(|f| f.pending.as_ref().map(|p| p.why)),
                by_dispute: fields
                    .is_some_and(|f| !f.disputes.legitimate_calls.is_empty() || !f.disputes.may_change.is_empty()),
            })
        }
        CurrentVerdict::Undecided { .. } => None,
    };
    let undecided_why = match &read.verdict {
        CurrentVerdict::Undecided { why } => Some(why.clone()),
        _ => None,
    };
    match by_verdict(read.verdict, read.record) {
        Some(settled) if !handed_to_agent => Settlement::Settled(settled),
        Some(settled) => {
            // Handed to the agent at the start while it carried a verdict: a
            // rule-track contains check THOR tried again and found false. A
            // verdict written since the start settles it; so does the literal
            // being back where the check reads it.
            let holds_again = matches!(&item.check, Some(Check::Contains { .. }))
                && crate::checkout::registered_checkout(store, item.project.as_deref())
                    .is_ok_and(|root| item.check.as_ref().is_some_and(|c| crate::check::run(c, &root) == crate::check::Outcome::Holds));
            if verdict_written_after(store, id, start.seq) || holds_again {
                Settlement::Settled(settled)
            } else {
                Settlement::Owed { why: found.map(|i| i.detail.clone()).unwrap_or_default() }
            }
        }
        None => match found {
            Some(MachineItem { outcome: MachineOutcome::Blocked, blocker: Some(blocker), .. }) => {
                Settlement::Settled(Settled::Blocked(blocker.to_blocker()))
            }
            Some(found) if handed_to_agent => Settlement::Owed { why: found.detail.clone() },
            _ => Settlement::Owed { why: undecided_why.unwrap_or_default() },
        },
    }
}

/// `settlement` right now, with a blocker read at read time.
fn settlement_now(store: &EventStore, id: &str) -> Settlement {
    let read = verdict_and_record(store, id);
    if read.retracted {
        return Settlement::Settled(Settled::Retracted);
    }
    let Some(item) = read.head else {
        let why = match read.verdict {
            CurrentVerdict::Undecided { why } => why,
            _ => format!("'{id}' does not read as an item"),
        };
        return Settlement::Owed { why };
    };
    if !item.kind.can_fire() {
        return Settlement::Settled(Settled::OffTheRuleTrack);
    }
    match (read.verdict, read.record) {
        (CurrentVerdict::RuleProven, _) => Settlement::Settled(Settled::Rule { proven: true }),
        (CurrentVerdict::RuleWired, _) => Settlement::Settled(Settled::Rule { proven: false }),
        (CurrentVerdict::Note, record) => {
            let fields = record.as_ref().map(TrialRecord::fields);
            Settlement::Settled(Settled::Note {
                pending: fields.and_then(|f| f.pending.as_ref().map(|p| p.why)),
                by_dispute: fields
                    .is_some_and(|f| !f.disputes.legitimate_calls.is_empty() || !f.disputes.may_change.is_empty()),
            })
        }
        (CurrentVerdict::Undecided { why }, _) => match crate::checkout::read_time_blocker(store, &item) {
            Some(blocker) => Settlement::Settled(Settled::Blocked(blocker)),
            None => Settlement::Owed { why },
        },
    }
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
    // A Command anchor is read once, as the call the runtime would see it
    // when this very command is typed (see `command_covers`).
    let new_command = match binding {
        Binding::Target { kind: crate::item::TargetKind::Command, value } => {
            Some(crate::shell::segments(value, crate::shell::Dialect::PowerShell))
        }
        _ => None,
    };
    let Some(kind_match): Option<&dyn Fn(&Binding) -> bool> = (match binding {
        Binding::Always => None,
        Binding::Moment(_) => Some(&|b: &Binding| matches!((b, binding), (Binding::Moment(x), Binding::Moment(y)) if x == y)),
        Binding::Target { .. } => Some(&|b: &Binding| match (b, binding) {
            (Binding::Target { kind: bk, value: bv }, Binding::Target { kind, value }) => {
                crate::normalize::target_matches(*bk, bv, *kind, value)
                    || new_command.as_ref().is_some_and(|segments| {
                        *bk == crate::item::TargetKind::Command && crate::shell::reaches_any(bv, segments)
                    })
            }
            _ => false,
        }),
    }) else {
        return Vec::new();
    };

    candidates
        .iter()
        .filter(|(id, other)| shares_a_pool_with(item, id, other))
        .filter(|(_, other)| other.bindings.iter().any(|b| kind_match(b)))
        .collect()
}

/// Whether `other` (stored as `id`) can sit in a pool `item` is in: a live
/// firing kind of its own project or of the global tier (scope: see
/// `pool_rivals`).
fn shares_a_pool_with(item: &Item, id: &str, other: &Item) -> bool {
    if id == item.id || !other.kind.can_fire() {
        return false;
    }
    match (item.project.as_deref(), other.project.as_deref()) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some(_), None) => true,
        (Some(mine), Some(theirs)) => mine == theirs,
    }
}

/// Whether the Command anchor `broad` fires on every call the Command anchor
/// `narrow` fires on - the runtime's own word-prefix reading
/// (`shell::reaches_any`: the anchor's words are a prefix of the call's), asked
/// of `narrow` read as a call. `normalize::target_matches` cannot answer this:
/// it compares paths (equality or a path-segment suffix), so `node
/// tools/push.mjs` never counted as a rival of `node tools/push.mjs 10.0.0.5`
/// while at runtime it reaches every call the longer anchor was written for.
/// Both anchors are read in the PowerShell dialect, the one an anchor itself is
/// parsed in (a backslash is literal).
fn command_covers(broad: &str, narrow: &str) -> bool {
    crate::shell::reaches_any(broad, &crate::shell::segments(narrow, crate::shell::Dialect::PowerShell))
}

/// The live items that fire together on the BUSIEST call a narrower rival
/// anchor stands for, when the NEW Command `binding` is a word prefix of that
/// anchor: the new item fires on that call too, so those items and the new one
/// all compete for its block. Empty for any other kind of binding, or when no
/// live anchor is narrower than the new one.
///
/// Counted by the write gate's capacity refusal only (`full_reliable_anchor`),
/// never by `pool_rivals`. Not every narrower anchor counts: `ssh`, written
/// when a dozen different `ssh <host>` anchors exist, is not crowded by them,
/// because no single call reaches all twelve. A call is the unit that
/// competes, so this takes the one call (a narrower anchor read as a call)
/// where the most live items fire at once - the displacement the refusal can
/// actually promise.
fn busiest_narrower_call<'a>(
    candidates: &'a [(String, Item)],
    item: &Item,
    binding: &Binding,
) -> Vec<&'a (String, Item)> {
    let Binding::Target { kind: crate::item::TargetKind::Command, value } = binding else {
        return Vec::new();
    };
    let in_scope: Vec<&(String, Item)> =
        candidates.iter().filter(|(id, other)| shares_a_pool_with(item, id, other)).collect();
    let commands = |other: &Item| -> Vec<String> {
        other
            .bindings
            .iter()
            .filter_map(|b| match b {
                Binding::Target { kind: crate::item::TargetKind::Command, value } => Some(value.clone()),
                _ => None,
            })
            .collect()
    };
    let mut narrower: Vec<String> = in_scope
        .iter()
        .flat_map(|(_, other)| commands(other))
        .filter(|ov| ov != value && command_covers(value, ov))
        .collect();
    narrower.sort();
    narrower.dedup();

    let mut busiest: Vec<&(String, Item)> = Vec::new();
    for call in &narrower {
        let firing: Vec<&(String, Item)> = in_scope
            .iter()
            .copied()
            .filter(|(_, other)| commands(other).iter().any(|ov| command_covers(ov, call)))
            .collect();
        if firing.len() > busiest.len() {
            busiest = firing;
        }
    }
    busiest
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
    // THE SAME RULE FOR "CAN NEVER REACH A BLOCK" (step 24). The estimate
    // said an item standing on dashboard.html was held by 6 heavier rivals
    // and "can never reach a block", while the log showed it served 1028
    // times in 30 days, 432 of them at that very file. For an item that
    // already stands on every binding it keeps, the log's servings answer
    // both estimates the same way: shown, so not crowded out.
    if let Capacity::DeadOnArrival(_) = &cap {
        let served = crate::served::served_count(store, &updated.id, now, crate::served::SERVED_WINDOW_DAYS);
        if served == 0 {
            return Ok(cap);
        }
        let place = updated
            .bindings
            .iter()
            .find(|b| !matches!(b, Binding::Always))
            .map(describe)
            .unwrap_or_else(|| "its bindings".to_string());
        return Ok(Capacity::Crowded(format!(
            "shown {served} time(s) in the last {} days at {place} - not crowded out",
            crate::served::SERVED_WINDOW_DAYS
        )));
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
        let mut rivals = pool_rivals(candidates, item, binding);
        let busiest = busiest_narrower_call(candidates, item, binding);
        if busiest.len() > rivals.len() {
            rivals = busiest;
        }
        if rivals.len() < crate::item::MAX_ITEMS {
            continue;
        }
        let named = rivals
            .iter()
            // Each occupant's severity AND whether it carries a check (step
            // 26, field test 2): a description gives way before a guard.
            .map(|(id, other)| format!("'{id}' ({}, {}): {}", severity_word(other.severity), check_word(other.check.as_ref()), other.text))
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

/// Whether an occupant of a place carries a check, in words: "check:
/// <the check>" or "no check".
fn check_word(check: Option<&Check>) -> String {
    match check {
        Some(check) => format!("check: {}", check_named(check)),
        None => "no check".to_string(),
    }
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

/// The live facts sitting on `binding` for `item`, when that place is already
/// full - `item::MAX_ITEMS` or more of them, the count `capacity` refuses a new
/// claimant by (`full_reliable_anchor`) - as (id, text); `None` while it has
/// room. A Path, Dir or Command target only: the places a write is refused for.
pub fn full_place_occupants(store: &EventStore, item: &Item, binding: &Binding) -> Option<Vec<(String, String)>> {
    if !matches!(
        binding,
        Binding::Target { kind: crate::item::TargetKind::Path | crate::item::TargetKind::Dir | crate::item::TargetKind::Command, .. }
    ) {
        return None;
    }
    let candidates = if store.heads_projection_current() { live_items_from_projection(store) } else { live_items_from_fold(store) }.ok()?;
    let rivals = pool_rivals(&candidates, item, binding);
    (rivals.len() >= crate::item::MAX_ITEMS).then(|| rivals.into_iter().map(|(id, other)| (id.clone(), other.text.clone())).collect())
}

/// The live facts sitting on a full `binding` for `item`, as whole items -
/// `full_place_occupants` with everything a reader needs to choose which one
/// gives way (step 26).
pub fn full_place_items(store: &EventStore, item: &Item, binding: &Binding) -> Option<Vec<(String, Item)>> {
    if !matches!(
        binding,
        Binding::Target { kind: crate::item::TargetKind::Path | crate::item::TargetKind::Dir | crate::item::TargetKind::Command, .. }
    ) {
        return None;
    }
    let candidates = if store.heads_projection_current() { live_items_from_projection(store) } else { live_items_from_fold(store) }.ok()?;
    let rivals = pool_rivals(&candidates, item, binding);
    (rivals.len() >= crate::item::MAX_ITEMS).then(|| rivals.into_iter().map(|(id, other)| (id.clone(), other.clone())).collect())
}

/// An informing note for a GLOBAL Rule/Orientation written onto a place that is
/// already full in one or more registered projects; `None` otherwise.
///
/// WHY A NOTE AND NOT A REFUSAL. `shares_a_pool_with` compares a global write
/// only with other globals, on purpose: refusing on a guess about the projects
/// a global item will land in would refuse honest writes. That trade-off stays.
/// But it left the writer blind to a measured case (field test 12): a rule was
/// revised to global at an ssh command anchor, the gate accepted it, and in the
/// one project of the incident the anchor already held four heavier facts, so
/// the rule was outranked there and nothing said so. This asks the question per
/// registered project (a project with a registered working copy) with the item
/// standing in that project's pool, counted exactly as the gate counts a place
/// (`pool_rivals` and `busiest_narrower_call`, full at `item::MAX_ITEMS`).
///
/// Only a binding the previous version did not already carry as a global item
/// is asked about, as `full_reliable_anchor` does; a Moment or an Always binding
/// is never a countable place.
pub fn global_write_crowded_in_projects(store: &EventStore, item: &Item, previous: Option<&Item>) -> Option<String> {
    if item.project.is_some() || !item.kind.can_fire() {
        return None;
    }
    let previously_global: &[Binding] = match previous {
        Some(prev) if prev.project.is_none() => prev.bindings.as_slice(),
        _ => &[],
    };
    let bindings: Vec<&Binding> = item
        .bindings
        .iter()
        .filter(|b| {
            matches!(
                b,
                Binding::Target { kind: crate::item::TargetKind::Path | crate::item::TargetKind::Dir | crate::item::TargetKind::Command, .. }
            ) && !previously_global.contains(b)
        })
        .collect();
    if bindings.is_empty() {
        return None;
    }
    let candidates =
        if store.heads_projection_current() { live_items_from_projection(store) } else { live_items_from_fold(store) }.ok()?;
    let projects: std::collections::BTreeSet<&str> = candidates
        .iter()
        .filter_map(|(_, other)| other.project.as_deref())
        .filter(|project| registered_root_raw(store, project).is_some())
        .collect();
    let mut lines: Vec<String> = Vec::new();
    for project in projects {
        let mut as_project = item.clone();
        as_project.project = Some(project.to_string());
        for binding in &bindings {
            let mut rivals = pool_rivals(&candidates, &as_project, binding);
            let busiest = busiest_narrower_call(&candidates, &as_project, binding);
            if busiest.len() > rivals.len() {
                rivals = busiest;
            }
            if rivals.len() >= crate::item::MAX_ITEMS {
                lines.push(format!("{} in project '{project}' ({} occupant(s))", describe(binding), rivals.len()));
            }
        }
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "this is a GLOBAL item, and the write gate compares a global write only with other globals. \
         Counted per registered project, it lands on a place that is already full: {}. A block shows {} \
         at most, so there it may be outranked and not shown. Nothing was refused. To make room, fold \
         it into an occupant that already carries the same ground, move one occupant to the narrower \
         file or command it is really about, or retract one that is stale, with a reason. \
         `serve why --command <anchor>` or `--file <anchor>` from inside that project shows the real block.",
        lines.join("; "),
        crate::item::MAX_ITEMS
    ))
}

/// One fact on a full place, as the agent needs it to choose which gives way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occupant {
    pub id: String,
    pub text: String,
    pub severity: Option<crate::item::Severity>,
    pub check: Option<Check>,
    /// On the rule track: its check can refuse a wrong change.
    pub refusing: bool,
}

impl Occupant {
    /// "'id' (costly, a check that can refuse: contains 'x' in f): first words".
    pub fn line(&self) -> String {
        let check = match (&self.check, self.refusing) {
            (None, _) => "no check".to_string(),
            (Some(check), true) => format!("a check that can refuse: {}", check_named(check)),
            (Some(check), false) => format!("a check not on the rule track: {}", check_named(check)),
        };
        let one_line = self.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let one_line = one_line.trim_end_matches('.');
        let head: String = one_line.chars().take(70).collect();
        let head = if one_line.chars().count() > 70 { format!("{head}...") } else { head };
        format!("'{}' ({}, {check}): {}", self.id, severity_word(self.severity), plain(&head))
    }
}

/// A full place a rule's check needs as its anchor (step 26): every fact on
/// it, and whether THOR sees an honest exit - a fact there that is not
/// heavier than the rule, or carries no check that can refuse. With no such
/// fact, nothing honestly gives way and the place stays THOR's own point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullPlace {
    pub path: String,
    pub occupants: Vec<Occupant>,
    pub honest_exit: bool,
}

impl FullPlace {
    /// The place `binding` names for `item`, when it is full - `None` while it
    /// has room.
    pub fn of(store: &EventStore, item: &Item, path: &str) -> Option<FullPlace> {
        let anchor = Binding::Target { kind: crate::item::TargetKind::Path, value: path.to_string() };
        let rank = crate::item::severity_rank(item.severity);
        let occupants: Vec<Occupant> = full_place_items(store, item, &anchor)?
            .into_iter()
            .map(|(id, other)| {
                // The trial's own verdict, never `settlement`: settlement reads
                // the full-place blocker, which reads this - a loop.
                let refusing = matches!(current_verdict(store, &id), CurrentVerdict::RuleProven | CurrentVerdict::RuleWired);
                Occupant { text: other.text.clone(), severity: other.severity, check: other.check.clone(), refusing, id }
            })
            .collect();
        let honest_exit = occupants.iter().any(|o| !o.refusing || crate::item::severity_rank(o.severity) >= rank);
        Some(FullPlace { path: path.to_string(), occupants, honest_exit })
    }

    /// The agent's line for rule `rule_id`: every occupant, the three honest
    /// exits, and the binding to add once there is room.
    pub fn agent_line(&self, rule_id: &str) -> String {
        let path = &self.path;
        format!(
            "{path} is already full ({} live facts, the most a place ever shows), so this rule cannot be anchored there \
             yet. Making room is yours, by THOR's rules - never the owner's. On it now: {}. The honest exits: (1) FOLD - two \
             that say almost the same: revise one to carry the other's point, then retract the other with a reason naming \
             the survivor; (2) MOVE - one that is really about a narrower file or command: revise its bindings to that \
             place; (3) RETRACT - one that is stale: retract it with a reason saying what changed. A fact with no check gives \
             way before a rule whose check can refuse; never clear this rule's own true check to get past this. Then bind \
             this rule there: mcp__thor__revise(id: \"{rule_id}\", bindings: <its bindings plus {{\"target\": {{\"kind\": \
             \"path\", \"value\": \"{path}\"}}}}>)",
            self.occupants.len(),
            self.occupants.iter().map(Occupant::line).collect::<Vec<_>>().join("; ")
        )
    }
}

/// THE OWNER'S GO (step 26). A rule carrying this tag on a `requires` check
/// is satisfied only by the OWNER's own words: its required literal must stand
/// in his own message in this session - a real user turn of the transcript,
/// this turn or the one before - never in the call itself, a tool result, a
/// subagent's hand-back or quoted text (`serve::guard_verdict`). Field case
/// 2026-10-01: two guards whose point is to stop an agent were satisfied by a
/// marker any agent could type, and the A4 naming rule had put the marker in
/// their served text. Such a marker is exempt from A4
/// (`gate::unnamed_literals`): the text says it needs the owner's explicit go.
pub const OWNER_APPROVAL_TAG: &str = "owner-approval";

/// The shape of an approval marker a `requires` check was satisfied by before
/// the owner's go existed: `go-` and a name, one plain token
/// (`migrate_owner_approval`).
pub const APPROVAL_MARKER_PREFIX: &str = "go-";

/// Whether `item` is a rule whose `requires` answer only the owner gives.
pub fn is_owner_approval(item: &Item) -> bool {
    matches!(item.check, Some(Check::Requires { .. })) && item.tags.iter().any(|t| t == OWNER_APPROVAL_TAG)
}

/// Whether `literal` has the shape of an approval marker: `go-` and at least
/// one more letter or digit, lowercase letters, digits and hyphens only.
pub fn is_approval_marker(literal: &str) -> bool {
    literal
        .strip_prefix(APPROVAL_MARKER_PREFIX)
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
}

/// THE ONE-TIME MIGRATION TO THE OWNER'S GO (step 26): every live rule or
/// orientation whose `requires` check is answered only by approval markers
/// (`is_approval_marker`: "go-<name>") gets `OWNER_APPROVAL_TAG`, its trigger,
/// check and text untouched - from then on its marker counts only in the
/// owner's own message. Written by THOR (`installer`), one revise per rule.
/// Idempotent, and never twice: a rule that ever carried the tag (the owner
/// may have taken it off since, with a because) is left alone. Returns the ids
/// it marked; a rule its write is refused for is skipped.
pub fn migrate_owner_approval(store: &mut EventStore, session: &str) -> Vec<String> {
    let candidates = if store.heads_projection_current() { live_items_from_projection(store) } else { live_items_from_fold(store) };
    let Ok(candidates) = candidates else { return Vec::new() };
    let mut marked = Vec::new();
    for (id, item) in candidates {
        let Some(Check::Requires { required, .. }) = &item.check else { continue };
        if !item.kind.can_fire() || required.is_empty() || !required.iter().all(|r| is_approval_marker(r)) {
            continue;
        }
        if item.tags.iter().any(|t| t == OWNER_APPROVAL_TAG) {
            continue;
        }
        let ever = store.get_events_by_entity(&id).unwrap_or_default().iter().any(|e| {
            parse_body(&e.body).is_ok_and(|earlier| earlier.tags.iter().any(|t| t == OWNER_APPROVAL_TAG))
        });
        if ever {
            continue;
        }
        let mut updated = item.clone();
        updated.tags.push(OWNER_APPROVAL_TAG.to_string());
        if revise(store, session, session, SystemActor::new(INSTALLER_ACTOR), &item, &updated).is_ok() {
            marked.push(id);
        }
    }
    marked
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
/// fires), or there is genuinely nothing that could ever refuse anything
/// here (no check at all, or `path_exists`, which blocks nothing on its own
/// - see `serve::absent_guard::location_anchor`'s own doc comment for what
/// actually turns IT into a block). NEVER used any more for a check that
/// simply could not be TRIED here - see `TEETH_NOT_TRIED_TAG` for that,
/// added 2026-09-27 for exactly the shape this tag used to swallow.
pub const TEETH_INFORMING_TAG: &str = "teeth:informing";

/// THE LABEL THIS CLOSES (2026-09-27). `TEETH_INFORMING_TAG` used to also
/// cover a check that reads a file - `contains`, or an anchored
/// `absent`/`absent_all` with no working-copy root to try it against - and
/// the write receipt read "informing only - nothing to try: this check
/// reads a file, so it can only be proven against a real working copy" for
/// it, with the Stop hook then demanding "its check cannot refuse anything
/// as it stands". Measured: four rules labelled exactly that way all refused
/// a real edit that removed their literal, and let one through that left it,
/// once actually tried through the real hook in a real working copy. The
/// label and the demand were both false, and both sent a session to repair a
/// guard that already worked.
///
/// So a check this module's own write-time proof genuinely could not TRY -
/// `contains`, `path_exists` is excluded (see `TEETH_INFORMING_TAG`, it
/// truly refuses nothing), or an `absent`/`absent_all`/`forbidden`/
/// `requires` check the proof could not reach here (no root resolved, a
/// missing anchor) - gets this tag instead: NOT TRIED, never `TEETH_
/// INFORMING_TAG`. The distinction `mcp::teeth_test` makes it on: a refusing
/// kind (`gate::is_refusing_check`) or `contains`, whose synthesised attempt
/// never ran at all (`serve::prove::TeethBox::Informing`'s own `tried: None`)
/// - a check that WAS tried and came back silent (`tried: Some(_)`) is a
/// real, working test that found nothing, and stays `TEETH_INFORMING_TAG`.
///
/// RESERVED FROM A WRITER exactly like every other `TEETH_TAG_PREFIX` tag -
/// see that constant's own doc comment; nothing new is needed to protect it,
/// the existing prefix check already covers it.
pub const TEETH_NOT_TRIED_TAG: &str = "teeth:not-tried";

/// WIRED, NOT PROVEN (2026-09-26): both synthesised attempts held - the
/// forbidden one refused, the clean one let through - but the log holds no
/// real call the check ever met. That is reach, and reach is all a writer
/// can show at the moment of writing: the attempt is built from the writer's
/// own literal, so an invented literal nobody will ever type comes back
/// exactly as wired as a real one. Only `TEETH_PROVEN_TAG` means a real tooth
/// (see `serve::prove`'s "against reality" section for what counts).
pub const TEETH_WIRED_TAG: &str = "teeth:wired-not-proven";

/// THE NOTE TRACK: the rule's trial found no tooth that holds, so it stands
/// as a note - it still surfaces at its moment and in search, keeps its
/// severity, and claims no guard. A note carries no check; a check it dropped
/// or keeps pending lives only in its trial record (`TrialFields`). Written
/// only by THOR's own trial, never by a writer, like every
/// `TEETH_TAG_PREFIX` tag. Nothing writes it yet: the writes start carrying
/// trial records in one later step, all at once.
pub const TEETH_NOTE_TAG: &str = "teeth:note";

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
// eval_debt_owed`, then `newest_evaluation_report`) by merely EXISTING: a live
// `Report` scoped to the project and carrying `EVALUATION_REPORT_TAG`.
// Nobody read it, and nothing checked the log for the work the evaluation
// routine actually demands, so a session could discharge the
// whole obligation with prose. `check_evaluation_report_filing` below is the
// machine-checked replacement, called from `declare` - every condition is
// computed from the EVENT LOG, never from what the report's own text claims,
// and the report's prose stays completely unchecked; it no longer discharges
// anything by existing.
//
// Two conditions, both per-project: the evaluation's frozen judgement list
// is settled, and every rule THOR's machine pass handed to the agent at the
// start - the frozen batch (at most `EVALUATION_TEETH_BATCH`) and the frozen
// A4 list (at most `EVALUATION_A4_LIST`) - is settled (`settlement`, read for
// that evaluation). The rules beyond those lists, and the ones the pass did
// not reach, are not this filing's: doctor names them, and the next
// evaluation starts with them. OPEN POINTS HOLD NO FILING (step 14): THOR
// writes and closes its own, and a legacy one an agent filed settles nothing
// and holds nothing - doctor lists it for the owner.
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

/// How many SESSIONS a live item must have fired in since its own last verdict
/// before it counts as judgement debt (once per session, step 28) - mirrors
/// `serve::usefulness::JUDGEMENT_DEBT_AFTER` exactly (kept equal by hand:
/// `model` cannot import it, for the same reason every other constant in this
/// section is reimplemented rather than shared).
pub const JUDGEMENT_DEBT_AFTER: usize = 40;

/// The actor a real delivery is stamped with - mirrors `serve::usefulness::
/// ASSISTANT_DELIVERY_ACTOR` exactly, same reason.
pub const ASSISTANT_DELIVERY_ACTOR: &str = "hook";

// ------------------------------------------- field evidence, recorded once (X3)
//
// Whether a refusal of the real hook was a REAL refusal of a real tool call -
// its session's own transcript holds that call and the error the block left -
// is decided once, by `serve::attest`, and recorded here as one
// `gate_stood_aside` event by actor `attester` per refusal (entity = the rule,
// body `{refusal_seq, field, why}`). Transcripts are deleted after about 30
// days (N8), so readers use only the recorded verdict: a refusal with no
// recorded verdict is never proof, however real it was.

/// The actor a refusal's verification is recorded under.
pub const ATTESTER_ACTOR: &str = "attester";

/// The actor THOR's own trial stamps a refused write with (`GateRefused`,
/// body `{id, code, forcing, text_sha}`) - telemetry, never field evidence.
pub const TEETH_WRITE_ACTOR: &str = "teeth-write";

/// Record the verification of the refusal at log seq `refusal_seq`, once.
/// Only THOR's attester writes this (`actor` must be `ATTESTER_ACTOR`, held as
/// a `SystemActor`); a second verification of the same refusal appends
/// nothing and returns `Ok(None)` - the first recorded verdict is the one
/// every reader uses.
pub fn record_refusal_verification(
    store: &mut EventStore,
    session_id: &str,
    actor: SystemActor,
    rule_id: &str,
    refusal_seq: i64,
    field: bool,
    why: &str,
) -> Result<Option<Event>, WriteError> {
    if actor.name() != ATTESTER_ACTOR {
        return Err(WriteError::Refused(Refusal {
            problem: format!("a refusal's verification is recorded by THOR's attester only, not by '{}'", actor.name()),
            fix: "let the Stop hook record it (serve::attest::record_refusal_verifications)".to_string(),
        }));
    }
    let name = fence(store, actor.into(), rule_id)?;
    let events = store.get_events_by_entity(rule_id).map_err(|e| WriteError::Store(e.into()))?;
    if recorded_refusal_verdicts(&events).contains_key(&refusal_seq) {
        return Ok(None);
    }
    let body = serde_json::json!({ "refusal_seq": refusal_seq, "field": field, "why": why }).to_string();
    store
        .append_event(session_id, session_id, name, EventKind::GateStoodAside, rule_id, None, &body)
        .map(Some)
        .map_err(|e| WriteError::Store(e.into()))
}

/// REFUSED-WRITE TELEMETRY (step 8, K5): a write THOR's trial refused, as one
/// `GateRefused` by `TEETH_WRITE_ACTOR` on the rule's own id - body `{id,
/// code, forcing, text_sha}`. No new event kind (an older binary still reads
/// the log), head-neutral (a `GateRefused` never writes the item), never field
/// evidence (the actor is not the hook), and the trial reads it back for
/// K8(c): a real call made after an id's first refused write never stops a
/// derived tooth from forcing. `text_sha` is a hash of the refused text, so
/// doctor can say "refused, then stored as a note that no longer names it"
/// without keeping the text itself.
pub fn record_refused_write(
    store: &mut EventStore,
    session_id: &str,
    actor: SystemActor,
    id: &str,
    code: &str,
    forcing: &[String],
    text: &str,
) -> Result<Event, WriteError> {
    if actor.name() != TEETH_WRITE_ACTOR {
        return Err(WriteError::Refused(Refusal {
            problem: format!("a refused write is recorded by THOR's trial only, not by '{}'", actor.name()),
            fix: "let the write tool record it (serve::teeth::record_refused_write)".to_string(),
        }));
    }
    let name = fence(store, actor.into(), id)?;
    let text_sha = thor_core::event_store::hash_event("thor-teeth-refused-text-v1", text);
    let body = serde_json::json!({ "id": id, "code": code, "forcing": forcing, "text_sha": text_sha }).to_string();
    store
        .append_event(session_id, session_id, name, EventKind::GateRefused, id, None, &body)
        .map_err(|e| WriteError::Store(e.into()))
}

/// Every recorded refusal verification in `events`: refusal seq -> (field,
/// why). The first verdict recorded for a refusal is the one that counts; a
/// later one never overrides it. Only events by `ATTESTER_ACTOR` are read.
pub fn recorded_refusal_verdicts(events: &[Event]) -> HashMap<i64, (bool, String)> {
    let mut out: HashMap<i64, (bool, String)> = HashMap::new();
    for e in events.iter().filter(|e| e.kind == EventKind::GateStoodAside && e.actor == ATTESTER_ACTOR) {
        let Ok(body) = serde_json::from_str::<serde_json::Value>(&e.body) else { continue };
        let (Some(seq), Some(field)) = (body.get("refusal_seq").and_then(|v| v.as_i64()), body.get("field").and_then(|v| v.as_bool()))
        else {
            continue;
        };
        let why = body.get("why").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        out.entry(seq).or_insert((field, why));
    }
    out
}

/// The seqs of every refusal in `events` whose recorded verification says it
/// was field evidence - the verified set a model-level reader receives, since
/// `model` cannot run `serve`'s transcript check itself. A refusal only ever
/// sets the "proven" label of a trial; it settles nothing by itself.
pub fn attested_field_refusals(events: &[Event]) -> HashSet<i64> {
    recorded_refusal_verdicts(events).into_iter().filter(|(_, (field, _))| *field).map(|(seq, _)| seq).collect()
}

// ------------------------------------------ session identity for every write (step 12)
//
// Which Claude Code session - and which tool call in it - made a write. The
// tool server stamps every write it makes with `Att`, spliced into the event
// body as the trailing key `"att"` (after `because` and `teeth_trial`, by the
// same byte splice). It is a CLAIM, made while the call runs: the server can
// only refuse on positive evidence of a side door, never wait for a line to
// reach the disk (K21). Whether the claim holds is decided once, at a Stop of
// the session, against its whole transcript (`serve::attest::verify_write`),
// and recorded here as one `gate_stood_aside` by actor `attester` per write
// (entity = the item, body `{event_seq, attested, session, agent_id, why}`).
// Every reader of attestation reads that record, never a live transcript.

/// `inflight` value: the call's tool_use line was already in the session's
/// transcript while the call ran, under the same tool and arguments.
pub const INFLIGHT_SEEN: &str = "seen";

/// `inflight` value: the tool_use line was not on disk yet (or no transcript
/// of the session was) - never a refusal; the Stop decides.
pub const INFLIGHT_UNSEEN: &str = "unseen";

/// `inflight` value: positive evidence of a side door (`Att::side_door` says
/// which). Recorded, refuses nothing by itself.
pub const INFLIGHT_SIDE_DOOR: &str = "side-door";

/// The stamp a write through the tool server carries (see the section above).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Att {
    /// `claudecode/toolUseId` from the call's `_meta`, when it carried one.
    pub tool_use_id: Option<String>,
    /// `args_digest` of the call's arguments exactly as they arrived.
    pub args_sha256: String,
    /// `CLAUDE_CODE_SESSION_ID` of the server, fixed when it started: a hint.
    pub session_hint: Option<String>,
    /// `INFLIGHT_SEEN`, `INFLIGHT_UNSEEN` or `INFLIGHT_SIDE_DOOR`. The tool
    /// server always sets it (a replica's queued write replayed at the drain
    /// carries no `_meta`, so it reads `INFLIGHT_SIDE_DOOR`); `None` only in a
    /// body some other program wrote.
    pub inflight: Option<String>,
    /// Why the call is a side door, with `INFLIGHT_SIDE_DOOR`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_door: Option<String>,
}

/// The digest of a tool call's arguments: `hash_event("", <canonical JSON>)`,
/// where the canonical JSON is `serde_json`'s own serialisation of the value
/// (keys sorted: this workspace does not enable `preserve_order`). Measured
/// equal for the arguments the tool server receives and the input the
/// transcript's tool_use line holds (step 1 of the build, answer 2).
pub fn args_digest(args: &serde_json::Value) -> String {
    thor_core::event_store::hash_event("", &serde_json::to_string(args).unwrap_or_default())
}

/// `body` with one more trailing key, `"att":{...}`, spliced on exactly the
/// way `body_with_because` splices `because`. `att_of` is its one reader.
fn body_with_att(body: &str, att: &Att) -> Result<String, serde_json::Error> {
    let att = serde_json::to_string(att)?;
    debug_assert!(body.ends_with('}'), "every event body written here is a JSON object");
    Ok(format!("{},\"att\":{att}}}", &body[..body.len() - 1]))
}

/// `body`, stamped with `att` when there is one - the one splice every writer
/// of a stamped event uses (`serve::mark` included).
pub fn with_att(body: String, att: Option<&Att>) -> Result<String, serde_json::Error> {
    match att {
        Some(att) => body_with_att(&body, att),
        None => Ok(body),
    }
}

/// The stamp an event body carries, if any.
pub fn att_of(body: &str) -> Option<Att> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    serde_json::from_value(value.get("att")?.clone()).ok()
}

/// The event kinds a write tool of the tool server appends: the writes a
/// session's Stop verifies.
pub const ATTESTED_WRITE_KINDS: &[EventKind] = &[
    EventKind::FactCreated,
    EventKind::FactRevised,
    EventKind::FactRetracted,
    EventKind::FactResolved,
    EventKind::ItemMarkedUseful,
    EventKind::ItemMarkedNoise,
];

/// One recorded verification of a write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteVerdict {
    pub attested: bool,
    pub session: String,
    pub agent_id: Option<String>,
    pub why: String,
}

/// Record the verification of the write at log seq `event_seq` (on item
/// `entity_id`), once. Only THOR's attester writes this (`actor` must be
/// `ATTESTER_ACTOR`); a second verification of the same write appends nothing
/// and returns `Ok(None)` - the first recorded verdict is the one every
/// reader uses.
#[allow(clippy::too_many_arguments)]
pub fn record_write_verification(
    store: &mut EventStore,
    session_id: &str,
    actor: SystemActor,
    entity_id: &str,
    event_seq: i64,
    attested: bool,
    agent_id: Option<&str>,
    why: &str,
) -> Result<Option<Event>, WriteError> {
    record_write_verification_by(store, session_id, session_id, actor, entity_id, event_seq, attested, agent_id, why)
}

/// `record_write_verification`, recorded at the Stop of ANOTHER session
/// (`recorder`, step 24): the verdict names the write's own session in its
/// body, while the event itself is stamped with the recording session - so a
/// session that checks another's writes never makes that one look active.
#[allow(clippy::too_many_arguments)]
pub fn record_write_verification_by(
    store: &mut EventStore,
    recorder: &str,
    session_id: &str,
    actor: SystemActor,
    entity_id: &str,
    event_seq: i64,
    attested: bool,
    agent_id: Option<&str>,
    why: &str,
) -> Result<Option<Event>, WriteError> {
    if actor.name() != ATTESTER_ACTOR {
        return Err(WriteError::Refused(Refusal {
            problem: format!("a write's verification is recorded by THOR's attester only, not by '{}'", actor.name()),
            fix: "let the Stop hook record it (serve::attest::record_write_verifications)".to_string(),
        }));
    }
    let events = store.get_events_by_entity(entity_id).map_err(|e| WriteError::Store(e.into()))?;
    if recorded_write_verdicts(&events).contains_key(&event_seq) {
        return Ok(None);
    }
    let name = fence(store, actor.into(), entity_id)?;
    let body = serde_json::json!({
        "event_seq": event_seq,
        "attested": attested,
        "session": session_id,
        "agent_id": agent_id,
        "why": why,
    })
    .to_string();
    store
        .append_event(recorder, recorder, name, EventKind::GateStoodAside, entity_id, None, &body)
        .map(Some)
        .map_err(|e| WriteError::Store(e.into()))
}

/// Every recorded write verification in `events`: write seq -> verdict. The
/// first one recorded for a write counts; only events by `ATTESTER_ACTOR`
/// are read.
pub fn recorded_write_verdicts(events: &[Event]) -> HashMap<i64, WriteVerdict> {
    let mut out: HashMap<i64, WriteVerdict> = HashMap::new();
    for e in events.iter().filter(|e| e.kind == EventKind::GateStoodAside && e.actor == ATTESTER_ACTOR) {
        let Ok(body) = serde_json::from_str::<serde_json::Value>(&e.body) else { continue };
        let (Some(seq), Some(attested)) =
            (body.get("event_seq").and_then(|v| v.as_i64()), body.get("attested").and_then(|v| v.as_bool()))
        else {
            continue;
        };
        let text = |key: &str| body.get(key).and_then(|v| v.as_str()).map(str::to_string);
        out.entry(seq).or_insert(WriteVerdict {
            attested,
            session: text("session").unwrap_or_default(),
            agent_id: text("agent_id"),
            why: text("why").unwrap_or_default(),
        });
    }
    out
}

/// The seqs of every write whose recorded verification says it was made
/// through an attested THOR call - the one set every judgement-debt reader
/// resets on (D9). Fails open to an empty set on an unreadable log: then no
/// verdict clears anything, which asks more, never less.
pub fn attested_write_seqs(store: &EventStore) -> HashSet<i64> {
    let Ok(events) = store.events_of_kinds(&[EventKind::GateStoodAside], 0) else { return HashSet::new() };
    recorded_write_verdicts(&events).into_iter().filter(|(_, v)| v.attested).map(|(seq, _)| seq).collect()
}

/// An in-memory store holding exactly `item`, written as a raw `fact_created`
/// with no gate at all: THOR's trial runs the hook's own pipeline against a
/// store like this one, so it tries the item exactly as it would be written,
/// never as the gate would have shaped it. It is a store of its own, in
/// memory: nothing here can reach any store on disk.
pub fn one_item_store(item: &Item) -> anyhow::Result<EventStore> {
    one_item_store_among(item, &[])
}

/// `one_item_store`, in a world that also KNOWS the projects named in
/// `others` (step 29). The hook sends a command run in the folder of another
/// KNOWN project (`cd ../other && git push`) to that project's rules, so a
/// rule of this project does not apply there; a store that holds one rule
/// knows no other project, and a trial in it refused calls the live hook lets
/// through (field 2026-10-02: 57 pushes made in two other repositories counted
/// as refused by a rule of this project). Each other project is made known by
/// one inert Report - it can never fire, it only makes the project known.
pub fn one_item_store_among(item: &Item, others: &[String]) -> anyhow::Result<EventStore> {
    let mut store = EventStore::in_memory()?;
    let body = canonical_body(item)?;
    store.append_event("trial", "trial", "trial", EventKind::FactCreated, &item.id, None, &body)?;
    for (n, project) in others.iter().enumerate() {
        let known = Item {
            id: format!("trial-known-project-{n}"),
            kind: Kind::Report,
            text: "a project this memory knows".to_string(),
            bindings: vec![],
            severity: None,
            project: Some(project.clone()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        store.append_event("trial", "trial", "trial", EventKind::FactCreated, &known.id, None, &canonical_body(&known)?)?;
    }
    Ok(store)
}

/// The actor of the retired `serve probe-teeth` subcommand (step 9 replaced it
/// with `serve retest`). Kept as a RESERVED name only, so nothing can ever
/// write as it again; a probe event on the log settles nothing and is never
/// field evidence - doctor counts it apart.
pub const TEETH_PROBE_ACTOR: &str = "teeth-probe";

/// An open point: a small, project-scoped `Report` tagged with this, naming
/// something that holds rules undecided. Never served at a gate (it is a
/// `Report`, archive-kind, exactly like an evaluation report itself), found by
/// `lookup` and handed to a session directly by the hook's SessionStart note
/// and the evaluation's start note.
///
/// SINCE STEP 14 THOR WRITES AND CLOSES THEM ITSELF (`write_machine_point`,
/// `retract_machine_point`): one per subject a machine blocker holds
/// (`Blocker`), on a reserved id (`machine_point_id`), by `eval-machine`. A
/// reader accepts a point as THOR's own only when `eval-machine` wrote it
/// (`OpenPoint::machine`). Every other open point is LEGACY - filed by an
/// agent before this build: it settles nothing and holds no filing, and
/// doctor lists it for the owner.
pub const OPEN_POINT_TAG: &str = "open-point";

/// Prefix of the tag naming what a machine open point is about: the project
/// for `Blocker::NoWorkingCopy`, the file for `Blocker::WorkingTreeDiffers`.
pub const OPEN_POINT_ABOUT_PREFIX: &str = "about:";

/// Prefix of the tag naming the blocker a machine open point holds rules by
/// (`Blocker::code`).
pub const OPEN_POINT_BLOCKER_PREFIX: &str = "blocker:";

/// Whether `tag` is one only THOR writes on an open point: the open-point tag
/// itself, or an `about:`/`blocker:` tag.
pub fn is_open_point_tag(tag: &str) -> bool {
    tag == OPEN_POINT_TAG || tag.starts_with(OPEN_POINT_ABOUT_PREFIX) || tag.starts_with(OPEN_POINT_BLOCKER_PREFIX)
}

/// What a write tool answers a caller who tries to file or tag an open point.
pub const OPEN_POINT_REFUSAL: &str =
    "REFUSED: open points are written and closed by THOR itself; settle the rule instead - the start note says how";

/// What a write tool answers a caller who tries to retract THOR's own point.
pub const MACHINE_POINT_REFUSAL: &str = "REFUSED: this open point is THOR's own; it closes itself when its subject settles";

const MACHINE_POINT_PREFIXES: &[&str] = &["open-no-working-copy-", "open-working-tree-differs-", "open-anchor-full-"];

/// Whether `id` is shaped like one of THOR's own open points - an id only
/// `eval-machine` may write (`reserved_id_owner`).
pub fn is_machine_point_id(id: &str) -> bool {
    MACHINE_POINT_PREFIXES.iter().any(|prefix| id.starts_with(prefix))
}

/// Lower-case ASCII letters, digits and single hyphens - a plain, greppable
/// piece of an id. Empty input gives `fallback`.
fn slug(text: &str, fallback: &str) -> String {
    let mut slug = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() { fallback.to_string() } else { slug.to_string() }
}

/// The id of THOR's open point for `blocker` in `project`: one per subject.
/// `open-no-working-copy-<project>` for a project with no usable registered
/// working copy; `open-working-tree-differs-<project>-<file>` for a file whose
/// registered working tree lacks a literal the default branch still has; and
/// `GLOBAL_RULES_POINT_ID` - whatever `project` is evaluated - for the rules
/// with no project (step 21: one store-wide point).
pub fn machine_point_id(project: &str, blocker: &Blocker) -> String {
    match blocker {
        Blocker::NoWorkingCopy { .. } if blocker.is_global_rules() => GLOBAL_RULES_POINT_ID.to_string(),
        Blocker::NoWorkingCopy { project } => format!("open-no-working-copy-{}", slug(project, "project")),
        Blocker::WorkingTreeDiffers { path } => {
            format!("open-working-tree-differs-{}-{}", slug(project, "project"), slug(path, "file"))
        }
        Blocker::AnchorFull { path } => format!("open-anchor-full-{}-{}", slug(project, "project"), slug(path, "file")),
    }
}

/// The tags THOR's open point for `blocker` carries: the open-point tag, what
/// it is about, and the blocker's code.
pub fn machine_point_tags(blocker: &Blocker) -> Vec<String> {
    vec![
        OPEN_POINT_TAG.to_string(),
        format!("{OPEN_POINT_ABOUT_PREFIX}{}", blocker.subject()),
        format!("{OPEN_POINT_BLOCKER_PREFIX}{}", blocker.code()),
    ]
}

/// Write THOR's open point for `blocker` in `project`, with `text`: created
/// when its id holds nothing yet, revised when its text changed, re-opened
/// when the machine closed it earlier and the blocker is back, and left alone
/// (`Ok(None)`) when it already says exactly this. One event at most, straight
/// onto the log - a machine point is THOR's own record, not a claim for the
/// write gate to judge. Only `eval-machine` writes one (`fence`).
///
/// RE-OPENING A CLOSED POINT writes on the same id, as a revise whose parent
/// is the tombstone. The store refuses that for every other writer (a
/// retraction is a decision, not a pause); for THOR's own point, open and
/// closed are the two states one record moves between, and its history then
/// shows every time the blocker held and lifted.
pub fn write_machine_point(
    store: &mut EventStore,
    actor: SystemActor,
    session: &str,
    project: &str,
    blocker: &Blocker,
    text: &str,
) -> Result<Option<Event>, WriteError> {
    if actor.name() != EVAL_MACHINE_ACTOR {
        return Err(WriteError::Refused(Refusal {
            problem: format!("an open point is written by THOR's eval-machine only, not by '{}'", actor.name()),
            fix: "let the evaluation start write it (serve::eval_session::machine_pass)".to_string(),
        }));
    }
    let id = machine_point_id(project, blocker);
    let name = fence(store, actor.into(), &id)?;
    // The store-wide point of the rules with no project belongs to no
    // project: no project's owner budget counts it (step 21).
    let scope = (!blocker.is_global_rules()).then(|| project.to_string());
    let item = Item {
        id: id.clone(),
        kind: Kind::Report,
        text: text.to_string(),
        bindings: vec![],
        severity: None,
        project: scope,
        tags: machine_point_tags(blocker),
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    let body = canonical_body(&normalized(&item)).map_err(WriteError::Serialize)?;
    let events = store.get_events_by_entity(&id).map_err(|e| WriteError::Store(e.into()))?;
    if events.is_empty() {
        return store.append_created_unique(session, session, name, &id, &body, |_, _, _| false).map(Some).map_err(WriteError::Store);
    }
    let heads = thor_core::cas::compute_head_sets(&events);
    let head = heads
        .get(&id)
        .filter(|set| set.heads.len() == 1)
        .and_then(|set| set.heads.iter().next())
        .and_then(|hash| events.iter().find(|e| &e.this_hash == hash));
    let Some(head) = head else {
        return Err(WriteError::Store(anyhow::anyhow!("'{id}' has more than one head")));
    };
    if head.kind == EventKind::FactRetracted {
        return store
            .append_event(session, session, name, EventKind::FactRevised, &id, Some(&head.this_hash), &body)
            .map(Some)
            .map_err(WriteError::Store);
    }
    if head.body == body {
        return Ok(None);
    }
    store
        .append_mutate_checked(session, session, name, EventKind::FactRevised, &id, Some(&head.this_hash), &body)
        .map(Some)
        .map_err(WriteError::Store)
}

/// Close THOR's open point `id` with a derived `reason` - its subject settled.
/// Only `eval-machine` (`fence`); `Ok(None)` when the point is not live.
pub fn retract_machine_point(
    store: &mut EventStore,
    actor: SystemActor,
    session: &str,
    id: &str,
    reason: &str,
) -> Result<Option<Event>, WriteError> {
    if actor.name() != EVAL_MACHINE_ACTOR || !is_machine_point_id(id) {
        return Err(WriteError::Refused(Refusal {
            problem: format!("'{id}' is not an open point THOR's eval-machine may close as '{}'", actor.name()),
            fix: "let the evaluation start close it (serve::eval_session::machine_pass)".to_string(),
        }));
    }
    let name = fence(store, actor.into(), id)?;
    if show(store, id).is_err() {
        return Ok(None);
    }
    let body = serde_json::json!({ "retracted": id, "reason": reason }).to_string();
    store
        .append_mutate_checked(session, session, name, EventKind::FactRetracted, id, None, &body)
        .map(Some)
        .map_err(WriteError::Store)
}

/// Prefix for the tag naming the evaluation report an open point was raised
/// in, e.g. `from-report:eval-acme-2026-01-01`. Informational only - nothing
/// in this file matches on it - but it is what lets a reader trace an open
/// point back to the evaluation that found it.
pub const OPEN_POINT_SOURCE_PREFIX: &str = "from-report:";

/// Prefix for the tag recording how many times a LEGACY open point has been
/// carried forward, e.g. `carried:1`. `open_point_carried_count` below reads
/// it; `gate::revise`'s own GROUND 30 refuses ever writing a count above 1.
/// THOR's own points carry no such tag: their carry is counted from the log.
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
/// ACTOR`) since its own last ATTESTED verdict (an `ItemMarkedUseful`/
/// `ItemMarkedNoise` resets the count to zero only when its recorded
/// verification says it was made through an attested THOR call,
/// `attested_write_seqs` - D9; from any audience - a verdict is about the
/// item, not the audience). Fails open to an empty map, the same stance
/// `all_live_items` takes and for the same reason.
fn served_since_last_verdict_local(store: &EventStore) -> HashMap<String, usize> {
    let Ok(events) = store.firings_and_verdicts() else { return HashMap::new() };
    let attested = attested_write_seqs(store);
    // Counted once per session (step 28): the sessions served since the last verdict.
    let mut sessions: HashMap<String, HashSet<String>> = HashMap::new();
    for (seq, kind, id, actor, session) in events {
        match kind {
            EventKind::ItemServed if actor == ASSISTANT_DELIVERY_ACTOR => {
                sessions.entry(id).or_default().insert(session);
            }
            EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise if attested.contains(&seq) => {
                sessions.insert(id, HashSet::new());
            }
            _ => {}
        }
    }
    sessions.into_iter().map(|(id, sessions)| (id, sessions.len())).collect()
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

// ------------------------------------------------ the filing context (step 15)
//
// WHY A FILING NEEDS ONE. Until step 15 an evaluation report could be written
// by anything that could reach the store: a second JSON-RPC client, a script,
// the model CLI, an older binary. Measured on a store copy: a side-door client
// filed "eval-fxp-2026-09-28" and the receipt said no verdict was required,
// because no hook record said which session filed it. A report like that can
// never count - the Stop hook (the reader, step 16) only accepts one whose
// filing call is in the evaluating session's own transcript - so the writer
// refuses it as the friendly first line.
//
// WHAT THE WRITER CAN KNOW (K21, F2). While the filing call runs, its own
// tool_result and hook attachment do not exist yet, so the writer never asks
// for them and never waits. It refuses only on POSITIVE evidence of a side
// door (`serve::attest::check_call_in_flight`), and it needs the open
// evaluation start record of the filing session and the report's project,
// read from the log. Everything the gate COUNTS - a verdict on the judgement
// list, a write to a rule on the batch or the A4 list - is judged by its
// RECORDED attestation (step 12): those calls finished earlier, and the
// session's Stop recorded each one. A write of the filing turn has no record
// yet and counts as not yet attested; the refusal says to end the turn once.

/// What a write that files an evaluation report brings. Built only by the
/// tool server's remember and revise (and, from step 16, by the validator
/// that recomputes the filing). A write with none - the model CLI, a replica's
/// drain, a one-off binary, a call that is a side door - is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilingContext {
    /// What the tool server saw of the filing call while it ran:
    /// `INFLIGHT_SEEN` or `INFLIGHT_UNSEEN`. Never a side door.
    pub inflight: String,
    /// The open evaluation start record of the filing session and the
    /// report's project: its frozen lists are what the filing is held to.
    pub start: EvaluationStart,
    /// THOR's own open points as that start's machine pass left them: the
    /// blockers that hold rules of the project, each with its subject and the
    /// rules it holds (`Frozen::machine`). A rule held by one is settled for
    /// this evaluation and holds no filing.
    pub blockers: Vec<MachinePoint>,
}

impl FilingContext {
    /// The context of a filing call the tool server saw as `inflight`
    /// (`Att::inflight`) for the evaluation `start` records. `None` when the
    /// call is a side door: that gives no context, and the filing is refused.
    pub fn new(inflight: &str, start: EvaluationStart) -> Option<Self> {
        if inflight != INFLIGHT_SEEN && inflight != INFLIGHT_UNSEEN {
            return None;
        }
        let blockers = start.frozen.machine.as_ref().map(|m| m.points.clone()).unwrap_or_default();
        Some(FilingContext { inflight: inflight.to_string(), start, blockers })
    }
}

/// A refusal as the one sentence a tool prints: `REFUSED: <problem>. <fix>.`
pub fn refusal_sentence(refusal: &Refusal) -> String {
    format!("REFUSED: {}. {}.", refusal.problem, refusal.fix)
}

/// The refusal of an evaluation report that is not coming through a THOR tool
/// call Claude Code made - `why` says what gave it away. The same sentence
/// for every writer: the tool server on a side door, the model CLI, the store
/// itself for a write with no filing context.
pub fn unattested_filing_refusal(why: &str) -> Refusal {
    Refusal {
        problem: format!(
            "this evaluation report is not coming through a THOR tool call that both Claude Code and THOR's hook saw \
             ({why}), so it could never count - the Stop hook only accepts a report whose filing call is in this \
             session's own transcript. Nothing was written"
        ),
        fix: "File it with the THOR tool itself, not a script, a second client, the model CLI or an older binary: if \
              mcp__thor__remember is not loaded yet, load it with ToolSearch, query \
              \"select:mcp__thor__remember,mcp__thor__revise,mcp__thor__mark\", then call it with the same arguments"
            .to_string(),
    }
}

/// The refusal of an evaluation report for a project the filing session has
/// no evaluation open for. There is then nothing to file: a report is filed
/// for an evaluation THOR started, and is held to the lists that start froze.
pub fn no_open_evaluation_refusal(store: &EventStore, session: &str, project: &str) -> Refusal {
    let newest = evaluation_starts_of_session(store, session).into_iter().filter(|s| s.frozen.project == project).max_by_key(|s| s.seq);
    let detail = match newest {
        None => "THOR's log holds no start record for it".to_string(),
        Some(start) => match &start.frozen.void {
            Some(void) => format!(
                "its last one, '{}', is void: '{}' was changed at log seq {} outside an attested THOR call of it ({})",
                start.id, void.id, void.seq, void.door
            ),
            None if start.frozen.is_superseded() => format!(
                "its last one, '{}', was superseded: {}",
                start.id,
                start.frozen.superseded.as_ref().map(superseded_words).unwrap_or_default()
            ),
            None if own_report_filed(store, &start) => format!("its last one, '{}', already has its report on file", start.id),
            None => format!("its last one, '{}', is older than {EVALUATION_OPEN_HOURS} hours and closed", start.id),
        },
    };
    Refusal {
        problem: format!(
            "this evaluation report cannot be filed: no evaluation of '{project}' is open in this session ({detail}). A \
             report is filed for an evaluation THOR started, and is held to the lists that start froze. Nothing was written"
        ),
        fix: "There is nothing to file now, so file nothing. An evaluation starts when THOR's Stop hook asks for one at \
              the end of a turn (its ask carries the start note) or when the owner types /thor-eval; do what that start \
              note lists, then file the report in that same session"
            .to_string(),
    }
}

/// A write to an item on an evaluation's frozen lists (or to its start
/// record) made outside an attested THOR call of that evaluation (D10): which
/// item, at which log seq, and through which door. Kept on the start record
/// once the evaluation is closed for it (`Frozen::void`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenVoid {
    pub id: String,
    pub seq: i64,
    pub door: String,
}

/// What a filing is told when an item of its evaluation's lists was changed
/// behind it (D10). The evaluation is closed; a fresh one has to start.
pub fn changed_behind_refusal(void: &FrozenVoid) -> Refusal {
    Refusal {
        problem: format!(
            "'{}' was changed at log seq {} outside an attested THOR call of this evaluation ({}). An evaluation whose \
             list was changed behind it cannot count. The next Stop starts a fresh evaluation with a fresh list",
            void.id, void.seq, void.door
        ),
        fix: "Nothing was written, and this evaluation is closed. End this turn: while the evaluation is still due, \
              THOR's Stop hook starts it again and its start note lists what is yours to do - then file the report with \
              the THOR tool itself. Change a listed item only with the THOR tools, inside the evaluating session"
            .to_string(),
    }
}

/// What a side-door write to an item on an open evaluation's frozen lists is
/// told, on the spot (D10): nothing is written, so the evaluation stands.
pub fn frozen_side_door_refusal(id: &str, start: &EvaluationStart, door: &str) -> Refusal {
    Refusal {
        problem: format!(
            "'{id}' is on the frozen lists of an open evaluation of '{}' ('{}', session {}), and this write is coming \
             from outside an attested THOR call ({door}). An evaluation whose list was changed behind it cannot count, \
             so nothing was written",
            start.frozen.project, start.id, start.frozen.session
        ),
        fix: "Make this change with the THOR tool itself, inside Claude Code, in the session that runs the evaluation \
              - not a script or a second client: if the tool is not loaded yet, load it with ToolSearch, query \
              \"select:mcp__thor__remember,mcp__thor__revise,mcp__thor__retract,mcp__thor__mark\". Or make it after \
              that evaluation's report is filed"
            .to_string(),
    }
}

/// What the filing gate tells a session whose own writes of this turn are
/// not verified yet.
pub const TURN_NOT_ENDED: &str =
    "the verdicts you gave in this turn are checked when the turn ends - end the turn once, then file again";

/// How one write stands against an evaluation by `session` - read from the
/// write's own event and its RECORDED verification, never a live transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteDoor {
    /// By one of THOR's own actors (`RESERVED_ACTORS`): the machine pass, a
    /// retest, the attester. Never a side door.
    Machine,
    /// Recorded as an attested THOR call of the evaluating session (made by
    /// its subagent `agent_id`, when one made it).
    Attested { agent_id: Option<String> },
    /// Stamped as a THOR call of the evaluating session and not verified yet:
    /// the session's next Stop records it.
    Pending,
    /// Recorded as an attested THOR call of ANOTHER session (step 24). Never a
    /// side door: it voids nothing, and an item it changed on an evaluation's
    /// lists counts as settled by that session (credited to it in the machine
    /// section). Field case 2026-10-01: two sessions evaluated one project at
    /// once, and the first one's attested edits voided the other's evaluation.
    OtherSession { session: String, agent_id: Option<String> },
    /// Stamped as a THOR call of another session and not verified yet: the
    /// evaluating session's next Stop checks it against that session's
    /// transcript (`serve::attest::record_foreign_write_verifications`).
    OtherSessionPending { session: String },
    /// Outside an attested THOR call of the evaluating session: the door.
    Outside(String),
}

/// `WriteDoor` of `event`, given the recorded verifications of its entity
/// (`recorded_write_verdicts`).
pub fn write_door(event: &Event, verdicts: &HashMap<i64, WriteVerdict>, session: &str) -> WriteDoor {
    if RESERVED_ACTORS.contains(&event.actor.as_str()) {
        return WriteDoor::Machine;
    }
    if let Some(verdict) = verdicts.get(&event.seq) {
        return if verdict.attested && verdict.session == session {
            WriteDoor::Attested { agent_id: verdict.agent_id.clone() }
        } else if verdict.attested {
            WriteDoor::OtherSession { session: verdict.session.clone(), agent_id: verdict.agent_id.clone() }
        } else {
            WriteDoor::Outside(format!("not an attested THOR call: {}", verdict.why))
        };
    }
    let Some(att) = att_of(&event.body) else {
        return WriteDoor::Outside(format!(
            "written by '{}' with no stamp of THOR's tool server - the command line, a repair tool or an older build",
            event.actor
        ));
    };
    if att.inflight.as_deref() == Some(INFLIGHT_SIDE_DOOR) {
        return WriteDoor::Outside(format!("a side door: {}", att.side_door.as_deref().unwrap_or("no reason recorded")));
    }
    if att.tool_use_id.as_deref().is_none_or(str::is_empty) {
        return WriteDoor::Outside("no toolUseId in the call's _meta".to_string());
    }
    if event.session_id != session {
        // A stamp with no session of its own ("mcp" from a tool server with no
        // session hint, "cli") can never be checked against a transcript.
        if matches!(event.session_id.as_str(), "" | "mcp" | "cli") {
            return WriteDoor::Outside(format!("a THOR call of another session, {}", event.session_id));
        }
        return WriteDoor::OtherSessionPending { session: event.session_id.clone() };
    }
    WriteDoor::Pending
}

/// The kinds of event that change an item: the ones D10 counts as a write to
/// a frozen item. A verdict (`mark`) is not one - it is judged on its own.
fn changes_an_item(kind: EventKind) -> bool {
    moves_the_head(kind) || kind == EventKind::FactReprojected
}

/// Every id on `start`'s frozen lists - judgement, batch, A4 list, disputes to
/// verify - once each, in that order.
pub fn frozen_ids(start: &EvaluationStart) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    start
        .frozen
        .judgement
        .iter()
        .chain(start.frozen.batch())
        .chain(start.frozen.a4_list())
        .chain(start.frozen.dispute_list().iter().map(|d| &d.id))
        .filter(|id| seen.insert(id.as_str()))
        .cloned()
        .collect()
}

/// The writes to `start`'s frozen items, and to its start record, in its
/// window: after the start, up to log seq `until` (`None`: the log's tip).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrozenWrites {
    /// Made outside an attested THOR call of the evaluation, oldest first:
    /// each one voids it (D10).
    pub outside: Vec<FrozenVoid>,
    /// Made through a THOR call of the evaluating session that is not
    /// verified yet, as (id, log seq): they count once the turn has ended.
    pub pending: Vec<(String, i64)>,
    /// Made through an attested THOR call of ANOTHER session, as (id, log
    /// seq, that session), oldest first (step 24): never a void - the item
    /// counts as settled by that session.
    pub foreign: Vec<(String, i64, String)>,
    /// Stamped as a THOR call of another session and not verified yet, as
    /// (id, log seq, that session): the evaluating session's next Stop checks
    /// each one. Neither a void nor a credit until then.
    pub foreign_pending: Vec<(String, i64, String)>,
}

/// `FrozenWrites` of `start`. Reads each frozen id's own events (indexed),
/// never the whole log. An id whose events cannot be read is skipped: an
/// unreadable log is not evidence of a side door.
pub fn frozen_writes_in_window(store: &EventStore, start: &EvaluationStart, until: Option<i64>) -> FrozenWrites {
    frozen_writes_judged(store, start, until, until)
}

/// `frozen_writes_in_window` for the writes up to log seq `until`, each judged
/// by the verifications recorded up to `judged_until` (`None`: every one on
/// the log now). The Stop-time validator (step 16) judges the writes of a
/// filing's window by every verification recorded since - its own Stop
/// completes them first - while the filing gate itself only knew the ones
/// recorded before it.
pub fn frozen_writes_judged(
    store: &EventStore,
    start: &EvaluationStart,
    until: Option<i64>,
    judged_until: Option<i64>,
) -> FrozenWrites {
    let session = start.frozen.session.as_str();
    let mut found = FrozenWrites::default();
    let mut ids = frozen_ids(start);
    ids.push(start.id.clone());
    for id in ids {
        let Ok(events) = store.get_events_by_entity(&id) else { continue };
        let judged: Vec<Event> = events.iter().filter(|e| judged_until.is_none_or(|u| e.seq <= u)).cloned().collect();
        let verdicts = recorded_write_verdicts(&judged);
        let events: Vec<Event> = events.into_iter().filter(|e| until.is_none_or(|u| e.seq <= u)).collect();
        for event in events.iter().filter(|e| e.seq > start.frozen.started_seq && changes_an_item(e.kind)) {
            match write_door(event, &verdicts, session) {
                WriteDoor::Machine | WriteDoor::Attested { .. } => {}
                WriteDoor::Pending => found.pending.push((id.clone(), event.seq)),
                WriteDoor::OtherSession { session: other, .. } => found.foreign.push((id.clone(), event.seq, other)),
                WriteDoor::OtherSessionPending { session: other } => {
                    found.foreign_pending.push((id.clone(), event.seq, other))
                }
                WriteDoor::Outside(door) => found.outside.push(FrozenVoid { id: id.clone(), seq: event.seq, door }),
            }
        }
    }
    found.outside.sort_by_key(|w| w.seq);
    found.pending.sort_by_key(|(_, seq)| *seq);
    found.foreign.sort_by_key(|(_, seq, _)| *seq);
    found.foreign_pending.sort_by_key(|(_, seq, _)| *seq);
    found
}

/// The other session whose attested THOR call last changed `id` in `start`'s
/// window, with that write's log seq - the session an item on the lists is
/// credited to (step 24). `None` when no other session's attested write
/// changed it, or when this session changed it again after that.
pub fn credited_to_other_session(store: &EventStore, start: &EvaluationStart, id: &str) -> Option<(String, i64)> {
    let events = store.get_events_by_entity(id).ok()?;
    credited_in(&events, start.frozen.started_seq, &start.frozen.session)
}

/// `credited_to_other_session` over one entity's events (oldest first).
fn credited_in(events: &[Event], started_seq: i64, session: &str) -> Option<(String, i64)> {
    let verdicts = recorded_write_verdicts(events);
    let last = events.iter().filter(|e| e.seq > started_seq && changes_an_item(e.kind)).filter(|e| {
        !RESERVED_ACTORS.contains(&e.actor.as_str())
    });
    match last.last().map(|e| (e.seq, write_door(e, &verdicts, session))) {
        Some((seq, WriteDoor::OtherSession { session: other, .. })) => Some((other, seq)),
        _ => None,
    }
}

/// The open evaluation of any session whose frozen lists hold `id` - for the
/// tool server's on-the-spot refusal of a side-door write to a frozen item.
/// The newest one when several do.
pub fn open_evaluation_freezing(store: &EventStore, id: &str) -> Option<EvaluationStart> {
    let events = store.events_of_kinds(&[EventKind::FactCreated, EventKind::FactRevised], 0).ok()?;
    let now = now_unix_secs();
    let mut newest_per_key: HashMap<(String, String), EvaluationStart> = HashMap::new();
    for start in starts_in(&events) {
        newest_per_key.insert((start.frozen.session.clone(), start.frozen.project.clone()), start);
    }
    newest_per_key
        .into_values()
        .filter(|s| s.id == id || frozen_ids(s).iter().any(|frozen| frozen == id))
        .filter(|s| evaluation_still_open(store, s, now))
        .max_by_key(|s| s.seq)
}

// ------------------------------------------ the evaluation start record (step 13)
//
// An evaluation starts IN THE LOG: one `fact_created` by THOR's own
// `eval-machine`, entity `evaluation-start-<project slug>-<N>`, session S,
// whose body is a canonical Report (the project, the tag `evaluation-start`,
// the start note in words as its text) with a trailing `"frozen"` key spliced
// on the same way `because` is. `frozen` holds the lists the report is held
// to - the judgement list, the teeth batch, the A4 list and the open points -
// fixed at log position N, so nothing that fires or is written afterwards can
// grow them. The id is reserved for `eval-machine` at the fence
// (`reserved_id_owner`), so no other writer can plant or change one.
//
// TWO WRITES (step 14). The start record is written FIRST, with the judgement
// list and the open points and NO batch: THOR's machine pass over the
// project's old rules runs after it, under a time box, and its result is
// written as ONE `fact_revised` by `eval-machine` on the same record, adding
// `frozen.batch`, `frozen.a4_list` and `frozen.machine`
// (`record_evaluation_batch`). A start record with no batch is the marker of
// a pass that has not finished - a hook killed in between - and the filing
// gate refuses until it is there.
//
// It replaces the hook's handover sidecar (`evaluation-sessions.json`): any
// local process could write that file, and an evaluation whose start and
// list lived there could be shrunk or moved by editing it.

/// The id prefix of every evaluation start record.
pub const EVALUATION_START_PREFIX: &str = "evaluation-start-";

/// The tag every evaluation start record carries.
pub const EVALUATION_START_TAG: &str = "evaluation-start";

/// The actor that writes evaluation start records (a reserved name).
pub const EVAL_MACHINE_ACTOR: &str = "eval-machine";

/// How long an evaluation that never filed its report stays open. After this
/// a new start signal opens a fresh one: an evaluation abandoned yesterday
/// must not fix today's list.
pub const EVALUATION_OPEN_HOURS: i64 = 16;

/// How many rules whose check bites and whose text only fails to name the
/// literal (A4) one evaluation hands to the agent, beside its batch (K23,
/// Q24).
pub const EVALUATION_A4_LIST: usize = 16;

/// How many disputes one evaluation hands to the agent to verify: the batch's
/// own cap (`EVALUATION_TEETH_BATCH`) - one real look at one rule's file per
/// item, which is what a dispute asks for. The rest wait for the next one.
pub const EVALUATION_DISPUTE_LIST: usize = EVALUATION_TEETH_BATCH;

/// A blocker as the start record keeps it (`Blocker`, in a serde shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineBlocker {
    /// `Blocker::code`.
    pub code: String,
    /// `Blocker::subject`: the project, or the file.
    pub subject: String,
}

impl MachineBlocker {
    pub fn of(blocker: &Blocker) -> Self {
        MachineBlocker { code: blocker.code().to_string(), subject: blocker.subject().to_string() }
    }

    pub fn to_blocker(&self) -> Blocker {
        if self.code == "working-tree-differs" {
            Blocker::WorkingTreeDiffers { path: self.subject.clone() }
        } else if self.code == "anchor-full" {
            Blocker::AnchorFull { path: self.subject.clone() }
        } else {
            Blocker::NoWorkingCopy { project: self.subject.clone() }
        }
    }
}

/// What THOR's machine pass did with one old rule at an evaluation start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineOutcome {
    /// Its check bites: written on the rule track by `teeth-eval`.
    Rule,
    /// No tooth holds: written as a note by `teeth-eval`.
    Note,
    /// A note's pending check now holds and bites: hung by `teeth-eval`.
    PendingHung,
    /// A note's pending check is still not a working tooth: nothing written.
    PendingKept,
    /// Held by a machine blocker: THOR's open point for its subject holds it.
    Blocked,
    /// The agent has to act: on the frozen batch, or waiting for a later one.
    NeedsAgent,
    /// The check bites and only the naming is missing (A4): on the A4 list,
    /// or waiting for a later one.
    A4Only,
    /// Tried again and still the same rule: nothing written.
    Unchanged,
}

/// One old rule as the machine pass left it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineItem {
    pub id: String,
    pub outcome: MachineOutcome,
    /// Why, in words: the verdict's first line, the because of a note, the
    /// fix the agent has to make, or the blocker.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// The exact call the agent runs for it, when it needs the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    /// The blocker holding it, when it is blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<MachineBlocker>,
    /// The seq of the `fact_revised` `teeth-eval` wrote for it, when it
    /// wrote one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
}

/// One of THOR's own open points as the machine pass left it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachinePoint {
    pub id: String,
    pub blocker: MachineBlocker,
    /// The rules it holds (the ones this pass found blocked by it).
    #[serde(default)]
    pub rules: Vec<String>,
    /// Every VALID evaluation that filed while this subject was held by an
    /// open point, as the validator recorded them up to this start (step 16,
    /// B5). Not empty: the point is in the OWNER QUEUE - the owner decides.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried_by: Vec<Carrier>,
}

/// What THOR's machine pass decided at an evaluation start
/// (`Frozen::machine`): the seven totals, every item's outcome, and the open
/// points it wrote or closed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachinePass {
    /// Rules whose check bites, written on the rule track.
    #[serde(default)]
    pub bites: usize,
    /// Rules with no working tooth, written as notes.
    #[serde(default)]
    pub notes: usize,
    /// Notes whose pending check now bites, hung.
    #[serde(default)]
    pub pending_hung: usize,
    /// Rules held by a machine blocker.
    #[serde(default)]
    pub blocked: usize,
    /// Rules that need the agent for anything but the A4 naming - the batch
    /// holds the first `EVALUATION_TEETH_BATCH` of them.
    #[serde(default)]
    pub needs_agent: usize,
    /// Rules whose check bites and whose text only fails to name the literal
    /// - the A4 list holds the first `EVALUATION_A4_LIST` of them.
    #[serde(default)]
    pub a4_only: usize,
    /// How many of the rules the pass tried (`items`) belong to no project.
    /// They apply to every project, so each evaluation tries them, and the
    /// start note counts them apart from the project's own rules. 0 for a
    /// record written before this field existed.
    #[serde(default)]
    pub projectless: usize,
    /// Rules the pass did not reach inside its time box. Not owed in this
    /// evaluation; the next start begins with them.
    #[serde(default)]
    pub not_reached: usize,
    /// Every item's outcome, in the order the pass tried them.
    #[serde(default)]
    pub items: Vec<MachineItem>,
    /// The ids the pass did not reach, in the order it would have tried them.
    #[serde(default)]
    pub not_reached_ids: Vec<String>,
    /// THOR's open points live after the pass.
    #[serde(default)]
    pub points: Vec<MachinePoint>,
    /// THOR's open points the pass closed, each with its derived reason.
    #[serde(default)]
    pub points_closed: Vec<(String, String)>,
    /// How long the pass had, in seconds (its time box).
    #[serde(default)]
    pub box_secs: u64,
    /// The disputes the agent is to verify in this evaluation: every
    /// unanswered dispute written since the project's previous report (or left
    /// over from the previous list), at most `EVALUATION_DISPUTE_LIST`.
    #[serde(default)]
    pub disputes: Vec<DisputeItem>,
    /// The ids of the disputes that did not fit; the next start lists them first.
    #[serde(default)]
    pub disputes_later_ids: Vec<String>,
}

impl MachinePass {
    /// Rules tried again and left as they were: `Unchanged` (still the same
    /// rule) and `PendingKept` (the pending check still cannot be tried).
    /// Neither has a counter field of its own, and a sentence that lists the
    /// others without these sums to less than the number tried.
    pub fn left_as_they_were(&self) -> usize {
        self.items.iter().filter(|i| matches!(i.outcome, MachineOutcome::Unchanged | MachineOutcome::PendingKept)).count()
    }
}

/// The lists an evaluation start froze, spliced into the start record's body
/// as the trailing key `"frozen"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frozen {
    /// The Claude Code session that runs the evaluation.
    pub session: String,
    pub project: String,
    /// N: the highest log seq that existed when the evaluation started.
    pub started_seq: i64,
    /// What started it: "stop-ask", "command" or "skill".
    pub started_by: String,
    /// When it started (unix seconds). The log itself records no time, and
    /// both the open window (`EVALUATION_OPEN_HOURS`) and the "started
    /// <date>" a reader is shown need one.
    pub started_unix: i64,
    /// Creation seq of the project's newest evaluation report at the start
    /// (0 for none): where the machine pass counted "fired since" from.
    pub prev_report_seq: i64,
    /// The judgement list (`evaluation_judgement_list`).
    pub judgement: Vec<String>,
    /// The teeth batch: at most `EVALUATION_TEETH_BATCH` rule ids that need
    /// the agent. `None` until THOR's machine pass has finished and written
    /// the batch record - the marker of a pass still to run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<Vec<String>>,
    /// How many rules that need the agent did not fit in the batch.
    #[serde(default)]
    pub batch_later: usize,
    /// The A4 list: at most `EVALUATION_A4_LIST` rule ids whose check bites
    /// and whose text only has to name the literal. Written with the batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a4_list: Option<Vec<String>>,
    /// The project's open points at the start, by id: THOR's own first, then
    /// the legacy ones.
    pub open_points: Vec<String>,
    /// What THOR's own machine pass decided at the start. Written with the
    /// batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<MachinePass>,
    /// Set when the evaluation was closed as VOID (step 15, D10): an item on
    /// its frozen lists was changed outside an attested THOR call of it. A
    /// void evaluation is never open again and no report can be filed for
    /// it; the next start signal opens a fresh one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub void: Option<FrozenVoid>,
    /// Set when the evaluation was closed as SUPERSEDED (step 24): another
    /// session started the project's evaluation while this one's session had
    /// been idle for `EVALUATION_IDLE_MINUTES` or more. Never a void: the work
    /// it did stays on record, it is only no longer the one running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded: Option<FrozenSuperseded>,
    /// Where each item on the judgement list fired in the evaluating session
    /// before the start (step 24): how often, and its last few distinct
    /// triggers - so a verdict rests on the place, not a guess. Field case
    /// 2026-10-01: the start note said "`get` shows where it fired", `get`
    /// showed no trigger at all, and one session judged 205 items in four
    /// minutes without looking.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fired: BTreeMap<String, Firings>,
}

/// How one item fired in one session (`Frozen::fired`, `session_firings`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Firings {
    /// How many times it was served in the session.
    pub count: usize,
    /// Its last up to `FIRED_TRIGGERS` distinct triggers, newest first, each
    /// at most `FIRED_TRIGGER_CHARS` characters, the part around the item's
    /// own anchor when it names one.
    #[serde(default)]
    pub triggers: Vec<String>,
}

/// How many distinct triggers per item `session_firings` keeps.
pub const FIRED_TRIGGERS: usize = 3;

/// Above this many judgement items an evaluation keeps - and its start note
/// shows - one place per item instead of `FIRED_TRIGGERS`, so the record and
/// the note stay bounded; a list that long is mapped from the transcript
/// anyway (the routine says how), and `get` with `fired` shows more.
pub const FIRED_FULL_LIST_MAX: usize = 50;

/// The longest trigger `session_firings` keeps, in characters.
pub const FIRED_TRIGGER_CHARS: usize = 120;

/// What a firing with no command or file of its own is shown as (a prompt,
/// or a tool call the hook recorded no trigger for).
pub const FIRED_WITHOUT_TRIGGER: &str = "(a prompt or a tool call with no command or file)";

/// `trigger` cut to `FIRED_TRIGGER_CHARS` characters: the part around the
/// first of `anchors` it names (slashes compared alike, case ignored), else
/// its start.
pub fn trigger_excerpt(trigger: &str, anchors: &[String]) -> String {
    trigger_excerpt_to(trigger, anchors, FIRED_TRIGGER_CHARS)
}

/// `trigger_excerpt` with the size given: the log records a longer excerpt
/// than an evaluation shows (`serve::deliver::TRIGGER_LIMIT`), so the trial of
/// a rule can read most real calls whole.
pub fn trigger_excerpt_to(trigger: &str, anchors: &[String], limit: usize) -> String {
    let chars: Vec<char> = trigger.chars().collect();
    if chars.len() <= limit {
        return trigger.to_string();
    }
    // An excerpt this function wrote itself at the time of the firing (its
    // "..." marks included) is already the right size: cut again it would
    // lose its own end.
    if chars.len() <= limit + 6 && (trigger.starts_with("...") || trigger.ends_with("...")) {
        return trigger.to_string();
    }
    let folded: String = trigger.chars().map(|c| if c == '\\' { '/' } else { c.to_ascii_lowercase() }).collect();
    let hit = anchors
        .iter()
        .map(|a| a.replace('\\', "/").to_ascii_lowercase())
        .filter(|a| !a.is_empty())
        .find_map(|a| folded.find(&a).map(|byte| (folded[..byte].chars().count(), a.chars().count())));
    let start = match hit {
        Some((at, len)) if at + len > limit => (at + len / 2).saturating_sub(limit / 2),
        _ => 0,
    };
    let start = start.min(chars.len() - limit);
    let cut: String = chars[start..start + limit].iter().collect();
    match (start > 0, start + limit < chars.len()) {
        (true, true) => format!("...{cut}..."),
        (true, false) => format!("...{cut}"),
        (false, true) => format!("{cut}..."),
        (false, false) => cut,
    }
}

/// How each of `ids` fired in `session` at or before log seq `until_seq`
/// (`Frozen::fired`): one read of the session's served events. An id that
/// never fired there is left out. Fails open to an empty map.
pub fn session_firings(store: &EventStore, session: &str, ids: &[String], until_seq: i64) -> BTreeMap<String, Firings> {
    let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let Ok(events) = store.events_of_kinds_in_session(&[EventKind::ItemServed], session) else { return BTreeMap::new() };
    let mut out: BTreeMap<String, Firings> = BTreeMap::new();
    let mut anchors: HashMap<String, Vec<String>> = HashMap::new();
    for event in events.iter().rev().filter(|e| e.seq <= until_seq && wanted.contains(e.entity_id.as_str())) {
        let anchor = anchors.entry(event.entity_id.clone()).or_insert_with(|| {
            show(store, &event.entity_id)
                .map(|item| {
                    item.bindings
                        .iter()
                        .filter_map(|b| match b {
                            Binding::Target { value, .. } => Some(value.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        });
        let trigger = serde_json::from_str::<crate::served::ItemServed>(&event.body).ok().and_then(|b| b.trigger);
        let shown = match trigger.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => trigger_excerpt(t, anchor),
            None => FIRED_WITHOUT_TRIGGER.to_string(),
        };
        let entry = out.entry(event.entity_id.clone()).or_default();
        entry.count += 1;
        if entry.triggers.len() < FIRED_TRIGGERS && !entry.triggers.contains(&shown) {
            entry.triggers.push(shown);
        }
    }
    out
}

/// How an evaluation was closed as superseded (`Frozen::superseded`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenSuperseded {
    /// The session whose start closed it.
    pub by_session: String,
    /// The log's tip when it was closed.
    pub at_seq: i64,
    /// How many minutes its session had shown no activity on the log when it
    /// was closed - at least this many; `None` when none was found at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_minutes: Option<i64>,
}

/// One project has at most one evaluation running (step 24): another
/// session's open evaluation of it holds a new start off while that session
/// showed activity on the log within this many minutes, and is superseded
/// once it has been idle longer. Measured 2026-10-01: two sessions evaluated
/// one project at once, with overlapping frozen lists, and destroyed each
/// other's evaluation.
pub const EVALUATION_IDLE_MINUTES: i64 = 30;

impl Frozen {
    /// Whether THOR's machine pass has finished: the batch record is written.
    pub fn decided(&self) -> bool {
        self.batch.is_some()
    }

    /// The frozen batch; empty while the machine pass has not finished.
    pub fn batch(&self) -> &[String] {
        self.batch.as_deref().unwrap_or(&[])
    }

    /// The frozen A4 list; empty while the machine pass has not finished.
    pub fn a4_list(&self) -> &[String] {
        self.a4_list.as_deref().unwrap_or(&[])
    }

    /// The frozen disputes to verify; empty while the machine pass has not finished.
    pub fn dispute_list(&self) -> &[DisputeItem] {
        self.machine.as_ref().map(|m| m.disputes.as_slice()).unwrap_or(&[])
    }

    /// Whether the evaluation was closed as void (`Frozen::void`).
    pub fn is_void(&self) -> bool {
        self.void.is_some()
    }

    /// Whether the evaluation was closed as superseded (`Frozen::superseded`).
    pub fn is_superseded(&self) -> bool {
        self.superseded.is_some()
    }
}

/// One evaluation start record, read back from the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationStart {
    /// `evaluation-start-<project slug>-<N>`.
    pub id: String,
    /// The start record's own event seq (N + 1 when nothing raced it).
    pub seq: i64,
    /// The frozen lists as the record holds them now: the ones written at
    /// the start, plus the batch, the A4 list and the machine's decisions
    /// once the batch record is written.
    pub frozen: Frozen,
    /// The seq of the batch record (`record_evaluation_batch`), once written.
    pub batch_seq: Option<i64>,
}

/// The id of the start record of `project`'s evaluation starting at log
/// position `n`: the project folded to lower-case ASCII letters, digits and
/// single hyphens, so any project name gives a plain, greppable id.
pub fn evaluation_start_id(project: &str, n: i64) -> String {
    format!("{EVALUATION_START_PREFIX}{}-{n}", slug(project, "project"))
}

/// `body` with the trailing key `"frozen":{...}` spliced on, byte for byte
/// the way `body_with_because` splices `because`. Public so a test can write
/// a start record raw (a test holds no THOR actor token).
pub fn body_with_frozen(body: &str, frozen: &Frozen) -> Result<String, serde_json::Error> {
    let frozen = serde_json::to_string(frozen)?;
    debug_assert!(body.ends_with('}'), "every event body written here is a JSON object");
    Ok(format!("{},\"frozen\":{frozen}}}", &body[..body.len() - 1]))
}

/// The frozen lists an event body carries, if any.
pub fn frozen_of(body: &str) -> Option<Frozen> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    serde_json::from_value(value.get("frozen")?.clone()).ok()
}

/// The start record `event` is, when it is one: a `fact_created` by
/// `eval-machine` on an `evaluation-start-` id whose body carries `frozen`.
/// Anything else - an event by another actor included, which the fence
/// never lets through - is not a start.
fn start_of_event(event: &Event) -> Option<EvaluationStart> {
    if event.kind != EventKind::FactCreated
        || event.actor != EVAL_MACHINE_ACTOR
        || !event.entity_id.starts_with(EVALUATION_START_PREFIX)
    {
        return None;
    }
    Some(EvaluationStart { id: event.entity_id.clone(), seq: event.seq, frozen: frozen_of(&event.body)?, batch_seq: None })
}

/// Every start record in `events` (created and revised events of start
/// records, in log order), each with the lists its record holds NOW: a later
/// `fact_revised` by `eval-machine` on the same record - the batch record, or
/// the one that closes it as void - replaces the frozen lists the start
/// wrote, and is the only thing that can.
fn starts_in(events: &[Event]) -> Vec<EvaluationStart> {
    let mut starts: Vec<EvaluationStart> = Vec::new();
    for event in events {
        if let Some(start) = start_of_event(event) {
            starts.push(start);
            continue;
        }
        let revised = event.kind == EventKind::FactRevised
            && event.actor == EVAL_MACHINE_ACTOR
            && event.entity_id.starts_with(EVALUATION_START_PREFIX);
        if !revised {
            continue;
        }
        let Some(frozen) = frozen_of(&event.body) else { continue };
        if let Some(start) = starts.iter_mut().find(|s| s.id == event.entity_id) {
            // The batch record is the revise that ADDS the batch; a later
            // one (the void mark) leaves its seq alone.
            if !start.frozen.decided() && frozen.decided() {
                start.batch_seq = Some(event.seq);
            }
            start.frozen = frozen;
        }
    }
    starts
}

/// What an evaluation of `project` by `session` starting now freezes FIRST,
/// computed from the log alone: the judgement list
/// (`evaluation_judgement_list`, attested verdicts only) and the project's
/// open points. N is the log's tip. The batch, the A4 list and the machine's
/// decisions are not in it: THOR's machine pass writes them after the start
/// record is on the log (`record_evaluation_batch`). Pure: nothing is written.
pub fn frozen_now(store: &EventStore, session: &str, project: &str, started_by: &str, started_unix: i64) -> Option<Frozen> {
    let started_seq = store.get_next_seq().ok()? - 1;
    Some(Frozen {
        session: session.to_string(),
        project: project.to_string(),
        started_seq,
        started_by: started_by.to_string(),
        started_unix,
        prev_report_seq: newest_evaluation_report_seq(store, Some(project)),
        judgement: evaluation_judgement_list(store, Some(project), session, started_seq),
        batch: None,
        batch_later: 0,
        a4_list: None,
        // The project's own points, then THOR's store-wide one, which every
        // project's evaluation holds the rules with no project by (step 21).
        open_points: open_points(store, Some(project))
            .into_iter()
            .chain(open_points(store, None).into_iter().filter(|p| p.machine))
            .map(|p| p.id)
            .collect(),
        machine: None,
        void: None,
        superseded: None,
        fired: BTreeMap::new(),
    })
    .map(|mut frozen| {
        let mut fired = session_firings(store, session, &frozen.judgement, started_seq);
        if frozen.judgement.len() > FIRED_FULL_LIST_MAX {
            fired.values_mut().for_each(|f| f.triggers.truncate(1));
        }
        frozen.fired = fired;
        frozen
    })
}

/// Append the start record: one `fact_created`, straight onto the log - not
/// through `declare` (a start is THOR's own record, not a claim for the write
/// gate to judge). Only `eval-machine` writes one; `item` must be a Report on
/// an `evaluation-start-` id carrying `EVALUATION_START_TAG`, and the id must
/// not exist yet (two starts racing for the same N: one lands, the other is
/// refused and reads the one that landed).
pub fn record_evaluation_start(
    store: &mut EventStore,
    actor: SystemActor,
    item: &Item,
    frozen: &Frozen,
) -> Result<EvaluationStart, WriteError> {
    if actor.name() != EVAL_MACHINE_ACTOR {
        return Err(WriteError::Refused(Refusal {
            problem: format!("an evaluation start is recorded by THOR's eval-machine only, not by '{}'", actor.name()),
            fix: "let the hook start the evaluation (serve::eval_session::start)".to_string(),
        }));
    }
    let shaped = item.id.starts_with(EVALUATION_START_PREFIX)
        && item.kind == Kind::Report
        && item.tags.iter().any(|t| t == EVALUATION_START_TAG);
    if !shaped {
        return Err(WriteError::Refused(Refusal {
            problem: format!("'{}' is not shaped like an evaluation start record", item.id),
            fix: format!("write a Report on an id beginning with '{EVALUATION_START_PREFIX}' tagged '{EVALUATION_START_TAG}'"),
        }));
    }
    let name = fence(store, actor.into(), &item.id)?;
    let body = canonical_body(&normalized(item)).map_err(WriteError::Serialize)?;
    let body = body_with_frozen(&body, frozen).map_err(WriteError::Serialize)?;
    let event = store
        .append_created_unique(&frozen.session, &frozen.session, name, &item.id, &body, |_, _, _| false)
        .map_err(WriteError::Store)?;
    Ok(EvaluationStart { id: item.id.clone(), seq: event.seq, frozen: frozen.clone(), batch_seq: None })
}

/// Write the batch record: ONE `fact_revised` by `eval-machine` on the start
/// record `start`, whose body is the record with `text` (the start note, now
/// naming what is the agent's to do) and the frozen lists with `batch`,
/// `a4_list`, `batch_later` and `machine` added. Everything else the start
/// froze - the session, N, the judgement list, the open points - is carried
/// over unchanged. Refused when the record already holds a batch: an
/// evaluation's lists are written once.
pub fn record_evaluation_batch(
    store: &mut EventStore,
    actor: SystemActor,
    start: &EvaluationStart,
    text: &str,
    batch: Vec<String>,
    batch_later: usize,
    a4_list: Vec<String>,
    machine: MachinePass,
) -> Result<EvaluationStart, WriteError> {
    if actor.name() != EVAL_MACHINE_ACTOR {
        return Err(WriteError::Refused(Refusal {
            problem: format!("an evaluation's batch is recorded by THOR's eval-machine only, not by '{}'", actor.name()),
            fix: "let the hook run the machine pass (serve::eval_session::start)".to_string(),
        }));
    }
    let name = fence(store, actor.into(), &start.id)?;
    let events = store.get_events_by_entity(&start.id).map_err(|e| WriteError::Store(e.into()))?;
    let current = starts_in(&events).into_iter().next();
    let Some(current) = current else {
        return Err(WriteError::Store(anyhow::anyhow!("'{}' is not an evaluation start record on this log", start.id)));
    };
    if current.frozen.decided() {
        return Err(WriteError::Refused(Refusal {
            problem: format!("'{}' already holds its batch (log seq {})", start.id, current.batch_seq.unwrap_or(0)),
            fix: "read the start record back (open_evaluation_start): an evaluation's lists are written once".to_string(),
        }));
    }
    if current.frozen.is_void() || current.frozen.is_superseded() {
        return Err(WriteError::Refused(Refusal {
            problem: format!("'{}' is closed (void or superseded): it is no longer the evaluation running", start.id),
            fix: "start a fresh evaluation (serve::eval_session::start): a closed one is never decided".to_string(),
        }));
    }
    let mut item = show(store, &start.id).map_err(|e| WriteError::Store(anyhow::anyhow!("{e}")))?;
    item.text = text.to_string();
    let mut frozen = current.frozen.clone();
    frozen.batch = Some(batch);
    frozen.batch_later = batch_later;
    frozen.a4_list = Some(a4_list);
    frozen.machine = Some(machine);
    let body = canonical_body(&normalized(&item)).map_err(WriteError::Serialize)?;
    let body = body_with_frozen(&body, &frozen).map_err(WriteError::Serialize)?;
    let event = store
        .append_mutate_checked(&frozen.session, &frozen.session, name, EventKind::FactRevised, &start.id, None, &body)
        .map_err(WriteError::Store)?;
    Ok(EvaluationStart { id: current.id, seq: current.seq, frozen, batch_seq: Some(event.seq) })
}

/// Close the evaluation `start` records as VOID (step 15, D10): ONE more
/// `fact_revised` by `eval-machine` on the start record, whose frozen lists
/// carry `void` - the item that was changed behind the evaluation, the log
/// seq of that write and its door - and whose text says so. Everything else
/// the record froze is carried over unchanged. A record that is void already
/// is returned as it is: an evaluation is voided once.
pub fn record_evaluation_void(
    store: &mut EventStore,
    actor: SystemActor,
    start: &EvaluationStart,
    void: &FrozenVoid,
) -> Result<EvaluationStart, WriteError> {
    if actor.name() != EVAL_MACHINE_ACTOR {
        return Err(WriteError::Refused(Refusal {
            problem: format!("an evaluation is closed as void by THOR's eval-machine only, not by '{}'", actor.name()),
            fix: "let the tool server close it (serve::eval_session::void)".to_string(),
        }));
    }
    let name = fence(store, actor.into(), &start.id)?;
    let events = store.get_events_by_entity(&start.id).map_err(|e| WriteError::Store(e.into()))?;
    let Some(current) = starts_in(&events).into_iter().next() else {
        return Err(WriteError::Store(anyhow::anyhow!("'{}' is not an evaluation start record on this log", start.id)));
    };
    if current.frozen.is_void() {
        return Ok(current);
    }
    let mut item = show(store, &start.id).map_err(|e| WriteError::Store(anyhow::anyhow!("{e}")))?;
    item.text = format!("{}\n\n[THOR] THIS EVALUATION IS VOID: {}.", item.text, changed_behind_refusal(void).problem);
    let mut frozen = current.frozen.clone();
    frozen.void = Some(void.clone());
    let body = canonical_body(&normalized(&item)).map_err(WriteError::Serialize)?;
    let body = body_with_frozen(&body, &frozen).map_err(WriteError::Serialize)?;
    store
        .append_mutate_checked(&frozen.session, &frozen.session, name, EventKind::FactRevised, &start.id, None, &body)
        .map_err(WriteError::Store)?;
    Ok(EvaluationStart { frozen, ..current })
}

/// Close the evaluation `start` records as SUPERSEDED (step 24): ONE more
/// `fact_revised` by `eval-machine` on the start record, whose frozen lists
/// carry `superseded` and whose text says so. Called when another session
/// starts the project's evaluation while this one's session has been idle for
/// `EVALUATION_IDLE_MINUTES` or more. Never a void: its verdicts and edits
/// stay on record. A record already closed (void or superseded) is returned
/// as it is.
pub fn record_evaluation_superseded(
    store: &mut EventStore,
    actor: SystemActor,
    start: &EvaluationStart,
    superseded: &FrozenSuperseded,
) -> Result<EvaluationStart, WriteError> {
    if actor.name() != EVAL_MACHINE_ACTOR {
        return Err(WriteError::Refused(Refusal {
            problem: format!("an evaluation is closed as superseded by THOR's eval-machine only, not by '{}'", actor.name()),
            fix: "let the hook close it (serve::eval_session::start)".to_string(),
        }));
    }
    let name = fence(store, actor.into(), &start.id)?;
    let events = store.get_events_by_entity(&start.id).map_err(|e| WriteError::Store(e.into()))?;
    let Some(current) = starts_in(&events).into_iter().next() else {
        return Err(WriteError::Store(anyhow::anyhow!("'{}' is not an evaluation start record on this log", start.id)));
    };
    if current.frozen.is_void() || current.frozen.is_superseded() {
        return Ok(current);
    }
    let mut item = show(store, &start.id).map_err(|e| WriteError::Store(anyhow::anyhow!("{e}")))?;
    item.text = format!("{}\n\n[THOR] THIS EVALUATION WAS SUPERSEDED: {}.", item.text, superseded_words(superseded));
    let mut frozen = current.frozen.clone();
    frozen.superseded = Some(superseded.clone());
    let body = canonical_body(&normalized(&item)).map_err(WriteError::Serialize)?;
    let body = body_with_frozen(&body, &frozen).map_err(WriteError::Serialize)?;
    store
        .append_mutate_checked(&frozen.session, &frozen.session, name, EventKind::FactRevised, &start.id, None, &body)
        .map_err(WriteError::Store)?;
    Ok(EvaluationStart { frozen, ..current })
}

/// Why an evaluation was superseded, in one plain clause.
pub fn superseded_words(superseded: &FrozenSuperseded) -> String {
    let idle = match superseded.idle_minutes {
        Some(minutes) => format!("{minutes} minute(s)"),
        None => format!("more than {EVALUATION_IDLE_MINUTES} minutes"),
    };
    format!(
        "session {} started this project's evaluation at log seq {} after this evaluation's session had shown no \
         activity for {idle} - one project runs one evaluation at a time. Its verdicts and edits stay on record; only \
         its report can no longer be filed",
        superseded.by_session, superseded.at_seq
    )
}

/// The newest evaluation start of `project` written before log seq
/// `before_seq`, in any session - where a machine pass reads which rules the
/// previous pass did not reach.
pub fn previous_evaluation_start(store: &EventStore, project: &str, before_seq: i64) -> Option<EvaluationStart> {
    let events = store.events_of_kinds(&[EventKind::FactCreated, EventKind::FactRevised], 0).ok()?;
    starts_in(&events).into_iter().filter(|s| s.frozen.project == project && s.seq < before_seq).max_by_key(|s| s.seq)
}

/// Every evaluation start record `session` wrote, oldest first.
pub fn evaluation_starts_of_session(store: &EventStore, session: &str) -> Vec<EvaluationStart> {
    let Ok(events) = store.events_of_kinds_in_session(&[EventKind::FactCreated, EventKind::FactRevised], session) else {
        return Vec::new();
    };
    starts_in(&events).into_iter().filter(|s| s.frozen.session == session).collect()
}

/// Whether the evaluation `start` records has filed its OWN report: a live
/// evaluation report of its project, created or newly tagged after the start
/// by a write whose session is the evaluating session. A report another
/// session files for the same project in parallel closes nothing here - it
/// was never this evaluation's report.
fn own_report_filed(store: &EventStore, start: &EvaluationStart) -> bool {
    let Ok(events) = store.events_of_kinds_in_session(&[EventKind::FactCreated, EventKind::FactRevised], &start.frozen.session)
    else {
        return false;
    };
    let mut candidates: Vec<&str> = events
        .iter()
        .filter(|e| e.seq > start.seq)
        .filter(|e| parse_body(&e.body).is_ok_and(|item| is_evaluation_report_of(&item, &start.frozen.project)))
        .map(|e| e.entity_id.as_str())
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    candidates.into_iter().any(|id| {
        show(store, id).is_ok_and(|item| is_evaluation_report_of(&item, &start.frozen.project))
            && filed_by_session_after(store, id, start)
    })
}

/// Whether report `id` was FILED for `start`'s project after the start by
/// the evaluating session: the write that made it an evaluation report of
/// that project (its creation, or the revise that tagged it or moved it
/// there) came after the start, in that session. A later correction of a
/// report filed before the start - a typo fix - is not a filing.
fn filed_by_session_after(store: &EventStore, id: &str, start: &EvaluationStart) -> bool {
    filing_seq_after(store, id, start).is_some()
}

/// The log seq of the first write that FILED report `id` for `start`'s
/// project after the start, in the evaluating session (see
/// `filed_by_session_after`), if any.
fn filing_seq_after(store: &EventStore, id: &str, start: &EvaluationStart) -> Option<i64> {
    let events = store.get_events_by_entity(id).ok()?;
    let mut was_report = false;
    for e in events.iter().filter(|e| writes_the_item(e.kind)) {
        // A body that is no item (a resolve names heads, not fields) changes
        // nothing about what the report is.
        let Ok(item) = parse_body(&e.body) else { continue };
        let is_report = is_evaluation_report_of(&item, &start.frozen.project);
        if is_report && !was_report && e.seq > start.seq && e.session_id == start.frozen.session {
            return Some(e.seq);
        }
        was_report = is_report;
    }
    None
}

/// THE SESSION'S OWN EVALUATION AT A POINT IN THE LOG (step 20, NEW-2): the
/// evaluation `session` had open at log seq `seq` - its start record comes
/// before `seq`, it was not closed as void before `seq`, the session had not
/// filed its report for it before `seq`, and (when `at_unix`, the time of
/// the event at `seq`, is known) the start was not older than
/// `EVALUATION_OPEN_HOURS` then. Read from the log alone. A refusal the
/// evaluating session met inside this window is a call it made while judging
/// the very rules that refused it: it never proves a tooth.
pub fn evaluation_open_in_session_at(store: &EventStore, session: &str, seq: i64, at_unix: Option<i64>) -> Option<EvaluationStart> {
    let events = store.events_of_kinds_in_session(&[EventKind::FactCreated, EventKind::FactRevised], session).ok()?;
    let starts: Vec<EvaluationStart> =
        starts_in(&events).into_iter().filter(|s| s.frozen.session == session && s.seq < seq).collect();
    starts.into_iter().rev().find(|start| {
        if at_unix.is_some_and(|at| at - start.frozen.started_unix > EVALUATION_OPEN_HOURS * 3600) {
            return false;
        }
        let voided_before = events.iter().any(|e| {
            e.entity_id == start.id
                && e.kind == EventKind::FactRevised
                && e.actor == EVAL_MACHINE_ACTOR
                && e.seq < seq
                && frozen_of(&e.body).is_some_and(|f| f.is_void() || f.is_superseded())
        });
        if voided_before {
            return false;
        }
        let mut reports: Vec<&str> = events
            .iter()
            .filter(|e| e.seq > start.seq && e.seq < seq)
            .filter(|e| parse_body(&e.body).is_ok_and(|item| is_evaluation_report_of(&item, &start.frozen.project)))
            .map(|e| e.entity_id.as_str())
            .collect();
        reports.sort_unstable();
        reports.dedup();
        !reports.into_iter().any(|id| filing_seq_after(store, id, start).is_some_and(|filed| filed < seq))
    })
}

fn is_evaluation_report_of(item: &Item, project: &str) -> bool {
    item.kind == Kind::Report
        && item.project.as_deref() == Some(project)
        && item.tags.iter().any(|t| t == EVALUATION_REPORT_TAG)
}

/// Whether `start` is still the evaluation running at `now_unix`: not void,
/// younger than `EVALUATION_OPEN_HOURS`, and its own report not filed yet.
pub fn evaluation_still_open(store: &EventStore, start: &EvaluationStart, now_unix: i64) -> bool {
    !start.frozen.is_void()
        && !start.frozen.is_superseded()
        && now_unix - start.frozen.started_unix <= EVALUATION_OPEN_HOURS * 3600
        && !own_report_filed(store, start)
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// The open evaluation `session` runs for `project`, read from the log: its
/// newest start record for that project, when that one is still open
/// (`evaluation_still_open`). `None` when there is none.
pub fn open_evaluation_start(store: &EventStore, session: &str, project: &str) -> Option<EvaluationStart> {
    open_evaluation_start_at(store, session, project, now_unix_secs())
}

/// `open_evaluation_start`, at a given time.
pub fn open_evaluation_start_at(store: &EventStore, session: &str, project: &str, now_unix: i64) -> Option<EvaluationStart> {
    evaluation_starts_of_session(store, session)
        .into_iter()
        .filter(|s| s.frozen.project == project)
        .max_by_key(|s| s.seq)
        .filter(|s| evaluation_still_open(store, s, now_unix))
}

/// Every evaluation of `project` still open at `now_unix`, in any session,
/// oldest first - so a session that opens the routine while another
/// session's evaluation runs can be told where it runs.
pub fn open_evaluation_starts_of_project(store: &EventStore, project: &str, now_unix: i64) -> Vec<EvaluationStart> {
    let Ok(events) = store.events_of_kinds(&[EventKind::FactCreated, EventKind::FactRevised], 0) else { return Vec::new() };
    let mut newest_per_session: HashMap<String, EvaluationStart> = HashMap::new();
    for start in starts_in(&events).into_iter().filter(|s| s.frozen.project == project) {
        newest_per_session.insert(start.frozen.session.clone(), start);
    }
    let mut open: Vec<EvaluationStart> =
        newest_per_session.into_values().filter(|s| evaluation_still_open(store, s, now_unix)).collect();
    open.sort_by_key(|s| s.seq);
    open
}

/// Whether the recorded verification of the write at `seq` says it was made
/// through an attested THOR call of `session` (its subagents included: their
/// writes verify under the main session, with an agent id beside it).
fn attested_for_session(verdicts: &HashMap<i64, WriteVerdict>, seq: i64, session: &str) -> bool {
    verdicts.get(&seq).is_some_and(|v| v.attested && v.session == session)
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
    /// Of the owed ones: given a verdict (or taken off the rule track) by a
    /// THOR call of the evaluating session that is not verified yet, as
    /// (id, log seq of that write). They settle once the turn has ended.
    pub unverified: Vec<(String, i64)>,
    /// Of the settled ones: changed in the window by an attested THOR call of
    /// ANOTHER session (step 24), as (id, that session) - settled by that
    /// session, credited to it, and no longer this evaluation's to settle.
    pub credited: Vec<(String, String)>,
}

/// The judgement list of an evaluation of `project` by `session` starting at
/// log position `started_seq` - what its start record freezes
/// (`Frozen::judgement`). FAILS OPEN to an empty list on an unreadable log,
/// the same stance `all_live_items` takes.
///
/// THE SET, computed from the log alone: an item belongs to it when, counting
/// only events at or before `started_seq`, it had fired in
/// `JUDGEMENT_DEBT_AFTER`+ SESSIONS (assistant deliveries only, once per
/// session, exactly as the store-wide debt counts) since its own last
/// ATTESTED verdict, AND it fired in `session` at least once at or before the
/// start - the "was the reader being asked ever in the room" test: a verdict
/// guessed on an item the evaluating session never saw is no verdict. This is
/// the ONLY place the owner is asked for a judgement (step 28): no Stop holds
/// a turn for it any more. It must also still be live,
/// still apply to `project` and still not be pinned. A firing after the start
/// never adds anything, whichever session it came from, which is what keeps
/// the evaluation's own writes - and every rule bound to the `remember` moment
/// they fire - from growing the list while it is being paid.
///
/// ONLY ATTESTED VERDICTS RESET A COUNT HERE (D9): a verdict whose recorded
/// verification (`attested_write_seqs`) does not say it came through an
/// attested THOR call - a `serve mark` from a shell, a side-door client, an
/// older binary - resets nothing, so it cannot shrink the list before the
/// evaluation even starts.
pub fn evaluation_judgement_list(store: &EventStore, project: Option<&str>, session: &str, started_seq: i64) -> Vec<String> {
    let Ok(events) = store.firings_and_verdicts() else { return Vec::new() };
    let attested = attested_write_seqs(store);
    // The sessions that were served each item since its last verdict, as of
    // the start: an item counts once per session (step 28).
    let mut sessions_at_start: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut fired_here: HashSet<&str> = HashSet::new();
    for (seq, kind, id, actor, event_session) in &events {
        if *seq > started_seq {
            continue;
        }
        match kind {
            EventKind::ItemServed if actor == ASSISTANT_DELIVERY_ACTOR => {
                sessions_at_start.entry(id.as_str()).or_default().insert(event_session.as_str());
                if event_session == session {
                    fired_here.insert(id.as_str());
                }
            }
            EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise if attested.contains(seq) => {
                sessions_at_start.insert(id.as_str(), HashSet::new());
            }
            _ => {}
        }
    }
    let live = all_live_items(store);
    let live_by_id: HashMap<&str, &Item> = live.iter().map(|(id, it)| (id.as_str(), it)).collect();
    let mut ids: Vec<String> = sessions_at_start
        .iter()
        .filter(|(id, sessions)| sessions.len() >= JUDGEMENT_DEBT_AFTER && fired_here.contains(**id))
        .filter_map(|(id, _)| {
            let item = live_by_id.get(id)?;
            let pinned = item.bindings.iter().any(|b| matches!(b, Binding::Always));
            (!pinned && project_applies_to(item.project.as_deref(), project)).then(|| id.to_string())
        })
        .collect();
    ids.sort();
    ids
}

/// The set one evaluation must settle, split by state. `report_tags` are the
/// tags of the report being filed - only its `not-judgeable` entries are
/// read. FAILS OPEN to an empty result on an unreadable log, the same stance
/// `all_live_items` takes.
///
/// THE LIST is the one the evaluation's start record froze
/// (`Frozen::judgement`): what fired in the evaluating session before it
/// started. Measured on 2026-09-26, before the list was frozen: a session
/// filed its report eight times and was refused eight times, each time on a
/// different, growing list (14, 6, 13, 4, 4, 1, 5, 4) - the evaluation's own
/// writes and parallel sessions kept pushing items over the threshold - and
/// it ended up marking sixty items useful, some it had never seen fire.
///
/// WHAT SETTLES AN ITEM ON IT (step 13), each read from the RECORDED
/// verification of a write (`recorded_write_verdicts`), never from who the
/// log says wrote it:
/// - a verdict (`mark`) after the start whose verification says it came
///   through an attested THOR call of the evaluating session (its subagents
///   included). A verdict from another session, or one outside an attested
///   call (a `serve mark` from a shell, a side-door client), settles nothing;
/// - the item gone from the rule track since the start - retracted, archived,
///   or revised into a Report (folding a duplicate into another item is
///   legitimate work) - when the write that took it off is attested for the
///   evaluating session. The same change through any other door settles
///   nothing: the item stays owed, and the report can still skip it with a
///   reason (`not-judgeable`);
/// - a `not-judgeable:<id>:<reason>` tag on the report skips it for this
///   filing (the item stays owed store-wide).
///
/// A PINNED item is skipped: neither owed nor settled (mark refuses a pinned
/// item, so a verdict could never be given).
pub fn evaluation_judgement_owed(store: &EventStore, start: &EvaluationStart, report_tags: &[String]) -> EvaluationJudgement {
    let project = Some(start.frozen.project.as_str());
    let session = start.frozen.session.as_str();
    let started_seq = start.frozen.started_seq;
    let list = start.frozen.judgement.clone();
    let live = all_live_items(store);
    let live_by_id: HashMap<&str, &Item> = live.iter().map(|(id, it)| (id.as_str(), it)).collect();
    let mut result = EvaluationJudgement::default();
    for id in list {
        let Ok(events) = store.get_events_by_entity(&id) else {
            result.owed.push(id);
            continue;
        };
        let verified = recorded_write_verdicts(&events);
        let after = |e: &&Event| e.seq > started_seq;
        // Archived, or revised into a Report, since the start: its head then
        // is a Report while the one it had at the start was not.
        let kind_at_start = events
            .iter()
            .filter(|e| e.seq <= started_seq && writes_the_item(e.kind))
            .filter_map(|e| parse_body(&e.body).ok())
            .last()
            .map(|item| item.kind);
        let gone = match live_by_id.get(id.as_str()) {
            None => true,
            Some(item) => item.kind == Kind::Report && kind_at_start.is_some_and(|k| k != Kind::Report),
        };
        if gone {
            // Gone since the start: settled only when the write that took it
            // off the rule track is the evaluating session's own attested call.
            let last_write = events
                .iter()
                .filter(after)
                .filter(|e| writes_the_item(e.kind) || e.kind == EventKind::FactRetracted)
                .max_by_key(|e| e.seq);
            if last_write.is_some_and(|e| attested_for_session(&verified, e.seq, session)) {
                result.settled.push(id);
            } else if let Some((other, _)) = credited_in(&events, started_seq, session) {
                result.credited.push((id.clone(), other));
                result.settled.push(id);
            } else if not_judgeable_reason(report_tags, &id).is_some() {
                result.skipped.push(id);
            } else {
                if let Some(e) = last_write.filter(|e| write_door(e, &verified, session) == WriteDoor::Pending) {
                    result.unverified.push((id.clone(), e.seq));
                }
                result.owed.push(id);
            }
            continue;
        }
        let item = live_by_id[id.as_str()];
        if item.bindings.iter().any(|b| matches!(b, Binding::Always)) || !project_applies_to(item.project.as_deref(), project) {
            continue;
        }
        let marks: Vec<&Event> = events
            .iter()
            .filter(after)
            .filter(|e| matches!(e.kind, EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise))
            .collect();
        let judged = marks.iter().any(|e| attested_for_session(&verified, e.seq, session));
        if judged {
            result.settled.push(id);
        } else if let Some((other, _)) = credited_in(&events, started_seq, session) {
            // Changed by another session's attested THOR call (step 24): the
            // item this session saw fire is not the item that stands now, so
            // it is that session's, never a void of this evaluation.
            result.credited.push((id.clone(), other));
            result.settled.push(id);
        } else if not_judgeable_reason(report_tags, &id).is_some() {
            result.skipped.push(id);
        } else {
            if let Some(e) = marks.iter().rev().find(|e| write_door(e, &verified, session) == WriteDoor::Pending) {
                result.unverified.push((id.clone(), e.seq));
            }
            result.owed.push(id);
        }
    }
    result
}

/// The highest creation seq among this project's own live evaluation
/// reports - `0` when it has none yet, so "since the previous report" reads
/// as "since the beginning of the log" for a project's first-ever
/// evaluation. EXACT-project match (never `project_applies_to`'s "or global"
/// rule: a report filed for one project must never silence another's debt).
/// It only orders the machine pass ("fired since the previous report") and
/// doctor's batch preview; whether a report COUNTS as the project's
/// evaluation is the Stop-time validator's answer
/// (`serve::eval_filing::newest_valid_evaluation`, step 16).
pub fn newest_evaluation_report_seq(store: &EventStore, project: Option<&str>) -> i64 {
    all_live_items(store)
        .into_iter()
        .filter(|(_, item)| item.kind == Kind::Report)
        .filter(|(_, item)| item.project.as_deref() == project)
        .filter(|(_, item)| item.tags.iter().any(|t| t == EVALUATION_REPORT_TAG))
        .filter_map(|(id, _)| store.get_events_by_entity(&id).ok()?.first().map(|e| e.seq))
        .max()
        .unwrap_or(0)
}

/// How many rules that need a fix from the agent one evaluation filing is
/// held to (since step 14: the ones THOR's machine pass could not decide
/// itself, beside the A4 list of `EVALUATION_A4_LIST`).
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

/// Prefixed onto a filler candidate's own reason (`teeth_batch_since`'s own
/// doc comment, "THE BATCH MUST ALSO REACH A RULE THAT NEVER FIRES") so a
/// reader - the Stop hook's own teeth question, or the filing gate's own
/// refusal - can tell a rule filling a place because the primary pool ran
/// short apart from one that is actually owed for firing recently. Both
/// surfaces read the identical `TeethBatch` this same function builds, so
/// they can never say something different about the same rule.
pub const TEETH_NOT_FIRED_RECENTLY_NOTE: &str = "it did not fire since the previous report, and this batch had room left, so: ";

/// The owed rules one evaluation filing is held to (see
/// `EVALUATION_TEETH_BATCH`), and the ones left for a later filing - each
/// with `settlement`'s own reason, and a filler candidate's prefixed with
/// `TEETH_NOT_FIRED_RECENTLY_NOTE`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeethBatch {
    /// In this filing's batch and not settled yet, each with what is missing.
    pub owed_now: Vec<(String, String)>,
    /// SETTLED SINCE THE PREVIOUS REPORT (2026-09-27: no longer "in this
    /// filing's batch and settled" - a settled rule never took a batch place
    /// to begin with, see `teeth_batch_since`'s own doc comment, "A SETTLED
    /// RULE NEVER TAKES A PLACE"). Listed here regardless of how many there
    /// are; only `owed_now` is held to `EVALUATION_TEETH_BATCH`.
    pub settled: Vec<String>,
    /// Not settled, and beyond this filing's batch: still owed, asked first
    /// by a later filing.
    pub owed_later: Vec<(String, String)>,
}

/// The rules of `project` with no verdict, first eight first - doctor's
/// PREVIEW of what the next evaluation start will try (step 14: THOR's
/// machine pass decides what it can, and the frozen batch holds only what
/// needs the agent), and what the filing gate falls back to for a scope with
/// no start record.
pub fn evaluation_teeth_batch(store: &EventStore, project: Option<&str>) -> TeethBatch {
    teeth_batch_since(store, project, newest_evaluation_report_seq(store, project))
}

/// The kinds of event that WRITE an item (create, correct, settle a
/// divergence, move it to another project), as opposed to the telemetry kinds
/// (served, marked, refused) that only record what happened to it.
fn writes_the_item(kind: EventKind) -> bool {
    matches!(kind, EventKind::FactCreated | EventKind::FactRevised | EventKind::FactResolved | EventKind::FactReprojected)
}

/// Every live Rule or Orientation that applies to `project` and is owed
/// (`settlement`), whatever its severity (D8) - split into one filing's batch
/// (at most `EVALUATION_TEETH_BATCH`) and the rest. A rule held by a machine
/// blocker is settled while the blocker holds and never takes a place (F9).
///
/// THE ORDER, AND WHY IT ROTATES. The rules written longest ago come first:
/// each rule's key is its last write at or before `cutoff_seq`, so a rule
/// created after the previous report waits behind every older one. Nothing
/// that happens after the cutoff moves a key. At the next filing the cutoff
/// moves to that report, and the ones left owed lead the new batch, oldest
/// first - "least recently asked first", computed from the log alone.
///
/// A SETTLED RULE NEVER TAKES A PLACE: it is listed in `TeethBatch::settled`
/// when its current verdict was written after `cutoff_seq`, with no cap.
/// Settling a rule during an evaluation therefore pulls the next owed rule
/// into `owed_now` for that same filing; the cap bounds how many are asked of
/// one filing at once, never which ones are asked at all.
///
/// TWO POOLS, in order: every owed rule that fired since `cutoff_seq` fills
/// the batch first, oldest write first; only when fewer than
/// `EVALUATION_TEETH_BATCH` of those are owed does an owed rule that did NOT
/// fire since then fill the remaining places (named with
/// `TEETH_NOT_FIRED_RECENTLY_NOTE`), so a rule that never fires can still be
/// reached.
///
/// WHY NO SEVERITY FILTER (D8). With "heavy only", a bin that lowers a rule's
/// severity took it out of the batch without any trial. The severity is part
/// of the fingerprint, so lowering it makes the rule owed again, and it stays
/// in this batch until THOR's trial decides it.
fn teeth_batch_since(store: &EventStore, project: Option<&str>, cutoff_seq: i64) -> TeethBatch {
    let Ok(events) = store.get_all_events() else { return TeethBatch::default() };
    let mut fired_after: HashSet<String> = HashSet::new();
    // Per item: the seq of its last write at or before the cutoff, and of its
    // first write ever (the key of an item born after the cutoff).
    let mut last_write_by_cutoff: HashMap<&str, i64> = HashMap::new();
    let mut first_write: HashMap<&str, i64> = HashMap::new();
    for e in &events {
        if e.kind == EventKind::ItemServed && e.seq > cutoff_seq {
            fired_after.insert(e.entity_id.clone());
        }
        if !writes_the_item(e.kind) {
            continue;
        }
        first_write.entry(e.entity_id.as_str()).and_modify(|s| *s = (*s).min(e.seq)).or_insert(e.seq);
        if e.seq <= cutoff_seq {
            last_write_by_cutoff.entry(e.entity_id.as_str()).and_modify(|s| *s = (*s).max(e.seq)).or_insert(e.seq);
        }
    }

    // Pool 1: fired since the cutoff and owed; pool 2: did not fire since the
    // cutoff and owed. Each (key, id, why).
    let mut fired_owed: Vec<(i64, String, String)> = Vec::new();
    let mut not_fired_owed: Vec<(i64, String, String)> = Vec::new();
    let mut settled: Vec<String> = Vec::new();
    for (id, _) in all_live_items(store)
        .into_iter()
        .filter(|(_, item)| item.kind.can_fire())
        .filter(|(_, item)| project_applies_to(item.project.as_deref(), project))
    {
        let key = last_write_by_cutoff.get(id.as_str()).or_else(|| first_write.get(id.as_str())).copied().unwrap_or(i64::MAX);
        match settlement(store, &id, None) {
            Settlement::Owed { why } => {
                if fired_after.contains(&id) {
                    fired_owed.push((key, id, why));
                } else {
                    not_fired_owed.push((key, id, why));
                }
            }
            // Held by a machine blocker: not this filing's business, and
            // never listed as settled by work either.
            Settlement::Settled(Settled::Blocked(_)) => {}
            Settlement::Settled(_) => {
                if verdict_written_after(store, &id, cutoff_seq) {
                    settled.push(id);
                }
            }
        }
    }

    fired_owed.sort();
    not_fired_owed.sort();
    settled.sort();
    let mut batch = TeethBatch { settled, ..TeethBatch::default() };
    for (place, (_, id, why)) in fired_owed.into_iter().enumerate() {
        if place < EVALUATION_TEETH_BATCH {
            batch.owed_now.push((id, why));
        } else {
            batch.owed_later.push((id, why));
        }
    }
    let remaining = EVALUATION_TEETH_BATCH.saturating_sub(batch.owed_now.len());
    for (place, (_, id, why)) in not_fired_owed.into_iter().enumerate() {
        // See `TEETH_NOT_FIRED_RECENTLY_NOTE`'s own doc comment: named as a
        // filler, so the Stop hook and the filing gate can never say
        // anything different about the same rule for the same reason - both
        // read this one function.
        let why = format!("{TEETH_NOT_FIRED_RECENTLY_NOTE}{why}");
        if place < remaining {
            batch.owed_now.push((id, why));
        } else {
            batch.owed_later.push((id, why));
        }
    }
    batch
}

/// Whether the trial record `id`'s current verdict rests on was written after
/// `cutoff_seq` - "settled since the previous report".
fn verdict_written_after(store: &EventStore, id: &str, cutoff_seq: i64) -> bool {
    let Ok(events) = store.get_events_by_entity(id) else { return false };
    events.iter().rev().find(|e| matches!(trial_of(e), Ok(Some(_)))).is_some_and(|e| e.seq > cutoff_seq)
}

/// The command that shows THOR's trial of rule `id` without writing anything,
/// always with `--db` (F7). Add `--apply` to write the rule or the note it
/// decides.
///
/// Names `serve` by its full path when it sits next to the running program
/// (every installed THOR binary does); else the bare program name. See
/// `retest_command_with` for the form, which is the one the evaluation
/// routine's `allowed-tools` entry allows.
pub fn retest_command_for(db: Option<&Path>, id: &str) -> String {
    let name = format!("serve{}", std::env::consts::EXE_SUFFIX);
    let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join(&name))).filter(|p| p.is_file());
    retest_command_with(&beside.unwrap_or_else(|| PathBuf::from(name)), db, id)
}

/// The retest command for `serve` at `program`: `<serve> --db <store> retest
/// --id '<id>'`, in the SHELL-NEUTRAL form the evaluation routine's own
/// `allowed-tools` entry allows (`retest_allow_entry`) - forward slashes, no
/// PowerShell `&`, a path quoted only when it holds a space. Claude Code
/// matches a `Bash(...)` entry against the command text from its start, so a
/// command in PowerShell form (`& "C:\...\serve.exe" ...`) was refused again
/// and again in a headless session (C3b, E1) and could not be run at all.
pub fn retest_command_with(program: &Path, db: Option<&Path>, id: &str) -> String {
    format!("{} retest --id '{id}'", retest_prefix(program, db))
}

/// The `allowed-tools` entry that pre-approves `retest_command_with` (and the
/// same command with `--apply`, or any other id): `Bash(<serve> --db <store>
/// retest:*)`. One function shapes the entry and the printed command, so they
/// cannot drift apart; a test pins that the printed command starts with it.
pub fn retest_allow_entry(program: &Path, db: Option<&Path>) -> String {
    format!("Bash({} retest:*)", retest_prefix(program, db))
}

fn retest_prefix(program: &Path, db: Option<&Path>) -> String {
    let word = |text: String| {
        let text = text.replace('\\', "/");
        if text.chars().any(char::is_whitespace) {
            format!("\"{text}\"")
        } else {
            text
        }
    };
    format!("{} --db {}", word(program.display().to_string()), word(db_argument(db)))
}

/// One live open point, as handed to a reader: its id, its text, and whether
/// it is THOR's own (`machine`, with its subject and blocker code) or a legacy
/// one an agent filed before step 14 (with the carried count its tags record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPoint {
    pub id: String,
    pub text: String,
    /// A legacy point's `carried:<n>` tag; always 0 for THOR's own.
    pub carried: u32,
    /// Written by `eval-machine` on a reserved id: THOR's own point. Every
    /// other open point is legacy - it settles nothing and holds no filing.
    pub machine: bool,
    /// What a machine point is about (its `about:` tag).
    pub subject: Option<String>,
    /// The blocker a machine point holds rules by (its `blocker:` tag).
    pub blocker: Option<String>,
}

/// Whether the live head of `id` is one of THOR's own open points: a reserved
/// id whose current head was written by `eval-machine`. An item that merely
/// looks like one - the tags, or the id, written by anyone else before the
/// fence existed - is not.
pub fn is_machine_point(store: &EventStore, id: &str) -> bool {
    if !is_machine_point_id(id) {
        return false;
    }
    let Ok(events) = store.get_events_by_entity(id) else { return false };
    let heads = thor_core::cas::compute_head_sets(&events);
    let Some(set) = heads.get(id).filter(|set| set.heads.len() == 1) else { return false };
    set.heads
        .iter()
        .next()
        .and_then(|hash| events.iter().find(|e| &e.this_hash == hash))
        .is_some_and(|head| head.actor == EVAL_MACHINE_ACTOR && head.kind != EventKind::FactRetracted)
}

/// Every live open point scoped EXACTLY to `project` (an open point belongs
/// to the project it is about): THOR's own first, then the legacy ones, each
/// group oldest first (ascending creation seq; a failed seq lookup sorts an
/// item last rather than dropping it).
pub fn open_points(store: &EventStore, project: Option<&str>) -> Vec<OpenPoint> {
    let mut with_seq: Vec<(bool, i64, OpenPoint)> = all_live_items(store)
        .into_iter()
        .filter(|(_, item)| item.kind == Kind::Report)
        .filter(|(_, item)| item.project.as_deref() == project)
        .filter(|(_, item)| item.tags.iter().any(|t| t == OPEN_POINT_TAG))
        .map(|(id, item)| {
            let seq = store.get_events_by_entity(&id).ok().and_then(|e| e.first().map(|e| e.seq)).unwrap_or(i64::MAX);
            let machine = is_machine_point(store, &id);
            let tagged = |prefix: &str| item.tags.iter().find_map(|t| t.strip_prefix(prefix)).map(str::to_string);
            let point = OpenPoint {
                carried: if machine { 0 } else { open_point_carried_count(&item.tags) },
                subject: if machine { tagged(OPEN_POINT_ABOUT_PREFIX) } else { None },
                blocker: if machine { tagged(OPEN_POINT_BLOCKER_PREFIX) } else { None },
                machine,
                id,
                text: item.text,
            };
            (!machine, seq, point)
        })
        .collect();
    with_seq.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then_with(|| a.2.id.cmp(&b.2.id)));
    with_seq.into_iter().map(|(_, _, p)| p).collect()
}

/// The rules of `start`'s frozen batch and A4 list that `settlement` still
/// calls owed for this evaluation, each with its reason, in the order they
/// were frozen - what the filing gate holds that report to in teeth. Nothing
/// outside the frozen ids is read: a rule on a list that settles never pulls
/// the next owed rule in, and a rule the machine pass did not reach is not
/// this evaluation's.
pub fn evaluation_frozen_batch_owed(store: &EventStore, start: &EvaluationStart) -> Vec<(String, String)> {
    start
        .frozen
        .batch()
        .iter()
        .chain(start.frozen.a4_list())
        // Changed by another session's attested THOR call (step 24): settled
        // by that session and credited to it, never this evaluation's.
        .filter(|id| credited_to_other_session(store, start, id).is_none())
        .filter_map(|id| match settlement(store, id, Some(start)) {
            Settlement::Owed { why } => Some((id.clone(), why)),
            Settlement::Settled(_) => None,
        })
        .collect()
}

/// What the filing gate answers while THOR's machine pass has not finished
/// for this evaluation (its start record holds no batch yet).
pub const STILL_DECIDING: &str =
    "THOR is still deciding the old rules - end this turn once and the start note will list what is yours to do";

/// THE GATE ITSELF. `None` for every write that is not an evaluation report
/// (`item.kind != Kind::Report`, or a `Report` with no
/// `EVALUATION_REPORT_TAG`) - untouched. For one that is, every condition is
/// computed fresh from the event log; `Some(refusal)` names every condition
/// that is not yet met, with the concrete action that settles it, never just
/// the first one found (the same "collect every problem, not just the first"
/// stance `gate::declare_inner`'s own doc comment explains and measures).
///
/// IN ORDER (step 15):
/// - `ctx: None` - the write brings no filing context (`FilingContext`): the
///   model CLI, a replica's drain, a one-off binary, a test without an
///   attested fixture, the tool server on a side door. REFUSED, never waived:
///   such a report could never count.
/// - the evaluation is void, or an item on its frozen lists (or its start
///   record) was written in its window outside an attested THOR call of the
///   evaluating session and not by one of THOR's own actors (D10): refused -
///   an evaluation whose list was changed behind it cannot count. The tool
///   server then closes the start record as void (`record_evaluation_void`).
/// - THOR's machine pass has not finished (the start record holds no batch:
///   the hook was killed between its two writes): nothing can be filed yet,
///   and the session's next Stop finishes the pass.
/// - the frozen judgement list (`evaluation_judgement_owed`) and the frozen
///   batch and A4 list (`evaluation_frozen_batch_owed`) are settled. Only the
///   frozen ids are read. Every verdict and every write that settles one is
///   judged by its RECORDED attestation; a write of the filing turn has none
///   yet, counts as not yet attested, and the refusal says to end the turn
///   once (`TURN_NOT_ENDED`).
fn check_evaluation_report_filing(store: &EventStore, item: &Item, ctx: Option<&FilingContext>) -> Option<Refusal> {
    if !gate::is_evaluation_report(item) {
        return None;
    }
    let Some(ctx) = ctx else {
        return Some(unattested_filing_refusal(
            "this write carries no filing context: no THOR tool call with an open evaluation behind it is making it",
        ));
    };
    let start = &ctx.start;
    if item.project.as_deref() != Some(start.frozen.project.as_str()) {
        return Some(Refusal {
            problem: format!(
                "this evaluation report is scoped to '{}', but the evaluation this session has open ('{}') is of '{}'",
                item.project.as_deref().unwrap_or("no project"),
                start.id,
                start.frozen.project
            ),
            fix: format!("file it with project '{}', the project the open evaluation is of", start.frozen.project),
        });
    }
    if let Some(void) = &start.frozen.void {
        return Some(changed_behind_refusal(void));
    }
    let written = frozen_writes_in_window(store, start, None);
    if let Some(outside) = written.outside.first() {
        return Some(changed_behind_refusal(outside));
    }
    if !start.frozen.decided() {
        return Some(Refusal {
            problem: format!("this evaluation report cannot be filed yet: {STILL_DECIDING}"),
            fix: "nothing to file yet: the next Stop finishes THOR's pass over the old rules and shows the start note \
                  with the batch - do what it lists, then file this report again"
                .to_string(),
        });
    }
    let mut missing: Vec<String> = Vec::new();

    let judgement = evaluation_judgement_owed(store, start, &item.tags);
    if !judgement.owed.is_empty() {
        let named = judgement.owed.iter().map(|id| format!("'{id}'")).collect::<Vec<_>>().join(", ");
        missing.push(format!(
            "{} item(s) fired in this session before this evaluation started (log position {}) and still owe a \
             judgement - each had fired in {JUDGEMENT_DEBT_AFTER}+ sessions since its last verdict by then: {named}. This \
             list was fixed when the evaluation started and cannot grow: nothing that fires after that, and nothing \
             that fired only in another session, is this report's to settle. Only a verdict this session gives \
             through the THOR tool itself counts - one from another session, or from a shell or a script, settles \
             nothing - and a verdict given in this turn counts once the turn has ended (THOR checks it against the \
             session's transcript at the Stop), so end the turn once before filing again. For each one, read where \
             it fired (`get` shows the item) and decide whether it belonged there: mcp__thor__mark(id: \"<id>\") when it \
             did, mcp__thor__mark(id: \"<id>\", noise: true) when it did not. If you cannot honestly judge one \
             here, do not guess - add the tag '{NOT_JUDGEABLE_TAG_PREFIX}<id>:<reason>' to this report (a reason \
             of at least {NOT_JUDGEABLE_REASON_MIN_CHARS} characters); it then stays owed store-wide instead. A \
             verdict you did not read the item for is a false record, and it is worse than the debt",
            judgement.owed.len(),
            start.frozen.started_seq
        ));
    }

    // THE BATCH AND THE A4 LIST: the ones the start record holds - a rule on
    // them that settles never pulls the next owed rule in, so the lists the
    // session was shown at the start are the whole of what it is held to.
    let owed_now = evaluation_frozen_batch_owed(store, start);
    if !owed_now.is_empty() {
        let named = owed_now.iter().map(|(id, why)| format!("'{id}' ({why})")).collect::<Vec<_>>().join(", ");
        let later = match start.frozen.batch_later {
            0 => String::new(),
            n => format!(
                " {n} more rule(s) in the same state are not this report's to settle: they stay owed, doctor names \
                 them, and a later evaluation asks for them first."
            ),
        };
        let first = &owed_now[0].0;
        missing.push(format!(
            "{} rule(s) on this evaluation's frozen lists (the batch of at most {EVALUATION_TEETH_BATCH} rules that \
             need your fix and the A4 list of at most {EVALUATION_A4_LIST} whose text must name its check's literal, \
             both fixed by THOR's pass at the start) apply to this project (its own or global) and have no current \
             verdict from THOR's trial: {named}.{later} What settles one, and nothing else does: THOR's trial, run on \
             the rule as it stands. See what it decides with `{}` - it prints the registered working copy it used, \
             every attempt, and a rule, a note, or the one fix you have to make yourself - then run the same command \
             with --apply to write the rule or the note it decided. Or mcp__thor__revise with only the rule's id, \
             which runs the same trial and writes its verdict. A tag, a reason, an open point, a refusal on record or \
             a probe settles nothing. Retract the rule with a reason if it is simply wrong",
            owed_now.len(),
            retest_command_for(store.path().as_deref(), first)
        ));
    }

    // THE DISPUTES TO VERIFY: the ones the start record froze. Settled by the
    // agent's own revise of the rule - the real check, or its reason restated.
    let disputes_owed = evaluation_frozen_disputes_owed(store, start);
    if !disputes_owed.is_empty() {
        let named = disputes_owed.iter().map(|(id, why)| format!("'{id}' ({why})")).collect::<Vec<_>>().join(", ");
        missing.push(format!(
            "{} dispute(s) on this evaluation's frozen dispute list (at most {EVALUATION_DISPUTE_LIST}, fixed by THOR's \
             pass at the start) have not been verified: {named}. A dispute named a span that may change or a call that \
             is legitimate, and that is why the rule is a note or refuses real calls; an audit found 7 of 11 such \
             disputes wrong. For each one open the rule and the file or command it is about and decide whether the \
             disputed literal IS what the rule is about. If it is, hang the real check with mcp__thor__revise(id: \"<id>\", \
             check_kind: \"forbidden\", check_literals: [\"<the literal>\"]) - or check_kind \"contains\" with \
             check_path and check_literal for a file - and name the literal in the rule's text. If it is only an \
             incidental span, confirm it with mcp__thor__revise(id: \"<id>\", because: \"<why this span may change \
             while the rule stays true, at least {DISPUTE_REASON_MIN_CHARS} characters>\"). Either call settles it; a \
             tag, a note elsewhere or a report that says it was checked settles nothing",
            disputes_owed.len()
        ));
    }

    // WRITES OF THE FILING TURN: stamped as this session's own THOR calls,
    // not verified yet. They count as not yet attested, and ending the turn
    // once is the whole of what settles them.
    let mut unverified: Vec<String> =
        judgement.unverified.iter().map(|(id, seq)| format!("'{id}' (its verdict at log seq {seq})")).collect();
    unverified.extend(written.pending.iter().map(|(id, seq)| format!("'{id}' (written at log seq {seq})")));
    if !unverified.is_empty() {
        missing.push(format!(
            "{} write(s) of this turn on this evaluation's lists are not checked yet: {}. {TURN_NOT_ENDED} - THOR reads \
             each call back from this session's transcript at the Stop, and until then it counts as not yet attested. \
             (When you filed, THOR already tried to read each rule write back from the transcript and attested those \
             it could; what is named here is a verdict, which waits for the Stop by design, or a call whose lines \
             were not all on disk yet)",
            unverified.len(),
            unverified.join(", ")
        ));
    }
    // WRITES OF ANOTHER SESSION (step 24): an attested one is credited to
    // that session and voids nothing; one not checked yet is checked against
    // that session's transcript at THIS session's next Stop.
    if !written.foreign_pending.is_empty() {
        let named = written
            .foreign_pending
            .iter()
            .map(|(id, seq, other)| format!("'{id}' (written at log seq {seq} by session {other})"))
            .collect::<Vec<_>>()
            .join(", ");
        missing.push(format!(
            "{} write(s) by another session on this evaluation's lists are not checked yet: {named}. THOR checks each \
             one against that session's transcript at this session's next Stop: end the turn once, then file again - \
             an attested one counts as settled by that session, never as a void of this evaluation",
            written.foreign_pending.len()
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
              in bulk. For a write that is only waiting to be checked, ending the turn once is the whole of it"
            .to_string(),
    })
}

/// The filing gate's answer for `item` under `ctx`, exactly as a filing would
/// get it (`check_evaluation_report_filing`), without writing anything. For
/// the Stop-time validator (step 16), which recomputes it on the log
/// truncated at the filing's own log position (`EventStore::truncated_copy`)
/// with the evaluation start as it stood there.
pub fn evaluation_filing_refusal(store: &EventStore, item: &Item, ctx: Option<&FilingContext>) -> Option<Refusal> {
    check_evaluation_report_filing(store, item, ctx)
}

/// What an agent text that carries one of THOR's two section headers is told
/// at the filing.
fn typed_header_refusal() -> Refusal {
    Refusal {
        problem: format!(
            "the text of this evaluation report carries a section header only THOR writes ('{}' or '{}')",
            gate::MACHINE_SECTION_MARK,
            gate::AGENT_SECTION_BEGIN
        ),
        fix: "take those lines out and send only your own TLDR and remarks: THOR writes the machine section above \
              them itself, from its log, when the report is filed"
            .to_string(),
    }
}

/// THE FILING OF AN EVALUATION REPORT (step 15). `Ok(None)` for a write that
/// is not one. For one that is: the filing gate
/// (`check_evaluation_report_filing`), and - when it passes - the item as it
/// is STORED: THOR's machine section for this evaluation
/// (`render_machine_section`, derived from the log up to its tip right now),
/// a blank line, `gate::AGENT_SECTION_BEGIN`, a newline, and the writer's own
/// text. `carried` is the machine block the report had before, for a revise
/// that files an existing report again: when the writer's text still opens
/// with it, only what follows it is the writer's own. A writer's text that
/// carries either header is refused - only THOR writes them.
fn filed_evaluation_report(
    store: &EventStore,
    item: &Item,
    carried: Option<&str>,
    ctx: Option<&FilingContext>,
) -> Result<Option<Item>, Refusal> {
    if !gate::is_evaluation_report(item) {
        return Ok(None);
    }
    let own_text = match carried {
        Some(block) if item.text.starts_with(block) => &item.text[block.len()..],
        _ => item.text.as_str(),
    };
    if ctx.is_some() && gate::names_a_section_header(own_text) {
        return Err(typed_header_refusal());
    }
    if let Some(refusal) = check_evaluation_report_filing(store, item, ctx) {
        return Err(refusal);
    }
    let Some(ctx) = ctx else { return Ok(None) };
    let filed_seq = store.get_next_seq().map(|next| next - 1).unwrap_or(ctx.start.frozen.started_seq);
    let machine = render_machine_section(store, &ctx.start, filed_seq);
    let mut filed = item.clone();
    filed.text = format!("{machine}\n\n{}\n{own_text}", gate::AGENT_SECTION_BEGIN);
    Ok(Some(filed))
}

// ------------------------------------------------ the machine section (step 15, B1)
//
// WHAT IT CLOSES. An evaluation report was whatever its session typed.
// Measured on a store copy: get showed "TLDR: evaluation of fxp. Nothing to
// add." and nothing else, and a revise could rewrite all of it. Now the first
// part of every filed report is written by THOR, from its event log alone:
// what was on the frozen lists, who judged what, how each rule settled, what
// was retracted or weakened in the window, and every write that came in
// through a side door.
//
// PURE, FROM THE LOG TRUNCATED AT F. `render_machine_section(store, start,
// filed_seq)` reads only events with seq <= `filed_seq` - the attester's
// recorded verifications written before it included - and never a live
// transcript, a file of the working copy or the clock. So the filing and the
// Stop-time validation (step 16) render the same bytes, and a later event
// changes nothing about a section already written. `filed_seq` (F) is the
// log's tip when the filing was made; the report's own event comes after it.

/// The file a check reads, if it reads one.
pub fn check_file_of(check: &Check) -> Option<&str> {
    match check {
        Check::Contains { path, .. } | Check::Absent { path, .. } | Check::AbsentAll { path, .. } | Check::PathExists { path } => Some(path),
        _ => None,
    }
}

/// A check in a few plain words: its kind and the file it reads, if any.
pub fn check_in_words(check: &Check) -> String {
    let kind = serde_json::to_value(check)
        .ok()
        .and_then(|v| v.as_object().and_then(|o| o.keys().next().cloned()))
        .unwrap_or_else(|| "check".to_string());
    match check_file_of(check) {
        Some(path) => format!("{kind} {path}"),
        None => kind,
    }
}

/// A check in full plain words: its kind, its literals and the file it
/// reads - "forbidden '--force'", "contains 'only-owner' in files/INGRESS.md".
/// A literal outside plain ASCII is named by its code point.
pub fn check_named(check: &Check) -> String {
    let lit = |l: &str| format!("'{}'", gate::shown_literal(l));
    let all = |ls: &[String]| ls.iter().map(|l| lit(l)).collect::<Vec<_>>().join(", ");
    match check {
        Check::PathExists { path } => format!("path_exists {path}"),
        Check::Contains { path, literal } => format!("contains {} in {path}", lit(literal)),
        Check::Absent { path, literal } => format!("absent {} in {path}", lit(literal)),
        Check::AbsentAll { path, literals } => format!("absent_all {} in {path}", all(literals)),
        Check::Forbidden { literals } => format!("forbidden {}", all(literals)),
        Check::Requires { when, required } => format!("requires {} on '{when}'", all(required)),
    }
}

/// What a filing's receipt says about its machine section, in numbers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachineSummary {
    /// N: where the evaluation started.
    pub started_seq: i64,
    /// F: the log's tip the section was derived at.
    pub filed_seq: i64,
    /// How many items the frozen judgement list held.
    pub listed: usize,
    pub useful: usize,
    pub noise: usize,
    /// No verdict from this evaluation: skipped with a reason on the report.
    pub skipped: usize,
    /// Retracted, archived or moved to a Report by an attested call.
    pub gone: usize,
    /// Pinned since the start: owes no verdict.
    pub pinned: usize,
    /// How many rules the frozen batch and A4 list held together.
    pub teeth_listed: usize,
    /// Writes in the window outside an attested THOR call.
    pub side_doors: usize,
    /// Writes in the window whose check is still to come.
    pub unverified: usize,
    /// Rules whose check an agent cleared, moved or changed in the window.
    pub checks_weakened: usize,
    /// Judgement items another session's attested THOR call changed in the
    /// window: settled by that session, credited to it (step 24).
    pub credited: usize,
}

/// One line of machine-section text out of words some writer chose (an id, a
/// reason, a door): plain ASCII on one line, at most 200 characters. A
/// character outside printable ASCII is written as its code point, so the
/// section never carries a glyph, a line break or a control character.
fn plain(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c == ' ' || c.is_ascii_graphic() {
            out.push(c);
        } else if c.is_whitespace() || c.is_control() {
            out.push(' ');
        } else {
            out.push_str(&format!("U+{:04X}", c as u32));
        }
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 200 {
        format!("{}...", collapsed.chars().take(200).collect::<String>())
    } else {
        collapsed
    }
}

/// Who made an attested write: the main session, or one of its subagents.
fn by_whom(agent_id: &Option<String>) -> String {
    match agent_id {
        Some(agent) => format!("agent {}", plain(agent)),
        None => "the main session".to_string(),
    }
}

/// The door of a write, in words for the machine section.
fn door_words(door: &WriteDoor) -> String {
    match door {
        WriteDoor::Machine => "THOR itself".to_string(),
        WriteDoor::Attested { agent_id } => format!("an attested THOR call of {}", by_whom(agent_id)),
        WriteDoor::Pending => "a THOR call of this session, checked at the end of this turn".to_string(),
        WriteDoor::OtherSession { session, .. } => {
            format!("an attested THOR call of another session, {} (credited to that session)", plain(session))
        }
        WriteDoor::OtherSessionPending { session } => {
            format!("a THOR call of another session, {}, not checked yet", plain(session))
        }
        WriteDoor::Outside(door) => plain(door),
    }
}

/// A severity in words.
fn severity_name(severity: Option<Severity>) -> &'static str {
    match severity {
        Some(Severity::Irreversible) => "irreversible",
        Some(Severity::Costly) => "costly",
        Some(Severity::HouseStyle) => "house_style",
        None => "none",
    }
}

/// The log as it stood at log seq `until`: each entity's own events, read
/// once (indexed) and truncated there.
struct LogAt<'a> {
    store: &'a EventStore,
    until: i64,
    entities: std::cell::RefCell<HashMap<String, std::rc::Rc<Vec<Event>>>>,
}

impl<'a> LogAt<'a> {
    fn new(store: &'a EventStore, until: i64) -> Self {
        LogAt { store, until, entities: std::cell::RefCell::new(HashMap::new()) }
    }

    /// Every event on `id` at or before `until`, oldest first.
    fn entity(&self, id: &str) -> std::rc::Rc<Vec<Event>> {
        if let Some(found) = self.entities.borrow().get(id) {
            return found.clone();
        }
        let events: Vec<Event> =
            self.store.get_events_by_entity(id).unwrap_or_default().into_iter().filter(|e| e.seq <= self.until).collect();
        let events = std::rc::Rc::new(events);
        self.entities.borrow_mut().insert(id.to_string(), events.clone());
        events
    }
}

/// The item `events` (one entity's, oldest first) held just before log seq
/// `before`: the body of its last write that parses as one. `None` when it
/// had none, or its last write then was a retraction.
fn item_before(events: &[Event], before: i64) -> Option<Item> {
    let last = events.iter().filter(|e| e.seq < before && changes_an_item(e.kind)).next_back()?;
    if last.kind == EventKind::FactRetracted {
        return None;
    }
    events.iter().filter(|e| e.seq <= last.seq && changes_an_item(e.kind)).filter_map(|e| parse_body(&e.body).ok()).next_back()
}

/// How one rule on the frozen batch or A4 list stood at F, from its own
/// events alone.
fn settled_how(events: &[Event], started_seq: i64, session: &str) -> String {
    let verdicts = recorded_write_verdicts(events);
    let window: Vec<&Event> = events.iter().filter(|e| e.seq > started_seq && changes_an_item(e.kind)).collect();
    let door = |e: &Event| door_words(&write_door(e, &verdicts, session));
    if let Some(gone) = window.iter().rev().find(|e| e.kind == EventKind::FactRetracted) {
        if events.iter().filter(|e| changes_an_item(e.kind)).next_back().is_some_and(|last| last.seq == gone.seq) {
            return format!("retracted at log seq {} by {} - reason: {}", gone.seq, door(gone), plain(&tombstone_reason(&gone.body)));
        }
    }
    let now = item_before(events, i64::MAX);
    if let Some(item) = now.as_ref().filter(|item| !item.kind.can_fire()) {
        let seq = window.last().map(|e| e.seq).unwrap_or(0);
        let door = window.last().map(|e| door(e)).unwrap_or_default();
        return format!("taken off the rule track at log seq {seq} by {door} (now a {:?})", item.kind);
    }
    let tried = window.iter().rev().find_map(|e| match trial_of(e) {
        Ok(Some(record)) => Some((*e, record)),
        _ => None,
    });
    match (tried, now) {
        (Some((event, record)), Some(item)) if record.fields().fingerprint == fingerprint(&item) => {
            let verdict = match record.fields().verdict {
                RecordedVerdict::RuleProven => "a rule (proven)",
                RecordedVerdict::RuleWired => "a rule (wired, not proven)",
                RecordedVerdict::Note => "a note",
            };
            format!("{verdict} by THOR's trial at log seq {}, written by {}", event.seq, door(event))
        }
        (Some((event, _)), _) => format!("tried at log seq {} and rewritten since - no verdict stands for it at the filing", event.seq),
        _ => "no write in the window settled it - the filing gate read its check in the registered working copy".to_string(),
    }
}

/// THE MACHINE SECTION of an evaluation report (see this section's banner),
/// as text: `gate::machine_section_begin(N, F)` first, then the header line,
/// JUDGEMENT, TEETH, OPEN POINTS, REFUSED and SIDE DOORS. Pure: the same
/// `start` and `filed_seq` on the same log give the same bytes, whatever was
/// appended after `filed_seq`.
/// The machine section's line for each earlier round of `start`'s project
/// (step 26): a start before it, of the same project, that started within
/// `EVALUATION_OPEN_HOURS` before it and was closed as void or superseded on
/// the log at F - with its session's attested verdicts and edits between its
/// own N and this start's N. Pure, from the log up to F.
fn earlier_rounds(store: &EventStore, log: &LogAt, start: &EvaluationStart, filed_seq: i64) -> Vec<String> {
    let frozen = &start.frozen;
    let Ok(events) = store.events_of_kinds(&[EventKind::FactCreated, EventKind::FactRevised], 0) else { return Vec::new() };
    let events: Vec<Event> = events.into_iter().filter(|e| e.seq <= filed_seq).collect();
    let from_unix = frozen.started_unix - EVALUATION_OPEN_HOURS * 3600;
    let mut lines = Vec::new();
    for round in starts_in(&events) {
        let closed = round.frozen.is_void() || round.frozen.is_superseded();
        if round.frozen.project != frozen.project || round.seq >= start.seq || round.frozen.started_unix < from_unix || !closed {
            continue;
        }
        let session = round.frozen.session.as_str();
        let how = match (&round.frozen.void, &round.frozen.superseded) {
            (Some(void), _) => format!("closed as void: '{}' was changed at log seq {}", plain(&void.id), void.seq),
            (None, Some(sup)) => format!("superseded by session {}", plain(&sup.by_session)),
            (None, None) => continue,
        };
        let (mut useful, mut noise, mut edits) = (0usize, 0usize, 0usize);
        let range = store.events_since(round.frozen.started_seq).unwrap_or_default();
        for event in range.iter().filter(|e| e.seq <= frozen.started_seq) {
            let verdict = matches!(event.kind, EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise);
            if !verdict && !changes_an_item(event.kind) {
                continue;
            }
            if event.entity_id.starts_with(EVALUATION_START_PREFIX) {
                continue;
            }
            let entity = log.entity(&event.entity_id);
            let verdicts = recorded_write_verdicts(&entity);
            if !matches!(write_door(event, &verdicts, session), WriteDoor::Attested { .. }) {
                continue;
            }
            match event.kind {
                EventKind::ItemMarkedUseful => useful += 1,
                EventKind::ItemMarkedNoise => noise += 1,
                _ => edits += 1,
            }
        }
        lines.push(format!(
            "\n- '{}' by session {} ({how}): {} verdict(s) ({useful} useful, {noise} noise) and {edits} edit(s) by that \
             session between log seq {} and {}.",
            plain(&round.id),
            plain(session),
            useful + noise,
            round.frozen.started_seq,
            frozen.started_seq
        ));
    }
    lines
}

/// THE DISPUTES OF THE WINDOW, from the log, one line per item: every note by
/// dispute and every rule kept with accept_refusing written between the start
/// and the filing, with the argument, the calls or spans it names and the
/// writer's own reason ("no reason recorded" when there is none - a dispute
/// from before a reason was required, or a call that gave none). When the
/// agent revised the rule with a reason afterwards, the line says so: that is
/// the dispute verified. The evaluating session types none of it.
fn disputes_in_window(window: &[Event], log: &LogAt, project: Option<&str>, session: &str) -> String {
    let mut disputes: Vec<String> = Vec::new();
    let mut accepted: Vec<String> = Vec::new();
    for event in window.iter().filter(|e| matches!(e.kind, EventKind::FactRevised | EventKind::FactCreated)) {
        let (Some(item), Ok(Some(record))) = (parse_body(&event.body).ok(), trial_of(event)) else { continue };
        if !project_applies_to(item.project.as_deref(), project) {
            continue;
        }
        let fields = record.fields();
        let by_dispute =
            fields.verdict == RecordedVerdict::Note && (!fields.disputes.legitimate_calls.is_empty() || !fields.disputes.may_change.is_empty());
        let kept = !fields.accept_refusing.is_empty();
        if !by_dispute && !kept {
            continue;
        }
        let events = log.entity(&event.entity_id);
        let verdicts = recorded_write_verdicts(&events);
        let door = door_words(&write_door(event, &verdicts, session));
        let reason = dispute_reason(&events, event.seq).map(|r| plain(&r)).unwrap_or_else(|| "no reason recorded".to_string());
        let verified = dispute_answered_since(&events, event.seq)
            .map(|e| {
                format!(
                    " - since then verified at log seq {} by {}: {}",
                    e.seq,
                    door_words(&write_door(e, &verdicts, session)),
                    plain(&because_reason(&e.body).unwrap_or_default())
                )
            })
            .unwrap_or_default();
        let id = plain(&event.entity_id);
        let named = |names: &[&str]| {
            let used: Vec<(String, Vec<String>)> =
                dispute_arguments(fields).into_iter().filter(|(name, _)| names.contains(&name.as_str())).collect();
            dispute_in_words(&used)
        };
        if by_dispute {
            let what = named(&["legitimate_calls", "may_change"]);
            disputes.push(format!("\n- '{id}' at log seq {} by {door}: {what} - because: {reason}{verified}", event.seq));
        }
        if kept {
            let what = named(&["accept_refusing"]);
            accepted.push(format!("\n- '{id}' at log seq {} by {door}: {what} - because: {reason}{verified}", event.seq));
        }
    }
    let mut out = format!("\nnotes by dispute written in the window: {}.", disputes.len());
    disputes.iter().for_each(|line| out.push_str(line));
    out.push_str(&format!("\nrules kept with accept_refusing written in the window: {}.", accepted.len()));
    accepted.iter().for_each(|line| out.push_str(line));
    out
}

pub fn render_machine_section(store: &EventStore, start: &EvaluationStart, filed_seq: i64) -> String {
    machine_section(store, start, filed_seq).0
}

/// The numbers of the machine section `render_machine_section` writes for the
/// same arguments - what a filing's receipt summarises.
pub fn machine_section_summary(store: &EventStore, start: &EvaluationStart, filed_seq: i64) -> MachineSummary {
    machine_section(store, start, filed_seq).1
}

fn machine_section(store: &EventStore, start: &EvaluationStart, filed_seq: i64) -> (String, MachineSummary) {
    let frozen = &start.frozen;
    let n = frozen.started_seq;
    let session = frozen.session.as_str();
    let project = Some(frozen.project.as_str());
    let log = LogAt::new(store, filed_seq);
    let window: Vec<Event> =
        store.events_since(n).unwrap_or_default().into_iter().filter(|e| e.seq <= filed_seq).collect();
    let mut summary = MachineSummary { started_seq: n, filed_seq, listed: frozen.judgement.len(), ..MachineSummary::default() };
    let mut out = gate::machine_section_begin(n, filed_seq);
    out.push_str("\nEvery line of this machine section was computed by THOR from its event log; the evaluating session typed none of it.");

    // ---- header: who took part, from the recorded verifications in the window
    let mut agents: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut main_seen = false;
    for event in window.iter().filter(|e| e.kind == EventKind::GateStoodAside && e.actor == ATTESTER_ACTOR) {
        let Ok(body) = serde_json::from_str::<serde_json::Value>(&event.body) else { continue };
        let attested = body.get("attested").and_then(|v| v.as_bool()).unwrap_or(false);
        if !attested || body.get("session").and_then(|v| v.as_str()) != Some(session) {
            continue;
        }
        match body.get("agent_id").and_then(|v| v.as_str()) {
            Some(agent) => {
                agents.insert(plain(agent));
            }
            None => main_seen = true,
        }
    }
    let mut who: Vec<String> = Vec::new();
    if main_seen || agents.is_empty() {
        who.push("the main session".to_string());
    }
    who.extend(agents.iter().map(|a| format!("agent {a}")));
    out.push_str(&format!(
        "\nEVALUATION: project '{}'; session {} ({}); started by {} (THOR's record '{}', log seq {}); N = {n}, F = {filed_seq}.",
        plain(&frozen.project),
        plain(session),
        who.join(", "),
        plain(&frozen.started_by),
        plain(&start.id),
        start.seq
    ));

    // ---- EARLIER ROUNDS (step 26, field test 2): an earlier evaluation of
    // this project, closed as void or superseded before this one started and
    // within its open window, so restarted evaluations never hide the work
    // done in the round before. Only when there is one: a section without
    // earlier rounds is byte for byte what it always was.
    let earlier = earlier_rounds(store, &log, start, filed_seq);
    if !earlier.is_empty() {
        out.push_str(&format!(
            "\nEARLIER ROUNDS of this project closed before this one: {}. Their verdicts and edits stay on record; this \
             evaluation does not judge them again.",
            earlier.len()
        ));
        earlier.iter().for_each(|line| out.push_str(line));
    }

    // ---- JUDGEMENT
    out.push_str(&format!("\n\nJUDGEMENT: {} item(s) on the frozen list.", frozen.judgement.len()));
    for id in &frozen.judgement {
        let events = log.entity(id);
        let verdicts = recorded_write_verdicts(&events);
        let in_window = |e: &&Event| e.seq > n;
        let mark = events
            .iter()
            .filter(in_window)
            .filter(|e| matches!(e.kind, EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise))
            .filter_map(|e| match write_door(e, &verdicts, session) {
                WriteDoor::Attested { agent_id } => Some((e, agent_id)),
                _ => None,
            })
            .next_back();
        let credited = if mark.is_none() { credited_in(&events, n, session) } else { None };
        let line = if let Some((other, seq)) = credited {
            summary.credited += 1;
            format!(
                "settled by session {} - an attested THOR call of that session changed it at log seq {seq} (credited to \
                 that session; not this evaluation's to judge)",
                plain(&other)
            )
        } else if let Some((event, agent_id)) = mark {
            let word = if event.kind == EventKind::ItemMarkedUseful {
                summary.useful += 1;
                "useful"
            } else {
                summary.noise += 1;
                "noise"
            };
            format!("{word} - verdict at log seq {} by {}", event.seq, by_whom(&agent_id))
        } else {
            let at_start = item_before(&events, n + 1);
            let at_filing = item_before(&events, i64::MAX);
            let off_track = match (&at_start, &at_filing) {
                (_, None) => true,
                (Some(before), Some(now)) => before.kind != Kind::Report && now.kind == Kind::Report,
                (None, Some(_)) => false,
            };
            let last_write = events.iter().filter(in_window).filter(|e| changes_an_item(e.kind)).next_back();
            match (off_track, last_write, &at_filing) {
                (true, Some(event), _) if matches!(write_door(event, &verdicts, session), WriteDoor::Attested { .. }) => {
                    summary.gone += 1;
                    let how = if event.kind == EventKind::FactRetracted {
                        format!("retracted - reason: {}", plain(&tombstone_reason(&event.body)))
                    } else {
                        "taken off the rule track".to_string()
                    };
                    format!("{how}, at log seq {} by {}", event.seq, door_words(&write_door(event, &verdicts, session)))
                }
                (false, _, Some(now)) if now.bindings.iter().any(|b| matches!(b, Binding::Always)) => {
                    summary.pinned += 1;
                    "pinned - owes no verdict".to_string()
                }
                _ => {
                    summary.skipped += 1;
                    "skipped - no verdict from this evaluation (the report's own not-judgeable tag gives the reason)".to_string()
                }
            }
        };
        out.push_str(&format!("\n- '{}': {line}", plain(id)));
    }
    let share = match summary.useful + summary.noise {
        0 => "no verdict was given".to_string(),
        given => format!("{}% useful of {given} verdict(s)", (summary.useful * 100 + given / 2) / given),
    };
    let credited = match summary.credited {
        0 => String::new(),
        n => format!(", {n} settled by another session's attested call"),
    };
    out.push_str(&format!(
        "\ntotals: {} useful, {} noise, {} skipped, {} retracted or taken off the rule track, {} pinned{credited}; {share}.",
        summary.useful, summary.noise, summary.skipped, summary.gone, summary.pinned
    ));

    // ---- TEETH
    out.push_str("\n\nTEETH: ");
    match &frozen.machine {
        Some(machine) => out.push_str(&format!(
            "THOR tried {} old rule(s) itself at the start (time box {} s): {} bite, {} note(s), {} pending check(s) hung, {} \
             blocked, {} need the agent, {} need only their literal named (A4), {} tried again and left as they were, not reached: {}.",
            machine.items.len(),
            machine.box_secs,
            machine.bites,
            machine.notes,
            machine.pending_hung,
            machine.blocked,
            machine.needs_agent,
            machine.a4_only,
            machine.left_as_they_were(),
            machine.not_reached
        )),
        None => out.push_str("THOR's pass over the old rules left no record."),
    }
    summary.teeth_listed = frozen.batch().len() + frozen.a4_list().len();
    for (name, ids) in [("frozen batch", frozen.batch()), ("frozen A4 list", frozen.a4_list())] {
        out.push_str(&format!("\n{name}: {} rule(s).", ids.len()));
        for id in ids {
            out.push_str(&format!("\n- '{}': {}", plain(id), settled_how(&log.entity(id), n, session)));
        }
    }
    // Every rule of this project (its own or global) retracted, archived,
    // taken off the rule track or given a lower severity in the window; every
    // note by dispute and every accept_refusing written in it.
    let mut weakened: Vec<String> = Vec::new();
    // Every rule whose CHECK an agent cleared or changed in the window (a
    // weakening the gate lets through with a because, step 22 - C3b B4
    // cleared a true check and the section said 0). THOR's own verdicts clear
    // a check too (a note, by its trial) and are the pass's, so only a write
    // by someone else is listed.
    let mut checks: Vec<String> = Vec::new();
    for event in window.iter().filter(|e| matches!(e.kind, EventKind::FactRevised | EventKind::FactRetracted | EventKind::FactCreated)) {
        let events = log.entity(&event.entity_id);
        let verdicts = recorded_write_verdicts(&events);
        let door = door_words(&write_door(event, &verdicts, session));
        let id = plain(&event.entity_id);
        let after = parse_body(&event.body).ok();
        if event.kind == EventKind::FactCreated {
            continue;
        }
        let Some(before) = item_before(&events, event.seq) else { continue };
        if !before.kind.can_fire() || !project_applies_to(before.project.as_deref(), project) {
            continue;
        }
        let because = || plain(&because_reason(&event.body).unwrap_or_else(|| "no reason recorded".to_string()));
        if event.kind == EventKind::FactRevised && !RESERVED_ACTORS.contains(&event.actor.as_str()) {
            if let Some(after) = after.as_ref().filter(|a| a.kind.can_fire()) {
                let changed = gate::weakenings(&before, after).iter().any(|w| matches!(w.label, "check cleared" | "check changed"));
                if let (true, Some(old)) = (changed, &before.check) {
                    let what = match &after.check {
                        None => format!("check cleared ({})", plain(&check_in_words(old))),
                        Some(new) if check_file_of(old) != check_file_of(new) => {
                            format!("check moved from {} to {}", plain(&check_in_words(old)), plain(&check_in_words(new)))
                        }
                        Some(new) => format!("check changed from {} to {}", plain(&check_in_words(old)), plain(&check_in_words(new))),
                    };
                    checks.push(format!("\n- '{id}': {what} at log seq {} by {door} - reason: {}", event.seq, because()));
                }
            }
        }
        // A RETRACT TAKES ITS CHECK WITH IT (step 26, field test 2): a fold
        // that retracted a rule whose check still held listed it only as
        // "retracted", so the lost guard was invisible. Listed here too.
        if event.kind == EventKind::FactRetracted && !RESERVED_ACTORS.contains(&event.actor.as_str()) {
            if let Some(old) = &before.check {
                checks.push(format!(
                    "\n- '{id}': check lost with the retract ({}) at log seq {} by {door} - reason: {}",
                    plain(&check_in_words(old)),
                    event.seq,
                    plain(&tombstone_reason(&event.body))
                ));
            }
        }
        if event.kind == EventKind::FactRetracted {
            weakened.push(format!(
                "\n- '{id}': retracted at log seq {} by {door} - reason: {}",
                event.seq,
                plain(&tombstone_reason(&event.body))
            ));
        } else if let Some(after) = after {
            if !after.kind.can_fire() {
                let reason = after
                    .tags
                    .iter()
                    .filter(|t| !before.tags.contains(t))
                    .find_map(|t| t.strip_prefix("archived:"))
                    .map(plain)
                    .unwrap_or_else(because);
                let word = if after.tags.iter().any(|t| t.starts_with("archived:") && !before.tags.contains(t)) {
                    "archived"
                } else {
                    "taken off the rule track"
                };
                weakened.push(format!("\n- '{id}': {word} at log seq {} by {door} - reason: {reason}", event.seq));
            } else if crate::item::severity_rank(after.severity) > crate::item::severity_rank(before.severity) {
                weakened.push(format!(
                    "\n- '{id}': severity lowered from {} to {} at log seq {} by {door} - reason: {}",
                    severity_name(before.severity),
                    severity_name(after.severity),
                    event.seq,
                    because()
                ));
            }
        }
    }
    out.push_str(&format!(
        "\nrules retracted, archived, taken off the rule track or given a lower severity in the window: {}.",
        weakened.len()
    ));
    weakened.iter().for_each(|line| out.push_str(line));
    out.push_str(&format!(
        "\nrules whose check was cleared, moved or changed by an agent's write rather than THOR's own trial in the window: {}.",
        checks.len()
    ));
    checks.iter().for_each(|line| out.push_str(line));
    summary.checks_weakened = checks.len();
    out.push_str(&disputes_in_window(&window, &log, project, session));

    // ---- OPEN POINTS: THOR's own as the start's pass left them, each with
    // how many valid evaluations carried it (recorded by the validator,
    // step 16), then the legacy ones.
    let points = frozen.machine.as_ref().map(|m| m.points.as_slice()).unwrap_or(&[]);
    let legacy: Vec<&String> = frozen.open_points.iter().filter(|id| !is_machine_point_id(id)).collect();
    out.push_str(&format!("\n\nOPEN POINTS: {} of THOR's own, {} legacy.", points.len(), legacy.len()));
    for point in points {
        let carriers = carriers_of(store, &frozen.project, &point.blocker.subject, filed_seq);
        let carried = carriers.len();
        let queue = match carried {
            0 => String::new(),
            _ => format!(" - in the owner queue: the owner decides - carried by {}", carriers_in_words(&carriers)),
        };
        out.push_str(&format!(
            "\n- '{}' about '{}' ({}): holds {} rule(s); carried by {carried} valid evaluation(s){queue}",
            plain(&point.id),
            plain(&point.blocker.subject),
            plain(&point.blocker.code),
            point.rules.len()
        ));
    }
    if !legacy.is_empty() {
        let named = legacy.iter().map(|id| format!("'{}'", plain(id))).collect::<Vec<_>>().join(", ");
        out.push_str(&format!("\n- legacy, filed by an agent before THOR wrote them itself (they settle nothing): {named}"));
    }

    // ---- REFUSED: what the hook refused in the window, split by whether a
    // real tool call stands behind it.
    let (mut verified, mut unchecked, mut never_proof, mut trial_refused) = (0usize, 0usize, 0usize, 0usize);
    // A call the evaluating session made inside its own evaluation proves no
    // tooth, whatever its transcript holds (step 20, NEW-2).
    let mut own_session = 0usize;
    // Writes the trial refused in OTHER sessions during the window: not this
    // evaluation's, counted apart (field test 5, D7: four counted, three its own).
    let mut trial_refused_elsewhere = 0usize;
    for event in window.iter().filter(|e| e.kind == EventKind::GateRefused) {
        if event.actor == TEETH_WRITE_ACTOR {
            if event.session_id == session {
                trial_refused += 1;
            } else {
                trial_refused_elsewhere += 1;
            }
        } else if event.actor == ASSISTANT_DELIVERY_ACTOR && event.session_id == session {
            own_session += 1;
            never_proof += 1;
        } else if event.actor == ASSISTANT_DELIVERY_ACTOR {
            match recorded_refusal_verdicts(&log.entity(&event.entity_id)).get(&event.seq) {
                Some((true, _)) => verified += 1,
                Some((false, _)) => never_proof += 1,
                None => unchecked += 1,
            }
        } else {
            never_proof += 1;
        }
    }
    out.push_str(&format!(
        "\n\nREFUSED: during the evaluation itself (log seq {n} to {filed_seq}; work before it is not counted here) the \
         hook refused {verified} real tool call(s) (verified in a session's transcript); {never_proof} probe or hand-fed refusal(s) (never proof); {unchecked} not verified yet; {trial_refused} write(s) \
         refused by THOR's own trial."
    ));
    if trial_refused_elsewhere > 0 {
        out.push_str(&format!(
            " {trial_refused_elsewhere} more write(s) refused by the trial in other sessions during the window are not counted here."
        ));
    }
    if own_session > 0 {
        out.push_str(&format!(
            " {own_session} of the refusals never proof were calls the evaluating session made itself during this \
             evaluation - refused during the session's own evaluation, not proof."
        ));
    }

    out.push_str(&refusal_outcomes_line(&log, &window, session));

    // ---- SIDE DOORS: every write in the window, on an item of this project
    // or made in the evaluating session, that did not come through an
    // attested THOR call - and the ones whose check is still to come.
    let mut side_doors: Vec<String> = Vec::new();
    let mut unverified: Vec<String> = Vec::new();
    for event in window.iter().filter(|e| ATTESTED_WRITE_KINDS.contains(&e.kind)) {
        if RESERVED_ACTORS.contains(&event.actor.as_str()) {
            continue;
        }
        let events = log.entity(&event.entity_id);
        let ours = event.session_id == session
            || events
                .iter()
                .filter(|e| changes_an_item(e.kind))
                .filter_map(|e| parse_body(&e.body).ok())
                .next_back()
                .is_some_and(|item| project_applies_to(item.project.as_deref(), project));
        if !ours {
            continue;
        }
        let verdicts = recorded_write_verdicts(&events);
        let line = |door: &str| format!("\n- '{}' {} at log seq {}: {door}", plain(&event.entity_id), event.kind.as_str(), event.seq);
        match verdicts.get(&event.seq) {
            Some(verdict) if verdict.attested => {}
            Some(verdict) => side_doors.push(line(&plain(&verdict.why))),
            None => match write_door(event, &verdicts, &event.session_id) {
                WriteDoor::Outside(door) => side_doors.push(line(&plain(&door))),
                _ => unverified.push(line("a THOR call not verified yet")),
            },
        }
    }
    summary.side_doors = side_doors.len();
    summary.unverified = unverified.len();
    out.push_str(&format!(
        "\n\nSIDE DOORS: {} write(s) outside an attested THOR call during the evaluation itself (log seq {n} to {filed_seq}).",
        side_doors.len()
    ));
    side_doors.iter().for_each(|line| out.push_str(line));
    out.push_str(&format!("\nchecked at the end of this turn: {} write(s).", unverified.len()));
    unverified.iter().for_each(|line| out.push_str(line));
    (out, summary)
}

/// REFUSAL OUTCOMES, for the machine section: of the real refusals the hook
/// made in the window (the same ones the REFUSED line calls verified, the
/// evaluating session's own calls left out), how many were followed, before
/// F, by a weakening or retract of the refusing rule - a probable false
/// block (`crate::refusal_outcome`). Derived from the log, never typed.
fn refusal_outcomes_line(log: &LogAt, window: &[Event], session: &str) -> String {
    let in_window: HashSet<i64> = window
        .iter()
        .filter(|e| e.kind == EventKind::GateRefused && e.actor == ASSISTANT_DELIVERY_ACTOR && e.session_id != session)
        .map(|e| e.seq)
        .collect();
    let rules: std::collections::BTreeSet<&str> =
        window.iter().filter(|e| in_window.contains(&e.seq)).map(|e| e.entity_id.as_str()).collect();
    let (mut verified, mut probable) = (0usize, 0usize);
    for rule in rules {
        let events = log.entity(rule);
        let verdicts = recorded_refusal_verdicts(&events);
        let is_real = |e: &Event, _: &Item, _: i64| in_window.contains(&e.seq) && verdicts.get(&e.seq).is_some_and(|(field, _)| *field);
        for fate in crate::refusal_outcome::fates(rule, &events, &is_real, &|_, _| true) {
            verified += 1;
            probable += usize::from(fate.probable_false_block());
        }
    }
    if verified == 0 {
        return "\nREFUSAL OUTCOMES in the window: no verified refusal in the window to follow.".to_string();
    }
    format!(
        "\nREFUSAL OUTCOMES in the window: {verified} verified refusal(s), {probable} followed by a weakening or retract of the \
         refusing rule (probable false blocks), {}. A proxy read from the log: a rule can change for other reasons, and it \
         feeds no ranking and no gate.",
        crate::refusal_outcome::rate_words(probable, verified)
    )
}

/// The actor that records a filing's validation (step 16).
pub const EVAL_VALIDATOR_ACTOR: &str = "eval-validator";

// ------------------------------------------------ the validator's record (step 16)
//
// THE READER OF A FILING. The writer (step 15) refuses what it can see is a
// side door, but it cannot see the filing call's own lines - they reach the
// transcript only after the call. So the first main-session Stop after a new
// report validates it once (`serve::eval_filing::validate`), from the log and
// Claude Code's transcript only, and records the answer here: ONE
// `gate_stood_aside` by `eval-validator` on the report's id, per filing event.
// Every later reader - the Stop hook's evaluation debt, doctor, the carry of
// THOR's own open points - reads that record, never a live transcript
// (transcripts are deleted after about 30 days).

/// How the verdicts of one evaluation split (B7): on its frozen judgement
/// list by an attested call of the evaluating session (its subagents
/// included) - useful, noise, or skipped - plus attested verdicts of that
/// session on items OFF the list, and verdicts in the window that came in
/// outside an attested THOR call (side doors).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictShares {
    #[serde(default)]
    pub useful: usize,
    #[serde(default)]
    pub noise: usize,
    #[serde(default)]
    pub skipped: usize,
    #[serde(default)]
    pub off_list: usize,
    #[serde(default)]
    pub unattested: usize,
}

/// One subject a valid evaluation filed while THOR's own open point about it
/// was live (B5): `carried` is how many valid evaluations had carried it
/// BEFORE this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarryEntry {
    pub subject: String,
    pub carried: usize,
    pub session: String,
    #[serde(default)]
    pub agent: Option<String>,
}

/// The validator's answer for one filing of one evaluation report, as its
/// record holds it. Fields that are only known for a filing that got far
/// enough to be checked are `None` otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationValidation {
    pub valid: bool,
    /// Why it does not count, in words; empty for a valid one.
    #[serde(default)]
    pub why: String,
    /// The session's transcript is in a shape THOR cannot read (D14): never
    /// valid, and nothing is asked that cannot be filed.
    #[serde(default)]
    pub drift: bool,
    /// The report's id.
    pub report: String,
    /// The log seq of the event that filed it (created it, or newly tagged
    /// or moved it as an evaluation report).
    pub report_seq: i64,
    #[serde(default)]
    pub project: Option<String>,
    /// The session column of the filing event (S).
    pub session: String,
    /// The subagent that made the filing call, when one did.
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub started_seq: Option<i64>,
    #[serde(default)]
    pub filed_seq: Option<i64>,
    /// The transcript timestamp of the filing call's result.
    #[serde(default)]
    pub filed_at: Option<String>,
    /// `machine_block_digest` of the stored machine block.
    #[serde(default)]
    pub machine_sha256: Option<String>,
    #[serde(default)]
    pub verdicts: VerdictShares,
    #[serde(default)]
    pub carry: Vec<CarryEntry>,
}

/// The digest a validator record keeps of a report's machine block
/// (`gate::machine_block`): `hash_event("", block)`, the same SHA-256 based
/// digest `args_digest` uses. A later typo fix below the block leaves it
/// unchanged; any change to the block changes it.
pub fn machine_block_digest(block: &str) -> String {
    thor_core::event_store::hash_event("", block)
}

/// Append the validator's record for one filing: ONE `gate_stood_aside` by
/// `eval-validator` on the report's id, in the filing session's column. Once
/// per filing event: a second record for the same (report, report_seq)
/// appends nothing and returns `Ok(None)` - the first answer is the one every
/// reader uses. Only `eval-validator` writes it.
pub fn record_evaluation_validation(
    store: &mut EventStore,
    actor: SystemActor,
    validation: &EvaluationValidation,
) -> Result<Option<Event>, WriteError> {
    if actor.name() != EVAL_VALIDATOR_ACTOR {
        return Err(WriteError::Refused(Refusal {
            problem: format!("an evaluation report's validation is recorded by THOR's eval-validator only, not by '{}'", actor.name()),
            fix: "let the Stop hook validate it (serve::eval_filing::validate_new_reports)".to_string(),
        }));
    }
    if evaluation_validation_of(store, &validation.report, validation.report_seq).is_some() {
        return Ok(None);
    }
    let name = fence(store, actor.into(), &validation.report)?;
    let body = serde_json::to_string(validation).map_err(WriteError::Serialize)?;
    let session = validation.session.as_str();
    store
        .append_event(session, session, name, EventKind::GateStoodAside, &validation.report, None, &body)
        .map(Some)
        .map_err(|e| WriteError::Store(e.into()))
}

/// Every validator record on the log, oldest first, as (record seq,
/// validation): the first one per filing (report, report_seq) only. Reads
/// the validator's own events alone. Empty on an unreadable log - then
/// nothing counts as a valid evaluation, which asks more, never less.
pub fn evaluation_validations(store: &EventStore) -> Vec<(i64, EvaluationValidation)> {
    let Ok(events) = store.events_of_kinds_by_actor(&[EventKind::GateStoodAside], EVAL_VALIDATOR_ACTOR) else {
        return Vec::new();
    };
    let mut seen: HashSet<(String, i64)> = HashSet::new();
    events
        .iter()
        .filter_map(|e| serde_json::from_str::<EvaluationValidation>(&e.body).ok().map(|v| (e.seq, v)))
        .filter(|(_, v)| seen.insert((v.report.clone(), v.report_seq)))
        .collect()
}

/// The validator's record for the filing of `report` at log seq
/// `report_seq`, when one was written.
pub fn evaluation_validation_of(store: &EventStore, report: &str, report_seq: i64) -> Option<EvaluationValidation> {
    let events = store.get_events_by_entity(report).ok()?;
    events
        .iter()
        .filter(|e| e.kind == EventKind::GateStoodAside && e.actor == EVAL_VALIDATOR_ACTOR)
        .filter_map(|e| serde_json::from_str::<EvaluationValidation>(&e.body).ok())
        .find(|v| v.report_seq == report_seq)
}

/// One valid evaluation that carried an open point about a subject: who
/// filed it, when, and which report it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carrier {
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The transcript timestamp of that filing call's result.
    pub filed_at: String,
    pub report: String,
}

/// Every VALID evaluation filing of `project` at whose filing THOR's own
/// open point about `subject` was live (B5), from the validator's records at
/// or before log seq `until`, oldest first. Counted per SUBJECT, never per
/// point: a point that is closed and re-opened, or retracted and written
/// again, keeps its carry. The store-wide point of the rules with no project
/// (`GLOBAL_RULES_SUBJECT`, step 21) is carried by every project's valid
/// evaluations together - one carry, counted once - so `project` is not read
/// for it.
pub fn carriers_of(store: &EventStore, project: &str, subject: &str, until: i64) -> Vec<Carrier> {
    let any_project = subject == GLOBAL_RULES_SUBJECT;
    evaluation_validations(store)
        .into_iter()
        .filter(|(seq, v)| *seq <= until && v.valid && (any_project || v.project.as_deref() == Some(project)))
        .filter_map(|(_, v)| {
            let entry = v.carry.iter().find(|c| c.subject == subject)?;
            Some(Carrier {
                session: entry.session.clone(),
                agent: entry.agent.clone(),
                filed_at: v.filed_at.clone().unwrap_or_default(),
                report: v.report.clone(),
            })
        })
        .collect()
}

/// How many VALID evaluation filings of `project` carried an open point about
/// `subject`, at or before log seq `until` (`carriers_of`).
pub fn carried_by_valid_evaluations(store: &EventStore, project: &str, subject: &str, until: i64) -> usize {
    carriers_of(store, project, subject, until).len()
}

/// The carrying sessions in words: "session S1 on 2026-09-30, S2 (agent a1)
/// on 2026-10-01" - each with the date its filing's result was written.
pub fn carriers_in_words(carriers: &[Carrier]) -> String {
    let one = |c: &Carrier| {
        let agent = c.agent.as_deref().map(|a| format!(" (agent {})", plain(a))).unwrap_or_default();
        let date = c.filed_at.get(..10).unwrap_or(c.filed_at.as_str());
        format!("{}{agent} on {}", plain(&c.session), plain(date))
    };
    format!("session {}", carriers.iter().map(one).collect::<Vec<_>>().join(", "))
}

/// How many of THOR's own open points of one project may wait on the owner
/// before his own session is told so at its start (B6, Q8). Nothing is ever
/// refused over it.
pub const OPEN_POINTS_OWNER_BUDGET: usize = 8;

/// THOR's own open points of `project` that wait on the OWNER (B5): live,
/// and their subject carried by at least one valid evaluation. Each comes
/// with its carriers, oldest first.
pub fn owner_queue(store: &EventStore, project: &str) -> Vec<(OpenPoint, Vec<Carrier>)> {
    open_points(store, Some(project))
        .into_iter()
        .filter(|p| p.machine)
        .filter_map(|p| {
            let carriers = carriers_of(store, project, p.subject.as_deref()?, i64::MAX);
            (!carriers.is_empty()).then_some((p, carriers))
        })
        .collect()
}

/// THOR's own store-wide open points that wait on the owner (step 21): the
/// one point of the rules with no project, when it is live and a valid
/// evaluation of any project was filed while it held. Counted once, here,
/// and against no project's `OPEN_POINTS_OWNER_BUDGET`.
pub fn store_wide_owner_queue(store: &EventStore) -> Vec<(OpenPoint, Vec<Carrier>)> {
    open_points(store, None)
        .into_iter()
        .filter(|p| p.machine && p.subject.as_deref() == Some(GLOBAL_RULES_SUBJECT))
        .filter_map(|p| {
            let carriers = carriers_of(store, "", GLOBAL_RULES_SUBJECT, i64::MAX);
            (!carriers.is_empty()).then_some((p, carriers))
        })
        .collect()
}

/// The line that opens the owner's own session start - and doctor, and a
/// filing's receipt - when more of THOR's open points of `project` wait on
/// him than `OPEN_POINTS_OWNER_BUDGET`. `None` within the budget.
pub fn owner_queue_banner(project: &str, waiting: usize) -> Option<String> {
    (waiting > OPEN_POINTS_OWNER_BUDGET).then(|| {
        format!(
            "[THOR] {waiting} open points wait on YOU in {project} (budget {OPEN_POINTS_OWNER_BUDGET}) - each one says \
             what to do, step by step"
        )
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
pub fn archive<'a>(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: impl Into<WriteActor<'a>>,
    entity_id: &str,
    reason: &str,
    root: Option<&Path>,
) -> Result<Event, WriteError> {
    let actor = fence(store, actor.into(), entity_id)?;
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

    gate::declare_carried(&updated).map_err(WriteError::Refused)?;
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
pub fn restore_deliberate_anchor<'a>(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: impl Into<WriteActor<'a>>,
    entity_id: &str,
    kind: Kind,
    anchor: &str,
) -> Result<Event, WriteError> {
    let actor = fence(store, actor.into(), entity_id)?;
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

    gate::declare_carried(&updated).map_err(WriteError::Refused)?;
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
///
/// `att` is the tool server's stamp of the call that makes the retraction
/// (step 12), spliced onto the tombstone body as `"att"`; `None` (the command
/// line, a repair tool, a test) writes no key.
pub fn retract<'a>(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: impl Into<WriteActor<'a>>,
    entity_id: &str,
    reason: &str,
    att: Option<&Att>,
) -> Result<Event, WriteError> {
    let actor = fence(store, actor.into(), entity_id)?;
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
    let body = with_att(body, att).map_err(WriteError::Serialize)?;
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
///
/// `att` is the tool server's stamp of the call that makes the resolve (step
/// 12), spliced onto the body as `"att"`; `None` writes no key.
pub fn resolve<'a>(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: impl Into<WriteActor<'a>>,
    entity_id: &str,
    keep: &str,
    discard: &[String],
    att: Option<&Att>,
) -> Result<Event, WriteError> {
    let actor = fence(store, actor.into(), entity_id)?;
    let att = att.map(serde_json::to_string).transpose().map_err(WriteError::Serialize)?;
    store
        .append_resolve_stamped(session_id, lineage_id, actor, entity_id, keep, discard, att.as_deref())
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
    /// THOR's old-stock mark (Q24), when this step's trial record carries
    /// one: the fragment an old contains check guards although the rule's
    /// text does not name it - shown on the step's own line.
    pub a4_old_stock: Option<OldContainsStock>,
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
            a4_old_stock: trial_of(&e).ok().flatten().and_then(|r| r.fields().a4_old_stock.clone()),
        })
        .collect())
}

// ------------------------------------ a project's registered working copy (A2, Z3)
//
// Where a project's checkout lives on this machine, written by the installer
// only: as the trailing `"root"`/`"root_at"` keys of the project's own record
// (`project_opened_id`, or `project_root_id` when that record was retracted
// before this build). `fence` refuses every other actor on those ids, so an
// ordinary revise or retract can never move or remove a registration, and
// `root_registrations` reads installer events only, so an event some other
// actor wrote before this build can never be mistaken for one.

/// One registration of a project's working copy, as the installer wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootRegistration {
    /// The record it was written on.
    pub id: String,
    pub seq: i64,
    /// The top of the checkout, with forward slashes, exactly as registered.
    pub root: String,
    /// When, as `YYYY-MM-DDTHH:MM:SSZ`; `None` when the body carries none.
    pub at: Option<String>,
}

/// Every registration of project `key`'s working copy, oldest first: every
/// event by `INSTALLER_ACTOR` whose body carries a `"root"` key, on
/// `project_opened_id(key)` - or, when that record was retracted before this
/// build, on `project_root_id(key)` instead. The last one is the current
/// registration; an earlier one is where the project lived before a move.
pub fn root_registrations(store: &EventStore, key: &str) -> Vec<RootRegistration> {
    let opened = project_opened_id(key);
    let id = if matches!(show(store, &opened), Err(ReadError::Retracted(_))) { project_root_id(key) } else { opened };
    let Ok(events) = store.get_events_by_entity(&id) else { return Vec::new() };
    let mut out: Vec<RootRegistration> = events
        .iter()
        .filter(|e| e.actor == INSTALLER_ACTOR)
        .filter_map(|e| {
            let body: serde_json::Value = serde_json::from_str(&e.body).ok()?;
            let root = body.get("root")?.as_str()?.to_string();
            let at = body.get("root_at").and_then(|v| v.as_str()).map(str::to_string);
            Some(RootRegistration { id: id.clone(), seq: e.seq, root, at })
        })
        .collect();
    out.sort_by_key(|r| r.seq);
    out
}

/// The working copy project `key` is registered at, and the seq of the event
/// that registered it - the newest of `root_registrations`. Raw: nothing here
/// checks that the path still exists or is still that project's checkout;
/// `serve::project::registered_checkout` does, every time it is read.
pub fn registered_root_raw(store: &EventStore, key: &str) -> Option<(PathBuf, i64)> {
    root_registrations(store, key).pop().map(|r| (PathBuf::from(r.root), r.seq))
}

/// Refused unless `id` is a project record - the only kind of item that
/// carries a working copy.
fn refuse_unless_project_record(id: &str) -> Result<(), WriteError> {
    if reserved_id_owner(id) == Some(INSTALLER_ACTOR) {
        return Ok(());
    }
    Err(WriteError::Refused(Refusal {
        problem: format!("'{id}' is not a project record, and only a project record carries a working copy"),
        fix: "register a working copy through install.exe, run from inside the checkout".to_string(),
    }))
}

/// Create a project record that carries its registered working copy from its
/// first event: everything `declare` checks, then the body with `root` and
/// `root_at` spliced on. `id` must be a project record id, and `actor` must be
/// the installer (`fence`).
pub fn declare_project_record(
    store: &mut EventStore,
    item: &Item,
    root: &str,
    at: &str,
    actor: SystemActor,
) -> Result<Event, WriteError> {
    fence(store, actor.into(), &item.id)?;
    refuse_unless_project_record(&item.id)?;
    declare_gated(store, "install", "install", actor.into(), item, None, None, None, Some((root, at)), None)
}

/// Register (or move) the working copy on an existing project record: one
/// `fact_revised` event whose body is the record exactly as it stands, with
/// `root` and `root_at` spliced on. The one revise entry point that ever
/// writes a `"root"` key (X5). Nothing about the item itself changes, so the
/// write gate has nothing new to judge; the fence decides who may write.
pub fn revise_project_root(
    store: &mut EventStore,
    id: &str,
    root: &str,
    at: &str,
    actor: SystemActor,
) -> Result<Event, WriteError> {
    let name = fence(store, actor.into(), id)?;
    refuse_unless_project_record(id)?;
    let existing = show(store, id).map_err(|e| WriteError::Store(anyhow::anyhow!("{e}")))?;
    let body = canonical_body(&existing).map_err(WriteError::Serialize)?;
    let body = body_with_root(&body, root, at).map_err(WriteError::Serialize)?;
    store
        .append_mutate_checked("install", "install", name, EventKind::FactRevised, id, None, &body)
        .map_err(WriteError::Store)
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

        retract(&mut store, "s1", "l1", "test", &item.id, "config/app.toml moved", None).unwrap();

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

        let err = retract(&mut store, "s1", "l1", "test", &item.id, "   ", None).unwrap_err();
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
        retract(&mut store, "s1", "l1", "test", &item.id, "app.toml was superseded by the new layout", None).unwrap();

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
        // Field test 2 (2026-10-01): each occupant's severity AND whether it
        // carries a check, so the writer can tell a description from a guard.
        assert!(msg.contains("'sit-0' (costly, no check)"), "severity and check status per occupant: {msg}");
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

        retract(&mut store, "s", "l", "t", "sit-0", "folded into 'sit-1'", None).unwrap();
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

    fn command_bound(id: &str, nth: usize, command: &str) -> Item {
        let mut item = sample_with(id, DISTINCT[nth % DISTINCT.len()]);
        item.bindings = vec![Binding::Target { kind: TargetKind::Command, value: command.to_string() }];
        item
    }

    /// THE DEFECT THIS PREVENTS (field test 11): a full command anchor
    /// (`node tools/push.mjs`, four facts) was dodged by binding the new rule
    /// to the narrower `node tools/push.mjs 10.0.0.5`. The gate counted
    /// rivals as path strings, so the broad anchor was no rival; at runtime
    /// (`shell::reaches_any`) the broad anchor reaches every call the narrow
    /// one is meant for, so the new rule was crowded out the moment it landed.
    #[test]
    fn a_narrower_command_anchor_under_a_full_broader_one_is_refused() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let sitting = command_bound(&format!("sit-{i}"), i, "node tools/push.mjs");
            declare(&mut store, "s", "l", "t", &sitting).expect("fixture must store");
        }
        let narrow = command_bound("narrow", 8, "node tools/push.mjs 10.0.0.5");
        let err = declare(&mut store, "s", "l", "t", &narrow).expect_err("the broad anchor reaches every call the narrow one does");
        let msg = format!("{err}");
        assert!(msg.contains("sit-0"), "the refusal must name the occupants: {msg}");
        assert!(msg.contains("fold, move or archive"), "and the three exits: {msg}");
    }

    /// The other direction of the same runtime relation: a new BROAD anchor
    /// fires on the calls four existing narrower rules fire on.
    #[test]
    fn a_broader_command_anchor_over_four_narrower_ones_is_refused() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let sitting = command_bound(&format!("sit-{i}"), i, "node tools/push.mjs 10.0.0.5");
            declare(&mut store, "s", "l", "t", &sitting).expect("fixture must store");
        }
        let broad = command_bound("broad", 8, "node tools/push.mjs");
        declare(&mut store, "s", "l", "t", &broad).expect_err("the narrow anchors fire on calls the broad one fires on");
    }

    /// Narrower anchors only crowd a broad one when a single call reaches them
    /// all: four DIFFERENT `node tools/push.mjs <host>` anchors never fire on
    /// the same call, so a broad `node tools/push.mjs` rule is not displaced.
    #[test]
    fn a_broad_command_anchor_over_unrelated_narrower_ones_is_not_refused() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let sitting = command_bound(&format!("sit-{i}"), i, &format!("node tools/push.mjs host-{i}"));
            declare(&mut store, "s", "l", "t", &sitting).expect("fixture must store");
        }
        let broad = command_bound("broad", 8, "node tools/push.mjs");
        declare(&mut store, "s", "l", "t", &broad).expect("no single call reaches all four narrower anchors");
    }

    /// A command that shares only its first word, and is not a word prefix of
    /// the other, never fires on the same calls: not a rival.
    #[test]
    fn a_command_anchor_that_is_not_a_word_prefix_is_not_a_rival() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let sitting = command_bound(&format!("sit-{i}"), i, "node tools/push.mjs");
            declare(&mut store, "s", "l", "t", &sitting).expect("fixture must store");
        }
        for (n, other) in ["node other.mjs", "node tools/push.mjs.bak", "node tools/push", "nodejs tools/push.mjs"].iter().enumerate() {
            let unrelated = command_bound(&format!("unrelated-{n}"), 4 + n, other);
            declare(&mut store, "s", "l", "t", &unrelated)
                .unwrap_or_else(|e| panic!("'{other}' is no word prefix of 'node tools/push.mjs': {e}"));
        }
    }

    /// An existing crowded item is never demoted or re-checked: revising it in
    /// place (same command binding) is judged only for bindings new to the
    /// write.
    #[test]
    fn an_existing_crowded_command_item_is_untouched() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            let sitting = command_bound(&format!("sit-{i}"), i, "node tools/push.mjs");
            declare(&mut store, "s", "l", "t", &sitting).expect("fixture must store");
        }
        // Crowded already: stored through a back door, as a pre-fix write.
        let crowded = command_bound("crowded", 8, "node tools/push.mjs 10.0.0.5");
        seed_live(&mut store, &crowded);
        let existing = show(&store, "crowded").unwrap();
        let mut reworded = existing.clone();
        reworded.text = format!("{} - reworded", existing.text);
        revise(&mut store, "s", "l", "t", &existing, &reworded).expect("a binding the item already stood on is never refused");
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

    /// FIELD DEFECT 6 (2026-10-01): THOR's machine pass said a rule standing on
    /// dashboard.html "can never reach a block" (6 heavier rivals) while the
    /// revise reply said "shown 1028 time(s) ... not crowded out" - the log
    /// showed 432 servings at that very file. One rule now: an item that keeps
    /// every binding it stands on is answered by its servings, for the
    /// dead-on-arrival estimate exactly as for the crowded one.
    #[test]
    fn capacity_for_revise_answers_a_dead_on_arrival_estimate_by_servings() {
        let mut store = EventStore::in_memory().unwrap();
        let path = "server/public/dashboard.html";
        let mine = path_bound("light", 0, path, Some(Severity::HouseStyle));
        seed_live(&mut store, &mine);
        for i in 0..crate::item::MAX_ITEMS + 2 {
            seed_live(&mut store, &path_bound(&format!("heavy-{i}"), i + 1, path, Some(Severity::Irreversible)));
        }
        let mut updated = mine.clone();
        updated.text = format!("{} Reworded.", mine.text);
        assert!(matches!(capacity(&store, &updated).unwrap(), Capacity::DeadOnArrival(_)), "fixture: the estimate says hopeless");
        assert!(
            matches!(capacity_for_revise(&store, &mine, &updated, NOW).unwrap(), Capacity::DeadOnArrival(_)),
            "never served: the estimate stands"
        );
        seed_served(&mut store, &mine.id, RECENTLY);
        match capacity_for_revise(&store, &mine, &updated, NOW).unwrap() {
            Capacity::Crowded(note) => assert!(note.contains("shown 1 time(s)") && note.contains("not crowded out"), "{note}"),
            other => panic!("served at its own binding, so never 'can never reach a block': {other:?}"),
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
        legacy(&mut store, &live);
        let mut dead = sample_with("file-gone", "the tree this one watched was deleted");
        dead.check = Some(Check::Contains {
            path: "gone/src/lib.rs".to_string(),
            literal: "fn main".to_string(),
        });
        legacy(&mut store, &dead);

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
        retract(&mut store, "s1", "l1", "test", &p3.id, "app.toml was superseded by the new layout", None).unwrap();

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

        let err = retract(&mut store, "s", "l", "a", SETUP_NOTE_ID, "done", None).unwrap_err();
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

        retract(&mut store, "s", "l", "a", SETUP_NOTE_ID, "owner walked through setup", None).unwrap();
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

        retract(&mut store, "s", "l", "a", SETUP_NOTE_ID, "owner declined setup", None).unwrap();
    }

    /// THE NARROW SCOPE THIS GROUND MUST KEEP: an ordinary retract, on an
    /// ordinary id, must never even notice this ground exists.
    #[test]
    fn retracting_an_unrelated_id_never_needs_the_answers_item() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s", "l", "a", &item).unwrap();

        retract(&mut store, "s", "l", "a", &item.id, "config/app.toml is no longer true", None).unwrap();
    }

    /// A store that never had the seeded note at all (an old install, or one
    /// that predates this feature) must retract normally - `show` on an
    /// unknown id already refuses with `NotFound` before this ground is ever
    /// reached, so there is nothing new here to trip over.
    #[test]
    fn a_store_with_no_seeded_note_is_unaffected() {
        let mut store = EventStore::in_memory().unwrap();
        let err = retract(&mut store, "s", "l", "a", SETUP_NOTE_ID, "cleanup", None).unwrap_err();
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
    /// so this is enough to owe a verdict on its own. Declared through
    /// `declare_noted` (a Note verdict from a trial record), so it can never
    /// ALSO trip the evaluation-filing gate's condition 2 - since step 9 a
    /// light rule WITHOUT a verdict is owed like any other (D8) - the
    /// isolation `evaluation_report_filing_is_refused_while_judgement_debt_is_
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

    /// Firings in `n` sessions, one each ("s", then "s-1", "s-2" ...): the
    /// judgement debt counts a fact once per session (step 28).
    fn serve_n(store: &mut EventStore, id: &str, actor: &str, n: usize) {
        for i in 0..n {
            let session = if i == 0 { "s".to_string() } else { format!("s-{i}") };
            store.append_event(&session, "l", actor, EventKind::ItemServed, id, None, "{}").unwrap();
        }
    }

    fn mark_useful(store: &mut EventStore, id: &str) {
        store.append_event("s", "l", "a", EventKind::ItemMarkedUseful, id, None, "{}").unwrap();
    }

    /// Firings the way the hook stamps them, in `n` sessions: the named one
    /// first, then "<session>-1", "<session>-2" ... (once per session, step 28).
    fn serve_in(store: &mut EventStore, session: &str, id: &str, n: usize) {
        for i in 0..n {
            let this = if i == 0 { session.to_string() } else { format!("{session}-{i}") };
            store.append_event(&this, session, ASSISTANT_DELIVERY_ACTOR, EventKind::ItemServed, id, None, "{}").unwrap();
        }
    }

    /// FILE `report` THE WAY THE TOOL SERVER DOES (step 15), in one go:
    /// session "s" starts an evaluation of the report's project right now
    /// (`record_start`: the start record, then the batch record with the
    /// live batch handed to the agent), and the report is filed for it
    /// through a THOR call the writer saw in flight (`file_in`). A fresh
    /// start at every attempt, so what is asked is what an evaluation
    /// starting now would be held to.
    fn file_now(store: &mut EventStore, report: &Item) -> Result<Event, WriteError> {
        let start = record_start(store, "s", report.project.as_deref().expect("an evaluation report names its project"));
        file_in(store, report, &start)
    }

    /// Record the write at `seq` on `id` as an attested THOR call of
    /// `session`, the way the session's Stop records it.
    fn attest(store: &mut EventStore, session: &str, id: &str, seq: i64) {
        record_write_verification(store, session, SystemActor::new(ATTESTER_ACTOR), id, seq, true, None, "fixture: attested").unwrap();
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
        // This door carries no filing context at all (`FilingContext`), so
        // a revise that files through it is refused outright (step 15).
        let mut plain = eval_report("not-yet-an-eval-report", "z");
        plain.tags = vec!["some-other-tag".to_string()];
        declare(&mut store, "s", "l", "a", &plain).expect("an ordinary, untagged Report may be declared freely");

        let mut now_tagged = plain.clone();
        now_tagged.tags.push(EVALUATION_REPORT_TAG.to_string());
        let err = revise(&mut store, "s", "l", "a", &plain, &now_tagged).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("not coming through a THOR tool call"), "{}", refusal.problem);
        assert_eq!(show(&store, "not-yet-an-eval-report").unwrap().tags, vec!["some-other-tag".to_string()], "nothing written");
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
        declare_noted(&mut store, &light);
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
        declare_noted(&mut store, &light);
        serve_n(&mut store, "owed-1", ASSISTANT_DELIVERY_ACTOR, JUDGEMENT_DEBT_AFTER);

        let start = record_start(&mut store, "s", "p");
        let err = file_in(&mut store, &eval_report("eval-p-1", "p"), &start).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("owe a judgement"), "{}", refusal.problem);
        assert!(show(&store, "eval-p-1").is_err(), "a refused report must not be live");
    }

    #[test]
    fn evaluation_report_filing_succeeds_once_judgement_debt_is_paid_down() {
        let mut store = EventStore::in_memory().unwrap();
        let light = light_command_rule("owed-2", "p");
        declare_noted(&mut store, &light);
        serve_n(&mut store, "owed-2", ASSISTANT_DELIVERY_ACTOR, JUDGEMENT_DEBT_AFTER);
        // Attested: since step 12 only such a verdict clears anything (D9).
        mark_verified(&mut store, "owed-2", "s", true);

        let start = record_start(&mut store, "s", "p");
        file_in(&mut store, &eval_report("eval-p-2", "p"), &start).expect("debt is settled, filing must succeed");
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
        file_now(&mut store, &eval_report("eval-q-old", "q")).unwrap();
        legacy_thinly_answered(&mut store, "unboxed-heavy", Some("q"));
        serve_n(&mut store, "unboxed-heavy", "hook", 1);

        let err = file_now(&mut store, &eval_report("eval-q-new", "q")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("unboxed-heavy"), "{}", refusal.problem);
        assert!(refusal.problem.contains("no current verdict from THOR's trial"), "{}", refusal.problem);
    }

    /// C (2026-09-26): the refusal used to point at a maintenance pass that
    /// labels in bulk as the way out. It must send a fresh agent to the real
    /// work instead - since step 9 THOR's trial, by the retest command that
    /// decides the rule - and never to relabelling.
    #[test]
    fn the_filing_refusal_names_the_real_work_and_never_the_bulk_relabel() {
        let mut store = EventStore::in_memory().unwrap();
        file_now(&mut store, &eval_report("eval-c-old", "c")).unwrap();
        legacy_thinly_answered(&mut store, "needs-work", Some("c"));
        serve_n(&mut store, "needs-work", "hook", 1);
        let err = file_now(&mut store, &eval_report("eval-c-new", "c")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        let text = format!("{} {}", refusal.problem, refusal.fix);
        assert!(!text.contains("backfill_teeth"), "a bulk relabel is never the way out: {text}");
        assert!(text.contains("retest --id 'needs-work'"), "the command that decides it: {text}");
        assert!(text.contains("mcp__thor__revise with only the rule's id"), "{text}");
        assert!(text.contains("A tag, a reason, an open point, a refusal on record or a probe settles nothing"), "{text}");
    }

    /// C (2026-09-26): the judgement half names the owed items, so a writer
    /// does not have to go and find them - and says what a verdict must rest
    /// on.
    #[test]
    fn the_filing_refusal_names_every_item_that_owes_a_judgement() {
        let mut store = EventStore::in_memory().unwrap();
        declare_noted(&mut store, &light_command_rule("owed-a", "o"));
        declare_noted(&mut store, &light_command_rule("owed-b", "o"));
        serve_n(&mut store, "owed-a", ASSISTANT_DELIVERY_ACTOR, JUDGEMENT_DEBT_AFTER);
        serve_n(&mut store, "owed-b", ASSISTANT_DELIVERY_ACTOR, JUDGEMENT_DEBT_AFTER);
        let start = record_start(&mut store, "s", "o");
        let err = file_in(&mut store, &eval_report("eval-o-1", "o"), &start).unwrap_err();
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
    // items useful. These pin the fix (see `evaluation_judgement_owed`).

    /// Items that fired only in another session stay owed store-wide, but
    /// never hold this session's report.
    #[test]
    fn an_item_that_fired_only_in_another_session_never_holds_this_report() {
        let mut store = EventStore::in_memory().unwrap();
        declare_noted(&mut store, &light_command_rule("elsewhere", "w"));
        serve_in(&mut store, "other-session", "elsewhere", JUDGEMENT_DEBT_AFTER);
        let start = record_start(&mut store, "this-session", "w");

        file_in(&mut store, &eval_report("eval-w-1", "w"), &start).expect("another session's firings are not this report's to settle");
        assert_eq!(
            judgement_debt_in_project(&store, Some("w")),
            vec!["elsewhere".to_string()],
            "it stays owed store-wide, where doctor keeps naming it"
        );
    }

    /// STEP 28: a fact counts ONCE PER SESSION toward the 40 that make a verdict
    /// owed - in the store-wide debt and in an evaluation's frozen list alike.
    /// Eighty showings in one session are one session; forty sessions that saw
    /// it once each are forty.
    #[test]
    fn a_fact_counts_once_per_session_toward_the_debt_and_the_frozen_list() {
        let mut store = EventStore::in_memory().unwrap();
        declare_noted(&mut store, &light_command_rule("one-session", "o"));
        declare_noted(&mut store, &light_command_rule("many-sessions", "o"));
        for _ in 0..JUDGEMENT_DEBT_AFTER * 2 {
            store.append_event("busy", "busy", ASSISTANT_DELIVERY_ACTOR, EventKind::ItemServed, "one-session", None, "{}").unwrap();
        }
        serve_in(&mut store, "busy", "many-sessions", JUDGEMENT_DEBT_AFTER);
        assert_eq!(served_since_last_verdict_local(&store).get("one-session"), Some(&1));
        assert_eq!(served_since_last_verdict_local(&store).get("many-sessions"), Some(&JUDGEMENT_DEBT_AFTER));
        assert_eq!(judgement_debt_in_project(&store, Some("o")), vec!["many-sessions".to_string()]);
        assert_eq!(evaluation_judgement_list(&store, Some("o"), "busy", i64::MAX), vec!["many-sessions".to_string()]);
    }

    /// The list is fixed when the evaluation starts: an item pushed over the
    /// threshold afterwards - by the evaluation's own writes firing the
    /// remember-bound rules, or by anything else - never joins it.
    #[test]
    fn nothing_that_fires_after_the_evaluation_started_can_add_to_its_list() {
        let mut store = EventStore::in_memory().unwrap();
        declare_noted(&mut store, &light_command_rule("owed-before", "f"));
        declare_noted(&mut store, &light_command_rule("remember-bound", "f"));
        serve_in(&mut store, "eval-session", "owed-before", JUDGEMENT_DEBT_AFTER);
        serve_in(&mut store, "eval-session", "remember-bound", JUDGEMENT_DEBT_AFTER - 1);
        let start = record_start(&mut store, "eval-session", "f");
        // The evaluation's own writes fire it again, well past the threshold.
        serve_in(&mut store, "late-session", "remember-bound", 10);

        let err = file_in(&mut store, &eval_report("eval-f-1", "f"), &start).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("'owed-before'"), "{}", refusal.problem);
        assert!(!refusal.problem.contains("remember-bound"), "the list cannot grow: {}", refusal.problem);

        mark_useful(&mut store, "owed-before");
        assert!(file_in(&mut store, &eval_report("eval-f-1", "f"), &start).is_err(), "a verdict with no recorded attestation settles nothing");
        mark_verified(&mut store, "owed-before", "eval-session", true);
        file_in(&mut store, &eval_report("eval-f-1", "f"), &start).expect("the fixed list is settled, so the report files");
    }

    /// "When in doubt, skip" and "the list must be settled" no longer
    /// collide: an explicit skip with a reason settles an item for this
    /// filing, and the item stays owed store-wide. A skip without a real
    /// reason settles nothing.
    #[test]
    fn a_skip_with_a_reason_settles_an_item_for_this_filing_only() {
        let mut store = EventStore::in_memory().unwrap();
        declare_noted(&mut store, &light_command_rule("unsure", "n"));
        serve_in(&mut store, "eval-session", "unsure", JUDGEMENT_DEBT_AFTER);
        let start = record_start(&mut store, "eval-session", "n");

        let mut thin = eval_report("eval-n-1", "n");
        thin.tags.push(format!("{NOT_JUDGEABLE_TAG_PREFIX}unsure:no"));
        assert!(file_in(&mut store, &thin, &start).is_err(), "a skip with no real reason settles nothing");

        let mut honest = eval_report("eval-n-1", "n");
        honest.tags.push(format!("{NOT_JUDGEABLE_TAG_PREFIX}unsure:it fired on a grep of a log I never opened"));
        file_in(&mut store, &honest, &start).expect("an honest skip settles it for this filing");
        assert!(judgement_debt_in_project(&store, Some("n")).contains(&"unsure".to_string()), "and it stays owed store-wide");
    }

    /// R15.1/R15.2 at the store: a write that files an evaluation report
    /// with NO filing context - the model CLI, a replica's drain, a one-off
    /// binary, the tool server on a side door - is REFUSED, never waived.
    /// Measured before step 15: it was stored, and its receipt said "no
    /// verdict was required here". Nothing is written, through any door.
    #[test]
    fn filing_without_context_is_refused_not_waived() {
        let mut store = EventStore::in_memory().unwrap();
        declare_noted(&mut store, &light_command_rule("owed-x", "x"));
        serve_n(&mut store, "owed-x", ASSISTANT_DELIVERY_ACTOR, JUDGEMENT_DEBT_AFTER);
        let report = eval_report("eval-x-1", "x");
        let before = store.get_next_seq().unwrap();

        let refused = |err: WriteError| {
            let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
            assert!(
                refusal.problem.contains("not coming through a THOR tool call that both Claude Code and THOR's hook saw"),
                "{}",
                refusal.problem
            );
            assert!(refusal.problem.contains("Nothing was written"), "{}", refusal.problem);
            assert!(refusal.fix.contains("File it with the THOR tool itself") && refusal.fix.contains("ToolSearch"), "{}", refusal.fix);
        };
        refused(declare(&mut store, "s", "l", "a", &report).unwrap_err());
        refused(declare_in(&mut store, "s", "l", "a", &report, None).unwrap_err());
        refused(declare_scoped(&mut store, "s", "l", "mcp", &report, None, None).unwrap_err());
        // An open evaluation changes nothing about it: the context is what
        // the write brings, and this one brings none.
        let start = record_start(&mut store, "s", "x");
        refused(declare_scoped(&mut store, "s", "l", "mcp", &report, None, None).unwrap_err());
        // A side door gives no context at all.
        assert_eq!(FilingContext::new(INFLIGHT_SIDE_DOOR, start.clone()), None);
        // The revise doors: tagging an existing report files it.
        let mut plain = report.clone();
        plain.id = "notes-x".to_string();
        plain.tags = vec!["notes".to_string()];
        declare(&mut store, "s", "l", "a", &plain).unwrap();
        let mut tagged = plain.clone();
        tagged.tags.push(EVALUATION_REPORT_TAG.to_string());
        refused(revise(&mut store, "s", "l", "a", &plain, &tagged).unwrap_err());
        refused(revise_because(&mut store, "s", "l", "a", &plain, &tagged, &plain, None).unwrap_err());

        assert!(show(&store, "eval-x-1").is_err(), "nothing stored");
        let written: Vec<String> =
            store.events_since(before - 1).unwrap().into_iter().map(|e| e.entity_id).filter(|id| id.starts_with("eval-")).collect();
        assert!(written.is_empty(), "no event on the report at all: {written:?}");
        // And the one sentence every door prints.
        let sentence = refusal_sentence(&unattested_filing_refusal("no toolUseId in the call's _meta"));
        assert!(
            sentence.starts_with(
                "REFUSED: this evaluation report is not coming through a THOR tool call that both Claude Code and THOR's hook \
                 saw (no toolUseId in the call's _meta), so it could never count - the Stop hook only accepts a report whose \
                 filing call is in this session's own transcript. Nothing was written. File it with the THOR tool itself, not \
                 a script, a second client, the model CLI or an older binary: if mcp__thor__remember is not loaded yet, load \
                 it with ToolSearch, query \"select:mcp__thor__remember,mcp__thor__revise,mcp__thor__mark\", then call it with \
                 the same arguments."
            ),
            "{sentence}"
        );
    }

    // ------------------------------------------ the start record (step 13)

    /// A verdict on `id` by `session`, with its recorded verification: one
    /// `attester` record saying whether it came through an attested THOR call.
    fn mark_verified(store: &mut EventStore, id: &str, session: &str, attested: bool) -> i64 {
        let seq = store.append_event(session, session, "mcp", EventKind::ItemMarkedUseful, id, None, "{}").unwrap().seq;
        let why = if attested { "fixture: attested" } else { "fixture: no toolUseId in the call's _meta" };
        record_write_verification(store, session, SystemActor::new(ATTESTER_ACTOR), id, seq, attested, None, why).unwrap();
        seq
    }

    /// The start record of an evaluation of `project` by `session`, starting
    /// now, written the way the hook writes it: the start record first, then
    /// the batch record. THOR's machine pass lives in `serve`, so the batch
    /// here is the live batch, every rule on it handed to the agent - what
    /// the pass writes when none of them can be decided without one.
    fn record_start(store: &mut EventStore, session: &str, project: &str) -> EvaluationStart {
        let batch = evaluation_teeth_batch(store, Some(project));
        let items = batch
            .owed_now
            .iter()
            .chain(&batch.owed_later)
            .map(|(id, why)| machine_item(id, MachineOutcome::NeedsAgent, why, None))
            .collect();
        let ids = batch.owed_now.iter().map(|(id, _)| id.clone()).collect();
        let first = record_start_only(store, session, project);
        decide_start(store, &first, ids, batch.owed_later.len(), vec![], items)
    }

    fn machine_item(id: &str, outcome: MachineOutcome, detail: &str, blocker: Option<&Blocker>) -> MachineItem {
        MachineItem {
            id: id.to_string(),
            outcome,
            detail: detail.to_string(),
            call: None,
            blocker: blocker.map(MachineBlocker::of),
            seq: None,
        }
    }

    /// The batch record on `start`, as THOR's machine pass writes it.
    fn decide_start(
        store: &mut EventStore,
        start: &EvaluationStart,
        batch: Vec<String>,
        batch_later: usize,
        a4_list: Vec<String>,
        items: Vec<MachineItem>,
    ) -> EvaluationStart {
        let count = |o: MachineOutcome| items.iter().filter(|i| i.outcome == o).count();
        let machine = MachinePass {
            blocked: count(MachineOutcome::Blocked),
            needs_agent: count(MachineOutcome::NeedsAgent),
            a4_only: count(MachineOutcome::A4Only),
            items,
            ..MachinePass::default()
        };
        let actor = SystemActor::new(EVAL_MACHINE_ACTOR);
        record_evaluation_batch(store, actor, start, "fixture start note, decided", batch, batch_later, a4_list, machine).unwrap()
    }

    /// Only the FIRST of the two records: the start, with no batch yet.
    fn record_start_only(store: &mut EventStore, session: &str, project: &str) -> EvaluationStart {
        let frozen = frozen_now(store, session, project, "skill", now_unix_secs()).unwrap();
        let item = Item {
            id: evaluation_start_id(project, frozen.started_seq),
            kind: Kind::Report,
            text: format!("fixture start note of {project}"),
            bindings: vec![],
            severity: None,
            project: Some(project.to_string()),
            tags: vec![EVALUATION_START_TAG.to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        record_evaluation_start(store, SystemActor::new(EVAL_MACHINE_ACTOR), &item, &frozen).unwrap()
    }

    /// File `report` for the evaluation `start` records, the way the tool
    /// server does: a filing context (the call seen in flight, the start
    /// record) and the call's own stamp.
    fn file_in(store: &mut EventStore, report: &Item, start: &EvaluationStart) -> Result<Event, WriteError> {
        let session = start.frozen.session.clone();
        let att = Att {
            tool_use_id: Some(format!("toolu_fixture_{}", report.id)),
            args_sha256: args_digest(&serde_json::json!({"id": report.id})),
            session_hint: Some(session.clone()),
            inflight: Some(INFLIGHT_SEEN.to_string()),
            side_door: None,
        };
        let ctx = FilingContext::new(INFLIGHT_SEEN, start.clone()).expect("seen in flight gives a context");
        declare_scoped(store, &session, "l", "mcp", report, Some(&ctx), Some(&att))
    }

    /// B5, B6: a point in the OWNER QUEUE - its subject carried by a valid
    /// evaluation, still blocked at the next start - holds no filing: the
    /// report of that next evaluation files. It counts against the owner's
    /// budget, and only more than `OPEN_POINTS_OWNER_BUDGET` of them open the
    /// owner's session with the banner.
    #[test]
    fn a_point_in_the_owner_queue_does_not_hold_the_filing() {
        let mut store = EventStore::in_memory().unwrap();
        let blocker = Blocker::NoWorkingCopy { project: "q".to_string() };
        let mut held = heavy_command_rule("q-file", "q");
        held.check = Some(Check::Contains { path: "README.md".to_string(), literal: "alpha line".to_string() });
        legacy(&mut store, &held);
        let machine = SystemActor::new(EVAL_MACHINE_ACTOR);
        let point_id = machine_point_id("q", &blocker);
        write_machine_point(&mut store, machine, "s1", "q", &blocker, "fixture point: no working copy of q").unwrap();
        let carried = |carried_by: Vec<Carrier>| MachinePass {
            blocked: 1,
            items: vec![machine_item("q-file", MachineOutcome::Blocked, "no working copy", Some(&blocker))],
            points: vec![MachinePoint {
                id: point_id.clone(),
                blocker: MachineBlocker::of(&blocker),
                rules: vec!["q-file".to_string()],
                carried_by,
            }],
            ..MachinePass::default()
        };
        let first = record_start_only(&mut store, "s1", "q");
        record_evaluation_batch(&mut store, machine, &first, "fixture, decided", vec![], 0, vec![], carried(vec![])).unwrap();
        assert!(owner_queue(&store, "q").is_empty(), "no valid evaluation has carried it yet");

        // The validator's record of a valid filing while the point held.
        let validation = EvaluationValidation {
            valid: true,
            why: String::new(),
            drift: false,
            report: "eval-q-1".to_string(),
            report_seq: 1,
            project: Some("q".to_string()),
            session: "s1".to_string(),
            agent: None,
            started_seq: Some(first.frozen.started_seq),
            filed_seq: Some(first.frozen.started_seq + 2),
            filed_at: Some("2026-09-29T10:00:00Z".to_string()),
            machine_sha256: Some("x".to_string()),
            verdicts: VerdictShares::default(),
            carry: vec![CarryEntry { subject: "q".to_string(), carried: 0, session: "s1".to_string(), agent: None }],
        };
        let validator = SystemActor::new(EVAL_VALIDATOR_ACTOR);
        assert!(record_evaluation_validation(&mut store, validator, &validation).unwrap().is_some());
        assert!(record_evaluation_validation(&mut store, validator, &validation).unwrap().is_none(), "recorded once");
        let err = record_evaluation_validation(&mut store, machine, &validation).unwrap_err().to_string();
        assert!(err.contains("eval-validator only"), "{err}");

        let queue = owner_queue(&store, "q");
        assert_eq!(queue.len(), 1);
        assert_eq!((queue[0].0.id.as_str(), queue[0].1.len()), (point_id.as_str(), 1));
        assert_eq!(carriers_in_words(&queue[0].1), "session s1 on 2026-09-29");
        assert_eq!(owner_queue_banner("q", queue.len()), None, "within the budget: no banner");
        assert_eq!(owner_queue_banner("q", OPEN_POINTS_OWNER_BUDGET), None);
        assert_eq!(
            owner_queue_banner("q", OPEN_POINTS_OWNER_BUDGET + 1).as_deref(),
            Some("[THOR] 9 open points wait on YOU in q (budget 8) - each one says what to do, step by step")
        );

        // The next evaluation, the point in the owner queue: the report files.
        let carriers = carriers_of(&store, "q", "q", i64::MAX);
        let second = record_start_only(&mut store, "s2", "q");
        let second = record_evaluation_batch(&mut store, machine, &second, "fixture, decided", vec![], 0, vec![], carried(carriers)).unwrap();
        let filed = file_in(&mut store, &eval_report("eval-q-2", "q"), &second).expect("an owner-queue point holds no filing");
        let text = show(&store, "eval-q-2").unwrap().text;
        assert!(
            text.contains(&format!("'{point_id}' about 'q' (no-working-copy): holds 1 rule(s); carried by 1 valid evaluation(s) - in the owner queue: the owner decides - carried by session s1 on 2026-09-29")),
            "{text}"
        );
        assert!(filed.seq > second.seq);
    }

    #[test]
    fn start_record_round_trips_its_frozen_lists() {
        let mut store = EventStore::in_memory().unwrap();
        declare_noted(&mut store, &light_command_rule("rt-judge", "rt"));
        serve_in(&mut store, "rt-session", "rt-judge", JUDGEMENT_DEBT_AFTER);
        legacy_thinly_answered(&mut store, "rt-owed", Some("rt"));
        declare(&mut store, "s", "l", "a", &open_point("open-rt-1", "rt")).unwrap();
        let start = record_start(&mut store, "rt-session", "rt");

        assert_eq!(start.id, format!("evaluation-start-rt-{}", start.frozen.started_seq));
        assert_eq!(start.seq, start.frozen.started_seq + 1, "written right at the tip it froze");
        assert_eq!(start.frozen.judgement, vec!["rt-judge".to_string()]);
        assert_eq!(start.frozen.batch, Some(vec!["rt-owed".to_string()]));
        assert_eq!(start.frozen.batch_later, 0);
        assert_eq!(start.frozen.a4_list, Some(vec![]));
        assert_eq!(start.frozen.open_points, vec!["open-rt-1".to_string()]);
        assert_eq!(start.frozen.machine.as_ref().map(|m| m.needs_agent), Some(1));

        assert_eq!(open_evaluation_start(&store, "rt-session", "rt"), Some(start.clone()), "read back from the log");
        assert_eq!(evaluation_starts_of_session(&store, "rt-session"), vec![start.clone()]);
        assert_eq!(open_evaluation_start(&store, "another-session", "rt"), None);
        // Two events, both by eval-machine in the evaluating session: the
        // start record with no batch, then the ONE revise that adds it.
        let events = store.get_events_by_entity(&start.id).unwrap();
        assert_eq!(events.iter().map(|e| e.kind).collect::<Vec<_>>(), vec![EventKind::FactCreated, EventKind::FactRevised]);
        assert!(events.iter().all(|e| e.actor == EVAL_MACHINE_ACTOR && e.session_id == "rt-session"));
        assert!(!events[0].body.contains("\"batch\":"), "the start record is written before the batch exists: {}", events[0].body);
        assert!(frozen_of(&events[0].body).is_some_and(|f| !f.decided() && f.judgement == start.frozen.judgement));
        assert_eq!(start.batch_seq, Some(events[1].seq));
        let event = events.last().unwrap();
        assert!(event.body.ends_with(&format!(",\"frozen\":{}}}", serde_json::to_string(&start.frozen).unwrap())), "{}", event.body);
        let item = show(&store, &start.id).unwrap();
        assert_eq!((item.kind, item.tags.clone()), (Kind::Report, vec![EVALUATION_START_TAG.to_string()]));

        // Only eval-machine writes one, and never twice on one id.
        let again = Item { text: "again".to_string(), ..item.clone() };
        assert!(record_evaluation_start(&mut store, SystemActor::new(EVAL_MACHINE_ACTOR), &again, &start.frozen).is_err());
        let other = Item { id: evaluation_start_id("rt", 999_999), ..item.clone() };
        assert!(record_evaluation_start(&mut store, SystemActor::new("retest"), &other, &start.frozen).is_err());
        let err = declare(&mut store, "s", "l", "mcp", &other).unwrap_err();
        assert!(err.to_string().contains("does not begin with 'evaluation-start-'"), "{err}");
        assert_eq!(evaluation_starts_of_session(&store, "rt-session").len(), 1);
    }

    /// D9: a verdict outside an attested THOR call - a `serve mark` from a
    /// shell - before the start cannot shrink the frozen list; an attested
    /// one can.
    #[test]
    fn an_unattested_verdict_does_not_shrink_the_frozen_list() {
        let mut store = EventStore::in_memory().unwrap();
        for id in ["ua-1", "ua-2", "ua-3"] {
            declare_noted(&mut store, &light_command_rule(id, "ua"));
            serve_in(&mut store, "ua-session", id, JUDGEMENT_DEBT_AFTER);
        }
        mark_verified(&mut store, "ua-1", "shell", false);
        store.append_event("shell", "shell", "cli", EventKind::ItemMarkedNoise, "ua-2", None, "{}").unwrap();
        mark_verified(&mut store, "ua-3", "ua-session", true);
        let start = record_start(&mut store, "ua-session", "ua");
        assert_eq!(start.frozen.judgement, vec!["ua-1".to_string(), "ua-2".to_string()], "only the attested verdict cleared its item");
    }

    #[test]
    fn verdict_from_another_session_does_not_settle_the_judgement_list() {
        let mut store = EventStore::in_memory().unwrap();
        declare_noted(&mut store, &light_command_rule("vs-item", "vs"));
        serve_in(&mut store, "vs-session", "vs-item", JUDGEMENT_DEBT_AFTER);
        let start = record_start(&mut store, "vs-session", "vs");
        assert_eq!(start.frozen.judgement, vec!["vs-item".to_string()]);

        mark_verified(&mut store, "vs-item", "another-session", true);
        mark_verified(&mut store, "vs-item", "vs-session", false);
        let err = file_in(&mut store, &eval_report("eval-vs-1", "vs"), &start).unwrap_err();
        assert!(err.to_string().contains("'vs-item'"), "another session's verdict, and an unattested one, settle nothing: {err}");

        mark_verified(&mut store, "vs-item", "vs-session", true);
        file_in(&mut store, &eval_report("eval-vs-1", "vs"), &start).expect("this session's attested verdict settles it");
    }

    /// F15: folding a listed item into another (an attested retract) settles
    /// it; the same retract from a shell does not; a listed item pinned
    /// mid-evaluation is skipped. None of them holds the filing for good: the
    /// unattested one can still be skipped with a reason.
    #[test]
    fn an_attested_retract_after_the_start_settles_a_listed_item() {
        let mut store = EventStore::in_memory().unwrap();
        for id in ["ar-folded", "ar-shell", "ar-pinned"] {
            declare_noted(&mut store, &light_command_rule(id, "ar"));
            serve_in(&mut store, "ar-session", id, JUDGEMENT_DEBT_AFTER);
        }
        let start = record_start(&mut store, "ar-session", "ar");
        assert_eq!(start.frozen.judgement.len(), 3);

        let folded = retract(&mut store, "ar-session", "l", "mcp", "ar-folded", "folded into 'ar-pinned'", None).unwrap();
        attest(&mut store, "ar-session", "ar-folded", folded.seq);
        let mut pinned = show(&store, "ar-pinned").unwrap();
        let before = pinned.clone();
        pinned.bindings.push(Binding::Always);
        let pin = revise(&mut store, "ar-session", "l", "mcp", &before, &pinned).unwrap();
        attest(&mut store, "ar-session", "ar-pinned", pin.seq);

        let judgement = evaluation_judgement_owed(&store, &start, &[]);
        assert_eq!(judgement.settled, vec!["ar-folded".to_string()]);
        assert_eq!(judgement.owed, vec!["ar-shell".to_string()], "nothing was done about this one yet");
        assert!(!judgement.owed.contains(&"ar-pinned".to_string()) && !judgement.settled.contains(&"ar-pinned".to_string()), "pinned: skipped");
        assert!(frozen_writes_in_window(&store, &start, None).outside.is_empty(), "attested writes are no side door");

        let mut skipping = eval_report("eval-ar-1", "ar");
        skipping.tags.push(format!("{NOT_JUDGEABLE_TAG_PREFIX}ar-shell:its command is gone and I never saw it fire"));
        file_in(&mut store, &skipping, &start).expect("the one left owed can be skipped with a reason");
    }

    /// D10: the same retract from a shell - no stamp of the tool server - is
    /// a write to a frozen item behind the evaluation. It settles nothing,
    /// the filing is refused naming the item, the log seq and the door, and
    /// once the start record is closed as void the session has no evaluation
    /// open any more. THOR's own writes in the window (a retest, the machine
    /// pass, the attester) never do that (G15.4).
    #[test]
    fn a_side_door_write_to_a_frozen_item_refuses_the_filing_and_voids_the_evaluation() {
        let mut store = EventStore::in_memory().unwrap();
        for id in ["sd-shell", "sd-machine"] {
            declare_noted(&mut store, &light_command_rule(id, "sd"));
            serve_in(&mut store, "sd-session", id, JUDGEMENT_DEBT_AFTER);
        }
        let start = record_start(&mut store, "sd-session", "sd");
        assert_eq!(start.frozen.judgement.len(), 2);

        // THOR's own writes: a revise by the retest, and the attester's record.
        let rule = show(&store, "sd-machine").unwrap();
        let record = sealed(&rule, RecordedVerdict::Note);
        let by_retest =
            revise_tried_scoped(&mut store, "retest", "retest", SystemActor::new("retest"), &rule, &rule, &rule, None, None, Some(&record), None, None)
                .unwrap();
        assert_eq!(by_retest.actor, "retest");
        mark_verified(&mut store, "sd-machine", "sd-session", true);
        mark_verified(&mut store, "sd-shell", "sd-session", true);
        assert_eq!(frozen_writes_in_window(&store, &start, None), FrozenWrites::default(), "machine writes and verdicts void nothing");
        assert!(check_evaluation_report_filing(&store, &eval_report("eval-sd-1", "sd"), FilingContext::new(INFLIGHT_UNSEEN, start.clone()).as_ref()).is_none());

        let shell = retract(&mut store, "shell", "l", "cli", "sd-shell", "fixture-command-sd-shell no longer exists", None).unwrap();
        let written = frozen_writes_in_window(&store, &start, None);
        assert_eq!(written.outside.len(), 1, "{written:?}");
        assert_eq!((written.outside[0].id.as_str(), written.outside[0].seq), ("sd-shell", shell.seq));
        assert!(written.outside[0].door.contains("written by 'cli' with no stamp of THOR's tool server"), "{:?}", written.outside[0]);
        assert_eq!(frozen_writes_in_window(&store, &start, Some(shell.seq - 1)).outside.len(), 0, "truncated before it: nothing");

        let err = file_in(&mut store, &eval_report("eval-sd-1", "sd"), &start).unwrap_err().to_string();
        assert!(
            err.starts_with(&format!("'sd-shell' was changed at log seq {} outside an attested THOR call of this evaluation (", shell.seq)),
            "{err}"
        );
        assert!(err.contains("An evaluation whose list was changed behind it cannot count. The next Stop starts a fresh evaluation with a fresh list"), "{err}");
        assert!(show(&store, "eval-sd-1").is_err(), "nothing filed");

        // Closing it: one more revise by eval-machine on the start record.
        assert_eq!(open_evaluation_start(&store, "sd-session", "sd").map(|s| s.id), Some(start.id.clone()));
        let actor = || SystemActor::new(EVAL_MACHINE_ACTOR);
        assert!(record_evaluation_void(&mut store, SystemActor::new("retest"), &start, &written.outside[0]).is_err(), "eval-machine only");
        let void = record_evaluation_void(&mut store, actor(), &start, &written.outside[0]).unwrap();
        assert_eq!(void.frozen.void.as_ref(), Some(&written.outside[0]));
        assert_eq!(void.batch_seq, start.batch_seq, "the batch record's seq stays the batch record's");
        assert_eq!(open_evaluation_start(&store, "sd-session", "sd"), None, "a void evaluation is not open");
        let events = store.get_events_by_entity(&start.id).unwrap();
        assert_eq!(events.len(), 3, "the start, the batch record, the void mark");
        assert!(events.iter().all(|e| e.actor == EVAL_MACHINE_ACTOR));
        assert!(show(&store, &start.id).unwrap().text.contains("THIS EVALUATION IS VOID: 'sd-shell' was changed at log seq"));
        let again = record_evaluation_void(&mut store, actor(), &start, &written.outside[0]).unwrap();
        assert_eq!(again, void, "voided once");
        assert_eq!(store.get_events_by_entity(&start.id).unwrap().len(), 3);
        // Filing under the void record is refused with the same sentence,
        // and the refusal for "nothing open" names why.
        let err = file_in(&mut store, &eval_report("eval-sd-1", "sd"), &void).unwrap_err().to_string();
        assert!(err.starts_with("'sd-shell' was changed at log seq"), "{err}");
        let none = no_open_evaluation_refusal(&store, "sd-session", "sd");
        assert!(none.problem.contains("is void: 'sd-shell' was changed at log seq"), "{}", none.problem);
        // A fresh start has a fresh list: the retracted item is not on it.
        let fresh = record_start(&mut store, "sd-session", "sd");
        assert_eq!(fresh.frozen.judgement, Vec::<String>::new(), "sd-machine was judged, sd-shell is gone");
        file_in(&mut store, &eval_report("eval-sd-1", "sd"), &fresh).expect("the fresh evaluation files");
    }

    /// A write of the filing turn: stamped as the evaluating session's own
    /// THOR call, not verified yet. It counts as not yet attested, and the
    /// refusal says to end the turn once - it is never a side door.
    #[test]
    fn a_write_of_the_filing_turn_is_not_yet_attested_and_the_refusal_says_end_the_turn() {
        let mut store = EventStore::in_memory().unwrap();
        for id in ["tn-marked", "tn-folded"] {
            declare_noted(&mut store, &light_command_rule(id, "tn"));
            serve_in(&mut store, "tn-session", id, JUDGEMENT_DEBT_AFTER);
        }
        let start = record_start(&mut store, "tn-session", "tn");
        let stamp = |tool_use_id: &str| Att {
            tool_use_id: Some(tool_use_id.to_string()),
            args_sha256: "fixture".to_string(),
            session_hint: Some("tn-session".to_string()),
            inflight: Some(INFLIGHT_UNSEEN.to_string()),
            side_door: None,
        };
        let body = with_att(serde_json::json!({"id": "tn-marked"}).to_string(), Some(&stamp("toolu_mark"))).unwrap();
        let mark = store.append_event("tn-session", "tn-session", "mcp", EventKind::ItemMarkedUseful, "tn-marked", None, &body).unwrap();
        let fold = retract(&mut store, "tn-session", "l", "mcp", "tn-folded", "folded into 'tn-marked'", Some(&stamp("toolu_fold"))).unwrap();

        let judgement = evaluation_judgement_owed(&store, &start, &[]);
        assert_eq!(judgement.owed.len(), 2, "{judgement:?}");
        assert_eq!(judgement.unverified, vec![("tn-folded".to_string(), fold.seq), ("tn-marked".to_string(), mark.seq)]);
        let written = frozen_writes_in_window(&store, &start, None);
        assert!(written.outside.is_empty(), "a call of this session waiting for its check is no side door: {written:?}");
        assert_eq!(written.pending, vec![("tn-folded".to_string(), fold.seq)]);

        let err = file_in(&mut store, &eval_report("eval-tn-1", "tn"), &start).unwrap_err().to_string();
        assert!(err.contains(TURN_NOT_ENDED), "{err}");
        assert!(err.contains("the verdicts you gave in this turn are checked when the turn ends - end the turn once, then file again"), "{err}");
        assert!(err.contains(&format!("'tn-marked' (its verdict at log seq {})", mark.seq)), "{err}");

        // The turn ends: the Stop records both as attested.
        attest(&mut store, "tn-session", "tn-marked", mark.seq);
        attest(&mut store, "tn-session", "tn-folded", fold.seq);
        file_in(&mut store, &eval_report("eval-tn-1", "tn"), &start).expect("verified, so the report files");
    }

    /// R15.6 at the store: a verdict on an item that is NOT on the frozen
    /// list settles nothing on it, and neither does an unattested verdict on
    /// one that is - a side-door mark, a mark from another session. The list
    /// stays owed for exactly those ids, and the evaluation is not voided: a
    /// verdict is no write to the item.
    #[test]
    fn off_list_and_unattested_verdicts_do_not_settle_the_list() {
        let mut store = EventStore::in_memory().unwrap();
        for id in ["ol-a", "ol-b", "ol-c"] {
            declare_noted(&mut store, &light_command_rule(id, "ol"));
            serve_in(&mut store, "ol-session", id, JUDGEMENT_DEBT_AFTER);
        }
        declare_noted(&mut store, &light_command_rule("ol-off-list", "ol"));
        serve_in(&mut store, "ol-session", "ol-off-list", 3);
        let start = record_start(&mut store, "ol-session", "ol");
        assert_eq!(start.frozen.judgement, vec!["ol-a".to_string(), "ol-b".to_string(), "ol-c".to_string()]);

        // Off the list: an attested verdict of this very session.
        mark_verified(&mut store, "ol-off-list", "ol-session", true);
        // On the list, none attested for this session: a side-door client
        // (recorded unattested), a shell mark with no stamp at all (never
        // recorded), another session's attested verdict.
        mark_verified(&mut store, "ol-a", "ol-session", false);
        store.append_event("shell", "shell", "cli", EventKind::ItemMarkedUseful, "ol-b", None, "{}").unwrap();
        mark_verified(&mut store, "ol-c", "another-session", true);

        let judgement = evaluation_judgement_owed(&store, &start, &[]);
        assert_eq!(judgement.owed, vec!["ol-a".to_string(), "ol-b".to_string(), "ol-c".to_string()]);
        assert!(judgement.settled.is_empty() && judgement.unverified.is_empty(), "{judgement:?}");
        assert_eq!(frozen_writes_in_window(&store, &start, None), FrozenWrites::default(), "a verdict voids nothing");
        let err = file_in(&mut store, &eval_report("eval-ol-1", "ol"), &start).unwrap_err().to_string();
        assert!(err.contains("3 item(s) fired in this session"), "{err}");
        for id in ["'ol-a'", "'ol-b'", "'ol-c'"] {
            assert!(err.contains(id), "{id}: {err}");
        }
        assert!(!err.contains("ol-off-list"), "the off-list item is not this report's: {err}");

        // The machine section of the report that does file lists them as
        // side doors, and the three items as skipped.
        let mut skipping = eval_report("eval-ol-1", "ol");
        for id in ["ol-a", "ol-b", "ol-c"] {
            skipping.tags.push(format!("{NOT_JUDGEABLE_TAG_PREFIX}{id}:judged through a side door, never read here"));
        }
        file_in(&mut store, &skipping, &start).expect("skipped with a reason");
        let text = show(&store, "eval-ol-1").unwrap().text;
        assert!(text.contains("totals: 0 useful, 0 noise, 3 skipped"), "{text}");
        let side = text.split("SIDE DOORS: ").nth(1).expect("the side-door block");
        assert!(side.starts_with("2 write(s) outside an attested THOR call during the evaluation itself (log seq "), "{side}");
        assert!(side.contains("- 'ol-a' item_marked_useful at log seq") && side.contains("fixture: no toolUseId in the call's _meta"), "{side}");
        assert!(side.contains("- 'ol-b' item_marked_useful at log seq") && side.contains("written by 'cli' with no stamp"), "{side}");
        assert!(!side.contains("'ol-c'") && !side.contains("ol-off-list"), "an attested call of any session is no side door: {side}");
    }

    /// Field test 11: the TEETH sentence left out the rules tried again and
    /// left as they were, so "tried 36" listed parts that summed to 35. Every
    /// outcome the pass can record has a part in the sentence.
    #[test]
    fn the_teeth_sentence_parts_add_up_to_the_number_tried() {
        let mut store = EventStore::in_memory().unwrap();
        let outcomes = [
            MachineOutcome::Rule,
            MachineOutcome::Rule,
            MachineOutcome::Note,
            MachineOutcome::PendingHung,
            MachineOutcome::PendingKept,
            MachineOutcome::PendingKept,
            MachineOutcome::Blocked,
            MachineOutcome::NeedsAgent,
            MachineOutcome::A4Only,
            MachineOutcome::Unchanged,
            MachineOutcome::Unchanged,
            MachineOutcome::Unchanged,
        ];
        let items: Vec<MachineItem> =
            outcomes.iter().enumerate().map(|(n, o)| machine_item(&format!("tp-{n}"), *o, "fixture", None)).collect();
        let count = |o: MachineOutcome| items.iter().filter(|i| i.outcome == o).count();
        let pass = MachinePass {
            bites: count(MachineOutcome::Rule),
            notes: count(MachineOutcome::Note),
            pending_hung: count(MachineOutcome::PendingHung),
            blocked: count(MachineOutcome::Blocked),
            needs_agent: count(MachineOutcome::NeedsAgent),
            a4_only: count(MachineOutcome::A4Only),
            items: items.clone(),
            ..MachinePass::default()
        };
        assert_eq!(
            pass.bites + pass.notes + pass.pending_hung + pass.blocked + pass.needs_agent + pass.a4_only + pass.left_as_they_were(),
            items.len(),
            "every outcome has exactly one part"
        );
        let first = record_start_only(&mut store, "tp-session", "tp");
        let start = record_evaluation_batch(&mut store, SystemActor::new(EVAL_MACHINE_ACTOR), &first, "fixture, decided", vec![], 0, vec![], pass)
            .unwrap();
        let filed_seq = store.get_next_seq().unwrap() - 1;
        let section = render_machine_section(&store, &start, filed_seq);
        let line = section.lines().find(|l| l.starts_with("TEETH: THOR tried")).expect("the TEETH sentence");
        let tried: usize = line.split_whitespace().nth(3).and_then(|n| n.parse().ok()).expect("the tried count");
        let parts = line.split("): ").nth(1).and_then(|p| p.split(", not reached").next()).expect("the parts");
        let sum: usize =
            parts.split(", ").map(|part| part.split_whitespace().next().and_then(|n| n.parse::<usize>().ok()).expect("a count")).sum();
        assert_eq!((tried, sum), (12, 12), "{line}");
        assert!(line.contains("5 tried again and left as they were"), "{line}");
    }

    /// B1: the machine section is a pure function of the log truncated at F.
    /// The same start and the same F give the same bytes - whatever was
    /// appended after F, and on a copy of the log that ends at F.
    #[test]
    fn machine_section_is_a_pure_function_of_the_log() {
        let mut store = EventStore::in_memory().unwrap();
        for id in ["mp-useful", "mp-noise", "mp-skipped"] {
            declare_noted(&mut store, &light_command_rule(id, "mp"));
            serve_in(&mut store, "mp-session", id, JUDGEMENT_DEBT_AFTER);
        }
        legacy_thinly_answered(&mut store, "mp-batch", Some("mp"));
        serve_n(&mut store, "mp-batch", "hook", 1);
        let start = record_start(&mut store, "mp-session", "mp");
        assert_eq!(start.frozen.batch(), ["mp-batch".to_string()]);

        let useful = mark_verified(&mut store, "mp-useful", "mp-session", true);
        let noise = store.append_event("mp-session", "mp-session", "mcp", EventKind::ItemMarkedNoise, "mp-noise", None, "{}").unwrap().seq;
        record_write_verification(&mut store, "mp-session", SystemActor::new(ATTESTER_ACTOR), "mp-noise", noise, true, Some("a1"), "fixture").unwrap();
        let settled = settle_by_naming_the_trigger(&mut store, "mp-batch");
        attest(&mut store, "mp-session", "mp-batch", settled.seq);
        store.append_event("x", "x", "hook", EventKind::GateRefused, "mp-batch", None, "{}").unwrap();

        let filed_seq = store.get_next_seq().unwrap() - 1;
        let section = render_machine_section(&store, &start, filed_seq);
        assert_eq!(render_machine_section(&store, &start, filed_seq), section, "twice the same");
        let expected_first = format!("=== MACHINE SECTION: derived from the log, seq {} to {filed_seq}; it cannot be edited ===", start.frozen.started_seq);
        assert_eq!(section.lines().next(), Some(expected_first.as_str()));
        assert!(section.is_ascii() && !section.contains('\r'), "plain ASCII, LF only");
        assert!(section.contains(&format!("EVALUATION: project 'mp'; session mp-session (the main session, agent a1); started by skill (THOR's record '{}'", start.id)), "{section}");
        assert!(section.contains(&format!("- 'mp-useful': useful - verdict at log seq {useful} by the main session")), "{section}");
        assert!(section.contains(&format!("- 'mp-noise': noise - verdict at log seq {noise} by agent a1")), "{section}");
        assert!(section.contains("- 'mp-skipped': skipped - no verdict from this evaluation"), "{section}");
        assert!(section.contains("totals: 1 useful, 1 noise, 1 skipped, 0 retracted or taken off the rule track, 0 pinned; 50% useful of 2 verdict(s)."), "{section}");
        assert!(section.contains("frozen batch: 1 rule(s)."), "{section}");
        assert!(
            section.contains(&format!("- 'mp-batch': a note by THOR's trial at log seq {}, written by an attested THOR call of the main session", settled.seq)),
            "{section}"
        );
        assert!(section.contains("frozen A4 list: 0 rule(s)."), "{section}");
        assert!(section.contains("OPEN POINTS: 0 of THOR's own, 0 legacy."), "{section}");
        assert!(section.contains("1 not verified yet"), "the hook's refusal in the window has no recorded check: {section}");
        assert!(section.contains("SIDE DOORS: 0 write(s) outside an attested THOR call during the evaluation itself (log seq "), "{section}");
        assert_eq!(
            machine_section_summary(&store, &start, filed_seq),
            MachineSummary {
                started_seq: start.frozen.started_seq,
                filed_seq,
                listed: 3,
                useful: 1,
                noise: 1,
                skipped: 1,
                teeth_listed: 1,
                ..MachineSummary::default()
            }
        );

        // A copy of the log that ENDS at F gives the same bytes.
        let shipped: Vec<thor_core::event_store::ShippedEvent> =
            store.get_all_events().unwrap().iter().filter(|e| e.seq <= filed_seq).map(|e| e.to_shipped()).collect();
        let mut truncated = EventStore::in_memory().unwrap();
        truncated.ingest_batch(&shipped).unwrap();
        assert_eq!(truncated.get_next_seq().unwrap() - 1, filed_seq);
        assert_eq!(render_machine_section(&truncated, &start, filed_seq), section, "the truncated log renders the same bytes");

        // Everything appended after F changes nothing about the section at F:
        // a side-door verdict, a retract of a listed item, a new verdict, the
        // attester's record of each, more refusals.
        mark_verified(&mut store, "mp-skipped", "mp-session", true);
        mark_verified(&mut store, "mp-useful", "shell", false);
        retract(&mut store, "shell", "l", "cli", "mp-noise", "fixture-command-mp-noise no longer exists", None).unwrap();
        store.append_event("x", "x", "hook", EventKind::GateRefused, "mp-batch", None, "{}").unwrap();
        let other = record_start(&mut store, "other-session", "mp");
        file_in(&mut store, &eval_report("eval-mp-other", "mp"), &other).unwrap();
        assert_eq!(render_machine_section(&store, &start, filed_seq), section, "later events do not reach a section at F");
        assert_ne!(render_machine_section(&store, &start, store.get_next_seq().unwrap() - 1), section, "a later F reads them");
    }

    /// Step 22 (C3b B4): an agent cleared a TRUE check with a because and the
    /// machine section said "0 weakenings", so the owner could only see it in
    /// the item's history. A check cleared, moved to another file or changed in
    /// the window by a write that is not THOR's own is listed and counted; THOR's
    /// own verdict clearing a check (a note by its trial) is the pass's, not a
    /// weakening to report.
    #[test]
    fn a_check_cleared_or_moved_in_the_window_is_in_the_machine_section() {
        let mut store = EventStore::in_memory().unwrap();
        let rule_with = |id: &str, anchor: &str, check: Check| {
            let mut item = checked_sample();
            item.id = id.to_string();
            item.project = Some("mp".to_string());
            item.text = format!("{anchor} keeps the line `keep me` for {id}.");
            item.bindings = vec![Binding::Target { kind: crate::item::TargetKind::Path, value: anchor.to_string() }];
            item.check = Some(check);
            // A check that guards its point carries no "nothing can catch
            // this" reason beside it (step 26).
            item.tags.clear();
            item
        };
        let plan = rule_with("plan-here", "files/PLAN.md", Check::PathExists { path: "files/PLAN.md".to_string() });
        let moved = rule_with("moved-here", "docs/A.md", Check::Contains { path: "docs/A.md".to_string(), literal: "keep me".to_string() });
        let noted = rule_with("noted-here", "docs/B.md", Check::PathExists { path: "docs/B.md".to_string() });
        for item in [&plan, &moved, &noted] {
            declare(&mut store, "mp-session", "l", "test", item).unwrap();
        }
        let start = record_start(&mut store, "mp-session", "mp");

        // An agent clears one check and moves another's file, each with a because.
        let mut cleared = plan.clone();
        cleared.check = None;
        let baseline = gate::ClearedFields { check: true, ..Default::default() }.baseline(&plan);
        let cleared_at = revise_because(&mut store, "mp-session", "l", "mcp", &baseline, &cleared, &plan, Some("the file is in a full anchor")).unwrap().seq;
        let mut elsewhere = moved.clone();
        elsewhere.check = Some(Check::Contains { path: "docs/B.md".to_string(), literal: "keep me".to_string() });
        let moved_at = revise_because(&mut store, "mp-session", "l", "mcp", &moved, &elsewhere, &moved, Some("the file moved")).unwrap().seq;
        // THOR's own verdict clears a check as well (a note): not listed.
        let mut as_note = noted.clone();
        as_note.check = None;
        let body = canonical_body(&as_note).unwrap();
        store.append_event("mp-session", "l", "teeth-eval", EventKind::FactRevised, &noted.id, None, &body).unwrap();

        let filed_seq = store.get_next_seq().unwrap() - 1;
        let section = render_machine_section(&store, &start, filed_seq);
        assert!(
            section.contains("rules whose check was cleared, moved or changed by an agent's write rather than THOR's own trial in the window: 2."),
            "{section}"
        );
        assert!(
            section.contains(&format!("- 'plan-here': check cleared (path_exists files/PLAN.md) at log seq {cleared_at} by")),
            "{section}"
        );
        assert!(section.contains("reason: the file is in a full anchor"), "{section}");
        assert!(
            section.contains(&format!("- 'moved-here': check moved from contains docs/A.md to contains docs/B.md at log seq {moved_at} by")),
            "{section}"
        );
        assert!(!section.contains("'noted-here'"), "THOR's own verdict is not an agent's weakening: {section}");
        assert_eq!(machine_section_summary(&store, &start, filed_seq).checks_weakened, 2);
    }

    /// A rule stored by dispute, the way the write tools store one: a Note
    /// whose record carries what `edit` puts in it, and the writer's `because`
    /// spliced into the event body.
    fn declare_disputed(store: &mut EventStore, id: &str, because: Option<&str>, edit: impl FnOnce(&mut TrialFields)) -> Event {
        let mut item = plain_note(id);
        // Distinct words per id, or the gate reads two of them as one fact.
        let stem: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        item.text = format!("{stem}aa {stem}bb {stem}cc {stem}dd {stem}ee {stem}ff {stem}gg {stem}hh.");
        let mut fields = trial_fields(&item, RecordedVerdict::Note);
        edit(&mut fields);
        let record = TrialRecord::seal(fields);
        declare_in_scoped_because(store, "s", "l", "a", &item, None, None, Some(&record), None, because).unwrap()
    }

    fn may_change(span: &str) -> impl FnOnce(&mut TrialFields) {
        let span = span.to_string();
        move |f| f.disputes.may_change = vec![span]
    }

    /// The disputes of the window, one line each, derived from the log: the
    /// argument, the calls or spans it names and the writer's own reason -
    /// "no reason recorded" when there is none.
    #[test]
    fn the_machine_section_lists_every_dispute_with_its_argument_its_literal_and_its_reason() {
        let mut store = EventStore::in_memory().unwrap();
        let start = record_start(&mut store, "md-session", "demo");
        declare_disputed(&mut store, "dsp-span", Some("the version line moves with every release, so it may change"), may_change("version"));
        declare_disputed(&mut store, "dsp-call", None, |f| {
            f.disputes.legitimate_calls = vec!["git bundle list-heads backup.bundle".to_string()]
        });
        declare_disputed(&mut store, "dsp-kept", Some("refusing every push is the whole intent of this ban"), |f| {
            f.accept_refusing = vec!["git push origin main".to_string()]
        });
        let filed_seq = store.get_next_seq().unwrap() - 1;
        let section = render_machine_section(&store, &start, filed_seq);
        assert!(section.contains("\nnotes by dispute written in the window: 2.\n- 'dsp-span' at log seq"), "{section}");
        assert!(
            section.contains(": may_change ['version'] - because: the version line moves with every release, so it may change"),
            "{section}"
        );
        assert!(
            section.contains(": legitimate_calls ['git bundle list-heads backup.bundle'] - because: no reason recorded"),
            "a dispute with no reason says so: {section}"
        );
        assert!(section.contains("\nrules kept with accept_refusing written in the window: 1.\n- 'dsp-kept' at log seq"), "{section}");
        assert!(
            section.contains(": accept_refusing ['git push origin main'] - because: refusing every push is the whole intent of this ban"),
            "{section}"
        );
        assert!(section.is_ascii() && !section.contains('\r'), "plain ASCII, LF only");
    }

    /// The agent's later revise with a reason is the dispute verified, and the
    /// section says so on the same line.
    #[test]
    fn the_machine_section_says_when_a_dispute_was_verified_afterwards() {
        let mut store = EventStore::in_memory().unwrap();
        let start = record_start(&mut store, "mv-session", "demo");
        declare_disputed(&mut store, "dsp-later", Some("the first reason, written with the dispute"), may_change("version"));
        let existing = show(&store, "dsp-later").unwrap();
        let seq = revise_tried_scoped(&mut store, "mv-session", "l", "a", &existing, &existing, &existing, Some("restated: only the version number moves"), None, None, None, None)
            .unwrap()
            .seq;
        let filed_seq = store.get_next_seq().unwrap() - 1;
        let section = render_machine_section(&store, &start, filed_seq);
        assert!(section.contains(&format!("- since then verified at log seq {seq} by ")), "{section}");
        assert!(section.contains(": restated: only the version number moves"), "{section}");
    }

    /// THOR's own retest carries a dispute forward under its own name; the
    /// reason is still the writer's, looked up through it.
    #[test]
    fn a_dispute_reason_is_read_through_thors_own_retest_writes() {
        let mut store = EventStore::in_memory().unwrap();
        declare_disputed(&mut store, "dsp-carried", Some("the version line moves with every release"), may_change("version"));
        let existing = show(&store, "dsp-carried").unwrap();
        let mut fields = trial_fields(&existing, RecordedVerdict::Note);
        fields.disputes.may_change = vec!["version".to_string()];
        let record = TrialRecord::seal(fields);
        revise_tried_scoped(&mut store, "retest", "l", SystemActor::new("retest"), &existing, &existing, &existing, Some("no tooth bites"), None, Some(&record), None, None)
            .unwrap();
        assert_eq!(current_dispute_reason(&store, "dsp-carried").as_deref(), Some("the version line moves with every release"));
        declare_disputed(&mut store, "dsp-silent", None, may_change("version"));
        assert_eq!(current_dispute_reason(&store, "dsp-silent"), None, "no reason was ever recorded");
        assert_eq!(current_dispute_reason(&store, "no-such-item"), None);
    }

    /// Which disputes an evaluation hands to the agent: the ones written since
    /// the previous report, plus the left-over ones first, minus the answered.
    #[test]
    fn unverified_disputes_follow_the_window_the_carry_and_the_answers() {
        let mut store = EventStore::in_memory().unwrap();
        declare_disputed(&mut store, "old-one", None, may_change("version"));
        let since = store.get_next_seq().unwrap() - 1;
        declare_disputed(&mut store, "new-one", Some("only the version number moves, never the rule"), may_change("version"));
        declare_disputed(&mut store, "new-two", None, may_change("version"));
        declare_disputed(&mut store, "new-answered", None, may_change("version"));
        let existing = show(&store, "new-answered").unwrap();
        revise_tried_scoped(&mut store, "s", "l", "a", &existing, &existing, &existing, Some("restated: the span is incidental"), None, None, None, None)
            .unwrap();
        let ids = |carried: &[String]| -> Vec<String> {
            unverified_disputes(&store, "demo", since, carried).into_iter().map(|d| d.id).collect()
        };
        assert_eq!(ids(&[]), ["new-one", "new-two"], "after the window, answered ones left out");
        assert_eq!(ids(&["old-one".to_string()]), ["old-one", "new-one", "new-two"], "a left-over one comes first");
        let list = unverified_disputes(&store, "demo", since, &[]);
        assert_eq!(list[0].because.as_deref(), Some("only the version number moves, never the rule"));
        assert_eq!(list[1].because, None);
        assert_eq!(list[0].arguments, [("may_change".to_string(), vec!["version".to_string()])]);
        assert!(unverified_disputes(&store, "other-project", since, &[]).is_empty(), "another project's rules are not listed");
    }

    /// A start freezes the disputes; the filing is held until the agent has
    /// answered each one with its own revise and a reason, and nothing else
    /// answers it.
    #[test]
    fn a_frozen_dispute_holds_the_filing_until_the_agent_answers_it() {
        let mut store = EventStore::in_memory().unwrap();
        declare_disputed(&mut store, "dsp-held", Some("the version line moves with every release"), may_change("version"));
        let first = record_start_only(&mut store, "gd-session", "demo");
        let pass = MachinePass { disputes: unverified_disputes(&store, "demo", 0, &[]), ..MachinePass::default() };
        let start = record_evaluation_batch(&mut store, SystemActor::new(EVAL_MACHINE_ACTOR), &first, "fixture, decided", vec![], 0, vec![], pass)
            .unwrap();
        assert_eq!(start.frozen.dispute_list().len(), 1);
        assert!(frozen_ids(&start).contains(&"dsp-held".to_string()), "a frozen dispute is a frozen item");

        let err = file_in(&mut store, &eval_report("eval-demo-1", "demo"), &start).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("1 dispute(s) on this evaluation's frozen dispute list"), "{}", refusal.problem);
        assert!(refusal.problem.contains("'dsp-held'") && refusal.problem.contains("may_change ['version']"), "{}", refusal.problem);
        assert!(refusal.problem.contains("mcp__thor__revise(id: \"<id>\", because:"), "{}", refusal.problem);

        // A revise that gives no reason answers nothing.
        let existing = show(&store, "dsp-held").unwrap();
        let mut reworded = existing.clone();
        reworded.tags = vec!["a-tag".to_string()];
        let silent = revise_tried_scoped(&mut store, "gd-session", "l", "a", &existing, &reworded, &existing, None, None, None, None, None).unwrap().seq;
        attest(&mut store, "gd-session", "dsp-held", silent);
        assert_eq!(evaluation_frozen_disputes_owed(&store, &start).len(), 1);

        // One with a reason does.
        let existing = show(&store, "dsp-held").unwrap();
        let answered =
            revise_tried_scoped(&mut store, "gd-session", "l", "a", &existing, &existing, &existing, Some("only the version number moves, never the rule"), None, None, None, None)
                .unwrap()
                .seq;
        attest(&mut store, "gd-session", "dsp-held", answered);
        assert!(evaluation_frozen_disputes_owed(&store, &start).is_empty());
        file_in(&mut store, &eval_report("eval-demo-1", "demo"), &start).expect("answered, so the report files");
    }

    /// FALSE BLOCKS AS A NUMBER: the machine section links each verified
    /// refusal of the window to what happened to its rule afterwards. A rule
    /// whose check an agent cleared or that was retracted after refusing is a
    /// probable false block; one left alone is not; a refusal that is not
    /// verified, or was made by the evaluating session itself, is not counted.
    #[test]
    fn the_machine_section_links_each_verified_refusal_to_what_happened_to_its_rule() {
        let mut store = EventStore::in_memory().unwrap();
        let ids = [
            ("ro-weak", "ledger"),
            ("ro-gone", "manifest"),
            ("ro-same", "roster"),
            ("ro-own", "timetable"),
            ("ro-unverified", "glossary"),
        ];
        for (id, subject) in ids {
            let mut item = checked_sample();
            item.id = id.to_string();
            item.project = Some("mp".to_string());
            item.text = format!("The {subject} file files/{id}.md keeps the line `keep me`.");
            item.bindings = vec![Binding::Target { kind: crate::item::TargetKind::Path, value: format!("files/{id}.md") }];
            item.check = Some(Check::Contains { path: format!("files/{id}.md"), literal: "keep me".to_string() });
            item.tags.clear();
            declare(&mut store, "mp-session", "l", "test", &item).unwrap();
        }
        let start = record_start(&mut store, "mp-session", "mp");
        let quiet_seq = store.get_next_seq().unwrap() - 1;
        let refuse = |store: &mut EventStore, session: &str, id: &str, verified: Option<bool>| {
            let body = r#"{"at":"2026-10-01T00:00:00Z","target":"files/x.md","tool_use_id":"toolu_1"}"#;
            let seq = store.append_event(session, session, "hook", EventKind::GateRefused, id, None, body).unwrap().seq;
            if let Some(field) = verified {
                record_refusal_verification(store, session, SystemActor::new(ATTESTER_ACTOR), id, seq, field, "fixture").unwrap();
            }
        };
        for id in ["ro-weak", "ro-gone", "ro-same"] {
            refuse(&mut store, "other-session", id, Some(true));
        }
        refuse(&mut store, "mp-session", "ro-own", Some(true));
        refuse(&mut store, "other-session", "ro-unverified", None);

        let item_of = |store: &EventStore, id: &str| {
            let events = store.get_events_by_entity(id).unwrap();
            item_before(&events, i64::MAX).unwrap()
        };
        let weak = item_of(&store, "ro-weak");
        let mut cleared = weak.clone();
        cleared.check = None;
        let baseline = gate::ClearedFields { check: true, ..Default::default() }.baseline(&weak);
        revise_because(&mut store, "mp-session", "l", "mcp", &baseline, &cleared, &weak, Some("the file is gone")).unwrap();
        retract(&mut store, "shell", "l", "cli", "ro-gone", "files/ro-gone.md is gone", None).unwrap();

        let none = render_machine_section(&store, &start, quiet_seq);
        assert!(none.contains("\nREFUSAL OUTCOMES in the window: no verified refusal "), "{none}");
        let filed_seq = store.get_next_seq().unwrap() - 1;
        let section = render_machine_section(&store, &start, filed_seq);
        let line = section.lines().find(|l| l.starts_with("REFUSAL OUTCOMES in the window:")).expect("the line");
        assert!(line.starts_with("REFUSAL OUTCOMES in the window: 3 verified refusal(s)"), "{line}");
        assert!(line.contains("2 followed by a weakening or retract of the refusing rule (probable false blocks)"), "{line}");
        assert!(line.contains(&crate::refusal_outcome::rate_words(2, 3)), "{line}");
        assert!(line.contains("A proxy"), "{line}");
        assert!(section.is_ascii() && !section.contains('\r'), "plain ASCII, LF only");
        assert_eq!(render_machine_section(&store, &start, filed_seq), section, "pure");
    }

    /// THE OWNER'S GO (step 26), at the write gate: the marker of a rule marked
    /// owner-approval need not be named in its text (it is answered by the
    /// owner, never the call, and naming it published it to every agent); the
    /// tag belongs on a requires check only; taking it off is a weakening that
    /// needs a because; and the migration marks a go-marker rule once.
    #[test]
    fn the_owners_go_at_the_write_gate_and_its_migration() {
        let mut store = EventStore::in_memory().unwrap();
        let mut rule = heavy_command_rule("og-rule", "og");
        rule.tags = vec![OWNER_APPROVAL_TAG.to_string()];
        rule.text = "Never touch the fixture command without the owner's explicit go.".to_string();
        rule.check = Some(Check::Requires { when: "fixture-command-og-rule".to_string(), required: vec!["go-owner".to_string()] });
        assert!(gate::unnamed_literals(&rule).is_empty(), "the owner's marker is exempt from A4");
        declare(&mut store, "s", "l", "a", &rule).expect("an owner rule whose text does not name its marker");

        let mut plain = heavy_command_rule("og-plain", "og");
        plain.tags = vec![OWNER_APPROVAL_TAG.to_string()];
        let err = declare(&mut store, "s", "l", "a", &plain).unwrap_err();
        assert!(err.to_string().contains("carries no requires check"), "{err}");

        let mut dropped = rule.clone();
        dropped.tags.clear();
        dropped.text = "Never touch the fixture command without the owner's explicit go-owner.".to_string();
        assert!(
            gate::weakenings(&rule, &dropped).iter().any(|w| w.label == "owner approval dropped"),
            "taking the owner's go off is a weakening"
        );

        // The migration: a go-marker rule without the tag is marked, once.
        let mut old = heavy_command_rule("og-old", "og");
        old.tags.clear();
        old.text = "Never touch the fixture command without go-owner in the call.".to_string();
        old.check = Some(Check::Requires { when: "fixture-command-og-old".to_string(), required: vec!["go-owner".to_string()] });
        legacy(&mut store, &old);
        let other = Check::Requires { when: "fixture-command-og-other".to_string(), required: vec!["--dry-run".to_string()] };
        let mut not_a_marker = heavy_command_rule("og-other", "og");
        not_a_marker.tags.clear();
        not_a_marker.text = "The fixture command always runs with --dry-run first.".to_string();
        not_a_marker.check = Some(other);
        legacy(&mut store, &not_a_marker);
        assert_eq!(migrate_owner_approval(&mut store, "s"), vec!["og-old".to_string()]);
        assert!(is_owner_approval(&show(&store, "og-old").unwrap()));
        assert!(!is_owner_approval(&show(&store, "og-other").unwrap()), "a literal that is no approval marker stays as it is");
        assert!(migrate_owner_approval(&mut store, "s").is_empty(), "idempotent");
        assert!(is_approval_marker("go-alpha") && is_approval_marker("go-owner2") && !is_approval_marker("go-") && !is_approval_marker("Go-Alpha"));
    }

    /// Field test 2 (2026-10-01): session B's first round was closed as void
    /// and a fresh one started; the report's machine section covered only the
    /// fresh round, so the 61 verdicts and the edits of the first round had no
    /// line and the agent summarised them from memory (67, 66, 62). An earlier
    /// round of the same project that was closed as void or superseded gets a
    /// line: its session, how it closed, and that session's verdicts and edits
    /// between its start and this one.
    #[test]
    fn an_earlier_round_closed_before_this_one_has_a_line_in_the_machine_section() {
        let mut store = EventStore::in_memory().unwrap();
        let mut rule = checked_sample();
        rule.id = "er-rule".to_string();
        rule.project = Some("mp".to_string());
        declare(&mut store, "s-old", "l", "test", &rule).unwrap();
        let old = record_start(&mut store, "s-old", "mp");
        mark_verified(&mut store, "er-rule", "s-old", true);
        mark_verified(&mut store, "er-rule", "s-old", true);
        mark_verified(&mut store, "er-rule", "someone-else", true);
        let at_seq = store.get_next_seq().unwrap() - 1;
        let superseded = FrozenSuperseded { by_session: "s-new".to_string(), at_seq, idle_minutes: Some(45) };
        record_evaluation_superseded(&mut store, SystemActor::new(EVAL_MACHINE_ACTOR), &old, &superseded).unwrap();
        let start = record_start(&mut store, "s-new", "mp");
        let filed_seq = store.get_next_seq().unwrap() - 1;
        let section = render_machine_section(&store, &start, filed_seq);
        assert!(section.contains("EARLIER ROUNDS of this project closed before this one: 1."), "{section}");
        assert!(
            section.contains(&format!("- '{}' by session s-old (superseded by session s-new): 2 verdict(s) (2 useful, 0 noise) and 0 edit(s)", old.id)),
            "{section}"
        );

        // No earlier round: no line at all, so a section is byte for byte what
        // it was before this existed.
        let mut fresh = EventStore::in_memory().unwrap();
        declare(&mut fresh, "s-new", "l", "test", &rule).unwrap();
        let only = record_start(&mut fresh, "s-new", "mp");
        let filed = fresh.get_next_seq().unwrap() - 1;
        assert!(!render_machine_section(&fresh, &only, filed).contains("EARLIER ROUNDS"));
    }

    /// Field test 2 (2026-10-01): a fold retracted a rule whose check still
    /// held (`contains durable dark` in files/INGRESS.md) and the machine
    /// section listed it only as "retracted", so the lost guard was invisible.
    /// A retract of a rule that carried a check is listed under the checks
    /// too, naming the check that is gone with it.
    #[test]
    fn a_retract_that_takes_a_check_with_it_is_listed_under_the_checks() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = checked_sample();
        item.id = "gone-here".to_string();
        item.project = Some("mp".to_string());
        item.text = "docs/C.md keeps the line `keep me`.".to_string();
        item.bindings = vec![Binding::Target { kind: crate::item::TargetKind::Path, value: "docs/C.md".to_string() }];
        item.check = Some(Check::Contains { path: "docs/C.md".to_string(), literal: "keep me".to_string() });
        item.tags.clear();
        declare(&mut store, "mp-session", "l", "test", &item).unwrap();
        let start = record_start(&mut store, "mp-session", "mp");
        let at = retract(&mut store, "mp-session", "l", "mcp", "gone-here", "folded into the rule on docs/C.md that keeps 'keep me'", None).unwrap().seq;
        let filed_seq = store.get_next_seq().unwrap() - 1;
        let section = render_machine_section(&store, &start, filed_seq);
        assert!(
            section.contains(&format!("- 'gone-here': check lost with the retract (contains docs/C.md) at log seq {at} by")),
            "{section}"
        );
        assert_eq!(machine_section_summary(&store, &start, filed_seq).checks_weakened, 1);
    }

    /// Field test 11: REFUSED and SIDE DOORS read "0" for every report because
    /// they cover only the evaluation itself (SPEC-ENFORCEMENT 7.8 N15 names
    /// that window: from its start to its filing - a refusal outside it could
    /// be planted). The window is deliberately NOT widened; the two lines now
    /// say which window they count, so a reader does not take "0" for the
    /// work period.
    #[test]
    fn refused_and_side_doors_name_the_window_they_count() {
        let mut store = EventStore::in_memory().unwrap();
        declare_noted(&mut store, &light_command_rule("win-rule", "mp"));
        let early = store.append_event("work-session", "work-session", "hook", EventKind::GateRefused, "win-rule", None, "{}").unwrap().seq;
        record_refusal_verification(&mut store, "work-session", SystemActor::new(ATTESTER_ACTOR), "win-rule", early, true, "fixture").unwrap();
        let start = record_start(&mut store, "mp-session", "mp");
        assert!(early < start.frozen.started_seq, "fixture: the refusal came before the evaluation");
        let late = store.append_event("work-session", "work-session", "hook", EventKind::GateRefused, "win-rule", None, "{}").unwrap().seq;
        record_refusal_verification(&mut store, "work-session", SystemActor::new(ATTESTER_ACTOR), "win-rule", late, true, "fixture").unwrap();
        let filed_seq = store.get_next_seq().unwrap() - 1;
        let section = render_machine_section(&store, &start, filed_seq);
        let window = format!("log seq {} to {filed_seq}", start.frozen.started_seq);
        assert!(
            section.contains(&format!("REFUSED: during the evaluation itself ({window}")) && section.contains("the hook refused 1 real tool call(s)"),
            "only the refusal inside the evaluation counts, and the line says so: {section}"
        );
        assert!(
            section.contains(&format!("SIDE DOORS: 0 write(s) outside an attested THOR call during the evaluation itself ({window})")),
            "{section}"
        );
    }

    /// Step 20 (NEW-2): in the machine section, a refusal the EVALUATING
    /// session met inside its own evaluation is never a verified real call -
    /// even with a field verdict recorded before the fix - and the section
    /// says how many were its own. Another session's refusal in the same
    /// window still counts. `evaluation_open_in_session_at` reads the same
    /// window: open after the start, not before it, closed by the filing.
    #[test]
    fn the_evaluating_sessions_own_refusals_are_never_proof_in_its_machine_section() {
        let mut store = EventStore::in_memory().unwrap();
        declare_noted(&mut store, &light_command_rule("own-rule", "mp"));
        let before = store.get_next_seq().unwrap();
        let start = record_start(&mut store, "mp-session", "mp");
        assert!(evaluation_open_in_session_at(&store, "mp-session", before, None).is_none(), "not before its start");
        let own = store.append_event("mp-session", "mp-session", "hook", EventKind::GateRefused, "own-rule", None, "{}").unwrap().seq;
        let other = store.append_event("other-session", "other-session", "hook", EventKind::GateRefused, "own-rule", None, "{}").unwrap().seq;
        for (session, seq) in [("mp-session", own), ("other-session", other)] {
            record_refusal_verification(&mut store, session, SystemActor::new(ATTESTER_ACTOR), "own-rule", seq, true, "fixture").unwrap();
        }
        assert_eq!(evaluation_open_in_session_at(&store, "mp-session", own, None).map(|s| s.id), Some(start.id.clone()));
        assert!(evaluation_open_in_session_at(&store, "other-session", other, None).is_none(), "another session has no evaluation open");
        let filed_seq = store.get_next_seq().unwrap() - 1;
        let section = render_machine_section(&store, &start, filed_seq);
        assert!(section.contains("the hook refused 1 real tool call(s)") && section.contains("1 probe or hand-fed refusal(s)"), "{section}");
        assert!(section.contains("1 of the refusals never proof were calls the evaluating session made itself"), "{section}");
        file_in(&mut store, &eval_report("eval-mp-own", "mp"), &start).unwrap();
        let after = store.get_next_seq().unwrap();
        assert!(evaluation_open_in_session_at(&store, "mp-session", after, None).is_none(), "closed by its filing");
    }

    /// Field test 5 (D7): "write(s) refused by THOR's own trial" counts the
    /// evaluating session's own refusals only; another session's are named
    /// apart, never added.
    #[test]
    fn the_machine_section_counts_only_the_evaluating_sessions_trial_refusals() {
        let mut store = EventStore::in_memory().unwrap();
        let start = record_start(&mut store, "mp-session", "mp");
        for (session, id) in [("mp-session", "own-one"), ("mp-session", "own-two"), ("other-session", "theirs")] {
            record_refused_write(&mut store, session, SystemActor::new(TEETH_WRITE_ACTOR), id, "forcing", &[], "the text").unwrap();
        }
        let filed_seq = store.get_next_seq().unwrap() - 1;
        let section = render_machine_section(&store, &start, filed_seq);
        assert!(section.contains("2 write(s) refused by THOR's own trial."), "{section}");
        assert!(section.contains("1 more write(s) refused by the trial in other sessions during the window are not counted here"), "{section}");
    }

    /// G15.2: two quiet evaluations - nothing on any list, the same one-line
    /// TLDR - both store. Their texts differ only in the log positions of
    /// the machine section, which the near-duplicate check must not read as
    /// "the same fact told twice". Any OTHER report with that text is still
    /// a duplicate of neither.
    #[test]
    fn two_quiet_evaluations_are_not_near_duplicates() {
        let mut store = EventStore::in_memory().unwrap();
        let tldr = "TLDR: evaluation of q. Nothing to add.";
        // A report filed before step 15 - no machine section - with the very
        // same one-line TLDR, the way yesterday's quiet evaluation left it.
        let mut older = eval_report("eval-q-2026-09-27", "q");
        older.text = tldr.to_string();
        legacy(&mut store, &older);
        let mut first = eval_report("eval-q-2026-09-28", "q");
        first.text = tldr.to_string();
        let mut second = eval_report("eval-q-2026-09-29", "q");
        second.text = tldr.to_string();
        file_now(&mut store, &first).expect("the first quiet evaluation stores");
        file_now(&mut store, &second).expect("and so does the second, a day later");
        let (a, b) = (show(&store, &first.id).unwrap().text, show(&store, &second.id).unwrap().text);
        assert!(a.starts_with("=== MACHINE SECTION") && b.starts_with("=== MACHINE SECTION"));
        assert!(a.ends_with(&format!("\n\n{}\n{tldr}", gate::AGENT_SECTION_BEGIN)), "{a}");
        let words = |text: &str| normalize_for_comparison(text);
        assert!(
            jaccard_similarity(&word_set(&words(&a)), &word_set(&words(&b))) >= NEAR_DUPLICATE_JACCARD_THRESHOLD,
            "fixture sanity: the two stored texts ARE near-identical"
        );
        // The exemption is for two evaluation reports only: an ordinary
        // report with the old one's text is still a near-duplicate of it.
        let mut notes = older.clone();
        notes.id = "notes-q-tldr".to_string();
        notes.tags = vec!["notes".to_string()];
        let err = declare(&mut store, "s", "l", "a", &notes).unwrap_err().to_string();
        assert!(err.contains("near-duplicate") && err.contains("eval-q-2026-09-27"), "{err}");
        let mut copy = first.clone();
        copy.id = "notes-q".to_string();
        copy.tags = vec!["notes".to_string()];
        copy.text = a.replace("=== MACHINE SECTION", "machine section").replace("=== TLDR AND REMARKS", "tldr and remarks");
        let err = declare(&mut store, "s", "l", "a", &copy).unwrap_err().to_string();
        assert!(err.contains("near-duplicate"), "an ordinary report copying one is still refused: {err}");
    }

    /// R15.5 at the store: the stored text of a filed report is the machine
    /// section, a blank line, the agent header, a newline, the writer's own
    /// text - and a text that carries either header is refused at the filing.
    #[test]
    fn a_filed_report_is_stored_below_the_machine_section_and_a_typed_header_is_refused() {
        let mut store = EventStore::in_memory().unwrap();
        let start = record_start(&mut store, "s", "h");
        for forged in [
            "=== MACHINE SECTION: derived from the log, seq 1 to 2; it cannot be edited ===\nall settled",
            "Summary.\n=== TLDR AND REMARKS: written by the evaluating session; they settle nothing ===\nmore",
            "a line naming === MACHINE SECTION in passing",
        ] {
            let mut report = eval_report("eval-h-1", "h");
            report.text = forged.to_string();
            let err = file_in(&mut store, &report, &start).unwrap_err().to_string();
            assert!(err.contains("carries a section header only THOR writes"), "{forged}: {err}");
            assert!(err.contains("send only your own TLDR and remarks"), "{err}");
        }
        assert!(show(&store, "eval-h-1").is_err(), "nothing stored");
        let mut report = eval_report("eval-h-1", "h");
        report.text = "TLDR: evaluation of h. Nothing to add.".to_string();
        let event = file_in(&mut store, &report, &start).unwrap();
        let stored = show(&store, "eval-h-1").unwrap().text;
        let filed_seq = event.seq - 1;
        assert_eq!(
            stored,
            format!(
                "{}\n\n{}\n{}",
                render_machine_section(&store, &start, filed_seq),
                gate::AGENT_SECTION_BEGIN,
                report.text
            ),
            "machine section + blank line + agent header + newline + the agent's text"
        );
        assert_eq!(gate::machine_section_range(&stored), Some((start.frozen.started_seq, filed_seq)));
        assert_eq!(gate::agent_section(&stored), report.text);
    }



    /// R-b's unit: settling a member of the frozen batch never pulls the next
    /// owed rule in - the filing is held to the frozen ids only, while the
    /// live batch (doctor's view) moves on.
    #[test]
    fn frozen_batch_never_grows_when_a_member_is_settled() {
        let mut store = EventStore::in_memory().unwrap();
        let ids: Vec<String> = (1..=10).map(|n| format!("fb-heavy-{n:02}")).collect();
        for id in &ids {
            legacy_thinly_answered(&mut store, id, Some("fb"));
            serve_n(&mut store, id, "hook", 1);
        }
        let start = record_start(&mut store, "fb-session", "fb");
        assert_eq!(start.frozen.batch(), &ids[..8]);
        assert_eq!(start.frozen.batch_later, 2);

        // Settled by the evaluating session's own attested call (a write
        // to a frozen rule from anywhere else would void the evaluation).
        let settled = settle_by_naming_the_trigger(&mut store, &ids[0]);
        attest(&mut store, "fb-session", &ids[0], settled.seq);
        assert!(!settlement(&store, &ids[0], None).is_owed());

        let owed: Vec<String> = evaluation_frozen_batch_owed(&store, &start).into_iter().map(|(id, _)| id).collect();
        assert_eq!(owed, ids[1..8].to_vec(), "seven left, and nobody new");
        let live: Vec<String> = evaluation_teeth_batch(&store, Some("fb")).owed_now.into_iter().map(|(id, _)| id).collect();
        assert!(live.contains(&ids[8]), "the live batch pulls the ninth in: {live:?}");

        let err = file_in(&mut store, &eval_report("eval-fb-1", "fb"), &start).unwrap_err().to_string();
        assert!(err.contains("'fb-heavy-02'") && err.contains("'fb-heavy-08'"), "{err}");
        assert!(!err.contains("fb-heavy-09") && !err.contains("fb-heavy-10") && !err.contains("'fb-heavy-01'"), "{err}");
    }

    /// Since step 8 a reason tag settles NOTHING, before or after a check was
    /// tried (K14): the trial decides whether a rule can have a tooth. The
    /// same rule, put on the note track by a trial record, settles.
    #[test]
    fn a_no_literal_reason_settles_nothing_and_a_note_verdict_settles() {
        let mut store = EventStore::in_memory().unwrap();
        file_now(&mut store, &eval_report("eval-t-old", "t")).unwrap();
        let mut rule = heavy_command_rule("never-tried", "t");
        rule.tags = vec![format!("{NO_CATCH_ON_COMMAND_REASON_PREFIX}the same text is in 'fixture-command-never-tried --help' which is legitimate")];
        let body = serde_json::to_string(&rule).unwrap();
        store.append_event("legacy", "never-tried", "migration", EventKind::FactCreated, "never-tried", None, &body).unwrap();
        serve_n(&mut store, "never-tried", "hook", 1);

        let err = file_now(&mut store, &eval_report("eval-t-new", "t")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("'never-tried'"), "a reason is no answer: {}", refusal.problem);

        // A check tried once and given up again: the reason still settles
        // nothing (the old "after a real try" arm is gone).
        try_a_check_then_give_it_up(&mut store, &rule);
        let err = file_now(&mut store, &eval_report("eval-t-new", "t")).unwrap_err();
        assert!(err.to_string().contains("'never-tried'"), "{err}");

        settle_by_naming_the_trigger(&mut store, "never-tried");
        assert_eq!(current_verdict(&store, "never-tried"), CurrentVerdict::Note);
        file_now(&mut store, &eval_report("eval-t-new", "t")).expect("a note verdict settles the rule");
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
        file_now(&mut store, &eval_report("eval-g-old", "g")).unwrap();
        legacy_thinly_answered(&mut store, "global-heavy", None);
        serve_n(&mut store, "global-heavy", "hook", 1);

        let err = file_now(&mut store, &eval_report("eval-g-new", "g")).unwrap_err();
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
        file_now(&mut store, &eval_report("eval-r-old", "r")).unwrap();
        legacy_thinly_answered(&mut store, "boxed-heavy", Some("r"));
        let existing = show(&store, "boxed-heavy").unwrap();
        let mut labelled = existing.clone();
        labelled.tags.push(TEETH_INFORMING_TAG.to_string());
        legacy_revise(&mut store, &labelled);
        serve_n(&mut store, "boxed-heavy", "hook", 1);

        let err = file_now(&mut store, &eval_report("eval-r-new", "r")).unwrap_err();
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

    /// Settle a heavy rule the one way a checkless rule settles since step 8
    /// of the two tracks: THOR's trial put it on the note track - a revise
    /// carrying a Note record, the check cleared and the label `teeth:note`
    /// (a reason tag settles nothing any more).
    fn settle_by_naming_the_trigger(store: &mut EventStore, id: &str) -> Event {
        let existing = show(store, id).unwrap();
        let mut noted = existing.clone();
        noted.check = None;
        noted.tags = vec![TEETH_NOTE_TAG.to_string()];
        let record = TrialRecord::seal(trial_fields(&noted, RecordedVerdict::Note));
        let baseline = gate::ClearedFields { check: true, ..Default::default() }.baseline(&existing);
        revise_tried_scoped(store, "s", "l", "a", &baseline, &noted, &existing, Some("fixture: the trial found no tooth"), None, Some(&record), None, None)
            .unwrap()
    }

    fn filing_refusal(store: &mut EventStore, report: &str, project: &str) -> Option<String> {
        match file_now(store, &eval_report(report, project)) {
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
        file_now(&mut store, &eval_report("eval-w-0", "w")).unwrap();
        let ids = unsettled_fired_rules(&mut store, "w", 12);

        let (batch, rest) = ids.split_at(EVALUATION_TEETH_BATCH.min(ids.len()));
        let problem = filing_refusal(&mut store, "eval-w-1", "w").expect("the batch is owed");
        for id in batch {
            assert!(problem.contains(&format!("'{id}'")), "the batch must name {id}: {problem}");
        }
        for id in rest {
            assert!(!problem.contains(&format!("'{id}'")), "{id} is beyond the batch: {problem}");
        }
        assert!(problem.contains("4 more rule(s)"), "the rest must be counted, not hidden: {problem}");

        for id in batch {
            settle_by_naming_the_trigger(&mut store, id);
        }
        // UPDATED FOR CHANGE 3 (2026-09-27): a settled rule no longer takes a
        // place, so the 4 left behind - only 4 owed candidates now, well
        // inside the cap of 8 - move into `owed_now` for THIS SAME filing the
        // moment the first 8 clear, rather than waiting for a later report's
        // own cutoff. Settling only the named 8 therefore no longer lets this
        // report through; every one of the 12 still has to be settled first.
        let problem = filing_refusal(&mut store, "eval-w-1", "w").expect("the 4 left behind are still owed");
        for id in rest {
            assert!(problem.contains(&format!("'{id}'")), "the 4 left behind must now be named too: {problem}");
        }
        for id in rest {
            settle_by_naming_the_trigger(&mut store, id);
        }
        assert_eq!(filing_refusal(&mut store, "eval-w-1", "w"), None, "a fully settled pool must let the report through");
        let owed: Vec<String> = evaluation_teeth_batch(&store, Some("w")).owed_now.into_iter().map(|(id, _)| id).collect();
        assert!(owed.is_empty(), "nothing has fired since the new report: {owed:?}");
    }

    /// UPDATED FOR CHANGE 3 (2026-09-27): settling one rule of the batch now
    /// DOES pull the next one in, on purpose - see `teeth_batch_since`'s own
    /// doc comment, "A SETTLED RULE NEVER TAKES A PLACE" / "CONSEQUENCE,
    /// NAMED SO NOBODY IS SURPRISED BY IT". Of 10 fired rules, settling the
    /// first leaves 9 owed; the cap of 8 now applies to THOSE 9 alone, so the
    /// 8th-oldest of them (`ids[8]`, previously left for a later report)
    /// moves into `owed_now` for this same filing, and only the 9th
    /// (`ids[9]`) still waits.
    #[test]
    fn settling_a_batch_member_pulls_the_next_owed_rule_in() {
        let mut store = EventStore::in_memory().unwrap();
        file_now(&mut store, &eval_report("eval-s-0", "s")).unwrap();
        let ids = unsettled_fired_rules(&mut store, "s", 10);
        settle_by_naming_the_trigger(&mut store, &ids[0]);

        let batch = evaluation_teeth_batch(&store, Some("s"));
        assert_eq!(batch.settled, vec![ids[0].clone()]);
        let owed: Vec<&str> = batch.owed_now.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(owed, ids[1..=EVALUATION_TEETH_BATCH].iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(batch.owed_later.len(), 1);
        assert_eq!(batch.owed_later[0].0, ids[9]);
    }

    /// UPDATED FOR CHANGE 3 (2026-09-27): a report can only file once EVERY
    /// candidate it fired is settled - a settled one no longer wastes a
    /// place that would otherwise defer an owed one to `owed_later` (see
    /// `teeth_batch_since`'s own doc comment), so `owed_now` always surfaces
    /// up to `EVALUATION_TEETH_BATCH` of whatever is CURRENTLY owed. Settling
    /// only the first 8 of 11 therefore no longer lets `eval-r1` through -
    /// all 11 have to be settled first. What still holds, and is what this
    /// test now proves: once that report is filed, a FRESH batch of rules
    /// that fire afterwards is still asked oldest-write-first, with a rule
    /// written even later waiting behind them.
    #[test]
    fn the_next_filing_asks_first_for_what_the_last_one_left() {
        let mut store = EventStore::in_memory().unwrap();
        file_now(&mut store, &eval_report("eval-r0", "rot")).unwrap();
        let ids = unsettled_fired_rules(&mut store, "rot", 11);
        for id in &ids {
            settle_by_naming_the_trigger(&mut store, id);
        }
        file_now(&mut store, &eval_report("eval-r1", "rot")).unwrap();

        // A fresh batch, written and fired only after `eval-r1` - oldest
        // first, and a rule written later waits behind the older ones.
        let fresh_ids: Vec<String> = (0..3).map(|i| format!("v-{i:02}")).collect();
        for id in &fresh_ids {
            legacy_thinly_answered(&mut store, id, Some("rot"));
        }
        legacy_thinly_answered(&mut store, "z-newer", Some("rot"));
        for id in fresh_ids.iter().map(String::as_str).chain(["z-newer"]) {
            serve_n(&mut store, id, "hook", 1);
        }
        let batch = evaluation_teeth_batch(&store, Some("rot"));
        let owed: Vec<&str> = batch.owed_now.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(owed, vec!["v-00", "v-01", "v-02", "z-newer"], "oldest write first, the newer one waits behind");
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
            settle_by_naming_the_trigger(&mut store, &settled.id);
        }
        file_now(&mut store, &eval_report("eval-k-0", "k")).unwrap();
        let ids = unsettled_fired_rules(&mut store, "k", 3);
        for i in 0..EVALUATION_TEETH_BATCH {
            serve_n(&mut store, &format!("old-{i}"), "hook", 3);
        }
        let batch = evaluation_teeth_batch(&store, Some("k"));
        let owed: Vec<String> = batch.owed_now.into_iter().map(|(id, _)| id).collect();
        assert_eq!(owed, ids);
        assert!(batch.settled.is_empty(), "{:?}", batch.settled);
    }

    /// Since step 8 a reason that names the rule's own command settles
    /// NOTHING, before or after a check was tried (K14) - THOR's trial
    /// decides. The same rule on the note track settles.
    #[test]
    fn a_reason_that_names_the_trigger_no_longer_settles_and_a_note_does() {
        let mut store = EventStore::in_memory().unwrap();
        file_now(&mut store, &eval_report("eval-n-old", "n")).unwrap();
        let mut heavy = heavy_command_rule("named-heavy", "n");
        heavy.tags = vec![format!(
            "{}an authorised fixture-command-named-heavy run is typed exactly like a wrong one",
            NO_CATCH_ON_COMMAND_REASON_PREFIX
        )];
        declare(&mut store, "s", "l", "a", &heavy).unwrap();
        serve_n(&mut store, "named-heavy", "hook", 1);
        assert!(file_now(&mut store, &eval_report("eval-n-new", "n")).is_err(), "a reason is no answer");
        try_a_check_then_give_it_up(&mut store, &heavy);
        assert!(
            file_now(&mut store, &eval_report("eval-n-new", "n")).is_err(),
            "not after a try either"
        );
        settle_by_naming_the_trigger(&mut store, "named-heavy");
        file_now(&mut store, &eval_report("eval-n-new", "n")).expect("a note verdict settles it");
    }
    /// Step 9, the one reader: no label, reason or open point settles a rule
    /// any more - not `teeth:proven-guard`, not `teeth:wired-not-proven`, not
    /// an old engine's `teeth:informing`/`not-tried`/`refuses-legitimate-use`
    /// /`anchor-collapsed`, not a no-literal reason, not an answer-guard tag,
    /// and not an open point naming it. Only a current verdict does.
    #[test]
    fn settlement_ignores_tags_and_reasons() {
        let mut store = EventStore::in_memory().unwrap();
        file_now(&mut store, &eval_report("eval-w9-old", "w9")).unwrap();
        let labels = [
            TEETH_PROVEN_TAG.to_string(),
            TEETH_WIRED_TAG.to_string(),
            TEETH_INFORMING_TAG.to_string(),
            TEETH_NOT_TRIED_TAG.to_string(),
            TEETH_REFUSES_LEGITIMATE_TAG.to_string(),
            TEETH_ANCHOR_COLLAPSED_TAG.to_string(),
            format!("{NO_LITERAL_REASON_PREFIX}an authorised run of this command reads exactly like a wrong one"),
            format!("{ANSWER_GUARD_TAG_PREFIX}some-rulebook-entry"),
        ];
        let mut ids = Vec::new();
        for (i, label) in labels.iter().enumerate() {
            let mut rule = heavy_command_rule(&format!("labelled-{i}"), "w9");
            rule.text = format!("fixture heavy rule labelled-{i}: never with --force");
            rule.tags = vec![label.clone()];
            rule.check = Some(Check::Forbidden { literals: vec!["--force".to_string()] });
            legacy(&mut store, &rule);
            serve_n(&mut store, &rule.id, "hook", 1);
            assert!(settlement(&store, &rule.id, None).is_owed(), "{label} settles nothing: {:?}", settlement(&store, &rule.id, None));
            ids.push(rule.id);
        }
        let mut point = open_point("open-labelled", "w9");
        point.text = format!("{} still has to refuse a real --force push", ids.join(", "));
        declare(&mut store, "s", "l", "a", &point).unwrap();
        let err = file_now(&mut store, &eval_report("eval-w9-new", "w9")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        for id in &ids {
            assert!(refusal.problem.contains(&format!("'{id}'")), "an open point carries nothing: {}", refusal.problem);
        }

        // Green: the same rules with a verdict of THOR's trial settle.
        for id in &ids {
            let rule = show(&store, id).unwrap();
            let record = sealed(&rule, RecordedVerdict::RuleWired);
            revise_tried_scoped(&mut store, "s", "l", "a", &rule, &rule, &rule, None, None, Some(&record), None, None).unwrap();
            assert_eq!(settlement(&store, id, None), Settlement::Settled(Settled::Rule { proven: false }));
        }
        file_now(&mut store, &eval_report("eval-w9-new", "w9")).expect("verdicts settle them");
    }

    /// A verdict counts only while the rule is the one tried: a rewrite of
    /// its text makes it owed again, with the reason named.
    #[test]
    fn settlement_resets_when_the_rule_is_rewritten() {
        let mut store = EventStore::in_memory().unwrap();
        let rule = force_push_rule("rewritten");
        declare_tried(&mut store, &rule, &sealed(&rule, RecordedVerdict::RuleWired));
        assert_eq!(settlement(&store, "rewritten", None), Settlement::Settled(Settled::Rule { proven: false }));
        let mut changed = rule.clone();
        changed.text = "Never run `git push --force`, not even on a branch of your own.".to_string();
        legacy_revise(&mut store, &changed);
        assert_eq!(
            settlement(&store, "rewritten", None),
            Settlement::Owed { why: "its text changed after its last trial".to_string() }
        );
        // Back to the text it was tried with: the same record counts again.
        legacy_revise(&mut store, &rule);
        assert_eq!(settlement(&store, "rewritten", None), Settlement::Settled(Settled::Rule { proven: false }));
    }

    /// D8: lowering a heavy rule's severity (a raw fixture event, the shape
    /// any bin can write) never takes it out of rot - the severity is part of
    /// the fingerprint, so a settled rule is owed again, and an owed one
    /// stays owed and stays in the filing's batch, light or not.
    #[test]
    fn lowering_severity_never_takes_a_rule_out_of_rot() {
        let mut store = EventStore::in_memory().unwrap();
        file_now(&mut store, &eval_report("eval-ls-old", "ls")).unwrap();
        let mut noted = heavy_command_rule("noted-heavy", "ls");
        noted.tags = vec![];
        declare_noted(&mut store, &noted);
        assert!(!settlement(&store, "noted-heavy", None).is_owed());
        let mut lowered = noted.clone();
        lowered.severity = None;
        legacy_revise(&mut store, &lowered);
        assert_eq!(
            settlement(&store, "noted-heavy", None),
            Settlement::Owed { why: "its severity changed after its last trial".to_string() }
        );

        legacy_thinly_answered(&mut store, "owed-heavy", Some("ls"));
        let mut light = show(&store, "owed-heavy").unwrap();
        light.severity = None;
        legacy_revise(&mut store, &light);
        assert!(settlement(&store, "owed-heavy", None).is_owed());
        let owed: Vec<String> = evaluation_teeth_batch(&store, Some("ls")).owed_now.into_iter().map(|(id, _)| id).collect();
        assert!(owed.contains(&"owed-heavy".to_string()) && owed.contains(&"noted-heavy".to_string()), "{owed:?}");
    }

    /// F9: a file check of a project with no usable registered working copy
    /// is settled while that blocker holds - it takes no batch place, since
    /// nothing the agent runs settles it - and owed again the moment the
    /// project is registered. A contains literal missing from the registered
    /// working tree while the default branch still has it blocks the same
    /// way, until the tree has it again. A global rule's file check is
    /// blocked too, by THOR's blocker for rules with no project (step 20).
    #[test]
    fn a_blocked_rule_is_settled_only_while_its_blocker_holds() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_memory().unwrap();
        file_now(&mut store, &eval_report("eval-gamma-old", "gamma")).unwrap();
        let mut rule = heavy_command_rule("gamma-keeps", "gamma");
        rule.text = "NOTES.md must keep the line `keep me`.".to_string();
        rule.tags = vec![];
        rule.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "NOTES.md".to_string() }];
        rule.check = Some(Check::Contains { path: "NOTES.md".to_string(), literal: "keep me".to_string() });
        legacy(&mut store, &rule);
        serve_n(&mut store, "gamma-keeps", "hook", 1);
        assert_eq!(
            settlement(&store, "gamma-keeps", None),
            Settlement::Settled(Settled::Blocked(Blocker::NoWorkingCopy { project: "gamma".to_string() }))
        );
        file_now(&mut store, &eval_report("eval-gamma-1", "gamma")).expect("a blocked rule holds no filing");

        // Registered: a real checkout named gamma, NOTES.md committed on main.
        let checkout = dir.path().join("gamma");
        std::fs::create_dir_all(&checkout).unwrap();
        let hooks = dir.path().join("no-hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(&checkout).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
        git(&["config", "user.email", "fixture@example.invalid"]);
        git(&["config", "user.name", "fixture"]);
        std::fs::write(checkout.join("NOTES.md"), "keep me\nplain line\n").unwrap();
        git(&["add", "NOTES.md"]);
        git(&["commit", "-q", "-m", "notes"]);
        let record = project_record("gamma");
        let body = body_with_root(&canonical_body(&record).unwrap(), checkout.to_str().unwrap(), "2026-09-29T00:00:00Z").unwrap();
        store.append_event("install", "install", INSTALLER_ACTOR, EventKind::FactCreated, &record.id, None, &body).unwrap();
        assert!(settlement(&store, "gamma-keeps", None).is_owed(), "registered: owed again");
        let err = file_now(&mut store, &eval_report("eval-gamma-2", "gamma")).unwrap_err();
        assert!(err.to_string().contains("'gamma-keeps'"), "{err}");

        // Mid-change in the owner's checkout: the default branch still has it.
        std::fs::write(checkout.join("NOTES.md"), "plain line\n").unwrap();
        assert_eq!(
            settlement(&store, "gamma-keeps", None),
            Settlement::Settled(Settled::Blocked(Blocker::WorkingTreeDiffers { path: "NOTES.md".to_string() }))
        );
        std::fs::write(checkout.join("NOTES.md"), "keep me\nplain line\n").unwrap();
        assert!(settlement(&store, "gamma-keeps", None).is_owed(), "the tree has it again: owed");

        // A global rule's file check (step 20, K10): blocked by THOR's own
        // blocker for rules with no project - never the agent's, and it
        // holds no filing.
        let mut global = rule.clone();
        global.id = "global-keeps".to_string();
        global.project = None;
        legacy(&mut store, &global);
        assert_eq!(settlement(&store, "global-keeps", None), Settlement::Settled(Settled::Blocked(Blocker::global_rules())));
    }

    /// Retracted, or taken off the rule track: settled; a rule with a note
    /// verdict by dispute or with a pending check: settled as such.
    #[test]
    fn a_retracted_or_archived_rule_and_every_note_shape_are_settled() {
        let mut store = EventStore::in_memory().unwrap();
        legacy_thinly_answered(&mut store, "to-retract", Some("rt"));
        retract(&mut store, "s", "l", "a", "to-retract", "fixture-command-to-retract no longer exists", None).unwrap();
        assert_eq!(settlement(&store, "to-retract", None), Settlement::Settled(Settled::Retracted));

        let mut report = heavy_command_rule("now-a-report", "rt");
        report.kind = Kind::Report;
        report.bindings = vec![];
        report.severity = None;
        report.tags = vec![];
        legacy(&mut store, &report);
        assert_eq!(settlement(&store, "now-a-report", None), Settlement::Settled(Settled::OffTheRuleTrack));

        let mut disputed = heavy_command_rule("disputed", "rt");
        disputed.tags = vec![];
        let mut fields = trial_fields(&disputed, RecordedVerdict::Note);
        fields.disputes.legitimate_calls = vec!["fixture-command-disputed --dry-run".to_string()];
        fields.pending = Some(PendingCheck {
            check: Check::Contains { path: "NOTES.md".to_string(), literal: "x".to_string() },
            why: PendingWhy::NoWorkingCopy,
        });
        declare_tried(&mut store, &disputed, &TrialRecord::seal(fields));
        assert_eq!(
            settlement(&store, "disputed", None),
            Settlement::Settled(Settled::Note { pending: Some(PendingWhy::NoWorkingCopy), by_dispute: true })
        );
    }

    /// UPDATED FOR CHANGE 3 (2026-09-27), REVERSED ON PURPOSE - see
    /// `teeth_batch_since`'s own doc comment, "THE BATCH MUST ALSO REACH A
    /// RULE THAT NEVER FIRES". This used to prove the opposite: an unfired
    /// heavy rule could never hold up filing at all, which was measured to
    /// leave about 126 of roughly 260 heavy rules permanently unreachable by
    /// any evaluation. It now DOES hold up filing, exactly like a rule that
    /// fired and is owed - it simply fills a place from the second pool.
    #[test]
    fn a_heavy_rule_that_never_fired_since_the_previous_report_now_fills_a_place() {
        let mut store = EventStore::in_memory().unwrap();
        file_now(&mut store, &eval_report("eval-t-old", "t")).unwrap();
        // Declared after the previous report and never served at all.
        declare(&mut store, "s", "l", "a", &heavy_command_rule("never-fired", "t")).unwrap();

        let err = file_now(&mut store, &eval_report("eval-t-new", "t")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("never-fired"), "{}", refusal.problem);
        assert!(refusal.problem.contains("did not fire since the previous report"), "{}", refusal.problem);

        settle_by_naming_the_trigger(&mut store, "never-fired");
        file_now(&mut store, &eval_report("eval-t-newer", "t")).expect("settled, so it fills its place cleanly");
    }

    /// Step 14: an open point an agent filed (a legacy point) holds no
    /// filing - not when it is older than the previous report, not when it
    /// was never closed or carried.
    #[test]
    fn a_legacy_open_point_holds_no_filing() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "s", "l", "a", &open_point("open-u-1", "u")).unwrap();
        file_now(&mut store, &eval_report("eval-u-1", "u")).unwrap();
        file_now(&mut store, &eval_report("eval-u-2", "u")).expect("a legacy open point holds no filing");
        let points = open_points(&store, Some("u"));
        assert_eq!(points.len(), 1);
        assert!(!points[0].machine, "filed by an agent: legacy");

        retract(&mut store, "s", "l", "a", "open-u-1", "resolved during the next evaluation", None).expect("a legacy point is still closed by a retraction");
        assert!(open_points(&store, Some("u")).is_empty());
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
        assert!(points.iter().all(|p| p.carried == 0 && !p.machine));

        // THOR's own point, written later, still leads the list.
        let blocker = Blocker::NoWorkingCopy { project: "x".to_string() };
        let actor = SystemActor::new(EVAL_MACHINE_ACTOR);
        write_machine_point(&mut store, actor, "sess", "x", &blocker, "fixture machine point text").unwrap().expect("written");
        let points = open_points(&store, Some("x"));
        assert_eq!(
            points.iter().map(|p| (p.id.as_str(), p.machine)).collect::<Vec<_>>(),
            vec![("open-no-working-copy-x", true), ("open-x-1", false), ("open-x-2", false)]
        );
        assert_eq!((points[0].subject.as_deref(), points[0].blocker.as_deref()), (Some("x"), Some("no-working-copy")));
    }

    /// THOR's own open point: written once, left alone when nothing changed,
    /// revised when its text did, closed with a derived reason, and re-opened
    /// on the same id when the blocker is back - by `eval-machine` only.
    #[test]
    fn a_machine_point_is_written_closed_and_reopened_by_eval_machine_only() {
        let mut store = EventStore::in_memory().unwrap();
        let blocker = Blocker::WorkingTreeDiffers { path: "docs/NOTES.md".to_string() };
        let machine = || SystemActor::new(EVAL_MACHINE_ACTOR);
        let id = machine_point_id("Acme Demo", &blocker);
        assert_eq!(id, "open-working-tree-differs-acme-demo-docs-notes-md");

        let created = write_machine_point(&mut store, machine(), "sess", "Acme Demo", &blocker, "first text").unwrap().expect("created");
        assert_eq!((created.kind, created.actor.as_str(), created.session_id.as_str()), (EventKind::FactCreated, EVAL_MACHINE_ACTOR, "sess"));
        let item = show(&store, &id).unwrap();
        assert_eq!(item.tags, vec!["open-point".to_string(), "about:docs/NOTES.md".to_string(), "blocker:working-tree-differs".to_string()]);
        assert!(is_machine_point(&store, &id));
        assert!(write_machine_point(&mut store, machine(), "sess", "Acme Demo", &blocker, "first text").unwrap().is_none(), "same text: nothing written");
        let revised = write_machine_point(&mut store, machine(), "sess", "Acme Demo", &blocker, "second text").unwrap().expect("revised");
        assert_eq!(revised.kind, EventKind::FactRevised);

        // Nobody else writes on it: not a retract, not a revise, not a create.
        let err = retract(&mut store, "s", "l", "mcp", &id, "closing it by hand", None).unwrap_err().to_string();
        assert!(err.contains("this open point is THOR's own") && err.contains("closes itself when its subject settles"), "{err}");
        let err = declare(&mut store, "s", "l", "mcp", &open_point("open-no-working-copy-acme", "acme")).unwrap_err().to_string();
        assert!(err.contains("THOR's own"), "{err}");
        assert!(write_machine_point(&mut store, SystemActor::new("retest"), "sess", "Acme Demo", &blocker, "x").is_err());

        let closed = retract_machine_point(&mut store, machine(), "sess", &id, "docs/NOTES.md holds the literal again").unwrap();
        assert_eq!(closed.map(|e| e.kind), Some(EventKind::FactRetracted));
        assert!(!is_machine_point(&store, &id) && open_points(&store, Some("Acme Demo")).is_empty());
        assert!(retract_machine_point(&mut store, machine(), "sess", &id, "again").unwrap().is_none(), "not live: nothing written");

        let reopened = write_machine_point(&mut store, machine(), "sess", "Acme Demo", &blocker, "third text").unwrap().expect("re-opened");
        assert_eq!(reopened.kind, EventKind::FactRevised);
        assert_eq!(show(&store, &id).unwrap().text, "third text");
        assert!(is_machine_point(&store, &id));
        let listed = open_points(&store, Some("Acme Demo"));
        assert_eq!(listed.iter().map(|p| (p.id.as_str(), p.machine)).collect::<Vec<_>>(), vec![(id.as_str(), true)], "live again, on the same id");
        assert_eq!(
            store.get_events_by_entity(&id).unwrap().iter().map(|e| e.kind).collect::<Vec<_>>(),
            vec![EventKind::FactCreated, EventKind::FactRevised, EventKind::FactRetracted, EventKind::FactRevised],
            "its history shows every time the blocker held and lifted"
        );
    }

    /// K10, step 14: a rule is held only by a blocker THOR itself found. An
    /// open point an agent filed about it settles nothing; neither does a
    /// Report that merely looks like THOR's point. And for one evaluation the
    /// blocker counts only when its machine pass found it AT THE START.
    #[test]
    fn only_a_machine_blocker_settles_a_rule() {
        let mut store = EventStore::in_memory().unwrap();
        // A rule with no verdict, and an agent-filed open point naming it.
        legacy_thinly_answered(&mut store, "mb-rule", Some("mb"));
        let mut point = open_point("open-mb-rule", "mb");
        point.text = "mb-rule still needs a real call, later.".to_string();
        declare(&mut store, "s", "l", "a", &point).unwrap();
        assert!(settlement(&store, "mb-rule", None).is_owed(), "an agent-filed open point settles nothing");

        // The same, shaped like THOR's own point and written raw by another
        // actor (what an older binary could have left): not a machine point.
        let blocker = Blocker::NoWorkingCopy { project: "mb".to_string() };
        let forged = Item { id: machine_point_id("mb", &blocker), tags: machine_point_tags(&blocker), ..point.clone() };
        let body = serde_json::to_string(&forged).unwrap();
        store.append_event("s", "l", "a", EventKind::FactCreated, &forged.id, None, &body).unwrap();
        assert!(!is_machine_point(&store, &forged.id));
        assert!(open_points(&store, Some("mb")).iter().all(|p| !p.machine));
        assert!(settlement(&store, "mb-rule", None).is_owed());

        // A file check in a project with no registered working copy: held by
        // the machine's own blocker, read from the registration.
        let mut rule = heavy_command_rule("mb-file", "mb");
        rule.text = "NOTES.md must keep the line `keep me`.".to_string();
        rule.tags = vec![];
        rule.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "NOTES.md".to_string() }];
        rule.check = Some(Check::Contains { path: "NOTES.md".to_string(), literal: "keep me".to_string() });
        legacy(&mut store, &rule);
        assert_eq!(settlement(&store, "mb-file", None), Settlement::Settled(Settled::Blocked(blocker.clone())));

        // For one evaluation: blocked only when its pass found it blocked.
        let first = record_start_only(&mut store, "mb-session", "mb");
        let unseen = decide_start(&mut store, &first, vec![], 0, vec![], vec![]);
        assert!(settlement(&store, "mb-file", Some(&unseen)).is_owed(), "the pass never found it blocked");
        let second = record_start_only(&mut store, "mb-session-2", "mb");
        let items = vec![machine_item("mb-file", MachineOutcome::Blocked, "blocked: 'mb' has no registered working copy", Some(&blocker))];
        let seen = decide_start(&mut store, &second, vec![], 0, vec![], items);
        assert_eq!(settlement(&store, "mb-file", Some(&seen)), Settlement::Settled(Settled::Blocked(blocker)));
        file_in(&mut store, &eval_report("eval-mb-1", "mb"), &seen).expect("a rule held by the machine's blocker holds no filing");
    }

    /// A blocker that held at one evaluation's start and no longer holds:
    /// the rule is the next start's to decide - in that start's batch when it
    /// needs the agent - and it holds that filing until it has a verdict.
    #[test]
    fn a_blocker_that_stopped_holding_puts_the_rule_in_the_next_batch() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_memory().unwrap();
        let mut rule = heavy_command_rule("bs-keeps", "gamma");
        rule.text = "NOTES.md must keep its first line.".to_string();
        rule.tags = vec![];
        rule.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "NOTES.md".to_string() }];
        rule.check = Some(Check::Contains { path: "NOTES.md".to_string(), literal: "keep me".to_string() });
        legacy(&mut store, &rule);
        let blocker = Blocker::NoWorkingCopy { project: "gamma".to_string() };

        let first = record_start_only(&mut store, "bs-session-1", "gamma");
        let items = vec![machine_item("bs-keeps", MachineOutcome::Blocked, "blocked: no registered working copy", Some(&blocker))];
        let first = decide_start(&mut store, &first, vec![], 0, vec![], items);
        assert_eq!(settlement(&store, "bs-keeps", Some(&first)), Settlement::Settled(Settled::Blocked(blocker.clone())));
        file_in(&mut store, &eval_report("eval-gamma-1", "gamma"), &first).expect("blocked at the start: not this filing's");

        // The project gets registered: the blocker no longer holds.
        let checkout = dir.path().join("gamma");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        std::fs::write(checkout.join("NOTES.md"), "keep me\nplain line\n").unwrap();
        let record = project_record("gamma");
        let body = body_with_root(&canonical_body(&record).unwrap(), checkout.to_str().unwrap(), "2026-09-29T00:00:00Z").unwrap();
        store.append_event("install", "install", INSTALLER_ACTOR, EventKind::FactCreated, &record.id, None, &body).unwrap();
        assert!(settlement(&store, "bs-keeps", None).is_owed(), "registered: owed again, read now");
        assert_eq!(
            settlement(&store, "bs-keeps", Some(&first)),
            Settlement::Settled(Settled::Blocked(blocker)),
            "the first evaluation is judged by what held at ITS start"
        );

        // The next start tries it for real; here its text does not name the
        // literal, so it needs the agent: it is on that start's A4 list.
        let second = record_start_only(&mut store, "bs-session-2", "gamma");
        let items = vec![machine_item("bs-keeps", MachineOutcome::A4Only, "A4: 'keep me' not named in the text", None)];
        let second = decide_start(&mut store, &second, vec![], 0, vec!["bs-keeps".to_string()], items);
        assert_eq!(settlement(&store, "bs-keeps", Some(&second)), Settlement::Owed { why: "A4: 'keep me' not named in the text".to_string() });
        let err = file_in(&mut store, &eval_report("eval-gamma-2", "gamma"), &second).unwrap_err().to_string();
        assert!(err.contains("'bs-keeps'") && err.contains("A4: 'keep me' not named in the text"), "{err}");

        // A verdict written after that start - by the evaluating session's
        // own attested call - settles it.
        let settled = settle_by_naming_the_trigger(&mut store, "bs-keeps");
        attest(&mut store, "bs-session-2", "bs-keeps", settled.seq);
        assert!(!settlement(&store, "bs-keeps", Some(&second)).is_owed());
        file_in(&mut store, &eval_report("eval-gamma-2", "gamma"), &second).expect("decided since the start");
    }

    /// F10: while THOR's machine pass has not finished - the start record
    /// holds no batch - nothing can be filed, and the refusal says to end the
    /// turn once.
    #[test]
    fn filing_is_refused_while_the_start_record_holds_no_batch() {
        let mut store = EventStore::in_memory().unwrap();
        let pending = record_start_only(&mut store, "sd-session", "sd");
        assert!(!pending.frozen.decided());
        let err = file_in(&mut store, &eval_report("eval-sd-1", "sd"), &pending).unwrap_err().to_string();
        assert!(err.contains(STILL_DECIDING), "{err}");
        assert!(show(&store, "eval-sd-1").is_err(), "nothing stored");

        let decided = decide_start(&mut store, &pending, vec![], 0, vec![], vec![]);
        assert_eq!(open_evaluation_start(&store, "sd-session", "sd"), Some(decided.clone()), "read back with its batch");
        file_in(&mut store, &eval_report("eval-sd-1", "sd"), &decided).expect("the batch is written and empty: it files");
        let actor = SystemActor::new(EVAL_MACHINE_ACTOR);
        assert!(
            record_evaluation_batch(&mut store, actor, &decided, "again", vec![], 0, vec![], MachinePass::default()).is_err(),
            "an evaluation's lists are written once"
        );
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
        file_now(&mut store, &eval_report("eval-elsewhere", "other")).unwrap();

        let filed = show(&store, "eval-elsewhere").unwrap();
        let mut moved = filed.clone();
        moved.project = Some("dd".to_string());
        // Through a door with no filing context: refused outright (step 15).
        let err = revise(&mut store, "s", "l", "a", &filed, &moved).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("not coming through a THOR tool call"), "{}", refusal.problem);

        // With the context of an evaluation of 'dd': held to that project's
        // own lists, which hold the owed rule.
        let start = record_start(&mut store, "s", "dd");
        let ctx = FilingContext::new(INFLIGHT_SEEN, start).unwrap();
        let err = revise_because_scoped(&mut store, "s", "l", "a", &filed, &moved, &filed, None, Some(&ctx)).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("no current verdict from THOR's trial"), "the assistant's own revise door too: {}", refusal.problem);
        assert_eq!(show(&store, "eval-elsewhere").unwrap().project.as_deref(), Some("other"), "nothing moved");
    }

    /// D, the other half: adding the tag through the assistant's own door
    /// (`revise_because`) is a filing too - it never ran the gate at all.
    #[test]
    fn tagging_a_report_through_the_assistants_revise_door_is_gated() {
        let mut store = EventStore::in_memory().unwrap();
        declare_noted(&mut store, &light_command_rule("owed-e", "ee"));
        serve_n(&mut store, "owed-e", ASSISTANT_DELIVERY_ACTOR, JUDGEMENT_DEBT_AFTER);
        let mut plain = eval_report("plain-e", "ee");
        plain.tags = vec!["notes".to_string()];
        declare(&mut store, "s", "l", "a", &plain).unwrap();
        let mut tagged = plain.clone();
        tagged.tags.push(EVALUATION_REPORT_TAG.to_string());
        // Even with the filing context of an open evaluation: the owed
        // verdict on its list refuses this filing like any other.
        let start = record_start(&mut store, "s", "ee");
        let ctx = FilingContext::new(INFLIGHT_SEEN, start).unwrap();
        let err = revise_because_scoped(&mut store, "s", "l", "a", &plain, &tagged, &plain, None, Some(&ctx)).unwrap_err();
        assert!(err.to_string().contains("'owed-e'"), "{err}");
    }

    /// E (2026-09-26): a heavy rule is not retracted on a reason that fits
    /// any rule; one that names what changed about it goes through.
    #[test]
    fn a_heavy_rule_needs_a_reason_about_itself_to_be_retracted() {
        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s", "l", "a", &item).unwrap();
        assert!(retract(&mut store, "s", "l", "a", &item.id, "no longer relevant for this project", None).is_err());
        retract(&mut store, "s", "l", "a", &item.id, "config/app.toml no longer exists", None).unwrap();
    }

    // ------------------------------------------------ the batch gate, 2026-09-27
    //
    // A fresh evaluation could not file its report: a check that really bit
    // still read "wired", nobody knew what an open point was, a reason stood
    // beside a check that contradicted it, and an open point was tagged onto
    // the rule itself. Each test below is one of those, end to end through
    // the public write path.

    /// Written straight into the log, the way an older store holds it - the
    /// write gate of today may refuse the same shape fresh.
    fn legacy(store: &mut EventStore, item: &Item) {
        let body = serde_json::to_string(item).unwrap();
        store.append_event("legacy", &item.id, "migration", EventKind::FactCreated, &item.id, None, &body).unwrap();
    }

    /// A raw `fact_revised`, the shape an older binary could have written -
    /// for tests about what the log already holds, not about what a write
    /// through the gate may do today.
    fn legacy_revise(store: &mut EventStore, item: &Item) {
        let body = serde_json::to_string(item).unwrap();
        store.append_mutate_checked("legacy", &item.id, "migration", EventKind::FactRevised, &item.id, None, &body).unwrap();
    }

    /// A raw `fact_created` carrying a trial record, past every ground - for
    /// a test about how a record is READ back (an A4 failure recorded by a
    /// trial), never about what the gate lets in.
    fn legacy_tried(store: &mut EventStore, item: &Item, trial: &TrialRecord) {
        let body = body_with_trial(&canonical_body(item).unwrap(), trial).unwrap();
        store.append_event("legacy", &item.id, "migration", EventKind::FactCreated, &item.id, None, &body).unwrap();
    }

    fn wired_forbidden(id: &str, project: &str, literal: &str) -> Item {
        let mut item = heavy_command_rule(id, project);
        item.tags = vec![TEETH_WIRED_TAG.to_string()];
        item.check = Some(Check::Forbidden { literals: vec![literal.to_string()] });
        item
    }

    /// A heavy Rule anchored to a file, carrying a `Contains` check on it -
    /// the one kind `probe_both_sides_settle` may settle from a probe alone
    /// (see that function's own doc comment). The binding is a `Path`, not
    /// the `Command` `heavy_command_rule` starts from, so a fixture built
    /// from this one is never mistaken for a command-anchored rule by
    /// anything that reads `item.bindings`.
    fn wired_contains(id: &str, project: &str, path: &str, literal: &str) -> Item {
        let mut item = heavy_command_rule(id, project);
        item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: path.to_string() }];
        item.tags = vec![TEETH_WIRED_TAG.to_string()];
        item.check = Some(Check::Contains { path: path.to_string(), literal: literal.to_string() });
        item
    }

    /// A Claude Code session id, the shape `serve::attest` requires.
    const REAL_SESSION: &str = "0de7dc52-1b97-4b66-948f-61e4438043b2";

    fn refused_in(store: &mut EventStore, session: &str, id: &str, target: &str) -> i64 {
        let body = serde_json::json!({"at": "2026-09-27T00:00:00Z", "target": target, "tool_use_id": "toolu_fixture"})
            .to_string();
        store.append_event(session, session, "hook", EventKind::GateRefused, id, None, &body).unwrap().seq
    }

    fn served_in(store: &mut EventStore, session: &str, id: &str) {
        store.append_event(session, session, "hook", EventKind::ItemServed, id, None, "{}").unwrap();
    }

    /// The verification `serve::attest` records for a refusal, written raw
    /// the way a test writes any fixture event (a test holds no
    /// `SystemActor`).
    fn attested(store: &mut EventStore, session: &str, id: &str, refusal_seq: i64, field: bool) {
        let body = serde_json::json!({"refusal_seq": refusal_seq, "field": field, "why": "fixture"}).to_string();
        store.append_event(session, session, ATTESTER_ACTOR, EventKind::GateStoodAside, id, None, &body).unwrap();
    }

    /// Step 9: a refusal on record settles NOTHING - not an unverified one,
    /// not one whose verification says no, and not even one verified as
    /// field evidence from the session's own transcript. A refusal only sets
    /// the "proven" label of a trial; the rule is settled by the trial's
    /// verdict.
    #[test]
    fn no_refusal_settles_anything_attested_or_not() {
        let mut store = EventStore::in_memory().unwrap();
        file_now(&mut store, &eval_report("eval-bit-old", "bit")).unwrap();
        legacy(&mut store, &wired_forbidden("bit-rule", "bit", "--force"));
        served_in(&mut store, REAL_SESSION, "bit-rule");
        let seq = refused_in(&mut store, REAL_SESSION, "bit-rule", "fixture-command-bit-rule --force");
        assert!(settlement(&store, "bit-rule", None).is_owed(), "unverified");
        attested(&mut store, REAL_SESSION, "bit-rule", seq, true);
        assert_eq!(attested_field_refusals(&store.get_all_events().unwrap()), HashSet::from([seq]));
        assert!(settlement(&store, "bit-rule", None).is_owed(), "verified as field evidence, still no verdict");
        let err = file_now(&mut store, &eval_report("eval-bit-new", "bit")).unwrap_err();
        assert!(err.to_string().contains("'bit-rule'"), "{err}");

        // A refusal under a non-hook actor, and one from a session with no
        // other activity, are the same: nothing.
        refused_in(&mut store, "lonely", "bit-rule", "fixture-command-bit-rule --force");
        store
            .append_event("s", "l", "probe", EventKind::GateRefused, "bit-rule", None, &serde_json::json!({"target": "x --force"}).to_string())
            .unwrap();
        assert!(settlement(&store, "bit-rule", None).is_owed());

        // Green: the trial's verdict settles it.
        let rule = show(&store, "bit-rule").unwrap();
        let mut named = rule.clone();
        named.text = "fixture heavy rule bit-rule: never with --force".to_string();
        revise_tried_scoped(&mut store, "s", "l", "a", &rule, &named, &rule, None, None, Some(&sealed(&named, RecordedVerdict::RuleWired)), None, None)
            .unwrap();
        file_now(&mut store, &eval_report("eval-bit-new", "bit")).expect("the verdict settles it");
    }

    /// The writer of a verification: only THOR's attester, once per refusal.
    /// A second verification of the same refusal appends nothing, and any
    /// other system actor is refused before anything is read.
    #[test]
    fn a_refusal_verification_is_recorded_by_the_attester_once() {
        let mut store = EventStore::in_memory().unwrap();
        let before = store.get_all_events().unwrap().len();
        let first = record_refusal_verification(&mut store, REAL_SESSION, SystemActor::new(ATTESTER_ACTOR), "r", 7, true, "held")
            .unwrap();
        assert!(first.is_some());
        let again = record_refusal_verification(&mut store, REAL_SESSION, SystemActor::new(ATTESTER_ACTOR), "r", 7, false, "no")
            .unwrap();
        assert!(again.is_none(), "a refusal is verified once");
        let wrong = record_refusal_verification(&mut store, REAL_SESSION, SystemActor::new("retest"), "r", 8, true, "x");
        assert!(matches!(wrong, Err(WriteError::Refused(_))), "{wrong:?}");
        let events = store.get_all_events().unwrap();
        assert_eq!(events.len(), before + 1);
        let verdicts = recorded_refusal_verdicts(&events);
        assert_eq!(verdicts.get(&7), Some(&(true, "held".to_string())));
        assert_eq!(attested_field_refusals(&events), HashSet::from([7]));
    }

    /// A verification written raw under any other actor is never read.
    #[test]
    fn only_the_attester_s_verdicts_are_read() {
        let mut store = EventStore::in_memory().unwrap();
        let body = serde_json::json!({"refusal_seq": 3, "field": true, "why": "forged"}).to_string();
        store.append_event("s", "s", "hook", EventKind::GateStoodAside, "r", None, &body).unwrap();
        assert!(attested_field_refusals(&store.get_all_events().unwrap()).is_empty());
    }

    /// The trial's store holds exactly the item, as written, and nothing else.
    #[test]
    fn one_item_store_holds_exactly_the_item_as_written() {
        let item = force_push_rule("solo");
        let store = one_item_store(&item).unwrap();
        assert_eq!(show(&store, "solo").unwrap(), item);
        assert_eq!(store.get_all_events().unwrap().len(), 1);
        assert!(store.item_binding_projection_current());
    }

    /// R9.10: an event of the retired probe (actor `teeth-probe`, both of its
    /// sides holding - the receipt shape it used to write) settles nothing,
    /// on a contains check too, and is never field evidence.
    #[test]
    fn a_probe_event_is_never_field_evidence() {
        let mut store = EventStore::in_memory().unwrap();
        file_now(&mut store, &eval_report("eval-co-old", "co")).unwrap();
        // Registered, its file holding the literal, so no blocker holds it
        // (a missing working copy, or no project at all since step 20, would
        // settle it as blocked): owed, and a probe changes nothing about that.
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("co");
        std::fs::create_dir_all(&checkout).unwrap();
        let hooks = dir.path().join("no-hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(&checkout).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
        git(&["config", "user.email", "fixture@example.invalid"]);
        git(&["config", "user.name", "fixture"]);
        std::fs::write(checkout.join("NOTES.md"), "keep me
").unwrap();
        git(&["add", "NOTES.md"]);
        git(&["commit", "-q", "-m", "notes"]);
        let record = project_record("co");
        let body = body_with_root(&canonical_body(&record).unwrap(), checkout.to_str().unwrap(), "2026-09-29T00:00:00Z").unwrap();
        store.append_event("install", "install", INSTALLER_ACTOR, EventKind::FactCreated, &record.id, None, &body).unwrap();
        let rule = wired_contains("co-rule", "co", "NOTES.md", "keep me");
        legacy(&mut store, &rule);
        serve_n(&mut store, "co-rule", "hook", 1);
        for kind in [EventKind::GateRefused, EventKind::GateStoodAside] {
            store
                .append_event(
                    "probe-session",
                    "probe-session",
                    TEETH_PROBE_ACTOR,
                    kind,
                    "co-rule",
                    None,
                    &serde_json::json!({
                        "at": "2026-09-27T00:00:00Z",
                        "target": "probe-teeth co-rule",
                        "forbidden_holds": true,
                        "clean_holds": true,
                    })
                    .to_string(),
                )
                .unwrap();
        }
        assert!(attested_field_refusals(&store.get_all_events().unwrap()).is_empty(), "never field evidence");
        assert!(settlement(&store, "co-rule", None).is_owed(), "{:?}", settlement(&store, "co-rule", None));
        let err = file_now(&mut store, &eval_report("eval-co-new", "co")).unwrap_err();
        assert!(err.to_string().contains("'co-rule'"), "the filing stays refused: {err}");
    }

    /// The filing refusal names the one command that decides each rule, on
    /// the very store the refusing process has open (F7), and nothing that
    /// no longer settles anything.
    /// C3b E1: the printed retest command is the shell-neutral form the
    /// evaluation routine's `allowed-tools` entry allows - forward slashes, no
    /// `&` - and it always starts with that entry's own text, with or without
    /// a space in a path.
    #[test]
    fn the_retest_command_is_the_form_its_allow_entry_allows() {
        for (program, db) in [
            ("C:\\thor2\\bin\\serve.exe", "C:\\thor2\\thor.db"),
            ("C:\\Program Files\\thor2\\bin\\serve.exe", "C:\\Users\\a b\\thor.db"),
            ("/opt/thor2/bin/serve", "/var/thor 2/thor.db"),
        ] {
            let (program, db) = (Path::new(program), Path::new(db));
            let command = retest_command_with(program, Some(db), "rule-a");
            let entry = retest_allow_entry(program, Some(db));
            let prefix = entry.strip_prefix("Bash(").and_then(|e| e.strip_suffix(":*)")).expect("a Bash(...:*) entry");
            assert!(command.starts_with(prefix), "{command} vs {entry}");
            assert!(prefix.ends_with(" retest"), "{entry}");
            assert!(!command.starts_with('&') && !command.contains('\\'), "no call operator, no backslash: {command}");
            assert!(command.ends_with(" retest --id 'rule-a'"), "{command}");
            assert!(command.contains(" --db "), "every printed command carries --db: {command}");
        }
        assert_eq!(
            retest_command_with(Path::new("C:\\thor2\\bin\\serve.exe"), None, "r"),
            "C:/thor2/bin/serve.exe --db \"<path to thor.db>\" retest --id 'r'"
        );
    }

    #[test]
    fn the_filing_refusal_names_the_retest_command_on_this_store() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        file_now(&mut store, &eval_report("eval-op-old", "op")).unwrap();
        legacy(&mut store, &wired_forbidden("op-rule", "op", "--force"));
        serve_n(&mut store, "op-rule", "hook", 1);
        let err = file_now(&mut store, &eval_report("eval-op-new", "op")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        let text = refusal.problem;
        let shown = db.display().to_string().replace('\\', "/");
        let shown = if shown.contains(' ') { format!("\"{shown}\"") } else { shown };
        assert!(text.contains(&format!("--db {shown} retest --id 'op-rule'")), "{text}");
        assert!(!text.contains("& \""), "the shell-neutral form, no PowerShell call operator: {text}");
        assert!(text.contains("--apply"), "{text}");
        assert!(!text.contains("probe-teeth"), "a retired command is never named: {text}");
        assert!(!text.contains("tags [\"open-point\"]"), "an open point settles nothing any more: {text}");
        assert!(!text.contains("trial_check"), "no field that does not exist any more: {text}");
    }

    /// Fix 3: a reason saying nothing can catch the mistake never settles a
    /// rule whose own check can refuse - the check is the answer, and the
    /// reason beside it contradicts it.
    #[test]
    fn a_reason_beside_a_refusing_check_never_settles_the_filing() {
        let mut store = EventStore::in_memory().unwrap();
        file_now(&mut store, &eval_report("eval-rc-old", "rc")).unwrap();
        let mut both = wired_forbidden("rc-rule", "rc", "--force");
        both.tags.push(format!(
            "{}same text as 'fixture-command-rc-rule --force' in a legitimate call",
            NO_CATCH_ON_COMMAND_REASON_PREFIX
        ));
        legacy(&mut store, &both);
        serve_n(&mut store, "rc-rule", "hook", 1);
        let err = file_now(&mut store, &eval_report("eval-rc-new", "rc")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("rc-rule"), "{}", refusal.problem);
    }

    /// Fix 3: the write gate refuses hanging a refusing check beside such a
    /// reason, or adding the reason beside the check - and leaves a rule that
    /// already carried both correctable.
    #[test]
    fn the_write_gate_refuses_a_refusing_check_and_a_reason_introduced_together() {
        let mut store = EventStore::in_memory().unwrap();
        let mut both = heavy_command_rule("wg-both", "wg");
        both.check = Some(Check::Forbidden { literals: vec!["--force".to_string()] });
        let err = declare(&mut store, "s", "l", "a", &both).unwrap_err();
        assert!(err.to_string().contains("one of the two is wrong"), "{err}");

        let plain = heavy_command_rule("wg-plain", "wg");
        declare(&mut store, "s", "l", "a", &plain).unwrap();
        let mut hung = plain.clone();
        hung.check = Some(Check::Forbidden { literals: vec!["--force".to_string()] });
        let err = revise(&mut store, "s", "l", "a", &plain, &hung).unwrap_err();
        assert!(err.to_string().contains("one of the two is wrong"), "{err}");

        let mut old = heavy_command_rule("wg-old", "wg");
        old.check = Some(Check::Forbidden { literals: vec!["--force".to_string()] });
        legacy(&mut store, &old);
        // A rule that already carried both stays correctable: dropping the
        // reason tag lands, and so does a write that leaves its text alone.
        let mut pinned = old.clone();
        pinned.bindings.push(Binding::Always);
        revise(&mut store, "s", "l", "a", &old, &pinned).expect("a write that leaves the text alone lands");
        let mut corrected = pinned.clone();
        corrected.tags = vec!["fixture-topic".to_string()];
        revise(&mut store, "s", "l", "a", &pinned, &corrected).expect("dropping the reason corrects it");
    }

    /// Field test 2 (2026-10-01): a-2eeb was refused for carrying a working
    /// check beside a reason saying nothing can catch its mistake, while
    /// b-2a644 kept exactly that pair through a reword - the gate ran only when
    /// a revise touched the check or the tags, and never counted a contains
    /// check as one that refuses. Now a text-only revise of an item that
    /// carries both is held to it too, and a contains check counts.
    #[test]
    fn a_text_revise_of_a_rule_carrying_a_check_and_a_reason_is_refused() {
        let mut store = EventStore::in_memory().unwrap();
        let mut old = heavy_command_rule("wg-reword", "wg");
        old.check = Some(Check::Forbidden { literals: vec!["--force".to_string()] });
        legacy(&mut store, &old);
        let mut reworded = old.clone();
        reworded.text = "fixture heavy rule wg-reword, reworded".to_string();
        let err = revise(&mut store, "s", "l", "a", &old, &reworded).unwrap_err();
        assert!(err.to_string().contains("one of the two is wrong"), "{err}");

        let mut guarded = heavy_command_rule("wg-contains", "wg");
        guarded.bindings = vec![Binding::Target { kind: crate::item::TargetKind::Path, value: "files/INGRESS.md".to_string() }];
        guarded.text = "files/INGRESS.md keeps `only-owner` for the dashboard route.".to_string();
        guarded.check = Some(Check::Contains { path: "files/INGRESS.md".to_string(), literal: "only-owner".to_string() });
        legacy(&mut store, &guarded);
        let mut reworded = guarded.clone();
        reworded.text = "files/INGRESS.md keeps `only-owner` for the dashboard route, reworded.".to_string();
        let err = revise(&mut store, "s", "l", "a", &guarded, &reworded).unwrap_err();
        assert!(err.to_string().contains("one of the two is wrong"), "a contains check counts too: {err}");
    }

    /// Fix 1: an open point tagged onto the rule itself counts for nothing,
    /// so the write gate says so and names the recipe that counts.
    #[test]
    fn an_open_point_tag_on_the_rule_itself_is_refused_with_the_recipe() {
        let mut store = EventStore::in_memory().unwrap();
        let rule = wired_forbidden("tag-rule", "tg", "--force");
        legacy(&mut store, &rule);
        let mut tagged = rule.clone();
        tagged.tags.push("open:tag-rule:a real push with --force would prove it".to_string());
        let err = revise(&mut store, "s", "l", "a", &rule, &tagged).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("never a tag on the rule itself"), "{text}");
        assert!(text.contains("written and closed by THOR itself"), "{text}");
        assert!(!text.contains("kind \"report\""), "nobody files an open point any more: {text}");
    }

    /// Step 9: an open point naming a rule carries nothing any more - with a
    /// check or without one, the rule stays owed until THOR's trial decides
    /// it.
    #[test]
    fn an_open_point_never_settles_a_rule_with_or_without_a_check() {
        let mut store = EventStore::in_memory().unwrap();
        file_now(&mut store, &eval_report("eval-ci-old", "ci")).unwrap();
        let mut informing = heavy_command_rule("ci-check", "ci");
        informing.tags = vec![TEETH_INFORMING_TAG.to_string()];
        informing.check = Some(Check::Forbidden { literals: vec!["up -d --build".to_string()] });
        legacy(&mut store, &informing);
        let mut bare = heavy_command_rule("ci-bare", "ci");
        bare.tags = vec![TEETH_INFORMING_TAG.to_string()];
        legacy(&mut store, &bare);
        serve_n(&mut store, "ci-check", "hook", 1);
        serve_n(&mut store, "ci-bare", "hook", 1);
        let mut point = open_point("open-ci", "ci");
        point.text = "ci-check and ci-bare: the path binding deploy/watch.sh is full".to_string();
        declare(&mut store, "s", "l", "a", &point).unwrap();
        let err = file_now(&mut store, &eval_report("eval-ci-new", "ci")).unwrap_err();
        let WriteError::Refused(refusal) = err else { panic!("expected a refusal: {err}") };
        assert!(refusal.problem.contains("'ci-check'"), "{}", refusal.problem);
        assert!(refusal.problem.contains("'ci-bare'"), "{}", refusal.problem);
    }

    // ------------------------------------ a global write on a place full per project

    /// A live item at one Command anchor, in `project` (None = global).
    fn scoped_command_item(id: &str, nth: usize, command: &str, project: Option<&str>) -> Item {
        let mut item = sample_with(id, DISTINCT[nth % DISTINCT.len()]);
        item.bindings = vec![Binding::Target { kind: TargetKind::Command, value: command.to_string() }];
        item.project = project.map(str::to_string);
        item
    }

    /// `project` registered with a working copy, holding `occupants` live items
    /// at `command`.
    fn project_with_occupants(store: &mut EventStore, project: &str, command: &str, occupants: usize) {
        declare_project_record(store, &project_record(project), "C:/tmp/fixture", "2026-10-07T10:00:00Z", installer()).unwrap();
        for i in 0..occupants {
            seed_live(store, &scoped_command_item(&format!("{project}-occ-{i}"), i, command, Some(project)));
        }
    }

    /// THE DEFECT THIS ANSWERS (field test 12, R2): the gate compares a global
    /// write only with globals, so a rule written global onto a command that is
    /// already full in one project was accepted without a word and then
    /// outranked in that project. The note names the project and the count.
    #[test]
    fn a_global_write_on_a_place_full_in_one_project_gets_a_note_naming_it() {
        let mut store = EventStore::in_memory().unwrap();
        project_with_occupants(&mut store, "alpha", "ssh fixture-host", crate::item::MAX_ITEMS);
        project_with_occupants(&mut store, "beta", "ssh fixture-host", 1);
        let global = scoped_command_item("new-global", 8, "ssh fixture-host", None);
        let note = global_write_crowded_in_projects(&store, &global, None).expect("full in alpha");
        assert!(note.contains("project 'alpha' (4 occupant(s))"), "{note}");
        assert!(!note.contains("beta"), "beta has room and must not be named: {note}");
        assert!(note.contains("fold") && note.contains("move") && note.contains("retract"), "the three exits: {note}");
    }

    /// The other half: no registered project is full there, no note.
    #[test]
    fn a_global_write_on_a_place_with_room_everywhere_gets_no_note() {
        let mut store = EventStore::in_memory().unwrap();
        project_with_occupants(&mut store, "alpha", "ssh fixture-host", crate::item::MAX_ITEMS - 1);
        project_with_occupants(&mut store, "beta", "ssh fixture-host", 1);
        let global = scoped_command_item("new-global", 8, "ssh fixture-host", None);
        assert_eq!(global_write_crowded_in_projects(&store, &global, None), None);
    }

    /// A project nobody registered a working copy for is not a place a
    /// session ever works in, so a crowd there is not named.
    #[test]
    fn a_project_without_a_registered_working_copy_is_not_named() {
        let mut store = EventStore::in_memory().unwrap();
        for i in 0..crate::item::MAX_ITEMS {
            seed_live(&mut store, &scoped_command_item(&format!("gamma-occ-{i}"), i, "ssh fixture-host", Some("gamma")));
        }
        let global = scoped_command_item("new-global", 8, "ssh fixture-host", None);
        assert_eq!(global_write_crowded_in_projects(&store, &global, None), None);
    }

    /// A revise that keeps a global item's binding is not asked again (the
    /// gate skips an already-held binding the same way), and a project item
    /// is never asked: its own pool is already counted by the gate.
    #[test]
    fn a_kept_global_binding_and_a_project_item_get_no_note() {
        let mut store = EventStore::in_memory().unwrap();
        project_with_occupants(&mut store, "alpha", "ssh fixture-host", crate::item::MAX_ITEMS);
        let global = scoped_command_item("kept-global", 8, "ssh fixture-host", None);
        assert_eq!(global_write_crowded_in_projects(&store, &global, Some(&global)), None);
        let scoped = scoped_command_item("scoped", 8, "ssh fixture-host", Some("alpha"));
        assert_eq!(global_write_crowded_in_projects(&store, &scoped, None), None);
        // Moving a project item to global IS asked.
        assert!(global_write_crowded_in_projects(&store, &global, Some(&scoped)).is_some());
    }

    // ---------------------------------------------- the actor fence (D1, D2)

    /// A project record the way the installer writes one - a Report, scoped
    /// to its project, no binding.
    fn project_record(key: &str) -> Item {
        Item {
            id: project_opened_id(key),
            kind: Kind::Report,
            text: format!("Project {key}: a fixture record of a registered working copy."),
            bindings: Vec::new(),
            severity: None,
            project: Some(key.to_string()),
            tags: vec!["project-record".to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        }
    }

    fn installer() -> SystemActor {
        SystemActor::new(INSTALLER_ACTOR)
    }

    /// THE LEAK THIS CLOSES (D1): the reserved names were refused by one
    /// binary at a time, so any other caller of this module could write as
    /// `eval-machine`. Now every public write function refuses a reserved
    /// name passed as a plain string, before anything is read or appended -
    /// `declare_scoped` called directly, as any bin would, included - and
    /// THOR's own writer, holding a `SystemActor`, still gets through.
    #[test]
    fn a_reserved_actor_is_refused_at_the_store() {
        for name in RESERVED_ACTORS {
            let mut store = EventStore::in_memory().unwrap();
            let item = sample_with(&format!("fenced-{name}"), "a fixture written under a name only THOR may use");
            let err = declare_scoped(&mut store, "S1", "S1", *name, &item, None, None).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("the actor name {name} is reserved for THOR itself - write without --actor, or with your own name")
            );
            assert!(store.get_all_events().unwrap().is_empty(), "{name}: a refused write must append nothing");
        }

        let mut store = EventStore::in_memory().unwrap();
        let item = sample();
        declare(&mut store, "s", "l", "a", &item).unwrap();
        let before = store.get_all_events().unwrap().len();
        let mut updated = item.clone();
        updated.text = "the config now lives in config/app.toml, read once at startup".to_string();
        let reserved = "eval-machine";
        let other = sample_with("other-id", "an entirely different fixture sentence");
        let refusals: Vec<(&str, Result<Event, WriteError>)> = vec![
            ("declare", declare(&mut store, "s", "l", reserved, &other)),
            ("declare_in", declare_in(&mut store, "s", "l", reserved, &other, None)),
            ("revise", revise(&mut store, "s", "l", reserved, &item, &updated)),
            ("revise_because", revise_because(&mut store, "s", "l", reserved, &item, &updated, &item, None)),
            (
                "revise_because_scoped",
                revise_because_scoped(&mut store, "s", "l", reserved, &item, &updated, &item, None, None),
            ),
            (
                "revise_tried_scoped",
                revise_tried_scoped(&mut store, "s", "l", reserved, &item, &updated, &item, None, None, None, None, None),
            ),
            ("archive", archive(&mut store, "s", "l", reserved, &item.id, "its subject is gone", None)),
            (
                "restore_deliberate_anchor",
                restore_deliberate_anchor(&mut store, "s", "l", reserved, &item.id, Kind::Rule, "config/app.toml"),
            ),
            ("retract", retract(&mut store, "s", "l", reserved, &item.id, "a fixture retraction", None)),
            ("resolve", resolve(&mut store, "s", "l", reserved, &item.id, "x", &[], None)),
        ];
        for (door, result) in refusals {
            let err = result.expect_err(door);
            assert!(err.to_string().contains("is reserved for THOR itself"), "{door}: {err}");
        }
        assert_eq!(store.get_all_events().unwrap().len(), before, "no door may append under a reserved name");

        // THOR's own writer: the same name, carried as a token, is let through.
        revise(&mut store, "s", "l", SystemActor::new(reserved), &item, &updated).expect("a SystemActor writes");
        let head = history(&store, &item.id).unwrap().pop().unwrap();
        assert_eq!(head.actor, reserved);
    }

    /// THE LEAK THIS CLOSES (D2): a retract through the tool server would
    /// tombstone a project's record, and a tombstone can never be revised, so
    /// no registration could ever land again. Every event on a reserved id
    /// by any other actor - an ordinary name, or THOR's own token under the
    /// wrong name - is refused with the sentence that names the one fix.
    #[test]
    fn a_reserved_id_refuses_every_other_actor() {
        let mut store = EventStore::in_memory().unwrap();
        let record = project_record("demo");

        for actor in [WriteActor::Named("mcp"), WriteActor::System(SystemActor::new("teeth-eval"))] {
            let err = declare(&mut store, "s", "l", actor, &record).unwrap_err().to_string();
            assert!(err.contains("this record is THOR's own registration of a project's working copy"), "{err}");
            assert!(err.contains("--register-root --db"), "the fix names the store to register in: {err}");
        }
        assert!(store.get_all_events().unwrap().is_empty());

        declare_project_record(&mut store, &record, "C:/tmp/demo", "2026-09-28T10:00:00Z", installer()).unwrap();
        let before = store.get_all_events().unwrap().len();
        let mut changed = record.clone();
        changed.tags.push("checkout-root:C:/tmp/copy".to_string());
        let doors: Vec<(&str, Result<Event, WriteError>)> = vec![
            ("revise", revise(&mut store, "mcp", "mcp", "mcp", &record, &changed)),
            ("retract", retract(&mut store, "mcp", "mcp", "mcp", &record.id, "a project record nobody wants", None)),
            ("archive", archive(&mut store, "mcp", "mcp", "mcp", &record.id, "moved", None)),
            ("resolve", resolve(&mut store, "mcp", "mcp", "mcp", &record.id, "x", &[], None)),
            (
                "revise_project_root",
                revise_project_root(&mut store, &record.id, "C:/tmp/copy", "2026-09-28T11:00:00Z", SystemActor::new("retest")),
            ),
        ];
        for (door, result) in doors {
            let err = result.expect_err(door).to_string();
            assert!(err.contains("THOR's own registration"), "{door}: {err}");
        }
        assert_eq!(store.get_all_events().unwrap().len(), before, "no refused door may append");
        assert!(show(&store, &record.id).is_ok(), "the record stays live");
        assert_eq!(registered_root_raw(&store, "demo").unwrap().0, PathBuf::from("C:/tmp/demo"));

        let mut fallback = project_record("gamma");
        fallback.id = project_root_id("gamma");
        assert!(declare(&mut store, "s", "l", "mcp", &fallback).is_err(), "root-by-install is reserved too");

        let mut start = project_record("demo");
        start.id = "evaluation-start-demo-1".to_string();
        start.text = "A fixture evaluation start for demo.".to_string();
        start.tags = Vec::new();
        let err = declare(&mut store, "s", "l", "mcp", &start).unwrap_err().to_string();
        assert!(err.contains("does not begin with 'evaluation-start-'"), "{err}");
        declare(&mut store, "s", "l", SystemActor::new("eval-machine"), &start).expect("eval-machine writes its own start");

        assert_eq!(reserved_id_owner("project-demo-notes"), None);
        assert_eq!(reserved_id_owner("demo-opened-by-install"), None);
        assert_eq!(reserved_id_owner("project-demo-opened-by-install"), Some(INSTALLER_ACTOR));
    }

    /// Only the installer's own events say where a working copy is. An event
    /// by any other actor that carries a `"root"` key - a raw write from
    /// before the fence, planted straight into the log - is ignored, before
    /// and after a real registration; a record retracted before this build
    /// is read from `project_root_id` instead.
    #[test]
    fn registered_root_raw_ignores_non_installer_events() {
        let mut store = EventStore::in_memory().unwrap();
        let record = project_record("demo");
        declare(&mut store, "install", "install", installer(), &record).unwrap();
        assert_eq!(registered_root_raw(&store, "demo"), None, "a record with no root registers nothing");

        let forged = body_with_root(&canonical_body(&record).unwrap(), "C:/tmp/copy", "2026-09-28T10:00:00Z").unwrap();
        store.append_mutate_checked("mcp", "mcp", "mcp", EventKind::FactRevised, &record.id, None, &forged).unwrap();
        assert_eq!(registered_root_raw(&store, "demo"), None, "a root written by another actor never counts");

        revise_project_root(&mut store, &record.id, "C:/tmp/demo", "2026-09-28T10:05:00Z", installer()).unwrap();
        let (root, seq) = registered_root_raw(&store, "demo").unwrap();
        assert_eq!(root, PathBuf::from("C:/tmp/demo"));

        store.append_mutate_checked("mcp", "mcp", "mcp", EventKind::FactRevised, &record.id, None, &forged).unwrap();
        assert_eq!(
            registered_root_raw(&store, "demo"),
            Some((PathBuf::from("C:/tmp/demo"), seq)),
            "a later forgery moves nothing"
        );
        let regs = root_registrations(&store, "demo");
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].at.as_deref(), Some("2026-09-28T10:05:00Z"));

        // Retracted before this build (a raw tombstone, as the tool server could once write).
        let mut old = EventStore::in_memory().unwrap();
        let beta = project_record("beta");
        declare(&mut old, "install", "install", installer(), &beta).unwrap();
        revise_project_root(&mut old, &beta.id, "C:/tmp/beta-old", "2026-09-01T00:00:00Z", installer()).unwrap();
        let tomb = serde_json::json!({ "retracted": beta.id, "reason": "a fixture retraction" }).to_string();
        old.append_mutate_checked("mcp", "mcp", "mcp", EventKind::FactRetracted, &beta.id, None, &tomb).unwrap();
        assert_eq!(registered_root_raw(&old, "beta"), None, "a tombstoned record's own events no longer count");
        let mut fallback = project_record("beta");
        fallback.id = project_root_id("beta");
        declare_project_record(&mut old, &fallback, "C:/tmp/beta", "2026-09-28T10:00:00Z", installer()).unwrap();
        assert_eq!(registered_root_raw(&old, "beta").unwrap().0, PathBuf::from("C:/tmp/beta"));
    }

    #[test]
    fn a_system_actor_carries_only_a_reserved_name() {
        assert!(RESERVED_ACTORS.contains(&TEETH_PROBE_ACTOR));
        assert!(RESERVED_ACTORS.contains(&ASSISTANT_DELIVERY_ACTOR));
        assert_eq!(SystemActor::new(INSTALLER_ACTOR).name(), "installer");
        assert!(std::panic::catch_unwind(|| SystemActor::new("mcp")).is_err(), "an ordinary name is no system actor");
    }

    #[test]
    fn the_register_command_always_names_the_store() {
        let cmd = register_command_for(Some(Path::new("C:/data/thor.db")));
        assert!(cmd.starts_with("install"), "{cmd}");
        assert!(cmd.ends_with("--register-root --db \"C:/data/thor.db\""), "{cmd}");
        assert!(register_command_for(None).contains("--db \""), "even without a path it asks for one");
    }

    // ---------------------------------------------- grep tests over the workspace

    /// The workspace's own crates: the only code that is built.
    const WORKSPACE_MEMBERS: &[&str] = &["core", "intent", "model", "serve", "codeindex", "mcp", "ops", "library"];

    fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                collect_rs(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Every `.rs` file under the members of `root`, with its path relative
    /// to `root` in forward slashes, and its text.
    fn member_files(root: &Path) -> Vec<(String, String)> {
        let mut files = Vec::new();
        for member in WORKSPACE_MEMBERS {
            collect_rs(&root.join(member), &mut files);
        }
        let mut out: Vec<(String, String)> = files
            .into_iter()
            .filter_map(|p| {
                let rel = p.strip_prefix(root).ok()?.to_string_lossy().replace('\\', "/");
                Some((rel, std::fs::read_to_string(&p).ok()?))
            })
            .collect();
        out.sort();
        out
    }

    /// `text` with every inline test module blanked out: a `#[cfg(test)]` or
    /// `#[cfg(all(test, ...))]` attribute on a `mod name {`, up to the first
    /// line that is exactly `}` (rustfmt closes a top-level module there). A
    /// module file whose first line of code is `#![cfg(test)]` (a test module
    /// kept in a file of its own, like `serve/src/teeth/tests.rs`) is test
    /// code as a whole; the attribute anywhere else changes nothing.
    fn without_test_modules(text: &str) -> String {
        let first_code = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("//"));
        if first_code == Some("#![cfg(test)]") {
            return String::new();
        }
        let lines: Vec<&str> = text.lines().collect();
        let mut out = Vec::with_capacity(lines.len());
        let mut i = 0;
        while i < lines.len() {
            let t = lines[i].trim();
            if t == "#[cfg(test)]" || t.starts_with("#[cfg(all(test") {
                let mut j = i + 1;
                while j < lines.len() && (lines[j].trim().is_empty() || lines[j].trim().starts_with("#[")) {
                    j += 1;
                }
                let head = lines.get(j).map(|l| l.trim_start()).unwrap_or("");
                let head = head.strip_prefix("pub ").unwrap_or(head);
                if head.starts_with("mod ") && head.trim_end().ends_with('{') {
                    let mut k = j + 1;
                    while k < lines.len() && lines[k] != "}" {
                        k += 1;
                    }
                    out.extend(std::iter::repeat("").take(k + 1 - i));
                    i = k + 1;
                    continue;
                }
            }
            out.push(lines[i]);
            i += 1;
        }
        out.join("\n")
    }

    /// The modules allowed to build a `SystemActor` - THOR's own writers:
    /// the installer, the teeth trial, the evaluation's session and filing,
    /// and the attester - plus this file, which defines it and tests it.
    const SYSTEM_ACTOR_MODULES: &[&str] = &[
        "model/src/store.rs",
        "ops/src/install.rs",
        "serve/src/teeth.rs",
        "serve/src/eval_session.rs",
        "serve/src/eval_filing.rs",
        "serve/src/attest.rs",
    ];

    /// Every file under `root` that builds a `SystemActor` (or renames the
    /// type, which is how a grep would be walked around) outside the allowed
    /// modules - test code included: a test has no business holding THOR's
    /// own token either, it writes a fixture event raw instead.
    fn system_actor_offenders(root: &Path) -> Vec<String> {
        member_files(root)
            .into_iter()
            .filter(|(rel, _)| !SYSTEM_ACTOR_MODULES.contains(&rel.as_str()))
            .filter(|(_, text)| text.contains("SystemActor::new(") || text.contains("SystemActor as "))
            .map(|(rel, _)| rel)
            .collect()
    }

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("model sits inside the workspace").to_path_buf()
    }

    #[test]
    fn system_actor_is_built_only_in_the_named_modules() {
        let root = workspace_root();
        assert!(member_files(&root).len() > 50, "the workspace sources must be found under {}", root.display());
        assert_eq!(system_actor_offenders(&root), Vec::<String>::new());

        // Red: the same scan over a fixture tree that builds one where it may not.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("serve/src/bin")).unwrap();
        std::fs::write(tmp.path().join("serve/src/bin/repair.rs"), "fn f() { let a = SystemActor::new(\"installer\"); }\n")
            .unwrap();
        std::fs::create_dir_all(tmp.path().join("ops/src")).unwrap();
        std::fs::write(tmp.path().join("ops/src/install.rs"), "fn f() { let a = SystemActor::new(\"installer\"); }\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("mcp/src")).unwrap();
        std::fs::write(tmp.path().join("mcp/src/lib.rs"), "use model::store::SystemActor as Token;\n").unwrap();
        assert_eq!(
            system_actor_offenders(tmp.path()),
            vec!["mcp/src/lib.rs".to_string(), "serve/src/bin/repair.rs".to_string()]
        );
    }

    /// Every production call site of core's raw append functions outside
    /// this module, their definitions in `core/src/event_store.rs`, and the
    /// hook's own recorders (`serve/src/deliver.rs`). A new call site fails
    /// `every_raw_append_call_site_is_listed` until someone has looked at it
    /// and added it here, with the reason it may write around the fence.
    const REVIEWED_RAW_APPENDS: &[(&str, usize)] = &[
        // Restoring a backup replays its events into an empty store.
        ("core/src/backup.rs", 1),
        // The mark writers: each goes through `fence` before it appends.
        ("serve/src/mark.rs", 2),
        // (`probe-teeth`'s one event in serve/src/bin/serve.rs went with the
        // subcommand in step 9: `retest --apply` writes through the fenced
        // `revise_tried_scoped`.)
    ];

    fn raw_append_sites(root: &Path) -> std::collections::BTreeMap<String, usize> {
        let needles = [".append_event(", ".append_resolve(", ".append_resolve_stamped(", ".append_mutate_checked(", ".append_created_unique("];
        let excluded = ["core/src/event_store.rs", "model/src/store.rs", "serve/src/deliver.rs"];
        let mut out = std::collections::BTreeMap::new();
        for (rel, text) in member_files(root) {
            if rel.contains("/tests/") || excluded.contains(&rel.as_str()) {
                continue;
            }
            let production = without_test_modules(&text);
            let count: usize = needles.iter().map(|n| production.matches(n).count()).sum();
            if count > 0 {
                out.insert(rel, count);
            }
        }
        out
    }

    #[test]
    fn every_raw_append_call_site_is_listed() {
        let found = raw_append_sites(&workspace_root());
        let reviewed: std::collections::BTreeMap<String, usize> =
            REVIEWED_RAW_APPENDS.iter().map(|(f, n)| (f.to_string(), *n)).collect();
        assert_eq!(
            found, reviewed,
            "a raw append call site outside model::store changed - review it (it writes around the actor fence) \
             and update REVIEWED_RAW_APPENDS"
        );

        // Red: one planted production call site is found; the same call inside
        // a test module, or in a tests/ file, is not.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("serve/src")).unwrap();
        std::fs::create_dir_all(tmp.path().join("serve/tests")).unwrap();
        std::fs::write(
            tmp.path().join("serve/src/side_door.rs"),
            "fn f(s: &mut S) {\n    s\n        .append_event(\"s\", \"l\", \"eval-machine\", K, \"x\", None, \"{}\");\n}\n\
             \n#[cfg(test)]\nmod tests {\n    fn t(s: &mut S) {\n        s.append_event();\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("serve/tests/fixture.rs"), "fn t(s: &mut S) { s.append_event(); }\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("serve/src/trial")).unwrap();
        std::fs::write(
            tmp.path().join("serve/src/trial/tests.rs"),
            "//! Units.\n\n#![cfg(test)]\n\nfn t(s: &mut S) { s.append_event(); }\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("serve/src/trial/late.rs"),
            "fn f(s: &mut S) { s.append_event(); }\n#![cfg(test)]\n",
        )
        .unwrap();
        let planted = raw_append_sites(tmp.path());
        assert_eq!(
            planted.into_iter().collect::<Vec<_>>(),
            vec![("serve/src/side_door.rs".to_string(), 1), ("serve/src/trial/late.rs".to_string(), 1)],
            "a file that opens with #![cfg(test)] is test code; the attribute further down changes nothing"
        );
    }

    // ------------------------------------------ the verdict in the log (step 6)

    /// A light rule bound to 'git push', carrying a forbidden check its text
    /// names - the shape the trial will store as wired.
    fn force_push_rule(id: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: "Never run `git push --force` on main.".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: "git push".to_string() }],
            severity: None,
            project: Some("demo".to_string()),
            tags: Vec::new(),
            expires: None,
            key: None,
            falsifier: Some("A force push to main turns out harmless.".to_string()),
            check: Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        }
    }

    /// A light rule with nothing to build a tooth from - the shape the trial
    /// will store as a note.
    fn plain_note(id: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: "Keep commit subjects under 60 characters.".to_string(),
            bindings: vec![Binding::Moment(Action::Commit)],
            severity: None,
            project: Some("demo".to_string()),
            tags: Vec::new(),
            expires: None,
            key: None,
            falsifier: Some("Long commit subjects turn out to read fine.".to_string()),
            check: None,
        }
    }

    fn trial_fields(item: &Item, verdict: RecordedVerdict) -> TrialFields {
        TrialFields {
            v: TRIAL_RECORD_VERSION,
            fingerprint: fingerprint(item),
            root: None,
            project: item.project.clone(),
            head_commit: None,
            candidates: gate::teeth_candidates(item),
            attempts: vec![
                TrialAttempt { form: "git push --force x".to_string(), via: "Bash".to_string(), outcome: AttemptOutcome::Refused },
                TrialAttempt { form: "git push".to_string(), via: "Bash".to_string(), outcome: AttemptOutcome::LetThrough },
            ],
            corpus: TrialCorpus { reached: 0, whole_hits: 0, cut: 0, stopped_forcing: Vec::new() },
            clean_failed: Vec::new(),
            disputes: TrialDisputes { legitimate_calls: Vec::new(), may_change: Vec::new() },
            accept_refusing: Vec::new(),
            history: Vec::new(),
            pending: None,
            a4: gate::unnamed_literals(item),
            verdict,
            evidence: Vec::new(),
            uncommitted: false,
            parent_rev: None,
            file_blob: None,
            a4_old_stock: None,
            unreached: None,
            matcher_rev: None,
        }
    }

    fn sealed(item: &Item, verdict: RecordedVerdict) -> TrialRecord {
        TrialRecord::seal(trial_fields(item, verdict))
    }

    fn declare_tried(store: &mut EventStore, item: &Item, trial: &TrialRecord) -> Event {
        declare_in_scoped(store, "s", "l", "a", item, None, None, Some(trial), None).unwrap()
    }

    /// Declare `item` (checkless) with a Note verdict from a trial record -
    /// settled, so it takes no place in a filing's batch.
    fn declare_noted(store: &mut EventStore, item: &Item) -> Event {
        declare_tried(store, item, &sealed(item, RecordedVerdict::Note))
    }

    fn undecided(why: &str) -> CurrentVerdict {
        CurrentVerdict::Undecided { why: why.to_string() }
    }

    /// Whether changing `item` with `change` changes its fingerprint.
    fn fingerprint_moves(change: impl Fn(&mut Item)) -> bool {
        let base = force_push_rule("fp");
        let mut other = base.clone();
        change(&mut other);
        assert_ne!(base, other, "the change must change the item");
        fingerprint(&base) != fingerprint(&other)
    }

    #[test]
    fn the_fingerprint_changes_with_the_kind() {
        assert!(fingerprint_moves(|i| i.kind = Kind::Orientation));
    }

    #[test]
    fn the_fingerprint_changes_with_the_text() {
        assert!(fingerprint_moves(|i| i.text = "Never run `git push --force` on the main branch.".to_string()));
    }

    #[test]
    fn the_fingerprint_changes_with_the_bindings() {
        assert!(fingerprint_moves(|i| i.bindings.push(Binding::Moment(Action::Push))));
        assert!(fingerprint_moves(|i| {
            i.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "git pull".to_string() }]
        }));
    }

    #[test]
    fn the_fingerprint_changes_with_the_check() {
        assert!(fingerprint_moves(|i| i.check = Some(Check::Forbidden { literals: vec!["--mirror".to_string()] })));
        assert!(fingerprint_moves(|i| i.check = None));
    }

    #[test]
    fn the_fingerprint_changes_with_the_project() {
        assert!(fingerprint_moves(|i| i.project = Some("other".to_string())));
        assert!(fingerprint_moves(|i| i.project = None));
    }

    #[test]
    fn the_fingerprint_changes_with_the_severity() {
        assert!(fingerprint_moves(|i| i.severity = Some(Severity::Irreversible)));
    }

    #[test]
    fn the_fingerprint_ignores_the_tags() {
        assert!(!fingerprint_moves(|i| i.tags = vec![TEETH_PROVEN_TAG.to_string(), "no-literal:a fixture reason".to_string()]));
    }

    #[test]
    fn the_fingerprint_ignores_the_falsifier() {
        assert!(!fingerprint_moves(|i| i.falsifier = Some("Another falsifier entirely.".to_string())));
    }

    #[test]
    fn the_fingerprint_ignores_the_expiry() {
        assert!(!fingerprint_moves(|i| i.expires = Some("2030-01-01".to_string())));
    }

    #[test]
    fn the_fingerprint_ignores_the_key() {
        assert!(!fingerprint_moves(|i| i.key = Some("force-push".to_string())));
    }

    #[test]
    fn the_fingerprint_reads_the_project_the_way_a_write_stores_it() {
        let mut global = force_push_rule("fp");
        global.project = None;
        let mut spelled = global.clone();
        spelled.project = Some("global".to_string());
        assert_eq!(fingerprint(&global), fingerprint(&spelled), "'global' is stored as no project at all");
    }

    #[test]
    fn a_trial_record_rides_in_the_event_body_and_reads_back_unchanged() {
        let mut store = EventStore::in_memory().unwrap();
        let item = force_push_rule("splice");
        let trial = sealed(&item, RecordedVerdict::RuleWired);
        let event = declare_tried(&mut store, &item, &trial);
        let canonical = canonical_body(&item).unwrap();
        let record = serde_json::to_string(&trial).unwrap();
        assert_eq!(event.body, format!("{},\"teeth_trial\":{record}}}", &canonical[..canonical.len() - 1]));
        assert!(record.starts_with("{\"v\":1,\"fingerprint\":\""), "the record keeps its field order: {record}");
        assert_eq!(parse_body(&event.body).unwrap(), item, "the item reads exactly as before");
        assert_eq!(show(&store, "splice").unwrap(), item);
        assert_eq!(trial_of(&event), Ok(Some(trial)));

        // Green: an event written without a record carries none.
        let plain = declare(&mut store, "s", "l", "a", &plain_note("no-record")).unwrap();
        assert_eq!(trial_of(&plain), Ok(None));
        assert!(!plain.body.contains("teeth_trial"));
    }

    #[test]
    fn the_trial_record_follows_because_in_the_body() {
        let mut store = EventStore::in_memory().unwrap();
        let item = force_push_rule("ordered");
        declare(&mut store, "s", "l", "a", &item).unwrap();
        let mut updated = item.clone();
        updated.check = Some(Check::Forbidden { literals: vec!["--force".to_string(), "-f".to_string()] });
        updated.text = "Never run `git push --force` or `git push -f` on main.".to_string();
        let trial = sealed(&updated, RecordedVerdict::RuleWired);
        let event = revise_tried_scoped(
            &mut store,
            "s",
            "l",
            "a",
            &item,
            &updated,
            &item,
            Some("the short flag does the same"),
            None,
            Some(&trial),
            None,
            None,
        )
        .unwrap();
        let because = event.body.find(",\"because\":").expect("because is on the body");
        let record = event.body.find(",\"teeth_trial\":").expect("the record is on the body");
        assert!(because < record, "because first, then the record: {}", event.body);
        assert!(event.body.ends_with(&format!(",\"teeth_trial\":{}}}", serde_json::to_string(&trial).unwrap())));
        assert_eq!(history(&store, "ordered").unwrap()[1].because.as_deref(), Some("the short flag does the same"));
        assert_eq!(trial_of(&event), Ok(Some(trial)));
        assert_eq!(show(&store, "ordered").unwrap(), updated);
    }

    /// An older binary reads a body through the very same `parse_body`: a
    /// plain `Item` deserialize with no `deny_unknown_fields`, so the extra
    /// key is skipped. Proven on the exact bytes a write produces.
    #[test]
    fn an_old_binary_parse_body_reads_a_body_carrying_teeth_trial_unchanged() {
        let item = force_push_rule("old-reader");
        let body = body_with_trial(&canonical_body(&item).unwrap(), &sealed(&item, RecordedVerdict::RuleWired)).unwrap();
        assert_eq!(parse_body(&body).unwrap(), item);
        let because = body_with_because(&item, "a reason").unwrap();
        let both = body_with_trial(&because, &sealed(&item, RecordedVerdict::Note)).unwrap();
        assert_eq!(parse_body(&both).unwrap(), item);
        assert_eq!(because_reason(&both).as_deref(), Some("a reason"));
    }

    #[test]
    fn a_record_this_build_cannot_read_is_unreadable_never_absent() {
        let mut store = EventStore::in_memory().unwrap();
        let item = force_push_rule("unreadable");
        let canonical = canonical_body(&item).unwrap();
        let open = &canonical[..canonical.len() - 1];
        let newer = store
            .append_event("s", "l", "a", EventKind::FactCreated, "v2", None, &format!("{open},\"teeth_trial\":{{\"v\":2}}}}"))
            .unwrap();
        let err = trial_of(&newer).unwrap_err();
        assert!(err.contains("is version 2, and this THOR reads version 1 only"), "{err}");
        let broken = store
            .append_event(
                "s",
                "l",
                "a",
                EventKind::FactCreated,
                "broken",
                None,
                &format!("{open},\"teeth_trial\":{{\"v\":1,\"fingerprint\":\"x\"}}}}"),
            )
            .unwrap();
        assert!(trial_of(&broken).unwrap_err().contains("does not read"));
        let prose = store.append_event("s", "l", "a", EventKind::FactCreated, "prose", None, "a 1.0 prose body").unwrap();
        assert_eq!(trial_of(&prose), Ok(None));
        assert_eq!(
            current_verdict(&store, "v2"),
            undecided(&format!(
                "its trial record at log seq {} is version 2, and this THOR reads version 1 only",
                newer.seq
            ))
        );
    }

    #[test]
    fn g6_2_a_record_counts_only_while_its_fingerprint_equals_the_heads() {
        let mut store = EventStore::in_memory().unwrap();
        let item = force_push_rule("cv");
        declare_tried(&mut store, &item, &sealed(&item, RecordedVerdict::RuleWired));
        assert_eq!(current_verdict(&store, "cv"), CurrentVerdict::RuleWired);

        // A text change written the way a bin writes it: no trial.
        let mut changed = item.clone();
        changed.text = "Never run `git push --force` on the main branch.".to_string();
        revise(&mut store, "s", "l", "a", &item, &changed).unwrap();
        assert_eq!(current_verdict(&store, "cv"), undecided("its text changed after its last trial"));

        // Back to exactly what was tried: the last trial matches again.
        revise(&mut store, "s", "l", "a", &changed, &item).unwrap();
        assert_eq!(current_verdict(&store, "cv"), CurrentVerdict::RuleWired);
    }

    #[test]
    fn g6_2_a_reason_tag_never_changes_the_verdict() {
        let mut store = EventStore::in_memory().unwrap();
        let item = force_push_rule("tagged");
        declare_tried(&mut store, &item, &sealed(&item, RecordedVerdict::RuleWired));
        let mut tagged = item.clone();
        tagged.tags = vec![TEETH_PROVEN_TAG.to_string(), "gotcha".to_string()];
        // Since step 8 the store refuses the label outright (K16)...
        let err = revise(&mut store, "s", "l", "a", &item, &tagged).unwrap_err();
        assert!(err.to_string().contains("which only THOR's own trial writes"), "{err}");
        // ...and a label an older binary wrote changes nothing either.
        legacy_revise(&mut store, &tagged);
        assert_eq!(current_verdict(&store, "tagged"), CurrentVerdict::RuleWired, "a proven label is only a label");

        let note = plain_note("tagged-note");
        declare_tried(&mut store, &note, &sealed(&note, RecordedVerdict::Note));
        let mut reasoned = note.clone();
        reasoned.tags = vec![format!("{NO_LITERAL_REASON_PREFIX}a commit subject leaves no fixed text"), TEETH_WIRED_TAG.to_string()];
        legacy_revise(&mut store, &reasoned);
        assert_eq!(current_verdict(&store, "tagged-note"), CurrentVerdict::Note);

        // Red: the same tags on an item never tried settle nothing.
        let mut store = EventStore::in_memory().unwrap();
        let mut never = force_push_rule("never-tried");
        never.tags = vec![TEETH_PROVEN_TAG.to_string()];
        legacy(&mut store, &never);
        assert_eq!(
            current_verdict(&store, "never-tried"),
            undecided("it has never been tried - there is no trial record on its log")
        );
    }
    #[test]
    fn the_verdict_names_every_field_that_changed_since_the_trial() {
        let cases: Vec<(&str, Box<dyn Fn(&mut Item)>, &str)> = vec![
            ("k", Box::new(|i: &mut Item| i.kind = Kind::Orientation), "its kind changed after its last trial"),
            (
                "b",
                Box::new(|i: &mut Item| i.bindings.push(Binding::Moment(Action::Push))),
                "its anchors changed after its last trial",
            ),
            ("p", Box::new(|i: &mut Item| i.project = Some("other".to_string())), "its project changed after its last trial"),
            (
                "cs",
                Box::new(|i: &mut Item| {
                    i.check = Some(Check::Forbidden { literals: vec!["--force".to_string(), "main".to_string()] });
                    i.severity = Some(Severity::Costly);
                }),
                "its check and severity changed after its last trial",
            ),
        ];
        for (id, change, why) in cases {
            let mut store = EventStore::in_memory().unwrap();
            let item = force_push_rule(id);
            declare_tried(&mut store, &item, &sealed(&item, RecordedVerdict::RuleWired));
            let mut changed = item.clone();
            change(&mut changed);
            // Raw, as an older binary could have written it: the write gate's
            // own view of the change is not what is under test here.
            let body = canonical_body(&changed).unwrap();
            store.append_mutate_checked("s", "l", "a", EventKind::FactRevised, id, None, &body).unwrap();
            assert_eq!(current_verdict(&store, id), undecided(why), "{id}");
        }
    }

    #[test]
    fn the_last_trial_decides_never_an_older_matching_one() {
        let mut store = EventStore::in_memory().unwrap();
        let first = force_push_rule("last");
        declare_tried(&mut store, &first, &sealed(&first, RecordedVerdict::RuleWired));
        let mut second = first.clone();
        second.text = "Never run `git push --force` on main or release.".to_string();
        let trial = sealed(&second, RecordedVerdict::RuleWired);
        revise_tried_scoped(&mut store, "s", "l", "a", &first, &second, &first, None, None, Some(&trial), None, None).unwrap();
        assert_eq!(current_verdict(&store, "last"), CurrentVerdict::RuleWired);
        // Back to the first text without a trial: the first record matches it,
        // but the LAST trial was of the second text.
        revise(&mut store, "s", "l", "a", &second, &first).unwrap();
        assert_eq!(current_verdict(&store, "last"), undecided("its text changed after its last trial"));
    }

    #[test]
    fn a_proven_verdict_needs_evidence() {
        let mut store = EventStore::in_memory().unwrap();
        let item = force_push_rule("proven");
        let mut fields = trial_fields(&item, RecordedVerdict::RuleProven);
        fields.evidence = vec![TrialEvidence::FieldRefusal { seq: 7 }];
        declare_tried(&mut store, &item, &TrialRecord::seal(fields));
        assert_eq!(current_verdict(&store, "proven"), CurrentVerdict::RuleProven);

        let mut store = EventStore::in_memory().unwrap();
        let bare = force_push_rule("proven-bare");
        declare_tried(&mut store, &bare, &sealed(&bare, RecordedVerdict::RuleProven));
        assert_eq!(current_verdict(&store, "proven-bare"), CurrentVerdict::RuleWired, "no evidence, no proof");
    }

    #[test]
    fn k23_an_a4_failure_keeps_the_verdict_undecided() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = force_push_rule("a4");
        item.check = Some(Check::Forbidden { literals: vec!["--force".to_string(), "zqxj-never-typed".to_string()] });
        let trial = sealed(&item, RecordedVerdict::RuleWired);
        assert_eq!(trial.fields().a4, vec!["zqxj-never-typed".to_string()]);
        // The gate refuses to write it (A4, step 8)...
        let err = declare_in_scoped(&mut store, "s", "l", "a", &item, None, None, Some(&trial), None).unwrap_err();
        assert!(err.to_string().contains("not named anywhere in the rule's own text"), "{err}");
        // ...and a record like it on the log (an older write) reads undecided.
        legacy_tried(&mut store, &item, &trial);
        assert_eq!(
            current_verdict(&store, "a4"),
            undecided(
                "A4: 'zqxj-never-typed' is not named in the text - revise the text to name it, or clear the check with a \
                 because"
            )
        );
        // Green: the same rule whose text names both.
        let mut named = item.clone();
        named.id = "a4-named".to_string();
        named.text = "Never run `git push --force` on main, and never zqxj-never-typed.".to_string();
        declare_tried(&mut store, &named, &sealed(&named, RecordedVerdict::RuleWired));
        assert_eq!(current_verdict(&store, "a4-named"), CurrentVerdict::RuleWired);
    }

    #[test]
    fn a_record_that_contradicts_its_item_is_undecided() {
        let mut store = EventStore::in_memory().unwrap();
        let checked = force_push_rule("note-with-check");
        declare_tried(&mut store, &checked, &sealed(&checked, RecordedVerdict::Note));
        assert_eq!(
            current_verdict(&store, "note-with-check"),
            undecided("its last trial made it a note, yet it carries a check - a note carries none")
        );
        let bare = plain_note("rule-without-check");
        declare_tried(&mut store, &bare, &sealed(&bare, RecordedVerdict::RuleWired));
        assert_eq!(
            current_verdict(&store, "rule-without-check"),
            undecided("its last trial put it on the rule track, yet it carries no check")
        );
        let mut store = EventStore::in_memory().unwrap();
        let note = plain_note("a-real-note");
        declare_tried(&mut store, &note, &sealed(&note, RecordedVerdict::Note));
        assert_eq!(current_verdict(&store, "a-real-note"), CurrentVerdict::Note);
    }

    #[test]
    fn no_item_a_retracted_or_diverged_one_and_a_report_are_undecided() {
        let mut store = EventStore::in_memory().unwrap();
        assert_eq!(current_verdict(&store, "nothing"), undecided("there is no item 'nothing'"));

        let gone = force_push_rule("gone");
        declare_tried(&mut store, &gone, &sealed(&gone, RecordedVerdict::RuleWired));
        retract(&mut store, "s", "l", "a", "gone", "a fixture retraction of the git push rule", None).unwrap();
        assert_eq!(current_verdict(&store, "gone"), undecided("it was retracted"));

        let split = force_push_rule("split");
        declare_tried(&mut store, &split, &sealed(&split, RecordedVerdict::RuleWired));
        let body = canonical_body(&split).unwrap();
        store.append_event("s", "l", "a", EventKind::FactRevised, "split", None, &body).unwrap();
        assert_eq!(current_verdict(&store, "split"), undecided("its log has 2 heads - resolve it first"));

        let mut report = plain_note("a-report");
        report.kind = Kind::Report;
        report.bindings = Vec::new();
        report.tags = vec!["a-report".to_string()];
        declare(&mut store, "s", "l", "a", &report).unwrap();
        assert_eq!(
            current_verdict(&store, "a-report"),
            undecided("a report is never tried - only a rule or an orientation carries a verdict")
        );
    }

    #[test]
    fn r6_2_a_revise_citing_a_head_that_moved_is_refused_and_appends_nothing() {
        let mut store = EventStore::in_memory().unwrap();
        let item = force_push_rule("cas");
        let first = declare(&mut store, "s", "l", "a", &item).unwrap();
        let mut moved = item.clone();
        moved.text = "Never run `git push --force` on main or release.".to_string();
        revise(&mut store, "s", "l", "a", &item, &moved).unwrap();
        let count = store.get_all_events().unwrap().len();

        let mut tried = moved.clone();
        tried.text = "Never run `git push --force` on any shared branch.".to_string();
        let trial = sealed(&tried, RecordedVerdict::RuleWired);
        let err = revise_tried_scoped(
            &mut store,
            "s",
            "l",
            "a",
            &moved,
            &tried,
            &moved,
            None,
            None,
            Some(&trial),
            Some(&first.this_hash),
            None,
        )
        .unwrap_err();
        assert!(
            matches!(&err, WriteError::Store(e) if e.to_string().contains("parent_rev is not a current head")),
            "{err}"
        );
        assert_eq!(store.get_all_events().unwrap().len(), count, "a refused write appends nothing");
        assert_eq!(show(&store, "cas").unwrap(), moved);
    }

    #[test]
    fn g6_5_a_revise_citing_the_current_head_lands() {
        let mut store = EventStore::in_memory().unwrap();
        let item = force_push_rule("cas-ok");
        let head = declare(&mut store, "s", "l", "a", &item).unwrap();
        let mut tried = item.clone();
        tried.text = "Never run `git push --force` on any shared branch.".to_string();
        let trial = sealed(&tried, RecordedVerdict::RuleWired);
        let event = revise_tried_scoped(
            &mut store,
            "s",
            "l",
            "a",
            &item,
            &tried,
            &item,
            None,
            None,
            Some(&trial),
            Some(&head.this_hash),
            None,
        )
        .unwrap();
        assert_eq!(event.parent_rev.as_deref(), Some(head.this_hash.as_str()));
        assert_eq!(show(&store, "cas-ok").unwrap(), tried);
        assert_eq!(current_verdict(&store, "cas-ok"), CurrentVerdict::RuleWired);
    }

    #[test]
    fn the_note_tag_is_a_teeth_tag() {
        assert_eq!(TEETH_NOTE_TAG, "teeth:note");
        assert!(TEETH_NOTE_TAG.starts_with(TEETH_TAG_PREFIX));
    }

    // ------------------------------------ the record's two fences (X6)

    /// The one module allowed to seal a new trial record: THOR's own trial.
    const TRIAL_RECORD_BUILDERS: &[&str] = &["serve/src/teeth.rs"];

    /// `text` with its test modules blanked and its comment lines dropped: a
    /// comment builds and reads nothing.
    fn production_lines(text: &str) -> Vec<String> {
        without_test_modules(text)
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .map(str::to_string)
            .collect()
    }

    /// Whether `line` builds a trial record, or opens a way to: a struct
    /// literal, `seal`, a trait impl for the type (a `From`, a `Default`), or
    /// a rename the needles would miss.
    fn builds_a_trial_record(line: &str) -> bool {
        let t = line.trim_start();
        let declares = t.starts_with("pub struct TrialRecord") || t.starts_with("impl TrialRecord");
        (!declares && line.contains("TrialRecord {"))
            || line.contains("TrialRecord::seal(")
            || line.contains("for TrialRecord")
            || line.contains("TrialRecord as ")
            || line.contains("= TrialRecord")
    }

    /// Every production file under `root` that builds a trial record outside
    /// the allowed module. Test modules and `tests/` files are allowed: a
    /// fixture needs a record.
    fn trial_record_builders(root: &Path) -> Vec<String> {
        member_files(root)
            .into_iter()
            .filter(|(rel, _)| !rel.contains("/tests/") && !TRIAL_RECORD_BUILDERS.contains(&rel.as_str()))
            .filter(|(_, text)| production_lines(text).iter().any(|line| builds_a_trial_record(line)))
            .map(|(rel, _)| rel)
            .collect()
    }

    /// The functions of every inherent `impl TrialRecord` block under `root`,
    /// each as `<file>: <fn>`. A new function there could build a record for
    /// anyone, so the list is fixed.
    fn trial_record_impl_fns(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for (rel, text) in member_files(root) {
            let lines = production_lines(&text);
            let mut inside = false;
            for line in &lines {
                if line.starts_with("impl TrialRecord") {
                    inside = true;
                    continue;
                }
                if inside && line == "}" {
                    inside = false;
                }
                if inside {
                    if let Some(rest) = line.trim_start().strip_prefix("pub fn ").or_else(|| line.trim_start().strip_prefix("fn ")) {
                        let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                        out.push(format!("{rel}: {name}"));
                    }
                }
            }
        }
        out
    }

    #[test]
    fn trial_record_is_built_only_in_teeth_rs() {
        let root = workspace_root();
        assert!(member_files(&root).len() > 50, "the workspace sources must be found under {}", root.display());
        assert_eq!(trial_record_builders(&root), Vec::<String>::new());
        assert_eq!(
            trial_record_impl_fns(&root),
            vec![
                "model/src/store.rs: seal".to_string(),
                "model/src/store.rs: fields".to_string(),
                "model/src/store.rs: from_log".to_string(),
            ]
        );

        // Red: every way in, planted in a fixture tree; the allowed places
        // are not named.
        let tmp = tempfile::tempdir().unwrap();
        let plant = |rel: &str, text: &str| {
            let path = tmp.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        plant("mcp/src/lib.rs", "fn f(fields: TrialFields) -> TrialRecord { TrialRecord { fields } }\n");
        plant("serve/src/bin/forge.rs", "fn f(x: TrialFields) { let r = model::store::TrialRecord::seal(x); }\n");
        // A trait impl with a where clause: only the `for TrialRecord` needle
        // sees its first line.
        plant(
            "ops/src/health.rs",
            "impl<T> From<T> for TrialRecord\nwhere\n    T: Into<TrialFields>,\n{\n    fn from(t: T) -> Self { todo!() }\n}\n",
        );
        plant("serve/src/rename.rs", "use model::store::TrialRecord as Box;\n");
        plant("serve/src/teeth.rs", "fn f(x: TrialFields) -> TrialRecord { TrialRecord::seal(x) }\n");
        plant("serve/tests/fixture.rs", "fn f(x: TrialFields) -> TrialRecord { TrialRecord::seal(x) }\n");
        plant(
            "serve/src/fixture_in_a_test_module.rs",
            "fn g() {}\n\n#[cfg(test)]\nmod tests {\n    fn f(x: TrialFields) { let _ = TrialRecord::seal(x); }\n}\n",
        );
        plant("serve/src/commented.rs", "// TrialRecord::seal(fields) is called in teeth.rs only\nfn g() {}\n");
        plant(
            "model/src/store.rs",
            "pub struct TrialRecord {\n    fields: TrialFields,\n}\n\nimpl TrialRecord {\n    pub fn seal(fields: TrialFields) -> Self {\n        Self { fields }\n    }\n\n    pub fn forge(fields: TrialFields) -> Self {\n        Self { fields }\n    }\n}\n",
        );
        assert_eq!(
            trial_record_builders(tmp.path()),
            vec![
                "mcp/src/lib.rs".to_string(),
                "ops/src/health.rs".to_string(),
                "serve/src/bin/forge.rs".to_string(),
                "serve/src/rename.rs".to_string(),
            ]
        );
        assert_eq!(
            trial_record_impl_fns(tmp.path()),
            vec!["model/src/store.rs: seal".to_string(), "model/src/store.rs: forge".to_string()]
        );
    }

    /// The functions of `model/src/store.rs` allowed to touch the record's
    /// key or read one back: its one writer and its one reader.
    const TRIAL_RECORD_KEY_FNS: &[&str] = &["fn body_with_trial(", "fn trial_of(", "fn from_log("];

    /// `lines` with the body of every function named in `fns` blanked, from
    /// its signature to the closing brace at the signature's own indent.
    fn without_fns(lines: Vec<String>, fns: &[&str]) -> Vec<String> {
        let mut out = Vec::with_capacity(lines.len());
        let mut closing: Option<String> = None;
        for line in lines {
            if let Some(end) = &closing {
                if &line == end {
                    closing = None;
                }
                out.push(String::new());
                continue;
            }
            if fns.iter().any(|f| line.contains(f)) {
                let indent: String = line.chars().take_while(|c| *c == ' ').collect();
                closing = Some(format!("{indent}}}"));
                out.push(String::new());
                continue;
            }
            out.push(line);
        }
        out
    }

    /// Whether `line` reads a trial record back, or the key that holds one.
    fn reads_a_trial_record(line: &str) -> bool {
        let names_the_type = line.contains("TrialRecord") || line.contains("TrialFields");
        let deserializes = ["from_str", "from_value", "from_slice", "from_reader", "deserialize"]
            .iter()
            .any(|d| line.contains(d));
        line.contains("teeth_trial") || line.contains("from_log(") || (names_the_type && deserializes)
    }

    /// Every production file under `root` that reads a trial record or its
    /// key anywhere but `trial_of` (and the one writer of the key,
    /// `body_with_trial`).
    fn trial_record_readers(root: &Path) -> Vec<String> {
        member_files(root)
            .into_iter()
            .filter(|(rel, _)| !rel.contains("/tests/"))
            .filter_map(|(rel, text)| {
                let mut lines = production_lines(&text);
                if rel == "model/src/store.rs" {
                    lines = without_fns(lines, TRIAL_RECORD_KEY_FNS);
                }
                lines.iter().any(|line| reads_a_trial_record(line)).then_some(rel)
            })
            .collect()
    }

    #[test]
    fn trial_record_is_deserialized_only_in_trial_of() {
        let root = workspace_root();
        assert!(member_files(&root).len() > 50, "the workspace sources must be found under {}", root.display());
        assert_eq!(trial_record_readers(&root), Vec::<String>::new());
        let store_rs = std::fs::read_to_string(root.join("model/src/store.rs")).unwrap();
        let production = production_lines(&store_rs);
        for f in TRIAL_RECORD_KEY_FNS {
            assert_eq!(production.iter().filter(|l| l.contains(f)).count(), 1, "{f} is defined once in store.rs");
        }

        // Red: every way in, planted in a fixture tree.
        let tmp = tempfile::tempdir().unwrap();
        let plant = |rel: &str, text: &str| {
            let path = tmp.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        plant("serve/src/lib.rs", "fn f(s: &str) { let _ = serde_json::from_str::<TrialRecord>(s); }\n");
        plant("ops/src/health.rs", "fn f(v: &serde_json::Value) -> bool { v.get(\"teeth_trial\").is_some() }\n");
        plant("mcp/src/lib.rs", "fn f(v: serde_json::Value) { let r: TrialFields = serde_json::from_value(v).unwrap(); }\n");
        plant("serve/src/body.rs", "#[derive(Deserialize)]\nstruct Body {\n    teeth_trial: TrialFields,\n}\n");
        plant("serve/tests/fixture.rs", "fn f(s: &str) { let _ = serde_json::from_str::<TrialFields>(s); }\n");
        plant(
            "model/src/store.rs",
            "fn body_with_trial(body: &str) -> String {\n    format!(\"{body},\\\"teeth_trial\\\":1\")\n}\n\n\
             pub fn trial_of(v: &serde_json::Value) -> Option<TrialRecord> {\n    let raw = v.get(\"teeth_trial\")?;\n    \
             Some(TrialRecord::from_log(TrialFields::deserialize(raw).ok()?))\n}\n\n\
             pub fn side_door(v: &serde_json::Value) -> bool {\n    v.get(\"teeth_trial\").is_some()\n}\n",
        );
        assert_eq!(
            trial_record_readers(tmp.path()),
            vec![
                "mcp/src/lib.rs".to_string(),
                "model/src/store.rs".to_string(),
                "ops/src/health.rs".to_string(),
                "serve/src/body.rs".to_string(),
                "serve/src/lib.rs".to_string(),
            ]
        );
    }

    // ------------------------------------------ the grounds switched on in step 8

    fn head_tags(store: &EventStore, id: &str) -> Vec<String> {
        show(store, id).unwrap().tags
    }

    /// STRIP-ON-CHANGE: a revise with no trial record that changes one field
    /// of the fingerprint drops every `teeth:` label from the new body - one
    /// case per field - and one that changes only tags, falsifier, expires or
    /// key keeps it.
    #[test]
    fn a_revise_without_a_record_that_changes_the_fingerprint_drops_every_teeth_label() {
        let changes: Vec<(&str, Box<dyn Fn(&mut Item)>)> = vec![
            ("kind", Box::new(|i: &mut Item| i.kind = Kind::Orientation)),
            ("text", Box::new(|i: &mut Item| i.text = "Never run `git push --force` on main, ever.".to_string())),
            ("bindings", Box::new(|i: &mut Item| i.bindings.push(Binding::Always))),
            ("check", Box::new(|i: &mut Item| i.check = Some(Check::Forbidden { literals: vec!["--force".to_string(), "push".to_string()] }))),
            ("project", Box::new(|i: &mut Item| i.project = Some("other".to_string()))),
            ("severity", Box::new(|i: &mut Item| i.severity = Some(Severity::Costly))),
        ];
        for (field, change) in changes {
            let mut store = EventStore::in_memory().unwrap();
            let mut item = force_push_rule(&format!("strip-{field}"));
            item.tags = vec![TEETH_WIRED_TAG.to_string(), "gotcha".to_string()];
            let record = sealed(&item, RecordedVerdict::RuleWired);
            declare_tried(&mut store, &item, &record);
            let mut updated = item.clone();
            change(&mut updated);
            revise(&mut store, "s", "l", "a", &item, &updated).unwrap_or_else(|e| panic!("{field}: {e}"));
            assert_eq!(head_tags(&store, &item.id), vec!["gotcha".to_string()], "{field}: the stale label is gone");
            assert!(matches!(current_verdict(&store, &item.id), CurrentVerdict::Undecided { .. }), "{field}");
        }
        let keeps: Vec<(&str, Box<dyn Fn(&mut Item)>)> = vec![
            ("tags", Box::new(|i: &mut Item| i.tags.push("more".to_string()))),
            ("falsifier", Box::new(|i: &mut Item| i.falsifier = Some("A force push to main turns out harmless, twice.".to_string()))),
            ("key", Box::new(|i: &mut Item| i.key = Some("k".to_string()))),
        ];
        for (field, change) in keeps {
            let mut store = EventStore::in_memory().unwrap();
            let mut item = force_push_rule(&format!("keep-{field}"));
            item.tags = vec![TEETH_WIRED_TAG.to_string()];
            declare_tried(&mut store, &item, &sealed(&item, RecordedVerdict::RuleWired));
            let mut updated = item.clone();
            change(&mut updated);
            revise(&mut store, "s", "l", "a", &item, &updated).unwrap_or_else(|e| panic!("{field}: {e}"));
            assert!(head_tags(&store, &item.id).contains(&TEETH_WIRED_TAG.to_string()), "{field}: kept");
            assert_eq!(current_verdict(&store, &item.id), CurrentVerdict::RuleWired, "{field}");
        }
    }

    /// R8.21: a text change written by a bin through `revise` - on a note, or
    /// on a wired rule - leaves the item undecided with no `teeth:` label.
    #[test]
    fn r8_21_a_bin_text_change_leaves_a_note_or_a_wired_rule_undecided_and_unlabelled() {
        let mut store = EventStore::in_memory().unwrap();
        let mut note = plain_note("bin-note");
        note.tags = vec![TEETH_NOTE_TAG.to_string()];
        declare_tried(&mut store, &note, &sealed(&note, RecordedVerdict::Note));
        let mut rewritten = note.clone();
        rewritten.text = format!("{} Also on tags.", note.text);
        revise(&mut store, "bin", "bin", "repair-project", &note, &rewritten).unwrap();
        assert!(matches!(current_verdict(&store, "bin-note"), CurrentVerdict::Undecided { .. }));
        assert!(!head_tags(&store, "bin-note").iter().any(|t| t.starts_with(TEETH_TAG_PREFIX)));

        let mut wired = force_push_rule("bin-wired");
        wired.tags = vec![TEETH_WIRED_TAG.to_string()];
        declare_tried(&mut store, &wired, &sealed(&wired, RecordedVerdict::RuleWired));
        let mut rewritten = wired.clone();
        rewritten.text = "Never run `git push --force` on main or on a release branch.".to_string();
        revise(&mut store, "bin", "bin", "restore-attribution", &wired, &rewritten).unwrap();
        assert_eq!(
            current_verdict(&store, "bin-wired"),
            undecided("its text changed after its last trial")
        );
        assert!(!head_tags(&store, "bin-wired").iter().any(|t| t.starts_with(TEETH_TAG_PREFIX)));
    }

    /// R8.20: a direct `revise` (as a bin would do it) that keeps `teeth:note`
    /// and adds a forbidden check is refused with the note-track sentence.
    #[test]
    fn r8_20_a_bin_hanging_a_check_on_a_note_is_refused() {
        let mut store = EventStore::in_memory().unwrap();
        let mut note = force_push_rule("bin-hang");
        note.check = None;
        note.tags = vec![TEETH_NOTE_TAG.to_string()];
        declare_tried(&mut store, &note, &sealed(&note, RecordedVerdict::Note));
        let mut hung = note.clone();
        hung.check = Some(Check::Forbidden { literals: vec!["--force".to_string()] });
        let err = revise(&mut store, "bin", "bin", "repair-project", &note, &hung).unwrap_err();
        assert!(err.to_string().contains("an item on the note track carries no check"), "{err}");
        assert!(err.to_string().contains("write it through remember/revise so THOR can try the new one"), "{err}");
        assert!(show(&store, "bin-hang").unwrap().check.is_none(), "nothing written");
    }

    /// K16 and R8.22: a `teeth:` label no record implies is refused on
    /// declare and on revise; the record that implies it lets it in; a
    /// label the head already carries is not new; removing one is allowed.
    #[test]
    fn k16_a_teeth_label_needs_the_record_that_implies_it() {
        let mut store = EventStore::in_memory().unwrap();
        let mut forged = force_push_rule("forged");
        forged.tags = vec![TEETH_PROVEN_TAG.to_string()];
        let before = store.get_all_events().unwrap().len();
        let err = declare(&mut store, "s", "l", "model", &forged).unwrap_err();
        assert!(err.to_string().contains("'teeth:proven-guard' starts with 'teeth:', which only THOR's own trial writes"), "{err}");
        assert_eq!(store.get_all_events().unwrap().len(), before, "nothing written");
        for retired in [TEETH_INFORMING_TAG, TEETH_NOT_TRIED_TAG, TEETH_REFUSES_LEGITIMATE_TAG, TEETH_ANCHOR_COLLAPSED_TAG, "teeth:hard"] {
            forged.tags = vec![retired.to_string()];
            assert!(declare(&mut store, "s", "l", "model", &forged).is_err(), "{retired} is never written again");
        }

        // The record implies wired: wired goes in, proven does not.
        let mut wired = force_push_rule("wired-by-record");
        wired.tags = vec![TEETH_PROVEN_TAG.to_string()];
        let record = sealed(&wired, RecordedVerdict::RuleWired);
        assert!(declare_in_scoped(&mut store, "s", "l", "a", &wired, None, None, Some(&record), None).is_err(), "the record does not imply proven");
        wired.tags = vec![TEETH_WIRED_TAG.to_string()];
        declare_tried(&mut store, &wired, &sealed(&wired, RecordedVerdict::RuleWired));

        // Carried: not new. Differing: refused. Removed: allowed.
        let mut carried = wired.clone();
        carried.tags.push("gotcha".to_string());
        revise(&mut store, "s", "l", "a", &wired, &carried).expect("a label the head carries is not new");
        let mut differing = carried.clone();
        differing.tags = vec![TEETH_PROVEN_TAG.to_string(), "gotcha".to_string()];
        assert!(revise(&mut store, "s", "l", "a", &carried, &differing).unwrap_err().to_string().contains("only THOR's own trial writes"));
        let mut removed = carried.clone();
        removed.tags = vec!["gotcha".to_string()];
        revise(&mut store, "s", "l", "a", &carried, &removed).expect("removing a label is always allowed");
    }

    /// R8.25 (D16): revising a wired rule into a Report is taken off the rule
    /// track - refused without a because, landing with one.
    #[test]
    fn r8_25_moving_a_wired_rule_to_a_report_needs_a_because() {
        let mut store = EventStore::in_memory().unwrap();
        let mut rule = force_push_rule("to-report");
        rule.check = None;
        rule.tags = vec![TEETH_NOTE_TAG.to_string()];
        declare_tried(&mut store, &rule, &sealed(&rule, RecordedVerdict::Note));
        let mut report = rule.clone();
        report.kind = Kind::Report;
        report.bindings = vec![];
        report.tags = vec!["archived".to_string()];
        let baseline = rule.clone();
        let err = revise_because(&mut store, "s", "l", "a", &Item { bindings: vec![], tags: vec![], ..baseline.clone() }, &report, &rule, None)
            .unwrap_err();
        assert!(err.to_string().contains("takes the rule off the rule track exactly like a retract"), "{err}");
        revise_because(
            &mut store,
            "s",
            "l",
            "a",
            &Item { bindings: vec![], tags: vec![], ..baseline },
            &report,
            &rule,
            Some("the release branch this guarded is gone"),
        )
        .expect("with a because it lands");
        assert_eq!(show(&store, "to-report").unwrap().kind, Kind::Report);
    }

    /// R8.17 at the store: a refused write is one `GateRefused` by
    /// `teeth-write` on the id - head-neutral, the item does not exist, and
    /// only THOR's `teeth-write` actor may record one.
    #[test]
    fn a_refused_write_is_recorded_once_and_never_writes_the_item() {
        let mut store = EventStore::in_memory().unwrap();
        record_refused_write(&mut store, "s", SystemActor::new(TEETH_WRITE_ACTOR), "never-stored", "forcing", &["forbidden '--force'".to_string()], "the text")
            .unwrap();
        let events = store.get_events_by_entity("never-stored").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::GateRefused);
        assert_eq!(events[0].actor, TEETH_WRITE_ACTOR);
        let body: serde_json::Value = serde_json::from_str(&events[0].body).unwrap();
        assert_eq!(body["code"], "forcing");
        assert_eq!(body["id"], "never-stored");
        assert!(body["text_sha"].as_str().is_some_and(|h| !h.is_empty() && !h.contains("the text")));
        assert!(show(&store, "never-stored").is_err(), "the item does not exist");
        assert!(record_refused_write(&mut store, "s", SystemActor::new(ATTESTER_ACTOR), "x", "a4", &[], "t").is_err());
        // Stored later: its history has one fact write, the refusal is no head.
        let item = force_push_rule("never-stored");
        declare_tried(&mut store, &item, &sealed(&item, RecordedVerdict::RuleWired));
        let heads = compute_head_sets(&store.get_events_by_entity("never-stored").unwrap());
        assert_eq!(heads["never-stored"].heads.len(), 1);
        assert_eq!(current_verdict(&store, "never-stored"), CurrentVerdict::RuleWired);
    }

    /// No write path emits the four retired labels any more: in production
    /// code they are only ever read (compared), never pushed onto tags.
    #[test]
    fn no_write_path_emits_a_retired_teeth_label() {
        let root = workspace_root();
        let retired = [
            "TEETH_INFORMING_TAG",
            "TEETH_NOT_TRIED_TAG",
            "TEETH_REFUSES_LEGITIMATE_TAG",
            "TEETH_ANCHOR_COLLAPSED_TAG",
            "\"teeth:informing\"",
            "\"teeth:not-tried\"",
            "\"teeth:refuses-legitimate-use\"",
            "\"teeth:anchor-collapsed\"",
        ];
        let writes = [".push(", "vec![", ".insert(", ".extend(", "tags ="];
        let mut found = Vec::new();
        for (rel, text) in member_files(&root) {
            if rel.contains("/tests/") || rel.contains("/examples/") {
                continue;
            }
            for (n, line) in without_test_modules(&text).lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                if retired.iter().any(|r| code.contains(r)) && writes.iter().any(|w| code.contains(w)) {
                    found.push(format!("{rel}:{}", n + 1));
                }
            }
        }
        assert_eq!(found, Vec::<String>::new(), "a retired label is written here");
    }

    /// No code reads the log's old `"check":{` needle (the tried-set that let
    /// a reason count after a "real try"): a reason settles nothing now.
    #[test]
    fn no_code_reads_the_old_check_needle() {
        let root = workspace_root();
        let needles = ["\"\\\"check\\\":{\"", "ids_with_a_write_containing", "ids_whose_check_was_tried"];
        let mut found = Vec::new();
        for (rel, text) in member_files(&root) {
            if rel.contains("/tests/") || rel.contains("/examples/") {
                continue;
            }
            for (n, line) in without_test_modules(&text).lines().enumerate() {
                if needles.iter().any(|needle| line.contains(needle)) {
                    found.push(format!("{rel}:{}", n + 1));
                }
            }
        }
        assert_eq!(found, Vec::<String>::new());
    }

    // ------------------------------------------ old contains stock (Q24)

    /// A rule bound to NOTES.md whose contains check keeps 'keep me' there,
    /// while its text never names the literal - the old stock Q24 is about.
    fn old_contains_rule(id: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: "NOTES.md keeps its opening line.".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: "NOTES.md".to_string() }],
            severity: None,
            project: Some("demo".to_string()),
            tags: Vec::new(),
            expires: None,
            key: None,
            falsifier: Some("The opening line turns out not to matter.".to_string()),
            check: Some(Check::Contains { path: "NOTES.md".to_string(), literal: "keep me".to_string() }),
        }
    }

    /// `item` written straight into the log, the way an older build wrote it:
    /// a created event first, a revise after that.
    fn raw_write(store: &mut EventStore, item: &Item) -> Event {
        let body = canonical_body(item).unwrap();
        if store.get_events_by_entity(&item.id).unwrap().is_empty() {
            store.append_event("legacy", &item.id, "migration", EventKind::FactCreated, &item.id, None, &body).unwrap()
        } else {
            store.append_mutate_checked("legacy", &item.id, "migration", EventKind::FactRevised, &item.id, None, &body).unwrap()
        }
    }

    fn retest_actor() -> SystemActor {
        SystemActor::new(RETEST_ACTOR)
    }

    /// THE CUTOFF NEVER MOVES (Q24, 2): recorded once by THOR's retest; a
    /// second call, a later raw record on the same id, or a record claiming a
    /// point after its own place in the log changes nothing. If any of these
    /// could move it, this test fails.
    #[test]
    fn the_a4_cutoff_is_recorded_once_and_never_moves() {
        let mut store = EventStore::in_memory().unwrap();
        assert_eq!(a4_cutoff(&store), None, "nothing is recorded until the retest records it");
        raw_write(&mut store, &old_contains_rule("a"));
        raw_write(&mut store, &old_contains_rule("b"));
        let last = prospective_a4_cutoff(&store);
        assert_eq!(last, 2);
        assert_eq!(record_a4_cutoff(&mut store, retest_actor(), last).unwrap(), 2);
        assert_eq!(a4_cutoff(&store), Some(2));

        // Later writes, a second recording, and a raw later record on the id.
        raw_write(&mut store, &old_contains_rule("c"));
        let now = prospective_a4_cutoff(&store);
        assert_eq!(record_a4_cutoff(&mut store, retest_actor(), now).unwrap(), 2);
        let later = serde_json::json!({ "cutoff_seq": 99, "why": "moved" }).to_string();
        store.append_event("x", "x", RETEST_ACTOR, EventKind::GateStoodAside, A4_CUTOFF_ID, None, &later).unwrap();
        assert_eq!(a4_cutoff(&store), Some(2), "only the first record is ever read");
        assert_eq!(store.get_events_by_entity(A4_CUTOFF_ID).unwrap().len(), 2, "the second call wrote nothing");

        // A first record can never claim a point after its own place.
        let mut other = EventStore::in_memory().unwrap();
        raw_write(&mut other, &old_contains_rule("a"));
        let future = serde_json::json!({ "cutoff_seq": 50 }).to_string();
        let at = other.append_event("x", "x", RETEST_ACTOR, EventKind::GateStoodAside, A4_CUTOFF_ID, None, &future).unwrap();
        assert_eq!(a4_cutoff(&other), Some(at.seq - 1));
    }

    /// Only THOR's retest records the cutoff or writes on its id; a cutoff
    /// outside the log is refused.
    #[test]
    fn only_the_retest_records_the_a4_cutoff() {
        let mut store = EventStore::in_memory().unwrap();
        raw_write(&mut store, &old_contains_rule("a"));
        let err = record_a4_cutoff(&mut store, SystemActor::new(ATTESTER_ACTOR), 1).unwrap_err();
        assert!(err.to_string().contains("recorded by THOR's retest only"), "{err}");
        let err = record_a4_cutoff(&mut store, retest_actor(), 5).unwrap_err();
        assert!(err.to_string().contains("lies outside the log"), "{err}");
        let mut squatter = plain_note(A4_CUTOFF_ID);
        squatter.project = Some("demo".to_string());
        let err = declare(&mut store, "s", "l", "mcp", &squatter).unwrap_err();
        assert!(err.to_string().contains("THOR's own record of the A4 cutoff"), "{err}");
        assert_eq!(a4_cutoff(&store), None);
        assert_eq!(store.get_events_by_entity(A4_CUTOFF_ID).unwrap().len(), 0, "nothing written");
    }

    /// OLD STOCK is read from the whole log: the check stood at the cutoff,
    /// with its text and anchors, and nothing changed any of the three since.
    #[test]
    fn old_contains_stock_reads_the_whole_log() {
        let stock = |store: &EventStore, id: &str, cutoff: i64| {
            old_contains_stock(&store.get_events_by_entity(id).unwrap(), cutoff)
        };
        let mut store = EventStore::in_memory().unwrap();
        let rule = old_contains_rule("old");
        let set = raw_write(&mut store, &rule);
        let mut tagged = rule.clone();
        tagged.tags.push("gotcha".to_string());
        raw_write(&mut store, &tagged);
        let cutoff = prospective_a4_cutoff(&store);
        let mut tagged_after = tagged.clone();
        tagged_after.falsifier = Some("A reworded falsifier changes nothing tried.".to_string());
        raw_write(&mut store, &tagged_after);
        let got = stock(&store, "old", cutoff).expect("set before the cutoff and untouched since");
        assert_eq!((got.literal.as_str(), got.path.as_str(), got.set_at, got.cutoff), ("keep me", "NOTES.md", set.seq, cutoff));

        // The text changed after the cutoff: gone - and putting it back does
        // not bring it back.
        let mut reworded = tagged_after.clone();
        reworded.text = "NOTES.md keeps its first line.".to_string();
        raw_write(&mut store, &reworded);
        assert_eq!(stock(&store, "old", cutoff), None);
        raw_write(&mut store, &tagged_after);
        assert_eq!(stock(&store, "old", cutoff), None, "a revise that puts the text back does not undo the change");

        // Anchors and check alike.
        let mut store = EventStore::in_memory().unwrap();
        raw_write(&mut store, &rule);
        let cutoff = prospective_a4_cutoff(&store);
        let mut rebound = rule.clone();
        rebound.bindings.push(Binding::Always);
        raw_write(&mut store, &rebound);
        assert_eq!(stock(&store, "old", cutoff), None, "a pin changes the anchors");
        let mut store = EventStore::in_memory().unwrap();
        raw_write(&mut store, &rule);
        let cutoff = prospective_a4_cutoff(&store);
        let mut moved = rule.clone();
        moved.check = Some(Check::Contains { path: "NOTES.md".to_string(), literal: "keep me too".to_string() });
        raw_write(&mut store, &moved);
        assert_eq!(stock(&store, "old", cutoff), None);

        // A check attached after the cutoff, and a rule written after it.
        let mut store = EventStore::in_memory().unwrap();
        let mut bare = rule.clone();
        bare.check = None;
        raw_write(&mut store, &bare);
        let cutoff = prospective_a4_cutoff(&store);
        raw_write(&mut store, &rule);
        assert_eq!(stock(&store, "old", cutoff), None, "a check attached after the cutoff is never old stock");
        raw_write(&mut store, &old_contains_rule("young"));
        assert_eq!(stock(&store, "young", cutoff), None);

        // Retracted after the cutoff.
        let mut store = EventStore::in_memory().unwrap();
        raw_write(&mut store, &rule);
        let cutoff = prospective_a4_cutoff(&store);
        retract(&mut store, "s", "l", "a", "old", "gone", None).unwrap();
        assert_eq!(stock(&store, "old", cutoff), None);
    }

    /// No other check kind is ever old stock.
    #[test]
    fn no_other_check_kind_is_old_contains_stock() {
        let checks = [
            Check::Forbidden { literals: vec!["--mirr0r-zz".to_string()] },
            Check::Absent { path: "NOTES.md".to_string(), literal: "drop me".to_string() },
            Check::AbsentAll { path: "NOTES.md".to_string(), literals: vec!["drop me".to_string()] },
            Check::PathExists { path: "NOTES.md".to_string() },
            Check::Requires { when: "git tag".to_string(), required: vec!["-s".to_string()] },
        ];
        for check in checks {
            let mut store = EventStore::in_memory().unwrap();
            let mut rule = old_contains_rule("old");
            rule.check = Some(check.clone());
            raw_write(&mut store, &rule);
            let cutoff = prospective_a4_cutoff(&store);
            assert_eq!(old_contains_stock(&store.get_events_by_entity("old").unwrap(), cutoff), None, "{check:?}");
        }
    }

    /// `rule`'s record as THOR's retest writes it for old stock, sealed.
    fn old_stock_record(rule: &Item, mark: OldContainsStock) -> TrialRecord {
        let mut fields = trial_fields(rule, RecordedVerdict::RuleWired);
        fields.a4_old_stock = Some(mark);
        TrialRecord::seal(fields)
    }

    /// THE MARK IS THOR'S (Q24, 3): a record carrying it is refused from a
    /// declare, and from every writer of a revise but THOR's retest - nothing
    /// is written. Only the retest's own write lands.
    #[test]
    fn a_record_carrying_the_old_stock_mark_is_refused_from_every_writer_but_the_retest() {
        let mut store = EventStore::in_memory().unwrap();
        let rule = old_contains_rule("old");
        raw_write(&mut store, &rule);
        let cutoff = prospective_a4_cutoff(&store);
        let mark = old_contains_stock(&store.get_events_by_entity("old").unwrap(), cutoff).unwrap();
        let record = old_stock_record(&rule, mark.clone());
        let before = store.get_all_events().unwrap().len();

        // A new rule is never old stock, even one whose text names its literal.
        let mut named = old_contains_rule("new");
        named.text = "NOTES.md keeps `keep me`.".to_string();
        let err = declare_in_scoped(&mut store, "s", "l", "mcp", &named, None, None, Some(&record), None).unwrap_err();
        assert!(err.to_string().contains("only THOR's retest writes that mark"), "{err}");
        for actor in [WriteActor::Named("mcp"), WriteActor::System(SystemActor::new("teeth-eval"))] {
            let err = revise_tried_scoped(&mut store, "s", "l", actor, &rule, &rule, &rule, None, None, Some(&record), None, None).unwrap_err();
            assert!(err.to_string().contains("only THOR's retest writes that mark"), "{err}");
        }
        assert_eq!(store.get_all_events().unwrap().len(), before, "nothing written");

        record_a4_cutoff(&mut store, retest_actor(), cutoff).unwrap();
        revise_tried_scoped(&mut store, "retest", "old", retest_actor(), &rule, &rule, &rule, None, None, Some(&record), None, None)
            .expect("THOR's retest writes its own mark");
        assert_eq!(current_verdict(&store, "old"), CurrentVerdict::RuleWired);
        let log = history(&store, "old").unwrap();
        assert_eq!(log.last().unwrap().a4_old_stock.as_ref(), Some(&mark), "the step shows the fragment it guards");
    }

    /// THE MARK COUNTS ONLY WHILE THE LOG BEARS IT OUT (Q24, 4): a tags-only
    /// revise keeps it; a revise of the text ends it for good, even one that
    /// carries no record and a later one that puts the text back; and with no
    /// recorded cutoff it never counts.
    #[test]
    fn the_old_stock_mark_counts_only_while_the_log_bears_it_out() {
        let mut store = EventStore::in_memory().unwrap();
        let rule = old_contains_rule("old");
        raw_write(&mut store, &rule);
        let cutoff = prospective_a4_cutoff(&store);
        let mark = old_contains_stock(&store.get_events_by_entity("old").unwrap(), cutoff).unwrap();
        let record = old_stock_record(&rule, mark);

        // No cutoff recorded: the mark alone counts for nothing.
        let mut unrecorded = EventStore::in_memory().unwrap();
        raw_write(&mut unrecorded, &rule);
        let body = body_with_trial(&canonical_body(&rule).unwrap(), &record).unwrap();
        unrecorded.append_mutate_checked("retest", "old", RETEST_ACTOR, EventKind::FactRevised, "old", None, &body).unwrap();
        assert!(matches!(current_verdict(&unrecorded, "old"), CurrentVerdict::Undecided { why } if why.starts_with("A4: 'keep me'")));

        record_a4_cutoff(&mut store, retest_actor(), cutoff).unwrap();
        revise_tried_scoped(&mut store, "retest", "old", retest_actor(), &rule, &rule, &rule, None, None, Some(&record), None, None).unwrap();
        assert_eq!(current_verdict(&store, "old"), CurrentVerdict::RuleWired);

        let mut tagged = rule.clone();
        tagged.tags.push("gotcha".to_string());
        revise(&mut store, "s", "l", "a", &rule, &tagged).unwrap();
        assert_eq!(current_verdict(&store, "old"), CurrentVerdict::RuleWired, "a tags-only revise keeps it");

        let mut reworded = tagged.clone();
        reworded.text = "NOTES.md keeps its first line.".to_string();
        raw_write(&mut store, &reworded);
        assert!(matches!(current_verdict(&store, "old"), CurrentVerdict::Undecided { .. }));
        raw_write(&mut store, &tagged);
        assert!(
            matches!(current_verdict(&store, "old"), CurrentVerdict::Undecided { why } if why.starts_with("A4: 'keep me'")),
            "the old record matches the text again, and still counts for nothing"
        );
    }
}
