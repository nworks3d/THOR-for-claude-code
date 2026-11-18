//! `install --uninstall`: take THOR out again, in two phases, run by the
//! assistant itself so the person types nothing.
//!
//! WHAT IS NEVER TOUCHED. THOR removes only what THOR put there. It never
//! removes, and never offers to remove, git, Python, Microsoft's Visual C++
//! runtime or any other program: Claude Code itself uses Git for Windows, and
//! an uninstall script that also took git out (it happened) leaves the person
//! without a working Claude Code. Foreign hooks, foreign tool servers and a git
//! hooks folder somebody else set are left exactly as they are.
//!
//! PHASE 1 (`phase_one`, Claude Code may be running): THOR's own hook entries
//! (every event, shell form and exec form, the backup hook too), the `thor`
//! tool server, the /thor-eval command file THOR wrote, and git's global
//! `core.hooksPath` but only when it points at THOR's own githooks folder.
//!
//! PHASE 2 (`phase_two`, after Claude Code was closed and opened again so it
//! no longer runs a THOR program): refuses while a program from THOR's folder
//! is running, removes the `.bak` copies THOR's install made, and keeps the
//! memory unless the person said to delete it. A program cannot delete its own
//! folder while it runs, so it ends by naming that folder for the assistant.

use crate::githooks::{decide_hooks_path, HooksPathOutcome};
use crate::install::{backup_once, command_words, flatten_word};
use serde_json::Value;
use serve::missing_programs::hook_command_line;
use std::path::{Path, PathBuf};

/// Where THOR's pieces live for one uninstall.
#[derive(Debug, Clone)]
pub struct Targets {
    /// Claude Code's settings file (holds the hooks).
    pub settings: PathBuf,
    /// Claude Code's tool-server file (`~/.claude.json`).
    pub claude_json: PathBuf,
    /// The /thor-eval command file, when the home folder is known.
    pub eval_command: Option<PathBuf>,
    /// The memory file; its folder also holds THOR's githooks folder.
    pub db: PathBuf,
}

/// Git's global `core.hooksPath`, behind a seam so a test never touches the
/// machine's real configuration.
pub trait GitHooksPath {
    fn current(&self) -> Option<String>;
    fn unset(&self) -> bool;
}

/// The real thing: the `git` program, started without a window.
pub struct RealGit;

fn git_command() -> std::process::Command {
    let mut command = std::process::Command::new("git");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
}

impl GitHooksPath for RealGit {
    fn current(&self) -> Option<String> {
        git_command()
            .args(["config", "--global", "--get", "core.hooksPath"])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    }
    fn unset(&self) -> bool {
        git_command().args(["config", "--global", "--unset", "core.hooksPath"]).status().is_ok_and(|s| s.success())
    }
}

/// What phase 1 did, in the order the plain lines are printed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhaseOneReport {
    pub hooks_removed: usize,
    pub tool_server_removed: bool,
    pub eval_command_removed: bool,
    /// `Some(path)` when git's hooks folder was THOR's and was cleared.
    pub git_hooks_path_cleared: Option<String>,
    /// `Some(path)` when git's hooks folder is somebody else's and was left.
    pub git_hooks_path_left: Option<String>,
}

impl PhaseOneReport {
    pub fn nothing_to_remove(&self) -> bool {
        self.hooks_removed == 0
            && !self.tool_server_removed
            && !self.eval_command_removed
            && self.git_hooks_path_cleared.is_none()
    }

