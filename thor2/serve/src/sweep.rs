//! THE STANDING SWEEP: every rule that can refuse a call, and every entry of
//! the answer rulebook, tried against the real calls and replies of the last
//! 30 days (`corpus`), through the very functions the hook uses
//! (`guard_verdict::pre_tool_in`, `respond::guard_verdict_in_turn`).
//!
//! WHY. The owner's standard (2026-10-09): THOR must work whatever kind of
//! work is done, without him hunting for defects. Two one-off sweeps that day
//! replayed 146,000 real calls and 6,600 real replies and found twelve rules
//! with false blocks and 21 false answer-guard triggers that no review had
//! shown. This module makes that sweep a check THOR runs itself: at every
//! evaluation start, in `doctor`, at write time and as `serve sweep`.
//!
//! WHAT IT REPORTS, per rule: it would refuse K of N real calls; R of those
//! ran fine. "Ran fine" is evidence, never a verdict - two heuristics, both
//! read from the corpus:
//! - LET THROUGH: THOR's hook did not refuse the call, it ran and succeeded,
//!   it was made after the rule last changed (plus `GRACE_SECS`), and in a
//!   session in which a hook was alive. A rule that would refuse it NOW, though
//!   it stood when the call ran, is wrong about it or about the hook.
//! - RAN UNCHANGED: THOR's hook refused the call under this rule, and the very
//!   same call (tool, folder, input) was later made again, was not refused, and
//!   ran. Something got past the rule: it was weakened, approved or walked
//!   around.
//! A rule bound to the owner's go (`owner-approval`) is never flagged by
//! either: it is meant to let a call through once he said so.
//! The rest of the K are calls the rule really refused (or would refuse) and
//! nobody has said were fine; they are in `serve sweep`, not in the nag.
//!
//! WHAT IT KEEPS (`sweep-state.json`, beside the store): per distinct call
//! that some rule would refuse, the rules and its latest occurrences; per rule
//! the fingerprint it was last swept as; how far into the corpus it got.
//! INCREMENTAL: a run replays only the corpus rows written since the last run
//! (against the whole store, exactly as the hook sees it); a rule that is new
//! or whose text, check, bindings, scope or severity changed is replayed over
//! the rows it can reach (`needles`, a necessary condition read off the raw
//! line) and the calls it had refused. A new build of THOR (`BUILD_ID`) or
//! seven days since the last full run starts over, because the engine or a
//! project's registration may have changed what a rule refuses.
//! TIME-BOXED: a run stops at its budget and the state says where it stopped;
//! the next run - an evaluation, doctor, `serve sweep` - carries on from there.
//!
//! KNOWN LIMIT: the command arm of the hook names the FIRST rule that refuses
//! a call, so a rule hidden behind another on the same call shows up when the
//! first is fixed (a `requires` arm names them all).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use thor_core::event_store::{EventKind, EventStore};

use crate::corpus::{self, CallRow, ReplyRow};
use crate::guard_verdict::{self, Live};
use model::item::{Binding, Check, Item, TargetKind};

pub const STATE_FILE: &str = "sweep-state.json";
const STATE_VERSION: u32 = 1;
/// A full run again after this long, whatever else did not change.
const FULL_EVERY_SECS: i64 = 7 * 86_400;
/// A call counts as made "after the rule stood" this long after the rule last
/// changed - the change and the calls around it are not evidence.
pub const GRACE_SECS: i64 = 3_600;
/// The latest occurrences of one call that are kept.
const OCC_KEPT: usize = 12;
/// The examples a rule's line shows.
pub const EXAMPLES_SHOWN: usize = 3;
/// A call as an example, cut.
const CALL_EXCERPT: usize = 140;
/// A reply as an example, cut.
const REPLY_EXCERPT: usize = 160;
/// The most calls the write gate's note tries a rule on (the newest ones).
const NOTE_CANDIDATES_MAX: usize = 25_000;
/// The most rules the doctor line and the start note name.
pub const NAMED_MAX: usize = 6;

pub fn state_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(STATE_FILE)
}

/// FNV-1a, 64 bit, as 16 hex digits: stable across builds and platforms.
pub fn fnv(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

// ----------------------------------------------------------------- the rules

/// Whether `item` is a rule whose check can refuse a call: the sweep tries
/// exactly these. A superset of what `doctor` counts as a rule with teeth - it
/// also tries a `contains` check, which refuses a write that removes the
/// line it requires.
pub fn can_refuse(item: &Item) -> bool {
    item.kind.can_fire()
        && matches!(
            item.check,
            Some(Check::Absent { .. })
                | Some(Check::AbsentAll { .. })
                | Some(Check::Forbidden { .. })
                | Some(Check::Requires { .. })
                | Some(Check::Contains { .. })
        )
}

/// What a rule is, for "did it change since it was swept": its kind, text,
/// bindings, check, scope, severity and tags.
pub fn fingerprint(item: &Item) -> String {
    let parts = serde_json::json!([item.kind, item.text, item.bindings, item.check, item.project, item.severity, item.tags]);
    fnv(&parts.to_string())
}

fn is_owner_approval(item: &Item) -> bool {
    item.tags.iter().any(|t| t == model::store::OWNER_APPROVAL_TAG)
}

/// A necessary condition for a rule to refuse a call: some alternative of the
/// result has every one of its words in the call's lowercased text (the tool
/// name and the input). `None`: the rule can reach any call, so no row is left
/// out. Only a rule bound to commands alone is narrowed: a call that never
/// names the command's words is not reached by it.
pub fn needles(item: &Item) -> Option<Vec<Vec<String>>> {
    let mut alternatives: Vec<Vec<String>> = Vec::new();
    for binding in &item.bindings {
        let Binding::Target { kind: TargetKind::Command, value } = binding else { return None };
        let words: Vec<String> = value
            .split_whitespace()
            .filter_map(|w| {
                let token: String = w
                    .to_lowercase()
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'))
                    .max_by_key(|piece| piece.len())
                    .unwrap_or_default()
                    .trim_matches(['.', '-'])
                    .to_string();
                (!token.is_empty()).then_some(token)
            })
            .collect();
        if words.is_empty() {
            return None;
        }
        alternatives.push(words);
    }
    (!alternatives.is_empty()).then_some(alternatives)
}

fn reached_by_needles(needles: &[Vec<String>], haystack_lower: &str) -> bool {
    needles.iter().any(|words| words.iter().all(|w| haystack_lower.contains(w.as_str())))
}

// --------------------------------------------------------------------- state

/// One time a distinct call was made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Occ {
    /// The last 8 characters of the tool call's id.
    pub id: String,
    pub t: i64,
    pub s: String,
    /// The rules THOR's hook refused it under at the time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub r: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// The build that recorded it (`CallRow::b`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub b: String,
}

