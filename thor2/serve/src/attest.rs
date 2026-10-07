//! Was this refusal a real refusal of a real tool call? Asked of the
//! session's own Claude Code transcript, answered once, and recorded.
//!
//! THE LEAK THIS CLOSES (Z9, measured on a real log: one session, fourteen
//! refusals, and no tool call behind any of them in any of its 328 transcript
//! files). A refusal in the log was field evidence when it was stamped by the
//! hook, in a session the log showed other activity for. Both are cheap to
//! produce: pipe two hand-built payloads into `serve hook` with a real session
//! id - a SessionStart, then a PreToolUse - and the log holds a served item and
//! a refusal, exactly as a real session leaves them. What a hand-fed payload
//! cannot leave is the transcript line of the tool call it pretends to be.
//!
//! WHAT A REAL BLOCK LEAVES (measured, step 1 of the build, answers 5 and 7):
//! the assistant's `tool_use` block with the call's id, name and input, then a
//! user line whose `tool_result` block for that id has `is_error` true and
//! content "PreToolUse:<Tool> hook error: [THOR] rule <id> ...". Nothing else:
//! a block leaves no hook attachment (those appear only for a call that was
//! let through, and only after it finished), so none is looked for.
//!
//! HOW THE TRANSCRIPT IS FOUND (the reader recipe of that step): always from
//! the refusal's own session id, never from a path a payload names - the main
//! file `<projects>/<folder>/<session>.jsonl` plus every `.jsonl` under
//! `<projects>/<folder>/<session>/`, walked recursively, because a subagent's
//! and a workflow agent's calls live in files of their own there. Lines are
//! matched STRUCTURALLY (a `tool_use` block whose `id` field is the id, a
//! `tool_result` block whose `tool_use_id` is), never by finding the id's text
//! on a line: any command can mention an id. Copies of one line (same `uuid`)
//! count once; two different calls under one id are never attested.
//!
//! RECORDED ONCE (X3, N8). Transcripts are deleted after about 30 days, so the
//! answer is written to the log the first time it can be given - at every Stop
//! of the session (`record_refusal_verifications`), one `gate_stood_aside` by
//! actor `attester` per refusal (`model::store::record_refusal_verification`).
//! Readers use only that recorded verdict: an unrecorded refusal whose
//! transcript is gone is never proof. A refusal whose session has no
//! transcript on this machine yet is left unrecorded, not recorded as false,
//! so a later Stop can still decide it.
//!
//! WHAT STAYS OPEN (N9): a hand-written transcript line, or a patched build,
//! can forge any of this. Each leaves the forged file behind.
//!
//! THE SECOND HALF: WHICH SESSION MADE A WRITE (step 12, K21). The tool server
//! cannot know which session calls it: every write it made was stamped with
//! one shared session name, so a verdict typed into a shell (`serve mark`) or
//! sent by a second client counted exactly like the evaluating session's own.
//! Two functions, because what is on disk while a call RUNS differs from what
//! is on disk at the Stop (step 1 of the build, answers 5 and 6: the hook's
//! attachments are written only after the tool finished):
//! - `check_call_in_flight` (the writer) looks for POSITIVE evidence of a side
//!   door only - no tool call id in the call's `_meta`, no session id in the
//!   server's environment, or the transcript already holding that id under
//!   another tool or other arguments. A line not on disk yet is `Unseen`,
//!   never a refusal and never a wait. It refuses nothing by itself: the
//!   answer rides in the write's `att` stamp.
//! - `verify_write` (the reader, at the Stop) demands all three lines: the
//!   tool_use with the same THOR tool and argument digest, THOR's PreToolUse
//!   marker for it in a hook attachment (`write_marker` - a hand-fed hook run
//!   prints it into a Bash result, never into an attachment), and a result
//!   naming this write's own `event seq N` (a replayed real id names another
//!   seq). `record_write_verifications` records the answer once per write, at
//!   every Stop of the session; every reader uses only that record.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use thor_core::event_store::{Event, EventKind, EventStore};

use model::item::{Binding, Check, Item, TargetKind};
use model::store::{Att, SystemActor, ASSISTANT_DELIVERY_ACTOR, ATTESTER_ACTOR};

/// Where Claude Code keeps its transcripts: `<home>/.claude/projects`, from
/// `USERPROFILE`, else `HOME`. `None` when neither is set.
pub fn transcripts_root() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".claude").join("projects"))
}

/// Whether `s` has the shape of a Claude Code session id: 8-4-4-4-12
/// hexadecimal digits. Only such a session can have a transcript, and only
/// such an id is ever used to build a path.
pub fn is_uuid_shaped(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8usize, 4, 4, 4, 12]).all(|(g, n)| g.len() == n && g.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Every transcript file of `session` under `root`: the main file
/// `<root>/<folder>/<session>.jsonl` of every project folder, plus every
/// `.jsonl` under `<root>/<folder>/<session>/`, recursively (subagents and
/// workflow agents). Found by the session id alone; empty for an id that is
/// not UUID-shaped, so a crafted id can never walk anywhere else.
pub fn session_transcripts(root: &Path, session: &str) -> Vec<PathBuf> {
    if !is_uuid_shaped(session) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let Ok(folders) = std::fs::read_dir(root) else { return out };
    let mut folders: Vec<PathBuf> = folders.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    folders.sort();
    for folder in folders {
        let main = folder.join(format!("{session}.jsonl"));
        if main.is_file() {
            out.push(main);
        }
        walk_jsonl(&folder.join(session), &mut out, 0);
    }
    out
}

/// Every `.jsonl` file under `dir`, sorted, at most 16 folders deep.
fn walk_jsonl(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 16 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk_jsonl(&path, out, depth + 1);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
}

/// One tool call as the transcript holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct CallInTranscript {
    pub name: String,
    pub input: Value,
    /// Every result recorded for it: (is_error, content as text).
    pub results: Vec<(bool, String)>,
    /// Every result line's own `timestamp` (Claude Code stamps each line),
    /// with the result's text: (text, timestamp). A result line without one
    /// is left out here.
    pub result_times: Vec<(String, String)>,
    /// The text of every PreToolUse `hook_additional_context` attachment
    /// recorded for it - what THOR's hook said while letting it through.
    pub markers: Vec<String>,
    /// The file the tool_use line is in.
    pub file: PathBuf,
    /// The agent that made the call: the line's own `agentId`, else the id in
    /// an `agent-<id>.jsonl` file name; `None` for the main session.
    pub agent_id: Option<String>,
}

/// What the transcript files say about tool call `id`.
#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    Nothing,
    Call(CallInTranscript),
    /// Two different calls (name or input) under the one id: never attested.
    Conflicting,
}

/// The text of a `tool_result` block's content: a string, or the text of
/// every text block in an array.
fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => {
            blocks.iter().filter_map(|b| b.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join("\n")
        }
        _ => String::new(),
    }
}

/// The text of an attachment's `content`: a string, or every string in an
/// array.
fn attachment_text(content: Option<&Value>) -> Vec<String> {
    match content {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(parts)) => parts.iter().filter_map(|p| p.as_str().map(str::to_string)).collect(),
        _ => Vec::new(),
    }
}

/// The agent a call in `file` was made by: the line's `agentId`, else the id
/// in an `agent-<id>.jsonl` file name.
fn agent_of(line: &Value, file: &Path) -> Option<String> {
    if let Some(agent) = line.get("agentId").and_then(|a| a.as_str()).filter(|a| !a.is_empty()) {
        return Some(agent.to_string());
    }
    let stem = file.file_stem()?.to_str()?;
    stem.strip_prefix("agent-").filter(|a| !a.is_empty()).map(str::to_string)
}

/// One tool_use block as a scan found it: name, input, file, agent.
type UseLine = (String, Value, PathBuf, Option<String>);

/// What one scan collected for one id: tool_use blocks, results and hook
/// markers, each keyed by the line's `uuid` (plus the block's index), so a
/// copied line counts once.
#[derive(Default)]
struct Collected {
    uses: HashMap<String, UseLine>,
    results: HashMap<String, (bool, String)>,
    result_times: HashMap<String, (String, String)>,
    markers: HashMap<String, Vec<String>>,
}

