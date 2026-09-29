//! The three hard guards that read only the TEXT of a call - the requires
//! guard, the command guard and the always-forbidden content guard - refuse
//! only for rules that apply to the session's own project, end to end
//! through the real compiled `serve hook` binary.
//!
//! MEASURED (2026-09-26, on a copy of a real store): a rule written for one
//! repository ("a pip install of the fork must carry --no-deps", scoped to
//! that repository) refused a plain `pip install` in a completely different
//! repository. Nothing in a command's text says which checkout it belongs
//! to, and the guard read every live item in the store. A false block is the
//! most expensive outcome this system has, so these tests pin both halves:
//! another project's rule stays silent, and the rule still refuses inside
//! its own project - and a global rule refuses everywhere.

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn run_hook(db: &Path, payload: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    String::from_utf8(out.stdout).unwrap()
}

fn blocked(out: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(out.trim()).ok().and_then(|v| v.get("decision").cloned())
        == Some(serde_json::json!("block"))
}

/// A working directory that resolves to `project` through its own marker file.
fn checkout(root: &Path, project: &str) -> PathBuf {
    let dir = root.join(project);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".thor-project"), format!("{project}\n")).unwrap();
    dir
}

fn bash(session: &str, cwd: &Path, command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Bash",
        "tool_input": {"command": command},
    })
    .to_string()
}

fn write_file(session: &str, cwd: &Path, file: &str, content: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Write",
        "tool_input": {"file_path": cwd.join(file).to_string_lossy(), "content": content},
    })
    .to_string()
}

fn rule(id: &str, project: Option<&str>, bindings: Vec<Binding>, check: Check) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("probe rule {id} for the project scope of the hard guards"),
        bindings,
        severity: Some(Severity::Costly),
        project: project.map(str::to_string),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the guard turns out to refuse nothing in its own project".to_string()),
        check: Some(check),
    }
}

fn command(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Command, value: value.to_string() }
}

fn store_with(dir: &Path, items: &[Item]) -> PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    for item in items {
        model::store::declare(&mut store, "s", "l", "a", &named(item)).unwrap_or_else(|e| panic!("{}: {e}", item.id));
    }
    db
}

fn gate_refusals(db: &Path, id: &str) -> usize {
    let store = thor_core::event_store::EventStore::open_existing(db).unwrap();
    store
        .get_events_by_entity(id)
        .unwrap()
        .iter()
        .filter(|e| e.kind == thor_core::event_store::EventKind::GateRefused)
        .count()
}

#[test]
fn a_requires_rule_from_another_project_never_refuses_an_ordinary_call_here() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(
        tmp.path(),
        &[rule(
            "fork-install-needs-no-deps",
            Some("printer-project"),
            vec![command("pip")],
            Check::Requires { when: "pip".to_string(), required: vec!["--no-deps".to_string()] },
        )],
    );
    let elsewhere = checkout(tmp.path(), "memory-project");
    let out = run_hook(&db, &bash("s1", &elsewhere, "pip install trimesh"));
    assert!(!blocked(&out), "another project's requires rule must not refuse a plain pip install here: {out}");
    assert_eq!(gate_refusals(&db, "fork-install-needs-no-deps"), 0, "nothing was refused, so nothing is recorded");
}

#[test]
fn a_requires_rule_still_refuses_inside_its_own_project_and_the_refusal_is_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(
        tmp.path(),
        &[rule(
            "fork-install-needs-no-deps",
            Some("printer-project"),
            vec![command("pip")],
            Check::Requires { when: "pip".to_string(), required: vec!["--no-deps".to_string()] },
        )],
    );
    let home = checkout(tmp.path(), "printer-project");
    let out = run_hook(&db, &bash("s1", &home, "pip install ./fork"));
    assert!(blocked(&out), "the rule must still refuse inside its own project: {out}");
    assert_eq!(gate_refusals(&db, "fork-install-needs-no-deps"), 1, "a requires refusal must leave a record in the log");
    let out = run_hook(&db, &bash("s1", &home, "pip install ./fork --no-deps"));
    assert!(!blocked(&out), "the satisfied form must pass: {out}");
}

#[test]
fn a_forbidden_command_rule_from_another_project_never_refuses_here() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(
        tmp.path(),
        &[rule(
            "fork-never-force-pushed",
            Some("printer-project"),
            vec![command("git push")],
            Check::Forbidden { literals: vec!["--force".to_string()] },
        )],
    );
    let elsewhere = checkout(tmp.path(), "memory-project");
    let out = run_hook(&db, &bash("s1", &elsewhere, "git push --force origin topic"));
    assert!(!blocked(&out), "another project's forbidden rule must not refuse here: {out}");
    let home = checkout(tmp.path(), "printer-project");
    let out = run_hook(&db, &bash("s1", &home, "git push --force origin main"));
    assert!(blocked(&out), "the rule must still refuse inside its own project: {out}");
}