/// A distinct call (tool, folder, input) that some rule would refuse now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub tool: String,
    /// The call as an example shows it.
    pub call: String,
    /// Every rule that would refuse it.
    pub rules: Vec<String>,
    /// How many times it was made.
    pub n: u32,
    /// The latest occurrences, oldest first.
    pub occ: Vec<Occ>,
    /// The pass over the corpus (`CallState::scan`) that judged it.
    #[serde(default)]
    pub g: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    /// Rules to replay over the rows before `done_id`, with the fingerprint
    /// each had when this was queued.
    pub rules: BTreeMap<String, String>,
    /// The id of the last row the replay got through.
    pub after: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CallState {
    /// The id of the last corpus call the main pass replayed.
    pub done_id: Option<String>,
    /// How many calls the corpus holds.
    pub rows: u64,
    /// Calls with a cut input, which are not replayed.
    pub cut_rows: u64,
    /// The fingerprint each refusing rule was last swept as.
    pub fps: BTreeMap<String, String>,
    pub pending: Option<Pending>,
    /// Which pass over the whole corpus this is; a hit judged in an earlier one
    /// is judged again when its call comes up (`Hit::g`).
    #[serde(default)]
    pub scan: u32,
    /// Sessions in which a hook was alive (it refused or attached something).
    pub active: BTreeSet<String>,
    pub hits: BTreeMap<String, Hit>,
}

/// A reply that some answer-rulebook entry fires on now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplyHit {
    pub t: i64,
    pub s: String,
    pub excerpt: String,
    /// Entry id -> "block" or "warn".
    pub fired: BTreeMap<String, String>,
    /// The entries whose reminder the guard said at the time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub said_then: Vec<String>,
    /// The guard neither held nor warned when this build delivered the reply.
    #[serde(default)]
    pub delivered_clean: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplyState {
    pub rows: u64,
    pub cut_rows: u64,
    /// The fingerprint each entry was last swept as, and since when.
    pub fps: BTreeMap<String, String>,
    pub since: BTreeMap<String, i64>,
    /// The replies already tried, by key.
    pub seen: BTreeSet<String>,
    pub hits: BTreeMap<String, ReplyHit>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub v: u32,
    pub engine: String,
    pub full_at: i64,
    pub last_run: i64,
    pub pruned_at: i64,
    pub calls: CallState,
    pub replies: ReplyState,
}

pub fn load_state(db: &Path) -> State {
    std::fs::read_to_string(state_path(db)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_state(db: &Path, state: &State) {
    let path = state_path(db);
    if crate::store_dir::is_held(&path) {
        return;
    }
    if let Ok(text) = serde_json::to_string(state) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

// --------------------------------------------------------------- the judge

/// Where a call ran, as the hook would have resolved it.
struct Place {
    project: Option<String>,
    root: Option<PathBuf>,
    cwd: Option<PathBuf>,
}

/// The hook's pipeline over one store, shared across many calls.
pub struct Judge<'a> {
    store: &'a EventStore,
    live: Live<'a>,
    projects: Vec<String>,
    places: HashMap<String, Place>,
}

fn norm(s: &str) -> String {
    s.to_lowercase().replace(['_', ' ', '.'], "-")
}

impl<'a> Judge<'a> {
    pub fn new(store: &'a EventStore) -> Self {
        let live = Live::for_replay(store);
        let mut projects: Vec<String> = live.items().iter().filter_map(|li| li.item.project.clone()).collect();
        projects.sort();
        projects.dedup();
        Judge { store, live, projects, places: HashMap::new() }
    }

    fn place(&mut self, row: &CallRow) -> &Place {
        let key = format!("{}\u{1}{}\u{1}{}", row.cwd, row.p.as_deref().unwrap_or_default(), row.d.as_deref().unwrap_or_default());
        if !self.places.contains_key(&key) {
            let cwd = (!row.cwd.is_empty()).then(|| PathBuf::from(&row.cwd));
            let existing = cwd.as_deref().and_then(corpus::nearest_existing);
            let folder_project = existing.as_deref().and_then(crate::project::resolve_project);
            let mut session_project = folder_project.clone().or_else(|| row.p.clone());
            if session_project.is_none() {
                let dir = norm(row.d.as_deref().unwrap_or_default());
                let mut best: Option<&String> = None;
                for p in &self.projects {
                    if dir.contains(&format!("dev-{}", norm(p))) && best.is_none_or(|b| p.len() > b.len()) {
                        best = Some(p);
                    }
                }
                session_project = best.cloned();
            }
            let root = crate::project::guard_root(self.store, existing.as_deref(), folder_project.as_deref(), session_project.as_deref());
            self.places.insert(key.clone(), Place { project: session_project, root, cwd: existing });
        }
        &self.places[&key]
    }

    /// The rules the hook would refuse `row` under, as the store stands. The
    /// owner has said nothing (a rule that needs his go refuses).
    pub fn refusing_rules(&mut self, row: &CallRow) -> Vec<String> {
        let owner_said_nothing: &dyn Fn() -> Result<Vec<String>, String> = &|| Ok(Vec::new());
        let (project, root, cwd) = {
            let place = self.place(row);
            (place.project.clone(), place.root.clone(), place.cwd.clone())
        };
        let input = (!row.input.is_null()).then_some(&row.input);
        let judged = guard_verdict::pre_tool_in(
            &self.live,
            &row.tool,
            input,
            project.as_deref(),
            root.as_deref(),
            cwd.as_deref(),
            Some(owner_said_nothing),
        );
        let Some(verdict) = judged.verdict else { return Vec::new() };
        let mut rules: Vec<String> = crate::absent_guard::rule_ids_of(&verdict.reason).into_iter().map(str::to_string).collect();
        if rules.is_empty() {
            rules.extend(verdict.rule_id);
        }
        rules
    }
}

fn excerpt(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}...", flat.chars().take(max).collect::<String>())
    } else {
        flat
    }
}

