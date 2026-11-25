//! Serving history, and the hypothesis it falsified.
//!
//! THE HYPOTHESIS - "decay by silence". An item served repeatedly and never
//! once marked useful (`crate::mark::record_useful`) stops reaching an
//! injection surface. Never deleted, never hidden from `lookup::search`; only
//! the three injection paths stop showing it. A single mark cancels it
//! permanently. The reasoning was that an item nobody ever engages with
//! should not linger, and the threshold was documented from the start as NOT
//! MEASURED, with "revisit once real serving numbers exist".
//!
//! THE MEASUREMENT, on the first real day of use (2026-08-03). Real numbers
//! now exist, and they falsify it outright:
//!
//! - Marks of usefulness recorded in a full day of real work: ZERO. Not few.
//!   None. Nothing in any surface asks for one, and an agent with work to do
//!   does not stop to press a button. So "never marked useful" is not a
//!   signal about the item - it is a constant.
//! - With that constant, the rule degenerates into an unconditional timer:
//!   every item that fires often enough disappears, whatever it says.
//! - It therefore retires the MOST relevant items first, because firing often
//!   is what relevance looks like. Backwards by construction.
//! - Observed after ONE day: four rules had already gone silent. All four
//!   bound to the `prod_data` moment, three of severity `costly`, one
//!   `irreversible`. Rules about not testing a write path against the real
//!   store, and about not running a reseed inside the live container. The
//!   mechanism's first act in production was to remove the owner's data-loss
//!   protection, silently, on the day he started using it.
//!
//! WHAT CHANGED. Silence no longer removes anything from an injection
//! surface. The counters stay, and `status`/`doctor` report "served N times,
//! never marked" as something a person can look at - observability without a
//! silent delete (CONTRACT R3 asks for delivery to be observable; it never
//! asked for unobserved usefulness to mean removal).
//!
//! WHY NOT JUST RAISE THE THRESHOLD, OR ADD A NUDGE. Both are compensating
//! mechanisms for a modelling error, which CONTRACT R9 says must be reported
//! rather than quietly added. A higher threshold delays the same silent
//! deletion; a nudge asking agents to mark things is exactly the kind of
//! behaviour-dependent convention R7 forbids relying on, and the one
//! comparable measurement anyone has (0 of 20 model compliance on a similar
//! ask) says it would not work either.
//!
//! WHAT REPLACED IT, the same day. An explicit NEGATIVE signal - the reader
//! saying "this did not belong here" - is a real observation, where the
//! absence of a positive one is not. And the owner had already produced 182
//! of them: 1.0 kept a `noise` ledger, which the migration left behind, and
//! 455 fireable 2.0 items descend from a fact he had explicitly called noise,
//! one of them nine times. So decay now reads THAT, and only that.
//!
//! WHAT DECAY IS NOW. An item called noise at least `NOISE_MARKS_BEFORE_STALE`
//! times SINCE the last time anyone called it useful stops reaching an
//! injection surface. Never deleted, never hidden from `lookup::search`, and
//! a mark of usefulness still clears every noise mark recorded before it. A
//! serving count decides nothing.
//!
//! TWO LIMITS ADDED LATER (found on a live rule, 2026-10-09). A noise mark
//! judged the version served when it was given, so a revision that changes the
//! rule's text, check or bindings clears the marks before it
//! (`usefulness::noise_since_last_change`). And a rule whose check can refuse
//! (`sweep::can_refuse`) is never retired from showing: the write guard
//! refuses with it regardless, and a refusal must not come from a rule the
//! user is never shown.
//!
//! WHAT THAT LAST SENTENCE USED TO SAY, AND THE DEFECT IT CARRIED. Until
//! 2026-08-08 one mark of usefulness made an item immortal: `is_stale`
//! returned false forever after it, and no operation anywhere undoes a
//! verdict. The judgement debt asks first about the items served most often -
//! the ones already winning every place they can - so the only maintenance
//! loop in the system was quietly freezing exactly the items that dominate,
//! and the cheap one-call answer was the one that did it. Retiring still
//! takes two noise marks, so one stray verdict still decides nothing; what a
//! useful mark no longer does is outrank a reader who changes their mind
//! later, about an item that has since drifted.
//! `status`/`doctor` still report "served N times, never marked" as something
//! to look at, never as a filter.

use crate::input::ServeInput;
use crate::rank::RankedItem;
use crate::usefulness::NoiseTally;
use model::item::{Binding, Item};
use std::collections::{HashMap, HashSet};
use thor_core::event_store::EventStore;

/// Kept for `status`/`doctor`'s "served this many times and never marked
/// useful" line, which is a number to LOOK AT. It decides nothing: see this
/// module's own doc comment for the day that rule was falsified in
/// production.
pub const STALE_AFTER_SERVINGS: usize = 5;

