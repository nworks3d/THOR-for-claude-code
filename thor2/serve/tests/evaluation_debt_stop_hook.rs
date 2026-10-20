//! THE EVALUATION DEBT (`serve::usefulness::eval_debt_owed` for the pure
//! predicate, `serve::usefulness::update_eval_debt_state`/`record_eval_
//! debt_asked` for the sidecar writes, `bin/serve.rs`'s own `evaluation_
//! debt` for the Stop-hook wiring), end to end through the real compiled
//! `serve hook` binary - mirrors `setup_debt_stop_hook.rs`'s own shape for
//! the same reason: the pure decision logic already has its own unit tests
//! (`usefulness.rs`'s `eval_debt_predicate_tests`/`eval_debt_state_tests`,
//! `bin/serve.rs`'s `evaluation_debt_tests`); this file proves the WIRING -
//! the real event store, the real sidecar file on disk, the real hook JSON
//! shape on stdout, the "this session has actually worked here long enough"
//! gate, and above all the two properties that cannot be proven at the unit
//! level at all, because they live in `hook_once`'s payload dispatch rather
//! than in `evaluation_debt` itself: this debt must NEVER hold a subagent's
//! own Stop, and it must speak BEFORE the judgement debt when both are due.
//!
//! THE TRIGGER WAS REWRITTEN FOUR TIMES, three of them on 2026-09-16. First,
//! a single global "newest verdict" clock - which any verdict on ANY item
//! applying to a checkout reset, global items included - was replaced with
//! a per-project sidecar (`eval-debt-state.json`, `serve::usefulness::
//! ProjectEvalState`) tracking how long a backlog of ten-or-more items had
//! sat continuously at or over that ceiling, and whether an evaluation
//! report had been seen for it since. Then, the same day, the ceiling
//! itself was dropped entirely in favour of asking daily, per project
//! actually worked in, once the session had put in at least `serve::
//! usefulness::EVAL_MIN_SESSION_MINUTES` there - measured against a rolling
//! 24 hours since the later of `tracking_since`/`last_evaluation_seen`.
//! Then, still the same day, no evaluation was ever asked for a checkout
//! that resolves to no project at all, since no Report can be filed there
//! to silence it (`model::gate`'s ground 21, `NO_SCOPE_PROBLEM`).
//!
//! THE FOURTH REWRITE (owner's decision, 2026-09-16: he does not want to
//! ever have to run an evaluation himself) drops the 24-hour rolling window
//! and `tracking_since` entirely, in favour of a UTC CALENDAR DAY: the
//! obligation holds whenever no evaluation report for this project has been
//! first seen on the current UTC day (`serve::usefulness::eval_done_today`,
//! `crate::time::same_utc_day`), regardless of how long ago THOR started
//! tracking the project - so a project with NO report ever seen fires the
//! very first time enough minutes are worked, with no first-day grace
//! period any more. The once-per-SESSION gate (the old `eval-debt-
//! asked.json`) is retired too: the debt now blocks the FIRST STOP OF EVERY
//! TURN for as long as it holds (`blocks_on_the_stop_of_a_new_turn_again_
//! and_again_while_no_report_exists` below), relying entirely on Claude
//! Code's own `stop_hook_active` (`does_not_block_twice_in_the_same_turn`)
//! for the "at most once per turn" safety - never a second copy of that
//! mechanism. Every ask is now counted on the PROJECT's own sidecar entry
//! (`asked_count`/`first_asked_since_report`) instead of a session's, reset
//! the moment a new report is seen (`the_ask_counter_increments_per_turn_
//! and_resets_once_a_report_is_seen`). `seed_valid_report`/`recently_covered`/
//! `no_longer_covered` below give the project a VALID report filed at an
//! exact instant relative to the real wall clock this binary reads (since
//! step 16 `last_evaluation_seen` comes from the validator's record in the
//! log, and every Stop overwrites the sidecar's copy with it);
//! `seed_tracking_since` (still calling the real, unchanged `update_eval_
//! debt_state` write) remains only for the no-project tests, which care
//! about whether the sidecar is touched AT ALL, never about which
//! particular field it carries.
//!
//! A SEVENTH REWRITE (2026-09-17, `serve::usefulness`'s own "evaluation
//! debt" section) drops the UTC CALENDAR DAY the fourth rewrite chose, in
//! favour of a rolling `usefulness::EVAL_REPORT_COVERS_HOURS`-hour window
//! from `last_evaluation_seen` (`usefulness::eval_report_covers`, replacing
//! `eval_done_today`/`crate::time::same_utc_day`) - measured (acme-shop
//! eval 2): a report filed about 01:30 local time (23:30 UTC) left `doctor`
//! saying "today's evaluation is not done" 39 minutes later, at 00:09 UTC,
//! naming that very report as the newest thing in the store. `recently_
//! covered`/`no_longer_covered` replace the retired `start_of_today_utc`;
//! `seed_session_work_with_risk`'s own doc comment covers the accrual-side
//! half of this same rewrite.
//!
//! EVERY PAYLOAD BELOW NAMES AN EXPLICIT `cwd`, unlike this file's own
//! earlier shape - the sidecar is keyed by the EXACT project a Stop
//! resolves to (`project::resolve_project`), not by `project::applies_to`'s
//! looser "or global" rule the rest of the debt machinery uses, so a test
//! that seeds the sidecar for `None` (global) must make sure the subprocess
//! actually resolves to `None` too, rather than inheriting whatever
//! directory `cargo test` itself happened to be invoked from (which sits
//! inside this very repository, and so resolves to a REAL project).
//! `Sandbox::global_cwd`/`Sandbox::project_dir` give every test a `cwd` with
//! a known, deliberate resolution instead of an ambient one.
//!
//! Every test below runs the hook with `USERPROFILE`/`HOME` pointed at a
//! throwaway sandbox directory (mirrors `ops/tests/install_eval_command.rs`'s
//! own sandbox), so the message this debt prints - which names a real file
//! path when one exists - never depends on, and never reads, anything under
//! the real user's own `~/.claude`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use model::item::{Binding, Item, Kind, TargetKind};
use thor_core::event_store::{EventKind, EventStore};

/// A comfortably-sized backlog for the message's own "N item(s) currently
/// owe a verdict here" context line - no longer tied to any threshold the
/// evaluation debt itself gates on (see this file's own module doc comment
/// for why the ceiling is gone).
const OWED_CONTEXT_COUNT: usize = 12;
const AFTER: usize = serve::usefulness::JUDGEMENT_DEBT_AFTER;

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self { home: tempfile::tempdir().unwrap() }
    }

    fn eval_command_path(&self) -> PathBuf {
        self.home.path().join(".claude").join("commands").join("thor-eval.md")
    }

    /// Write a (fake, non-empty) eval command file at the exact path
    /// `usefulness::default_eval_command_path` would resolve to under this
    /// sandbox's own HOME - so the debt's message can name a file that is
    /// really there.
    fn seed_eval_command(&self) {
        let path = self.eval_command_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "fixture eval routine").unwrap();
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", self.home.path());
        cmd.env("USERPROFILE", self.home.path());
    }

    /// A real, empty directory with no marker and no `.git` anywhere above
    /// it (it lives under the OS temp directory, never inside this
    /// repository), so `project::resolve_project` reliably resolves it to
    /// `None` - the deliberate stand-in for "global", used instead of
    /// simply omitting `cwd` from a payload, which would instead inherit
    /// wherever `cargo test` itself was invoked from (inside this very
    /// repository, and so a REAL project). This is also the fixture for "no
    /// project at all", which is a checkout the evaluation debt must never
    /// speak in - see this file's own module doc comment.
    fn global_cwd(&self) -> PathBuf {
        let dir = self.home.path().join("no-project-cwd");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A real, empty directory carrying a `.thor-project` marker naming
    /// `project` - so a payload's own `cwd` resolves to a REAL, named
    /// project (`project::resolve_project`). Needed by every test that
    /// expects the evaluation debt to actually fire: `model::gate`'s
    /// ground 21 refuses a Report with no project at all except one exempt
    /// id, so "global" is not an option for those, and the debt itself
    /// never speaks for a checkout with no project regardless of whether a
    /// report is ever filed.
    fn project_dir(&self, project: &str) -> PathBuf {
        let dir = self.home.path().join("checkouts").join(project);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".thor-project"), format!("{project}\n")).unwrap();
        dir
    }
}

/// The hook binary, run from a folder of its own with no tool server (`mcp`)
/// beside it. Run in place it would find the `mcp` the workspace build leaves
/// next to it and, as the stale-tool-server check compares a session's start
/// with that file's age, a session start written seconds ago could look older
/// than a build finished a minute ago (field test 14, H1).
fn isolated_serve() -> &'static Path {
    static COPY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    COPY.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("isolated-serve");
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join(format!("serve{}", std::env::consts::EXE_SUFFIX));
        let original = env!("CARGO_BIN_EXE_serve");
        let _ = std::fs::remove_file(&copy);
        if std::fs::hard_link(original, &copy).is_err() {
            std::fs::copy(original, &copy).unwrap();
        }
        copy
    })
}

fn run_hook(db: &Path, payload: &str, sandbox: &Sandbox) -> String {
    let mut cmd = Command::new(isolated_serve());
    cmd.arg("--db").arg(db).arg("hook").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    sandbox.apply(&mut cmd);
    let mut child = cmd.spawn().expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    assert!(out.stderr.is_empty(), "the hook must never write to stderr: {:?}", out.stderr);
    String::from_utf8(out.stdout).unwrap()
}

/// A Stop payload with an EMPTY last assistant message (the Response Guard
/// has nothing to say), `stop_hook_active: false` (the FIRST Stop of a new
/// turn), and an explicit `cwd` - see this file's own module doc comment
/// for why every payload here names one deliberately.
fn stop_payload(session_id: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": false,
        "last_assistant_message": "",
        "cwd": cwd.to_string_lossy(),
    })
    .to_string()
}

/// The identical shape as `stop_payload`, except `stop_hook_active: true` -
/// Claude Code's own signal that a Stop hook already held this turn once
/// and this is a retry within the SAME turn. Needed by `does_not_block_
/// twice_in_the_same_turn` below: this debt relies entirely on `hook_once`'s
/// own top-of-`Stop`-arm handling of this flag (`bin/serve.rs`'s
/// `already_fired` branch) for its "at most once per turn" safety, adding
/// no second copy of the mechanism itself, so this is the one payload shape
/// that actually exercises it.
fn retry_stop_payload(session_id: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": true,
        "last_assistant_message": "",
        "cwd": cwd.to_string_lossy(),
    })
    .to_string()
}

/// The same shape as `stop_payload`, plus `agent_id` - Claude Code's own
/// documented signal (per `payload_is_from_a_subagent`'s doc comment in
/// `serve.rs`) that a Stop payload arrived from inside a Task-tool subagent
/// rather than the owner's own main session.
fn subagent_stop_payload(session_id: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": false,
        "last_assistant_message": "",
        "cwd": cwd.to_string_lossy(),
        "agent_id": "a1dca2c0feb7f44fb",
        "agent_type": "general-purpose",
    })
    .to_string()
}

/// A `PreToolUse` payload from inside a subagent - the identical `agent_id`
/// signal as `subagent_stop_payload` above, on the event type the fifth
/// rewrite's own accrual (`usefulness::record_hook_event`) runs on
/// unconditionally, subagent or not (see that function's own doc comment).
fn subagent_pretooluse_payload(session_id: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "agent_id": "a1dca2c0feb7f44fb",
        "agent_type": "general-purpose",
        "tool_name": "Read",
        "tool_input": { "file_path": "fixture.txt" },
    })
    .to_string()
}

