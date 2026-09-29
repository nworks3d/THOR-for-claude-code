//! Does this rule's check actually refuse anything? Answered by trying it,
//! not by reading it.
//!
//! THE DEFECT THIS CLOSES, named by the owner on 2026-08-09: "the gate must
//! not just store something as important - it has to be PROVEN working."
//!
//! Until now a rule counted as armed the moment somebody wrote a check on it.
//! Nothing ever ran that check against a write it claimed to stop, so a check
//! could be shaped wrong - a forbidden literal on a binding the guard's
//! command arm never reads, a literal with a typo in it - and the rule would
//! sit in the store looking like protection, be counted as protection by
//! `doctor`, and refuse nothing for the rest of its life. That is worse than
//! no check at all: it is cover you believe in.
//!
//! So a claim now has to survive being tried: the forbidden fragment goes
//! literally into a command or into file content and is handed to the same
//! guard arms the hook uses.
//!
//! WHAT THAT DOES AND DOES NOT SHOW, because the first version of this file
//! overclaimed and a test caught it. The attempt is BUILT from the literal, so
//! the literal is always present in it by construction. That makes this a
//! proof of REACH - the check is wired so that a real command carrying this
//! fragment would be refused - and never a proof that the fragment is the
//! right one. A literal with a typo in it is wired perfectly and guards
//! nothing, and no machine here can tell: only the person who knows the real
//! command can. Reach is still worth proving, because the shape that fails it
//! is the one that looks strongest and can never fire.
//!
//! SCOPE, WIDENED 2026-09-24 (the second half of this file's own defect: a
//! heavy fact, or one carrying a check that COULD refuse, being stored with
//! nobody ever having tried it, and the write receipt saying only that it was
//! stored). Four check kinds are now genuinely tried: `Forbidden` (as
//! before), plus `Absent`/`AbsentAll` (a real root is required - see
//! `test_anchored`) and `Requires` (its own reach is always a `Command`
//! binding - `model::gate`'s own ground 23 forces this at write time, so
//! there is no "neither reach exists" shape for this kind the way there is
//! for `Forbidden`). `PathExists`/`Contains` stay unproven here on purpose:
//! neither is a REFUSING check in this project's own vocabulary (see
//! `teeth_box` below) - `Contains` only ever protects a line already there
//! from disappearing, and `PathExists` blocks nothing on its own at all (see
//! `absent_guard::location_anchor`'s own doc comment for what actually turns
//! IT into a block).
//!
//! `teeth_box` (below `prove`/`Proof`) is the write-time POLICY this file now
//! also carries: whether a fact even OWES a proof, and which of the two
//! boxes, "proven guard" or "informing", it falls into once tried.
//! `prove`/`Proof` stay the mechanism; `teeth_box`/`TeethBox` are the
//! decision built on top of it. See `TeethBox`'s own doc comment for the
//! boxes themselves, and `model::store::TEETH_TAG_PREFIX` for how the
//! decision becomes a label nothing but this tool ever writes - the caller
//! (`mcp::remember`/`mcp::revise`) is what actually appends it; this module
//! only ever decides which one.
//!
//! TWO SIDES, NOT ONE (2026-09-24, the same day, continued). Everything
//! above this paragraph proves REACH, and reach alone was measured to be
//! almost free: the synthesised payload is BUILT from the literal, so it
//! always carries it, and once any reach exists at all the guard cannot help
//! but refuse it. A one-sided test built this way can never tell a working
//! guard apart from a rule that refuses EVERYTHING reaching its own anchor,
//! forbidden literal or not - and a rule that refuses legitimate work is the
//! most expensive failure this system has: it teaches people to work around
//! the tool, and that does not come back.
//!
//! So `ProvenGuard` now needs BOTH sides to hold. The FORBIDDEN form is what
//! this file already built: the anchor carrying one of its own literals - see
//! `prove`. The CLEAN form is new: the identical anchor, with NONE of the
//! check's own literals anywhere in it - see `prove_clean`, built from the
//! anchor ALONE, never by editing a literal into something that happens to
//! dodge the match (editing the literal would prove the edit, not the rule).
//! The guard must refuse the forbidden form and let the clean one through;
//! only then is the fact a proven guard. When it refuses BOTH,
//! `TeethBox::RefusesLegitimateUse` says so plainly - never filed as proven,
//! and never filed quietly as informing either, because silence there is
//! exactly the failure this paragraph opened with. The write still goes
//! through either way: only the label changes, exactly as before.
//!
//! THE COLLAPSED CASE. A fact carrying a refusing-kind check can have an
//! anchor that already CONTAINS one of that same check's own literals - an
//! anchor "git push --force" whose own literal is "--force", for example,
//! or an anchor that simply IS the literal. For those there is no
//! literal-free form of the anchor left to build at all, so the two-sided
//! test cannot run -
//! `collapsed_literal` catches this BEFORE any payload is built or any guard
//! is called, and the fact is filed as `TeethBox::AnchorCollapsed`, never
//! dressed up as proven. This is a structural fact about the anchor, not a
//! probe result: it is exactly as indistinguishable from an over-blocking
//! rule as `RefusesLegitimateUse` is, it just gets there without running
//! anything.
//!
//! WHAT STAYS ONE-SIDED, AND WHY NEITHER IS A GAP. `Requires` INVERTS which
//! side is the bad one: the forbidden form is the trigger with NONE of
//! `required` present (the thing forgotten), the clean form is the trigger
//! WITH one of `required` present (the thing satisfied) - see
//! `prove_requires_clean`. Its own trigger/anchor text is never scanned for
//! `required` literals (only the call's own flattened INPUT is - see
//! `absent_guard::find_requires_violation`'s own doc comment on the
//! requirement side), so the two sides are always built from independent
//! text and `Requires` can never collapse. An `Always`-bound `Forbidden`
//! check has no anchor at all to collapse either - see `absent_guard`'s own
//! doc comment ("a rule forbidding a character names nothing outside
//! itself"); its clean form is simply empty content, which cannot carry a
//! non-empty literal by construction.

use crate::teeth;
use model::item::{Binding, Check, Item, Severity, TargetKind};
use serde_json::json;
use std::path::{Path, PathBuf};
use thor_core::event_store::{EventKind, EventStore};

// ------------------------------------------------------------- against reality
//
// THE DEFECT THIS CLOSES (2026-09-26). Everything below this section proves
// REACH: an attempt is built out of the rule's own anchor and its own
// literal, and handed to the guard. That can never tell a real tooth from an
// invented one. Measured on a copy of a real store: a rule about
// repartitioning a disk was given the forbidden literal "zqxj-never-typed"
// - a string no command will ever carry - and came back a proven guard,
// because the attempt built from that literal carried it by construction.
// The doctor's count of proven guards rose by four that way without a single
// real tooth being added.
//
// So a guard is PROVEN only against something the rule's author did not
// write for the purpose: a REAL REFUSAL - the real hook refused a real call
// under this check, in a Claude Code session whose own transcript holds that
// very call and the block it left (`serve::attest::is_field_refusal`, the
// one predicate, K6). For a check that reads a command, the recorded call is
// tried again against the check as it stands now, so a refusal earned by an
// older literal never vouches for a newer one. For a check on a file, whose
// written content the log does not keep, the refusal only counts when it
// came after the current check was set.
//
// A SERVED CALL NEVER PROVES ANYTHING (2026-09-28, N12). A call a rule was
// served on used to count too, when the current check would refuse it. A
// hand-fed payload makes served events as easily as a real session does, so
// that door is closed: only an attested refusal is evidence.
//
// A check that reaches correctly both ways but has never met a real refusal
// is `TeethBox::Wired`: honest about being wired, and never counted as
// proven.

/// What the log says one item's check has met in the real world - see this
/// section's own comment. Built by `reality`; `Reality::default()` is an item
/// the log knows nothing about (a brand-new write), which can never be proven.
#[derive(Debug, Clone, Default)]
pub struct Reality {
    /// Every attested refusal of this item after its current check was set
    /// (`serve::attest::is_attested_refusal`): (seq, the refused target).
    pub refusals: Vec<(i64, String)>,
    /// The seq from which the check being judged has stood unchanged in the
    /// log - `i64::MAX` when that check is not the one the log holds (a revise
    /// that is about to change it, or a fresh write).
    pub check_since: i64,
}

