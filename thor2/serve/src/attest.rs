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

use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use thor_core::event_store::{Event, EventKind, EventStore};

use model::item::{Binding, Check, Item, TargetKind};
use model::store::{SystemActor, ASSISTANT_DELIVERY_ACTOR, ATTESTER_ACTOR};

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

/// Look tool call `id` up in `files`, structurally - see this module's doc
/// comment. A line is parsed only when its text holds the id at all (the
/// cheap filter); it counts only when a block's own `id` / `tool_use_id`
/// field equals it.
pub fn find_call(files: &[PathBuf], id: &str) -> Found {
    let mut uses: HashMap<String, (String, Value)> = HashMap::new();
    let mut results: HashMap<String, (bool, String)> = HashMap::new();
    for file in files {
        let Ok(handle) = std::fs::File::open(file) else { continue };
        for line in BufReader::new(handle).lines() {
            let Ok(line) = line else { break };
            if !line.contains(id) {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            let uuid = v.get("uuid").and_then(|u| u.as_str()).unwrap_or_default().to_string();
            let Some(blocks) = v.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) else {
                continue;
            };
            for (i, block) in blocks.iter().enumerate() {
                let key = format!("{uuid}#{i}");
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("tool_use") if block.get("id").and_then(|b| b.as_str()) == Some(id) => {
                        let name = block.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
                        let input = block.get("input").cloned().unwrap_or(Value::Null);
                        uses.insert(key, (name, input));
                    }
                    Some("tool_result") if block.get("tool_use_id").and_then(|b| b.as_str()) == Some(id) => {
                        let is_error = block.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
                        results.insert(key, (is_error, result_text(block.get("content"))));
                    }
                    _ => {}
                }
            }
        }
    }
    let mut distinct: Vec<(String, Value)> = Vec::new();
    for call in uses.into_values() {
        if !distinct.contains(&call) {
            distinct.push(call);
        }
    }
    match distinct.len() {
        0 => Found::Nothing,
        1 => {
            let (name, input) = distinct.remove(0);
            let mut results: Vec<(bool, String)> = results.into_values().collect();
            results.sort();
            results.dedup();
            Found::Call(CallInTranscript { name, input, results })
        }
        _ => Found::Conflicting,
    }
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
        return command == target;
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
                .and_then(crate::absent_guard::rule_id_of)
                .is_some_and(|id| id == event.entity_id)
    });
    if !blocked {
        return no(format!(
            "the tool call {tool_use_id} ran without THOR's hook blocking it for rule {} - its result is not the hook's \
             error naming that rule",
            event.entity_id
        ));
    }
    Some(Verification {
        field: true,
        why: format!(
            "session {session}'s transcript holds tool call {tool_use_id} ({}) and the hook's block of it naming rule {}",
            call.name, event.entity_id
        ),
    })
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
    if event.entity_id != id || item.check.is_none() || !is_attested_refusal(store, event, since, verdicts) {
        return false;
    }
    if reads_a_command(item) {
        let body = serde_json::from_str::<Value>(&event.body).unwrap_or(Value::Null);
        let target = body.get("target").and_then(|t| t.as_str()).unwrap_or_default();
        return crate::teeth::refuses_call_in(store, id, item, target);
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

/// TEST SUPPORT, shared by every crate that needs a refusal to count: writes
/// Claude Code transcript fixtures in the order a real session writes them
/// (step 1 of the build, answers 5 and 7), and the whole chain a counting
/// refusal needs. Writes only under the folder it is given.
pub mod fixture {
    use super::*;

    /// The project folder name fixtures write under (any name works: the
    /// reader walks every folder under the root).
    pub const PROJECT_FOLDER: &str = "C--work-fixture";

    /// A transcript file being written, line by line.
    pub struct Transcript {
        path: PathBuf,
        session: String,
        lines: Vec<String>,
        next: usize,
    }

    /// The main transcript of `session` under `root`.
    pub fn main_transcript(root: &Path, session: &str) -> Transcript {
        let path = root.join(PROJECT_FOLDER).join(format!("{session}.jsonl"));
        Transcript { path, session: session.to_string(), lines: Vec::new(), next: 0 }
    }

    /// A subagent's transcript of `session` under `root` (an Agent-tool
    /// subagent's layout; a workflow agent's sits one folder deeper, and the
    /// reader finds both).
    pub fn subagent_transcript(root: &Path, session: &str, agent: &str) -> Transcript {
        let path = root.join(PROJECT_FOLDER).join(session).join("subagents").join(format!("agent-{agent}.jsonl"));
        Transcript { path, session: session.to_string(), lines: Vec::new(), next: 0 }
    }

    impl Transcript {
        fn uuid(&mut self) -> String {
            self.next += 1;
            format!("00000000-0000-4000-8000-{:012x}", self.next)
        }

        fn push(&mut self, mut line: Value) {
            line["uuid"] = Value::String(self.uuid());
            line["sessionId"] = Value::String(self.session.clone());
            self.lines.push(line.to_string());
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
            self.push(serde_json::json!({
                "attachment": {"type": "hook_success", "hookName": format!("PreToolUse:{tool}"), "toolUseID": id,
                               "hookEvent": "PreToolUse", "content": "", "stdout": context}
            }));
            self.push(serde_json::json!({
                "attachment": {"type": "hook_additional_context", "content": [context], "hookName": format!("PreToolUse:{tool}"),
                               "toolUseID": id, "hookEvent": "PreToolUse"}
            }));
            self.push(serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": [
                    {"tool_use_id": id, "type": "tool_result", "content": [{"type": "text", "text": result}]}
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
}
