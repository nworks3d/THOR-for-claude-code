//! Two folders the owner may name, and what THOR does with them. Nothing here
//! is on until the owner names a folder, and nothing here ever deletes, moves
//! or writes inside either folder: THOR only reads them and says what it sees.
//!
//! 1. A TEMP-UNTIL-LIVE folder: one sub-folder per production, named
//!    `<YYYY-MM-DD>_<repo>_<subject>`, each with a `STATUS.txt` whose first
//!    line starts with `STATUS:`. A production whose first line starts with
//!    `STATUS: live` is finished and may be deleted by the owner. A production
//!    older than `temp_max_age_days` that is not live is also named. A session
//!    start says so at most once a day (`notices::due`).
//! 2. A TOOLS folder: one sub-folder per reusable helper tool, each with a
//!    `README.txt` (or `README.md`) whose first non-empty line says in one
//!    sentence what the tool does. Every session start hands the assistant
//!    that list, built from the folders themselves, so nobody maintains it
//!    and a helper that already exists is found before a new one is written.
//!    A tool folder with no README, a README with no first line, and loose
//!    script files directly in the tools folder are named once a day.
//!
//! The settings are one small file beside the store, `work-folders.json`, in
//! the pattern of every other setting there: a missing or malformed file
//! means silence.
//!
//! NETWORK SAFETY. Both folders may sit on a network share that is switched
//! off or unreachable, and a read of such a share can block for half a minute.
//! A session start must never wait for that, so every read of either folder
//! runs on a worker thread that is given `HOOK_READ_LIMIT` in total. On
//! timeout or error the temp notice is silent and the tools overview falls
//! back to the copy kept beside the store (marked with its date), or is
//! silent. The worker is simply left behind: the hook process ends right
//! after, and the operating system ends the thread with it.
//!
//! I/O and judgement are kept apart: `read_*` functions touch the disk and are
//! passed in as plain function pointers (`Readers`), everything else is pure
//! over what they return, so the slow-share path is tested with a reader that
//! sleeps.

use crate::{notices, store_dir, time};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

// ----------------------------------------------------------------- settings

/// The settings file beside the store.
pub const FILE: &str = "work-folders.json";
/// The tools overview, built once and kept until the folder changes.
pub const CACHE_FILE: &str = "tools-overview-cache.json";
/// How old a production may get before it is named, unless the file says other.
pub const DEFAULT_MAX_AGE_DAYS: i64 = 30;
/// All reads of both folders together, at a session start.
pub const HOOK_READ_LIMIT: Duration = Duration::from_millis(1500);
/// The same for `doctor`, which can afford to wait a little longer.
pub const DOCTOR_READ_LIMIT: Duration = Duration::from_secs(4);
/// The tools overview stays about this long, in bytes.
pub const OVERVIEW_BYTES: usize = 2000;
/// One tool's line is cut to about this many characters of its README line.
const SUMMARY_CHARS: usize = 100;
/// A name in a list is cut to this many characters.
const NAME_CHARS: usize = 60;
/// A list names at most this many folders, then says how many more.
const SHOWN_NAMES: usize = 4;
/// The notices are not about one project, so they share one rate-limit key.
const NOTICE_PROJECT: &str = "";
const TEMP_KIND: &str = "temp-until-live";
const TOOLS_CHECK_KIND: &str = "tools-check";
/// The file extensions that make a file a script (also used to find leftovers).
pub const SCRIPT_EXTENSIONS: [&str; 11] = ["py", "ps1", "psm1", "mjs", "cjs", "js", "ts", "sh", "bat", "cmd", "vbs"];
/// The README names a tool folder may carry, in the order they are tried.
const README_NAMES: [&str; 2] = ["README.txt", "README.md"];
const STATUS_FILE: &str = "STATUS.txt";
/// How much of a file is read to find its first line.
const FIRST_LINE_BYTES: u64 = 4096;

/// The one sentence that closes the tools overview.
pub const OVERVIEW_ADVICE: &str = "Look here before writing a new helper script; a new reusable helper gets its own folder here with a README whose first line says what it does.";

pub fn path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(FILE)
}

fn cache_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(CACHE_FILE)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub temp_until_live: Option<PathBuf>,
    pub temp_max_age_days: i64,
    pub tools: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { temp_until_live: None, temp_max_age_days: DEFAULT_MAX_AGE_DAYS, tools: None }
    }
}

impl Settings {
    /// Nothing is named: every part of this module stays silent.
    pub fn is_empty(&self) -> bool {
        self.temp_until_live.is_none() && self.tools.is_none()
    }
}

/// The file as an object. A missing file is an empty one; a file that is not
/// JSON, or not an object, is an error (the hooks ignore it, `install` refuses
/// to overwrite what a person may have edited by hand).
fn read_object_strict(db: &Path) -> std::io::Result<Map<String, Value>> {
    let text = match std::fs::read_to_string(path(db)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(e),
    };
    let invalid = |why: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{} {why}", path(db).display()));
    match serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(invalid("is not a JSON object")),
        Err(e) => Err(invalid(&format!("is not valid JSON ({e})"))),
    }
}

fn folder_from(object: &Map<String, Value>, key: &str) -> Option<PathBuf> {
    let text = object.get(key)?.as_str()?.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

/// What the file names. Anything missing, malformed or out of range reads as
/// "not set".
pub fn settings(db: &Path) -> Settings {
    let object = read_object_strict(db).unwrap_or_default();
    let max_age = object.get("temp_max_age_days").and_then(Value::as_i64).filter(|days| *days > 0);
    Settings {
        temp_until_live: folder_from(&object, "temp_until_live"),
        temp_max_age_days: max_age.unwrap_or(DEFAULT_MAX_AGE_DAYS),
        tools: folder_from(&object, "tools"),
    }
}

fn update(db: &Path, change: impl FnOnce(&mut Map<String, Value>)) -> std::io::Result<()> {
    let mut object = read_object_strict(db)?;
    change(&mut object);
    let text = serde_json::to_string_pretty(&Value::Object(object)).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(path(db), format!("{text}\n"))
}

/// Name the temp-until-live folder, or take it away (`None`). Naming it also
/// writes the age limit into the file, so the knob is visible to somebody who
/// opens the file.
pub fn set_temp_folder(db: &Path, folder: Option<&str>) -> std::io::Result<()> {
    update(db, |object| match folder {
        Some(folder) => {
            object.insert("temp_until_live".to_string(), Value::from(folder));
            object.entry("temp_max_age_days").or_insert_with(|| Value::from(DEFAULT_MAX_AGE_DAYS));
        }
        None => {
            object.remove("temp_until_live");
        }
    })
}

/// Name the tools folder, or take it away (`None`).
pub fn set_tools_folder(db: &Path, folder: Option<&str>) -> std::io::Result<()> {
    update(db, |object| match folder {
        Some(folder) => {
            object.insert("tools".to_string(), Value::from(folder));
        }
        None => {
            object.remove("tools");
        }
    })
}

/// How long `check_folder` waits for a folder to answer.
const CHECK_LIMIT: Duration = Duration::from_secs(5);

/// A folder the owner names, checked before it is written down: it must be a
/// full path and an existing folder that answers. Returns the path to store
/// (trimmed, without surrounding quotes) or a plain sentence saying what to do.
pub fn check_folder(value: &str) -> Result<String, String> {
    let clean = value.trim().trim_matches('"').trim().to_string();
    if clean.is_empty() {
        return Err("no folder was given.".to_string());
    }
    let folder = PathBuf::from(&clean);
    if !folder.is_absolute() {
        let advice = if cfg!(windows) {
            r"Give the whole path, starting with a drive letter (for example C:\Work\tools) or, for a network folder, with two backslashes and the server name (\\server\share\tools)."
        } else {
            "Give the whole path, starting with a slash (for example /home/you/tools)."
        };
        return Err(format!("'{clean}' is not a full path. {advice}"));
    }
    let probe = folder.clone();
    match within(CHECK_LIMIT, move || probe.is_dir()) {
        Some(true) => Ok(clean),
        Some(false) => Err(format!(
            "there is no folder at {clean}. Check the spelling and, for a network folder, that the share is switched on and connected."
        )),
        None => Err(format!(
            "{clean} did not answer within {}. If it is a network folder, check that the share is switched on and connected, then run this again.",
            seconds(CHECK_LIMIT)
        )),
    }
}

// ------------------------------------------------------ the time limit

/// A read running on its own thread, waited for until a deadline.
pub struct Job<T> {
    rx: mpsc::Receiver<T>,
}

/// Start `work` on a worker thread. If no thread can be started the job simply
/// never answers, which reads as a timeout.
pub fn spawn<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Job<T> {
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new().name("work-folders".to_string()).spawn(move || {
        let _ = tx.send(work());
    });
    Job { rx }
}

impl<T> Job<T> {
    /// The answer, or `None` when it did not come by `deadline` (or the worker
    /// panicked).
    pub fn wait(self, deadline: Instant) -> Option<T> {
        self.rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok()
    }
}

/// `work`, if it finishes within `limit`.
pub fn within<T: Send + 'static>(limit: Duration, work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    spawn(work).wait(Instant::now() + limit)
}