/// A call as an example shows it: the command, the file, or the tool.
pub fn call_excerpt(row: &CallRow) -> String {
    let named = |field: &str| row.input.get(field).and_then(|v| v.as_str()).map(str::to_string);
    let what = named("command")
        .or_else(|| named("file_path").map(|f| format!("{} {f}", row.tool)))
        .or_else(|| named("prompt").map(|p| format!("{}: {p}", row.tool)))
        .unwrap_or_else(|| format!("{} {}", row.tool, row.input));
    excerpt(&what, CALL_EXCERPT)
}

fn call_key(row: &CallRow) -> String {
    fnv(&format!("{}\u{1}{}\u{1}{}", row.tool, row.cwd, row.input))
}

fn short_id(id: &str) -> String {
    let n = id.chars().count();
    id.chars().skip(n.saturating_sub(8)).collect()
}

fn occ_of(row: &CallRow) -> Occ {
    Occ { id: short_id(&row.id), t: row.t, s: row.s.clone(), r: row.r.clone(), ok: row.ok, b: row.b.clone() }
}

/// Add one occurrence to a hit (once per call id), keeping the latest.
fn add_occ(hit: &mut Hit, occ: Occ) {
    if hit.occ.iter().any(|o| o.id == occ.id) {
        return;
    }
    hit.n += 1;
    hit.occ.push(occ);
    hit.occ.sort_by_key(|o| o.t);
    if hit.occ.len() > OCC_KEPT {
        hit.occ.remove(0);
    }
}

// ------------------------------------------------------------- the sweep run

/// How a run is bounded and what it is allowed to do.
#[derive(Debug, Clone)]
pub struct Opts {
    pub budget: Duration,
    /// Start over: forget what was swept.
    pub full: bool,
    pub now: i64,
}

impl Opts {
    pub fn new(budget: Duration) -> Self {
        Opts { budget, full: false, now: crate::time::now_unix() }
    }
}

/// The refusing rules of the store by id, with their fingerprints.
fn refusing_rules_of(store: &EventStore) -> BTreeMap<String, (String, bool)> {
    crate::live::live_items(store)
        .into_iter()
        .filter(|li| can_refuse(&li.item))
        .map(|li| (li.id, (fingerprint(&li.item), is_owner_approval(&li.item))))
        .collect()
}

/// What a run covered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Covered {
    pub calls_replayed: u64,
    pub replies_replayed: u64,
    pub rules_replayed: usize,
    /// Everything the run set out to do was done.
    pub complete: bool,
}

/// A run's whole answer.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub calls: Vec<RuleLine>,
    pub answers: Vec<RuleLine>,
    pub rows: u64,
    pub reply_rows: u64,
    pub covered: Covered,
    pub took_ms: u128,
}

impl Report {
    pub fn flagged_rules(&self) -> Vec<&RuleLine> {
        self.calls.iter().filter(|l| l.flagged()).collect()
    }

    pub fn flagged_answers(&self) -> Vec<&RuleLine> {
        self.answers.iter().filter(|l| l.flagged()).collect()
    }
}

/// One rule's (or answer-rulebook entry's) result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleLine {
    pub id: String,
    /// Distinct calls (replies) it would refuse (fire on).
    pub refuses: usize,
    /// How many corpus rows were tried.
    pub of: u64,
    /// How many of the refused ran fine (see the module doc).
    pub ran_fine: usize,
    /// How many of them THOR's hook refused at the time too.
    pub refused_then: usize,
    pub owner_approval: bool,
    /// How many of the refused ran fine too, but before the rule stood last,
    /// or in a session no hook was alive in: not evidence, so not counted.
    pub uncounted: usize,
    /// When the rule last changed (unix seconds), when the log can tell.
    pub changed: Option<i64>,
    /// One line per example, those that ran fine first.
    pub examples: Vec<String>,
}

impl RuleLine {
    pub fn flagged(&self) -> bool {
        self.ran_fine > 0 && !self.owner_approval
    }
}

/// The unix time a log position was written, read off the nearest earlier
/// event that carries a time (a serving, a refusal or a stand-aside). An
/// event has no time of its own. `None` when none is near.
pub fn time_of_seq(store: &EventStore, seq: i64) -> Option<i64> {
    for s in (seq.saturating_sub(400)..=seq).rev() {
        let Ok(Some(event)) = store.get_event_by_seq(s) else { continue };
        if !matches!(event.kind, EventKind::ItemServed | EventKind::GateRefused | EventKind::GateStoodAside) {
            continue;
        }
        let body = serde_json::from_str::<Value>(&event.body).unwrap_or(Value::Null);
        let when = body.get("served_at").or_else(|| body.get("at")).and_then(|v| v.as_str());
        if let Some(t) = when.and_then(crate::time::unix_from_iso8601) {
            return Some(t);
        }
    }
    None
}

/// When the rule last changed: the time of its newest created or revised
/// event. `None` when it cannot be told.
pub fn rule_changed_at(store: &EventStore, id: &str) -> Option<i64> {
    let events = store.get_events_by_entity(id).ok()?;
    let last = events.iter().rev().find(|e| matches!(e.kind, EventKind::FactCreated | EventKind::FactRevised))?;
    time_of_seq(store, last.seq)
}

/// Run the sweep: replay what is new (and what a changed rule reaches), until
/// `opts.budget` is spent, keep the state, and report.
pub fn run(db: &Path, store: &EventStore, opts: &Opts) -> Report {
    let started = Instant::now();
    let deadline = started + opts.budget;
    let mut state = load_state(db);
    if state.v != STATE_VERSION {
        state = State { v: STATE_VERSION, engine: crate::BUILD_ID.to_string(), full_at: opts.now, ..State::default() };
    } else if state.engine != crate::BUILD_ID || opts.full || opts.now - state.full_at > FULL_EVERY_SECS {
        // Everything is tried again - the engine may decide differently, a
        // project may have been registered - but what the sweep found stays
        // in sight until the new pass replaces it, row by row.
        state.engine = crate::BUILD_ID.to_string();
        state.full_at = opts.now;
        state.calls.done_id = None;
        state.calls.pending = None;
        state.calls.fps.clear();
        state.calls.scan += 1;
        state.replies.seen.clear();
        state.replies.fps.clear();
    }
    if opts.now - state.pruned_at > 86_400 {
        corpus::prune(db, opts.now);
        state.pruned_at = opts.now;
    }
    let mut covered = Covered { complete: true, ..Covered::default() };
    // Calls take three quarters of the time; replies what is left.
    let calls_deadline = started + opts.budget.mul_f64(0.75);
    let rules = refusing_rules_of(store);
    sweep_calls(db, store, &rules, &mut state.calls, calls_deadline.min(deadline), opts.now, &mut covered);
    sweep_replies(db, &mut state.replies, deadline, opts.now, &mut covered);
    state.last_run = opts.now;
    save_state(db, &state);
    let mut report = build_report(store, &state);
    report.covered = covered;
    report.took_ms = started.elapsed().as_millis();
    report
}

