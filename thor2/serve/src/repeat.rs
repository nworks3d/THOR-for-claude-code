//! HOW OFTEN ONE AGENT SEES THE SAME FACT (step 27, field test 3).
//!
//! Field case 2026-10-01: in 60 minutes two sessions and three subagents were
//! handed 628 fact showings (250 KB) for work that needed a dozen; one fact was
//! put in front of one subagent 78 times, on 70% of its shell calls. A fact
//! bound to a command such as `cd` or `node --test` fires on every such call,
//! and an agent that has read it twice learns nothing from a third copy.
//!
//! So what is DISPLAYED is limited, per reader. A reader is one session's main
//! conversation or one subagent (`key_for`). For one reader and one fact:
//!
//! - the first `FULL_SHOWINGS` showings carry the fact in full;
//! - the next ones, up to `LAST_SHOWING` in all, are one line: the id and the
//!   head of the text, enough to jog a memory and to `get` the rest;
//! - after that nothing, until the reader's count is reset (`reset`): a new
//!   session start, or a prompt the owner typed himself, which opens a new turn
//!   of the conversation and may need the fact again. A subagent's count is never
//!   reset: an agent id is one run.
//!
//! Only DISPLAY is limited. Every refusal, requirement and check runs before
//! this and knows nothing of it: a rule that blocks a call blocks it the
//! hundredth time too.
//!
//! JUDGEMENT DEBT. A serving is an `ItemServed` event, and 40 of them without a
//! verdict make an item owe one. Only a FULL showing is recorded as a serving
//! (the caller records `Arranged::selection`, never the reminders). A one-line
//! reminder carries no new content and a suppressed showing nothing at all, so
//! neither is something the agent could be asked to judge; counting them would
//! make the debt grow with the very repeats this module removes.
//!
//! NEAR-IDENTICAL FACTS in one block (two facts that say the same about the
//! same place) are shown once: the higher ranked one stays, the other is left
//! out of this block and not counted as shown.
//!
//! State is one small file per reader beside the store (`shown-counts/`), so
//! two readers never write the same file. Best effort like every sidecar: a
//! file that cannot be read or written means every fact shows in full again,
//! never that one is hidden by mistake.

use crate::rank::RankedItem;
use crate::render::{self, Selection};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// The folder beside the store.
pub const DIR: &str = "shown-counts";

/// How many showings of one fact one reader gets in full.
pub const FULL_SHOWINGS: u32 = 2;

/// How many showings (full and one-line together) one reader gets at all.
pub const LAST_SHOWING: u32 = 5;

/// How many one-line reminders one block carries at most.
pub const MAX_REMINDERS: usize = 6;

/// How much of a fact's text a reminder keeps, in characters.
const REMINDER_HEAD_CHARS: usize = 60;

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

/// What one block is made of after the limits.
pub struct Arranged {
    /// The facts shown in full, capped like every block.
    pub selection: Selection,
    /// The facts shown as one line.
    pub reminders: Vec<RankedItem>,
}

