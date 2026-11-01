//! THE WEEKLY CHECK ON A READ-ONLY COPY (owner decision 2026-10-26: "the report
//! is made where the session runs").
//!
//! A copy cannot write the log, so it cannot start an evaluation or file a
//! report the way the main machine does. What it CAN do is what a session on
//! it already does: call the THOR tool server, which on a copy is the replica
//! connector, and the connector QUEUES every write for the main machine.
//! This module is the rest of that road, in two halves. Nothing here needs the
//! connector's address: a session's THOR tools may come from a connector that
//! is added in claude.ai (Claude's own signed-in connection), whose address
//! is stored nowhere on the machine and which only that connection can pass.
//! So the proof travels through the SAME tool as the report.
//!
//! THE COPY'S HALF (`copy_stop`, run by the Stop hook):
//!
//! 1. When the project is due (the same rhythm as the main machine: the cadence
//!    of the plan, ten minutes of the session's own work, never its first Stop,
//!    `usefulness::stop_decision`) and the session is connected (`connected`:
//!    a configured connector, or THOR tools in this session's own transcript),
//!    the Stop holds the turn with a copy-specific ask: judge, correct, and
//!    file the report with `mcp__thor__remember` (kind report, tag
//!    evaluation-report). The connector queues it and answers "queued ...
//!    capture <id>".
//! 2. At the next Stop, THOR reads Claude Code's own transcript of the session
//!    (`find_filing`): the filing must be a real tool call of that session with
//!    a non-error result naming its queue capture id. It then builds an
//!    ATTESTATION (`CopyAttestation`: session, project, report id, tool-use id,
//!    capture id, the sha256 of the report text, the transcript time of the
//!    result, the session's other queued writes, and a seal over all of it)
//!    and holds the turn ONCE more, with the exact `remember` call that sends
//!    it (`proof_text`): kind report, id `eval-attest-<capture id>`, tag
//!    `copy-attestation`, the attestation as the text. The connector queues
//!    that call like any write. It is never held a second time for the same
//!    capture.
//!
//! THE MAIN MACHINE'S HALF (`settle`, run by `sync drain`): a queued
//! evaluation report is not replayed as an ordinary `remember` (it would be
//! refused: no evaluation start is open on this log for it). It is HELD until
//! the queued attestation note with its capture id arrives (`is_copy_attestation`;
//! it is read, never stored as a memory item). Then `verify` compares the two:
//! the seal, the capture id, the report id, the project and the sha256 of the
//! queued text. A match files the report with the machine section THOR composes
//! from the attestation (`machine_lines`) and a valid validator record, so the
//! report COUNTS like any other (`model::store::file_copy_evaluation`). Any
//! mismatch files it dead with the reason. No attestation within a day files
//! it dead too. Nothing is ever counted on a text's word.
//!
//! ONE REPORT UNDER AN ID. A session may file the same report id twice (the
//! first never got its proof). The proven filings settle first, newest capture
//! first; any other held filing of an id whose report already counts is closed
//! as SUPERSEDED, not filed, and its capture is remembered so a replay or the
//! late proof of it files nothing.
//!
//! WHAT A COPY'S EVALUATION COVERS, SAID PLAINLY IN THE MACHINE SECTION. A copy
//! keeps no record of which notes fired, and the frozen lists live in the main
//! machine's log. So a copy's evaluation holds only what a copy can do: the
//! verdicts and corrections the session made through the tool (the queued
//! writes, named in the attestation) and its own summary. The verdict list, the
//! batch, the A4 list, the disputes and the false-block sweep stay owed on the
//! main machine, whose own weekly check settles them.
//!
//! WHAT THIS DOES NOT PROVE. The connector carries no auth of its own (the
//! tunnel or Claude's connection is the gate), so the attestation proves that
//! the report is the one the copy's Stop verified in a transcript, not who
//! sent it. A forged one needs a connection to the connector and a report
//! that was really queued, and the seal stops an edited one.

use crate::attest;
use model::item::{Item, Kind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use thor_core::event_store::EventStore;
use thor_core::inbox::InboxOp;

/// The tag of the queued note that carries an attestation (a Report written
/// with `remember`, never stored as a memory item: the drain reads it).
pub const ATTESTATION_TAG: &str = "copy-attestation";
/// The captured call name a 2.6.2 connector gave an attestation posted to its
/// `/copy/attest` route. Nothing writes it any more; the drain still reads one
/// that is already queued, and the version guard still lets it through.
pub const LEGACY_ATTEST_TOOL: &str = "copy_attest";
/// The format of `CopyAttestation`.
pub const ATTESTATION_VERSION: u32 = 1;
/// How long a held report waits for its attestation before it is filed dead.
pub const WAIT_FOR_ATTESTATION_SECS: i64 = 24 * 3600;
/// How long a project is not asked again after its report was sent: the main
/// machine has to drain, ship, and the copy to pull before the log shows it.
pub const SUPPRESS_AFTER_SEND_SECS: i64 = 24 * 3600;
/// How many times one turn is held again for a report that is not filed.
pub const MAX_REHOLDS: u32 = 2;
const QUEUED_PREFIX: &str = "queued for the main machine (";
const REPORT_TAG: &str = model::store::EVALUATION_REPORT_TAG;

// ----------------------------------------------------------- the attestation

/// One write of the session that the connector queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Covered {
    pub tool: String,
    pub id: String,
    pub capture_id: String,
}

/// What the copy's Stop verified about a weekly-check report, from the
/// session's own Claude Code transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyAttestation {
    pub v: u32,
    /// The Claude Code session that filed the report.
    pub session: String,
    pub project: String,
    /// The report's id.
    pub report: String,
    /// The id of the filing tool call in that session's transcript.
    pub tool_use_id: String,
    /// The connector's queue capture id of the filing (its reply names it).
    pub capture_id: String,
    /// `text_digest` of the report text the call carried.
    pub text_sha256: String,
    /// The transcript timestamp of the filing call's result.
    pub attested_at: String,
    /// The other writes of the session the connector queued.
    #[serde(default)]
    pub covered: Vec<Covered>,
    /// `seal_of` the fields above.
    pub seal: String,
}