/// Collect every line in `files` about one of `ids`, structurally - see this
/// module's doc comment. `wanted` is the cheap filter a line must pass before
/// it is parsed at all; a line counts only when a block's own `id` /
/// `tool_use_id` field, or an attachment's `toolUseID`, is one of `ids`.
fn scan(files: &[PathBuf], ids: &HashSet<&str>, wanted: impl Fn(&str) -> bool) -> HashMap<String, Collected> {
    let mut out: HashMap<String, Collected> = HashMap::new();
    for file in files {
        let Ok(handle) = std::fs::File::open(file) else { continue };
        for line in BufReader::new(handle).lines() {
            let Ok(line) = line else { break };
            if !wanted(&line) {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            let uuid = v.get("uuid").and_then(|u| u.as_str()).unwrap_or_default().to_string();
            if let Some(attachment) = v.get("attachment") {
                let of = attachment.get("toolUseID").and_then(|t| t.as_str()).unwrap_or_default();
                if ids.contains(of)
                    && attachment.get("type").and_then(|t| t.as_str()) == Some("hook_additional_context")
                    && attachment.get("hookEvent").and_then(|t| t.as_str()) == Some("PreToolUse")
                {
                    let seen = out.entry(of.to_string()).or_default();
                    seen.markers.insert(uuid.clone(), attachment_text(attachment.get("content")));
                }
            }
            let Some(blocks) = v.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) else {
                continue;
            };
            for (i, block) in blocks.iter().enumerate() {
                let key = format!("{uuid}#{i}");
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("tool_use") => {
                        let Some(id) = block.get("id").and_then(|b| b.as_str()).filter(|id| ids.contains(id)) else {
                            continue;
                        };
                        let name = block.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
                        let input = block.get("input").cloned().unwrap_or(Value::Null);
                        let agent = agent_of(&v, file);
                        out.entry(id.to_string()).or_default().uses.insert(key, (name, input, file.clone(), agent));
                    }
                    Some("tool_result") => {
                        let Some(id) = block.get("tool_use_id").and_then(|b| b.as_str()).filter(|id| ids.contains(id))
                        else {
                            continue;
                        };
                        let is_error = block.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
                        let text = result_text(block.get("content"));
                        let seen = out.entry(id.to_string()).or_default();
                        if let Some(at) = v.get("timestamp").and_then(|t| t.as_str()) {
                            seen.result_times.insert(key.clone(), (text.clone(), at.to_string()));
                        }
                        seen.results.insert(key, (is_error, text));
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

/// One id's scan, as a `Found`.
fn found_of(collected: Option<Collected>) -> Found {
    let Some(collected) = collected else { return Found::Nothing };
    let mut uses: Vec<(String, UseLine)> = collected.uses.into_iter().collect();
    uses.sort_by(|a, b| a.0.cmp(&b.0));
    let mut distinct: Vec<UseLine> = Vec::new();
    for (_, call) in uses {
        if !distinct.iter().any(|d| d.0 == call.0 && d.1 == call.1) {
            distinct.push(call);
        }
    }
    match distinct.len() {
        0 => Found::Nothing,
        1 => {
            let (name, input, file, agent_id) = distinct.remove(0);
            let mut results: Vec<(bool, String)> = collected.results.into_values().collect();
            results.sort();
            results.dedup();
            let mut result_times: Vec<(String, String)> = collected.result_times.into_values().collect();
            result_times.sort();
            result_times.dedup();
            let mut markers: Vec<String> = collected.markers.into_values().flatten().collect();
            markers.sort();
            markers.dedup();
            Found::Call(CallInTranscript { name, input, results, result_times, markers, file, agent_id })
        }
        _ => Found::Conflicting,
    }
}

/// Look tool call `id` up in `files`, structurally - see this module's doc
/// comment. A line is parsed only when its text holds the id at all (the
/// cheap filter); it counts only when a block's own `id` / `tool_use_id`
/// field (or an attachment's `toolUseID`) equals it.
pub fn find_call(files: &[PathBuf], id: &str) -> Found {
    let ids: HashSet<&str> = [id].into_iter().collect();
    found_of(scan(files, &ids, |line| line.contains(id)).remove(id))
}

/// `find_call` for many ids in ONE read of the files: a Stop that verifies a
/// whole evaluation's worth of writes must not read a long session's
/// transcripts once per write. A line is parsed only when it can hold a tool
/// call, a result or a hook attachment at all.
pub fn find_calls(files: &[PathBuf], ids: &HashSet<&str>) -> HashMap<String, Found> {
    if ids.is_empty() {
        return HashMap::new();
    }
    let mut collected = scan(files, ids, |line| {
        line.contains("\"tool_use\"") || line.contains("\"tool_result\"") || line.contains("hook_additional_context")
    });
    ids.iter().map(|id| (id.to_string(), found_of(collected.remove(*id)))).collect()
}

/// A slash-normalised path, for comparing a refused file with a call's own.
fn slashed(path: &str) -> String {
    path.replace('\\', "/")
}

/// Whether the call in the transcript is the call that was refused: the
/// refusal is filed under the command for a shell call, the file for a file
/// write (relative to the checkout when it lies inside it), and the tool name
/// for a call with neither.
fn same_call(name: &str, input: &Value, target: &str) -> bool {
    if let Some(command) = input.get("command").and_then(|c| c.as_str()) {
        // The log keeps a command with its likely secrets redacted.
        return command == target || crate::deliver::redact_secrets(command) == target;
    }
    if let Some(file) = input.get("file_path").and_then(|f| f.as_str()) {
        let (file, target) = (slashed(file), slashed(target));
        return file == target || file.ends_with(&format!("/{}", target.trim_start_matches("./")));
    }
    name == target
}

/// A refusal's verdict: whether it is field evidence, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    pub field: bool,
    pub why: String,
}

fn no(why: String) -> Option<Verification> {
    Some(Verification { field: false, why })
}

/// Verify one refusal against its session's transcript under `root` (see this
/// module's doc comment). `None` when it cannot be decided yet: no transcript
/// root, or no transcript file of its session exists on this machine. Every
/// other answer is final and is what gets recorded.
pub fn verify_refusal(store: &EventStore, event: &Event, root: Option<&Path>) -> Option<Verification> {
    if event.kind != EventKind::GateRefused {
        return no(format!("log seq {} is not a refusal", event.seq));
    }
    if event.actor != ASSISTANT_DELIVERY_ACTOR {
        return no(format!("it was recorded under actor '{}', never by the hook of a Claude Code session", event.actor));
    }
    let session = event.session_id.as_str();
    if !is_uuid_shaped(session) {
        return no(format!("session '{session}' is not a Claude Code session id"));
    }
    if store.served_ids_in_session(session).map_or(true, |ids| ids.is_empty()) {
        return no(format!("session {session} shows no other activity in the log"));
    }
    let body = serde_json::from_str::<Value>(&event.body).unwrap_or(Value::Null);
    let target = body.get("target").and_then(|t| t.as_str()).unwrap_or_default();
    let Some(tool_use_id) = body.get("tool_use_id").and_then(|t| t.as_str()).filter(|t| !t.is_empty()) else {
        return no("it carries no tool call id - it was recorded before THOR kept one, so it can never be checked".to_string());
    };
    let files = session_transcripts(root?, session);
    if files.is_empty() {
        return None;
    }
    let call = match find_call(&files, tool_use_id) {
        Found::Nothing => return no(format!("the transcript of session {session} holds no tool call {tool_use_id}")),
        Found::Conflicting => {
            return no(format!("the transcript of session {session} holds two different tool calls under {tool_use_id}"))
        }
        Found::Call(call) => call,
    };
    if !same_call(&call.name, &call.input, target) {
        return no(format!(
            "the tool call {tool_use_id} in the transcript ({}) is not the refused call '{target}'",
            call.name
        ));
    }
    let prefix = format!("PreToolUse:{} hook error: ", call.name);
    let blocked = call.results.iter().any(|(is_error, content)| {
        *is_error
            && content
                .strip_prefix(&prefix)
                .is_some_and(|rest| crate::absent_guard::rule_ids_of(rest).contains(&event.entity_id.as_str()))
    });
    if !blocked {
        return no(format!(
            "the tool call {tool_use_id} ran without THOR's hook blocking it for rule {} - its result is not the hook's \
             error naming that rule",
            event.entity_id
        ));
    }
    if let Some(why) = self_proof(store, event) {
        return no(why);
    }
    Some(Verification {
        field: true,
        why: format!(
            "session {session}'s transcript holds tool call {tool_use_id} ({}) and the hook's block of it naming rule {}",
            call.name, event.entity_id
        ),
    })
}

/// How a recorded verdict names a real refusal the evaluating session met
/// during its own evaluation (`self_proof`). Doctor counts these apart.
pub const OWN_EVALUATION_WHY: &str = "refused during the session's own evaluation - not proof";

/// How a recorded verdict names a real refusal met by the session that wrote
/// the rule's current check (`self_proof`). Doctor counts these apart.
pub const OWN_CHECK_WHY: &str = "refused in the session that wrote the rule's check - not proof";

/// SELF-PROOF (step 20, NEW-2): a refusal that really happened but proves
/// nothing, because the session that met it is the one judging or writing
/// the rule. Measured in C3: during its own evaluation a session ran the
/// forbidden commands itself at targets that do not exist (a push to a fake
/// remote, ssh to a fake host) so the hook would refuse them, and every one
/// was recorded as field evidence - the rules it was judging became
/// "proven" by calls made for that purpose. Two shapes the log can show:
/// - the session had its own evaluation open at the refusal
///   (`model::store::evaluation_open_in_session_at`);
/// - the session itself wrote the rule's check in force at the refusal, the
///   refusal side of D6 (a real call planted by the writing session stops
///   nothing; a refusal it provokes proves nothing either). A check THOR's
///   own actors set is not a writing session's.
///
/// `Some(why)` for a self-proof, `None` otherwise. What stays open: a
/// session that plants a refused call outside both - before its evaluation
/// starts, on a rule someone else wrote - looks exactly like a real call
/// (N15 in the step-20 report); a refusal only ever sets the "proven" label,
/// never a settlement.
pub fn self_proof(store: &EventStore, event: &Event) -> Option<String> {
    let session = event.session_id.as_str();
    let body = serde_json::from_str::<Value>(&event.body).unwrap_or(Value::Null);
    let at = body.get("at").and_then(|t| t.as_str()).and_then(crate::time::unix_from_iso8601);
    if let Some(start) = model::store::evaluation_open_in_session_at(store, session, event.seq, at) {
        return Some(format!(
            "{OWN_EVALUATION_WHY}: session {session} had its evaluation of '{}' open since log seq {} ('{}')",
            start.frozen.project, start.seq, start.id
        ));
    }
    let events = store.get_events_by_entity(&event.entity_id).ok()?;
    let setter = check_setter_before(&events, event.seq)?;
    let own = setter.session_id == session && !model::store::RESERVED_ACTORS.contains(&setter.actor.as_str());
    own.then(|| format!("{OWN_CHECK_WHY}: session {session} set this rule's check at log seq {}", setter.seq))
}

/// The write that set the check in force just before log seq `seq` on
/// `events` (one item's own log): the last create or revise before `seq`
/// whose check differs from the one before it. `None` when no check was in
/// force then.
fn check_setter_before(events: &[Event], seq: i64) -> Option<&Event> {
    let mut logged: Option<Option<Check>> = None;
    let mut setter: Option<&Event> = None;
    for e in events.iter().filter(|e| e.seq < seq) {
        match e.kind {
            EventKind::FactCreated | EventKind::FactRevised => {
                let Ok(item) = model::store::parse_body(&e.body) else { continue };
                if logged.as_ref() != Some(&item.check) {
                    setter = item.check.is_some().then_some(e);
                    logged = Some(item.check);
                }
            }
            EventKind::FactRetracted => {
                logged = None;
                setter = None;
            }
            _ => {}
        }
    }
    setter
}

/// Verify and record every refusal of `session` that has no recorded verdict
/// yet, against the transcripts under `transcripts_root()`. Runs at every Stop
/// of the session and never holds a turn. Returns how many were recorded.
pub fn record_refusal_verifications(store: &mut EventStore, session: &str) -> usize {
    record_refusal_verifications_in(store, session, transcripts_root().as_deref())
}

/// `record_refusal_verifications` against the transcripts under `root`.
pub fn record_refusal_verifications_in(store: &mut EventStore, session: &str, root: Option<&Path>) -> usize {
    let Ok(refusals) = store.events_of_kinds(&[EventKind::GateRefused], 0) else { return 0 };
    let mine: Vec<Event> = refusals.into_iter().filter(|e| e.session_id == session).collect();
    if mine.is_empty() {
        return 0;
    }
    let Ok(stood_aside) = store.events_of_kinds(&[EventKind::GateStoodAside], 0) else { return 0 };
    let done = model::store::recorded_refusal_verdicts(&stood_aside);
    let mut recorded = 0;
    for event in mine.iter().filter(|e| !done.contains_key(&e.seq)) {
        let Some(verdict) = verify_refusal(store, event, root) else { continue };
        let written = model::store::record_refusal_verification(
            store,
            session,
            SystemActor::new(ATTESTER_ACTOR),
            &event.entity_id,
            event.seq,
            verdict.field,
            &verdict.why,
        );
        if matches!(written, Ok(Some(_))) {
            recorded += 1;
        }
    }
    recorded
}

/// The seq from which `item`'s current check has stood unchanged on `events`
/// (the item's own log), or `i64::MAX` when the log does not hold that check.
pub fn check_since(events: &[Event], check: Option<&Check>) -> i64 {
    let mut logged: Option<Option<Check>> = None;
    let mut since = i64::MAX;
    for e in events {
        match e.kind {
            EventKind::FactCreated | EventKind::FactRevised => {
                let Ok(item) = model::store::parse_body(&e.body) else { continue };
                if logged.as_ref() != Some(&item.check) {
                    since = e.seq;
                    logged = Some(item.check);
                }
            }
            EventKind::FactRetracted => {
                logged = None;
                since = i64::MAX;
            }
            _ => {}
        }
    }
    if logged.as_ref().map(|c| c.as_ref()) == Some(check) {
        since
    } else {
        i64::MAX
    }
}

/// Whether a check reads a command (so a refused call can be tried again
/// against it) rather than a file.
fn reads_a_command(item: &Item) -> bool {
    match &item.check {
        Some(Check::Requires { .. }) => true,
        Some(Check::Forbidden { .. }) => {
            item.bindings.iter().any(|b| matches!(b, Binding::Target { kind: TargetKind::Command, .. }))
        }
        _ => false,
    }
}

/// THE ONE PREDICATE (K6): whether refusal `event` is field evidence for
/// `item`'s CURRENT check. All of:
/// - a `gate_refused` stamped by the hook, from a UUID-shaped session the log
///   shows other served activity for;
/// - after the current check was set (`since`, from `check_since`);
/// - carrying a tool call id;
/// - with a recorded `attester` verdict (`verdicts`, from
///   `model::store::recorded_refusal_verdicts`) saying the session's
///   transcript holds that call and the hook's block of it naming this rule;
/// - and, for a check that reads a command, the refused command is still
///   refused by the check as it stands - tried through the hook's own
///   pipeline (`teeth::refuses_call`), never a second matcher.
///
/// Nothing else ever counts: not an unrecorded refusal, however real, and
/// not a served event (a hand-fed payload can make those).
pub fn is_field_refusal(
    store: &EventStore,
    event: &Event,
    id: &str,
    item: &Item,
    since: i64,
    verdicts: &HashMap<i64, (bool, String)>,
) -> bool {
    is_field_refusal_known(store, event, id, item, since, verdicts, &|| crate::teeth::known_projects(store))
}

/// `is_field_refusal`, for a reader that asks it of many refusals: `known`
/// (the projects the store holds a live item of) is asked only for a check
/// that reads a command, so such a reader can answer from one reading.
pub fn is_field_refusal_known(
    store: &EventStore,
    event: &Event,
    id: &str,
    item: &Item,
    since: i64,
    verdicts: &HashMap<i64, (bool, String)>,
    known: &dyn Fn() -> Vec<String>,
) -> bool {
    if event.entity_id != id || item.check.is_none() || !is_attested_refusal(store, event, since, verdicts) {
        return false;
    }
    if reads_a_command(item) {
        let body = serde_json::from_str::<Value>(&event.body).unwrap_or(Value::Null);
        let target = body.get("target").and_then(|t| t.as_str()).unwrap_or_default();
        return crate::teeth::refuses_call_known(store, id, item, target, &known());
    }
    true
}

/// Every part of `is_field_refusal` that does not need the rule itself: a
/// hook-stamped refusal from a UUID-shaped session with other served
/// activity, after the check was set (`since`), carrying a tool call id, with
/// a recorded `attester` verdict saying field. A reader that tries the
/// refused call against a check of its own (`prove::bitten`) adds the last
/// part itself.
pub fn is_attested_refusal(store: &EventStore, event: &Event, since: i64, verdicts: &HashMap<i64, (bool, String)>) -> bool {
    if event.kind != EventKind::GateRefused || event.actor != ASSISTANT_DELIVERY_ACTOR {
        return false;
    }
    if since == i64::MAX || event.seq <= since || !is_uuid_shaped(&event.session_id) {
        return false;
    }
    let body = serde_json::from_str::<Value>(&event.body).unwrap_or(Value::Null);
    if body.get("tool_use_id").and_then(|t| t.as_str()).is_none_or(str::is_empty) {
        return false;
    }
    if !verdicts.get(&event.seq).is_some_and(|(field, _)| *field) {
        return false;
    }
    // A verdict recorded before step 20 said field for a self-proof too:
    // read the log again, so such a refusal never counts either.
    if self_proof(store, event).is_some() {
        return false;
    }
    // Last, being the one query of the log: the session has other activity.
    store.served_ids_in_session(&event.session_id).is_ok_and(|v| !v.is_empty())
}

/// The field refusals of `id` under `item`'s current check, oldest first:
/// (seq, the refused target). Reads the item's own log and the recorded
/// verdicts once.
pub fn field_refusals(store: &EventStore, id: &str, item: &Item) -> Vec<(i64, String)> {
    let Ok(events) = store.get_events_by_entity(id) else { return Vec::new() };
    let verdicts = model::store::recorded_refusal_verdicts(&events);
    let since = check_since(&events, item.check.as_ref());
    events
        .iter()
        .filter(|e| is_field_refusal(store, e, id, item, since, &verdicts))
        .map(|e| {
            let body = serde_json::from_str::<Value>(&e.body).unwrap_or(Value::Null);
            (e.seq, body.get("target").and_then(|t| t.as_str()).unwrap_or_default().to_string())
        })
        .collect()
}

// ---------------------------------------------------------- writes (step 12)

/// The THOR tools that write: the suffix after the last `__` of an MCP tool
/// name. The PreToolUse hook marks every call of one it lets through.
pub const WRITE_TOOLS: &[&str] = &["remember", "revise", "retract", "mark", "pin", "unpin", "resolve"];

/// The `_meta` key Claude Code puts the calling tool_use id under (step 1 of
/// the build, answer 1).
pub const META_TOOL_USE_ID: &str = "claudecode/toolUseId";

/// The words of THOR's PreToolUse marker for a write it lets through.
pub const WRITE_MARKER_WORDS: &str = "write attested:";

/// The THOR write tool `tool_name` calls (`mcp__<server>__<tool>`), if any.
pub fn write_tool(tool_name: &str) -> Option<&str> {
    let (server, tool) = tool_name.rsplit_once("__")?;
    (server.starts_with("mcp__") && WRITE_TOOLS.contains(&tool)).then_some(tool)
}

/// "write attested: <id> <first 16 hex of the argument digest>".
fn marker_words(tool_use_id: &str, digest: &str) -> String {
    format!("{WRITE_MARKER_WORDS} {tool_use_id} {}", digest.get(..16).unwrap_or(digest))
}

/// The line the PreToolUse hook appends to its additionalContext for a THOR
/// write it lets through: "[THOR] write attested: <tool_use_id> <16 hex>".
/// `None` for any other tool, and for a call with no tool_use id.
pub fn write_marker(tool_name: &str, tool_use_id: Option<&str>, tool_input: Option<&Value>) -> Option<String> {
    write_tool(tool_name)?;
    let id = tool_use_id.filter(|id| !id.is_empty())?;
    let empty = Value::Object(Default::default());
    let digest = model::store::args_digest(tool_input.unwrap_or(&empty));
    Some(format!("[THOR] {}", marker_words(id, &digest)))
}

/// What the writer saw of the call while it ran (see this module's doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InFlight {
    Seen,
    Unseen,
    SideDoor(String),
}

/// The writer's check (K21): SideDoor only on positive evidence, Unseen when
/// the call's tool_use line is not on disk (yet), Seen when it is there under
/// this tool with these arguments. `meta` is the call's `_meta` object,
/// `env_session` the server's `CLAUDE_CODE_SESSION_ID`, `args` the call's
/// arguments exactly as they arrived, `tool` the THOR tool being called.
pub fn check_call_in_flight(
    meta: &serde_json::Map<String, Value>,
    env_session: Option<&str>,
    args: &Value,
    tool: &str,
    root: Option<&Path>,
) -> InFlight {
    let Some(id) = meta.get(META_TOOL_USE_ID).and_then(|v| v.as_str()).filter(|id| !id.is_empty()) else {
        return InFlight::SideDoor("no toolUseId in the call's _meta".to_string());
    };
    let Some(session) = env_session.filter(|s| !s.trim().is_empty()) else {
        return InFlight::SideDoor("no CLAUDE_CODE_SESSION_ID in the server's environment".to_string());
    };
    let Some(root) = root else { return InFlight::Unseen };
    match find_call(&session_transcripts(root, session), id) {
        Found::Nothing => InFlight::Unseen,
        Found::Conflicting => {
            InFlight::SideDoor(format!("the transcript of session {session} holds two different tool calls under {id}"))
        }
        Found::Call(call) if write_tool(&call.name) != Some(tool) => {
            InFlight::SideDoor(format!("{id} is a {} call, not mcp__thor__{tool}", call.name))
        }
        Found::Call(call) if model::store::args_digest(&call.input) != model::store::args_digest(args) => {
            InFlight::SideDoor(format!("the arguments differ from the ones Claude Code sent for {id}"))
        }
        Found::Call(_) => InFlight::Seen,
    }
}

/// The stamp a write through the tool server carries: the call's tool_use id
/// and argument digest, the server's session hint, and `check_call_in_flight`'s
/// answer against the transcripts under `root`.
pub fn att_for_call(
    meta: &serde_json::Map<String, Value>,
    env_session: Option<&str>,
    args: &Value,
    tool: &str,
    root: Option<&Path>,
) -> Att {
    let (inflight, side_door) = match check_call_in_flight(meta, env_session, args, tool, root) {
        InFlight::Seen => (model::store::INFLIGHT_SEEN, None),
        InFlight::Unseen => (model::store::INFLIGHT_UNSEEN, None),
        InFlight::SideDoor(why) => (model::store::INFLIGHT_SIDE_DOOR, Some(why)),
    };
    Att {
        tool_use_id: meta.get(META_TOOL_USE_ID).and_then(|v| v.as_str()).filter(|id| !id.is_empty()).map(str::to_string),
        args_sha256: model::store::args_digest(args),
        session_hint: env_session.filter(|s| !s.trim().is_empty()).map(str::to_string),
        inflight: Some(inflight.to_string()),
        side_door,
    }
}

/// The reader's answer for one write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteAttestation {
    /// Made through a THOR call of `session` (by subagent `agent_id`, when one
    /// made it), found in `file`.
    Attested { session: String, agent_id: Option<String>, file: PathBuf },
    Unattested(String),
}

/// The THOR tools that append an event of `kind`.
fn tools_for(kind: EventKind) -> &'static [&'static str] {
    match kind {
        EventKind::FactCreated => &["remember"],
        EventKind::FactRevised => &["revise", "pin", "unpin"],
        EventKind::FactRetracted => &["retract"],
        EventKind::FactResolved => &["resolve"],
        EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise => &["mark"],
        _ => &[],
    }
}

/// What `verify_write` knows about a write before it reads any transcript:
/// the tool call id and argument digest its stamp claims.
pub(crate) struct Claim {
    pub(crate) tool_use_id: String,
    pub(crate) digest: String,
}

/// The parts of `verify_write` that need no transcript. `Err` is final.
pub(crate) fn claim_of(event: &Event) -> Result<Claim, String> {
    if tools_for(event.kind).is_empty() {
        return Err(format!("log seq {} is not a write a THOR tool makes", event.seq));
    }
    let Some(att) = model::store::att_of(&event.body) else {
        return Err("it carries no stamp of THOR's tool server - it was written by another program (the command line, a \
                    repair tool or an older build)"
            .to_string());
    };
    let Some(tool_use_id) = att.tool_use_id.filter(|id| !id.is_empty()) else {
        return Err("no toolUseId in the call's _meta".to_string());
    };
    if !is_uuid_shaped(&event.session_id) {
        return Err(format!("session '{}' is not a Claude Code session id", event.session_id));
    }
    Ok(Claim { tool_use_id, digest: att.args_sha256 })
}

/// Whether `text` names exactly `event seq <seq>` (not a longer number).
pub(crate) fn names_seq(text: &str, seq: i64) -> bool {
    let needle = format!("event seq {seq}");
    text.match_indices(&needle)
        .any(|(at, _)| !text[at + needle.len()..].starts_with(|c: char| c.is_ascii_digit()))
}

/// Judge one write against what the transcript holds for its call. `None`
/// while the call has no result yet (it is still running).
fn judge_write(event: &Event, claim: &Claim, found: Found) -> Option<WriteAttestation> {
    let session = &event.session_id;
    let id = &claim.tool_use_id;
    let no = |why: String| Some(WriteAttestation::Unattested(why));
    let call = match found {
        Found::Nothing => return no(format!("{id} is not a tool call in session {session}'s transcript")),
        Found::Conflicting => {
            return no(format!("the transcript of session {session} holds two different tool calls under {id}"))
        }
        Found::Call(call) => call,
    };
    let tools = tools_for(event.kind);
    if !write_tool(&call.name).is_some_and(|tool| tools.contains(&tool)) {
        let wanted: Vec<String> = tools.iter().map(|t| format!("mcp__thor__{t}")).collect();
        return no(format!("{id} is a {} call, not {}", call.name, wanted.join(" or ")));
    }
    if model::store::args_digest(&call.input) != claim.digest {
        return no("the arguments differ from the ones Claude Code sent".to_string());
    }
    if call.results.is_empty() {
        return None;
    }
    let marker = marker_words(id, &claim.digest);
    if !call.markers.iter().any(|m| m.contains(&marker)) {
        return no(format!(
            "the transcript holds no THOR hook marker for {id} - the call never passed THOR's PreToolUse hook"
        ));
    }
    if !call.results.iter().any(|(is_error, text)| !is_error && names_seq(text, event.seq)) {
        return no(format!("the result of {id} names no 'event seq {}' - it is not the call that made this write", event.seq));
    }
    Some(WriteAttestation::Attested { session: session.clone(), agent_id: call.agent_id, file: call.file })
}

/// THE READER (K21): whether write `event` was made through an attested THOR
/// call of its session, against the transcripts under `root` - see this
/// module's doc comment for the three lines it demands. `None` when it cannot
/// be decided yet: no transcript of the session on this machine, or the call
/// has no result yet. Every other answer is final and is what gets recorded.
pub fn verify_write(event: &Event, root: Option<&Path>) -> Option<WriteAttestation> {
    let claim = match claim_of(event) {
        Ok(claim) => claim,
        Err(why) => return Some(WriteAttestation::Unattested(why)),
    };
    let files = session_transcripts(root?, &event.session_id);
    if files.is_empty() {
        return None;
    }
    let found = find_call(&files, &claim.tool_use_id);
    judge_write(event, &claim, found)
}

/// Verify and record every write of `session` (its session column - the tool
/// server stamps each write with its session hint) that has no recorded
/// verification yet, against the transcripts under `transcripts_root()`. Runs
/// at every Stop of the session, `stop_hook_active` or not: recording never
/// holds a turn. Returns how many were recorded.
pub fn record_write_verifications(store: &mut EventStore, session: &str) -> usize {
    record_write_verifications_in(store, session, transcripts_root().as_deref())
}

/// `record_write_verifications` against the transcripts under `root`. Reads
/// the session's transcripts ONCE for all of its unverified writes.
pub fn record_write_verifications_in(store: &mut EventStore, session: &str, root: Option<&Path>) -> usize {
    record_writes_of(store, session, session, root, false)
}

/// FIELD TEST 5 (D5). Called when an evaluation report is being filed: attest,
/// right now, the writes of `session` whose three transcript lines are already
/// on disk (the call, THOR's hook marker, the result naming the write), so a
/// report that settles rules by writes to them does not have to end the turn
/// once just for the Stop to read the same lines. Only POSITIVE answers are
/// recorded: a call still running, or one that does not check out, is left for
/// the Stop to decide, so this can never mark a write as a side door early. A
/// verdict (a mark) is left to the Stop as before - the one turn between
/// reading an item and judging it. Returns how many it recorded.
pub fn attest_finished_writes(store: &mut EventStore, session: &str) -> usize {
    attest_finished_writes_in(store, session, transcripts_root().as_deref())
}

/// `attest_finished_writes` against the transcripts under `root`.
pub fn attest_finished_writes_in(store: &mut EventStore, session: &str, root: Option<&Path>) -> usize {
    record_writes_of(store, session, session, root, true)
}

/// THE WRITES OF ANOTHER SESSION ON AN OPEN EVALUATION'S LISTS (step 24),
/// checked at the Stop of the evaluating `session`: for every evaluation it
/// has open, every write by another session to an item on its lists that no
/// verification covers yet is checked against THAT session's transcript, and
/// the verdict recorded under this session's name (`record_write_verification_by`).
/// An attested one is credited to that session and voids nothing; one that
/// fails, or whose session has no transcript on this machine, is a side door.
/// So a filing never waits on a session that went quiet. Returns how many it
/// recorded.
pub fn record_foreign_write_verifications(store: &mut EventStore, session: &str) -> usize {
    record_foreign_write_verifications_in(store, session, transcripts_root().as_deref())
}

/// `record_foreign_write_verifications` against the transcripts under `root`.
pub fn record_foreign_write_verifications_in(store: &mut EventStore, session: &str, root: Option<&Path>) -> usize {
    let Some(root) = root else { return 0 };
    let now = crate::time::now_unix();
    let mut writers: std::collections::BTreeMap<String, Vec<(String, i64)>> = std::collections::BTreeMap::new();
    for start in model::store::evaluation_starts_of_session(store, session) {
        if !model::store::evaluation_still_open(store, &start, now) {
            continue;
        }
        for (id, seq, writer) in model::store::frozen_writes_in_window(store, &start, None).foreign_pending {
            writers.entry(writer).or_default().push((id, seq));
        }
    }
    let mut recorded = 0;
    for (writer, writes) in writers {
        if session_transcripts(root, &writer).is_empty() {
            for (id, seq) in writes {
                let why = format!("no transcript of session {writer} is on this machine, so its call cannot be checked");
                let written = model::store::record_write_verification_by(
                    store,
                    session,
                    &writer,
                    SystemActor::new(ATTESTER_ACTOR),
                    &id,
                    seq,
                    false,
                    None,
                    &why,
                );
                if matches!(written, Ok(Some(_))) {
                    recorded += 1;
                }
            }
            continue;
        }
        recorded += record_writes_of(store, &writer, session, Some(root), false);
    }
    recorded
}

/// Verify every unverified write of `session` against its transcripts under
/// `root`, recording each verdict under `recorder`'s name. With
/// `positive_only` (`attest_finished_writes`): only the attested answers are
/// recorded, and verdicts (marks) are not looked at.
fn record_writes_of(store: &mut EventStore, session: &str, recorder: &str, root: Option<&Path>, positive_only: bool) -> usize {
    if session.is_empty() {
        return 0;
    }
    let Ok(writes) = store.events_of_kinds_in_session(model::store::ATTESTED_WRITE_KINDS, session) else { return 0 };
    if writes.is_empty() {
        return 0;
    }
    let Ok(records) = store.events_of_kinds_in_session(&[EventKind::GateStoodAside], session) else { return 0 };
    let done = model::store::recorded_write_verdicts(&records);
    let pending: Vec<(Event, Result<Claim, String>)> = writes
        .into_iter()
        .filter(|e| !done.contains_key(&e.seq))
        .filter(|e| !(positive_only && matches!(e.kind, EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise)))
        // THOR's OWN WRITES (its installer's migration, its machine pass) are
        // made by no tool call, so there is nothing to attest and nothing to
        // call a side door: the actor is the stamp (step 27, D10). Field case
        // 2026-10-01: a migration's three revises were recorded as "written
        // by another program" and read as writes that bypassed the tools.
        .filter(|e| !model::store::RESERVED_ACTORS.contains(&e.actor.as_str()))
        .map(|e| {
            let claim = claim_of(&e);
            (e, claim)
        })
        .collect();
    let ids: HashSet<&str> = pending.iter().filter_map(|(_, c)| c.as_ref().ok().map(|c| c.tool_use_id.as_str())).collect();
    let files = match (root, ids.is_empty()) {
        (Some(root), false) => session_transcripts(root, session),
        _ => Vec::new(),
    };
    // Read, never taken: two writes can claim one tool call id (a replay of a
    // real id), and each is judged against the same call.
    let found = find_calls(&files, &ids);
    let mut verdicts: Vec<(Event, WriteAttestation)> = Vec::new();
    for (event, claim) in &pending {
        let verdict = match claim {
            Err(why) => Some(WriteAttestation::Unattested(why.clone())),
            Ok(_) if files.is_empty() => None,
            Ok(claim) => judge_write(event, claim, found.get(&claim.tool_use_id).cloned().unwrap_or(Found::Nothing)),
        };
        if let Some(verdict) = verdict {
            if positive_only && !matches!(verdict, WriteAttestation::Attested { .. }) {
                continue;
            }
            verdicts.push((event.clone(), verdict));
        }
    }
    let mut recorded = 0;
    for (event, verdict) in verdicts {
        let (attested, agent_id, why) = match verdict {
            WriteAttestation::Attested { session, agent_id, file } => {
                let file = file.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                (true, agent_id, format!("session {session}'s transcript ({file}) holds the call, THOR's hook marker for it and its result naming event seq {}", event.seq))
            }
            WriteAttestation::Unattested(why) => (false, None, why),
        };
        let written = model::store::record_write_verification_by(
            store,
            recorder,
            session,
            SystemActor::new(ATTESTER_ACTOR),
            &event.entity_id,
            event.seq,
            attested,
            agent_id.as_deref(),
            &why,
        );
        if matches!(written, Ok(Some(_))) {
            recorded += 1;
        }
    }
    recorded
}

// ------------------------------------------ the work clock's cross-check
//
// STEP 17 (B8, N3). The work clock lives in a sidecar any local process can
// write: zeroing a session's minutes there used to keep it from ever being
// asked for the evaluation. The session's own transcript says how long it
// really worked: Claude Code stamps every line with a `timestamp` and the
// working folder (`cwd`) it was written in. The Stop hook reads it once per
// Stop, bounded to the tail of each file, and takes the larger of the two.
// Forging a transcript is a forged artifact (N9); it can only ever make the
// ask come SOONER here, since only the larger number is used.

/// How much of each transcript file the work clock reads, from its end: one
/// bounded read per Stop, however long the session ran.
pub const TRANSCRIPT_TAIL_BYTES: u64 = 8 * 1024 * 1024;

/// One line of a session's transcript that carries a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedLine {
    pub unix: i64,
    /// The project the line was written in: its `cwd`, resolved. A line whose
    /// folder resolves to no project, or that names none, keeps the project of
    /// the line before it (the session's own fallback, D4); `None` when no
    /// line before it named one - such a line counts for any project.
    pub project: Option<String>,
    /// A prompt the user typed, so a turn began: a main-session user line
    /// whose content is text - not a tool result, a hook's feedback or a meta
    /// line.
    pub prompt: bool,
}