impl Arranged {
    /// Ids of everything displayed, in full or as a line.
    pub fn displayed_ids(&self) -> Vec<&str> {
        self.selection.shown.iter().chain(self.reminders.iter()).map(|r| r.id.as_str()).collect()
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Counts {
    counts: BTreeMap<String, u32>,
}

/// The reader a hook call belongs to: the session's main conversation, or one
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

fn read(db: &Path, key: &str) -> Counts {
    std::fs::read_to_string(file_for(db, key)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn write(db: &Path, key: &str, counts: &Counts) {
    let path = file_for(db, key);
    if crate::store_dir::is_held(&path) {
        return;
    }
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string(counts) {
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

/// Forget what this reader has been shown: a new session start, or a prompt
/// the owner typed himself.
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

/// One line for a fact this reader has already seen twice.
pub fn reminder_line(ranked: &RankedItem) -> String {
    let flat = ranked.item.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let head: String = flat.chars().take(REMINDER_HEAD_CHARS).collect();
    let cut = if flat.chars().count() > REMINDER_HEAD_CHARS { "..." } else { "" };
    format!("- [{}] (seen before) {head}{cut}", ranked.id)
}

/// Decide what one block shows this reader, and count it. `all` is the ranked
/// list the block would have been made from; `record` is false for a run that
/// must leave no trace (the counts are then read but not written).
pub fn arrange(db: &Path, key: &str, all: &[RankedItem], record: bool) -> Arranged {
    let mut counts = read(db, key);
    let mut full: Vec<RankedItem> = Vec::new();
    let mut reminders: Vec<RankedItem> = Vec::new();
    for item in without_near_duplicates(all) {
        let seen = counts.counts.get(&item.id).copied().unwrap_or(0);
        if seen >= LAST_SHOWING {
            continue;
        }
        if seen >= FULL_SHOWINGS {
            reminders.push(item.clone());
        } else {
            full.push(item.clone());
        }
    }
    let selection = render::cap(full);
    reminders.truncate(MAX_REMINDERS);
    let arranged = Arranged { selection, reminders };
    if record {
        for id in arranged.displayed_ids() {
            *counts.counts.entry(id.to_string()).or_insert(0) += 1;
        }
        if !arranged.displayed_ids().is_empty() {
            write(db, key, &counts);
        }
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
    fn a_fact_is_shown_in_full_twice_then_as_a_line_then_not_at_all() {
        let (_dir, db) = state();
        let all = vec![ranked("f1", "Never trust the working directory after a cd in a shell call.")];
        let mut shapes = Vec::new();
        for _ in 0..7 {
            let a = arrange(&db, "s1", &all, true);
            shapes.push((a.selection.shown.len(), a.reminders.len()));
        }
        assert_eq!(shapes, vec![(1, 0), (1, 0), (0, 1), (0, 1), (0, 1), (0, 0), (0, 0)]);
    }

    #[test]
    fn two_readers_count_apart_and_a_reset_starts_over() {
        let (_dir, db) = state();
        let all = vec![ranked("f1", "Some fact that is long enough to be a fact.")];
        for _ in 0..4 {
            arrange(&db, "s1", &all, true);
        }
        assert_eq!(arrange(&db, "s1", &all, true).selection.shown.len(), 0);
        assert_eq!(arrange(&db, "s1-agentA", &all, true).selection.shown.len(), 1, "a subagent starts at zero");
        reset(&db, "s1");
        assert_eq!(arrange(&db, "s1", &all, true).selection.shown.len(), 1, "a new turn shows it again");
    }

    #[test]
    fn a_hidden_fact_leaves_its_slot_to_the_next_one() {
        let (_dir, db) = state();
        let mut all: Vec<RankedItem> = (0..5).map(|i| ranked(&format!("f{i}"), &format!("distinct fact number {i} about topic{i} alpha{i} beta{i}"))).collect();
        for _ in 0..5 {
            arrange(&db, "s1", &all[..4], true);
        }
        all.rotate_left(0);
        let a = arrange(&db, "s1", &all, true);
        let ids: Vec<&str> = a.selection.shown.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["f4"], "the four that were seen five times are quiet, the new one shows");
        assert!(a.reminders.is_empty());
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
    fn a_reminder_is_one_line_with_the_id_and_the_head_of_the_text() {
        let line = reminder_line(&ranked("f1", &format!("{} {}", "a".repeat(80), "tail")));
        assert!(line.starts_with("- [f1] (seen before) aaaa"), "{line}");
        assert!(line.ends_with("..."), "{line}");
        assert!(!line.contains('\n'));
        assert!(line.chars().count() < 100, "{line}");
    }

    #[test]
    fn an_unreadable_counting_file_shows_everything_again() {
        let (dir, db) = state();
        let all = vec![ranked("f1", "A fact long enough to be a fact for this test of an unreadable file.")];
        std::fs::create_dir_all(dir.path().join(DIR)).unwrap();
        std::fs::write(file_for(&db, "s1"), "not json").unwrap();
        assert_eq!(arrange(&db, "s1", &all, true).selection.shown.len(), 1);
    }
}
