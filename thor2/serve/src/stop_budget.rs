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
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The steps that must stay together in the rotation: the report validation
/// reads the verifications the three records before it write, so it never runs
/// at a Stop that skipped one of them, and they take their turn as one unit.
const ATTEST_UNIT: &[&str] = &["refusal verifications", "write verifications", "foreign write verifications", "report validation"];
const ATTEST_KEY: &str = "attest unit";

/// How many sessions' owed steps the record keeps.
const KEEP_SESSIONS: usize = 40;

fn unit_of(step: &str) -> String {
    if ATTEST_UNIT.contains(&step) {
        ATTEST_KEY.to_string()
    } else {
        step.to_string()
    }
}

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
    /// Per session: the steps its latest Stop did not get to, and when. The next
    /// Stop of that session runs these first.
    #[serde(default)]
    pub owed: BTreeMap<String, Owed>,
}

/// The upkeep steps one session's Stops still owe.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owed {
    pub at: i64,
    pub steps: Vec<String>,
    /// When each step last ran at a Stop of this session, unix seconds.
    #[serde(default)]
    pub last_ran: BTreeMap<String, i64>,
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
    /// A test's allowance: this many steps per Stop instead of a time.
    max_steps: Option<usize>,
    /// The session whose owed steps are kept, with the db they are kept beside.
    keep: Option<(PathBuf, String)>,
    /// The steps (as units) the session's last Stop did not get to.
    owed: Vec<String>,
    ran: RefCell<BTreeSet<String>>,
    skipped_steps: RefCell<Vec<String>>,
    /// Whether any step of this Stop asked to be recorded.
    recording: std::cell::Cell<bool>,
}

impl StopBudget {
    /// A budget of `STOP_UPKEEP_BUDGET`, counted from now.
    pub fn new() -> Self {
        Self::with_limit(STOP_UPKEEP_BUDGET)
    }

    pub fn with_limit(limit: Duration) -> Self {
        Self {
            started: Instant::now(),
            limit: Some(limit),
            skipped: std::cell::Cell::new(false),
            max_steps: None,
            keep: None,
            owed: Vec::new(),
            ran: RefCell::default(),
            skipped_steps: RefCell::default(),
            recording: std::cell::Cell::new(false),
        }
    }

    /// A budget that is never spent.
    pub fn unlimited() -> Self {
        let mut budget = Self::with_limit(Duration::ZERO);
        budget.limit = None;
        budget
    }

    /// The budget of one session's Stop: the steps its last Stop skipped come
    /// first now (`allow`), and the ones this Stop skips are kept for the next.
    pub fn for_session(db: &Path, session_id: &str) -> Self {
        let mut budget = Self::new();
        budget.owed = read(db).owed.get(session_id).map(|o| o.steps.iter().cloned().collect()).unwrap_or_default();
        budget.keep = Some((db.to_path_buf(), session_id.to_string()));
        budget
    }

    /// A session budget that allows `steps` steps instead of a time, for tests.
    #[cfg(test)]
    fn for_session_with_steps(db: &Path, session_id: &str, steps: usize) -> Self {
        let mut budget = Self::for_session(db, session_id);
        budget.limit = Some(Duration::from_secs(3600));
        budget.max_steps = Some(steps);
        budget
    }

    /// Whether the allowance is used up.
    pub fn spent(&self) -> bool {
        match self.max_steps {
            Some(max) => self.ran.borrow().len() >= max,
            None => self.limit.is_some_and(|limit| self.started.elapsed() >= limit),
        }
    }

    /// Whether half the allowance is used up: from here on only a step the last
    /// Stop skipped may start while one of those is still waiting.
    fn half_spent(&self) -> bool {
        match self.max_steps {
            Some(max) => self.ran.borrow().len() >= max / 2,
            None => self.limit.is_some_and(|limit| self.started.elapsed() >= limit / 2),
        }
    }

    /// The step skipped longest ago that has not run yet at this Stop.
    fn oldest_owed(&self) -> Option<String> {
        let ran = self.ran.borrow();
        self.owed.iter().find(|unit| !ran.contains(*unit)).cloned()
    }