// --------------------------------------------------------- reading text

/// The text of the first bytes of a file the way an editor wrote it: UTF-8
/// with or without a byte-order mark, or UTF-16 with one (what a PowerShell
/// redirect writes).
fn decode_text(bytes: &[u8]) -> String {
    let utf16 = |rest: &[u8], to_unit: fn([u8; 2]) -> u16| {
        // A trailing odd byte, from a read cut in the middle of a letter, is dropped.
        let units: Vec<u16> = rest.chunks(2).filter(|pair| pair.len() == 2).map(|pair| to_unit([pair[0], pair[1]])).collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, u16::from_be_bytes);
    }
    String::from_utf8_lossy(bytes).trim_start_matches('\u{feff}').to_string()
}

/// The first non-empty line of a file, trimmed; `None` when the file cannot be
/// read or has no such line.
fn first_line_of(file: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(file).ok()?.take(FIRST_LINE_BYTES).read_to_end(&mut bytes).ok()?;
    decode_text(&bytes).lines().map(str::trim).find(|line| !line.is_empty()).map(str::to_string)
}

fn entry_is_dir(entry: &std::fs::DirEntry) -> bool {
    match entry.file_type() {
        Ok(kind) if kind.is_dir() => true,
        // A junction or a link to a folder is a folder.
        Ok(kind) if kind.is_symlink() => entry.path().is_dir(),
        _ => false,
    }
}

fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.') || name == "__pycache__"
}

fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(3)).collect();
    format!("{cut}...")
}

/// `a, b, c and 2 more`, each name cut to a sensible length.
fn names_list(names: &[String], shown: usize) -> String {
    let mut parts: Vec<String> = names.iter().take(shown).map(|name| shorten(name, NAME_CHARS)).collect();
    if names.len() > shown {
        parts.push(format!("and {} more", names.len() - shown));
    }
    parts.join(", ")
}

// ------------------------------------------------- temp-until-live folder

/// One production folder as read from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempEntry {
    pub name: String,
    /// The first non-empty line of its `STATUS.txt`; `None` when there is none.
    pub status_line: Option<String>,
}

/// Read the folder: its sub-folders and the first line of each `STATUS.txt`.
/// Files directly in the folder are not productions and are ignored.
pub fn read_temp_folder(root: &Path) -> std::io::Result<Vec<TempEntry>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if is_hidden_name(&name) || !entry_is_dir(&entry) {
            continue;
        }
        let status_line = first_line_of(&entry.path().join(STATUS_FILE));
        out.push(TempEntry { name, status_line });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Whether a status line says the production is live, and so may be deleted:
/// `STATUS: live`, in any letter case, followed by the end of the line or a
/// character that is not part of a word (`STATUS: live (site online)` counts,
/// `STATUS: lives`, `STATUS: not live` do not).
pub fn is_live(status_line: &str) -> bool {
    let lower = status_line.trim().to_lowercase();
    let Some(rest) = lower.strip_prefix("status:") else { return false };
    let Some(after) = rest.trim_start().strip_prefix("live") else { return false };
    !after.chars().next().is_some_and(char::is_alphanumeric)
}

/// The unix time of the date a production's name starts with
/// (`2026-09-03_shop_banner`), or `None` when the name has no such prefix.
pub fn date_prefix_unix(name: &str) -> Option<i64> {
    let date = name.get(..10)?;
    let rest = name.get(10..)?;
    if !(rest.is_empty() || rest.starts_with('_')) {
        return None;
    }
    let shape_ok = date.char_indices().all(|(i, c)| if i == 4 || i == 7 { c == '-' } else { c.is_ascii_digit() });
    if !shape_ok {
        return None;
    }
    time::unix_from_iso8601(&format!("{date}T00:00:00Z"))
}

/// What the temp-until-live folder holds, in numbers and names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TempReport {
    pub total: usize,
    pub max_age_days: i64,
    /// Productions whose status says live: they may be deleted.
    pub live: Vec<String>,
    /// Productions older than the limit that are not live.
    pub old: Vec<String>,
}

/// Judge the folder's entries at `now`. A production with no date in its name
/// has no age and is never called old.
pub fn judge_temp(entries: &[TempEntry], now: i64, max_age_days: i64) -> TempReport {
    let mut report = TempReport { total: entries.len(), max_age_days, ..Default::default() };
    for entry in entries {
        if entry.status_line.as_deref().is_some_and(is_live) {
            report.live.push(entry.name.clone());
            continue;
        }
        let too_old = date_prefix_unix(&entry.name).is_some_and(|made| now - made > max_age_days * 86_400);
        if too_old {
            report.old.push(entry.name.clone());
        }
    }
    report
}

impl TempReport {
    pub fn is_quiet(&self) -> bool {
        self.live.is_empty() && self.old.is_empty()
    }

    fn parts(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if !self.live.is_empty() {
            parts.push(format!(
                "{} production folder(s) say they may be deleted ({})",
                self.live.len(),
                names_list(&self.live, SHOWN_NAMES)
            ));
        }
        if !self.old.is_empty() {
            parts.push(format!(
                "{} folder(s) are older than {} days and not marked live ({})",
                self.old.len(),
                self.max_age_days,
                names_list(&self.old, SHOWN_NAMES)
            ));
        }
        parts
    }

    /// The session-start line, or `None` when there is nothing to say.
    pub fn session_line(&self) -> Option<String> {
        (!self.is_quiet()).then(|| {
            format!(
                "temp-until-live: {}. THOR deletes nothing: tell the owner, and delete a folder only when he agrees.",
                self.parts().join(", ")
            )
        })
    }

    fn doctor_detail(&self, root: &str) -> String {
        let said = if self.is_quiet() { "nothing to clean up".to_string() } else { self.parts().join(", ") };
        format!("work folders: temp-until-live {root}: {} production folder(s); {said}", self.total)
    }
}

// ------------------------------------------------------------ tools folder

/// A README as the disk lists it, without its contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadmeMeta {
    pub file: &'static str,
    pub size: u64,
    pub modified_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolListing {
    pub name: String,
    pub readme: Option<ReadmeMeta>,
}

/// The tools folder as a listing: no file is opened but to ask its size.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolsListing {
    pub tools: Vec<ToolListing>,
    /// Script files directly in the tools folder, which belong in a folder of their own.
    pub loose_scripts: Vec<String>,
}

pub fn is_script_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| SCRIPT_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
}

fn readme_meta(dir: &Path) -> Option<ReadmeMeta> {
    README_NAMES.iter().find_map(|file| {
        let meta = std::fs::metadata(dir.join(file)).ok().filter(|m| m.is_file())?;
        let modified_ms = meta
            .modified()
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_millis())
            .unwrap_or(0);
        Some(ReadmeMeta { file, size: meta.len(), modified_ms })
    })
}

