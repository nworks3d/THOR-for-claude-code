//! The tidy step of a first session must be settleable by an honest run, and
//! must never stand in the way of the weekly check. Field report: a person said
//! yes, the assistant ran the tidy, everything was already in its folder, and
//! THOR held every turn for ever. Fully isolated: a throwaway home folder and
//! store, no settings file, no real home folder.

use ops::install::{ensure_and_seed_store, record_topic_opened, topic_of, write_project_marker};
use serve::order_earlier::{judge, runs_inventory, timeline_of, Entry, Verdict};
use std::path::{Path, PathBuf};
use thor_core::event_store::EventStore;

fn person(at: &str, text: &str) -> String {
    serde_json::json!({"type":"user","timestamp":at,"message":{"role":"user","content":text}}).to_string()
}

fn assistant_text(at: &str, text: &str) -> String {
    serde_json::json!({"type":"assistant","timestamp":at,"message":{"role":"assistant","content":[{"type":"text","text":text}]}}).to_string()
}

fn shell_call(at: &str, command: &str, name: &str) -> String {
    serde_json::json!({"type":"assistant","timestamp":at,"message":{"role":"assistant","content":[{"type":"tool_use","name":name,"input":{"command":command}}]}}).to_string()
}

const PROPOSAL: &str = "I looked at what you did before. Proposal:\n1. CV and job hunting: Documenten\\CV\nShall I do it? Say yes.";

/// A session as the Stop hook reads it: welcome answered, inventory run,
/// proposal shown, person answers `answer`.
fn session(date: &str, proposal: &str, answer: &str) -> Vec<String> {
    vec![
        person(&format!("{date}T10:00:00Z"), "Help me with my CV"),
        assistant_text(&format!("{date}T10:01:00Z"), "Welcome. Shall I set it up like this?"),
        person(&format!("{date}T10:02:00Z"), "ja"),
        shell_call(&format!("{date}T10:03:00Z"), "\"C:/thor2/serve.exe\" --db \"C:/thor2/thor.db\" inventory", "Bash"),
        assistant_text(&format!("{date}T10:04:00Z"), proposal),
        person(&format!("{date}T10:05:00Z"), answer),
    ]
}

fn store_with_note(dir: &Path) -> (PathBuf, PathBuf) {
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let db = dir.join("thor.db");
    ensure_and_seed_store(&db).unwrap();
    (home, db)
}

fn tidy_records(db: &Path) -> Vec<model::store::OrderEvidence> {
    let store = EventStore::open_existing(db).unwrap();
    model::store::order_evidence(&store).into_iter().filter(|e| e.kind == "tidy").collect()
}

// ------------------------------------------------------------------ fix 1

/// The field case: the subject folder already exists with its own memory and
/// the file already lies in it. The tidy moves nothing, and still leaves its
/// record (with 0 moved), so the person's yes plus the run settle the step.
#[test]
fn a_tidy_run_with_everything_already_in_place_still_leaves_its_record_and_settles() {
    let dir = tempfile::tempdir().unwrap();
    let (home, db) = store_with_note(dir.path());
    let subject = home.join("Documents").join("Cv en werk");
    std::fs::create_dir_all(&subject).unwrap();
    let cv = subject.join("cv.docx");
    std::fs::write(&cv, "cv").unwrap();
    let (top, key) = topic_of(&subject).unwrap();
    write_project_marker(&top, &key).unwrap();
    record_topic_opened(&db, &top, &key).unwrap();

    let plan = serde_json::json!([{ "folder": subject.display().to_string(), "files": [cv.display().to_string()] }]).to_string();
    let lines = ops::tidy::tidy(&db, &plan, &home).unwrap();
    assert!(lines.iter().any(|l| l.contains("moved 0 file(s)")), "{lines:?}");
    let records = tidy_records(&db);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].body["moved"], 0);
    assert_eq!(std::fs::read_to_string(&cv).unwrap(), "cv", "nothing was touched");

    let store = EventStore::open_existing(&db).unwrap();
    // The person's yes is in the transcript before the run: the run settles it.
    let t = timeline_of(&session("2026-03-02", PROPOSAL, "ja").join("\n"));
    let verdict = judge(&t, &model::store::order_evidence(&store), None);
    assert!(matches!(verdict, Verdict::Settled(_)), "{verdict:?}");
    // ... but the same run BEFORE the person's answer settles nothing (the person's words stay the evidence).
    let early = timeline_of(&session("2099-03-02", PROPOSAL, "ja").join("\n"));
    assert!(matches!(judge(&early, &model::store::order_evidence(&store), None), Verdict::Owed(_)));
}