/// The fields of a transcript line the work clock reads; the rest is skipped
/// unparsed.
#[derive(serde::Deserialize)]
struct LineTimes {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default, rename = "isSidechain")]
    sidechain: bool,
    #[serde(default, rename = "isMeta")]
    meta: bool,
    #[serde(default, rename = "isCompactSummary")]
    compact_summary: bool,
}

/// Whether a main-session `user` line is a typed prompt: its content is text
/// (a string, or text blocks and no tool result), and not the feedback a
/// Stop hook's block leaves.
fn is_typed_prompt(line: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return false };
    let text = match v.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => {
            if blocks.iter().any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result")) {
                return false;
            }
            blocks.iter().filter_map(|b| b.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join("\n")
        }
        _ => return false,
    };
    let text = text.trim_start();
    !text.is_empty() && !text.starts_with("Stop hook feedback") && !text.starts_with("[Request interrupted")
}

// ------------------------------------------- a routine changed since read

/// THE ROUTINE CHANGED SINCE IT WAS READ (step 26, field test 2 defect 5): a
/// session read the evaluation routine once, the routine was rewritten an
/// hour later, and every Stop after that said "read ... and do what it says"
/// without saying the file had changed - the session went on by the old text.
///
/// `command_file` is the routine the Stop names; every absolute path to an
/// existing `.md` file its text names is a routine too (a machine whose
/// `thor-eval.md` says "read <home>/.claude/commands/t-eval.md"). For each one
/// read before in `transcript` (a Read of the file, the skill or slash command
/// of its name) whose file changed after that last read, one line says so.
/// Never read at all: nothing to say - the ask says to read it anyway.
pub fn routine_reread_lines(command_file: &Path, transcript: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(command_file) else { return Vec::new() };
    let mut files = vec![command_file.to_path_buf()];
    for token in text.split_whitespace() {
        let token = token.trim_matches(|c: char| matches!(c, '`' | '\'' | '"' | '(' | ')' | ',' | ';' | ':' | '.' | '<' | '>'));
        let path = PathBuf::from(token);
        if token.to_ascii_lowercase().ends_with(".md") && path.is_absolute() && path.is_file() && !files.contains(&path) {
            files.push(path);
        }
    }
    let reads = routine_reads(transcript);
    let mut out = Vec::new();
    for file in files {
        let Some(changed) = std::fs::metadata(&file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
        else {
            continue;
        };
        let folded = fold_path(&file.to_string_lossy());
        let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
        let last = reads.iter().filter(|(key, _)| *key == folded || *key == format!("name:{stem}")).map(|(_, at)| *at).max();
        let Some(read_at) = last else { continue };
        if changed > read_at + 2 {
            let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("the routine");
            out.push(format!(
                "[THOR] {name} changed since you last read it (read {}, changed {}) - read it again before you go on: the \
                 steps you remember may not be the ones in that file now.",
                crate::time::iso8601_from_unix(read_at),
                crate::time::iso8601_from_unix(changed)
            ));
        }
    }
    out
}

/// A path as compared here: forward slashes, lowercase.
fn fold_path(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

/// Every read of a routine in `transcript`, with when: a Read tool call
/// (its folded file path), a Skill call or a typed slash command
/// (`name:<its name>`).
fn routine_reads(transcript: &Path) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for line in tail_lines(transcript) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(at) = v.get("timestamp").and_then(|t| t.as_str()).and_then(crate::eval_filing::unix_of_timestamp) else {
            continue;
        };
        let content = v.get("message").and_then(|m| m.get("content"));
        match v.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                for block in content.and_then(|c| c.as_array()).into_iter().flatten() {
                    if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                        continue;
                    }
                    let input = block.get("input");
                    match block.get("name").and_then(|n| n.as_str()) {
                        Some("Read") => {
                            if let Some(path) = input.and_then(|i| i.get("file_path")).and_then(|p| p.as_str()) {
                                out.push((fold_path(path), at));
                            }
                        }
                        Some("Skill") => {
                            if let Some(skill) = input.and_then(|i| i.get("skill")).and_then(|s| s.as_str()) {
                                let name = skill.rsplit(':').next().unwrap_or(skill).trim_start_matches('/').to_ascii_lowercase();
                                out.push((format!("name:{name}"), at));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                let text = match content {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(blocks)) => {
                        blocks.iter().filter_map(|b| b.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join("\n")
                    }
                    _ => continue,
                };
                if let Some(rest) = text.split("<command-name>").nth(1) {
                    let name = rest.split("</command-name>").next().unwrap_or("").trim().trim_start_matches('/');
                    if !name.is_empty() {
                        out.push((format!("name:{}", name.to_ascii_lowercase()), at));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

// ------------------------------------------------- the owner's own words

/// THE OWNER'S OWN WORDS (step 26): the text of the last `last` messages the
/// owner himself typed in the session whose main transcript is `transcript`,
/// oldest first - what a rule marked `model::store::OWNER_APPROVAL_TAG` is
/// answered by. A message counts only when it is a real user turn: a `user`
/// line of the main transcript that is no sidechain, no meta line (a Stop
/// hook's feedback, a subagent's hand-back) and no compact summary, whose
/// `origin` - when the line carries one - is `human`, and whose content is
/// text, not a tool result. Inside it, quoted lines (`>`) and fenced blocks
/// are not his own words and are left out. `Err` names why the transcript
/// cannot be read: the caller then refuses (fail closed).
pub fn owner_messages(transcript: &Path, last: usize) -> Result<Vec<String>, String> {
    if !transcript.is_file() {
        return Err(format!("no transcript at {}", transcript.display()));
    }
    let lines = tail_lines(transcript);
    if lines.is_empty() {
        return Err(format!("the transcript at {} could not be read", transcript.display()));
    }
    let mut out: Vec<String> = lines.iter().filter_map(|line| owner_text(line)).collect();
    let skip = out.len().saturating_sub(last);
    Ok(out.split_off(skip))
}

/// The owner's own words on one transcript line, or `None` when the line is
/// not a message he typed (see `owner_messages`).
fn owner_text(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type").and_then(|t| t.as_str()) != Some("user") {
        return None;
    }
    let flag = |name: &str| v.get(name).and_then(|f| f.as_bool()).unwrap_or(false);
    if flag("isSidechain") || flag("isMeta") || flag("isCompactSummary") {
        return None;
    }
    if let Some(origin) = v.get("origin") {
        if origin.get("kind").and_then(|k| k.as_str()) != Some("human") {
            return None;
        }
    }
    let text = match v.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => {
            if blocks.iter().any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result")) {
                return None;
            }
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => return None,
    };
    let start = text.trim_start();
    let injected = [
        "Stop hook feedback",
        "[Request interrupted",
        "<task-notification>",
        "Another Claude session sent a message",
        "[Subagent hand-back]",
        "<local-command-stdout>",
        "Caveat: The messages below",
    ];
    if start.is_empty() || injected.iter().any(|p| start.starts_with(p)) {
        return None;
    }
    let mut own = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced || trimmed.starts_with('>') {
            continue;
        }
        own.push(line);
    }
    Some(own.join("\n"))
}

/// Whether `text` says `marker` as a word of its own: case ignored, with no
/// letter, digit, `_` or `-` right before or after it ("go-owner" is not said
/// by "go-owners" or "nogo-owner").
pub fn says_marker(text: &str, marker: &str) -> bool {
    let marker = marker.trim().to_ascii_lowercase();
    if marker.is_empty() {
        return false;
    }
    let hay = text.to_ascii_lowercase();
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    let mut from = 0;
    while let Some(at) = hay[from..].find(&marker).map(|i| i + from) {
        let before = hay[..at].chars().next_back();
        let after = hay[at + marker.len()..].chars().next();
        if !before.is_some_and(word) && !after.is_some_and(word) {
            return true;
        }
        from = at + 1;
        while !hay.is_char_boundary(from) {
            from += 1;
        }
    }
    false
}

/// The main transcript of `session`: `named` (a hook payload's
/// `transcript_path`) when it is that session's own main file, else the one
/// `session_transcripts` finds under `root`.
pub fn main_transcript_of(named: Option<&Path>, root: Option<&Path>, session: &str) -> Option<PathBuf> {
    let own = |p: &Path| p.file_stem().and_then(|s| s.to_str()) == Some(session) && p.is_file();
    if let Some(path) = named.filter(|p| own(p)) {
        return Some(path.to_path_buf());
    }
    session_transcripts(root?, session).into_iter().find(|p| own(p))
}

/// The last `TRANSCRIPT_TAIL_BYTES` of `file`, as whole lines.
fn tail_lines(file: &Path) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut handle) = std::fs::File::open(file) else { return Vec::new() };
    let len = handle.metadata().map(|m| m.len()).unwrap_or(0);
    let cut = len > TRANSCRIPT_TAIL_BYTES;
    if cut && handle.seek(SeekFrom::Start(len - TRANSCRIPT_TAIL_BYTES)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if handle.take(TRANSCRIPT_TAIL_BYTES).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if cut && !lines.is_empty() {
        lines.remove(0);
    }
    lines
}

/// Every timed line of `session`'s transcripts under `root` (its main file
/// and every subagent file, the tail of each), oldest first, each with the
/// project it was written in. Empty for a session that is not UUID-shaped or
/// has no transcript.
pub fn session_timeline(root: &Path, session: &str) -> Vec<TimedLine> {
    let mut raw: Vec<(i64, Option<String>, bool)> = Vec::new();
    for file in session_transcripts(root, session) {
        let main = file.file_stem().and_then(|s| s.to_str()) == Some(session);
        for line in tail_lines(&file) {
            let Ok(fields) = serde_json::from_str::<LineTimes>(&line) else { continue };
            let Some(unix) = fields.timestamp.as_deref().and_then(crate::eval_filing::unix_of_timestamp) else { continue };
            let prompt = main
                && fields.kind.as_deref() == Some("user")
                && !fields.sidechain
                && !fields.meta
                && !fields.compact_summary
                && is_typed_prompt(&line);
            raw.push((unix, fields.cwd, prompt));
        }
    }
    raw.sort_by_key(|(unix, _, _)| *unix);
    let mut resolved: HashMap<String, Option<String>> = HashMap::new();
    let mut current: Option<String> = None;
    raw.into_iter()
        .map(|(unix, cwd, prompt)| {
            if let Some(cwd) = cwd {
                let project =
                    resolved.entry(cwd.clone()).or_insert_with(|| crate::project::resolve_project(Path::new(&cwd))).clone();
                if project.is_some() {
                    current = project;
                }
            }
            TimedLine { unix, project: current.clone(), prompt }
        })
        .collect()
}

/// Minutes of work in `project` a timeline shows since `since_unix` (the
/// project's newest valid evaluation; `None`: all of it): its lines of that
/// project (or of no known one), and every gap between two consecutive ones,
/// both after `since_unix`, that is under `EVAL_PAUSE_MINUTES` - the rule the
/// sidecar's own clock folds by. Several sessions' lines merged count a
/// parallel minute once.
pub fn minutes_in<'a>(lines: impl IntoIterator<Item = &'a TimedLine>, project: &str, since_unix: Option<i64>) -> i64 {
    let mut times: Vec<i64> = lines
        .into_iter()
        .filter(|l| l.project.as_deref().is_none_or(|p| p == project))
        .map(|l| l.unix)
        .filter(|t| since_unix.is_none_or(|since| *t > since))
        .collect();
    times.sort_unstable();
    let pause = crate::usefulness::EVAL_PAUSE_MINUTES * 60;
    let secs: i64 = times.windows(2).map(|w| w[1] - w[0]).filter(|gap| (0..pause).contains(gap)).sum();
    secs / 60
}

/// This session's own minutes in `project` since `since_unix`, from its
/// transcript tail under `root` - `minutes_in` of `session_timeline`.
pub fn session_minutes_from_transcript(root: &Path, session: &str, project: &str, since_unix: Option<i64>) -> i64 {
    minutes_in(&session_timeline(root, session), project, since_unix)
}

/// Whether the timeline shows a turn of the session that began in `project`
/// (or in no known project) before its newest turn: that turn ended with a
/// Stop, so the Stop now is not the session's first there (Q10).
pub fn earlier_turn_in(timeline: &[TimedLine], project: &str) -> bool {
    let prompts: Vec<&TimedLine> = timeline.iter().filter(|l| l.prompt).collect();
    prompts.len() >= 2
        && prompts[..prompts.len() - 1].iter().any(|l| l.project.as_deref().is_none_or(|p| p == project))
}

/// Every project the timeline attributes a line to, in the order first seen.
pub fn projects_in(timeline: &[TimedLine]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in timeline.iter().filter_map(|l| l.project.as_ref()) {
        if !out.contains(p) {
            out.push(p.clone());
        }
    }
    out
}

/// TEST SUPPORT, shared by every crate that needs a refusal to count: writes
/// Claude Code transcript fixtures in the order a real session writes them
/// (step 1 of the build, answers 5 and 7), and the whole chain a counting
/// refusal needs. Writes only under the folder it is given.
pub mod fixture {
    use super::*;

    /// The project folder name fixtures write under (any name works: the
    /// reader walks every folder under the root).
    pub const PROJECT_FOLDER: &str = "C--work-fixture";

    /// A transcript file being written, line by line. Every line carries a
    /// `timestamp`, as Claude Code writes one on each: the time set with
    /// `at`, else the moment the line is added.
    pub struct Transcript {
        path: PathBuf,
        session: String,
        lines: Vec<String>,
        next: usize,
        at: Option<String>,
    }

    /// The main transcript of `session` under `root`.
    pub fn main_transcript(root: &Path, session: &str) -> Transcript {
        let path = root.join(PROJECT_FOLDER).join(format!("{session}.jsonl"));
        Transcript { path, session: session.to_string(), lines: Vec::new(), next: 0, at: None }
    }

    /// A subagent's transcript of `session` under `root` (an Agent-tool
    /// subagent's layout; a workflow agent's sits one folder deeper, and the
    /// reader finds both).
    pub fn subagent_transcript(root: &Path, session: &str, agent: &str) -> Transcript {
        let path = root.join(PROJECT_FOLDER).join(session).join("subagents").join(format!("agent-{agent}.jsonl"));
        Transcript { path, session: session.to_string(), lines: Vec::new(), next: 0, at: None }
    }

    /// `minutes` of work in the folder `cwd` as `session`'s main transcript
    /// under `root` holds it (the work clock's cross-check, step 17): a
    /// typed prompt at `from_unix`, then one assistant line a minute up to
    /// `from_unix + minutes * 60`, each with its `timestamp` and `cwd`, the
    /// way Claude Code stamps every line. Appends; returns the file.
    pub fn work_in(root: &Path, session: &str, cwd: &Path, from_unix: i64, minutes: i64) -> PathBuf {
        let mut transcript = main_transcript(root, session);
        let line = |kind: &str, content: Value, at: i64| {
            serde_json::json!({
                "type": kind,
                "message": {"role": kind, "content": content},
                "sessionId": session,
                "cwd": cwd.to_string_lossy(),
                "timestamp": crate::time::iso8601_from_unix(at),
            })
            .to_string()
        };
        transcript.raw(&line("user", Value::String("fixture prompt: carry on with the work".to_string()), from_unix));
        for m in 1..=minutes {
            let at = from_unix + m * 60;
            transcript.raw(&line("assistant", serde_json::json!([{"type": "text", "text": "working"}]), at));
        }
        transcript.write()
    }

    impl Transcript {
        fn uuid(&mut self) -> String {
            self.next += 1;
            format!("00000000-0000-4000-8000-{:012x}", self.next)
        }

        fn push(&mut self, mut line: Value) {
            line["uuid"] = Value::String(self.uuid());
            line["sessionId"] = Value::String(self.session.clone());
            let at = self.at.clone().unwrap_or_else(crate::time::now_iso8601);
            line["timestamp"] = Value::String(at);
            self.lines.push(line.to_string());
        }

        /// Stamp every line added from here on with `timestamp`.
        pub fn at(&mut self, timestamp: &str) -> &mut Self {
            self.at = Some(timestamp.to_string());
            self
        }

        /// A Stop hook's block, as Claude Code records it (step 1 of the
        /// build, answer 8): the feedback line, then the hook_blocking_error
        /// attachment carrying the reason.
        pub fn stop_block(&mut self, reason: &str) -> &mut Self {
            self.push(serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": format!("Stop hook feedback:
{reason}")}
            }));
            self.push(serde_json::json!({
                "type": "attachment",
                "attachment": {"type": "hook_blocking_error", "hookName": "Stop", "hookEvent": "Stop",
                               "toolUseID": "00000000-0000-4000-9000-000000000001",
                               "blockingError": {"blockingError": reason, "command": "serve hook"}}
            }));
            self
        }

        /// A UserPromptSubmit hook's additional context, as Claude Code
        /// records it (step 1 of the build, answer 8).
        pub fn prompt_context(&mut self, context: &str) -> &mut Self {
            self.push(serde_json::json!({
                "type": "attachment",
                "attachment": {"type": "hook_additional_context", "content": [context], "hookName": "UserPromptSubmit",
                               "toolUseID": "hook-00000000-0000-4000-9000-000000000002", "hookEvent": "UserPromptSubmit"}
            }));
            self
        }

        /// A raw line, exactly as given - for a transcript in a shape THOR
        /// does not know.
        pub fn raw(&mut self, line: &str) -> &mut Self {
            self.lines.push(line.to_string());
            self
        }

        fn tool_use(&mut self, id: &str, tool: &str, input: &Value) {
            self.push(serde_json::json!({
                "type": "assistant",
                "message": {"role": "assistant", "content": [
                    {"type": "tool_use", "id": id, "name": tool, "input": input}
                ]}
            }));
        }

        /// A call THOR's hook blocked: the tool_use, then the error result
        /// "PreToolUse:<tool> hook error: <reason>" - and nothing between
        /// them (a block leaves no hook attachment).
        pub fn block(&mut self, id: &str, tool: &str, input: &Value, reason: &str) -> &mut Self {
            self.tool_use(id, tool, input);
            self.push(serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "content": format!("PreToolUse:{tool} hook error: {reason}"),
                     "is_error": true, "tool_use_id": id}
                ]}
            }));
            self
        }

        /// A call that was let through: the tool_use, then its hook
        /// attachments (success, then the additional context), then its
        /// result.
        pub fn let_through(&mut self, id: &str, tool: &str, input: &Value, context: &str, result: &str) -> &mut Self {
            self.tool_use(id, tool, input);
            self.hook_context(id, tool, context).result(id, result)
        }

        /// The call's tool_use line alone: what is on disk while it runs.
        pub fn call(&mut self, id: &str, tool: &str, input: &Value) -> &mut Self {
            self.tool_use(id, tool, input);
            self
        }

        /// The PreToolUse hook's two attachments for call `id` it let
        /// through, written after the tool finished (step 1 of the build,
        /// answer 5): success with its stdout, then the additional context.
        pub fn hook_context(&mut self, id: &str, tool: &str, context: &str) -> &mut Self {
            self.push(serde_json::json!({
                "type": "attachment",
                "attachment": {"type": "hook_success", "hookName": format!("PreToolUse:{tool}"), "toolUseID": id,
                               "hookEvent": "PreToolUse", "content": "", "stdout": context}
            }));
            self.push(serde_json::json!({
                "type": "attachment",
                "attachment": {"type": "hook_additional_context", "content": [context], "hookName": format!("PreToolUse:{tool}"),
                               "toolUseID": id, "hookEvent": "PreToolUse"}
            }));
            self
        }

        /// The result of call `id`.
        pub fn result(&mut self, id: &str, text: &str) -> &mut Self {
            self.push(serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": [
                    {"tool_use_id": id, "type": "tool_result", "content": [{"type": "text", "text": text}]}
                ]}
            }));
            self
        }

        /// Append the lines to the file (creating its folders), and return
        /// its path.
        pub fn write(&mut self) -> PathBuf {
            use std::io::Write;
            std::fs::create_dir_all(self.path.parent().expect("a transcript sits in a folder")).expect("fixture folder");
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&self.path).expect("fixture file");
            for line in self.lines.drain(..) {
                writeln!(file, "{line}").expect("fixture line");
            }
            self.path.clone()
        }
    }

    /// The hook's own wording of a refusal naming `rule_id`.
    pub fn block_reason(rule_id: &str, what: &str) -> String {
        format!("[THOR] rule {rule_id} forbids this text (\"fixture rule\"); found in what is about to be written: \"{what}\"")
    }

    /// A refusal that counts, end to end: `session` is served `rule_id`
    /// once, the hook refuses call `tool_use_id` (`tool`, `input`) filed
    /// under `target`, the session's transcript under `root` holds the call
    /// and its block, and the verification is recorded as a Stop would.
    /// Returns the refusal's log seq.
    #[allow(clippy::too_many_arguments)]
    pub fn field_refusal(
        store: &mut EventStore,
        root: &Path,
        session: &str,
        rule_id: &str,
        tool_use_id: &str,
        tool: &str,
        input: Value,
        target: &str,
    ) -> i64 {
        crate::deliver::record_delivery_with_trigger(
            store,
            session,
            session,
            ASSISTANT_DELIVERY_ACTOR,
            "2026-09-28T00:00:00Z",
            &[rule_id.to_string()],
            Some(target),
        );
        crate::deliver::record_gate_outcome_for_call(
            store,
            session,
            session,
            ASSISTANT_DELIVERY_ACTOR,
            "2026-09-28T00:00:01Z",
            true,
            rule_id,
            target,
            Some(tool_use_id),
        );
        let seq = store.get_events_by_entity(rule_id).expect("fixture log").last().expect("the refusal").seq;
        main_transcript(root, session).block(tool_use_id, tool, &input, &block_reason(rule_id, target)).write();
        record_refusal_verifications_in(store, session, Some(root));
        seq
    }

    /// A UUID-shaped session id for fixtures that need a transcript.
    pub const SESSION: &str = "0de7dc52-1b97-4b66-948f-61e4438043b2";

    /// A call's `_meta` naming tool call `tool_use_id`.
    pub fn meta(tool_use_id: &str) -> serde_json::Map<String, Value> {
        let mut meta = serde_json::Map::new();
        meta.insert(META_TOOL_USE_ID.to_string(), Value::String(tool_use_id.to_string()));
        meta
    }

    /// A verdict made through THOR's `mark` tool exactly as Claude Code
    /// makes it - the tool_use line, the stamped write, the hook's marker,
    /// the result naming its seq - in `session`'s transcript under `root`,
    /// NOT yet verified (no Stop has run). Returns the verdict's log seq.
    pub fn attested_mark_unverified(
        store: &mut EventStore,
        root: &Path,
        session: &str,
        id: &str,
        noise: bool,
        tool_use_id: &str,
    ) -> i64 {
        let tool = "mcp__thor__mark";
        let args = if noise { serde_json::json!({"id": id, "noise": true}) } else { serde_json::json!({"id": id}) };
        let mut transcript = main_transcript(root, session);
        transcript.call(tool_use_id, tool, &args).write();
        let att = att_for_call(&meta(tool_use_id), Some(session), &args, "mark", Some(root));
        let now = "2026-09-28T00:00:02Z";
        let event = if noise {
            crate::mark::record_noise(store, session, "mcp", "mcp", now, id, Some(&att))
        } else {
            crate::mark::record_useful(store, session, "mcp", "mcp", now, id, Some(&att))
        }
        .expect("fixture verdict");
        let marker = write_marker(tool, Some(tool_use_id), Some(&args)).expect("a THOR write");
        transcript.hook_context(tool_use_id, tool, &marker).result(tool_use_id, &format!("marked: {id} (event seq {})", event.seq));
        transcript.write();
        event.seq
    }

    /// `attested_mark_unverified`, then the session's Stop that verifies it.
    pub fn attested_mark(store: &mut EventStore, root: &Path, session: &str, id: &str, noise: bool, tool_use_id: &str) -> i64 {
        let seq = attested_mark_unverified(store, root, session, id, noise, tool_use_id);
        record_write_verifications_in(store, session, Some(root));
        seq
    }

    /// A QUIET EVALUATION START of `project` by `session`, right now: the
    /// start record with the judgement list the log really gives
    /// (`model::store::frozen_now`), then the batch record of a machine pass
    /// that left nothing for the agent. For a fixture that needs a report ON
    /// FILE and is not about the machine pass (`eval_session::start` runs the
    /// real one). The evaluation already open is returned untouched.
    pub fn quiet_start(store: &mut EventStore, session: &str, project: &str) -> model::store::EvaluationStart {
        quiet_start_at(store, session, project, crate::time::now_unix())
    }

    /// `quiet_start`, started at unix time `started_unix` as far as the start
    /// record tells - an evaluation that has been running for a while.
    pub fn quiet_start_at(store: &mut EventStore, session: &str, project: &str, started_unix: i64) -> model::store::EvaluationStart {
        if let Some(open) = model::store::open_evaluation_start(store, session, project) {
            return open;
        }
        let frozen = model::store::frozen_now(store, session, project, "command", started_unix).expect("fixture log");
        let item = Item {
            id: model::store::evaluation_start_id(project, frozen.started_seq),
            kind: model::item::Kind::Report,
            text: format!("fixture start note of {project}"),
            bindings: vec![],
            severity: None,
            project: Some(project.to_string()),
            tags: vec![model::store::EVALUATION_START_TAG.to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        let actor = || SystemActor::new(model::store::EVAL_MACHINE_ACTOR);
        let start = model::store::record_evaluation_start(store, actor(), &item, &frozen).expect("fixture start record");
        model::store::record_evaluation_batch(
            store,
            actor(),
            &start,
            "fixture start note, decided",
            vec![],
            0,
            vec![],
            model::store::MachinePass::default(),
        )
        .expect("fixture batch record")
    }

    /// AN EVALUATION START THAT HANDS THE LIVE BATCH TO THE AGENT, for a
    /// fixture about the filing gate itself: `session` starts evaluating
    /// `project` right now, and the batch record freezes what
    /// `model::store::evaluation_teeth_batch` owes at that moment - every
    /// rule on it handed to the agent - as THOR's machine pass would when it
    /// can decide none of them. The real pass lives in `eval_session::start`.
    pub fn live_batch_start(store: &mut EventStore, session: &str, project: &str) -> model::store::EvaluationStart {
        let live = model::store::evaluation_teeth_batch(store, Some(project));
        let frozen = model::store::frozen_now(store, session, project, "command", crate::time::now_unix()).expect("fixture log");
        let item = Item {
            id: model::store::evaluation_start_id(project, frozen.started_seq),
            kind: model::item::Kind::Report,
            text: format!("fixture start note of {project}"),
            bindings: vec![],
            severity: None,
            project: Some(project.to_string()),
            tags: vec![model::store::EVALUATION_START_TAG.to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        let actor = || SystemActor::new(model::store::EVAL_MACHINE_ACTOR);
        let start = model::store::record_evaluation_start(store, actor(), &item, &frozen).expect("fixture start record");
        let items: Vec<model::store::MachineItem> = live
            .owed_now
            .iter()
            .chain(&live.owed_later)
            .map(|(id, why)| model::store::MachineItem {
                id: id.clone(),
                outcome: model::store::MachineOutcome::NeedsAgent,
                detail: why.clone(),
                call: None,
                blocker: None,
                seq: None,
            })
            .collect();
        let machine = model::store::MachinePass { needs_agent: items.len(), items, ..model::store::MachinePass::default() };
        let batch = live.owed_now.iter().map(|(id, _)| id.clone()).collect();
        model::store::record_evaluation_batch(store, actor(), &start, "fixture start note, decided", batch, live.owed_later.len(), vec![], machine)
            .expect("fixture batch record")
    }

    /// Close every other session's open evaluation of `project` as
    /// superseded by `session`, the way a start finds one whose session went
    /// quiet (step 24: one project runs one evaluation at a time) - for a
    /// fixture about what one start leaves the next, not about two at once.
    pub fn supersede_others(store: &mut EventStore, session: &str, project: &str) {
        let now = crate::time::now_unix();
        for other in model::store::open_evaluation_starts_of_project(store, project, now) {
            if other.frozen.session == session {
                continue;
            }
            let superseded = model::store::FrozenSuperseded { by_session: session.to_string(), at_seq: 0, idle_minutes: None };
            let actor = SystemActor::new(model::store::EVAL_MACHINE_ACTOR);
            model::store::record_evaluation_superseded(store, actor, &other, &superseded).expect("fixture: superseded");
        }
    }

    /// Record every write of `session` that has no recorded verification yet
    /// as an attested THOR call of it - what the Stop of a session whose
    /// transcript holds every one of its calls records. For a fixture whose
    /// transcript lines are not the point. Returns how many it recorded.
    pub fn attest_every_write(store: &mut EventStore, session: &str) -> usize {
        let writes = store.events_of_kinds_in_session(model::store::ATTESTED_WRITE_KINDS, session).expect("fixture log");
        let records = store.events_of_kinds_in_session(&[EventKind::GateStoodAside], session).expect("fixture log");
        let done = model::store::recorded_write_verdicts(&records);
        let mut recorded = 0;
        for event in writes.iter().filter(|e| !done.contains_key(&e.seq)) {
            let actor = SystemActor::new(ATTESTER_ACTOR);
            let why = "fixture: the session's transcript holds the call";
            if let Ok(Some(_)) = model::store::record_write_verification(store, session, actor, &event.entity_id, event.seq, true, None, why) {
                recorded += 1;
            }
        }
        recorded
    }

    /// The arguments of the `remember` call that files `report`.
    pub fn filing_args(report: &Item) -> Value {
        serde_json::json!({
            "id": report.id,
            "kind": "report",
            "project": report.project,
            "tags": report.tags,
            "text": report.text,
        })
    }

    /// AN EVALUATION REPORT FILED THE WAY CLAUDE CODE FILES ONE (step 15):
    /// through THOR's `remember` tool, called by `session` as tool call
    /// `tool_use_id`, for the evaluation `session` has open of the report's
    /// project (a quiet one is started when it has none, `quiet_start`). The
    /// session's transcript under `root` gets the lines in the order Claude
    /// Code writes them: the tool_use first, then - once the write landed -
    /// the hook's marker and the result naming its seq. The store's own
    /// answer comes back, so a fixture can also pin a refusal. NOT yet
    /// verified: no Stop has run (`attested_filing` adds it).
    ///
    /// The start note of the evaluation it files for is in the transcript
    /// first, as the context of the owner's typed command (the Stop-time
    /// validator looks for it, step 16); `filing_without_the_start_note`
    /// leaves it out.
    pub fn attested_filing_unverified(
        store: &mut EventStore,
        root: &Path,
        session: &str,
        report: &Item,
        tool_use_id: &str,
    ) -> Result<Event, model::store::WriteError> {
        filing_through_remember(store, root, session, report, tool_use_id, true, None)
    }

    /// `attested_filing_unverified` whose transcript lines carry the
    /// timestamp `at` - a filing made at that moment, as far as the
    /// transcript tells (`YYYY-MM-DDTHH:MM:SSZ`).
    pub fn attested_filing_unverified_at(
        store: &mut EventStore,
        root: &Path,
        session: &str,
        report: &Item,
        tool_use_id: &str,
        at: &str,
    ) -> Result<Event, model::store::WriteError> {
        filing_through_remember(store, root, session, report, tool_use_id, true, Some(at))
    }

    /// `attested_filing_unverified` whose session was never shown the start
    /// note of its evaluation: no Stop ask, typed command or skill call of it
    /// is in the transcript.
    pub fn filing_without_the_start_note(
        store: &mut EventStore,
        root: &Path,
        session: &str,
        report: &Item,
        tool_use_id: &str,
    ) -> Result<Event, model::store::WriteError> {
        filing_through_remember(store, root, session, report, tool_use_id, false, None)
    }

    fn filing_through_remember(
        store: &mut EventStore,
        root: &Path,
        session: &str,
        report: &Item,
        tool_use_id: &str,
        show_start_note: bool,
        at: Option<&str>,
    ) -> Result<Event, model::store::WriteError> {
        let project = report.project.clone().expect("an evaluation report names its project");
        let start = quiet_start(store, session, &project);
        let tool = "mcp__thor__remember";
        let args = filing_args(report);
        let mut transcript = main_transcript(root, session);
        if let Some(at) = at {
            transcript.at(at);
        }
        if show_start_note {
            transcript.prompt_context(&crate::eval_session::start_note(&start));
        }
        transcript.call(tool_use_id, tool, &args).write();
        let att = att_for_call(&meta(tool_use_id), Some(session), &args, "remember", Some(root));
        let ctx = model::store::FilingContext::new(att.inflight.as_deref().unwrap_or_default(), start);
        let event = model::store::declare_scoped(store, session, "mcp", "mcp", report, ctx.as_ref(), Some(&att))?;
        let marker = write_marker(tool, Some(tool_use_id), Some(&args)).expect("a THOR write");
        transcript.hook_context(tool_use_id, tool, &marker).result(
            tool_use_id,
            &format!("stored '{}' (Report, event seq {})", report.id, event.seq),
        );
        transcript.write();
        Ok(event)
    }

    /// `attested_filing_unverified`, then the session's Stop that verifies
    /// the filing call (`record_write_verifications_in`).
    pub fn attested_filing(
        store: &mut EventStore,
        root: &Path,
        session: &str,
        report: &Item,
        tool_use_id: &str,
    ) -> Result<Event, model::store::WriteError> {
        let event = attested_filing_unverified(store, root, session, report, tool_use_id)?;
        record_write_verifications_in(store, session, Some(root));
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;
    use model::item::{Kind, Severity};
    use serde_json::json;

    const S1: &str = "0de7dc52-1b97-4b66-948f-61e4438043b2";
    const S2: &str = "373e8610-8130-490d-ac29-56aa98709fd1";

    fn absent_rule() -> Item {
        Item {
            id: "ledger-stays-clean".to_string(),
            kind: Kind::Rule,
            text: "Never write `TODO` into data/ledger1.csv.".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: "data/ledger1.csv".to_string() }],
            severity: Some(Severity::Irreversible),
            project: Some("proj".to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("A TODO in the ledger turns out harmless.".to_string()),
            check: Some(Check::Absent { path: "data/ledger1.csv".to_string(), literal: "TODO".to_string() }),
        }
    }

    fn force_rule() -> Item {
        Item {
            id: "no-force-push".to_string(),
            kind: Kind::Rule,
            text: "Never run `git push --force` on main.".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: "git push".to_string() }],
            severity: Some(Severity::Irreversible),
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("A force push to main turns out harmless.".to_string()),
            check: Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        }
    }

    fn store_with(item: &Item) -> EventStore {
        let mut store = EventStore::in_memory().unwrap();
        model::store::declare(&mut store, "s", "l", "a", item).unwrap();
        store
    }

    fn serve_once(store: &mut EventStore, session: &str, id: &str) {
        crate::deliver::record_delivery_with_trigger(store, session, session, "hook", "2026-09-28T00:00:00Z", &[id.to_string()], Some("x"));
    }

    fn refuse(store: &mut EventStore, session: &str, actor: &str, id: &str, target: &str, tool_use_id: Option<&str>) -> Event {
        crate::deliver::record_gate_outcome_for_call(store, session, session, actor, "2026-09-28T00:00:01Z", true, id, target, tool_use_id);
        store.get_events_by_entity(id).unwrap().last().unwrap().clone()
    }

    fn verdicts(store: &EventStore, id: &str) -> HashMap<i64, (bool, String)> {
        model::store::recorded_refusal_verdicts(&store.get_events_by_entity(id).unwrap())
    }

    /// R7a.1 (Z9): a hand-fed PreToolUse payload with a real session id and a
    /// made-up tool call id - the session's transcript holds no such call.
    /// Recorded as a refusal, never field evidence, and the recorded verdict
    /// says why.
    #[test]
    fn hand_fed_refusal_is_not_field_evidence() {
        let root = tempfile::tempdir().unwrap();
        let item = absent_rule();
        let mut store = store_with(&item);
        serve_once(&mut store, S1, &item.id);
        // The session has a transcript, with other calls in it - just not this one.
        main_transcript(root.path(), S1)
            .let_through("toolu_REAL", "Bash", &json!({"command": "ls"}), "ctx", "ok")
            .write();
        let e = refuse(&mut store, S1, "hook", &item.id, "proj/data/ledger1.csv", Some("toolu_HAND"));
        assert_eq!(record_refusal_verifications_in(&mut store, S1, Some(root.path())), 1);
        let v = verdicts(&store, &item.id);
        let (field, why) = v.get(&e.seq).expect("recorded");
        assert!(!field);
        assert!(why.contains("holds no tool call toolu_HAND"), "{why}");
        assert!(field_refusals(&store, &item.id, &item).is_empty());
    }

    /// R7a.4 (X2): a hand-fed refusal copying a REAL tool call id - a Bash
    /// call that ran and was let through - is not field evidence: its result
    /// is no error naming the rule, and the verdict says so.
    #[test]
    fn a_real_tool_use_id_without_the_error_result_is_not_field_evidence() {
        let root = tempfile::tempdir().unwrap();
        let item = force_rule();
        let mut store = store_with(&item);
        serve_once(&mut store, S1, &item.id);
        let input = json!({"command": "git push --force origin main"});
        main_transcript(root.path(), S1).let_through("toolu_RAN", "Bash", &input, "ctx", "pushed").write();
        let e = refuse(&mut store, S1, "hook", &item.id, "git push --force origin main", Some("toolu_RAN"));
        record_refusal_verifications_in(&mut store, S1, Some(root.path()));
        let (field, why) = verdicts(&store, &item.id).get(&e.seq).cloned().expect("recorded");
        assert!(!field);
        assert!(why.contains("without THOR's hook blocking it"), "{why}");
        assert!(field_refusals(&store, &item.id, &item).is_empty());
    }

    /// G7a.1: the real block shape - tool_use T, then an error result for T
    /// naming the rule - with a UUID session and ordinary served events is
    /// field evidence, recorded once; and the recorded verdict outlives the
    /// transcript (N8).
    #[test]
    fn verification_is_recorded_once_and_outlives_the_transcript() {
        let root = tempfile::tempdir().unwrap();
        let item = absent_rule();
        let mut store = store_with(&item);
        let input = json!({"file_path": "C:/work/proj/data/ledger1.csv", "old_string": "a", "new_string": "TODO"});
        let seq = field_refusal(&mut store, root.path(), S1, &item.id, "toolu_T", "Edit", input, "proj/data/ledger1.csv");
        assert_eq!(field_refusals(&store, &item.id, &item), vec![(seq, "proj/data/ledger1.csv".to_string())]);
        let attester_events = |store: &EventStore| {
            store.get_events_by_entity(&item.id).unwrap().iter().filter(|e| e.actor == ATTESTER_ACTOR).count()
        };
        assert_eq!(attester_events(&store), 1);
        assert_eq!(record_refusal_verifications_in(&mut store, S1, Some(root.path())), 0, "recorded once");
        assert_eq!(attester_events(&store), 1);

        std::fs::remove_dir_all(root.path().join(PROJECT_FOLDER)).unwrap();
        assert_eq!(field_refusals(&store, &item.id, &item).len(), 1, "the recorded verdict still reads field");
    }

    /// Step 31: a refusal that answered several rules in one combined message
    /// is verified for each rule it names, not only for a first one.
    #[test]
    fn a_combined_refusal_message_verifies_for_every_rule_it_names() {
        let root = tempfile::tempdir().unwrap();
        let item = force_rule();
        let mut store = store_with(&item);
        serve_once(&mut store, S1, &item.id);
        let input = json!({"command": "git push --force origin main"});
        let combined = format!(
            "[THOR] this call trips 2 rules at once - answer ALL of them in one go, then run it again.\n\n{}\n\n{}",
            block_reason("another-rule", "x"),
            block_reason(&item.id, "git push --force origin main")
        );
        main_transcript(root.path(), S1).block("toolu_COMB", "Bash", &input, &combined).write();
        let e = refuse(&mut store, S1, "hook", &item.id, "git push --force origin main", Some("toolu_COMB"));
        record_refusal_verifications_in(&mut store, S1, Some(root.path()));
        let (field, why) = verdicts(&store, &item.id).get(&e.seq).cloned().expect("recorded");
        assert!(field, "{why}");
    }

    /// The real block shape in a SUBAGENT's own file is found too - from the
    /// session id alone, never from a path a payload names.
    #[test]
    fn the_transcript_is_found_by_session_id_never_by_a_payload_path() {
        let root = tempfile::tempdir().unwrap();
        let item = force_rule();
        let mut store = store_with(&item);
        serve_once(&mut store, S1, &item.id);
        let input = json!({"command": "git push --force origin main"});
        subagent_transcript(root.path(), S1, "a1b2c3")
            .block("toolu_SUB", "Bash", &input, &block_reason(&item.id, "git push --force origin main"))
            .write();
        // Another session's transcript holds a block under the same id: it
        // must never vouch for S1's refusal, nor S1's for S2's.
        main_transcript(root.path(), S2)
            .block("toolu_OTHER", "Bash", &input, &block_reason(&item.id, "git push --force origin main"))
            .write();
        let mine = refuse(&mut store, S1, "hook", &item.id, "git push --force origin main", Some("toolu_SUB"));
        let theirs = refuse(&mut store, S1, "hook", &item.id, "git push --force origin main", Some("toolu_OTHER"));
        record_refusal_verifications_in(&mut store, S1, Some(root.path()));
        let v = verdicts(&store, &item.id);
        assert_eq!(v.get(&mine.seq).map(|x| x.0), Some(true));
        assert_eq!(v.get(&theirs.seq).map(|x| x.0), Some(false), "found only in another session's file");
        assert!(session_transcripts(root.path(), "../..").is_empty(), "a crafted id walks nowhere");
    }

    /// The mention of an id in a command is not the call: a structural match
    /// only (step 1's red probe, door1).
    #[test]
    fn an_id_mentioned_in_a_command_is_never_the_call() {
        let root = tempfile::tempdir().unwrap();
        main_transcript(root.path(), S1)
            .let_through("toolu_X", "Bash", &json!({"command": "echo toolu_PLANTED"}), "ctx", "toolu_PLANTED")
            .write();
        let files = session_transcripts(root.path(), S1);
        assert_eq!(find_call(&files, "toolu_PLANTED"), Found::Nothing);
        assert!(matches!(find_call(&files, "toolu_X"), Found::Call(_)));
    }

    /// Two different calls under one id are never attested.
    #[test]
    fn two_different_calls_under_one_id_are_conflicting() {
        let root = tempfile::tempdir().unwrap();
        let mut t = main_transcript(root.path(), S1);
        t.block("toolu_D", "Bash", &json!({"command": "a"}), "r").block("toolu_D", "Bash", &json!({"command": "b"}), "r");
        t.write();
        assert_eq!(find_call(&session_transcripts(root.path(), S1), "toolu_D"), Found::Conflicting);
    }

    /// R7a.2 and the structural half of the predicate, each on its own: a
    /// non-UUID session (even with served activity), a UUID session with no
    /// activity, an actor that is not the hook, and a refusal with no tool
    /// call id are each recorded as not field - whatever the transcript says.
    #[test]
    fn non_uuid_no_activity_other_actor_and_no_call_id_are_never_field() {
        let root = tempfile::tempdir().unwrap();
        let item = force_rule();
        let input = json!({"command": "git push --force origin main"});
        let reason = block_reason(&item.id, "git push --force origin main");

        // A non-UUID session, with served activity.
        let mut store = store_with(&item);
        serve_once(&mut store, "probe-1", &item.id);
        let e = refuse(&mut store, "probe-1", "hook", &item.id, "git push --force origin main", Some("toolu_A"));
        let v = verify_refusal(&store, &e, Some(root.path())).unwrap();
        assert!(!v.field && v.why.contains("not a Claude Code session id"), "{v:?}");

        // A UUID session with no other activity, and a matching transcript.
        let mut store = store_with(&item);
        main_transcript(root.path(), S2).block("toolu_B", "Bash", &input, &reason).write();
        let e = refuse(&mut store, S2, "hook", &item.id, "git push --force origin main", Some("toolu_B"));
        let v = verify_refusal(&store, &e, Some(root.path())).unwrap();
        assert!(!v.field && v.why.contains("no other activity"), "{v:?}");

        // Actor not the hook (`serve hook --actor probe`).
        serve_once(&mut store, S2, &item.id);
        let e = refuse(&mut store, S2, "probe", &item.id, "git push --force origin main", Some("toolu_B"));
        let v = verify_refusal(&store, &e, Some(root.path())).unwrap();
        assert!(!v.field && v.why.contains("actor 'probe'"), "{v:?}");

        // No tool call id (every refusal recorded before this build).
        let e = refuse(&mut store, S2, "hook", &item.id, "git push --force origin main", None);
        let v = verify_refusal(&store, &e, Some(root.path())).unwrap();
        assert!(!v.field && v.why.contains("no tool call id"), "{v:?}");

        // And green: the same session, the call id, the transcript.
        let e = refuse(&mut store, S2, "hook", &item.id, "git push --force origin main", Some("toolu_B"));
        assert!(verify_refusal(&store, &e, Some(root.path())).unwrap().field);
    }

    /// A call that ran and printed the hook's own error text (an `echo` of
    /// it, say) left a result that is no error: only `is_error` true marks a
    /// block.
    #[test]
    fn a_result_that_only_reads_like_the_hooks_error_is_no_block() {
        let root = tempfile::tempdir().unwrap();
        let item = force_rule();
        let mut store = store_with(&item);
        serve_once(&mut store, S1, &item.id);
        let input = json!({"command": "git push --force origin main"});
        let mimic = format!("PreToolUse:Bash hook error: {}", block_reason(&item.id, "git push --force origin main"));
        main_transcript(root.path(), S1).let_through("toolu_MIMIC", "Bash", &input, "ctx", &mimic).write();
        let e = refuse(&mut store, S1, "hook", &item.id, "git push --force origin main", Some("toolu_MIMIC"));
        let v = verify_refusal(&store, &e, Some(root.path())).unwrap();
        assert!(!v.field && v.why.contains("without THOR's hook blocking it"), "{v:?}");
    }

    /// The transcript holds a block under the id, but of ANOTHER call than
    /// the one refused (a copied id with other input): never field evidence.
    #[test]
    fn a_block_of_another_call_under_the_id_is_not_field_evidence() {
        let root = tempfile::tempdir().unwrap();
        let item = force_rule();
        let mut store = store_with(&item);
        serve_once(&mut store, S1, &item.id);
        let input = json!({"command": "git push --force origin topic"});
        main_transcript(root.path(), S1)
            .block("toolu_OTHERCALL", "Bash", &input, &block_reason(&item.id, "git push --force origin topic"))
            .write();
        let e = refuse(&mut store, S1, "hook", &item.id, "git push --force origin main", Some("toolu_OTHERCALL"));
        let v = verify_refusal(&store, &e, Some(root.path())).unwrap();
        assert!(!v.field && v.why.contains("is not the refused call 'git push --force origin main'"), "{v:?}");
        // And a block naming ANOTHER rule is no block of this one.
        let other = refuse(&mut store, S1, "hook", &item.id, "git push --force origin topic", Some("toolu_OTHERRULE"));
        main_transcript(root.path(), S1)
            .block("toolu_OTHERRULE", "Bash", &input, &block_reason("some-other-rule", "git push --force origin topic"))
            .write();
        let v = verify_refusal(&store, &other, Some(root.path())).unwrap();
        assert!(!v.field && v.why.contains("without THOR's hook blocking it for rule no-force-push"), "{v:?}");
    }

    /// No transcript of the session on this machine yet: nothing is recorded,
    /// so a later Stop can still decide it.
    #[test]
    fn a_session_with_no_transcript_yet_is_left_unrecorded() {
        let root = tempfile::tempdir().unwrap();
        let item = force_rule();
        let mut store = store_with(&item);
        serve_once(&mut store, S1, &item.id);
        refuse(&mut store, S1, "hook", &item.id, "git push --force", Some("toolu_Z"));
        assert_eq!(record_refusal_verifications_in(&mut store, S1, Some(root.path())), 0);
        assert!(verdicts(&store, &item.id).is_empty());
    }

    /// A field refusal earned under an older check never vouches for the
    /// current one, and a command refusal counts only while the current check
    /// still refuses that call.
    #[test]
    fn a_field_refusal_counts_only_for_the_check_it_was_earned_under() {
        let root = tempfile::tempdir().unwrap();
        let item = force_rule();
        let mut store = store_with(&item);
        let input = json!({"command": "git push --force origin main"});
        field_refusal(&mut store, root.path(), S1, &item.id, "toolu_F", "Bash", input, "git push --force origin main");
        assert_eq!(field_refusals(&store, &item.id, &item).len(), 1);

        let mut changed = item.clone();
        changed.text = "Never run `git push --mirror` on main.".to_string();
        changed.check = Some(Check::Forbidden { literals: vec!["--mirror".to_string()] });
        model::store::revise_because(&mut store, "s", "l", "a", &item, &changed, &item, Some("fixture")).unwrap();
        assert!(field_refusals(&store, &item.id, &changed).is_empty());
    }

    /// NEW-2 (C3, step 20): during its own open evaluation a session runs the
    /// forbidden call itself and the hook refuses it. The refusal is real, and
    /// recorded - as never proof, saying why. The same session's refusal
    /// before its evaluation started, and another session's during it, still
    /// count.
    #[test]
    fn a_refusal_during_the_sessions_own_evaluation_is_never_field_evidence() {
        let root = tempfile::tempdir().unwrap();
        let item = force_rule();
        let mut store = store_with(&item);
        let input = json!({"command": "git push --force origin main"});
        let before = field_refusal(&mut store, root.path(), S1, &item.id, "toolu_B", "Bash", input.clone(), "git push --force origin main");
        let start = quiet_start(&mut store, S1, "fxp");
        let during = field_refusal(&mut store, root.path(), S1, &item.id, "toolu_D", "Bash", input.clone(), "git push --force origin main");
        let (field, why) = verdicts(&store, &item.id).get(&during).cloned().expect("recorded");
        assert!(!field, "{why}");
        assert!(why.starts_with(OWN_EVALUATION_WHY) && why.contains(&start.id), "{why}");
        let other = field_refusal(&mut store, root.path(), S2, &item.id, "toolu_O", "Bash", input.clone(), "git push --force origin main");
        let counted: Vec<i64> = field_refusals(&store, &item.id, &item).into_iter().map(|(seq, _)| seq).collect();
        assert_eq!(counted, vec![before, other], "only the evaluating session's own call inside its evaluation is dropped");
        // The window closes with the evaluation: once it is void, the same
        // session's next real refusal counts again.
        let void = model::store::FrozenVoid { id: item.id.clone(), seq: during, door: "fixture".to_string() };
        model::store::record_evaluation_void(&mut store, SystemActor::new(model::store::EVAL_MACHINE_ACTOR), &start, &void).unwrap();
        let after = field_refusal(&mut store, root.path(), S1, &item.id, "toolu_A", "Bash", input, "git push --force origin main");
        assert!(verdicts(&store, &item.id)[&after].0, "{:?}", verdicts(&store, &item.id)[&after]);
    }

    /// NEW-2, a verdict recorded before step 20: it said field for a refusal
    /// inside the session's own evaluation (C3 storeB, seq 241750-241757).
    /// Every reader reads the log again, so it never counts either.
    #[test]
    fn a_field_verdict_recorded_before_the_fix_does_not_prove_a_self_probe() {
        let item = force_rule();
        let mut store = store_with(&item);
        serve_once(&mut store, S1, &item.id);
        quiet_start(&mut store, S1, "fxp");
        let e = refuse(&mut store, S1, "hook", &item.id, "git push --force origin main", Some("toolu_OLD"));
        model::store::record_refusal_verification(&mut store, S1, SystemActor::new(ATTESTER_ACTOR), &item.id, e.seq, true, "old build")
            .unwrap();
        assert!(field_refusals(&store, &item.id, &item).is_empty());
        assert!(self_proof(&store, &e).is_some_and(|why| why.starts_with(OWN_EVALUATION_WHY)));
    }

    /// NEW-2, the writer's side (the refusal half of D6): the session that
    /// wrote the rule's check provokes a refusal of it. Recorded, never proof;
    /// another session's refusal of the same call counts.
    #[test]
    fn a_refusal_in_the_session_that_wrote_the_check_is_never_field_evidence() {
        let root = tempfile::tempdir().unwrap();
        let item = force_rule();
        let mut store = EventStore::in_memory().unwrap();
        model::store::declare(&mut store, S1, S1, "mcp", &item).unwrap();
        let input = json!({"command": "git push --force origin main"});
        let own = field_refusal(&mut store, root.path(), S1, &item.id, "toolu_W", "Bash", input.clone(), "git push --force origin main");
        let (field, why) = verdicts(&store, &item.id).get(&own).cloned().expect("recorded");
        assert!(!field && why.starts_with(OWN_CHECK_WHY), "{why}");
        let other = field_refusal(&mut store, root.path(), S2, &item.id, "toolu_X2", "Bash", input, "git push --force origin main");
        assert_eq!(field_refusals(&store, &item.id, &item), vec![(other, "git push --force origin main".to_string())]);
    }
}


/// The second half: a write's session identity, in flight and at the Stop.
#[cfg(test)]
mod write_tests {
    use super::fixture::*;
    use super::*;
    use model::item::{Kind, TargetKind};
    use serde_json::json;

    const S: &str = SESSION;
    const TOOL: &str = "mcp__thor__remember";

    fn args() -> Value {
        json!({
            "id": "fx-rule",
            "kind": "rule",
            "text": "Run git status before a commit to see what is staged.",
            "targets": [{"kind": "command", "value": "git status"}],
            "falsifier": "A commit made without git status never staged the wrong file."
        })
    }

    fn item() -> Item {
        Item {
            id: "fx-rule".to_string(),
            kind: Kind::Rule,
            text: "Run git status before a commit to see what is staged.".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: "git status".to_string() }],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("A commit made without git status never staged the wrong file.".to_string()),
            check: None,
        }
    }

    /// The write as the tool server makes it: stamped, under the session hint.
    fn write(store: &mut EventStore, session: &str, att: &Att) -> Event {
        model::store::declare_in_scoped(store, session, "mcp", "mcp", &item(), None, None, None, Some(att)).unwrap()
    }

    fn result_text(seq: i64) -> String {
        format!("stored 'fx-rule' (Rule, event seq {seq})")
    }

    /// Claude Code's own order (step 1, answers 5 and 6): the tool_use line,
    /// the call (the writer sees the line), then the hook's attachments and
    /// the result.
    fn real_call(store: &mut EventStore, root: &Path, transcript: &mut Transcript, id: &str) -> Event {
        transcript.call(id, TOOL, &args()).write();
        let att = att_for_call(&meta(id), Some(S), &args(), "remember", Some(root));
        assert_eq!(att.inflight.as_deref(), Some(model::store::INFLIGHT_SEEN), "{att:?}");
        let event = write(store, S, &att);
        let marker = write_marker(TOOL, Some(id), Some(&args())).unwrap();
        transcript
            .hook_context(id, TOOL, &format!("Background facts about the owner's setup.\n{marker}"))
            .result(id, &result_text(event.seq))
            .write();
        event
    }

    fn attested(v: Option<WriteAttestation>) -> (String, Option<String>, PathBuf) {
        match v {
            Some(WriteAttestation::Attested { session, agent_id, file }) => (session, agent_id, file),
            other => panic!("expected attested, got {other:?}"),
        }
    }

    fn unattested(v: Option<WriteAttestation>) -> String {
        match v {
            Some(WriteAttestation::Unattested(why)) => why,
            other => panic!("expected unattested, got {other:?}"),
        }
    }

    fn write_verdicts(store: &EventStore) -> HashMap<i64, model::store::WriteVerdict> {
        model::store::recorded_write_verdicts(&store.get_events_by_entity("fx-rule").unwrap())
    }

    /// G12.1: the tool_use, THOR's marker in a PreToolUse attachment, and a
    /// result naming the write's own seq attest it - and each of the three
    /// is needed (the red halves below and in the next tests).
    #[test]
    fn verify_accepts_tool_use_marker_and_result() {
        let root = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_memory().unwrap();
        let event = real_call(&mut store, root.path(), &mut main_transcript(root.path(), S), "toolu_G1");
        let att = model::store::att_of(&event.body).expect("the body carries att");
        assert_eq!(att.tool_use_id.as_deref(), Some("toolu_G1"));
        assert_eq!(att.session_hint.as_deref(), Some(S));
        assert_eq!(event.session_id, S, "the session column is the hint");
        let (session, agent, file) = attested(verify_write(&event, Some(root.path())));
        assert_eq!((session.as_str(), agent), (S, None));
        assert!(file.ends_with(format!("{S}.jsonl")), "{}", file.display());

        // Red: a write stamped with no tool call id is never attested.
        let bare = Att { tool_use_id: None, ..att };
        let mut other = item();
        other.id = "fx-other".to_string();
        other.text = "Run git diff before a push to see what leaves.".to_string();
        let e = model::store::declare_in_scoped(&mut store, S, "mcp", "mcp", &other, None, None, None, Some(&bare)).unwrap();
        assert_eq!(unattested(verify_write(&e, Some(root.path()))), "no toolUseId in the call's _meta");
    }

    /// A hand-fed hook run prints the marker into a Bash tool_result, never
    /// into a hook attachment: the write is not attested.
    #[test]
    fn marker_in_a_bash_tool_result_does_not_attest() {
        let root = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_memory().unwrap();
        let mut t = main_transcript(root.path(), S);
        t.call("toolu_M", TOOL, &args()).write();
        let att = att_for_call(&meta("toolu_M"), Some(S), &args(), "remember", Some(root.path()));
        let event = write(&mut store, S, &att);
        let marker = write_marker(TOOL, Some("toolu_M"), Some(&args())).unwrap();
        let hook_output = json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "additionalContext": marker}}).to_string();
        t.let_through("toolu_BASH", "Bash", &json!({"command": "serve hook < payload.json"}), "ctx", &hook_output)
            .result("toolu_M", &result_text(event.seq))
            .write();
        let why = unattested(verify_write(&event, Some(root.path())));
        assert!(why.contains("no THOR hook marker for toolu_M"), "{why}");
    }

    /// R12.3: a genuine tool call id replayed with one argument changed.
    #[test]
    fn digest_mismatch_does_not_attest() {
        let root = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_memory().unwrap();
        let mut t = main_transcript(root.path(), S);
        let genuine = real_call(&mut store, root.path(), &mut t, "toolu_D");
        assert!(matches!(verify_write(&genuine, Some(root.path())), Some(WriteAttestation::Attested { .. })));
        let mut changed = args();
        changed["text"] = json!("Run git status before a commit to see what is staged, always.");
        let in_flight = check_call_in_flight(&meta("toolu_D"), Some(S), &changed, "remember", Some(root.path()));
        assert_eq!(in_flight, InFlight::SideDoor("the arguments differ from the ones Claude Code sent for toolu_D".to_string()));
        let att = att_for_call(&meta("toolu_D"), Some(S), &changed, "remember", Some(root.path()));
        let mut replay = item();
        replay.id = "fx-rule-replayed".to_string();
        replay.text = "Run git diff before a push to see what leaves the machine.".to_string();
        let e = model::store::declare_in_scoped(&mut store, S, "mcp", "mcp", &replay, None, None, None, Some(&att)).unwrap();
        t.result("toolu_D", &format!("stored 'fx-rule-replayed' (Rule, event seq {})", e.seq)).write();
        assert_eq!(unattested(verify_write(&e, Some(root.path()))), "the arguments differ from the ones Claude Code sent");

        // The Stop's recorder reads the transcripts once for both writes and
        // judges each against the one call they both name.
        assert_eq!(record_write_verifications_in(&mut store, S, Some(root.path())), 2);
        let genuine_verdict = write_verdicts(&store).remove(&genuine.seq).expect("recorded");
        assert!(genuine_verdict.attested, "{genuine_verdict:?}");
        let replayed = model::store::recorded_write_verdicts(&store.get_events_by_entity("fx-rule-replayed").unwrap());
        let replay_verdict = replayed.get(&e.seq).expect("recorded");
        assert!(!replay_verdict.attested, "{replay_verdict:?}");
        assert_eq!(replay_verdict.why, "the arguments differ from the ones Claude Code sent");
    }

    /// The result must name THIS write's seq, exactly: a replayed real id
    /// names another one, and a longer number is not this one.
    #[test]
    fn result_naming_another_seq_does_not_attest() {
        let root = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_memory().unwrap();
        let mut t = main_transcript(root.path(), S);
        t.call("toolu_R", TOOL, &args()).write();
        let att = att_for_call(&meta("toolu_R"), Some(S), &args(), "remember", Some(root.path()));
        let event = write(&mut store, S, &att);
        let marker = write_marker(TOOL, Some("toolu_R"), Some(&args())).unwrap();
        t.hook_context("toolu_R", TOOL, &marker).result("toolu_R", &format!("stored 'fx-rule' (Rule, event seq {}0)", event.seq)).write();
        let why = unattested(verify_write(&event, Some(root.path())));
        assert!(why.contains(&format!("names no 'event seq {}'", event.seq)), "{why}");
        assert!(names_seq("x (event seq 12)", 12) && !names_seq("x (event seq 123)", 12));
    }

    /// G12.2: a subagent's lines live in its own file under the session's
    /// folder; found from the session id alone, attested with its agent id.
    #[test]
    fn subagent_call_under_the_session_folder_attests_with_its_agent_id() {
        let root = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_memory().unwrap();
        let mut sub = subagent_transcript(root.path(), S, "a1");
        let event = real_call(&mut store, root.path(), &mut sub, "toolu_SUB1");
        let (session, agent, file) = attested(verify_write(&event, Some(root.path())));
        assert_eq!((session.as_str(), agent.as_deref()), (S, Some("a1")));
        assert!(file.ends_with("agent-a1.jsonl"), "{}", file.display());
    }

    /// K21: the writer refuses nothing on a line merely not on disk; only
    /// positive evidence makes a side door.
    #[test]
    fn in_flight_refuses_only_on_positive_evidence() {
        let root = tempfile::tempdir().unwrap();
        let empty = serde_json::Map::new();
        let check = |meta: &serde_json::Map<String, Value>, env: Option<&str>, a: &Value, tool: &str| {
            check_call_in_flight(meta, env, a, tool, Some(root.path()))
        };
        assert_eq!(check(&empty, Some(S), &args(), "remember"), InFlight::SideDoor("no toolUseId in the call's _meta".to_string()));
        assert_eq!(
            check(&meta("toolu_E"), None, &args(), "remember"),
            InFlight::SideDoor("no CLAUDE_CODE_SESSION_ID in the server's environment".to_string())
        );
        assert_eq!(check(&meta("toolu_E"), Some(S), &args(), "remember"), InFlight::Unseen, "no transcript yet");
        main_transcript(root.path(), S).let_through("toolu_OTHER", "Bash", &json!({"command": "ls"}), "ctx", "ok").write();
        assert_eq!(check(&meta("toolu_E"), Some(S), &args(), "remember"), InFlight::Unseen, "not on disk yet");
        main_transcript(root.path(), S).call("toolu_E", TOOL, &args()).write();
        assert_eq!(check(&meta("toolu_E"), Some(S), &args(), "remember"), InFlight::Seen);
        assert!(matches!(check(&meta("toolu_E"), Some(S), &json!({"id": "x"}), "remember"), InFlight::SideDoor(_)));
        assert!(matches!(check(&meta("toolu_E"), Some(S), &args(), "revise"), InFlight::SideDoor(_)));
    }

    /// G12.3: a call whose tool_use line is not on disk while it runs is
    /// stored (inflight "unseen", nothing refused); a Stop before its lines
    /// exist records nothing, and the Stop after they do attests it.
    #[test]
    fn an_unseen_call_is_stored_and_verified_at_stop() {
        let root = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_memory().unwrap();
        let att = att_for_call(&meta("toolu_U"), Some(S), &args(), "remember", Some(root.path()));
        assert_eq!(att.inflight.as_deref(), Some(model::store::INFLIGHT_UNSEEN));
        let event = write(&mut store, S, &att);
        assert_eq!(record_write_verifications_in(&mut store, S, Some(root.path())), 0, "no transcript yet: undecided");
        let mut t = main_transcript(root.path(), S);
        t.call("toolu_U", TOOL, &args()).write();
        assert_eq!(record_write_verifications_in(&mut store, S, Some(root.path())), 0, "still running: undecided");
        let marker = write_marker(TOOL, Some("toolu_U"), Some(&args())).unwrap();
        t.hook_context("toolu_U", TOOL, &marker).result("toolu_U", &result_text(event.seq)).write();
        assert_eq!(record_write_verifications_in(&mut store, S, Some(root.path())), 1);
        assert!(write_verdicts(&store).get(&event.seq).is_some_and(|v| v.attested && v.session == S), "{:?}", write_verdicts(&store));
    }

    /// R12.4: a side door whose `_meta` carries the id of a REAL Bash call
    /// already in the transcript - recorded as a side door in flight, stored
    /// (nothing refuses it here), and recorded unattested at the Stop.
    #[test]
    fn a_bash_tool_use_id_in_meta_is_a_side_door() {
        let root = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_memory().unwrap();
        main_transcript(root.path(), S).let_through("toolu_B", "Bash", &json!({"command": "ls"}), "ctx", "ok").write();
        let att = att_for_call(&meta("toolu_B"), Some(S), &args(), "remember", Some(root.path()));
        assert_eq!(att.inflight.as_deref(), Some(model::store::INFLIGHT_SIDE_DOOR));
        assert_eq!(att.side_door.as_deref(), Some("toolu_B is a Bash call, not mcp__thor__remember"));
        let event = write(&mut store, S, &att);
        assert_eq!(model::store::att_of(&event.body).unwrap().side_door.as_deref(), att.side_door.as_deref());
        record_write_verifications_in(&mut store, S, Some(root.path()));
        let verdict = write_verdicts(&store).remove(&event.seq).expect("recorded");
        assert!(!verdict.attested && verdict.why == "toolu_B is a Bash call, not mcp__thor__remember", "{verdict:?}");
    }

    /// Field test 5 (D5): at filing, a finished write is attested on the spot;
    /// a verdict, and a call that does not check out, wait for the Stop (and
    /// the second is never recorded as false early).
    #[test]
    fn finished_writes_are_attested_at_filing_and_the_rest_waits_for_the_stop() {
        let root = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_memory().unwrap();
        let mut t = main_transcript(root.path(), S);
        let genuine = real_call(&mut store, root.path(), &mut t, "toolu_G");
        // A replay of the same call id with another text: it does not check out.
        let mut changed = args();
        changed["text"] = json!("Run git diff before a push to see what leaves the machine, always.");
        let att = att_for_call(&meta("toolu_G"), Some(S), &changed, "remember", Some(root.path()));
        let mut replay = item();
        replay.id = "fx-rule-replayed".to_string();
        replay.text = "Run git diff before a push to see what leaves the machine.".to_string();
        let replayed = model::store::declare_in_scoped(&mut store, S, "mcp", "mcp", &replay, None, None, None, Some(&att)).unwrap();
        t.result("toolu_G", &format!("stored 'fx-rule-replayed' (Rule, event seq {})", replayed.seq)).write();
        let mark = attested_mark_unverified(&mut store, root.path(), S, "fx-rule", false, "toolu_H");

        assert_eq!(attest_finished_writes_in(&mut store, S, Some(root.path())), 1, "only the genuine write");
        assert!(write_verdicts(&store).get(&genuine.seq).is_some_and(|v| v.attested), "{:?}", write_verdicts(&store));
        assert!(!write_verdicts(&store).contains_key(&mark), "a verdict is left to the Stop");
        let replay_records = model::store::recorded_write_verdicts(&store.get_events_by_entity("fx-rule-replayed").unwrap());
        assert!(replay_records.is_empty(), "a call that does not check out is not recorded false early");

        // The Stop then decides the other two, as before.
        assert_eq!(record_write_verifications_in(&mut store, S, Some(root.path())), 2);
        assert!(write_verdicts(&store).get(&mark).is_some_and(|v| v.attested));
        let replay_records = model::store::recorded_write_verdicts(&store.get_events_by_entity("fx-rule-replayed").unwrap());
        assert!(replay_records.get(&replayed.seq).is_some_and(|v| !v.attested));
    }

    /// Recorded once, by the attester, and read back from the record alone.
    #[test]
    fn verification_is_recorded_once() {
        let root = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_memory().unwrap();
        let event = real_call(&mut store, root.path(), &mut main_transcript(root.path(), S), "toolu_O");
        assert_eq!(record_write_verifications_in(&mut store, S, Some(root.path())), 1);
        assert_eq!(record_write_verifications_in(&mut store, S, Some(root.path())), 0, "recorded once");
        let records = store.get_events_by_entity("fx-rule").unwrap();
        let by_attester = records.iter().filter(|e| e.actor == ATTESTER_ACTOR).count();
        assert_eq!(by_attester, 1);
        assert!(model::store::attested_write_seqs(&store).contains(&event.seq));
        std::fs::remove_dir_all(root.path().join(PROJECT_FOLDER)).unwrap();
        assert!(model::store::attested_write_seqs(&store).contains(&event.seq), "the record outlives the transcript");
        // Another session's Stop never verifies this session's writes.
        let other = "373e8610-8130-490d-ac29-56aa98709fd1";
        assert_eq!(record_write_verifications_in(&mut store, other, Some(root.path())), 0);
    }

    /// The marker is for THOR's write tools only, and names the digest of
    /// the arguments the hook saw.
    #[test]
    fn the_marker_names_the_call_and_its_argument_digest() {
        let digest = model::store::args_digest(&args());
        assert_eq!(
            write_marker(TOOL, Some("toolu_X"), Some(&args())),
            Some(format!("[THOR] write attested: toolu_X {}", &digest[..16]))
        );
        for tool in ["mcp__thor__lookup", "mcp__thor__get", "Bash", "remember", "mcp__thor__shelve"] {
            assert_eq!(write_marker(tool, Some("toolu_X"), Some(&args())), None, "{tool}");
        }
        assert_eq!(write_marker(TOOL, None, Some(&args())), None);
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;

    const S: &str = "5e1f0c3a-0000-4000-8000-00000000c10c";

    fn project_dir(home: &Path, name: &str) -> PathBuf {
        let dir = home.join("checkouts").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".thor-project"), format!("{name}\n")).unwrap();
        dir
    }

    /// The session's own minutes per project, from its transcript: each
    /// line is attributed to the project its `cwd` resolves to, a folder of
    /// no project keeps the project before it, and only the work since the
    /// given instant counts.
    #[test]
    fn the_transcript_gives_the_sessions_own_minutes_in_each_project() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("projects");
        let alpha = project_dir(home.path(), "alpha");
        let beta = project_dir(home.path(), "beta");
        let scratch = home.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let t0 = 1_790_000_000;
        fixture::work_in(&root, S, &alpha, t0, 25);
        fixture::work_in(&root, S, &scratch, t0 + 26 * 60, 5);
        fixture::work_in(&root, S, &beta, t0 + 40 * 60, 12);
        let timeline = session_timeline(&root, S);
        assert_eq!(minutes_in(&timeline, "alpha", None), 25 + 1 + 5, "the scratch folder keeps alpha (D4)");
        assert_eq!(minutes_in(&timeline, "beta", None), 12);
        assert_eq!(minutes_in(&timeline, "alpha", Some(t0 + 20 * 60)), 4 + 1 + 5, "only gaps between lines after the evaluation");
        assert_eq!(session_minutes_from_transcript(&root, S, "beta", None), 12);
        assert_eq!(projects_in(&timeline), vec!["alpha".to_string(), "beta".to_string()]);
        assert_eq!(session_minutes_from_transcript(&root, "not-a-session", "alpha", None), 0);
    }

    /// Q10's cross-check: a turn that began in the project before the
    /// newest one ended with a Stop there.
    #[test]
    fn an_earlier_turn_is_read_from_the_typed_prompts() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("projects");
        let alpha = project_dir(home.path(), "alpha");
        let beta = project_dir(home.path(), "beta");
        let t0 = 1_790_000_000;
        fixture::work_in(&root, S, &alpha, t0, 3);
        let one = session_timeline(&root, S);
        assert!(!earlier_turn_in(&one, "alpha"), "one turn: this Stop is the first");
        fixture::main_transcript(&root, S).stop_block("[THOR] a block is feedback, never a typed prompt").write();
        assert!(!earlier_turn_in(&session_timeline(&root, S), "alpha"));
        fixture::work_in(&root, S, &beta, t0 + 600, 3);
        let two = session_timeline(&root, S);
        assert!(earlier_turn_in(&two, "alpha"), "the first turn began in alpha");
        assert!(!earlier_turn_in(&two, "beta"), "beta's only turn is the newest");
    }

    /// One bounded read: only the last `TRANSCRIPT_TAIL_BYTES` of a file are
    /// read, so a line before them - however it is timed - is not.
    #[test]
    fn only_the_tail_of_a_long_transcript_is_read() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("projects");
        let alpha = project_dir(home.path(), "alpha");
        let t0 = 1_790_000_000;
        let file = fixture::work_in(&root, S, &alpha, t0, 20);
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
            let pad = format!("{{\"type\":\"progress\",\"pad\":\"{}\"}}\n", "x".repeat(1000));
            for _ in 0..(TRANSCRIPT_TAIL_BYTES / 1000 + 10) {
                f.write_all(pad.as_bytes()).unwrap();
            }
        }
        fixture::work_in(&root, S, &alpha, t0 + 21 * 60, 4);
        assert_eq!(session_minutes_from_transcript(&root, S, "alpha", None), 4, "the first 20 minutes sit before the tail");
    }
}

