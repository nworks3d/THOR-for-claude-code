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

/// Every THOR hook launch failure recorded in the transcript file `path`.
/// Fails quiet: an unreadable file, an unreadable line or a record of another
/// shape is skipped, never an error.
pub fn faults_in_file(path: &Path) -> Vec<Scanned> {
    let Ok(file) = std::fs::File::open(path) else { return Vec::new() };
    let mut reader = BufReader::new(file);
    let mut found = Vec::new();
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
        if let Some(scanned) = scanned_of(&line) {
            found.push(scanned);
        }
    }
    found
}

fn scanned_of(line: &str) -> Option<Scanned> {
    let record: Value = serde_json::from_str(line.trim()).ok()?;
    let attachment = record.get("attachment")?;
    if attachment.get("type")?.as_str()? != MARK || !is_thor_hook_command(attachment.get("command")?.as_str()?) {
        return None;
    }
    let exit_code = attachment.get("exitCode")?.as_i64()?;
    let stderr = attachment.get("stderr").and_then(Value::as_str).unwrap_or_default();
    Some(Scanned {
        session: record.get("sessionId")?.as_str()?.to_string(),
        fault: HookFault {
            tool_use_id: attachment.get("toolUseID")?.as_str()?.to_string(),
            hook_event: attachment.get("hookEvent")?.as_str()?.to_string(),
            exit_code,
            at: record.get("timestamp")?.as_str()?.to_string(),
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
            "timestamp": "2026-10-06T22:29:00.231Z",
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
}