#[test]
fn an_empty_files_list_for_an_existing_folder_leaves_its_record_and_an_empty_plan_none() {
    let dir = tempfile::tempdir().unwrap();
    let (home, db) = store_with_note(dir.path());
    let empty = ops::tidy::tidy(&db, "[]", &home).unwrap();
    assert!(empty.iter().any(|l| l.contains("No file was moved")), "{empty:?}");
    assert!(tidy_records(&db).is_empty(), "no folder at all: no record");

    let subject = home.join("Documents").join("Reizen");
    std::fs::create_dir_all(&subject).unwrap();
    let plan = serde_json::json!([{ "folder": subject.display().to_string(), "files": [] }]).to_string();
    ops::tidy::tidy(&db, &plan, &home).unwrap();
    assert_eq!(tidy_records(&db).len(), 1);

    // A plan whose only folder is refused (the Documents folder itself) leaves NO record.
    let (home2, db2) = {
        let d = dir.path().join("two");
        std::fs::create_dir_all(&d).unwrap();
        store_with_note(&d)
    };
    let broad = home2.join("Documents");
    std::fs::create_dir_all(&broad).unwrap();
    let plan = serde_json::json!([{ "folder": broad.display().to_string(), "files": [] }]).to_string();
    let lines = ops::tidy::tidy(&db2, &plan, &home2).unwrap();
    assert!(lines.iter().any(|l| l.contains("REFUSED")), "{lines:?}");
    assert!(tidy_records(&db2).is_empty());
}

// ------------------------------------------------------------------ fix 3

#[test]
fn a_file_anywhere_inside_the_target_folder_is_left_where_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let (home, db) = store_with_note(dir.path());
    let subject = home.join("Documents").join("Cv en werk");
    let deep = subject.join("cv").join("old").join("a.docx");
    std::fs::create_dir_all(deep.parent().unwrap()).unwrap();
    std::fs::write(&deep, "a").unwrap();
    let plan = serde_json::json!([{ "folder": subject.display().to_string(), "files": [deep.display().to_string()] }]).to_string();
    let lines = ops::tidy::tidy(&db, &plan, &home).unwrap();
    assert!(lines.iter().any(|l| l.contains("moved 0 file(s)")) && lines.iter().any(|l| l.contains("already in that folder")), "{lines:?}");
    assert!(deep.is_file(), "never moved up");
    assert!(!subject.join("a.docx").exists());
    // A folder whose name only STARTS like the target is another folder: its file moves.
    let sibling = home.join("Documents").join("Cv en werk 2");
    std::fs::create_dir_all(&sibling).unwrap();
    let other = sibling.join("b.docx");
    std::fs::write(&other, "b").unwrap();
    let plan = serde_json::json!([{ "folder": subject.display().to_string(), "files": [other.display().to_string()] }]).to_string();
    let lines = ops::tidy::tidy(&db, &plan, &home).unwrap();
    assert!(lines.iter().any(|l| l.contains("moved 1 file(s)")), "{lines:?}");
}

// ------------------------------------------------------------------ fix 2

fn write_transcript(projects: &Path, folder_key: &str, name: &str, lines: &[String]) {
    let d = projects.join(folder_key);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(format!("{name}.jsonl")), lines.join("\n") + "\n").unwrap();
}

fn with_cwd(line: &str, cwd: &Path) -> String {
    let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
    v["cwd"] = serde_json::Value::String(cwd.display().to_string());
    v.to_string()
}