/// The digest of a report's text, as the attestation and the queued call are
/// compared by.
pub fn text_digest(text: &str) -> String {
    model::store::args_digest(&json!({ "text": text }))
}

impl CopyAttestation {
    fn seal_of(&self) -> String {
        model::store::args_digest(&json!([
            self.v,
            self.session,
            self.project,
            self.report,
            self.tool_use_id,
            self.capture_id,
            self.text_sha256,
            self.attested_at,
            self.covered
        ]))
    }

    /// This attestation with its seal filled in.
    pub fn sealed(mut self) -> Self {
        self.seal = self.seal_of();
        self
    }

    /// The attestation as the text of the note that carries it: one line.
    pub fn to_text(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// An attestation from the text of a note. The text between the first `{`
    /// and the last `}` is read, so a session that wrapped the line in a code
    /// fence still sends a proof; a changed word still breaks the seal.
    pub fn from_text(text: &str) -> Result<Self, String> {
        let (Some(from), Some(to)) = (text.find('{'), text.rfind('}')) else { return Err("the text holds no attestation".to_string()) };
        serde_json::from_str(&text[from..=to.max(from)]).map_err(|e| e.to_string())
    }
}

/// The replica connector's on-the-spot check of a note tagged
/// `copy-attestation`: the text must be an attestation that still matches its
/// own seal, written under the id and project it names. A session that
/// paraphrased it hears so now, while it can still repeat the call, instead of
/// in a drain report nobody reads.
pub fn check_attestation_note(id: &str, project: Option<&str>, text: &str) -> Result<(), String> {
    let att = CopyAttestation::from_text(text).map_err(|why| {
        format!("REFUSED: a copy-attestation note must carry the attestation THOR printed, copied exactly, as its text ({why}). Nothing was queued; call it again with the text as printed.")
    })?;
    if att.seal != att.seal_of() {
        return Err("REFUSED: this attestation does not match its own seal - a character of it was changed. Nothing was queued; copy the text exactly as THOR printed it and call again.".to_string());
    }
    if id != attestation_id(&att.capture_id) || project != Some(att.project.as_str()) {
        return Err(format!(
            "REFUSED: this attestation belongs under id '{}' and project '{}'. Nothing was queued; call again with those.",
            attestation_id(&att.capture_id),
            att.project
        ));
    }
    Ok(())
}

/// Whether the attestation `att` is the proof of the queued report `op`, and
/// if not, why not - in words for the report's machine section. Nothing here
/// reads a free-text claim: every check compares two values.
pub fn verify(att: &CopyAttestation, op: &InboxOp) -> Result<(), String> {
    if att.v != ATTESTATION_VERSION {
        return Err(format!("its attestation is format {}, this main machine reads format {ATTESTATION_VERSION}", att.v));
    }
    if att.seal != att.seal_of() {
        return Err("its attestation does not match its own seal - it was edited after the copy's Stop wrote it".to_string());
    }
    if !attest::is_uuid_shaped(&att.session) {
        return Err(format!("its attestation names '{}', which is not a Claude Code session id", att.session));
    }
    if att.tool_use_id.trim().is_empty() {
        return Err("its attestation names no tool-use id of the filing call".to_string());
    }
    if att.capture_id != op.capture_id {
        return Err(format!("its attestation names capture {}, the report was queued as capture {}", att.capture_id, op.capture_id));
    }
    let arg = |key: &str| op.args.get(key).and_then(Value::as_str).unwrap_or_default();
    if att.report != arg("id") {
        return Err(format!("its attestation is for report '{}', the queued report is '{}'", att.report, arg("id")));
    }
    if att.project != arg("project") {
        return Err(format!("its attestation is for project '{}', the queued report is for '{}'", att.project, arg("project")));
    }
    if att.text_sha256 != text_digest(arg("text")) {
        return Err("the report text that was queued is not the text the copy's Stop verified (the sha256 of the text differs)".to_string());
    }
    if crate::eval_filing::unix_of_timestamp(&att.attested_at).is_none() {
        return Err(format!("its attestation carries '{}' as the time of the filing, which is not a time", att.attested_at));
    }
    Ok(())
}

/// The lines of the machine section for a copy's report: what the attestation
/// proves, what a copy's evaluation covers, and what it does not. Plain ASCII,
/// no blank line (the section ends at the agent marker).
pub fn machine_lines(att: Option<&CopyAttestation>, verdict: &Result<(), String>) -> String {
    let mut out = String::from(
        "COPY EVALUATION: this report was written in a session on a read-only copy of the memory and reached this log through the connector's queue.\n",
    );
    match (att, verdict) {
        (Some(a), Ok(())) => {
            out.push_str("Attested by the copy's own Stop hook from that session's Claude Code transcript, and matched here:\n");
            out.push_str(&format!("  session: {}\n  filing call (tool use id): {}\n", a.session, a.tool_use_id));
            out.push_str(&format!("  queue capture id: {}\n  report text sha256: {}\n  filed at (transcript time): {}\n", a.capture_id, a.text_sha256, a.attested_at));
            if a.covered.is_empty() {
                out.push_str("Writes this session queued besides the report: none.\n");
            } else {
                out.push_str(&format!("Writes this session queued besides the report ({}; the same drain applied them, its report says how each landed):\n", a.covered.len()));
                for c in &a.covered {
                    out.push_str(&format!("  - {} {} (capture {})\n", c.tool, if c.id.is_empty() { "(no id)" } else { &c.id }, c.capture_id));
                }
            }
        }
        (a, Err(why)) => {
            out.push_str(&format!("DOES NOT COUNT: {why}.\n"));
            if let Some(a) = a {
                out.push_str(&format!("  the attestation named session {} and filing call {}\n", a.session, a.tool_use_id));
            }
        }
        (None, Ok(())) => out.push_str("(no attestation)\n"),
    }
    out.push_str(
        "Not computed for a copy evaluation, because they live on the main machine and stay owed there: the frozen verdict list \
         (a copy keeps no record of which notes fired), the batch and the A4 list, the disputes to verify and the false-block \
         sweep. The main machine's own weekly check and its teeth trial settle those.\n",
    );
    out
}

// ------------------------------------------------------- is this session connected

/// Whether this copy has a replica connector WIRED: the copy marker's
/// `connector` (any text), else a `thor` tool server of Claude Code's
/// `~/.claude.json` that is an http one. A connector added in claude.ai is in
/// neither place - `session_has_connector` sees that one.
pub fn connector_configured(db: &Path) -> bool {
    let named = thor_core::copy_mode::read_marker(db).connector.is_some_and(|c| !c.trim().is_empty());
    named || crate::plan::default_claude_json().is_some_and(|path| connector_in(&path))
}

/// `connector_configured`, for one Claude Code config file.
pub fn connector_in(claude_json: &Path) -> bool {
    let Some(root) = std::fs::read_to_string(claude_json).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return false };
    root.get("mcpServers")
        .and_then(|servers| servers.get("thor"))
        .and_then(|server| server.get("url"))
        .and_then(Value::as_str)
        .is_some_and(|url| url.starts_with("http://") || url.starts_with("https://"))
}

