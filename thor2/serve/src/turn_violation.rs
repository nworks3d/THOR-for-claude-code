//! The turn-end check for a rule that was broken by a route no tool guard
//! can see. A forbidden-text check (`absent`, `absent_all`) is enforced
//! when an Edit or a Write names the file, or when a shell command carries the
//! literal. A small script written elsewhere and run with `node script.mjs`
//! does neither, so the forbidden text lands in the file and THOR never sees
//! the write (measured in a benchmark run, 2026-11-19).
//!
//! THE FIX, at the end of a turn (the Stop hook). At the first tool call of a
//! turn that is not read-only (PreToolUse, before it runs: a shell command, an
//! Edit, an MCP file tool, a subagent...) `snapshot` tries the file check of every rule of the
//! project (global ones included, at most `MAX_WATCHED`; ONLY forbidden-text
//! checks, never `contains`: a required text is mostly a fact that goes out
//! of date when the owner asks for a change, and that is the stale-rule
//! guard's business, exactly as before), with the SAME runner
//! the write gate uses (`model::check::run`), and keeps for each whether it
//! already failed. A turn that made no change-capable tool call has no snapshot and costs
//! nothing: a new prompt only retires the previous one (`turn_started`), and the
//! rule list comes from a small cache (`rules`), never from parsing the store.
//! At the end of the turn `stop_check` tries them again. A
//! check that did not fail at the start and fails now was broken during the
//! turn, by whatever route. "Changed during this turn" is therefore decided
//! by the check itself, not by a file hash: a check that held and now fails
//! proves the file changed, and nothing else is read or scanned.
//!
//! NEVER A HOLD THAT CANNOT BE RELEASED. The way out is always the same:
//! undo the change, and the check holds again. A violation that stays is held
//! at most `MAX_HOLDS` times in a session; after that THOR says one plain line
//! once, records that it stood aside, and never holds for it again, so the
//! owner's own decision can never be trapped by it.
//!
//! NOT STALENESS. A check that held and now fails because of a change in this
//! turn is a violation, not a rule that went stale. The ids this check
//! covers are handed back so the stale-rule guard leaves them alone (and does
//! not invite weakening or retracting the rule).
//!
//! A violation that was already there at the start of a turn is not this
//! guard's business (it stays the stale-rule guard's), with one exception: a
//! violation found at a Stop and not yet put right is remembered (`open`) and
//! held again at a later Stop, within the cap above. A violation left by a
//! turn that never reached its Stop (the owner interrupted it) is carried over
//! the same way at the next turn start.
//!
//! A branch switch, checkout, pull, rebase or merge during the turn moves the
//! checkout's HEAD: then the turn is not judged at all (the files changed
//! under the session, not by it), and its baseline is dropped. (A stash moves
//! no HEAD and is not detected.)
//!
//! Everything fails open: an unreadable sidecar, a root that cannot be
//! resolved or a check that cannot run is silence, never a hold.

use crate::live::LiveItem;
use model::check::Outcome;
use model::item::Check;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Most rules watched per project and turn: keeps the turn start and the Stop
/// cheap, whatever the store holds.
pub const MAX_WATCHED: usize = 200;
/// How often one violation holds a session. After it, one line and silence.
pub const MAX_HOLDS: u32 = 2;
/// Sessions kept in the sidecar; the oldest go first.
const MAX_SESSIONS: usize = 40;
/// Violations named in one message.
const MAX_LISTED: usize = 5;

