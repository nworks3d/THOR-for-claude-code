//! `install --tidy --plan` and `install --tidy-undo`: loose files go into one
//! folder per subject, each with its own memory, and can be put back.
//!
//! Every program runs with HOME, USERPROFILE and LOCALAPPDATA pointed at a
//! throwaway sandbox, so no real file of the machine is ever opened. All file
//! names and contents are invented.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use thor_core::event_store::EventStore;

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let s = Self { home: tempfile::tempdir().unwrap() };
        std::fs::create_dir_all(s.path("Desktop")).unwrap();
        std::fs::create_dir_all(s.path("Documents")).unwrap();
        let db = s.db();
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_initstore"));
        cmd.arg("--db").arg(&db);
        s.apply(&mut cmd);
        assert!(cmd.output().unwrap().status.success());
        s
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.home.path().join(rel)
    }

    fn db(&self) -> PathBuf {
        self.path("localappdata").join("thor2").join("thor.db")
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", self.home.path());
        cmd.env("USERPROFILE", self.home.path());
        cmd.env("LOCALAPPDATA", self.path("localappdata"));
        cmd.env("GIT_CONFIG_GLOBAL", self.path(".gitconfig"));
        cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    }

    fn file(&self, rel: &str, body: &str) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_install"));
        cmd.arg("--db").arg(self.db()).args(args);
        self.apply(&mut cmd);
        cmd.output().unwrap()
    }

    /// Write the plan file and run `--tidy` on it.
    fn tidy(&self, plan: serde_json::Value) -> Output {
        let file = self.path("plan.json");
        std::fs::write(&file, plan.to_string()).unwrap();
        self.run(&["--tidy", "--plan", file.to_str().unwrap()])
    }
}

fn text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn s(p: &Path) -> String {
    p.display().to_string()
}

#[test]
fn files_are_moved_into_a_folder_with_its_own_memory() {
    let sb = Sandbox::new();
    let cv = sb.file("Documents/cv.docx", "cv");
    let letter = sb.file("Desktop/letter.txt", "letter");
    let folder = sb.path("Documents/Cv en werk zoeken");

    let out = sb.tidy(serde_json::json!([{ "folder": s(&folder), "files": [s(&cv), s(&letter)] }]));
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("+ made the folder") && said.contains("moved 2 file(s)"), "{said}");
    assert!(said.contains("--tidy-undo"), "{said}");

    assert!(folder.join("cv.docx").is_file() && folder.join("letter.txt").is_file());
    assert!(!cv.exists() && !letter.exists());
    // Its own memory: the marker and the registered working copy.
    assert_eq!(std::fs::read_to_string(folder.join(".thor-project")).unwrap().trim(), "Cv en werk zoeken");
    let store = EventStore::open_existing(&sb.db()).unwrap();
    assert!(serve::project::registered_checkout(&store, Some("Cv en werk zoeken")).is_ok());
}

#[test]
fn a_taken_name_gets_a_number_and_nothing_is_overwritten() {
    let sb = Sandbox::new();
    let folder = sb.path("Documents/Trip");
    sb.file("Documents/Trip/plan.txt", "old");
    let new1 = sb.file("Desktop/plan.txt", "new one");
    let new2 = sb.file("Downloads/plan.txt", "new two");

    let out = sb.tidy(serde_json::json!([{ "folder": s(&folder), "files": [s(&new1), s(&new2)] }]));
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(std::fs::read_to_string(folder.join("plan.txt")).unwrap(), "old");
    assert_eq!(std::fs::read_to_string(folder.join("plan (2).txt")).unwrap(), "new one");
    assert_eq!(std::fs::read_to_string(folder.join("plan (3).txt")).unwrap(), "new two");
}

