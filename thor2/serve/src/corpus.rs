//! THE REAL-CALL CORPUS: the tool calls and the replies THOR has seen in the
//! last 30 days, kept in two sidecars beside the store so that a rule can be
//! tried against what really happened (`sweep`) instead of against the few
//! forms its writer thought of.
//!
//! WHY IT EXISTS. Two one-off sweeps on 2026-10-09 replayed 146,000 real calls
//! through the guard and 6,600 real replies through the Response Guard. They
//! found 12 rules that refused calls which were fine and 21 answer-guard
//! triggers that were false - none of which a review of reports had shown. A
//! check that has to be run by hand is a check that is not run. This module
//! is the half that keeps the material by itself; `sweep` is the half that
//! tries the rules against it.
//!
//! WHAT IS KEPT. `real-calls.jsonl`, one line per tool call: the tool, its
//! input (every text redacted the way the log redacts a command,
//! `deliver::redact_secrets`, and cut at `STRING_CLIP` characters), the folder
//! and project it ran in, whether THOR's hook refused it and under which
//! rules, whether another hook did, and whether the call ran and succeeded.
//! `real-replies.jsonl`, one line per reply the Response Guard judged: the
//! reply, the owner's prompt, the turn's tool-call list (`respond::
//! turn_commands`) and what the guard said at the time.
//!
//! WHEN IT IS WRITTEN. At every Stop (`record_session`), from the session's
//! own transcript files - the ones THOR already reads there (`attest::
//! session_transcripts`) - and only what is new since the last Stop: a cursor
//! per transcript file (`real-calls-cursor.json`) remembers how far it was
//! read, so a long session is never read twice. The first read of a file is
//! bounded to its last `FIRST_READ_BYTES`, and one Stop spends at most
//! `STOP_BUDGET_MS` on it; what it did not reach is read at the next Stop.
//! `backfill` is the same reader over every transcript of the last 30 days,
//! for a machine that starts with an empty corpus.
//!
//! HOW IT STAYS SMALL. A line older than `KEEP_DAYS` days goes, and when a file
//! is over its cap the oldest lines go until it is under (`prune`). A write
//! that races another Stop's can leave one call twice; a reader keeps the first
//! (`load_calls`). A call whose result arrives after the line that holds it
//! was written is a second, short line (`k:"r"`) that patches the first.
//!
//! BEST EFFORT, LIKE EVERY SIDECAR: a line that cannot be written is lost,
//! never an error for the hook, and a held store directory (`--no-record`)
//! is left exactly as it was (`store_dir::is_held`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The calls sidecar, beside the store.
pub const CALLS_FILE: &str = "real-calls.jsonl";
/// The replies sidecar, beside the store.
pub const REPLIES_FILE: &str = "real-replies.jsonl";
/// How far each transcript file was read.
pub const CURSOR_FILE: &str = "real-calls-cursor.json";

/// How long a line is kept.
pub const KEEP_DAYS: i64 = 30;
/// The size each file is cut back to (its oldest lines go) once it is over.
///
/// Thirty days of a heavy machine (150,000 calls) come to about 130 MB at the
/// clips below, so 144 MB keeps all of them; a lighter machine keeps far less.
pub const CALLS_CAP_BYTES: u64 = 144 * 1024 * 1024;
pub const REPLIES_CAP_BYTES: u64 = 32 * 1024 * 1024;

/// A text in the input of a call a guard reads (a shell command, a file
/// write, an agent's brief) is cut here (and the call marked `cut`), so one
/// pasted file cannot fill the corpus.
pub const STRING_CLIP: usize = 6_000;
/// A text in the input of any other tool is cut much shorter: no rule reads a
/// page the call asked for, only the few words that name it.
pub const OTHER_TOOL_CLIP: usize = 1_500;

/// How much of each text of `tool`'s input is kept.
fn clip_for(tool: &str) -> usize {
    let reads_text = matches!(tool, "Bash" | "PowerShell" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "Agent" | "Task" | "Workflow")
        || tool.starts_with("mcp__thor__");
    if reads_text {
        STRING_CLIP
    } else {
        OTHER_TOOL_CLIP
    }
}
/// A whole line over this is not one a call can come from (an image, a dump).
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
/// A call whose cut input is still over this is kept without its input.
const MAX_ROW_BYTES: usize = 48 * 1024;
/// The first read of a transcript file reaches back this far at most.
pub const FIRST_READ_BYTES: u64 = 8 * 1024 * 1024;
/// What one Stop spends reading transcripts for the corpus.
pub const STOP_BUDGET_MS: u64 = 1_000;

/// A reply is cut here (and marked `cut`; a cut reply is never replayed).
pub const REPLY_CLIP: usize = 24_000;
/// The owner's prompt, cut.
const PROMPT_CLIP: usize = 3_000;
/// One command of the turn's list, cut; and how many are kept.
const COMMAND_CLIP: usize = 500;
const COMMANDS_KEPT: usize = 200;
/// What the guard said at the time, cut.
const WHY_CLIP: usize = 700;

pub fn calls_path(db: &Path) -> PathBuf {
    sidecar(db, CALLS_FILE)
}

pub fn replies_path(db: &Path) -> PathBuf {
    sidecar(db, REPLIES_FILE)
}

fn cursor_path(db: &Path) -> PathBuf {
    sidecar(db, CURSOR_FILE)
}