/// Called noise this many times SINCE the last mark of usefulness is where
/// an item stops reaching an injection surface.
///
/// TWO, and not a number picked here: it is what 1.0 already did. Its own
/// rule was that a second noise judgement is where a fact starts costing a
/// slot, and that rule ran for months over the same memory and the same
/// reader. One stray judgement means "not here, this time" - a rule can fire
/// once in the wrong place and still be right. A second means the reader has
/// seen it fire wrongly twice, which is a pattern.
///
/// The owner's own ledger backs the shape: of his 182 noise judgements 135
/// are a single mark and 47 are two or more. A threshold of one would retire
/// nearly four times as much on a signal that has not repeated itself.
pub const NOISE_MARKS_BEFORE_STALE: usize = 2;

/// Noise marks at this many DIFFERENT places say a note is wrong in general, not
/// at one place, and silence it everywhere - today's global rule, reached by
/// places. Two marks at one place silence the note at that place only
/// (`NOISE_MARKS_BEFORE_STALE`); two marks that name no place (every mark
/// written before places existed) silence it everywhere, as they always did.
pub const NOISE_PLACES_BEFORE_GLOBAL: usize = 3;

/// THE ONE comparison with `NOISE_MARKS_BEFORE_STALE`: is this many marks enough?
/// `serve/tests/decay_is_decided_in_exactly_one_place.rs` fails if the literal
/// is ever spelled anywhere else.
fn enough_marks(n: usize) -> bool {
    n >= NOISE_MARKS_BEFORE_STALE
}

/// Could this tally silence the note somewhere? Only then is the note's own
/// history (its revisions since the marks) worth reading.
fn reaches_a_threshold(tally: &NoiseTally) -> bool {
    enough_marks(tally.placeless) || tally.places.values().any(|n| enough_marks(*n)) || tally.places.len() >= NOISE_PLACES_BEFORE_GLOBAL
}

/// Everything decay needs about the store, loaded once per call so a serving
/// path folds the log a single time rather than once per candidate.
pub struct DecayContext {
    ever_marked_useful: HashSet<String>,
    noise: HashMap<String, NoiseTally>,
}

impl DecayContext {
    /// Load what a decay decision needs, once, from the real store: one pass over the verdict events.
    pub fn load(store: &EventStore) -> Self {
        // Deliberately does NOT fold the serving history any more: a serving
        // count decides nothing here, so loading it on every hook call would
        // be work done to reach a number nobody reads.
        // One pass over the verdict events only (an index on kind answers it
        // without reading the firings), not two folds over the whole log.
        let (ever_marked_useful, mut noise) = crate::usefulness::mark_folds(store);
        // A verdict judged the version it was given on. Only an id that
        // already sits at a threshold can change the answer, so only those
        // pay for a look at their own revisions.
        for (id, tally) in noise.iter_mut() {
            if !reaches_a_threshold(tally) {
                continue;
            }
            *tally = crate::usefulness::tally_since_last_change(store, id);
        }
        DecayContext { ever_marked_useful, noise }
    }

    /// Was this item ever marked useful? Exposed so `status` can report the
    /// look-at number without folding the log a second time, and without
    /// re-deriving the answer itself.
    pub fn was_ever_marked_useful(&self, id: &str) -> bool {
        self.ever_marked_useful.contains(id)
    }

    /// A context with no serving history at all: nothing is stale. The right
    /// default for any caller (and this module's own tests) that has no
    /// event log to fold at all yet.
    pub fn empty() -> Self {
        DecayContext { ever_marked_useful: HashSet::new(), noise: HashMap::new() }
    }

    /// Whether this item is NEVER hidden, whatever was said about it: pinned,
    /// or a rule whose check can refuse.
    ///
    /// An `Always`-bound item is NEVER stale, and that is not a tuning knob -
    /// it is what `Always` means. Every other binding says "fire when this
    /// file or this action turns up"; `Always` says the owner elevated this
    /// to standing, which is the strongest mark of usefulness there is. A pin
    /// is already a mark, so retiring one for never having been marked
    /// retires it for the wrong reason. Taking a standing rule down is an
    /// unpin, and that is the owner's own act (the evaluation re-reads a pinned
    /// note and has it confirmed or rewritten, never unpinned).
    ///
    /// MEASURED, ON THE FIRST REAL DAY (2026-08-03), so this is not a
    /// precaution: the owner runs several Claude Code sessions at once, and
    /// every one of them fires SessionStart. His first restart on 2.0 opened
    /// four sessions inside ten seconds, so all thirteen of his standing
    /// rules spent four of their five allowed servings in one go and decayed
    /// out of the injection surfaces before he had used the tool once.
    /// Raising the threshold would only have moved that failure a restart or
    /// two out (CONTRACT.md R9: a compensating knob is a reported design
    /// failure, not a fix). The modelling error was treating a pin as
    /// something that still has to earn its place.
    ///
    /// A rule whose check can refuse is never hidden either. The write guard
    /// refuses with it whether or not decay shows it, so hiding it would
    /// mean a refusal from a rule the user is never shown.
    fn is_exempt(item: &Item) -> bool {
        item.bindings.iter().any(|b| matches!(b, Binding::Always)) || crate::sweep::can_refuse(item)
    }