pub fn list_tools_folder(root: &Path) -> std::io::Result<ToolsListing> {
    let mut listing = ToolsListing::default();
    for entry in std::fs::read_dir(root)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if is_hidden_name(&name) {
            continue;
        }
        if entry_is_dir(&entry) {
            listing.tools.push(ToolListing { readme: readme_meta(&entry.path()), name });
        } else if is_script_name(&name) {
            listing.loose_scripts.push(name);
        }
    }
    listing.tools.sort_by(|a, b| a.name.cmp(&b.name));
    listing.loose_scripts.sort();
    Ok(listing)
}

/// What changes when the overview has to be built again: the names of the tool
/// folders and loose scripts, and the size and time of every README.
pub fn fingerprint(listing: &ToolsListing) -> String {
    let mut out = String::from("v1");
    for tool in &listing.tools {
        match &tool.readme {
            Some(r) => out.push_str(&format!("|{}:{}:{}:{}", tool.name, r.file, r.size, r.modified_ms)),
            None => out.push_str(&format!("|{}:-", tool.name)),
        }
    }
    for script in &listing.loose_scripts {
        out.push_str(&format!("|file:{script}"));
    }
    out
}

/// What a tool's README says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadmeState {
    Missing,
    /// There is a README but no usable first line.
    NoFirstLine,
    /// The first line, cleaned and cut to length.
    Line(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolEntry {
    pub name: String,
    pub readme: ReadmeState,
}

/// A README's first line as the overview shows it: a markdown heading mark is
/// dropped, spaces are squeezed, the line is cut to length. `None` when nothing
/// is left.
fn clean_summary(raw: &str) -> Option<String> {
    let squeezed = raw.trim_start_matches('#').split_whitespace().collect::<Vec<_>>().join(" ");
    (!squeezed.is_empty()).then(|| shorten(&squeezed, SUMMARY_CHARS))
}

/// Open every README of the listing and read its first line.
pub fn read_summaries(root: &Path, listing: &ToolsListing) -> Vec<ToolEntry> {
    listing
        .tools
        .iter()
        .map(|tool| {
            let readme = match &tool.readme {
                None => ReadmeState::Missing,
                Some(meta) => match first_line_of(&root.join(&tool.name).join(meta.file)).as_deref().and_then(clean_summary) {
                    Some(line) => ReadmeState::Line(line),
                    None => ReadmeState::NoFirstLine,
                },
            };
            ToolEntry { name: tool.name.clone(), readme }
        })
        .collect()
}

/// What is wrong with the tools folder, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsFindings {
    #[serde(default)]
    pub no_readme: Vec<String>,
    #[serde(default)]
    pub empty_readme: Vec<String>,
    #[serde(default)]
    pub loose_scripts: Vec<String>,
}

impl ToolsFindings {
    pub fn is_empty(&self) -> bool {
        self.no_readme.is_empty() && self.empty_readme.is_empty() && self.loose_scripts.is_empty()
    }

    fn parts(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if !self.no_readme.is_empty() {
            parts.push(format!(
                "{} tool folder(s) have no README.txt or README.md ({})",
                self.no_readme.len(),
                names_list(&self.no_readme, SHOWN_NAMES)
            ));
        }
        if !self.empty_readme.is_empty() {
            parts.push(format!(
                "{} README(s) have no first line saying what the tool does ({})",
                self.empty_readme.len(),
                names_list(&self.empty_readme, SHOWN_NAMES)
            ));
        }
        if !self.loose_scripts.is_empty() {
            parts.push(format!(
                "{} loose script file(s) sit directly in the tools folder ({})",
                self.loose_scripts.len(),
                names_list(&self.loose_scripts, SHOWN_NAMES)
            ));
        }
        parts
    }

    /// The session-start line, or `None` when the folder is in order.
    pub fn session_line(&self, root: &str) -> Option<String> {
        (!self.is_empty()).then(|| {
            format!(
                "tools folder check ({root}): {}. To fix it, give each tool its own sub-folder with a README.txt whose first line says in one sentence what the tool does, and move a loose script into such a folder. Tell the owner first; THOR changes nothing itself.",
                self.parts().join("; ")
            )
        })
    }

    fn doctor_detail(&self, root: &str, tool_count: usize) -> String {
        let said = if self.is_empty() { "in order".to_string() } else { self.parts().join("; ") };
        format!("work folders: tools {root}: {tool_count} tool folder(s); {said}")
    }
}

fn more_line(count: usize, root: &str) -> String {
    format!("... and {count} more in {root}")
}

/// The overview text and the findings, from what was read. The overview is
/// kept to about `OVERVIEW_BYTES`: tool lines are added in name order while
/// they fit, then one line says how many more there are.
pub fn assemble(root: &str, tools: &[ToolEntry], loose_scripts: &[String]) -> (String, ToolsFindings) {
    let mut findings = ToolsFindings { loose_scripts: loose_scripts.to_vec(), ..Default::default() };
    let mut lines = Vec::new();
    for tool in tools {
        let summary = match &tool.readme {
            ReadmeState::Line(line) => line.clone(),
            ReadmeState::NoFirstLine => {
                findings.empty_readme.push(tool.name.clone());
                "(README has no first line)".to_string()
            }
            ReadmeState::Missing => {
                findings.no_readme.push(tool.name.clone());
                "(no README)".to_string()
            }
        };
        lines.push(format!("{} - {summary}", tool.name));
    }

    let header = format!("Helper tools available in {root}:");
    let mut used = header.len() + 1 + OVERVIEW_ADVICE.len();
    let mut kept = 0;
    for (i, line) in lines.iter().enumerate() {
        let after = lines.len() - i - 1;
        let reserve = if after > 0 { more_line(after, root).len() + 1 } else { 0 };
        if used + line.len() + 1 + reserve > OVERVIEW_BYTES {
            break;
        }
        used += line.len() + 1;
        kept += 1;
    }
    let mut out = vec![header];
    if lines.is_empty() {
        out.push("none yet".to_string());
    }
    out.extend(lines.iter().take(kept).cloned());
    if kept < lines.len() {
        out.push(more_line(lines.len() - kept, root));
    }
    out.push(OVERVIEW_ADVICE.to_string());
    (out.join("\n"), findings)
}

/// What one read of the tools folder found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolsRead {
    /// The listing has the fingerprint the caller already holds an overview
    /// for: no README was opened.
    Unchanged { fingerprint: String },
    Built { fingerprint: String, tool_count: usize, overview: String, findings: ToolsFindings },
}

/// Read the tools folder. With `known` (the fingerprint of the overview the
/// caller already holds), a folder that has not changed costs a listing and
/// nothing more.
pub fn read_tools(root: &Path, known: Option<&str>) -> std::io::Result<ToolsRead> {
    let listing = list_tools_folder(root)?;
    let fingerprint = fingerprint(&listing);
    if known == Some(fingerprint.as_str()) {
        return Ok(ToolsRead::Unchanged { fingerprint });
    }
    let entries = read_summaries(root, &listing);
    let (overview, findings) = assemble(&root.display().to_string(), &entries, &listing.loose_scripts);
    Ok(ToolsRead::Built { fingerprint, tool_count: entries.len(), overview, findings })
}

// ------------------------------------------------------------------ cache

/// The overview kept beside the store, so a folder that has not changed costs
/// a listing and one that cannot be reached still has an answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsCache {
    /// The tools folder it was built for, as the settings spell it.
    pub tools: String,
    pub fingerprint: String,
    /// When it was built, as a unix time.
    pub built_at: i64,
    pub overview: String,
    #[serde(default)]
    pub findings: ToolsFindings,
}

fn read_cache(db: &Path, tools_label: &str) -> Option<ToolsCache> {
    let text = std::fs::read_to_string(cache_path(db)).ok()?;
    serde_json::from_str::<ToolsCache>(text.trim_start_matches('\u{feff}')).ok().filter(|cache| cache.tools == tools_label)
}

fn write_cache(db: &Path, cache: &ToolsCache) {
    if let Ok(text) = serde_json::to_string(cache) {
        let _ = store_dir::write(&cache_path(db), text);
    }
}