/// Declares `n` never-judged rules, scoped to `project` (`None` for
/// global), and serves each one `AFTER` times under a throwaway fixture
/// session id, so all `n` sit in this checkout's own judgement debt - feeds
/// the evaluation debt's own message, "N item(s) currently owe a verdict
/// here", never a condition for it to speak any more. Returns the ids.
///
/// TARGET-BOUND, EACH ON ITS OWN UNIQUE COMMAND ANCHOR - never `Binding::
/// Always`, since `judgement_debt_counts` (and so this debt's own owed
/// count) excludes pinned items since 2026-09-12: an all-pinned fixture here
/// would silently stop testing the evaluation debt at all. A `Command`
/// target, not `Path`: ground 19 (`model::gate`) refuses a GLOBAL item
/// anchored at a source file, and a unique value per item (rather than one
/// shared anchor) keeps every one of them clear of `model::item::MAX_ITEMS`
/// crowding regardless of how large `n` is.
///
/// NEVER SERVED UNDER A REAL SESSION ID HERE - only under the throwaway
/// "fixture" one, which feeds the OWED COUNT (`judgement_debt_counts`,
/// unscoped by session) but is never anyone's `served_ids_in_session`. A
/// test that also needs THIS session to have accrued work in the project
/// uses `seed_session_work` below instead of serving one of these owed
/// items directly - serving an OWED item under the real session would also
/// satisfy `judgement_debt`'s own per-item "seen" filter and make it a
/// second, unwanted contender for most of the tests below.
fn declare_owed_items(store: &mut EventStore, n: usize, project: Option<&str>) -> Vec<String> {
    let label = project.unwrap_or("global");
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let id = format!("owed-{label}-{i:02}");
        let item = Item {
            id: id.clone(),
            kind: Kind::Rule,
            text: format!("fixture evaluation debt item number {i} for {label}"),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-eval-debt-command-{label}-{i:02}") }],
            severity: None,
            project: project.map(str::to_string),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some(format!("fixture evaluation debt item number {i} turns out to be wrong")),
            check: None,
        };
        model::store::declare(store, "fixture", "fixture", "fixture", &item).unwrap();
        // Decided by THOR's trial (a note): since step 9 the teeth question
        // asks about every rule without a verdict, whatever its weight, and
        // these fixtures are about the judgement and evaluation debts.
        serve::teeth::fixture::decide(store, &id);
        for i in 0..AFTER {
            serve::deliver::record_delivery(store, &serve::deliver::nth_session("fixture", i), "fixture", serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-08T00:00:00Z", &[id.clone()]);
        }
        ids.push(id);
    }
    ids
}

/// Seed this (project, session) pair's own accrued-work sidecar entry
/// directly, with `minutes` worth of work already accrued - the fifth
/// rewrite's own fixture lever (2026-09-17, `serve::usefulness`'s own
/// "evaluation debt" section), replacing the retired `serve_marker_in_
/// session`/`serve_marker_in_session_minutes_ago` (which drove the OLD
/// mechanism, a wall-clock timestamp on an `item_served` event) now that
/// "how long has this session worked here" comes from hook-event accrual
/// (`serve::usefulness::SessionWorkState`/`record_hook_event`) instead.
///
/// THE ANCHOR IS BUILT FROM THE LITERAL "now" this process reads right now,
/// `minutes` in the past - safe since 2026-09-17 (seventh rewrite,
/// `serve::usefulness`'s own "evaluation debt" section) in a way it was not
/// before: `accrue_session_work` (unlike a plain `last_evaluation_seen`
/// comparison) re-derives its own answer from a FRESH wall-clock read
/// inside the real hook subprocess, so a seed built from literal "now"
/// minus `minutes` used to be able to land on a DIFFERENT UTC calendar day
/// than the subprocess's own "now" whenever a test happened to run within
/// `minutes` of real UTC midnight - measured directly: this test suite
/// failed exactly this way when run at 00:25 UTC, seeding "90 minutes ago"
/// one calendar day before the subprocess's own clock read moments later,
/// which `session_work_reset_needed` used to read as a new-day reset,
/// silently zeroing the very accrual the test meant to prove. That rule is
/// gone: `session_work_reset_needed` no longer reads a calendar day at all,
/// only whether there is an anchor yet and whether a newer report was seen,
/// so there is no boundary left here to dodge and the elaborate "pin at
/// noon UTC" workaround this comment used to describe is retired along with
/// it.
///
/// A seed `last_event_unix` that lands slightly in the FUTURE relative to
/// the real subprocess's own clock (a few milliseconds of test overhead) is
/// harmless: `accrue_session_work` only ever ADDS a gap when it is positive
/// and under `EVAL_PAUSE_MINUTES`, so a negative gap just leaves the seeded
/// total untouched, which is exactly what a fixture asking for an EXACT
/// accrued total needs anyway.
///
/// Reads the sidecar first and merges in, rather than overwriting the whole
/// file, so another session's own accrual for the same project is never
/// clobbered.
fn seed_session_work(db: &Path, project: Option<&str>, session_id: &str, minutes: i64) {
    seed_session_work_with_risk(db, project, session_id, minutes, 0, false);
}

/// The identical fixture as `seed_session_work` above, plus the two risk
/// counters the sixth rewrite (2026-09-17) added to `SessionWorkState` - see
/// `serve::usefulness`'s own "the repeat's own risk" doc comment. Needed by
/// every test proving a REPEAT ask actually fires: accrued time alone is no
/// longer enough, so `seed_session_work`'s own all-default risk (0, false)
/// would silently stay silent for any of those.
fn seed_session_work_with_risk(db: &Path, project: Option<&str>, session_id: &str, minutes: i64, edits_since_test: u32, compacted_since_anchor: bool) {
    let mut all = serve::usefulness::read_eval_debt_state(db);
    let key = project.unwrap_or("").to_string();
    let mut entry = all.get(&key).cloned().unwrap_or_default();
    let now = serve::time::now_unix();
    entry.sessions.insert(
        session_id.to_string(),
        serve::usefulness::SessionWorkState {
            anchor_unix: Some(now - minutes * 60),
            accrued_secs: minutes * 60,
            last_event_unix: Some(now),
            edits_since_test,
            compacted_since_anchor,
            // A session that worked here this long has ended a turn here
            // before: its first Stop in the project is never held (Q10).
            stops: 1,
        },
    );
    // The project's own clock (step 17, B8): never below what any one of
    // its sessions accrued, so a seeded session carries the project with it.
    if entry.work.accrued_secs < minutes * 60 {
        entry.work = serve::usefulness::SessionWorkState {
            anchor_unix: Some(now - minutes * 60),
            accrued_secs: minutes * 60,
            last_event_unix: Some(now),
            edits_since_test,
            compacted_since_anchor,
            stops: 0,
        };
    }
    all.insert(key, entry);
    std::fs::write(serve::usefulness::eval_debt_state_path(db), serde_json::to_string(&all).unwrap()).unwrap();
}

/// Seed the sidecar as though THOR started tracking this project
/// `hours_ago` hours before the real "now" - drives the sidecar-backed
/// `tracking_since` field (retired from the predicate itself, still
/// written - see `usefulness`'s own "evaluation debt" section, fourth
/// rewrite) through the exact same production write `bin/serve.rs`'s Stop
/// arm itself uses (`usefulness::update_eval_debt_state`). Kept only for
/// the "no project at all" tests below, which care about whether the
/// sidecar is touched AT ALL by a no-project Stop, never about which
/// particular field a pre-existing entry happens to carry.
fn seed_tracking_since(store: &EventStore, db: &Path, project: Option<&str>, hours_ago: i64) {
    let since = serve::time::now_unix() - hours_ago * 3600;
    serve::usefulness::update_eval_debt_state(store, db, project, since);
}

/// A VALID evaluation report of `project`, filed at unix time `seen` as far
/// as its transcript tells, and validated the way a Stop validates it (step
/// 16) - the only way to give the project a covering report at an exact
/// instant: since step 16 `last_evaluation_seen` comes from the log, and a
/// Stop overwrites whatever the sidecar says with it. Filed by an evaluating
/// session of its own (`EVALUATOR`), which saw none of the fixture's
/// firings, so its frozen judgement list is empty.
fn seed_valid_report(db: &Path, project: &str, seen: i64, id: &str) {
    let mut store = EventStore::open_existing(db).unwrap();
    let transcripts = tempfile::tempdir().unwrap();
    let report = serve::eval_filing::fixture::report(id, project, "fixture evaluation report");
    serve::eval_filing::fixture::valid_filing_at(&mut store, transcripts.path(), EVALUATOR, &report, "toolu_fixture_report", seen);
}

/// The evaluating session of the fixtures' reports (UUID-shaped: only such a
/// session has a transcript THOR reads).
const EVALUATOR: &str = "7a1e0c3a-0000-4000-8000-00000000e7a1";

/// An instant `usefulness::eval_report_covers` will always agree still
/// covers, relative to the real "now" - regardless of what wall-clock hour
/// the test suite happens to run at. Replaces the retired
/// `start_of_today_utc` (seventh rewrite, 2026-09-17: the UTC calendar day
/// it measured against is gone).
///
/// FOUR HOURS, NOT ONE - MEASURED WHY. Several tests in this file seed this
/// alongside `seed_session_work_with_risk(..., serve::usefulness::
/// EVAL_REPEAT_WORK_MINUTES + 1, ...)`, whose own anchor lands
/// `EVAL_REPEAT_WORK_MINUTES + 1` (181) minutes - about three hours - before
/// "now". `session_work_reset_needed` resets the accrual the instant a
/// report is seen AFTER that anchor (`last_evaluation_seen` strictly later
/// than `anchor_unix`), so an offset shorter than the accrual it is paired
/// with silently zeroes the very accrual the test means to prove: measured
/// directly, one hour here against a three-hour accrual reset it to zero
/// and turned a "fires" test into a false silence with no visible error
/// beyond the eventual assertion failure. Four hours clears the longest
/// accrual this file seeds with room to spare, while staying comfortably
/// inside the 16-hour window.
fn recently_covered() -> i64 {
    serve::time::now_unix() - 4 * 3600
}

/// The mirror of `recently_covered` above: an instant safely OUTSIDE
/// `usefulness::EVAL_REPORT_COVERS_HOURS`, so a fixture can prove the "does
/// not cover any more" branch regardless of wall-clock time.
fn no_longer_covered() -> i64 {
    serve::time::now_unix() - (serve::usefulness::EVAL_WEEKLY_DAYS * 24 + 1) * 3600
}

/// Settles the judgement debt `declare_owed_items` creates, by giving every
/// id it returns a plain "useful" verdict - step 2 ("Judge what fired") of
/// `eval-command.example.md`, in miniature. Needed (2026-09-25) by any
/// test that both creates owed items for a project AND then files that
/// SAME project's evaluation report: `model::store`'s own evaluation-filing
/// gate now refuses to declare a live `evaluation-report` while real
/// judgement debt remains, so `declare_report` below would otherwise be
/// refused for the exact reason this whole rebuild exists - see that
/// gate's own doc comment (`model::store::check_evaluation_report_filing`).
fn pay_down_judgement_debt(store: &mut EventStore, ids: &[String]) {
    for id in ids {
        serve::mark::record_useful(store, "fixture", "fixture", "fixture", "2026-09-17T00:00:00Z", id, None).unwrap();
    }
}