/// The calls half of a run: the rules that changed are replayed over the rows
/// they can reach, then the rows written since the last run are replayed
/// against the whole store.
fn sweep_calls(
    db: &Path,
    store: &EventStore,
    rules: &BTreeMap<String, (String, bool)>,
    state: &mut CallState,
    deadline: Instant,
    now: i64,
    covered: &mut Covered,
) {
    // Rules that are gone: their hits are forgotten with them, and the rows
    // they hid behind are looked at again.
    let removed: BTreeSet<String> = state.fps.keys().filter(|id| !rules.contains_key(*id)).cloned().collect();
    let dirty: BTreeMap<String, String> = rules
        .iter()
        .filter(|(id, (fp, _))| state.fps.get(*id) != Some(fp))
        .map(|(id, (fp, _))| (id.clone(), fp.clone()))
        .collect();
    if state.done_id.is_none() {
        // Nothing was swept yet: the main pass tries every row against every
        // rule as it stands, so nothing is dirty.
        state.fps = rules.iter().map(|(id, (fp, _))| (id.clone(), fp.clone())).collect();
    } else if state.pending.is_none() && (!dirty.is_empty() || !removed.is_empty()) {
        let mut queued = dirty;
        for id in removed {
            queued.insert(id, String::new());
        }
        state.pending = Some(Pending { rules: queued, after: None });
    }
    if state.pending.is_some() && !replay_pending(db, store, rules, state, deadline, now, covered) {
        covered.complete = false;
        return;
    }
    main_pass(db, store, state, deadline, now, covered);
}

/// How many rows one round of the replay takes from the file.
const ROUND_ROWS: usize = 12_000;

/// The next rows of the corpus after the call `after` names, at most `limit`,
/// ending at (and including) the call `stop_at` names when one is given.
fn next_rows(
    db: &Path,
    now: i64,
    patches: &HashMap<String, CallRow>,
    after: Option<&str>,
    limit: usize,
    stop_at: Option<&str>,
) -> (Vec<CallRow>, corpus::Scanned) {
    let mut rows: Vec<CallRow> = Vec::new();
    let scanned = corpus::for_each_call_patched(db, now, patches, after, |row| {
        let last = stop_at == Some(row.id.as_str());
        rows.push(row);
        !last && rows.len() < limit
    });
    (rows, scanned)
}

/// What the hook would refuse each row under, as the store stands: `None` for
/// a row the time ran out before. One thread: measured on 156,000 real calls,
/// six threads finished no sooner than one - the time goes into reading the
/// files a write is judged against, which the machine does one at a time.
fn replay_rows(store: &EventStore, rows: &[CallRow], deadline: Instant) -> Vec<Option<Vec<String>>> {
    let mut out: Vec<Option<Vec<String>>> = vec![None; rows.len()];
    let mut judge = Judge::new(store);
    for (i, row) in rows.iter().enumerate() {
        if i % 16 == 0 && Instant::now() >= deadline {
            break;
        }
        out[i] = Some(judge.refusing_rules(row));
    }
    out
}

/// Replay the rows a changed or removed rule can reach, up to the last row the
/// main pass has done, replacing what the sweep knew of each. `true` when it
/// got to the end; `false` when the time ran out, with the place it got to kept
/// in `state.pending` for the next run.
fn replay_pending(
    db: &Path,
    store: &EventStore,
    rules: &BTreeMap<String, (String, bool)>,
    state: &mut CallState,
    deadline: Instant,
    now: i64,
    covered: &mut Covered,
) -> bool {
    let Some(pending) = state.pending.clone() else { return true };
    let items: HashMap<String, Item> = crate::live::live_items(store).into_iter().map(|li| (li.id, li.item)).collect();
    let needs: Vec<Option<Vec<Vec<String>>>> = pending.rules.keys().filter_map(|id| items.get(id)).map(needles).collect();
    // A rule with no way to narrow it can reach any row.
    let reach_all = needs.iter().any(Option::is_none);
    let alternatives: Vec<Vec<String>> = needs.into_iter().flatten().flatten().collect();
    let touched: BTreeSet<&String> = pending.rules.keys().collect();
    let mut replayed: HashMap<String, bool> = HashMap::new();
    let mut after = pending.after.clone();
    let stop_at = state.done_id.clone();
    let mut reached_end = false;
    let patches = corpus::load_patches(db);
    while !reached_end {
        let (rows, scanned) = next_rows(db, now, &patches, after.as_deref(), ROUND_ROWS, stop_at.as_deref());
        if !scanned.found {
            // The row it stopped at was cut away from the corpus: start again.
            if let Some(p) = state.pending.as_mut() {
                p.after = None;
            }
            return replay_pending(db, store, rules, state, deadline, now, covered);
        }
        if rows.is_empty() {
            break;
        }
        reached_end = stop_at.as_deref() == rows.last().map(|r| r.id.as_str());
        // The rows to judge: one per call, those a changed rule can reach.
        let mut wanted: Vec<usize> = Vec::new();
        let mut keys: Vec<String> = Vec::with_capacity(rows.len());
        let mut reached: Vec<bool> = Vec::with_capacity(rows.len());
        let mut queued: HashSet<String> = HashSet::new();
        for (i, row) in rows.iter().enumerate() {
            let key = call_key(row);
            let known = replayed.contains_key(&key) || state.hits.get(&key).is_some_and(|h| h.rules.iter().any(|r| touched.contains(r)));
            let is_reached = !row.cut
                && (reach_all
                    || known
                    || (!alternatives.is_empty() && reached_by_needles(&alternatives, &format!("{} {}", row.tool, row.input).to_lowercase())));
            if is_reached && !replayed.contains_key(&key) && queued.insert(key.clone()) {
                wanted.push(i);
            }
            keys.push(key);
            reached.push(is_reached);
        }
        let judged_rows: Vec<CallRow> = wanted.iter().map(|&i| rows[i].clone()).collect();
        let verdicts = replay_rows(store, &judged_rows, deadline);
        let mut by_row: HashMap<usize, Option<Vec<String>>> = wanted.iter().copied().zip(verdicts).collect();
        let mut finished_round = true;
        for (i, row) in rows.iter().enumerate() {
            let key = &keys[i];
            if reached[i] && !replayed.contains_key(key) {
                match by_row.remove(&i).flatten() {
                    // No time was left for this row: the run stops before it.
                    None => {
                        finished_round = false;
                        break;
                    }
                    Some(now_refused) => {
                        // The first sight of this call in this replay: what the
                        // sweep knew of it is replaced by what the store says now.
                        state.hits.remove(key);
                        if !now_refused.is_empty() {
                            state.hits.insert(
                                key.clone(),
                                Hit { tool: row.tool.clone(), call: call_excerpt(row), rules: now_refused.clone(), n: 0, occ: Vec::new(), g: state.scan },
                            );
                        }
                        replayed.insert(key.clone(), !now_refused.is_empty());
                    }
                }
            }
            if !row.r.is_empty() || row.h {
                state.active.insert(row.s.clone());
            }
            if reached[i] && replayed.get(key).copied().unwrap_or(false) {
                if let Some(hit) = state.hits.get_mut(key) {
                    add_occ(hit, occ_of(row));
                }
            }
            after = Some(row.id.clone());
        }
        covered.rules_replayed = covered.rules_replayed.max(pending.rules.len());
        if !finished_round || Instant::now() >= deadline {
            if let Some(p) = state.pending.as_mut() {
                p.after = after;
            }
            return false;
        }
    }
    for (id, fp) in &pending.rules {
        if rules.contains_key(id) {
            state.fps.insert(id.clone(), fp.clone());
        }
    }
    state.fps.retain(|id, _| rules.contains_key(id));
    state.pending = None;
    true
}

