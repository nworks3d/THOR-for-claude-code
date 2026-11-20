//! The tidy step, end to end through the real `serve` program: a memory that
//! was stuck by the old behaviour (the person said yes, the tidy found nothing
//! to move, an earlier conversation ran in the desktop app's scratch workspace
//! without leaving any file). A plain `serve inventory` now finds nothing to
//! move and settles the step; a scratch conversation that still has an existing
//! file keeps the hold. Isolated: HOME and USERPROFILE point at a throwaway
//! folder.

use model::store::{ORDER_NOTE_ID, OWNER_SETUP_ANSWERS_ID, SETUP_NOTE_ID};
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use thor_core::event_store::EventStore;

const SESSION: &str = "11111111-2222-3333-4444-555555555555";

fn person(at: &str, text: &str, cwd: &str) -> String {
    serde_json::json!({"type":"user","timestamp":at,"cwd":cwd,"message":{"role":"user","content":text}}).to_string()
}

fn assistant(at: &str, text: &str, cwd: &str) -> String {
    serde_json::json!({"type":"assistant","timestamp":at,"cwd":cwd,"message":{"role":"assistant","content":[{"type":"text","text":text}]}}).to_string()
}

fn stuck_memory(scratch_keeps_a_file: bool) -> (tempfile::TempDir, PathBuf, PathBuf, String) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().to_path_buf();
    let db = home.join("thor.db");
    let projects = home.join(".claude").join("projects");
    let item = |id: &str, kind: model::item::Kind| model::item::Item {
        id: id.to_string(), kind, text: "fixture note".to_string(), bindings: vec![model::item::Binding::Always], severity: None, project: None,
        tags: vec![], expires: None, key: None, falsifier: Some("fixture falsifier".to_string()), check: None,
    };
    {
        let mut store = EventStore::new(&db).unwrap();
        model::store::declare(&mut store, "install", "install", "install-fixture", &item(SETUP_NOTE_ID, model::item::Kind::Rule)).unwrap();
        model::store::declare(&mut store, "install", "install", "install-fixture", &item(ORDER_NOTE_ID, model::item::Kind::Orientation)).unwrap();
        let mut answers = item(OWNER_SETUP_ANSWERS_ID, model::item::Kind::Report);
        answers.bindings = vec![];
        model::store::declare(&mut store, "install", "install", "install-fixture", &answers).unwrap();
        model::store::retract(&mut store, "s", "l", "fixture", SETUP_NOTE_ID, "answered", None).unwrap();
    }
    // The held session runs in a subject folder that already has its own memory.
    let subject = home.join("Documents").join("Cv");
    std::fs::create_dir_all(&subject).unwrap();
    std::fs::write(subject.join(".thor-project"), "Cv\n").unwrap();
    let cwd = subject.display().to_string();
    let inv = serde_json::json!({"type":"assistant","timestamp":"2026-03-02T10:03:00Z","cwd":cwd,"message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"\"C:/thor2/serve.exe\" --db x inventory"}}]}}).to_string();
    let lines = vec![
        person("2026-03-02T10:00:00Z", "Help me with my CV", &cwd),
        assistant("2026-03-02T10:01:00Z", "Welcome. Shall I set it up like this?", &cwd),
        person("2026-03-02T10:02:00Z", "ja", &cwd),
        serde_json::json!({"type":"assistant","timestamp":"2026-03-02T10:02:30Z","cwd":cwd,"message":{"role":"assistant","content":[{"type":"tool_use","name":"mcp__thor__remember","input":{"id":"owner-setup-answers","kind":"report","text":"x"}}]}}).to_string(),
        inv,
        assistant("2026-03-02T10:04:00Z", "Voorstel:\n1. CV en werk zoeken\nZal ik dat doen? Zeg ja.", &cwd),
        person("2026-03-02T10:05:00Z", "ja", &cwd),
    ];
    let held: PathBuf = projects.join("enc").join(format!("{SESSION}.jsonl"));
    std::fs::create_dir_all(held.parent().unwrap()).unwrap();
    std::fs::write(&held, lines.join("\n") + "\n").unwrap();

    // An earlier conversation of the desktop app with no folder, and no file written.
    let scratch = home.join("AppData").join("Roaming").join("Claude").join("local-agent-mode-sessions").join("scratch-workspaces").join("abc123");
    std::fs::create_dir_all(&scratch).unwrap();
    let other = projects.join("scratch").join("99999999-0000-0000-0000-000000000009.jsonl");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    let mut body = person("2026-02-01T09:00:00Z", "what is a good cv layout", &scratch.display().to_string()) + "