/// A live evaluation-report Report, correctly tagged and scoped - the exact
/// shape `eval-command.example.md`'s own final step now writes with
/// `remember`. `model::gate`'s ground 21 refuses a Report with no project
/// at all (except one exempt id), so this always needs a real project.
fn declare_report(store: &mut EventStore, id: &str, project: &str) {
    let item = Item {
        id: id.to_string(),
        kind: Kind::Report,
        text: "fixture evaluation report".to_string(),
        bindings: vec![],
        severity: None,
        project: Some(project.to_string()),
        tags: vec!["evaluation-report".to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    // Filed the way Claude Code files one (step 15): through THOR's remember
    // tool, for a quiet evaluation of the project, by the shared fixture -
    // in an evaluating session of its own, which saw none of the fixture's
    // firings (its frozen judgement list is empty) - and validated the way
    // the next Stop validates it (step 16).
    let transcripts = tempfile::tempdir().unwrap();
    serve::eval_filing::fixture::valid_filing(store, transcripts.path(), EVALUATOR, &item, "toolu_fixture_report");
}

/// The exact shape `ops::install::working_contract` seeds the real setup
/// note as - written out by hand (`serve/tests` has no dependency on `ops`) -
/// copied from `setup_debt_stop_hook.rs`'s own `declare_setup_note` for the
/// regression test below, which needs setup_debt to still fire first.
fn declare_setup_note(store: &mut EventStore) {
    let item = Item {
        id: model::store::SETUP_NOTE_ID.to_string(),
        kind: Kind::Rule,
        text: "walk the owner through setup once, then retract this note".to_string(),
        bindings: vec![Binding::Always],
        severity: None,
        project: None,
        tags: vec!["working-contract".to_string()],
        expires: None,
        key: None,
        falsifier: Some("this note is still served after the owner already answered".to_string()),
        check: None,
    };
    // Not "installer": that name is reserved for THOR itself and the store
    // refuses it from anything but the installer's own token.
    model::store::declare(store, "install", "install", "install-fixture", &item).unwrap();
}

// ------------------------------------------------------------- fires
//
// Every test below is scoped to a REAL project (`Sandbox::project_dir`),
// never `Sandbox::global_cwd`: a checkout with no project can never file
// the report that would silence this debt, so it must never be asked in
// the first place - see the "silences: no project at all" section below.

#[test]
fn fires_for_a_main_session_and_names_the_real_eval_path_when_it_exists() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    sandbox.seed_eval_command();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("[THOR]"), "{reason}");
    assert!(reason.contains("This project has had no evaluation in the last 7 day(s)"), "{reason}");
    assert!(reason.contains("This session has worked here for"), "{reason}");
    assert!(reason.contains(&format!("{OWED_CONTEXT_COUNT} more item(s) of this project owe a verdict because other sessions fired them")), "{reason}");
    assert!(reason.contains("It has been asked 1 time(s)"), "{reason}");
    let expected_path = sandbox.eval_command_path();
    assert!(reason.contains(&expected_path.display().to_string()), "must name the real eval file path: {reason}");
    assert!(reason.contains("/thor-eval"), "must tell the owner how to run it: {reason}");
    assert!(reason.contains("A turn cannot end until the evaluation report for this project is filed"), "{reason}");
    assert!(reason.contains("After that it is quiet for 7 day(s)"), "{reason}");
}

#[test]
fn falls_back_to_the_generic_note_when_no_eval_file_exists() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    // Deliberately no `seed_eval_command()` call: the sandbox HOME exists,
    // but nothing lives at `.claude/commands/thor-eval.md` under it.
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("[THOR]"), "{reason}");
    assert!(
        !reason.contains(&sandbox.home.path().display().to_string()),
        "must never claim a path that does not exist: {reason}"
    );
    assert!(reason.contains("no evaluation routine is installed"), "{reason}");
    assert!(reason.contains("install"), "must say install writes one: {reason}");
    assert!(
        reason.contains(&format!("--db \"{}\"", db.display())),
        "the printed install command names the store this hook reads (F7): {reason}"
    );
    assert!(reason.contains("/thor-eval"), "{reason}");
}

/// Case named in the build brief: a store where BOTH the evaluation and the
/// judgement debt are due shows the evaluation first - since an evaluation
/// settles the judgement debt anyway. Every owed item is served under the
/// real session here, on purpose - unlike every other test in this file -
/// so the judgement debt's own `seen` filter (`bin/serve.rs`'s `judgement_
/// debt`) would genuinely also fire were it ever reached.
#[test]
fn both_debts_due_shows_the_evaluation_first() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    let ids = declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    for id in &ids {
        serve::deliver::record_delivery(&mut store, "s1", "fixture", serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-08T00:00:00Z", &[id.clone()]);
    }
    drop(store);
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("Run the THOR evaluation"), "the evaluation debt must speak first: {reason}");
    assert!(!reason.contains("Judge ALL"), "the judgement debt's own per-item text must not also appear: {reason}");
}

/// Case named in the build brief: "blocks on the Stop of a new turn again
/// and again while no report exists" - three separate, fresh turns (each
/// its own `stop_hook_active: false` payload) with no report ever filed
/// must ALL block, each naming one more ask than the last - the
/// once-per-session wall this debt used to hit is gone.
#[test]
fn blocks_on_the_stop_of_a_new_turn_again_and_again_while_no_report_exists() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    for turn in 1..=3 {
        let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
        let v: serde_json::Value =
            serde_json::from_str(&out).unwrap_or_else(|e| panic!("turn {turn}: expected a decision JSON: {e}: {out}"));
        assert_eq!(v["decision"], "block", "turn {turn} must still block: {out}");
        let reason = v["reason"].as_str().unwrap();
        assert!(reason.contains(&format!("It has been asked {turn} time(s)")), "turn {turn}: {reason}");
    }
}

/// Case named in the build brief: "does not block twice in the same turn
/// (stop_hook_active)". The first Stop of a turn blocks; a RETRY of that
/// same turn (`stop_hook_active: true`, `retry_stop_payload`) must not -
/// this debt adds no loop-safety mechanism of its own, it relies entirely
/// on `hook_once`'s own top-of-`Stop`-arm handling of the flag, which never
/// even reaches this debt's own code on a retry carrying an empty message.
#[test]
fn does_not_block_twice_in_the_same_turn() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    let first = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&first).expect("the first Stop of the turn must fire");
    assert_eq!(v["decision"], "block", "fixture sanity: {first}");

    // Nothing was done since the hold: the retry never holds again (loop
    // safety). Since step 24 it is not silent either while the evaluation it
    // started is still owed - one line says so.
    let retry = run_hook(&db, &retry_stop_payload("s1", &project_dir), &sandbox);
    assert!(!is_block(&retry), "a retry with nothing done since the hold must never block again: {retry}");
    assert!(retry.contains("does not hold the turn again (nothing was done since the last hold)"), "{retry}");
}

/// FIELD DEFECT 2 (2026-10-01): both sessions did their evaluation work INSIDE
/// the turn the ask held, then ended it meaning to "file next turn" - and the
/// Stop of a held turn (`stop_hook_active`) let both go silently, so no next
/// turn ever came. Now the evaluation the session owes holds that Stop again
/// once the session did something since the hold, says to file in this very
/// turn, and lets go only after nothing more is done.
#[test]
fn a_turn_held_for_the_evaluation_is_held_again_until_its_report_is_filed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    let s = uuid(40);
    drop(EventStore::new(&db).unwrap());
    seed_session_work(&db, Some("fxp"), &s, 90);
    let first = run_hook(&db, &stop_payload(&s, &fxp), &sandbox);
    assert!(is_block(&first) && first.contains("[THOR] The evaluation of 'fxp' started"), "{first}");

    // The session works inside the held turn (a THOR-visible call), then
    // ends the turn without filing.
    {
        let mut store = EventStore::new(&db).unwrap();
        served_in(&mut store, "fxp", &s, 0);
    }
    let again = run_hook(&db, &retry_stop_payload(&s, &fxp), &sandbox);
    assert!(is_block(&again), "the evaluation it owes holds the held turn again: {again}");
    assert!(again.contains("is still open and its report is not filed") && again.contains("file the report NOW"), "{again}");

    // Nothing more done: never a loop.
    let then = run_hook(&db, &retry_stop_payload(&s, &fxp), &sandbox);
    assert!(!is_block(&then), "{then}");
    assert!(then.contains("is still open and its report is not filed"), "never silent while it is owed: {then}");
}

/// FIELD DEFECT 2, the void half: a filing refused as void promised "the next
/// Stop starts a fresh evaluation", and the Stop of the held turn let the
/// session go silently. Now that Stop starts the fresh evaluation it promised
/// and holds the turn with its start note.
#[test]
fn a_voided_evaluation_starts_the_fresh_one_inside_the_held_turn() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    let s = uuid(41);
    drop(EventStore::new(&db).unwrap());
    seed_session_work(&db, Some("fxp"), &s, 90);
    let first = run_hook(&db, &stop_payload(&s, &fxp), &sandbox);
    assert!(is_block(&first), "{first}");
    let voided = {
        let mut store = EventStore::new(&db).unwrap();
        let open = model::store::open_evaluation_start(&store, &s, "fxp").expect("the ask started one");
        served_in(&mut store, "fxp", &s, 0);
        let outside = model::store::FrozenVoid { id: "fx-active-fxp".to_string(), seq: 1, door: "a side door in the fixture".to_string() };
        serve::eval_session::void(&mut store, &open, &outside).expect("void");
        open.id
    };
    let again = run_hook(&db, &retry_stop_payload(&s, &fxp), &sandbox);
    assert!(is_block(&again), "the fresh evaluation the void promised: {again}");
    assert!(again.contains(&format!("('{voided}') was closed as void")), "{again}");
    let store = EventStore::new(&db).unwrap();
    let fresh = model::store::open_evaluation_start(&store, &s, "fxp").expect("a fresh evaluation is open");
    assert_ne!(fresh.id, voided);
    assert!(again.contains(&fresh.id), "its start note rides with the hold: {again}");
}

/// FIELD DEFECT 1 through the real hook: S1 started the project's evaluation
/// 90 minutes ago and worked a minute ago. S2's Stop is due, yet it starts no
/// second evaluation - one line names S1 - however old S1's start is (the old
/// one-hour exemption let the second start happen). Once S1 has been quiet
/// for 30 minutes, S2's Stop takes the evaluation over and S1's is closed as
/// superseded, never voided.
#[test]
fn one_project_runs_one_evaluation_and_a_quiet_one_is_taken_over() {
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    let (s1, s2) = (uuid(42), uuid(43));
    let now = serve::time::now_unix();
    let case = |quiet_minutes: i64| -> (String, std::path::PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            serve::attest::fixture::quiet_start_at(&mut store, &s1, "fxp", now - 90 * 60);
            served_in(&mut store, "fxp", "fixture", 0);
            let at = serve::time::iso8601_from_unix(now - quiet_minutes * 60);
            serve::mark::record_useful(&mut store, &s1, "mcp", "mcp", &at, "fx-active-fxp", None).unwrap();
        }
        seed_project_clock(&db, "fxp", 120);
        seed_own_work(&db, "fxp", &s2, 30, 1);
        let out = run_hook(&db, &stop_payload(&s2, &fxp), &sandbox);
        (out, db, dir)
    };
    let (active, db, _keep) = case(1);
    assert!(!is_block(&active), "{active}");
    assert!(active.contains(&format!("is running in session {s1}")) && active.contains("since its last activity"), "{active}");
    let store = EventStore::new(&db).unwrap();
    assert!(model::store::open_evaluation_start(&store, &s2, "fxp").is_none(), "no second evaluation was started");
    drop(store);

    let (quiet, db, _keep) = case(45);
    assert!(is_block(&quiet) && quiet.contains("Run the THOR evaluation"), "{quiet}");
    let store = EventStore::new(&db).unwrap();
    assert!(model::store::open_evaluation_start(&store, &s2, "fxp").is_some(), "S2 runs it now");
    let s1_start = model::store::evaluation_starts_of_session(&store, &s1).pop().unwrap();
    assert!(model::store::open_evaluation_start(&store, &s1, "fxp").is_none(), "S1's is no longer open");
    assert_eq!(s1_start.frozen.void, None, "superseded, never voided");
}

/// Case named in the build brief: "the ask counter increments per blocked
/// turn and resets when a report is seen" - proven end to end: three fresh
/// turns each name the next count on the sidecar itself, then a filed
/// report resets `asked_count`/`first_asked_since_report` back to
/// zero/`None` (doctor reads the identical fields - see `ops::health`'s own
/// `judgement_debt_line` tests for the doctor-facing half of this).
#[test]
fn the_ask_counter_increments_per_turn_and_resets_once_a_report_is_seen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    let owed_ids = declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    for turn in 1..=3 {
        run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
        let state = serve::usefulness::project_eval_state(&db, Some("thor-fixture"));
        assert_eq!(state.asked_count, turn, "turn {turn}: the ask count must match exactly");
        assert!(state.first_asked_since_report.is_some(), "turn {turn}: the first-asked clock must be set");
    }

    // Settle the judgement debt before filing - see `pay_down_judgement_
    // debt`'s own doc comment: the filing gate now refuses otherwise.
    let mut store = EventStore::open_existing(&db).unwrap();
    pay_down_judgement_debt(&mut store, &owed_ids);
    declare_report(&mut store, "eval-thor-fixture-2026-09-17", "thor-fixture");
    seed_session_work(&db, Some("thor-fixture"), "s2", 90);
    drop(store);
    let after_report = run_hook(&db, &stop_payload("s2", &project_dir), &sandbox);
    assert!(after_report.trim().is_empty(), "a freshly filed report must silence this turn: {after_report}");

    let state = serve::usefulness::project_eval_state(&db, Some("thor-fixture"));
    assert_eq!(state.asked_count, 0, "a newly filed report must reset the ask counter");
    assert_eq!(state.first_asked_since_report, None, "and clear the first-asked clock");
}

