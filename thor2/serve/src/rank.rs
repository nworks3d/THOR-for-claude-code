//! Selection and ranking: everything a `Rule`/`Orientation` binding matches,
//! ordered worst-first. Used by every channel (hook/check/why) through
//! `select` - the one selection function the workspace design note requires.
//!
//! Ranking order (CONTRACT.md, this crate's own brief):
//! 1. severity: irreversible > costly > house_style > no severity at all.
//! 2. a derived overlap key: how many distinctive words of the ACTUAL input
//!    (never the item's own declared tags) the item's text also contains.
//! 3. never sentence length - see `ranking_never_falls_back_to_sentence_length`
//!    below for the defect this refuses to reintroduce. The final tiebreaker
//!    is the item's own id, which is arbitrary but never a length-shaped bias.

use crate::input::ServeInput;
use crate::live::LiveItem;
use crate::shell::Segment;
use model::item::{Binding, Item, Kind, Severity, TargetKind};
use regex::Regex;
use std::sync::OnceLock;

/// One item plus everything the render step needs to know about why it is here.
#[derive(Debug, Clone)]
pub struct RankedItem {
    pub id: String,
    pub item: Item,
}

/// Moved to `model::item` so the write gate can rank a candidate against its
/// rivals without `model` depending on `serve`. Re-exported here because the
/// ranking is still this module's business and every doc comment names it so.
pub use model::item::severity_rank;

/// A distinctive term: letters/digits plus the punctuation an identifier or a
/// path realistically carries, at least 4 characters. Same shape as the
/// sibling Python arm's `_TERM` (slices/python/gate.py) - the idea that a rule
/// naming what is actually happening beats a generic one is not new here,
/// only ported into the item model.
fn term_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[a-z0-9][a-z0-9._/-]{3,}").unwrap())
}

fn terms(text: &str) -> std::collections::HashSet<String> {
    term_regex().find_iter(&text.to_lowercase()).map(|m| m.as_str().to_string()).collect()
}

/// How many distinctive terms of the INPUT's own context (the real command,
/// path or draft text - never a field the item declared about itself) also
/// appear in the item's text. Derived fresh every call, so an item cannot
/// game its own rank by declaring keywords; only what is actually happening
/// counts.
pub fn closeness(item_text: &str, context: &str) -> usize {
    let ctx_terms = terms(context);
    if ctx_terms.is_empty() {
        return 0;
    }
    let item_terms = terms(item_text);
    ctx_terms.intersection(&item_terms).count()
}

/// Same target: moved to `model::normalize` so the write gate can ask the
/// same question without `model` having to depend on `serve`. Re-exported
/// here under its original name, because every caller and half the doc
/// comments in this workspace name it `rank::target_matches`.
pub use model::normalize::target_matches;

/// `Always` never matches here - on purpose. The five injection surfaces
/// (CONTRACT.md's "vijf oppervlakken die elkaars plaatsen nooit afnemen")
/// never share a candidate pool: `Always` is exactly and only surface 1's
/// (session start, see `crate::session_start`), so it is refused here even
/// though the type still allows an item to declare it - the same
/// belt-and-braces the `Kind::can_fire` gate already applies to `Report`/
/// `Chunk`/`Lookup`. An item bound to BOTH `Always` and a real `Moment`/
/// `Target` still reaches the moment/prompt surfaces through that other
/// binding; only the `Always` binding itself is inert here.
///
/// `item_project` is the CANDIDATE's own scope (never the session's - that
/// comparison already happened in `select`'s own `project::applies_to`
/// filter before this function is ever called), and `input.root` is the
/// session's own project root, when one was resolved. Both exist only to
/// feed `absent_guard::scoped_target_matches`'s own root-aware Path/Dir
/// comparison - see that function's own doc comment for why a bare
/// `target_matches` let a project-scoped, repo-relative anchor reach a file
/// far outside the project (measured 2026-09-08).
fn binding_matches(binding: &Binding, input: &ServeInput, item_project: Option<&str>, commands: &[Vec<Segment>]) -> bool {
    match binding {
        Binding::Always => false,
        Binding::Moment(action) => input.moments.contains(action),
        // A Command binding is the one kind whose two sides are not the same
        // sort of string: the rule declares a command ("gh repo edit"), the
        // input carries a whole invocation ("gh repo edit x --visibility
        // public"). `target_matches` compares those as paths - equality or a
        // segment suffix - so a real, argument-bearing command essentially
        // never matched, and every fact anchored to a command was silent at
        // the exact moment it was written for. Reproduced against the real
        // store on 2026-08-14 on the rule guarding repo visibility.
        //
        // The guard arm had already solved this and said so in its own doc
        // comment; the serving path simply never used it. This is that one
        // definition, not a second one - `absent_guard::command_anchor_names`,
        // an anchor reaching ANY segment of the command (`serve::shell`), read
        // in the sh dialect. `commands` holds each Command target already cut
        // into its segments (`command_segments`), once per call, not once per
        // candidate.
        Binding::Target { kind: TargetKind::Command, value } => {
            commands.iter().any(|segments| crate::shell::reaches_any(value, segments))
        }
        // Path/Dir (and every other kind: Symbol/Host/Route/Project, where
        // `scoped_target_matches` itself falls straight through to the plain
        // `target_matches` below, unchanged) go through the ONE root-aware
        // comparison - never `target_matches` directly any more, so a
        // project-scoped anchor can no longer reach a file the project's own
        // root does not contain.
        Binding::Target { kind, value } => input
            .targets
            .iter()
            .any(|(k, v)| crate::absent_guard::scoped_target_matches(item_project, *kind, value, *k, v, input.root.as_deref())),
    }
}