    /// Whether the upkeep step `step` may start. When it may not, the first such
    /// step of this Stop is counted in the record (`record` false: nothing is
    /// written, a read-only or no-record call).
    pub fn allow(&self, db: &Path, step: &str, record: bool) -> bool {
        if record {
            self.recording.set(true);
        }
        let unit = unit_of(step);
        let first_ask = !self.ran.borrow().contains(&unit);
        // A step the last Stop skipped has its turn first; one that ran last
        // time gives way once half the allowance is used and one of those is
        // still waiting. A step already running (it asks again to see if it may
        // go on) is judged by the same rule, so a slow one is cut for them.
        let yields = self.half_spent() && self.oldest_owed().is_some_and(|first| first != unit);
        // The report validation never runs at a Stop that skipped a record it reads.
        let lacks_input = step == "report validation" && self.skipped_steps.borrow().iter().any(|s| s != step && ATTEST_UNIT.contains(&s.as_str()));
        let in_started_unit = self.max_steps.is_some() && unit != step && self.ran.borrow().contains(&unit);
        if (!self.spent() || in_started_unit) && !yields && !lacks_input {
            if first_ask {
                self.ran.borrow_mut().insert(unit);
            }
            return true;
        }
        {
            let mut skipped = self.skipped_steps.borrow_mut();
            if !skipped.iter().any(|s| s == step) {
                skipped.push(step.to_string());
            }
        }
        if !self.skipped.replace(true) && record {
            let _lock = crate::store_dir::lock(&crate::store_dir::lock_path_of(&path(db)));
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

impl Drop for StopBudget {
    /// Keep what this Stop did not get to for the session's next one.
    fn drop(&mut self) {
        let Some((db, session)) = &self.keep else { return };
        if !self.recording.get() {
            return;
        }
        let ran = self.ran.borrow();
        // Oldest first: what was already owed and still is, then what this Stop skipped.
        let mut steps: Vec<String> = self.owed.iter().filter(|u| !ran.contains(*u)).cloned().collect();
        for step in self.skipped_steps.borrow().iter() {
            let unit = unit_of(step);
            if !steps.contains(&unit) {
                steps.push(unit);
            }
        }
        let _lock = crate::store_dir::lock(&crate::store_dir::lock_path_of(&path(db)));
        let mut skips = read(db);
        let now = crate::time::now_unix();
        let mut entry = skips.owed.get(session).cloned().unwrap_or_default();
        for unit in ran.iter() {
            entry.last_ran.insert(unit.clone(), now);
        }
        entry.at = now;
        entry.steps = steps;
        skips.owed.insert(session.clone(), entry);
        while skips.owed.len() > KEEP_SESSIONS {
            let Some(oldest) = skips.owed.iter().min_by_key(|(_, o)| o.at).map(|(k, _)| k.clone()) else { break };
            skips.owed.remove(&oldest);
        }
        if let Ok(text) = serde_json::to_string(&skips) {
            let _ = crate::store_dir::write(&path(db), text);
        }
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

    /// Eight steps, a budget that allows one per Stop: the step skipped at one
    /// Stop is the first to run at the next, so every step gets its turn.
    #[test]
    fn with_one_step_per_stop_every_step_runs_within_n_stops() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let steps = ["refusal verifications", "write verifications", "note reads", "report validation", "real-call corpus", "crowding check", "leftover scripts", "evaluation ask"];
        let mut ran: BTreeSet<&str> = BTreeSet::new();
        for _ in 0..steps.len() {
            let budget = StopBudget::for_session_with_steps(&db, "s1", 1);
            for step in steps {
                if budget.allow(&db, step, true) {
                    ran.insert(step);
                }
            }
        }
        // the four attest steps are one unit and count as one turn
        assert_eq!(ran.len(), steps.len(), "every step ran at least once: {ran:?}");
    }

    #[test]
    fn a_slow_step_that_always_ran_first_no_longer_starves_the_ones_behind_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let first = StopBudget::for_session_with_steps(&db, "s1", 2);
        assert!(first.allow(&db, "note reads", true));
        assert!(first.allow(&db, "real-call corpus", true));
        assert!(!first.allow(&db, "evaluation ask", true));
        drop(first);
        assert_eq!(read(&db).owed["s1"].steps, vec!["evaluation ask".to_string()]);
        let second = StopBudget::for_session_with_steps(&db, "s1", 2);
        assert!(second.allow(&db, "note reads", true), "before half the allowance is used");
        assert!(second.allow(&db, "real-call corpus", true) == false, "gives way to the step owed");
        assert!(second.allow(&db, "evaluation ask", true));
    }

    #[test]
    fn the_report_validation_waits_for_the_records_it_reads() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let budget = StopBudget::for_session_with_steps(&db, "s1", 10);
        assert!(budget.allow(&db, "refusal verifications", true));
        budget.skipped_steps.borrow_mut().push("write verifications".to_string());
        assert!(!budget.allow(&db, "report validation", true));
    }

    #[test]
    fn an_unlimited_budget_is_never_spent() {
        assert!(!StopBudget::unlimited().spent());
    }
}
