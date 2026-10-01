//! HOW OFTEN ONE AGENT SEES THE SAME FACT (step 27, field test 3; one full
//! showing per context since step 28).
//!
//! Field case 2026-10-01: in 60 minutes two sessions and three subagents were
//! handed 628 fact showings (250 KB) for work that needed a dozen; one fact was
//! put in front of one subagent 78 times, on 70% of its shell calls. A fact
//! bound to a command such as `cd` or `node --test` fires on every such call,
//! and an agent that has read it once learns nothing from a second copy.
//! The owner's decision, 2026-10-02: "1x per sessie is meer dan voldoende".
//!
//! So what is DISPLAYED is limited, per context. A context is one session's
//! main conversation or one subagent (`key_for`): a subagent starts with a
//! fresh context that has not seen anything, so it is counted apart. For one
//! context and one fact:
//!
//! - the first showing carries the fact in full;
//! - every later showing shows nothing at all, no reminder line either;
//! - the count is reset only by `reset`, called at a new session start (also a
//!   resume or a compaction, which lose what the agent was shown). An owner's
//!   prompt resets nothing: a fact seen once in this context stays seen.
//!
//! Only DISPLAY is limited. Every refusal, requirement and check runs before
//! this and knows nothing of it: a rule that blocks a call blocks it the
//! hundredth time too.
//!
//! JUDGEMENT DEBT. A serving is an `ItemServed` event, recorded for what is
//! shown in full (the caller records `Arranged::selection`). A suppressed
//! showing is not a serving, so repeats cannot build debt; and the debt counts
//! a fact once per session (`usefulness::served_since_last_verdict`).
//!
//! NEAR-IDENTICAL FACTS in one block (two facts that say the same about the
//! same place) are shown once: the higher ranked one stays, the other is left
//! out of this block and not counted as shown.
//!
//! State is one small file per context beside the store (`shown-counts/`), so
//! two contexts never write the same file. Best effort like every sidecar: a
//! file that cannot be read or written means every fact shows in full again,
//! never that one is hidden by mistake.

use crate::rank::RankedItem;
use crate::render::{self, Selection};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

/// The folder beside the store.
pub const DIR: &str = "shown-counts";

/// Two facts count as the same when at least this share of the smaller one's
/// words (four letters or more) is in the other.
const SAME_OVERLAP: f64 = 0.6;

/// ...and only when the smaller one has at least this many such words, so two
/// short facts sharing a couple of words are never merged.
const SAME_MIN_WORDS: usize = 8;

/// Counting files older than this are removed when a new one is written.
const KEEP_SECS: u64 = 2 * 24 * 3600;

/// How many counting files may sit there before old ones are looked for.
const PRUNE_ABOVE: usize = 200;

/// What one block is made of after the limit.
pub struct Arranged {
    /// The facts shown in full, capped like every block.
    pub selection: Selection,
    /// How many facts this context had already been shown and were left out.
    pub suppressed: usize,
}

impl Arranged {
    /// Ids of everything displayed.
    pub fn displayed_ids(&self) -> Vec<&str> {
        self.selection.shown.iter().map(|r| r.id.as_str()).collect()
    }
}

/// The ids this context has been shown in full.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Seen {
    seen: BTreeSet<String>,
}

/// The context a hook call belongs to: the session's main conversation, or one
/// subagent of it.
pub fn key_for(session_id: &str, agent_id: Option<&str>) -> String {
    match agent_id.filter(|a| !a.is_empty()) {
        Some(agent) => format!("{session_id}-{agent}"),
        None => session_id.to_string(),
    }
}

fn file_for(db: &Path, key: &str) -> PathBuf {
    let name: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).take(120).collect();
    db.parent().unwrap_or_else(|| Path::new(".")).join(DIR).join(format!("{name}.json"))
}