#[test]
fn broad_and_program_folders_are_refused_and_the_next_entry_still_runs() {
    let sb = Sandbox::new();
    let f1 = sb.file("Desktop/a.txt", "a");
    let f2 = sb.file("Desktop/b.txt", "b");
    let good = sb.path("Documents/Garden");
    let out = sb.tidy(serde_json::json!([
        { "folder": s(sb.home.path()), "files": [s(&f1)] },
        { "folder": s(&sb.path("Desktop")), "files": [s(&f1)] },
        { "folder": s(&sb.path("Documents")), "files": [s(&f1)] },
        { "folder": s(&sb.path("Downloads")), "files": [s(&f1)] },
        { "folder": s(&sb.path("AppData/Local/Thing")), "files": [s(&f1)] },
        { "folder": s(&sb.path("Program Files/Thing")), "files": [s(&f1)] },
        { "folder": s(&sb.home.path().parent().unwrap().join("outside-home")), "files": [s(&f1)] },
        { "folder": s(&good), "files": [s(&f2)] },
    ]));
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert_eq!(said.matches("! REFUSED").count(), 7, "{said}");
    assert!(said.contains("home folder") && said.contains("Desktop") && said.contains("Documents") && said.contains("Downloads"), "{said}");
    assert!(said.contains("application data") && said.contains("program files") && said.contains("outside your home folder"), "{said}");
    assert!(f1.is_file(), "a refused entry moves nothing");
    assert!(good.join("b.txt").is_file());
    assert!(!sb.path("Program Files").exists() && !sb.path("AppData").exists());
}

#[test]
fn files_that_are_gone_outside_home_or_in_a_repository_are_left_alone() {
    let sb = Sandbox::new();
    let repo_file = sb.file("dev/site/index.html", "<html>");
    std::fs::create_dir_all(sb.path("dev/site/.git")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("elsewhere.txt");
    std::fs::write(&outside_file, "x").unwrap();
    let gone = sb.path("Desktop/never-existed.txt");
    let ok = sb.file("Desktop/ok.txt", "ok");
    let folder = sb.path("Documents/Mixed");

    let out = sb.tidy(serde_json::json!([{ "folder": s(&folder), "files": [s(&repo_file), s(&outside_file), s(&gone), s(&ok), "relative.txt"] }]));
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("moved 1 file(s)"), "{said}");
    assert!(said.contains("git repository") && said.contains("outside your home folder") && said.contains("gone") && said.contains("not a full path"), "{said}");
    assert!(repo_file.is_file() && outside_file.is_file());
}

#[test]
fn a_folder_inside_a_repository_is_refused() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.path("dev/site/.git")).unwrap();
    let f = sb.file("Desktop/a.txt", "a");
    let out = sb.tidy(serde_json::json!([{ "folder": s(&sb.path("dev/site/docs")), "files": [s(&f)] }]));
    assert!(text(&out).contains("! REFUSED") && text(&out).contains("git repository"), "{}", text(&out));
    assert!(f.is_file() && !sb.path("dev/site/docs").exists());
}

#[test]
fn undo_puts_everything_back_without_overwriting() {
    let sb = Sandbox::new();
    let cv = sb.file("Documents/cv.docx", "cv");
    let letter = sb.file("Desktop/letter.txt", "letter");
    let folder = sb.path("Documents/Work");
    assert!(sb.tidy(serde_json::json!([{ "folder": s(&folder), "files": [s(&cv), s(&letter)] }])).status.success());
    // Somebody made a new file in the old place meanwhile.
    sb.file("Desktop/letter.txt", "a different letter");

    let out = sb.run(&["--tidy-undo"]);
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("Put 2 file(s) back"), "{said}");
    assert_eq!(std::fs::read_to_string(&cv).unwrap(), "cv");
    assert_eq!(std::fs::read_to_string(&letter).unwrap(), "a different letter", "never overwritten");
    assert_eq!(std::fs::read_to_string(sb.path("Desktop/letter (2).txt")).unwrap(), "letter");
    assert!(folder.is_dir(), "the folder and its memory stay");
    assert!(!folder.join("cv.docx").exists());

    // The log is used up: a second undo finds nothing.
    let again = sb.run(&["--tidy-undo"]);
    assert!(text(&again).contains("nothing to put back"), "{}", text(&again));
}