#[test]
fn a_global_forbidden_command_rule_still_refuses_in_every_project() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(
        tmp.path(),
        &[rule(
            "never-make-a-repo-public",
            None,
            vec![command("gh repo edit")],
            Check::Forbidden { literals: vec!["--visibility public".to_string()] },
        )],
    );
    for project in ["memory-project", "printer-project"] {
        let cwd = checkout(tmp.path(), project);
        let out = run_hook(&db, &bash("s1", &cwd, "gh repo edit acme/x --visibility public"));
        assert!(blocked(&out), "a global rule must refuse in {project}: {out}");
    }
}

// ------------------------------------------------ a session that drifts
//
// D4: the guards use the session's project (`project::session_project`) -
// the folder's own, else the project the session last worked in. Before,
// they used the folder's answer alone, so one `cd` into a folder of no
// project switched off every guard of the project the session was working in.
// These tests cover the guards that read only the text of a call; the
// path-anchored half (the root after a drift is the project's registered
// checkout, `project::guard_root`) is
// `absent_check_guard.rs::a_session_that_drifted_out_of_its_project_keeps_its_file_guards`.

const SESSION: &str = "5f0c3a2e-8d41-4b7a-9e0f-2c6d8b1a7e53";

/// The work clock stamps whole seconds, so two projects touched in the same
/// second tie; a switch waits for the next one.
fn next_second() {
    std::thread::sleep(std::time::Duration::from_millis(1100));
}

fn two_projects_with_push_rules(dir: &Path) -> PathBuf {
    store_with(
        dir,
        &[
            rule(
                "alpha-never-pushes",
                Some("alpha"),
                vec![command("git push")],
                Check::Forbidden { literals: vec!["git push".to_string()] },
            ),
            rule(
                "beta-never-force-pushes",
                Some("beta"),
                vec![command("git push")],
                Check::Forbidden { literals: vec!["--force".to_string()] },
            ),
            rule(
                "never-make-a-repo-public",
                None,
                vec![command("gh repo edit")],
                Check::Forbidden { literals: vec!["--visibility public".to_string()] },
            ),
        ],
    )
}

fn reason(out: &str) -> String {
    serde_json::from_str::<serde_json::Value>(out.trim()).unwrap_or_default()["reason"].as_str().unwrap_or("").to_string()
}

#[test]
fn a_session_that_drifted_out_of_its_project_keeps_its_guards() {
    let tmp = tempfile::tempdir().unwrap();
    let db = two_projects_with_push_rules(tmp.path());
    let alpha = checkout(tmp.path(), "alpha");
    let beta = checkout(tmp.path(), "beta");
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();

    assert!(!blocked(&run_hook(&db, &bash(SESSION, &alpha, "git status"))));
    let out = run_hook(&db, &bash(SESSION, &scratch, "git push origin main"));
    assert!(blocked(&out), "a cd into a folder of no project must keep alpha's guards (R4.3): {out}");
    assert!(reason(&out).contains("alpha-never-pushes"), "{out}");
    assert_eq!(gate_refusals(&db, "alpha-never-pushes"), 1, "the refusal is recorded like any other");

    // G4.1: a folder of ANOTHER project is judged under that project's rules.
    next_second();
    let out = run_hook(&db, &bash(SESSION, &beta, "git push origin main"));
    assert!(!blocked(&out), "alpha's rule must not follow the session into beta: {out}");
    let out = run_hook(&db, &bash(SESSION, &beta, "git push --force origin main"));
    assert!(blocked(&out) && reason(&out).contains("beta-never-force-pushes"), "beta's own rule applies there: {out}");

    // Drifting out of beta keeps beta's rules, not alpha's.
    let out = run_hook(&db, &bash(SESSION, &scratch, "git push origin main"));
    assert!(!blocked(&out), "the session last worked in beta: {out}");
    let out = run_hook(&db, &bash(SESSION, &scratch, "git push --force origin main"));
    assert!(blocked(&out) && reason(&out).contains("beta-never-force-pushes"), "{out}");

    // Back to alpha well inside the work clock's one-minute write throttle:
    // the switch back is on disk at once, so drifting out keeps alpha again.
    next_second();
    assert!(blocked(&run_hook(&db, &bash(SESSION, &alpha, "git push origin main"))));
    let out = run_hook(&db, &bash(SESSION, &scratch, "git push origin main"));
    assert!(blocked(&out) && reason(&out).contains("alpha-never-pushes"), "back in alpha, then out again: {out}");

    // A global rule applies wherever the session stands.
    let out = run_hook(&db, &bash(SESSION, &scratch, "gh repo edit acme/x --visibility public"));
    assert!(blocked(&out) && reason(&out).contains("never-make-a-repo-public"), "{out}");
}

