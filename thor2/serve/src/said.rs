//! What the owner said: the part of an evaluation that reads the messages the
//! owner typed since the project's previous evaluation, so the evaluating
//! assistant can store the wishes among them that should still hold later.
//!
//! WHY. A beginner never says "this is a rule". They just talk: "always ask me
//! before you delete anything", "keep the answers short". The assistant has to
//! find such a wish itself, in whatever language it is written, and THOR has no
//! word list to find it with. So THOR hands the evaluation the owner's own
//! messages and holds the report until the assistant has printed them.
//!
//! WHAT THIS MODULE DOES, in the order the evaluation uses it:
//! 1. `freeze` - at the evaluation start, collects the owner-typed prompts of
//!    the project's main sessions in a window (after the project's cursor, or
//!    the last 7 days when it has none) and keeps only the window and the
//!    counts (`model::store::SaidFrozen`). The words themselves are never
//!    written to the log.
//! 2. `render` and `plan` - what `serve said --project <name>` prints: the same
//!    window read again from the transcripts, oldest first, one line each with
//!    its date, capped at `PRINT_CAP_CHARS`; the last line says how many newer
//!    messages wait for the next evaluation.
//! 3. `record_print_evidence` - when the filing is made, THOR reads this
//!    session's own transcript and, if the print command ran after the start,
//!    writes `model::store::SAID_PRINTED_KIND` on the log. The filing gate and
//!    the machine section read that record, never a live transcript.
//! 4. `advance_cursor` - only when a report that counts is filed, the cursor
//!    moves to the last message that was printed, so nothing is dropped.
//!
//! WHAT THIS CANNOT PROVE. THOR can show that the list existed and was printed.
//! It cannot tell whether the assistant read it well or stored the right
//! wishes: that judgement is the assistant's own and is only written down in
//! its own text.
//!
//! A transcript is somebody else's file: one that cannot be read is skipped
//! and counted, never an error.

use model::store::{EvaluationStart, SaidFrozen};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use thor_core::event_store::EventStore;

/// A message shorter than this many characters (after white space is folded
/// to single spaces) is left out: "yes", "go", "ok".
pub const MIN_CHARS: usize = 20;

/// The window of a project that has no cursor yet, in days.
pub const FIRST_WINDOW_DAYS: i64 = 7;

/// `serve said` prints at most about this many characters.
pub const PRINT_CAP_CHARS: usize = 20_000;

/// One message is cut at this many characters.
pub const MESSAGE_CLIP_CHARS: usize = 2_000;

/// Room kept below the cap for the heading and the last line.
const PRINT_RESERVE_CHARS: usize = 400;

/// One message the owner typed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Said {
    pub at_ms: i64,
    pub text: String,
}

/// What `collect_in` found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Collected {
    /// Oldest first; short and repeated messages left out; secrets blanked.
    pub list: Vec<Said>,
    /// Transcript files in the window that could not be read.
    pub unreadable: usize,
}

// ---------------------------------------------------------------- time

/// The time of a transcript timestamp in milliseconds (`2026-03-02T10:00:30.123Z`).
pub fn ms_of_timestamp(at: &str) -> Option<i64> {
    let seconds = crate::eval_filing::unix_of_timestamp(at)?;
    let fraction = at.split_once('.').map(|(_, rest)| rest.chars().take_while(|c| c.is_ascii_digit()).take(3).collect::<String>()).unwrap_or_default();
    let ms = format!("{fraction:0<3}").parse::<i64>().unwrap_or(0);
    Some(seconds * 1000 + ms)
}

pub fn date_of_ms(ms: i64) -> String {
    crate::time::iso8601_from_unix(ms / 1000).chars().take(16).collect::<String>().replace('T', " ")
}

// ------------------------------------------------------------- reading

/// The owner's own text on one transcript line, or `None` when the line is not
/// a message he typed. Built on `attest::owner_text` (a user line whose origin
/// is human, not meta, not a sidechain, not a tool result, not a hook, task
/// notification, cross-session message or compaction summary, quoted and
/// fenced lines taken out) and drops what is left of system text: a reminder or
/// a command wrapper starts with a tag, a compaction summary with its own words.
fn owner_prompt(line: &str) -> Option<String> {
    let text = crate::attest::owner_text(line)?;
    let start = text.trim_start();
    if start.starts_with('<') || start.starts_with("This session is being continued") || start.starts_with("Caveat:") {
        return None;
    }
    Some(text)
}

fn tidy(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let blanked = crate::inventory::redact(&one_line);
    if blanked.chars().count() <= MESSAGE_CLIP_CHARS {
        return blanked;
    }
    let cut: String = blanked.chars().take(MESSAGE_CLIP_CHARS).collect();
    format!("{cut}...")
}

/// The working folders the first lines of a transcript name.
fn head_cwds(file: &Path) -> std::io::Result<Vec<String>> {
    use std::io::BufRead;
    let reader = std::io::BufReader::new(std::fs::File::open(file)?);
    let mut out: Vec<String> = Vec::new();
    for line in reader.split(b'\n').take(200) {
        let line = line?;
        if let Some(cwd) = serde_json::from_slice::<serde_json::Value>(&line).ok().and_then(|v| v.get("cwd").and_then(|c| c.as_str()).map(str::to_string)) {
            if !out.contains(&cwd) {
                out.push(cwd);
            }
        }
    }
    Ok(out)
}

/// The working folders the last `attest::TRANSCRIPT_TAIL_BYTES` of a
/// transcript name, found by their bytes (`"cwd"`, a colon, a string) so a tail
/// of megabytes is never parsed line by line.
fn tail_cwds(file: &Path) -> std::io::Result<Vec<String>> {
    let mut handle = std::fs::File::open(file)?;
    let len = handle.metadata()?.len();
    let span = len.min(crate::attest::TRANSCRIPT_TAIL_BYTES);
    handle.seek(SeekFrom::Start(len - span))?;
    let mut bytes = Vec::with_capacity(span as usize);
    handle.take(span).read_to_end(&mut bytes)?;
    let skip_space = |mut i: usize| {
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        i
    };
    let mut raw_seen: Vec<&[u8]> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for at in memchr::memmem::find_iter(&bytes, b"\"cwd\"") {
        let colon = skip_space(at + 5);
        if bytes.get(colon) != Some(&b':') {
            continue;
        }
        let open = skip_space(colon + 1);
        if bytes.get(open) != Some(&b'"') {
            continue;
        }
        let mut end = open + 1;
        while end < bytes.len() && bytes[end] != b'"' {
            end += if bytes[end] == b'\\' { 2 } else { 1 };
        }
        let Some(raw) = bytes.get(open..=end) else { break };
        if raw_seen.contains(&raw) {
            continue;
        }
        raw_seen.push(raw);
        if let Ok(cwd) = serde_json::from_slice::<String>(raw) {
            if !out.contains(&cwd) {
                out.push(cwd);
            }
        }
    }
    Ok(out)
}

