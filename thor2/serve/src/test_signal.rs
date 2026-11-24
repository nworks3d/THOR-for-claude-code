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
    /// The run was only STARTED in the background; its outcome was not seen.
    pub background: bool,
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

/// The file a command's output was sent to (`> out.txt`, `>> out.txt`, `2> out.txt`, `&> out.txt`,
/// `| tee out.txt`), when there is one other than a null device or a stream (`2>&1`).
fn output_file_of(command: &str) -> Option<String> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let clean = |raw: &str| raw.trim_matches(|c| c == '"' || c == '\'').to_string();
    let mut found = None;
    for (at, token) in tokens.iter().enumerate() {
        let target = if *token == "tee" {
            tokens.get(at + 1).filter(|next| !next.starts_with('-')).map(|next| clean(next))
        } else if let Some(start) = token.find('>') {
            let after = token[start..].trim_start_matches('>');
            if after.starts_with('&') {
                continue;
            }
            if after.is_empty() { tokens.get(at + 1).map(|next| clean(next)) } else { Some(clean(after)) }
        } else {
            None
        };
        if let Some(target) = target.filter(|t| !t.is_empty() && !t.eq_ignore_ascii_case("nul") && !t.starts_with("/dev/") && !t.starts_with('&')) {
            found = Some(target);
        }
    }
    found
}

/// How long an output file must have been left alone before its result is believed: a run
/// that is still writing shows the first crate's "ok" long before its last one fails.
const OUTPUT_SETTLED_SECS: u64 = 10;