fn scratch_workspace(home: &Path) -> PathBuf {
    home.join("AppData").join("Roaming").join("Claude").join("local-agent-mode-sessions").join("scratch-workspaces").join("abc123")
}

fn write_call(at: &str, path: &Path) -> String {
    serde_json::json!({"type":"assistant","timestamp":at,"message":{"role":"assistant","content":[{"type":"tool_use","name":"Write","input":{"file_path":path.display().to_string(),"content":"x"}}]}}).to_string()
}

fn nothing_to_order(inv: &serve::inventory::Inventory) -> bool {
    inv.folders.iter().all(|f| f.ordered || f.nothing_to_move) && inv.loose_files.is_empty()
}

#[test]
fn a_conversation_without_a_folder_and_without_files_does_not_block_but_stays_listed() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let scratch = scratch_workspace(&home);
    std::fs::create_dir_all(&scratch).unwrap();
    let projects = home.join(".claude").join("projects");
    // It wrote one file, which is gone now.
    let gone = scratch.join("draft.md");
    write_transcript(
        &projects,
        "scratch",
        "11111111-0000-0000-0000-000000000001",
        &[with_cwd(&person("2026-02-01T09:00:00Z", "what is a good cv layout"), &scratch), with_cwd(&write_call("2026-02-01T09:01:00Z", &gone), &scratch)],
    );
    let inv = serve::inventory::inventory(&projects, &home);
    let f = inv.folders.iter().find(|f| f.without_folder).expect("still listed");
    assert!(!f.ordered && f.nothing_to_move, "{f:?}");
    assert!(nothing_to_order(&inv), "{inv:?}");
    let text = serve::inventory::render_text(&inv);
    assert!(text.contains("CONVERSATIONS WITHOUT A FOLDER") && text.contains("NOTHING TO MOVE"), "{text}");
    assert!(text.contains("0 not in order yet"), "{text}");
}

#[test]
fn a_conversation_without_a_folder_with_an_existing_file_keeps_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let scratch = scratch_workspace(&home);
    std::fs::create_dir_all(&scratch).unwrap();
    let kept = scratch.join("cv.md");
    std::fs::write(&kept, "cv").unwrap();
    let projects = home.join(".claude").join("projects");
    write_transcript(
        &projects,
        "scratch",
        "11111111-0000-0000-0000-000000000002",
        &[with_cwd(&person("2026-02-01T09:00:00Z", "write my cv"), &scratch), with_cwd(&write_call("2026-02-01T09:01:00Z", &kept), &scratch)],
    );
    let inv = serve::inventory::inventory(&projects, &home);
    let f = inv.folders.iter().find(|f| f.without_folder).unwrap();
    assert!(!f.nothing_to_move, "{f:?}");
    assert!(!nothing_to_order(&inv));
    assert!(serve::inventory::render_text(&inv).contains("AT RISK"));
}

#[test]
fn a_loose_file_that_was_moved_is_no_longer_listed() {
    let dir = tempfile::tempdir().unwrap();
    let (home, db) = store_with_note(dir.path());
    let docs = home.join("Documents");
    std::fs::create_dir_all(&docs).unwrap();
    let cv = docs.join("cv.md");
    std::fs::write(&cv, "cv").unwrap();
    let projects = home.join(".claude").join("projects");
    let folder = docs.join("Cv");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(".thor-project"), "Cv\n").unwrap();
    write_transcript(
        &projects,
        "cv",
        "22222222-0000-0000-0000-000000000003",
        &[with_cwd(&person("2026-02-01T09:00:00Z", "write my cv"), &folder), with_cwd(&write_call("2026-02-01T09:01:00Z", &cv), &folder)],
    );
    assert_eq!(serve::inventory::inventory(&projects, &home).loose_files.len(), 1);
    let plan = serde_json::json!([{ "folder": folder.display().to_string(), "files": [cv.display().to_string()] }]).to_string();
    let lines = ops::tidy::tidy(&db, &plan, &home).unwrap();
    assert!(lines.iter().any(|l| l.contains("moved 1 file(s)")), "{lines:?}");
    let after = serve::inventory::inventory(&projects, &home);
    assert!(after.loose_files.is_empty(), "{:?}", after.loose_files);
    assert!(nothing_to_order(&after));
}