/// Read `id`'s real record from the log, judged for `check` - the check about
/// to be stored, which may differ from the one the log holds. Only refusals
/// that pass the attested half of the one predicate count
/// (`serve::attest::is_attested_refusal`); `bitten` tries each against the
/// check itself. Fails open to "nothing on record", never to evidence: an
/// unreadable log proves nothing.
pub fn reality(store: &EventStore, id: &str, check: Option<&Check>) -> Reality {
    let Ok(events) = store.get_events_by_entity(id) else {
        return Reality { check_since: i64::MAX, ..Default::default() };
    };
    let check_since = crate::attest::check_since(&events, check);
    let verdicts = model::store::recorded_refusal_verdicts(&events);
    let refusals = events
        .iter()
        .filter(|e| e.kind == EventKind::GateRefused)
        .filter(|e| crate::attest::is_attested_refusal(store, e, check_since, &verdicts))
        .filter_map(|e| {
            let target = serde_json::from_str::<serde_json::Value>(&e.body)
                .ok()
                .and_then(|v| v.get("target").and_then(|t| t.as_str()).map(str::to_string))?;
            (!target.trim().is_empty()).then_some((e.seq, target))
        })
        .collect();
    Reality { refusals, check_since }
}

/// A folder no trial depends on: a command check is tried here when this
/// module has no checkout to try it in, never in the process's own working
/// folder.
fn neutral_root() -> PathBuf {
    std::env::temp_dir()
}

/// Whether `item`'s CURRENT check would refuse the real call `text` - through
/// the hook's own pipeline (`teeth::refuses_call`), never a second matcher.
/// `None` when the check does not read a call at all (a file check), so a
/// caller knows the question could not be asked rather than reading it as
/// "let through".
fn refuses_real_call(id: &str, item: &Item, text: &str) -> Option<bool> {
    match (&item.check, forbidden_reach(item)) {
        (Some(Check::Forbidden { .. }), ForbiddenReach::Command(_)) | (Some(Check::Requires { .. }), _) => {
            Some(teeth::refuses_call(id, item, text, &neutral_root()))
        }
        _ => None,
    }
}

/// The real-world evidence that `item`'s current check bites, or `None` - see
/// this section's own comment for the one kind that counts and why nothing
/// else does. The returned sentence names the exact call and its log seq, so
/// a reader can disagree with it.
pub fn bitten(id: &str, item: &Item, reality: &Reality) -> Option<String> {
    item.check.as_ref()?;
    for (seq, target) in &reality.refusals {
        match refuses_real_call(id, item, target) {
            Some(true) => {
                return Some(format!("the real hook refused `{target}` (log seq {seq}), and this check still refuses it"));
            }
            Some(false) => {}
            None if *seq > reality.check_since => {
                return Some(format!("the real hook refused a write to {target} (log seq {seq}) under this same check"));
            }
            None => {}
        }
    }
    None
}

/// The commit where `literal` last entered or left `path` in the checkout at
/// `root` (`git log -S<literal> --format=%h -1 -- <path>`): the literal really
/// lived in that file, so an absent check naming it is about a real mistake.
/// Sets the proven label only, never settlement (Q4). `None` when git finds
/// nothing, or cannot run.
pub fn lived_in_history(root: &Path, path: &str, literal: &str) -> Option<String> {
    if literal.is_empty() {
        return None;
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", &format!("-S{literal}"), "--format=%h", "-1", "--", path])
        .output()
        .ok()?;
    let commit = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !commit.is_empty()).then_some(commit)
}

/// What trying the check actually showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proof {
    /// The guard refused the attempt. Carries the attempt that was refused,
    /// so a caller can say what was proven rather than assert that something
    /// was.
    Fires(String),
    /// The guard let the attempt through. Carries what was tried, because the
    /// attempt is the evidence and a reader needs to see it to fix the rule.
    Silent(String),
    /// An anchored check (`Absent`/`AbsentAll`) whose own path does not
    /// resolve on disk RIGHT NOW, relative to the root it was tried against -
    /// a DIFFERENT outcome from `Silent`, on purpose: `Silent` means the
    /// check was actually run and declined to refuse (a "wrong anchor" in
    /// the sense that this item's own binding never reaches a write to its
    /// check's own path); this means the check could not even be RUN,
    /// because the one thing it is anchored to is not there right now.
    /// Carries the check's own path.
    AnchorMissing(String),
    /// Nothing to try: no check, a kind that cannot be tried without a
    /// working copy (`PathExists`/`Contains` - see this module's own doc
    /// comment on scope; `serve::teeth` tries those against the registered
    /// working copy), an empty literal/command-binding set that should
    /// never reach a live item in the first place, or an anchored check with
    /// no root to resolve against. Never dressed up as any of the other three.
    NotProvable(&'static str),
}

/// Try the rule's own check against an attempt it claims to forbid. `root` is
/// where an anchored check's own path is resolved (`Absent`/`AbsentAll`
/// only - `Forbidden`/`Requires` need none, see this module's own doc
/// comment); `None` means an anchored check can never be tried, exactly like
/// every other anchored proof in this crate when no root was ever resolved.
pub fn prove(id: &str, item: &Item, root: Option<&Path>) -> Proof {
    match &item.check {
        Some(Check::Forbidden { literals }) => prove_forbidden(id, item, literals),
        Some(Check::Absent { path, literal }) => test_anchored(id, item, path, std::slice::from_ref(literal), root),
        Some(Check::AbsentAll { path, literals }) => test_anchored(id, item, path, literals, root),
        Some(Check::Requires { .. }) => prove_requires(id, item),
        Some(Check::PathExists { .. }) | Some(Check::Contains { .. }) => {
            Proof::NotProvable("this check reads a file, so it can only be proven against a real working copy")
        }
        None => Proof::NotProvable("this rule carries no check"),
    }
}

/// Where a `Forbidden` check reaches, if anywhere - read off `item`'s own
/// bindings once and shared by BOTH sides of the teeth test
/// (`prove_forbidden`/`prove_forbidden_clean`), so the two sides can never
/// silently disagree about which binding they are trying. Command wins over
/// Always when an item somehow carries both.
enum ForbiddenReach<'a> {
    Command(&'a str),
    Always,
    /// The shape that looks strongest and can never fire: no command target,
    /// no always binding.
    Neither,
}

fn forbidden_reach(item: &Item) -> ForbiddenReach<'_> {
    for binding in &item.bindings {
        if let Binding::Target { kind: TargetKind::Command, value } = binding {
            return ForbiddenReach::Command(value.as_str());
        }
    }
    if item.bindings.iter().any(|b| matches!(b, Binding::Always)) {
        return ForbiddenReach::Always;
    }
    ForbiddenReach::Neither
}

/// `item` under `id`, the form every trial here runs through the pipeline.
fn named(id: &str, item: &Item) -> Item {
    let mut item = item.clone();
    item.id = id.to_string();
    item
}

/// Try a `Forbidden` check's real guard against ONE payload built for
/// `reach` - `appended` is the text to carry alongside the anchor: `Some`
/// literal for the FORBIDDEN form (`prove_forbidden`), `None` for the CLEAN
/// form (`prove_forbidden_clean`), which is the anchor with nothing added at
/// all. Every attempt goes through the hook's own pipeline
/// (`teeth::pipeline_refuses`), never a guard arm called directly.
fn try_forbidden(id: &str, item: &Item, reach: &ForbiddenReach<'_>, appended: Option<&str>) -> Proof {
    let item = named(id, item);
    match reach {
        ForbiddenReach::Command(value) => {
            let attempt = match appended {
                Some(literal) => format!("{value} {literal}"),
                None => (*value).to_string(),
            };
            if teeth::pipeline_refuses(&item, "Bash", &json!({ "command": attempt }), &neutral_root()) {
                Proof::Fires(attempt)
            } else {
                Proof::Silent(attempt)
            }
        }
        ForbiddenReach::Always => {
            // No anchor exists for an Always binding at all (see this file's
            // own top-of-file doc comment) - the clean form is simply empty
            // content, never a literal, and never derived from one.
            let attempt = appended.unwrap_or("").to_string();
            let tried = format!("writing {attempt:?} into a file");
            let file = neutral_root().join("thor-trial.txt");
            let input = json!({ "file_path": file.to_string_lossy(), "content": attempt });
            if teeth::pipeline_refuses(&item, "Write", &input, &neutral_root()) {
                Proof::Fires(tried)
            } else {
                Proof::Silent(tried)
            }
        }
        ForbiddenReach::Neither => Proof::NotProvable(
            "a forbidden check reaches only through an always binding or a command target, and this rule has neither",
        ),
    }
}

/// The `Forbidden` half of `prove`, the FORBIDDEN side of the two-sided teeth
/// test: the anchor carrying its own first literal.
fn prove_forbidden(id: &str, item: &Item, literals: &[String]) -> Proof {
    let Some(literal) = literals.first() else {
        return Proof::NotProvable("the forbidden set is empty");
    };
    try_forbidden(id, item, &forbidden_reach(item), Some(literal.as_str()))
}