/// The THOR tools that name a THOR server on their own: the writes, and the
/// code tools and the library tool no other memory server has.
const THOR_TOOLS: &[&str] = &["remember", "revise", "retract", "mark", "pin", "unpin", "resolve", "shelve", "where_used", "search_code", "outline", "library"];

fn is_thor_tool(name: &str) -> bool {
    name.rsplit_once("__").is_some_and(|(server, tool)| server.starts_with("mcp__") && THOR_TOOLS.contains(&tool))
}

/// WHETHER THIS SESSION HAS THE REPLICA CONNECTOR'S TOOLS, from its own
/// transcript (a hook cannot ask Claude Code what tools a session has). Any of:
/// a call of a THOR tool; a result saying "queued for the main machine (...)"
/// (the replica's own reply); or a tool list (`deferred_tools_delta`) naming
/// THOR's `remember` and `where_used` on one server. A tool of a session whose
/// tools are not deferred leaves no trace until it is used, so such a session
/// is not asked before then.
pub fn session_has_connector(files: &[PathBuf]) -> bool {
    use std::io::{BufRead, BufReader};
    for file in files {
        let Ok(handle) = std::fs::File::open(file) else { continue };
        for line in BufReader::new(handle).lines() {
            let Ok(line) = line else { break };
            if !line.contains("mcp__") && !line.contains(QUEUED_PREFIX) {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if lists_thor_tools(&v) {
                return true;
            }
            let Some(blocks) = v.get("message").and_then(|m| m.get("content")).and_then(Value::as_array) else { continue };
            for block in blocks {
                let kind = block.get("type").and_then(Value::as_str);
                let called = kind == Some("tool_use") && block.get("name").and_then(Value::as_str).is_some_and(is_thor_tool);
                let queued = kind == Some("tool_result") && attest::result_text(block.get("content")).contains(QUEUED_PREFIX);
                if called || queued {
                    return true;
                }
            }
        }
    }
    false
}

/// A `deferred_tools_delta` attachment (Claude Code's list of the tools a
/// session has, written when they are announced) that names `remember` and
/// `where_used` of one MCP server.
fn lists_thor_tools(line: &Value) -> bool {
    let Some(attachment) = line.get("attachment").filter(|a| a.get("type").and_then(Value::as_str) == Some("deferred_tools_delta")) else { return false };
    let Some(names) = attachment.get("addedNames").and_then(Value::as_array) else { return false };
    let names: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
    let servers_with = |tool: &str| -> HashSet<&str> { names.iter().filter_map(|n| n.strip_suffix(tool)?.strip_suffix("__")).collect() };
    let withwhere = servers_with("where_used");
    servers_with("remember").iter().any(|server| server.starts_with("mcp__") && withwhere.contains(server))
}

// ------------------------------------------------- the copy reading its transcript

/// A THOR write the session made, as its transcript holds it.
#[derive(Debug, Clone)]
struct QueuedWrite {
    tool_use_id: String,
    tool: String,
    input: Value,
    capture_id: String,
    result_at: Option<String>,
}

/// What the transcript says about the weekly-check report of the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filing {
    /// No filing call of an evaluation report in the session.
    None,
    /// A filing call whose result is not on disk yet.
    Pending,
    /// A filing call with a result that names its queue capture.
    Found(Box<CopyAttestation>),
}

/// The THOR write calls of the session's transcripts, in order, one per tool
/// use id: (id, tool name as the transcript spells it, input).
fn thor_write_calls(files: &[PathBuf]) -> Vec<(String, String, Value)> {
    use std::io::{BufRead, BufReader};
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for file in files {
        let Ok(handle) = std::fs::File::open(file) else { continue };
        for line in BufReader::new(handle).lines() {
            let Ok(line) = line else { break };
            if !line.contains("\"tool_use\"") || !line.contains("mcp__") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            let Some(blocks) = v.get("message").and_then(|m| m.get("content")).and_then(Value::as_array) else { continue };
            for block in blocks.iter().filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use")) {
                let name = block.get("name").and_then(Value::as_str).unwrap_or_default();
                let Some(id) = block.get("id").and_then(Value::as_str) else { continue };
                if attest::write_tool(name).is_some() && seen.insert(id.to_string()) {
                    out.push((id.to_string(), name.to_string(), block.get("input").cloned().unwrap_or(Value::Null)));
                }
            }
        }
    }
    out
}

/// The capture id in a connector reply: "queued for the main machine (<tool>,
/// capture <id>)".
fn capture_in(reply: &str, tool: &str) -> Option<String> {
    let at = reply.find(QUEUED_PREFIX)? + QUEUED_PREFIX.len();
    let inner = reply[at..].split(')').next()?;
    let (named, id) = inner.split_once(", capture ")?;
    (named.trim() == tool && !id.trim().is_empty()).then(|| id.trim().to_string())
}

fn is_report_filing(tool: &str, input: &Value) -> bool {
    tool == "remember"
        && input.get("kind").and_then(Value::as_str) == Some("report")
        && input.get("tags").and_then(Value::as_array).is_some_and(|tags| tags.iter().any(|t| t.as_str() == Some(REPORT_TAG)))
}

