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

fn set_read_only(path: &std::path::Path, on: bool) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(on);
    std::fs::set_permissions(path, perms).unwrap();
}
