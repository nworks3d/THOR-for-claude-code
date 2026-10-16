//! `install --copy-from`: set THOR up on a SECOND machine in one command.
//!
//! The second machine does not get a memory of its own. It gets a read-only
//! COPY of the main machine's memory (see `thor_core::copy_mode`), optionally
//! only a selection of it, built from the main machine's backup. This module
//! does the whole job in order and says what it did in plain words:
//!
//! 1. checks nothing on this machine is replaced (a memory that is not a copy
//!    is never touched),
//! 2. builds the copy from the backup with `pull` and writes the marker that
//!    makes every hook read-only,
//! 3. connects THOR's four hooks to Claude Code,
//! 4. writes the small file that keeps replies tidy.
//!
//! It deliberately does NOT register the tool server that saves notes (a note
//! written on a copy would fork the log), the machine-wide git hooks, or the
//! `/thor-eval` command (the weekly check runs on the main machine). A failed
//! first build leaves nothing behind.

use crate::install::{install_hooks, seed_response_rulebook, standard_hooks, HookOutcome};
use crate::pull::{pull, PullOptions, PullOutcome};
use std::path::PathBuf;
use thor_core::copy_mode::{self, Selection};

pub struct Request {
    /// Where the copy lives (its `thor.db`).
    pub db: PathBuf,
    /// The backup: a folder, a bare repository or an address.
    pub source: String,
    pub scopes: Vec<String>,
    pub include_global: bool,
    /// Hold the whole memory (no selection).
    pub whole: bool,
    /// The folder the command was run from. It names the project when no scope is given.
    pub folder: PathBuf,
    /// Claude Code's settings file.
    pub settings: PathBuf,
    /// The `serve` program the hooks call.
    pub serve_exe: PathBuf,
}

fn selection_for(req: &Request) -> Result<(Selection, Vec<String>), String> {
    let mut notes = Vec::new();
    if req.whole {
        notes.push("= the whole memory was asked for: nothing is left out of the copy".to_string());
        return Ok((Selection::default(), notes));
    }
    if !req.scopes.is_empty() {
        return Ok((Selection { scopes: req.scopes.clone(), include_global: req.include_global }, notes));
    }
    match serve::project::resolve_project(&req.folder) {
        Some(key) => {
            notes.push(format!(
                "= you did not name a project, so this copy holds the project of the folder you ran this in: '{key}' (add --scope <name> to choose another)"
            ));
            if !req.include_global {
                notes.push("= the general work rules are not included (add --include-global to get them too)".to_string());
            }
            Ok((Selection { scopes: vec![key], include_global: req.include_global }, notes))
        }
        None if req.include_global => {
            notes.push(
                "= this folder is not a project (no git repository, no topic marker), so this copy holds only the general work rules. To get a project too, run this from the project's folder or add --scope <name>"
                    .to_string(),
            );
            Ok((Selection { scopes: vec![], include_global: true }, notes))
        }
        None => Err(
            "Nothing was changed. I could not tell which part of the memory this machine should get. Say it with --scope <project name> (repeat it for more projects) and add --include-global if you also want the general work rules, or add --whole-memory to copy everything."
                .to_string(),
        ),
    }
}

/// How many live items of each selected project (and general ones) the copy holds.
fn count_items(db: &std::path::Path, selection: &Selection) -> (usize, Vec<(String, usize)>, usize) {
    let Ok(store) = thor_core::event_store::EventStore::open_read_only(db) else { return (0, Vec::new(), 0) };
    let items = serve::live::live_items(&store);
    let per_scope = selection
        .scopes
        .iter()
        .map(|scope| {
            let wanted = model::normalize::normalize_project(Some(scope));
            let n = items.iter().filter(|i| model::normalize::normalize_project(i.item.project.as_deref()) == wanted).count();
            (scope.clone(), n)
        })
        .collect();
    let general = items.iter().filter(|i| model::normalize::normalize_project(i.item.project.as_deref()).is_none()).count();
    (items.len(), per_scope, general)
}