#[test]
fn a_second_run_of_the_same_plan_is_safe() {
    let sb = Sandbox::new();
    let cv = sb.file("Documents/cv.docx", "cv");
    let folder = sb.path("Documents/Work");
    let plan = serde_json::json!([{ "folder": s(&folder), "files": [s(&cv)] }]);
    assert!(sb.tidy(plan.clone()).status.success());
    let events = EventStore::open_existing(&sb.db()).unwrap().get_all_events().unwrap().len();

    let out = sb.tidy(plan);
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("= the folder was already there") && said.contains("moved 0 file(s)"), "{said}");
    assert!(said.contains("nothing to undo"), "{said}");
    // No second registration: the only new event is THOR's own tidy record, which says 0 files moved.
    let store = EventStore::open_existing(&sb.db()).unwrap();
    let after = store.get_all_events().unwrap().len();
    let records: Vec<_> = model::store::order_evidence(&store).into_iter().filter(|e| e.kind == "tidy").collect();
    let seeded = model::store::show(&store, model::store::ORDER_NOTE_ID).is_ok();
    assert_eq!(after, events + usize::from(seeded), "events {events} -> {after}");
    if seeded {
        assert_eq!(records.last().unwrap().body["moved"], 0);
    }
    assert_eq!(std::fs::read_to_string(folder.join("cv.docx")).unwrap(), "cv");
    assert!(!folder.join("cv (2).docx").exists());
}

#[test]
fn a_broken_plan_changes_nothing() {
    let sb = Sandbox::new();
    let file = sb.file("plan.json", "{ not json");
    let out = sb.run(&["--tidy", "--plan", file.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(text(&out).contains("Nothing was changed"), "{}", text(&out));
}

#[test]
fn a_file_in_the_desktop_apps_scratch_workspace_is_moved_out_but_never_a_folder_made_there() {
    let sb = Sandbox::new();
    let scratch = "AppData/Roaming/Claude/scratch-workspaces/u1/u2/scratch-2026-03-01-abc123";
    let cv = sb.file(&format!("{scratch}/cv-draft.docx"), "cv");
    let other_appdata = sb.file("AppData/Local/Thing/state.db", "db");
    let folder = sb.path("Documents/Cv en werk zoeken");
    let out = sb.tidy(serde_json::json!([
        { "folder": s(&folder), "files": [s(&cv), s(&other_appdata)] },
        { "folder": s(&sb.path(&format!("{scratch}/new-folder"))), "files": [] },
    ]));
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("moved 1 file(s)"), "{said}");
    assert!(folder.join("cv-draft.docx").is_file() && !cv.exists());
    assert!(other_appdata.is_file(), "other application data is still left alone: {said}");
    assert!(said.contains("! REFUSED") && said.contains("application data"), "a folder inside AppData is still refused: {said}");
    assert!(!sb.path(&format!("{scratch}/new-folder")).exists());
}

/// A conversation without a folder often writes into Claude Code's own
/// per-session scratchpad under Temp, which a restart clears: such a file is
/// moved out too, while other files under Temp stay where they are.
#[test]
fn a_file_in_a_claude_scratchpad_under_temp_is_moved_out_and_other_temp_files_stay() {
    let sb = Sandbox::new();
    let pad = "AppData/Local/Temp/claude/C--Users-x-AppData-Roaming-Claude-scratch-workspaces-u1/s1/scratchpad";
    let letter = sb.file(&format!("{pad}/motivation-letter.md"), "letter");
    let other_temp = sb.file("AppData/Local/Temp/installer.log", "log");
    let folder = sb.path("Documents/Cv en werk zoeken");
    let out = sb.tidy(serde_json::json!([{ "folder": s(&folder), "files": [s(&letter), s(&other_temp)] }]));
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("moved 1 file(s)"), "{said}");
    assert!(folder.join("motivation-letter.md").is_file() && !letter.exists());
    assert!(other_temp.is_file(), "other files under Temp are left alone: {said}");
}