/// The result of a test or build run whose output went to a file that exists now, read from
/// that file: `Some(false)` when it shows a failure, `Some(true)` when it shows a pass and no
/// failure, `None` when there is no such file, it is older than the run's start (an earlier
/// run's output), still being written, or says neither. Generic across the runners in
/// `EVAL_TEST_RUNNER_COMMANDS`.
fn result_from_output_file(row: &CallRow, command: &str) -> Option<bool> {
    let path = std::path::PathBuf::from(output_file_of(command)?);
    let path = if path.is_absolute() || row.cwd.is_empty() { path } else { Path::new(&row.cwd).join(path) };
    let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
    let modified_unix = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    if modified_unix + 60 < row.t || modified.elapsed().map_or(true, |age| age.as_secs() < OUTPUT_SETTLED_SECS) {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let tail = String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(200_000)..]).into_owned();
    let failed = tail.contains("test result: FAILED")
        || tail.contains("error: could not compile")
        || tail.contains("error: test failed")
        || tail.lines().any(|l| {
            let l = l.trim_start();
            l.starts_with("error[E") || l.starts_with("FAIL") || l.starts_with("FAILED") || l.starts_with("--- FAIL")
                || l.strip_prefix("# fail ").is_some_and(|n| n.trim() != "0")
                || (l.contains(" failed") && l.contains(" passed") && !l.contains("0 failed") && !l.starts_with("test result"))
        });
    if failed {
        return Some(false);
    }
    // A build prints "Finished" only at its very end; a test run prints it before the tests start.
    let is_build = command.contains("cargo build");
    let passed = tail.contains("test result: ok")
        || tail.lines().any(|l| {
            let l = l.trim();
            l == "OK" || l == "# fail 0" || l.starts_with("ok  \t") || (is_build && l.starts_with("Finished ")) || (l.contains(" passed") && !l.contains("failed"))
        });
    passed.then_some(true)
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
            // A run started in the background answers only that it started; when its output went
            // to a file that is there now, the result is read from that file.
            let ok = row.ok.or_else(|| command_of(row).and_then(|command| result_from_output_file(row, command)));
            last_run = Some(LastRun { at: row.t, runner: runner.to_string(), ok, background: corpus::started_in_background(&row.input) });
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
        None if run.background => "started in the background, result unknown",
        None => "result not recorded",
    };
    let run_words = format!("last run: {} {result}, {}", run.runner, age_words(now, run.at));
    if signal.may_skip(now) {
        return format!("{LABEL} no code change since the last test or build run ({run_words}) - may skip: quote that run in step 1 instead of running the tests again");
    }
    let why = if signal.changes_since > 0 {
        format!("{} code change(s) since the last test or build run", signal.changes_since)
    } else if run.background && run.ok.is_none() {
        "the last run was only started in the background and its result is unknown, so run it again".to_string()
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

    /// A test run started in the background answers only that it started.
    #[test]
    fn a_background_start_is_not_a_passed_run() {
        let started = row("1", NOW - HOUR, "Bash", json!({ "command": "cargo test", "run_in_background": true }), None);
        let signal = of(&[started]);
        assert!(!signal.may_skip(NOW));
        let text = line(&signal, NOW);
        assert!(!text.contains("passed"), "{text}");
        assert!(text.contains("started in the background, result unknown") && text.contains("run it again"), "{text}");
    }

    /// A background run whose output went to a file is read from that file once it is settled:
    /// a pass is a pass, a failure is a failure, and a file that is missing, older than the run,
    /// still being written or silent leaves the result unknown.
    #[test]
    fn a_background_run_with_its_output_in_a_file_is_read_from_that_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.txt");
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let settle = |path: &std::path::Path, text: &str| {
            std::fs::write(path, text).unwrap();
            let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
            file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(120)).unwrap();
        };
        let run = |command: String| {
            let mut started = row("1", now - 600, "Bash", json!({ "command": command, "run_in_background": true }), None);
            started.cwd = dir.path().display().to_string();
            of(&[started])
        };
        let command = format!("cargo test > {} 2>&1", out.display());

        assert_eq!(run(command.clone()).last_run.unwrap().ok, None, "no output file yet");
        settle(&out, "running 3 tests\ntest result: ok. 3 passed; 0 failed\n");
        let passed = run(command.clone());
        assert_eq!(passed.last_run.as_ref().unwrap().ok, Some(true));
        assert!(line(&passed, now).contains("passed"), "{}", line(&passed, now));
        settle(&out, "test result: ok. 2 passed\ntest result: FAILED. 1 passed; 1 failed\n");
        assert_eq!(run(command.clone()).last_run.unwrap().ok, Some(false), "one failing crate fails the run");
        settle(&out, "Compiling thing\n");
        assert_eq!(run(command.clone()).last_run.unwrap().ok, None, "neither a pass nor a failure in it");
        // A relative target is found from the call's working folder, `tee` and `2>` work too.
        settle(&out, "test result: ok. 1 passed\n");
        assert_eq!(run("cargo test 2>&1 | tee out.txt".to_string()).last_run.unwrap().ok, Some(true));
        // Output from before the run started, or still being written, is not this run's result.
        let file = std::fs::OpenOptions::new().write(true).open(&out).unwrap();
        file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200)).unwrap();
        assert_eq!(run(command.clone()).last_run.unwrap().ok, None, "an earlier run's file");
        std::fs::write(&out, "test result: ok. 1 passed\n").unwrap();
        assert_eq!(run(command).last_run.unwrap().ok, None, "still being written");
        assert_eq!(output_file_of("cargo test > /dev/null 2>&1"), None);
        assert_eq!(output_file_of("cargo test 2>&1"), None);
    }

    /// Field case: a Python project ran `python -m unittest` and was told to run
    /// its ten-minute suite again, because only pytest was recognised.
    #[test]
    fn every_python_test_command_is_a_test_run() {
        for command in [
            "python -m unittest discover -s tests",
            "python3 -m unittest tests.test_a",
            "py -m unittest -v",
            "py -3 -m unittest",
            "cd C:/proj && python.exe -m unittest",
            "python -m coverage run -m unittest",
            "python -m pytest -x",
            "py -m pytest tests",
            "pytest -q",
        ] {
            let signal = of(&[edit("1", NOW - 5 * HOUR, "app.py"), bash("2", NOW - 4 * HOUR, command, Some(true))]);
            assert!(signal.last_run.is_some(), "not seen as a test run: {command}");
            assert!(signal.may_skip(NOW), "a green run with no change after it may be skipped: {command}");
        }
        let after = of(&[bash("1", NOW - 4 * HOUR, "python -m unittest", Some(true)), edit("2", NOW - HOUR, "app.py")]);
        assert!(!after.may_skip(NOW), "a code change after the run still means run again");
        let not_a_run = of(&[bash("1", NOW - HOUR, "python manage.py migrate", Some(true))]);
        assert_eq!(not_a_run.last_run, None);
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