/// Run the setup. `Ok` carries the lines to print; `Err` one plain message
/// that says what was and was not changed.
pub fn run(req: &Request) -> Result<Vec<String>, String> {
    let marker_there = copy_mode::marker_path(&req.db).is_file();
    if req.db.is_file() && !marker_there {
        return Err(format!(
            "Nothing was changed. This machine already has a memory of its own at {}, and it is not a copy, so I will not replace it. If it is only the empty memory a first install made, move it out of the way (rename the file, do not delete it) and run this again. If it holds notes you wrote, ask the owner of those notes first.",
            req.db.display()
        ));
    }
    if !req.serve_exe.exists() {
        return Err(format!(
            "Nothing was changed. The program {} is missing, and THOR cannot work without it. Unpack the whole download into one folder and run install from there.",
            req.serve_exe.display()
        ));
    }
    let (selection, mut lines) = selection_for(req)?;
    if let Some(parent) = req.db.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("Nothing was changed. The folder {} could not be made: {e}", parent.display()))?;
    }

    if let Some(parent) = req.settings.parent().filter(|p| !p.as_os_str().is_empty()) {
        // Claude Code's folder may not exist yet on a machine where it never ran.
        let _ = std::fs::create_dir_all(parent);
    }
    lines.insert(0, "Setting THOR up as a read-only copy of another machine's memory.".to_string());
    lines.insert(1, String::new());
    let options = PullOptions {
        repo: Some(PathBuf::from(&req.source)),
        scopes: selection.scopes.clone(),
        include_global: selection.include_global,
        whole: req.whole,
        ..Default::default()
    };
    let started = std::time::Instant::now();
    let outcome = match pull(&req.db, &options) {
        Ok(outcome) => outcome,
        Err(e) => {
            if !marker_there && !req.db.is_file() {
                // A first build that failed leaves nothing behind, so a second try starts clean.
                let _ = std::fs::remove_file(copy_mode::marker_path(&req.db));
            }
            return Err(format!(
                "The copy could not be made, so nothing was set up. Reason: {e:#}. Check that the backup is reachable from this machine (see \"What you need first\" in the README section \"Use THOR on a second machine\"), then run the same command again; it is safe to repeat."
            ));
        }
    };
    let (total, per_scope, general) = count_items(&req.db, &selection);
    match outcome {
        PullOutcome::Updated { applied, .. } => lines.push(format!(
            "+ made the copy from the backup: {total} notes in it, {applied} records written ({} ms)",
            started.elapsed().as_millis()
        )),
        PullOutcome::UpToDate { .. } => lines.push(format!("= the copy was already up to date: {total} notes in it")),
    }
    lines.push(format!("  what it holds: {}", selection.describe()));
    for (scope, n) in &per_scope {
        if *n == 0 {
            lines.push(format!(
                "  ! the project '{scope}' has no notes in the backup. Check the spelling (it must be the project's name exactly as the main machine uses it), then run this again with the right name."
            ));
        } else {
            lines.push(format!("  project '{scope}': {n} notes"));
        }
    }
    if selection.include_global {
        lines.push(format!("  general work rules: {general} notes"));
    }
    if selection.is_active() {
        lines.push("  every other project, and your personal library, were left out of the copy file on purpose".to_string());
    }

    let specs = standard_hooks(&req.serve_exe.display().to_string(), &req.db.display().to_string());
    match install_hooks(&req.settings, &specs) {
        Ok(report) => {
            let changed = report.results.iter().filter(|(_, o)| *o != HookOutcome::AlreadyPresent).count();
            if changed == 0 {
                lines.push("= THOR was already connected to Claude Code".to_string());
            } else {
                lines.push(format!("+ connected THOR to Claude Code ({changed} of {} connections), so the memory speaks up by itself", report.results.len()));
            }
            if let Some(p) = report.backup_path.as_ref().filter(|_| changed > 0) {
                lines.push(format!("  your previous Claude Code settings were saved as {}", p.display()));
            }
        }
        Err(e) => {
            return Err(format!(
                "The copy was made, but connecting THOR to Claude Code failed: {e}. Fix the problem named there, then run the same command again; it is safe to repeat."
            ))
        }
    }
    match seed_response_rulebook(&req.db) {
        Ok(_) => lines.push("+ the small file that keeps your assistant's replies tidy is in place".to_string()),
        Err(e) => lines.push(format!("  ! could not write the reply-tidying file: {e}")),
    }

    lines.push(String::new());
    lines.push("What you have now".to_string());
    lines.push(format!("  the copy:       {}", req.db.display()));
    lines.push("  refreshed:      by itself, in the background, at the start of a Claude Code session (at most once an hour)".to_string());
    lines.push("What is read-only here".to_string());
    lines.push("  - nothing in this copy can be changed from this machine: your assistant reads it, and rules still fire and still refuse".to_string());
    lines.push("  - notes are saved on the main machine, and show up here after the next refresh".to_string());
    lines.push("  - the weekly check runs on the main machine only".to_string());
    lines.push(String::new());
    lines.push("Done. ONE THING LEFT FOR YOU: close Claude Code completely and open it again.".to_string());
    lines.push("Claude Code only learns about THOR when it starts, so until you do this nothing above is active.".to_string());
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Item, Kind};
    use thor_core::backup::export_segments;
    use thor_core::event_store::EventStore;

    fn rule(id: &str, project: Option<&str>, words: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("{id} {words}"),
            bindings: vec![Binding::Always],
            severity: None,
            project: project.map(str::to_string),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("it stops being true".to_string()),
            check: None,
        }
    }

    struct Setup {
        dir: tempfile::TempDir,
        req: Request,
    }

    fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let mut main = EventStore::new(&dir.path().join("main.db")).unwrap();
        for item in [
            rule("alpha-one", Some("alpha"), "guards the alpha pipeline every single release"),
            rule("beta-one", Some("beta"), "CANARYBETASETUP describes the beta customer contract"),
            rule("general-one", None, "never commit credentials into any repository whatsoever"),
        ] {
            model::store::declare(&mut main, "s", "s", "s", &item).unwrap();
        }
        let backup = dir.path().join("backup");
        std::fs::create_dir_all(backup.join("thor2")).unwrap();
        export_segments(&main, &backup.join("thor2"), 2_000).unwrap();
        let serve_exe = dir.path().join("bin").join("serve.exe");
        std::fs::create_dir_all(serve_exe.parent().unwrap()).unwrap();
        std::fs::write(&serve_exe, b"not a real program").unwrap();
        let req = Request {
            db: dir.path().join("laptop").join("thor.db"),
            source: backup.display().to_string(),
            scopes: vec!["alpha".into()],
            include_global: true,
            whole: false,
            folder: dir.path().join("somewhere"),
            settings: dir.path().join("claude").join("settings.json"),
            serve_exe,
        };
        Setup { dir, req }
    }

    #[test]
    fn one_run_builds_the_selective_copy_connects_the_hooks_and_explains_itself() {
        let s = setup();
        let lines = run(&s.req).unwrap();
        let said = lines.join("\n");
        assert!(said.contains("what it holds: project alpha, general rules (no project)"), "{said}");
        assert!(said.contains("project 'alpha': 1 notes"), "{said}");
        assert!(said.contains("general work rules: 1 notes"), "{said}");
        assert!(said.contains("left out of the copy file on purpose"), "{said}");
        assert!(said.contains("What is read-only here"), "{said}");
        assert!(said.contains("close Claude Code completely"), "{said}");
        assert!(copy_mode::is_copy(&s.req.db));
        let marker = copy_mode::read_marker(&s.req.db);
        assert_eq!(marker.scopes, vec!["alpha".to_string()]);
        let settings = std::fs::read_to_string(&s.req.settings).unwrap();
        assert!(settings.contains("SessionStart") && settings.contains("PreToolUse") && settings.contains("Stop"));
        assert!(!said.contains("CANARYBETASETUP") && !said.contains("beta"));
        let bytes = std::fs::read(&s.req.db).unwrap();
        assert!(!bytes.windows(13).any(|w| w == b"CANARYBETASET"), "the excluded note is not in the copy file");
        // No tool server, no git hooks: nothing that could write to the copy.
        assert!(!s.dir.path().join("claude").join("claude.json").exists());
    }

    #[test]
    fn a_second_run_changes_nothing_and_says_so() {
        let s = setup();
        run(&s.req).unwrap();
        let before = std::fs::read(&s.req.db).unwrap();
        let again = run(&s.req).unwrap().join("\n");
        assert!(again.contains("already up to date"), "{again}");
        assert!(again.contains("already connected"), "{again}");
        assert!(before == std::fs::read(&s.req.db).unwrap());
    }

    #[test]
    fn a_memory_that_is_not_a_copy_is_never_replaced() {
        let s = setup();
        std::fs::create_dir_all(s.req.db.parent().unwrap()).unwrap();
        EventStore::new(&s.req.db).unwrap();
        let bytes = std::fs::read(&s.req.db).unwrap();
        let err = run(&s.req).unwrap_err();
        assert!(err.contains("Nothing was changed") && err.contains("not a copy"), "{err}");
        assert!(bytes == std::fs::read(&s.req.db).unwrap());
        assert!(!s.req.settings.exists(), "no hook was written");
    }

    #[test]
    fn a_backup_that_cannot_be_read_leaves_nothing_behind_and_says_how_to_retry() {
        let mut s = setup();
        s.req.source = s.dir.path().join("no-such-backup").display().to_string();
        let err = run(&s.req).unwrap_err();
        assert!(err.contains("nothing was set up") && err.contains("safe to repeat"), "{err}");
        assert!(!s.req.db.exists());
        assert!(!copy_mode::marker_path(&s.req.db).exists(), "a failed first build is not left marked");
        assert!(!s.req.settings.exists());
    }

    #[test]
    fn without_a_scope_the_folders_project_is_used_and_said() {
        let mut s = setup();
        s.req.scopes.clear();
        s.req.include_global = false;
        let folder = s.dir.path().join("alpha");
        std::fs::create_dir_all(folder.join(".git")).unwrap();
        s.req.folder = folder;
        let said = run(&s.req).unwrap().join("\n");
        assert!(said.contains("the project of the folder you ran this in: 'alpha'"), "{said}");
        assert_eq!(copy_mode::read_marker(&s.req.db).scopes, vec!["alpha".to_string()]);
    }

    #[test]
    fn the_general_switch_alone_adds_to_the_folders_project_and_a_plain_folder_gets_only_the_general_rules() {
        let mut s = setup();
        s.req.scopes.clear();
        s.req.include_global = true;
        let folder = s.dir.path().join("alpha");
        std::fs::create_dir_all(folder.join(".git")).unwrap();
        s.req.folder = folder;
        let said = run(&s.req).unwrap().join("\n");
        assert!(said.contains("what it holds: project alpha, general rules (no project)"), "{said}");

        let mut plain = setup();
        plain.req.scopes.clear();
        plain.req.include_global = true;
        let said = run(&plain.req).unwrap().join("\n");
        assert!(said.contains("what it holds: general rules (no project)"), "{said}");
        assert!(said.contains("this folder is not a project"), "{said}");
    }

    #[test]
    fn nothing_to_go_on_is_refused_with_the_three_ways_out() {
        let mut s = setup();
        s.req.scopes.clear();
        s.req.include_global = false;
        let err = run(&s.req).unwrap_err();
        assert!(err.contains("--scope") && err.contains("--include-global") && err.contains("--whole-memory"), "{err}");
        assert!(!s.req.db.exists() && !s.req.settings.exists(), "nothing was changed");
    }

    #[test]
    fn a_misspelled_project_is_called_out() {
        let mut s = setup();
        s.req.scopes = vec!["alfa".into()];
        let said = run(&s.req).unwrap().join("\n");
        assert!(said.contains("'alfa' has no notes in the backup"), "{said}");
    }

    #[test]
    fn the_whole_memory_can_still_be_asked_for() {
        let mut s = setup();
        s.req.scopes.clear();
        s.req.include_global = false;
        s.req.whole = true;
        let said = run(&s.req).unwrap().join("\n");
        assert!(said.contains("nothing is left out"), "{said}");
        assert!(!copy_mode::read_marker(&s.req.db).selection().is_active());
    }
}
