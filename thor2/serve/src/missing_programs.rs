//! A THOR program that something calls but that is not on disk.
//!
//! THE DEFECT THIS CLOSES. On 2026-10-11 Windows Defender quarantined THOR's
//! sync program as a false positive. The hourly ship task then failed for two
//! days and nothing said why, because nothing checked that the program a hook,
//! the tool server entry or a scheduled task calls is actually there. This
//! reads those places and names every THOR program that is missing, with the
//! most likely cause (an antivirus) and the steps to get it back.
//!
//! Pure where it can be: the readers take text, the check takes an `exists`
//! answer, and only `defender_names` runs a program (read-only, no window).
//! Used by `doctor` (all four places) and by the session start (hooks, tool
//! server and the programs the memory's own sidecars show it has used - a
//! scheduled-task query is too slow for a session start).

use std::path::{Path, PathBuf};

/// The file stems of THOR's own programs.
pub const THOR_PROGRAMS: [&str; 7] = ["serve", "mcp", "doctor", "sync", "backup", "install", "initstore"];

/// The first word of a command line, honouring double quotes.
fn first_word(command: &str) -> Option<String> {
    let command = command.trim();
    if let Some(rest) = command.strip_prefix('"') {
        let end = rest.find('"')?;
        return Some(rest[..end].to_string());
    }
    command.split_whitespace().next().map(str::to_string)
}