    /// The plain lines for the person, ending with the one next step.
    pub fn plain_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.hooks_removed > 0 {
            lines.push(format!("+ removed THOR's {} connection(s) from Claude Code's settings (nothing else there was touched)", self.hooks_removed));
        }
        if self.tool_server_removed {
            lines.push("+ removed the note-saving connection of THOR from Claude Code".to_string());
        }
        if self.eval_command_removed {
            lines.push("+ removed the /thor-eval command".to_string());
        }
        if let Some(path) = &self.git_hooks_path_cleared {
            lines.push(format!("+ told git to stop using THOR's helper folder ({path}); git itself stays installed"));
        }
        if let Some(path) = &self.git_hooks_path_left {
            lines.push(format!("= git's helper folder ({path}) is not THOR's, so it was left alone"));
        }
        if self.nothing_to_remove() {
            lines.push("= nothing of THOR was left in Claude Code's settings".to_string());
        }
        lines.push("Next step: close Claude Code completely and open it again, then say: finish removing THOR.".to_string());
        lines
    }
}

fn read_json_object(path: &Path) -> anyhow::Result<Option<(String, Value)>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)?;
    let root: Value = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("{} is not valid JSON ({e}) - refusing to touch it", path.display()))?;
    anyhow::ensure!(root.is_object(), "{} does not hold a JSON object at the top level - refusing to touch it", path.display());
    Ok(Some((raw, root)))
}

/// Whether one hook entry is THOR's own, for the memory at `db`: the program
/// is `serve` (its last word `hook`) or `backup`, and its `--db` is this
/// memory, in any spelling of the path, in shell form or in exec form.
fn is_thors_hook(hook: &Value, db: &str) -> bool {
    if hook.get("type").and_then(Value::as_str) != Some("command") {
        return false;
    }
    let Some(line) = hook_command_line(hook) else { return false };
    let words = command_words(&line);
    let Some(program) = words.first() else { return false };
    let stem = Path::new(program).file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_lowercase();
    let wanted = flatten_word(db);
    let names_db = words.windows(2).any(|pair| pair[0] == "--db" && pair[1] == wanted);
    match stem.as_str() {
        "serve" => names_db && words.last().is_some_and(|w| w == "hook"),
        "backup" => names_db,
        _ => false,
    }
}

/// How many of THOR's hook entries for `db` the settings file holds.
pub fn thor_hooks_in(settings: &Path, db: &Path) -> anyhow::Result<usize> {
    let Some((_, root)) = read_json_object(settings)? else { return Ok(0) };
    let db = db.display().to_string();
    let mut count = 0;
    for groups in root.get("hooks").and_then(Value::as_object).into_iter().flat_map(|h| h.values()) {
        for group in groups.as_array().into_iter().flatten() {
            count += group.get("hooks").and_then(Value::as_array).into_iter().flatten().filter(|h| is_thors_hook(h, &db)).count();
        }
    }
    Ok(count)
}

/// Remove THOR's hook entries (every event) and nothing else. A group that
/// held only THOR's entries goes; a group with anything else keeps it. The
/// file is saved as `<name>.bak` first (only when no backup exists yet), must
/// parse before and after, and is not rewritten at all when there is nothing
/// to remove. Returns how many entries went.
pub fn remove_thor_hooks(settings: &Path, db: &Path) -> anyhow::Result<usize> {
    let Some((raw, mut root)) = read_json_object(settings)? else { return Ok(0) };
    let db = db.display().to_string();
    let mut removed = 0;
    if let Some(events) = root.get_mut("hooks").and_then(Value::as_object_mut) {
        for groups in events.values_mut() {
            let Some(groups) = groups.as_array_mut() else { continue };
            // Only a group THOR emptied goes; a group somebody left empty stays.
            let mut emptied_by_us = Vec::new();
            for (i, group) in groups.iter_mut().enumerate() {
                if let Some(hooks) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                    let before = hooks.len();
                    hooks.retain(|h| !is_thors_hook(h, &db));
                    removed += before - hooks.len();
                    if before > hooks.len() && hooks.is_empty() {
                        emptied_by_us.push(i);
                    }
                }
            }
            let mut index = 0;
            groups.retain(|_| {
                let keep = !emptied_by_us.contains(&index);
                index += 1;
                keep
            });
        }
    }
    if removed == 0 {
        return Ok(0);
    }
    let text = serde_json::to_string_pretty(&root)? + "\n";
    // The result must parse again before it replaces anything.
    serde_json::from_str::<Value>(&text).map_err(|e| anyhow::anyhow!("the rewritten settings would not parse ({e}); nothing was changed"))?;
    backup_once(settings, &raw)?;
    std::fs::write(settings, text)?;
    let after = std::fs::read_to_string(settings)?;
    if serde_json::from_str::<Value>(&after).is_err() {
        std::fs::write(settings, raw)?;
        anyhow::bail!("the settings file did not parse after the change; the original was put back");
    }
    Ok(removed)
}

