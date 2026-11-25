//! `serve inventory`: a read-only look at the earlier Claude Code sessions on
//! this computer - which folder each ran in, what it was about, and which
//! files it wrote where - so the assistant can propose to put it in order
//! (one fixed folder per subject, with its own memory).
//!
//! WHY IT EXISTS. A first-time user's first complaint was that Claude "puts
//! everything randomly everywhere": files in the home folder, on the Desktop,
//! in Downloads, and a pile of old conversations nobody can find again. The
//! assistant cannot propose an order without seeing that pile, and reading raw
//! transcripts costs far too much, so this module boils each session down to a
//! few lines and `digest` boils one session down to something cheap to read.
//!
//! Claude Code keeps every session as a JSON-lines file under
//! `~/.claude/projects/<encoded folder>/<session id>.jsonl`. Each line has a
//! `type` (user, assistant, system, summary, ...), a `cwd`, a `timestamp` and
//! a `sessionId`; an assistant line carries `message.content[]` items, and a
//! `tool_use` item names the tool and its `input` (Write and Edit carry
//! `input.file_path`). Nothing here ever writes anywhere, and a line that does
//! not parse is skipped, never an error: a transcript is somebody else's file.
//!
//! The wording of what is printed is for the ASSISTANT to read; the person
//! sees only what the assistant makes of it.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// At most this many sessions are listed; the rest are counted.
pub const MAX_SESSIONS: usize = 40;
/// At most this many written files are listed per session (and loose files).
pub const MAX_FILES: usize = 30;
/// A first prompt is cut at this many characters.
pub const PROMPT_CHARS: usize = 200;
/// Each prompt or reply in a digest is cut at this many characters.
pub const DIGEST_ITEM_CHARS: usize = 600;
/// A digest stops at about this many characters.
pub const DIGEST_CHARS: usize = 20_000;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WrittenFile {
    pub path: String,
    pub exists: bool,
    /// The file sits in a Claude desktop app scratch workspace, a temporary
    /// place the app may clean up.
    pub at_risk: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub session_id: String,
    /// The transcript file, for `inventory --digest`.
    pub file: String,
    pub first: String,
    pub last: String,
    pub prompts: usize,
    pub first_prompt: String,
    pub files: Vec<WrittenFile>,
    pub files_left_out: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderInfo {
    pub folder: String,
    pub folder_exists: bool,
    /// A git repository, or already a THOR project or topic folder.
    pub ordered: bool,
    /// Why it is not in order: "home folder", "Desktop", "no project", ...
    pub why_not_ordered: Option<String>,
    /// The conversations ran in the desktop app's scratch workspace ("No
    /// folder" in its sidebar).
    pub without_folder: bool,
    /// A conversation without a folder that wrote no file that still exists:
    /// there is nothing to move out of the temporary workspace, so it does
    /// not stand in the way of "nothing to order". It stays in the listing
    /// because what it taught can still be stored. Always false for a
    /// folder that is not a temporary workspace.
    pub nothing_to_move: bool,
    pub sessions: Vec<SessionInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Inventory {
    pub sessions_found: usize,
    pub sessions_left_out: usize,
    pub sessions_left_out_unordered: usize,
    pub sessions_left_out_ordered: usize,
    pub folders: Vec<FolderInfo>,
    /// Files sessions wrote straight into the home folder, Desktop,
    /// Documents or Downloads.
    pub loose_files: Vec<WrittenFile>,
    pub loose_files_left_out: usize,
    /// Files that were loose when THOR was installed (the snapshot beside the
    /// store), are gone now, and are in no tidy undo log: someone moved or
    /// deleted them behind the tidy step's back. They stay listed, and they
    /// stand in the way of "nothing to order". Empty without a snapshot.
    #[serde(default)]
    pub vanished: Vec<String>,
}

// ------------------------------------------------------------------ places

// Two different questions are asked of a path, and they are kept apart on purpose:
//
// 1. IDENTITY of a whole path: is this folder the home folder, is this file inside
//    that target, is this the path the snapshot named. That follows the system:
//    case does not matter on Windows, and not for any path that starts with a
//    drive letter (`C:/...`) wherever the program runs; on Linux and macOS a
//    path without a drive letter is case-sensitive (two names that differ in
//    case are two folders). This is `norm` / `norm_with`.
// 2. RECOGNISING a name or a pattern inside a path: the broad folder names
//    (Desktop, Documents ...), OneDrive, AppData/Local/Temp, claude/scratchpad,
//    scratch-workspaces, Program Files. These are names Windows and the apps
//    write in their own case ("AppData", "Program Files"), and a person's
//    transcript can come from another machine, so they are matched
//    case-insensitively on EVERY system. That is `pattern_form`.
//
// Mixing the two (lower-casing only on Windows and then matching lower-case
// names) is what broke the Linux build of 2.6.12.

/// Whether `s` (a path with forward slashes) starts with a drive letter, `X:`.
fn has_drive_letter(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.next() == Some(':')
}

/// The text form of a path for comparing its IDENTITY: forward slashes, no
/// trailing slash, `.` and `..` resolved lexically (never looking at the disk),
/// and lower case when `system_folds_case` (Windows) or when the path starts
/// with a drive letter. `norm` calls this with the system's answer; a test calls
/// it with `false` to see what Linux does.
pub fn norm_with(p: &Path, system_folds_case: bool) -> String {
    let s = p.display().to_string().replace('\\', "/");
    let s = s.strip_prefix("//?/").unwrap_or(&s);
    let mut parts: Vec<&str> = Vec::new();
    for (i, part) in s.split('/').enumerate() {
        match part {
            "" if i > 0 => {}
            "." => {}
            ".." => {
                // Go up one level, but never above the root or a drive.
                if parts.last().is_some_and(|l| !l.is_empty() && !l.ends_with(':') && *l != "..") {
                    parts.pop();
                } else if parts.is_empty() {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    let t = joined.trim_end_matches('/');
    if system_folds_case || has_drive_letter(t) {
        t.to_lowercase()
    } else {
        t.to_string()
    }
}

/// `norm_with` for this system: the identity form of a path.
pub fn norm(p: &Path) -> String {
    norm_with(p, cfg!(windows))
}

/// The form of a path in which NAMES and patterns are recognised: always lower
/// case, whatever the system (see the comment above).
fn pattern_form(p: &Path) -> String {
    norm_with(p, true)
}

/// The user's home folder: `USERPROFILE`, then `HOME`.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).map(PathBuf::from).ok()
}

const BROAD_NAMES: [(&str, &str); 6] = [
    ("desktop", "Desktop"),
    ("bureaublad", "Desktop"),
    ("documents", "Documents"),
    ("documenten", "Documents"),
    ("downloads", "Downloads"),
    ("download", "Downloads"),
];

/// Is this folder one of the places that are far too broad to be one subject:
/// the home folder, Desktop, Documents, Downloads (also below a OneDrive
/// folder) or a drive root? The word it gives is for the person's eyes.
pub fn broad_place(home: &Path, dir: &Path) -> Option<String> {
    broad_place_with(home, dir, cfg!(windows))
}

/// `broad_place` with the system's case answer given (see `norm_with`). The
/// home folder is an identity question; the folder names are recognised in any case.
pub fn broad_place_with(home: &Path, dir: &Path, system_folds_case: bool) -> Option<String> {
    let id = |p: &Path| norm_with(p, system_folds_case);
    let d = id(dir);
    if d.is_empty() || d.ends_with(':') || dir.parent().is_none_or(|p| p.as_os_str().is_empty()) {
        return Some("drive root".to_string());
    }
    let h = id(home);
    if d == h {
        return Some("home folder".to_string());
    }
    let name = dir.file_name().and_then(|n| n.to_str()).map(str::to_lowercase).unwrap_or_default();
    let parent = dir.parent().map(id).unwrap_or_default();
    let under_home = parent == h;
    let under_onedrive =
        parent.strip_prefix(&format!("{h}/")).is_some_and(|rest| !rest.contains('/') && rest.to_lowercase().starts_with("onedrive"));
    if under_home || under_onedrive {
        if let Some((_, label)) = BROAD_NAMES.iter().find(|(n, _)| *n == name) {
            return Some((*label).to_string());
        }
    }
    None
}

/// A folder that belongs to a program, not to the person's work (application
/// data, program files, Claude's own settings folder).
pub fn system_place(home: &Path, dir: &Path) -> Option<&'static str> {
    system_place_with(home, dir, cfg!(windows))
}

/// `system_place` with the system's case answer given (see `norm_with`). Where
/// the home folder ends is an identity question; the program folder names after
/// it are recognised in any case.
pub fn system_place_with(home: &Path, dir: &Path, system_folds_case: bool) -> Option<&'static str> {
    // Only the part below the home folder counts: the home folder itself may
    // sit anywhere (a test folder under a temp folder, say).
    let full = norm_with(dir, system_folds_case);
    let h = format!("{}/", norm_with(home, system_folds_case));
    let below = full.strip_prefix(&h).unwrap_or(&full).to_lowercase();
    let d = format!("/{below}/");
    for (needle, label) in [("/appdata/", "application data"), ("/program files/", "program files"), ("/program files (x86)/", "program files"), ("/.claude/", "Claude's own settings")] {
        if d.contains(needle) {
            return Some(label);
        }
    }
    None
}

/// Whether a path lies in a Claude desktop app SCRATCH WORKSPACE: where the app
/// runs a conversation that has no folder ("No folder" in its sidebar). Under
/// application data, below a folder named `claude` or `claude-...` (the
/// `Claude` and `Claude-Data\\claude-<id>` folders), then `scratch-workspaces`.
pub fn is_app_scratch(p: &Path) -> bool {
    let n = pattern_form(p);
    let parts: Vec<&str> = n.split('/').collect();
    let Some(at) = parts.iter().position(|s| *s == "scratch-workspaces") else { return false };
    let before = &parts[..at];
    before.contains(&"appdata") && before.iter().any(|s| *s == "claude" || s.starts_with("claude-"))
}

/// Is `p` inside a per-session scratchpad Claude Code keeps under the system's
/// temporary folder (`AppData\Local\Temp\claude\...\scratchpad`)? A
/// conversation without a folder often writes its files there, and that folder
/// is cleared by a restart or a clean-up, so such a file is at risk too.
pub fn is_temp_scratchpad(p: &Path) -> bool {
    let n = pattern_form(p);
    let parts: Vec<&str> = n.split('/').collect();
    // Every AppData\Local\Temp in the path is tried, not only the first: a home
    // folder can itself sit under another one (a test sandbox does).
    (0..parts.len().saturating_sub(3)).any(|i| {
        parts[i..i + 3] == ["appdata", "local", "temp"] && parts[i + 3] == "claude" && parts[i + 4..].contains(&"scratchpad")
    })
}

/// A written file that sits in a place the app or the system may clear.
pub fn is_at_risk(p: &Path) -> bool {
    is_app_scratch(p) || is_temp_scratchpad(p)
}

/// `system_place` for a FILE that is to be moved out: a file in a temporary
/// place (`is_at_risk`) is the person's work and may be moved out of it.
pub fn file_source_system_place(home: &Path, file: &Path) -> Option<&'static str> {
    if is_at_risk(file) {
        None
    } else {
        system_place(home, file)
    }
}

/// `system_place` for a place THOR READS FROM: a scratch workspace of the
/// desktop app is the person's work (loose conversations), not a program's own
/// folder. Never use this for a place something is put INTO.
pub fn source_system_place(home: &Path, dir: &Path) -> Option<&'static str> {
    if is_app_scratch(dir) {
        None
    } else {
        system_place(home, dir)
    }
}

/// Is `p` somewhere below `home` (not `home` itself)?
pub fn is_inside(home: &Path, p: &Path) -> bool {
    is_inside_with(home, p, cfg!(windows))
}

/// `is_inside` with the system's case answer given (see `norm_with`).
pub fn is_inside_with(home: &Path, p: &Path, system_folds_case: bool) -> bool {
    let h = norm_with(home, system_folds_case);
    let n = norm_with(p, system_folds_case);
    n.starts_with(&format!("{h}/"))
}

// ----------------------------------------------------------------- secrets

/// The text with every real secret token (the same shapes the write gate
/// refuses in a note) replaced.
pub fn redact(text: &str) -> String {
    let found = model::secrets::find(text);
    if found.is_empty() {
        return text.to_string();
    }
    let mut out = String::new();
    let mut at = 0;
    for f in found {
        out.push_str(&text[at..f.start]);
        out.push_str("[secret removed]");
        at = f.end;
    }
    out.push_str(&text[at..]);
    out
}

fn clip(text: &str, max: usize) -> String {
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let one_line = redact(&one_line);
    if one_line.chars().count() <= max {
        return one_line;
    }
    let cut: String = one_line.chars().take(max).collect();
    format!("{cut}...")
}

// ------------------------------------------------------------- transcripts

/// One parsed line of interest from a transcript.
enum Event {
    Prompt { text: String, ts: String },
    Reply { text: String },
    Tool { name: String, detail: String, written: Option<String> },
}

fn text_of_prompt(content: &serde_json::Value) -> Option<String> {
    let raw = match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => {
            // A tool result is not something the person typed.
            if items.iter().any(|i| i.get("type").and_then(|t| t.as_str()) == Some("tool_result")) {
                return None;
            }
            items
                .iter()
                .filter(|i| i.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => return None,
    };
    let trimmed = raw.trim();
    // System text: reminders, slash-command echoes, interruptions, caveats.
    if trimmed.is_empty() || trimmed.starts_with('<') || trimmed.starts_with("Caveat:") || trimmed.starts_with("[Request interrupted") {
        return None;
    }
    Some(trimmed.to_string())
}

fn tool_detail(name: &str, input: &serde_json::Value) -> (String, Option<String>) {
    let s = |k: &str| input.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let path = s("file_path").or_else(|| s("notebook_path"));
    let written = if matches!(name, "Write" | "Edit" | "MultiEdit" | "NotebookEdit") { path.clone() } else { None };
    let detail = path
        .or_else(|| s("path"))
        .or_else(|| s("command").map(|c| clip(&c, 80)))
        .or_else(|| s("pattern"))
        .or_else(|| s("url"))
        .or_else(|| s("description"))
        .unwrap_or_default();
    (detail, written)
}

fn events_of_line(v: &serde_json::Value) -> Vec<Event> {
    let mut out = Vec::new();
    if v.get("isSidechain").and_then(|b| b.as_bool()) == Some(true) || v.get("isMeta").and_then(|b| b.as_bool()) == Some(true) {
        return out;
    }
    let ts = v.get("timestamp").and_then(|t| t.as_str()).unwrap_or("").to_string();
    let Some(message) = v.get("message") else { return out };
    match v.get("type").and_then(|t| t.as_str()) {
        Some("user") => {
            if let Some(text) = message.get("content").and_then(text_of_prompt) {
                out.push(Event::Prompt { text, ts });
            }
        }
        Some("assistant") => {
            if let Some(items) = message.get("content").and_then(|c| c.as_array()) {
                for item in items {
                    match item.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = item.get("text").and_then(|t| t.as_str()).filter(|t| !t.trim().is_empty()) {
                                out.push(Event::Reply { text: t.to_string() });
                            }
                        }
                        Some("tool_use") => {
                            let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                            let (detail, written) = tool_detail(&name, item.get("input").unwrap_or(&serde_json::Value::Null));
                            out.push(Event::Tool { name, detail, written });
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// The `cwd` and timestamp of a line, whatever its type.
fn cwd_and_ts(v: &serde_json::Value) -> (Option<String>, Option<String>) {
    (v.get("cwd").and_then(|c| c.as_str()).map(str::to_string), v.get("timestamp").and_then(|c| c.as_str()).map(str::to_string))
}

/// What one transcript says, or `None` when it holds no prompt of the person
/// (a session that only ran a helper, or an empty file).
pub fn read_session(file: &Path) -> Option<(String, SessionInfo)> {
    let content = std::fs::read_to_string(file).ok()?;
    let mut cwd: Option<String> = None;
    let mut first: Option<String> = None;
    let mut last: Option<String> = None;
    let mut first_prompt = String::new();
    let mut prompts = 0usize;
    let mut files: Vec<String> = Vec::new();
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let (c, ts) = cwd_and_ts(&v);
        // The LAST folder the transcript records: a conversation that moved to
        // another folder mid-way belongs to where it ended up.
        if c.is_some() {
            cwd = c;
        }
        if let Some(ts) = ts.filter(|t| t.get(..10).is_some()) {
            if first.as_ref().is_none_or(|f| ts < *f) {
                first = Some(ts.clone());
            }
            if last.as_ref().is_none_or(|l| ts > *l) {
                last = Some(ts);
            }
        }
        for event in events_of_line(&v) {
            match event {
                Event::Prompt { text, .. } => {
                    prompts += 1;
                    if first_prompt.is_empty() {
                        first_prompt = clip(&text, PROMPT_CHARS);
                    }
                }
                Event::Tool { written: Some(p), .. } => {
                    if Path::new(&p).is_absolute() && !files.contains(&p) {
                        files.push(p);
                    }
                }
                _ => {}
            }
        }
    }
    if prompts == 0 {
        return None;
    }
    let id = file.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let files: Vec<WrittenFile> = files.into_iter().map(|p| WrittenFile { exists: Path::new(&p).exists(), at_risk: is_at_risk(Path::new(&p)), path: p }).collect();
    let info = SessionInfo {
        session_id: id,
        file: file.display().to_string(),
        first: first.as_deref().and_then(|t| t.get(..10)).unwrap_or("").to_string(),
        last: last.as_deref().and_then(|t| t.get(..10)).unwrap_or("").to_string(),
        prompts,
        first_prompt,
        files,
        files_left_out: 0,
    };
    Some((cwd.unwrap_or_default(), info))
}

/// Where Claude Code keeps the transcripts: `<home>/.claude/projects`.
pub fn default_projects_dir(home: &Path) -> PathBuf {
    home.join(".claude").join("projects")
}

fn transcript_files(projects_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(projects_dir) else { return out };
    for d in dirs.flatten() {
        let p = d.path();
        if !p.is_dir() {
            continue;
        }
        if let Ok(files) = std::fs::read_dir(&p) {
            for f in files.flatten() {
                let fp = f.path();
                if fp.extension().is_some_and(|e| e == "jsonl") && fp.is_file() {
                    out.push(fp);
                }
            }
        }
    }
    out.sort();
    out
}

/// The inventory of every earlier session found under `projects_dir`, for a
/// person whose home folder is `home`.
pub fn inventory(projects_dir: &Path, home: &Path) -> Inventory {
    let mut all: Vec<(String, SessionInfo)> = Vec::new();
    for file in transcript_files(projects_dir) {
        if let Some((cwd, info)) = read_session(&file) {
            // Sessions that ran inside a program's own folders (application
            // data, Claude's settings) are not the person's work.
            if !cwd.is_empty() && source_system_place(home, Path::new(&cwd)).is_some() {
                continue;
            }
            all.push((cwd, info));
        }
    }
    let sessions_found = all.len();
    // Not in order first (the ones to put in order), newest first inside each
    // half, then keep the cap: no unordered session is left out while an
    // ordered one is listed.
    let mut keyed: Vec<(bool, String, SessionInfo)> = all.into_iter().map(|(cwd, info)| (classify(home, Path::new(&cwd)).0, cwd, info)).collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.2.last.cmp(&a.2.last)).then_with(|| a.2.session_id.cmp(&b.2.session_id)));
    let dropped = keyed.split_off(keyed.len().min(MAX_SESSIONS));
    let sessions_left_out = dropped.len();
    let sessions_left_out_ordered = dropped.iter().filter(|d| d.0).count();
    let sessions_left_out_unordered = sessions_left_out - sessions_left_out_ordered;
    let all: Vec<(String, SessionInfo)> = keyed.into_iter().map(|(_, cwd, info)| (cwd, info)).collect();

    let mut by_folder: BTreeMap<String, FolderInfo> = BTreeMap::new();
    let mut loose: Vec<WrittenFile> = Vec::new();
    // Folders (by key) where some session wrote a file that still exists.
    let mut has_existing: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (cwd, mut info) in all {
        // Only a file that still sits in the app's temporary workspace (marked AT
        // RISK) is something to move out of it.
        if info.files.iter().any(|f| f.exists && f.at_risk) {
            has_existing.insert(norm(Path::new(&cwd)));
        }
        for f in &info.files {
            let parent = Path::new(&f.path).parent().map(Path::to_path_buf).unwrap_or_default();
            // A file that is gone is not loose any more (it was moved or deleted).
            if f.exists && broad_place(home, &parent).is_some_and(|w| w != "drive root") && !loose.iter().any(|l| l.path == f.path) {
                loose.push(f.clone());
            }
        }
        if info.files.len() > MAX_FILES {
            info.files_left_out = info.files.len() - MAX_FILES;
            info.files.truncate(MAX_FILES);
        }
        let key = norm(Path::new(&cwd));
        let entry = by_folder.entry(key).or_insert_with(|| {
            let dir = Path::new(&cwd);
            let (ordered, why) = classify(home, dir);
            FolderInfo { folder: cwd.clone(), folder_exists: dir.is_dir(), ordered, why_not_ordered: why, without_folder: is_app_scratch(dir), nothing_to_move: false, sessions: Vec::new() }
        });
        entry.sessions.push(info);
    }
    for (key, f) in by_folder.iter_mut() {
        f.nothing_to_move = f.without_folder && !has_existing.contains(key);
    }
    let mut folders: Vec<FolderInfo> = by_folder.into_values().collect();
    // Not in order first; inside each half, the busiest folder first.
    folders.sort_by(|a, b| b.without_folder.cmp(&a.without_folder).then(a.ordered.cmp(&b.ordered)).then(b.sessions.len().cmp(&a.sessions.len())).then(a.folder.cmp(&b.folder)));
    let loose_files_left_out = loose.len().saturating_sub(MAX_FILES);
    loose.truncate(MAX_FILES);
    Inventory { sessions_found, sessions_left_out, sessions_left_out_unordered, sessions_left_out_ordered, folders, loose_files: loose, loose_files_left_out, vanished: Vec::new() }
}

/// A folder is in order when it is a git repository or already a THOR project
/// or topic folder; the broad places never are.
fn classify(home: &Path, dir: &Path) -> (bool, Option<String>) {
    if dir.as_os_str().is_empty() {
        return (false, Some("no folder recorded".to_string()));
    }
    if is_app_scratch(dir) {
        return (false, Some("no folder".to_string()));
    }
    if let Some(w) = broad_place(home, dir) {
        return (false, Some(w));
    }
    if crate::project::git_root(dir).is_some() || crate::project::plain_root(dir).is_some() {
        return (true, None);
    }
    (false, Some("no project".to_string()))
}

// ------------------------------------------------------------ the loose-file snapshot

/// Beside the store: the loose files that existed when THOR was installed.
pub const SNAPSHOT_FILE: &str = "loose-snapshot.json";

fn snapshot_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(SNAPSHOT_FILE)
}

/// The most paths the snapshot keeps: a hard cap so the file stays small on a
/// machine with a very long history. Files beyond it are simply not protected.
pub const SNAPSHOT_CAP: usize = 2000;

/// The existing files that matter for the snapshot, read from EVERY session
/// before any listing cap (`MAX_FILES`) cuts anything: loose files in the home
/// folder, Desktop, Documents or Downloads, and files inside the app's
/// temporary workspace (the AT RISK ones).
fn files_worth_a_snapshot(projects_dir: &Path, home: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for file in transcript_files(projects_dir) {
        let Some((cwd, info)) = read_session(&file) else { continue };
        if !cwd.is_empty() && source_system_place(home, Path::new(&cwd)).is_some() {
            continue;
        }
        for f in info.files {
            if !f.exists {
                continue;
            }
            let parent = Path::new(&f.path).parent().map(Path::to_path_buf).unwrap_or_default();
            let loose = broad_place(home, &parent).is_some_and(|w| w != "drive root");
            if (loose || f.at_risk) && !out.contains(&f.path) {
                out.push(f.path);
                if out.len() >= SNAPSHOT_CAP {
                    return out;
                }
            }
        }
    }
    out
}

/// Record the paths (nothing else) of the existing loose files and of the
/// existing files in the app's temporary workspace, right now. Called once when
/// the tidy note is seeded; files already gone then never appear.
pub fn snapshot_loose_now(db: &Path, home: &Path) -> std::io::Result<usize> {
    let paths = files_worth_a_snapshot(&default_projects_dir(home), home);
    std::fs::write(snapshot_path(db), serde_json::to_string(&paths).unwrap_or_default())?;
    Ok(paths.len())
}

/// The snapshot, `None` for a store seeded before it existed (2.6.11 or older).
pub fn read_snapshot(db: &Path) -> Option<Vec<String>> {
    std::fs::read_to_string(snapshot_path(db)).ok().and_then(|t| serde_json::from_str(&t).ok())
}

/// Every path a tidy run moved away from (the `from` of every move in every
/// undo log beside the store, undone ones included), in comparable form.
fn moved_by_a_tidy(db: &Path) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Ok(rd) = std::fs::read_dir(db.parent().unwrap_or_else(|| Path::new("."))) else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !(name.starts_with("tidy-") && name.ends_with(".json")) {
            continue;
        }
        let Some(v) = std::fs::read_to_string(e.path()).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) else { continue };
        for m in v.get("moves").and_then(|m| m.as_array()).into_iter().flatten() {
            if let Some(from) = m.get("from").and_then(|f| f.as_str()) {
                out.insert(norm(Path::new(from)));
            }
        }
    }
    out
}