/// THE READ-ONLY RULE (step 32). A call that only looks (`cat`, `grep`, `ls`,
/// `git diff` ... - `read_only::is_read_only_call`) cannot break what a fact
/// about CHANGING a file is about, so a fact reached on such a call only
/// through a file or directory anchor is served when its text names the reader
/// that is running (`grep`, `git diff`: a fact about reading with that tool,
/// like a grep trap in a log file) and is otherwise left out. A fact reached
/// through any other binding (a command anchor, a moment, a host) is served
/// as before, and so is every fact on a call that writes or that cannot be
/// read in full.
fn skipped_on_a_read_only_call(candidate: &LiveItem, input: &ServeInput, commands: &[Vec<Segment>]) -> bool {
    if !input.shell_call || commands.is_empty() || !commands.iter().all(|segments| crate::read_only::is_read_only_call(segments)) {
        return false;
    }
    let reached_otherwise = candidate.item.bindings.iter().any(|b| {
        !matches!(b, Binding::Target { kind: TargetKind::Path | TargetKind::Dir, .. })
            && binding_matches(b, input, candidate.item.project.as_deref(), commands)
    });
    if reached_otherwise {
        return false;
    }
    let names_the_reader = commands
        .iter()
        .flat_map(|segments| crate::read_only::readers_of(segments))
        .any(|reader| crate::read_only::text_names(&candidate.item.text, &reader));
    !names_the_reader
}

/// Every Command target of `input`, cut into its segments in the sh dialect
/// - the reading `absent_guard::command_anchor_names` gives a command's text,
/// done once per call here rather than once per candidate.
fn command_segments(input: &ServeInput) -> Vec<Vec<Segment>> {
    input
        .targets
        .iter()
        .filter(|(kind, _)| *kind == TargetKind::Command)
        .map(|(_, command)| crate::shell::segments(command, crate::shell::Dialect::Sh))
        .collect()
}

/// Is this item even eligible to reach a gate at all? Delegates to the ONE
/// definition of "which kinds can fire" (`model::item::Kind::can_fire`) -
/// this function must never re-decide that boolean itself (see
/// `model/tests/single_can_fire_definition.rs`, which greps the whole
/// workspace for exactly that regression).
///
/// A `Report` or a `Chunk` may never reach a gate (the write gate already
/// refuses either of them with a binding - this is the SECOND lock on the
/// same door: even one hand-built with a binding, bypassing the gate
/// entirely, must still never be selected here). A `Lookup` is a different
/// door: it answers only an explicit request for its own key, never a
/// moment/target match (see `item::Kind`'s own doc comment), so it is
/// excluded here too, not served by this path at all.
fn eligible(kind: Kind) -> bool {
    kind.can_fire()
}