/// Read the session's transcripts for its weekly-check report of `project`
/// (any project when `project` is `None`) and build the attestation of it.
pub fn find_filing(files: &[PathBuf], session: &str, project: Option<&str>) -> Filing {
    let calls = thor_write_calls(files);
    if calls.is_empty() {
        return Filing::None;
    }
    let ids: HashSet<&str> = calls.iter().map(|(id, _, _)| id.as_str()).collect();
    let found = attest::find_calls(files, &ids);
    let mut pending = false;
    let mut queued: Vec<QueuedWrite> = Vec::new();
    for (id, name, input) in &calls {
        let tool = attest::write_tool(name).unwrap_or_default().to_string();
        let Some(attest::Found::Call(call)) = found.get(id) else { continue };
        if call.name != *name || call.input != *input {
            continue;
        }
        let ok_results: Vec<&String> = call.results.iter().filter(|(is_error, _)| !is_error).map(|(_, t)| t).collect();
        let Some(capture_id) = ok_results.iter().find_map(|t| capture_in(t, &tool)) else {
            if call.results.is_empty() && is_report_filing(&tool, input) {
                pending = true;
            }
            continue;
        };
        let result_at = call.result_times.iter().find(|(text, _)| text.contains(&capture_id)).map(|(_, at)| at.clone());
        queued.push(QueuedWrite { tool_use_id: id.clone(), tool, input: input.clone(), capture_id, result_at });
    }
    let filing = queued
        .iter()
        .rev()
        .find(|w| is_report_filing(&w.tool, &w.input) && project.is_none_or(|p| w.input.get("project").and_then(Value::as_str) == Some(p)));
    let Some(filing) = filing else {
        return if pending { Filing::Pending } else { Filing::None };
    };
    let text = |key: &str| filing.input.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    let Some(attested_at) = filing.result_at.clone() else { return Filing::Pending };
    let covered = queued
        .iter()
        .filter(|w| w.tool_use_id != filing.tool_use_id)
        .take(40)
        .map(|w| Covered {
            tool: w.tool.clone(),
            id: w.input.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
            capture_id: w.capture_id.clone(),
        })
        .collect();
    Filing::Found(Box::new(
        CopyAttestation {
            v: ATTESTATION_VERSION,
            session: session.to_string(),
            project: text("project"),
            report: text("id"),
            tool_use_id: filing.tool_use_id.clone(),
            capture_id: filing.capture_id.clone(),
            text_sha256: text_digest(&text("text")),
            attested_at,
            covered,
            seal: String::new(),
        }
        .sealed(),
    ))
}

// ---------------------------------------------------------- the copy's own state

/// What a copy remembers about its weekly checks, in `copy-eval.json` beside
/// the store. Written directly, not through `store_dir::write`: the copy's
/// directory is held read-only for every other state file, and this one is
/// the copy's own (it never travels and the main machine never reads it).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyEvalState {
    #[serde(default)]
    pub asks: BTreeMap<String, Ask>,
}

/// One weekly check asked of one session in one project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    pub session: String,
    pub asked_unix: i64,
    #[serde(default)]
    pub holds: u32,
    /// When the session was handed the call that sends the proof of its
    /// report (held for once, never again for the same ask).
    #[serde(default)]
    pub sent_unix: Option<i64>,
    #[serde(default)]
    pub report: Option<String>,
    #[serde(default)]
    pub gave_up: bool,
}

pub fn state_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("copy-eval.json")
}