/// Replay the rows written since the last main pass against the whole store.
fn main_pass(db: &Path, store: &EventStore, state: &mut CallState, deadline: Instant, now: i64, covered: &mut Covered) {
    let mut finished = true;
    let mut tried = 0u64;
    let mut cut = 0u64;
    let patches = corpus::load_patches(db);
    loop {
        let after = state.done_id.clone();
        let (rows, scanned) = next_rows(db, now, &patches, after.as_deref(), ROUND_ROWS, None);
        if !scanned.found {
            // The row the last pass stopped at was cut away from the corpus;
            // every row that is left is new to it.
            state.done_id = None;
            continue;
        }
        if rows.is_empty() {
            break;
        }
        // The rows to judge: one per call, none the sweep already knows.
        let mut wanted: Vec<usize> = Vec::new();
        let mut keys: Vec<String> = Vec::with_capacity(rows.len());
        let mut queued: HashSet<String> = HashSet::new();
        for (i, row) in rows.iter().enumerate() {
            let key = call_key(row);
            // A call some earlier pass judged is judged again in a new one.
            let known = state.hits.get(&key).is_some_and(|h| h.g == state.scan);
            if !row.cut && !known && queued.insert(key.clone()) {
                wanted.push(i);
            }
            keys.push(key);
        }
        let judged_rows: Vec<CallRow> = wanted.iter().map(|&i| rows[i].clone()).collect();
        let verdicts = replay_rows(store, &judged_rows, deadline);
        let mut by_row: HashMap<usize, Option<Vec<String>>> = wanted.iter().copied().zip(verdicts).collect();
        let mut stopped = false;
        for (i, row) in rows.iter().enumerate() {
            let key = &keys[i];
            if wanted.binary_search(&i).is_ok() {
                match by_row.remove(&i).flatten() {
                    None => {
                        stopped = true;
                        break;
                    }
                    Some(refused) => {
                        // What the sweep knew of this call is replaced by
                        // what the store says now.
                        state.hits.remove(key);
                        if !refused.is_empty() {
                            state.hits.insert(
                                key.clone(),
                                Hit { tool: row.tool.clone(), call: call_excerpt(row), rules: refused, n: 0, occ: Vec::new(), g: state.scan },
                            );
                        }
                    }
                }
            }
            if !row.r.is_empty() || row.h {
                state.active.insert(row.s.clone());
            }
            if let Some(hit) = state.hits.get_mut(key) {
                add_occ(hit, occ_of(row));
            }
            if row.cut {
                cut += 1;
            }
            tried += 1;
            state.done_id = Some(row.id.clone());
        }
        if stopped || Instant::now() >= deadline {
            finished = false;
            break;
        }
    }
    state.rows = corpus::count_calls(db, now);
    state.cut_rows += cut;
    covered.calls_replayed += tried;
    covered.complete &= finished;
    // What is older than the window goes.
    let from = now - corpus::KEEP_DAYS * 86_400;
    state.hits.retain(|_, hit| {
        hit.occ.retain(|o| o.t >= from);
        !hit.occ.is_empty()
    });
}

// ------------------------------------------------------------------- replies

/// One entry of the answer rulebook.
struct Entry {
    id: String,
    fp: String,
    single: String,
    reminder: String,
}