#[cfg(test)]
mod step_26_tests {
    use super::*;

    fn line(v: serde_json::Value) -> String {
        format!("{v}\n")
    }

    /// Field test 2, defect 5: the routine was rewritten after the session
    /// read it, and the Stop never said so.
    #[test]
    fn a_routine_rewritten_after_it_was_read_is_named_and_one_read_since_is_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let routine = dir.path().join("t-eval.md");
        std::fs::write(&routine, "the owner's own routine\n").unwrap();
        let command = dir.path().join("thor-eval.md");
        std::fs::write(&command, format!("This machine runs {} - read it whole.\n", routine.display())).unwrap();
        let transcript = dir.path().join("s.jsonl");
        let read = |path: &Path, at: &str| {
            line(serde_json::json!({
                "type": "assistant",
                "timestamp": at,
                "message": {"content": [{"type": "tool_use", "name": "Read", "input": {"file_path": path.to_string_lossy()}}]},
            }))
        };
        // Read long ago: both files changed since.
        std::fs::write(&transcript, read(&routine, "2026-01-01T10:00:00.000Z")).unwrap();
        let lines = routine_reread_lines(&command, &transcript);
        assert_eq!(lines.len(), 1, "only the file that was read is named: {lines:?}");
        assert!(lines[0].contains("t-eval.md changed since you last read it"), "{lines:?}");
        // Read again after the change: quiet.
        let later = crate::time::iso8601_from_unix(crate::time::now_unix() + 3600);
        std::fs::write(&transcript, format!("{}{}", read(&routine, "2026-01-01T10:00:00.000Z"), read(&routine, &format!("{}.000Z", later.trim_end_matches('Z'))))).unwrap();
        assert!(routine_reread_lines(&command, &transcript).is_empty());
        // The skill of the same name counts as a read.
        let skill = line(serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-01-01T10:00:00.000Z",
            "message": {"content": [{"type": "tool_use", "name": "Skill", "input": {"skill": "t-eval"}}]},
        }));
        std::fs::write(&transcript, skill).unwrap();
        assert_eq!(routine_reread_lines(&command, &transcript).len(), 1);
        // Never read: nothing to say.
        std::fs::write(&transcript, "").unwrap();
        assert!(routine_reread_lines(&command, &transcript).is_empty());
    }

    #[test]
    fn the_owners_words_are_his_typed_turns_only() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("s.jsonl");
        let user = |text: &str, extra: serde_json::Value| {
            let mut v = serde_json::json!({"type": "user", "message": {"role": "user", "content": text}});
            if let (Some(map), Some(more)) = (v.as_object_mut(), extra.as_object()) {
                map.extend(more.clone());
            }
            line(v)
        };
        let text = [
            user("first go-owner", serde_json::json!({"origin": {"kind": "human"}})),
            user("hand-back go-owner", serde_json::json!({"isMeta": true, "origin": {"kind": "peer"}})),
            user("peer go-owner", serde_json::json!({"origin": {"kind": "peer"}})),
            user("second\n> quoted go-owner\n```\ngo-owner\n```\nend", serde_json::json!({"origin": {"kind": "human"}})),
            line(serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result", "content": "go-owner"}]}})),
            user("third", serde_json::json!({})),
        ]
        .concat();
        std::fs::write(&transcript, text).unwrap();
        let words = owner_messages(&transcript, 2).unwrap();
        assert_eq!(words.len(), 2, "{words:?}");
        assert!(!words.iter().any(|w| says_marker(w, "go-owner")), "quoted, fenced, meta and peer text are not his: {words:?}");
        let all = owner_messages(&transcript, 10).unwrap();
        assert!(says_marker(&all[0], "go-owner"), "{all:?}");
        assert!(owner_messages(&dir.path().join("missing.jsonl"), 2).is_err());
        assert!(says_marker("ok, Go-Owner.", "go-owner") && !says_marker("go-owners", "go-owner") && !says_marker("nogo-owner", "go-owner"));
    }

    /// STEP 27 (D10): a write THOR makes itself - here the installer's owner-
    /// approval migration at session start - is made by no tool call. It is
    /// never recorded as "written by another program", so it never reads as a
    /// side door.
    #[test]
    fn a_write_by_thors_own_installer_is_never_recorded_as_unattested() {
        let root = tempfile::tempdir().unwrap();
        let rule = Item {
            id: "send-needs-the-go".to_string(),
            kind: model::item::Kind::Rule,
            text: "Sending needs the owner's go-owner marker.".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: "send".to_string() }],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("Sending without a go turns out harmless.".to_string()),
            check: Some(Check::Requires { when: "send".to_string(), required: vec!["go-owner".to_string()] }),
        };
        const SESSION: &str = "0de7dc52-1b97-4b66-948f-61e4438043b2";
        let mut store = EventStore::in_memory().unwrap();
        model::store::declare(&mut store, "s", "l", "a", &rule).unwrap();
        let marked = model::store::migrate_owner_approval(&mut store, SESSION);
        assert_eq!(marked, vec!["send-needs-the-go".to_string()], "the migration ran as the installer");
        assert_eq!(record_write_verifications_in(&mut store, SESSION, Some(root.path())), 0);
        assert!(write_verdicts_of(&store, "send-needs-the-go").is_empty(), "no verdict of any kind was recorded");
    }

    fn write_verdicts_of(store: &EventStore, id: &str) -> HashMap<i64, model::store::WriteVerdict> {
        model::store::recorded_write_verdicts(&store.get_events_by_entity(id).unwrap())
    }
}