pub fn read_state(db: &Path) -> CopyEvalState {
    std::fs::read_to_string(state_path(db)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn write_state(db: &Path, state: &CopyEvalState) {
    if let Ok(text) = serde_json::to_string(state) {
        let _ = std::fs::write(state_path(db), text);
    }
}

// ------------------------------------------------------------- the copy's Stop

/// What the copy's Stop decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyStop {
    /// Hold the turn with this ask.
    Ask(String),
    /// Hold nothing; say this one line.
    Say(String),
    Quiet,
}

fn ask_text(project: &str, date: &str) -> String {
    format!(
        "[THOR] The weekly check of '{project}' is due. This machine runs a read-only copy of the memory, so the report is filed \
         through the connector (the 'thor' tool server), which queues it for the main machine. Tell the owner in one plain sentence \
         what you are about to do; nothing is needed from them. Then, in this turn:\n\
         1. Judge what this session used from the memory. For each note that helped, call mcp__thor__mark with its id; for one that \
         did not belong where it fired, call it with noise true. Correct a note that is wrong with mcp__thor__revise, and remove one \
         that is simply gone with mcp__thor__retract (a reason is required). All of these are queued for the main machine. A copy \
         keeps no record of which notes fired, so judge from what you saw in this session; if you cannot honestly judge one, leave it.\n\
         2. File the report with the THOR tool and only with it: mcp__thor__remember(id: \"eval-{project}-{date}\", kind: \"report\", \
         project: \"{project}\", tags: [\"evaluation-report\"], text: your own summary in plain words - what you judged, what you \
         corrected, and anything that chafed). Do not write a machine section; the main machine adds it.\n\
         3. End the turn once. THOR then checks the filing against this session's own transcript and hands you one more call to \
         make: the proof of the filing, for the main machine. Make that call and end the turn again; the report counts when the main \
         machine has applied the queue, within the hour.\n\
         The parts of an evaluation that live on the main machine (the frozen verdict list, the batch of rules for the teeth trial, \
         the disputes and the false-block sweep) are not part of an evaluation made here: they stay owed there and its own weekly \
         check settles them."
    )
}

fn rehold_text(project: &str, left: u32) -> String {
    format!(
        "[THOR] The weekly-check report of '{project}' is not filed yet. File it now with mcp__thor__remember (kind report, project \
         '{project}', tags [\"evaluation-report\"], id eval-{project}-<date>, your own summary as the text), then end the turn. \
         You can be held {left} more time(s) for it in this session."
    )
}

/// The id of the note that carries the attestation of the report queued as
/// `capture_id`.
pub fn attestation_id(capture_id: &str) -> String {
    format!("eval-attest-{capture_id}")
}

/// The hold that hands the session the call that sends its proof: the exact
/// `remember` arguments, the attestation as the text.
fn proof_text(att: &CopyAttestation) -> String {
    format!(
        "[THOR] Your weekly-check report '{report}' is filed and queued. One more call sends the main machine the proof of it \
         (a copy-attestation, taken from this session's own transcript). Tell the owner in one plain sentence that you are sending \
         the proof of the weekly check; nothing is needed from them. Then call mcp__thor__remember with exactly these arguments and \
         end the turn. Copy the text character for character: do not change, wrap, shorten or explain it, because the main machine \
         refuses a proof that was edited.\n\
         id: \"{id}\"\nkind: \"report\"\nproject: \"{project}\"\ntags: [\"{tag}\"]\ntext: {text}\n\
         The connector queues it like any other write; the main machine reads it to file your report and does not keep it as a note. \
         You are not held for it again. The report counts once the main machine has applied the queue, within the hour.",
        report = att.report,
        id = attestation_id(&att.capture_id),
        project = att.project,
        tag = ATTESTATION_TAG,
        text = att.to_text()
    )
}

/// THE COPY'S WEEKLY CHECK AT A STOP (see the module doc). `folder_project` is
/// the project the Stop was counted in; `transcripts` the root of Claude Code's
/// transcripts; `may_ask` is false at a Stop that may not start a new ask (a
/// second pass of a held turn, or a transcript THOR cannot read). Nothing here
/// writes the store, and nothing is held for a session that shows no sign of
/// the connector's tools (`session_has_connector`, `connector_configured`).
pub fn copy_stop(db: &Path, session: &str, folder_project: Option<&str>, transcripts: Option<&Path>, now: i64, may_ask: bool) -> CopyStop {
    let Some(root) = transcripts else { return CopyStop::Quiet };
    let _hold = crate::store_dir::hold_copy(db);
    let mut state = read_state(db);
    // The ask this session already holds: hand it the call that sends the
    // proof when its report is filed, hold again while it is not (a bounded
    // number of times).
    let open = state.asks.iter().find(|(_, a)| a.session == session && a.sent_unix.is_none() && !a.gave_up).map(|(p, _)| p.clone());
    if let Some(project) = open {
        let files = attest::session_transcripts(root, session);
        match find_filing(&files, session, Some(&project)) {
            Filing::Found(att) => {
                if let Some(ask) = state.asks.get_mut(&project) {
                    ask.sent_unix = Some(now);
                    ask.report = Some(att.report.clone());
                }
                write_state(db, &state);
                return CopyStop::Ask(proof_text(&att));
            }
            Filing::Pending => return CopyStop::Quiet,
            Filing::None => {
                let Some(ask) = state.asks.get_mut(&project) else { return CopyStop::Quiet };
                if ask.holds < MAX_REHOLDS {
                    ask.holds += 1;
                    let left = MAX_REHOLDS - ask.holds;
                    write_state(db, &state);
                    return CopyStop::Ask(rehold_text(&project, left));
                }
                ask.gave_up = true;
                write_state(db, &state);
                return CopyStop::Say(format!(
                    "[THOR] The weekly-check report of '{project}' was not filed in this session, so nothing is held for it any \
                     more here. It is asked again in a later session."
                ));
            }
        }
    }
    if !may_ask {
        return CopyStop::Quiet;
    }
    let Ok(store) = crate::store_dir::open_existing(db) else { return CopyStop::Quiet };
    let timeline = attest::session_timeline(root, session);
    let candidates = crate::usefulness::projects_of_session(
        &crate::usefulness::read_eval_debt_state(db),
        session,
        folder_project,
        &attest::projects_in(&timeline),
    );
    for project in candidates {
        let recently = |a: &Ask| a.sent_unix.is_some_and(|t| now - t < SUPPRESS_AFTER_SEND_SECS) || (a.sent_unix.is_none() && a.session != session && now - a.asked_unix < 1800);
        if state.asks.get(&project).is_some_and(recently) {
            continue;
        }
        let facts = crate::usefulness::update_eval_debt_state(&store, db, Some(&project), now);
        let stop_here = folder_project == Some(project.as_str());
        let decision = crate::usefulness::stop_decision(
            &store,
            &facts,
            &project,
            session,
            &timeline,
            stop_here,
            true,
            now,
            crate::usefulness::cadence_in(db),
        );
        if !matches!(decision, crate::usefulness::StopDecision::Ask(_)) {
            continue;
        }
        // Due. It is asked only where the report can be filed: a session with
        // no sign of THOR's connector tools is left alone, and says nothing.
        if !connector_configured(db) && !session_has_connector(&attest::session_transcripts(root, session)) {
            return CopyStop::Quiet;
        }
        state.asks.insert(
            project.clone(),
            Ask { session: session.to_string(), asked_unix: now, holds: 0, sent_unix: None, report: None, gave_up: false },
        );
        write_state(db, &state);
        let date: String = crate::time::iso8601_from_unix(now).chars().take(10).collect();
        return CopyStop::Ask(ask_text(&project, &date));
    }
    CopyStop::Quiet
}

// ------------------------------------------------- the main machine's half (drain)

/// What the drain holds between runs: reports waiting for their attestation,
/// attestations waiting for their report, and the captures already settled
/// (the inbox is at-least-once, so a replayed batch must not file twice).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pending {
    #[serde(default)]
    pub reports: Vec<InboxOp>,
    #[serde(default)]
    pub attests: Vec<(i64, CopyAttestation)>,
    #[serde(default)]
    pub done: Vec<String>,
}

/// Where the drain keeps `Pending`: beside the store.
pub fn pending_path(db: &Path) -> PathBuf {
    let mut name = db.file_name().unwrap_or_else(|| std::ffi::OsStr::new("thor.db")).to_os_string();
    name.push(".copy-evals.json");
    db.with_file_name(name)
}