// ------------------------------------------------------------------ fix 5

fn proposal_found(body: &str) -> bool {
    let lines = [shell_call("2026-03-02T10:03:00Z", "\"C:/thor2/serve.exe\" --db x inventory", "Bash"), assistant_text("2026-03-02T10:04:00Z", body)];
    timeline_of(&lines.join("\n")).iter().any(|e| matches!(e, Entry::Proposal { .. }))
}

#[test]
fn proposal_shapes_are_recognised_and_ordinary_prose_is_not() {
    for (name, body) in [
        ("plain", "Voorstel:\n1. CV\n2. Reis"),
        ("bold number", "**1.** CV\n**2.** Reis"),
        ("paren", "1) CV\n2) Reis"),
        ("heading", "### 1. CV\n### 2. Reis"),
        ("bullet then number", "- 1. CV\n- 2. Reis"),
        ("bold bullet", "- **1. CV**\n- **2. Reis**"),
        ("emoji", "\u{1F4C1} 1. CV\n\u{1F4C1} 2. Reis"),
        ("escaped dot", "1\\. CV\n2\\. Reis"),
        ("blockquote", "> 1. CV\n> 2. Reis"),
        ("table", "| # | Onderwerp |\n|---|---|\n| 1 | CV |\n| 2 | Reis |"),
    ] {
        assert!(proposal_found(body), "{name} should be a proposal");
    }
    for (name, body) in [
        ("prose", "I looked at your earlier sessions. There were 2 of them, and 1 stood out."),
        ("a year line", "2026. was the year we started."),
        ("bullets", "- the CV folder\n- the trip folder"),
        ("no number one", "2. CV\n3. Reis"),
    ] {
        assert!(!proposal_found(body), "{name} must not be read as a proposal");
    }
}

#[test]
fn inventory_calls_are_recognised_in_every_usual_form() {
    for command in [
        "\"C:/Users/ann/thor2/serve.exe\" --db \"C:/x/thor.db\" inventory",
        "& 'C:\\Users\\ann\\thor2\\serve.exe' --db 'C:\\x\\thor.db' inventory",
        "$serve = 'C:\\Users\\ann\\thor2\\serve.exe'; & $serve --db x inventory",
        "Start-Process -FilePath 'C:\\Users\\ann\\thor2\\serve.exe' -ArgumentList '--db','x','inventory' -Wait -WindowStyle Hidden",
        "\"C:/thor2/serve.exe\" inventory --db x",
    ] {
        assert!(runs_inventory(command), "{command}");
    }
    assert!(!runs_inventory("\"C:/thor2/serve.exe\" --db x inventory --digest s.jsonl"));
    assert!(!runs_inventory("Start-Process notepad -ArgumentList 'inventory'"));
}

// ------------------------------------------------------------------ fix 4 and 6

fn first_evaluation(db: &Path) -> (EventStore, model::store::EvaluationStart) {
    let mut store = EventStore::open_existing(db).unwrap();
    let began = serve::eval_session::begin(db, &mut store, "5e1f0c3a-0000-4000-8000-0000000000aa", "Cv-en-werk", "command")
        .expect("an evaluation can start in the very first session");
    let start = serve::eval_session::decide(&mut store, began.start, std::time::Duration::from_secs(120));
    (store, start)
}

fn assert_clean_first_evaluation(store: &EventStore, start: &model::store::EvaluationStart) {
    let machine = start.frozen.machine.clone().unwrap();
    let detail: Vec<String> = machine.items.iter().filter(|i| i.outcome == model::store::MachineOutcome::NeedsAgent).map(|i| format!("{}: {}", i.id, i.detail)).collect();
    assert!(start.frozen.batch().is_empty() && start.frozen.a4_list().is_empty(), "batch {:?} a4 {:?}: {detail:?}", start.frozen.batch(), start.frozen.a4_list());
    assert!(model::store::evaluation_frozen_batch_owed(store, start).is_empty(), "nothing holds the first report");
    assert!(!machine.items.iter().any(|i| i.id == model::store::ORDER_NOTE_ID), "the tidy note is not an evaluation item");
}