#[test]
fn a_session_that_never_had_a_project_gets_only_global_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let db = two_projects_with_push_rules(tmp.path());
    let alpha = checkout(tmp.path(), "alpha");
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();

    // Another session worked in alpha; this one never did.
    assert!(!blocked(&run_hook(&db, &bash(SESSION, &alpha, "git status"))));
    let other = "0d9e7a61-3b2c-4f58-a1e4-7c5b9d2f8a06";
    let out = run_hook(&db, &bash(other, &scratch, "git push origin main"));
    assert!(!blocked(&out), "a session with no project never gets alpha's rules: {out}");
    let out = run_hook(&db, &bash(other, &scratch, "git push --force origin main"));
    assert!(!blocked(&out), "nor beta's: {out}");
    let out = run_hook(&db, &bash(other, &scratch, "gh repo edit acme/x --visibility public"));
    assert!(blocked(&out), "a global rule still refuses: {out}");
    assert_eq!(gate_refusals(&db, "alpha-never-pushes"), 0);
}

#[test]
fn an_always_forbidden_text_from_another_project_never_refuses_a_write_here() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(
        tmp.path(),
        &[rule(
            "no-customer-code-in-files",
            Some("shop-project"),
            vec![Binding::Always],
            Check::Forbidden { literals: vec!["CUST-SECRET-42".to_string()] },
        )],
    );
    let elsewhere = checkout(tmp.path(), "memory-project");
    let out = run_hook(&db, &write_file("s1", &elsewhere, "notes.md", "a test fixture: CUST-SECRET-42\n"));
    assert!(!blocked(&out), "another project's forbidden text must not refuse a write here: {out}");
    let home = checkout(tmp.path(), "shop-project");
    let out = run_hook(&db, &write_file("s1", &home, "notes.md", "a note: CUST-SECRET-42\n"));
    assert!(blocked(&out), "the rule must still refuse inside its own project: {out}");
}

// ------------------------------------------- each segment in its own project
//
// 5c (the door): the text guards no longer pre-filter by the session's
// project; each segment of a call is judged under the project it runs in
// (`guard_verdict::segment_projects`). A `cd` into another checkout the store
// knows moves the segment there, a `cd` into a folder of no known project keeps
// the session's, and a network path is never walked.

/// A checkout that also carries an (empty) `.git` folder, so `GIT_DIR` and
/// `git -C` can point at it. No git runs here.
fn repo(root: &Path, project: &str) -> PathBuf {
    let dir = checkout(root, project);
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    dir
}

#[test]
fn a_cd_into_another_checkout_is_judged_by_that_checkouts_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let db = two_projects_with_push_rules(tmp.path());
    let alpha = repo(tmp.path(), "alpha");
    repo(tmp.path(), "beta");
    // From alpha, into beta: alpha's push ban is not beta's business ...
    let out = run_hook(&db, &bash("s-5c", &alpha, "cd ../beta && git push origin main"));
    assert!(!blocked(&out), "a push run in beta is judged by beta's rules: {out}");
    // ... and beta's own rule applies there.
    let out = run_hook(&db, &bash("s-5c", &alpha, "cd ../beta && git push --force origin main"));
    assert!(blocked(&out) && reason(&out).contains("beta-never-force-pushes"), "{out}");
    // The same session pushing alpha itself is still refused.
    let out = run_hook(&db, &bash("s-5c", &alpha, "cd ../beta && git -C ../alpha push"));
    assert!(blocked(&out) && reason(&out).contains("alpha-never-pushes"), "git -C names the folder: {out}");
    let out = run_hook(&db, &bash("s-5c", &alpha, "cd ../beta && GIT_DIR=../alpha/.git git push"));
    assert!(blocked(&out) && reason(&out).contains("alpha-never-pushes"), "GIT_DIR names the folder: {out}");
    let out = run_hook(&db, &bash("s-5c", &alpha, "cd ../beta && git --git-dir=../alpha/.git push"));
    assert!(blocked(&out) && reason(&out).contains("alpha-never-pushes"), "--git-dir names the folder: {out}");
}