    /// Whether this item is currently stale EVERYWHERE: called noise at least
    /// `NOISE_MARKS_BEFORE_STALE` times with no place named, or at
    /// `NOISE_PLACES_BEFORE_GLOBAL` different places, SINCE the last time anyone
    /// called it useful (never "and never once marked useful" - see below). The
    /// one place this module decides it - see `serve/tests/
    /// decay_is_decided_in_exactly_one_place.rs`, which fails if the literal
    /// comparison is ever duplicated elsewhere in this crate.
    ///
    /// A SERVING COUNT DECIDES NOTHING HERE, and that is the whole lesson of
    /// this module's own doc comment: firing often is what relevance looks
    /// like, so retiring on it is backwards. Nor is it "was it ever marked
    /// useful": that form made one verdict permanent and let the maintenance
    /// loop freeze exactly the items that already dominate - see
    /// `usefulness::noise_since_last_useful`. A useful mark still protects: it
    /// clears the marks before it.
    pub fn is_stale(&self, item: &Item) -> bool {
        if Self::is_exempt(item) {
            return false;
        }
        self.noise
            .get(&item.id)
            .is_some_and(|tally| enough_marks(tally.placeless) || tally.places.len() >= NOISE_PLACES_BEFORE_GLOBAL)
    }

    /// Whether this item is stale at the places `input` reaches it through:
    /// stale everywhere (`is_stale`), or called noise at least
    /// `NOISE_MARKS_BEFORE_STALE` times at one of those very places. At any
    /// other place it is still served.
    pub fn is_stale_at(&self, item: &Item, input: &ServeInput) -> bool {
        if self.is_stale(item) {
            return true;
        }
        if Self::is_exempt(item) {
            return false;
        }
        let Some(tally) = self.noise.get(&item.id) else { return false };
        if !tally.places.values().any(|n| enough_marks(*n)) {
            return false;
        }
        crate::rank::matched_places(item, input).iter().any(|place| self.is_stale_at_place(item, place))
    }

    /// Whether this item is silenced at one place (a label of `rank::
    /// matched_places`): stale everywhere, or called noise often enough at
    /// that very place. THE one definition of "silenced at this place": the
    /// serving paths and the judgement debt both ask it.
    pub fn is_stale_at_place(&self, item: &Item, place: &str) -> bool {
        if self.is_stale(item) {
            return true;
        }
        if Self::is_exempt(item) {
            return false;
        }
        self.noise.get(&item.id).is_some_and(|tally| enough_marks(tally.at(place)))
    }

    /// Where the noise marks of `id` stand, for the mark tool's reply.
    pub fn tally_of(&self, id: &str) -> NoiseTally {
        self.noise.get(id).cloned().unwrap_or_default()
    }
}

/// The one place every injection surface calls to drop a stale item AFTER
/// `rank::select`/`session_start::select` has already decided kind/binding
/// eligibility. Order-preserving (a plain filter, never a re-sort): decay
/// never touches ranking, only removes what should not reach a gate at all.
/// Drops what the reader has twice said did not belong, and nothing else.
///
/// THE WRITE GUARD IS DELIBERATELY NOT ONE OF THESE CALLERS. "Every
/// injection surface" above means exactly the three that only ever SHOW
/// something - session start, the per-prompt/per-action render, and their
/// `why`/`check` previews. `serve::absent_guard` (wired into
/// `serve::guard_verdict`'s `file_write`/`command`/`requires`, which the
/// hook runs) is not a fourth one: it decides whether a write is
/// REFUSED outright, and it must never route its own candidate pool through
/// this function. Investigated 2026-09-08 against another memory server's
/// own rule ("an agent cannot write its own success story"): `mark::
/// record_noise` is callable by the very agent a block just refused, and
/// `NOISE_MARKS_BEFORE_STALE` calls to it is all `is_stale` above needs. Had
/// the guard's candidate pool ever been taken through `retain_live`, two
/// `mark noise` calls plus the write a rule exists to stop would have been
/// enough to get it through - an agent judging the very rule that judges it.
/// It is not wired that way, and `absent_guard.rs` itself names none of this
/// module at all - see `serve/tests/decay_is_decided_in_exactly_one_place.rs`'s
/// own `the_write_guard_never_applies_decay_to_its_candidate_pool` (fails the
/// build the day that source-level claim stops being true) and `serve/tests/
/// decay_never_disarms_the_write_guard.rs` for the end-to-end proof through
/// the real compiled binary. A proof-backed rule comes down only by
/// `retract` (with its reason, logged) or by the owner revising it - never by
/// an agent's own noise verdict on the refusal it just received.
pub fn retain_live(items: Vec<RankedItem>, decay: &DecayContext) -> Vec<RankedItem> {
    items.into_iter().filter(|r| !decay.is_stale(&r.item)).collect()
}