#[test]
fn a_freshly_seeded_store_starts_an_evaluation_with_an_empty_batch_while_the_tidy_note_is_live() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let report = ensure_and_seed_store(&db).unwrap();
    let refused: Vec<_> = report.seeded.iter().filter(|i| !i.stored).map(|i| format!("{}: {:?}", i.id, i.refusal)).collect();
    assert!(refused.is_empty(), "every seeded note was stored: {refused:?}");
    {
        let store = EventStore::open_existing(&db).unwrap();
        assert!(model::store::show(&store, model::store::ORDER_NOTE_ID).is_ok(), "the tidy note is live and unsettled");
    }
    let (store, start) = first_evaluation(&db);
    assert_clean_first_evaluation(&store, &start);
}

/// The same for a store seeded WITHOUT the Windows-only notes (what a Linux
/// install gets), declared the way the installer does.
#[test]
fn a_store_seeded_without_the_windows_notes_starts_an_evaluation_with_an_empty_batch_too() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let mut store = EventStore::new(&db).unwrap();
    for item in ops::install::working_contract()
        .into_iter()
        .chain(ops::install::workflow_starter_pack_for(false))
        .chain(ops::install::helper_mode_notes())
        .chain(std::iter::once(ops::install::order_earlier_note()))
    {
        model::store::declare(&mut store, "install", "install", "install-fixture", &item).unwrap_or_else(|e| panic!("{}: {e}", item.id));
    }
    drop(store);
    let (store, start) = first_evaluation(&db);
    assert_clean_first_evaluation(&store, &start);
}

// ------------------------------------------------------------------ fix 7

/// A memory already stuck by the old behaviour: yes answered, tidy run with
/// nothing to move (so no record), one conversation without a folder and no
/// file. After the upgrade the honest next step is a plain `serve inventory`
/// (see serve/tests/tidy_step_inventory_settles.rs), or the tidy run again,
/// which now leaves its record. This is the second way.
#[test]
fn a_stuck_memory_is_settled_by_running_the_tidy_again_after_the_yes() {
    let dir = tempfile::tempdir().unwrap();
    let (home, db) = store_with_note(dir.path());
    let subject = home.join("Documents").join("Cv en werk");
    std::fs::create_dir_all(&subject).unwrap();
    let (top, key) = topic_of(&subject).unwrap();
    write_project_marker(&top, &key).unwrap();
    record_topic_opened(&db, &top, &key).unwrap();
    assert!(tidy_records(&db).is_empty(), "the stuck state: no record");
    let t = timeline_of(&session("2026-03-02", PROPOSAL, "ja").join("\n"));
    let store = EventStore::open_existing(&db).unwrap();
    assert!(matches!(judge(&t, &model::store::order_evidence(&store), None), Verdict::Owed(_)));
    drop(store);

    let plan = serde_json::json!([{ "folder": subject.display().to_string(), "files": [] }]).to_string();
    ops::tidy::tidy(&db, &plan, &home).unwrap();
    let store = EventStore::open_existing(&db).unwrap();
    assert!(matches!(judge(&t, &model::store::order_evidence(&store), None), Verdict::Settled(_)));
}

// ------------------------------------------------------------------ cwd that moved