/// Beside the store, like every other sidecar: never inside a project folder.
pub fn default_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("turn-violations.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Watched {
    check: String,
    start_fails: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Open {
    check: String,
    holds: u32,
    #[serde(default)]
    told: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SessionRecord {
    at: i64,
    root: String,
    /// The checkout's branch and commit at the turn start (`head_state`).
    #[serde(default)]
    head: Option<String>,
    /// Lines of the checkout's `logs/HEAD` at the snapshot: what was added
    /// since tells a plain commit from a checkout, pull, reset and the like.
    #[serde(default)]
    reflog: usize,
    /// This snapshot belongs to the turn in progress: taken at the first tool
    /// call that can change things, cleared when the next prompt arrives
    /// (`turn_started`).
    #[serde(default)]
    current: bool,
    /// A Stop evaluated this record: the turn it belongs to was not cut off.
    #[serde(default)]
    stopped: bool,
    start: BTreeMap<String, Watched>,
    open: BTreeMap<String, Open>,
}

type Sidecar = BTreeMap<String, SessionRecord>;

// No lock on the sidecar: two hooks racing lose one update at worst, which
// fails open (a missed hold), never a wrong one.
fn read(path: &Path) -> Sidecar {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn write(path: &Path, mut all: Sidecar) {
    while all.len() > MAX_SESSIONS {
        let Some(oldest) = all.iter().min_by_key(|(_, r)| r.at).map(|(k, _)| k.clone()) else { break };
        all.remove(&oldest);
    }
    if let Ok(text) = serde_json::to_string(&all) {
        let _ = crate::store_dir::write(path, text);
    }
}

/// Only forbidden-text checks are watched: a text that appeared where it is
/// not allowed. `contains` (required text) is left to the stale-rule guard.
fn is_file_check(check: &Check) -> bool {
    matches!(check, Check::Absent { .. } | Check::AbsentAll { .. })
}

/// The checkout's own git directory (a linked worktree has its own).
fn git_dir_of(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if !dot_git.is_file() {
        return dot_git.is_dir().then_some(dot_git);
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let target = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
    Some(if target.is_absolute() { target } else { root.join(target) })
}

/// The lines of the checkout's HEAD reflog, read from the file (no process).
fn reflog(root: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(git_dir_of(root)?.join("logs").join("HEAD")).ok()?;
    Some(text.lines().map(str::to_string).collect())
}

/// Did the checkout change under the session since the snapshot, by more than
/// the session's own plain commits? True when the branch changed, or when any
/// reflog entry added since is not a `commit` or `commit (amend)` (a checkout,
/// pull, merge, rebase, reset, cherry-pick). A commit alone moves HEAD but not
/// the files, so the guard stays active. When the reflog cannot be read the
/// answer is true: judge nothing rather than blame the turn.
fn checkout_changed(root: &Path, head_then: &Option<String>, reflog_then: usize) -> bool {
    let head_now = head_state(root);
    if head_now == *head_then {
        return false;
    }
    let branch = |h: &Option<String>| h.as_deref().map(|s| s.split('@').next().unwrap_or("").to_string());
    if branch(&head_now) != branch(head_then) {
        return true;
    }
    let Some(lines) = reflog(root) else { return true };
    if lines.len() <= reflog_then {
        return true;
    }
    lines[reflog_then..].iter().any(|line| {
        let kind = line.split_once('\t').map(|(_, what)| what.split(':').next().unwrap_or("")).unwrap_or("");
        kind != "commit" && kind != "commit (amend)"
    })
}

/// The checkout's branch and commit, read from the `.git` files directly (no
/// process started). `None` outside a git checkout or when unreadable.
fn head_state(root: &Path) -> Option<String> {
    let git_dir = git_dir_of(root)?;
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref:").map(str::trim) else {
        return Some(format!("detached@{head}"));
    };
    // A linked worktree keeps its branches in the common directory.
    let common = std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|c| git_dir.join(c.trim()))
        .unwrap_or_else(|| git_dir.clone());
    let commit = std::fs::read_to_string(common.join(reference))
        .ok()
        .map(|c| c.trim().to_string())
        .or_else(|| {
            let packed = std::fs::read_to_string(common.join("packed-refs")).ok()?;
            packed
                .lines()
                .find_map(|l| l.strip_suffix(reference).and_then(|h| h.split_whitespace().next()).map(str::to_string))
        })
        .unwrap_or_default();
    Some(format!("{reference}@{commit}"))
}

/// The rules worth trying: fireable, of this project or global, carrying a
/// forbidden-text check. Sorted by id and capped AFTER that filter, so the cap
/// picks among forbidden-text rules only and is the same at start and Stop.
fn watched<'a>(items: &'a [LiveItem], project: Option<&str>) -> BTreeMap<&'a str, (&'a LiveItem, String)> {
    let mut out = BTreeMap::new();
    for li in items {
        let Some(check) = &li.item.check else { continue };
        if !li.item.kind.can_fire() || !is_file_check(check) || !crate::project::applies_to(li.item.project.as_deref(), project) {
            continue;
        }
        out.insert(li.id.as_str(), (li, format!("{check:?}")));
    }
    out.into_iter().take(MAX_WATCHED).collect()
}

fn run(li: &LiveItem, root: &Path) -> Option<Outcome> {
    li.item.check.as_ref().map(|check| model::check::run(check, root))
}

