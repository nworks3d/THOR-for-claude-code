//! THE TEST SIGNAL: whether the evaluation has to run a project's tests again,
//! read from the real-call corpus (`corpus`) and nothing else.
//!
//! WHY IT EXISTS. The evaluation routine's step 1 used to say "run the tests
//! now". Measured on 2026-10-09: repeated full suites were one of the biggest
//! wastes of a usage limit, and an evaluation that opens right after a green
//! run on unchanged code learns nothing from running it again. THOR already
//! records every tool call of every session at each Stop, tests and builds
//! included, with whether they succeeded. This module answers one question
//! from that record: since the last test or build run in this project, did any
//! code change?
//!
//! WHAT COUNTS. A run is a Bash or PowerShell call whose command contains a
//! test or build runner (`usefulness::EVAL_TEST_RUNNER_COMMANDS`), in this
//! project. A change is an Edit, Write, MultiEdit or NotebookEdit call on a
//! file that is not prose (`.md`, `.txt`, `.rst`), or a command that rewrites
//! the working tree under git (`pull`, `checkout`, `merge`, `rebase`, `reset`,
//! `cherry-pick`, `apply`, `stash pop`) or edits in place (`sed -i`, `patch`) -
//! made by any session or subagent, after that run.
//!
//! WHEN THE TESTS MAY BE SKIPPED. Only when the last run succeeded, is not
//! older than `FRESH_DAYS` days, and no change followed it. A failed run, an
//! unknown result, an old run, or any change after it: run the tests again.
//! The record is written at each Stop, so a change made in the turn that is
//! still running is not in it; the routine says so.
//!
//! Best effort like the corpus itself: no corpus, no signal, and then the
//! answer is the old one - run the tests.

use crate::corpus::{self, CallRow};
use crate::usefulness::EVAL_TEST_RUNNER_COMMANDS;
use std::path::Path;

/// The label `doctor --eval` prints this line under, and the evaluation routine
/// names. THOR checks its doctor against its routine (`thor_bugs::routine_gaps`).
pub const LABEL: &str = "test signal:";

/// A recorded run older than this many days never lets the tests be skipped:
/// dependencies and the machine change without a code edit in the project.
pub const FRESH_DAYS: i64 = 7;

/// The newest calls read for one project.
const CALLS_READ: usize = 40_000;

/// File extensions whose edit is no code change.
const PROSE: &[&str] = &[".md", ".txt", ".rst"];

/// Commands that change the working tree without an editor tool.
const TREE_CHANGING: &[&str] =
    &["git pull", "git checkout", "git switch", "git merge", "git rebase", "git reset", "git cherry-pick", "git apply", "git stash pop", "sed -i", "patch -p"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastRun {
    pub at: i64,
    /// The runner found in the command (`cargo test`), not the whole command.
    pub runner: String,
    /// `None` when the result was never seen.
    pub ok: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestSignal {
    pub last_run: Option<LastRun>,
    /// Code changes recorded after `last_run` (all of them when there is none).
    pub changes_since: usize,
}

fn command_of(row: &CallRow) -> Option<&str> {
    match row.tool.as_str() {
        "Bash" | "PowerShell" => row.input.get("command").and_then(|c| c.as_str()),
        _ => None,
    }
}

fn runner_in(command: &str) -> Option<&'static str> {
    EVAL_TEST_RUNNER_COMMANDS.iter().copied().find(|runner| command.contains(runner))
}

/// Whether `row` changed code.
fn is_change(row: &CallRow) -> bool {
    if let Some(command) = command_of(row) {
        return TREE_CHANGING.iter().any(|c| command.contains(c));
    }
    if matches!(row.tool.as_str(), "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
        let path = row.input.get("file_path").or_else(|| row.input.get("notebook_path")).and_then(|p| p.as_str()).unwrap_or("");
        return !PROSE.iter().any(|ext| path.ends_with(ext));
    }
    false
}

/// The signal for `project`, from the corpus beside `db`, as of `now`.
pub fn read(db: &Path, project: &str, now: i64) -> TestSignal {
    let needle = format!("\"p\":\"{project}\"");
    let (rows, _, _) = corpus::select_calls(db, now, CALLS_READ, &|line| line.contains(&needle));
    signal_of(rows.iter().filter(|row| row.p.as_deref() == Some(project)))
}

/// The signal over `rows`, oldest first.
pub fn signal_of<'a>(rows: impl Iterator<Item = &'a CallRow>) -> TestSignal {
    let mut last_run: Option<LastRun> = None;
    let mut changes_since = 0usize;
    for row in rows {
        if let Some(runner) = command_of(row).and_then(runner_in) {
            last_run = Some(LastRun { at: row.t, runner: runner.to_string(), ok: row.ok });
            changes_since = 0;
        } else if is_change(row) {
            changes_since += 1;
        }
    }
    TestSignal { last_run, changes_since }
}

