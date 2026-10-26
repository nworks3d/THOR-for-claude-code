//! THE WEEKLY CHECK ON A READ-ONLY COPY (owner decision 2026-10-26: "the report
//! is made where the session runs").
//!
//! A copy cannot write the log, so it cannot start an evaluation or file a
//! report the way the main machine does. What it CAN do is what a session on
//! it already does: call the THOR tool server, which on a copy is the replica
//! connector, and the connector QUEUES every write for the main machine.
//! This module is the rest of that road, in two halves.
//!
//! THE COPY'S HALF (`copy_stop`, run by the Stop hook):
//!
//! 1. When the project is due (the same rhythm as the main machine: the cadence
//!    of the plan, ten minutes of the session's own work, never its first Stop,
//!    `usefulness::stop_decision`) and the connector answers, the Stop holds the
//!    turn with a copy-specific ask: judge, correct, and file the report with
//!    `mcp__thor__remember` (kind report, tag evaluation-report). The connector
//!    queues it and answers "queued ... capture <id>".
//! 2. At the next Stop, THOR reads Claude Code's own transcript of the session
//!    (`find_filing`): the filing must be a real tool call of that session with
//!    a non-error result naming its queue capture id. It then builds an
//!    ATTESTATION (`CopyAttestation`: session, project, report id, tool-use id,
//!    capture id, the sha256 of the report text, the transcript time of the
//!    result, the session's other queued writes, and a seal over all of it)
//!    and posts it to the connector (`POST <connector>/copy/attest`), which
//!    queues it in the SAME inbox as a `copy_attest` call. No MCP client is
//!    needed: the Stop speaks plain HTTP (`Net`).
//!
//! THE MAIN MACHINE'S HALF (`settle`, run by `sync drain`): a queued
//! evaluation report is not replayed as an ordinary `remember` (it would be
//! refused: no evaluation start is open on this log for it). It is HELD until
//! an attestation with its capture id arrives. Then `verify` compares the two:
//! the seal, the capture id, the report id, the project and the sha256 of the
//! queued text. A match files the report with the machine section THOR composes
//! from the attestation (`machine_lines`) and a valid validator record, so the
//! report COUNTS like any other (`model::store::file_copy_evaluation`). Any
//! mismatch files it dead with the reason. No attestation within a day files
//! it dead too. Nothing is ever counted on a text's word.
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
//! tunnel is the gate), so the attestation proves that the report is the one
//! the copy's Stop verified in a transcript, not who sent it. A forged one
//! needs the connector's address and a report that was really queued.

