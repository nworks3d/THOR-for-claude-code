//! Script files an assistant wrote outside any git repository and left behind.
//!
//! Only when the owner has named a tools folder (`work_folders`, key `tools`).
//! A helper script written to a scratch folder is either worth keeping, and
//! then it belongs in the tools folder with a README, or it was a one-off and
//! should be deleted. Left alone it is neither: it is found again by accident,
//! or never. So:
//!
//! 1. Before a Write, Edit, MultiEdit or NotebookEdit tool call runs,
//!    `record_write` notes the file when it is a script (`.py`, `.ps1`, ...),
//!    lies outside every git work tree and outside the tools folder and the
//!    temp-until-live folder. The note is kept per session in
//!    `leftover-scripts.json` beside the store.
//! 2. At the end of a turn, `hold_reason` lists the noted files that still
//!    exist and were last changed more than 30 minutes ago, and asks the
//!    assistant to move each reusable one into the tools folder (with a README
//!    whose first line says what it does) or to delete it. A file is asked
//!    about ONCE per session: the hold marks it, and a later turn end does not
//!    hold for it again, so the question can never become a loop.
//!
//! WHAT THIS DOES NOT SEE. Only the file tools are watched. A script made by a
//! shell redirect (`echo ... > x.py`, `cat <<EOF > x.ps1`) never passes
//! through them and is not noted.
//!
//! Everything here is best effort and fails open: an unreadable settings file,
//! an unreadable or unwritable note file, a git lookup that does not answer in
//! time, a copy of the memory (which takes no state writes at all) - each means
//! no hold, never a wrong one.

use crate::{project, store_dir, work_folders};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The note file beside the store.
pub const FILE: &str = "leftover-scripts.json";
/// A file must have been left alone this long before it is asked about.
pub const MIN_AGE_SECS: i64 = 30 * 60;
/// A session's notes are forgotten after this long.
const KEEP_SECS: i64 = 14 * 86_400;
const MAX_FILES_PER_SESSION: usize = 200;
const MAX_LISTED: usize = 10;
/// Looking for a git work tree must not hold up a tool call.
const GIT_LOOKUP_LIMIT: Duration = Duration::from_millis(500);
/// The tool calls that write a file.
const WRITE_TOOLS: [&str; 4] = ["Write", "Edit", "MultiEdit", "NotebookEdit"];

/// The words the hold opens with. The serve binary recognises the hold by them
/// to put its plain sentence for the person beside it.
pub const HOLD_MARKER: &str = "Script file(s) were written outside any git repository";

fn book_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(FILE)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SessionRecord {
    /// When this session last noted a file.
    #[serde(default)]
    at: i64,
    /// Files noted, as the tool call spelled them (made absolute).
    #[serde(default)]
    files: Vec<String>,
    /// Files this session was already asked about.
    #[serde(default)]
    held: Vec<String>,
}

type Book = BTreeMap<String, SessionRecord>;