// ------------------------------------------------------------- silences

#[test]
fn is_silent_for_a_subagent_payload_on_the_same_store() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    let out = run_hook(&db, &subagent_stop_payload("s1", &project_dir), &sandbox);
    assert!(out.trim().is_empty(), "a subagent's Stop must never be held for the evaluation debt: {out}");
}

/// Case named in the build brief: subagent `PreToolUse` events accrue work
/// for their own session (`usefulness::record_hook_event` runs
/// unconditionally, subagent or not - see that function's own doc comment),
/// but a subagent `Stop` still never blocks, however much that accrual has
/// grown - proven by reading the sidecar directly after the `PreToolUse`
/// call, then sending a `Stop` for the SAME session and confirming silence.
#[test]
fn subagent_pretooluse_events_accrue_work_but_a_subagent_stop_never_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    seed_session_work(&db, Some("thor-fixture"), "sub1", 90);

    let out = run_hook(&db, &subagent_pretooluse_payload("sub1", &project_dir), &sandbox);
    assert!(out.trim().is_empty(), "PreToolUse never blocks for this debt, subagent or not: {out}");
    let accrued_after_pretooluse = serve::usefulness::project_eval_state(&db, Some("thor-fixture"))
        .sessions
        .get("sub1")
        .expect("a subagent's own PreToolUse must still record its own session's accrual")
        .accrued_secs;
    assert!(
        accrued_after_pretooluse >= 90 * 60,
        "the seeded 90 minutes must still be there, not reset by being a subagent event: {accrued_after_pretooluse}"
    );

    let out = run_hook(&db, &subagent_stop_payload("sub1", &project_dir), &sandbox);
    assert!(
        out.trim().is_empty(),
        "a subagent's Stop must never be held for the evaluation debt, however much it has accrued: {out}"
    );
}

/// Case named in the build brief: zero items owed still fires once enough
/// time has been worked - the item count is message context now, never a
/// condition. Deliberately no `declare_owed_items` call at all.
#[test]
fn fires_with_zero_items_owed_once_worked_long_enough() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let store = EventStore::new(&db).unwrap();
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "a zero-item backlog must still owe an evaluation: {out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("No item is on this evaluation's verdict list"), "{reason}");
}

/// A read-only COPY of the memory cannot start or file an evaluation (no
/// writable store, no write tools), so its Stop must never hold the turn for
/// one - the same store, session and work that hold a normal Stop above.
#[test]
fn a_copy_never_holds_the_turn_for_an_evaluation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let store = EventStore::new(&db).unwrap();
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(out.contains("\"block\""), "control: without the copy marker the same Stop is held: {out}");

    thor_core::copy_mode::write_marker(&db, &Default::default()).unwrap();
    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(!out.contains("block"), "a copy must not hold the turn for an evaluation: {out}");
    assert!(!out.contains("evaluation"), "and must not nag about one: {out}");
}

/// Case named in the build brief: the session's first serving here was less
/// than an hour ago -> silent, however long this project has gone without a
/// report.
#[test]
fn is_silent_when_the_session_has_worked_here_less_than_an_hour() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 30);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(out.trim().is_empty(), "less than an hour worked here must never speak: {out}");
}

/// Case named in the build brief: "59 minutes silent" - one minute under
/// the floor, however long this project has gone without a report.
#[test]
fn is_silent_at_fifty_nine_minutes_worked() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 59);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(out.trim().is_empty(), "59 minutes must not yet be enough: {out}");
}

/// Case named in the build brief: a session with no accrued work yet is
/// silent - the gate against hijacking a session that has done nothing here
/// at all. Since the fifth rewrite (2026-09-17), this is also exactly what a
/// session's very FIRST hook event in a project looks like: `record_hook_
/// event` resets a never-before-seen (project, session) pair's accrual to
/// zero rather than crashing or guessing (`usefulness::accrue_session_
/// work`'s own "the very first event is a reset to zero" behaviour).
#[test]
fn is_silent_for_a_session_with_no_accrued_work_yet() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    // Deliberately no `seed_session_work` call: this is "s1"'s very first
    // hook event ever seen in this project.
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(out.trim().is_empty(), "a session with no accrued work yet must not be hijacked into the evaluation: {out}");
}

/// Case named in the build brief: a report that still covers silences it
/// for as long as it covers - proven across TWO separate fresh turns, not
/// just the one Stop that immediately follows the report.
#[test]
fn a_recently_seen_report_silences_it_for_as_long_as_it_covers() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    seed_valid_report(&db, "thor-fixture", recently_covered(), "eval-thor-fixture-covering");
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);

    let first = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(first.trim().is_empty(), "a report that still covers must silence the first turn: {first}");
    let second = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(second.trim().is_empty(), "and every turn after it, for as long as it keeps covering: {second}");
}

/// Case named in the build brief: a report seen outside the coverage window
/// does not silence the obligation.
#[test]
fn a_report_outside_the_coverage_window_does_not_silence_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    seed_valid_report(&db, "thor-fixture", no_longer_covered(), "eval-thor-fixture-covering");
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("a report outside the coverage window must not buy silence: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
}

/// Part G: two days after a report, however long the work goes on (59 minutes
/// or 10 hours), a small plan is not asked again: the rhythm is one
/// evaluation per week.
#[test]
fn two_days_after_a_report_nothing_is_asked_however_long_the_work() {
    for minutes in [59, 61, 600] {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let sandbox = Sandbox::new();
        let project_dir = sandbox.project_dir("thor-fixture");

        let mut store = EventStore::new(&db).unwrap();
        declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
        drop(store);
        seed_valid_report(&db, "thor-fixture", serve::time::now_unix() - 2 * 86400, "eval-thor-fixture-covering");
        seed_session_work(&db, Some("thor-fixture"), "s1", minutes);
        let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
        assert!(out.trim().is_empty(), "{minutes} minutes, two days after a report, must be silent on a small plan: {out}");
    }
}

/// A week and an hour after a report the project is owed its evaluation
/// again, and the ask says the rhythm in one line.
#[test]
fn a_week_after_a_report_the_ask_comes_and_names_the_rhythm() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    seed_valid_report(&db, "thor-fixture", no_longer_covered(), "eval-thor-fixture-covering");
    seed_session_work(&db, Some("thor-fixture"), "s1", 61);
    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("a week-old report must be asked about again: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("The last evaluation report ('eval-thor-fixture-covering') is 7 day(s) old"), "{reason}");
    assert!(reason.contains("your plan is a smaller one, so this check runs once a week at most"), "{reason}");
}

/// The plan decides: the same store with a Max plan on record and rot (owed
/// verdicts) is asked after two days, and the ask says it is daily.
#[test]
fn on_max_rot_brings_the_ask_back_after_a_day() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    serve::plan::write(&db, serve::plan::Plan::Max, serve::plan::PlanSource::Confirmed).unwrap();
    seed_valid_report(&db, "thor-fixture", serve::time::now_unix() - 2 * 86400, "eval-thor-fixture-covering");
    seed_session_work(&db, Some("thor-fixture"), "s1", 61);
    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("Max with rot must be asked after a day: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("your plan is Max and something is wrong in this project, so the check runs daily"), "{reason}");
}

// ------------------------------------------------------------ repeat: fires
//
// Added 2026-09-17 (fifth rewrite, `serve::usefulness`'s own "evaluation
// debt" section): once a report already exists for today, `EVAL_FIRST_WORK_
// MINUTES` no longer applies at all - only `EVAL_REPEAT_WORK_MINUTES`,
// measured from whenever that report reset the accrual, does.

/// A report seen today, with accrued work since it well past `EVAL_FIRST_
/// WORK_MINUTES` but nowhere near `EVAL_REPEAT_WORK_MINUTES`, must stay
/// silent - the first threshold simply does not apply any more once today's
/// report already exists.
#[test]
fn a_report_seen_today_with_90_minutes_accrued_since_stays_silent() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    seed_valid_report(&db, "thor-fixture", recently_covered(), "eval-thor-fixture-covering");
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(out.trim().is_empty(), "well past the first threshold but nowhere near the repeat one must stay silent: {out}");
}

/// The 3-hour re-ask is gone (part G): 181 minutes of work after a report seen
/// today, with the untested edits that used to be the risk, still asks nothing
/// - end to end, through the real hook binary.
#[test]
fn a_report_seen_today_with_three_hours_and_a_risk_no_longer_fires_a_repeat() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    seed_valid_report(&db, "thor-fixture", recently_covered(), "eval-thor-fixture-2026-09-17");
    seed_session_work_with_risk(
        &db,
        Some("thor-fixture"),
        "s1",
        181,
        serve::usefulness::EVAL_REPEAT_MIN_UNTESTED_EDITS,
        true,
    );

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(out.trim().is_empty(), "no repeat ask after a few hours, whatever the risk: {out}");
}

/// THE CENTRAL CASE THE SIXTH REWRITE EXISTS FOR (owner's decision,
/// 2026-09-17): the identical fixture as the test just above, MINUS any
/// risk, must now stay silent - end to end, through the real hook binary.
#[test]
fn three_hours_accrued_with_no_risk_at_all_stays_silent_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    seed_valid_report(&db, "thor-fixture", recently_covered(), "eval-thor-fixture-2026-09-17");
    seed_session_work(&db, Some("thor-fixture"), "s1", 181);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(out.trim().is_empty(), "three hours accrued with no code change and no summary must stay silent: {out}");
}

/// Case named in the build brief: subagent edits count toward the risk
/// counter, but a subagent Stop never blocks - proven with the SAME
/// three-hours-plus-a-report fixture the "fires" test above uses, so a
/// version of the code that forgot the risk gate entirely would be caught
/// here too. `agent_id` marks every payload below as coming from a
/// Task-tool subagent, the identical signal `subagent_pretooluse_payload`/
/// `subagent_stop_payload` already use.
#[test]
fn subagent_untested_edits_count_toward_the_risk_counter_but_a_subagent_stop_never_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    drop(store);
    seed_valid_report(&db, "thor-fixture", recently_covered(), "eval-thor-fixture-2026-09-17");
    seed_session_work(&db, Some("thor-fixture"), "sub1", 181);

    for _ in 0..serve::usefulness::EVAL_REPEAT_MIN_UNTESTED_EDITS {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "sub1",
            "cwd": project_dir.to_string_lossy(),
            "agent_id": "a1dca2c0feb7f44fb",
            "agent_type": "general-purpose",
            "tool_name": "Edit",
            "tool_input": { "file_path": "fixture.rs" },
        })
        .to_string();
        let out = run_hook(&db, &payload, &sandbox);
        assert!(out.trim().is_empty(), "PreToolUse never blocks for this debt, subagent or not: {out}");
    }

    let edits = serve::usefulness::project_eval_state(&db, Some("thor-fixture"))
        .sessions
        .get("sub1")
        .expect("a subagent's own PreToolUse must still record its own session's accrual")
        .edits_since_test;
    assert_eq!(
        edits, serve::usefulness::EVAL_REPEAT_MIN_UNTESTED_EDITS,
        "a subagent's own untested edits must still count toward the risk counter"
    );

    let out = run_hook(&db, &subagent_stop_payload("sub1", &project_dir), &sandbox);
    assert!(
        out.trim().is_empty(),
        "a subagent's Stop must never be held for the evaluation debt, however large its risk counter has grown: {out}"
    );
}