/// Whether the transcript `file` is a session of `project`: a working folder
/// named in its first lines resolves to it, or else one named in its tail -
/// the same tail the work clock reads, so the list follows the work as the
/// ask does. A conversation that began in a folder of no project (the desktop
/// app's temporary one) and moved to a project later names that project only
/// further on: measured 2026-11-27, none of 228 messages of such a session
/// reached its project's list while the first folder alone decided.
fn session_of(file: &Path, project: &str, resolved: &mut HashMap<String, bool>) -> std::io::Result<bool> {
    let mut ours = |cwds: Vec<String>| {
        cwds.into_iter().any(|cwd| {
            *resolved.entry(cwd.clone()).or_insert_with(|| {
                crate::project::resolve_project(Path::new(&cwd)).is_some_and(|p| crate::project::same_project_name(&p, project))
            })
        })
    };
    if ours(head_cwds(file)?) {
        return Ok(true);
    }
    Ok(ours(tail_cwds(file)?))
}

/// The main transcripts under `root`: `<root>/<folder>/<session>.jsonl`, never
/// the helpers' files in the folders below them.
fn main_transcripts(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(folders) = std::fs::read_dir(root) else { return out };
    for folder in folders.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let Ok(files) = std::fs::read_dir(&folder) else { continue };
        out.extend(files.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "jsonl")));
    }
    out.sort();
    out
}

/// THE REFERENCE READER, kept only so a test or the comparison example can prove
/// that `collect_in` gives the same list: it reads every file whole. Never
/// called by THOR itself.
///
/// Every message the owner typed in the main sessions of `project` after
/// `since_ms` and at or before `until_ms`: 20 characters or longer, identical
/// texts once (the oldest), secrets blanked, oldest first. A session belongs to
/// every project a working folder in its first lines or its tail resolves to.
#[doc(hidden)]
pub fn collect_in_full(root: &Path, project: &str, since_ms: i64, until_ms: i64) -> Collected {
    let mut found: Vec<Said> = Vec::new();
    let mut unreadable = 0;
    let mut resolved: HashMap<String, bool> = HashMap::new();
    for file in main_transcripts(root) {
        // A file nobody wrote to since the window opened holds nothing in it.
        let modified = std::fs::metadata(&file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64);
        if modified.is_some_and(|m| m < since_ms) {
            continue;
        }
        match session_of(&file, project, &mut resolved) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(_) => {
                unreadable += 1;
                continue;
            }
        }
        let Ok(bytes) = std::fs::read(&file) else {
            unreadable += 1;
            continue;
        };
        let content = String::from_utf8_lossy(&bytes);
        for line in content.lines() {
            let Some(text) = owner_prompt(line) else { continue };
            let Some(at_ms) = serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| v.get("timestamp").and_then(|t| t.as_str()).and_then(ms_of_timestamp))
            else {
                continue;
            };
            if at_ms <= since_ms || at_ms > until_ms {
                continue;
            }
            let said = tidy(&text);
            if said.chars().count() >= MIN_CHARS {
                found.push(Said { at_ms, text: said });
            }
        }
    }
    found.sort_by(|a, b| a.at_ms.cmp(&b.at_ms).then_with(|| a.text.cmp(&b.text)));
    let mut seen = std::collections::HashSet::new();
    found.retain(|s| seen.insert(s.text.clone()));
    Collected { list: found, unreadable }
}

// ------------------------------------------------- the fast, bounded reader
//
// WHAT IS DIFFERENT FROM `collect_in_full`, AND WHY THE LIST IS THE SAME.
// 1. A file is streamed line by line instead of being read whole and turned
//    into one big string. Lines are cut at the same `\n` (a trailing `\r` goes,
//    as `str::lines` does) and converted with the same lossy rule, so every line
//    is the same text.
// 2. A raw-byte test comes before any JSON parsing: `owner_text` only accepts a
//    line whose `type` is the string "user", which holds the bytes `"user"`, or
//    a message typed while a turn ran, an attachment of type `queued_command`,
//    which holds the bytes `"queued_command"` (the writer, JSON.stringify, never
//    escapes an ASCII letter). A line with neither is skipped unparsed. This is
//    the one assumption.
// 3. The window's upper end is applied when the list is assembled, not per
//    line, so what a file gave can be kept and reused (see 4); the assembly
//    filters, sorts and de-duplicates exactly as `collect_in_full` does, and
//    that result does not depend on the order files or lines were met in.
// 4. A time box. Every Stop that starts an evaluation reads under a deadline.
//    When it runs out, the progress (per file: the byte offset up to the last
//    whole line read, a hash of the first and of the last bytes before it, and
//    the messages found so far) goes to a sidecar file and the evaluation is not
//    started; the next Stop goes on from there. A file that shrank, or whose
//    guarded bytes changed, is read again from its start. The sidecar is only
//    ever used for the same window start and is removed when the list is whole,
//    so no partial list is ever frozen.

/// How long one Stop may spend reading transcripts for the list.
pub const TIME_BOX: Duration = Duration::from_secs(10);

/// How many bytes at the start of a file, and right before the saved offset,
/// are hashed to notice a file that was rewritten instead of appended to.
const GUARD_BYTES: u64 = 4096;

const USER_NEEDLE: &[u8] = b"\"user\"";
const QUEUED_NEEDLE: &[u8] = b"\"queued_command\"";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileProgress {
    /// Bytes read, up to the end of the last whole line.
    pub offset: u64,
    head: u64,
    tail: u64,
    /// Messages of the whole lines read: after the window start, any end, not
    /// yet de-duplicated.
    pub items: Vec<Said>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub since_ms: i64,
    pub files: BTreeMap<String, FileProgress>,
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// The hashes of the first and of the last `GUARD_BYTES` bytes before `offset`.
fn guard(file: &Path, offset: u64) -> Option<(u64, u64)> {
    let mut handle = std::fs::File::open(file).ok()?;
    let span = offset.min(GUARD_BYTES) as usize;
    let mut head = vec![0u8; span];
    handle.read_exact(&mut head).ok()?;
    let mut tail = vec![0u8; span];
    handle.seek(SeekFrom::Start(offset - span as u64)).ok()?;
    handle.read_exact(&mut tail).ok()?;
    Some((fnv(&head), fnv(&tail)))
}

/// One line as a message, when it is one inside the window start. The same
/// steps as `collect_in_full` takes per line, minus the window's end.
fn said_of_line(line: &str, since_ms: i64) -> Option<Said> {
    let text = owner_prompt(line)?;
    let at_ms = serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|v| v.get("timestamp").and_then(|t| t.as_str()).and_then(ms_of_timestamp))?;
    if at_ms <= since_ms {
        return None;
    }
    let said = tidy(&text);
    (said.chars().count() >= MIN_CHARS).then_some(Said { at_ms, text: said })
}