/// What the file says. A file of the earlier format (a map of counts) is read
/// as well: every id in it has been shown at least once.
fn read(db: &Path, key: &str) -> Seen {
    let Some(text) = std::fs::read_to_string(file_for(db, key)).ok() else { return Seen::default() };
    if let Ok(seen) = serde_json::from_str::<Seen>(&text) {
        return seen;
    }
    #[derive(serde::Deserialize)]
    struct Earlier {
        counts: std::collections::BTreeMap<String, u32>,
    }
    serde_json::from_str::<Earlier>(&text).map(|e| Seen { seen: e.counts.into_keys().collect() }).unwrap_or_default()
}

fn write(db: &Path, key: &str, seen: &Seen) {
    let path = file_for(db, key);
    if crate::store_dir::is_held(&path) {
        return;
    }
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string(seen) {
        let _ = std::fs::write(&path, text);
    }
    prune(dir);
}

fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let entries: Vec<_> = entries.flatten().collect();
    if entries.len() <= PRUNE_ABOVE {
        return;
    }
    let now = std::time::SystemTime::now();
    for entry in entries {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age.as_secs() > KEEP_SECS);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Forget what this context has been shown: a new session start (also a resume
/// or a compaction). Nothing else calls this.
pub fn reset(db: &Path, key: &str) {
    let path = file_for(db, key);
    if crate::store_dir::is_held(&path) {
        return;
    }
    let _ = std::fs::remove_file(path);
}

/// Words of four letters or more, lowercased.
fn words_of(text: &str) -> HashSet<String> {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 4).map(str::to_string).collect()
}

fn same_thing(a: &HashSet<String>, b: &HashSet<String>) -> bool {
    let smaller = a.len().min(b.len());
    if smaller < SAME_MIN_WORDS {
        return false;
    }
    a.intersection(b).count() as f64 / smaller as f64 >= SAME_OVERLAP
}

/// `all` (ranked, worst first) without any fact that near-duplicates a higher
/// ranked one.
fn without_near_duplicates(all: &[RankedItem]) -> Vec<&RankedItem> {
    let mut kept: Vec<(&RankedItem, HashSet<String>)> = Vec::new();
    for item in all {
        let words = words_of(&item.item.text);
        if kept.iter().any(|(_, other)| same_thing(&words, other)) {
            continue;
        }
        kept.push((item, words));
    }
    kept.into_iter().map(|(item, _)| item).collect()
}