/// The cached overview, said to be old: its first line gets the date it was
/// built and why it is not fresh.
fn mark_as_old(cache: &ToolsCache) -> String {
    let header = format!("Helper tools available in {}:", cache.tools);
    let date = time::iso8601_from_unix(cache.built_at);
    let date = date.get(..10).unwrap_or(&date);
    match cache.overview.strip_prefix(&header) {
        Some(rest) => format!(
            "Helper tools available in {} (list from {date}; the folder could not be reached just now):{rest}",
            cache.tools
        ),
        None => cache.overview.clone(),
    }
}

// ----------------------------------------------------------- the readers

/// The two disk reads, passed in as plain functions so a test can hand in one
/// that sleeps or fails.
#[derive(Clone, Copy)]
pub struct Readers {
    pub temp: fn(&Path) -> std::io::Result<Vec<TempEntry>>,
    pub tools: fn(&Path, Option<&str>) -> std::io::Result<ToolsRead>,
}

/// The real readers.
pub const READERS: Readers = Readers { temp: read_temp_folder, tools: read_tools };

// ---------------------------------------------------------- session start

/// What a session start adds to its block. Every part is `None` when nothing
/// is configured, when there is nothing to say, or when the folder could not
/// be read in time and there is no older answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartLines {
    pub temp_notice: Option<String>,
    pub tools_overview: Option<String>,
    pub tools_check: Option<String>,
}

impl StartLines {
    /// The lines in the order the block carries them.
    pub fn into_lines(self) -> Vec<String> {
        [self.temp_notice, self.tools_check, self.tools_overview].into_iter().flatten().collect()
    }
}

/// The session start's part of this feature. `for_owner` is false for a
/// headless run: it gets the tools overview (an agent can use it) but not the
/// notices, which are for the person and must not use up the day's showing.
/// `record` is false for a measurement run, which leaves no trace.
pub fn session_start_lines(db: &Path, now: i64, for_owner: bool, record: bool, limit: Duration, readers: &Readers) -> StartLines {
    let settings = settings(db);
    let mut lines = StartLines::default();
    if settings.is_empty() {
        return lines;
    }
    let deadline = Instant::now() + limit;

    // Both reads start before either is waited for, so they share one limit.
    // A temp folder that has already been reported today is not even read.
    let temp_job = settings
        .temp_until_live
        .clone()
        .filter(|_| for_owner && notices::due(db, NOTICE_PROJECT, TEMP_KIND, now, false))
        .map(|root| {
            let read = readers.temp;
            spawn(move || read(&root))
        });
    let tools_label = settings.tools.as_ref().map(|tools| tools.display().to_string());
    let cache = tools_label.as_deref().and_then(|label| read_cache(db, label));
    let tools_job = settings.tools.clone().map(|root| {
        let read = readers.tools;
        let known = cache.as_ref().map(|cache| cache.fingerprint.clone());
        spawn(move || read(&root, known.as_deref()))
    });

    let entries = temp_job.and_then(|job| job.wait(deadline)).and_then(|read| read.ok());
    if let Some(entries) = entries {
        let report = judge_temp(&entries, now, settings.temp_max_age_days);
        if !report.is_quiet() && notices::due(db, NOTICE_PROJECT, TEMP_KIND, now, record) {
            lines.temp_notice = report.session_line();
        }
    }

    let (Some(label), Some(job)) = (tools_label, tools_job) else { return lines };
    let findings = match job.wait(deadline).and_then(|read| read.ok()) {
        Some(ToolsRead::Built { fingerprint, overview, findings, .. }) => {
            write_cache(db, &ToolsCache { tools: label.clone(), fingerprint, built_at: now, overview: overview.clone(), findings: findings.clone() });
            lines.tools_overview = Some(overview);
            Some(findings)
        }
        Some(ToolsRead::Unchanged { .. }) => cache.map(|cache| {
            lines.tools_overview = Some(cache.overview);
            cache.findings
        }),
        // Not reached in time, or an error: the older answer, said to be old.
        None => {
            lines.tools_overview = cache.as_ref().map(mark_as_old);
            None
        }
    };
    if let Some(findings) = findings.filter(|f| for_owner && !f.is_empty()) {
        if notices::due(db, NOTICE_PROJECT, TOOLS_CHECK_KIND, now, record) {
            lines.tools_check = findings.session_line(&label);
        }
    }
    lines
}

// ----------------------------------------------------------------- doctor

/// What `doctor` says about the two folders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoctorLines {
    /// Plain sentences for the short version: only what needs a look.
    pub notes: Vec<String>,
    /// One line per configured folder for the detailed report.
    pub detail: Vec<String>,
}

fn seconds(limit: Duration) -> String {
    match limit.as_secs().max(1) {
        1 => "1 second".to_string(),
        n => format!("{n} seconds"),
    }
}

/// Why a folder could not be read, in words for the person.
fn failure_reason(read: Option<std::io::Result<()>>, limit: Duration) -> String {
    match read {
        Some(Err(e)) => format!("could not be read ({e})"),
        _ => format!("did not answer within {}", seconds(limit)),
    }
}

/// A folder that could not be read: the sentence for the short version.
fn unreadable_note(what: &str, root: &str, reason: &str) -> String {
    format!(
        "your {what} folder ({root}) {reason}. Check that the folder is still there and, if it is on a network share, that the share is switched on and connected."
    )
}