/// Every word of a command line, honouring double quotes.
fn words(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in command.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Whether `path` names one of THOR's programs by its file stem.
fn is_thor_program(path: &Path) -> bool {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
    THOR_PROGRAMS.contains(&stem.as_str()) && (extension == "exe" || extension.is_empty())
}

/// `%NAME%` words expanded from the environment; `None` when one is unknown
/// (the path cannot be judged then, so it is skipped rather than reported).
fn expand_env(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = word;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after.find('%')?;
        out.push_str(&std::env::var(&after[..end]).ok()?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// The THOR programs the hooks of Claude Code's settings file call, and the
/// program of the `thor` tool server in its tool-server file. Other programs'
/// hooks are never read as ours.
pub fn programs_called(settings_text: Option<&str>, mcp_text: Option<&str>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(root) = settings_text.and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok()) {
        if let Some(events) = root.get("hooks").and_then(|h| h.as_object()) {
            let commands = events
                .values()
                .filter_map(|groups| groups.as_array())
                .flatten()
                .filter_map(|group| group.get("hooks").and_then(|h| h.as_array()))
                .flatten()
                .filter_map(|hook| hook.get("command").and_then(|c| c.as_str()));
            for command in commands {
                let Some(program) = first_word(command).map(PathBuf::from) else { continue };
                if is_thor_program(&program) && command.contains("--db") {
                    out.push(program);
                }
            }
        }
    }
    if let Some(text) = mcp_text.filter(|t| t.contains("thor")) {
        let server = serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|root| root.get("mcpServers")?.get("thor")?.get("command")?.as_str().map(PathBuf::from));
        if let Some(program) = server.filter(|p| is_thor_program(p)) {
            out.push(program);
        }
    }
    out
}

/// What a script file (the body of a `.ps1`) calls: every quoted `...exe`
/// path naming a THOR program, with no variable in it.
fn programs_in_script(text: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for quote in ['"', '\''] {
        for (i, piece) in text.split(quote).enumerate() {
            if i % 2 == 1 && piece.to_lowercase().ends_with(".exe") && !piece.contains('$') {
                let path = PathBuf::from(piece);
                if is_thor_program(&path) {
                    out.push(path);
                }
            }
        }
    }
    out
}

/// The THOR programs and scripts the scheduled tasks call, from the XML
/// `schtasks /query /xml` prints (language independent). A THOR script is
/// followed one step: the THOR programs it names are read as called too.
pub fn task_programs(xml: &str, read_script: &dyn Fn(&Path) -> Option<String>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for tag in ["Command", "Arguments"] {
        let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
        for piece in xml.split(&open).skip(1) {
            let Some(end) = piece.find(&close) else { continue };
            for word in words(&piece[..end]) {
                let Some(word) = expand_env(&word) else { continue };
                let path = PathBuf::from(&word);
                let lower = word.to_lowercase();
                if is_thor_program(&path) {
                    out.push(path);
                } else if (lower.ends_with(".ps1") || lower.ends_with(".vbs") || lower.ends_with(".cmd"))
                    && lower.replace('\\', "/").contains("thor")
                {
                    if let Some(text) = read_script(&path) {
                        out.extend(programs_in_script(&text));
                    }
                    out.push(path);
                }
            }
        }
    }
    out
}

/// The programs the memory's own sidecars show it has used, each expected
/// beside the other THOR programs in `bin_dir`: the ship sidecar means the
/// sync program, the backup sidecar the backup program. A folder holding none
/// of THOR's programs is not THOR's folder, and says nothing.
pub fn used_by_sidecars(db: &Path, bin_dir: &Path, exists: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let beside = |name: &str| bin_dir.join(if cfg!(windows) { format!("{name}.exe") } else { name.to_string() });
    if !THOR_PROGRAMS.iter().any(|p| exists(&beside(p))) {
        return Vec::new();
    }
    let dir = db.parent().unwrap_or_else(|| Path::new("."));
    let mut out = Vec::new();
    if dir.join("sync-ship-state.json").is_file() {
        out.push(beside("sync"));
    }
    if dir.join("backup-state.json").is_file() {
        out.push(beside("backup"));
    }
    out
}

/// The called programs that are not there, each named once.
pub fn missing(called: Vec<PathBuf>, exists: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for path in called {
        let same = |other: &PathBuf| other.display().to_string().to_lowercase().replace('\\', "/")
            == path.display().to_string().to_lowercase().replace('\\', "/");
        if !exists(&path) && !out.iter().any(same) {
            out.push(path);
        }
    }
    out
}

/// Every THOR program called by the settings file's hooks, the tool-server
/// file, the sidecars of `db` (beside `bin_dir`) and, when given, the
/// scheduled tasks' XML - and not on disk.
pub fn find(
    db: &Path,
    settings: Option<&Path>,
    mcp_json: Option<&Path>,
    bin_dir: Option<&Path>,
    tasks_xml: Option<&str>,
) -> Vec<PathBuf> {
    let read = |p: Option<&Path>| p.and_then(|p| std::fs::read_to_string(p).ok());
    let exists = |p: &Path| p.is_file();
    let mut called = programs_called(read(settings).as_deref(), read(mcp_json).as_deref());
    if let Some(bin_dir) = bin_dir {
        called.extend(used_by_sidecars(db, bin_dir, &exists));
    }
    if let Some(xml) = tasks_xml {
        called.extend(task_programs(xml, &|p| std::fs::read_to_string(p).ok()));
    }
    missing(called, &exists)
}

/// The session start's one line: hooks, tool server and sidecars (no
/// scheduled-task query, which is too slow here). `None` when all is there,
/// and it does not read Defender's history (a session start stays fast).
pub fn session_line(db: &Path) -> Option<String> {
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).ok().map(PathBuf::from)?;
    let bin_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let gone = find(
        db,
        Some(&home.join(".claude").join("settings.json")),
        Some(&home.join(".claude.json")),
        bin_dir.as_deref(),
        None,
    );
    words_for(&gone, &[], cfg!(windows))
}

