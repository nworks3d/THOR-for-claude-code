//! Hook launch failures, read from Claude Code's own records.
//!
//! THE GAP THIS CLOSES. When Claude Code cannot start a THOR hook (Windows
//! Smart App Control or an antivirus blocking `serve.exe`, a permission, the
//! wrong shell), it writes an attachment of type `hook_non_blocking_error`
//! into the session's transcript and carries on without THOR. Nothing THOR
//! owns ever ran, so nothing THOR owns could say so: the memory was silent
//! with no error anywhere. A LATER hook that does start (the next Stop) and
//! `doctor` read those records here - the one place that knows - and say it.
//!
//! Only the fields that name the failure are kept: the hook event, the exit
//! code, the time and the tool call id. The stderr text is read for a plain
//! cause and then dropped, and no conversation content is ever read.

use model::store::HookFault;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// The attachment type Claude Code writes for a hook that could not run.
const MARK: &str = "hook_non_blocking_error";

/// One hook launch failure found in a transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scanned {
    pub session: String,
    pub fault: HookFault,
    pub cause: &'static str,
}

/// Whether `command` is THOR's own hook command, the shape the installer
/// writes: `"<path>/serve.exe" --db "<db>" hook`. Any other hook, and
/// `backup.exe` (whose failures `doctor` already reports), is not ours.
pub fn is_thor_hook_command(command: &str) -> bool {
    let command = command.trim();
    if !command.ends_with(" hook") {
        return false;
    }
    let program = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(""),
        None => command.split_whitespace().next().unwrap_or(""),
    };
    let name = program.rsplit(['/', '\\']).next().unwrap_or("").to_ascii_lowercase();
    name == "serve.exe" || name == "serve"
}

/// The cause of a launch failure in plain words, from the exit code and the
/// stderr Claude Code kept. Exit 126 is the shell's "found it, could not run
/// it", which on Windows is a security product blocking the program.
pub fn cause_in_words(exit_code: i64, stderr: &str) -> &'static str {
    let stderr = stderr.to_ascii_lowercase();
    if exit_code == 126 || stderr.contains("permission denied") {
        "the program was blocked or could not be opened (Windows security, an antivirus or a file permission)"
    } else if exit_code == 127 || stderr.contains("not found") || stderr.contains("not recognized") {
        "the program or the shell that starts it was not found"
    } else if stderr.contains("unexpected token") || stderr.contains("parsererror") {
        "the command ran under the wrong shell"
    } else {
        "the hook could not run"
    }
}

/// How close in time, in milliseconds, a hook of another program must have
/// failed with the same exit code for a THOR failure to count as host-wide.
/// Measured 2026-10-06: the PC was suspending, a THOR hook and a node hook
/// failed in the same second with exit 66 and no output.
pub const HOST_WIDE_WINDOW_MS: i64 = 2_000;

/// Every THOR hook launch failure recorded in the transcript file `path`.
/// A failure is marked `host_wide` when a hook of another program failed in
/// the same file within `HOST_WIDE_WINDOW_MS` with the same exit code: the
/// machine was going to sleep or shut down, which is not a THOR fault.
/// Fails quiet: an unreadable file, an unreadable line or a record of another
/// shape is skipped, never an error.
pub fn faults_in_file(path: &Path) -> Vec<Scanned> {
    let Ok(file) = std::fs::File::open(path) else { return Vec::new() };
    let mut reader = BufReader::new(file);
    let mut found = Vec::new();
    // (exit code, time in milliseconds) of every hook of another program.
    let mut others: Vec<(i64, i64)> = Vec::new();
    let mut raw = Vec::new();
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&raw);
        if !line.contains(MARK) {
            continue;
        }
        match failure_of(&line) {
            Some(Failure::Thor(scanned)) => found.push(scanned),
            Some(Failure::Other { exit_code, at }) => others.push((exit_code, at)),
            None => {}
        }
    }
    for scanned in &mut found {
        let Some(at) = millis_of_timestamp(&scanned.fault.at) else { continue };
        scanned.fault.host_wide =
            others.iter().any(|(exit, other)| *exit == scanned.fault.exit_code && (other - at).abs() <= HOST_WIDE_WINDOW_MS);
    }
    found
}

/// A transcript timestamp (`2026-10-06T04:18:12.231Z`) in milliseconds.
fn millis_of_timestamp(at: &str) -> Option<i64> {
    let seconds = crate::eval_filing::unix_of_timestamp(at)?;
    let fraction = at.split_once('.').map(|(_, rest)| rest.trim_end_matches('Z')).unwrap_or("");
    let digits: String = fraction.chars().take_while(char::is_ascii_digit).take(3).collect();
    let millis = format!("{digits:0<3}").parse::<i64>().unwrap_or(0);
    Some(seconds * 1000 + millis)
}