/// A folder of no project, a real scratch repository (`git init` gives it a
/// `.git`, so its folder name resolves as a project nobody ever named), a
/// folder whose marker names a project this store does not know, a folder
/// that is not there, and every folder no reader can resolve all keep the
/// session's project: none of them silences alpha's push ban.
#[test]
fn a_cd_into_no_project_or_a_network_path_keeps_the_sessions_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let db = two_projects_with_push_rules(tmp.path());
    let alpha = repo(tmp.path(), "alpha");
    repo(tmp.path(), "beta");
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let scratch_repo = tmp.path().join("scratch-repo");
    std::fs::create_dir_all(scratch_repo.join(".git")).unwrap();
    let stranger = repo(tmp.path(), "gamma");
    let text = |p: &Path| p.to_string_lossy().replace('\\', "/");
    for command in [
        format!("cd {} && git push origin main", text(&scratch)),
        format!("cd {} && git push origin main", text(&scratch_repo)),
        format!("cd {} && git push origin main", text(&stranger)),
        "cd ../beta/not-there; git push origin main".to_string(),
        "cd //nas-share/docker/x && git push origin main".to_string(),
        "cd $HOME/x && git push origin main".to_string(),
        "cd ~/x && git push origin main".to_string(),
        "cd - && git push origin main".to_string(),
    ] {
        let out = run_hook(&db, &bash("s-5c", &alpha, &command));
        assert!(blocked(&out) && reason(&out).contains("alpha-never-pushes"), "{command} must keep alpha's rules: {out}");
    }
}

/// The measured false block (2026-09-26) still holds per segment: another
/// project's requires rule never reaches a call, whatever folder the call
/// names, unless the segment really runs in that project.
#[test]
fn the_pip_no_deps_case_still_holds_per_segment() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store_with(
        tmp.path(),
        &[rule(
            "fork-install-needs-no-deps",
            Some("printer-project"),
            vec![command("pip")],
            Check::Requires { when: "pip".to_string(), required: vec!["--no-deps".to_string()] },
        )],
    );
    let elsewhere = checkout(tmp.path(), "memory-project");
    checkout(tmp.path(), "printer-project");
    let out = run_hook(&db, &bash("s1", &elsewhere, "cd /tmp && pip install trimesh"));
    assert!(!blocked(&out), "a folder of no project keeps the session's project: {out}");
    let out = run_hook(&db, &bash("s1", &elsewhere, "cd ../printer-project && pip install ./fork"));
    assert!(blocked(&out), "a segment run in the printer project owes that project's answer: {out}");
    let out = run_hook(&db, &bash("s1", &elsewhere, "cd ../printer-project && pip install ./fork --no-deps"));
    assert!(!blocked(&out), "{out}");
}

/// A `cd` the shell refuses and a `--work-tree` both leave a push in alpha
/// (measured against Git Bash 5.3, 2026-09-28: `cd ../beta extra` fails with
/// "too many arguments", and `git --work-tree=../beta rev-parse --git-dir`
/// names alpha's `.git`), so alpha's push ban refuses them.
#[test]
fn a_refused_cd_or_a_work_tree_keeps_the_sessions_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let db = two_projects_with_push_rules(tmp.path());
    let alpha = repo(tmp.path(), "alpha");
    repo(tmp.path(), "beta");
    for command in [
        "cd ../beta extra; git push origin main",
        "cd -x ../beta; git push origin main",
        "git --work-tree=../beta push origin main",
        "GIT_WORK_TREE=../beta git push origin main",
    ] {
        let out = run_hook(&db, &bash("s-5c", &alpha, command));
        assert!(blocked(&out) && reason(&out).contains("alpha-never-pushes"), "{command} pushes alpha: {out}");
    }
    let out = run_hook(&db, &bash("s-5c", &alpha, "cd -P ../beta; git push origin main"));
    assert!(!blocked(&out), "a cd bash takes still moves the push into beta: {out}");
}

/// `item` with every check literal its text does not name appended to the
/// text (in `U+XXXX` form for a glyph): since step 8 of the two tracks the
/// write gate refuses a check whose literal the rule's own words never name
/// (A4), and these fixtures are about the hook, not about naming.
fn named(item: &model::item::Item) -> model::item::Item {
    let mut out = item.clone();
    let unnamed = model::gate::unnamed_literals(&out);
    if !unnamed.is_empty() {
        let shown: Vec<String> = unnamed.iter().map(|l| format!("'{}'", model::gate::shown_literal(l))).collect();
        out.text = format!("{} (names {})", out.text, shown.join(" "));
    }
    out
}