/// Read both folders afresh (no cache) and say what is found.
pub fn doctor_lines(db: &Path, now: i64, limit: Duration, readers: &Readers) -> DoctorLines {
    let settings = settings(db);
    let mut out = DoctorLines::default();
    let deadline = Instant::now() + limit;
    let temp = settings.temp_until_live.clone().map(|root| {
        let read = readers.temp;
        let label = root.display().to_string();
        (label, spawn(move || read(&root)))
    });
    let tools = settings.tools.clone().map(|root| {
        let read = readers.tools;
        let label = root.display().to_string();
        (label, spawn(move || read(&root, None)))
    });

    if let Some((label, job)) = temp {
        match job.wait(deadline) {
            Some(Ok(entries)) => {
                let report = judge_temp(&entries, now, settings.temp_max_age_days);
                out.detail.push(report.doctor_detail(&label));
                if !report.is_quiet() {
                    out.notes.push(format!(
                        "in your temp-until-live folder, {}. THOR deletes nothing: look at them and delete what you agree with.",
                        report.parts().join(", ")
                    ));
                }
            }
            failed => {
                let reason = failure_reason(failed.map(|read| read.map(|_| ())), limit);
                out.detail.push(format!("work folders: temp-until-live {label}: {reason}"));
                out.notes.push(unreadable_note("temp-until-live", &label, &reason));
            }
        }
    }
    if let Some((label, job)) = tools {
        match job.wait(deadline) {
            Some(Ok(ToolsRead::Built { tool_count, findings, .. })) => {
                out.detail.push(findings.doctor_detail(&label, tool_count));
                if !findings.is_empty() {
                    out.notes.push(format!(
                        "in your tools folder, {}. Next step: ask your assistant to tidy it, so that each tool has its own folder with a README whose first line says what it does.",
                        findings.parts().join("; ")
                    ));
                }
            }
            failed => {
                let reason = failure_reason(failed.map(|read| read.map(|_| ())), limit);
                out.detail.push(format!("work folders: tools {label}: {reason}"));
                out.notes.push(unreadable_note("tools", &label, &reason));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const DAY: i64 = 86_400;
    /// 2026-10-20 00:00:00 UTC.
    const NOW: i64 = 1_792_454_400;

    fn store(dir: &Path) -> PathBuf {
        let db = dir.join("thor.db");
        std::fs::write(&db, b"").unwrap();
        db
    }

    fn write_settings(db: &Path, json: &str) {
        std::fs::write(path(db), json).unwrap();
    }

    fn make_production(root: &Path, name: &str, status: Option<&str>) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(status) = status {
            std::fs::write(dir.join("STATUS.txt"), status).unwrap();
        }
    }

    fn make_tool(root: &Path, name: &str, readme: Option<(&str, &str)>) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some((file, text)) = readme {
            std::fs::write(dir.join(file), text).unwrap();
        }
    }

    fn json_path(p: &Path) -> String {
        serde_json::to_string(&p.display().to_string()).unwrap()
    }

    // --------------------------------------------------------- settings

    #[test]
    fn with_no_settings_file_nothing_is_set() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        assert!(settings(&db).is_empty());
        assert_eq!(settings(&db).temp_max_age_days, 30);
    }

    #[test]
    fn a_malformed_settings_file_reads_as_nothing_set() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        for bad in ["not json", "[]", "\"x\"", ""] {
            write_settings(&db, bad);
            assert!(settings(&db).is_empty(), "{bad}");
        }
    }

    #[test]
    fn the_settings_are_read_and_a_bad_age_falls_back_to_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        write_settings(&db, r#"{"temp_until_live":" C:\\work\\temp ","temp_max_age_days":14,"tools":"C:\\work\\tools"}"#);
        let s = settings(&db);
        assert_eq!(s.temp_until_live, Some(PathBuf::from(r"C:\work\temp")));
        assert_eq!(s.tools, Some(PathBuf::from(r"C:\work\tools")));
        assert_eq!(s.temp_max_age_days, 14);
        for bad in ["0", "-3", "\"many\"", "null"] {
            write_settings(&db, &format!(r#"{{"tools":"x","temp_max_age_days":{bad}}}"#));
            assert_eq!(settings(&db).temp_max_age_days, 30, "{bad}");
        }
        write_settings(&db, r#"{"tools":"   ","temp_until_live":""}"#);
        assert!(settings(&db).is_empty(), "a blank path is no path");
    }

    #[test]
    fn a_byte_order_mark_does_not_hide_the_settings() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        write_settings(&db, "\u{feff}{\"tools\":\"/srv/tools\"}");
        assert_eq!(settings(&db).tools, Some(PathBuf::from("/srv/tools")));
    }

    #[test]
    fn setting_and_removing_the_folders_round_trips_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        // A UNC path has backslashes that JSON must escape and read back whole.
        let unc = r"\\server\share\temp until live";
        set_temp_folder(&db, Some(unc)).unwrap();
        set_tools_folder(&db, Some(r"\\server\share\tools")).unwrap();
        let s = settings(&db);
        assert_eq!(s.temp_until_live.as_deref(), Some(Path::new(unc)));
        assert_eq!(s.tools.as_deref(), Some(Path::new(r"\\server\share\tools")));
        let text = std::fs::read_to_string(path(&db)).unwrap();
        assert!(text.contains("temp_max_age_days"), "the age knob is written where it can be seen: {text}");
        assert!(!text.contains('\r'), "LF line endings only");

        // A hand-made age limit and an unknown key survive a change.
        write_settings(&db, &format!(r#"{{"temp_until_live":"x","temp_max_age_days":7,"note":"mine","tools":{}}}"#, json_path(Path::new("y"))));
        set_temp_folder(&db, Some("z")).unwrap();
        let s = settings(&db);
        assert_eq!(s.temp_max_age_days, 7);
        assert!(std::fs::read_to_string(path(&db)).unwrap().contains("\"note\""));

        set_temp_folder(&db, None).unwrap();
        set_tools_folder(&db, None).unwrap();
        assert!(settings(&db).is_empty());
        set_tools_folder(&db, None).unwrap();
    }

    #[test]
    fn a_folder_the_owner_names_must_be_a_full_path_to_a_folder_that_answers() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().display().to_string();
        assert_eq!(check_folder(&real).unwrap(), real);
        assert_eq!(check_folder(&format!("  \"{real}\"  ")).unwrap(), real, "quotes and spaces from a paste come off");
        let relative = check_folder("tools").unwrap_err();
        assert!(relative.contains("not a full path") && relative.contains("Give the whole path"), "{relative}");
        let missing = check_folder(&dir.path().join("nope").display().to_string()).unwrap_err();
        assert!(missing.contains("there is no folder at") && missing.contains("network folder"), "{missing}");
        let file = dir.path().join("a-file.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(check_folder(&file.display().to_string()).is_err(), "a file is not a folder");
        assert!(check_folder("   ").is_err());
    }

    #[test]
    fn a_settings_file_that_cannot_be_read_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        write_settings(&db, "{ \"tools\": \"a\", oops");
        assert!(set_tools_folder(&db, Some("b")).is_err());
        assert_eq!(std::fs::read_to_string(path(&db)).unwrap(), "{ \"tools\": \"a\", oops", "what a person typed is kept");
    }

    // ------------------------------------------------------ the status

    #[test]
    fn live_is_a_status_line_that_starts_with_status_live() {
        for yes in ["STATUS: live", "status: LIVE", "STATUS:live", "  STATUS: live (site online since 2026-10-01)", "STATUS: live, may be deleted", "STATUS: Live."] {
            assert!(is_live(yes), "{yes}");
        }
        for no in ["STATUS: lives", "STATUS: not live", "STATUS: in progress", "STATUS: liveliness", "live", "", "NOTE: STATUS: live", "STATUS:"] {
            assert!(!is_live(no), "{no}");
        }
    }

    #[test]
    fn text_is_decoded_whatever_an_editor_wrote() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFSTATUS: live"), "STATUS: live");
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("STATUS: live\r\n".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(decode_text(&utf16), "STATUS: live\r\n");
        let be: Vec<u8> = [0xFE, 0xFF].into_iter().chain("STATUS: live".encode_utf16().flat_map(u16::to_be_bytes)).collect();
        assert_eq!(decode_text(&be), "STATUS: live");
        assert_eq!(decode_text(b"plain"), "plain");
    }

    #[test]
    fn the_date_prefix_of_a_name_is_read() {
        let day = |y: i32, m: u32, d: u32| time::unix_from_iso8601(&format!("{y:04}-{m:02}-{d:02}T00:00:00Z")).unwrap();
        assert_eq!(date_prefix_unix("2026-09-03_shop_banner"), Some(day(2026, 9, 3)));
        assert_eq!(date_prefix_unix("2026-09-03"), Some(day(2026, 9, 3)), "a name that is only a date");
        for no in ["shop_banner", "2026-9-3_shop", "2026-13-03_shop", "2026-09-03shop", "x2026-09-03_shop", "2026-09-0_", "", "2026"] {
            assert_eq!(date_prefix_unix(no), None, "{no}");
        }
        assert_eq!(date_prefix_unix("20\u{e9}6-09-03_x"), None, "multi-byte characters never panic");
    }

    #[test]
    fn productions_are_judged_live_old_or_neither() {
        let entry = |name: &str, status: Option<&str>| TempEntry { name: name.to_string(), status_line: status.map(str::to_string) };
        let date = |days_ago: i64| time::iso8601_from_unix(NOW - days_ago * DAY)[..10].to_string();
        let entries = vec![
            entry(&format!("{}_shop_live-one", date(2)), Some("STATUS: live")),
            entry(&format!("{}_shop_live-and-old", date(90)), Some("STATUS: live")),
            entry(&format!("{}_shop_old", date(31)), Some("STATUS: waiting for the client")),
            entry(&format!("{}_shop_exactly-thirty", date(30)), Some("STATUS: waiting")),
            entry(&format!("{}_shop_fresh", date(5)), None),
            entry("no-date-at-all", Some("STATUS: waiting")),
            entry(&format!("{}_shop_old-no-status", date(60)), None),
        ];
        let report = judge_temp(&entries, NOW, 30);
        assert_eq!(report.total, 7);
        assert_eq!(report.live, vec![entries[0].name.clone(), entries[1].name.clone()]);
        assert_eq!(report.old, vec![entries[2].name.clone(), entries[6].name.clone()], "a live production is never also old; exactly 30 days is not older");
        let tight = judge_temp(&entries, NOW, 3);
        assert_eq!(tight.old.len(), 4, "a lower limit names more");
    }

    #[test]
    fn the_temp_line_names_the_folders_and_says_nothing_is_deleted() {
        let report = TempReport {
            total: 9,
            max_age_days: 30,
            live: vec!["2026-09-01_a_one".into(), "2026-09-02_a_two".into()],
            old: vec!["2026-08-01_a_old".into()],
        };
        let line = report.session_line().unwrap();
        assert!(line.starts_with("temp-until-live: 2 production folder(s) say they may be deleted (2026-09-01_a_one, 2026-09-02_a_two)"), "{line}");
        assert!(line.contains("1 folder(s) are older than 30 days and not marked live (2026-08-01_a_old)"), "{line}");
        assert!(line.contains("THOR deletes nothing"), "{line}");
        assert_eq!(TempReport { max_age_days: 30, ..Default::default() }.session_line(), None);

        let many = TempReport { live: (0..12).map(|i| format!("2026-09-{:02}_{}", i + 1, "x".repeat(80))).collect(), max_age_days: 30, ..Default::default() };
        let line = many.session_line().unwrap();
        assert!(line.contains("and 8 more"), "{line}");
        assert!(line.len() < 700, "a long list stays short: {} bytes", line.len());
    }

    #[test]
    fn the_temp_folder_is_read_for_its_sub_folders_only() {
        let dir = tempfile::tempdir().unwrap();
        make_production(dir.path(), "2026-10-01_shop_b", Some("\u{feff}STATUS: live\r\nrest"));
        make_production(dir.path(), "2026-09-01_shop_a", None);
        make_production(dir.path(), ".hidden", Some("STATUS: live"));
        std::fs::write(dir.path().join("2026-08-01_loose_file.txt"), "STATUS: live").unwrap();
        let entries = read_temp_folder(dir.path()).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["2026-09-01_shop_a", "2026-10-01_shop_b"]);
        assert_eq!(entries[0].status_line, None);
        assert_eq!(entries[1].status_line.as_deref(), Some("STATUS: live"));
        assert!(read_temp_folder(&dir.path().join("missing")).is_err());
    }

    // --------------------------------------------------- the tools folder

    #[test]
    fn a_readme_line_is_cleaned_and_cut() {
        assert_eq!(clean_summary("# Resize   images\tin bulk").as_deref(), Some("Resize images in bulk"));
        assert_eq!(clean_summary("###"), None);
        assert_eq!(clean_summary("   "), None);
        let long = "word ".repeat(50);
        let cut = clean_summary(&long).unwrap();
        assert_eq!(cut.chars().count(), 100);
        assert!(cut.ends_with("..."));
    }

    #[test]
    fn script_names_are_recognised_by_extension() {
        for yes in ["a.py", "A.PS1", "x.psm1", "x.mjs", "x.cjs", "x.js", "x.ts", "x.sh", "x.bat", "x.cmd", "x.vbs"] {
            assert!(is_script_name(yes), "{yes}");
        }
        for no in ["README.md", "notes.txt", "py", "x.pyc", ".gitignore", "x.json"] {
            assert!(!is_script_name(no), "{no}");
        }
    }

    fn tools_fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tools");
        std::fs::create_dir_all(&root).unwrap();
        make_tool(&root, "pdf-merge", Some(("README.txt", "\n\nMerge several PDF files into one.\nUsage: ...")));
        make_tool(&root, "img-resize", Some(("README.md", "# Resize every image in a folder to a given width")));
        make_tool(&root, "empty-readme", Some(("README.txt", "\n   \n")));
        make_tool(&root, "no-readme", None);
        make_tool(&root, ".git", None);
        std::fs::write(root.join("quick.py"), "print(1)").unwrap();
        std::fs::write(root.join("README.md"), "about this folder").unwrap();
        (dir, root)
    }

    #[test]
    fn the_overview_has_one_line_per_tool_built_from_the_folders() {
        let (_dir, root) = tools_fixture();
        let ToolsRead::Built { overview, findings, tool_count, .. } = read_tools(&root, None).unwrap() else { panic!("built") };
        let label = root.display().to_string();
        assert_eq!(tool_count, 4);
        let expected = format!(
            "Helper tools available in {label}:\nempty-readme - (README has no first line)\nimg-resize - Resize every image in a folder to a given width\nno-readme - (no README)\npdf-merge - Merge several PDF files into one.\n{OVERVIEW_ADVICE}"
        );
        assert_eq!(overview, expected);
        assert_eq!(findings.no_readme, ["no-readme"]);
        assert_eq!(findings.empty_readme, ["empty-readme"]);
        assert_eq!(findings.loose_scripts, ["quick.py"], "a README beside the tools is not a script");
    }

    #[test]
    fn an_empty_tools_folder_still_teaches_the_convention() {
        let dir = tempfile::tempdir().unwrap();
        let ToolsRead::Built { overview, findings, .. } = read_tools(dir.path(), None).unwrap() else { panic!("built") };
        assert!(overview.contains("none yet"), "{overview}");
        assert!(overview.ends_with(OVERVIEW_ADVICE), "{overview}");
        assert!(findings.is_empty());
    }

    #[test]
    fn a_long_overview_is_capped_and_says_how_many_more() {
        let tools: Vec<ToolEntry> = (0..80)
            .map(|i| ToolEntry { name: format!("tool-{i:02}"), readme: ReadmeState::Line(format!("does thing number {i} with a sentence of moderate length to fill space")) })
            .collect();
        let (overview, _) = assemble("/srv/tools", &tools, &[]);
        assert!(overview.len() <= OVERVIEW_BYTES, "{} bytes", overview.len());
        assert!(overview.len() > OVERVIEW_BYTES - 150, "the room is used: {} bytes", overview.len());
        let more = overview.lines().find(|l| l.starts_with("... and ")).expect("a line saying how many more");
        let n: usize = more.trim_start_matches("... and ").split(' ').next().unwrap().parse().unwrap();
        let shown = overview.lines().filter(|l| l.starts_with("tool-")).count();
        assert_eq!(shown + n, 80, "nothing is lost from the count: {more}");
        assert!(more.ends_with("in /srv/tools"), "{more}");
        assert!(overview.ends_with(OVERVIEW_ADVICE), "the advice is always there");
        assert!(overview.starts_with("Helper tools available in /srv/tools:\ntool-00 - "));

        let (small, _) = assemble("/srv/tools", &tools[..3], &[]);
        assert!(!small.contains("more in"), "{small}");
    }

    #[test]
    fn the_fingerprint_follows_names_and_readmes_and_nothing_else() {
        let (_dir, root) = tools_fixture();
        let first = fingerprint(&list_tools_folder(&root).unwrap());
        assert_eq!(first, fingerprint(&list_tools_folder(&root).unwrap()), "stable while nothing changes");

        std::fs::write(root.join("pdf-merge").join("README.txt"), "Merge PDF files into one single file.").unwrap();
        let edited = fingerprint(&list_tools_folder(&root).unwrap());
        assert_ne!(first, edited, "a README that changed size");

        make_tool(&root, "zip-all", Some(("README.txt", "Zip things.")));
        assert_ne!(edited, fingerprint(&list_tools_folder(&root).unwrap()), "a new tool folder");

        let before = fingerprint(&list_tools_folder(&root).unwrap());
        std::fs::write(root.join("other.sh"), "echo").unwrap();
        assert_ne!(before, fingerprint(&list_tools_folder(&root).unwrap()), "a new loose script");

        let before = fingerprint(&list_tools_folder(&root).unwrap());
        std::fs::write(root.join("pdf-merge").join("notes.txt"), "not a README").unwrap();
        std::fs::write(root.join("notes.txt"), "not a script").unwrap();
        assert_eq!(before, fingerprint(&list_tools_folder(&root).unwrap()), "other files change nothing");
    }

    #[test]
    fn a_folder_that_has_not_changed_is_not_read_again() {
        let (_dir, root) = tools_fixture();
        let ToolsRead::Built { fingerprint: fp, .. } = read_tools(&root, None).unwrap() else { panic!("built") };
        assert_eq!(read_tools(&root, Some(&fp)).unwrap(), ToolsRead::Unchanged { fingerprint: fp.clone() });
        assert!(matches!(read_tools(&root, Some("v1|something-else")).unwrap(), ToolsRead::Built { .. }));
        // README.txt wins over README.md when both exist.
        make_tool(&root, "img-resize", Some(("README.txt", "Resize images.")));
        assert!(matches!(read_tools(&root, Some(&fp)).unwrap(), ToolsRead::Built { .. }));
    }

    // ----------------------------------------------------- session start

    static TEMP_READS: AtomicUsize = AtomicUsize::new(0);

    fn counting_temp(root: &Path) -> std::io::Result<Vec<TempEntry>> {
        TEMP_READS.fetch_add(1, Ordering::SeqCst);
        read_temp_folder(root)
    }

    fn slow_temp(_: &Path) -> std::io::Result<Vec<TempEntry>> {
        std::thread::sleep(Duration::from_secs(3));
        Ok(Vec::new())
    }

    fn slow_tools(_: &Path, _: Option<&str>) -> std::io::Result<ToolsRead> {
        std::thread::sleep(Duration::from_secs(3));
        Err(std::io::Error::other("never answers in time"))
    }

    fn broken_tools(_: &Path, _: Option<&str>) -> std::io::Result<ToolsRead> {
        Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access is denied"))
    }

    const START: fn(&Path, i64, bool, bool, Duration, &Readers) -> StartLines = session_start_lines;

    fn configured(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let db = store(dir);
        let temp = dir.join("temp-until-live");
        let tools = dir.join("tools");
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::create_dir_all(&tools).unwrap();
        write_settings(&db, &format!(r#"{{"temp_until_live":{},"tools":{}}}"#, json_path(&temp), json_path(&tools)));
        (db, temp, tools)
    }

    #[test]
    fn with_no_settings_a_session_start_says_nothing_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        let lines = START(&db, NOW, true, true, HOOK_READ_LIMIT, &READERS);
        assert_eq!(lines, StartLines::default());
        assert!(lines.into_lines().is_empty());
        let mut names: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        names.sort();
        assert_eq!(names, ["thor.db"], "no cache, no notice log");
    }

    #[test]
    fn a_session_start_hands_over_the_tools_overview_every_time() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _temp, tools) = configured(dir.path());
        make_tool(&tools, "pdf-merge", Some(("README.txt", "Merge several PDF files into one.")));
        for run in 0..3 {
            let lines = START(&db, NOW + run * 60, true, true, HOOK_READ_LIMIT, &READERS);
            let overview = lines.tools_overview.unwrap_or_else(|| panic!("run {run}: no overview"));
            assert!(overview.contains("pdf-merge - Merge several PDF files into one."), "run {run}: {overview}");
            assert!(overview.contains(OVERVIEW_ADVICE));
        }
    }

    #[test]
    fn the_overview_is_rebuilt_only_when_the_folder_changed() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _temp, tools) = configured(dir.path());
        make_tool(&tools, "pdf-merge", Some(("README.txt", "Merge several PDF files into one.")));
        START(&db, NOW, true, true, HOOK_READ_LIMIT, &READERS);
        let cache: ToolsCache = serde_json::from_str(&std::fs::read_to_string(cache_path(&db)).unwrap()).unwrap();
        assert_eq!(cache.built_at, NOW);
        assert!(cache.overview.contains("pdf-merge"));

        // Nothing changed: the cache is used and left as it was.
        let again = START(&db, NOW + 600, true, true, HOOK_READ_LIMIT, &READERS);
        assert_eq!(again.tools_overview.as_deref(), Some(cache.overview.as_str()));
        let kept: ToolsCache = serde_json::from_str(&std::fs::read_to_string(cache_path(&db)).unwrap()).unwrap();
        assert_eq!(kept.built_at, NOW, "an unchanged folder does not rewrite the cache");

        // A README changed: built again.
        std::fs::write(tools.join("pdf-merge").join("README.txt"), "Join PDF files.").unwrap();
        let changed = START(&db, NOW + 1200, true, true, HOOK_READ_LIMIT, &READERS);
        assert!(changed.tools_overview.unwrap().contains("pdf-merge - Join PDF files."));
        let rebuilt: ToolsCache = serde_json::from_str(&std::fs::read_to_string(cache_path(&db)).unwrap()).unwrap();
        assert_eq!(rebuilt.built_at, NOW + 1200);
        assert_ne!(rebuilt.fingerprint, cache.fingerprint);
    }

    #[test]
    fn a_cache_for_another_tools_folder_is_not_used() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _temp, tools) = configured(dir.path());
        make_tool(&tools, "a-tool", Some(("README.txt", "Does A.")));
        START(&db, NOW, true, true, HOOK_READ_LIMIT, &READERS);
        let other = dir.path().join("other-tools");
        std::fs::create_dir_all(&other).unwrap();
        make_tool(&other, "b-tool", Some(("README.txt", "Does B.")));
        set_tools_folder(&db, Some(&other.display().to_string())).unwrap();
        let lines = START(&db, NOW + 10, true, true, HOOK_READ_LIMIT, &READERS);
        let overview = lines.tools_overview.unwrap();
        assert!(overview.contains("b-tool") && !overview.contains("a-tool"), "{overview}");
    }

    #[test]
    fn a_share_that_does_not_answer_gives_the_old_overview_or_silence() {
        let dir = tempfile::tempdir().unwrap();
        let (db, temp, tools) = configured(dir.path());
        make_tool(&tools, "pdf-merge", Some(("README.txt", "Merge several PDF files into one.")));
        make_production(&temp, "2026-01-01_shop_old", Some("STATUS: live"));
        let slow = Readers { temp: slow_temp, tools: slow_tools };
        let limit = Duration::from_millis(250);

        // No cache yet: silent, and quick.
        let started = Instant::now();
        let silent = START(&db, NOW, true, true, limit, &slow);
        assert!(started.elapsed() < Duration::from_millis(1500), "waited {:?}", started.elapsed());
        assert_eq!(silent, StartLines::default());

        // With a cache from an earlier day: the old answer, said to be old, and quick.
        START(&db, NOW - 3 * DAY, false, true, HOOK_READ_LIMIT, &READERS);
        let started = Instant::now();
        let stale = START(&db, NOW, true, true, limit, &slow);
        assert!(started.elapsed() < Duration::from_millis(1500), "waited {:?}", started.elapsed());
        let overview = stale.tools_overview.expect("the cached overview");
        let date = &time::iso8601_from_unix(NOW - 3 * DAY)[..10];
        assert!(overview.starts_with(&format!("Helper tools available in {} (list from {date};", tools.display())), "{overview}");
        assert!(overview.contains("pdf-merge - Merge several PDF files into one."), "{overview}");
        assert_eq!(stale.temp_notice, None, "a temp folder that did not answer is silent");
        assert_eq!(stale.tools_check, None);
    }

    #[test]
    fn a_folder_that_cannot_be_read_falls_back_the_same_way() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _temp, tools) = configured(dir.path());
        make_tool(&tools, "pdf-merge", Some(("README.txt", "Merge several PDF files into one.")));
        START(&db, NOW - DAY, true, true, HOOK_READ_LIMIT, &READERS);
        let broken = Readers { tools: broken_tools, ..READERS };
        let lines = START(&db, NOW, true, true, HOOK_READ_LIMIT, &broken);
        assert!(lines.tools_overview.unwrap().contains("could not be reached just now"));
        // A folder that does not exist at all is an error too, not a crash.
        set_tools_folder(&db, Some(&dir.path().join("gone").display().to_string())).unwrap();
        assert_eq!(START(&db, NOW, true, true, HOOK_READ_LIMIT, &READERS), StartLines::default());
    }

    #[test]
    fn the_temp_notice_comes_once_a_day_and_a_quiet_day_is_not_even_read() {
        let dir = tempfile::tempdir().unwrap();
        let (db, temp, _tools) = configured(dir.path());
        make_production(&temp, "2026-10-01_shop_done", Some("STATUS: live"));
        make_production(&temp, "2026-08-01_shop_forgotten", Some("STATUS: waiting"));
        make_production(&temp, "2026-10-15_shop_busy", Some("STATUS: in progress"));
        let counting = Readers { temp: counting_temp, ..READERS };

        let before = TEMP_READS.load(Ordering::SeqCst);
        let first = START(&db, NOW, true, true, HOOK_READ_LIMIT, &counting);
        let line = first.temp_notice.expect("the first start of the day says it");
        assert!(line.contains("1 production folder(s) say they may be deleted (2026-10-01_shop_done)"), "{line}");
        assert!(line.contains("older than 30 days") && line.contains("2026-08-01_shop_forgotten"), "{line}");
        assert!(!line.contains("2026-10-15_shop_busy"));

        let second = START(&db, NOW + 3_600, true, true, HOOK_READ_LIMIT, &counting);
        assert_eq!(second.temp_notice, None, "once a day");
        assert_eq!(TEMP_READS.load(Ordering::SeqCst) - before, 1, "the second start did not read the folder");

        let next_day = START(&db, NOW + DAY + 60, true, true, HOOK_READ_LIMIT, &counting);
        assert!(next_day.temp_notice.is_some(), "a day later it is due again");
    }

    #[test]
    fn nothing_to_clean_up_says_nothing_and_does_not_use_up_the_day() {
        let dir = tempfile::tempdir().unwrap();
        let (db, temp, _tools) = configured(dir.path());
        make_production(&temp, "2026-10-15_shop_busy", Some("STATUS: in progress"));
        assert_eq!(START(&db, NOW, true, true, HOOK_READ_LIMIT, &READERS).temp_notice, None);
        make_production(&temp, "2026-10-16_shop_done", Some("STATUS: live"));
        assert!(START(&db, NOW + 60, true, true, HOOK_READ_LIMIT, &READERS).temp_notice.is_some(), "a quiet look did not silence the day");
    }

    #[test]
    fn a_measurement_run_leaves_no_trace_and_a_headless_run_gets_the_overview_only() {
        let dir = tempfile::tempdir().unwrap();
        let (db, temp, tools) = configured(dir.path());
        make_production(&temp, "2026-10-01_shop_done", Some("STATUS: live"));
        make_tool(&tools, "no-readme", None);
        // `record` false: the same lines, but the day is not used up.
        let probe = START(&db, NOW, true, false, HOOK_READ_LIMIT, &READERS);
        assert!(probe.temp_notice.is_some() && probe.tools_check.is_some());
        assert!(START(&db, NOW + 1, true, false, HOOK_READ_LIMIT, &READERS).temp_notice.is_some());
        // Headless: the overview, never the notices, and the day is kept for the owner.
        let headless = START(&db, NOW + 2, false, true, HOOK_READ_LIMIT, &READERS);
        assert!(headless.tools_overview.is_some());
        assert_eq!((headless.temp_notice, headless.tools_check), (None, None));
        assert!(START(&db, NOW + 3, true, true, HOOK_READ_LIMIT, &READERS).temp_notice.is_some());
    }

    #[test]
    fn the_tools_check_is_said_once_a_day_with_the_names() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _temp, tools) = configured(dir.path());
        make_tool(&tools, "no-readme", None);
        make_tool(&tools, "empty-readme", Some(("README.txt", "\n")));
        make_tool(&tools, "good", Some(("README.txt", "Does a good thing.")));
        std::fs::write(tools.join("quick.py"), "print(1)").unwrap();
        let first = START(&db, NOW, true, true, HOOK_READ_LIMIT, &READERS);
        let line = first.tools_check.expect("findings are said");
        assert!(line.contains("1 tool folder(s) have no README.txt or README.md (no-readme)"), "{line}");
        assert!(line.contains("1 README(s) have no first line saying what the tool does (empty-readme)"), "{line}");
        assert!(line.contains("1 loose script file(s) sit directly in the tools folder (quick.py)"), "{line}");
        assert!(line.contains("README.txt whose first line says"), "the way out is spelled out: {line}");
        // The next start of the same day: the overview again, the check not.
        let second = START(&db, NOW + 3_600, true, true, HOOK_READ_LIMIT, &READERS);
        assert!(second.tools_overview.is_some());
        assert_eq!(second.tools_check, None);
        // A tools folder in order says nothing.
        let tidy = tempfile::tempdir().unwrap();
        let (db2, _t, tools2) = configured(tidy.path());
        make_tool(&tools2, "good", Some(("README.md", "Does a good thing.")));
        let lines = START(&db2, NOW, true, true, HOOK_READ_LIMIT, &READERS);
        assert_eq!(lines.tools_check, None);
        assert!(lines.tools_overview.unwrap().contains("good - Does a good thing."));
    }

    #[test]
    fn only_one_folder_configured_works_alone() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        let temp = dir.path().join("t");
        std::fs::create_dir_all(&temp).unwrap();
        make_production(&temp, "2026-10-01_shop_done", Some("STATUS: live"));
        write_settings(&db, &format!(r#"{{"temp_until_live":{}}}"#, json_path(&temp)));
        let lines = START(&db, NOW, true, true, HOOK_READ_LIMIT, &READERS);
        assert!(lines.temp_notice.is_some());
        assert_eq!((lines.tools_overview, lines.tools_check), (None, None));
    }

    // ------------------------------------------------------------ doctor

    #[test]
    fn doctor_says_nothing_without_settings() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        assert_eq!(doctor_lines(&db, NOW, DOCTOR_READ_LIMIT, &READERS), DoctorLines::default());
    }

    #[test]
    fn doctor_has_a_note_for_what_needs_a_look_and_a_detail_line_for_each_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (db, temp, tools) = configured(dir.path());
        make_production(&temp, "2026-10-01_shop_done", Some("STATUS: live"));
        make_production(&temp, "2026-10-15_shop_busy", Some("STATUS: in progress"));
        make_tool(&tools, "no-readme", None);
        let lines = doctor_lines(&db, NOW, DOCTOR_READ_LIMIT, &READERS);
        assert_eq!(lines.notes.len(), 2, "{:?}", lines.notes);
        assert!(lines.notes[0].starts_with("in your temp-until-live folder, 1 production folder(s) say they may be deleted"), "{:?}", lines.notes);
        assert!(lines.notes[1].starts_with("in your tools folder, 1 tool folder(s) have no README"), "{:?}", lines.notes);
        assert!(lines.notes.iter().all(|n| !n.contains('\n')));
        assert_eq!(lines.detail.len(), 2);
        assert!(lines.detail[0].contains("2 production folder(s)") && lines.detail[0].contains(&temp.display().to_string()), "{:?}", lines.detail);
        assert!(lines.detail[1].contains("1 tool folder(s)") && lines.detail[1].contains("no-readme"), "{:?}", lines.detail);

        // In order: detail lines, no notes.
        let tidy = tempfile::tempdir().unwrap();
        let (db2, temp2, tools2) = configured(tidy.path());
        make_production(&temp2, "2026-10-15_shop_busy", Some("STATUS: in progress"));
        make_tool(&tools2, "good", Some(("README.txt", "Does a good thing.")));
        let quiet = doctor_lines(&db2, NOW, DOCTOR_READ_LIMIT, &READERS);
        assert!(quiet.notes.is_empty(), "{:?}", quiet.notes);
        assert!(quiet.detail[0].contains("nothing to clean up") && quiet.detail[1].contains("in order"), "{:?}", quiet.detail);
    }

    #[test]
    fn doctor_names_a_folder_that_does_not_answer() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _temp, _tools) = configured(dir.path());
        let slow = Readers { temp: slow_temp, tools: slow_tools };
        let started = Instant::now();
        let lines = doctor_lines(&db, NOW, Duration::from_millis(250), &slow);
        assert!(started.elapsed() < Duration::from_millis(1500), "waited {:?}", started.elapsed());
        assert_eq!(lines.notes.len(), 2);
        assert!(lines.notes[0].contains("did not answer within 1 second.") && lines.notes[0].contains("network share"), "{:?}", lines.notes);
        assert!(lines.detail[1].contains("did not answer within 1 second"), "{:?}", lines.detail);
    }

    #[test]
    fn doctor_tells_a_folder_that_fails_from_one_that_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _temp, _tools) = configured(dir.path());
        let broken = Readers { tools: broken_tools, ..READERS };
        let lines = doctor_lines(&db, NOW, DOCTOR_READ_LIMIT, &broken);
        assert_eq!(lines.notes.len(), 1, "only the tools folder failed: {:?}", lines.notes);
        assert!(lines.notes[0].starts_with("your tools folder (") && lines.notes[0].contains("could not be read (access is denied)"), "{:?}", lines.notes);
        assert!(lines.detail[1].contains("could not be read (access is denied)"), "{:?}", lines.detail);
    }
}