/// Case named in the build brief: a new evaluation-report Report for the
/// project silences it - the whole point of this debt: filing the report is
/// what tells THOR the evaluation happened. Uses the REAL production
/// detection path (a report declared in the store, read back through a real
/// Stop) rather than a hand-seeded sidecar.
#[test]
fn a_new_evaluation_report_for_the_project_silences_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    let owed_ids = declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    let first = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&first).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {first}"));
    assert_eq!(v["decision"], "block", "fixture sanity, before the report is filed: {first}");

    // File the evaluation report through the store directly - the same
    // shape `eval-command.example.md`'s own final step files with
    // `remember`: kind Report, this project, tagged `evaluation-report`.
    // The judgement debt fixture sanity just proved above must be settled
    // FIRST - see `pay_down_judgement_debt`'s own doc comment - a real
    // evaluation pays it down before it ever files the report too.
    let mut store = EventStore::open_existing(&db).unwrap();
    pay_down_judgement_debt(&mut store, &owed_ids);
    declare_report(&mut store, "eval-thor-fixture-2026-09-17", "thor-fixture");
    // A fresh session, so a stale minutes-worked timestamp is not what is
    // silencing the second call - it must clear the "served this project"
    // gate on its own too.
    seed_session_work(&db, Some("thor-fixture"), "s2", 90);
    drop(store);

    let second = run_hook(&db, &stop_payload("s2", &project_dir), &sandbox);
    assert!(second.trim().is_empty(), "a newly filed evaluation report must silence the obligation: {second}");
}

/// THE DEFECT THE FIRST 2026-09-16 REWRITE EXISTED FOR: a clock fed by ANY
/// verdict that applied to a checkout - global items included - reset
/// itself from unrelated activity elsewhere. `last_evaluation_seen`
/// structurally cannot regress the same way: `update_eval_debt_state` only
/// ever stamps it from a live Report tagged `evaluation-report`, never from
/// a plain `mark`. Marking a handful of totally unrelated global facts must
/// not silence this project's own obligation.
#[test]
fn regression_verdicts_on_unrelated_global_items_never_silence_this() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);

    // Verdicts on OTHER global items - none of these apply to the backlog
    // under test, and none carry the `evaluation-report` tag; they exist
    // only to prove a plain `mark` never masquerades as a filed report.
    for i in 0..6 {
        let id = format!("unrelated-global-{i}");
        let item = Item {
            id: id.clone(),
            kind: Kind::Rule,
            text: format!("an unrelated global fixture fact number {i}"),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("this stops being true".to_string()),
            check: None,
        };
        model::store::declare(&mut store, "fixture", "fixture", "fixture", &item).unwrap();
        serve::mark::record_useful(&mut store, "s", "s", "t", "2026-09-08T00:00:00Z", &id, None).unwrap();
    }
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("unrelated global verdicts must never silence this: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("Run the THOR evaluation"), "{reason}");
}

// --------------------------------------------- silences: no project at all
//
// A checkout that resolves to NO project at all can never file the Report
// that silences this debt (`model::gate`'s ground 21, `NO_SCOPE_PROBLEM`),
// so it must never be asked, and the sidecar update must never even run,
// regardless of how long the session has worked or what a pre-existing
// sidecar entry happens to carry.

/// Case named in the build brief: "no project silent" - proven even with a
/// pre-existing sidecar entry (seeded through the real, unchanged
/// `update_eval_debt_state` write) and 61 minutes of serving in that
/// session: stays silent, and leaves the sidecar file byte-for-byte exactly
/// as `seed_tracking_since` left it - not reset, not bumped, not touched at
/// all. This is also the shape a sidecar written before the third rewrite
/// (2026-09-16) could still carry on a real machine - proving that legacy
/// entry is inert, never acted on again.
#[test]
fn is_silent_for_a_checkout_with_no_project_even_with_a_pre_existing_sidecar_entry_and_enough_time_worked() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.global_cwd();

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, None);
    // Deliberately no `seed_session_work` call: a no-project checkout can
    // never accrue work that matters here either - `record_hook_event`
    // itself refuses to touch the sidecar at all for `project: None` (see
    // its own doc comment), so there is nothing honest a "session has
    // worked here" fixture could even represent for this case.
    seed_tracking_since(&store, &db, None, 25);
    drop(store);

    let sidecar = serve::usefulness::eval_debt_state_path(&db);
    let before = std::fs::read_to_string(&sidecar).expect("fixture sanity: seed_tracking_since must have written the sidecar");

    let out = run_hook(&db, &stop_payload("s1", &cwd), &sandbox);
    assert!(out.trim().is_empty(), "a checkout with no project must never be asked for the evaluation: {out}");

    let after = std::fs::read_to_string(&sidecar).unwrap();
    assert_eq!(before, after, "a no-project Stop must never touch the sidecar, not even a pre-existing entry already there");
}

/// A companion to the test above, proving the gate structurally rather than
/// only by outcome. The test above seeds the sidecar before the Stop runs,
/// so - even if the `stop_project.is_some()` half of the gate were missing
/// entirely - `usefulness::update_eval_debt_state` would still be a no-op
/// the second time (its own `get_or_insert` never overwrites an existing
/// value, and no new report exists to stamp), so a byte-for-byte comparison
/// alone cannot tell "the write was skipped" apart from "the write ran and
/// happened to change nothing". This test closes that gap: with NO sidecar
/// seeded at all - the very first Stop this checkout would ever see - a
/// version of the code missing the gate would create the file right here.
/// This is the one assertion in this file that actually goes red if the
/// gate in `bin/serve.rs`'s `hook_once` (`if !is_subagent && stop_project.
/// is_some()`) is weakened back to `if !is_subagent`.
#[test]
fn a_no_project_stop_never_creates_the_sidecar_file_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let cwd = sandbox.global_cwd();

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, None);
    // Deliberately no `seed_tracking_since` or `seed_session_work` call:
    // nothing has written the sidecar yet, the same as the very first Stop
    // this checkout would ever see - and a no-project checkout could never
    // honestly seed accrued work anyway (`record_hook_event` itself refuses
    // to touch the sidecar at all for `project: None`).
    drop(store);

    let sidecar = serve::usefulness::eval_debt_state_path(&db);
    assert!(!sidecar.exists(), "fixture sanity: nothing has written the sidecar yet");

    let out = run_hook(&db, &stop_payload("s1", &cwd), &sandbox);
    assert!(out.trim().is_empty(), "a checkout with no project must never be asked for the evaluation: {out}");
    assert!(
        !sidecar.exists(),
        "a no-project Stop must never create the sidecar file at all - the write is skipped entirely, not merely a no-op"
    );
}

/// THE CONTRAST both tests above need: a checkout that resolves to a real
/// project, 61 minutes worked, no report ever seen - and this one must
/// still fire, proving the silence above is really about the missing
/// project and nothing else accidentally different between the fixtures.
#[test]
fn the_identical_setup_with_a_real_project_still_fires() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 61);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("the identical setup, scoped to a real project, must still fire: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("Run the THOR evaluation"), "{reason}");
}

// --------------------------------------------------- regression: the others

/// THE DEFECT THIS GUARDS AGAINST: this project has measured twice what one
/// shared early return across several Stop-time gates does to the ones after
/// it (see `serve/src/bin/serve.rs`'s own `is_subagent` doc comment). Adding
/// a sixth gate between `judgement_debt` and the backlog burn must not
/// swallow an EARLIER debt - `setup_debt` runs first and must still win
/// outright, even on a store that ALSO satisfies the evaluation debt's own
/// condition.
#[test]
fn setup_debt_still_fires_with_an_eval_eligible_backlog_also_present() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");

    let mut store = EventStore::new(&db).unwrap();
    declare_setup_note(&mut store);
    declare_owed_items(&mut store, OWED_CONTEXT_COUNT, Some("thor-fixture"));
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);
    drop(store);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a decision JSON: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.starts_with("[THOR] This memory was just installed"), "setup_debt must still come first: {reason}");
    // Step 21: the evaluation ask rides after it in the same block, instead
    // of waiting for a Stop setup does not hold
    // (`a_setup_block_carries_the_evaluation_ask`).
    assert!(reason.contains("Run the THOR evaluation"), "the ask rides with the setup block: {reason}");
}

// ------------------------------------------ step 16: only a valid report counts
//
// The Stop hook validates every report on file once (`serve::eval_filing`),
// and the evaluation's facts in the sidecar come from that record in the
// log only - overwritten at every Stop.

/// R16.2 (EV Z10): setting `last_evaluation_seen` to "now" in
/// `eval-debt-state.json` - no report behind it - used to silence the
/// evaluation for 16 hours. Both Stops block now, and the Stop puts the
/// sidecar back to what the log says.
#[test]
fn an_edited_debt_sidecar_does_not_silence_the_ask() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");
    drop(EventStore::new(&db).unwrap());
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);

    let first = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(first.contains("\"decision\":\"block\""), "fixture sanity, the first Stop asks: {first}");

    let mut all = serve::usefulness::read_eval_debt_state(&db);
    let now = serve::time::now_unix();
    let entry = all.get_mut("thor-fixture").expect("the Stop wrote this project's entry");
    entry.last_evaluation_seen = Some(now);
    entry.last_evaluation_report_id = Some("eval-thor-fixture-2026-09-30".to_string());
    for session in entry.sessions.values_mut() {
        session.anchor_unix = Some(now + 1);
    }
    std::fs::write(serve::usefulness::eval_debt_state_path(&db), serde_json::to_string(&all).unwrap()).unwrap();

    let second = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    assert!(second.contains("\"decision\":\"block\""), "an edited sidecar never stands in for a valid report: {second}");
    let state = serve::usefulness::project_eval_state(&db, Some("thor-fixture"));
    assert_eq!((state.last_evaluation_seen, state.last_evaluation_report_id), (None, None), "put back from the log");
}

/// R16.1 / R16.3 (EV Z11, Z1 c): a report that reached the log past the
/// tool server's filing gate - here written raw, the shape an older `mcp.exe`
/// or `model.exe` leaves (since step 15 `model::store::declare` itself
/// refuses an evaluation report without a filing context, so the raw write
/// stands in for it) - does not count: the Stop still holds the turn, names
/// the report and says why.
#[test]
fn a_report_written_past_the_gate_does_not_count() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");
    let mut store = EventStore::new(&db).unwrap();
    let report = serve::eval_filing::fixture::report("eval-thor-fixture-2026-09-28", "thor-fixture", "TLDR: all fine.");
    let refused = model::store::declare(&mut store, "mcp", "mcp", "mcp", &report).unwrap_err().to_string();
    assert!(refused.contains("not coming through a THOR tool call"), "fixture sanity, the store's own gate refuses it: {refused}");
    let body = model::store::canonical_body(&report).unwrap();
    store.append_event("mcp", "mcp", "mcp", EventKind::FactCreated, &report.id, None, &body).unwrap();
    drop(store);
    seed_session_work(&db, Some("thor-fixture"), "s1", 90);

    let out = run_hook(&db, &stop_payload("s1", &project_dir), &sandbox);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("expected a block: {e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    let reason = v["reason"].as_str().unwrap();
    assert!(
        reason.starts_with(
            "[THOR] The report 'eval-thor-fixture-2026-09-28' is on file but does not count as this project's evaluation: not \
             filed through an attested THOR tool call"
        ),
        "{reason}"
    );
    assert!(reason.contains("The evaluation is still owed.") && reason.contains("Run the THOR evaluation"), "{reason}");
    let store = EventStore::open_existing(&db).unwrap();
    let record = serve::eval_filing::current_validation(&store, &report.id).expect("the Stop recorded its check");
    assert!(!record.valid && !record.drift, "{record:?}");
}

// -------------------------------------- step 17: the project's work clock
//
// B8, D11, D12, D13, F14, Q10. The clock is the PROJECT's (every session
// together, parallel minutes once); the ask follows the work, not the
// folder; it lands only on a session with ten minutes of its own work in
// the project, never at its first Stop there, and not while another session
// runs the evaluation - for at most an hour. A zeroed sidecar is overruled by
// the session's own transcript and bounded by the weekly floor, which reads
// the log only. Every session here is UUID-shaped: only such a session has
// a transcript, and only such a session is log activity.

fn uuid(n: u32) -> String {
    format!("5e1f0c3a-0000-4000-8000-0000000017{n:02}")
}

fn pretooluse_payload(session_id: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Read",
        "tool_input": { "file_path": "fixture.txt" },
    })
    .to_string()
}

