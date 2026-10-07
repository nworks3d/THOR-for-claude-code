//! FALSE BLOCKS AS A NUMBER, read from THOR's own log and nothing else.
//!
//! THE GAP THIS CLOSES. THOR counted refusals and never asked what happened
//! to the rule that made them. A rule that refused real calls and was then
//! weakened or retracted is the false block the doctrine fears most, and
//! until now that sequence existed in the log but in no number.
//!
//! WHAT IT MEASURES. For each real refusal of a rule (the caller decides
//! which refusals are real - doctor and the evaluation's machine section each
//! pass the proof they already use), the first thing that happened to that
//! rule afterwards, inside a window the caller also decides:
//! - a RETRACT, or a WEAKENING (`gate::weakenings_widened`: check cleared or
//!   changed, severity lowered, binding dropped, taken off the rule track,
//!   and the rest of that list) by anyone but THOR itself: a probable false
//!   block;
//! - else a NOISE mark: listed, never counted, because `mark` is callable by
//!   the very agent that was refused (`serve::decay`, the self-judging
//!   channel it closed);
//! - else nothing: unchanged.
//!
//! A PROXY, NOT A VERDICT. A rule can be retracted because it went stale, or
//! reworded because the project moved; a false block can also go unrepaired.
//! Every reader labels the number a proxy. It feeds no ranking, no demotion
//! and no gate. A rule that refused many calls before one revise counts once
//! per refusal: the refusals of one rule are not independent, so the bound
//! below is a floor on the uncertainty, not a ceiling.

use thor_core::event_store::{Event, EventKind};

use crate::item::Item;
use crate::store::{parse_body, RESERVED_ACTORS};

/// How long after a refusal a weakening or retract of the same rule still
/// counts as its outcome.
pub const OUTCOME_WINDOW_DAYS: i64 = 7;

/// What happened to a rule after one of its real refusals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fate {
    /// Nothing that changed the rule's reach, or nothing at all.
    Unchanged,
    /// Someone marked it noise (never counted as a false block).
    MarkedNoise,
    /// A write by someone other than THOR weakened it; the labels, in words.
    Weakened(String),
    Retracted,
}

/// One real refusal and its fate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusalFate {
    pub rule: String,
    pub refusal_seq: i64,
    pub fate: Fate,
    /// The log seq of the event the fate names; `None` for the two that are
    /// not a write by the rule's owner side (unchanged).
    pub fate_seq: Option<i64>,
}

impl RefusalFate {
    /// The refusal counts toward the false-block number.
    pub fn probable_false_block(&self) -> bool {
        matches!(self.fate, Fate::Weakened(_) | Fate::Retracted)
    }
}

