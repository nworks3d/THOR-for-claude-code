//! THE SCRIPT LOOK-THROUGH (item 71, part A). A rule bound to a host, or to an
//! MCP tool by name, stays silent when the call runs a script file that holds
//! that host or that tool name INSIDE the file: the command line then names
//! neither. This module reads the script file a shell call is about to run and
//! hands back the hosts and tool-name words it holds, so the serving path can
//! offer the facts that belong to them, marked as reached through the script.
//!
//! IT ONLY INFORMS. It is called after the guard has decided
//! (`serve::guard_verdict` never sees any of this), its answer is read only by
//! `rank` (matching), `render` (the line) and the recording of a serving. It
//! never refuses, never writes, never executes or imports what it reads, and
//! never touches the network: it opens one file the machine can already open.
//!
//! BUDGET. Every read runs on its own worker thread against one shared
//! deadline ([`BUDGET`]). A timeout, a missing file, a refused read, a share
//! that is off or a path that holds a variable means: no routes, no message,
//! and the call goes on exactly as before. One level only: what the script
//! itself calls is not followed.
//!
//! The script detection (`script_of` and its tables) lives here, and
//! `route_probe` imports it, so the hook and `serve sweep` read a call the same
//! way.

use model::shell::Segment;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// How long the whole look-through may take, reading included: a 400 ms ceiling
/// on the wall clock, less the roughly 15 ms tick of the Windows timer a wait
/// can overshoot by (measured: a dead share returned at 414 ms with 400 here).
pub const BUDGET: Duration = Duration::from_millis(380);

/// How much of one script is read. Covers every script measured on the owner's
/// machine but one; a bigger file is read in part, which is fine for a path
/// that only informs.
pub const READ_LIMIT: u64 = 256 * 1024;

/// How many scripts one call may have read.
pub const MAX_SCRIPTS: usize = 3;

const MAX_HOSTS: usize = 64;
const MAX_WORDS: usize = 500;

/// The shortest tool-name suffix that counts as a route (`fusion_mcp_execute`
/// yes, `execute` no).
const MIN_SUFFIX: usize = 8;

/// Programs that run the script file they are given.
const INTERPRETERS: &[&str] = &[
    "node", "nodejs", "python", "python3", "py", "bash", "sh", "zsh", "dash", "ksh", "ruby", "perl", "php", "deno", "bun", "tsx", "ts-node",
    "cscript", "wscript", "pwsh", "powershell", "cmd",
];

/// Words in front of the program that change nothing about what runs.
const PASS_THROUGH: &[&str] = &["sudo", "env", "nohup", "exec", "command", "time", "nice"];

pub const SCRIPT_EXTENSIONS: &[&str] = &[".ps1", ".sh", ".bash", ".py", ".js", ".mjs", ".cjs", ".ts", ".bat", ".cmd", ".rb", ".pl", ".vbs", ".php"];

/// PowerShell options that take a value, which is not the script.
const POWERSHELL_VALUE_FLAGS: &[&str] = &["-executionpolicy", "-ep", "-windowstyle", "-w", "-workingdirectory", "-wd", "-configurationname", "-version"];

/// What a script file names: the hosts and the tool-name words in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteTargets {
    /// The script's file name as the call spelled it (for the line shown).
    pub script: String,
    /// `host` and `host:port`, as `hosts_in_command` reads a command line.
    pub hosts: Vec<String>,
    /// Lowercase identifier-like words of 8+ characters that hold an
    /// underscore. Compared only with the tool-name suffix of a bound
    /// `mcp__<server>__<tool>`.
    pub words: Vec<String>,
}

pub fn file_name_of(path: &str) -> String {
    path.replace('\\', "/").rsplit('/').next().unwrap_or("").to_lowercase()
}