fn read_book(db: &Path) -> Book {
    std::fs::read_to_string(book_path(db))
        .ok()
        .and_then(|text| serde_json::from_str(text.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default()
}

fn write_book(db: &Path, book: &Book) {
    if let Ok(text) = serde_json::to_string(book) {
        let _ = store_dir::write(&book_path(db), text);
    }
}

// ------------------------------------------------------------ comparing paths

/// A path as it is compared: forward slashes, no `\\?\` prefix, no trailing
/// slash, and on Windows lower case. A UNC path (`\\server\share\x`) becomes
/// `//server/share/x`.
pub fn normalize(path: &str) -> String {
    let mut text = path.replace('\\', "/");
    if let Some(rest) = text.strip_prefix("//?/UNC/") {
        text = format!("//{rest}");
    } else if let Some(rest) = text.strip_prefix("//?/") {
        text = rest.to_string();
    }
    let trimmed = text.trim_end_matches('/');
    let text = if trimmed.is_empty() { "/" } else { trimmed };
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

/// Whether `path` lies below `folder` (the folder itself is not below itself).
pub fn is_inside(path: &str, folder: &str) -> bool {
    let (path, folder) = (normalize(path), normalize(folder));
    let prefix = if folder.ends_with('/') { folder } else { format!("{folder}/") };
    path.len() > prefix.len() && path.starts_with(&prefix)
}

// ---------------------------------------------------------------- recording

/// Whether `file` lies in THOR's own folders: the one holding the store and the
/// one the running program was installed in. Scripts there (a scheduled
/// task's, a backup helper) belong to THOR's setup, not to a session's scratch.
fn in_thor_folder(db: &Path, file: &str) -> bool {
    let store = db.parent().filter(|p| !p.as_os_str().is_empty());
    let program = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
    store.map(Path::to_path_buf).into_iter().chain(program).any(|folder| is_inside(file, &folder.display().to_string()))
}

/// Whether a git work tree (a `.git` folder or file) sits at or above `dir`.
/// `None` when the lookup did not answer in time.
fn in_git_work_tree(dir: &Path) -> Option<bool> {
    let dir = dir.to_path_buf();
    work_folders::within(GIT_LOOKUP_LIMIT, move || project::git_root(&dir).is_some())
}

/// Note a tool call's file when it is a leftover candidate. Called before every
/// tool call; the cheap checks come first, so an ordinary call costs a string
/// comparison.
pub fn record_write(db: &Path, session_id: &str, tool_name: &str, tool_input: Option<&Value>, cwd: Option<&Path>, now: i64) {
    record_write_with(db, session_id, tool_name, tool_input, cwd, now, &in_git_work_tree);
}

/// `record_write` with the git lookup passed in (`None` = unknown, which notes
/// nothing).
pub fn record_write_with(
    db: &Path,
    session_id: &str,
    tool_name: &str,
    tool_input: Option<&Value>,
    cwd: Option<&Path>,
    now: i64,
    in_git: &dyn Fn(&Path) -> Option<bool>,
) {
    if !WRITE_TOOLS.contains(&tool_name) {
        return;
    }
    let Some(file) = tool_input.and_then(|i| i.get("file_path").or_else(|| i.get("notebook_path"))).and_then(Value::as_str) else {
        return;
    };
    if !work_folders::is_script_name(file) {
        return;
    }
    let settings = work_folders::settings(db);
    let Some(tools) = settings.tools else { return };
    let absolute = match (Path::new(file).is_absolute(), cwd) {
        (true, _) => PathBuf::from(file),
        (false, Some(cwd)) => cwd.join(file),
        (false, None) => return,
    };
    let absolute_text = absolute.display().to_string();
    let exempt = std::iter::once(&tools).chain(settings.temp_until_live.as_ref()).any(|folder| is_inside(&absolute_text, &folder.display().to_string()));
    if exempt || in_thor_folder(db, &absolute_text) {
        return;
    }
    let Some(parent) = absolute.parent() else { return };
    if in_git(parent) != Some(false) {
        return;
    }

    let mut book = read_book(db);
    book.retain(|_, record| now - record.at < KEEP_SECS);
    let record = book.entry(session_id.to_string()).or_default();
    record.at = now;
    let key = normalize(&absolute_text);
    if record.files.len() < MAX_FILES_PER_SESSION && !record.files.iter().any(|known| normalize(known) == key) {
        record.files.push(absolute_text);
    }
    write_book(db, &book);
}

// -------------------------------------------------------------------- the hold

/// Whether the file still exists and was last changed more than
/// `MIN_AGE_SECS` before `now`.
fn left_over(file: &str, now: i64) -> bool {
    std::fs::metadata(file)
        .ok()
        .filter(|meta| meta.is_file())
        .and_then(|meta| meta.modified().ok())
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|since| now - since.as_secs() as i64 > MIN_AGE_SECS)
}

fn message(files: &[String], tools: &str) -> String {
    let mut out = format!("[THOR] {HOLD_MARKER} during this session and are still there:\n");
    for file in files.iter().take(MAX_LISTED) {
        out.push_str(&format!("- {file}\n"));
    }
    if files.len() > MAX_LISTED {
        out.push_str(&format!("...and {} more.\n", files.len() - MAX_LISTED));
    }
    out.push_str(&format!(
        "Your tools folder is {tools}. For each file: if it is a reusable helper, move it into a new sub-folder of the tools folder (one folder per tool, named after what it does) and add a README.txt in that sub-folder whose first line says in one sentence what the tool does; if it was a one-off, delete it. Then end the turn. THOR will not ask again about these files in this session."
    ));
    out
}

/// What the turn end should say about leftover scripts, or `None`. Marks the
/// files it lists as asked about, so the same file is never asked about twice
/// in one session.
pub fn hold_reason(db: &Path, session_id: &str, now: i64) -> Option<String> {
    let tools = work_folders::settings(db).tools?;
    let mut book = read_book(db);
    let record = book.get_mut(session_id)?;
    let candidates: Vec<String> = record
        .files
        .iter()
        .filter(|file| !record.held.iter().any(|held| normalize(held) == normalize(file)))
        .filter(|file| !in_thor_folder(db, file))
        .cloned()
        .collect();
    if candidates.is_empty() {
        return None;
    }
    // The files were written by this session, so their disk is there; the
    // limit only keeps a folder that has since gone away from stalling the turn end.
    let due = work_folders::within(work_folders::HOOK_READ_LIMIT, move || {
        candidates.into_iter().filter(|file| left_over(file, now)).collect::<Vec<String>>()
    })?;
    if due.is_empty() {
        return None;
    }
    record.held.extend(due.iter().cloned());
    write_book(db, &book);
    Some(message(&due, &tools.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_792_454_400;

    struct Fx {
        dir: tempfile::TempDir,
        db: PathBuf,
        tools: PathBuf,
        temp: PathBuf,
        scratch: PathBuf,
    }

    fn fixture() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("store")).unwrap();
        let db = dir.path().join("store").join("thor.db");
        std::fs::write(&db, b"").unwrap();
        let tools = dir.path().join("tools");
        let temp = dir.path().join("temp-until-live");
        let scratch = dir.path().join("scratch");
        for folder in [&tools, &temp, &scratch] {
            std::fs::create_dir_all(folder).unwrap();
        }
        work_folders::set_tools_folder(&db, Some(&tools.display().to_string())).unwrap();
        work_folders::set_temp_folder(&db, Some(&temp.display().to_string())).unwrap();
        Fx { dir, db, tools, temp, scratch }
    }

    fn outside_git(_: &Path) -> Option<bool> {
        Some(false)
    }

    fn write(fx: &Fx, session: &str, tool: &str, file: &Path) {
        let input = json!({ "file_path": file.display().to_string() });
        record_write_with(&fx.db, session, tool, Some(&input), None, NOW, &outside_git);
    }

    fn noted(fx: &Fx, session: &str) -> Vec<String> {
        read_book(&fx.db).get(session).map(|r| r.files.clone()).unwrap_or_default()
    }

    /// A real script file, changed just now. A test moves the clock it passes in
    /// (`mtime(file) + minutes`) instead of the file's time.
    fn make_script(folder: &Path, name: &str) -> PathBuf {
        let file = folder.join(name);
        std::fs::write(&file, "print('hi')\n").unwrap();
        file
    }

    fn mtime(file: &Path) -> i64 {
        std::fs::metadata(file).unwrap().modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
    }

    // --------------------------------------------------------- the paths

    #[test]
    fn paths_are_compared_the_same_whatever_their_spelling() {
        assert_eq!(normalize(r"C:\Work\Tools\"), if cfg!(windows) { "c:/work/tools" } else { "C:/Work/Tools" });
        assert_eq!(normalize(r"\\?\C:\x"), normalize("C:/x"));
        assert_eq!(normalize(r"\\?\UNC\server\share\x"), normalize(r"\\server\share\x"));
        assert_eq!(normalize(r"\\server\share\x"), "//server/share/x");
        assert_eq!(normalize("/"), "/");
    }

    #[test]
    fn inside_means_below_and_not_merely_sharing_a_prefix() {
        assert!(is_inside(r"\\server\share\tools\pdf\run.py", r"\\server\share\tools"));
        assert!(is_inside("C:/work/tools/a.py", r"C:\work\tools\"));
        assert!(is_inside("/srv/tools/a/b.py", "/srv/tools"));
        assert!(!is_inside("/srv/tools2/a.py", "/srv/tools"), "a sibling with the same start");
        assert!(!is_inside("/srv/tools", "/srv/tools"), "the folder is not below itself");
        assert!(!is_inside("/srv/other/a.py", "/srv/tools"));
        assert!(is_inside("/srv/a.py", "/"));
        if cfg!(windows) {
            assert!(is_inside(r"C:\Work\TOOLS\a.py", r"c:\work\tools"), "Windows paths ignore letter case");
        } else {
            assert!(!is_inside("/srv/TOOLS/a.py", "/srv/tools"));
        }
    }

    // --------------------------------------------------------- recording

    #[test]
    fn a_script_outside_git_and_the_two_folders_is_noted() {
        let fx = fixture();
        let file = fx.scratch.join("fix.py");
        write(&fx, "s1", "Write", &file);
        assert_eq!(noted(&fx, "s1"), vec![file.display().to_string()]);
        for tool in ["Edit", "MultiEdit"] {
            write(&fx, "s2", tool, &fx.scratch.join(format!("{tool}.ps1")));
        }
        assert_eq!(noted(&fx, "s2").len(), 2);
        let notebook = json!({ "notebook_path": fx.scratch.join("n.sh").display().to_string() });
        record_write_with(&fx.db, "s3", "NotebookEdit", Some(&notebook), None, NOW, &outside_git);
        assert_eq!(noted(&fx, "s3").len(), 1, "a notebook call names its file by notebook_path");
    }

    #[test]
    fn every_script_extension_counts_and_other_files_do_not() {
        let fx = fixture();
        for ext in work_folders::SCRIPT_EXTENSIONS {
            write(&fx, "ext", "Write", &fx.scratch.join(format!("a.{ext}")));
        }
        assert_eq!(noted(&fx, "ext").len(), work_folders::SCRIPT_EXTENSIONS.len());
        for name in ["notes.md", "data.json", "a.pyc", "Makefile", "n.ipynb"] {
            write(&fx, "other", "Write", &fx.scratch.join(name));
        }
        assert!(noted(&fx, "other").is_empty());
    }

    #[test]
    fn only_the_tools_that_write_a_file_are_watched() {
        let fx = fixture();
        let file = fx.scratch.join("fix.py");
        for tool in ["Bash", "Read", "Grep", "Agent", "PowerShell", ""] {
            write(&fx, "s1", tool, &file);
        }
        record_write_with(&fx.db, "s1", "Write", None, None, NOW, &outside_git);
        record_write_with(&fx.db, "s1", "Write", Some(&json!({ "content": "x" })), None, NOW, &outside_git);
        assert!(noted(&fx, "s1").is_empty());
        assert!(!book_path(&fx.db).exists(), "nothing noted, nothing written");
    }

    #[test]
    fn the_tools_folder_and_the_temp_folder_are_exempt() {
        let fx = fixture();
        write(&fx, "s1", "Write", &fx.tools.join("pdf-merge").join("merge.py"));
        write(&fx, "s1", "Write", &fx.tools.join("loose.py"));
        write(&fx, "s1", "Write", &fx.temp.join("2026-10-01_shop_banner").join("build.mjs"));
        assert!(noted(&fx, "s1").is_empty(), "{:?}", noted(&fx, "s1"));
        // A folder that only starts with the same letters is not exempt.
        let near = fx.dir.path().join("tools-old");
        std::fs::create_dir_all(&near).unwrap();
        write(&fx, "s1", "Write", &near.join("old.py"));
        assert_eq!(noted(&fx, "s1").len(), 1);
    }

    #[test]
    fn a_script_in_thors_own_store_folder_is_not_noted_or_held() {
        let fx = fixture();
        // The store folder is the fixture's temp dir: a script beside the store.
        let own = make_script(fx.db.parent().unwrap(), "ship-naar-nas.ps1");
        write(&fx, "s1", "Write", &own);
        write(&fx, "s1", "Write", &fx.db.parent().unwrap().join("sub").join("x.py"));
        assert!(noted(&fx, "s1").is_empty(), "{:?}", noted(&fx, "s1"));
        // A note made by an older build is not held either.
        let mut book = Book::new();
        book.insert("s2".into(), SessionRecord { at: NOW, files: vec![own.display().to_string()], held: vec![] });
        write_book(&fx.db, &book);
        assert_eq!(hold_reason(&fx.db, "s2", mtime(&own) + 3_600), None);
    }

    #[test]
    fn a_script_inside_a_git_work_tree_is_not_noted() {
        let fx = fixture();
        let repo = fx.dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("scripts").join("deep")).unwrap();
        // The real lookup, not a stand-in.
        for file in [repo.join("build.py"), repo.join("scripts").join("deep").join("run.sh")] {
            let input = json!({ "file_path": file.display().to_string() });
            record_write(&fx.db, "s1", "Write", Some(&input), None, NOW);
        }
        assert!(noted(&fx, "s1").is_empty());
        // A worktree has a `.git` FILE.
        let worktree = fx.dir.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: /elsewhere/.git/worktrees/wt\n").unwrap();
        let input = json!({ "file_path": worktree.join("x.py").display().to_string() });
        record_write(&fx.db, "s1", "Write", Some(&input), None, NOW);
        assert!(noted(&fx, "s1").is_empty());
        // And a lookup that is not sure notes nothing.
        let input = json!({ "file_path": fx.scratch.join("x.py").display().to_string() });
        record_write_with(&fx.db, "s1", "Write", Some(&input), None, NOW, &|_| None);
        record_write_with(&fx.db, "s1", "Write", Some(&input), None, NOW, &|_| Some(true));
        assert!(noted(&fx, "s1").is_empty());
    }

    #[test]
    fn without_a_tools_folder_nothing_is_noted_and_nothing_is_written() {
        let fx = fixture();
        work_folders::set_tools_folder(&fx.db, None).unwrap();
        write(&fx, "s1", "Write", &fx.scratch.join("fix.py"));
        assert!(!book_path(&fx.db).exists());
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let input = json!({ "file_path": dir.path().join("fix.py").display().to_string() });
        record_write_with(&db, "s1", "Write", Some(&input), None, NOW, &outside_git);
        assert!(!book_path(&db).exists(), "no settings file at all");
    }

    #[test]
    fn a_relative_path_is_read_against_the_working_folder() {
        let fx = fixture();
        let input = json!({ "file_path": "helper.py" });
        record_write_with(&fx.db, "s1", "Write", Some(&input), Some(&fx.scratch), NOW, &outside_git);
        assert_eq!(noted(&fx, "s1"), vec![fx.scratch.join("helper.py").display().to_string()]);
        record_write_with(&fx.db, "s2", "Write", Some(&input), None, NOW, &outside_git);
        assert!(noted(&fx, "s2").is_empty(), "no folder to read it against");
    }

    #[test]
    fn the_same_file_is_noted_once_and_sessions_stay_apart() {
        let fx = fixture();
        let file = fx.scratch.join("fix.py");
        write(&fx, "s1", "Write", &file);
        write(&fx, "s1", "Edit", &file);
        write(&fx, "s2", "Write", &fx.scratch.join("other.py"));
        assert_eq!(noted(&fx, "s1").len(), 1);
        assert_eq!(noted(&fx, "s2").len(), 1);
    }

    #[test]
    fn old_sessions_are_forgotten() {
        let fx = fixture();
        write(&fx, "old", "Write", &fx.scratch.join("a.py"));
        let input = json!({ "file_path": fx.scratch.join("b.py").display().to_string() });
        record_write_with(&fx.db, "new", "Write", Some(&input), None, NOW + KEEP_SECS + 1, &outside_git);
        let book = read_book(&fx.db);
        assert!(book.contains_key("new") && !book.contains_key("old"), "{:?}", book.keys().collect::<Vec<_>>());
    }

    #[test]
    fn a_broken_note_file_starts_over() {
        let fx = fixture();
        std::fs::write(book_path(&fx.db), "{ not json").unwrap();
        write(&fx, "s1", "Write", &fx.scratch.join("a.py"));
        assert_eq!(noted(&fx, "s1").len(), 1);
    }

    // ------------------------------------------------------------ the hold

    #[test]
    fn a_file_left_over_for_half_an_hour_is_asked_about_once() {
        let fx = fixture();
        let file = make_script(&fx.scratch, "fix.py");
        write(&fx, "s1", "Write", &file);
        let changed = mtime(&file);

        assert_eq!(hold_reason(&fx.db, "s1", changed + 10 * 60), None, "ten minutes is too soon");
        assert_eq!(hold_reason(&fx.db, "s1", changed + 30 * 60), None, "exactly half an hour is not more");
        let reason = hold_reason(&fx.db, "s1", changed + 31 * 60).expect("half an hour and a minute");
        assert!(reason.starts_with(&format!("[THOR] {HOLD_MARKER}")), "{reason}");
        assert!(reason.contains(&file.display().to_string()), "{reason}");
        assert!(reason.contains(&fx.tools.display().to_string()), "{reason}");
        assert!(reason.contains("README.txt") && reason.contains("first line says in one sentence what the tool does"), "{reason}");
        assert!(reason.contains("if it was a one-off, delete it"), "{reason}");
        assert!(reason.contains("will not ask again"), "{reason}");

        assert_eq!(hold_reason(&fx.db, "s1", changed + 3_600), None, "never twice for the same file in one session");
        assert_eq!(hold_reason(&fx.db, "s1", changed + 86_400), None);
    }

    #[test]
    fn a_new_file_is_asked_about_but_the_old_one_is_not_asked_again() {
        let fx = fixture();
        let first = make_script(&fx.scratch, "first.py");
        write(&fx, "s1", "Write", &first);
        let later = mtime(&first) + 40 * 60;
        assert!(hold_reason(&fx.db, "s1", later).is_some());

        let second = make_script(&fx.scratch, "second.sh");
        write(&fx, "s1", "Write", &second);
        let reason = hold_reason(&fx.db, "s1", later + 3_600).expect("a file not asked about yet");
        assert!(reason.contains("second.sh") && !reason.contains("first.py"), "{reason}");
    }

    #[test]
    fn a_file_that_is_gone_or_still_fresh_is_not_listed() {
        let fx = fixture();
        let gone = make_script(&fx.scratch, "gone.py");
        let fresh = make_script(&fx.scratch, "fresh.py");
        write(&fx, "s1", "Write", &gone);
        write(&fx, "s1", "Write", &fresh);
        std::fs::remove_file(&gone).unwrap();
        let reason = hold_reason(&fx.db, "s1", mtime(&fresh) + 5 * 60);
        assert_eq!(reason, None, "the other file is only five minutes old and this one is deleted");
        let reason = hold_reason(&fx.db, "s1", mtime(&fresh) + 45 * 60).unwrap();
        assert!(reason.contains("fresh.py") && !reason.contains("gone.py"), "{reason}");
        // A file moved into the tools folder leaves nothing behind.
        let moved = make_script(&fx.scratch, "moved.py");
        write(&fx, "s2", "Write", &moved);
        std::fs::rename(&moved, fx.tools.join("moved.py")).unwrap();
        assert_eq!(hold_reason(&fx.db, "s2", NOW * 2), None);
    }

    #[test]
    fn one_session_is_never_asked_about_another_sessions_files() {
        let fx = fixture();
        let file = make_script(&fx.scratch, "mine.py");
        write(&fx, "s1", "Write", &file);
        let later = mtime(&file) + 3_600;
        assert_eq!(hold_reason(&fx.db, "s2", later), None);
        assert_eq!(hold_reason(&fx.db, "unknown", later), None);
        assert!(hold_reason(&fx.db, "s1", later).is_some());
    }

    #[test]
    fn the_hold_needs_a_tools_folder() {
        let fx = fixture();
        let file = make_script(&fx.scratch, "fix.py");
        write(&fx, "s1", "Write", &file);
        work_folders::set_tools_folder(&fx.db, None).unwrap();
        assert_eq!(hold_reason(&fx.db, "s1", mtime(&file) + 3_600), None);
        work_folders::set_tools_folder(&fx.db, Some(&fx.tools.display().to_string())).unwrap();
        assert!(hold_reason(&fx.db, "s1", mtime(&file) + 3_600).is_some(), "nothing was lost while it was off");
    }

    #[test]
    fn a_long_list_is_cut_and_says_how_many_more() {
        let fx = fixture();
        let mut files = Vec::new();
        for i in 0..13 {
            let file = make_script(&fx.scratch, &format!("s{i:02}.py"));
            write(&fx, "s1", "Write", &file);
            files.push(file);
        }
        let reason = hold_reason(&fx.db, "s1", mtime(&files[0]) + 3_600).unwrap();
        assert_eq!(reason.matches("\n- ").count(), 10, "{reason}");
        assert!(reason.contains("...and 3 more."), "{reason}");
    }

    #[test]
    fn a_copy_of_the_memory_takes_no_state_write_and_so_never_holds() {
        let fx = fixture();
        let _held = store_dir::hold(&fx.db);
        write(&fx, "s1", "Write", &fx.scratch.join("fix.py"));
        assert!(!book_path(&fx.db).exists(), "a held directory takes no write");
        assert_eq!(hold_reason(&fx.db, "s1", NOW * 2), None);
    }
}