/// One `hook_non_blocking_error` record: THOR's own hook, or another program's.
enum Failure {
    Thor(Scanned),
    Other { exit_code: i64, at: i64 },
}

/// Whether `command` runs one of THOR's own programs (its hook, the backup,
/// the tool server...). A failure of one of those at the same moment is not
/// evidence of the machine going to sleep: it is THOR's own.
fn runs_a_thor_program(command: &str) -> bool {
    let command = command.trim();
    let program = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(""),
        None => command.split_whitespace().next().unwrap_or(""),
    };
    let name = program.rsplit(['/', '\\']).next().unwrap_or("").to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    ["serve", "backup", "mcp", "doctor", "install", "sync", "library"].contains(&name)
}

fn failure_of(line: &str) -> Option<Failure> {
    let record: Value = serde_json::from_str(line.trim()).ok()?;
    let attachment = record.get("attachment")?;
    if attachment.get("type")?.as_str()? != MARK {
        return None;
    }
    let command = attachment.get("command")?.as_str()?;
    if is_thor_hook_command(command) {
        return scanned_of(&record, attachment).map(Failure::Thor);
    }
    if runs_a_thor_program(command) {
        return None;
    }
    let exit_code = attachment.get("exitCode")?.as_i64()?;
    let at = millis_of_timestamp(record.get("timestamp")?.as_str()?)?;
    Some(Failure::Other { exit_code, at })
}

fn scanned_of(record: &Value, attachment: &Value) -> Option<Scanned> {
    let exit_code = attachment.get("exitCode")?.as_i64()?;
    let stderr = attachment.get("stderr").and_then(Value::as_str).unwrap_or_default();
    Some(Scanned {
        session: record.get("sessionId")?.as_str()?.to_string(),
        fault: HookFault {
            tool_use_id: attachment.get("toolUseID")?.as_str()?.to_string(),
            hook_event: attachment.get("hookEvent")?.as_str()?.to_string(),
            exit_code,
            at: record.get("timestamp")?.as_str()?.to_string(),
            host_wide: false,
        },
        cause: cause_in_words(exit_code, stderr),
    })
}