/// The script file an interpreter is told to run, if its arguments name one.
fn script_argument(program: &str, args: &[String]) -> Option<String> {
    match program {
        "pwsh" | "powershell" => {
            let mut i = 0;
            while i < args.len() {
                let arg = args[i].to_ascii_lowercase();
                if arg == "-file" || arg == "-f" {
                    return args.get(i + 1).cloned();
                }
                if matches!(arg.as_str(), "-command" | "-c" | "-encodedcommand" | "-e") {
                    return None;
                }
                if !arg.starts_with('-') {
                    return Some(args[i].clone());
                }
                i += if POWERSHELL_VALUE_FLAGS.contains(&arg.as_str()) { 2 } else { 1 };
            }
            None
        }
        "cmd" => {
            let at = args.iter().position(|a| matches!(a.to_ascii_lowercase().as_str(), "/c" | "/k"))?;
            args.get(at + 1)?.split_whitespace().next().map(str::to_string)
        }
        _ => {
            for arg in args {
                if matches!(arg.as_str(), "-c" | "-e" | "-m" | "-p" | "--eval" | "--print") {
                    return None;
                }
                if !arg.starts_with('-') {
                    return Some(arg.clone());
                }
            }
            None
        }
    }
}

/// The script file one simple command executes: an interpreter followed by the
/// file, the file itself as the command, or a `&` / `.` / `source` call.
pub fn script_of(seg: &Segment) -> Option<String> {
    let words = &seg.words;
    let mut i = 0;
    while i < words.len() {
        let word = words[i].to_ascii_lowercase();
        let assignment = word
            .split_once('=')
            .is_some_and(|(name, _)| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        if !assignment && !PASS_THROUGH.contains(&word.as_str()) {
            break;
        }
        i += 1;
    }
    let first = words.get(i)?;
    let rest = &words[i + 1..];
    if matches!(first.as_str(), "&" | "." | "source") {
        return rest.first().cloned();
    }
    let program = file_name_of(first);
    let program = program.strip_suffix(".exe").unwrap_or(&program);
    if INTERPRETERS.contains(&program) {
        script_argument(program, rest)
    } else {
        let flat = first.replace('\\', "/").to_lowercase();
        (flat.contains('/') || SCRIPT_EXTENSIONS.iter().any(|e| flat.ends_with(e))).then(|| first.clone())
    }
}

/// `/c/Users/x` (how Git Bash spells a drive) as `C:/Users/x`, on Windows only.
fn drive_form(path: &str) -> String {
    let bytes = path.as_bytes();
    if cfg!(windows) && bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b'/' {
        return format!("{}:{}", (bytes[1] as char).to_ascii_uppercase(), &path[2..]);
    }
    path.to_string()
}

fn is_rooted(path: &str) -> bool {
    path.starts_with('/') || path.starts_with('\\') || path.as_bytes().get(1) == Some(&b':')
}

/// The file `script` means, when it can be told without guessing: a script
/// extension, no variable or glob in it, a relative path read against the
/// folder the segment runs in (`dir`, when an earlier `cd` says so, else
/// `cwd`). `dir_unsure` is true when a `cd` that may not have run would put the
/// call elsewhere: then a relative path is not guessed.
pub fn script_path(script: &str, dir: Option<&str>, dir_unsure: bool, cwd: Option<&Path>) -> Option<PathBuf> {
    let lower = script.to_lowercase();
    if !SCRIPT_EXTENSIONS.iter().any(|e| lower.ends_with(e)) {
        return None;
    }
    if script.chars().any(|c| matches!(c, '$' | '%' | '*' | '?' | '{' | '}' | '`')) || script.starts_with('~') {
        return None;
    }
    let script = drive_form(script);
    if is_rooted(&script) {
        return Some(PathBuf::from(script));
    }
    if dir_unsure {
        return None;
    }
    let base = match dir {
        Some(dir) => {
            if dir.chars().any(|c| matches!(c, '$' | '%' | '*' | '?' | '{' | '}' | '`')) || dir.starts_with('~') || dir == "-" {
                return None;
            }
            let dir = drive_form(dir);
            if is_rooted(&dir) {
                PathBuf::from(dir)
            } else {
                cwd?.join(dir)
            }
        }
        None => cwd?.to_path_buf(),
    };
    Some(base.join(script))
}

fn is_comment(line: &str) -> bool {
    let line = line.trim_start();
    let lower = line.to_ascii_lowercase();
    line.starts_with('#') || line.starts_with("//") || line.starts_with("::") || lower == "rem" || lower.starts_with("rem ")
}

fn word_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"[A-Za-z0-9_]{8,}").expect("a fixed pattern"))
}