use crate::attest;
use model::item::{Item, Kind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use thor_core::event_store::EventStore;
use thor_core::inbox::InboxOp;

/// The captured call name of an attestation in the connector's inbox.
pub const ATTEST_TOOL: &str = "copy_attest";
/// The format of `CopyAttestation`.
pub const ATTESTATION_VERSION: u32 = 1;
/// How long a held report waits for its attestation before it is filed dead.
pub const WAIT_FOR_ATTESTATION_SECS: i64 = 24 * 3600;
/// How long a project is not asked again after its report was sent: the main
/// machine has to drain, ship, and the copy to pull before the log shows it.
pub const SUPPRESS_AFTER_SEND_SECS: i64 = 24 * 3600;
/// How many times one turn is held again for a report that is not filed.
pub const MAX_REHOLDS: u32 = 2;
/// How long an unreachable connector is left alone before the next try.
const UNREACHABLE_BACKOFF_SECS: i64 = 600;
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

// -------------------------------------------------------- the transport (copy)

/// What the Stop needs from the network: one GET to see the connector answer,
/// one POST of the attestation. Behind a trait so a test needs no server.
pub trait Net {
    fn get(&self, url: &str) -> Result<(), String>;
    fn post_json(&self, url: &str, body: &str) -> Result<(), String>;
}

/// The real transport: plain `http://` over a socket (the LAN or a loopback
/// connector), `https://` through the system's own `curl` (a tunnel in front
/// of the connector), both under a few seconds' limit, no console window.
pub struct RealNet;

const NET_LIMIT: std::time::Duration = std::time::Duration::from_secs(4);

impl Net for RealNet {
    fn get(&self, url: &str) -> Result<(), String> {
        request("GET", url, None)
    }
    fn post_json(&self, url: &str, body: &str) -> Result<(), String> {
        request("POST", url, Some(body))
    }
}

fn request(method: &str, url: &str, body: Option<&str>) -> Result<(), String> {
    match url.strip_prefix("http://") {
        Some(rest) => plain_http(method, rest, body),
        None if url.starts_with("https://") => curl(method, url, body),
        None => Err(format!("'{url}' is not an http or https address")),
    }
}

fn plain_http(method: &str, rest: &str, body: Option<&str>) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::net::ToSocketAddrs;
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    let target = if host.contains(':') { host.to_string() } else { format!("{host}:80") };
    let addr = target.to_socket_addrs().map_err(|e| format!("{host}: {e}"))?.next().ok_or_else(|| format!("{host}: no address"))?;
    let mut stream = std::net::TcpStream::connect_timeout(&addr, NET_LIMIT).map_err(|e| format!("{host}: {e}"))?;
    let _ = stream.set_read_timeout(Some(NET_LIMIT));
    let _ = stream.set_write_timeout(Some(NET_LIMIT));
    let payload = body.unwrap_or("");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        payload.len()
    );
    stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(payload.as_bytes())).map_err(|e| format!("{host}: {e}"))?;
    let mut reply = Vec::new();
    let _ = stream.take(64 * 1024).read_to_end(&mut reply);
    let text = String::from_utf8_lossy(&reply);
    let status = text.lines().next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse::<u16>().ok());
    match status {
        Some(code) if (200..300).contains(&code) => Ok(()),
        Some(code) => Err(format!("{host} answered {code}")),
        None => Err(format!("{host} did not answer")),
    }
}