/// The fate of every real refusal in `events`, ONE rule's own events, oldest
/// first. `is_real(refusal, item_in_force, check_set_at)` says which
/// `GateRefused` events count (the item is the rule as it stood at that
/// refusal, `check_set_at` the log seq at which its check in force was set).
/// `follows(refusal, later)` says whether a later event of the rule is still
/// inside the window of that refusal.
pub fn fates(
    rule: &str,
    events: &[Event],
    is_real: &dyn Fn(&Event, &Item, i64) -> bool,
    follows: &dyn Fn(&Event, &Event) -> bool,
) -> Vec<RefusalFate> {
    // The rule as it stood after each write, with the seq its check in force
    // was set at; `stood[i]` names the entry in force after event `i`.
    let mut snapshots: Vec<(Option<Item>, i64)> = vec![(None, i64::MAX)];
    let mut stood: Vec<usize> = Vec::with_capacity(events.len());
    for e in events {
        let (item, set_at) = snapshots.last().cloned().unwrap_or((None, i64::MAX));
        match e.kind {
            EventKind::FactCreated | EventKind::FactRevised => {
                if let Ok(next) = parse_body(&e.body) {
                    let set_at = if item.as_ref().map(|i| &i.check) == Some(&next.check) { set_at } else { e.seq };
                    snapshots.push((Some(next), set_at));
                }
            }
            EventKind::FactRetracted => snapshots.push((None, i64::MAX)),
            _ => {}
        }
        stood.push(snapshots.len() - 1);
    }
    let mut out = Vec::new();
    for (i, refusal) in events.iter().enumerate().filter(|(_, e)| e.kind == EventKind::GateRefused) {
        let (Some(item), set_at) = &snapshots[stood[i]] else { continue };
        if !is_real(refusal, item, *set_at) {
            continue;
        }
        let mut noise: Option<i64> = None;
        let mut fate = RefusalFate { rule: rule.to_string(), refusal_seq: refusal.seq, fate: Fate::Unchanged, fate_seq: None };
        for (j, later) in events.iter().enumerate().skip(i + 1).filter(|(_, l)| follows(refusal, l)) {
            let by_thor = RESERVED_ACTORS.contains(&later.actor.as_str());
            match later.kind {
                EventKind::FactRetracted if !by_thor => {
                    fate.fate = Fate::Retracted;
                    fate.fate_seq = Some(later.seq);
                    break;
                }
                EventKind::FactRevised if !by_thor => {
                    let (Some(before), Ok(after)) = (&snapshots[stood[j - 1]].0, parse_body(&later.body)) else { continue };
                    let weakened = before.kind.can_fire().then(|| crate::gate::weakenings_widened(before, &after)).unwrap_or_default();
                    if !weakened.is_empty() {
                        let labels = weakened.iter().map(|w| w.label).collect::<Vec<_>>().join(", ");
                        fate.fate = Fate::Weakened(labels);
                        fate.fate_seq = Some(later.seq);
                        break;
                    }
                }
                EventKind::ItemMarkedNoise => noise = noise.or(Some(later.seq)),
                _ => {}
            }
        }
        if fate.fate == Fate::Unchanged {
            if let Some(seq) = noise {
                fate.fate = Fate::MarkedNoise;
                fate.fate_seq = Some(seq);
            }
        }
        out.push(fate);
    }
    out
}

/// The one-sided 95 percent upper bound on the rate `failures` of `n`.
/// Zero failures: the rule of three, 3/n. Otherwise the Clopper-Pearson
/// bound: the rate at which seeing `failures` or fewer in `n` has fallen to
/// 5 percent, found by bisection. Capped at 1.
pub fn upper_bound(failures: usize, n: usize) -> f64 {
    if n == 0 || failures >= n {
        return 1.0;
    }
    if failures == 0 {
        return (3.0 / n as f64).min(1.0);
    }
    let at_most = |p: f64| {
        let mut term = (1.0 - p).powi(n as i32);
        let mut total = term;
        for i in 0..failures {
            term *= (n - i) as f64 / (i + 1) as f64 * p / (1.0 - p);
            total += term;
        }
        total
    };
    let (mut low, mut high) = (failures as f64 / n as f64, 1.0_f64);
    for _ in 0..60 {
        let mid = (low + high) / 2.0;
        if at_most(mid) > 0.05 {
            low = mid;
        } else {
            high = mid;
        }
    }
    high
}

