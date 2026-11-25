//! Holds, refusals and the texts around them, end to end through the real
//! compiled tool server: a folder for a subject is never "crowded", and the
//! other plain-text answers a first-time user reads.

mod common;

use common::*;
use serde_json::json;

fn orientation(id: &str, project: &str) -> serde_json::Value {
    json!({
        "id": id,
        "kind": "orientation",
        "text": format!("Fact {id} about the garden shed."),
        "project": project,
        "targets": [{"kind": "project", "value": project}],
        "falsifier": "A later note says the shed was moved.",
    })
}

/// THE CONTROL: the same seven notes in a git project are warned about, so the
/// test above can fail.
#[test]
fn a_git_project_is_still_warned_about_a_crowded_project_binding() {
    let dir = tempfile::tempdir().unwrap();
    let db = store(dir.path(), &["shop"]);
    let root = checkout(dir.path(), "shop", &[("a.txt", "x")], true);
    register(&db, "shop", &root);
    let mut mcp = Mcp::start(&db, &root);
    let warned = (0..7).any(|n| mcp.call("remember", orientation(&format!("shed-{n}"), "shop")).contains("never be shown"));
    assert!(warned, "the control must show the warning that the subject folder does not get");
}

/// In a folder for a subject (own marker, no git) a note bound to its own
/// project is found by matching the person's prompt, not by place, so the
/// seventh of them is not "crowded": no warning in the reply.
#[test]
fn a_folder_for_a_subject_never_warns_that_a_note_may_never_be_shown() {
    let dir = tempfile::tempdir().unwrap();
    let db = store(dir.path(), &["garden"]);
    let topic = dir.path().join("garden");
    std::fs::create_dir_all(&topic).unwrap();
    std::fs::write(topic.join(".thor-project"), "garden\n").unwrap();
    register(&db, "garden", &topic);
    let mut mcp = Mcp::start(&db, &topic);
    for n in 0..7 {
        let reply = mcp.call("remember", orientation(&format!("shed-{n}"), "garden"));
        assert!(reply.starts_with("stored"), "{reply}");
        assert!(!reply.contains("never be shown"), "a subject folder is not crowded: {reply}");
    }
}

/// A memory file that cannot be written (read-only file, full disk) answers in
/// plain words with what to tell the person, never a bare "store error".
#[test]
fn a_memory_file_that_cannot_be_written_says_so_plainly() {
    let dir = tempfile::tempdir().unwrap();
    let db = store(dir.path(), &["demo"]);
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join(".thor-project"), "demo\n").unwrap();
    let mut mcp = Mcp::start(&db, &work);
    let ok = mcp.call("remember", orientation("first", "demo"));
    assert!(ok.starts_with("stored"), "fixture: {ok}");
    drop(mcp);
    set_read_only(&db, true);
    let mut mcp = Mcp::start(&db, &work);
    let reply = mcp.call("remember", orientation("second", "demo"));
    let retract = mcp.call("retract", json!({"id": "first", "reason": "this note is wrong and was written by mistake"}));
    let revise = mcp.call("revise", json!({"id": "first", "text": "Fact first about the garden shed, corrected."}));
    set_read_only(&db, false);
    for other in [&retract, &revise] {
        assert!(other.contains("memory file cannot be written right now"), "every write says it: {other}");
    }
    assert!(reply.starts_with("REFUSED"), "{reply}");
    assert!(reply.contains("memory file cannot be written right now"), "{reply}");
    assert!(reply.contains("Tell the person in one sentence") && reply.contains("nothing was saved"), "{reply}");
    assert!(!reply.contains("store error"), "{reply}");
}

/// A refused write is a failed call to the client (`isError`); a stored one is not.
#[test]
fn a_refusal_sets_is_error_and_a_stored_note_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let db = store(dir.path(), &["demo"]);
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join(".thor-project"), "demo\n").unwrap();
    let mut mcp = Mcp::start(&db, &work);
    let stored = mcp.call_result("remember", orientation("fine", "demo"));
    assert!(stored["content"][0]["text"].as_str().unwrap().starts_with("stored"), "{stored}");
    assert_ne!(stored["isError"], json!(true), "a stored note is no error: {stored}");
    let refused = mcp.call_result("remember", rule("bad", "A rule with no way to fire.", Some("demo")));
    assert!(refused["content"][0]["text"].as_str().unwrap().starts_with("REFUSED"), "{refused}");
    assert_eq!(refused["isError"], json!(true), "a refusal is an error to the client: {refused}");
    let retract = mcp.call_result("retract", json!({"id": "no-such-note", "reason": "this note never existed at all"}));
    let text = retract["content"][0]["text"].as_str().unwrap().to_lowercase();
    assert_eq!(retract["isError"], json!(text.starts_with("refused")), "{retract}");
}