/// The file names Windows Defender's own detection history names, from the
/// lines of resources it printed: only THOR's (a program name or a THOR
/// folder). One name once.
pub fn parse_defender(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches("file:_").to_string();
        let lower = line.to_lowercase().replace('\\', "/");
        let name = lower.rsplit('/').next().unwrap_or("").to_string();
        let ours = THOR_PROGRAMS.iter().any(|p| name == format!("{p}.exe")) || lower.contains("/thor");
        if ours && !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// Windows Defender's detection history, read-only and without a window, only
/// when Windows lets this user read it (it may not without administrator
/// rights; then nothing is said). The THOR file names it shows.
#[cfg(windows)]
pub fn defender_names() -> Vec<String> {
    use std::os::windows::process::CommandExt;
    let script = "try { Get-MpThreatDetection -ErrorAction Stop | ForEach-Object { $_.Resources } } catch {}";
    match std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(0x0800_0000)
        .output()
    {
        Ok(out) => parse_defender(&String::from_utf8_lossy(&out.stdout)),
        Err(_) => Vec::new(),
    }
}

#[cfg(not(windows))]
pub fn defender_names() -> Vec<String> {
    Vec::new()
}

/// The scheduled tasks' XML (`schtasks /query /xml`), readable without
/// administrator rights. Empty where there is no such program. The file named
/// by `THOR_SCHTASKS_XML` is read instead when that is set (a test, or a
/// machine where the query is not wanted); a missing file reads as no tasks.
pub fn scheduled_tasks_xml() -> String {
    if let Ok(file) = std::env::var("THOR_SCHTASKS_XML") {
        return std::fs::read_to_string(file).unwrap_or_default();
    }
    query_scheduled_tasks()
}

#[cfg(windows)]
fn query_scheduled_tasks() -> String {
    use std::os::windows::process::CommandExt;
    match std::process::Command::new("schtasks").args(["/query", "/xml"]).creation_flags(0x0800_0000).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).to_string(),
        Err(_) => String::new(),
    }
}

#[cfg(not(windows))]
fn query_scheduled_tasks() -> String {
    String::new()
}