/// Let `secs` seconds pass, as far as the work clock can tell: every instant
/// the sidecar holds moves that far into the past. The hook itself reads
/// the real clock, so this is how a test makes minutes go by between events.
fn age_sidecar(db: &Path, secs: i64) {
    fn age(v: &mut serde_json::Value, secs: i64) {
        match v {
            serde_json::Value::Object(map) => {
                for (key, value) in map.iter_mut() {
                    let instant = matches!(
                        key.as_str(),
                        "anchor_unix" | "last_event_unix" | "tracking_since" | "first_asked_since_report" | "last_evaluation_seen"
                    );
                    match value.as_i64() {
                        Some(t) if instant => *value = serde_json::json!(t - secs),
                        _ => age(value, secs),
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(|item| age(item, secs)),
            _ => {}
        }
    }
    let path = serve::usefulness::eval_debt_state_path(db);
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    age(&mut v, secs);
    std::fs::write(&path, v.to_string()).unwrap();
}

/// Seed `session`'s OWN work in `project` (`minutes`, and how many of its
/// Stops were seen there) without touching the project's clock.
fn seed_own_work(db: &Path, project: &str, session: &str, minutes: i64, stops: u32) {
    let mut all = serve::usefulness::read_eval_debt_state(db);
    let entry = all.entry(project.to_string()).or_default();
    let now = serve::time::now_unix();
    entry.sessions.insert(
        session.to_string(),
        serve::usefulness::SessionWorkState {
            anchor_unix: Some(now - minutes * 60),
            accrued_secs: minutes * 60,
            last_event_unix: Some(now),
            stops,
            ..Default::default()
        },
    );
    std::fs::write(serve::usefulness::eval_debt_state_path(db), serde_json::to_string(&all).unwrap()).unwrap();
}

/// Set the PROJECT's own clock to `minutes`.
fn seed_project_clock(db: &Path, project: &str, minutes: i64) {
    let mut all = serve::usefulness::read_eval_debt_state(db);
    let entry = all.entry(project.to_string()).or_default();
    let now = serve::time::now_unix();
    entry.work = serve::usefulness::SessionWorkState {
        anchor_unix: Some(now - minutes * 60),
        accrued_secs: minutes * 60,
        last_event_unix: Some(now),
        ..Default::default()
    };
    std::fs::write(serve::usefulness::eval_debt_state_path(db), serde_json::to_string(&all).unwrap()).unwrap();
}

fn is_block(out: &str) -> bool {
    out.contains("\"decision\":\"block\"")
}

fn transcripts(sandbox: &Sandbox) -> PathBuf {
    sandbox.home.path().join(".claude").join("projects")
}

/// A rule of `project`, decided (a note), served to `session` at
/// `days_ago` days before now: log activity of the project in that session.
fn served_in(store: &mut EventStore, project: &str, session: &str, days_ago: i64) {
    let id = format!("fx-active-{project}");
    if model::store::show(store, &id).is_err() {
        let item = Item {
            id: id.clone(),
            kind: Kind::Rule,
            text: format!("fixture rule that fires in {project}"),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-active-{project}") }],
            severity: None,
            project: Some(project.to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("the fixture rule turns out to be wrong".to_string()),
            check: None,
        };
        model::store::declare(store, "fixture", "fixture", "fixture", &item).unwrap();
        serve::teeth::fixture::decide(store, &id);
    }
    let at = serve::time::iso8601_from_unix(serve::time::now_unix() - days_ago * 86_400);
    serve::deliver::record_delivery(store, session, session, serve::usefulness::ASSISTANT_DELIVERY_ACTOR, &at, &[id]);
}

/// R17.1 (EV B8): S1 works 40 minutes in the project (a hook event every two
/// minutes), then S2 works 25. Neither session reaches an hour, the project
/// does: S1's Stops stay quiet (40 project minutes), S2's first Stop is free
/// (Q10), and S2's next Stop holds the turn. Driven through the real hook,
/// with the sidecar aged two minutes between events.
#[test]
fn two_short_sessions_add_up() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    drop(EventStore::new(&db).unwrap());
    let (s1, s2) = (uuid(1), uuid(2));
    for _ in 0..=20 {
        run_hook(&db, &pretooluse_payload(&s1, &fxp), &sandbox);
        age_sidecar(&db, 120);
    }
    assert!(!is_block(&run_hook(&db, &stop_payload(&s1, &fxp), &sandbox)), "S1 alone: 40 minutes");
    assert!(!is_block(&run_hook(&db, &stop_payload(&s1, &fxp), &sandbox)), "S1 alone, again");
    for _ in 0..=12 {
        run_hook(&db, &pretooluse_payload(&s2, &fxp), &sandbox);
        age_sidecar(&db, 120);
    }
    let state = serve::usefulness::project_eval_state(&db, Some("fxp"));
    assert!(state.work.accrued_secs >= 64 * 60, "the project's clock holds both: {:?}", state.work);
    assert!(state.sessions[&s2].accrued_secs < 30 * 60, "S2's own: {:?}", state.sessions[&s2]);
    let first = run_hook(&db, &stop_payload(&s2, &fxp), &sandbox);
    assert!(!is_block(&first), "S2's first Stop is never held: {first}");
    let second = run_hook(&db, &stop_payload(&s2, &fxp), &sandbox);
    assert!(is_block(&second) && second.contains("Run the THOR evaluation"), "{second}");
    assert!(second.contains("every session together"), "the ask names the project's clock: {second}");
}

/// G17.3 (F14): a session with four minutes of its own work in an overdue
/// project is never held, on any Stop; the next session, with twelve, is held
/// at its second Stop.
#[test]
fn a_session_without_ten_minutes_of_its_own_is_never_held() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    drop(EventStore::new(&db).unwrap());
    seed_project_clock(&db, "fxp", 90);
    let (short, next) = (uuid(3), uuid(4));
    seed_own_work(&db, "fxp", &short, 4, 0);
    for stop in 1..=3 {
        let out = run_hook(&db, &stop_payload(&short, &fxp), &sandbox);
        assert!(!is_block(&out), "stop {stop}: four minutes of its own is never enough: {out}");
    }
    seed_own_work(&db, "fxp", &next, 12, 0);
    assert!(!is_block(&run_hook(&db, &stop_payload(&next, &fxp), &sandbox)), "its first Stop is free");
    assert!(is_block(&run_hook(&db, &stop_payload(&next, &fxp), &sandbox)), "its second Stop is held");
}

/// G17.1 second half (Q10): a brand-new session's first Stop in a project
/// that is already due is never held; its second Stop is.
#[test]
fn first_stop_of_a_session_is_never_held() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    drop(EventStore::new(&db).unwrap());
    seed_project_clock(&db, "fxp", 90);
    let fresh = uuid(5);
    seed_own_work(&db, "fxp", &fresh, 30, 0);
    let first = run_hook(&db, &stop_payload(&fresh, &fxp), &sandbox);
    assert!(!is_block(&first), "{first}");
    let second = run_hook(&db, &stop_payload(&fresh, &fxp), &sandbox);
    assert!(is_block(&second), "{second}");

    // The Stop count is a sidecar copy: zeroed by hand, the transcript's
    // earlier turn still says this is not the first Stop. On a store of its
    // own: one project runs one evaluation at a time (step 24), and the
    // first session's is still running in the one above.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    drop(EventStore::new(&db).unwrap());
    seed_project_clock(&db, "fxp", 90);
    let dodger = uuid(6);
    seed_own_work(&db, "fxp", &dodger, 30, 0);
    let now = serve::time::now_unix();
    serve::attest::fixture::work_in(&transcripts(&sandbox), &dodger, &fxp, now - 40 * 60, 10);
    serve::attest::fixture::work_in(&transcripts(&sandbox), &dodger, &fxp, now - 20 * 60, 10);
    let out = run_hook(&db, &stop_payload(&dodger, &fxp), &sandbox);
    assert!(is_block(&out), "two turns in its transcript: a zeroed Stop count frees nothing: {out}");
}

/// R17.4 (D11): the ask follows the work, not the folder - S2 worked 25
/// minutes in fxp (the project is due), then its folder moved into gamma,
/// a project with no debt: its next Stop still asks for fxp, and says so. A
/// move into a folder of no project does not silence it either.
#[test]
fn the_ask_follows_the_work_not_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let gamma = sandbox.project_dir("gamma");
    drop(EventStore::new(&db).unwrap());
    let s2 = uuid(7);
    seed_project_clock(&db, "fxp", 70);
    seed_own_work(&db, "fxp", &s2, 25, 1);
    let out = run_hook(&db, &stop_payload(&s2, &gamma), &sandbox);
    assert!(is_block(&out), "{out}");
    assert!(out.contains("This session worked in project 'fxp' since its last evaluation; this folder belongs to 'gamma'"), "{out}");
    assert!(out.contains("(project 'fxp')") && out.contains("The evaluation of 'fxp' started"), "{out}");
    let elsewhere = run_hook(&db, &stop_payload(&s2, &sandbox.global_cwd()), &sandbox);
    assert!(is_block(&elsewhere) && elsewhere.contains("(project 'fxp')"), "{elsewhere}");
    assert!(elsewhere.contains("this folder belongs to no project"), "{elsewhere}");
}

/// G17.5 (D11): a session that worked only in its folder's own project is
/// asked about that project only, exactly as before - no line about another
/// project.
#[test]
fn a_session_in_its_own_project_is_asked_about_that_project_only() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    drop(EventStore::new(&db).unwrap());
    let s = uuid(8);
    seed_project_clock(&db, "fxp", 70);
    seed_own_work(&db, "fxp", &s, 70, 1);
    let out = run_hook(&db, &stop_payload(&s, &fxp), &sandbox);
    assert!(is_block(&out) && out.contains("(project 'fxp')"), "{out}");
    assert!(!out.contains("this folder belongs to"), "{out}");
}

/// G17.1 first half (D12): S1 holds an open evaluation start ten minutes old
/// and gave a verdict two minutes ago: S2's Stop is not held - one line says
/// where the evaluation runs.
#[test]
fn a_running_evaluation_elsewhere_is_not_asked_twice() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    let (s1, s2) = (uuid(9), uuid(10));
    {
        let mut store = EventStore::new(&db).unwrap();
        let now = serve::time::now_unix();
        serve::attest::fixture::quiet_start_at(&mut store, &s1, "fxp", now - 10 * 60);
        served_in(&mut store, "fxp", "fixture", 0);
        let at = serve::time::iso8601_from_unix(now - 2 * 60);
        serve::mark::record_useful(&mut store, &s1, "mcp", "mcp", &at, "fx-active-fxp", None).unwrap();
    }
    seed_project_clock(&db, "fxp", 90);
    seed_own_work(&db, "fxp", &s2, 30, 1);
    let out = run_hook(&db, &stop_payload(&s2, &fxp), &sandbox);
    assert!(!is_block(&out), "{out}");
    assert!(out.contains(&format!("[THOR] the evaluation of fxp is running in session {s1}")), "{out}");
}

/// R17.2 (EV B8, the step-16 leftover): the sidecar's minutes zeroed for S2
/// between Stops. S2's own transcript shows 25 minutes of work in the
/// project, so the ask still lands: with the project's clock intact the
/// transcript restores S2's own minutes; with the project's zeroed too, the
/// weekly floor (log activity, no valid evaluation) makes it due.
#[test]
fn zeroed_sidecar_is_overruled_by_the_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    let s2 = uuid(13);
    drop(EventStore::new(&db).unwrap());
    seed_project_clock(&db, "fxp", 70);
    seed_own_work(&db, "fxp", &s2, 0, 1);
    let control = run_hook(&db, &stop_payload(&s2, &fxp), &sandbox);
    assert!(!is_block(&control), "control: no transcript, zero minutes of its own: {control}");
    let now = serve::time::now_unix();
    serve::attest::fixture::work_in(&transcripts(&sandbox), &s2, &fxp, now - 26 * 60, 25);
    seed_own_work(&db, "fxp", &s2, 0, 1);
    let out = run_hook(&db, &stop_payload(&s2, &fxp), &sandbox);
    assert!(is_block(&out), "the transcript's 25 minutes overrule the zeroed sidecar: {out}");

    // The project's clock zeroed as well: S2 was served one of the
    // project's rules today (log activity) and the project has no valid
    // evaluation, so the weekly floor holds.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    {
        let mut store = EventStore::new(&db).unwrap();
        served_in(&mut store, "fxp", &s2, 0);
    }
    seed_project_clock(&db, "fxp", 0);
    seed_own_work(&db, "fxp", &s2, 0, 1);
    let out = run_hook(&db, &stop_payload(&s2, &fxp), &sandbox);
    assert!(is_block(&out) && out.contains("(the weekly floor)"), "{out}");
}