/// Does this tool call take the turn's snapshot? Every tool that is not
/// read-only does (Edit, Write, NotebookEdit, Bash, PowerShell, Agent, any
/// other `mcp__` tool): forbidden text may come in through any of them.
pub fn takes_snapshot(tool_name: &str) -> bool {
    const READ_ONLY: &[&str] = &["Read", "Grep", "Glob", "LS", "ToolSearch", "TodoWrite", "WebFetch", "WebSearch"];
    const THOR_READS: &[&str] = &["lookup", "get", "status", "history", "search_code", "where_used", "outline"];
    if READ_ONLY.contains(&tool_name) {
        return false;
    }
    !tool_name.strip_prefix("mcp__thor__").is_some_and(|t| THOR_READS.contains(&t))
}

/// Does the turn in progress already have its snapshot? One small file read;
/// everything else is skipped for a turn that never made a change-capable call.
pub fn has_snapshot(db: &Path, session_id: &str) -> bool {
    read(&default_path(db)).get(session_id).is_some_and(|r| r.current)
}

/// Does the session still owe a hold for a violation found at an earlier Stop? True while an
/// open violation has holds left or its one closing line is still unsaid. Such a violation is
/// checked at the end of the NEXT turn too, even one that made no change-capable tool call and
/// so has no snapshot: that is what the hold text promises.
pub fn has_open_violation(db: &Path, session_id: &str) -> bool {
    read(&default_path(db)).get(session_id).is_some_and(|r| r.open.values().any(|o| o.holds < MAX_HOLDS || !o.told))
}

/// A new prompt arrived: the snapshot of the previous turn no longer belongs
/// to the turn in progress, so the first change-capable tool call takes a fresh one. No
/// write at all for a session with no snapshot.
pub fn turn_started(db: &Path, session_id: &str) {
    let path = default_path(db);
    let mut all = read(&path);
    if let Some(record) = all.get_mut(session_id).filter(|r| r.current) {
        record.current = false;
        write(&path, all);
    }
}

#[derive(Serialize, Deserialize)]
struct RuleCache {
    fingerprint: (i64, i64),
    items: Vec<model::item::Item>,
}

/// The fireable forbidden-text rules of the whole store, WITHOUT parsing every
/// live item (about 70 ms on a store of 5000 items): kept in a small sidecar
/// next to the store, valid while the heads fingerprint is the one it was
/// built from (`projected_heads_fingerprint` moves when an item is created,
/// revised, retracted or resolved, never for telemetry). A store whose
/// projection is not current takes the full read, and writes no cache.
pub fn rules(db: &Path, store: &thor_core::event_store::EventStore) -> Vec<LiveItem> {
    let fingerprint = store.heads_projection_current().then(|| store.projected_heads_fingerprint().ok()).flatten();
    // Named after the store file: two stores in one folder never share a cache.
    let stem = db.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let path = db.parent().unwrap_or_else(|| Path::new(".")).join(format!("turn-violation-rules.{stem}.json"));
    if let Some(fp) = fingerprint {
        let cached: Option<RuleCache> = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok());
        if let Some(cache) = cached.filter(|c| c.fingerprint == fp) {
            return cache.items.into_iter().map(|item| LiveItem { id: item.id.clone(), item }).collect();
        }
    }
    let kept: Vec<LiveItem> = crate::live::live_items(store)
        .into_iter()
        .filter(|li| li.item.kind.can_fire() && li.item.check.as_ref().is_some_and(is_file_check))
        .collect();
    if let Some(fingerprint) = fingerprint {
        let cache = RuleCache { fingerprint, items: kept.iter().map(|li| li.item.clone()).collect() };
        if let Ok(text) = serde_json::to_string(&cache) {
            let _ = crate::store_dir::write(&path, text);
        }
    }
    kept
}

/// The turn's first change-capable tool call: remember, per rule, whether its check already fails.
pub fn snapshot(db: &Path, session_id: &str, items: &[LiveItem], project: Option<&str>, root: &Path, now: i64) {
    let path = default_path(db);
    let mut all = read(&path);
    let previous = all.remove(session_id);
    let watched = watched(items, project);
    let mut record = SessionRecord {
        at: now,
        root: root.display().to_string(),
        head: head_state(root),
        reflog: reflog(root).map_or(0, |l| l.len()),
        current: true,
        ..Default::default()
    };
    if let Some(previous) = previous {
        record.open = previous.open;
        // A turn that never reached its Stop (interrupted) may have left a
        // violation nobody was told about: carry it over, never lose it - but
        // only when the checkout is the same one in the same state: a pull or
        // a checkout between the turns changed the files, not the session.
        if !previous.stopped && previous.root == record.root && previous.head == record.head {
            for (id, was) in &previous.start {
                let Some((li, check)) = watched.get(id.as_str()) else { continue };
                if !was.start_fails && *check == was.check && run(li, root) == Some(Outcome::Fails) {
                    record.open.entry(id.clone()).or_insert(Open { check: check.clone(), holds: 0, told: false });
                }
            }
        }
    }
    for (id, (li, check)) in &watched {
        let outcome = run(li, root);
        if outcome == Some(Outcome::Holds) {
            record.open.remove(*id); // put right: the count starts over
        }
        record.start.insert((*id).to_string(), Watched { check: check.clone(), start_fails: outcome == Some(Outcome::Fails) });
    }
    // A rule that was revised or retracted is the owner's decision: let go.
    record.open.retain(|id, open| watched.get(id.as_str()).is_some_and(|(_, check)| *check == open.check));
    all.insert(session_id.to_string(), record);
    write(&path, all);
}