/// Fill `inv.vanished` from the snapshot: a loose file of the snapshot that is
/// gone and that no tidy moved. A store with no snapshot keeps today's behaviour.
pub fn apply_snapshot(inv: &mut Inventory, db: &Path) {
    let Some(snapshot) = read_snapshot(db) else { return };
    let moved = moved_by_a_tidy(db);
    inv.vanished = snapshot.into_iter().filter(|p| !Path::new(p).exists() && !moved.contains(&norm(Path::new(p)))).collect();
    // A conversation without a folder whose temporary file vanished behind the
    // tidy step's back is not "nothing to move": it stays not in order.
    let gone: std::collections::HashSet<String> = inv.vanished.iter().map(|p| norm(Path::new(p))).collect();
    for f in inv.folders.iter_mut().filter(|f| f.without_folder) {
        if f.sessions.iter().any(|s| s.files.iter().any(|w| gone.contains(&norm(Path::new(&w.path))))) {
            f.nothing_to_move = false;
        }
    }
}

impl Inventory {
    /// Whether there is nothing to put in order: every folder is a project or a
    /// subject folder already (or left no file behind that still exists), no file
    /// lies loose, and none that was loose has vanished.
    pub fn nothing_to_order(&self) -> bool {
        self.folders.iter().all(|f| f.ordered || f.nothing_to_move) && self.loose_files.is_empty() && self.vanished.is_empty()
    }
}