/// `retain_live` for a surface that knows what is happening (a file, a command,
/// a tool, a prompt): an item called noise twice AT the place `input` reaches
/// it through is dropped here too, and still served at every other place.
/// Order-preserving, exactly like `retain_live`.
pub fn retain_live_at(items: Vec<RankedItem>, decay: &DecayContext, input: &ServeInput) -> Vec<RankedItem> {
    items.into_iter().filter(|r| !decay.is_stale_at(&r.item, input)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Item, Kind};

    fn item_of(id: &str, bindings: Vec<Binding>) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("rule {id}"),
            bindings,
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        }
    }

    fn bound(id: &str) -> Item {
        item_of(
            id,
            vec![Binding::Target { kind: model::item::TargetKind::Path, value: format!("src/{id}.rs") }],
        )
    }

    fn pinned(id: &str) -> Item {
        item_of(id, vec![Binding::Always])
    }

    fn ranked_from(item: Item) -> RankedItem {
        RankedItem { id: item.id.clone(), item, via: None }
    }

    /// A context where `id` was called noise `n` times and served a lot.
    fn called_noise(id: &str, n: usize) -> DecayContext {
        let mut noise = HashMap::new();
        noise.insert(id.to_string(), NoiseTally::placeless(n));
        DecayContext { ever_marked_useful: HashSet::new(), noise }
    }

    #[test]
    fn one_noise_judgement_is_not_enough() {
        assert!(
            !called_noise("i1", NOISE_MARKS_BEFORE_STALE - 1).is_stale(&bound("i1")),
            "a rule can fire once in the wrong place and still be right"
        );
    }

    #[test]
    fn the_second_noise_judgement_retires_it() {
        assert!(called_noise("i1", NOISE_MARKS_BEFORE_STALE).is_stale(&bound("i1")));
    }

    /// THE DEFECT THIS PREVENTS, measured on the first real day (2026-08-03).
    /// Decay used to retire on a SERVING count with no mark of usefulness.
    /// Zero marks occur in real use, so that was a timer, and it retired the
    /// most-used rules first - four `prod_data` rules, one of them
    /// irreversible, were gone after one day. Firing often is what relevance
    /// looks like. A serving count must never retire anything again.
    #[test]
    fn a_serving_count_alone_never_retires_anything() {
        let decay = DecayContext { ever_marked_useful: HashSet::new(), noise: HashMap::new() };
        assert!(
            !decay.is_stale(&bound("hot-1")),
            "served constantly, never marked either way - that says nothing bad about it"
        );
    }

    /// THE OTHER DEFECT, same day: the owner runs several sessions at once,
    /// so one restart spent four of five allowed servings on every standing
    /// rule and all thirteen decayed out before he had used the tool. A pin
    /// is already a mark of usefulness; it comes down by an unpin.
    #[test]
    fn a_pinned_rule_never_decays_however_often_it_is_called_noise() {
        assert!(
            !called_noise("pin-1", NOISE_MARKS_BEFORE_STALE * 50).is_stale(&pinned("pin-1")),
            "an Always-bound rule is the owner's own standing choice"
        );
    }

    /// Built from real events, in order, because ORDER is now the whole
    /// point: a mark of usefulness clears the noise recorded BEFORE it.
    fn judged(id: &str, verdicts: &[bool]) -> DecayContext {
        let mut store = EventStore::in_memory().unwrap();
        for (n, useful) in verdicts.iter().enumerate() {
            let at = format!("2026-08-{:02}T00:00:00Z", n + 1);
            if *useful {
                crate::mark::record_useful(&mut store, "s", "l", "a", &at, id, None).unwrap();
            } else {
                crate::mark::record_noise(&mut store, "s", "l", "a", &at, id, None).unwrap();
            }
        }
        DecayContext::load(&store)
    }

    #[test]
    fn a_mark_of_usefulness_clears_the_noise_recorded_before_it() {
        let mut verdicts = vec![false; NOISE_MARKS_BEFORE_STALE * 20];
        verdicts.push(true);
        assert!(!judged("i1", &verdicts).is_stale(&bound("i1")), "a later verdict settles it");
    }

    /// THE DEFECT THIS PREVENTS, found independently by two reviews on
    /// 2026-08-08. "Ever marked useful" made one verdict permanent, and the
    /// judgement debt asks first about the items served most often - the ones
    /// already winning every place. So the only maintenance loop in the
    /// system quietly made the dominant items immortal, and the cheapest
    /// answer was the one that did it. The latest verdict counts now.
    #[test]
    fn noise_after_a_mark_of_usefulness_still_retires_it() {
        let mut verdicts = vec![true];
        verdicts.extend(std::iter::repeat(false).take(NOISE_MARKS_BEFORE_STALE));
        assert!(
            judged("i1", &verdicts).is_stale(&bound("i1")),
            "a verdict from before must not outrank one made later with better information"
        );
    }

    /// A rule stored for real, so its revisions are on the log in order.
    fn stored_rule(store: &mut EventStore, id: &str, text: &str, check: Option<model::item::Check>) -> Item {
        let mut item = item_of(id, vec![Binding::Moment(intent::Action::Commit)]);
        item.text = text.to_string();
        item.falsifier = Some("it stops being true".to_string());
        item.check = check;
        model::store::declare(store, "t", "t", "t", &item).expect("fixture must store");
        item
    }

    /// A revision written raw, as an older binary could have written it: the
    /// write gate's own view of the change is not what is under test here.
    fn revise_raw(store: &mut EventStore, item: &Item) {
        let body = serde_json::to_string(item).unwrap();
        store.append_mutate_checked("s", "l", "a", thor_core::event_store::EventKind::FactRevised, &item.id, None, &body).unwrap();
    }

    fn noise_n(store: &mut EventStore, id: &str, n: usize) {
        for i in 0..n {
            let at = format!("2026-09-{:02}T00:00:00Z", i + 1);
            crate::mark::record_noise(store, "s", "l", "a", &at, id, None).unwrap();
        }
    }

    /// The control: nothing changes the rule, so two noise marks still retire
    /// a plain note. Fix 1 must not weaken decay for an unrevised note.
    #[test]
    fn an_unrevised_plain_note_with_two_noise_marks_still_decays() {
        let mut store = EventStore::in_memory().unwrap();
        let item = stored_rule(&mut store, "plain", "a plain note", None);
        noise_n(&mut store, "plain", NOISE_MARKS_BEFORE_STALE);
        assert!(DecayContext::load(&store).is_stale(&item));
    }

    /// THE LIVE CASE (mechanische-agentbrief-verbiedt-subagenten): two noise
    /// marks judged an OLD text; the rule was then rewritten. The verdict
    /// judged the old version, so it must not retire the new one.
    #[test]
    fn noise_marks_from_before_a_text_revision_do_not_count() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = stored_rule(&mut store, "rewritten", "the old text", None);
        noise_n(&mut store, "rewritten", NOISE_MARKS_BEFORE_STALE);
        assert!(DecayContext::load(&store).is_stale(&item), "fixture sanity: retired before the revision");
        item.text = "a better text".to_string();
        revise_raw(&mut store, &item);
        assert!(!DecayContext::load(&store).is_stale(&item), "a verdict on the old text retired the new one");
        // Noise on the NEW version counts again, from zero.
        noise_n(&mut store, "rewritten", NOISE_MARKS_BEFORE_STALE);
        assert!(DecayContext::load(&store).is_stale(&item));
    }

    #[test]
    fn noise_marks_from_before_a_check_or_binding_change_do_not_count() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = stored_rule(&mut store, "gains-teeth", "same text", None);
        noise_n(&mut store, "gains-teeth", NOISE_MARKS_BEFORE_STALE);
        item.bindings = vec![Binding::Moment(intent::Action::Push)];
        revise_raw(&mut store, &item);
        assert!(!DecayContext::load(&store).is_stale(&item), "a binding change must clear older verdicts");

        let mut store = EventStore::in_memory().unwrap();
        let mut item = stored_rule(&mut store, "gains-check", "same text", None);
        noise_n(&mut store, "gains-check", NOISE_MARKS_BEFORE_STALE);
        item.check = Some(model::item::Check::Forbidden { literals: vec!["--force".to_string()] });
        revise_raw(&mut store, &item);
        // Compare through a plain note's own view: the count itself, not the
        // refusing-rule exemption (which the next test covers).
        let mut as_note = item.clone();
        as_note.check = None;
        assert!(!DecayContext::load(&store).is_stale(&as_note), "a check change must clear older verdicts");
    }

    /// A revision that changes neither text, check nor bindings (a tag, a
    /// severity, the falsifier) judged the same rule: the verdicts stand.
    #[test]
    fn a_revision_that_changes_only_tags_keeps_the_verdicts() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = stored_rule(&mut store, "retagged", "same text", None);
        noise_n(&mut store, "retagged", NOISE_MARKS_BEFORE_STALE);
        item.tags = vec!["housekeeping".to_string()];
        revise_raw(&mut store, &item);
        assert!(DecayContext::load(&store).is_stale(&item));
    }

    /// The decay context as it was loaded before the verdict-only pass: two
    /// folds over the whole log (`usefulness::ever_marked_useful`,
    /// `noise_since_last_useful`, both still there) and a look at the revisions
    /// of every id at the threshold. Kept here as the oracle.
    fn load_by_the_old_folds(store: &EventStore) -> (HashSet<String>, HashMap<String, usize>) {
        let mut noise = crate::usefulness::noise_since_last_useful(store);
        for (id, count) in noise.iter_mut() {
            if *count < NOISE_MARKS_BEFORE_STALE {
                continue;
            }
            *count = crate::usefulness::tally_since_last_change(store, id).total();
        }
        (crate::usefulness::ever_marked_useful(store), noise)
    }

    /// What the context holds, as the plain counts the old folds produced (a
    /// store with no place in any verdict has only placeless marks).
    fn counts_of(decay: &DecayContext) -> HashMap<String, usize> {
        decay.noise.iter().map(|(id, tally)| (id.clone(), tally.total())).collect()
    }

    fn serve_n(store: &mut EventStore, id: &str, n: usize) {
        for _ in 0..n {
            store
                .append_event("s", "l", "hook", thor_core::event_store::EventKind::ItemServed, id, None, "{}")
                .unwrap();
        }
    }

    /// The hook loads the decay context on every call, so it reads only the
    /// verdict events now. It must find exactly what the two whole-log folds
    /// found: the same ids ever marked useful, the same noise counts (a useful
    /// mark resets, a revision of text, check or bindings resets, a tag change
    /// does not), with firings and revisions interleaved.
    #[test]
    fn the_verdict_only_pass_equals_the_old_whole_log_folds() {
        let mut store = EventStore::in_memory().unwrap();
        stored_rule(&mut store, "a", "text a", None);
        let mut b = stored_rule(&mut store, "b", "text b", None);
        let mut c = stored_rule(&mut store, "c", "text c", None);
        stored_rule(&mut store, "d", "text d", None);
        stored_rule(&mut store, "e", "text e", None);
        serve_n(&mut store, "a", 3);
        noise_n(&mut store, "a", 3);
        crate::mark::record_useful(&mut store, "s", "l", "a", "2026-10-01T00:00:00Z", "a", None).unwrap();
        serve_n(&mut store, "b", 2);
        noise_n(&mut store, "b", 2);
        b.text = "text b rewritten".to_string();
        revise_raw(&mut store, &b);
        noise_n(&mut store, "b", 1);
        serve_n(&mut store, "a", 2);
        noise_n(&mut store, "a", 1);
        noise_n(&mut store, "c", 2);
        c.tags = vec!["only-a-tag".to_string()];
        revise_raw(&mut store, &c);
        noise_n(&mut store, "d", 4);
        crate::mark::record_useful(&mut store, "s", "l", "a", "2026-10-02T00:00:00Z", "e", None).unwrap();
        noise_n(&mut store, "ghost", 2);
        serve_n(&mut store, "d", 5);

        let now = DecayContext::load(&store);
        let old = load_by_the_old_folds(&store);
        assert_eq!(now.ever_marked_useful, old.0);
        assert_eq!(counts_of(&now), old.1);
        // And the fixture really exercises each branch.
        assert!(now.was_ever_marked_useful("a") && now.was_ever_marked_useful("e") && !now.was_ever_marked_useful("d"));
        let counts = counts_of(&now);
        assert_eq!(counts.get("a"), Some(&1), "noise after the useful mark");
        assert_eq!(counts.get("b"), Some(&1), "a text revision cleared the older verdicts");
        assert_eq!(counts.get("c"), Some(&2), "a tag change keeps them");
        assert_eq!(counts.get("d"), Some(&4));
        assert_eq!(counts.get("e"), Some(&0));
    }

    /// The same equality on a store file opened the way the hook opens it
    /// (read-only), so the connection that runs the narrow query is the real one.
    #[test]
    fn the_verdict_only_pass_equals_the_old_folds_on_a_read_only_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&path).unwrap();
            stored_rule(&mut store, "a", "text a", None);
            serve_n(&mut store, "a", 4);
            noise_n(&mut store, "a", 2);
            crate::mark::record_useful(&mut store, "s", "l", "a", "2026-10-01T00:00:00Z", "a", None).unwrap();
            noise_n(&mut store, "a", 2);
        }
        let reader = EventStore::open_read_only(&path).unwrap();
        let now = DecayContext::load(&reader);
        let old = load_by_the_old_folds(&reader);
        assert_eq!(now.ever_marked_useful, old.0);
        assert_eq!(counts_of(&now), old.1);
        assert_eq!(counts_of(&now).get("a"), Some(&2));
    }

    /// FIX 2: a rule whose check can refuse is never retired from showing,
    /// however many noise marks it has. The guard refuses with it whether or
    /// not decay hides it, so hiding it means a refusal from a rule the user
    /// is never shown.
    #[test]
    fn a_rule_whose_check_can_refuse_never_decays() {
        for check in [
            model::item::Check::Forbidden { literals: vec!["--force".to_string()] },
            model::item::Check::Requires { when: "Agent".to_string(), required: vec!["model".to_string()], exempt: Vec::new() },
            model::item::Check::Absent { path: "a.txt".to_string(), literal: "x".to_string() },
            model::item::Check::AbsentAll { path: "a.txt".to_string(), literals: vec!["x".to_string()] },
            model::item::Check::Contains { path: "a.txt".to_string(), literal: "x".to_string() },
        ] {
            let mut item = bound("armed");
            item.check = Some(check.clone());
            assert!(
                !called_noise("armed", NOISE_MARKS_BEFORE_STALE * 10).is_stale(&item),
                "a refusing rule must stay shown: {check:?}"
            );
        }
    }

    /// A check that cannot refuse (a path_exists currency proof) is not an
    /// exemption: that rule can only inform, and decay still applies.
    #[test]
    fn a_rule_whose_check_cannot_refuse_still_decays() {
        let mut item = bound("informs");
        item.check = Some(model::item::Check::PathExists { path: "a.txt".to_string() });
        assert!(called_noise("informs", NOISE_MARKS_BEFORE_STALE).is_stale(&item));
    }

    #[test]
    fn an_item_nobody_judged_is_never_stale() {
        assert!(!DecayContext::empty().is_stale(&bound("never-judged")));
    }

    #[test]
    fn retain_live_drops_only_the_retired_item_and_keeps_order() {
        let mut noise = HashMap::new();
        noise.insert("noisy-1".to_string(), NoiseTally::placeless(NOISE_MARKS_BEFORE_STALE));
        let decay = DecayContext { ever_marked_useful: HashSet::new(), noise };
        let items = vec![ranked_from(bound("first")), ranked_from(bound("noisy-1")), ranked_from(bound("last"))];
        let ids: Vec<String> = retain_live(items, &decay).iter().map(|r| r.id.clone()).collect();
        assert_eq!(ids, vec!["first".to_string(), "last".to_string()]);
    }

    /// Reversible by design: this is a fold over the log re-evaluated every
    /// call, never a persisted flag, so a later mark of usefulness simply
    /// yields a different answer with nothing to undo.
    #[test]
    fn decay_is_reversible_a_mark_after_the_fact_un_stales_it() {
        let mut verdicts = vec![false; NOISE_MARKS_BEFORE_STALE];
        assert!(judged("i1", &verdicts).is_stale(&bound("i1")), "fixture sanity: retired before any mark");
        verdicts.push(true);
        assert!(!judged("i1", &verdicts).is_stale(&bound("i1")));
    }

    // ---------------------------------------------------- noise per place (2026-11-20)

    fn two_files() -> Item {
        let mut item = item_of(
            "two-files",
            vec![
                Binding::Target { kind: model::item::TargetKind::Path, value: "src/a.rs".to_string() },
                Binding::Target { kind: model::item::TargetKind::Path, value: "src/b.rs".to_string() },
            ],
        );
        item.project = Some("p".to_string());
        item
    }

    fn touching(path: &str) -> ServeInput {
        let mut input = ServeInput::default();
        input.add_file(path);
        input
    }

    fn mark_at(store: &mut EventStore, id: &str, noise: bool, n: usize, place: Option<&str>) {
        for i in 0..n {
            let at = format!("2026-11-{:02}T00:00:00Z", i + 1);
            if noise {
                crate::mark::record_noise_at(store, "s", "l", "a", &at, id, None, place).unwrap();
            } else {
                crate::mark::record_useful_at(store, "s", "l", "a", &at, id, None, place).unwrap();
            }
        }
    }

    fn declared_two_files(store: &mut EventStore) -> Item {
        let mut item = two_files();
        item.falsifier = Some("it stops being true".to_string());
        model::store::declare(store, "t", "t", "t", &item).expect("fixture must store");
        item
    }

    /// THE POINT: two noise marks at one place silence the note THERE and
    /// nowhere else.
    #[test]
    fn two_noise_marks_at_one_place_silence_it_there_only() {
        let mut store = EventStore::in_memory().unwrap();
        let item = declared_two_files(&mut store);
        mark_at(&mut store, "two-files", true, NOISE_MARKS_BEFORE_STALE, Some("path:src/a.rs"));
        let decay = DecayContext::load(&store);
        assert!(decay.is_stale_at(&item, &touching("src/a.rs")), "silenced at the place it was called noise at");
        assert!(!decay.is_stale_at(&item, &touching("src/b.rs")), "still served at its other place");
        assert!(!decay.is_stale(&item), "and not stale everywhere");
        let ids = |input: &ServeInput| -> Vec<String> {
            retain_live_at(vec![ranked_from(item.clone())], &decay, input).iter().map(|r| r.id.clone()).collect()
        };
        assert!(ids(&touching("src/a.rs")).is_empty());
        assert_eq!(ids(&touching("src/b.rs")), vec!["two-files".to_string()]);
    }

    #[test]
    fn one_noise_mark_per_place_silences_nothing() {
        let mut store = EventStore::in_memory().unwrap();
        let item = declared_two_files(&mut store);
        mark_at(&mut store, "two-files", true, 1, Some("path:src/a.rs"));
        mark_at(&mut store, "two-files", true, 1, Some("path:src/b.rs"));
        let decay = DecayContext::load(&store);
        assert!(!decay.is_stale_at(&item, &touching("src/a.rs")) && !decay.is_stale_at(&item, &touching("src/b.rs")));
    }

    /// OLD STORES BEHAVE AS BEFORE: two marks with no place silence the note
    /// everywhere, whatever the place.
    #[test]
    fn two_placeless_noise_marks_still_silence_it_everywhere() {
        let mut store = EventStore::in_memory().unwrap();
        let item = declared_two_files(&mut store);
        mark_at(&mut store, "two-files", true, NOISE_MARKS_BEFORE_STALE, None);
        let decay = DecayContext::load(&store);
        assert!(decay.is_stale(&item));
        assert!(decay.is_stale_at(&item, &touching("src/a.rs")) && decay.is_stale_at(&item, &touching("src/b.rs")));
        assert!(decay.is_stale_at(&item, &touching("somewhere/else.rs")));
    }

    #[test]
    fn one_placeless_mark_and_one_placed_mark_do_not_add_up() {
        let mut store = EventStore::in_memory().unwrap();
        let item = declared_two_files(&mut store);
        mark_at(&mut store, "two-files", true, 1, None);
        mark_at(&mut store, "two-files", true, 1, Some("path:src/a.rs"));
        let decay = DecayContext::load(&store);
        assert!(!decay.is_stale_at(&item, &touching("src/a.rs")));
    }

    /// Marks at three different places say the note is wrong in general.
    #[test]
    fn noise_at_three_different_places_silences_it_everywhere() {
        let mut store = EventStore::in_memory().unwrap();
        let item = declared_two_files(&mut store);
        for place in ["path:src/a.rs", "path:src/b.rs", "command:cargo"] {
            mark_at(&mut store, "two-files", true, 1, Some(place));
        }
        let decay = DecayContext::load(&store);
        assert!(decay.is_stale(&item), "three places, one mark each");
        assert!(decay.is_stale_at(&item, &touching("src/never-seen.rs")));
        // Two places are not enough.
        let mut store = EventStore::in_memory().unwrap();
        let item = declared_two_files(&mut store);
        for place in ["path:src/a.rs", "path:src/b.rs"] {
            mark_at(&mut store, "two-files", true, 1, Some(place));
        }
        assert!(!DecayContext::load(&store).is_stale(&item));
    }

    /// A useful mark at a place clears that place's noise only; a placeless one
    /// clears them all, as it always did.
    #[test]
    fn a_useful_mark_at_a_place_clears_that_places_noise() {
        let mut store = EventStore::in_memory().unwrap();
        let item = declared_two_files(&mut store);
        mark_at(&mut store, "two-files", true, NOISE_MARKS_BEFORE_STALE, Some("path:src/a.rs"));
        mark_at(&mut store, "two-files", true, NOISE_MARKS_BEFORE_STALE, Some("path:src/b.rs"));
        let decay = DecayContext::load(&store);
        assert!(decay.is_stale_at(&item, &touching("src/a.rs")) && decay.is_stale_at(&item, &touching("src/b.rs")));
        mark_at(&mut store, "two-files", false, 1, Some("path:src/a.rs"));
        let decay = DecayContext::load(&store);
        assert!(!decay.is_stale_at(&item, &touching("src/a.rs")), "brought back where it was confirmed");
        assert!(decay.is_stale_at(&item, &touching("src/b.rs")), "the other place stays silenced");
        mark_at(&mut store, "two-files", false, 1, None);
        let decay = DecayContext::load(&store);
        assert!(!decay.is_stale_at(&item, &touching("src/b.rs")), "a placeless useful mark clears every place");
    }

    /// A verdict judged the version served when it was given: a rewrite clears
    /// the marks at every place.
    #[test]
    fn a_text_change_clears_the_noise_at_every_place() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = declared_two_files(&mut store);
        mark_at(&mut store, "two-files", true, NOISE_MARKS_BEFORE_STALE, Some("path:src/a.rs"));
        assert!(DecayContext::load(&store).is_stale_at(&item, &touching("src/a.rs")));
        item.text = "a better text".to_string();
        revise_raw(&mut store, &item);
        assert!(!DecayContext::load(&store).is_stale_at(&item, &touching("src/a.rs")));
    }

    #[test]
    fn a_pinned_or_refusing_item_is_never_silenced_at_a_place() {
        let mut armed = two_files();
        armed.check = Some(model::item::Check::Forbidden { literals: vec!["--force".to_string()] });
        let mut decay = DecayContext::empty();
        decay.noise.insert("two-files".to_string(), NoiseTally { placeless: 0, places: [("path:src/a.rs".to_string(), 5)].into() });
        assert!(!decay.is_stale_at(&armed, &touching("src/a.rs")), "a rule whose check can refuse stays shown");
        let mut pinned = two_files();
        pinned.bindings.push(Binding::Always);
        assert!(!decay.is_stale_at(&pinned, &touching("src/a.rs")), "a pinned note is never hidden");
    }

    /// A command place, as a serving records it: the command text reaches the
    /// binding, so the place a mark derives from the trigger is the binding's.
    #[test]
    fn a_trigger_names_the_binding_it_reached() {
        let item = item_of(
            "cmd",
            vec![
                Binding::Target { kind: model::item::TargetKind::Command, value: "git push".to_string() },
                Binding::Target { kind: model::item::TargetKind::Path, value: "src/a.rs".to_string() },
            ],
        );
        assert_eq!(crate::rank::places_of_trigger(&item, "git push origin main"), vec!["command:git push".to_string()]);
        assert_eq!(crate::rank::places_of_trigger(&item, "src/a.rs"), vec!["path:src/a.rs".to_string()]);
        assert!(crate::rank::places_of_trigger(&item, "ls -la").is_empty());
        assert_eq!(crate::rank::resolve_place(&item, "src\\a.rs").unwrap(), "path:src/a.rs");
        assert_eq!(crate::rank::resolve_place(&item, "command:git push").unwrap(), "command:git push");
        assert!(crate::rank::resolve_place(&item, "src/zzz.rs").unwrap_err().contains("command:git push"));
    }
}