/// Every live item whose kind may reach a gate and at least one of whose
/// bindings matches the input, ranked worst-first: severity, then closeness
/// to what is actually happening, then id (never length - see the test
/// module). This is `select` in the CONTRACT sense: the ONE function every
/// channel (hook/check/why) calls to decide what applies, before any cap.
pub fn select(candidates: &[LiveItem], input: &ServeInput) -> Vec<RankedItem> {
    let commands = command_segments(input);
    let mut hits: Vec<RankedItem> = candidates
        .iter()
        .filter(|c| eligible(c.item.kind))
        // The same scoping session start applies, through the same single
        // place. Without it every project's moment-bound rules competed for
        // this surface's four slots in every project - see
        // `project::applies_to` for the measurement that found it.
        .filter(|c| crate::project::applies_to(c.item.project.as_deref(), input.project.as_deref()))
        .filter(|c| c.item.bindings.iter().any(|b| binding_matches(b, input, c.item.project.as_deref(), &commands)))
        .filter(|c| !skipped_on_a_read_only_call(c, input, &commands))
        .map(|c| RankedItem { id: c.id.clone(), item: c.item.clone() })
        .collect();

    hits.sort_by(|a, b| {
        // 1. IRREVERSIBLE KEEPS THE TOP BAND, always. Nothing lighter may
        //    displace a warning about something that cannot be undone, whatever
        //    else is true of it. This band is what makes the rest of this
        //    ordering safe to change at all.
        irreversible(&a.item)
            .cmp(&irreversible(&b.item))
            // 2. THEN THE ONE ANCHORED AT WHAT YOU ARE TOUCHING. Measured on
            //    the owner's store, 2026-08-09: opening the busiest file in it
            //    showed four general warnings about deploying and none of the
            //    twelve facts about that very file, because severity decided
            //    everything and closeness was never consulted across bands. A
            //    general rule still reaches you at the moment it is about; the
            //    specific one has only this one chance.
            .then_with(|| anchored_at_a_place(&b.item).cmp(&anchored_at_a_place(&a.item)))
            // 2b. AND AMONG PLACES, THE EXACT ONE FIRST. Since a directory
            //     anchor reaches the files inside it (see
            //     `normalize::target_matches`), one rule about a whole tree
            //     could otherwise sit in front of the rule about the very file
            //     being touched. The broad one still reaches you; the precise
            //     one has only this place.
            .then_with(|| at_this_exact_place(&b.item, input).cmp(&at_this_exact_place(&a.item, input)))
            // 3. Then weight, as before, among items equally close.
            .then_with(|| severity_rank(a.item.severity).cmp(&severity_rank(b.item.severity)))
            .then_with(|| {
                let ca = closeness(&a.item.text, &input.context);
                let cb = closeness(&b.item.text, &input.context);
                cb.cmp(&ca) // higher overlap first
            })
            .then_with(|| a.id.cmp(&b.id)) // deterministic, never length
    });
    hits
}

/// A warning about something that cannot be undone. Its own key in the
/// ordering, so nothing lighter can ever take its place - see `select`.
fn irreversible(item: &Item) -> bool {
    item.severity != Some(Severity::Irreversible)
}

/// Is this item anchored at the very thing being touched, rather than at a
/// directory somewhere above it? Both reach you; this decides which one is
/// read first when only a few fit.
fn at_this_exact_place(item: &Item, input: &ServeInput) -> bool {
    item.bindings.iter().any(|b| match b {
        Binding::Target { kind: TargetKind::Path, value } => input.targets.iter().any(|(k, v)| {
            *k == TargetKind::Path && model::normalize::normalize_target(value) == model::normalize::normalize_target(v)
        }),
        Binding::Target { kind: TargetKind::Dir, value } => input.targets.iter().any(|(_, v)| {
            model::normalize::normalize_target(value) == model::normalize::normalize_target(v)
        }),
        _ => false,
    })
}