/// Remove the `thor` entry of `mcpServers` when it is THOR's own program
/// (`mcp`). Another server, or a `thor` entry that runs something else, stays.
pub fn remove_thor_tool_server(claude_json: &Path) -> anyhow::Result<bool> {
    let Some((raw, mut root)) = read_json_object(claude_json)? else { return Ok(false) };
    let is_ours = root
        .get("mcpServers")
        .and_then(|s| s.get("thor"))
        .and_then(|e| e.get("command"))
        .and_then(Value::as_str)
        .is_some_and(|c| {
            let stem = Path::new(&c.replace('\\', "/")).file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_lowercase();
            stem == "mcp"
        });
    if !is_ours {
        return Ok(false);
    }
    root["mcpServers"].as_object_mut().expect("checked above").remove("thor");
    let text = serde_json::to_string_pretty(&root)? + "\n";
    serde_json::from_str::<Value>(&text)?;
    backup_once(claude_json, &raw)?;
    std::fs::write(claude_json, text)?;
    Ok(true)
}

/// Whether git's global hooks folder is THOR's own githooks folder.
pub fn hooks_path_is_thors(current: Option<&str>, db: &Path) -> bool {
    let Some(dir) = db.parent().map(|d| d.join("githooks")) else { return false };
    let ours = dir.to_string_lossy().replace('\\', "/");
    decide_hooks_path(current, &ours) == HooksPathOutcome::AlreadyOurs
}

/// Phase 1. Everything it removes is THOR's own; see the module comment.
pub fn phase_one(targets: &Targets, git: &dyn GitHooksPath) -> anyhow::Result<PhaseOneReport> {
    // Both files are checked before anything is written, so a damaged file
    // stops the whole thing with nothing changed.
    read_json_object(&targets.settings)?;
    read_json_object(&targets.claude_json)?;

    let mut report = PhaseOneReport { hooks_removed: remove_thor_hooks(&targets.settings, &targets.db)?, ..Default::default() };
    report.tool_server_removed = remove_thor_tool_server(&targets.claude_json)?;

    if let Some(path) = targets.eval_command.as_ref().filter(|p| p.exists()) {
        let ours = std::fs::read_to_string(path).map(|t| t.to_lowercase().contains("thor")).unwrap_or(false);
        if ours && std::fs::remove_file(path).is_ok() {
            report.eval_command_removed = true;
        }
    }

    if let Some(current) = git.current() {
        if hooks_path_is_thors(Some(&current), &targets.db) {
            if git.unset() {
                report.git_hooks_path_cleared = Some(current);
            }
        } else {
            report.git_hooks_path_left = Some(current);
        }
    }
    Ok(report)
}

/// The programs of THOR's folder that are running right now (file names),
/// found by asking the system. The installer itself is not counted.
pub trait RunningPrograms {
    fn running_in(&self, bin_dir: &Path) -> Vec<String>;
}

/// The real check: Windows asks PowerShell (windowless) for every running
/// program's path, other systems read `/proc`.
pub struct RealProcesses;