enum FileRead {
    /// All of it read: the progress up to the last whole line, and what a last
    /// line without a line end holds (never kept: the file may still be writing it).
    Done { state: FileProgress, open_line: Vec<Said> },
    /// The deadline came first: the progress so far.
    TimedOut(FileProgress),
    Unreadable,
}

fn read_file(file: &Path, prior: Option<&FileProgress>, since_ms: i64, deadline: Option<Instant>) -> FileRead {
    let Ok(mut handle) = std::fs::File::open(file) else { return FileRead::Unreadable };
    let Ok(len) = handle.metadata().map(|m| m.len()) else { return FileRead::Unreadable };
    let mut state = match prior {
        Some(p) if p.offset <= len && guard(file, p.offset) == Some((p.head, p.tail)) => p.clone(),
        _ => FileProgress::default(),
    };
    if handle.seek(SeekFrom::Start(state.offset)).is_err() {
        return FileRead::Unreadable;
    }
    let finder = memchr::memmem::Finder::new(USER_NEEDLE);
    let queued = memchr::memmem::Finder::new(QUEUED_NEEDLE);
    let mut reader = std::io::BufReader::with_capacity(256 * 1024, handle.take(len - state.offset));
    let mut raw = Vec::new();
    let mut open_line = Vec::new();
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) => return FileRead::Done { state, open_line },
            Ok(_) => {}
            Err(_) => return FileRead::Unreadable,
        }
        let whole = raw.ends_with(b"\n");
        let mut body: &[u8] = &raw;
        if whole {
            body = &body[..body.len() - 1];
        }
        if let Some(rest) = body.strip_suffix(b"\r") {
            body = rest;
        }
        let found = if finder.find(body).is_some() || queued.find(body).is_some() { said_of_line(&String::from_utf8_lossy(body), since_ms) } else { None };
        if whole {
            state.offset += raw.len() as u64;
            state.items.extend(found);
        } else {
            open_line.extend(found);
        }
        // Checked after a line was taken, so every call moves on by at least one.
        if whole && deadline.is_some_and(|d| Instant::now() >= d) {
            return FileRead::TimedOut(state);
        }
    }
}

/// The main transcripts of `project` that can hold a message after `since_ms`
/// (a file nobody wrote to since the window opened holds nothing in it: the
/// window is by message time, and a line is written no later than the file's
/// last modification), and how many files could not be read for their folder.
#[doc(hidden)]
pub fn files_of(root: &Path, project: &str, since_ms: i64) -> (Vec<PathBuf>, usize) {
    let mut out = Vec::new();
    let mut unreadable = 0;
    let mut resolved: HashMap<String, bool> = HashMap::new();
    for file in main_transcripts(root) {
        let modified = std::fs::metadata(&file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64);
        if modified.is_some_and(|m| m < since_ms) {
            continue;
        }
        match session_of(&file, project, &mut resolved) {
            Ok(true) => out.push(file),
            Ok(false) => {}
            Err(_) => unreadable += 1,
        }
    }
    (out, unreadable)
}

/// Every message the owner typed in the main sessions of `project` after
/// `since_ms` and at or before `until_ms`: 20 characters or longer, identical
/// texts once (the oldest), secrets blanked, oldest first. A session belongs to
/// every project a working folder in its first lines or its tail resolves to
/// (`session_of`). Reads under `deadline`
/// and goes on from `progress`; `None` when the deadline came first (the
/// progress then holds what was read, and nothing of the list may be used).
pub fn collect_resumable(root: &Path, project: &str, since_ms: i64, until_ms: i64, deadline: Option<Instant>, progress: &mut Progress) -> Option<Collected> {
    if progress.since_ms != since_ms {
        *progress = Progress { since_ms, files: BTreeMap::new() };
    }
    let mut found: Vec<Said> = Vec::new();
    let (files, mut unreadable) = files_of(root, project, since_ms);
    let mut read: BTreeMap<String, FileProgress> = BTreeMap::new();
    let mut timed_out: Option<(String, FileProgress)> = None;
    for file in files {
        let key = file.to_string_lossy().to_string();
        match read_file(&file, progress.files.get(&key), since_ms, deadline) {
            FileRead::Unreadable => unreadable += 1,
            FileRead::Done { state, open_line } => {
                found.extend(state.items.iter().cloned());
                found.extend(open_line);
                read.insert(key, state);
            }
            FileRead::TimedOut(state) => {
                timed_out = Some((key, state));
                break;
            }
        }
    }
    if let Some((key, state)) = timed_out {
        // Keep what is known: files read in full, the file the deadline came
        // in, and the earlier progress of files not reached yet.
        let mut next = std::mem::take(&mut progress.files);
        for (key, mut state) in read.into_iter().chain(std::iter::once((key, state))) {
            match guard(Path::new(&key), state.offset) {
                Some((head, tail)) => {
                    state.head = head;
                    state.tail = tail;
                    next.insert(key, state);
                }
                None => {
                    next.remove(&key);
                }
            }
        }
        progress.files = next;
        return None;
    }
    found.retain(|s| s.at_ms <= until_ms);
    found.sort_by(|a, b| a.at_ms.cmp(&b.at_ms).then_with(|| a.text.cmp(&b.text)));
    let mut seen = std::collections::HashSet::new();
    found.retain(|s| seen.insert(s.text.clone()));
    Some(Collected { list: found, unreadable })
}

/// `collect_resumable` without a deadline and without earlier progress: the
/// whole list at once.
pub fn collect_in(root: &Path, project: &str, since_ms: i64, until_ms: i64) -> Collected {
    collect_resumable(root, project, since_ms, until_ms, None, &mut Progress::default()).expect("no deadline, so it finishes")
}

/// Where a started but unfinished read keeps its progress, beside the store.
pub fn progress_path(db: &Path, project: &str) -> PathBuf {
    let key: String = cursor_key(project).chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    db.parent().unwrap_or_else(|| Path::new(".")).join(format!("evaluation-said-progress-{key}.json"))
}

#[cfg(test)]
thread_local! {
    pub static TIME_BOX_OVERRIDE: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

fn time_box() -> Duration {
    #[cfg(test)]
    if let Some(d) = TIME_BOX_OVERRIDE.with(|c| c.get()) {
        return d;
    }
    TIME_BOX
}

// ------------------------------------------------------------ printing

fn render_line(said: &Said) -> String {
    format!("[{}] {}", date_of_ms(said.at_ms), said.text)
}

/// How many of `list` the print shows (oldest first, at least one when there
/// is one) and the time of the last of them in milliseconds (0 for none).
pub fn plan(list: &[Said]) -> (usize, i64) {
    let mut used = 0;
    let mut shown = 0;
    for said in list {
        used += render_line(said).chars().count() + 1;
        if shown > 0 && used > PRINT_CAP_CHARS - PRINT_RESERVE_CHARS {
            break;
        }
        shown += 1;
    }
    (shown, list.get(shown.wrapping_sub(1)).map_or(0, |s| s.at_ms))
}

/// The text `serve said` prints for `list`: a heading, the messages shown,
/// oldest first, one line each with its date, and - when some do not fit - a
/// last line with how many newer messages wait for the next evaluation.
pub fn render(project: &str, list: &[Said]) -> String {
    let (shown, _) = plan(list);
    let mut out = format!(
        "What the owner typed in '{project}' since the previous evaluation, oldest first ({} message(s) on the list, {shown} printed). \
         Read them all. For every message that says how the owner wants something done in a way that should still hold in a \
         later conversation, look it up first and then store it or correct the note that holds it.\n",
        list.len()
    );
    for said in &list[..shown] {
        out.push_str(&render_line(said));
        out.push('\n');
    }
    if shown < list.len() {
        out.push_str(&format!("{} newer message(s) wait for the next evaluation.\n", list.len() - shown));
    }
    out
}

// -------------------------------------------------------------- cursor

/// The small state file beside the store that holds each project's cursor.
pub fn cursor_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("evaluation-said-cursors.json")
}

