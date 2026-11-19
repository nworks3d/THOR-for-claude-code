//! `install --tidy --plan <plan.json>` and `install --tidy-undo`: put a
//! person's loose files in order, one fixed folder per subject, each with its
//! own memory, and take it back on request.
//!
//! WHY. A first-time user's first complaint was that Claude "puts everything
//! randomly everywhere". After the first-session welcome the assistant looks
//! at the earlier sessions (`serve inventory`), groups them by subject and
//! proposes an order. When the person says yes, THIS is the part that does
//! the moving, so a plan is carried out the same careful way every time, by
//! code, and not by a loose pile of shell commands.
//!
//! The plan is JSON: `[{"folder": "<absolute path>", "files": ["<absolute
//! path>", ...]}]`. For each entry the folder is made if needed and given its
//! own memory exactly as `install --topic` does from inside it (the same
//! `topic_of`, marker and registration code), and each file is moved into it.
//!
//! WHAT IT NEVER DOES. It never deletes a file and never overwrites one (a
//! name that is taken gets " (2)", " (3)" ...). It never touches a file
//! outside the home folder, inside a program's own folders (application data,
//! program files, Claude's settings) or inside a git repository (that is a
//! programming project already, in order by itself). It refuses a folder that
//! is the home folder, Desktop, Documents, Downloads, a drive root, anything
//! below application data or program files, anything outside the home folder
//! and anything inside a git repository, with a plain reason, and carries on
//! with the next entry.
//!
//! Every move is written to an undo log beside the store
//! (`tidy-<date>-<n>.json`). `undo` puts the files of the newest log back,
//! again without overwriting, and renames that log `.undone.json` so the next
//! undo reaches the one before. The folders and their memories stay.

use serde::{Deserialize, Serialize};
use serve::inventory::{broad_place, file_source_system_place, is_inside, norm, system_place};
use std::path::{Path, PathBuf};

use crate::install::{check_registration_at, record_topic_opened, topic_of, write_project_marker, RegisterOutcome};

#[derive(Debug, Deserialize)]
pub struct PlanEntry {
    pub folder: String,
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct Move {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct UndoLog {
    pub made: String,
    pub moves: Vec<Move>,
    /// Folders this run made (they are left in place by an undo).
    pub folders: Vec<String>,
}

/// A name that is free in `dir`: `name`, else `stem (2).ext`, `stem (3).ext` ...
pub fn unique_dest(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let as_path = Path::new(name);
    let stem = as_path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = as_path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut n = 2;
    loop {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Move a file without ever deleting anything that was not copied first: a
/// rename, or (across disks) a copy that is checked before the original goes.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    let same = std::fs::metadata(from)?.len() == std::fs::metadata(to)?.len();
    if same {
        std::fs::remove_file(from)
    } else {
        let _ = std::fs::remove_file(to);
        Err(std::io::Error::other("the copy did not match the original, so nothing was moved"))
    }
}

fn refuse_folder(home: &Path, folder: &Path) -> Option<String> {
    if !folder.is_absolute() {
        return Some("it is not a full path".to_string());
    }
    if let Some(w) = broad_place(home, folder) {
        return Some(format!("that is your {w}, which is too broad to be one subject"));
    }
    if !is_inside(home, folder) {
        return Some("it is outside your home folder".to_string());
    }
    if let Some(s) = system_place(home, folder) {
        return Some(format!("that is a folder of {s}, not a place for your work"));
    }
    if serve::project::git_root(folder).is_some() {
        return Some("it is inside a programming project (a git repository), which is already in order by itself".to_string());
    }
    None
}

fn refuse_file(home: &Path, file: &Path, target: &Path) -> Option<String> {
    if !file.is_absolute() {
        return Some("it is not a full path".to_string());
    }
    if !file.is_file() {
        return Some("it is gone (or not a file)".to_string());
    }
    if !is_inside(home, file) {
        return Some("it is outside your home folder".to_string());
    }
    // A file may come out of the desktop app's scratch workspace or Claude
    // Code's per-session scratchpad under Temp (temporary places that get
    // cleared); a folder is never made there (`refuse_folder`).
    if let Some(s) = file_source_system_place(home, file) {
        return Some(format!("it belongs to {s}"));
    }
    let parent = file.parent().unwrap_or(file);
    if serve::project::git_root(parent).is_some() {
        return Some("it belongs to a programming project (a git repository)".to_string());
    }
    if norm(parent) == norm(target) {
        return Some("it is already in that folder".to_string());
    }
    None
}

fn today() -> String {
    serve::time::now_iso8601().chars().take(10).collect()
}

fn log_files(dir: &Path) -> Vec<(String, u32, PathBuf)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(rest) = name.strip_prefix("tidy-").and_then(|r| r.strip_suffix(".json")) else { continue };
        if rest.ends_with(".undone") {
            continue;
        }
        // <date>-<n>, the date being YYYY-MM-DD.
        if let Some((date, n)) = rest.rsplit_once('-') {
            if let Ok(n) = n.parse::<u32>() {
                out.push((date.to_string(), n, e.path()));
            }
        }
    }
    out.sort();
    out
}

fn save_log(path: &Path, log: &UndoLog) -> Result<(), String> {
    let text = serde_json::to_string_pretty(log).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("could not write the undo log {}: {e}", path.display()))
}