/// Is this item bound to a PLACE - a file or a directory - rather than
/// reaching the pool through a broad moment? A place-bound item gets one
/// chance, at that place; a moment-bound one reaches you wherever that moment
/// is detected, so it has others.
fn anchored_at_a_place(item: &Item) -> bool {
    item.bindings
        .iter()
        .any(|b| matches!(b, Binding::Target { kind: TargetKind::Path | TargetKind::Dir, .. }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::ServeInput;
    use intent::Action;
    use model::item::{Binding, Item, Kind, Severity, TargetKind};

    fn base(id: &str, kind: Kind) -> LiveItem {
        LiveItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind,
                text: "do the thing".to_string(),
                bindings: vec![Binding::Moment(Action::Configure)],
                severity: None,
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: None,
                check: None,
            },
        }
    }

    /// An input that matches `base`'s own default binding
    /// (`Moment(Configure)`) - the fixture ranking tests below use to get a
    /// guaranteed hit without caring about moment/target mechanics
    /// themselves. Named for what it does, not "always": `Binding::Always`
    /// itself is never matched by `select` (see `binding_matches`'s own doc
    /// comment) and deliberately has no representation here any more.
    fn input_matching_base() -> ServeInput {
        ServeInput {
            moments: vec![Action::Configure],
            targets: vec![],
            context: String::new(),
            project: None,
            command: None,
            file: None,
            root: None,
            shell_call: false,
        }
    }

    // ------------------------------------------------------ project scope

    fn owned_by(id: &str, project: Option<&str>) -> LiveItem {
        let mut c = base(id, Kind::Rule);
        c.item.project = project.map(str::to_string);
        c
    }

    fn in_project(name: Option<&str>) -> ServeInput {
        ServeInput {
            moments: vec![Action::Configure],
            targets: vec![],
            context: String::new(),
            project: name.map(str::to_string),
            command: None,
            file: None,
            root: None,
            shell_call: false,
        }
    }

    /// THE DEFECT THIS PREVENTS, measured on the live store (2026-08-03).
    /// Session start scoped by project; this surface did not. The store holds
    /// 136 moment-bound rules owned by one business project against 66 for
    /// another and 74 global, so every action in every project drew from the
    /// whole pile and four slots decided the rest. Caught in the act: writing
    /// a Dockerfile for a NAS drew rules about a website's dev directory, an
    /// admin HTTP route and a production database wipe. Each rule correct,
    /// none of them about the work in hand.
    #[test]
    fn another_projects_rule_never_reaches_this_projects_moment() {
        let hits = select(&[owned_by("other", Some("acme-shop"))], &in_project(Some("thor")));
        assert!(hits.is_empty(), "a rule owned by another project must not fire here");
    }

    #[test]
    fn this_projects_own_rule_still_fires() {
        let hits = select(&[owned_by("mine", Some("thor"))], &in_project(Some("thor")));
        assert_eq!(hits.len(), 1, "a rule owned by this project must still fire");
    }

    /// A global rule belongs to every project - that is what "no project of
    /// its own" means, and scoping must never quietly retire the global tier.
    #[test]
    fn a_global_rule_fires_in_every_project_and_with_no_project_at_all() {
        assert_eq!(select(&[owned_by("g", None)], &in_project(Some("thor"))).len(), 1);
        assert_eq!(select(&[owned_by("g", None)], &in_project(Some("anything-else"))).len(), 1);
        assert_eq!(select(&[owned_by("g", None)], &in_project(None)).len(), 1);
    }

    /// When the project cannot be resolved at all, serve the global tier and
    /// nothing else. Under-serving is the safe direction here: a rule that
    /// does not apply teaches the reader to skim past the whole block.
    #[test]
    fn an_unresolvable_project_gets_the_global_tier_only() {
        let candidates = vec![owned_by("g", None), owned_by("owned", Some("acme-shop"))];
        let hits = select(&candidates, &in_project(None));
        let ids: Vec<&str> = hits.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["g"]);
    }

    // -------------------------------------------------- root-aware reach
    //
    // THE DEFECT THIS PREVENTS, measured in a real session (2026-09-08): a
    // Rule scoped to a project, anchored at a repo-relative "README.md",
    // reached a foreign clone's own README.md sitting entirely outside the
    // project - `binding_matches` compared by bare suffix
    // (`model::normalize::target_matches`) with no notion of a root at all.
    // See `absent_guard::scoped_target_matches`'s own doc comment for the
    // three rules `select` now applies through it, and that function's own
    // test module for the unit-level cases (docs/README.md, Dir containment,
    // absolute anchors, no root resolved).
    //
    // THE FIXTURE DEFECT THIS ALSO FIXES: two of the three tests below used
    // to build `input.root`/`input.targets` from a hand-typed Windows
    // literal (`PathBuf::from("C:/repo")`, `"C:/elsewhere/scratch/
    // README.md"`), which is only actually rooted per `Path::has_root`/
    // `PathBuf::join` on Windows - "thor2 / linux (default)" went red on
    // commit 0c9aea8 over exactly this. See `scoped_target_matches_tests`'s
    // own module comment in `absent_guard.rs` for the full mechanism; every
    // root/target path below now comes from a real `tempfile::tempdir()`
    // instead.

    #[test]
    fn a_project_scoped_relative_path_anchor_never_reaches_a_file_outside_the_session_root() {
        let root = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let foreign_file = foreign.path().join("scratch").join("README.md");
        let mut c = owned_by("readme-rule", Some("acme-shop"));
        c.item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "README.md".to_string() }];
        let mut input = in_project(Some("acme-shop"));
        input.root = Some(root.path().to_path_buf());
        input.targets = vec![(TargetKind::Path, foreign_file.to_string_lossy().into_owned())];
        assert!(
            select(&[c], &input).is_empty(),
            "a project-scoped, repo-relative anchor must not reach a file outside its own project root"
        );
    }

    /// The same rule still reaches the exact file inside its own root - the
    /// fix narrows reach, it does not blind the rule to its own project.
    #[test]
    fn a_project_scoped_relative_path_anchor_still_reaches_the_exact_file_inside_the_session_root() {
        let root = tempfile::tempdir().unwrap();
        let mut c = owned_by("readme-rule", Some("acme-shop"));
        c.item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "README.md".to_string() }];
        let mut input = in_project(Some("acme-shop"));
        input.root = Some(root.path().to_path_buf());
        input.targets = vec![(TargetKind::Path, root.path().join("README.md").to_string_lossy().into_owned())];
        assert_eq!(select(&[c], &input).len(), 1, "the rule must still reach its own project's own file");
    }

    /// A GLOBAL rule's relative anchor keeps its old, root-blind suffix reach
    /// outside the session root - there is no root to resolve it against,
    /// and a global rule about "README.md" is meant everywhere, not only
    /// inside one checkout.
    #[test]
    fn a_global_relative_path_anchor_still_reaches_a_file_outside_the_session_root() {
        let root = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let foreign_file = foreign.path().join("scratch").join("README.md");
        let mut c = owned_by("readme-rule", None);
        c.item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "README.md".to_string() }];
        let mut input = in_project(Some("acme-shop"));
        input.root = Some(root.path().to_path_buf());
        input.targets = vec![(TargetKind::Path, foreign_file.to_string_lossy().into_owned())];
        assert_eq!(select(&[c], &input).len(), 1, "a global rule must keep reaching outside the session root");
    }

    // --------------------------------------------------------- eligibility

    #[test]
    fn a_report_can_never_reach_a_gate_even_bypassing_the_write_gate() {
        // The write gate already refuses a Report with a binding at declare
        // time; this constructs one directly (as if that check had never
        // run), WITH a binding that genuinely matches the input, to prove
        // SELECT is a second, independent lock on the door by KIND - not
        // merely a side effect of the binding never matching either.
        let mut c = base("r1", Kind::Report);
        c.item.bindings = vec![Binding::Moment(Action::Configure)];
        let hits = select(&[c], &input_matching_base());
        assert!(hits.is_empty(), "a Report must never be selected, gate or no gate");
    }

    #[test]
    fn a_chunk_can_never_reach_a_gate_even_bypassing_the_write_gate() {
        // Same guarantee as `a_report_can_never_reach_a_gate_even_bypassing_the_write_gate`,
        // for the new archive kind: the write gate already refuses a Chunk
        // with a binding, but this constructs one directly (as if that check
        // had never run), with a binding that genuinely matches, to prove
        // SELECT is a second, independent lock on the door by KIND.
        let mut c = base("k1", Kind::Chunk);
        c.item.bindings = vec![Binding::Moment(Action::Configure)];
        let hits = select(&[c], &input_matching_base());
        assert!(hits.is_empty(), "a Chunk must never be selected, gate or no gate");
    }

    #[test]
    fn a_lookup_answers_only_its_own_key_never_a_moment_or_target() {
        let mut c = base("l1", Kind::Lookup);
        c.item.key = Some("k".to_string());
        c.item.bindings = vec![Binding::Moment(Action::Configure)];
        let hits = select(&[c], &input_matching_base());
        assert!(hits.is_empty(), "a Lookup must not be reachable through moment/target selection");
    }

    #[test]
    fn an_item_bound_only_to_always_is_never_selected_by_moment_or_prompt_select() {
        // The defect this test names: `Always` used to fire on ANY input
        // here (the old behaviour `serve::session_start` now replaces),
        // which meant the "moment of action" and "session start" surfaces
        // silently served the same items - exactly the overlap CONTRACT.md's
        // five-surfaces rule forbids. `Always` is exclusively surface 1's now
        // (see `crate::session_start`); this input DOES match the item's
        // kind/eligibility, so if this test ever goes green for the wrong
        // reason, `an_item_bound_to_both_always_and_a_real_moment_still_fires_through_the_moment`
        // right below catches it (proves the same item DOES fire through a
        // real binding).
        let mut c = base("x1", Kind::Rule);
        c.item.bindings = vec![Binding::Always];
        let hits = select(&[c], &input_matching_base());
        assert!(hits.is_empty(), "an Always-only binding must never be selected outside session start");
    }

    #[test]
    fn an_item_bound_to_both_always_and_a_real_moment_still_fires_through_the_moment() {
        // The other half of the guarantee above: excluding `Always` from
        // matching must not poison the REST of an item's bindings - an item
        // bound to both `Always` (for session start) and a real `Moment`
        // (for the moment/prompt surfaces) still resolves through that
        // second binding exactly as if `Always` were not there at all.
        let mut c = base("x1b", Kind::Rule);
        c.item.bindings = vec![Binding::Always, Binding::Moment(Action::Configure)];
        let hits = select(&[c], &input_matching_base());
        assert_eq!(hits.len(), 1, "the Moment binding must still resolve regardless of the Always sibling");
    }

    #[test]
    fn a_moment_binding_only_fires_on_its_own_action() {
        let mut c = base("x2", Kind::Orientation);
        c.item.bindings = vec![Binding::Moment(Action::Push)];
        let miss = ServeInput {
            moments: vec![Action::Commit],
            targets: vec![],
            context: String::new(),
            project: None,
            command: None,
            file: None,
            root: None,
            shell_call: false,
        };
        assert!(select(&[c.clone_for_test()], &miss).is_empty());
        let hit = ServeInput {
            moments: vec![Action::Push],
            targets: vec![],
            context: String::new(),
            project: None,
            command: None,
            file: None,
            root: None,
            shell_call: false,
        };
        assert_eq!(select(&[c], &hit).len(), 1);
    }

    impl LiveItem {
        fn clone_for_test(&self) -> LiveItem {
            LiveItem { id: self.id.clone(), item: self.item.clone() }
        }
    }

    #[test]
    fn a_target_binding_matches_by_full_normalized_value() {
        let mut c = base("x3", Kind::Orientation);
        c.item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: r"Src\Main.rs".to_string() }];
        let input = ServeInput {
            moments: vec![],
            targets: vec![(TargetKind::Path, "src/main.rs".to_string())],
            context: String::new(),
            project: None,
            command: None,
            file: None,
            root: None,
            shell_call: false,
        };
        assert_eq!(select(&[c], &input).len(), 1);
    }

    #[test]
    fn a_target_bound_by_full_path_still_fires_on_its_bare_name() {
        // The defect this test names: an item declared against its full path
        // (as the migration's real data does) must still fire when the touched
        // target is given only by its last segment, and vice versa - the same
        // "same target" doctrine `normalize::last_segment` documents.
        let mut c = base("x4", Kind::Orientation);
        c.item.bindings =
            vec![Binding::Target { kind: TargetKind::Path, value: r"C:\repo\swap-binary.ps1".to_string() }];
        let input = ServeInput {
            moments: vec![],
            targets: vec![(TargetKind::Path, "swap-binary.ps1".to_string())],
            context: String::new(),
            project: None,
            command: None,
            file: None,
            root: None,
            shell_call: false,
        };
        assert_eq!(select(&[c], &input).len(), 1);
    }

    /// THE DEFECT, reproduced against the real store on 2026-08-14 before it
    /// was fixed here: a Command binding was compared the way a path is, so a
    /// rule anchored to "gh repo edit" applied to a bare `gh repo edit` and
    /// was SILENT for `gh repo edit <repo> --visibility public` - the only
    /// form anyone actually types, and the one the rule exists for. A whole
    /// binding kind that does not fire at its own moment is the worst failure
    /// this system has, because nothing reports it.
    #[test]
    fn a_command_binding_fires_on_the_real_invocation_not_only_the_bare_words() {
        let mut c = base("x4b", Kind::Rule);
        c.item.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "gh repo edit".to_string() }];
        let with_args = ServeInput {
            moments: vec![],
            targets: vec![(TargetKind::Command, "gh repo edit nworks3d/x --visibility public".to_string())],
            context: String::new(),
            project: None,
            command: None,
            file: None,
            root: None,
            shell_call: false,
        };
        assert_eq!(select(&[c.clone_for_test()], &with_args).len(), 1, "the arguments are the normal case");

        // And the narrowness the guard arm already proved: a different
        // subcommand is a different command, and a mention is not a run.
        for miss in ["gh repo view nworks3d/x", "echo remember to gh repo edit later"] {
            let input = ServeInput {
                moments: vec![],
                targets: vec![(TargetKind::Command, miss.to_string())],
                context: String::new(),
                project: None,
                command: None,
                file: None,
                root: None,
                shell_call: false,
            };
            assert!(select(&[c.clone_for_test()], &input).is_empty(), "must not fire on: {miss}");
        }
    }

    /// THE DOOR (2026-09-28): serving reads the same segments the guards do,
    /// so a rule anchored to a command is served when that command is ANY
    /// segment of the call - after a `cd`, behind a pipe, inside `$(...)` -
    /// and still not for a mere mention in an `echo`.
    #[test]
    fn a_command_binding_fires_on_every_segment_of_a_compound_call() {
        let mut c = base("x4c", Kind::Rule);
        c.item.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "gh repo edit".to_string() }];
        let input_for = |command: &str| ServeInput {
            moments: vec![],
            targets: vec![(TargetKind::Command, command.to_string())],
            context: String::new(),
            project: None,
            command: None,
            file: None,
            root: None,
            shell_call: false,
        };
        for hit in [
            "cd repo && gh repo edit acme/x --visibility public",
            "git status | head -3; gh repo edit acme/x --description y",
            "x=$(gh repo edit acme/x --visibility public)",
        ] {
            assert_eq!(select(&[c.clone_for_test()], &input_for(hit)).len(), 1, "must fire on: {hit}");
        }
        for miss in ["echo gh repo edit acme/x; ls", "cd repo && gh repo view acme/x"] {
            assert!(select(&[c.clone_for_test()], &input_for(miss)).is_empty(), "must not fire on: {miss}");
        }
    }

    #[test]
    fn a_target_binding_never_matches_a_different_kind() {
        let mut c = base("x5", Kind::Orientation);
        c.item.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "main.rs".to_string() }];
        let input = ServeInput {
            moments: vec![],
            targets: vec![(TargetKind::Path, "main.rs".to_string())],
            context: String::new(),
            project: None,
            command: None,
            file: None,
            root: None,
            shell_call: false,
        };
        assert!(select(&[c], &input).is_empty(), "a Command binding must not answer a Path target");
    }

    // --------------------------------------------------------- ranking

    #[test]
    fn heaviest_severity_ranks_first() {
        let mut irr = base("i", Kind::Rule);
        irr.item.severity = Some(Severity::Irreversible);
        let mut costly = base("c", Kind::Rule);
        costly.item.severity = Some(Severity::Costly);
        let hits = select(&[costly, irr], &input_matching_base());
        assert_eq!(hits[0].id, "i");
        assert_eq!(hits[1].id, "c");
    }

    #[test]
    fn a_missing_severity_never_ranks_as_a_middle_value() {
        // Derived Ord on Option<Severity> would put None FIRST (ahead of
        // Irreversible). This proves a severity-less item sinks BELOW every
        // severity, not above Irreversible and not between Costly and
        // HouseStyle either.
        let mut irr = base("i", Kind::Rule);
        irr.item.severity = Some(Severity::Irreversible);
        let mut house = base("h", Kind::Rule);
        house.item.severity = Some(Severity::HouseStyle);
        let mut none = base("n", Kind::Rule);
        none.item.severity = None;
        let hits = select(&[none, house, irr], &input_matching_base());
        assert_eq!(hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), vec!["i", "h", "n"]);
    }

    #[test]
    fn an_item_naming_what_actually_happened_beats_a_generic_item() {
        let mut generic = base("g", Kind::Rule);
        generic.item.severity = Some(Severity::Costly);
        generic.item.text = "be careful with deployments in general".to_string();
        let mut specific = base("s", Kind::Rule);
        specific.item.severity = Some(Severity::Costly);
        specific.item.text = "docker-compose.yml controls the payment-gateway container".to_string();
        let input = ServeInput {
            // Both items carry base()'s default Moment(Configure) binding, so
            // this must still resolve it to get either item selected at all -
            // only the CLOSENESS tiebreaker below is what this test is
            // actually about.
            moments: vec![Action::Configure],
            targets: vec![],
            context: "docker compose -f docker-compose.yml up payment-gateway".to_string(),
            project: None,
            command: None,
            file: None,
            root: None,
            shell_call: false,
        };
        let hits = select(&[generic, specific], &input);
        assert_eq!(hits[0].id, "s", "the item naming the real command/path must rank first");
    }

    /// THE INVERSION THIS FIXES, measured on the owner's real store
    /// (2026-08-09): opening the busiest file in it showed four general
    /// warnings about deploying and NONE of the twelve facts about that very
    /// file. Severity decided everything and closeness was never consulted
    /// across bands, so a rule bound to a broad moment always beat one
    /// anchored at the file in your hands. The general rule still reaches you
    /// at the moment it is about; the specific one had only that one chance.
    #[test]
    fn a_fact_anchored_at_this_file_beats_a_general_one_of_the_same_weight() {
        let mut anchored = base("anchored", Kind::Rule);
        anchored.item.severity = Some(Severity::Costly);
        anchored.item.bindings =
            vec![Binding::Target { kind: TargetKind::Path, value: "deploy/compose.yml".to_string() }];

        let mut general = base("general", Kind::Rule);
        general.item.severity = Some(Severity::Costly);
        general.item.bindings = vec![Binding::Moment(Action::Deploy)];

        let mut input = ServeInput::default();
        input.add_file("deploy/compose.yml");
        input.moments.push(Action::Deploy);

        let hits = select(&[general, anchored], &input);
        assert_eq!(hits[0].id, "anchored", "the fact about this very file must come first");
    }

    /// AND THE LINE THAT MAKES THAT SAFE. An irreversible warning keeps the top
    /// band whatever else is true: measured before shipping the rule above,
    /// plain closeness-first would have pushed six irreversible warnings out of
    /// their place on this store. A memory that shows the handy note instead of
    /// the one that stops you wrecking production is worse than one that shows
    /// neither.
    #[test]
    fn an_irreversible_warning_is_never_displaced_by_a_closer_lighter_one() {
        let mut anchored = base("anchored-light", Kind::Rule);
        anchored.item.severity = Some(Severity::HouseStyle);
        anchored.item.bindings =
            vec![Binding::Target { kind: TargetKind::Path, value: "deploy/compose.yml".to_string() }];

        let mut hard = base("hard-general", Kind::Rule);
        hard.item.severity = Some(Severity::Irreversible);
        hard.item.bindings = vec![Binding::Moment(Action::Deploy)];

        let mut input = ServeInput::default();
        input.add_file("deploy/compose.yml");
        input.moments.push(Action::Deploy);

        let hits = select(&[anchored, hard], &input);
        assert_eq!(hits[0].id, "hard-general", "nothing lighter may take an irreversible warning's place");
    }

    #[test]
    fn ranking_never_falls_back_to_sentence_length() {
        // Named after the defect it prevents: an earlier version silently
        // built a length lottery into this exact tiebreaker, and it dropped
        // six of ten irreversible rules by length alone. Ten irreversible
        // items, identical severity, identical (zero) closeness, varying only
        // in text length - the cut must never correlate with length.
        let mut items = Vec::new();
        for i in 0..10 {
            let mut c = base(&format!("item-{i}"), Kind::Rule);
            c.item.severity = Some(Severity::Irreversible);
            // deliberately alternate short/long so a length-based sort would
            // reorder these away from plain id order
            c.item.text = if i % 2 == 0 { "short".repeat(1) } else { "long text ".repeat(20) };
            items.push(c);
        }
        let hits = select(&items, &input_matching_base());
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        let mut expected: Vec<String> = (0..10).map(|i| format!("item-{i}")).collect();
        expected.sort();
        let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
        assert_eq!(ids, expected, "tie order must be id order, never correlated with text length");
    }

    #[test]
    fn target_matches_is_the_only_definition_used_here() {
        // Pins the single-normalization doctrine (R6) locally: target_matches
        // must go through model::normalize, never re-implement comparison.
        assert!(target_matches(TargetKind::Path, r"A\B.rs", TargetKind::Path, "a/b.rs"));
        assert!(!target_matches(TargetKind::Path, "a/b.rs", TargetKind::Symbol, "a/b.rs"));
    }

    // ------------------------------------- step 32: read-only calls and file anchors

    fn fact_at(id: &str, kind: Kind, path: &str, text: &str) -> LiveItem {
        let mut c = base(id, kind);
        c.item.text = text.to_string();
        c.item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: path.to_string() }];
        c
    }

    fn running(command: &str) -> ServeInput {
        let mut input = ServeInput::default();
        input.add_command(command);
        input
    }

    fn ids(hits: Vec<RankedItem>) -> Vec<String> {
        hits.into_iter().map(|h| h.id).collect()
    }

    /// THE DEFECT (field test 7): a read-only `cat server/package.json | grep
    /// test` served facts about changing that file.
    #[test]
    fn a_read_only_call_does_not_serve_advice_about_changing_the_file_it_reads() {
        let facts = [
            fact_at("bump", Kind::Rule, "server/package.json", "Bump the version with the set-version script."),
            fact_at("what", Kind::Orientation, "server/package.json", "The suite version lives in this file."),
        ];
        assert!(select(&facts, &running("cat server/package.json | grep -n test")).is_empty());
        assert!(select(&facts, &running("git diff --stat server/package.json")).is_empty());
        // The same file, changed: both come back.
        assert_eq!(ids(select(&facts, &running("sed -i s/1/2/ server/package.json"))).len(), 2);
        assert_eq!(ids(select(&facts, &running("cat server/package.json > copy.json"))).len(), 2);
        assert_eq!(ids(select(&facts, &running("npm install left-pad && cat server/package.json"))).len(), 2);
    }

    /// A fact that is about reading - it names the very tool - still arrives
    /// (the 2026-08-07 grep trap in a log file).
    #[test]
    fn a_read_only_call_still_serves_a_fact_that_names_the_reader() {
        let facts = [fact_at("trap", Kind::Rule, "logs/app.log", "A grep on this log misses lines with a NUL byte; use rg -a.")];
        assert_eq!(ids(select(&facts, &running("grep -n error logs/app.log"))), vec!["trap"]);
        // Another reader is not named, so it stays quiet.
        assert!(select(&facts, &running("tail -n 5 logs/app.log")).is_empty());
    }

    /// Only a place-only match is held back: a fact reached through its command
    /// anchor, a moment or a host is served as before.
    #[test]
    fn a_read_only_call_serves_what_another_binding_reached() {
        let mut by_command = fact_at("cmd", Kind::Rule, "server/package.json", "Bump the version.");
        by_command.item.bindings.push(Binding::Target { kind: TargetKind::Command, value: "cat".to_string() });
        assert_eq!(ids(select(&[by_command], &running("cat server/package.json"))), vec!["cmd"]);
    }

    /// The other half of the report: an anchor on package-lock.json must never
    /// be reached by package.json (and the reverse), read-only or not.
    #[test]
    fn a_lock_file_anchor_is_not_reached_by_the_manifest_next_to_it() {
        let lock = || fact_at("lock", Kind::Rule, "server/package-lock.json", "Regenerate with npm install.");
        let manifest = fact_at("manifest", Kind::Rule, "server/package.json", "Bump the version.");
        assert!(select(&[lock()], &running("sed -i s/1/2/ server/package.json")).is_empty());
        assert!(select(&[manifest], &running("sed -i s/1/2/ server/package-lock.json")).is_empty());
        assert_eq!(ids(select(&[lock()], &running("sed -i s/1/2/ server/package-lock.json"))), vec!["lock"]);
    }

    /// The Grep and Glob TOOLS are offered as a tool name, not as a shell
    /// command: a search of the place a fact is anchored at still draws it
    /// (measured 2026-09-26), whatever the fact says.
    #[test]
    fn a_grep_tool_call_is_not_a_read_only_shell_call() {
        let facts = [fact_at("anchored", Kind::Orientation, "src/lib.rs", "This file is generated.")];
        let mut input = ServeInput::default();
        input.add_tool_name("Grep");
        input.add_target(TargetKind::Path, "src/lib.rs");
        assert_eq!(ids(select(&facts, &input)), vec!["anchored"]);
    }
}