impl RunningPrograms for RealProcesses {
    fn running_in(&self, bin_dir: &Path) -> Vec<String> {
        let wanted = flatten_word(&bin_dir.display().to_string());
        let own = std::env::current_exe().ok().map(|p| flatten_word(&p.display().to_string()));
        let mut paths: Vec<String> = Vec::new();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let out = std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath } | ForEach-Object { $_.ExecutablePath }",
                ])
                .creation_flags(0x0800_0000)
                .output();
            if let Ok(out) = out {
                paths.extend(String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim().to_string()));
            }
        }
        #[cfg(not(windows))]
        {
            if let Ok(entries) = std::fs::read_dir("/proc") {
                for entry in entries.flatten() {
                    if let Ok(target) = std::fs::read_link(entry.path().join("exe")) {
                        paths.push(target.display().to_string());
                    }
                }
            }
        }
        let mut names: Vec<String> = paths
            .into_iter()
            .filter(|p| !p.is_empty())
            .filter(|p| {
                let flat = flatten_word(p);
                Path::new(&flat).parent().is_some_and(|d| d.to_string_lossy() == wanted.as_str()) && Some(&flat) != own.as_ref()
            })
            .filter_map(|p| Path::new(&p.replace('\\', "/")).file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// What phase 2 did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhaseTwoReport {
    pub backup_copies_removed: Vec<PathBuf>,
    pub memory_deleted: Vec<PathBuf>,
    pub memory_kept: Option<PathBuf>,
    /// The folder of the programs, for the assistant to delete itself.
    pub program_folder: PathBuf,
}

impl PhaseTwoReport {
    pub fn plain_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for path in &self.backup_copies_removed {
            lines.push(format!("+ removed THOR's saved copy of a Claude Code settings file ({})", path.display()));
        }
        if !self.memory_deleted.is_empty() {
            lines.push(format!("+ deleted your memory ({} item(s), as you asked)", self.memory_deleted.len()));
            lines.push("= kept, if you have them: THOR's safety copies (backup-repo) and your personal library (library.db)".to_string());
        }
        if let Some(path) = &self.memory_kept {
            lines.push(format!("= your memory was kept at {}; delete that folder yourself any time", path.display()));
        }
        lines.push(format!(
            "Last step for the assistant: delete the folder {} (with Remove-Item, to the Recycle Bin is fine); a program cannot delete the folder it runs from. git, Python and the Visual C++ runtime stay.",
            self.program_folder.display()
        ));
        lines
    }
}

/// The files that make up the memory and its sidecars. THOR's own data
/// folder (named `thor2`) is deleted entry by entry except `backup-repo`, the
/// person's safety copies, and the personal library (`library.db` and its
/// sidecars), which is the person's own everyday knowledge and not part of the
/// work memory; any other folder only loses the files named after the memory
/// file.
fn memory_entries(db: &Path) -> Vec<PathBuf> {
    let Some(dir) = db.parent() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let own_folder = dir.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("thor2"));
    let stem = db.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if own_folder {
                name != "backup-repo" && !name.starts_with("library.db")
            } else {
                name.starts_with(&stem)
            }
        })
        .collect()
}