/// Carry out a plan. Returns the plain lines to print. An `Err` means the
/// whole plan could not be read or there is no memory to register in; nothing
/// was changed then.
pub fn tidy(db: &Path, plan_text: &str, home: &Path) -> Result<Vec<String>, String> {
    if !db.is_file() {
        return Err(format!("there is no memory at {}; check the path given with --db. Nothing was changed.", db.display()));
    }
    let plan: Vec<PlanEntry> =
        serde_json::from_str(plan_text).map_err(|e| format!("the plan is not valid: {e}. Nothing was changed."))?;
    let log_dir = db.parent().unwrap_or(Path::new(".")).to_path_buf();
    let date = today();
    let mut n = 1;
    let log_path = loop {
        let p = log_dir.join(format!("tidy-{date}-{n}.json"));
        if !p.exists() && !log_dir.join(format!("tidy-{date}-{n}.undone.json")).exists() {
            break p;
        }
        n += 1;
    };
    let mut log = UndoLog { made: serve::time::now_iso8601(), ..Default::default() };
    let mut lines = Vec::new();
    let mut ready_folders: Vec<String> = Vec::new();
    for entry in &plan {
        let folder = PathBuf::from(&entry.folder);
        if let Some(why) = refuse_folder(home, &folder) {
            lines.push(format!("! REFUSED {}: {why}. Nothing was moved for this one.", entry.folder));
            continue;
        }
        let (top, key) = match topic_of(&folder) {
            Ok(found) => found,
            Err(refusal) => {
                lines.push(format!("! {refusal}"));
                continue;
            }
        };
        if let Err(refusal) = check_registration_at(db, &top, &key) {
            lines.push(format!("! {refusal}"));
            continue;
        }
        let existed = folder.is_dir();
        if let Err(e) = std::fs::create_dir_all(&folder) {
            lines.push(format!("! could not make the folder {}: {e}. Nothing was moved for this one.", entry.folder));
            continue;
        }
        if !existed {
            log.folders.push(entry.folder.clone());
        }
        let memory = match write_project_marker(&top, &key).map_err(|e| e.to_string()).and_then(|_| record_topic_opened(db, &top, &key).map_err(|e| e.to_string())) {
            Ok(RegisterOutcome::AlreadyHere { .. }) => "its own memory was already there",
            Ok(_) => "with its own memory",
            Err(e) => {
                lines.push(format!("! could not give {} its own memory: {e}. Nothing was moved for this one.", entry.folder));
                continue;
            }
        };
        let mut moved = 0usize;
        let mut skipped = Vec::new();
        for f in &entry.files {
            let file = PathBuf::from(f);
            if let Some(why) = refuse_file(home, &file, &folder) {
                skipped.push(format!("  - left {f}: {why}"));
                continue;
            }
            let Some(name) = file.file_name() else { continue };
            let dest = unique_dest(&folder, name);
            match move_file(&file, &dest) {
                Ok(()) => {
                    moved += 1;
                    log.moves.push(Move { from: file.display().to_string(), to: dest.display().to_string() });
                }
                Err(e) => skipped.push(format!("  - left {f}: could not move it ({e})")),
            }
        }
        let verb = if existed { "= the folder was already there" } else { "+ made the folder" };
        ready_folders.push(entry.folder.clone());
        lines.push(format!("{verb} {} ({memory}): moved {moved} file(s) into it", entry.folder));
        lines.extend(skipped);
        if !log.moves.is_empty() {
            save_log(&log_path, &log)?;
        }
    }
    if log.moves.is_empty() {
        lines.push("No file was moved, so there is nothing to undo.".to_string());
    } else {
        lines.push(format!("Every move is written down, so all of it can be put back with: install --tidy-undo ({}).", log_path.display()));
    }
    // THOR's own record that a plan was carried out: part of what settles the
    // tidy step (`model::store::record_order_evidence`; the Stop hook checks it
    // against the person's answer). Only when a folder was really made ready.
    if !ready_folders.is_empty() && (!log.moves.is_empty() || !log.folders.is_empty()) {
        if let Ok(mut store) = thor_core::event_store::EventStore::open_existing(db) {
            if model::store::show(&store, model::store::ORDER_NOTE_ID).is_ok() {
                let body = serde_json::json!({
                    "folders": ready_folders,
                    "moved": log.moves.len(),
                    "undo_log": if log.moves.is_empty() { String::new() } else { log_path.display().to_string() },
                });
                let _ = model::store::record_order_evidence(&mut store, "tidy", body);
            }
        }
    }
    Ok(lines)
}