/// What a script's text names. Full-line comments are dropped first: a host in
/// a comment is text about the host, not a route to it.
pub fn routes_in_text(script: &str, text: &str) -> RouteTargets {
    let mut routes = RouteTargets { script: script.to_string(), ..Default::default() };
    let mut seen_words: HashSet<String> = HashSet::new();
    for line in text.lines().filter(|l| !l.trim().is_empty() && !is_comment(l)) {
        if routes.hosts.len() < MAX_HOSTS {
            for host in model::normalize::hosts_in_command(line) {
                if routes.hosts.len() < MAX_HOSTS && !routes.hosts.contains(&host) {
                    routes.hosts.push(host);
                }
            }
        }
        if routes.words.len() < MAX_WORDS {
            for m in word_regex().find_iter(line) {
                let word = m.as_str().to_lowercase();
                if word.contains('_') && routes.words.len() < MAX_WORDS && seen_words.insert(word.clone()) {
                    routes.words.push(word);
                }
            }
        }
    }
    routes
}

/// The decoded first [`READ_LIMIT`] bytes of a script, or `None` when it cannot
/// be read or is a binary (a NUL byte in its first 8 KB that is not UTF-16).
pub fn read_script(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(READ_LIMIT).read_to_end(&mut bytes).ok()?;
    let has_bom = bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]);
    if !has_bom && bytes.iter().take(8 * 1024).any(|b| *b == 0) {
        return None;
    }
    Some(crate::work_folders::decode_text(&bytes))
}

/// The hosts and words of every script in `paths` (at most [`MAX_SCRIPTS`]),
/// each read by `read` on its own thread, all within `budget` in total. A read
/// that does not answer in time, or answers `None`, adds nothing.
pub fn routes_of_paths(
    paths: Vec<PathBuf>,
    budget: Duration,
    read: Arc<dyn Fn(&Path) -> Option<String> + Send + Sync>,
) -> Vec<RouteTargets> {
    let deadline = Instant::now() + budget;
    let jobs: Vec<_> = paths
        .into_iter()
        .take(MAX_SCRIPTS)
        .map(|path| {
            let read = Arc::clone(&read);
            crate::work_folders::spawn(move || {
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                read(&path).map(|text| routes_in_text(&name, &text))
            })
        })
        .collect();
    jobs.into_iter()
        .filter_map(|job| job.wait(deadline).flatten())
        .filter(|r| !r.hosts.is_empty() || !r.words.is_empty())
        .collect()
}

/// The script files a shell call runs that can be read without guessing.
pub fn script_files_of(tool: &str, command: &str, cwd: Option<&Path>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for seg in model::shell::call_segments(tool, command).iter().filter(|s| !s.remote && !s.tool) {
        let Some(script) = script_of(seg) else { continue };
        let Some(path) = script_path(&script, seg.dir.as_deref(), seg.dir != seg.any_cd_dir, cwd) else { continue };
        if !paths.contains(&path) {
            paths.push(path);
        }
        if paths.len() >= MAX_SCRIPTS {
            break;
        }
    }
    paths
}

/// What the scripts a shell call runs name. Empty for a call that runs none,
/// for anything that cannot be read in time, and for every other tool.
pub fn look_through(tool: &str, command: &str, cwd: Option<&Path>) -> Vec<RouteTargets> {
    let paths = script_files_of(tool, command, cwd);
    if paths.is_empty() {
        return Vec::new();
    }
    routes_of_paths(paths, BUDGET, Arc::new(read_script))
}