fn sidecar(db: &Path, name: &str) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(name)
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One tool call (`k:"c"`), or the late result of one (`k:"r"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallRow {
    /// "c" a call, "r" a result that arrived after the call's own line.
    pub k: String,
    /// The tool call's id, as Claude Code names it.
    pub id: String,
    /// When it was made, unix seconds.
    #[serde(default)]
    pub t: i64,
    /// The first 8 characters of the session id.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub s: String,
    /// Made by a subagent.
    #[serde(default, skip_serializing_if = "is_false")]
    pub a: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    /// The folder the call ran in.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    /// The project the call ran in: the folder's own, else the one the session
    /// last worked in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<String>,
    /// The name of the transcript's project folder, kept only when no project
    /// was found: a replay then has something to guess from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<String>,
    /// The tool input, redacted and cut.
    #[serde(default, skip_serializing_if = "Value::is_null", rename = "in")]
    pub input: Value,
    /// The rules THOR's hook refused it under.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub r: Vec<String>,
    /// Another hook refused it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub oth: bool,
    /// The call ran and succeeded (`true`), or did not (`false`); unknown when
    /// no result has been seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// A hook attached context to the call: a hook was alive in its session.
    #[serde(default, skip_serializing_if = "is_false")]
    pub h: bool,
    /// Some text in the input was cut.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cut: bool,
    /// The build of THOR that recorded it at a Stop (`BUILD_ID`); empty for a
    /// call read back from a transcript, whose engine is unknown. The hook's
    /// answer to a call is only comparable with a replay by the same build.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub b: String,
}

/// One reply the Response Guard judged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplyRow {
    pub t: i64,
    /// The first 8 characters of the session id.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub s: String,
    /// A subagent's reply.
    #[serde(default, skip_serializing_if = "is_false")]
    pub a: bool,
    /// The owner reads this agent's answer himself (`owner_reads_answer`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub o: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    pub reply: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    /// `respond::turn_commands`: the commands the turn ran and a `[tool] Name`
    /// marker for every other tool that looked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cmds: Vec<String>,
    /// The guard held the turn for this reply.
    #[serde(default, skip_serializing_if = "is_false")]
    pub held: bool,
    /// The guard warned about it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub warn: bool,
    /// What the guard said, cut.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// The reply was cut at `REPLY_CLIP`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cut: bool,
    /// The build of THOR that judged it (`BUILD_ID`); empty for a reply read
    /// back from a transcript.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub b: String,
}

fn clip(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_string(), false);
    }
    (text.chars().take(max_chars).collect(), true)
}

/// `value` with every text redacted as the log redacts a command and cut at
/// `STRING_CLIP`; whether anything was cut.
fn scrubbed(value: &Value, limit: usize, cut: &mut bool) -> Value {
    match value {
        Value::String(s) => {
            let (text, was_cut) = clip(s, limit);
            *cut |= was_cut;
            Value::String(crate::deliver::redact_secrets(&text))
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| scrubbed(v, limit, cut)).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), scrubbed(v, limit, cut))).collect()),
        other => other.clone(),
    }
}

/// The reply row for one reply the guard judged. `reply`, `prompt` and
/// `commands` are what the Stop already read; nothing is read again here.
pub fn reply_row(
    t: i64,
    session: &str,
    subagent: bool,
    owner_reads: bool,
    cwd: &str,
    reply: &str,
    prompt: &str,
    commands: &[String],
    held: bool,
    warn: bool,
    why: &str,
) -> ReplyRow {
    let (reply, cut) = clip(reply, REPLY_CLIP);
    ReplyRow {
        t,
        s: session.chars().take(8).collect(),
        a: subagent,
        o: owner_reads,
        cwd: cwd.to_string(),
        reply: crate::deliver::redact_secrets(&reply),
        prompt: crate::deliver::redact_secrets(&clip(prompt, PROMPT_CLIP).0),
        cmds: commands
            .iter()
            .take(COMMANDS_KEPT)
            .map(|c| crate::deliver::redact_secrets(&clip(c, COMMAND_CLIP).0))
            .collect(),
        held,
        warn,
        why: clip(why, WHY_CLIP).0,
        cut,
        b: crate::BUILD_ID.to_string(),
    }
}

/// Append one reply to the replies sidecar. Silent on any failure.
pub fn record_reply(db: &Path, row: &ReplyRow) {
    let Ok(line) = serde_json::to_string(row) else { return };
    append(&replies_path(db), &[line], REPLIES_CAP_BYTES, row.t);
}

// ------------------------------------------------------------ the files

/// Append whole lines to `path`; when the file is then over its cap, cut it
/// back (`prune_file`). Best effort. A held store directory takes no write.
fn append(path: &Path, lines: &[String], cap: u64, now: i64) {
    if lines.is_empty() || crate::store_dir::is_held(path) {
        return;
    }
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else { return };
    if file.write_all(text.as_bytes()).is_err() {
        return;
    }
    // Pruning here is the safety net only (a file a long way over its cap);
    // the sweep's own pass keeps the files to their cap in the ordinary way.
    if file.metadata().map(|m| m.len() > cap + cap / 4).unwrap_or(false) {
        drop(file);
        prune_file(path, cap, now);
    }
}

/// The time a corpus line carries (`t`), `None` for a line that is not one.
fn line_time(line: &str) -> Option<i64> {
    #[derive(Deserialize)]
    struct T {
        t: i64,
    }
    serde_json::from_str::<T>(line).ok().map(|t| t.t)
}

/// The id of a call line (`{"k":"c","id":"..."`), read off its start.
fn call_id_of(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("{\"k\":\"c\",\"id\":\"")?;
    rest.split_once('"').map(|(id, _)| id)
}

/// Cut `path` back: lines older than `KEEP_DAYS` go, then the oldest lines
/// until the file is at most 90 percent of `cap`. Written whole to a new file
/// and moved over, so a reader never sees half a file.
pub fn prune_file(path: &Path, cap: u64, now: i64) {
    if crate::store_dir::is_held(path) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let from = now - KEEP_DAYS * 86_400;
    // A call that two transcripts both hold (a resumed session copies the
    // lines before it) is kept once.
    let mut seen: HashSet<&str> = HashSet::new();
    let mut kept: Vec<&str> = text
        .lines()
        .filter(|l| !l.is_empty() && line_time(l).is_some_and(|t| t >= from))
        .filter(|l| call_id_of(l).is_none_or(|id| seen.insert(id)))
        .collect();
    let target = (cap / 10 * 9) as usize;
    let mut size: usize = kept.iter().map(|l| l.len() + 1).sum();
    let mut drop_front = 0;
    while size > target && drop_front < kept.len() {
        size -= kept[drop_front].len() + 1;
        drop_front += 1;
    }
    kept.drain(..drop_front);
    let mut out = String::with_capacity(size);
    for line in &kept {
        out.push_str(line);
        out.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    if std::fs::write(&tmp, out).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Keep both files to their window and cap (the sweep calls this).
pub fn prune(db: &Path, now: i64) {
    prune_file(&calls_path(db), CALLS_CAP_BYTES, now);
    prune_file(&replies_path(db), REPLIES_CAP_BYTES, now);
}

/// Every call of the corpus, oldest first, each with the late results already
/// applied, a call written twice kept once. Calls older than `KEEP_DAYS` are
/// left out.
pub fn load_calls(db: &Path, now: i64) -> Vec<CallRow> {
    let mut out = Vec::new();
    for_each_call(db, now, |row| {
        out.push(row);
        true
    });
    out
}

/// Stream the calls of the corpus in file order, with late results applied.
/// `visit` returns `false` to stop. A line that does not read is skipped.
pub fn for_each_call(db: &Path, now: i64, visit: impl FnMut(CallRow) -> bool) {
    for_each_call_after(db, now, None, visit);
}

/// What `for_each_call_after` passed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scanned {
    /// The call `after` named was found (or none was named).
    pub found: bool,
    /// The calls passed over without being read, `after` included.
    pub skipped: u64,
}

/// `for_each_call` for the rows after the call `after` names: every row up to
/// and including it is passed over without being parsed, which is what keeps a
/// sweep that has done most of the corpus cheap. When `after` is named and is
/// not in the file (it was cut away), nothing is visited and `found` is false.
pub fn for_each_call_after(db: &Path, now: i64, after: Option<&str>, visit: impl FnMut(CallRow) -> bool) -> Scanned {
    for_each_call_patched(db, now, &patches_of(&calls_path(db)), after, visit)
}

/// The calls of the last `KEEP_DAYS` days whose raw line `wanted` says yes to,
/// and how many calls the corpus holds in all - in ONE pass over the file, and
/// the others are not even parsed. For a reader that knows a cheap necessary
/// condition (the write gate's note on a rule bound to a command). Late results
/// are applied; a call written twice is kept once. At most the `limit` newest
/// are kept (the third answer says whether older ones were dropped), so a rule
/// that can reach any call cannot hold the whole corpus in memory.
pub fn select_calls(db: &Path, now: i64, limit: usize, wanted: &dyn Fn(&str) -> bool) -> (Vec<CallRow>, u64, bool) {
    let from = now - KEEP_DAYS * 86_400;
    let Ok(file) = std::fs::File::open(calls_path(db)) else { return (Vec::new(), 0, false) };
    let mut total = 0u64;
    let mut dropped = false;
    let mut picked: std::collections::VecDeque<CallRow> = std::collections::VecDeque::new();
    let mut patches: HashMap<String, CallRow> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        if line.starts_with("{\"k\":\"r\"") {
            if let Ok(row) = serde_json::from_str::<CallRow>(&line) {
                match patches.get_mut(&row.id) {
                    Some(known) => apply(known, &row),
                    None => {
                        patches.insert(row.id.clone(), row);
                    }
                }
            }
            continue;
        }
        if !line.starts_with("{\"k\":\"c\"") {
            continue;
        }
        if time_of_line(&line).is_none_or(|t| t >= from) {
            total += 1;
        }
        if !wanted(&line) {
            continue;
        }
        let Ok(row) = serde_json::from_str::<CallRow>(&line) else { continue };
        if row.t >= from && seen.insert(row.id.clone()) {
            picked.push_back(row);
            if picked.len() > limit {
                picked.pop_front();
                dropped = true;
            }
        }
    }
    let mut picked: Vec<CallRow> = picked.into();
    for row in &mut picked {
        if let Some(patch) = patches.get(&row.id) {
            apply(row, patch);
        }
    }
    (picked, total, dropped)
}

/// The `t` of a call line, read off its start without parsing the line.
fn time_of_line(line: &str) -> Option<i64> {
    let (_, rest) = line.split_once(",\"t\":")?;
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// How many calls of the last `KEEP_DAYS` days the corpus file holds (a call
/// written twice counts twice). The time is read off the line, not parsed.
pub fn count_calls(db: &Path, now: i64) -> u64 {
    let Ok(file) = std::fs::File::open(calls_path(db)) else { return 0 };
    let from = now - KEEP_DAYS * 86_400;
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| l.starts_with("{\"k\":\"c\""))
        .filter(|l| time_of_line(l).is_none_or(|t| t >= from))
        .count() as u64
}

/// The late results of the corpus, by call id - read once by a caller that
/// reads the file in several rounds.
pub fn load_patches(db: &Path) -> HashMap<String, CallRow> {
    patches_of(&calls_path(db))
}

/// `for_each_call_after`, with the late results already read.
pub fn for_each_call_patched(
    db: &Path,
    now: i64,
    patches: &HashMap<String, CallRow>,
    after: Option<&str>,
    mut visit: impl FnMut(CallRow) -> bool,
) -> Scanned {
    let path = calls_path(db);
    let from = now - KEEP_DAYS * 86_400;
    let mut scanned = Scanned { found: after.is_none(), skipped: 0 };
    let Ok(file) = std::fs::File::open(&path) else { return scanned };
    let needle = after.map(|id| format!("{{\"k\":\"c\",\"id\":{}", serde_json::Value::String(id.to_string())));
    let mut seen: HashSet<String> = HashSet::new();
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        if !line.starts_with("{\"k\":\"c\"") {
            continue;
        }
        if !scanned.found {
            scanned.skipped += 1;
            scanned.found = needle.as_deref().is_some_and(|n| line.starts_with(n) && line[n.len()..].starts_with(','));
            continue;
        }
        let Ok(mut row) = serde_json::from_str::<CallRow>(&line) else { continue };
        if row.t < from || !seen.insert(row.id.clone()) {
            continue;
        }
        if let Some(patch) = patches.get(&row.id) {
            apply(&mut row, patch);
        }
        if !visit(row) {
            return scanned;
        }
    }
    scanned
}

/// The late results of a calls file, by call id; several for one id are folded.
fn patches_of(path: &Path) -> HashMap<String, CallRow> {
    let mut out: HashMap<String, CallRow> = HashMap::new();
    let Ok(file) = std::fs::File::open(path) else { return out };
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        if !line.starts_with("{\"k\":\"r\"") {
            continue;
        }
        let Ok(row) = serde_json::from_str::<CallRow>(&line) else { continue };
        match out.get_mut(&row.id) {
            Some(known) => apply(known, &row),
            None => {
                out.insert(row.id.clone(), row);
            }
        }
    }
    out
}

/// Put a late result onto a call (or onto an earlier late result).
fn apply(row: &mut CallRow, patch: &CallRow) {
    if patch.ok.is_some() {
        row.ok = patch.ok;
    }
    for rule in &patch.r {
        if !row.r.contains(rule) {
            row.r.push(rule.clone());
        }
    }
    row.oth |= patch.oth;
    row.h |= patch.h;
}

/// Every reply of the corpus, oldest first, within `KEEP_DAYS`.
pub fn load_replies(db: &Path, now: i64) -> Vec<ReplyRow> {
    let from = now - KEEP_DAYS * 86_400;
    let Ok(file) = std::fs::File::open(replies_path(db)) else { return Vec::new() };
    // A reply that a live Stop recorded and a back-fill read again is one
    // reply: the same session, the same text, within two minutes.
    let mut seen: Vec<(String, u64, i64)> = Vec::new();
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<ReplyRow>(&l).ok())
        .filter(|r| r.t >= from)
        .filter(|r| {
            let digest = r.reply.bytes().fold(14_695_981_039_346_656_037u64, |h, b| (h ^ u64::from(b)).wrapping_mul(1_099_511_628_211));
            let twin = seen.iter().any(|(s, d, t)| *s == r.s && *d == digest && (t - r.t).abs() <= 120);
            if !twin {
                seen.push((r.s.clone(), digest, r.t));
            }
            !twin
        })
        .collect()
}

// ------------------------------------------------------- reading a transcript

/// Where one transcript file was read to, and the project the line before the
/// next one belonged to (a line of a folder that resolves to none keeps the
/// project of the line before it, as the work clock does).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileCursor {
    pub off: u64,
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Cursor {
    #[serde(default)]
    files: BTreeMap<String, FileCursor>,
}

fn load_cursor(db: &Path) -> Cursor {
    std::fs::read_to_string(cursor_path(db)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_cursor(db: &Path, cursor: &Cursor, now: i64) {
    let path = cursor_path(db);
    if crate::store_dir::is_held(&path) {
        return;
    }
    let from = now - (KEEP_DAYS + 1) * 86_400;
    let kept = Cursor { files: cursor.files.iter().filter(|(_, c)| c.at >= from).map(|(k, v)| (k.clone(), v.clone())).collect() };
    if let Ok(text) = serde_json::to_string(&kept) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// What a reader needs to know about the file it reads.
pub struct FileCtx<'a> {
    /// The first 8 characters of the session id, when the file's name gives it.
    pub session: String,
    /// Made by a subagent (an `agent-<id>.jsonl` file, or under `subagents`).
    pub agent: bool,
    /// The name of the transcript's project folder.
    pub folder: String,
    /// The project of a folder (`project::resolve_project`, or a test's).
    pub project_of: &'a dyn Fn(&Path) -> Option<String>,
    /// The project to fall back on when a line's folder has none and no line
    /// before it had.
    pub fallback: Option<String>,
    /// The build to stamp the calls with: the running one at a Stop, empty
    /// for a back-fill.
    pub build: String,
}

/// What reading one slice of a transcript gave.
#[derive(Debug, Default)]
pub struct Slice {
    pub calls: Vec<CallRow>,
    /// Results (and hook attachments) whose call is not in this slice.
    pub patches: Vec<CallRow>,
    /// Where the next read starts.
    pub end: u64,
    pub project: Option<String>,
    /// The whole available file was read (no time ran out).
    pub complete: bool,
}

/// The first 8 characters of a session id found in a transcript file's path:
/// the file's own stem, or - for a subagent's file - the folder of its session.
fn session_of_path(file: &Path) -> String {
    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    if crate::attest::is_uuid_shaped(stem) {
        return stem.chars().take(8).collect();
    }
    file.ancestors()
        .skip(1)
        .filter_map(|a| a.file_name().and_then(|n| n.to_str()))
        .find(|n| crate::attest::is_uuid_shaped(n))
        .map(|n| n.chars().take(8).collect())
        .unwrap_or_default()
}

fn is_agent_file(file: &Path) -> bool {
    file.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.starts_with("agent-"))
        || file.components().any(|c| c.as_os_str() == "subagents")
}

fn text_of_result(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => {
            blocks.iter().filter_map(|b| b.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join("\n")
        }
        _ => String::new(),
    }
}

/// The rules a hook refusal names, `[THOR] rule <id>` once per rule, and
/// whether the result is a hook refusal at all.
fn refusal_of(text: &str) -> (Vec<String>, bool) {
    let head: String = text.trim_start().chars().take(120).collect();
    if !(head.starts_with("PreToolUse:") && head.contains("hook error")) {
        return (Vec::new(), false);
    }
    let mut rules: Vec<String> = Vec::new();
    for paragraph in text.split("\n\n") {
        let paragraph = paragraph.trim_start();
        let paragraph = paragraph.split_once("hook error:").map_or(paragraph, |(_, rest)| rest.trim_start());
        if let Some(id) = crate::absent_guard::rule_id_of(paragraph) {
            if !rules.iter().any(|r| r == id) {
                rules.push(id.to_string());
            }
        }
    }
    (rules, true)
}

/// Read `file` from `from` (at most `max_bytes` of it when it is read for the
/// first time) into calls. Only complete lines are consumed: a line still
/// being written is left for the next read.
pub fn read_slice(
    file: &Path,
    from: u64,
    max_bytes: u64,
    deadline: Option<Instant>,
    ctx: &FileCtx,
    start_project: Option<String>,
) -> std::io::Result<Slice> {
    let mut handle = std::fs::File::open(file)?;
    let len = handle.metadata()?.len();
    let mut start = if from > len { 0 } else { from };
    let mut skip_partial = false;
    if start == 0 && len > max_bytes {
        start = len - max_bytes;
        skip_partial = true;
    }
    handle.seek(SeekFrom::Start(start))?;
    let mut reader = std::io::BufReader::with_capacity(256 * 1024, handle);
    let mut consumed = start;
    let mut buf: Vec<u8> = Vec::new();
    if skip_partial {
        let n = reader.read_until(b'\n', &mut buf)?;
        consumed += n as u64;
        buf.clear();
    }
    let mut slice = Slice { project: start_project, complete: true, ..Default::default() };
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut folders: HashMap<String, Option<String>> = HashMap::new();
    let mut lines = 0u32;
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        if buf.last() != Some(&b'\n') {
            break; // a line still being written
        }
        lines += 1;
        // The time ran out: this line was read but not used, so it is not
        // consumed - the next read starts at it.
        if lines % 256 == 0 && deadline.is_some_and(|d| Instant::now() >= d) {
            slice.complete = false;
            break;
        }
        consumed += n as u64;
        if n > MAX_LINE_BYTES {
            continue;
        }
        let line = String::from_utf8_lossy(&buf);
        if !(line.contains("\"tool_use\"") || line.contains("\"tool_result\"") || line.contains("hook_additional_context")) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let t = v.get("timestamp").and_then(|t| t.as_str()).and_then(crate::eval_filing::unix_of_timestamp).unwrap_or(0);
        let cwd = v.get("cwd").and_then(|c| c.as_str()).unwrap_or_default().to_string();
        if !cwd.is_empty() {
            let found = folders
                .entry(cwd.clone())
                .or_insert_with(|| (ctx.project_of)(&nearest_existing(Path::new(&cwd)).unwrap_or_else(|| PathBuf::from(&cwd))))
                .clone();
            if found.is_some() {
                slice.project = found;
            }
        }
        if let Some(attachment) = v.get("attachment") {
            let of = attachment.get("toolUseID").and_then(|t| t.as_str()).unwrap_or_default();
            if !of.is_empty()
                && attachment.get("type").and_then(|t| t.as_str()) == Some("hook_additional_context")
                && attachment.get("hookEvent").and_then(|t| t.as_str()) == Some("PreToolUse")
            {
                match index.get(of) {
                    Some(&i) => slice.calls[i].h = true,
                    None => slice.patches.push(CallRow { h: true, ..patch_row(of, t) }),
                }
            }
        }
        let Some(blocks) = v.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) else { continue };
        for block in blocks {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("tool_use") => {
                    let Some(id) = block.get("id").and_then(|i| i.as_str()) else { continue };
                    let tool = block.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
                    let mut cut = false;
                    let mut input = scrubbed(block.get("input").unwrap_or(&Value::Null), clip_for(&tool), &mut cut);
                    if input.to_string().len() > MAX_ROW_BYTES {
                        input = Value::Null;
                        cut = true;
                    }
                    let project = slice.project.clone().or_else(|| ctx.fallback.clone());
                    let row = CallRow {
                        k: "c".to_string(),
                        id: id.to_string(),
                        t,
                        s: ctx.session.clone(),
                        a: ctx.agent,
                        tool,
                        d: project.is_none().then(|| ctx.folder.clone()).filter(|f| !f.is_empty()),
                        p: project,
                        cwd: cwd.clone(),
                        input,
                        r: Vec::new(),
                        oth: false,
                        ok: None,
                        h: false,
                        cut,
                        b: ctx.build.clone(),
                    };
                    index.insert(id.to_string(), slice.calls.len());
                    slice.calls.push(row);
                }
                Some("tool_result") => {
                    let Some(id) = block.get("tool_use_id").and_then(|i| i.as_str()) else { continue };
                    let is_error = block.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
                    let (rules, hook) = if is_error { refusal_of(&text_of_result(block.get("content"))) } else { (Vec::new(), false) };
                    let target = match index.get(id) {
                        Some(&i) => &mut slice.calls[i],
                        None => {
                            slice.patches.push(patch_row(id, t));
                            slice.patches.last_mut().expect("just pushed")
                        }
                    };
                    target.ok = Some(!is_error);
                    for rule in rules {
                        if !target.r.contains(&rule) {
                            target.r.push(rule);
                        }
                    }
                    target.oth |= hook && target.r.is_empty();
                }
                _ => {}
            }
        }
    }
    slice.end = consumed;
    Ok(slice)
}

fn patch_row(id: &str, t: i64) -> CallRow {
    CallRow {
        k: "r".to_string(),
        id: id.to_string(),
        t,
        s: String::new(),
        a: false,
        tool: String::new(),
        cwd: String::new(),
        p: None,
        d: None,
        input: Value::Null,
        r: Vec::new(),
        oth: false,
        ok: None,
        h: false,
        cut: false,
        b: String::new(),
    }
}

/// `path`, or the nearest folder above it that exists.
pub fn nearest_existing(path: &Path) -> Option<PathBuf> {
    let mut current = path.to_path_buf();
    loop {
        if current.is_dir() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

// ------------------------------------------------- replies, read back from a transcript

/// What reading the replies of a transcript gave.
#[derive(Debug, Default)]
pub struct RepliesSlice {
    pub replies: Vec<ReplyRow>,
    /// Where the next read starts: the end of the file, or - when a turn is
    /// still open - the start of that turn, so it is read whole next time.
    pub end: u64,
    pub complete: bool,
}

/// The replies a transcript holds, one per Stop the session made
/// (`stop_hook_summary`): the last thing the assistant said in the turn, the
/// owner's prompt that began it, the turn's tool-call list, and what THOR's
/// guard said at the time. For a machine that starts with an empty record: a
/// live Stop records its reply as the guard judges it (`record_reply`), this is
/// only the back-fill. What the guard said is whatever THOR text the Stop
/// hook left in the summary; the transcript does not say whether it held the
/// turn or only warned, so such a reply counts as one THOR spoke about.
pub fn read_replies(file: &Path, from: u64, max_bytes: u64, deadline: Option<Instant>, ctx: &FileCtx) -> std::io::Result<RepliesSlice> {
    let mut handle = std::fs::File::open(file)?;
    let len = handle.metadata()?.len();
    let mut start = if from > len { 0 } else { from };
    let mut skip_partial = false;
    if start == 0 && len > max_bytes {
        start = len - max_bytes;
        skip_partial = true;
    }
    handle.seek(SeekFrom::Start(start))?;
    let mut reader = std::io::BufReader::with_capacity(256 * 1024, handle);
    let mut buf: Vec<u8> = Vec::new();
    let mut at = start;
    if skip_partial {
        at += reader.read_until(b'\n', &mut buf)? as u64;
    }
    let mut out = RepliesSlice { complete: true, ..Default::default() };
    // The turn so far: its first line (the prompt) and every line that holds a
    // tool call, as the Response Guard reads a transcript.
    let mut turn: Vec<String> = Vec::new();
    let mut turn_start: Option<u64> = None;
    let mut message_id: Option<String> = None;
    let mut message_text: Vec<String> = Vec::new();
    let mut consumed = at;
    let mut lines = 0u32;
    loop {
        buf.clear();
        let line_start = consumed;
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            break;
        }
        consumed += n as u64;
        lines += 1;
        if lines % 256 == 0 && deadline.is_some_and(|d| Instant::now() >= d) {
            out.complete = false;
            consumed = turn_start.unwrap_or(line_start);
            break;
        }
        if n > MAX_LINE_BYTES {
            continue;
        }
        let line = String::from_utf8_lossy(&buf);
        let wants = line.contains("\"type\":\"user\"")
            || line.contains("\"type\":\"assistant\"")
            || line.contains("\"stop_hook_summary\"");
        if !wants {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("user") => {
                if crate::respond::owner_prompt_of(&v).is_some() {
                    turn = vec![line.trim_end().to_string()];
                    turn_start = Some(line_start);
                    message_id = None;
                    message_text.clear();
                }
            }
            Some("assistant") => {
                let message = v.get("message");
                let id = message.and_then(|m| m.get("id")).and_then(|i| i.as_str()).or_else(|| v.get("uuid").and_then(|u| u.as_str()));
                let Some(blocks) = message.and_then(|m| m.get("content")).and_then(|c| c.as_array()) else { continue };
                if id.map(str::to_string) != message_id {
                    message_id = id.map(str::to_string);
                    message_text.clear();
                }
                for block in blocks {
                    if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                            message_text.push(t.to_string());
                        }
                    }
                }
                if line.contains("\"tool_use\"") && !turn.is_empty() {
                    turn.push(line.trim_end().to_string());
                }
            }
            _ if v.get("subtype").and_then(|s| s.as_str()) == Some("stop_hook_summary") => {
                let reply = message_text.join("\n");
                if !reply.trim().is_empty() && !turn.is_empty() {
                    let joined = turn.join("\n");
                    let prompt = crate::respond::last_user_prompt(&joined).unwrap_or_default();
                    let commands = crate::respond::turn_commands(&joined);
                    let said: Vec<String> = v
                        .get("hookErrors")
                        .and_then(|e| e.as_array())
                        .map(|all| all.iter().filter_map(|e| e.as_str()).filter(|e| e.contains("[THOR")).map(str::to_string).collect())
                        .unwrap_or_default();
                    let t = v.get("timestamp").and_then(|t| t.as_str()).and_then(crate::eval_filing::unix_of_timestamp).unwrap_or(0);
                    let cwd = v.get("cwd").and_then(|c| c.as_str()).unwrap_or_default();
                    let sidechain = v.get("isSidechain").and_then(|s| s.as_bool()).unwrap_or(false);
                    let mut row = reply_row(t, &ctx.session, ctx.agent || sidechain, false, cwd, &reply, &prompt, &commands, !said.is_empty(), false, &said.join("  ||  "));
                    row.b.clear(); // read back from a transcript: the build that judged it is unknown
                    if !v.get("sessionId").and_then(|s| s.as_str()).unwrap_or_default().is_empty() {
                        row.s = v["sessionId"].as_str().unwrap_or_default().chars().take(8).collect();
                    }
                    out.replies.push(row);
                }
                turn.clear();
                turn_start = None;
                message_text.clear();
                message_id = None;
            }
            _ => {}
        }
    }
    out.end = match turn_start {
        // A turn that has not ended: read from its start next time.
        Some(open) if !turn.is_empty() => open,
        _ => consumed,
    };
    Ok(out)
}

/// Back-fill the replies of `files` into the replies sidecar, resuming each
/// from its cursor. Returns how many replies were recorded.
pub fn backfill_replies(db: &Path, root: &Path, files: &[PathBuf], budget: Duration, now: i64) -> (usize, bool) {
    if crate::store_dir::is_held(&replies_path(db)) {
        return (0, true);
    }
    let deadline = Instant::now() + budget;
    let mut cursor = load_cursor(db);
    let project_of = |dir: &Path| crate::project::resolve_project(dir);
    let mut lines: Vec<String> = Vec::new();
    let mut complete = true;
    let mut changed = false;
    for file in files {
        if Instant::now() >= deadline {
            complete = false;
            break;
        }
        let key = format!("replies:{}", file.to_string_lossy());
        let known = cursor.files.get(&key).cloned().unwrap_or_default();
        let ctx = FileCtx {
            session: session_of_path(file),
            agent: is_agent_file(file),
            folder: folder_name(root, file),
            project_of: &project_of,
            fallback: None,
            build: String::new(),
        };
        let Ok(slice) = read_replies(file, known.off, 256 * 1024 * 1024, Some(deadline), &ctx) else { continue };
        complete &= slice.complete;
        for row in &slice.replies {
            if let Ok(line) = serde_json::to_string(row) {
                lines.push(line);
            }
        }
        changed |= slice.end != known.off || known.at == 0;
        cursor.files.insert(key, FileCursor { off: slice.end, at: now, project: None });
    }
    let count = lines.len();
    append(&replies_path(db), &lines, REPLIES_CAP_BYTES, now);
    if changed {
        save_cursor(db, &cursor, now);
    }
    (count, complete)
}

// --------------------------------------------------------------- recording

/// What a recording pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Recorded {
    pub files: usize,
    pub calls: usize,
    /// Every file was read to its end.
    pub complete: bool,
}

fn folder_name(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .ok()
        .and_then(|rest| rest.components().next())
        .and_then(|c| c.as_os_str().to_str())
        .unwrap_or_default()
        .to_string()
}

/// Record the new tool calls of `files` (transcript files under `root`),
/// resuming each from its cursor, for at most `budget`.
pub fn record_files(
    db: &Path,
    root: &Path,
    files: &[PathBuf],
    fallback_project: Option<&str>,
    build: &str,
    first_read_bytes: u64,
    budget: Duration,
    now: i64,
) -> Recorded {
    let mut recorded = Recorded { complete: true, ..Default::default() };
    if crate::store_dir::is_held(&calls_path(db)) {
        return recorded;
    }
    let deadline = Instant::now() + budget;
    let mut cursor = load_cursor(db);
    let project_of = |dir: &Path| crate::project::resolve_project(dir);
    let mut lines: Vec<String> = Vec::new();
    let mut written: HashSet<String> = HashSet::new();
    // A call older than the window is never written (a back-fill reads files that hold older ones).
    let keep_from = now - KEEP_DAYS * 86_400;
    let mut changed = false;
    for file in files {
        if Instant::now() >= deadline {
            recorded.complete = false;
            break;
        }
        let key = file.to_string_lossy().to_string();
        let known = cursor.files.get(&key).cloned().unwrap_or_default();
        let ctx = FileCtx {
            session: session_of_path(file),
            agent: is_agent_file(file),
            folder: folder_name(root, file),
            project_of: &project_of,
            fallback: fallback_project.map(str::to_string),
            build: build.to_string(),
        };
        let Ok(slice) = read_slice(file, known.off, first_read_bytes, Some(deadline), &ctx, known.project.clone()) else {
            continue;
        };
        recorded.files += 1;
        recorded.complete &= slice.complete;
        // A call two transcripts both hold (a resumed session copies the lines
        // before it) is written once.
        let before = lines.len();
        for row in slice.calls.iter().filter(|c| c.t >= keep_from && written.insert(c.id.clone())).chain(slice.patches.iter()) {
            if let Ok(line) = serde_json::to_string(row) {
                lines.push(line);
            }
        }
        recorded.calls += lines.len() - before;
        if slice.end != known.off || known.at == 0 {
            changed = true;
        }
        cursor.files.insert(key, FileCursor { off: slice.end, at: now, project: slice.project });
    }
    append(&calls_path(db), &lines, CALLS_CAP_BYTES, now);
    if changed {
        save_cursor(db, &cursor, now);
    }
    recorded
}

/// At a Stop: record what the session's transcripts hold that is new. The
/// files are the session's own - its main transcript and every subagent's -
/// found the way the Stop's other readers find them.
pub fn record_session(
    db: &Path,
    session_id: &str,
    named_transcript: Option<&Path>,
    transcripts_root: Option<&Path>,
    fallback_project: Option<&str>,
    now: i64,
) -> Recorded {
    let Some(root) = transcripts_root else { return Recorded { complete: true, ..Default::default() } };
    let mut files = crate::attest::session_transcripts(root, session_id);
    if let Some(named) = named_transcript {
        if named.is_file() && !files.iter().any(|f| f == named) {
            files.push(named.to_path_buf());
        }
    }
    record_files(db, root, &files, fallback_project, crate::BUILD_ID, FIRST_READ_BYTES, Duration::from_millis(STOP_BUDGET_MS), now)
}

/// What a backfill did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Backfilled {
    pub files_total: usize,
    pub files_read: usize,
    pub calls: usize,
    pub replies: usize,
    pub complete: bool,
}

/// Read every transcript of the last `KEEP_DAYS` days under `root` into the
/// corpus, resuming where an earlier backfill or Stop stopped. For a machine
/// that starts with nothing: after it, the corpus holds what the transcripts
/// still hold. At most `budget`; run it again to finish.
pub fn backfill(db: &Path, root: &Path, budget: Duration, now: i64) -> Backfilled {
    let mut files: Vec<PathBuf> = Vec::new();
    let Ok(folders) = std::fs::read_dir(root) else { return Backfilled { complete: true, ..Default::default() } };
    let from = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs((now - KEEP_DAYS * 86_400).max(0) as u64);
    for folder in folders.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let mut found = Vec::new();
        crate::attest::walk_jsonl(&folder, &mut found, 0);
        files.extend(found.into_iter().filter(|f| std::fs::metadata(f).and_then(|m| m.modified()).is_ok_and(|m| m >= from)));
    }
    let mut out = Backfilled { files_total: files.len(), complete: true, ..Default::default() };
    // In slices of 40 files, so the cursor is saved as it goes and a budget
    // that ends mid-way loses nothing.
    let deadline = Instant::now() + budget;
    for chunk in files.chunks(40) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            out.complete = false;
            break;
        }
        let done = record_files(db, root, chunk, None, "", 256 * 1024 * 1024, left, now);
        out.files_read += done.files;
        out.calls += done.calls;
        if !done.complete {
            out.complete = false;
            break;
        }
        let (replies, replies_complete) =
            backfill_replies(db, root, chunk, deadline.saturating_duration_since(Instant::now()), now);
        out.replies += replies;
        if !replies_complete {
            out.complete = false;
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hook_refusal_is_told_from_a_failed_call_and_names_every_rule() {
        let one = "PreToolUse:Bash hook error: [THOR] rule alpha forbids this text (\"x\"); found in what is about to be written: \"y\"";
        assert_eq!(refusal_of(one), (vec!["alpha".to_string()], true));
        let two = "PreToolUse:Agent hook error: [THOR] this call trips 2 rules at once - answer ALL of them in one go, then run it again.\n\n[THOR] rule alpha forbids this\n\n[THOR] rule beta forbids that";
        assert_eq!(refusal_of(two), (vec!["alpha".to_string(), "beta".to_string()], true));
        assert_eq!(refusal_of("Exit code 1\nboom"), (Vec::new(), false));
        let other = "PreToolUse:Bash hook error: precommit gate says no";
        assert_eq!(refusal_of(other), (Vec::new(), true));
    }

    #[test]
    fn a_text_is_cut_and_the_call_is_marked() {
        let mut cut = false;
        let long = "x".repeat(STRING_CLIP + 10);
        let out = scrubbed(&serde_json::json!({"command": long, "n": 3}), STRING_CLIP, &mut cut);
        assert!(cut);
        assert_eq!(out["command"].as_str().unwrap().chars().count(), STRING_CLIP);
        let mut cut = false;
        scrubbed(&serde_json::json!({"command": "ls"}), STRING_CLIP, &mut cut);
        assert!(!cut);
    }
}