fn cursor_key(project: &str) -> String {
    crate::project::project_match_key(project)
}

fn read_cursors(db: &Path) -> BTreeMap<String, i64> {
    std::fs::read_to_string(cursor_path(db)).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

/// The project's cursor in milliseconds, when it has one.
pub fn cursor_of(db: &Path, project: &str) -> Option<i64> {
    read_cursors(db).get(&cursor_key(project)).copied()
}

/// Move the project's cursor forward to `through_ms` (never back).
pub fn advance_cursor(db: &Path, project: &str, through_ms: i64) {
    if through_ms <= 0 {
        return;
    }
    let mut cursors = read_cursors(db);
    let entry = cursors.entry(cursor_key(project)).or_insert(0);
    if *entry >= through_ms {
        return;
    }
    *entry = through_ms;
    let path = cursor_path(db);
    let temp = path.with_extension("json.tmp");
    if let Ok(text) = serde_json::to_string_pretty(&cursors) {
        if std::fs::write(&temp, text).is_ok() {
            let _ = std::fs::rename(&temp, &path);
        }
    }
}

/// Move the cursor of the evaluation that `record` (a validation that counts)
/// is about, to the last message its list printed.
pub fn advance_for_valid(store: &EventStore, record: &model::store::EvaluationValidation) {
    let (Some(project), Some(started)) = (record.project.as_deref(), record.started_seq) else { return };
    let Some(db) = store.path() else { return };
    let start = model::store::evaluation_starts_of_session(store, &record.session)
        .into_iter()
        .filter(|s| s.frozen.project == project && s.frozen.started_seq == started)
        .max_by_key(|s| s.seq);
    if let Some(said) = start.and_then(|s| s.frozen.said) {
        advance_cursor(&db, project, said.through_ms);
    }
}

// -------------------------------------------------------------- freezing

/// What the evaluation of `project` starting at `now` (unix seconds) freezes:
/// the window and the counts. `None` when the list is empty and every
/// transcript could be read: such an evaluation asks nothing about it.
/// `Err(Pending)` when reading took longer than the time box: the progress is
/// saved, nothing is frozen and the next call goes on from there.
pub fn freeze(db: &Path, root: Option<&Path>, project: &str, now: i64) -> Result<Option<SaidFrozen>, Pending> {
    freeze_within(db, root, project, now, time_box())
}

/// `freeze`, reading for at most `box_` (never more than the usual time box).
pub fn freeze_within(db: &Path, root: Option<&Path>, project: &str, now: i64, box_: Duration) -> Result<Option<SaidFrozen>, Pending> {
    let until_ms = now * 1000;
    let since_ms = cursor_of(db, project).unwrap_or(until_ms - FIRST_WINDOW_DAYS * 86_400_000);
    let collected = match root {
        Some(root) => collect_boxed(db, root, project, since_ms, until_ms, Some(box_.min(time_box())))?,
        None => Collected::default(),
    };
    if collected.list.is_empty() && collected.unreadable == 0 {
        return Ok(None);
    }
    let (shown, through_ms) = plan(&collected.list);
    Ok(Some(SaidFrozen {
        since_ms,
        until_ms,
        total: collected.list.len(),
        shown,
        through_ms,
        unreadable: collected.unreadable,
        command: model::store::said_command_for(Some(db), project),
    }))
}

/// Reading the owner's messages is not finished; the next Stop goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pending;

/// `collect_resumable` under `box_` (none: no limit), with the progress kept in
/// the sidecar beside the store between calls. A sidecar that cannot be read or
/// written never stops the list: the read then runs to the end.
fn collect_boxed(db: &Path, root: &Path, project: &str, since_ms: i64, until_ms: i64, box_: Option<Duration>) -> Result<Collected, Pending> {
    let path = progress_path(db, project);
    let mut progress = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Progress>(&text).ok())
        .unwrap_or_default();
    let deadline = box_.map(|b| Instant::now() + b);
    if let Some(collected) = collect_resumable(root, project, since_ms, until_ms, deadline, &mut progress) {
        let _ = std::fs::remove_file(&path);
        return Ok(collected);
    }
    let saved = serde_json::to_string(&progress).ok().is_some_and(|text| {
        !crate::store_dir::is_held(&path) && crate::store_dir::write(&path, text).is_ok()
    });
    if saved {
        return Err(Pending);
    }
    let collected = collect_resumable(root, project, since_ms, until_ms, None, &mut progress).expect("no deadline, so it finishes");
    Ok(collected)
}

/// What `serve said --project <project>` prints: the list of the newest open
/// evaluation of `project` that froze one, read again inside the frozen window.
pub fn print_for(store: &EventStore, root: Option<&Path>, project: &str) -> Result<String, String> {
    let now = crate::time::now_unix();
    let start = model::store::open_evaluation_starts_of_project(store, project, now)
        .into_iter()
        .filter(|s| s.frozen.said.is_some())
        .max_by_key(|s| s.seq);
    let Some(start) = start else {
        return Err(format!(
            "no open evaluation of '{project}' holds a list of what the owner said: either none is open, or nothing was typed \
             since the previous one"
        ));
    };
    let said = start.frozen.said.as_ref().expect("filtered above");
    let Some(root) = root else { return Err("could not find the folder with the session transcripts".to_string()) };
    let collected = collect_in(root, project, said.since_ms, said.until_ms);
    Ok(render(project, &collected.list))
}

// ------------------------------------------------------------- evidence