/// The plain line that says there is nothing to order. The first-session text
/// tells the assistant to stay silent about the whole step when it reads it.
pub const NOTHING_TO_ORDER_LINE: &str = "Nothing to order: every earlier session ran in a folder that is already in order, and no file lies loose.";

pub fn render_text(inv: &Inventory) -> String {
    let mut out = String::new();
    let not_ordered = inv.folders.iter().filter(|f| !f.ordered && !f.nothing_to_move).count();
    out.push_str("Earlier Claude Code sessions on this computer (read-only look, nothing was changed)\n");
    out.push_str(&format!(
        "Sessions with something typed by the person: {}, listed: {} ({} left out: {} not in order, {} in order). Folders: {} ({} not in order yet).\n",
        inv.sessions_found,
        inv.sessions_found - inv.sessions_left_out,
        inv.sessions_left_out,
        inv.sessions_left_out_unordered,
        inv.sessions_left_out_ordered,
        inv.folders.len(),
        not_ordered
    ));
    if inv.folders.is_empty() {
        out.push_str("Nothing to order: no earlier sessions found.\n");
        return out;
    }
    if inv.nothing_to_order() {
        out.push_str(NOTHING_TO_ORDER_LINE);
        out.push('\n');
    }
    let file_line = |w: &WrittenFile| {
        format!("{}{} {}", if w.exists { "exists" } else { "gone  " }, if w.at_risk { " AT RISK" } else { "" }, w.path)
    };
    if inv.folders.iter().any(|f| f.without_folder) {
        out.push_str("\nCONVERSATIONS WITHOUT A FOLDER (the Claude desktop app runs these in a temporary scratch workspace it may clean up; files marked AT RISK sit there, so propose them first)\n");
        for f in inv.folders.iter().filter(|f| f.without_folder) {
            for s in &f.sessions {
                out.push_str(&format!("  session {}  {} .. {}  {} prompt(s)  [workspace {}]\n", s.session_id, s.first, s.last, s.prompts, f.folder));
                if f.nothing_to_move {
                    out.push_str("    NOTHING TO MOVE: this conversation left no file behind that still exists, so it does not stand in the way; you can still store what it taught\n");
                }
                out.push_str(&format!("    first prompt: {}\n", s.first_prompt));
                out.push_str(&format!("    transcript: {}\n", s.file));
                if !s.files.is_empty() {
                    out.push_str("    wrote:\n");
                    for w in &s.files {
                        out.push_str(&format!("      {}\n", file_line(w)));
                    }
                    if s.files_left_out > 0 {
                        out.push_str(&format!("      ... and {} more file(s) left out\n", s.files_left_out));
                    }
                }
            }
        }
    }
    for f in inv.folders.iter().filter(|f| !f.without_folder) {
        let state = match (&f.why_not_ordered, f.folder_exists) {
            (None, true) => "in order (a git repository or a THOR project)".to_string(),
            (None, false) => "in order, but the folder is gone".to_string(),
            (Some(w), true) => format!("NOT in order: {w}"),
            (Some(w), false) => format!("NOT in order: {w}; the folder is gone"),
        };
        out.push_str(&format!("\nFOLDER {}  [{}]\n", f.folder, state));
        for s in &f.sessions {
            out.push_str(&format!("  session {}  {} .. {}  {} prompt(s)\n", s.session_id, s.first, s.last, s.prompts));
            out.push_str(&format!("    first prompt: {}\n", s.first_prompt));
            out.push_str(&format!("    transcript: {}\n", s.file));
            if !s.files.is_empty() {
                out.push_str("    wrote:\n");
                for w in &s.files {
                    out.push_str(&format!("      {}\n", file_line(w)));
                }
                if s.files_left_out > 0 {
                    out.push_str(&format!("      ... and {} more file(s) left out\n", s.files_left_out));
                }
            }
        }
    }
    if !inv.vanished.is_empty() {
        out.push_str("\nNO LONGER THERE (these files were loose when THOR was installed, are gone now, and no tidy-up moved them; the proposal and the person's answer are still the way to settle the step)\n");
        for p in &inv.vanished {
            out.push_str(&format!("  gone   {p}\n"));
        }
    }
    if !inv.loose_files.is_empty() {
        out.push_str("\nLOOSE FILES that sessions wrote straight into the home folder, Desktop, Documents or Downloads\n");
        for w in &inv.loose_files {
            out.push_str(&format!("  {}\n", file_line(w)));
        }
        if inv.loose_files_left_out > 0 {
            out.push_str(&format!("  ... and {} more left out\n", inv.loose_files_left_out));
        }
    }
    out
}