/// The CLEAN side: the identical anchor `prove_forbidden` just tried, with
/// none of the check's own literals appended - never called when the anchor
/// itself already contains one (see `teeth_box`'s own collapse check, tried
/// first and always, before either side of this module ever runs).
fn prove_forbidden_clean(id: &str, item: &Item) -> Proof {
    try_forbidden(id, item, &forbidden_reach(item), None)
}

/// The item's own first `Command` binding value - the trigger a `Requires`
/// check is tried against, on EITHER side.
fn requires_anchor(item: &Item) -> Option<&str> {
    item.bindings.iter().find_map(|b| match b {
        Binding::Target { kind: TargetKind::Command, value } => Some(value.as_str()),
        _ => None,
    })
}

/// Whether the pipeline refuses the shell call `command` for `item`.
fn requires_refuses(id: &str, item: &Item, command: &str) -> bool {
    teeth::pipeline_refuses(&named(id, item), "Bash", &json!({ "command": command }), &neutral_root())
}

/// The `Requires` half of `prove`, the FORBIDDEN side of the two-sided teeth
/// test: the trigger call (`teeth::requires_attempt`) with none of `required`
/// in it - the shape the check exists to catch.
fn prove_requires(id: &str, item: &Item) -> Proof {
    let Some(anchor) = requires_anchor(item) else {
        // Should be unreachable on a live item (ground 23/1 already force
        // this shape at write time), but this module never dresses up a
        // guess as a proof - see `Proof::NotProvable`'s own doc comment.
        return Proof::NotProvable("a requires check has no command binding to try it against");
    };
    let attempt = teeth::requires_attempt(item, anchor);
    if requires_refuses(id, item, &attempt) {
        return Proof::Fires(attempt);
    }
    Proof::Silent(attempt)
}

/// The CLEAN side of a `Requires` check - INVERTED from every other clean
/// form in this file, and deliberately so: `Requires` is the one kind where
/// carrying a literal is the LEGITIMATE case, so the clean, should-not-refuse
/// form is the identical trigger call carrying the first `required` entry.
/// `Fires` here means the guard refused a call that satisfied its own stated
/// requirement - over-blocking, never a proof.
///
/// An INNER trigger is tried clean both ways: the call carrying the trigger
/// and the answer, and the bare binding with neither - the ordinary call on
/// that binding the literal exists to leave alone. Refusing either one is
/// over-blocking.
fn prove_requires_clean(id: &str, item: &Item, required: &[String]) -> Proof {
    let Some(anchor) = requires_anchor(item) else {
        return Proof::NotProvable("a requires check has no command binding to try it against");
    };
    let Some(satisfies) = required.first() else {
        return Proof::NotProvable("the required set is empty");
    };
    let attempt = teeth::requires_attempt(item, anchor);
    if requires_refuses(id, item, &format!("{attempt} {satisfies}")) {
        return Proof::Fires(format!("{attempt}, carrying {satisfies:?}"));
    }
    if attempt != anchor && requires_refuses(id, item, anchor) {
        return Proof::Fires(format!("{anchor}, without the trigger"));
    }
    Proof::Silent(format!("{attempt}, carrying {satisfies:?}"))
}

/// Currency plus reach, shared by BOTH sides of an `Absent`/`AbsentAll`
/// two-sided test (`test_anchored`/`test_anchored_clean`) - `path` doubles as
/// both the check's own anchor AND the file the synthesised write targets.
///
/// CURRENCY FIRST, always, via the same synthetic `PathExists` proof the
/// guard's own currency check runs: a path that does not resolve right now is
/// `Proof::AnchorMissing`, reported before reach is ever asked about.
///
/// REACH, once currency holds: a Write of `content` into `path`, through the
/// hook's own pipeline (`teeth::pipeline_refuses`), with the rule's project
/// and `root` - the same selection and the same arms a real write meets.
/// `Proof::Silent` is a "wrong anchor": the path is there, but nothing about
/// this item's own binding reaches a write to it.
fn try_anchored(id: &str, item: &Item, path: &str, content: &str, root: Option<&Path>) -> Proof {
    let Some(root) = root else {
        return Proof::NotProvable("no checkout root was resolved, so an anchored check can never be tried");
    };
    let existence = Check::PathExists { path: path.to_string() };
    if model::check::run(&existence, root) != model::check::Outcome::Holds {
        return Proof::AnchorMissing(path.to_string());
    }
    let input = json!({ "file_path": path, "content": content });
    if teeth::pipeline_refuses(&named(id, item), "Write", &input, root) {
        return Proof::Fires(format!("writing {content:?} into {path}"));
    }
    Proof::Silent(format!("writing {content:?} into {path}"))
}

/// The `Absent`/`AbsentAll` half of `prove`, the FORBIDDEN side: `literals`'
/// own first entry, written into `path` itself.
fn test_anchored(id: &str, item: &Item, path: &str, literals: &[String], root: Option<&Path>) -> Proof {
    let Some(literal) = literals.first() else {
        return Proof::NotProvable("the literal set is empty");
    };
    try_anchored(id, item, path, literal, root)
}

/// The CLEAN side: the SAME anchor `test_anchored` just tried, with the
/// path's own text written as the file's content instead of any of the
/// check's literals. Never called when the path text already contains one of
/// the check's own literals - `teeth_box` checks that FIRST, via
/// `collapsed_literal`.
fn test_anchored_clean(id: &str, item: &Item, path: &str, root: Option<&Path>) -> Proof {
    try_anchored(id, item, path, path, root)
}

/// `prove`'s own mirror for the CLEAN side: the identical dispatch over check
/// kinds, calling each kind's `_clean` sibling instead. `teeth_box` is the
/// caller, once `prove` has already shown the forbidden side fires (the
/// retired `probe-teeth` subcommand was the other; `serve retest` runs THOR's
/// trial instead).
pub fn prove_clean(id: &str, item: &Item, root: Option<&Path>) -> Proof {
    match &item.check {
        Some(Check::Forbidden { .. }) => prove_forbidden_clean(id, item),
        Some(Check::Absent { path, .. }) => test_anchored_clean(id, item, path, root),
        Some(Check::AbsentAll { path, .. }) => test_anchored_clean(id, item, path, root),
        Some(Check::Requires { required, .. }) => prove_requires_clean(id, item, required),
        Some(Check::PathExists { .. }) | Some(Check::Contains { .. }) => {
            Proof::NotProvable("this check reads a file, so it can only be proven against a real working copy")
        }
        None => Proof::NotProvable("this rule carries no check"),
    }
}

// ------------------------------------------------------- the collapsed case

/// The `(anchor, literals)` pair a fact's own check would need a literal-free
/// form of for the clean side to even be buildable - `None` for a shape that
/// either has no textual anchor at all (an `Always`-bound `Forbidden` check;
/// `Requires`, whose trigger is never scanned for `required` - see this
/// file's own top-of-file doc comment, "WHAT STAYS ONE-SIDED") or that never
/// reaches anything in the first place (`ForbiddenReach::Neither`, already
/// `KeepRefusing`'s own job).
///
/// THE ANCHOR NAMED IS EXACTLY THE ONE THE TWO-SIDED TEST WOULD BUILD A CLEAN
/// PAYLOAD FROM, never a hypothetically better one: a `Forbidden` check's
/// anchor is its first `Command` binding's own value (`forbidden_reach`, the
/// SAME lookup `prove_forbidden`/`prove_forbidden_clean` use); an `Absent`/
/// `AbsentAll` check's anchor is its own `path` (`test_anchored_clean`'s own
/// content). Checked against EVERY literal the check carries, not only the
/// first (the one the forbidden side actually tries): the real guard
/// (`absent_guard::first_present_literal`) matches on the WHOLE set, so a
/// clean form built from an anchor containing even a LATER literal would
/// still be wrongly refused by that other literal alone - indistinguishable
/// from `RefusesLegitimateUse` by any probe, which is exactly why this is
/// checked BEFORE any probe runs at all, never discovered by one.
fn anchor_and_literals_for_collapse(item: &Item) -> Option<(&str, &[String])> {
    match &item.check {
        Some(Check::Forbidden { literals }) => match forbidden_reach(item) {
            ForbiddenReach::Command(value) => Some((value, literals.as_slice())),
            ForbiddenReach::Always | ForbiddenReach::Neither => None,
        },
        Some(Check::Absent { path, literal }) => Some((path.as_str(), std::slice::from_ref(literal))),
        Some(Check::AbsentAll { path, literals }) => Some((path.as_str(), literals.as_slice())),
        _ => None,
    }
}

/// The first of `literals` that already sits inside `anchor`, if any - empty
/// literals never count (an empty string is trivially "contained" in
/// everything, which would collapse every anchor for nothing; a live item can
/// not actually carry one - `gate::check_check` already refuses an empty or
/// empty-containing literal set at write time - but this module never trusts
/// a caller it cannot see, exactly as `Proof::NotProvable`'s own doc comment
/// says of every other defensive check in this file).
fn collapsed_literal<'a>(anchor: &str, literals: &'a [String]) -> Option<&'a str> {
    literals.iter().find(|lit| !lit.is_empty() && anchor.contains(lit.as_str())).map(String::as_str)
}