/// Decide what one block shows this context, and count it. `all` is the ranked
/// list the block would have been made from; `record` is false for a run that
/// must leave no trace (the counts are then read but not written).
pub fn arrange(db: &Path, key: &str, all: &[RankedItem], record: bool) -> Arranged {
    let mut seen = read(db, key);
    let mut fresh: Vec<RankedItem> = Vec::new();
    let mut suppressed = 0;
    for item in without_near_duplicates(all) {
        match seen.seen.contains(&item.id) {
            true => suppressed += 1,
            false => fresh.push(item.clone()),
        }
    }
    let arranged = Arranged { selection: render::cap(fresh), suppressed };
    if record && !arranged.displayed_ids().is_empty() {
        seen.seen.extend(arranged.displayed_ids().into_iter().map(str::to_string));
        write(db, key, &seen);
    }
    arranged
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Item, Kind};

    fn ranked(id: &str, text: &str) -> RankedItem {
        RankedItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Orientation,
                text: text.to_string(),
                bindings: vec![Binding::Always],
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

    fn state() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        (dir, db)
    }

    #[test]
    fn a_fact_is_shown_in_full_once_then_never_again_in_that_context() {
        let (_dir, db) = state();
        let all = vec![ranked("f1", "Never trust the working directory after a cd in a shell call.")];
        let mut shown = Vec::new();
        let mut suppressed = Vec::new();
        for _ in 0..4 {
            let a = arrange(&db, "s1", &all, true);
            shown.push(a.selection.shown.len());
            suppressed.push(a.suppressed);
        }
        assert_eq!(shown, vec![1, 0, 0, 0]);
        assert_eq!(suppressed, vec![0, 1, 1, 1]);
    }

    #[test]
    fn two_contexts_count_apart_and_a_reset_starts_over() {
        let (_dir, db) = state();
        let all = vec![ranked("f1", "Some fact that is long enough to be a fact.")];
        arrange(&db, "s1", &all, true);
        assert_eq!(arrange(&db, "s1", &all, true).selection.shown.len(), 0);
        assert_eq!(arrange(&db, "s1-agentA", &all, true).selection.shown.len(), 1, "a subagent starts at zero");
        assert_eq!(arrange(&db, "s1-agentA", &all, true).selection.shown.len(), 0);
        assert_eq!(arrange(&db, "s1-agentB", &all, true).selection.shown.len(), 1, "each subagent is its own context");
        reset(&db, "s1");
        assert_eq!(arrange(&db, "s1", &all, true).selection.shown.len(), 1, "a new session start shows it again");
    }

    #[test]
    fn a_hidden_fact_leaves_its_slot_to_the_next_one() {
        let (_dir, db) = state();
        let mut all: Vec<RankedItem> = (0..5).map(|i| ranked(&format!("f{i}"), &format!("distinct fact number {i} about topic{i} alpha{i} beta{i}"))).collect();
        arrange(&db, "s1", &all[..4], true);
        all.rotate_left(0);
        let a = arrange(&db, "s1", &all, true);
        let ids: Vec<&str> = a.selection.shown.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["f4"], "the four that were seen are quiet, the new one shows");
        assert_eq!(a.suppressed, 4);
    }

    #[test]
    fn near_identical_facts_in_one_block_are_shown_once() {
        let (_dir, db) = state();
        let a = ranked("f1", "The preview folder is filled automatically by the restart trigger which copies from the dev clone - never copy there by hand.");
        let b = ranked("f2", "Preview folder is now filled automatically by the restart trigger, copying from the dev clone, so do not copy there by hand anymore.");
        let c = ranked("f3", "Totally different knowledge about invoices and the vat basis used in quotes for customers.");
        let arranged = arrange(&db, "s1", &[a, b, c], true);
        let ids: Vec<&str> = arranged.selection.shown.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["f1", "f3"]);
    }

    #[test]
    fn short_facts_sharing_words_are_never_merged() {
        let (_dir, db) = state();
        let a = ranked("f1", "Run tests from server/ first.");
        let b = ranked("f2", "Run tests from server/ always.");
        assert_eq!(arrange(&db, "s1", &[a, b], true).selection.shown.len(), 2);
    }

    #[test]
    fn a_run_that_records_nothing_changes_nothing() {
        let (dir, db) = state();
        let all = vec![ranked("f1", "A fact that may be shown any number of times in a run that records nothing.")];
        for _ in 0..6 {
            assert_eq!(arrange(&db, "s1", &all, false).selection.shown.len(), 1);
        }
        assert!(!dir.path().join(DIR).exists());
    }

    #[test]
    fn an_unreadable_counting_file_shows_everything_again() {
        let (dir, db) = state();
        let all = vec![ranked("f1", "A fact long enough to be a fact for this test of an unreadable file.")];
        std::fs::create_dir_all(dir.path().join(DIR)).unwrap();
        std::fs::write(file_for(&db, "s1"), "not json").unwrap();
        assert_eq!(arrange(&db, "s1", &all, true).selection.shown.len(), 1);
    }

    #[test]
    fn a_counting_file_of_the_earlier_format_still_counts_as_seen() {
        let (dir, db) = state();
        let all = vec![ranked("f1", "A fact long enough to be a fact for this test of the earlier format.")];
        std::fs::create_dir_all(dir.path().join(DIR)).unwrap();
        std::fs::write(file_for(&db, "s1"), r#"{"counts":{"f1":2}}"#).unwrap();
        assert_eq!(arrange(&db, "s1", &all, true).selection.shown.len(), 0);
    }
}