impl Pending {
    pub fn read(db: &Path) -> Self {
        std::fs::read_to_string(pending_path(db)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn write(&self, db: &Path) -> std::io::Result<()> {
        std::fs::write(pending_path(db), serde_json::to_string(self).unwrap_or_default())
    }

    pub fn is_empty(&self) -> bool {
        self.reports.is_empty() && self.attests.is_empty()
    }
}

/// Whether a captured call is a copy's weekly-check report.
pub fn is_copy_report(op: &InboxOp) -> bool {
    op.tool == "remember" && is_report_filing("remember", &op.args)
}

/// Whether a captured call carries a copy's attestation: the note the copy's
/// Stop asked its session to write (a report tagged `copy-attestation`), or
/// the call a 2.6.2 connector made of a posted one.
pub fn is_copy_attestation(op: &InboxOp) -> bool {
    let tagged = op.args.get("kind").and_then(Value::as_str) == Some("report")
        && op.args.get("tags").and_then(Value::as_array).is_some_and(|tags| tags.iter().any(|t| t.as_str() == Some(ATTESTATION_TAG)));
    op.tool == LEGACY_ATTEST_TOOL || (op.tool == "remember" && tagged)
}

/// What one drain's settling did.
#[derive(Debug, Default)]
pub struct Settled {
    pub lines: Vec<String>,
    pub applied: usize,
    pub not_applied: usize,
    /// Reports that could not be filed at all (kept in the stranded file).
    pub stranded: Vec<InboxOp>,
}

/// Take one captured op into `pending`: a report is held, an attestation is
/// parsed and kept. `Err` when an attestation cannot be read. A capture
/// already settled earlier is skipped with a line.
pub fn absorb(pending: &mut Pending, op: &InboxOp, now: i64) -> Result<String, String> {
    if is_copy_attestation(op) {
        let att = match op.tool == LEGACY_ATTEST_TOOL {
            true => serde_json::from_value(op.args.clone()).map_err(|e| e.to_string()),
            false => CopyAttestation::from_text(op.args.get("text").and_then(Value::as_str).unwrap_or_default()),
        }
        .map_err(|e| format!("copy attestation does not parse: {e}"))?;
        if pending.done.contains(&att.capture_id) || pending.attests.iter().any(|(_, a)| a.capture_id == att.capture_id) {
            return Ok(format!("attestation for capture {} is already known", att.capture_id));
        }
        let line = format!("attestation for report '{}' (capture {}) received", att.report, att.capture_id);
        pending.attests.push((now, att));
        return Ok(line);
    }
    let id = op.args.get("id").and_then(Value::as_str).unwrap_or("?").to_string();
    if pending.done.contains(&op.capture_id) || pending.reports.iter().any(|r| r.capture_id == op.capture_id) {
        return Ok(format!("copy evaluation report '{id}' (capture {}) is already known", op.capture_id));
    }
    pending.reports.push(op.clone());
    Ok(format!("copy evaluation report '{id}' (capture {}) held until its attestation arrives", op.capture_id))
}

/// The log seq of the filing of report `id` when that filing counts (its
/// validator's record is valid), else `None`.
fn counting_filing_seq(store: &EventStore, id: &str) -> Option<i64> {
    crate::eval_filing::current_validation(store, id).filter(|v| v.valid).map(|v| v.report_seq)
}

fn captured_at(op: &InboxOp) -> i64 {
    op.ts.parse().unwrap_or(0)
}

fn report_item(op: &InboxOp) -> Result<Item, String> {
    let text = |key: &str| op.args.get(key).and_then(Value::as_str).map(str::to_string);
    let tags: Vec<String> = op
        .args
        .get("tags")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Ok(Item {
        id: text("id").ok_or("the queued report has no id")?,
        kind: Kind::Report,
        text: text("text").ok_or("the queued report has no text")?,
        bindings: Vec::new(),
        severity: None,
        project: Some(text("project").ok_or("the queued report has no project")?),
        tags,
        expires: text("expires"),
        key: None,
        falsifier: None,
        check: None,
    })
}

/// SETTLE every held report that has its attestation, or has waited too long:
/// verify, then file it on the log (valid or dead, with the reason). Reports
/// still waiting stay in `pending` with one line each. An attestation whose
/// report never came is dropped after the same wait.
pub fn settle(store: &mut EventStore, pending: &mut Pending, now: i64) -> Settled {
    let mut out = Settled::default();
    let mut reports = std::mem::take(&mut pending.reports);
    // EXACTLY ONE REPORT STANDS UNDER AN ID. A session can file the same report
    // id twice (the first filing never got its proof, the second did), so the
    // proven ones settle first, newest capture first: the one that counts is
    // on the log before an unproven or older one is looked at, and that one is
    // then closed as superseded instead of being filed dead on top of it.
    reports.sort_by_key(|op| (!pending.attests.iter().any(|(_, a)| a.capture_id == op.capture_id), std::cmp::Reverse(captured_at(op))));
    for op in reports {
        let id = op.args.get("id").and_then(Value::as_str).unwrap_or("?").to_string();
        let found = pending.attests.iter().position(|(_, a)| a.capture_id == op.capture_id);
        if let Some(seq) = counting_filing_seq(store, &id) {
            if let Some(at) = found {
                pending.attests.remove(at);
            }
            pending.done.push(op.capture_id.clone());
            out.lines.push(format!(
                "SUPERSEDED copy evaluation report '{id}' (capture {}): a report with that id already counts (log seq {seq}), so this one is closed and not filed",
                op.capture_id
            ));
            continue;
        }
        let expired = now - captured_at(&op) > WAIT_FOR_ATTESTATION_SECS;
        let (attestation, verdict) = match (found, expired) {
            (Some(at), _) => {
                let (_, att) = pending.attests.remove(at);
                let verdict = verify(&att, &op);
                (Some(att), verdict)
            }
            (None, true) => (
                None,
                Err("no attestation arrived within 24 hours - the copy's own Stop sends it after reading the session's transcript, so a session that ended before its Stop, or a copy that could not reach the connector, leaves a report nothing vouches for".to_string()),
            ),
            (None, false) => {
                out.lines.push(format!("HELD copy evaluation report '{id}' (capture {}): its attestation has not arrived yet", op.capture_id));
                pending.reports.push(op);
                continue;
            }
        };
        let item = match report_item(&op) {
            Ok(item) => item,
            Err(why) => {
                out.not_applied += 1;
                out.lines.push(format!("LOST copy evaluation report '{id}': {why}"));
                out.stranded.push(op);
                continue;
            }
        };
        let session = attestation.as_ref().map(|a| a.session.clone()).filter(|s| attest::is_uuid_shaped(s)).unwrap_or_else(|| "copy-session".to_string());
        let lines = machine_lines(attestation.as_ref(), &verdict);
        let filed_at = attestation.as_ref().map(|a| a.attested_at.clone()).filter(|_| verdict.is_ok());
        let filing = model::store::CopyFiling { session: &session, report: &item, machine_lines: &lines, filed_at: filed_at.as_deref(), verdict: verdict.clone() };
        match model::store::file_copy_evaluation(store, &filing) {
            Ok((event, counts)) => {
                out.applied += 1;
                pending.done.push(op.capture_id.clone());
                out.lines.push(match counts {
                    true => format!("OK   copy evaluation report '{id}' filed at log seq {} and COUNTS (attested by the copy's Stop)", event.seq),
                    false => format!(
                        "DEAD copy evaluation report '{id}' filed at log seq {} and does NOT count: {}",
                        event.seq,
                        verdict.err().unwrap_or_default()
                    ),
                });
            }
            Err(e) => {
                out.not_applied += 1;
                out.lines.push(format!("LOST copy evaluation report '{id}': the write gate refused it: {e}"));
                out.stranded.push(op);
            }
        }
    }
    let before = pending.attests.len();
    pending.attests.retain(|(received, _)| now - received <= WAIT_FOR_ATTESTATION_SECS);
    if pending.attests.len() < before {
        out.lines.push(format!("dropped {} attestation(s) whose report never arrived within 24 hours", before - pending.attests.len()));
    }
    for (_, att) in &pending.attests {
        out.lines.push(format!("HELD attestation for report '{}' (capture {}): its report has not arrived yet", att.report, att.capture_id));
    }
    let keep = pending.done.len().saturating_sub(500);
    pending.done.drain(..keep);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attest::fixture;

    const SESSION: &str = "11111111-2222-4333-8444-555555555555";

    fn filing_input(text: &str) -> Value {
        json!({"id": "eval-demo-2026-10-26", "kind": "report", "project": "demo", "tags": ["evaluation-report"], "text": text})
    }

    fn write_filing(root: &Path, text: &str, capture: &str) -> Value {
        let input = filing_input(text);
        fixture::main_transcript(root, SESSION)
            .at("2026-10-26T10:00:00.000Z")
            .let_through(
                "toolu_mark",
                "mcp__thor__mark",
                &json!({"id": "some-note"}),
                "x",
                &format!("queued for the main machine (mark, capture {capture}-m). This connector is a replica"),
            )
            .let_through(
                "toolu_file",
                "mcp__thor__remember",
                &input,
                "x",
                &format!("queued for the main machine (remember, capture {capture}). This connector is a replica"),
            )
            .write();
        input
    }

    #[test]
    fn the_filing_is_read_from_the_transcript_into_a_sealed_attestation() {
        let dir = tempfile::tempdir().unwrap();
        write_filing(dir.path(), "Judged two notes.", "cap-1");
        let files = attest::session_transcripts(dir.path(), SESSION);
        let Filing::Found(att) = find_filing(&files, SESSION, Some("demo")) else { panic!("filing not found") };
        assert_eq!(att.report, "eval-demo-2026-10-26");
        assert_eq!(att.tool_use_id, "toolu_file");
        assert_eq!(att.capture_id, "cap-1");
        assert_eq!(att.text_sha256, text_digest("Judged two notes."));
        assert_eq!(att.attested_at, "2026-10-26T10:00:00.000Z");
        assert_eq!(att.covered, vec![Covered { tool: "mark".into(), id: "some-note".into(), capture_id: "cap-1-m".into() }]);
        assert_eq!(att.seal, att.seal_of());
        assert_eq!(find_filing(&files, SESSION, Some("other")), Filing::None, "another project is not this filing");
    }

    #[test]
    fn a_call_without_a_result_is_pending_and_an_error_result_is_no_filing() {
        let dir = tempfile::tempdir().unwrap();
        fixture::main_transcript(dir.path(), SESSION).call("toolu_file", "mcp__thor__remember", &filing_input("x")).write();
        let files = attest::session_transcripts(dir.path(), SESSION);
        assert_eq!(find_filing(&files, SESSION, None), Filing::Pending);
        let dir = tempfile::tempdir().unwrap();
        fixture::main_transcript(dir.path(), SESSION)
            .let_through("toolu_file", "mcp__thor__remember", &filing_input("x"), "x", "REFUSED: nope")
            .write();
        let files = attest::session_transcripts(dir.path(), SESSION);
        assert_eq!(find_filing(&files, SESSION, None), Filing::None);
    }

    fn op_for(att: &CopyAttestation, text: &str) -> InboxOp {
        let mut op = InboxOp::new("remember", filing_input(text));
        op.capture_id = att.capture_id.clone();
        op
    }

    fn good_attestation(text: &str) -> CopyAttestation {
        CopyAttestation {
            v: ATTESTATION_VERSION,
            session: SESSION.into(),
            project: "demo".into(),
            report: "eval-demo-2026-10-26".into(),
            tool_use_id: "toolu_file".into(),
            capture_id: "cap-1".into(),
            text_sha256: text_digest(text),
            attested_at: "2026-10-26T10:00:00.000Z".into(),
            covered: vec![],
            seal: String::new(),
        }
        .sealed()
    }

    #[test]
    fn a_matching_attestation_verifies_and_each_forgery_says_why() {
        let att = good_attestation("Judged two notes.");
        let op = op_for(&att, "Judged two notes.");
        assert_eq!(verify(&att, &op), Ok(()));

        let edited_text = op_for(&att, "Judged ten notes.");
        assert!(verify(&att, &edited_text).unwrap_err().contains("sha256 of the text differs"));

        let mut moved = att.clone();
        moved.tool_use_id = "toolu_other".into();
        assert!(verify(&moved, &op).unwrap_err().contains("own seal"), "a changed field breaks the seal");

        let mut other_session = att.clone();
        other_session.session = "not-a-session".into();
        let other_session = other_session.sealed();
        assert!(verify(&other_session, &op).unwrap_err().contains("not a Claude Code session id"));

        let mut other_capture = att.clone();
        other_capture.capture_id = "cap-2".into();
        let other_capture = other_capture.sealed();
        assert!(verify(&other_capture, &op).unwrap_err().contains("capture cap-2"));
    }

    #[test]
    fn a_configured_connector_is_found_in_claude_json() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("claude.json");
        std::fs::write(&cfg, r#"{"mcpServers":{"thor":{"type":"http","url":"https://mcp.example.com/mcp"}}}"#).unwrap();
        assert!(connector_in(&cfg));
        std::fs::write(&cfg, r#"{"mcpServers":{"thor":{"type":"stdio","command":"mcp"}}}"#).unwrap();
        assert!(!connector_in(&cfg), "a local tool server is no connector");
    }

    #[test]
    fn the_proof_text_is_the_exact_call_and_reads_back_into_the_same_attestation() {
        let att = good_attestation("Judged two notes.");
        let text = proof_text(&att);
        assert!(text.contains("mcp__thor__remember") && text.contains("id: \"eval-attest-cap-1\""), "{text}");
        assert!(text.contains("tags: [\"copy-attestation\"]") && text.contains("project: \"demo\""), "{text}");
        let line = text.lines().find_map(|l| l.strip_prefix("text: ")).expect("the text line");
        assert_eq!(CopyAttestation::from_text(line), Ok(att.clone()));
        assert_eq!(CopyAttestation::from_text(&format!("```json
{line}
```")), Ok(att.clone()), "a code fence around it is tolerated");
        assert_eq!(check_attestation_note("eval-attest-cap-1", Some("demo"), line), Ok(()));
    }

    #[test]
    fn a_changed_attestation_note_is_refused_on_the_spot() {
        let att = good_attestation("Judged two notes.");
        let line = att.to_text();
        let edited = line.replace("toolu_file", "toolu_other");
        assert!(check_attestation_note("eval-attest-cap-1", Some("demo"), &edited).unwrap_err().contains("own seal"));
        assert!(check_attestation_note("eval-attest-cap-1", Some("demo"), "I sent the proof.").unwrap_err().contains("copied exactly"));
        let wrong_id = check_attestation_note("eval-attest-cap-9", Some("demo"), &line).unwrap_err();
        assert!(wrong_id.contains("eval-attest-cap-1") && wrong_id.contains("'demo'"), "{wrong_id}");
        assert!(check_attestation_note("eval-attest-cap-1", Some("other"), &line).is_err());
    }

    #[test]
    fn a_queued_attestation_note_is_recognised_by_the_drain() {
        let att = good_attestation("Judged two notes.");
        let note = |tags: Value, kind: &str| InboxOp::new("remember", json!({"id": "eval-attest-cap-1", "kind": kind, "project": "demo", "tags": tags, "text": att.to_text()}));
        assert!(is_copy_attestation(&note(json!(["copy-attestation"]), "report")));
        assert!(!is_copy_attestation(&note(json!(["evaluation-report"]), "report")), "a report is not its proof");
        assert!(!is_copy_attestation(&note(json!(["copy-attestation"]), "rule")));
        assert!(is_copy_attestation(&InboxOp::new(LEGACY_ATTEST_TOOL, serde_json::to_value(&att).unwrap())));
        let mut pending = Pending::default();
        assert!(absorb(&mut pending, &note(json!(["copy-attestation"]), "report"), 100).unwrap().contains("received"));
        assert_eq!(pending.attests.len(), 1);
        let mangled = InboxOp::new("remember", json!({"id": "x", "kind": "report", "project": "demo", "tags": ["copy-attestation"], "text": "no proof here"}));
        assert!(absorb(&mut pending, &mangled, 100).unwrap_err().contains("does not parse"));
    }

    fn lines_to_transcript(root: &Path, lines: &[Value]) -> Vec<PathBuf> {
        let mut transcript = fixture::main_transcript(root, SESSION);
        for line in lines {
            transcript.raw(&line.to_string());
        }
        transcript.write();
        attest::session_transcripts(root, SESSION)
    }

    #[test]
    fn a_session_shows_the_connector_by_its_calls_its_queued_replies_or_its_tool_list() {
        let dir = tempfile::tempdir().unwrap();
        let none = lines_to_transcript(dir.path(), &[json!({"type": "user", "message": {"role": "user", "content": "mcp__thor__remember is just a word here"}})]);
        assert!(!session_has_connector(&none), "a word in a prompt proves nothing");
        let other = tempfile::tempdir().unwrap();
        let tool_list = |names: Value| json!({"type": "attachment", "attachment": {"type": "deferred_tools_delta", "addedNames": names}});
        let files = lines_to_transcript(other.path(), &[tool_list(json!(["mcp__a1b2__remember", "mcp__docs__where_used"]))]);
        assert!(!session_has_connector(&files), "two servers: no one of them is THOR's");
        let again = tempfile::tempdir().unwrap();
        let files = lines_to_transcript(again.path(), &[tool_list(json!(["Bash", "mcp__a1b2__remember", "mcp__a1b2__where_used", "mcp__a1b2__lookup"]))]);
        assert!(session_has_connector(&files), "THOR's tool list");
        let called = tempfile::tempdir().unwrap();
        let files = lines_to_transcript(
            called.path(),
            &[json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "mcp__claude_ai_Thor__mark", "input": {"id": "x"}}]}})],
        );
        assert!(session_has_connector(&files), "a call of a THOR tool");
        let replied = tempfile::tempdir().unwrap();
        let files = lines_to_transcript(
            replied.path(),
            &[json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "queued for the main machine (revise, capture c9). This connector is a replica"}]}})],
        );
        assert!(session_has_connector(&files), "a replica's own reply");
    }
}