/// Splits a shell command into words the way Bash and PowerShell do for the
/// forms THOR prints: white space separates words except inside single or
/// double quotes, the quotes themselves are dropped (`--project='a b'` is one
/// word `--project=a b`), and a backslash before a space or, outside single
/// quotes, before a quote keeps that character (`a\ b`). Any other backslash is
/// literal, so Windows paths such as `C:\thor2\serve.exe` stay whole. PowerShell
/// (`powershell`) has no backslash escape at all: there a backslash is always literal.
fn shell_words(command: &str, powershell: bool) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some('\''), _) => word.push(c),
            (Some(_), '\\') if !powershell && matches!(chars.peek(), Some('"')) => word.push(chars.next().unwrap()),
            (Some(_), _) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, '\\') if !powershell && matches!(chars.peek(), Some(' ' | '"' | '\'')) => {
                word.push(chars.next().unwrap());
                started = true;
            }
            (None, _) if c.is_whitespace() => {
                if started || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                started = false;
            }
            (None, _) => word.push(c),
        }
    }
    if started || !word.is_empty() {
        words.push(word);
    }
    words
}

/// Whether a shell command runs THOR's `serve` with the `said` subcommand and
/// `--project <project>` (the program is `serve` or `serve.exe`, bare or at the
/// end of a path; the project may be quoted).
pub fn runs_said(command: &str, project: &str) -> bool {
    runs_said_in(command, project, false)
}

/// `runs_said` for a command of the PowerShell tool (`powershell`) or of Bash.
pub fn runs_said_in(command: &str, project: &str, powershell: bool) -> bool {
    let clean = |t: String| t.trim_matches(|c| c == '&' || c == ';' || c == '|' || c == '(' || c == ')').to_string();
    let tokens: Vec<String> = shell_words(command, powershell).into_iter().map(clean).filter(|t| !t.is_empty()).collect();
    let is_serve = |t: &str| {
        let base = t.rsplit(['/', '\\']).next().unwrap_or("").to_lowercase();
        base == "serve" || base == "serve.exe"
    };
    let Some(at) = tokens.iter().position(|t| is_serve(t)) else { return false };
    let rest = &tokens[at + 1..];
    if !rest.iter().any(|t| t == "said") {
        return false;
    }
    let named = rest.iter().enumerate().find_map(|(i, t)| match t.strip_prefix("--project=") {
        Some(value) => Some(value.to_string()),
        None if t == "--project" => rest.get(i + 1).cloned(),
        None => None,
    });
    named.is_some_and(|name| crate::project::same_project_name(&name, project))
}

/// Whether `transcript` shows a shell call, made after `started_unix`, that
/// runs the print command for `project`. Lines of helpers never count.
pub fn printed_in_transcript(transcript: &Path, started_unix: i64, project: &str) -> bool {
    let Ok(bytes) = std::fs::read(transcript) else { return false };
    String::from_utf8_lossy(&bytes).lines().any(|raw| {
        let Ok(line) = serde_json::from_str::<serde_json::Value>(raw) else { return false };
        if line.get("type").and_then(|t| t.as_str()) != Some("assistant") || line.get("isSidechain").and_then(|b| b.as_bool()) == Some(true) {
            return false;
        }
        let at = line.get("timestamp").and_then(|t| t.as_str()).and_then(crate::eval_filing::unix_of_timestamp).unwrap_or(0);
        if at < started_unix {
            return false;
        }
        let items = line.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array());
        items.into_iter().flatten().any(|item| {
            let tool = item.get("name").and_then(|n| n.as_str());
            let shell = item.get("type").and_then(|t| t.as_str()) == Some("tool_use") && matches!(tool, Some("Bash") | Some("PowerShell"));
            shell && runs_said_in(item.get("input").and_then(|i| i.get("command")).and_then(|c| c.as_str()).unwrap_or(""), project, tool == Some("PowerShell"))
        })
    })
}