/// What the end of a turn found.
#[derive(Debug, Default)]
pub struct Found {
    /// The hold message, when something broken this turn is still broken.
    pub hold: Option<String>,
    /// The one plain line for a violation that stays after the cap.
    pub say: Option<String>,
    /// Every rule id with an open violation: the stale-rule guard skips them.
    pub covered: HashSet<String>,
    /// Ids that just reached the cap, to record that THOR stood aside.
    pub stood_aside: Vec<String>,
}

struct Broken {
    id: String,
    text: String,
    file: String,
    detail: String,
    /// Already failing when this turn began (an earlier, interrupted or held turn).
    earlier: bool,
    /// This is the last hold THOR makes for it (the cap `MAX_HOLDS`).
    last: bool,
}

fn short(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(200) {
        Some((at, _)) => format!("{}...", &flat[..at]),
        None => flat,
    }
}

/// What is wrong in the file, in plain words, for a check that now fails.
fn describe(check: &Check, root: &Path) -> (String, String) {
    let (path, literals): (&str, Vec<&str>) = match check {
        Check::Absent { path, literal } => (path, vec![literal.as_str()]),
        Check::AbsentAll { path, literals } => (path, literals.iter().map(String::as_str).collect()),
        _ => return (String::new(), String::new()),
    };
    let contents = model::check::read_bounded_targets(&root.join(path), true).unwrap_or_default();
    let present = literals.iter().find(|l| contents.iter().any(|c| c.contains(**l))).or(literals.first());
    let literal = present.copied().unwrap_or("");
    (path.to_string(), format!("the text \"{literal}\", which is not allowed there, is now in it"))
}

fn hold_message(broken: &[Broken]) -> String {
    let mut out = String::from(
        "[THOR] A rule is broken: a file now holds text that a rule forbids (a change made by a script, by another \
         conversation or by the owner would look the same):\n",
    );
    for b in broken.iter().take(MAX_LISTED) {
        let when = if b.earlier { "it came in during an earlier, interrupted turn" } else { "it was not there when this turn began" };
        out.push_str(&format!("- rule {}: \"{}\"\n  file: {} - {}; {}\n", b.id, b.text, b.file, b.detail, when));
    }
    if broken.len() > MAX_LISTED {
        out.push_str(&format!("...and {} more not shown.\n", broken.len() - MAX_LISTED));
    }
    out.push_str(
        "What to do: if this turn made that change, undo it so the file no longer contains that text. If this turn did \
         not make it, tell the owner what you found and do not change the file yourself. If the owner asked for it on \
         purpose, ask the owner before revising the rule, and revise it only with a reason (because). Never weaken or \
         retract the rule on your own to get past this. ",
    );
    // THOR holds a violation at most `MAX_HOLDS` times, one per turn end: say which one this is.
    if broken.iter().take(MAX_LISTED).all(|b| b.last) {
        out.push_str("THOR holds a turn for this no more after this one; if it is still broken it says so once and leaves it to the owner.");
    } else {
        out.push_str("THOR checks again at the end of the next turn, also when that turn runs no command.");
    }
    out
}

fn say_message(ids: &[String]) -> String {
    format!(
        "[THOR] The rule broken earlier is still broken ({}), and THOR has already asked twice to undo it, so it will not \
         hold the turn again. Tell the owner plainly that this rule is broken and let the owner decide.",
        ids.join(", ")
    )
}