/// A folder with no code to search says so plainly, without naming flags the
/// installed tool server is not started with.
#[test]
fn the_code_tools_say_a_folder_has_no_code_to_search() {
    let dir = tempfile::tempdir().unwrap();
    let db = store(dir.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, dir.path());
    for (tool, args) in [
        ("search_code", json!({"query": "main"})),
        ("where_used", json!({"name": "main"})),
        ("outline", json!({"path": "src/main.rs"})),
    ] {
        let reply = mcp.call(tool, args);
        assert!(reply.contains("this folder has no code to search") && reply.contains("Nothing is wrong"), "{tool}: {reply}");
        assert!(!reply.contains("--code-index-db") && !reply.contains("--repo"), "{tool}: {reply}");
    }
}

/// A file in the project's own top folder can be anchored by its bare name, and
/// the note then comes back on an Edit however the call spells the path.
#[test]
fn a_file_in_the_project_root_is_anchored_by_its_name_and_fires_however_the_path_is_spelled() {
    let dir = tempfile::tempdir().unwrap();
    let db = store(dir.path(), &["demo"]);
    let session = Session::with_store(dir.path(), db.clone(), WRITING_SESSION);
    std::fs::write(session.cwd.join("main.py"), "print('hi')\n").unwrap();
    register(&db, "demo", &session.cwd);
    let mut mcp = session.mcp();
    let note = |id: &str, name: &str| {
        json!({
            "id": id, "kind": "orientation", "project": "demo",
            "targets": [{"kind": "path", "value": name}],
            "text": if id == "calc-1" { "Keep the pytest suite green before you touch this module again." } else { "main.py holds the calculator functions; division is the next one to add." },
            "falsifier": "main.py no longer holds the calculator functions.",
        })
    };
    // A name that is not a file there stays refused, and the refusal names the form that works.
    let missing = mcp.call("remember", note("calc-missing", "lib.py"));
    assert!(missing.starts_with("REFUSED") && missing.contains("bare role name") && missing.contains("src/main.py"), "{missing}");
    for (n, spelled) in ["main.py", "./main.py"].iter().enumerate() {
        let reply = mcp.call("remember", note(&format!("calc-{n}"), spelled));
        assert!(reply.starts_with("stored"), "{spelled}: {reply}");
    }
    let root = session.cwd.to_string_lossy().to_string();
    let spellings = [
        format!("{root}\\main.py"),
        format!("{}/main.py", root.replace('\\', "/")),
        "main.py".to_string(),
        "./main.py".to_string(),
    ];
    for (n, spelled) in spellings.into_iter().enumerate() {
        let out = session.hook(json!({
            "hook_event_name": "PreToolUse", "session_id": format!("0de7dc52-1b97-4b66-948f-61e4438043b{n}"), "cwd": session.cwd,
            "transcript_path": session.transcript, "tool_name": "Edit",
            "tool_input": {"file_path": spelled, "old_string": "a", "new_string": "b"}, "tool_use_id": "toolu_1"
        }));
        assert!(out.contains("calculator functions"), "an Edit of {spelled} must bring the note: {out}");
    }
}

/// `revise` makes the same project-root test `remember` does: a text-only
/// change of a note anchored to a file in the project's top folder, and a
/// change that names that file as the new anchor, are both accepted; a name
/// that is not a file there stays refused.
#[test]
fn revise_accepts_a_file_in_the_project_root_exactly_as_remember_does() {
    let dir = tempfile::tempdir().unwrap();
    let db = store(dir.path(), &["demo"]);
    let session = Session::with_store(dir.path(), db.clone(), WRITING_SESSION);
    std::fs::write(session.cwd.join("main.py"), "print('hi')\n").unwrap();
    register(&db, "demo", &session.cwd);
    let mut mcp = session.mcp();
    let stored = mcp.call(
        "remember",
        json!({
            "id": "calc-root", "kind": "orientation", "project": "demo",
            "targets": [{"kind": "path", "value": "main.py"}],
            "text": "main.py holds the calculator functions; division is the next one to add.",
            "falsifier": "main.py no longer holds the calculator functions.",
        }),
    );
    assert!(stored.starts_with("stored"), "{stored}");
    // A text-only change.
    let text_only = mcp.call(
        "revise",
        json!({"id": "calc-root", "text": "main.py holds the calculator functions; division and modulo are the next ones to add."}),
    );
    assert!(!text_only.starts_with("REFUSED"), "a text-only revise must not trip over the anchor: {text_only}");
    // A change that names the same file as its anchor again.
    let rebound = mcp.call(
        "revise",
        json!({"id": "calc-root", "project": "demo", "targets": [{"kind": "path", "value": "./main.py"}], "because": "the anchor is spelled with a dot-slash now, still the same file"}),
    );
    assert!(!rebound.starts_with("REFUSED"), "the same file as the anchor: {rebound}");
    // A bare name that is not a file there is still refused.
    let missing = mcp.call(
        "revise",
        json!({"id": "calc-root", "project": "demo", "targets": [{"kind": "path", "value": "lib.py"}], "because": "this binding names a file that is not in the project folder"}),
    );
    assert!(missing.starts_with("REFUSED") && missing.contains("bare role name"), "{missing}");
}

fn set_read_only(path: &std::path::Path, on: bool) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(on);
    std::fs::set_permissions(path, perms).unwrap();
}
