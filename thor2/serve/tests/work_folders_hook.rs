//! The owner's two work folders, end to end through the real compiled
//! `serve hook`: the tools overview and the temp-until-live notice at a
//! session start, the note of a script file written outside any git
//! repository, and the one question about it at the end of a turn.
//!
//! The pure decisions have their own unit tests in `serve::work_folders` and
//! `serve::leftover_scripts`; this proves the wiring: the settings file beside
//! the store, the real hook JSON, and that with no settings file the hook
//! behaves exactly as it did before.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn run_hook(db: &Path, cwd: &Path, payload: serde_json::Value) -> serde_json::Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .current_dir(cwd)
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    assert!(out.stderr.is_empty(), "the hook must never write to stderr: {:?}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    if stdout.trim().is_empty() {
        return serde_json::Value::Null;
    }
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("hook JSON ({e}): {stdout}"))
}

fn start(db: &Path, cwd: &Path, session: &str) -> String {
    let out = run_hook(
        db,
        cwd,
        serde_json::json!({ "hook_event_name": "SessionStart", "source": "startup", "session_id": session, "cwd": cwd.to_string_lossy() }),
    );
    out["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

fn write_call(db: &Path, cwd: &Path, session: &str, tool: &str, file: &Path) {
    let out = run_hook(
        db,
        cwd,
        serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": session,
            "cwd": cwd.to_string_lossy(),
            "tool_name": tool,
            "tool_input": { "file_path": file.to_string_lossy(), "content": "print(1)\n" },
        }),
    );
    assert!(out.get("decision").is_none(), "writing a script is never refused by this feature: {out}");
}

fn stop(db: &Path, cwd: &Path, session: &str) -> serde_json::Value {
    run_hook(
        db,
        cwd,
        serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": session,
            "cwd": cwd.to_string_lossy(),
            "stop_hook_active": false,
            "last_assistant_message": "",
        }),
    )
}

struct Fx {
    dir: tempfile::TempDir,
    db: PathBuf,
    work: PathBuf,
    scratch: PathBuf,
    tools: PathBuf,
    temp: PathBuf,
}

fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    thor_core::event_store::EventStore::new(&db).unwrap();
    let work = dir.path().join("work");
    let scratch = dir.path().join("scratch");
    let tools = dir.path().join("tools");
    let temp = dir.path().join("temp-until-live");
    for folder in [&work, &scratch, &tools, &temp] {
        std::fs::create_dir_all(folder).unwrap();
    }
    Fx { dir, db, work, scratch, tools, temp }
}

fn name_the_folders(fx: &Fx) {
    serve::work_folders::set_tools_folder(&fx.db, Some(&fx.tools.display().to_string())).unwrap();
    serve::work_folders::set_temp_folder(&fx.db, Some(&fx.temp.display().to_string())).unwrap();
}

/// Set a file's last-change time to `minutes` ago.
fn age(file: &Path, minutes: u64) {
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(minutes * 60);
    std::fs::OpenOptions::new().write(true).open(file).unwrap().set_modified(long_ago).unwrap();
}

/// The scratch folder must not sit inside a git repository for the "outside git"
/// half of these tests to mean anything (a temp folder inside a repository is
/// unusual; the test then has nothing to prove and says so).
fn scratch_is_outside_git(fx: &Fx) -> bool {
    let outside = serve::project::git_root(&fx.scratch).is_none();
    if !outside {
        eprintln!("skipped: the temp folder {} sits inside a git repository", fx.dir.path().display());
    }
    outside
}

// ------------------------------------------------------------ session start

#[test]
fn with_no_settings_file_a_session_start_has_no_trace_of_either_folder() {
    let fx = fixture();
    let block = start(&fx.db, &fx.work, "s1");
    assert!(!block.contains("Helper tools") && !block.contains("temp-until-live") && !block.contains("tools folder check"), "{block}");
    assert!(!fx.dir.path().join("tools-overview-cache.json").exists());
    assert!(!fx.dir.path().join("leftover-scripts.json").exists());
}

#[test]
fn a_session_start_lists_the_helper_tools_built_from_the_folders() {
    let fx = fixture();
    name_the_folders(&fx);
    std::fs::create_dir_all(fx.tools.join("pdf-merge")).unwrap();
    std::fs::write(fx.tools.join("pdf-merge").join("README.txt"), "\nMerge several PDF files into one.\nUsage: pdf-merge a.pdf b.pdf\n").unwrap();
    std::fs::create_dir_all(fx.tools.join("img-resize")).unwrap();
    std::fs::write(fx.tools.join("img-resize").join("README.md"), "# Resize every image in a folder\n").unwrap();

    for session in ["s1", "s2"] {
        let block = start(&fx.db, &fx.work, session);
        assert!(block.contains(&format!("Helper tools available in {}:", fx.tools.display())), "{session}: {block}");
        assert!(block.contains("pdf-merge - Merge several PDF files into one."), "{session}: {block}");
        assert!(block.contains("img-resize - Resize every image in a folder"), "{session}: {block}");
        assert!(block.contains("Look here before writing a new helper script"), "{session}: {block}");
    }
    assert!(fx.dir.path().join("tools-overview-cache.json").exists(), "the overview is kept beside the store");

    // The overview follows the folder: a new tool shows up at the next start.
    std::fs::create_dir_all(fx.tools.join("zip-all")).unwrap();
    std::fs::write(fx.tools.join("zip-all").join("README.txt"), "Zip every folder in a folder.\n").unwrap();
    assert!(start(&fx.db, &fx.work, "s3").contains("zip-all - Zip every folder in a folder."));
}