impl TestSignal {
    /// Whether step 1 may quote the recorded run instead of running the tests.
    pub fn may_skip(&self, now: i64) -> bool {
        match &self.last_run {
            Some(run) => run.ok == Some(true) && self.changes_since == 0 && now - run.at < FRESH_DAYS * 86_400,
            None => false,
        }
    }
}

fn age_words(now: i64, at: i64) -> String {
    let secs = (now - at).max(0);
    match secs {
        s if s < 3600 => format!("{} minute(s) ago", s / 60),
        s if s < 86_400 => format!("{} hour(s) ago", s / 3600),
        s => format!("{} day(s) ago", s / 86_400),
    }
}

/// The one line `doctor --eval` prints. It always carries the exact verdict
/// the routine keys on: "may skip" or "run the tests".
pub fn line(signal: &TestSignal, now: i64) -> String {
    let Some(run) = &signal.last_run else {
        return format!("{LABEL} no test or build run of this project is on record (last 30 days) - run the tests in step 1");
    };
    let result = match run.ok {
        Some(true) => "passed",
        Some(false) => "FAILED",
        None => "result not recorded",
    };
    let run_words = format!("last run: {} {result}, {}", run.runner, age_words(now, run.at));
    if signal.may_skip(now) {
        return format!("{LABEL} no code change since the last test or build run ({run_words}) - may skip: quote that run in step 1 instead of running the tests again");
    }
    let why = if signal.changes_since > 0 {
        format!("{} code change(s) since the last test or build run", signal.changes_since)
    } else if run.ok != Some(true) {
        "the last run did not pass or its result is unknown".to_string()
    } else {
        format!("the last run is older than {FRESH_DAYS} days")
    };
    format!("{LABEL} {why} ({run_words}) - run the tests in step 1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_800_014_400;
    const HOUR: i64 = 3600;
    const DAY: i64 = 86_400;

    fn row(id: &str, t: i64, tool: &str, input: serde_json::Value, ok: Option<bool>) -> CallRow {
        CallRow {
            k: "c".into(),
            id: id.into(),
            t,
            s: "abcd1234".into(),
            a: false,
            tool: tool.into(),
            cwd: String::new(),
            p: Some("fxp".into()),
            d: None,
            input,
            r: vec![],
            oth: false,
            ok,
            h: false,
            cut: false,
            b: String::new(),
        }
    }

    fn bash(id: &str, t: i64, command: &str, ok: Option<bool>) -> CallRow {
        row(id, t, "Bash", json!({ "command": command }), ok)
    }

    fn edit(id: &str, t: i64, path: &str) -> CallRow {
        row(id, t, "Edit", json!({ "file_path": path }), Some(true))
    }

    fn of(rows: &[CallRow]) -> TestSignal {
        signal_of(rows.iter())
    }

    #[test]
    fn a_passing_run_with_no_change_after_it_may_be_skipped() {
        let signal = of(&[edit("1", NOW - 5 * HOUR, "src/a.rs"), bash("2", NOW - 4 * HOUR, "cargo test --release", Some(true))]);
        assert_eq!(signal.changes_since, 0);
        assert!(signal.may_skip(NOW));
        let text = line(&signal, NOW);
        assert!(text.starts_with("test signal: no code change since the last test or build run"), "{text}");
        assert!(text.contains("cargo test passed, 4 hour(s) ago") && text.contains("may skip"), "{text}");
    }

    #[test]
    fn a_code_edit_after_the_run_means_run_the_tests() {
        let signal = of(&[bash("1", NOW - 4 * HOUR, "cargo test", Some(true)), edit("2", NOW - HOUR, "src/a.rs"), edit("3", NOW - 30 * 60, "src/b.rs")]);
        assert_eq!(signal.changes_since, 2);
        assert!(!signal.may_skip(NOW));
        let text = line(&signal, NOW);
        assert!(text.contains("2 code change(s) since the last test or build run") && text.ends_with("run the tests in step 1"), "{text}");
    }

    #[test]
    fn a_later_run_resets_the_count() {
        let signal = of(&[edit("1", NOW - 5 * HOUR, "src/a.rs"), bash("2", NOW - 4 * HOUR, "cargo test", Some(true)), edit("3", NOW - 3 * HOUR, "src/a.rs"), bash("4", NOW - 2 * HOUR, "cargo build", Some(true))]);
        assert_eq!(signal.changes_since, 0);
        assert_eq!(signal.last_run.as_ref().unwrap().runner, "cargo build");
        assert!(signal.may_skip(NOW));
    }

    #[test]
    fn prose_edits_are_no_code_change_but_a_tree_rewrite_is() {
        let prose = of(&[bash("1", NOW - 4 * HOUR, "npm test", Some(true)), edit("2", NOW - HOUR, "README.md"), edit("3", NOW - HOUR, "notes.txt")]);
        assert!(prose.may_skip(NOW), "documentation carries no behaviour a test could catch");
        for command in ["git pull origin main", "git checkout other", "git merge feature", "git rebase main", "sed -i s/a/b/ src/x.js"] {
            let signal = of(&[bash("1", NOW - 4 * HOUR, "npm test", Some(true)), bash("2", NOW - HOUR, command, Some(true))]);
            assert_eq!(signal.changes_since, 1, "{command}");
            assert!(!signal.may_skip(NOW), "{command}");
        }
    }

    #[test]
    fn a_failed_unknown_or_old_run_is_never_skipped() {
        let failed = of(&[bash("1", NOW - HOUR, "cargo test", Some(false))]);
        assert!(!failed.may_skip(NOW));
        assert!(line(&failed, NOW).contains("FAILED") && line(&failed, NOW).contains("did not pass"), "{}", line(&failed, NOW));
        let unknown = of(&[bash("1", NOW - HOUR, "pytest -q", None)]);
        assert!(!unknown.may_skip(NOW));
        assert!(line(&unknown, NOW).contains("result not recorded"));
        let old = of(&[bash("1", NOW - 8 * DAY, "go test ./...", Some(true))]);
        assert!(!old.may_skip(NOW));
        assert!(line(&old, NOW).contains("older than 7 days"), "{}", line(&old, NOW));
    }

    #[test]
    fn no_run_on_record_says_run_the_tests() {
        let none = of(&[edit("1", NOW - HOUR, "src/a.rs")]);
        assert_eq!(none.last_run, None);
        assert!(!none.may_skip(NOW));
        assert!(line(&none, NOW).contains("no test or build run of this project is on record"));
    }

    #[test]
    fn the_signal_is_read_for_one_project_from_the_corpus_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let mut other = bash("9", NOW - 3 * HOUR, "cargo test", Some(false));
        other.p = Some("elsewhere".into());
        let rows = [
            bash("1", NOW - 5 * HOUR, "cargo test", Some(true)),
            other,
            edit("3", NOW - 4 * HOUR, "src/a.rs"),
        ];
        let text: String = rows.iter().map(|r| serde_json::to_string(r).unwrap() + "\n").collect();
        std::fs::write(corpus::calls_path(&db), text).unwrap();
        let signal = read(&db, "fxp", NOW);
        assert_eq!(signal.changes_since, 1, "the other project's failed run is not this project's");
        assert_eq!(signal.last_run.as_ref().unwrap().ok, Some(true));
        let empty = tempfile::tempdir().unwrap();
        let none = read(&empty.path().join("thor.db"), "fxp", NOW);
        assert_eq!(none, TestSignal { last_run: None, changes_since: 0 }, "no corpus, no signal");
    }
}