/// Put the files of the newest undo log back, never overwriting anything.
pub fn undo(db: &Path, home: &Path) -> Result<Vec<String>, String> {
    let log_dir = db.parent().unwrap_or(Path::new(".")).to_path_buf();
    let Some((_, _, path)) = log_files(&log_dir).pop() else {
        return Ok(vec!["There is nothing to put back: no earlier tidy-up was found.".to_string()]);
    };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let log: UndoLog = serde_json::from_str(&text).map_err(|e| format!("{} is not a valid undo log: {e}", path.display()))?;
    let mut lines = Vec::new();
    let mut back = 0usize;
    for m in log.moves.iter().rev() {
        let (from, to) = (PathBuf::from(&m.from), PathBuf::from(&m.to));
        if !is_inside(home, &from) || !is_inside(home, &to) {
            lines.push(format!("  - left {}: it is outside your home folder", m.to));
            continue;
        }
        if !to.is_file() {
            lines.push(format!("  - left {}: it is not there any more", m.to));
            continue;
        }
        let Some(parent) = from.parent() else { continue };
        if let Err(e) = std::fs::create_dir_all(parent) {
            lines.push(format!("  - left {}: could not make {} ({e})", m.to, parent.display()));
            continue;
        }
        let name = from.file_name().unwrap_or_default();
        let target = unique_dest(parent, name);
        match move_file(&to, &target) {
            Ok(()) => {
                back += 1;
                if target != from {
                    lines.push(format!("  - the old place of {} was taken, so it went back as {}", m.to, target.display()));
                }
            }
            Err(e) => lines.push(format!("  - left {}: could not move it back ({e})", m.to)),
        }
    }
    let done = path.with_extension("undone.json");
    std::fs::rename(&path, &done).map_err(|e| format!("put the files back, but could not mark the log as used: {e}"))?;
    lines.insert(0, format!("Put {back} file(s) back where they were. The folders that were made stay, with their memory."));
    Ok(lines)
}