// -------------------------------------------------------------- the write-time box

/// The box a fact falls into once `teeth_box` has actually tried its teeth -
/// see `model::store::TEETH_TAG_PREFIX`'s own doc comment for the label this
/// becomes on the stored item, and for why the writer can never set it.
///
/// FIVE BOXES, not six, whatever the variant count below reads: `KeepRefusing`
/// is not a box at all, it is the one write-time refusal this task's own
/// brief keeps exactly as it was (a `Forbidden` check with neither an
/// `Always` binding nor a `Command` target - see `prove_forbidden`'s own
/// "neither reach exists" arm); `NotApplicable` means Change 2 simply does
/// not apply (light, and no refusing-kind check) - nothing is tried, nothing
/// is labelled, nothing is refused. `ProvenGuard` is the one box that means
/// what it says: BOTH sides held (see this file's own top-of-file doc
/// comment, "TWO SIDES, NOT ONE"). `RefusesLegitimateUse` and
/// `AnchorCollapsed` are the two ways a check that DOES reach can still fail
/// to earn that box - refused the clean form too, or had no clean form to
/// try at all. `Informing` is the last one, reached several different ways
/// `note` tells apart in prose - tried and toothless on the forbidden side
/// alone, or nothing to try at all - because the write gate must never
/// refuse ANY of the last four: "the knowledge always gets in" holds for
/// every box but `KeepRefusing`, unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeethBox {
    /// Not heavy, and carries no refusing-kind check - this fact does not
    /// owe a proof at all.
    NotApplicable,
    /// The one shape still refused outright, unchanged from before this
    /// module also decided the other three: a `Forbidden` check that can
    /// never reach anything, whatever it looks like.
    KeepRefusing(String),
    /// PROVEN AGAINST REALITY (2026-09-26): the check has bitten a real call
    /// on record - `evidence` names it (see `bitten`) - and, where a
    /// synthesised two-sided test can be built at all, that test held too
    /// (`forbidden_tried`/`clean_tried`, `None` for a file check that only a
    /// real write can try). The only box a counter of real teeth may count.
    ProvenGuard { evidence: String, forbidden_tried: Option<String>, clean_tried: Option<String> },
    /// WIRED, NOT PROVEN (2026-09-26): BOTH synthesised sides held - the
    /// forbidden form was refused and the clean one let through - but nothing
    /// in the log shows this check ever meeting a real call. It is reach,
    /// never a tooth: the attempt was built from the rule's own literal, so a
    /// literal no command will ever carry lands here exactly as a good one
    /// does. Never counted as proven; `note` says what would prove it.
    Wired { forbidden_tried: String, clean_tried: String, note: String },
    /// THE THIRD OUTCOME (2026-09-24): the guard refused the forbidden form
    /// AND the clean, legitimate one - it cannot tell a genuine violation of
    /// this fact apart from ordinary, authorised use of the very same
    /// anchor. Never filed as `ProvenGuard` (it is not one) and never filed
    /// quietly as `Informing` either: a rule that refuses legitimate work is
    /// the most expensive failure this system has (see this file's own
    /// top-of-file doc comment) and earns its own plain label, never a
    /// footnote. The write still goes through - only the label says what was
    /// found.
    RefusesLegitimateUse { forbidden_tried: String, clean_tried: String, note: String },
    /// THE COLLAPSED CASE (2026-09-24): the fact's own anchor already
    /// contains one of the check's own literals, so no clean form free of
    /// that literal could ever be built from the anchor at all - the
    /// two-sided test never even ran (`anchor_and_literals_for_collapse`/
    /// `collapsed_literal`, tried BEFORE either side of `prove`). Exactly as
    /// indistinguishable from an over-blocking rule as `RefusesLegitimateUse`
    /// is, for a structural reason rather than a tested one - never dressed
    /// up as `ProvenGuard`.
    AnchorCollapsed { anchor: String, literal: String, note: String },
    /// Tried and let straight through on the forbidden side alone (the
    /// clean side is never even reached in that case - see `teeth_box`'s own
    /// body), or nothing to try at all - `tried` tells the two apart (`Some`
    /// for the former, `None` for the latter); `note` is the full,
    /// human-readable explanation either way, including the concrete
    /// correction a reader can act on without opening a source file.
    Informing { tried: Option<String>, note: String },
}

/// Whether `severity` alone is heavy enough that Change 2 applies regardless
/// of what the check looks like.
fn is_heavy(severity: Option<Severity>) -> bool {
    matches!(severity, Some(Severity::Irreversible) | Some(Severity::Costly))
}

/// Whether `check` is one of the four REFUSING kinds this task names -
/// `Forbidden`, `Absent`, `AbsentAll`, `Requires` - the second, independent
/// reason Change 2 applies, regardless of severity. `PathExists`/`Contains`
/// are deliberately excluded: neither refuses an INTRODUCTION the way these
/// four do (see this file's own top-of-file doc comment on scope).
fn is_refusing_kind(check: Option<&Check>) -> bool {
    matches!(
        check,
        Some(Check::Forbidden { .. }) | Some(Check::Absent { .. }) | Some(Check::AbsentAll { .. }) | Some(Check::Requires { .. })
    )
}

/// A short, concrete correction for a `Silent` (tried, toothless) outcome -
/// which anchor or which literal a reader would change, named by the check's
/// own shape, so the receipt names a fix without sending anyone to open a
/// source file. Never a claim that this IS the fix (see `prove`'s own doc
/// comment on the limit of what reach alone can prove: a typo in the literal
/// itself is invisible to this whole module) - only the one place left to
/// look.
fn correction_hint(item: &Item) -> String {
    match &item.check {
        Some(Check::Forbidden { .. }) => {
            let has_command =
                item.bindings.iter().any(|b| matches!(b, Binding::Target { kind: TargetKind::Command, .. }));
            if has_command {
                "check the command binding's own text against how the command is really typed, or the literal's own spelling".to_string()
            } else {
                "check the literal's own spelling - it does not appear in what actually gets written".to_string()
            }
        }
        Some(Check::Absent { path, .. }) | Some(Check::AbsentAll { path, .. }) => format!(
            "bind a path target equal to '{path}', or a dir target containing it - as stored, none of this item's own bindings reach a write to its own check_path"
        ),
        Some(Check::Requires { when, .. }) => {
            format!("check the command binding's own text against how '{when}' is really typed")
        }
        _ => "give this fact a binding that actually reaches what it claims to guard".to_string(),
    }
}

/// A short, concrete correction for a `RefusesLegitimateUse` outcome - a
/// DIFFERENT fix from `correction_hint` above, and deliberately its own
/// function rather than a shared one: `correction_hint` is for a check that
/// reaches the WRONG place (a wrong anchor, or nothing at all); this one is
/// for a check that reaches the RIGHT place and then refuses too much once
/// there - the anchor is fine, the match itself is too broad to tell a
/// violation apart from ordinary use.
fn over_blocking_hint(item: &Item) -> String {
    match &item.check {
        Some(Check::Forbidden { .. }) => {
            "the literal itself may be too short or too common - check whether it also appears in ordinary, \
             legitimate use of this same command or file, and narrow it to the exact dangerous fragment"
                .to_string()
        }
        Some(Check::Absent { .. }) | Some(Check::AbsentAll { .. }) => {
            "the guard refused content that never carried any of this check's own literals - check the anchored \
             file's own real content and bindings against what this rule actually forbids, the reach may be \
             broader than intended"
                .to_string()
        }
        Some(Check::Requires { required, .. }) => format!(
            "the guard still refused a call that carried {required:?} - check the requirement is spelled exactly \
             the way it really appears in a call that satisfies it"
        ),
        _ => "narrow this check so it stops refusing ordinary, legitimate use of its own anchor".to_string(),
    }
}