/// The tool-name suffix of a bound `mcp__<server>__<tool>`: the text after the
/// last `__`, when it is long enough and holds an underscore.
pub fn tool_suffix(binding_value: &str) -> Option<String> {
    let first = binding_value.split_whitespace().next()?;
    if !first.starts_with("mcp__") {
        return None;
    }
    let suffix = first.rsplit("__").next()?.to_lowercase();
    (suffix.len() >= MIN_SUFFIX && suffix.contains('_')).then_some(suffix)
}

/// Whether the words of a script reach the tool a Command binding names.
pub fn words_reach_tool(words: &[String], binding_value: &str) -> bool {
    let Some(suffix) = tool_suffix(binding_value) else { return false };
    let tail = format!("__{suffix}");
    words.iter().any(|w| *w == suffix || w.ends_with(&tail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(name: &str, bytes: &[u8]) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("thor-script-routes-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn a_host_and_a_tool_word_are_found_and_comments_are_dropped() {
        let text = "# talks to http://10.0.0.9:80/old\n$u = \"http://127.0.0.1:27182/mcp\"\n// http://skipped.example.org\nname = \"fusion_mcp_execute\"\nREM http://rem.example.org\n";
        let routes = routes_in_text("x.ps1", text);
        assert_eq!(routes.hosts, vec!["127.0.0.1".to_string(), "127.0.0.1:27182".to_string()]);
        assert!(routes.words.contains(&"fusion_mcp_execute".to_string()));
        assert!(!routes.hosts.iter().any(|h| h.contains("skipped") || h.contains("rem.") || h.contains("10.0.0.9")));
    }

    #[test]
    fn words_keep_long_underscored_names_and_skip_the_rest() {
        let routes = routes_in_text("x.py", "call(fusion_mcp_execute); execute(); short_a(); plainlongword();");
        assert_eq!(routes.words, vec!["fusion_mcp_execute".to_string()]);
    }

    #[test]
    fn utf8_bom_and_utf16_scripts_are_decoded() {
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(b"$u = 'http://10.0.0.5:8080/x'");
        let path = scratch("bom.ps1", &bom);
        assert!(read_script(&path).unwrap().contains("10.0.0.5:8080"));
        let mut wide = vec![0xFF, 0xFE];
        for unit in "$u = 'http://10.0.0.6:9090/x'".encode_utf16() {
            wide.extend_from_slice(&unit.to_le_bytes());
        }
        let path = scratch("wide.ps1", &wide);
        assert!(read_script(&path).unwrap().contains("10.0.0.6:9090"));
    }

    #[test]
    fn a_binary_file_is_skipped() {
        let path = scratch("bin.js", b"abc\0def http://nope.example.org");
        assert!(read_script(&path).is_none());
    }

    #[test]
    fn only_the_first_256_kb_is_read() {
        let mut text = vec![b'a'; READ_LIMIT as usize];
        text.extend_from_slice(b"\nhttp://late.example.org/x\n");
        let path = scratch("big.js", &text);
        let read = read_script(&path).unwrap();
        assert_eq!(read.len() as u64, READ_LIMIT);
        assert!(!read.contains("late.example.org"));
    }

    #[test]
    fn a_path_with_a_variable_or_a_glob_is_never_read() {
        let cwd = Some(Path::new("C:/work"));
        for bad in ["$w\\list.py", "C:/x/*.test.cjs", "C:/x/%TMP%/a.ps1", "{a}.sh", "~/a.sh", "C:/x/a.txt"] {
            assert!(script_path(bad, None, false, cwd).is_none(), "{bad}");
        }
        assert_eq!(script_path("C:/work/a.ps1", None, false, None), Some(PathBuf::from("C:/work/a.ps1")));
    }

    #[test]
    fn a_relative_path_is_read_against_the_cwd_and_a_cd() {
        let cwd = Some(Path::new("C:/work"));
        assert_eq!(script_path("a.ps1", None, false, cwd), Some(PathBuf::from("C:/work").join("a.ps1")));
        assert_eq!(script_path("a.ps1", Some("sub"), false, cwd), Some(PathBuf::from("C:/work").join("sub").join("a.ps1")));
        assert_eq!(script_path("a.ps1", Some("C:/other"), false, cwd), Some(PathBuf::from("C:/other").join("a.ps1")));
        assert!(script_path("a.ps1", Some("$d"), false, cwd).is_none());
        assert!(script_path("a.ps1", None, true, cwd).is_none(), "a cd that may not have run is not guessed");
        assert!(script_path("a.ps1", None, false, None).is_none(), "no folder, no guess");
    }

    #[test]
    fn the_script_a_call_runs_is_found_through_the_segments() {
        let paths = script_files_of("PowerShell", "powershell -NoProfile -File C:/w/run.ps1 -Url x", None);
        assert_eq!(paths, vec![PathBuf::from("C:/w/run.ps1")]);
        assert!(script_files_of("Bash", "grep -n foo C:/w/run.ps1", None).is_empty(), "a script that is only read is not run");
        assert!(script_files_of("Bash", "echo hi", None).is_empty());
        let many = "node C:/a/1.js; node C:/a/2.js; node C:/a/3.js; node C:/a/4.js";
        assert_eq!(script_files_of("Bash", many, None).len(), MAX_SCRIPTS);
    }

    #[test]
    fn the_look_through_reads_the_file_a_call_runs() {
        let path = scratch("run.ps1", b"param($Py)\n$name = 'fusion_mcp_execute'\n$h = 'http://127.0.0.1:27182/mcp'\n");
        let call = format!("powershell -File \"{}\"", path.display());
        let routes = look_through("PowerShell", &call, None);
        assert_eq!(routes.len(), 1, "{routes:?}");
        assert!(routes[0].hosts.contains(&"127.0.0.1:27182".to_string()));
        assert!(routes[0].words.contains(&"fusion_mcp_execute".to_string()));
        assert_eq!(routes[0].script, "run.ps1");
    }

    #[test]
    fn a_missing_file_gives_no_routes() {
        let routes = look_through("Bash", "bash /definitely/not/here/x.sh", None);
        assert!(routes.is_empty());
    }

    /// THE BUDGET: a read that never answers costs the ceiling, not more, and
    /// adds nothing.
    #[test]
    fn a_read_that_never_answers_returns_within_the_budget() {
        let started = Instant::now();
        let never: Arc<dyn Fn(&Path) -> Option<String> + Send + Sync> = Arc::new(|_| {
            std::thread::sleep(Duration::from_secs(5));
            Some("http://late.example.org".to_string())
        });
        let routes = routes_of_paths(vec![PathBuf::from("a.sh"), PathBuf::from("b.sh")], Duration::from_millis(150), never);
        assert!(routes.is_empty());
        assert!(started.elapsed() < Duration::from_millis(900), "{:?}", started.elapsed());
    }

    #[test]
    fn the_tool_suffix_needs_eight_characters_and_an_underscore() {
        assert_eq!(tool_suffix("mcp__Autodesk_Fusion__fusion_mcp_execute").as_deref(), Some("fusion_mcp_execute"));
        assert_eq!(tool_suffix("mcp__thor__lookup"), None);
        assert_eq!(tool_suffix("mcp__x__execute"), None);
        assert_eq!(tool_suffix("Agent"), None);
        let words = vec!["fusion_mcp_execute".to_string()];
        assert!(words_reach_tool(&words, "mcp__Autodesk_Fusion__fusion_mcp_execute"));
        assert!(!words_reach_tool(&words, "mcp__Autodesk_Fusion__mcp_execute"));
        let full = vec!["mcp__autodesk_fusion__fusion_mcp_execute".to_string()];
        assert!(words_reach_tool(&full, "mcp__Autodesk_Fusion__fusion_mcp_execute"), "the whole tool name is a route too");
    }
}