";
    if scratch_keeps_a_file {
        let kept = scratch.join("cv.md");
        std::fs::write(&kept, "cv").unwrap();
        let w = serde_json::json!({"type":"assistant","timestamp":"2026-02-01T09:01:00Z","cwd":scratch.display().to_string(),"message":{"role":"assistant","content":[{"type":"tool_use","name":"Write","input":{"file_path":kept.display().to_string(),"content":"x"}}]}});
        body.push_str(&(w.to_string() + "
"));
    }
    std::fs::write(&other, body).unwrap();
    (root, db, held, cwd)
}

fn run(root: &std::path::Path, db: &std::path::Path, args: &[&str], stdin: Option<String>) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(db).args(args).env("HOME", root).env("USERPROFILE", root).env_remove("CLAUDE_CODE_ENTRYPOINT");
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let _ = child.stdin.take().unwrap().write_all(stdin.unwrap_or_default().as_bytes());
    String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap()
}

fn stop(root: &std::path::Path, db: &std::path::Path, held: &std::path::Path, cwd: &str) -> String {
    run(
        root,
        db,
        &["hook"],
        Some(serde_json::json!({"hook_event_name":"Stop","session_id":SESSION,"stop_hook_active":false,"last_assistant_message":"",
            "transcript_path":held.display().to_string(),"cwd":cwd}).to_string()),
    )
}

fn live(db: &std::path::Path) -> bool {
    let store = EventStore::open_existing(db).unwrap();
    model::store::show(&store, ORDER_NOTE_ID).is_ok()
}

#[test]
fn a_plain_inventory_settles_a_stuck_memory_when_nothing_is_movable() {
    let (root, db, held, cwd) = stuck_memory(false);
    // Before the honest step: the old hold (no tidy record, yes answered).
    assert!(stop(root.path(), &db, &held, &cwd).contains("\"block\""));
    let inventory = run(root.path(), &db, &["inventory"], None);
    assert!(inventory.contains("CONVERSATIONS WITHOUT A FOLDER") && inventory.contains("NOTHING TO MOVE"), "{inventory}");
    assert!(inventory.contains("0 not in order yet"), "{inventory}");
    let said = stop(root.path(), &db, &held, &cwd);
    assert!(said.trim().is_empty(), "the hold is gone: {said}");
    assert!(!live(&db), "THOR retracted the note itself");
}

#[test]
fn a_scratch_conversation_with_a_file_that_still_exists_keeps_the_hold() {
    let (root, db, held, cwd) = stuck_memory(true);
    let inventory = run(root.path(), &db, &["inventory"], None);
    assert!(inventory.contains("AT RISK"), "{inventory}");
    assert!(stop(root.path(), &db, &held, &cwd).contains("\"block\""));
    assert!(live(&db));
}

// ------------------------------------------------ files that vanish behind the step's back

/// A session that wrote a loose file straight into Documents (the file exists).
fn with_a_loose_file(root: &std::path::Path, cwd: &str) -> PathBuf {
    let docs = root.join("Documents");
    std::fs::create_dir_all(&docs).unwrap();
    let file = docs.join("cv.md");
    std::fs::write(&file, "cv").unwrap();
    let w = serde_json::json!({"type":"assistant","timestamp":"2026-02-02T09:01:00Z","cwd":cwd,"message":{"role":"assistant","content":[{"type":"tool_use","name":"Write","input":{"file_path":file.display().to_string(),"content":"x"}}]}});
    let p = root.join(".claude").join("projects").join("loose").join("77777777-0000-0000-0000-000000000007.jsonl");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, person("2026-02-02T09:00:00Z", "write my cv", cwd) + "\n" + &w.to_string() + "\n").unwrap();
    file
}