/// The turn ends: which rules whose check did not fail at the start fail now.
pub fn stop_check(db: &Path, session_id: &str, items: &[LiveItem], project: Option<&str>, root: &Path) -> Found {
    let path = default_path(db);
    let mut all = read(&path);
    let Some(record) = all.get_mut(session_id).filter(|r| r.current || !r.open.is_empty()) else { return Found::default() };
    if record.root != root.display().to_string() {
        return Found::default(); // another checkout than the one the turn started in
    }
    // A turn with no snapshot of its own (it made no change-capable call) only re-asks about
    // the violations still open from an earlier Stop: its `start` belongs to an older turn.
    let own_turn = record.current;
    if checkout_changed(root, &record.head, record.reflog) {
        // A checkout, pull, rebase, merge or reset moved HEAD (a plain commit
        // does not count): the files changed under the session. Judge nothing
        // this turn and forget its baseline.
        record.start.clear();
        record.open.clear();
        record.stopped = true;
        write(&path, all);
        return Found::default();
    }
    let watched = watched(items, project);
    let mut found = Found::default();
    let mut to_hold: Vec<Broken> = Vec::new();
    let mut standing: Vec<String> = Vec::new();
    let mut ids: Vec<String> = match own_turn {
        true => record.start.keys().chain(record.open.keys()).cloned().collect::<HashSet<_>>().into_iter().collect(),
        false => record.open.keys().cloned().collect(),
    };
    ids.sort();
    for id in ids {
        let Some((li, check_now)) = watched.get(id.as_str()) else { continue };
        let started_ok = own_turn && record.start.get(&id).is_some_and(|w| w.check == *check_now && !w.start_fails);
        let was_open = record.open.get(&id).is_some_and(|o| o.check == *check_now);
        let outcome = run(li, root);
        if outcome != Some(Outcome::Fails) {
            if outcome == Some(Outcome::Holds) {
                record.open.remove(&id);
            }
            continue;
        }
        if !started_ok && !was_open {
            continue; // already failing before the turn: the stale-rule guard's business
        }
        found.covered.insert(id.clone());
        let open = record.open.entry(id.clone()).or_insert(Open { check: check_now.clone(), holds: 0, told: false });
        if open.holds < MAX_HOLDS {
            open.holds += 1;
            let (file, detail) = li.item.check.as_ref().map(|c| describe(c, root)).unwrap_or_default();
            to_hold.push(Broken { id: id.clone(), text: short(&li.item.text), file, detail, earlier: !started_ok, last: open.holds >= MAX_HOLDS });
        } else if !open.told {
            open.told = true;
            standing.push(id.clone());
        }
    }
    record.stopped = true;
    write(&path, all);
    if !to_hold.is_empty() {
        found.hold = Some(hold_message(&to_hold));
    } else if !standing.is_empty() {
        found.say = Some(say_message(&standing));
    }
    found.stood_aside = standing;
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hold_message_names_the_rule_the_file_and_the_honest_way_out() {
        let msg = hold_message(&[Broken {
            id: "no-pa6".to_string(),
            text: "never name PA6".to_string(),
            file: "site/materialen.html".to_string(),
            detail: "the text \"PA6\", which is not allowed there, is now in it".to_string(),
            earlier: false,
            last: false,
        }]);
        assert!(msg.contains("no-pa6") && msg.contains("site/materialen.html") && msg.contains("\"PA6\""), "{msg}");
        assert!(msg.contains("undo it so the file no longer contains that text"), "{msg}");
        assert!(msg.contains("tell the owner what you found") && msg.contains("ask the owner before revising"), "{msg}");
        assert!(msg.contains("Never weaken or retract"), "{msg}");
    }

    fn live(id: &str, check: Check) -> LiveItem {
        LiveItem {
            id: id.to_string(),
            item: model::item::Item {
                id: id.to_string(),
                kind: model::item::Kind::Rule,
                text: "t".to_string(),
                bindings: vec![model::item::Binding::Always],
                severity: Some(model::item::Severity::HouseStyle),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some("f".to_string()),
                check: Some(check),
            },
        }
    }

    #[test]
    fn the_cap_picks_among_forbidden_text_rules_only() {
        let mut items: Vec<LiveItem> = Vec::new();
        // Contains rules sort first by id and must not use up the cap.
        for i in 0..250 {
            items.push(live(&format!("a-contains-{i:03}"), Check::Contains { path: "f".into(), literal: "x".into() }));
        }
        for i in 0..205 {
            items.push(live(&format!("b-absent-{i:03}"), Check::Absent { path: "f".into(), literal: "x".into() }));
        }
        let picked = watched(&items, None);
        assert_eq!(picked.len(), MAX_WATCHED);
        assert!(picked.keys().all(|id| id.starts_with("b-absent-")), "only forbidden-text rules are watched");
        assert!(picked.contains_key("b-absent-000") && !picked.contains_key("b-absent-204"), "stable order by id");
    }

    #[test]
    fn a_long_rule_text_is_cut_not_dumped() {
        assert!(short(&"word ".repeat(200)).chars().count() <= 203);
    }
}