fn rulebook_entries(db: &Path) -> (Vec<Entry>, i64) {
    let path = crate::respond::default_rulebook_path(db);
    let mtime = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    let Some(parsed) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        return (Vec::new(), mtime);
    };
    let entries = parsed
        .as_array()
        .map(|all| {
            all.iter()
                .filter_map(|r| {
                    let id = r.get("id")?.as_str()?.to_string();
                    Some(Entry {
                        fp: fnv(&r.to_string()),
                        single: serde_json::to_string(&serde_json::json!([r])).ok()?,
                        reminder: r.get("reminder").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                        id,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (entries, mtime)
}

fn reply_key(row: &ReplyRow) -> String {
    fnv(&format!("{}\u{1}{}\u{1}{}\u{1}{}", row.t, row.s, row.reply.len(), row.reply.chars().take(120).collect::<String>()))
}

/// The lookups the hook gives the Response Guard, for a reply made in `cwd`.
struct Lookups {
    root: Option<PathBuf>,
    cwd: Option<PathBuf>,
    commits: std::cell::RefCell<HashMap<String, Option<HashSet<String>>>>,
}

impl Lookups {
    fn of(cwd: &str) -> Lookups {
        let cwd = (!cwd.is_empty()).then(|| PathBuf::from(cwd)).filter(|p| p.exists());
        let root = cwd.as_deref().and_then(|c| c.canonicalize().ok());
        Lookups { root, cwd, commits: Default::default() }
    }
}

fn judge_reply(single: &str, row: &ReplyRow, lookups: &Lookups) -> (Option<String>, Option<String>) {
    let line_exists = |cited: &str, line: u64| lookups.root.as_deref().is_some_and(|r| crate::evidence::cited_line_exists(r, cited, line));
    let real_commits = |candidates: &[String]| {
        let cwd = lookups.cwd.as_deref()?;
        let key = candidates.join(",");
        if let Some(known) = lookups.commits.borrow().get(&key) {
            return known.clone();
        }
        let found = crate::evidence::real_commits(cwd, candidates);
        lookups.commits.borrow_mut().insert(key, found.clone());
        found
    };
    let evidence = crate::respond::EvidenceLookup { line_exists: &line_exists, real_commits: &real_commits };
    let lookup = if lookups.root.is_some() { Some(&evidence) } else { None };
    let verdict = crate::respond::guard_verdict_in_turn(
        Some(single),
        &row.reply,
        &row.prompt,
        row.a && !row.o,
        lookup,
        &row.cmds,
    );
    (verdict.block_reason, verdict.warn_reason)
}

fn sweep_replies(db: &Path, state: &mut ReplyState, deadline: Instant, now: i64, covered: &mut Covered) {
    let (entries, rulebook_mtime) = rulebook_entries(db);
    let replies = corpus::load_replies(db, now);
    state.rows = replies.len() as u64;
    state.cut_rows = replies.iter().filter(|r| r.cut).count() as u64;
    // Entries that changed (or are new) are tried over every reply; the rest
    // over the replies not tried yet.
    let first_run = state.fps.is_empty();
    let dirty: HashSet<String> = entries.iter().filter(|e| state.fps.get(&e.id) != Some(&e.fp)).map(|e| e.id.clone()).collect();
    for entry in &entries {
        if dirty.contains(&entry.id) {
            state.since.insert(entry.id.clone(), if first_run { rulebook_mtime } else { now });
        }
    }
    let live_ids: HashSet<&String> = entries.iter().map(|e| &e.id).collect();
    state.fps.retain(|id, _| live_ids.contains(id));
    state.since.retain(|id, _| live_ids.contains(id));
    for hit in state.hits.values_mut() {
        hit.fired.retain(|id, _| live_ids.contains(id) && !dirty.contains(id));
    }
    let mut keys_now: BTreeSet<String> = BTreeSet::new();
    let mut finished = true;
    let mut tried = 0u64;
    for row in &replies {
        let key = reply_key(row);
        keys_now.insert(key.clone());
        let fresh = !state.seen.contains(&key);
        if row.cut || (!fresh && dirty.is_empty()) {
            continue;
        }
        if tried % 8 == 0 && Instant::now() >= deadline {
            finished = false;
            break;
        }
        tried += 1;
        let lookups = Lookups::of(&row.cwd);
        if fresh {
            // Tried again from the start: what an earlier pass said of this
            // reply is said again, or no longer.
            if let Some(hit) = state.hits.get_mut(&key) {
                hit.fired.clear();
                hit.said_then.clear();
            }
        }
        for entry in &entries {
            if !fresh && !dirty.contains(&entry.id) {
                continue;
            }
            let (block, warn) = judge_reply(&entry.single, row, &lookups);
            let how = match (block, warn) {
                (Some(_), _) => "block",
                (None, Some(_)) => "warn",
                _ => continue,
            };
            let hit = state.hits.entry(key.clone()).or_insert_with(|| ReplyHit {
                t: row.t,
                s: row.s.clone(),
                excerpt: excerpt(&row.reply, REPLY_EXCERPT),
                fired: BTreeMap::new(),
                said_then: Vec::new(),
                delivered_clean: !row.held && !row.warn && row.b == crate::BUILD_ID,
            });
            hit.fired.insert(entry.id.clone(), how.to_string());
            if !entry.reminder.is_empty() && row.why.contains(entry.reminder.chars().take(60).collect::<String>().as_str()) {
                hit.said_then.retain(|e| e != &entry.id);
                hit.said_then.push(entry.id.clone());
            }
        }
        state.seen.insert(key);
    }
    state.hits.retain(|_, h| !h.fired.is_empty());
    if finished {
        for entry in &entries {
            state.fps.insert(entry.id.clone(), entry.fp.clone());
        }
        state.seen.retain(|k| keys_now.contains(k));
    } else {
        // A changed entry is not done: its fingerprint stays as it was, so
        // the next run tries it again from the start.
        for entry in &entries {
            if !dirty.contains(&entry.id) {
                state.fps.insert(entry.id.clone(), entry.fp.clone());
            }
        }
    }
    covered.replies_replayed += tried;
    covered.complete &= finished;
}

// -------------------------------------------------------------------- report

fn build_report(store: &EventStore, state: &State) -> Report {
    let rules: HashMap<String, Item> = crate::live::live_items(store).into_iter().map(|li| (li.id, li.item)).collect();
    let mut per_rule: BTreeMap<String, Vec<&Hit>> = BTreeMap::new();
    for hit in state.calls.hits.values() {
        for rule in &hit.rules {
            if rules.contains_key(rule) {
                per_rule.entry(rule.clone()).or_default().push(hit);
            }
        }
    }
    let mut calls: Vec<RuleLine> = Vec::new();
    for (id, hits) in per_rule {
        let item = &rules[&id];
        let owner = is_owner_approval(item);
        let since = rule_changed_at(store, &id);
        let mut ran: Vec<&Hit> = Vec::new();
        let mut other: Vec<&Hit> = Vec::new();
        let mut refused_then = 0;
        let mut uncounted = 0;
        for hit in &hits {
            if hit.occ.iter().any(|o| o.r.contains(&id)) {
                refused_then += 1;
            }
            // A rule that waits for the owner's go lets a call through when he
            // gave it: that call running is the rule working.
            if !owner && ran_fine(hit, &id, since, &state.calls.active) {
                ran.push(hit);
            } else {
                if hit.occ.iter().any(|o| o.r.is_empty() && o.ok == Some(true)) {
                    uncounted += 1;
                }
                other.push(hit);
            }
        }
        let examples = ran
            .iter()
            .map(|h| (h, "ran fine"))
            .chain(other.iter().map(|h| (h, "would be refused")))
            .take(EXAMPLES_SHOWN)
            .map(|(h, how)| {
                let when = h.occ.last().map_or(String::new(), |o| format!(" ({})", crate::time::iso8601_from_unix(o.t).chars().take(10).collect::<String>()));
                format!("'{}'{when}, {how}", h.call)
            })
            .collect();
        calls.push(RuleLine { id, refuses: hits.len(), of: state.calls.rows, ran_fine: ran.len(), refused_then, owner_approval: owner, uncounted, changed: since, examples });
    }
    calls.sort_by(|a, b| b.ran_fine.cmp(&a.ran_fine).then(b.refuses.cmp(&a.refuses)).then(a.id.cmp(&b.id)));
    let answers = answer_lines(&state.replies);
    Report {
        calls,
        answers,
        rows: state.calls.rows,
        reply_rows: state.replies.rows,
        covered: Covered::default(),
        took_ms: 0,
    }
}

/// Whether one hit counts as "ran fine" for `rule` (see the module doc).
pub fn ran_fine(hit: &Hit, rule: &str, since: Option<i64>, active: &BTreeSet<String>) -> bool {
    // Only a call the hook judged with THIS build is comparable with a replay
    // by it: an older build may have decided differently, and an install
    // would otherwise flag every rule it made stricter for 30 days.
    let same_build = |o: &Occ| o.b == crate::BUILD_ID;
    // RAN UNCHANGED: refused under this rule, then the very same call ran.
    if let Some(at) = hit.occ.iter().position(|o| o.r.iter().any(|r| r == rule)) {
        if hit.occ[at + 1..].iter().any(|o| o.r.is_empty() && o.ok == Some(true) && same_build(o)) {
            return true;
        }
    }
    // LET THROUGH: not refused, ran, after the rule stood, a hook alive.
    let Some(since) = since else { return false };
    hit.occ.iter().any(|o| o.r.is_empty() && o.ok == Some(true) && same_build(o) && o.t >= since + GRACE_SECS && active.contains(&o.s))
}

fn answer_lines(state: &ReplyState) -> Vec<RuleLine> {
    let mut per_entry: BTreeMap<&String, Vec<&ReplyHit>> = BTreeMap::new();
    for hit in state.hits.values() {
        for id in hit.fired.keys() {
            per_entry.entry(id).or_default().push(hit);
        }
    }
    let mut out = Vec::new();
    for (id, hits) in per_entry {
        let since = state.since.get(id).copied();
        let said_then = hits.iter().filter(|h| h.said_then.contains(id)).count();
        let ran: Vec<&&ReplyHit> = hits
            .iter()
            .filter(|h| h.delivered_clean && since.is_some_and(|s| h.t >= s + GRACE_SECS))
            .collect();
        let examples = ran
            .iter()
            .map(|h| (**h, "went through"))
            .chain(hits.iter().filter(|h| !ran.iter().any(|r| std::ptr::eq(**r, **h))).map(|h| (*h, "would fire")))
            .take(EXAMPLES_SHOWN)
            .map(|(h, how)| format!("'{}' ({}), {how}", h.excerpt, crate::time::iso8601_from_unix(h.t).chars().take(10).collect::<String>()))
            .collect();
        out.push(RuleLine { id: id.clone(), refuses: hits.len(), of: state.rows, ran_fine: ran.len(), refused_then: said_then, owner_approval: false, uncounted: 0, changed: since, examples });
    }
    out.sort_by(|a, b| b.ran_fine.cmp(&a.ran_fine).then(b.refuses.cmp(&a.refuses)).then(a.id.cmp(&b.id)));
    out
}

// ------------------------------------------------------------------ surfaces

fn named(lines: &[&RuleLine]) -> String {
    let mut names: Vec<String> = lines.iter().take(NAMED_MAX).map(|l| l.id.clone()).collect();
    if lines.len() > NAMED_MAX {
        names.push(format!("and {} more", lines.len() - NAMED_MAX));
    }
    names.join(", ")
}

/// The lines `doctor` shows: silent when no rule would refuse a call that ran
/// fine.
pub fn doctor_lines(report: &Report) -> Vec<String> {
    let mut out = Vec::new();
    let rules = report.flagged_rules();
    if !rules.is_empty() {
        out.push(format!(
            "false-block sweep: {} rule(s) would refuse real calls that ran fine: {} (run serve sweep for detail)",
            rules.len(),
            named(&rules)
        ));
    }
    let answers = report.flagged_answers();
    if !answers.is_empty() {
        out.push(format!(
            "answer-guard sweep: {} entr(y/ies) would fire on real replies that went through: {} (run serve sweep for detail)",
            answers.len(),
            named(&answers)
        ));
    }
    out
}

/// What `serve sweep` prints.
pub fn detail_lines(report: &Report) -> Vec<String> {
    let mut out = vec![format!(
        "sweep: {} real call(s) and {} real reply(ies) in the corpus; tried {} call(s) and {} reply(ies) this run in {} ms{}",
        report.rows,
        report.reply_rows,
        report.covered.calls_replayed,
        report.covered.replies_replayed,
        report.took_ms,
        if report.covered.complete { "" } else { "; the time ran out, the next run carries on" }
    )];
    let shown: Vec<&RuleLine> = report.calls.iter().collect();
    if shown.is_empty() {
        out.push("rules: none would refuse a real call of the last 30 days".to_string());
    }
    for line in shown {
        out.push(rule_line("rule", line));
    }
    for line in &report.answers {
        out.push(rule_line("answer-guard entry", line));
    }
    out
}

/// The report as `serve sweep --json` prints it.
pub fn report_json(report: &Report) -> Value {
    let lines = |lines: &[RuleLine]| -> Vec<Value> {
        lines
            .iter()
            .map(|l| {
                serde_json::json!({
                    "id": l.id, "refuses": l.refuses, "of": l.of, "ran_fine": l.ran_fine,
                    "refused_then": l.refused_then, "flagged": l.flagged(), "examples": l.examples,
                })
            })
            .collect()
    };
    serde_json::json!({
        "rows": report.rows, "reply_rows": report.reply_rows, "complete": report.covered.complete,
        "took_ms": report.took_ms as u64, "rules": lines(&report.calls), "answers": lines(&report.answers),
    })
}

fn rule_line(what: &str, line: &RuleLine) -> String {
    let mut out = format!(
        "{what} '{}': would {} {} of {} real, {} of them ran fine{}{}{}",
        line.id,
        if what == "rule" { "refuse" } else { "fire on" },
        line.refuses,
        line.of,
        line.ran_fine,
        if line.refused_then > 0 { format!(", {} also refused at the time", line.refused_then) } else { String::new() },
        match (line.uncounted, line.changed) {
            (0, _) => String::new(),
            (n, Some(t)) => format!(
                ", {n} more ran fine but before the rule last changed ({}) or with no live hook, not counted",
                crate::time::iso8601_from_unix(t).chars().take(10).collect::<String>()
            ),
            (n, None) => format!(", {n} more ran fine but the log cannot date the rule, not counted"),
        },
        if line.owner_approval { " (needs the owner's go, never flagged)" } else { "" },
    );
    for example in &line.examples {
        out.push_str(&format!("\n    e.g. {example}"));
    }
    out
}

/// Fill the frozen signals of an evaluation start with what the sweep found,
/// running the sweep first within `budget`.
pub fn for_evaluation(store: &EventStore, budget: Duration) -> (Vec<model::store::SweepHit>, usize, Covered) {
    let Some(db) = store.path() else { return (Vec::new(), 0, Covered::default()) };
    let report = run(&db, store, &Opts::new(budget));
    let mut hits: Vec<model::store::SweepHit> = Vec::new();
    for line in report.flagged_rules() {
        hits.push(sweep_hit("rule", line));
    }
    for line in report.flagged_answers() {
        hits.push(sweep_hit("answer", line));
    }
    let more = hits.len().saturating_sub(NAMED_MAX);
    hits.truncate(NAMED_MAX);
    (hits, more, report.covered)
}

fn sweep_hit(kind: &str, line: &RuleLine) -> model::store::SweepHit {
    model::store::SweepHit {
        rule: line.id.clone(),
        kind: kind.to_string(),
        refuses: line.refuses,
        of: line.of,
        ran_fine: line.ran_fine,
        example: line.examples.first().cloned().unwrap_or_default(),
    }
}

/// The sweep result as the false-block proxy's blind spot closes it: rules
/// the sweep names, not already named by the proxy itself.
pub fn proxy_clause(hits: &[model::store::SweepHit], more: usize) -> Option<String> {
    if hits.is_empty() {
        return None;
    }
    let ids: Vec<&str> = hits.iter().map(|h| h.rule.as_str()).collect();
    Some(format!(
        "The proxy cannot see a false block that was walked around without weakening the rule: the false-block sweep \
         (item d of section 2) names {} rule(s) or answer-rulebook entr(y/ies) that would refuse or fire on real calls \
         or replies which ran fine: {}{}.",
        hits.len() + more,
        ids.join(", "),
        if more > 0 { format!(" and {more} more") } else { String::new() }
    ))
}

/// The line the write gate adds when a rule that can refuse is written: how
/// many real calls of the last 30 days it would refuse, with up to three.
/// Informs; never refuses. `None` for an item that cannot refuse.
pub fn would_refuse_note(store: &EventStore, item: &Item, budget: Duration) -> Option<String> {
    if !can_refuse(item) {
        return None;
    }
    let db = store.path()?;
    let now = crate::time::now_unix();
    let path = corpus::calls_path(&db);
    if !path.is_file() {
        return None;
    }
    let started = Instant::now();
    let needles = needles(item);
    // A call whose line does not name the rule's command words cannot be
    // reached by it: it is counted, never parsed.
    let wanted = |line: &str| needles.as_ref().is_none_or(|n| reached_by_needles(n, &line.to_lowercase()));
    let (mut candidates, rows, dropped) = corpus::select_calls(&db, now, NOTE_CANDIDATES_MAX, &wanted);
    if rows == 0 {
        return None;
    }
    // Newest first: if the time runs out, it is the oldest calls that are left.
    candidates.sort_by_key(|r| std::cmp::Reverse(r.t));
    let known = crate::teeth::known_projects(store);
    let alone = model::store::one_item_store_among(item, &known).ok()?;
    let mut judge = Judge::new(&alone);
    let mut found: BTreeMap<String, (CallRow, usize)> = BTreeMap::new();
    let mut cut_off = false;
    let mut tried = 0usize;
    for (n, row) in candidates.into_iter().enumerate() {
        if row.cut {
            continue;
        }
        if let Some(needles) = &needles {
            if !reached_by_needles(needles, &format!("{} {}", row.tool, row.input).to_lowercase()) {
                continue;
            }
        }
        if n % 32 == 0 && started.elapsed() >= budget {
            cut_off = true;
            break;
        }
        tried += 1;
        let key = call_key(&row);
        if let Some((_, count)) = found.get_mut(&key) {
            *count += 1;
        } else if judge.refusing_rules(&row).iter().any(|r| r == &item.id) {
            found.insert(key, (row, 1));
        }
    }
    let mut ordered: Vec<&(CallRow, usize)> = found.values().collect();
    let fine = |r: &CallRow| r.r.is_empty() && r.ok == Some(true);
    ordered.sort_by(|a, b| fine(&b.0).cmp(&fine(&a.0)).then(b.0.t.cmp(&a.0.t)));
    let ran = ordered.iter().filter(|(r, _)| fine(r)).count();
    // Everything the rule could reach was tried, or only the newest part of it.
    let mut text = if cut_off || dropped {
        format!(
            "real calls: this rule would refuse at least {} of {} real calls in the last 30 days ({} of them ran fine when they \
             were made); only the newest {} of the calls it could reach were tried, so there may be more",
            ordered.len(),
            rows,
            ran,
            tried
        )
    } else {
        format!(
            "real calls: this rule would refuse {} of {} real calls in the last 30 days ({} of them ran fine when they were made)",
            ordered.len(),
            rows,
            ran
        )
    };
    for (row, n) in ordered.iter().take(EXAMPLES_SHOWN) {
        let day: String = crate::time::iso8601_from_unix(row.t).chars().take(10).collect();
        text.push_str(&format!(
            "\n    e.g. '{}' ({day}{}{})",
            call_excerpt(row),
            if *n > 1 { format!(", made {n} times") } else { String::new() },
            if fine(row) { ", ran fine" } else if row.r.is_empty() { "" } else { ", refused at the time" }
        ));
    }
    Some(text)
}