/// The finding in plain words, or `None` when nothing is missing. `windows`
/// adds the Windows Security steps and the exclusion the assistant may OFFER
/// (never run unasked). `detected` is what Defender's history named, if it
/// could be read.
pub fn words_for(missing: &[PathBuf], detected: &[String], windows: bool) -> Option<String> {
    let first = missing.first()?;
    let names: Vec<String> = missing.iter().map(|p| p.display().to_string()).collect();
    let folder = first.parent().map(|p| p.display().to_string()).unwrap_or_default();
    let mut out = format!(
        "PROBLEM: THOR program missing: {}. Something calls it and it is not there, so that part of THOR cannot run (a hook or a scheduled task then fails with no other message).",
        names.join(", ")
    );
    if windows {
        out.push_str(
            " Most likely cause: an antivirus (usually Windows Defender) quarantined it as a false positive. To check and get it back: open Windows Security > Virus & threat protection > Protection history, look for an entry naming the file, open it and choose Actions > Restore. If there is no such entry, run THOR's install again (the same command that installed it) and it puts the program back.",
        );
        if !detected.is_empty() {
            out.push_str(&format!(" Windows Defender's own history names: {}.", detected.join(", ")));
        }
        out.push_str(&format!(
            " FOR THE ASSISTANT: you may OFFER, never do it unasked, to add a Defender exclusion for the THOR program folder {folder}. Say plainly: it makes Defender stop scanning every file in that folder, so a bad file placed there would not be caught; Windows shows one administrator prompt; saying no is fine. Only after a clear yes in this conversation run: Start-Process powershell -Verb RunAs -WindowStyle Hidden -Wait -ArgumentList '-NoProfile -Command Add-MpPreference -ExclusionPath \"{folder}\"' and then check it with (Get-MpPreference).ExclusionPath. THOR itself never changes a Windows security setting."
        ));
    } else {
        out.push_str(
            " Most likely cause: an antivirus or a cleanup removed it. Run THOR's install again (the same command that installed it) and it puts the program back.",
        );
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS: &str = r#"{"hooks":{
        "SessionStart":[{"hooks":[{"type":"command","command":"\"C:\\Users\\a\\thor2\\serve.exe\" --db \"C:\\x\\thor.db\" hook"}]}],
        "Stop":[{"hooks":[{"type":"command","command":"C:/other/tool.exe --db y hook"}]}]}}"#;
    const MCP: &str = r#"{"mcpServers":{"thor":{"command":"C:\\Users\\a\\thor2\\mcp.exe","args":["--db","C:\\x\\thor.db"]},"other":{"command":"C:\\z\\serve.exe"}}}"#;

    #[test]
    fn the_hook_and_tool_server_programs_are_found_and_other_hooks_are_not() {
        let found = programs_called(Some(SETTINGS), Some(MCP));
        assert_eq!(found, vec![PathBuf::from("C:\\Users\\a\\thor2\\serve.exe"), PathBuf::from("C:\\Users\\a\\thor2\\mcp.exe")]);
    }

    #[test]
    fn a_missing_program_is_named_once_and_a_present_one_is_not() {
        let called = programs_called(Some(SETTINGS), Some(MCP));
        let only_mcp = |p: &Path| p.ends_with("mcp.exe");
        let gone = missing([called.clone(), called].concat(), &only_mcp);
        assert_eq!(gone, vec![PathBuf::from("C:\\Users\\a\\thor2\\serve.exe")]);
        assert!(missing(vec![PathBuf::from("mcp.exe")], &only_mcp).is_empty());
    }

    #[test]
    fn a_scheduled_task_and_the_script_it_runs_are_followed_to_the_program() {
        let xml = r#"<Task><Actions><Exec><Command>powershell.exe</Command>
            <Arguments>-NoProfile -File "C:\Users\a\thor2\ship-hourly.ps1" -Db x</Arguments></Exec></Actions></Task>
            <Task><Actions><Exec><Command>C:\Windows\notepad.exe</Command></Exec></Actions></Task>"#;
        let script = |p: &Path| p.ends_with("ship-hourly.ps1").then(|| "$bin = \"C:\\Users\\a\\thor2\\sync.exe\"\n& $bin ship".to_string());
        let found = task_programs(xml, &script);
        assert!(found.contains(&PathBuf::from("C:\\Users\\a\\thor2\\sync.exe")), "{found:?}");
        assert!(found.contains(&PathBuf::from("C:\\Users\\a\\thor2\\ship-hourly.ps1")), "{found:?}");
        assert!(!found.iter().any(|p| p.ends_with("notepad.exe")));
    }

    #[test]
    fn the_sidecars_say_which_programs_the_memory_has_used() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let db = dir.path().join("thor.db");
        let serve = bin.join(if cfg!(windows) { "serve.exe" } else { "serve" });
        std::fs::write(&serve, b"x").unwrap();
        assert!(used_by_sidecars(&db, &bin, &|p| p.is_file()).is_empty(), "no ship sidecar: sync is not expected");
        std::fs::write(dir.path().join("sync-ship-state.json"), b"{}").unwrap();
        let expected = used_by_sidecars(&db, &bin, &|p| p.is_file());
        assert_eq!(expected.len(), 1);
        assert!(expected[0].file_stem().unwrap() == "sync");
        // A folder with none of THOR's programs is not THOR's folder.
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        assert!(used_by_sidecars(&db, &elsewhere, &|p| p.is_file()).is_empty());
    }

    #[test]
    fn defender_history_lines_keep_only_thor_files() {
        let text = "file:_C:\\Users\\a\\thor2\\sync.exe\nfile:_C:\\Users\\a\\Downloads\\game.exe\nfile:_C:\\Users\\a\\thor2\\sync.exe\n";
        assert_eq!(parse_defender(text), vec!["sync.exe".to_string()]);
    }

    #[test]
    fn the_windows_line_names_the_cause_the_steps_and_the_consented_exclusion() {
        let line = words_for(&[PathBuf::from("C:\\Users\\a\\thor2\\sync.exe")], &["sync.exe".to_string()], true).unwrap();
        for must in [
            "THOR program missing: C:\\Users\\a\\thor2\\sync.exe",
            "quarantined",
            "Windows Security > Virus & threat protection > Protection history",
            "install again",
            "history names: sync.exe",
            "OFFER, never do it unasked",
            "stop scanning every file in that folder",
            "one administrator prompt",
            "saying no is fine",
            "Start-Process powershell -Verb RunAs -WindowStyle Hidden",
            "Add-MpPreference -ExclusionPath \"C:\\Users\\a\\thor2\"",
            "(Get-MpPreference).ExclusionPath",
        ] {
            assert!(line.contains(must), "missing '{must}' in: {line}");
        }
        assert!(line.is_ascii(), "plain ASCII only: {line}");
    }

    #[test]
    fn off_windows_there_is_no_defender_advice_and_nothing_missing_is_silent() {
        let line = words_for(&[PathBuf::from("/opt/thor2/sync")], &[], false).unwrap();
        assert!(!line.contains("Defender") && !line.contains("Add-MpPreference"), "{line}");
        assert!(line.contains("install again"));
        assert_eq!(words_for(&[], &[], true), None);
    }
}