/// When `start` froze a list and the session's own transcript shows the print
/// command run after the start, write that down on the log (once). Returns
/// whether the evidence is on the log afterwards.
pub fn record_print_evidence(store: &mut EventStore, root: Option<&Path>, start: &EvaluationStart) -> bool {
    if start.frozen.said.as_ref().is_none_or(|s| s.total == 0) {
        return true;
    }
    if model::store::said_printed(store, start) {
        return true;
    }
    let Some(file) = crate::attest::main_transcript_of(None, root, &start.frozen.session) else { return false };
    if !printed_in_transcript(&file, start.frozen.started_unix, &start.frozen.project) {
        return false;
    }
    model::store::record_said_printed(store, start).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(at: &str, text: &str, extra: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"{at}","cwd":"CWD","message":{{"role":"user","content":{}}}{extra}}}"#,
            serde_json::to_string(text).unwrap()
        )
    }

    /// A transcripts folder holding one main session of `cwd` with `lines`.
    fn session(root: &Path, folder: &str, id: &str, cwd: &Path, lines: &[String]) {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        let body = lines.iter().map(|l| l.replace("CWD", &cwd.to_string_lossy().replace('\\', "\\\\"))).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.join(format!("{id}.jsonl")), body).unwrap();
    }

    fn project_folder(base: &Path, name: &str) -> PathBuf {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(crate::project::MARKER_FILE_NAME), format!("{name}\n")).unwrap();
        dir
    }

    const T0: i64 = 1_790_000_000_000;

    fn at(offset_s: i64) -> String {
        crate::time::iso8601_from_unix(T0 / 1000 + offset_s)
    }

    #[test]
    fn the_list_holds_only_owner_typed_prompts_of_the_right_project_and_window() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let mine = project_folder(dir.path(), "demo");
        let other = project_folder(dir.path(), "elsewhere");
        let sub = mine.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        session(
            &root,
            "a",
            "s1",
            &mine,
            &[
                user(&at(10), "please always answer me in short sentences", ""),
                // Not typed by the owner: a tool result, a helper, a meta line, text from another origin, a reminder.
                r#"{"type":"user","timestamp":"2026-03-02T10:00:00Z","cwd":"CWD","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"a result that is long enough to count"}]}}"#.to_string(),
                user(&at(11), "a helper typed this long enough line", r#","isSidechain":true"#),
                user(&at(12), "a meta line that is long enough to count", r#","isMeta":true"#),
                user(&at(13), "a task notification that is long enough", r#","origin":{"kind":"task-notification"}"#),
                user(&at(14), "<system-reminder>a reminder that is long enough</system-reminder>", ""),
                user(&at(15), "This session is being continued from a previous conversation that ran out", ""),
                // Outside the window: before it and after it.
                user(&at(-5000), "an old message that is long enough to count", ""),
                user(&at(5000), "a later message that is long enough to count", ""),
            ],
        );
        // A session typed in a subfolder of the project counts too.
        session(&root, "b", "s2", &sub, &[user(&at(20), "keep every commit message plain and short", "")]);
        // Another project does not.
        session(&root, "c", "s3", &other, &[user(&at(30), "this belongs to some other project entirely", "")]);
        // A helper's file below the main session does not.
        let helper = root.join("a").join("s1");
        std::fs::create_dir_all(&helper).unwrap();
        std::fs::write(helper.join("agent-1.jsonl"), user(&at(40), "a helper file that is long enough to count", "").replace("CWD", &mine.to_string_lossy().replace('\\', "\\\\"))).unwrap();

        let got = collect_in(&root, "demo", T0, T0 + 1_000_000);
        let texts: Vec<&str> = got.list.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["please always answer me in short sentences", "keep every commit message plain and short"]);
        assert_eq!(got.unreadable, 0);
    }

    #[test]
    fn a_conversation_moved_into_the_project_later_is_on_its_list() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let mine = project_folder(dir.path(), "demo");
        let scratch = dir.path().join("scratch-workspace");
        std::fs::create_dir_all(&scratch).unwrap();
        let mine_json = mine.to_string_lossy().replace('\\', "\\\\");
        // Begun in a folder of no project, then moved: the project's folder is
        // named only after the first 200 lines.
        let mut lines: Vec<String> = (1..=250).map(|i| user(&at(i), &format!("an early message in the scratch folder {i}"), "")).collect();
        lines.push(user(&at(300), "the arms are printed in PPS from now on", "").replace("CWD", &mine_json));
        session(&root, "a", "s1", &scratch, &lines);

        let full = collect_in_full(&root, "demo", T0, T0 + 1_000_000);
        assert_eq!(full.list.len(), 251, "every message of the session is the owner's, the moved one included");
        assert!(full.list.iter().any(|s| s.text == "the arms are printed in PPS from now on"));
        assert_eq!(collect_in(&root, "demo", T0, T0 + 1_000_000), full, "the fast reader gives the same list");
        assert!(collect_in(&root, "elsewhere", T0, T0 + 1_000_000).list.is_empty(), "a project the session never worked in gets none of it");
    }

    #[test]
    fn a_message_typed_while_a_turn_runs_is_on_the_list_of_both_readers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let mine = project_folder(dir.path(), "demo");
        let queued = format!(
            r#"{{"isSidechain":false,"attachment":{{"type":"queued_command","prompt":"will the users notice this at all?","commandMode":"prompt","origin":{{"kind":"human"}},"humanTurn":true}},"type":"attachment","timestamp":"{}","cwd":"CWD"}}"#,
            at(20)
        );
        session(&root, "a", "s1", &mine, &[user(&at(10), "please look at the reports of today", ""), queued]);

        let full = collect_in_full(&root, "demo", T0, T0 + 1_000_000);
        let texts: Vec<&str> = full.list.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["please look at the reports of today", "will the users notice this at all?"]);
        assert_eq!(collect_in(&root, "demo", T0, T0 + 1_000_000), full, "the fast reader no longer skips it");
    }

    #[test]
    fn short_and_repeated_messages_are_left_out_and_a_secret_is_blanked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let mine = project_folder(dir.path(), "demo");
        let token = format!("{}{}", "ghp_", "a".repeat(30));
        session(
            &root,
            "a",
            "s1",
            &mine,
            &[
                user(&at(1), "yes", ""),
                user(&at(2), "go ahead and do it", ""),
                user(&at(3), "never push to the main branch without asking", ""),
                user(&at(4), "never   push to the main\nbranch without asking", ""),
                user(&at(5), &format!("my token is {token} so use it for the api"), ""),
            ],
        );
        let got = collect_in(&root, "demo", T0, T0 + 1_000_000);
        let texts: Vec<&str> = got.list.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts.len(), 2, "{texts:?}");
        assert_eq!(texts[0], "never push to the main branch without asking");
        assert!(texts[1].contains("[secret removed]") && !texts[1].contains(&token), "{texts:?}");
        assert_eq!(got.list[0].at_ms, T0 + 3000, "the repeat keeps the oldest");
    }

    #[test]
    fn the_print_is_capped_and_says_how_many_wait() {
        let list: Vec<Said> = (0..40).map(|n| Said { at_ms: T0 + n * 1000, text: format!("{n:02} {}", "word ".repeat(150)) }).collect();
        let (shown, through) = plan(&list);
        assert!(shown > 0 && shown < list.len(), "{shown}");
        assert_eq!(through, list[shown - 1].at_ms, "the cursor goes to the last message shown");
        let printed = render("demo", &list);
        assert!(printed.chars().count() <= PRINT_CAP_CHARS, "{}", printed.chars().count());
        assert!(printed.contains("00 word"), "the oldest part is printed");
        assert!(!printed.contains(&format!("\n{:02} word", 39)), "the newest part is not");
        assert!(printed.trim_end().ends_with(&format!("{} newer message(s) wait for the next evaluation.", list.len() - shown)), "{printed}");
        // A short list prints whole, with no waiting line.
        let few = &list[..3];
        assert_eq!(plan(few).0, 3);
        assert!(!render("demo", few).contains("wait for the next"));
        // One message alone is always shown.
        assert_eq!(plan(&list[..1]).0, 1);
    }

    #[test]
    fn the_cursor_moves_forward_only_and_per_project() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert_eq!(cursor_of(&db, "Demo App"), None);
        advance_cursor(&db, "Demo App", 5_000);
        advance_cursor(&db, "Demo App", 3_000);
        assert_eq!(cursor_of(&db, "demo-app"), Some(5_000), "spelling does not make a second project");
        assert_eq!(cursor_of(&db, "other"), None);
        advance_cursor(&db, "Demo App", 0);
        assert_eq!(cursor_of(&db, "Demo App"), Some(5_000));
    }

    #[test]
    fn only_the_print_command_of_the_right_project_counts() {
        assert!(runs_said("serve --db x said --project demo", "demo"));
        assert!(runs_said("\"C:/thor2/serve.exe\" --db \"C:/x y/thor.db\" said --project 'Demo'", "demo"));
        assert!(runs_said("cd x && ~/.thor2/serve --db y said --project=demo | head", "demo"));
        assert!(!runs_said("serve --db x said --project other", "demo"));
        assert!(!runs_said("serve --db x said", "demo"));
        assert!(!runs_said("serve --db x status --project demo", "demo"));
        assert!(!runs_said("echo said --project demo", "demo"));
    }

    #[test]
    fn a_project_name_with_a_space_passes_in_every_quoting_form() {
        for command in [
            "serve --db x said --project 'Trade bot'",
            "serve --db x said --project \"Trade bot\"",
            "serve --db x said --project=\"Trade bot\"",
            "serve --db x said --project='Trade bot'",
            "serve --db x said --project Trade\\ bot",
            "& \"C:\\thor2\\serve.exe\" --db \"C:\\my store\\thor.db\" said --project 'Trade bot'",
            "cd x && serve --db x said --project 'Trade bot' | head",
            "serve --db x said --project trade-bot",
        ] {
            assert!(runs_said(command, "Trade bot"), "{command}");
        }
        assert!(!runs_said("serve --db x said --project 'Trade'", "Trade bot"));
        assert!(!runs_said("serve --db x said --project Trade bot", "Trade bot"), "an unquoted space ends the name");
        assert!(!runs_said("serve --db x said --project 'Other bot'", "Trade bot"));
        assert!(!runs_said("serve --db x said --project \"Trade bot", "Trade bot x"));
    }

    #[test]
    fn a_backslash_before_a_quote_is_literal_in_powershell_and_an_escape_in_bash() {
        let ps = "Set-Location \"C:\\x\\\"; & C:/Users/a/thor2/serve.exe --db y said --project 'Trade bot'";
        assert!(runs_said_in(ps, "Trade bot", true), "PowerShell closes the word at the quote");
        assert!(!runs_said_in(ps, "Trade bot", false), "Bash reads the backslash as an escape");
        assert!(runs_said_in("serve --db \"a \\\"b\\\" c\" said --project demo", "demo", false));
        // Through the transcript reader: the tool name decides.
        let dir = tempfile::tempdir().unwrap();
        let line = |tool: &str| {
            serde_json::json!({ "type": "assistant", "timestamp": "2026-11-20T10:00:00Z",
                "message": { "content": [{ "type": "tool_use", "name": tool, "input": { "command": ps } }] } })
            .to_string()
        };
        std::fs::write(dir.path().join("p.jsonl"), line("PowerShell")).unwrap();
        std::fs::write(dir.path().join("b.jsonl"), line("Bash")).unwrap();
        assert!(printed_in_transcript(&dir.path().join("p.jsonl"), 0, "Trade bot"));
        assert!(!printed_in_transcript(&dir.path().join("b.jsonl"), 0, "Trade bot"));
    }

    #[test]
    fn the_start_notes_own_printed_command_passes_for_a_spaced_name() {
        let db = std::path::Path::new("C:/my store/thor.db");
        for program in [std::path::Path::new("C:/thor2/serve.exe"), std::path::Path::new("serve"), std::path::Path::new("C:/Program Files/thor/serve.exe")] {
            let printed = model::store::said_command_with(program, Some(db), "Trade bot");
            assert!(runs_said(&printed, "Trade bot"), "{printed}");
            // PowerShell form of the same command: call operator in front.
            assert!(runs_said(&format!("& {printed}"), "Trade bot"), "& {printed}");
            assert!(!runs_said(&printed, "Other bot"), "{printed}");
        }
    }

    // ------------------------------------------------ the fast reader

    /// Reads under an expired deadline round after round (each round moves on by
    /// at least one line, like a Stop whose time box ran out), through the JSON
    /// of the sidecar, and returns the list once it is whole.
    fn boxed_rounds(root: &Path, project: &str, since: i64, until: i64, between: &mut dyn FnMut(usize)) -> (Collected, usize) {
        let mut progress = Progress::default();
        let mut rounds = 0;
        loop {
            between(rounds);
            rounds += 1;
            if let Some(done) = collect_resumable(root, project, since, until, Some(Instant::now()), &mut progress) {
                return (done, rounds);
            }
            progress = serde_json::from_str(&serde_json::to_string(&progress).unwrap()).unwrap();
            assert!(rounds < 10_000, "no progress");
        }
    }

    fn texts(c: &Collected) -> Vec<String> {
        c.list.iter().map(|s| format!("{} {}", s.at_ms, s.text)).collect()
    }

    fn many(n: i64) -> Vec<String> {
        (0..n).map(|i| user(&at(i), &format!("message number {i} that is long enough to count"), "")).collect()
    }

    #[test]
    fn the_fast_reader_and_the_continued_reader_give_the_list_of_the_full_read() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let mine = project_folder(dir.path(), "demo");
        let mut lines = many(30);
        // Lines the byte test and the parser must treat as the full read does.
        lines.push(r#"{"type":"assistant","timestamp":"2026-03-02T10:00:00Z","cwd":"CWD","message":{"content":"the word \"user\" in a reply that is long"}}"#.to_string());
        lines.push(r#"{"type": "user", "timestamp": "2026-03-02T10:00:01Z", "cwd": "CWD", "message": {"role": "user", "content": "spaces after the colons in this long line"}}"#.to_string());
        lines.push("not json at all but it says \"user\" and is long enough".to_string());
        lines.push(String::new());
        lines.push(user(&at(40), "last line with a crlf ending that is long enough", "") + "\r");
        session(&root, "a", "s1", &mine, &lines);
        session(&root, "b", "s2", &mine, &many(12));
        let (since, until) = (T0 + 3_500, T0 + 100_000);
        let full = collect_in_full(&root, "demo", since, until);
        assert!(full.list.len() > 20, "{}", full.list.len());
        assert_eq!(collect_in(&root, "demo", since, until), full);
        let (boxed, rounds) = boxed_rounds(&root, "demo", since, until, &mut |_| {});
        assert!(rounds > 5, "the deadline was expired every time: {rounds}");
        assert_eq!(boxed, full);
        assert_eq!(render("demo", &boxed.list), render("demo", &full.list));
    }

    #[test]
    fn a_file_that_grows_between_two_reads_gives_the_full_read_of_the_final_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let mine = project_folder(dir.path(), "demo");
        session(&root, "a", "s1", &mine, &many(10));
        let file = root.join("a").join("s1.jsonl");
        let cwd = mine.to_string_lossy().replace('\\', "\\\\");
        let mut grown = false;
        let (got, _) = boxed_rounds(&root, "demo", T0 - 1, T0 + 1_000_000, &mut |round| {
            if round == 4 && !grown {
                grown = true;
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
                // A line cut in the middle first (the writer is not done), then its rest.
                let more = user(&at(50), "appended while the first read was going", "").replace("CWD", &cwd);
                let (a, b) = more.split_at(more.len() / 2);
                write!(f, "\n{a}").unwrap();
                f.flush().unwrap();
                write!(f, "{b}\n{}", user(&at(51), "and a second appended line that is long", "").replace("CWD", &cwd)).unwrap();
            }
        });
        assert!(grown);
        assert_eq!(got, collect_in_full(&root, "demo", T0 - 1, T0 + 1_000_000));
        assert!(got.list.iter().any(|s| s.text.contains("appended while")) && got.list.iter().any(|s| s.text.contains("second appended")));
    }

    #[test]
    fn a_file_replaced_by_a_shorter_or_other_one_is_read_again_from_its_start() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let mine = project_folder(dir.path(), "demo");
        session(&root, "a", "s1", &mine, &many(40));
        let mut progress = Progress::default();
        for _ in 0..10 {
            assert!(collect_resumable(&root, "demo", T0 - 1, T0 + 1_000_000, Some(Instant::now()), &mut progress).is_none());
        }
        let key = root.join("a").join("s1.jsonl").to_string_lossy().to_string();
        assert!(progress.files[&key].offset > 0 && !progress.files[&key].items.is_empty());
        // Shorter.
        let lines: Vec<String> = (0..5).map(|i| user(&at(i), &format!("a different short file, message {i} long enough"), "")).collect();
        session(&root, "a", "s1", &mine, &lines);
        let got = loop {
            if let Some(done) = collect_resumable(&root, "demo", T0 - 1, T0 + 1_000_000, Some(Instant::now()), &mut progress) {
                break done;
            }
        };
        assert_eq!(got, collect_in_full(&root, "demo", T0 - 1, T0 + 1_000_000));
        assert_eq!(got.list.len(), 5);
        // The same length with other bytes at the start: the guard sees it.
        let mut progress = Progress::default();
        for _ in 0..3 {
            let _ = collect_resumable(&root, "demo", T0 - 1, T0 + 1_000_000, Some(Instant::now()), &mut progress);
        }
        let lines: Vec<String> = (0..5).map(|i| user(&at(i), &format!("another short file of equal length {i} long enough"), "")).collect();
        let before = std::fs::metadata(&key).unwrap().len();
        session(&root, "a", "s1", &mine, &lines);
        assert!(std::fs::metadata(&key).unwrap().len() >= before);
        let got = loop {
            if let Some(done) = collect_resumable(&root, "demo", T0 - 1, T0 + 1_000_000, Some(Instant::now()), &mut progress) {
                break done;
            }
        };
        assert_eq!(got, collect_in_full(&root, "demo", T0 - 1, T0 + 1_000_000));
        assert!(got.list.iter().all(|s| s.text.starts_with("another short file")), "{:?}", texts(&got));
    }

    #[test]
    fn a_file_last_modified_before_the_window_is_not_read_and_the_list_is_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let mine = project_folder(dir.path(), "demo");
        session(&root, "a", "s1", &mine, &many(5));
        session(&root, "b", "s2", &mine, &[user(&at(60), "a newer session that is long enough to count", "")]);
        let old = std::fs::File::options().write(true).open(root.join("a").join("s1.jsonl")).unwrap();
        old.set_modified(std::time::UNIX_EPOCH + Duration::from_millis(T0 as u64 - 1_000_000)).unwrap();
        drop(old);
        // The window opens after the old file's last change and before its message times:
        // the file is skipped by both readers, and nothing newer is lost.
        let since = T0 - 1;
        let (files, _) = files_of(&root, "demo", since);
        assert_eq!(files.len(), 1);
        let got = collect_in(&root, "demo", since, T0 + 1_000_000);
        assert_eq!(got, collect_in_full(&root, "demo", since, T0 + 1_000_000));
        assert_eq!(got.list.len(), 1);
    }

    #[test]
    fn identical_messages_in_two_sessions_count_once_at_the_oldest_time() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let mine = project_folder(dir.path(), "demo");
        session(&root, "a", "s1", &mine, &[user(&at(30), "keep every answer short and plain please", "")]);
        session(&root, "b", "s2", &mine, &[user(&at(10), "keep every answer short and plain please", ""), user(&at(20), "a second wish that is long enough here", "")]);
        let (got, _) = boxed_rounds(&root, "demo", T0, T0 + 1_000_000, &mut |_| {});
        assert_eq!(got.list.len(), 2);
        assert_eq!(got.list[0].at_ms, T0 + 10_000);
        assert_eq!(got, collect_in_full(&root, "demo", T0, T0 + 1_000_000));
        assert_eq!(collect_in(&root, "demo", T0, T0 + 1_000_000), got);
        // Identical text on both sides of the window's end: the older one that is inside stays.
        assert_eq!(collect_in(&root, "demo", T0, T0 + 15_000).list.len(), 1);
        assert_eq!(collect_in(&root, "demo", T0, T0 + 15_000), collect_in_full(&root, "demo", T0, T0 + 15_000));
    }

    #[test]
    fn a_message_of_nineteen_characters_is_left_out_and_one_of_twenty_stays() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let mine = project_folder(dir.path(), "demo");
        let nineteen = "a".repeat(19);
        let twenty = "b".repeat(20);
        let spaced = format!("c d  e\n{}", "f".repeat(14));
        session(&root, "a", "s1", &mine, &[user(&at(1), &nineteen, ""), user(&at(2), &twenty, ""), user(&at(3), &spaced, "")]);
        let got = collect_in(&root, "demo", T0, T0 + 1_000_000);
        let t: Vec<&str> = got.list.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(t, vec![twenty.as_str(), "c d e ffffffffffffff"]);
        assert_eq!(got, collect_in_full(&root, "demo", T0, T0 + 1_000_000));
    }

    /// What is left of a start's hard limit is the box: none left reads nothing, plenty reads it all.
    #[test]
    fn what_is_left_of_the_start_limit_is_the_box() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let db = dir.path().join("store").join("thor.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let mine = project_folder(dir.path(), "demo");
        session(&root, "a", "s1", &mine, &many(25));
        let now = T0 / 1000 + 10_000;
        assert!(freeze_within(&db, Some(&root), "demo", now, Duration::ZERO).is_err(), "no time left: pending, progress saved");
        let whole = freeze_within(&db, Some(&root), "demo", now, Duration::from_secs(600)).unwrap().expect("a list");
        assert_eq!(whole.total, 25);
        assert_eq!(freeze(&db, Some(&root), "demo", now).unwrap().expect("a list"), whole, "the same list as the plain call");
    }

    #[test]
    fn a_time_box_that_runs_out_freezes_nothing_and_the_next_call_finishes_the_same_list() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let db = dir.path().join("store").join("thor.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let mine = project_folder(dir.path(), "demo");
        session(&root, "a", "s1", &mine, &many(25));
        session(&root, "b", "s2", &mine, &many(25).iter().map(|l| l.replace("number", "digit")).collect::<Vec<_>>());
        let now = T0 / 1000 + 10_000;
        let whole = freeze(&db, Some(&root), "demo", now).unwrap().expect("a list");
        assert!(!progress_path(&db, "demo").exists(), "a finished read leaves no progress behind");
        assert_eq!(whole.total, 50);
        TIME_BOX_OVERRIDE.with(|c| c.set(Some(Duration::ZERO)));
        let mut pending = 0;
        let got = loop {
            match freeze(&db, Some(&root), "demo", now) {
                Err(Pending) => {
                    pending += 1;
                    assert!(progress_path(&db, "demo").is_file(), "the progress is saved");
                    assert!(pending < 1000);
                }
                Ok(frozen) => break frozen.expect("a list"),
            }
        };
        TIME_BOX_OVERRIDE.with(|c| c.set(None));
        assert!(pending > 1, "{pending}");
        assert_eq!(got, whole);
        assert!(!progress_path(&db, "demo").exists(), "progress is removed once the list is whole");
        // Progress that belongs to another window start is not used.
        let mut stale = Progress { since_ms: 1, files: BTreeMap::new() };
        stale.files.insert("nowhere".to_string(), FileProgress::default());
        std::fs::write(progress_path(&db, "demo"), serde_json::to_string(&stale).unwrap()).unwrap();
        assert_eq!(freeze(&db, Some(&root), "demo", now).unwrap().expect("a list"), whole);
        // A damaged progress file is ignored too.
        std::fs::write(progress_path(&db, "demo"), "{ not json").unwrap();
        assert_eq!(freeze(&db, Some(&root), "demo", now).unwrap().expect("a list"), whole);
    }
}