/// Every THOR hook launch failure in the transcripts under `root` that were
/// written to since `since_unix` (by file time), oldest first. Only a file
/// changed in that span is opened, and a line is parsed only when it carries
/// the record's type, so a week of transcripts stays cheap to read.
pub fn faults_since(root: &Path, since_unix: i64) -> Vec<Scanned> {
    let mut files: Vec<PathBuf> = Vec::new();
    crate::attest::walk_jsonl(root, &mut files, 0);
    let mut found: Vec<Scanned> = files
        .iter()
        .filter(|file| {
            std::fs::metadata(file)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .is_some_and(|t| t.as_secs() as i64 >= since_unix)
        })
        .flat_map(|file| faults_in_file(file))
        .collect();
    found.sort_by(|a, b| a.fault.at.cmp(&b.fault.at));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "3f2ac470-767e-4df0-9ef3-2b89ed671524";

    /// The shape Claude Code wrote on 2026-10-06 when a security product
    /// blocked serve.exe (names and paths made up).
    fn failure_line(command: &str, exit: i64, stderr: &str, id: &str) -> String {
        failure_line_at(command, exit, stderr, id, "2026-10-06T22:29:00.231Z")
    }

    fn failure_line_at(command: &str, exit: i64, stderr: &str, id: &str, at: &str) -> String {
        serde_json::json!({
            "attachment": {
                "type": "hook_non_blocking_error",
                "hookName": "SessionStart:startup",
                "toolUseID": id,
                "hookEvent": "SessionStart",
                "stderr": stderr,
                "stdout": "",
                "exitCode": exit,
                "command": command,
                "durationMs": 1316
            },
            "type": "attachment",
            "timestamp": at,
            "sessionId": SESSION,
        })
        .to_string()
    }

    const OURS: &str = "\"C:\\\\Users\\\\someone\\\\thor2\\\\bin\\\\serve.exe\" --db \"C:\\\\Users\\\\someone\\\\thor2\\\\thor.db\" hook";

    #[test]
    fn only_thors_own_hook_command_counts() {
        assert!(is_thor_hook_command("\"C:\\Users\\someone\\thor2\\bin\\serve.exe\" --db \"C:\\x\\thor.db\" hook"));
        assert!(is_thor_hook_command("\"/home/someone/thor2/bin/serve\" --db \"/home/someone/thor.db\" hook"));
        assert!(!is_thor_hook_command("\"C:\\thor2\\bin\\backup.exe\" --db \"C:\\x\\thor.db\" --repo \"C:\\b\""));
        assert!(!is_thor_hook_command("\"C:\\tools\\other.exe\" hook"));
        assert!(!is_thor_hook_command("\"C:\\thor2\\bin\\serve.exe\" --db \"C:\\x\\thor.db\" status"));
    }

    #[test]
    fn the_cause_is_said_in_plain_words() {
        assert!(cause_in_words(126, "serve.exe: Permission denied").contains("blocked"));
        assert!(cause_in_words(1, "x: Permission denied").contains("blocked"));
        assert!(cause_in_words(127, "").contains("not found"));
        assert!(cause_in_words(1, "Unexpected token 'db' in expression").contains("wrong shell"));
        assert_eq!(cause_in_words(66, ""), "the hook could not run");
    }

    #[test]
    fn a_transcript_yields_only_thor_launch_failures() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(format!("{SESSION}.jsonl"));
        let lines = [
            "{\"type\":\"user\",\"message\":\"the words hook_non_blocking_error in a chat are no record\"}".to_string(),
            failure_line(OURS, 126, "serve.exe: Permission denied", "toolu-1"),
            failure_line("\"C:\\\\x\\\\other.exe\" hook", 1, "no", "toolu-2"),
            "not json but mentions hook_non_blocking_error".to_string(),
            failure_line(OURS, 66, "", "toolu-3"),
        ];
        std::fs::write(&file, lines.join("\n") + "\n").unwrap();
        let found = faults_in_file(&file);
        let ids: Vec<&str> = found.iter().map(|s| s.fault.tool_use_id.as_str()).collect();
        assert_eq!(ids, ["toolu-1", "toolu-3"]);
        assert_eq!((found[0].fault.hook_event.as_str(), found[0].fault.exit_code), ("SessionStart", 126));
        assert_eq!(found[0].session, SESSION);
        assert!(found[0].cause.contains("blocked"));
        assert!(faults_in_file(&dir.path().join("missing.jsonl")).is_empty());
    }

    #[test]
    fn the_stop_records_each_launch_failure_of_its_session_once() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("C--Users-someone-dev-demo");
        std::fs::create_dir_all(&project).unwrap();
        let lines = [failure_line(OURS, 126, "", "toolu-1"), failure_line(OURS, 126, "", "toolu-2")];
        std::fs::write(project.join(format!("{SESSION}.jsonl")), lines.join("\n") + "\n").unwrap();
        let mut store = thor_core::event_store::EventStore::in_memory().unwrap();
        assert_eq!(crate::attest::record_hook_faults_in(&mut store, SESSION, Some(dir.path())), 2);
        assert_eq!(crate::attest::record_hook_faults_in(&mut store, SESSION, Some(dir.path())), 0, "a second Stop adds nothing");
        let events = store.get_events_by_entity(&model::store::hook_fault_entity(SESSION)).unwrap();
        assert_eq!(model::store::hook_faults_in(&events).len(), 2);
        assert_eq!(crate::attest::record_hook_faults_in(&mut store, SESSION, None), 0, "no transcripts folder: nothing");
        assert_eq!(crate::attest::record_hook_faults_in(&mut store, "not-a-session", Some(dir.path())), 0);
    }

    #[test]
    fn faults_since_reads_only_recently_written_files_under_every_project_folder() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("C--Users-someone-dev-demo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join(format!("{SESSION}.jsonl")), failure_line(OURS, 126, "", "toolu-9") + "\n").unwrap();
        assert_eq!(faults_since(dir.path(), 0).len(), 1);
        let future = crate::time::now_unix() + 3600;
        assert!(faults_since(dir.path(), future).is_empty(), "a file older than the span is not opened");
        assert!(faults_since(&dir.path().join("nowhere"), 0).is_empty());
    }

    const NODE_HOOK: &str = "node C:/Users/someone/.claude/hooks/precommit-gate.js";

    /// FIELD TEST 13, D8: three sleep failures recorded before THOR classified
    /// them were listed as THOR's by three projects. A reader asks the
    /// transcript again, with the rule used at record time.
    #[test]
    fn a_failure_recorded_before_the_classification_is_found_host_wide_by_a_later_read() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("C--Users-someone-dev-demo");
        std::fs::create_dir_all(&project).unwrap();
        let lines = [
            failure_line_at(OURS, 66, "", "toolu-sleep", "2026-10-06T04:18:12.100Z"),
            failure_line_at(NODE_HOOK, 66, "", "toolu-node", "2026-10-06T04:18:12.900Z"),
            failure_line_at(OURS, 126, "", "toolu-thor", "2026-10-06T09:00:00.000Z"),
        ];
        std::fs::write(project.join(format!("{SESSION}.jsonl")), lines.join("\n") + "\n").unwrap();
        let mut store = thor_core::event_store::EventStore::in_memory().unwrap();
        // The legacy record: no host_wide mark, as the build before the classification wrote it.
        let entity = model::store::hook_fault_entity(SESSION);
        for (id, exit, at) in [("toolu-sleep", 66, "2026-10-06T04:18:12.100Z"), ("toolu-thor", 126, "2026-10-06T09:00:00.000Z")] {
            let body = serde_json::json!({"hook_fault": {"tool_use_id": id, "hook_event": "PreToolUse", "exit_code": exit, "at": at}});
            let kind = thor_core::event_store::EventKind::GateStoodAside;
            store.append_event(SESSION, SESSION, model::store::ATTESTER_ACTOR, kind, &entity, None, &body.to_string()).unwrap();
        }
        let until = store.get_next_seq().unwrap() - 1;
        let found = crate::attest::host_wide_recorded_faults(&store, 0, until, Some(dir.path()));
        assert_eq!(found.into_iter().collect::<Vec<_>>(), ["toolu-sleep".to_string()], "the lone failure stays THOR's");
        assert!(crate::attest::host_wide_recorded_faults(&store, 0, until, None).is_empty(), "no transcripts folder: as recorded");
        assert!(crate::attest::host_wide_recorded_faults(&store, until, until, Some(dir.path())).is_empty(), "outside the window");
        let gone = tempfile::tempdir().unwrap();
        assert!(crate::attest::host_wide_recorded_faults(&store, 0, until, Some(gone.path())).is_empty(), "transcript gone: as recorded");
    }

    fn host_wide_of(lines: &[String]) -> Vec<(String, bool)> {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(format!("{SESSION}.jsonl"));
        std::fs::write(&file, lines.join("\n") + "\n").unwrap();
        faults_in_file(&file).into_iter().map(|s| (s.fault.tool_use_id, s.fault.host_wide)).collect()
    }

    /// 2026-10-06 04:18Z: the PC was suspending, three THOR hooks and a node
    /// hook failed in the same second with exit 66 and no output. That is the
    /// machine, not THOR.
    #[test]
    fn a_thor_failure_beside_another_programs_failure_in_the_same_second_is_host_wide() {
        let lines = [
            failure_line_at(OURS, 66, "", "toolu-1", "2026-10-06T04:18:12.100Z"),
            failure_line_at(NODE_HOOK, 66, "", "toolu-node", "2026-10-06T04:18:12.900Z"),
            failure_line_at(OURS, 66, "", "toolu-2", "2026-10-06T04:18:13.400Z"),
        ];
        assert_eq!(host_wide_of(&lines), [("toolu-1".to_string(), true), ("toolu-2".to_string(), true)]);
    }

    #[test]
    fn a_lone_thor_failure_is_still_thors() {
        let lines = [failure_line_at(OURS, 66, "", "toolu-1", "2026-10-06T04:18:12.100Z")];
        assert_eq!(host_wide_of(&lines), [("toolu-1".to_string(), false)]);
    }

    /// Only the same exit code, within the window, from a program that is not
    /// THOR's own counts.
    #[test]
    fn another_failure_that_is_too_far_away_or_of_another_code_or_thors_own_does_not_count() {
        let far = [
            failure_line_at(OURS, 66, "", "toolu-1", "2026-10-06T04:18:12.000Z"),
            failure_line_at(NODE_HOOK, 66, "", "toolu-node", "2026-10-06T04:18:15.000Z"),
        ];
        assert_eq!(host_wide_of(&far), [("toolu-1".to_string(), false)], "3 s apart is not the same moment");
        let other_code = [
            failure_line_at(OURS, 66, "", "toolu-1", "2026-10-06T04:18:12.000Z"),
            failure_line_at(NODE_HOOK, 1, "", "toolu-node", "2026-10-06T04:18:12.000Z"),
        ];
        assert_eq!(host_wide_of(&other_code), [("toolu-1".to_string(), false)], "another exit code is another cause");
        let backup = [
            failure_line_at(OURS, 66, "", "toolu-1", "2026-10-06T04:18:12.000Z"),
            failure_line_at("\"C:\\x\\thor2\\bin\\backup.exe\" --db \"d\"", 66, "", "toolu-b", "2026-10-06T04:18:12.000Z"),
        ];
        assert_eq!(host_wide_of(&backup), [("toolu-1".to_string(), false)], "THOR's own program is not evidence of the machine");
    }

    #[test]
    fn a_timestamp_is_read_to_the_millisecond() {
        assert_eq!(millis_of_timestamp("2026-10-06T04:18:12.100Z").unwrap() % 1000, 100);
        assert_eq!(millis_of_timestamp("2026-10-06T04:18:12.5Z").unwrap() % 1000, 500);
        assert_eq!(millis_of_timestamp("2026-10-06T04:18:12Z").unwrap() % 1000, 0);
    }
}