#[test]
fn the_temp_notice_and_the_tools_check_come_once_a_day_and_delete_nothing() {
    let fx = fixture();
    name_the_folders(&fx);
    let done = fx.temp.join("2026-10-01_shop_banner");
    std::fs::create_dir_all(&done).unwrap();
    std::fs::write(done.join("STATUS.txt"), "STATUS: live\n").unwrap();
    std::fs::create_dir_all(fx.tools.join("no-readme")).unwrap();
    std::fs::write(fx.tools.join("quick.py"), "print(1)\n").unwrap();

    let first = start(&fx.db, &fx.work, "s1");
    assert!(first.contains("temp-until-live: 1 production folder(s) say they may be deleted (2026-10-01_shop_banner)"), "{first}");
    assert!(first.contains("tools folder check") && first.contains("no-readme") && first.contains("quick.py"), "{first}");
    let second = start(&fx.db, &fx.work, "s2");
    assert!(!second.contains("temp-until-live:") && !second.contains("tools folder check"), "once a day: {second}");
    assert!(second.contains("Helper tools available in"), "the overview comes every time: {second}");

    assert!(done.join("STATUS.txt").exists() && fx.tools.join("quick.py").exists(), "nothing was deleted or moved");
}

// ------------------------------------------------------------- the turn end

#[test]
fn a_script_left_outside_git_is_asked_about_once_at_a_turn_end() {
    let fx = fixture();
    if !scratch_is_outside_git(&fx) {
        return;
    }
    name_the_folders(&fx);
    let keep = fx.scratch.join("fix_images.py");
    let junk = fx.scratch.join("try-this.ps1");
    for file in [&keep, &junk] {
        write_call(&fx.db, &fx.work, "s1", "Write", file);
        std::fs::write(file, "x\n").unwrap();
    }
    // Written just now: too soon to ask.
    assert_eq!(stop(&fx.db, &fx.work, "s1"), serde_json::Value::Null);

    age(&keep, 45);
    age(&junk, 45);
    let held = stop(&fx.db, &fx.work, "s1");
    assert_eq!(held["decision"], "block", "{held}");
    let reason = held["reason"].as_str().unwrap();
    assert!(reason.contains("Script file(s) were written outside any git repository"), "{reason}");
    assert!(reason.contains("fix_images.py") && reason.contains("try-this.ps1"), "{reason}");
    assert!(reason.contains(&fx.tools.display().to_string()) && reason.contains("README.txt"), "{reason}");
    let for_the_person = held["systemMessage"].as_str().unwrap();
    assert!(for_the_person.contains("helper scripts") && for_the_person.contains("Nothing is needed from you"), "{for_the_person}");

    assert_eq!(stop(&fx.db, &fx.work, "s1"), serde_json::Value::Null, "never twice for the same files in one session");
    assert_eq!(stop(&fx.db, &fx.work, "s2"), serde_json::Value::Null, "another session never wrote them");
}

#[test]
fn nothing_is_asked_without_a_tools_folder_or_for_files_that_are_not_leftovers() {
    let fx = fixture();
    if !scratch_is_outside_git(&fx) {
        return;
    }
    // No settings at all: the file is not even noted.
    let early = fx.scratch.join("early.py");
    std::fs::write(&early, "x\n").unwrap();
    write_call(&fx.db, &fx.work, "s1", "Write", &early);
    age(&early, 90);
    assert!(!fx.dir.path().join("leftover-scripts.json").exists());
    assert_eq!(stop(&fx.db, &fx.work, "s1"), serde_json::Value::Null);

    name_the_folders(&fx);
    // A file in the tools folder, one in the temp folder, one in a git work tree,
    // one that is not a script, one written by another tool: none is a leftover.
    let in_tools = fx.tools.join("pdf").join("run.py");
    let in_temp = fx.temp.join("2026-10-01_shop_banner").join("build.mjs");
    let repo = fx.dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let in_repo = repo.join("tool.py");
    let notes = fx.scratch.join("notes.md");
    let by_shell = fx.scratch.join("by_shell.py");
    for file in [&in_tools, &in_temp, &in_repo, &notes, &by_shell] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "x\n").unwrap();
        age(file, 90);
    }
    for file in [&in_tools, &in_temp, &in_repo, &notes] {
        write_call(&fx.db, &fx.work, "s2", "Write", file);
    }
    write_call(&fx.db, &fx.work, "s2", "Bash", &by_shell);
    assert_eq!(stop(&fx.db, &fx.work, "s2"), serde_json::Value::Null);
}

#[test]
fn a_leftover_that_was_dealt_with_is_not_asked_about() {
    let fx = fixture();
    if !scratch_is_outside_git(&fx) {
        return;
    }
    name_the_folders(&fx);
    let gone = fx.scratch.join("gone.py");
    let moved = fx.scratch.join("moved.py");
    for file in [&gone, &moved] {
        write_call(&fx.db, &fx.work, "s1", "Write", file);
        std::fs::write(file, "x\n").unwrap();
        age(file, 90);
    }
    std::fs::remove_file(&gone).unwrap();
    std::fs::create_dir_all(fx.tools.join("mover")).unwrap();
    std::fs::rename(&moved, fx.tools.join("mover").join("moved.py")).unwrap();
    assert_eq!(stop(&fx.db, &fx.work, "s1"), serde_json::Value::Null);
}
