//! A time limit for the upkeep part of a Stop.
//!
//! A Stop does two kinds of work. The checks that refuse or protect (the reply
//! rules, the first-session setup, an owner decision that was not stored, the
//! stale-rule guard, the rule-violation holds) always run. The upkeep (verifying
//! earlier writes against the transcript, validating filed reports, recording
//! the real-call corpus, the crowding check, leftover scripts, the evaluation
//! ask) only catches the log up with things that already happened, and each of
//! those steps works from its own state, so a step skipped at one Stop is simply
//! done at the next.
//!
//! `StopBudget` is one wall-clock allowance for all of that upkeep. A step asks
//! `allow` before it starts; once the allowance is spent every later step is
//! skipped for this Stop, and the first skip is counted in a small file beside
//! the store (`stop-budget-skips.json`) so `doctor` can say how often it happened.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long the upkeep of one Stop may run before the rest is left for the next.
pub const STOP_UPKEEP_BUDGET: Duration = Duration::from_secs(5);

const FILE: &str = "stop-budget-skips.json";

/// The record of Stops that ran out of budget.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skips {
    /// How many Stops skipped upkeep.
    pub count: u64,
    /// When the latest one did, unix seconds.
    pub last_at: i64,
    /// The first step that was skipped at that Stop.
    pub last_step: String,
}

pub fn path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(FILE)
}

/// The record, or an empty one when there is none.
pub fn read(db: &Path) -> Skips {
    std::fs::read_to_string(path(db)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub struct StopBudget {
    started: Instant,
    limit: Option<Duration>,
    skipped: std::cell::Cell<bool>,
}

impl StopBudget {
    /// A budget of `STOP_UPKEEP_BUDGET`, counted from now.
    pub fn new() -> Self {
        Self::with_limit(STOP_UPKEEP_BUDGET)
    }

    pub fn with_limit(limit: Duration) -> Self {
        Self { started: Instant::now(), limit: Some(limit), skipped: std::cell::Cell::new(false) }
    }

    /// A budget that is never spent.
    pub fn unlimited() -> Self {
        Self { started: Instant::now(), limit: None, skipped: std::cell::Cell::new(false) }
    }

    /// Whether the allowance is used up.
    pub fn spent(&self) -> bool {
        self.limit.is_some_and(|limit| self.started.elapsed() >= limit)
    }

    /// Whether the upkeep step `step` may start. When it may not, the first such
    /// step of this Stop is counted in the record (`record` false: nothing is
    /// written, a read-only or no-record call).
    pub fn allow(&self, db: &Path, step: &str, record: bool) -> bool {
        if !self.spent() {
            return true;
        }
        if !self.skipped.replace(true) && record {
            let mut skips = read(db);
            skips.count += 1;
            skips.last_at = crate::time::now_unix();
            skips.last_step = step.to_string();
            if let Ok(text) = serde_json::to_string(&skips) {
                let _ = crate::store_dir::write(&path(db), text);
            }
        }
        false
    }

    /// Whether any step was skipped in this Stop.
    pub fn skipped(&self) -> bool {
        self.skipped.get()
    }
}

impl Default for StopBudget {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_budget_allows_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let budget = StopBudget::new();
        assert!(budget.allow(&db, "step", true));
        assert!(!budget.skipped());
        assert!(!path(&db).exists());
    }

    #[test]
    fn a_spent_budget_skips_every_step_and_counts_the_stop_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let budget = StopBudget::with_limit(Duration::ZERO);
        assert!(!budget.allow(&db, "first", true));
        assert!(!budget.allow(&db, "second", true));
        assert!(budget.skipped());
        let skips = read(&db);
        assert_eq!(skips.count, 1);
        assert_eq!(skips.last_step, "first");
        // The next Stop that runs out is counted again.
        assert!(!StopBudget::with_limit(Duration::ZERO).allow(&db, "later", true));
        assert_eq!(read(&db).count, 2);
    }

    #[test]
    fn a_call_that_records_nothing_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert!(!StopBudget::with_limit(Duration::ZERO).allow(&db, "step", false));
        assert!(!path(&db).exists());
    }

    #[test]
    fn an_unlimited_budget_is_never_spent() {
        assert!(!StopBudget::unlimited().spent());
    }
}