// ------------------------------------------------------------------ digest

/// One session as plain text that is cheap to read: what the person typed,
/// the assistant's last reply to each prompt, and one line per tool call.
/// Never a secret, and cut at about `DIGEST_CHARS`.
pub fn digest(file: &Path) -> Result<String, String> {
    let content = std::fs::read_to_string(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let mut lines: Vec<String> = Vec::new();
    let mut pending: Option<String> = None;
    let mut cwd = String::new();
    let flush = |pending: &mut Option<String>, lines: &mut Vec<String>| {
        if let Some(r) = pending.take() {
            lines.push(format!("CLAUDE: {}", clip(&r, DIGEST_ITEM_CHARS)));
        }
    };
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if cwd.is_empty() {
            cwd = cwd_and_ts(&v).0.unwrap_or_default();
        }
        for event in events_of_line(&v) {
            match event {
                Event::Prompt { text, ts } => {
                    flush(&mut pending, &mut lines);
                    let day = ts.get(..10).unwrap_or("");
                    lines.push(format!("PERSON ({day}): {}", clip(&text, DIGEST_ITEM_CHARS)));
                }
                Event::Reply { text } => pending = Some(text),
                Event::Tool { name, detail, .. } => {
                    let d = clip(&detail, 120);
                    lines.push(if d.is_empty() { format!("  tool: {name}") } else { format!("  tool: {name} {d}") });
                }
            }
        }
    }
    flush(&mut pending, &mut lines);
    let mut out = format!("Session {} (folder {})\n", file.file_stem().and_then(|s| s.to_str()).unwrap_or(""), cwd);
    let total = lines.len();
    for (i, l) in lines.iter().enumerate() {
        if out.chars().count() + l.chars().count() > DIGEST_CHARS {
            out.push_str(&format!("... cut here: {} more line(s) left out\n", total - i));
            break;
        }
        out.push_str(l);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(ordered: bool) -> FolderInfo {
        FolderInfo {
            folder: "C:/Users/ann/Documents/Tuinhuis".to_string(),
            folder_exists: true,
            ordered,
            why_not_ordered: (!ordered).then(|| "no project".to_string()),
            without_folder: false,
            nothing_to_move: false,
            sessions: vec![],
        }
    }

    fn inventory_of(folders: Vec<FolderInfo>) -> Inventory {
        Inventory {
            sessions_found: folders.len(),
            sessions_left_out: 0,
            sessions_left_out_unordered: 0,
            sessions_left_out_ordered: 0,
            folders,
            loose_files: vec![],
            loose_files_left_out: 0,
            vanished: vec![],
        }
    }

    /// The first-session text tells the assistant to stay silent when the listing says
    /// there is nothing to order, so the listing must say it in so many words.
    #[test]
    fn a_listing_with_every_folder_in_order_says_nothing_to_order() {
        let text = render_text(&inventory_of(vec![folder(true)]));
        assert!(text.contains(NOTHING_TO_ORDER_LINE), "{text}");
        let text = render_text(&inventory_of(vec![folder(true), folder(false)]));
        assert!(!text.contains("Nothing to order"), "a folder that is not in order is something to order: {text}");
        let mut loose = inventory_of(vec![folder(true)]);
        loose.vanished.push("C:/Users/ann/Desktop/cv.md".to_string());
        assert!(!render_text(&loose).contains("Nothing to order"));
        assert!(render_text(&inventory_of(vec![])).contains("Nothing to order: no earlier sessions found."));
    }

    #[test]
    fn a_claude_scratchpad_under_temp_is_at_risk_and_other_temp_files_are_not() {
        let pad = Path::new("C:/Users/ann/AppData/Local/Temp/claude/C--Users-ann-x/s1/scratchpad/cv.md");
        assert!(is_temp_scratchpad(pad) && is_at_risk(pad));
        assert!(!is_at_risk(Path::new("C:/Users/ann/AppData/Local/Temp/installer.log")));
        assert!(!is_at_risk(Path::new("C:/Users/ann/Documents/cv.md")));
        let home = Path::new("C:/Users/ann");
        assert_eq!(file_source_system_place(home, pad), None, "a file in a scratchpad may be moved out");
        assert!(file_source_system_place(home, Path::new("C:/Users/ann/AppData/Local/Temp/installer.log")).is_some());
    }

    #[test]
    fn broad_places_are_named() {
        let home = Path::new("C:/Users/ann");
        assert_eq!(broad_place(home, Path::new("C:/Users/ann")).as_deref(), Some("home folder"));
        assert_eq!(broad_place(home, Path::new("C:/Users/Ann/Desktop")).as_deref(), Some("Desktop"));
        assert_eq!(broad_place(home, Path::new("C:/Users/ann/Documenten")).as_deref(), Some("Documents"));
        assert_eq!(broad_place(home, Path::new("C:/Users/ann/OneDrive/Documents")).as_deref(), Some("Documents"));
        assert_eq!(broad_place(home, Path::new("C:/")).as_deref(), Some("drive root"));
        assert_eq!(broad_place(home, Path::new("C:/Users/ann/Documents/Cv en werk zoeken")), None);
    }

    // ------------------------------------------------ case: identity vs recognising names

    /// The two behaviours of `norm`, on every system: `false` is what Linux and
    /// macOS do, `true` is what Windows does.
    #[test]
    fn identity_folds_case_on_windows_and_for_drive_letters_only() {
        let n = |s: &str, fold: bool| norm_with(Path::new(s), fold);
        assert_eq!(n("/tmp/.tmpAbC/Docs", false), "/tmp/.tmpAbC/Docs", "Linux: case is kept");
        assert_eq!(n("/tmp/.tmpAbC/Docs", true), "/tmp/.tmpabc/docs", "Windows: case is folded");
        assert_eq!(n("C:/Users/Ann/x", false), "c:/users/ann/x", "a drive letter path folds even on Linux");
        assert_eq!(n("c:\\Users\\ANN", false), "c:/users/ann");
        assert_eq!(n("/a/B/../c/./d/", false), "/a/c/d");
    }

    #[test]
    fn a_home_folder_with_capitals_in_its_name_is_still_the_home_folder_on_linux() {
        // A tempdir-like name with capitals, as the CI machines make them.
        let home = Path::new("/tmp/.tmpjloTEP");
        for fold in [false, true] {
            assert_eq!(broad_place_with(home, home, fold).as_deref(), Some("home folder"), "fold {fold}");
            assert_eq!(broad_place_with(home, &home.join("Desktop"), fold).as_deref(), Some("Desktop"), "fold {fold}");
            assert!(is_inside_with(home, &home.join("Documents").join("Cv"), fold), "fold {fold}");
            assert!(!is_inside_with(home, home, fold));
        }
    }

    #[test]
    fn folder_names_are_recognised_in_any_case_on_every_system() {
        let home = Path::new("/tmp/.tmpAbC");
        for fold in [false, true] {
            for (dir, want) in [
                ("/tmp/.tmpAbC/DESKTOP", "Desktop"),
                ("/tmp/.tmpAbC/documenten", "Documents"),
                ("/tmp/.tmpAbC/DownLoads", "Downloads"),
                ("/tmp/.tmpAbC/OneDrive - Contoso/Documents", "Documents"),
                ("/tmp/.tmpAbC/ONEDRIVE/desktop", "Desktop"),
            ] {
                assert_eq!(broad_place_with(home, Path::new(dir), fold).as_deref(), Some(want), "{dir} fold {fold}");
            }
            assert_eq!(broad_place_with(home, Path::new("/tmp/.tmpAbC/Documents/Cv"), fold), None);
            // Program folders below the home folder, in the case Windows writes them.
            for dir in ["/tmp/.tmpAbC/AppData/Local/Thing", "/tmp/.tmpAbC/Program Files/Thing", "/tmp/.tmpAbC/Program Files (x86)/T", "/tmp/.tmpAbC/.CLAUDE/projects"] {
                assert!(system_place_with(home, Path::new(dir), fold).is_some(), "{dir} fold {fold}");
            }
            assert_eq!(system_place_with(home, Path::new("/tmp/.tmpAbC/Documents/Cv"), fold), None);
        }
    }

    #[test]
    fn a_temp_scratchpad_and_a_scratch_workspace_are_recognised_in_any_case() {
        for pad in [
            "/home/x/AppData/Local/Temp/claude/C--Users-ann/s1/scratchpad/cv.md",
            "/home/x/APPDATA/LOCAL/TEMP/Claude/s/SCRATCHPAD/cv.md",
            "C:/Users/ann/AppData/Local/Temp/claude/x/scratchpad/cv.md",
        ] {
            assert!(is_temp_scratchpad(Path::new(pad)) && is_at_risk(Path::new(pad)), "{pad}");
        }
        assert!(!is_at_risk(Path::new("/home/x/AppData/Local/Temp/installer.log")));
        for ws in [
            "/home/x/AppData/Roaming/Claude/local-agent-mode-sessions/scratch-workspaces/abc",
            "/home/x/APPDATA/Roaming/CLAUDE-Data/claude-1/Scratch-Workspaces/abc",
        ] {
            assert!(is_app_scratch(Path::new(ws)) && is_at_risk(Path::new(ws)), "{ws}");
        }
        assert!(!is_app_scratch(Path::new("/home/x/Documents/scratch-workspaces/abc")));
    }

    /// Identity: a sibling that differs only in case is another folder on Linux
    /// and the same folder on Windows; a drive-letter path is case-blind anywhere.
    #[test]
    fn a_sibling_that_differs_only_in_case_is_another_folder_on_linux_and_the_same_on_windows() {
        let home = Path::new("/tmp/.tmpAbC");
        let sibling = Path::new("/tmp/.tmpabc/Desktop");
        assert_eq!(broad_place_with(home, sibling, false), None, "Linux: a different folder");
        assert_eq!(broad_place_with(home, sibling, true).as_deref(), Some("Desktop"), "Windows: the same folder");
        assert!(!is_inside_with(home, Path::new("/tmp/.tmpabc/x"), false));
        assert!(is_inside_with(home, Path::new("/tmp/.tmpabc/x"), true));
        // Drive letters fold on any system.
        let drive_home = Path::new("C:/Users/ann");
        assert_eq!(broad_place_with(drive_home, Path::new("C:/Users/Ann/Desktop"), false).as_deref(), Some("Desktop"));
        assert!(is_inside_with(drive_home, Path::new("c:/USERS/ann/x"), false));
    }

    #[test]
    fn secrets_are_blanked() {
        let token = format!("{}{}", "ghp_", "a".repeat(30));
        let out = redact(&format!("use {token} now"));
        assert!(!out.contains(&token));
        assert!(out.contains("[secret removed]"));
    }
}