/// An assistant that moves the loose files away and then runs `serve inventory`
/// gets NO "nothing to order": the files were in the snapshot taken at the seed
/// and no tidy moved them. The proposal route stays the exit.
#[test]
fn a_loose_file_moved_away_by_hand_does_not_settle_the_step() {
    let (root, db, held, cwd) = stuck_memory(false);
    let file = with_a_loose_file(root.path(), &cwd);
    serve::inventory::snapshot_loose_now(&db, root.path()).unwrap();
    assert!(serve::inventory::read_snapshot(&db).unwrap().iter().any(|p| p.ends_with("cv.md")));
    // The escape: mv the file into the subject folder (a plain move, no tidy).
    std::fs::rename(&file, root.path().join("Documents").join("Cv").join("cv.md")).unwrap();
    let inventory = run(root.path(), &db, &["inventory"], None);
    assert!(inventory.contains("NO LONGER THERE") && inventory.contains("cv.md"), "{inventory}");
    assert!(stop(root.path(), &db, &held, &cwd).contains("\"block\""), "no proof without the proposal route");
    assert!(live(&db));
}

/// A store seeded by 2.6.11 or older has no snapshot: it keeps today's behaviour,
/// so a memory already stuck is still freed by a plain inventory.
#[test]
fn a_store_without_a_snapshot_is_still_freed_by_a_plain_inventory() {
    let (root, db, held, cwd) = stuck_memory(false);
    let file = with_a_loose_file(root.path(), &cwd);
    assert!(serve::inventory::read_snapshot(&db).is_none());
    std::fs::rename(&file, root.path().join("Documents").join("Cv").join("cv.md")).unwrap();
    let inventory = run(root.path(), &db, &["inventory"], None);
    assert!(!inventory.contains("NO LONGER THERE"), "{inventory}");
    assert!(stop(root.path(), &db, &held, &cwd).trim().is_empty());
    assert!(!live(&db));
}

/// A file that a tidy run moved (it is in the undo log) counts as handled.
#[test]
fn a_file_moved_by_a_tidy_run_counts_as_handled() {
    let (root, db, held, cwd) = stuck_memory(false);
    let file = with_a_loose_file(root.path(), &cwd);
    serve::inventory::snapshot_loose_now(&db, root.path()).unwrap();
    let to = root.path().join("Documents").join("Cv").join("cv.md");
    std::fs::rename(&file, &to).unwrap();
    let log = serde_json::json!({"made":"2026-11-19T10:00:00Z","moves":[{"from":file.display().to_string(),"to":to.display().to_string()}],"folders":[]});
    std::fs::write(db.parent().unwrap().join("tidy-2026-11-19-1.json"), log.to_string()).unwrap();
    let inventory = run(root.path(), &db, &["inventory"], None);
    assert!(!inventory.contains("NO LONGER THERE"), "{inventory}");
    assert!(stop(root.path(), &db, &held, &cwd).trim().is_empty());
}

// ------------------------------------------------ the temporary workspace file that vanishes

fn scratch_file(root: &std::path::Path) -> PathBuf {
    root.join("AppData").join("Roaming").join("Claude").join("local-agent-mode-sessions").join("scratch-workspaces").join("abc123").join("cv.md")
}

/// The only file in the app's temporary workspace is removed by hand (rm) and the
/// inventory is run: with the install-time snapshot that is NOT "nothing to move".
#[test]
fn removing_the_only_workspace_file_by_hand_does_not_free_the_conversation() {
    let (root, db, held, cwd) = stuck_memory(true);
    let file = scratch_file(root.path());
    assert!(file.is_file());
    serve::inventory::snapshot_loose_now(&db, root.path()).unwrap();
    assert!(serve::inventory::read_snapshot(&db).unwrap().iter().any(|p| p.ends_with("cv.md")), "the workspace file is in the snapshot");
    std::fs::remove_file(&file).unwrap();
    let inventory = run(root.path(), &db, &["inventory"], None);
    assert!(inventory.contains("NO LONGER THERE") && !inventory.contains("NOTHING TO MOVE"), "{inventory}");
    assert!(stop(root.path(), &db, &held, &cwd).contains("\"block\""));
    assert!(live(&db));
}

/// A store seeded by 2.6.11 (no snapshot) keeps today's behaviour: the file is gone,
/// so there is nothing to move and the plain inventory frees the step.
#[test]
fn without_a_snapshot_a_vanished_workspace_file_still_frees_the_step() {
    let (root, db, held, cwd) = stuck_memory(true);
    std::fs::remove_file(scratch_file(root.path())).unwrap();
    let inventory = run(root.path(), &db, &["inventory"], None);
    assert!(inventory.contains("NOTHING TO MOVE"), "{inventory}");
    assert!(stop(root.path(), &db, &held, &cwd).trim().is_empty());
    assert!(!live(&db));
}