fn curl(method: &str, url: &str, body: Option<&str>) -> Result<(), String> {
    let mut command = std::process::Command::new("curl");
    command.args(["--silent", "--show-error", "--fail", "--max-time", "6", "-o"]);
    command.arg(if cfg!(windows) { "NUL" } else { "/dev/null" });
    command.args(["-X", method]);
    if let Some(body) = body {
        command.args(["-H", "Content-Type: application/json", "--data-binary", body]);
    }
    command.arg(url);
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let output = command.output().map_err(|e| format!("curl could not run: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&output.stderr);
    Err(why.lines().next().unwrap_or("curl failed").trim().to_string())
}

/// The connector's base address (no trailing slash, no `/mcp`): the copy
/// marker's `connector`, else the `thor` tool server of Claude Code's
/// `~/.claude.json` when it is an http server. `None` when this copy has no
/// connector - the weekly check then stays on the main machine.
pub fn connector_base(db: &Path) -> Option<String> {
    let named = thor_core::copy_mode::read_marker(db).connector.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
    named.map(|c| clean_base(&c)).or_else(|| connector_in(&crate::plan::default_claude_json()?))
}

/// `connector_base`, from one Claude Code config file.
pub fn connector_in(claude_json: &Path) -> Option<String> {
    let root: Value = serde_json::from_str(&std::fs::read_to_string(claude_json).ok()?).ok()?;
    let server = root.get("mcpServers")?.get("thor")?;
    let url = server.get("url")?.as_str()?;
    (url.starts_with("http://") || url.starts_with("https://")).then(|| clean_base(url))
}

fn clean_base(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    url.strip_suffix("/mcp").unwrap_or(url).trim_end_matches('/').to_string()
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
    /// Do not try the connector again before this unix time.
    #[serde(default)]
    pub unreachable_until: i64,
}

/// One weekly check asked of one session in one project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    pub session: String,
    pub asked_unix: i64,
    #[serde(default)]
    pub holds: u32,
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
         3. End the turn once. THOR checks the filing against this session's own transcript and sends proof of it to the main \
         machine; the report counts when the main machine has applied the queue, within the hour.\n\
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

fn sent_line(report: &str) -> String {
    format!(
        "[THOR] Your weekly-check report '{report}' was sent to the main machine with its proof, taken from this session's own \
         transcript. It counts once the main machine has applied the queue, within the hour; nothing more is needed from you."
    )
}

/// THE COPY'S WEEKLY CHECK AT A STOP (see the module doc). `folder_project` is
/// the project the Stop was counted in; `transcripts` the root of Claude Code's
/// transcripts; `may_ask` is false at a Stop that may not start a new ask (a
/// second pass of a held turn, or a transcript THOR cannot read). Nothing here
/// writes the store, and nothing is held while no connector is configured or
/// reachable.
pub fn copy_stop(
    db: &Path,
    session: &str,
    folder_project: Option<&str>,
    transcripts: Option<&Path>,
    now: i64,
    may_ask: bool,
    net: &dyn Net,
) -> CopyStop {
    let (Some(base), Some(root)) = (connector_base(db), transcripts) else { return CopyStop::Quiet };
    let _hold = crate::store_dir::hold_copy(db);
    let mut state = read_state(db);
    // The ask this session already holds: send its proof when the report is
    // filed, hold again while it is not (a bounded number of times).
    let open = state.asks.iter().find(|(_, a)| a.session == session && a.sent_unix.is_none() && !a.gave_up).map(|(p, _)| p.clone());
    if let Some(project) = open {
        let files = attest::session_transcripts(root, session);
        match find_filing(&files, session, Some(&project)) {
            Filing::Found(att) => {
                let body = serde_json::to_string(&att).unwrap_or_default();
                return match net.post_json(&format!("{base}/copy/attest"), &body) {
                    Ok(()) => {
                        if let Some(ask) = state.asks.get_mut(&project) {
                            ask.sent_unix = Some(now);
                            ask.report = Some(att.report.clone());
                        }
                        write_state(db, &state);
                        CopyStop::Say(sent_line(&att.report))
                    }
                    Err(why) => CopyStop::Say(format!(
                        "[THOR] Your weekly-check report '{}' is filed, but its proof could not be sent to the connector ({why}). \
                         THOR tries again at the next Stop; nothing is held.",
                        att.report
                    )),
                };
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
    if state.unreachable_until > now {
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
        if let Err(why) = net.get(&format!("{base}/copy/ping")) {
            state.unreachable_until = now + UNREACHABLE_BACKOFF_SECS;
            write_state(db, &state);
            return CopyStop::Say(format!(
                "[THOR] The weekly check of '{project}' is due, but the connector cannot be reached from this machine right now \
                 ({why}), so nothing is held. It is asked again when the connection is back."
            ));
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
    if op.tool == ATTEST_TOOL {
        let att: CopyAttestation = serde_json::from_value(op.args.clone()).map_err(|e| format!("copy attestation does not parse: {e}"))?;
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
    let reports = std::mem::take(&mut pending.reports);
    for op in reports {
        let id = op.args.get("id").and_then(Value::as_str).unwrap_or("?").to_string();
        let found = pending.attests.iter().position(|(_, a)| a.capture_id == op.capture_id);
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
    fn the_connector_is_found_in_the_marker_or_in_claude_json() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("claude.json");
        std::fs::write(&cfg, r#"{"mcpServers":{"thor":{"type":"http","url":"https://mcp.example.com/mcp"}}}"#).unwrap();
        assert_eq!(connector_in(&cfg).as_deref(), Some("https://mcp.example.com"));
        std::fs::write(&cfg, r#"{"mcpServers":{"thor":{"type":"stdio","command":"mcp"}}}"#).unwrap();
        assert_eq!(connector_in(&cfg), None, "a local tool server is no connector");
        assert_eq!(clean_base("http://10.0.0.5:8078/mcp/"), "http://10.0.0.5:8078");
    }
}