/// R17.6 (D13): the project's minutes zeroed; S2 has 12 minutes of its own
/// (under the hour, so the clock alone would not ask); the log shows fxp
/// activity on three of the last seven days and the newest valid evaluation
/// is eight days old: S2's second Stop is held. G17.4: with no activity in
/// the last seven days, or a valid evaluation three days old, the floor
/// makes nothing due.
#[test]
fn zeroed_project_clock_meets_the_weekly_floor() {
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    let s2 = uuid(14);
    let now = serve::time::now_unix();
    let case = |eval_days: Option<i64>, activity_days: &[i64]| -> (String, String) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        drop(EventStore::new(&db).unwrap());
        if let Some(days) = eval_days {
            seed_valid_report(&db, "fxp", now - days * 86_400, "eval-fxp-fixture");
        }
        {
            let mut store = EventStore::open_existing(&db).unwrap();
            for (i, days) in activity_days.iter().enumerate() {
                served_in(&mut store, "fxp", &uuid(40 + i as u32), *days);
            }
        }
        seed_project_clock(&db, "fxp", 0);
        seed_own_work(&db, "fxp", &s2, 12, 0);
        let first = run_hook(&db, &stop_payload(&s2, &fxp), &sandbox);
        let second = run_hook(&db, &stop_payload(&s2, &fxp), &sandbox);
        (first, second)
    };
    let (first, second) = case(Some(8), &[1, 3, 5]);
    assert!(!is_block(&first), "the first Stop is free: {first}");
    assert!(is_block(&second) && second.contains("(the weekly floor)"), "{second}");
    let (_, quiet) = case(None, &[9]);
    assert!(!is_block(&quiet), "no activity in the last seven days: {quiet}");
    let (_, quiet) = case(Some(3), &[1, 2]);
    assert!(!is_block(&quiet), "a valid evaluation three days old: {quiet}");
}

// ---------------------------------------------------------------- step 20
//
// NEW-1 (C3): the Response Guard's block returned before the Stop was counted
// and before the evaluation was asked, and its retry cannot hold the turn - so
// a session whose replies were always long or had no summary was never asked
// and its Stops were never counted. The ask now rides with the guard's block,
// and every first-pass Stop is counted, whoever holds it.

/// A rulebook beside the store whose one rule blocks a long reply that does
/// not open with a TLDR.
fn seed_tldr_rulebook(db: &Path) {
    let rulebook = r#"[
  {"id":"fx-tldr-first",
   "any_of":["fixture-detail"],
   "none_of":["tldr"],
   "min_chars":400,
   "reminder":"open with a TLDR in plain language"}
]"#;
    std::fs::write(db.parent().unwrap().join("guard-response-rulebook.json"), rulebook).unwrap();
}

/// A first-pass (or, with `retry`, a retried) Stop whose reply the rulebook
/// above blocks: long, and no TLDR.
fn long_reply_stop(session_id: &str, cwd: &Path, retry: bool) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "stop_hook_active": retry,
        "last_assistant_message": format!("done. {}", "fixture-detail ".repeat(60)),
        "cwd": cwd.to_string_lossy(),
    })
    .to_string()
}

/// NEW-1, the ask: a session that is due (not its first Stop, ten minutes of
/// its own) answers long without a TLDR. The Stop holds the turn with the
/// guard's reason AND the evaluation ask with its start note, in one block;
/// the retry of that turn holds nothing.
#[test]
fn a_response_guard_block_carries_the_evaluation_ask() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    drop(EventStore::new(&db).unwrap());
    seed_tldr_rulebook(&db);
    let s = uuid(60);
    seed_project_clock(&db, "fxp", 90);
    seed_own_work(&db, "fxp", &s, 30, 1);
    let out = run_hook(&db, &long_reply_stop(&s, &fxp, false), &sandbox);
    assert!(is_block(&out), "{out}");
    assert!(out.contains("TLDR"), "the guard is never skipped: {out}");
    assert!(out.contains("Run the THOR evaluation") && out.contains("The evaluation of 'fxp' started"), "the ask rides with it: {out}");
    let retry = run_hook(&db, &long_reply_stop(&s, &fxp, true), &sandbox);
    assert!(!is_block(&retry), "the retry never holds the turn again: {retry}");
    assert_eq!(serve::usefulness::project_eval_state(&db, Some("fxp")).asked_count, 1, "asked once, counted once");
}

/// NEW-1, the count: a fresh session's first Stop is held by the guard (and
/// is free of the ask, Q10) - it is still counted, so the session's next
/// Stop is its second and is asked. Before step 20 the guard's block left the
/// count at zero and the next Stop was "the first" again.
#[test]
fn a_stop_held_by_the_response_guard_still_counts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    drop(EventStore::new(&db).unwrap());
    seed_tldr_rulebook(&db);
    let s = uuid(61);
    seed_project_clock(&db, "fxp", 90);
    seed_own_work(&db, "fxp", &s, 30, 0);
    let first = run_hook(&db, &long_reply_stop(&s, &fxp, false), &sandbox);
    assert!(is_block(&first) && first.contains("TLDR"), "{first}");
    assert!(!first.contains("Run the THOR evaluation"), "the first Stop is free of the ask: {first}");
    assert_eq!(serve::usefulness::project_eval_state(&db, Some("fxp")).sessions[&s].stops, 1, "the held Stop is counted");
    let second = run_hook(&db, &long_reply_stop(&s, &fxp, false), &sandbox);
    assert!(is_block(&second) && second.contains("TLDR") && second.contains("Run the THOR evaluation"), "{second}");
}

// ---------------------------------------------------------------- step 21
//
// C3 B2 L86: a short task session was held at its very first Stop by the
// teeth question (rules with no current verdict from THOR's trial), work it
// never touched. That hold now has the evaluation ask's own gates (F14,
// Q10): never a session's first Stop in the project, and only once the
// session has ten minutes of its own work there. And the setup, capture and
// crowding blocks no longer starve the evaluation ask: it rides with them,
// the way it rides with the Response Guard's block (step 20).

/// A main session's `SessionStart` in `cwd`: it records where the log stood,
/// the mark the crowding question reads what this session wrote by.
fn session_start_payload(session_id: &str, cwd: &Path) -> String {
    serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": session_id,
        "source": "startup",
        "cwd": cwd.to_string_lossy(),
    })
    .to_string()
}

/// The setup block holds the turn first, and the evaluation ask - with the
/// start note of the evaluation it starts - rides in the same block. The
/// Stop is counted once.
#[test]
fn a_setup_block_carries_the_evaluation_ask() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    let mut store = EventStore::new(&db).unwrap();
    declare_setup_note(&mut store);
    drop(store);
    let s = uuid(66);
    seed_project_clock(&db, "fxp", 90);
    seed_own_work(&db, "fxp", &s, 30, 1);

    let out = run_hook(&db, &stop_payload(&s, &fxp), &sandbox);
    assert!(is_block(&out), "{out}");
    let setup = out.find("First session with a new owner").unwrap_or_else(|| panic!("setup is never skipped: {out}"));
    let ask = out.find("Run the THOR evaluation").unwrap_or_else(|| panic!("the ask rides with it: {out}"));
    assert!(setup < ask && out.contains("The evaluation of 'fxp' started"), "setup first, then the ask and its start note: {out}");
    let state = serve::usefulness::project_eval_state(&db, Some("fxp"));
    assert_eq!((state.asked_count, state.sessions[&s].stops), (1, 2), "asked once, the Stop counted once");
    let retry = run_hook(&db, &retry_stop_payload(&s, &fxp), &sandbox);
    assert!(!is_block(&retry), "the retry never holds the turn again: {retry}");
}

/// The capture check (a durable decision the owner stated, not recorded)
/// holds the turn, and the evaluation ask rides in the same block.
#[test]
fn a_capture_block_carries_the_evaluation_ask() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    drop(EventStore::new(&db).unwrap());
    let s = uuid(67);
    let prompt = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": s,
        "cwd": fxp.to_string_lossy(),
        "prompt": "from now on always run the linter first",
    })
    .to_string();
    run_hook(&db, &prompt, &sandbox);
    seed_project_clock(&db, "fxp", 90);
    seed_own_work(&db, "fxp", &s, 30, 1);

    let out = run_hook(&db, &stop_payload(&s, &fxp), &sandbox);
    assert!(is_block(&out), "{out}");
    let capture = out.find("NEW durable decision").unwrap_or_else(|| panic!("the capture check is never skipped: {out}"));
    let ask = out.find("Run the THOR evaluation").unwrap_or_else(|| panic!("the ask rides with it: {out}"));
    assert!(capture < ask && out.contains("The evaluation of 'fxp' started"), "{out}");
    let state = serve::usefulness::project_eval_state(&db, Some("fxp"));
    assert_eq!((state.asked_count, state.sessions[&s].stops), (1, 2), "asked once, the Stop counted once");
}

/// The crowding question (a fact this session stored onto a full place)
/// holds the turn, and the evaluation ask rides in the same block.
#[test]
fn a_crowding_block_carries_the_evaluation_ask() {
    const DISTINCT: [&str; 5] = [
        "a webhook retry backs off before it gives up entirely",
        "the estimator rounds a quote up to whole cents",
        "a spool label carries the batch it came from",
        "the scheduler skips a printer that is on hold",
        "an invoice number never restarts inside a year",
    ];
    let moment_rule = |id: &str, text: &str| Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings: vec![Binding::Moment(intent::Action::Deploy)],
        severity: None,
        project: Some("fxp".to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(format!("{id} turns out not to matter")),
        check: None,
    };
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    let mut store = EventStore::new(&db).unwrap();
    for i in 0..model::item::MAX_ITEMS {
        let id = format!("holder-{i}");
        model::store::declare(&mut store, "earlier", "earlier", "t", &moment_rule(&id, DISTINCT[i % DISTINCT.len()])).unwrap();
        serve::teeth::fixture::decide(&mut store, &id);
    }
    drop(store);
    let s = uuid(68);
    run_hook(&db, &session_start_payload(&s, &fxp), &sandbox);
    let mut store = EventStore::open_existing(&db).unwrap();
    let newcomer = moment_rule("newcomer", "a shipment label is printed once and never reprinted silently");
    model::store::declare(&mut store, "mcp", "mcp", "t", &newcomer).unwrap();
    serve::teeth::fixture::decide(&mut store, "newcomer");
    drop(store);
    seed_project_clock(&db, "fxp", 90);
    seed_own_work(&db, "fxp", &s, 30, 1);

    let out = run_hook(&db, &stop_payload(&s, &fxp), &sandbox);
    assert!(is_block(&out), "{out}");
    let crowding = out.find("onto a place that is already full").unwrap_or_else(|| panic!("the crowding question is never skipped: {out}"));
    let ask = out.find("Run the THOR evaluation").unwrap_or_else(|| panic!("the ask rides with it: {out}"));
    assert!(crowding < ask && out.contains("The evaluation of 'fxp' started"), "{out}");
    let state = serve::usefulness::project_eval_state(&db, Some("fxp"));
    assert_eq!((state.asked_count, state.sessions[&s].stops), (1, 2), "asked once, the Stop counted once");
}

// ---------------------------------------------------------------------------
// STEP 25: a notice that does not hold the turn must never wake the model.
// Field case 2026-10-01 (session 615bdf12): the void notice said "this Stop
// does not hold the turn again" and was delivered with exit 0, yet Claude
// Code ended every Stop by answering it again, 9 times, until it overrode.
// Cause: the notice went out with `hookSpecificOutput.additionalContext`,
// which at Stop "continues the conversation" - a hold in everything but name.
// ---------------------------------------------------------------------------

/// A Stop answer that carries text for the model: it makes Claude Code start
/// another model turn, so it is a hold whatever its exit code says.
fn wakes_the_model(out: &str) -> bool {
    out.contains("additionalContext") || out.contains("\"decision\"")
}

/// The notice reached the owner as a plain system message.
fn system_message(out: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(out).unwrap_or_else(|e| panic!("not JSON: {out:?}: {e}"));
    v["systemMessage"].as_str().unwrap_or_else(|| panic!("no systemMessage: {out}")).to_string()
}