/// "rate X percent, one-sided 95 percent upper bound Y percent" for
/// `failures` of `n` (`n` above zero), the same words in doctor and in the
/// evaluation's machine section.
pub fn rate_words(failures: usize, n: usize) -> String {
    let rule_of_three = if failures == 0 { " (3/n rule)" } else { "" };
    format!(
        "rate {:.1} percent, one-sided 95 percent upper bound {:.1} percent{rule_of_three}",
        failures as f64 * 100.0 / n as f64,
        upper_bound(failures, n) * 100.0
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{Binding, Check, Kind, Severity, TargetKind};

    const HOOK: &str = "hook";

    fn rule(check: Option<Check>, severity: Option<Severity>) -> Item {
        Item {
            id: "r1".to_string(),
            kind: Kind::Rule,
            text: "never run git push with --force".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: "git push".to_string() }],
            severity,
            project: Some("fixture".to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("a forced push is fine".to_string()),
            check,
        }
    }

    fn forbid(literal: &str) -> Option<Check> {
        Some(Check::Forbidden { literals: vec![literal.to_string()] })
    }

    fn event(seq: i64, actor: &str, kind: EventKind, body: String) -> Event {
        Event {
            seq,
            event_uuid: format!("u{seq}"),
            session_id: "s".to_string(),
            lineage_id: "s".to_string(),
            actor: actor.to_string(),
            kind,
            entity_id: "r1".to_string(),
            parent_rev: None,
            body,
            body_ch: String::new(),
            prev_hash: String::new(),
            this_hash: String::new(),
        }
    }

    fn write(seq: i64, actor: &str, kind: EventKind, item: &Item) -> Event {
        event(seq, actor, kind, serde_json::to_string(item).unwrap())
    }

    fn refusal(seq: i64) -> Event {
        event(seq, HOOK, EventKind::GateRefused, "{}".to_string())
    }

    fn all_real(_: &Event, _: &Item, _: i64) -> bool {
        true
    }

    fn always(_: &Event, _: &Event) -> bool {
        true
    }

    fn fate_of(events: &[Event]) -> Vec<RefusalFate> {
        fates("r1", events, &all_real, &always)
    }

    #[test]
    fn a_refusal_with_nothing_after_it_is_unchanged() {
        let events = [write(1, "mcp", EventKind::FactCreated, &rule(forbid("--force"), None)), refusal(2)];
        let got = fate_of(&events);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].fate, Fate::Unchanged);
        assert!(!got[0].probable_false_block());
    }

    #[test]
    fn a_cleared_check_after_a_refusal_is_a_probable_false_block() {
        let events = [
            write(1, "mcp", EventKind::FactCreated, &rule(forbid("--force"), None)),
            refusal(2),
            write(3, "mcp", EventKind::FactRevised, &rule(None, None)),
        ];
        let got = fate_of(&events);
        assert_eq!(got[0].fate, Fate::Weakened("check cleared".to_string()));
        assert_eq!(got[0].fate_seq, Some(3));
        assert!(got[0].probable_false_block());
    }

    #[test]
    fn a_lowered_severity_and_a_dropped_binding_count_too() {
        let heavy = rule(forbid("--force"), Some(Severity::Irreversible));
        let mut lowered = heavy.clone();
        lowered.severity = Some(Severity::HouseStyle);
        let mut unbound = heavy.clone();
        unbound.bindings = vec![Binding::Always];
        for (after, label) in [(lowered, "severity lowered"), (unbound, "binding removed")] {
            let events = [write(1, "mcp", EventKind::FactCreated, &heavy), refusal(2), write(3, "mcp", EventKind::FactRevised, &after)];
            let got = fate_of(&events);
            assert!(matches!(&got[0].fate, Fate::Weakened(l) if l.contains(label)), "{:?}", got[0].fate);
        }
    }

    #[test]
    fn a_retract_after_a_refusal_is_a_probable_false_block() {
        let events = [
            write(1, "mcp", EventKind::FactCreated, &rule(forbid("--force"), None)),
            refusal(2),
            event(3, "mcp", EventKind::FactRetracted, "{\"reason\":\"wrong\"}".to_string()),
        ];
        let got = fate_of(&events);
        assert_eq!(got[0].fate, Fate::Retracted);
        assert!(got[0].probable_false_block());
    }

    #[test]
    fn a_noise_mark_is_listed_and_never_counted() {
        let events = [
            write(1, "mcp", EventKind::FactCreated, &rule(forbid("--force"), None)),
            refusal(2),
            event(3, "mcp", EventKind::ItemMarkedNoise, "{}".to_string()),
        ];
        let got = fate_of(&events);
        assert_eq!(got[0].fate, Fate::MarkedNoise);
        assert!(!got[0].probable_false_block());
    }

    #[test]
    fn a_weakening_wins_over_an_earlier_noise_mark() {
        let events = [
            write(1, "mcp", EventKind::FactCreated, &rule(forbid("--force"), None)),
            refusal(2),
            event(3, "mcp", EventKind::ItemMarkedNoise, "{}".to_string()),
            write(4, "mcp", EventKind::FactRevised, &rule(None, None)),
        ];
        assert!(fate_of(&events)[0].probable_false_block());
    }

    #[test]
    fn a_change_by_thor_itself_is_not_an_outcome() {
        let events = [
            write(1, "mcp", EventKind::FactCreated, &rule(forbid("--force"), None)),
            refusal(2),
            write(3, "retest", EventKind::FactRevised, &rule(None, None)),
        ];
        assert_eq!(fate_of(&events)[0].fate, Fate::Unchanged);
    }

    #[test]
    fn a_change_that_keeps_the_reach_is_not_a_weakening() {
        let events = [
            write(1, "mcp", EventKind::FactCreated, &rule(forbid("--force"), None)),
            refusal(2),
            write(3, "mcp", EventKind::FactRevised, &rule(forbid("--force"), Some(Severity::Irreversible))),
        ];
        assert_eq!(fate_of(&events)[0].fate, Fate::Unchanged);
    }

    #[test]
    fn only_what_the_caller_calls_real_is_measured_and_it_sees_the_check_in_force() {
        let events = [
            write(1, "mcp", EventKind::FactCreated, &rule(forbid("--force"), None)),
            refusal(2),
            write(3, "mcp", EventKind::FactRevised, &rule(forbid("--force-x"), None)),
            refusal(4),
        ];
        let seen = std::cell::RefCell::new(Vec::new());
        let real = |e: &Event, item: &Item, since: i64| {
            seen.borrow_mut().push((e.seq, item.check.clone(), since));
            e.seq == 2
        };
        let got = fates("r1", &events, &real, &always);
        assert_eq!(got.len(), 1, "the second refusal was not real");
        assert!(got[0].probable_false_block(), "the check changed after the first refusal");
        assert_eq!(
            *seen.borrow(),
            vec![(2, forbid("--force"), 1), (4, forbid("--force-x"), 3)],
            "each refusal is judged against the check in force then, set at the write that set it"
        );
    }

    #[test]
    fn a_later_event_outside_the_window_is_not_an_outcome() {
        let events = [
            write(1, "mcp", EventKind::FactCreated, &rule(forbid("--force"), None)),
            refusal(2),
            write(30, "mcp", EventKind::FactRevised, &rule(None, None)),
        ];
        let near = |_: &Event, later: &Event| later.seq < 10;
        assert_eq!(fates("r1", &events, &all_real, &near)[0].fate, Fate::Unchanged);
    }

    #[test]
    fn the_upper_bound_follows_the_rule_of_three_at_zero_and_clopper_pearson_above() {
        assert!((upper_bound(0, 30) - 0.1).abs() < 1e-12);
        assert_eq!(upper_bound(0, 2), 1.0, "capped at one");
        assert_eq!(upper_bound(0, 0), 1.0, "nothing measured bounds nothing");
        // One-sided 95 percent Clopper-Pearson bounds, checked against an exact binomial sum.
        assert!((upper_bound(1, 20) - 0.2161).abs() < 1e-3, "{}", upper_bound(1, 20));
        assert!((upper_bound(1, 10) - 0.3942).abs() < 1e-3, "{}", upper_bound(1, 10));
        assert!((upper_bound(5, 50) - 0.1988).abs() < 1e-3, "{}", upper_bound(5, 50));
        assert_eq!(upper_bound(4, 4), 1.0);
    }

    #[test]
    fn the_rate_is_worded_the_same_everywhere() {
        assert_eq!(rate_words(0, 30), "rate 0.0 percent, one-sided 95 percent upper bound 10.0 percent (3/n rule)");
        assert!(rate_words(1, 20).starts_with("rate 5.0 percent, one-sided 95 percent upper bound 21.6 percent"));
    }
}