/// A conversation that began in the app's temporary workspace and moved to a
/// subject folder belongs to where it ended up: it is not "without a folder".
#[test]
fn a_conversation_that_moved_to_another_folder_is_listed_under_the_last_one() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let scratch = scratch_workspace(&home);
    std::fs::create_dir_all(&scratch).unwrap();
    let subject = home.join("Documents").join("Kitchen renovation");
    std::fs::create_dir_all(&subject).unwrap();
    std::fs::write(subject.join(".thor-project"), "Kitchen renovation\n").unwrap();
    let projects = home.join(".claude").join("projects");
    write_transcript(
        &projects,
        "moved",
        "33333333-0000-0000-0000-000000000004",
        &[
            with_cwd(&person("2026-02-01T09:00:00Z", "plan my kitchen"), &scratch),
            with_cwd(&assistant_text("2026-02-01T09:01:00Z", "Moving to a folder."), &scratch),
            with_cwd(&person("2026-02-01T09:05:00Z", "ok"), &subject),
        ],
    );
    let inv = serve::inventory::inventory(&projects, &home);
    assert!(!inv.folders.iter().any(|f| f.without_folder), "{inv:?}");
    assert!(inv.folders.iter().any(|f| f.ordered && f.folder.ends_with("Kitchen renovation")), "{inv:?}");
    assert!(nothing_to_order(&inv));
}

// ------------------------------------------------------------------ all refused

/// A plan with one valid subject folder whose files were all refused (they sit in a
/// git repository) still leaves its record (0 moved, the refusals counted). A plan
/// whose only folders were refused at folder level (a drive root, the home folder,
/// Documents, a git repository, outside the home folder) leaves none: that junk
/// plan must never settle the step after any message of the person.
#[test]
fn a_plan_whose_files_were_all_refused_leaves_its_record_but_a_junk_plan_leaves_none() {
    let dir = tempfile::tempdir().unwrap();
    let (home, db) = store_with_note(dir.path());
    let repo = home.join("dev").join("site");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let file_in_repo = repo.join("notes.md");
    std::fs::write(&file_in_repo, "x").unwrap();
    let subject = home.join("Documents").join("Site notes");
    let plan = serde_json::json!([{ "folder": subject.display().to_string(), "files": [file_in_repo.display().to_string()] }]).to_string();
    ops::tidy::tidy(&db, &plan, &home).unwrap();
    let records = tidy_records(&db);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!((records[0].body["moved"].as_i64(), records[0].body["refused"].as_i64()), (Some(0), Some(1)));
    let store = EventStore::open_existing(&db).unwrap();
    let t = timeline_of(&session("2026-03-02", PROPOSAL, "ja").join("\n"));
    assert!(matches!(judge(&t, &model::store::order_evidence(&store), None), Verdict::Settled(_)));

    // The junk plan: a drive root, the home folder, Documents, a git repository, outside the home folder.
    let (home2, db2) = {
        let d = dir.path().join("two");
        std::fs::create_dir_all(&d).unwrap();
        store_with_note(&d)
    };
    let root = home2.ancestors().last().unwrap().to_path_buf();
    let docs = home2.join("Documents");
    std::fs::create_dir_all(&docs).unwrap();
    let repo2 = home2.join("dev").join("r");
    std::fs::create_dir_all(repo2.join(".git")).unwrap();
    let outside = dir.path().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let junk = serde_json::json!([
        { "folder": root.display().to_string(), "files": [] },
        { "folder": home2.display().to_string(), "files": [] },
        { "folder": docs.display().to_string(), "files": [] },
        { "folder": repo2.display().to_string(), "files": [] },
        { "folder": outside.display().to_string(), "files": [] },
    ])
    .to_string();
    let lines = ops::tidy::tidy(&db2, &junk, &home2).unwrap();
    assert_eq!(lines.iter().filter(|l| l.contains("REFUSED")).count(), 5, "{lines:?}");
    assert!(tidy_records(&db2).is_empty(), "no folder passed: no record");
    let store = EventStore::open_existing(&db2).unwrap();
    assert!(matches!(judge(&t, &model::store::order_evidence(&store), None), Verdict::Owed(_)));
}