/// The write-time decision Change 2 adds: whether `item` even owes a proof
/// (`is_heavy`/`is_refusing_kind`), and which box it falls into once `prove`
/// has actually tried it. Never refuses on its own account except the one
/// preserved shape (`TeethBox::KeepRefusing`) - see `TeethBox`'s own doc
/// comment. The caller (`mcp::remember`/`mcp::revise`) is what turns the
/// four write-through boxes into the stored tag and the write receipt;
/// nothing here writes anything.
///
/// ORDER: the collapsed case is checked FIRST and unconditionally, before
/// `prove` ever runs - see `anchor_and_literals_for_collapse`'s own doc
/// comment for why this can never be discovered any other way. Only once
/// that clears does the forbidden side run; the clean side (`prove_clean`)
/// runs ONLY when the forbidden side actually fired - if it did not, the
/// fact is already toothless from that one side alone, and testing legitimate
/// use of a check that catches nothing yet would add nothing but confusion.
pub fn teeth_box(id: &str, item: &Item, root: Option<&Path>, reality: &Reality) -> TeethBox {
    if !is_heavy(item.severity) && !is_refusing_kind(item.check.as_ref()) {
        return TeethBox::NotApplicable;
    }
    if let Some((anchor, literals)) = anchor_and_literals_for_collapse(item) {
        if let Some(literal) = collapsed_literal(anchor, literals) {
            // A collapsed anchor refuses every call on it, by construction. When
            // the log shows it doing exactly that to a real call, that is a
            // tooth that bit - the question of whether the anchor is meant to be
            // that specific is still named, but no longer a reason to count
            // this as unproven and push someone into swapping a working check
            // for an invented one (measured: exactly that happened, on request
            // of the Stop hook, in a test on a copy of a real store).
            if let Some(evidence) = bitten(id, item, reality) {
                return TeethBox::ProvenGuard {
                    evidence: format!(
                        "{evidence}; its anchor '{anchor}' contains its own literal '{literal}', so it refuses every call \
                         on that anchor - which is what it did"
                    ),
                    forbidden_tried: None,
                    clean_tried: None,
                };
            }
            return TeethBox::AnchorCollapsed {
                anchor: anchor.to_string(),
                literal: literal.to_string(),
                note: format!(
                    "the anchor '{anchor}' already contains the check's own literal '{literal}' - there is no \
                     literal-free form of this anchor left to test as legitimate use, so no probe can ever tell \
                     this rule apart from one that blocks everything reaching that anchor. This needs a person to \
                     look: either the anchor is deliberately this specific and the check is fine as it stands, or \
                     the anchor should name less and let the literal alone do the forbidding."
                ),
            };
        }
    }
    match prove(id, item, root) {
        Proof::Fires(forbidden_tried) => match prove_clean(id, item, root) {
            Proof::Silent(clean_tried) => match bitten(id, item, reality) {
                Some(evidence) => TeethBox::ProvenGuard {
                    evidence,
                    forbidden_tried: Some(forbidden_tried),
                    clean_tried: Some(clean_tried),
                },
                None => TeethBox::Wired {
                    note: format!(
                        "tried forbidden: {forbidden_tried}; refused. Tried clean: {clean_tried}; let through. That is \
                         reach only: both attempts were built from this rule's own literal, so they prove the check is \
                         wired, never that the literal is the mistake. Nothing in the log shows it meeting a real call \
                         yet - it counts as proven the first time the real hook refuses a real call with it, or when a \
                         real call this rule already fired on carries the literal"
                    ),
                    forbidden_tried,
                    clean_tried,
                },
            },
            Proof::Fires(clean_tried) => {
                let note = format!(
                    "tried forbidden: {forbidden_tried}; the guard refused it, correctly. Tried clean: \
                     {clean_tried}; the guard refused this too, and it should not have - this is ordinary, \
                     legitimate use of the same anchor, carrying none of the check's own literals. {}",
                    over_blocking_hint(item)
                );
                TeethBox::RefusesLegitimateUse { forbidden_tried, clean_tried, note }
            }
            // Defensive, should be unreachable: the clean side reuses the
            // identical reach and currency the forbidden side just proved,
            // synchronously, with nothing in between that could change the
            // filesystem or the item. Never promoted to `ProvenGuard` on an
            // assumption if it somehow does happen - reported plainly
            // instead, exactly like every other defensive branch in this
            // file (see `Proof::NotProvable`'s own doc comment).
            other => TeethBox::Informing {
                tried: Some(forbidden_tried.clone()),
                note: format!(
                    "tried: {forbidden_tried}; the guard refused it, but the clean, literal-free form of the \
                     identical anchor could not be tried under the same conditions ({other:?}) - reported rather \
                     than assumed proven"
                ),
            },
        },
        Proof::Silent(tried) => {
            let note = format!("tried: {tried}; the guard let it straight through - {}", correction_hint(item));
            TeethBox::Informing { tried: Some(tried), note }
        }
        Proof::AnchorMissing(path) => TeethBox::Informing {
            tried: None,
            note: format!(
                "the check's own path '{path}' does not currently exist, so this could not be tried at all - a missing anchor, not a wrong one"
            ),
        },
        Proof::NotProvable(why) if why.starts_with("a forbidden check reaches only") => {
            TeethBox::KeepRefusing(format!("REFUSED: {why}. As written it can never fire, whatever it looks like."))
        }
        // No synthesised attempt is possible here (a check that reads a file,
        // with no working copy, or one that can only be proven against real
        // content) - but the log may still hold the real thing: the real hook
        // refusing a real write under this very check.
        Proof::NotProvable(why) => match bitten(id, item, reality) {
            Some(evidence) => TeethBox::ProvenGuard { evidence, forbidden_tried: None, clean_tried: None },
            None => TeethBox::Informing { tried: None, note: format!("nothing to try: {why}") },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Kind, Severity};

    fn rule(text: &str, bindings: Vec<Binding>, check: Option<Check>) -> Item {
        Item {
            id: "r1".to_string(),
            kind: Kind::Rule,
            text: text.to_string(),
            bindings,
            severity: Some(Severity::Irreversible),
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("it turns out to be safe".to_string()),
            check,
        }
    }

    fn cmd(value: &str) -> Binding {
        Binding::Target { kind: TargetKind::Command, value: value.to_string() }
    }

    fn path(value: &str) -> Binding {
        Binding::Target { kind: TargetKind::Path, value: value.to_string() }
    }

    // --------------------------------------------------------------- Forbidden

    #[test]
    fn a_command_rule_whose_literal_really_appears_is_proven_by_being_refused() {
        let item = rule(
            "never force-push with --force",
            vec![cmd("git push")],
            Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        );
        match prove("r1", &item, None) {
            Proof::Fires(attempt) => assert!(attempt.contains("--force"), "{attempt}"),
            other => panic!("a literal that appears verbatim in the command must be refused: {other:?}"),
        }
    }

    /// THE LIMIT, kept as a test so nobody re-reads this module and believes
    /// it does more than it does. A literal with a typo in it is wired exactly
    /// as well as a correct one, so it passes here. It has to: the attempt is
    /// built from the literal. Only somebody who knows the real command can
    /// see that "--froce" guards nothing, and pretending otherwise would be
    /// the cover-you-believe-in defect this module exists to prevent, moved
    /// one level up.
    #[test]
    fn a_typo_in_the_literal_still_passes_and_that_is_the_honest_limit() {
        let item = rule(
            "never force-push with --force",
            vec![cmd("git push")],
            Some(Check::Forbidden { literals: vec!["--froce".to_string()] }),
        );
        assert!(
            matches!(prove("r1", &item, None), Proof::Fires(_)),
            "reach is all this proves; spelling is not something it can know"
        );
    }

    /// The shape that looks strongest and can never fire: a forbidden check on
    /// a binding the guard has no way to reach through.
    #[test]
    fn a_forbidden_check_on_a_moment_can_never_fire_and_says_so() {
        let item = rule(
            "never force-push with --force",
            vec![Binding::Moment(intent::Action::Push)],
            Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        );
        assert!(matches!(prove("r1", &item, None), Proof::NotProvable(_)));
    }

    #[test]
    fn a_rule_with_no_check_is_reported_as_nothing_to_try_never_as_working() {
        let item = rule("ask first", vec![Binding::Always], None);
        assert!(matches!(prove("r1", &item, None), Proof::NotProvable(_)));
    }

    /// A file-reading check that is not one of the four refusing kinds
    /// (`Contains`) is not silently blessed. It needs a working copy, and
    /// saying so is the honest answer.
    #[test]
    fn a_contains_check_is_reported_unproven_rather_than_assumed_good() {
        let item = rule(
            "NOTES.md must keep its header",
            vec![Binding::Always],
            Some(Check::Contains { path: "NOTES.md".to_string(), literal: "TODO".to_string() }),
        );
        assert!(matches!(prove("r1", &item, None), Proof::NotProvable(_)));
    }

    // --------------------------------------------------------- Absent/AbsentAll

    #[test]
    fn an_absent_check_whose_own_path_binding_matches_is_proven_by_being_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();
        let item = rule(
            "keep TODO out of README",
            vec![path("README.md")],
            Some(Check::Absent { path: "README.md".to_string(), literal: "TODO".to_string() }),
        );
        match prove("r1", &item, Some(dir.path())) {
            Proof::Fires(attempt) => {
                assert!(attempt.contains("TODO") && attempt.contains("README.md"), "{attempt}")
            }
            other => panic!("a path binding equal to the check's own path must be refused: {other:?}"),
        }
    }

    /// THE "WRONG ANCHOR" SHAPE: the file is there, but nothing about this
    /// item's own binding reaches a write to it - a rule anchored somewhere
    /// else entirely, whose check_path just happens to be a real file. This
    /// is `Silent`, never `AnchorMissing`: the check COULD be run (the path
    /// resolves), it simply never fires for this item.
    #[test]
    fn an_absent_check_whose_own_binding_never_reaches_its_own_path_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();
        let item = rule(
            "keep TODO out of README",
            vec![path("other.txt")],
            Some(Check::Absent { path: "README.md".to_string(), literal: "TODO".to_string() }),
        );
        match prove("r1", &item, Some(dir.path())) {
            Proof::Silent(attempt) => assert!(attempt.contains("README.md"), "{attempt}"),
            other => panic!("a binding that never reaches the check's own path must be silent: {other:?}"),
        }
    }

    /// THE OTHER SHAPE, and the task's own distinction: a check_path that
    /// does not currently exist can never even be TRIED, whatever the
    /// binding says - `AnchorMissing`, never `Silent`.
    #[test]
    fn an_absent_check_whose_own_path_does_not_currently_exist_is_reported_as_anchor_missing_not_silent() {
        let dir = tempfile::tempdir().unwrap();
        // README.md is deliberately never created in this checkout.
        let item = rule(
            "keep TODO out of README",
            vec![path("README.md")],
            Some(Check::Absent { path: "README.md".to_string(), literal: "TODO".to_string() }),
        );
        match prove("r1", &item, Some(dir.path())) {
            Proof::AnchorMissing(p) => assert_eq!(p, "README.md"),
            other => panic!("a path that does not resolve must be AnchorMissing, not Silent or Fires: {other:?}"),
        }
    }

    #[test]
    fn an_absent_check_with_no_root_resolved_is_not_provable() {
        let item = rule(
            "keep TODO out of README",
            vec![path("README.md")],
            Some(Check::Absent { path: "README.md".to_string(), literal: "TODO".to_string() }),
        );
        assert!(matches!(prove("r1", &item, None), Proof::NotProvable(_)));
    }

    /// `AbsentAll` is tried through the identical path as `Absent` - one loop,
    /// never a second implementation - proven here by exercising the SET
    /// form and confirming its first literal is what gets tried.
    #[test]
    fn an_absent_all_check_is_tried_the_same_way_as_absent_using_its_first_literal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("STYLE.md"), "hello").unwrap();
        let item = rule(
            "docs/STYLE.md keeps no stray TODO or FIXME",
            vec![path("STYLE.md")],
            Some(Check::AbsentAll { path: "STYLE.md".to_string(), literals: vec!["TODO".to_string(), "FIXME".to_string()] }),
        );
        match prove("r1", &item, Some(dir.path())) {
            Proof::Fires(attempt) => assert!(attempt.contains("TODO"), "the FIRST literal is what gets tried: {attempt}"),
            other => panic!("{other:?}"),
        }
    }

    /// A `Dir`-bound item reaches its own check_path through
    /// `find_dir_content_violation`, never `find_violation` (which
    /// structurally cannot see a `Dir` binding at all) - proven here so this
    /// module's own reuse of BOTH guard arms is exercised, not just the
    /// `Path`-bound one every other Absent/AbsentAll test above already
    /// covers.
    #[test]
    fn an_absent_check_reached_only_through_a_dir_binding_is_still_proven() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs").join("STYLE.md"), "hello").unwrap();
        let item = rule(
            "docs keeps no stray TODO",
            vec![Binding::Target { kind: TargetKind::Dir, value: "docs".to_string() }],
            Some(Check::Absent { path: "docs/STYLE.md".to_string(), literal: "TODO".to_string() }),
        );
        match prove("r1", &item, Some(dir.path())) {
            Proof::Fires(attempt) => assert!(attempt.contains("TODO"), "{attempt}"),
            other => panic!("a Dir binding containing the check's own path must be refused: {other:?}"),
        }
    }

    // --------------------------------------------------------------- Requires

    #[test]
    fn a_requires_check_whose_own_command_binding_is_present_is_proven_by_being_refused() {
        let item = rule(
            "a deploy names its target",
            vec![cmd("deploy")],
            Some(Check::Requires { when: "deploy".to_string(), required: vec!["--staging".to_string(), "--production".to_string()] }),
        );
        match prove("r1", &item, None) {
            Proof::Fires(attempt) => assert_eq!(attempt, "deploy"),
            other => panic!("a requires check with a real command binding must be refused by the empty-content attempt: {other:?}"),
        }
    }

    /// Defensive, not reachable through the gate (ground 23/1 already force
    /// a Requires check onto at least one Command binding at write time) -
    /// this module never assumes a caller it cannot see could not hand it
    /// this shape directly.
    #[test]
    fn a_requires_check_with_no_command_binding_at_all_is_not_provable() {
        let item = rule(
            "a deploy names its target",
            vec![Binding::Always],
            Some(Check::Requires { when: "deploy".to_string(), required: vec!["--staging".to_string()] }),
        );
        assert!(matches!(prove("r1", &item, None), Proof::NotProvable(_)));
    }

    // ------------------------------------------------------------- teeth_box

    #[test]
    fn a_light_item_with_no_check_is_not_applicable() {
        let mut item = rule("just a note", vec![Binding::Always], None);
        item.severity = None;
        assert_eq!(teeth_box("r1", &item, None, &Reality::default()), TeethBox::NotApplicable);
    }

    #[test]
    fn a_light_item_with_a_contains_check_is_not_applicable() {
        let mut item = rule(
            "NOTES.md keeps its header",
            vec![Binding::Always],
            Some(Check::Contains { path: "NOTES.md".to_string(), literal: "TODO".to_string() }),
        );
        item.severity = Some(Severity::HouseStyle);
        assert_eq!(teeth_box("r1", &item, None, &Reality::default()), TeethBox::NotApplicable);
    }

    #[test]
    fn a_heavy_item_with_no_check_lands_in_the_informing_box_with_nothing_tried() {
        let item = rule("ask before pushing to main", vec![Binding::Always], None);
        match teeth_box("r1", &item, None, &Reality::default()) {
            TeethBox::Informing { tried: None, note } => assert!(note.contains("nothing to try"), "{note}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_light_item_with_a_reachable_forbidden_check_is_still_tried_and_comes_back_wired() {
        // A refusing-kind check applies regardless of severity - the second,
        // independent half of the applicability rule. With nothing on record
        // it is wired, never proven (see "against reality").
        let mut item = rule(
            "never force-push with --force",
            vec![cmd("git push")],
            Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        );
        item.severity = Some(Severity::HouseStyle);
        assert!(matches!(teeth_box("r1", &item, None, &Reality::default()), TeethBox::Wired { .. }));
    }

    #[test]
    fn a_forbidden_check_reaching_neither_always_nor_command_still_keeps_the_old_refusal() {
        let item = rule(
            "never force-push with --force",
            vec![Binding::Moment(intent::Action::Push)],
            Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        );
        match teeth_box("r1", &item, None, &Reality::default()) {
            TeethBox::KeepRefusing(msg) => assert!(msg.starts_with("REFUSED:"), "{msg}"),
            other => panic!("the one preserved refusal must still fire: {other:?}"),
        }
    }

    #[test]
    fn a_toothless_check_lands_in_the_informing_box_with_what_was_tried_and_a_correction() {
        // Forbidden can never genuinely be Silent here once reach exists -
        // the attempt is BUILT from the literal (see `a_typo_in_the_literal_
        // still_passes_and_that_is_the_honest_limit` above), so it always
        // fires. `Absent`'s own reach question (does this item's OWN binding
        // actually get it SELECTED for a write to its own check_path) is the
        // one this module can genuinely answer "no" to - a "wrong anchor" in
        // the task's own words - which is exactly what this test exercises.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();
        let item = rule(
            "keep TODO out of README",
            vec![path("other.txt")],
            Some(Check::Absent { path: "README.md".to_string(), literal: "TODO".to_string() }),
        );
        match teeth_box("r1", &item, Some(dir.path()), &Reality::default()) {
            TeethBox::Informing { tried: Some(tried), note } => {
                assert!(tried.contains("TODO") && tried.contains("README.md"), "{tried}");
                assert!(note.contains("tried:"), "{note}");
                assert!(note.contains("README.md"), "the correction must name the anchor to fix: {note}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_anchor_missing_check_lands_in_the_informing_box_and_says_so_distinctly() {
        let dir = tempfile::tempdir().unwrap();
        let item = rule(
            "keep TODO out of README",
            vec![path("README.md")],
            Some(Check::Absent { path: "README.md".to_string(), literal: "TODO".to_string() }),
        );
        match teeth_box("r1", &item, Some(dir.path()), &Reality::default()) {
            TeethBox::Informing { tried: None, note } => {
                assert!(note.contains("does not currently exist"), "{note}");
            }
            other => panic!("{other:?}"),
        }
    }

    // ------------------------------------------------- two-sided teeth_box

    #[test]
    fn collapsed_literal_finds_a_literal_sitting_inside_its_own_anchor() {
        assert_eq!(
            collapsed_literal("git push --force", &["--force".to_string()]),
            Some("--force")
        );
        assert_eq!(collapsed_literal("git push", &["--force".to_string()]), None);
        // An empty literal is never treated as "collapsed" - trivially
        // matches everything, which would call every anchor collapsed for
        // nothing. It is caught downstream instead, as a genuine
        // `RefusesLegitimateUse` (see the pathological tests below).
        assert_eq!(collapsed_literal("git push", &[String::new()]), None);
    }

    #[test]
    fn the_clean_form_of_a_reachable_command_rule_is_silent_because_it_carries_no_literal() {
        let item = rule(
            "never force-push with --force",
            vec![cmd("git push")],
            Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        );
        match prove_clean("r1", &item, None) {
            Proof::Silent(attempt) => assert_eq!(attempt, "git push"),
            other => panic!("a literal-free anchor must never be refused: {other:?}"),
        }
    }

    /// THE PRIMARY CLAIM, end to end: a command rule is proven only once
    /// BOTH sides are shown, and the receipt (`teeth_box`'s own `forbidden_
    /// tried`/`clean_tried` pair) carries both, never just the one side that
    /// used to be the whole story.
    #[test]
    fn a_wired_guard_carries_both_the_forbidden_and_the_clean_attempt() {
        let item = rule(
            "never force-push with --force",
            vec![cmd("git push")],
            Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        );
        match teeth_box("r1", &item, None, &Reality::default()) {
            TeethBox::Wired { forbidden_tried, clean_tried, .. } => {
                assert!(forbidden_tried.contains("--force"), "{forbidden_tried}");
                assert_eq!(clean_tried, "git push", "the clean form is the anchor alone, nothing appended: {clean_tried}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_absent_wired_guard_carries_both_the_forbidden_and_the_clean_attempt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();
        let item = rule(
            "keep TODO out of README",
            vec![path("README.md")],
            Some(Check::Absent { path: "README.md".to_string(), literal: "TODO".to_string() }),
        );
        match teeth_box("r1", &item, Some(dir.path()), &Reality::default()) {
            TeethBox::Wired { forbidden_tried, clean_tried, .. } => {
                assert!(forbidden_tried.contains("TODO"), "{forbidden_tried}");
                assert!(clean_tried.contains("README.md") && !clean_tried.contains("TODO"), "{clean_tried}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// `Requires` is proven both ways too, with the polarity this file's own
    /// top-of-file doc comment names: the forgotten form (no answer) refuses,
    /// the satisfied form (one acceptable answer present) does not.
    #[test]
    fn a_requires_check_is_wired_both_ways_forgotten_refuses_satisfied_passes() {
        let item = rule(
            "a deploy names its target",
            vec![cmd("deploy")],
            Some(Check::Requires { when: "deploy".to_string(), required: vec!["--staging".to_string(), "--production".to_string()] }),
        );
        match teeth_box("r1", &item, None, &Reality::default()) {
            TeethBox::Wired { forbidden_tried, clean_tried, .. } => {
                assert_eq!(forbidden_tried, "deploy");
                assert!(clean_tried.contains("deploy") && clean_tried.contains("--staging"), "{clean_tried}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// An INNER trigger (2026-09-26) is tried as the binding carrying the
    /// trigger: without an answer it refuses, with one it passes, and the bare
    /// binding - any other call on it - passes too.
    #[test]
    fn an_inner_trigger_is_tried_inside_its_binding_and_leaves_the_bare_binding_alone() {
        let item = rule(
            "a dev compose over ssh names its compose file",
            vec![cmd("ssh")],
            Some(Check::Requires { when: "-p acme-dev".to_string(), required: vec!["-f docker-compose.dev.yml".to_string()] }),
        );
        match teeth_box("r1", &item, None, &Reality::default()) {
            TeethBox::Wired { forbidden_tried, clean_tried, .. } => {
                assert_eq!(forbidden_tried, "ssh -p acme-dev");
                assert!(clean_tried.contains("docker-compose.dev.yml"), "{clean_tried}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// THE COLLAPSED CASE, as a worked example: an anchor that
    /// already carries its own forbidden fragment leaves no literal-free
    /// form to test as legitimate use, so this is neither proven nor
    /// silently informing - its own distinct box.
    #[test]
    fn an_anchor_that_already_contains_its_own_literal_is_the_collapsed_case_not_proven() {
        let item = rule(
            "never force-push with --force",
            vec![cmd("git push --force")],
            Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        );
        match teeth_box("r1", &item, None, &Reality::default()) {
            TeethBox::AnchorCollapsed { anchor, literal, note } => {
                assert_eq!(anchor, "git push --force");
                assert_eq!(literal, "--force");
                assert!(note.contains("no literal-free form"), "{note}");
            }
            other => panic!("a literal sitting inside its own anchor must collapse, never prove: {other:?}"),
        }
    }

    #[test]
    fn an_absent_check_whose_path_already_names_its_own_literal_is_the_collapsed_case() {
        let item = rule(
            "keep TODO out of TODO.md",
            vec![path("TODO.md")],
            Some(Check::Absent { path: "TODO.md".to_string(), literal: "TODO".to_string() }),
        );
        match teeth_box("r1", &item, None, &Reality::default()) {
            TeethBox::AnchorCollapsed { anchor, literal, .. } => {
                assert_eq!(anchor, "TODO.md");
                assert_eq!(literal, "TODO");
            }
            other => panic!("{other:?}"),
        }
    }

    /// THE THIRD OUTCOME. A deliberately pathological construction: the
    /// write gate itself already refuses an empty literal (`gate::
    /// check_check`), so a live item can never actually carry one - but this
    /// module never trusts a caller it cannot see (see `Proof::NotProvable`'s
    /// own doc comment on every other defensive check in this file), and an
    /// empty literal is "contained" in absolutely everything, including the
    /// bare anchor with nothing appended. This is the one shape that can
    /// prove this outcome at all: every OTHER literal this module can build
    /// either collapses into its own anchor (caught first, above) or leaves
    /// a genuinely literal-free clean form the real guard cannot refuse (see
    /// this file's own top-of-file doc comment) - confirmed by reading
    /// `absent_guard`'s own matching functions, which are pure substring
    /// checks with no other source of a false refusal.
    #[test]
    fn an_empty_literal_refuses_its_own_clean_form_too_and_is_never_proven() {
        let item = rule(
            "pathological: an empty literal blocks everything",
            vec![cmd("git push")],
            Some(Check::Forbidden { literals: vec![String::new()] }),
        );
        match teeth_box("r1", &item, None, &Reality::default()) {
            TeethBox::RefusesLegitimateUse { forbidden_tried, clean_tried, note } => {
                assert!(forbidden_tried.contains("git push"), "{forbidden_tried}");
                assert_eq!(clean_tried, "git push");
                assert!(note.contains("ordinary, legitimate use"), "{note}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_absent_check_with_an_empty_literal_refuses_its_own_clean_form_too() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();
        let item = rule(
            "pathological: an empty literal blocks everything",
            vec![path("README.md")],
            Some(Check::Absent { path: "README.md".to_string(), literal: String::new() }),
        );
        match teeth_box("r1", &item, Some(dir.path()), &Reality::default()) {
            TeethBox::RefusesLegitimateUse { forbidden_tried, clean_tried, .. } => {
                assert!(forbidden_tried.contains("README.md"), "{forbidden_tried}");
                assert!(clean_tried.contains("README.md"), "{clean_tried}");
            }
            other => panic!("{other:?}"),
        }
    }

    // ------------------------------------------------------- against reality

    fn on_record(refusals: &[(i64, &str)], check_since: i64) -> Reality {
        Reality { refusals: refusals.iter().map(|(s, t)| (*s, t.to_string())).collect(), check_since }
    }

    /// THE ATTACK THIS CLOSES, measured on a copy of a real store: a literal
    /// no command will ever carry came back a proven guard, because the
    /// attempt was built from it. Now it is wired at most.
    #[test]
    fn an_invented_literal_with_nothing_on_record_is_never_proven() {
        let item = rule(
            "never repartition the data disk without a fresh backup",
            vec![cmd("diskpart")],
            Some(Check::Forbidden { literals: vec!["zqxj-never-typed".to_string()] }),
        );
        match teeth_box("r1", &item, None, &on_record(&[], 5)) {
            TeethBox::Wired { note, .. } => assert!(note.contains("never that the literal is the mistake"), "{note}"),
            other => panic!("an invented literal must never come back proven: {other:?}"),
        }
    }

    #[test]
    fn a_real_refusal_the_current_check_still_makes_proves_the_guard() {
        let item = rule(
            "never force-push with --force",
            vec![cmd("git push")],
            Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        );
        match teeth_box("r1", &item, None, &on_record(&[(42, "git push --force origin main")], i64::MAX)) {
            TeethBox::ProvenGuard { evidence, forbidden_tried, clean_tried } => {
                assert!(evidence.contains("git push --force origin main") && evidence.contains("42"), "{evidence}");
                assert!(forbidden_tried.is_some() && clean_tried.is_some(), "the synthesised test held too");
            }
            other => panic!("{other:?}"),
        }
    }

    /// A working check swapped for an invented one keeps none of the old
    /// check's record: the old refusal is tried again against the NEW check,
    /// which does not refuse it. Measured on a copy of a real store: the Stop
    /// hook pushed exactly this swap, to earn a "proven" label.
    #[test]
    fn a_refusal_earned_by_an_older_literal_never_vouches_for_a_new_one() {
        let item = rule(
            "never run the old importer",
            vec![cmd("thor")],
            Some(Check::Forbidden { literals: vec!["zqxj-invented-import".to_string()] }),
        );
        let old_refusal = on_record(&[(7, "thor import --from old")], i64::MAX);
        assert!(!matches!(teeth_box("r1", &item, None, &old_refusal), TeethBox::ProvenGuard { .. }));
    }

    /// N12: a call the rule was served on proves nothing, whatever the check
    /// would do with it - a hand-fed payload makes served events as easily as
    /// a real session. Only an attested refusal is evidence (`reality`).
    #[test]
    fn a_served_call_the_check_would_refuse_is_never_proof() {
        let mut store = EventStore::in_memory().unwrap();
        let mut item = rule(
            "never force-push with --force",
            vec![cmd("git push")],
            Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        );
        item.tags = vec![];
        model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
        let session = "0de7dc52-1b97-4b66-948f-61e4438043b2";
        for trigger in ["git push origin main", "git push --force origin main", "git push origin topic"] {
            crate::deliver::record_delivery_with_trigger(&mut store, session, session, "hook", "2026-09-26T00:00:00Z", &["r1".to_string()], Some(trigger));
        }
        let r = reality(&store, "r1", item.check.as_ref());
        assert!(r.refusals.is_empty());
        assert!(!matches!(teeth_box("r1", &item, None, &r), TeethBox::ProvenGuard { .. }));
    }

    /// A file check cannot be tried again against content the log never
    /// kept, so a real refusal counts only when it came after the current
    /// check was set.
    #[test]
    fn a_file_check_is_proven_only_by_a_refusal_after_its_check_was_set() {
        let item = rule(
            "the index keeps its header",
            vec![path("files/README.md")],
            Some(Check::Contains { path: "files/README.md".to_string(), literal: "agent zone".to_string() }),
        );
        let after = on_record(&[(30, "files/README.md")], 20);
        assert!(matches!(teeth_box("r1", &item, None, &after), TeethBox::ProvenGuard { .. }));
        let before = on_record(&[(10, "files/README.md")], 20);
        assert!(matches!(teeth_box("r1", &item, None, &before), TeethBox::Informing { .. }));
    }

    /// A collapsed anchor that refused a real call is a tooth that bit - it is
    /// not pushed into being swapped for an invented literal any more.
    #[test]
    fn a_collapsed_anchor_that_refused_a_real_call_is_proven() {
        let item = rule(
            "never run the old importer",
            vec![cmd("thor import")],
            Some(Check::Forbidden { literals: vec!["thor import".to_string()] }),
        );
        match teeth_box("r1", &item, None, &on_record(&[(5, "thor import --from old")], i64::MAX)) {
            TeethBox::ProvenGuard { evidence, .. } => assert!(evidence.contains("contains its own literal"), "{evidence}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(teeth_box("r1", &item, None, &Reality::default()), TeethBox::AnchorCollapsed { .. }));
    }

    #[test]
    fn a_requires_refusal_on_record_proves_the_requires_check() {
        let item = rule(
            "a deploy names its target",
            vec![cmd("deploy")],
            Some(Check::Requires { when: "deploy".to_string(), required: vec!["--staging".to_string()] }),
        );
        assert!(matches!(teeth_box("r1", &item, None, &on_record(&[(9, "deploy now")], i64::MAX)), TeethBox::ProvenGuard { .. }));
    }

    /// `reality` itself, against a real log: the seq from which the check
    /// being judged has stood (`i64::MAX` for a check the log does not hold),
    /// and only ATTESTED refusals (`serve::attest::is_attested_refusal`): a
    /// hook refusal with no recorded verification is not one, the same
    /// refusal verified from its session's transcript is.
    #[test]
    fn reality_reads_the_log_and_dates_the_current_check() {
        let root = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_memory().unwrap();
        let mut item = rule(
            "never force-push with --force",
            vec![cmd("git push")],
            Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        );
        // No reason tag beside the check: the write gate refuses a check that
        // refuses and "nothing can catch this" introduced together.
        item.tags = vec![];
        let created = model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
        crate::deliver::record_delivery_with_trigger(&mut store, "s", "l", "hook", "2026-09-26T00:00:00Z", &["r1".to_string()], Some("git push origin main"));
        crate::deliver::record_gate_outcome(&mut store, "s", "l", "hook", "2026-09-26T00:00:02Z", true, "r1", "git push --force");
        let r = reality(&store, "r1", item.check.as_ref());
        assert_eq!(r.check_since, created.seq);
        assert!(r.refusals.is_empty(), "an unverified refusal from a non-Claude Code session is no evidence");

        let session = "0de7dc52-1b97-4b66-948f-61e4438043b2";
        let seq = crate::attest::fixture::field_refusal(
            &mut store,
            root.path(),
            session,
            "r1",
            "toolu_R1",
            "Bash",
            serde_json::json!({"command": "git push --force origin main"}),
            "git push --force origin main",
        );
        let r = reality(&store, "r1", item.check.as_ref());
        assert_eq!(r.refusals, vec![(seq, "git push --force origin main".to_string())]);
        assert!(matches!(teeth_box("r1", &item, None, &r), TeethBox::ProvenGuard { .. }));
        let other = Check::Forbidden { literals: vec!["--mirror".to_string()] };
        assert_eq!(reality(&store, "r1", Some(&other)).check_since, i64::MAX, "a check the log does not hold has no date");
    }

    /// `bitten` reads only attested refusals: a hand-fed one (no transcript
    /// line behind it) never makes a rule proven.
    #[test]
    fn bitten_ignores_non_field_refusals() {
        let root = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_memory().unwrap();
        let mut item = rule("never force-push with --force", vec![cmd("git push")], Some(Check::Forbidden { literals: vec!["--force".to_string()] }));
        item.tags = vec![];
        model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
        let session = "0de7dc52-1b97-4b66-948f-61e4438043b2";
        crate::deliver::record_delivery_with_trigger(&mut store, session, session, "hook", "2026-09-26T00:00:00Z", &["r1".to_string()], Some("git push"));
        crate::deliver::record_gate_outcome_for_call(&mut store, session, session, "hook", "2026-09-26T00:00:01Z", true, "r1", "git push --force", Some("toolu_HAND"));
        // The session has a transcript, but not this call.
        crate::attest::fixture::main_transcript(root.path(), session)
            .let_through("toolu_OTHER", "Bash", &serde_json::json!({"command": "ls"}), "ctx", "ok")
            .write();
        crate::attest::record_refusal_verifications_in(&mut store, session, Some(root.path()));
        let r = reality(&store, "r1", item.check.as_ref());
        assert!(r.refusals.is_empty());
        assert_eq!(bitten("r1", &item, &r), None);
    }

    /// Git-history evidence for an absent check: found for a literal that
    /// really lived in the file, not found for one that never did.
    #[test]
    fn git_history_evidence_is_found_and_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(dir.path()).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q"]);
        git(&["config", "core.hooksPath", &hooks.path().to_string_lossy()]);
        git(&["config", "user.email", "fixture@example.invalid"]);
        git(&["config", "user.name", "fixture"]);
        std::fs::write(dir.path().join("README.md"), "hello\nTODO: remove\n").unwrap();
        git(&["add", "README.md"]);
        git(&["commit", "-q", "-m", "with the marker"]);
        std::fs::write(dir.path().join("README.md"), "hello\n").unwrap();
        git(&["commit", "-q", "-am", "without it"]);
        assert!(lived_in_history(dir.path(), "README.md", "TODO").is_some());
        assert_eq!(lived_in_history(dir.path(), "README.md", "zqxj-never-typed"), None);
        assert_eq!(lived_in_history(dir.path(), "README.md", ""), None);
    }
}