/// Phase 2. Refuses (nothing changed) while a THOR program is running or
/// while THOR's entries are still in Claude Code's settings.
pub fn phase_two(
    targets: &Targets,
    bin_dir: &Path,
    delete_memory: bool,
    processes: &dyn RunningPrograms,
) -> anyhow::Result<PhaseTwoReport> {
    let running = processes.running_in(bin_dir);
    anyhow::ensure!(
        running.is_empty(),
        "NOTHING WAS CHANGED: THOR is still running ({}). Close Claude Code completely and open it again, then finish removing THOR.",
        running.join(", ")
    );
    anyhow::ensure!(
        thor_hooks_in(&targets.settings, &targets.db)? == 0,
        "NOTHING WAS CHANGED: THOR is still connected to Claude Code. Run the first removal step (install --uninstall --db <memory>) before this one."
    );

    let mut report = PhaseTwoReport { program_folder: bin_dir.to_path_buf(), ..Default::default() };
    for file in [&targets.settings, &targets.claude_json] {
        let ext = file.extension().and_then(|e| e.to_str()).map(|e| format!("{e}.bak")).unwrap_or_else(|| "bak".to_string());
        let backup = file.with_extension(ext);
        if backup.exists() && std::fs::remove_file(&backup).is_ok() {
            report.backup_copies_removed.push(backup);
        }
    }
    if delete_memory {
        for path in memory_entries(&targets.db) {
            let gone = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            if gone.is_ok() {
                report.memory_deleted.push(path);
            }
        }
    } else if let Some(dir) = targets.db.parent() {
        report.memory_kept = Some(dir.to_path_buf());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::{Cell, RefCell};

    const DB: &str = "C:\\Users\\x\\AppData\\Local\\thor2\\thor.db";

    struct FakeGit {
        current: Option<String>,
        unset_called: Cell<bool>,
    }
    impl GitHooksPath for FakeGit {
        fn current(&self) -> Option<String> {
            self.current.clone()
        }
        fn unset(&self) -> bool {
            self.unset_called.set(true);
            true
        }
    }

    struct FakeProcesses(RefCell<Vec<String>>);
    impl RunningPrograms for FakeProcesses {
        fn running_in(&self, _: &Path) -> Vec<String> {
            self.0.borrow().clone()
        }
    }

    fn foreign_shell() -> Value {
        json!({"hooks": [{"type": "command", "command": "\"C:\\\\tools\\\\other.exe\" start"}]})
    }
    fn foreign_exec() -> Value {
        json!({"matcher": "Bash", "hooks": [{"type": "command", "command": "node", "args": ["guard.js", "hook"]}]})
    }
    fn shell_thor() -> Value {
        json!({"hooks": [{"type": "command", "command": format!("\"C:\\\\thor2\\\\serve.exe\" --db \"{DB}\" hook")}]})
    }
    fn exec_thor() -> Value {
        json!({"matcher": "*", "hooks": [{"type": "command", "command": "C:\\thor2\\serve.exe", "args": ["--db", DB, "hook"], "onFailure": "continue"}]})
    }
    fn backup_thor() -> Value {
        json!({"hooks": [{"type": "command", "command": "C:\\thor2\\backup.exe", "args": ["--db", DB, "--repo", "C:\\r"]}]})
    }
    fn mixed_group() -> Value {
        // One group holding a foreign hook and a THOR hook: only ours may go.
        json!({"hooks": [foreign_shell()["hooks"][0].clone(), shell_thor()["hooks"][0].clone()]})
    }

    fn targets(dir: &Path) -> Targets {
        Targets {
            settings: dir.join("settings.json"),
            claude_json: dir.join(".claude.json"),
            eval_command: Some(dir.join("thor-eval.md")),
            db: PathBuf::from(DB),
        }
    }

    fn write_settings(t: &Targets) -> Value {
        let settings = json!({
            "theme": "dark",
            "hooks": {
                "SessionStart": [foreign_shell(), shell_thor(), backup_thor()],
                "PreToolUse": [foreign_exec(), exec_thor()],
                "UserPromptSubmit": [mixed_group()],
                "Stop": [shell_thor()]
            }
        });
        std::fs::write(&t.settings, serde_json::to_string_pretty(&settings).unwrap()).unwrap();
        settings
    }

    #[test]
    fn phase_one_removes_thors_hooks_in_both_forms_and_leaves_foreign_ones_identical() {
        let dir = tempfile::tempdir().unwrap();
        let t = targets(dir.path());
        write_settings(&t);
        let git = FakeGit { current: None, unset_called: Cell::new(false) };

        let report = phase_one(&t, &git).unwrap();
        assert_eq!(report.hooks_removed, 5, "{report:?}");
        let now: Value = serde_json::from_str(&std::fs::read_to_string(&t.settings).unwrap()).unwrap();
        assert_eq!(now["theme"], json!("dark"));
        assert_eq!(now["hooks"]["SessionStart"], json!([foreign_shell()]));
        assert_eq!(now["hooks"]["PreToolUse"], json!([foreign_exec()]));
        assert_eq!(now["hooks"]["UserPromptSubmit"], json!([foreign_shell()]));
        assert_eq!(now["hooks"]["Stop"], json!([]), "a group that held only THOR goes");
        assert!(report.plain_lines().last().unwrap().contains("finish removing THOR"));
        assert!(dir.path().join("settings.json.bak").exists(), "the first backup is kept");

        // A second run is a no-op, byte for byte.
        let before = std::fs::read(&t.settings).unwrap();
        let again = phase_one(&t, &git).unwrap();
        assert_eq!(again.hooks_removed, 0);
        assert_eq!(std::fs::read(&t.settings).unwrap(), before);
    }

    #[test]
    fn phase_one_refuses_a_settings_file_that_does_not_parse_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let t = targets(dir.path());
        std::fs::write(&t.settings, "{ not json").unwrap();
        std::fs::write(&t.claude_json, json!({"mcpServers": {"thor": {"command": "C:\\thor2\\mcp.exe"}}}).to_string()).unwrap();
        let git = FakeGit { current: None, unset_called: Cell::new(false) };
        assert!(phase_one(&t, &git).is_err());
        assert_eq!(std::fs::read_to_string(&t.settings).unwrap(), "{ not json");
        assert!(std::fs::read_to_string(&t.claude_json).unwrap().contains("thor"), "the tool server was not touched either");
    }

    #[test]
    fn phase_one_removes_the_thor_server_only_and_the_eval_command() {
        let dir = tempfile::tempdir().unwrap();
        let t = targets(dir.path());
        let other = json!({"type": "stdio", "command": "npx", "args": ["x"]});
        std::fs::write(
            &t.claude_json,
            json!({"userID": "u", "mcpServers": {"thor": {"type": "stdio", "command": "C:\\thor2\\mcp.exe", "args": []}, "other": other}}).to_string(),
        )
        .unwrap();
        std::fs::write(t.eval_command.as_ref().unwrap(), "# THOR evaluation\n").unwrap();
        let git = FakeGit { current: None, unset_called: Cell::new(false) };

        let report = phase_one(&t, &git).unwrap();
        assert!(report.tool_server_removed && report.eval_command_removed);
        let now: Value = serde_json::from_str(&std::fs::read_to_string(&t.claude_json).unwrap()).unwrap();
        assert_eq!(now["mcpServers"], json!({"other": other}));
        assert_eq!(now["userID"], json!("u"));
        assert!(!t.eval_command.as_ref().unwrap().exists());

        let again = phase_one(&t, &git).unwrap();
        assert!(!again.tool_server_removed && !again.eval_command_removed);
    }

    #[test]
    fn a_thor_entry_that_runs_another_program_is_left_in_the_tool_server_file() {
        let dir = tempfile::tempdir().unwrap();
        let t = targets(dir.path());
        std::fs::write(&t.claude_json, json!({"mcpServers": {"thor": {"command": "C:\\mine\\my-server.exe"}}}).to_string()).unwrap();
        assert!(!remove_thor_tool_server(&t.claude_json).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn git_hooks_path_is_cleared_only_when_it_is_thors() {
        let dir = tempfile::tempdir().unwrap();
        let t = targets(dir.path());
        let theirs = FakeGit { current: Some("C:/their/hooks".to_string()), unset_called: Cell::new(false) };
        let report = phase_one(&t, &theirs).unwrap();
        assert!(!theirs.unset_called.get(), "somebody else's folder is never unset");
        assert_eq!(report.git_hooks_path_left.as_deref(), Some("C:/their/hooks"));

        let ours = FakeGit { current: Some("C:/Users/x/AppData/Local/thor2/githooks".to_string()), unset_called: Cell::new(false) };
        let report = phase_one(&t, &ours).unwrap();
        assert!(ours.unset_called.get());
        assert!(report.git_hooks_path_cleared.is_some());
        assert!(hooks_path_is_thors(Some("C:\\Users\\x\\AppData\\Local\\thor2\\githooks\\"), Path::new(DB)));
        assert!(!hooks_path_is_thors(None, Path::new(DB)));
    }

    fn data_dir_with_memory(dir: &Path) -> Targets {
        let data = dir.join("thor2");
        std::fs::create_dir_all(data.join("backup-repo")).unwrap();
        std::fs::write(data.join("thor.db"), b"memory").unwrap();
        std::fs::write(data.join("thor.db-wal"), b"wal").unwrap();
        std::fs::write(data.join("work-folders.json"), b"{}").unwrap();
        std::fs::write(data.join("library.db"), b"recipes").unwrap();
        let t = Targets {
            settings: dir.join("settings.json"),
            claude_json: dir.join(".claude.json"),
            eval_command: None,
            db: data.join("thor.db"),
        };
        std::fs::write(&t.settings, "{}").unwrap();
        std::fs::write(dir.join("settings.json.bak"), "{}").unwrap();
        std::fs::write(dir.join(".claude.json.bak"), "{}").unwrap();
        t
    }

    #[test]
    fn phase_two_refuses_while_a_thor_program_runs_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let t = data_dir_with_memory(dir.path());
        let running = FakeProcesses(RefCell::new(vec!["mcp.exe".to_string()]));
        let err = phase_two(&t, dir.path(), true, &running).unwrap_err().to_string();
        assert!(err.contains("NOTHING WAS CHANGED") && err.contains("mcp.exe"), "{err}");
        assert!(t.db.exists() && dir.path().join("settings.json.bak").exists());
    }

    #[test]
    fn phase_two_keeps_the_memory_without_the_flag_and_removes_the_backup_copies() {
        let dir = tempfile::tempdir().unwrap();
        let t = data_dir_with_memory(dir.path());
        let none = FakeProcesses(RefCell::new(vec![]));
        let report = phase_two(&t, dir.path(), false, &none).unwrap();
        assert!(t.db.exists(), "the memory stays without --delete-memory");
        assert!(report.memory_deleted.is_empty() && report.memory_kept.is_some());
        assert_eq!(report.backup_copies_removed.len(), 2);
        assert!(!dir.path().join("settings.json.bak").exists() && !dir.path().join(".claude.json.bak").exists());
        let last = report.plain_lines().pop().unwrap();
        assert!(last.contains(&dir.path().display().to_string()) && last.contains("git, Python and the Visual C++ runtime stay"), "{last}");
    }

    #[test]
    fn phase_two_deletes_the_memory_and_sidecars_only_with_the_flag_but_keeps_the_safety_copies() {
        let dir = tempfile::tempdir().unwrap();
        let t = data_dir_with_memory(dir.path());
        let none = FakeProcesses(RefCell::new(vec![]));
        let report = phase_two(&t, dir.path(), true, &none).unwrap();
        assert_eq!(report.memory_deleted.len(), 3, "{report:?}");
        let data = t.db.parent().unwrap();
        assert!(!t.db.exists() && !data.join("thor.db-wal").exists() && !data.join("work-folders.json").exists());
        assert!(data.join("backup-repo").exists(), "the safety copies are never deleted by this");
        assert!(data.join("library.db").exists(), "the personal library is not the work memory and stays");
    }

    #[test]
    fn phase_two_refuses_while_thors_hooks_are_still_in_the_settings() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = data_dir_with_memory(dir.path());
        t.db = PathBuf::from(DB);
        write_settings(&t);
        let none = FakeProcesses(RefCell::new(vec![]));
        let err = phase_two(&t, dir.path(), false, &none).unwrap_err().to_string();
        assert!(err.contains("still connected"), "{err}");
    }
}