/// A real answer may end in a question mark. When THOR's tidy record follows it and
/// no message that is not a question came first, that message is the answer, and a
/// later "thanks" does not bring the hold back. A tidy with no message of the
/// person after the proposal answers nothing.
#[test]
fn an_answer_ending_in_a_question_mark_settles_with_the_tidy_and_thanks_does_not_undo_it() {
    let dir = tempfile::tempdir().unwrap();
    let (home, db) = store_with_note(dir.path());
    let subject = home.join("Documents").join("Cv");
    let plan = serde_json::json!([{ "folder": subject.display().to_string(), "files": [] }]).to_string();
    ops::tidy::tidy(&db, &plan, &home).unwrap();
    let evidence = {
        let store = EventStore::open_existing(&db).unwrap();
        model::store::order_evidence(&store)
    };
    let head = vec![
        person("2026-03-02T10:00:00Z", "Help me with my CV"),
        assistant_text("2026-03-02T10:01:00Z", "Welcome."),
        person("2026-03-02T10:02:00Z", "ja"),
        shell_call("2026-03-02T10:03:00Z", "\"C:/thor2/serve.exe\" --db x inventory", "Bash"),
        assistant_text("2026-03-02T10:04:00Z", PROPOSAL),
    ];
    let mut answered = head.clone();
    answered.push(person("2026-03-02T10:05:00Z", "ja, en mag nummer 2 ook in de cv-map?"));
    assert!(matches!(judge(&timeline_of(&answered.join("
")), &evidence, None), Verdict::Settled(_)));
    answered.push(person("2026-03-02T10:20:00Z", "thanks"));
    assert!(matches!(judge(&timeline_of(&answered.join("
")), &evidence, None), Verdict::Settled(_)), "thanks does not bring the hold back");
    // Nothing typed after the proposal at all: the tidy answers nothing.
    assert_eq!(judge(&timeline_of(&head.join("
")), &evidence, None), Verdict::Waiting);
}

// ------------------------------------------------------------------ nothing to move: only the temporary workspace counts

/// A scratch conversation that wrote an existing file somewhere else (inside an
/// ordered subject folder) has nothing to move out of the temporary workspace.
#[test]
fn only_a_file_inside_the_temporary_workspace_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let scratch = scratch_workspace(&home);
    std::fs::create_dir_all(&scratch).unwrap();
    let subject = home.join("Documents").join("Cv");
    std::fs::create_dir_all(&subject).unwrap();
    std::fs::write(subject.join(".thor-project"), "Cv\n").unwrap();
    let elsewhere = subject.join("cv.md");
    std::fs::write(&elsewhere, "cv").unwrap();
    let projects = home.join(".claude").join("projects");
    write_transcript(
        &projects,
        "scratch",
        "44444444-0000-0000-0000-000000000005",
        &[with_cwd(&person("2026-02-01T09:00:00Z", "write my cv"), &scratch), with_cwd(&write_call("2026-02-01T09:01:00Z", &elsewhere), &scratch)],
    );
    let inv = serve::inventory::inventory(&projects, &home);
    let f = inv.folders.iter().find(|f| f.without_folder).unwrap();
    assert!(f.nothing_to_move, "the file is already in a subject folder: {f:?}");
}

// ------------------------------------------------------------------ paths

#[test]
fn paths_are_compared_without_dots_and_case_only_matters_on_windows() {
    let n = |s: &str| serve::inventory::norm(Path::new(s));
    assert_eq!(n("/a/b/../c/./d/"), "/a/c/d");
    assert_eq!(n("C:/Users/Ann/../Bob/x"), if cfg!(windows) { "c:/users/bob/x" } else { "C:/Users/Bob/x" });
    assert_eq!(n("/A/b"), if cfg!(windows) { "/a/b" } else { "/A/b" });
    assert_eq!(n("C:/../x"), if cfg!(windows) { "c:/x" } else { "C:/x" });
    // A file inside the target reached through a ".." is still inside it.
    let dir = tempfile::tempdir().unwrap();
    let (home, db) = store_with_note(dir.path());
    let subject = home.join("Documents").join("Cv");
    let file = subject.join("sub").join("a.md");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "a").unwrap();
    let odd_folder = home.join("Documents").join("Other").join("..").join("Cv");
    let plan = serde_json::json!([{ "folder": odd_folder.display().to_string(), "files": [file.display().to_string()] }]).to_string();
    let lines = ops::tidy::tidy(&db, &plan, &home).unwrap();
    assert!(lines.iter().any(|l| l.contains("already in that folder")), "{lines:?}");
    assert!(file.is_file());
}
