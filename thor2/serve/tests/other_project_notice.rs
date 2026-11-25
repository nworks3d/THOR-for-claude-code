//! Work that belongs elsewhere gets its own session: against the REAL compiled
//! `serve` binary, a prompt that names another project with its own folder is
//! answered with one plain line for the assistant, and a session that opens in
//! a folder of no project is told to propose a folder. Nothing is said for the
//! session's own project or for a prompt that names none.

use model::item::{Binding, Item, Kind, Severity};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use thor_core::event_store::{EventKind, EventStore};

fn rule_for(project: &str) -> Item {
    Item {
        id: format!("{project}-rule"),
        kind: Kind::Rule,
        text: format!("standing rule for {project}"),
        bindings: vec![Binding::Always],
        severity: Some(Severity::Irreversible),
        project: Some(project.to_string()),
        tags: vec![format!("{}a test fixture where 'the same call' is also the legitimate one", model::store::NO_LITERAL_REASON_PREFIX)],
        expires: None,
        key: None,
        falsifier: Some(format!("the standing rule for {project} turns out to be wrong")),
        check: None,
    }
}

/// The installer's registration of `root` for `key`, as a raw event.
fn register(store: &mut EventStore, key: &str, root: &Path) {
    let record = Item {
        id: model::store::project_opened_id(key),
        kind: Kind::Report,
        text: format!("Project {key}: a fixture record."),
        bindings: Vec::new(),
        severity: None,
        project: Some(key.to_string()),
        tags: vec!["project-record".to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    let body = model::store::canonical_body(&record).unwrap();
    let body = format!(
        "{},\"root\":{},\"root_at\":\"2026-09-29T00:00:00Z\"}}",
        &body[..body.len() - 1],
        serde_json::to_string(root.to_str().unwrap()).unwrap()
    );
    store.append_event("install", "install", model::store::INSTALLER_ACTOR, EventKind::FactCreated, &record.id, None, &body).unwrap();
}

struct Fixture {
    _dir: tempfile::TempDir,
    db: PathBuf,
    kitchen: PathBuf,
    garden: PathBuf,
    nowhere: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let kitchen = dir.path().join("Kitchen-renovation");
    let garden = dir.path().join("Garden");
    let nowhere = dir.path().join("plain-folder");
    for repo in [&kitchen, &garden] {
        std::fs::create_dir_all(repo.join(".git")).unwrap();
    }
    std::fs::create_dir_all(&nowhere).unwrap();
    let db = dir.path().join("store.db");
    {
        let mut store = EventStore::new(&db).unwrap();
        for project in ["Kitchen-renovation", "Garden"] {
            model::store::declare(&mut store, "s", "l", "a", &rule_for(project)).unwrap();
        }
        register(&mut store, "Kitchen-renovation", &kitchen);
    }
    Fixture { _dir: dir, db, kitchen, garden, nowhere }
}

/// What the serve binary printed for one hook payload, run in `cwd` as an
/// interactive session of the app (`entrypoint`).
fn hook_raw(f: &Fixture, cwd: &Path, payload: serde_json::Value, entrypoint: &str) -> String {
    let mut payload = payload;
    payload["cwd"] = serde_json::json!(cwd.to_str().unwrap());
    if payload.get("session_id").is_none() {
        payload["session_id"] = serde_json::json!("other-project-test");
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(&f.db)
        .arg("hook")
        .env("CLAUDE_CODE_ENTRYPOINT", entrypoint)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn hook(f: &Fixture, cwd: &Path, payload: serde_json::Value) -> String {
    let text = hook_raw(f, cwd, payload, "claude-desktop");
    serde_json::from_str::<serde_json::Value>(text.trim())
        .ok()
        .and_then(|v| v["hookSpecificOutput"]["additionalContext"].as_str().map(str::to_string))
        .unwrap_or_default()
}

fn prompt(f: &Fixture, cwd: &Path, text: &str) -> String {
    hook(f, cwd, serde_json::json!({ "hook_event_name": "UserPromptSubmit", "prompt": text }))
}

#[test]
fn a_prompt_naming_another_project_with_its_own_folder_gets_one_plain_line() {
    let f = fixture();
    let context = prompt(&f, &f.garden, "what did we decide about the kitchen renovation budget?");
    assert!(context.contains("This message is about Kitchen-renovation"), "{context}");
    assert!(context.contains(&f.kitchen.display().to_string()), "the folder is named: {context}");
    assert!(context.contains("only clicks"), "{context}");
}

#[test]
fn the_same_name_in_another_spelling_is_still_that_project() {
    let f = fixture();
    let context = prompt(&f, &f.garden, "Open KITCHEN-RENOVATION again");
    assert!(context.contains("This message is about Kitchen-renovation"), "{context}");
}

#[test]
fn nothing_is_said_inside_that_project_or_for_a_prompt_that_names_none() {
    let f = fixture();
    let inside = prompt(&f, &f.kitchen, "what did we decide about the kitchen renovation budget?");
    assert!(!inside.contains("This message is about"), "{inside}");
    let none = prompt(&f, &f.garden, "please water the tomatoes in the workshop");
    assert!(!none.contains("This message is about"), "{none}");
}

#[test]
fn a_session_in_a_folder_of_no_project_is_told_to_propose_a_folder() {
    let f = fixture();
    let start = |cwd: &Path| hook(&f, cwd, serde_json::json!({ "hook_event_name": "SessionStart", "source": "startup" }));
    let nowhere = start(&f.nowhere);
    assert!(nowhere.contains("belongs to no project") && nowhere.contains("only clicks"), "{nowhere}");
    let inside = start(&f.garden);
    assert!(!inside.contains("belongs to no project"), "a folder of a project gets no such line: {inside}");
}

// ---------------------------------------------------------------------------
// The known folders, the line at the first prompt, and the one hold at Stop.

fn with_session(session: &str, mut payload: serde_json::Value) -> serde_json::Value {
    payload["session_id"] = serde_json::json!(session);
    payload
}

#[test]
fn the_known_folders_are_bounded_and_hold_a_registered_working_copy() {
    let f = fixture();
    let mut store = EventStore::new(&f.db).unwrap();
    let known = serve::other_project::known_folders(&store, serve::other_project::KNOWN_FOLDERS_MAX);
    assert!(known.iter().any(|k| k.project == "Kitchen-renovation" && k.folder == f.kitchen), "{known:?}");
    // Twelve more registered working copies: the list never passes its bound.
    for n in 0..12 {
        let name = format!("Project-number-{n:02}");
        let root = f._dir.path().join(&name);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        model::store::declare(&mut store, "s", "l", "a", &rule_for(&name)).unwrap();
        register(&mut store, &name, &root);
    }
    let known = serve::other_project::known_folders(&store, serve::other_project::KNOWN_FOLDERS_MAX);
    assert_eq!(known.len(), serve::other_project::KNOWN_FOLDERS_MAX, "{known:?}");
    assert_eq!(serve::other_project::known_folders(&store, 3).len(), 3);
    // Whatever the list, the line stays inside its reserve and names whole entries only.
    let line = serve::other_project::no_project_line_with(&known);
    assert!(line.len() <= model::item::NO_PROJECT_LINE_MAX_BYTES, "{} bytes: {line}", line.len());
    assert!(line.starts_with(serve::other_project::NO_PROJECT_PREFIX) && line.contains("Known folders: "), "{line}");
    assert!(serve::other_project::no_project_line().len() < model::item::NO_PROJECT_LINE_MAX_BYTES);
}

#[test]
fn a_session_start_in_a_folder_of_no_project_names_the_registered_folder() {
    let f = fixture();
    let start = hook(&f, &f.nowhere, serde_json::json!({ "hook_event_name": "SessionStart", "source": "startup" }));
    assert!(start.contains("Known folders: ") && start.contains("Kitchen-renovation = "), "{start}");
}

#[test]
fn a_resumed_conversation_gets_the_line_at_its_first_prompt_once() {
    let f = fixture();
    let ask = |cwd: &Path, session: &str| {
        hook(&f, cwd, with_session(session, serde_json::json!({ "hook_event_name": "UserPromptSubmit", "prompt": "please water the tomatoes" })))
    };
    let first = ask(&f.nowhere, "resumed-1");
    assert!(first.contains("belongs to no project") && first.contains("Kitchen-renovation = "), "{first}");
    let second = ask(&f.nowhere, "resumed-1");
    assert!(!second.contains("belongs to no project"), "once per session: {second}");
    // A session whose start already said it does not say it again at the prompt.
    let start = hook(&f, &f.nowhere, with_session("started-1", serde_json::json!({ "hook_event_name": "SessionStart", "source": "resume" })));
    assert!(start.contains("belongs to no project"), "{start}");
    assert!(!ask(&f.nowhere, "started-1").contains("belongs to no project"));
    // A folder of a project never gets it.
    assert!(!ask(&f.garden, "garden-1").contains("belongs to no project"));
}

/// A transcript of one turn: the owner's prompt, then the assistant's tool calls by name.
fn transcript(f: &Fixture, name: &str, tools: &[&str]) -> PathBuf {
    let mut lines = vec![serde_json::json!({ "type": "user", "message": { "role": "user", "content": "please sort out the budget" } }).to_string()];
    for (n, tool) in tools.iter().enumerate() {
        lines.push(
            serde_json::json!({ "type": "assistant", "message": { "role": "assistant", "content": [
                { "type": "tool_use", "id": format!("t{n}"), "name": tool, "input": { "command": "ls" } }
            ] } })
            .to_string(),
        );
    }
    let path = f._dir.path().join(format!("{name}.jsonl"));
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    path
}

fn stop(f: &Fixture, cwd: &Path, session: &str, transcript: &Path, extra: serde_json::Value) -> String {
    let mut payload = serde_json::json!({ "hook_event_name": "Stop", "session_id": session, "transcript_path": transcript.to_str().unwrap() });
    for (key, value) in extra.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    hook_raw(f, cwd, payload, "claude-desktop")
}

fn held(out: &str) -> bool {
    out.contains("\"decision\":\"block\"") && out.contains(serve::other_project::HOLD_MARKER)
}

#[test]
fn the_stop_hold_happens_once_per_session_at_the_first_turn_end_in_a_folder_of_no_project() {
    let f = fixture();
    let log = transcript(&f, "worked", &["Bash"]);
    let first = stop(&f, &f.nowhere, "hold-1", &log, serde_json::json!({}));
    assert!(held(&first), "{first}");
    assert!(first.contains("Kitchen-renovation = ") && first.contains("mcp__ccd_directory__change_directory") && first.contains("spawn_task"), "{first}");
    assert!(first.contains("systemMessage") && first.contains("answer yes or no"), "the person gets one plain sentence: {first}");
    // Never a second time in the same session, however many turns follow.
    for _ in 0..2 {
        let again = stop(&f, &f.nowhere, "hold-1", &log, serde_json::json!({}));
        assert!(!held(&again), "{again}");
    }
}

#[test]
fn a_first_turn_with_only_a_question_is_held_once_and_the_second_turn_is_not() {
    let f = fixture();
    let question = transcript(&f, "question", &[]);
    assert!(held(&stop(&f, &f.nowhere, "question-1", &question, serde_json::json!({}))));
    assert!(!held(&stop(&f, &f.nowhere, "question-1", &question, serde_json::json!({}))));
}

#[test]
fn the_stop_hold_never_comes_in_a_project_or_for_an_agent_or_a_headless_session() {
    let f = fixture();
    let log = transcript(&f, "worked", &["Bash"]);
    // A folder of a project.
    assert!(!held(&stop(&f, &f.garden, "in-project", &log, serde_json::json!({}))));
    // A subagent.
    assert!(!held(&stop(&f, &f.nowhere, "agent-1", &log, serde_json::json!({ "agent_id": "a1", "agent_type": "general-purpose" }))));
    // A headless session.
    let headless = hook_raw(
        &f,
        &f.nowhere,
        serde_json::json!({ "hook_event_name": "Stop", "session_id": "headless-1", "transcript_path": log.to_str().unwrap() }),
        "sdk-cli",
    );
    assert!(!held(&headless), "{headless}");
    // A transcript that is not there: nothing is read, nothing is held.
    assert!(!held(&stop(&f, &f.nowhere, "gone-1", &f._dir.path().join("missing.jsonl"), serde_json::json!({}))));
}

#[test]
fn the_stop_hold_is_not_made_when_the_conversation_was_already_moved_or_prepared() {
    let f = fixture();
    for (n, tool) in ["mcp__ccd_directory__change_directory", "mcp__ccd_directory__request_directory", "mcp__ccd_session__spawn_task"].iter().enumerate() {
        let log = transcript(&f, &format!("moved-{n}"), &["Bash", tool]);
        let out = stop(&f, &f.nowhere, &format!("moved-{n}"), &log, serde_json::json!({}));
        assert!(!held(&out), "{tool}: {out}");
    }
}

#[test]
fn the_off_switch_silences_the_prompt_line_and_the_hold_but_not_the_start_line() {
    let f = fixture();
    std::fs::write(f.db.with_file_name("no-project-ask-mode.json"), "{\"mode\": \"off\"}").unwrap();
    let log = transcript(&f, "worked", &["Bash"]);
    assert!(!held(&stop(&f, &f.nowhere, "off-1", &log, serde_json::json!({}))));
    let prompt = hook(&f, &f.nowhere, with_session("off-2", serde_json::json!({ "hook_event_name": "UserPromptSubmit", "prompt": "please water the tomatoes" })));
    assert!(!prompt.contains("belongs to no project"), "{prompt}");
    let start = hook(&f, &f.nowhere, serde_json::json!({ "hook_event_name": "SessionStart", "source": "startup" }));
    assert!(start.contains("belongs to no project"), "{start}");
}

#[test]
fn the_transcript_facts_count_this_turn_and_remember_a_move_from_any_turn() {
    let one = serde_json::json!({ "type": "user", "message": { "role": "user", "content": "first" } }).to_string();
    let call = |name: &str| serde_json::json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": name, "input": {} }] } }).to_string();
    let text = [one.clone(), call("Bash"), call("mcp__ccd_directory__change_directory"), one.clone(), call("Read")].join("\n");
    let facts = serve::other_project::transcript_facts(&text);
    assert!(facts.moved_or_prepared);
    assert_eq!(serve::other_project::transcript_facts("not json\n\n"), serve::other_project::TranscriptFacts::default());
}

// ---------------------------------------------------------------------------
// The first-session welcome already proposes the folder: no second question.

/// The install's own notes: the setup note (live or retracted, with the answers
/// on record) and the tidy step's note (live or not there).
fn install_notes(f: &Fixture, setup_live: bool, order_live: bool) {
    let mut store = EventStore::new(&f.db).unwrap();
    let note = |id: &str, kind: Kind| Item {
        id: id.to_string(),
        kind,
        text: "fixture note".to_string(),
        bindings: if kind == Kind::Report { vec![] } else { vec![Binding::Always] },
        severity: None,
        project: None,
        tags: vec!["working-contract".to_string()],
        expires: None,
        key: None,
        falsifier: Some("fixture falsifier".to_string()),
        check: None,
    };
    model::store::declare(&mut store, "install", "install", "install-fixture", &note(model::store::SETUP_NOTE_ID, Kind::Rule)).unwrap();
    if order_live {
        model::store::declare(&mut store, "install", "install", "install-fixture", &note(model::store::ORDER_NOTE_ID, Kind::Orientation)).unwrap();
    }
    if !setup_live {
        model::store::declare(&mut store, "install", "install", "install-fixture", &note(model::store::OWNER_SETUP_ANSWERS_ID, Kind::Report)).unwrap();
        model::store::retract(&mut store, "s", "l", "fixture", model::store::SETUP_NOTE_ID, "answered", None).unwrap();
    }
}

fn first_prompt(f: &Fixture, session: &str) -> String {
    hook(f, &f.nowhere, with_session(session, serde_json::json!({ "hook_event_name": "UserPromptSubmit", "prompt": "please water the tomatoes" })))
}

#[test]
fn the_beginner_flow_gets_no_second_folder_question() {
    let f = fixture();
    install_notes(&f, true, true);
    // The welcome conversation: the first prompt carries no no-project line either.
    assert!(!first_prompt(&f, "welcome-1").contains("belongs to no project"));
    // Stop 1: the setup hold. Stop 2 (the setup note is live or just retracted): no folder hold.
    let log = transcript(&f, "welcome", &["Bash"]);
    let first = stop(&f, &f.nowhere, "welcome-1", &log, serde_json::json!({}));
    assert!(first.contains("walked through setup") && !held(&first), "{first}");
    {
        let mut store = EventStore::new(&f.db).unwrap();
        let answers = Item {
            id: model::store::OWNER_SETUP_ANSWERS_ID.to_string(),
            kind: Kind::Report,
            text: "fixture note".to_string(),
            bindings: vec![],
            severity: None,
            project: None,
            tags: vec!["working-contract".to_string()],
            expires: None,
            key: None,
            falsifier: Some("fixture falsifier".to_string()),
            check: None,
        };
        model::store::declare(&mut store, "install", "install", "install-fixture", &answers).unwrap();
        model::store::retract(&mut store, "s", "l", "fixture", model::store::SETUP_NOTE_ID, "answered", None).unwrap();
    }
    // The tidy note is still live: no folder hold.
    assert!(!held(&stop(&f, &f.nowhere, "welcome-1", &log, serde_json::json!({}))));
    // The tidy note gone too, but this very session retracted the setup note: still none.
    {
        let mut store = EventStore::new(&f.db).unwrap();
        let _ = model::store::retract(&mut store, "s", "l", "fixture", model::store::ORDER_NOTE_ID, "settled", None);
    }
    let mut lines = std::fs::read_to_string(&log).unwrap();
    lines.push_str(&serde_json::json!({ "type": "assistant", "message": { "content": [
        { "type": "tool_use", "id": "r", "name": "mcp__thor__retract", "input": { "id": model::store::SETUP_NOTE_ID, "reason": "done" } }
    ] } }).to_string());
    lines.push('\n');
    std::fs::write(&log, lines).unwrap();
    assert!(!held(&stop(&f, &f.nowhere, "welcome-2", &log, serde_json::json!({}))));
}

#[test]
fn after_the_setup_is_finished_a_new_conversation_with_no_folder_is_held_once() {
    let f = fixture();
    install_notes(&f, false, false);
    assert!(first_prompt(&f, "later-1").contains("belongs to no project"));
    let log = transcript(&f, "later", &["Bash"]);
    assert!(held(&stop(&f, &f.nowhere, "later-1", &log, serde_json::json!({}))));
    assert!(!held(&stop(&f, &f.nowhere, "later-1", &log, serde_json::json!({}))));
}

#[test]
fn a_subject_with_notes_and_no_folder_is_offered_a_folder_once_per_session() {
    let f = fixture();
    let first = prompt(&f, &f.kitchen, "can you check what we know about the garden?");
    assert!(first.contains("'Garden' has 1 note but no folder of its own"), "{first}");
    assert!(first.contains("offer to give it a folder") && first.contains("only answer yes or no"), "{first}");
    assert!(first.contains("run the installer with --topic from inside it"), "{first}");
    let again = prompt(&f, &f.kitchen, "and the garden again, please");
    assert!(!again.contains("no folder of its own"), "once per session: {again}");
}

/// A note bound to its own project (not pinned) of the kitchen folder.
fn kitchen_note(f: &Fixture, id: &str, text: &str) {
    let mut store = EventStore::open_existing(&f.db).unwrap();
    let note = Item {
        id: id.to_string(),
        kind: Kind::Orientation,
        text: text.to_string(),
        bindings: vec![Binding::Target { kind: model::item::TargetKind::Project, value: "Kitchen-renovation".to_string() }],
        severity: None,
        project: Some("Kitchen-renovation".to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the tiles come from another shop".to_string()),
        check: None,
    };
    model::store::declare(&mut store, "s", "l", "a", &note).unwrap();
}

#[test]
fn a_project_bound_note_comes_up_at_a_prompt_that_shares_a_word_with_it_in_its_own_project_only() {
    let f = fixture();
    // The kitchen is a plain folder of its own (a topic folder), not a code repository.
    std::fs::remove_dir_all(f.kitchen.join(".git")).unwrap();
    std::fs::write(f.kitchen.join(".thor-project"), "Kitchen-renovation\n").unwrap();
    kitchen_note(&f, "kitchen-tiles", "The kitchen tiles come from Tilehouse and arrive on Tuesday.");
    let matching = prompt(&f, &f.kitchen, "when do the tiles arrive, do you know?");
    assert!(matching.contains("kitchen-tiles"), "{matching}");
    let unrelated = prompt(&f, &f.kitchen, "please water the tomatoes in the workshop");
    assert!(!unrelated.contains("kitchen-tiles"), "{unrelated}");
    // Another project's session never sees it, whatever the prompt says.
    let elsewhere = prompt(&f, &f.garden, "when do the tiles arrive, do you know?");
    assert!(!elsewhere.contains("kitchen-tiles"), "{elsewhere}");
}

#[test]
fn in_a_code_repository_the_project_binding_is_not_read_at_a_prompt() {
    let f = fixture();
    kitchen_note(&f, "kitchen-tiles", "The kitchen tiles come from Tilehouse and arrive on Tuesday.");
    let matching = prompt(&f, &f.kitchen, "when do the tiles arrive, do you know?");
    assert!(!matching.contains("kitchen-tiles"), "{matching}");
}

#[test]
fn a_subject_with_a_folder_is_never_called_folderless() {
    let f = fixture();
    let context = prompt(&f, &f.garden, "what did we decide about the kitchen renovation budget?");
    assert!(context.contains("This message is about Kitchen-renovation"), "{context}");
    assert!(!context.contains("no folder of its own"), "{context}");
    let none = prompt(&f, &f.kitchen, "please water the tomatoes in the workshop");
    assert!(!none.contains("no folder of its own"), "{none}");
}