/// `s` has an evaluation of `fxp` that was started by the first Stop and then
/// closed as void by a side door, with the session doing nothing since.
fn voided_start_without_new_work(db: &Path, sandbox: &Sandbox, fxp: &Path, s: &str) -> String {
    drop(EventStore::new(db).unwrap());
    seed_session_work(db, Some("fxp"), s, 90);
    let first = run_hook(db, &stop_payload(s, fxp), sandbox);
    assert!(is_block(&first), "{first}");
    let mut store = EventStore::new(db).unwrap();
    let open = model::store::open_evaluation_start(&store, s, "fxp").expect("the ask started one");
    served_in(&mut store, "fxp", "fixture", 0);
    let outside = model::store::FrozenVoid { id: "fx-active-fxp".to_string(), seq: 1, door: "an attested THOR call of another session".to_string() };
    serve::eval_session::void(&mut store, &open, &outside).expect("void");
    open.id
}

/// THE FIELD CASE: a voided start, the session did nothing new, the same
/// Stop arrives again and again. The notice is shown once, as a system
/// message, never as something the model has to answer; the Stops after it
/// say nothing. And the next turn's first Stop still starts the fresh
/// evaluation the notice promised.
#[test]
fn a_void_notice_is_shown_once_and_never_wakes_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    let s = uuid(70);
    let voided = voided_start_without_new_work(&db, &sandbox, &fxp, &s);

    let first = run_hook(&db, &retry_stop_payload(&s, &fxp), &sandbox);
    assert!(!wakes_the_model(&first), "a notice that does not hold the turn must not wake the model: {first}");
    assert!(system_message(&first).contains(&format!("('{voided}') was closed as void")), "{first}");
    for round in 0..4 {
        let again = run_hook(&db, &retry_stop_payload(&s, &fxp), &sandbox);
        assert!(!wakes_the_model(&again), "round {round}: {again}");
        assert!(!again.contains("closed as void"), "round {round}: the notice is shown once, not on every Stop: {again}");
    }

    // The next turn's first Stop (no stop_hook_active) starts the fresh one.
    let next_turn = run_hook(&db, &stop_payload(&s, &fxp), &sandbox);
    assert!(is_block(&next_turn) && next_turn.contains("[THOR] The evaluation of 'fxp' started"), "{next_turn}");
    let store = EventStore::new(&db).unwrap();
    let fresh = model::store::open_evaluation_start(&store, &s, "fxp").expect("a fresh evaluation is open");
    assert_ne!(fresh.id, voided);
}

/// The same, while another session's evaluation of the project is active:
/// one line saying who runs it, as a system message, once - never a hold.
#[test]
fn a_running_elsewhere_notice_after_a_void_is_one_line_and_never_wakes_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    let (s, other) = (uuid(71), uuid(72));
    let voided = voided_start_without_new_work(&db, &sandbox, &fxp, &s);
    {
        // The other session's evaluation is open and it was active a moment
        // ago; this session then does something, so its Stop looks again.
        let mut store = EventStore::new(&db).unwrap();
        serve::attest::fixture::quiet_start_at(&mut store, &other, "fxp", serve::time::now_unix() - 40 * 60);
        let at = serve::time::iso8601_from_unix(serve::time::now_unix());
        serve::mark::record_useful(&mut store, &other, "mcp", "mcp", &at, "fx-active-fxp", None).unwrap();
        served_in(&mut store, "fxp", &s, 0);
    }
    let first = run_hook(&db, &retry_stop_payload(&s, &fxp), &sandbox);
    assert!(!wakes_the_model(&first), "{first}");
    let line = system_message(&first);
    assert!(line.contains(&format!("('{voided}') was closed as void")) && line.contains(&format!("running in session {other}")), "{line}");
    for round in 0..3 {
        let again = run_hook(&db, &retry_stop_payload(&s, &fxp), &sandbox);
        assert!(!wakes_the_model(&again) && !again.contains("closed as void"), "round {round}: {again}");
    }
}

/// An evaluation still open and unfiled, nothing done since the last hold:
/// the line says so, and says it as a system message only.
#[test]
fn the_report_still_owed_line_never_wakes_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    let s = uuid(73);
    drop(EventStore::new(&db).unwrap());
    seed_session_work(&db, Some("fxp"), &s, 90);
    let first = run_hook(&db, &stop_payload(&s, &fxp), &sandbox);
    assert!(is_block(&first), "{first}");
    let retry = run_hook(&db, &retry_stop_payload(&s, &fxp), &sandbox);
    assert!(!wakes_the_model(&retry), "{retry}");
    assert!(system_message(&retry).contains("is still open and its report is not filed"), "{retry}");
}

/// Claude Code overrides a Stop hook after 9 blocks in a row. A session that
/// does something between every Stop is held again at most `REHOLD_MAX`
/// times: the first hold and the re-holds together stay well under 9.
#[test]
fn a_held_turn_is_held_again_at_most_four_times() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let fxp = sandbox.project_dir("fxp");
    let s = uuid(74);
    drop(EventStore::new(&db).unwrap());
    seed_session_work(&db, Some("fxp"), &s, 90);
    let first = run_hook(&db, &stop_payload(&s, &fxp), &sandbox);
    assert!(is_block(&first), "{first}");
    let mut blocks = 1;
    for _ in 0..9 {
        {
            let mut store = EventStore::new(&db).unwrap();
            served_in(&mut store, "fxp", &s, 0);
        }
        let out = run_hook(&db, &retry_stop_payload(&s, &fxp), &sandbox);
        if is_block(&out) {
            blocks += 1;
        } else {
            assert!(!wakes_the_model(&out), "{out}");
        }
    }
    assert_eq!(blocks, 5, "the first hold plus four re-holds, never more");
}

/// FIELD TEST 13, D2: after a filing of THIS session was refused as version
/// skew, the Stop must not start a fresh evaluation and hold the turn for it
/// (the second report would be dead the same way, and the hold left no other
/// way out). It says one plain line instead, until the session's transcript
/// shows it started again - then it is asked normally.
#[test]
fn after_a_skew_refusal_the_stop_says_restart_and_does_not_hold_until_the_session_restarted() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");
    let session = uuid(7);
    let mut store = EventStore::new(&db).unwrap();
    let report = serve::eval_filing::fixture::report("eval-thor-fixture-2026-10-08", "thor-fixture", "TLDR: all fine.");
    let body = model::store::canonical_body(&report).unwrap();
    let filing = store.append_event(&session, &session, "mcp", EventKind::FactCreated, &report.id, None, &body).unwrap();
    let filed_at = serve::time::iso8601_from_unix(serve::time::now_unix() - 600);
    let skew = model::store::EvaluationValidation {
        valid: false,
        why: format!("{}: its machine section was rendered by format 5, this checker reads format 7 - restart Claude", model::store::SKEW_WHY),
        drift: false,
        report: report.id.clone(),
        report_seq: filing.seq,
        project: Some("thor-fixture".to_string()),
        session: session.clone(),
        agent: None,
        started_seq: None,
        filed_seq: None,
        filed_at: Some(filed_at.clone()),
        machine_sha256: None,
        verdicts: model::store::VerdictShares::default(),
        carry: Vec::new(),
    };
    let record = serde_json::to_string(&skew).unwrap();
    store.append_event(&session, &session, model::store::EVAL_VALIDATOR_ACTOR, EventKind::GateStoodAside, &report.id, None, &record).unwrap();
    drop(store);
    seed_session_work(&db, Some("thor-fixture"), &session, 90);
    let transcript = transcripts(&sandbox).join("C--fixture").join(format!("{session}.jsonl"));
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let line = |name: &str, at: String| {
        serde_json::json!({
            "type": "attachment",
            "attachment": {"type": "hook_success", "hookName": name, "hookEvent": "SessionStart", "exitCode": 0},
            "timestamp": at,
            "sessionId": session,
        })
        .to_string()
    };
    std::fs::write(&transcript, line("SessionStart:startup", serve::time::iso8601_from_unix(serve::time::now_unix() - 7200)) + "\n").unwrap();

    let out = run_hook(&db, &stop_payload(&session, &project_dir), &sandbox);
    assert!(!is_block(&out), "no hold while the tool server is the old build: {out}");
    assert!(out.contains("Restart Claude; the evaluation is asked again after the restart"), "{out}");
    let store = EventStore::open_existing(&db).unwrap();
    assert!(model::store::evaluation_starts_of_session(&store, &session).is_empty(), "no fresh evaluation was started");
    drop(store);

    // The session starts again (Claude restarted, same session id): asked normally.
    let mut file = std::fs::OpenOptions::new().append(true).open(&transcript).unwrap();
    writeln!(file, "{}", line("SessionStart:resume", serve::time::iso8601_from_unix(serve::time::now_unix() - 60))).unwrap();
    drop(file);
    let out = run_hook(&db, &stop_payload(&session, &project_dir), &sandbox);
    assert!(is_block(&out), "after the restart the ask holds the turn as usual: {out}");
    assert!(out.contains("the report 'eval-thor-fixture-2026-10-08'") || out.contains("eval-thor-fixture-2026-10-08"), "{out}");
}

/// FIELD TEST 14, H1: a session whose last start is older than the installed
/// tool server (`mcp` beside the checker) is told to restart, in one line,
/// before any dead report exists - and is not asked nor held. A session that
/// started after the install is asked as usual. The checker is a copy of the
/// real binary in a folder of its own, beside a fake `mcp` of a chosen age.
#[test]
fn a_session_older_than_the_installed_tool_server_is_told_to_restart_and_a_fresh_one_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("thor-fixture");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let serve_copy = bin.join(format!("serve{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(env!("CARGO_BIN_EXE_serve"), &serve_copy).unwrap();
    let mcp = bin.join(format!("mcp{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&mcp, b"fake tool server").unwrap();
    let now = serve::time::now_unix();
    let installed = now - 3600;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&mcp)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(installed as u64))
        .unwrap();
    let run = |payload: &str| {
        let mut cmd = Command::new(&serve_copy);
        cmd.arg("--db").arg(&db).arg("hook").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        sandbox.apply(&mut cmd);
        let mut child = cmd.spawn().expect("spawn the serve copy");
        let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
        let out = child.wait_with_output().expect("wait serve");
        String::from_utf8(out.stdout).unwrap()
    };
    drop(EventStore::new(&db).unwrap());
    let start_line = |session: &str, at: i64| {
        serde_json::json!({
            "type": "attachment",
            "attachment": {"type": "hook_success", "hookName": "SessionStart:startup", "hookEvent": "SessionStart", "exitCode": 0},
            "timestamp": serve::time::iso8601_from_unix(at),
            "sessionId": session,
        })
        .to_string()
    };
    let write_transcript = |session: &str, at: i64| {
        let path = transcripts(&sandbox).join("C--fixture").join(format!("{session}.jsonl"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, start_line(session, at) + "\n").unwrap();
    };

    // Started two hours ago, the tool server was replaced an hour ago.
    let old = uuid(81);
    write_transcript(&old, now - 7200);
    seed_session_work(&db, Some("thor-fixture"), &old, 90);
    let out = run(&stop_payload(&old, &project_dir));
    assert!(!is_block(&out), "no ask and no hold for a stale tool server: {out}");
    assert!(out.contains("Restart Claude; the evaluation is asked again after the restart"), "{out}");
    let store = EventStore::open_existing(&db).unwrap();
    assert!(model::store::evaluation_starts_of_session(&store, &old).is_empty(), "no evaluation was started");
    drop(store);

    // Started after the install: asked and held as usual.
    let fresh = uuid(82);
    write_transcript(&fresh, now - 600);
    seed_session_work(&db, Some("thor-fixture"), &fresh, 90);
    let out = run(&stop_payload(&fresh, &project_dir));
    assert!(is_block(&out), "a session started after the install is held normally: {out}");

    // The tool server is replaced while that evaluation is open: the retry of
    // the held turn is no longer held, it is told to restart.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&mcp)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(now as u64))
        .unwrap();
    {
        let mut store = EventStore::new(&db).unwrap();
        served_in(&mut store, "thor-fixture", &fresh, 0);
    }
    let out = run(&retry_stop_payload(&fresh, &project_dir));
    assert!(!is_block(&out), "an open evaluation is not held for once the tool server is older: {out}");
    assert!(out.contains("Restart Claude; the evaluation is asked again after the restart"), "{out}");
}
